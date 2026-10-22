//! 离线仿真抖动缓冲与漂移补偿。
//! 例：aurio-sim --hours 24 --ppm 80 --period-ms 10 --jitter-ms 2

use aurio_core::sim::{run, SimConfig};
use aurio_tools::Args;
use std::time::Instant;

fn main() {
    let a = Args::parse();
    if a.has("help") {
        println!(
            "用法：aurio-sim [选项]\n\
             --seconds N | --hours N   仿真时长（默认 600 秒）\n\
             --ppm N                   发送端时钟偏差（默认 80）\n\
             --period-ms N             发送端每批时长，模拟采集周期（默认 10）\n\
             --jitter-ms N             网络/调度抖动上限（默认 1）\n\
             --loss N                  丢包率 0..1（默认 0）\n\
             --buffer N                接收端设备缓冲帧数（默认 128）\n\
             --target-ms N             初始目标最低水位（默认 2）\n\
             --taps N                  重采样单侧抽头数（默认 8，仿真用小值提速）"
        );
        return;
    }
    let d = SimConfig::default();
    let mut c = SimConfig {
        seconds: if a.has("hours") { a.f64("hours", 1.0) * 3600.0 } else { a.f64("seconds", d.seconds) },
        sender_ppm: a.f64("ppm", d.sender_ppm),
        period_ms: a.f64("period-ms", d.period_ms),
        jitter_ms: a.f64("jitter-ms", d.jitter_ms),
        loss: a.f64("loss", d.loss),
        out_buffer: a.usize("buffer", d.out_buffer),
        seed: a.usize("seed", 1) as u64,
        ..d
    };
    c.playout.half_taps = a.usize("taps", 8);
    c.playout.drift.target_ms = a.f64("target-ms", c.playout.drift.target_ms);
    c.warmup_s = c.warmup_s.min(c.seconds / 4.0);

    println!(
        "仿真 {:.1} 小时：发送端偏差 {:+} ppm，每批 {} ms，抖动 ≤ {} ms，丢包率 {}，设备缓冲 {} 帧",
        c.seconds / 3600.0, c.sender_ppm, c.period_ms, c.jitter_ms, c.loss, c.out_buffer
    );
    let t = Instant::now();
    let r = run(c);
    println!("耗时 {:.1} 秒\n", t.elapsed().as_secs_f64());
    println!("欠载次数        {}（收敛后 {}）", r.underruns, r.underruns_after_warmup);
    println!("丢帧次数        {}", r.skips);
    println!("修正量          平均 {:+.1} ppm，最后 {:+.1} ppm（应接近 {:+}）", r.mean_ppm, r.final_ppm, c.sender_ppm);
    println!("水位            平均 {:.2} ms，最低 {:.2} ms，最高 {:.2} ms", r.mean_fill_ms, r.min_fill_ms, r.max_fill_ms);
    println!("目标最低水位    {:.2} ms", r.target_ms);
    println!("丢包补零帧数    {}", r.lost_frames);
    println!("接收端缓冲延迟  约 {:.2} ms（平均水位 + 重采样 + 一个设备缓冲）", r.est_rx_latency_ms);
}
