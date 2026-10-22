//! 命令行版：启动引擎，每秒打印一次状态。

use super::common::*;
use super::discover::discover;
use super::engine::{Engine, EngineConfig};
use aurio_tools::Args;
use std::time::Duration;

pub fn run(a: &Args) -> Res<()> {
    let mut dest = a.str("to", "");
    if dest.is_empty() {
        println!("没指定 --to，正在局域网里找 Mac……");
        let found = discover(Duration::from_secs(3))?;
        let mac = found.first().ok_or("没找到 Mac。确认 Mac 上的 Aurio 已经打开，或者用 --to 指定 IP")?;
        println!("找到 {}（{}）", mac.name, mac.addr);
        dest = mac.addr.clone();
    }
    let loopback = a.str("loopback", "");
    let cfg = EngineConfig {
        dest,
        loopback: if loopback == "true" { String::new() } else { loopback },
        capture_device: a.str("device", ""),
        mode: a.str("mode", "shared"),
        fpp: a.usize("fpp", 120),
        mic_mode: a.str("mic", "auto"),
        mic_render: a.str("mic-device", "CABLE Input"),
        mic_detect: a.str("mic-detect", "CABLE Output"),
        mic_queue_ms: a.f64("mic-queue-ms", 5.0),
    };
    let engine = Engine::start(&cfg)?;
    let s = engine.status();
    println!("发送到 {}", s.dest);
    println!("播放来源：{}（{}）", s.playback.device, s.playback.method);
    println!(
        "麦克风：{}",
        if s.mic.available { format!("写入「{}」，{}", s.mic.device, s.mic.mode) } else { "不可用".into() }
    );
    println!();
    let mut shown_errors = 0;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let s = engine.status();
        for e in &s.errors[shown_errors..] {
            println!("错误：{e}");
        }
        shown_errors = s.errors.len();
        let p = &s.playback;
        let peer = if s.peer.responding {
            format!("Mac 已响应，往返 {:.1} ms", s.peer.rtt_ms)
        } else if s.peer.since_reply_s < 0.0 {
            "Mac 没有响应".to_string()
        } else {
            format!("Mac {:.0} 秒没响应", s.peer.since_reply_s)
        };
        println!(
            "播放 {:>3} 包/秒 电平 {:>5.1} dB 最大间隔 {:5.2} ms 错误 {} | {peer}",
            p.packets_per_s, p.peak_db, p.max_data_gap_ms, p.send_errors
        );
        if s.mic.available {
            let m = &s.mic;
            println!(
                "麦克风 {} {} | 水位 {:5.2}/{:5.2} ms 修正 {:+6.1} ppm | {:>3} 包/秒 电平 {:>5.1} dB | 欠载 {} | 队列 {:4.1} ms",
                if m.requested { "请求中" } else { "未请求" },
                m.state,
                m.fill_ms,
                m.target_ms,
                m.ppm,
                m.packets_per_s,
                m.peak_db,
                m.underruns,
                m.queue_ms
            );
        }
    }
}
