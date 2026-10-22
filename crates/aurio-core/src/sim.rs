//! 离线仿真：在虚拟时间里跑「发送端 → 网络 → 接收端播放」，验证抖动缓冲和漂移补偿。
//! 不需要声卡和网络，几秒钟能跑完几小时的场景。

use crate::playout::{Playout, PlayoutConfig};
use crate::proto::{stream, Packetizer, FORMAT_48K_S24_STEREO};
use crate::stats::RxStats;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub struct SimConfig {
    pub seconds: f64,
    /// 发送端时钟相对接收端快多少（ppm，可为负）
    pub sender_ppm: f64,
    /// 发送端每批发出的时长（模拟 WASAPI 采集周期）
    pub period_ms: f64,
    /// 每批额外的随机延迟上限（毫秒）
    pub jitter_ms: f64,
    /// 随机丢包率（0..1）
    pub loss: f64,
    pub out_rate: f64,
    /// 接收端设备每次回调的帧数
    pub out_buffer: usize,
    pub frames_per_packet: usize,
    /// 统计时忽略开头这么多秒（收敛期）
    pub warmup_s: f64,
    pub playout: PlayoutConfig,
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            seconds: 600.0,
            sender_ppm: 80.0,
            period_ms: 10.0,
            jitter_ms: 1.0,
            loss: 0.0,
            out_rate: 48000.0,
            out_buffer: 128,
            frames_per_packet: 120,
            warmup_s: 60.0,
            playout: PlayoutConfig::default(),
            seed: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SimReport {
    pub underruns: u64,
    pub underruns_after_warmup: u64,
    pub skips: u64,
    pub final_ppm: f64,
    /// 收敛后（warmup 之后）的平均修正量，应接近 sender_ppm
    pub mean_ppm: f64,
    pub mean_fill_ms: f64,
    pub min_fill_ms: f64,
    pub max_fill_ms: f64,
    pub target_ms: f64,
    pub lost_frames: u64,
    /// 估算的接收端缓冲延迟：平均水位 + 重采样延迟 + 一个设备缓冲
    pub est_rx_latency_ms: f64,
}

struct Rng(u64);
impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub fn run(cfg: SimConfig) -> SimReport {
    let fmt = FORMAT_48K_S24_STEREO;
    let in_rate = fmt.rate as f64;
    let ch = fmt.channels as usize;
    let stats = Arc::new(RxStats::default());
    let (prod, cons) = rtrb::RingBuffer::new(fmt.rate as usize * ch);
    let mut depack = crate::receiver::Depacketizer::new(fmt, stream::PLAYBACK, prod, stats.clone());
    let mut playout = Playout::new(cons, ch, in_rate, ch, cfg.out_rate, cfg.playout, stats.clone());
    let mut pk = Packetizer::new(fmt, stream::PLAYBACK, cfg.frames_per_packet);
    let mut rng = Rng(cfg.seed.max(1) * 0x9E37_79B9_7F4A_7C15);

    let period_frames = (cfg.period_ms * in_rate / 1000.0).round() as usize;
    let sender_rate = in_rate * (1.0 + cfg.sender_ppm * 1e-6);
    let mut burst = vec![0f32; period_frames * ch];
    let mut phase = 0f64;
    let mut out = vec![0f32; cfg.out_buffer * ch];
    let mut pkts: Vec<Vec<u8>> = Vec::new();

    let mut k: u64 = 0; // 批号
    let mut m: u64 = 0; // 回调号
    let mut last_arrival = 0f64;
    let mut next_arrival = 0f64;
    let mut sum_fill = 0f64;
    let mut sum_ppm = 0f64;
    let mut n_fill = 0u64;
    let mut min_fill = f64::INFINITY;
    let mut max_fill = 0f64;
    let mut underruns_at_warmup = None;
    // 设置环境变量 AURIO_SIM_TRACE=1 时每秒打印一行状态
    let trace = std::env::var_os("AURIO_SIM_TRACE").is_some();

    loop {
        let t_render = m as f64 * cfg.out_buffer as f64 / cfg.out_rate;
        if t_render > cfg.seconds {
            break;
        }
        if next_arrival <= t_render {
            // 生成一批数据（1 kHz 正弦），打包，按丢包率投递
            for f in 0..period_frames {
                let v = (phase * std::f64::consts::TAU).sin() as f32 * 0.25;
                phase = (phase + 1000.0 / in_rate).fract();
                burst[f * ch] = v;
                burst[f * ch + 1] = v;
            }
            pkts.clear();
            pk.push(&burst, |b| pkts.push(b.to_vec()));
            let t_us = (next_arrival * 1e6) as u64;
            for p in &pkts {
                if cfg.loss > 0.0 && rng.next_f64() < cfg.loss {
                    continue;
                }
                depack.handle(p, t_us);
            }
            k += 1;
            let t_send = k as f64 * period_frames as f64 / sender_rate;
            let a = t_send + rng.next_f64() * cfg.jitter_ms / 1000.0;
            last_arrival = a.max(last_arrival);
            next_arrival = last_arrival;
        } else {
            playout.render(&mut out, cfg.out_buffer);
            m += 1;
            if trace && m.is_multiple_of(cfg.out_rate as u64 / cfg.out_buffer as u64) {
                eprintln!(
                    "t={t_render:8.1} 水位={:6.2} 最低={:6.2} 目标={:6.2} ppm={:+7.1} 状态={}",
                    stats.fill_frames.load() / 48.0,
                    stats.window_min_frames.load() / 48.0,
                    stats.target_frames.load() / 48.0,
                    stats.ppm.load(),
                    stats.state.load(Relaxed)
                );
            }
            if t_render >= cfg.warmup_s {
                if underruns_at_warmup.is_none() {
                    underruns_at_warmup = Some(stats.underruns.load(Relaxed));
                }
                let f = stats.fill_frames.load() as f64;
                sum_fill += f;
                sum_ppm += stats.ppm.load() as f64;
                n_fill += 1;
                min_fill = min_fill.min(f);
                max_fill = max_fill.max(f);
            }
        }
    }

    let ms = |f: f64| f * 1000.0 / in_rate;
    let underruns = stats.underruns.load(Relaxed);
    let mean_fill_ms = if n_fill > 0 { ms(sum_fill / n_fill as f64) } else { 0.0 };
    SimReport {
        underruns,
        underruns_after_warmup: underruns - underruns_at_warmup.unwrap_or(underruns),
        skips: stats.skips.load(Relaxed),
        final_ppm: stats.ppm.load() as f64,
        mean_ppm: if n_fill > 0 { sum_ppm / n_fill as f64 } else { 0.0 },
        mean_fill_ms,
        min_fill_ms: if min_fill.is_finite() { ms(min_fill) } else { 0.0 },
        max_fill_ms: ms(max_fill),
        target_ms: ms(stats.target_frames.load() as f64),
        lost_frames: stats.lost_frames.load(Relaxed),
        est_rx_latency_ms: mean_fill_ms
            + playout.resampler_latency_ms()
            + cfg.out_buffer as f64 * 1000.0 / cfg.out_rate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast(mut c: SimConfig) -> SimConfig {
        c.playout.half_taps = 4; // 控制环路与抽头数无关，测试里用短滤波器提速
        c
    }

    #[test]
    fn converges_to_sender_drift() {
        for ppm in [-150.0, 0.0, 90.0] {
            let r = run(fast(SimConfig { seconds: 300.0, sender_ppm: ppm, ..Default::default() }));
            assert_eq!(r.underruns_after_warmup, 0, "ppm={ppm} {r:?}");
            assert!((r.mean_ppm - ppm).abs() < 3.0, "ppm={ppm} {r:?}");
        }
    }

    #[test]
    fn survives_loss_and_jitter() {
        let r = run(fast(SimConfig {
            seconds: 300.0,
            sender_ppm: -60.0,
            jitter_ms: 3.0,
            loss: 0.001,
            ..Default::default()
        }));
        assert_eq!(r.underruns_after_warmup, 0, "{r:?}");
        assert!(r.lost_frames > 0);
    }
}
