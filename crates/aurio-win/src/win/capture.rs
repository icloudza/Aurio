//! 播放方向：回环采集系统默认播放设备（或从指定录音设备采集），打包发给 Mac。

use super::common::*;
use super::engine::EngineStatus;
use aurio_core::proto::flags;
use aurio_core::sender::UdpSender;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use wasapi::*;

pub struct CaptureSpec {
    /// 回环采集的播放设备，空字符串 = 系统默认播放设备
    pub loopback: String,
    /// 非空时改为从这个录音设备采集（不用回环）
    pub capture_device: String,
    pub mode: Mode,
    /// 麦克风方向写入的设备名，用来防止把虚拟线当成播放来源
    pub mic_render: String,
}

fn open(spec: &CaptureSpec) -> Res<(Stream, String, bool)> {
    let (device, loopback) = if !spec.capture_device.is_empty() {
        (find_device(Direction::Capture, &spec.capture_device)?, false)
    } else {
        (find_device(Direction::Render, &spec.loopback)?, true)
    };
    let name = device.get_friendlyname().unwrap_or_default();
    if loopback && is_virtual_cable(&name, &spec.mic_render) {
        return Err(format!(
            "播放来源是「{name}」，它是虚拟线的播放端：系统声音会直接进虚拟麦克风。\
             请把 Windows 默认播放设备换成显示器的 HDMI 音频之类没接喇叭的设备，或者在设置里选别的播放来源"
        )
        .into());
    }
    let st = open_stream(&device, Direction::Capture, spec.mode, loopback)?;
    Ok((st, name, loopback))
}

/// 采集线程主体。打开设备的结果通过 `init` 报告给启动方，之后一直运行到 `stop`。
pub fn run(
    spec: CaptureSpec,
    mut tx: UdpSender,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<EngineStatus>>,
    init: Sender<Result<(), String>>,
) {
    let _ = wasapi::initialize_mta();
    let opened = open(&spec).and_then(|(st, name, lb)| {
        let capture = st.client.get_audiocaptureclient()?;
        st.client.start_stream()?;
        Ok((st, capture, name, lb))
    });
    let (st, capture, name, loopback) = match opened {
        Ok(v) => v,
        Err(e) => {
            let _ = init.send(Err(e.to_string()));
            return;
        }
    };
    mmcss_pro_audio();
    {
        let mut s = status.lock().unwrap();
        s.playback.device = name;
        s.playback.method = format!("{}{}", if loopback { "回环采集，" } else { "" }, st.desc);
    }
    let _ = init.send(Ok(()));

    if let Err(e) = capture_loop(&st, &capture, &mut tx, loopback, spec.mode, &stop, &status) {
        status.lock().unwrap().errors.push(format!("播放方向停止了：{e}"));
    }
    let _ = st.client.stop_stream();
}

fn capture_loop(
    st: &Stream,
    capture: &AudioCaptureClient,
    tx: &mut UdpSender,
    loopback: bool,
    mode: Mode,
    stop: &AtomicBool,
    status: &Mutex<EngineStatus>,
) -> Res<()> {
    let mut raw = vec![0u8; (st.buffer_frames as usize + 64) * st.conv.bytes_per_frame()];
    let mut samples: Vec<f32> = Vec::with_capacity(st.buffer_frames as usize * 2 + 128);
    let mut expected_index: Option<u64> = None;
    let exclusive = mode == Mode::Exclusive;
    let silence = vec![0f32; 480 * 2];

    // 每秒统计
    let mut wakes = 0u32;
    let mut frames_min = u32::MAX;
    let mut frames_max = 0u32;
    let mut max_interval = 0i64;
    let mut peak = 0f32;
    let mut last_packets = 0u64;
    let mut discontinuities = 0u64;
    let mut pad_frames_total = 0u64;
    // 上一次拿到数据（或补静音）的时刻，100 ns 单位。从开始那一刻算起：
    // 开始时系统没在放声音，也要立刻补静音，否则 Mac 一直收不到包，会以为没连上。
    let mut last_data = qpc_100ns();
    let mut has_data = false;
    let mut last_report = qpc_100ns();

    while !stop.load(Relaxed) {
        st.wait()?;
        let now = qpc_100ns();
        let mut got = 0u32;
        loop {
            if !exclusive && capture.get_next_packet_size()? == Some(0) {
                break;
            }
            let (n, info) = capture.read_from_device(&mut raw)?;
            if n == 0 {
                break;
            }
            if info.flags.data_discontinuity {
                discontinuities += 1;
                // 回环模式下没声音时我们已经自己补过静音，不能再按设备位置补一次
                if loopback {
                    tx.packetizer().set_next_flags(flags::DISCONTINUITY);
                } else if let Some(exp) = expected_index {
                    if info.index > exp && info.index - exp < RATE as u64 {
                        tx.packetizer().skip_frames(info.index - exp);
                    } else {
                        tx.packetizer().set_next_flags(flags::DISCONTINUITY);
                    }
                }
            }
            expected_index = Some(info.index + n as u64);
            samples.clear();
            if info.flags.silent {
                samples.resize(n as usize * 2, 0.0);
            } else {
                st.conv.convert(&raw, n as usize, &mut samples);
                peak = samples.iter().fold(peak, |m, v| m.max(v.abs()));
            }
            tx.send(&samples);
            got += n;
            if exclusive {
                break;
            }
        }
        if got > 0 {
            wakes += 1;
            frames_min = frames_min.min(got);
            frames_max = frames_max.max(got);
            if has_data {
                max_interval = max_interval.max(now - last_data);
            }
            has_data = true;
            last_data = now;
        } else if loopback {
            // 回环采集在系统没放声音时不给数据。按真实时间补静音，让接收端的数据流不断；
            // 始终落后真实时间 10 ms，跟正常采集的节奏一致。
            let idle = now - last_data;
            if idle >= 200_000 {
                let frames = ((idle - 100_000) as i128 * RATE as i128 / 10_000_000) as usize;
                let mut left = frames;
                while left > 0 {
                    let k = left.min(480);
                    tx.send(&silence[..k * 2]);
                    left -= k;
                }
                pad_frames_total += frames as u64;
                last_data += (frames as i128 * 10_000_000 / RATE as i128) as i64;
            }
        }

        if now - last_report >= 10_000_000 {
            last_report = now;
            let ms = |f: u32| f as f32 * 1000.0 / RATE as f32;
            let mut s = status.lock().unwrap();
            let p = &mut s.playback;
            p.wakes_per_s = wakes;
            p.batch_ms_min = if frames_min == u32::MAX { 0.0 } else { ms(frames_min) };
            p.batch_ms_max = ms(frames_max);
            p.max_data_gap_ms = max_interval as f32 / 1e4;
            p.sent_packets = tx.sent_packets;
            p.packets_per_s = (tx.sent_packets - last_packets) as u32;
            p.send_errors = tx.send_errors;
            p.discontinuities = discontinuities;
            p.padded_s = pad_frames_total as f32 / RATE as f32;
            p.peak_db = peak_db(peak);
            drop(s);
            last_packets = tx.sent_packets;
            wakes = 0;
            frames_min = u32::MAX;
            frames_max = 0;
            max_interval = 0;
            peak = 0.0;
        }
    }
    Ok(())
}
