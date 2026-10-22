//! 麦克风方向（Mac → Windows）和连通性检测。
//!
//! - 网络接收线程：和播放方向共用同一个 socket。收 Mac 的麦克风包（写进环形缓冲）
//!   和心跳回声（算往返时间）。
//! - 渲染线程：虚拟线装好时才有。以 CABLE 的时钟为准拉取数据（抖动缓冲 + 漂移补偿），
//!   写进「CABLE Input」，应用从「CABLE Output」录音。
//! - 控制线程：每 0.5 秒发心跳（有应用在录音时带上「需要麦克风」），每秒刷新状态。

use super::common::*;
use super::engine::{EngineStatus, MicStatus, PeerStatus};
use aurio_core::playout::PlayoutConfig;
use aurio_core::proto::{flags, heartbeat, ptype, stream, Header, FORMAT_48K_S24_MONO, HEADER_LEN};
use aurio_core::stats::{state, AtomicF32};
use aurio_core::{new_rx_stream, Depacketizer, Playout, RxStats};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wasapi::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MicMode {
    Auto,
    On,
    Off,
}

impl MicMode {
    pub fn parse(s: &str) -> Res<MicMode> {
        match s {
            "auto" | "" => Ok(MicMode::Auto),
            "on" => Ok(MicMode::On),
            "off" => Ok(MicMode::Off),
            _ => Err(format!("麦克风模式只能是 auto / on / off，收到 {s}").into()),
        }
    }

    fn label(self) -> &'static str {
        match self {
            MicMode::Auto => "自动",
            MicMode::On => "常开",
            MicMode::Off => "关闭",
        }
    }
}

pub struct MicOptions {
    pub mode: MicMode,
    /// 写入的播放设备（虚拟线的播放端）
    pub render: String,
    /// 应用录音用的设备（虚拟线的录音端），自动模式下用来检测有没有应用在用
    pub detect: String,
    /// WASAPI 队列在一个引擎周期之外再留的余量（毫秒）
    pub queue_ms: f64,
}

/// 各线程共享的状态
struct Shared {
    stats: Arc<RxStats>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<EngineStatus>>,
    /// 渲染线程报告：WASAPI 队列长度、最近一秒写入的峰值
    padding_frames: AtomicU32,
    render_peak: AtomicF32,
    /// 心跳发送时刻（按 seq 取模），用来算往返时间
    hb_sent: Mutex<[Option<Instant>; 64]>,
    last_ack: Mutex<Option<Instant>>,
    rtt_ms: AtomicF32,
    mac_aec: AtomicBool,
}

/// 启动麦克风方向和连通性检测相关的线程。
pub fn spawn(
    sock: &UdpSocket,
    dest: SocketAddr,
    opts: MicOptions,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<EngineStatus>>,
) -> Res<Vec<JoinHandle<()>>> {
    let _ = initialize_mta();
    let render_name = find_device(Direction::Render, &opts.render).ok().and_then(|d| d.get_friendlyname().ok());
    let available = opts.mode != MicMode::Off && render_name.is_some();

    let (depack, playout, stats) =
        new_rx_stream(FORMAT_48K_S24_MONO, stream::MIC, 2, RATE as f64, PlayoutConfig::default());
    let shared = Arc::new(Shared {
        stats,
        stop,
        status,
        padding_frames: AtomicU32::new(0),
        render_peak: AtomicF32::default(),
        hb_sent: Mutex::new([None; 64]),
        last_ack: Mutex::new(None),
        rtt_ms: AtomicF32::default(),
        mac_aec: AtomicBool::new(false),
    });
    {
        let mut s = shared.status.lock().unwrap();
        s.mic = MicStatus {
            available,
            device: render_name.clone().unwrap_or_default(),
            mode: opts.mode.label().into(),
            state: "空闲".into(),
            ..Default::default()
        };
        if opts.mode != MicMode::Off && render_name.is_none() {
            s.errors.push(format!(
                "没找到「{}」，麦克风方向不可用。先安装 VB-Cable（安装包和 Aurio 放在一起），装完重启电脑。",
                opts.render
            ));
        }
    }

    let mut threads = Vec::new();
    let rx_sock = sock.try_clone()?;
    rx_sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    let sh = shared.clone();
    threads.push(
        std::thread::Builder::new()
            .name("aurio-net-rx".into())
            .spawn(move || net_thread(rx_sock, dest, depack, sh))?,
    );

    if available {
        let sh = shared.clone();
        let (name, queue_ms) = (opts.render.clone(), opts.queue_ms);
        threads.push(std::thread::Builder::new().name("aurio-mic-render".into()).spawn(move || {
            if let Err(e) = render_thread(&name, playout, queue_ms, &sh) {
                sh.status.lock().unwrap().errors.push(format!("麦克风方向停止了：{e}"));
            }
        })?);
    }

    let hb_sock = sock.try_clone()?;
    let sh = shared.clone();
    let (mode, detect) = (if available { opts.mode } else { MicMode::Off }, opts.detect.clone());
    threads.push(
        std::thread::Builder::new()
            .name("aurio-control".into())
            .spawn(move || control_thread(hb_sock, dest, mode, &detect, &sh))?,
    );
    Ok(threads)
}

fn net_thread(sock: UdpSocket, dest: SocketAddr, mut depack: Depacketizer, sh: Arc<Shared>) {
    aurio_core::rt::promote_current_thread();
    let start = Instant::now();
    let mut buf = [0u8; 2048];
    while !sh.stop.load(Relaxed) {
        match sock.recv_from(&mut buf) {
            // 只收 Mac 那边来的包
            Ok((n, from)) if from.ip() == dest.ip() => {
                let pkt = &buf[..n];
                if let Ok(h) = Header::parse(pkt) {
                    if h.ptype == ptype::HEARTBEAT && h.flags & flags::HEARTBEAT_ACK != 0 {
                        let now = Instant::now();
                        if let Some(t) = sh.hb_sent.lock().unwrap()[(h.seq % 64) as usize] {
                            sh.rtt_ms.store(now.duration_since(t).as_secs_f32() * 1000.0);
                        }
                        *sh.last_ack.lock().unwrap() = Some(now);
                        sh.mac_aec.store(h.flags & flags::AEC_ACTIVE != 0, Relaxed);
                        continue;
                    }
                }
                depack.handle(pkt, start.elapsed().as_micros() as u64);
            }
            Ok(_) => {}
            // 超时或 Windows 的 ConnectionReset（对端端口不可达时的 ICMP 回包），忽略即可
            Err(_) => {}
        }
    }
}

fn render_thread(name: &str, mut playout: Playout, queue_ms: f64, sh: &Shared) -> Res<()> {
    let _ = initialize_mta();
    mmcss_pro_audio();
    let dev = find_device(Direction::Render, name)?;
    let mut client = dev.get_iaudioclient()?;
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, RATE, 2, None);
    let (def_period, min_period) = client.get_device_period()?;
    client.initialize_client(
        &fmt,
        &Direction::Render,
        &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_period },
    )?;
    let ev = client.set_get_eventhandle()?;
    let render = client.get_audiorenderclient()?;
    let buf_frames = client.get_buffer_size()? as usize;
    // 引擎每个周期取走 period 帧；每次事件时补到 period + 余量，队列短、延迟低
    let period = (def_period as f64 * RATE as f64 / 1e7).round() as usize;
    let target = (period + (queue_ms * RATE as f64 / 1000.0) as usize).min(buf_frames);

    let mut samples = vec![0f32; buf_frames * 2];
    let mut bytes: Vec<u8> = Vec::with_capacity(buf_frames * 8);
    let mut peak = 0f32;
    let mut last_peak = Instant::now();
    client.start_stream()?;
    while !sh.stop.load(Relaxed) {
        ev.wait_for_event(2000).map_err(|_| "CABLE Input 事件超时（设备被拔掉或被禁用？）")?;
        let padding = client.get_current_padding()? as usize;
        sh.padding_frames.store(padding as u32, Relaxed);
        if padding < target {
            let n = (target - padding).min(client.get_available_space_in_frames()? as usize);
            if n > 0 {
                playout.render(&mut samples[..n * 2], n);
                peak = samples[..n * 2].iter().fold(peak, |m, v| m.max(v.abs()));
                bytes.clear();
                for s in &samples[..n * 2] {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                render.write_to_device(n, &bytes, None)?;
            }
        }
        if last_peak.elapsed() >= Duration::from_secs(1) {
            sh.render_peak.store(peak);
            peak = 0.0;
            last_peak = Instant::now();
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

/// 有没有应用正在从这个录音设备录音。
struct UsageDetector {
    manager: Option<AudioSessionManager>,
}

impl UsageDetector {
    fn new(name: &str) -> UsageDetector {
        let manager = find_device(Direction::Capture, name).ok().and_then(|d| d.get_iaudiosessionmanager().ok());
        UsageDetector { manager }
    }

    fn in_use(&self) -> bool {
        let Some(m) = &self.manager else { return false };
        let Ok(en) = m.get_audiosessionenumerator() else { return false };
        let n = en.get_count().unwrap_or(0);
        (0..n).any(|i| {
            en.get_session(i)
                .and_then(|s| s.get_state())
                .map(|st| st == SessionState::Active)
                .unwrap_or(false)
        })
    }
}

fn control_thread(sock: UdpSocket, dest: SocketAddr, mode: MicMode, detect: &str, sh: &Shared) {
    let _ = initialize_mta();
    let detector = if mode == MicMode::Auto { Some(UsageDetector::new(detect)) } else { None };
    let mut seq = 0u32;
    let mut hb = [0u8; HEADER_LEN];
    let mut tick = 0u32;
    let mut last_packets = 0u64;
    while !sh.stop.load(Relaxed) {
        let want = match &detector {
            Some(d) => d.in_use(),
            None => mode == MicMode::On,
        };
        heartbeat(stream::PLAYBACK, if want { flags::MIC_REQUEST } else { 0 }, seq, &mut hb);
        sh.hb_sent.lock().unwrap()[(seq % 64) as usize] = Some(Instant::now());
        let _ = sock.send_to(&hb, dest);
        seq = seq.wrapping_add(1);

        tick += 1;
        if tick.is_multiple_of(2) {
            let s = sh.stats.snapshot(RATE as f64);
            let pps = (s.packets - last_packets) as u32;
            last_packets = s.packets;
            let since_ack = sh.last_ack.lock().unwrap().map(|t| t.elapsed().as_secs_f32());
            let mut st = sh.status.lock().unwrap();
            let m = &mut st.mic;
            m.requested = want;
            m.state = match s.state {
                state::PLAYING => "播放",
                state::BUFFERING if s.packets > 0 => "缓冲",
                _ => "空闲",
            }
            .into();
            m.fill_ms = s.fill_ms;
            m.target_ms = s.target_ms;
            m.ppm = s.ppm;
            m.max_gap_ms = s.max_gap_ms;
            m.packets_per_s = pps;
            m.underruns = s.underruns;
            m.lost_frames = s.lost_frames;
            m.queue_ms = sh.padding_frames.load(Relaxed) as f32 * 1000.0 / RATE as f32;
            m.peak_db = peak_db(sh.render_peak.load());
            st.peer = PeerStatus {
                responding: since_ack.is_some_and(|t| t < 3.0),
                rtt_ms: sh.rtt_ms.load(),
                since_reply_s: since_ack.unwrap_or(-1.0),
                mac_echo_cancel: sh.mac_aec.load(Relaxed) && since_ack.is_some_and(|t| t < 3.0),
            };
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
