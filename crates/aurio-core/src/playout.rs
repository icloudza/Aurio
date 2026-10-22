//! 渲染线程这一半：从环形缓冲取数据，经重采样（含漂移补偿）输出到本地设备。
//!
//! `render` 在音频设备的实时回调里调用：不分配内存、不加锁、不做系统调用。

use crate::drift::{DriftConfig, DriftController};
use crate::resampler::FracResampler;
use crate::stats::{state, AtomicF32, RxStats};
use rtrb::Consumer;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

/// 每次内部处理的最大输出帧数。回调要求的帧数更多时分块处理。
const BLOCK: usize = 256;

#[derive(Clone, Copy, Debug)]
pub struct PlayoutConfig {
    pub drift: DriftConfig,
    /// 重采样器单侧抽头数
    pub half_taps: usize,
    /// 淡入淡出时长（毫秒）
    pub fade_ms: f64,
}

impl Default for PlayoutConfig {
    fn default() -> Self {
        PlayoutConfig { drift: DriftConfig::default(), half_taps: 32, fade_ms: 1.0 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Buffering,
    Playing,
}

pub struct Playout {
    cons: Consumer<f32>,
    in_ch: usize,
    out_ch: usize,
    in_rate: f64,
    nominal_step: f64,
    rs: FracResampler,
    ctl: DriftController,
    state: State,
    gain: f32,
    gain_target: f32,
    gain_step: f32,
    fade_frames: usize,
    pending_skip: usize,
    tmp: Vec<f32>,
    stats: Arc<RxStats>,
    volume: Arc<AtomicF32>,
}

impl Playout {
    pub fn new(
        cons: Consumer<f32>,
        in_ch: usize,
        in_rate: f64,
        out_ch: usize,
        out_rate: f64,
        cfg: PlayoutConfig,
        stats: Arc<RxStats>,
    ) -> Self {
        let ratio = out_rate / in_rate;
        // 降采样时截止频率跟着降，避免混叠
        let cutoff = 0.92 * ratio.min(1.0);
        let fade_frames = ((cfg.fade_ms * out_rate / 1000.0) as usize).max(1);
        let volume = Arc::new(AtomicF32::default());
        volume.store(1.0);
        stats.state.store(state::BUFFERING, Relaxed);
        Playout {
            cons,
            in_ch,
            out_ch,
            in_rate,
            nominal_step: in_rate / out_rate,
            rs: FracResampler::new(in_ch, cfg.half_taps, cutoff, 4 * BLOCK + 512),
            ctl: DriftController::new(cfg.drift, in_rate, out_rate),
            state: State::Buffering,
            gain: 0.0,
            gain_target: 0.0,
            gain_step: 1.0 / fade_frames as f32,
            fade_frames,
            pending_skip: 0,
            tmp: vec![0.0; BLOCK * in_ch],
            stats,
            volume,
        }
    }

    /// 音量（线性，0..1），可以从其他线程修改。
    pub fn volume_handle(&self) -> Arc<AtomicF32> {
        self.volume.clone()
    }

    pub fn stats(&self) -> &Arc<RxStats> {
        &self.stats
    }

    pub fn in_rate(&self) -> f64 {
        self.in_rate
    }

    /// 重采样器带来的固定延迟（毫秒）。
    pub fn resampler_latency_ms(&self) -> f64 {
        self.rs.half() as f64 * 1000.0 / self.in_rate
    }

    fn input_frames_available(&self) -> usize {
        self.cons.slots() / self.in_ch
    }

    fn fill(&self) -> f64 {
        self.input_frames_available() as f64 + self.rs.buffered_ahead()
    }

    /// 输出 `frames` 帧交错 f32 到 `out`（长度至少 frames × out_ch）。
    pub fn render(&mut self, out: &mut [f32], frames: usize) {
        let vol = self.volume.load();
        let burst = self.stats.burst_frames.load(Relaxed) as f64;
        let gap = self.stats.max_gap_us.load(Relaxed) as f64 * self.in_rate / 1e6;
        // 超过 100 ms 的间隔是断流（发送端重启、暂停），不算抖动
        if burst > 0.0 && gap > burst && gap - burst < 0.1 * self.in_rate {
            self.ctl.note_lateness(gap - burst);
        }
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(BLOCK);
            let o = &mut out[done * self.out_ch..(done + n) * self.out_ch];
            match self.state {
                State::Buffering => {
                    o.fill(0.0);
                    let burst = self.stats.burst_frames.load(Relaxed) as f64;
                    let threshold = self.ctl.target_frames() + burst;
                    if self.input_frames_available() as f64 >= threshold
                        && self.input_frames_available() > 0
                    {
                        self.start_playing();
                    }
                }
                State::Playing => self.render_block(o, n, vol),
            }
            done += n;
        }

        if self.state == State::Playing {
            let fill = self.fill();
            self.stats.fill_frames.store(fill as f32);
            if let Some(skip) = self.ctl.observe(fill, frames) {
                self.pending_skip = skip;
                self.gain_target = 0.0;
            }
            self.stats.window_min_frames.store(self.ctl.last_window_min() as f32);
        } else {
            self.stats.fill_frames.store(self.input_frames_available() as f32);
        }
        self.stats.target_frames.store(self.ctl.target_frames() as f32);
        self.stats.ppm.store(self.ctl.ppm() as f32);
        self.stats.rendered_frames.fetch_add(frames as u64, Relaxed);
    }

    fn start_playing(&mut self) {
        // 缓冲期间攒下的数据可能比需要的多（第一批到达后一直没消耗），
        // 现在还是静音，直接丢掉多余部分不会有任何痕迹
        let burst = self.stats.burst_frames.load(Relaxed) as f64;
        let want = (self.ctl.target_frames() + burst).ceil() as usize;
        let avail = self.input_frames_available();
        if avail > want {
            if let Ok(chunk) = self.cons.read_chunk((avail - want) * self.in_ch) {
                chunk.commit_all();
            }
        }
        self.state = State::Playing;
        self.rs.reset();
        self.gain = 0.0;
        self.gain_target = 1.0;
        self.ctl.restart();
        self.stats.state.store(state::PLAYING, Relaxed);
    }

    fn render_block(&mut self, o: &mut [f32], n: usize, vol: f32) {
        let ch = self.in_ch;
        let step = self.nominal_step * (1.0 + self.ctl.adj());
        let need = self.rs.input_needed(n, step);
        let take = need.min(self.input_frames_available()).min(self.rs.free_frames());
        if take > 0 {
            if let Ok(chunk) = self.cons.read_chunk(take * ch) {
                let (a, b) = chunk.as_slices();
                self.rs.push_input(a);
                self.rs.push_input(b);
                chunk.commit_all();
            }
        }
        let produced = self.rs.process(&mut self.tmp, n, step);

        // 渐变增益（淡入、为丢帧而淡出）
        for f in 0..produced {
            if self.gain != self.gain_target {
                if self.gain < self.gain_target {
                    self.gain = (self.gain + self.gain_step).min(self.gain_target);
                } else {
                    self.gain = (self.gain - self.gain_step).max(self.gain_target);
                }
            }
            let g = self.gain;
            for s in &mut self.tmp[f * ch..(f + 1) * ch] {
                *s *= g;
            }
        }

        if produced < n {
            // 欠载：把已产生部分的尾巴快速淡出，其余补静音，回到缓冲状态
            let fl = produced.min(self.fade_frames);
            for k in 0..fl {
                let g = 1.0 - (k + 1) as f32 / fl as f32;
                let f = produced - fl + k;
                for s in &mut self.tmp[f * ch..(f + 1) * ch] {
                    *s *= g;
                }
            }
            self.tmp[produced * ch..n * ch].fill(0.0);
            self.stats.underruns.fetch_add(1, Relaxed);
            self.ctl.on_underrun();
            self.state = State::Buffering;
            self.pending_skip = 0;
            self.stats.state.store(state::BUFFERING, Relaxed);
        }

        map_channels(&self.tmp[..n * ch], ch, o, self.out_ch, n, vol);

        // 淡出完成后执行丢帧
        if self.pending_skip > 0 && self.gain == 0.0 && self.state == State::Playing {
            let drop = self.pending_skip.min(self.input_frames_available());
            if let Ok(chunk) = self.cons.read_chunk(drop * ch) {
                chunk.commit_all();
            }
            self.rs.reset();
            self.pending_skip = 0;
            self.gain_target = 1.0;
            self.stats.skips.fetch_add(1, Relaxed);
        }
    }
}

/// 声道映射：同数直接拷贝；单声道输入复制到所有输出；
/// 多声道输入到单声道输出取平均；多于输入的输出声道补零。
fn map_channels(src: &[f32], in_ch: usize, dst: &mut [f32], out_ch: usize, frames: usize, vol: f32) {
    for f in 0..frames {
        let s = &src[f * in_ch..(f + 1) * in_ch];
        let d = &mut dst[f * out_ch..(f + 1) * out_ch];
        if in_ch == out_ch {
            for (dv, sv) in d.iter_mut().zip(s) {
                *dv = sv * vol;
            }
        } else if in_ch == 1 {
            d.fill(s[0] * vol);
        } else if out_ch == 1 {
            d[0] = s.iter().sum::<f32>() / in_ch as f32 * vol;
        } else {
            let m = in_ch.min(out_ch);
            for c in 0..out_ch {
                d[c] = if c < m { s[c] * vol } else { 0.0 };
            }
        }
    }
}
