//! 测试发送端：生成测试信号或读取 WAV，经 UDP 发给接收端。
//! 可以模拟 Windows 的采集周期（成批发送）、时钟偏差和调度抖动。

use aurio_core::proto::{self, format_by_id, stream};
use aurio_core::resampler::FracResampler;
use aurio_core::sender::UdpSender;
use aurio_tools::{die, Args};
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

enum Signal {
    Sine { freq: f64, phase: f64 },
    Click { counter: usize },
    Noise { state: u64 },
    Wav { data: Vec<f32>, pos: usize },
}

fn load_wav(path: &str, out_rate: f64) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).unwrap_or_else(|e| die(&format!("打不开 {path}：{e}")));
    let spec = r.spec();
    let ch = spec.channels as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.unwrap_or(0) as f32 * scale).collect()
        }
    };
    // 转成立体声
    let stereo: Vec<f32> = raw
        .chunks_exact(ch)
        .flat_map(|f| if ch == 1 { [f[0], f[0]] } else { [f[0], f[1]] })
        .collect();
    if spec.sample_rate as f64 == out_rate {
        return stereo;
    }
    // 采样率不同就用核心库的重采样器转一下
    let step = spec.sample_rate as f64 / out_rate;
    let cutoff = 0.92 * (out_rate / spec.sample_rate as f64).min(1.0);
    let in_frames = stereo.len() / 2;
    let mut rs = FracResampler::new(2, 32, cutoff, in_frames + 1024);
    rs.push_input(&stereo);
    rs.push_input(&vec![0.0; 128]);
    let out_frames = (in_frames as f64 / step) as usize;
    let mut out = vec![0f32; out_frames * 2];
    let n = rs.process(&mut out, out_frames, step);
    out.truncate(n * 2);
    eprintln!("WAV 采样率 {} Hz，已重采样到 {} Hz", spec.sample_rate, out_rate);
    out
}

impl Signal {
    fn fill(&mut self, buf: &mut [f32], ch: usize, rate: f64, level: f32) {
        for f in buf.chunks_exact_mut(ch) {
            let (l, r) = match self {
                Signal::Sine { freq, phase } => {
                    let v = (*phase * std::f64::consts::TAU).sin() as f32 * level;
                    *phase = (*phase + *freq / rate).fract();
                    (v, v)
                }
                Signal::Click { counter } => {
                    // 每秒一个 1 ms、2 kHz 的短音，用来测延迟
                    let period = rate as usize;
                    let len = (rate / 1000.0) as usize;
                    let v = if *counter < len {
                        (*counter as f64 / rate * 2000.0 * std::f64::consts::TAU).sin() as f32 * level
                    } else {
                        0.0
                    };
                    *counter = (*counter + 1) % period;
                    (v, v)
                }
                Signal::Noise { state } => {
                    *state ^= *state << 13;
                    *state ^= *state >> 7;
                    *state ^= *state << 17;
                    let v = ((*state >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * level;
                    (v, v)
                }
                Signal::Wav { data, pos } => {
                    if data.is_empty() {
                        (0.0, 0.0)
                    } else {
                        let v = (data[*pos] * level, data[*pos + 1] * level);
                        *pos = (*pos + 2) % data.len();
                        v
                    }
                }
            };
            f[0] = l;
            if ch > 1 {
                f[1] = r;
            }
        }
    }
}

fn main() {
    let a = Args::parse();
    if a.has("help") {
        println!(
            "用法：aurio-send --to 主机:端口 [选项]\n\
             --signal sine|click|noise|wav   测试信号（默认 sine）\n\
             --file 路径                      --signal wav 时读取的文件\n\
             --freq N                         正弦频率（默认 440）\n\
             --level-db N                     电平 dBFS（默认 -20）\n\
             --period-ms N                    每批时长，模拟采集周期（默认 10）\n\
             --fpp N                          每包帧数（默认 120）\n\
             --ppm N                          模拟发送端时钟偏差（默认 0）\n\
             --jitter-ms N                    每批随机延迟上限（默认 0）\n\
             --format N                       格式编号（默认 1 = 48k/24bit/立体声）\n\
             --stream N                       流编号（默认 0 = 播放方向）\n\
             --seconds N                      发送时长，0 表示一直发（默认 0）\n\
             --mic-request                    模拟 Windows：发心跳请求麦克风，并统计收到的麦克风数据"
        );
        return;
    }
    let to = a.str("to", &format!("127.0.0.1:{}", proto::DEFAULT_PORT));
    let dest: SocketAddr = to
        .to_socket_addrs()
        .ok()
        .and_then(|mut i| i.next())
        .unwrap_or_else(|| die(&format!("无法解析地址 {to}")));
    let fmt = format_by_id(a.usize("format", 1) as u16).unwrap_or_else(|| die("未知的格式编号"));
    let rate = fmt.rate as f64;
    let ch = fmt.channels as usize;
    let level = 10f32.powf(a.f64("level-db", -20.0) as f32 / 20.0);
    let period_ms = a.f64("period-ms", 10.0);
    let ppm = a.f64("ppm", 0.0);
    let jitter_ms = a.f64("jitter-ms", 0.0);
    let seconds = a.f64("seconds", 0.0);
    let mut sig = match a.str("signal", "sine").as_str() {
        "sine" => Signal::Sine { freq: a.f64("freq", 440.0), phase: 0.0 },
        "click" => Signal::Click { counter: 0 },
        "noise" => Signal::Noise { state: 0x1234_5678_9abc_def1 },
        "wav" => Signal::Wav { data: load_wav(&a.str("file", ""), rate), pos: 0 },
        s => die(&format!("未知信号 {s}")),
    };

    let mut tx = UdpSender::new(dest, fmt, a.usize("stream", stream::PLAYBACK as usize) as u8, a.usize("fpp", 120))
        .unwrap_or_else(|e| die(&format!("创建 socket 失败：{e}")));
    let period_frames = (period_ms * rate / 1000.0).round().max(1.0) as usize;
    let mut buf = vec![0f32; period_frames * ch];
    // 发送端「时钟」比真实时间快 ppm：每批的间隔相应缩短
    let period = Duration::from_secs_f64(period_frames as f64 / (rate * (1.0 + ppm * 1e-6)));
    let mut rng: u64 = 0x2545_F491_4F6C_DD1D;

    println!(
        "发送到 {dest}：格式 {} Hz/{} 声道，每包 {} 帧，每批 {} 帧（{:.2} ms），时钟偏差 {:+} ppm，抖动 ≤ {} ms",
        fmt.rate, ch, tx.packetizer().frames_per_packet(), period_frames, period_ms, ppm, jitter_ms
    );
    if a.has("mic-request") {
        start_mic_probe(&tx);
    }
    aurio_core::rt::promote_current_thread();
    let start = Instant::now();
    let mut k: u32 = 0;
    let mut last_report = Instant::now();
    let mut max_late = Duration::ZERO;
    loop {
        sig.fill(&mut buf, ch, rate, level);
        tx.send(&buf);
        k += 1;
        let mut deadline = start + period * k;
        if jitter_ms > 0.0 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let j = (rng >> 11) as f64 / (1u64 << 53) as f64 * jitter_ms / 1000.0;
            deadline += Duration::from_secs_f64(j);
        }
        // 先睡到截止前 1.5 ms，再自旋等待：macOS 会合并定时器，单纯 sleep 可能睡过头好几毫秒
        let now = Instant::now();
        let spin = Duration::from_micros(1500);
        if deadline > now + spin {
            std::thread::sleep(deadline - now - spin);
        }
        while Instant::now() < deadline {
            std::hint::spin_loop();
        }
        max_late = max_late.max(Instant::now().saturating_duration_since(deadline));
        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            let frames = tx.packetizer().sample_index();
            println!(
                "已发送 {:>7} 包，{:>9} 帧，错误 {}，最大发送迟到 {:.2} ms",
                tx.sent_packets, frames, tx.send_errors, max_late.as_secs_f64() * 1000.0
            );
            max_late = Duration::ZERO;
        }
        if seconds > 0.0 && start.elapsed().as_secs_f64() >= seconds {
            break;
        }
    }
}

/// 模拟 Windows 端的麦克风方向：每 0.5 秒发一个请求麦克风的心跳，
/// 在同一个 socket 上接收 Mac 发回来的麦克风包，每秒打印收到多少、电平多少。
fn start_mic_probe(tx: &UdpSender) {
    use aurio_core::proto::{flags, heartbeat, FORMAT_48K_S24_MONO, HEADER_LEN};
    use aurio_core::receiver::Depacketizer;
    use aurio_core::stats::RxStats;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::Arc;

    let dest = tx.dest();
    let hb_sock = tx.socket().try_clone().expect("复制 socket");
    std::thread::spawn(move || {
        let mut hb = [0u8; HEADER_LEN];
        for seq in 0u32.. {
            heartbeat(stream::PLAYBACK, flags::MIC_REQUEST, seq, &mut hb);
            let _ = hb_sock.send_to(&hb, dest);
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    let rx_sock = tx.socket().try_clone().expect("复制 socket");
    rx_sock.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    std::thread::spawn(move || {
        let stats = Arc::new(RxStats::default());
        let (prod, mut cons) = rtrb::RingBuffer::new(48000);
        let mut d = Depacketizer::new(FORMAT_48K_S24_MONO, stream::MIC, prod, stats.clone());
        let start = Instant::now();
        let mut buf = [0u8; 2048];
        let mut last = Instant::now();
        let mut peak = 0f32;
        let mut frames = 0usize;
        loop {
            if let Ok((n, _)) = rx_sock.recv_from(&mut buf) {
                d.handle(&buf[..n], start.elapsed().as_micros() as u64);
            }
            while let Ok(v) = cons.pop() {
                peak = peak.max(v.abs());
                frames += 1;
            }
            if last.elapsed() >= Duration::from_secs(1) {
                last = Instant::now();
                println!(
                    "  [麦克风] 收到 {:>5} 帧/秒，峰值 {:.3}，累计 {} 包，丢包补零 {} 帧，坏包 {}，最大间隔 {:.2} ms",
                    frames,
                    peak,
                    stats.packets.load(Relaxed),
                    stats.lost_frames.load(Relaxed),
                    stats.bad_packets.load(Relaxed),
                    stats.max_gap_us.load(Relaxed) as f64 / 1000.0
                );
                peak = 0.0;
                frames = 0;
            }
        }
    });
}
