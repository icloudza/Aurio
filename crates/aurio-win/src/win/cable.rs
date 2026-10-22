//! 测量 Windows 本机这一段的延迟：往虚拟线播放端写短促的「滴」声，在虚拟线录音端检测它。
//!
//! 播放侧用 IAudioClock 算出每个「滴」实际送进设备的时刻，录音侧用缓冲时间戳算出它被录到的时刻，
//! 两者都是 QPC 时基。另外记录我们的程序真正拿到数据的时刻（包含等待采集周期的时间）。

use super::common::*;
use aurio_tools::Args;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use wasapi::*;

const CLICK_MS: usize = 2;
const INTERVAL_MS: usize = 500;

fn render_thread(name: String, due_tx: mpsc::Sender<i64>, stop: Arc<AtomicBool>, info_tx: mpsc::Sender<String>) -> Res<()> {
    let _ = initialize_mta();
    mmcss_pro_audio();
    let dev = find_device(Direction::Render, &name)?;
    let mut client = dev.get_iaudioclient()?;
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, RATE, 2, None);
    let (_, min_period) = client.get_device_period()?;
    client.initialize_client(
        &fmt,
        &Direction::Render,
        &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_period },
    )?;
    let ev = client.set_get_eventhandle()?;
    let render = client.get_audiorenderclient()?;
    let clock = client.get_audioclock()?;
    let freq = clock.get_frequency()? as f64;
    let buf_frames = client.get_buffer_size()?;
    let _ = info_tx.send(format!(
        "播放：{}，共享模式，缓冲 {} 帧（{:.2} ms）",
        dev.get_friendlyname().unwrap_or_default(),
        buf_frames,
        buf_frames as f64 * 1000.0 / RATE as f64
    ));

    let click_len = (RATE * CLICK_MS / 1000) as u64;
    let interval = (RATE * INTERVAL_MS / 1000) as u64;
    let mut next_click = RATE as u64; // 1 秒后开始
    let mut pos: u64 = 0;
    let mut bytes: Vec<u8> = Vec::with_capacity(buf_frames as usize * 8);
    client.start_stream()?;
    while !stop.load(Ordering::Relaxed) {
        ev.wait_for_event(2000).map_err(|_| "播放事件超时")?;
        let avail = client.get_available_space_in_frames()? as u64;
        if avail == 0 {
            continue;
        }
        bytes.clear();
        for i in 0..avail {
            let p = pos + i;
            let v: f32 = if p >= next_click && p < next_click + click_len {
                // 1 kHz 方波
                if ((p - next_click) / 24).is_multiple_of(2) { 0.8 } else { -0.8 }
            } else {
                0.0
            };
            bytes.extend_from_slice(&v.to_le_bytes());
            bytes.extend_from_slice(&v.to_le_bytes());
            if p == next_click {
                // 设备时钟：(已播放位置, 对应的 QPC)。帧 p 送进设备的时刻 = qpc + (p - 已播放帧数)/采样率
                let (dpos, qpc) = clock.get_position()?;
                let played_s = dpos as f64 / freq;
                let due = qpc as f64 + (p as f64 / RATE as f64 - played_s) * 1e7;
                let _ = due_tx.send(due as i64);
            }
            if p + 1 == next_click + click_len {
                next_click += interval;
            }
        }
        render.write_to_device(avail as usize, &bytes, None)?;
        pos += avail;
    }
    client.stop_stream()?;
    Ok(())
}

pub fn run(a: &Args) -> Res<()> {
    let render_name = a.str("render", "CABLE Input");
    let capture_name = a.str("capture", "CABLE Output");
    let mode = Mode::parse(&a.str("mode", "shared"))?;
    let count = a.usize("count", 20);

    mmcss_pro_audio();
    let cap_dev = find_device(Direction::Capture, &capture_name)?;
    let st = open_stream(&cap_dev, Direction::Capture, mode, false)?;
    let capture = st.client.get_audiocaptureclient()?;
    println!("录音：{}，{}", cap_dev.get_friendlyname().unwrap_or_default(), st.desc);

    let (due_tx, due_rx) = mpsc::channel::<i64>();
    let (info_tx, info_rx) = mpsc::channel::<String>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let rt = std::thread::spawn(move || {
        if let Err(e) = render_thread(render_name, due_tx, stop2, info_tx) {
            eprintln!("播放线程出错：{e}");
        }
    });
    if let Ok(s) = info_rx.recv_timeout(Duration::from_secs(3)) {
        println!("{s}");
    }
    println!("每 {INTERVAL_MS} ms 播放一个 {CLICK_MS} ms 的「滴」，共测 {count} 次\n");

    let mut raw = vec![0u8; (st.buffer_frames as usize + 64) * st.conv.bytes_per_frame()];
    let mut samples: Vec<f32> = Vec::new();
    let exclusive = mode == Mode::Exclusive;
    let mut quiet_frames: u64 = 0;
    let mut dev_lat = Vec::new();
    let mut got_lat = Vec::new();

    st.client.start_stream()?;
    while dev_lat.len() < count {
        st.wait()?;
        loop {
            if !exclusive && capture.get_next_packet_size()? == Some(0) {
                break;
            }
            let (n, info) = capture.read_from_device(&mut raw)?;
            if n == 0 {
                break;
            }
            let t_read = qpc_100ns();
            samples.clear();
            if info.flags.silent {
                samples.resize(n as usize * 2, 0.0);
            } else {
                st.conv.convert(&raw, n as usize, &mut samples);
            }
            for k in 0..n as usize {
                let loud = samples[2 * k].abs() > 0.2;
                if loud && quiet_frames > (RATE / 10) as u64 {
                    // 检测到一个新的「滴」
                    if let Ok(due) = due_rx.try_recv() {
                        let captured = info.timestamp as f64 + k as f64 / RATE as f64 * 1e7;
                        let d = (captured - due as f64) / 1e4;
                        let g = (t_read - due) as f64 / 1e4;
                        dev_lat.push(d);
                        got_lat.push(g);
                        println!("#{:<3} 设备时间戳延迟 {:6.2} ms    程序实际拿到 {:6.2} ms", dev_lat.len(), d, g);
                    }
                }
                quiet_frames = if loud { 0 } else { quiet_frames + 1 };
            }
            if exclusive {
                break;
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = rt.join();

    let summary = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let avg = v.iter().sum::<f64>() / v.len() as f64;
        format!("最小 {:.2}  平均 {:.2}  最大 {:.2} ms", v[0], avg, v[v.len() - 1])
    };
    println!("\n设备时间戳延迟（播放端送进设备 → 录音端录到）：{}", summary(&mut dev_lat));
    println!("程序实际拿到（播放端送进设备 → 采集程序拿到数据）：{}", summary(&mut got_lat));
    println!("\n说明：真实应用还要加上它自己的播放缓冲；Aurio 端到端延迟里 Windows 这一段 ≈「程序实际拿到」。");
    Ok(())
}
