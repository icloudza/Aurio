//! Windows 端引擎：把播放方向（采集 → Mac）和麦克风方向（Mac → CABLE Input）组织在一起，
//! 可以启动、停止，状态随时可读。命令行和图形界面共用这一套。

use super::capture::{self, CaptureSpec};
use super::common::*;
use super::mic::{self, MicMode, MicOptions};
use aurio_core::proto::{self, stream, FORMAT_48K_S24_STEREO};
use aurio_core::sender::UdpSender;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    /// Mac 的地址：「IP」或「IP:端口」，端口默认 47800
    pub dest: String,
    /// 回环采集的播放设备，空 = 系统默认播放设备
    pub loopback: String,
    /// 非空时改为从录音设备采集（不用回环）
    pub capture_device: String,
    /// shared / lowlat / exclusive，只对 capture_device 有意义
    pub mode: String,
    pub fpp: usize,
    /// auto / on / off
    pub mic_mode: String,
    pub mic_render: String,
    pub mic_detect: String,
    pub mic_queue_ms: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            dest: String::new(),
            loopback: String::new(),
            capture_device: String::new(),
            mode: "shared".into(),
            fpp: proto::DEFAULT_FRAMES_PER_PACKET,
            mic_mode: "auto".into(),
            mic_render: "CABLE Input".into(),
            mic_detect: "CABLE Output".into(),
            mic_queue_ms: 5.0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PlaybackStatus {
    pub device: String,
    pub method: String,
    pub wakes_per_s: u32,
    pub batch_ms_min: f32,
    pub batch_ms_max: f32,
    pub max_data_gap_ms: f32,
    pub sent_packets: u64,
    pub packets_per_s: u32,
    pub send_errors: u64,
    pub discontinuities: u64,
    /// 系统没放声音时补的静音总时长
    pub padded_s: f32,
    /// 最近一秒采集到的峰值电平（dBFS）
    pub peak_db: f32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MicStatus {
    /// 虚拟线装好了、麦克风方向可用
    pub available: bool,
    pub device: String,
    pub mode: String,
    /// 正在请求 Mac 开麦克风
    pub requested: bool,
    /// 空闲 / 缓冲 / 播放
    pub state: String,
    pub fill_ms: f32,
    pub target_ms: f32,
    pub ppm: f32,
    pub max_gap_ms: f32,
    pub packets_per_s: u32,
    pub underruns: u64,
    pub lost_frames: u64,
    pub queue_ms: f32,
    pub peak_db: f32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PeerStatus {
    /// 最近 3 秒内 Mac 回过心跳
    pub responding: bool,
    pub rtt_ms: f32,
    /// 距离 Mac 上次回应多久，负数表示从没回应过
    pub since_reply_s: f32,
    /// Mac 正在做回声消除
    pub mac_echo_cancel: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct EngineStatus {
    pub running: bool,
    pub dest: String,
    pub playback: PlaybackStatus,
    pub mic: MicStatus,
    pub peer: PeerStatus,
    pub errors: Vec<String>,
}

pub struct Engine {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<EngineStatus>>,
    threads: Vec<JoinHandle<()>>,
}

/// 「192.168.1.20」「192.168.1.20:47800」「mac.local」都接受。
pub fn parse_dest(s: &str) -> Res<SocketAddr> {
    let s = s.trim();
    if s.is_empty() {
        return Err("还没选 Mac".into());
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, proto::DEFAULT_PORT));
    }
    let with_port = if s.contains(':') { s.to_string() } else { format!("{s}:{}", proto::DEFAULT_PORT) };
    with_port
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| format!("无法解析地址 {s}").into())
}

impl Engine {
    pub fn start(cfg: &EngineConfig) -> Res<Engine> {
        let dest = parse_dest(&cfg.dest)?;
        let mic_opts = MicOptions {
            mode: MicMode::parse(&cfg.mic_mode)?,
            render: cfg.mic_render.clone(),
            detect: cfg.mic_detect.clone(),
            queue_ms: cfg.mic_queue_ms,
        };
        let spec = CaptureSpec {
            loopback: cfg.loopback.clone(),
            capture_device: cfg.capture_device.clone(),
            mode: Mode::parse(&cfg.mode)?,
            mic_render: cfg.mic_render.clone(),
        };
        let tx = UdpSender::new(dest, FORMAT_48K_S24_STEREO, stream::PLAYBACK, cfg.fpp)?;
        let sock = tx.socket().try_clone()?;

        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(EngineStatus { running: true, dest: dest.to_string(), ..Default::default() }));

        // 采集线程负责打开设备；等它报告结果，打不开就直接把错误返回给调用方
        let (init_tx, init_rx) = mpsc::channel();
        let (stop1, status1) = (stop.clone(), status.clone());
        let cap = std::thread::Builder::new()
            .name("aurio-capture".into())
            .spawn(move || capture::run(spec, tx, stop1, status1, init_tx))?;
        match init_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = cap.join();
                return Err(e.into());
            }
            Err(_) => {
                stop.store(true, Relaxed);
                return Err("打开播放来源超时".into());
            }
        }

        let mut threads = vec![cap];
        match mic::spawn(&sock, dest, mic_opts, stop.clone(), status.clone()) {
            Ok(t) => threads.extend(t),
            Err(e) => {
                stop.store(true, Relaxed);
                for t in threads {
                    let _ = t.join();
                }
                return Err(e);
            }
        }
        Ok(Engine { stop, status, threads })
    }

    pub fn status(&self) -> EngineStatus {
        self.status.lock().unwrap().clone()
    }

    /// 停止所有线程，最多等两三秒（各线程的等待都有超时）。
    pub fn stop(self) {
        self.stop.store(true, Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
    }
}
