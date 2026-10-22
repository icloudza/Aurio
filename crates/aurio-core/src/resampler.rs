//! 可变比例重采样器：Kaiser 窗 sinc 分数插值。
//!
//! 每个输出帧按 `step`（每输出一帧前进多少输入帧）推进读位置，`step` 可以每次调用都变，
//! 所以漂移补偿只需要改 step。系数表在构造时一次算好，`process` 里不分配内存、不加锁。
//!
//! 引入的延迟约为 `half` 个输入帧（默认 32 帧，48 kHz 下约 0.67 ms）。

const PHASES: usize = 512;
pub const MAX_HALF: usize = 64;

pub struct FracResampler {
    ch: usize,
    half: usize,
    table: Vec<f32>,
    buf: Vec<f32>,
    cap: usize,
    len: usize,
    /// 当前插值点在 buf 里的位置（帧，含小数）。
    pos: f64,
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..60 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

impl FracResampler {
    /// - `half`：单侧抽头数，总抽头 2*half。越大音质越好、延迟越大。
    /// - `cutoff`：截止频率，相对输入奈奎斯特频率（0..1）。降采样时要乘上 out/in。
    /// - `cap_frames`：内部缓冲容量，需大于单次 `process` 所需输入加 2*half。
    pub fn new(ch: usize, half: usize, cutoff: f64, cap_frames: usize) -> Self {
        assert!(ch >= 1);
        let half = half.clamp(2, MAX_HALF);
        let taps = 2 * half;
        let beta = 8.0;
        let i0_beta = bessel_i0(beta);
        let mut table = vec![0f32; (PHASES + 1) * taps];
        let mut row = vec![0f64; taps];
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let mut sum = 0.0;
            for (j, r) in row.iter_mut().enumerate() {
                // 抽头 j 对应输入 i + k，k ∈ [-(half-1), half]
                let k = j as f64 - (half as f64 - 1.0);
                let t = k - frac;
                let x = t / half as f64;
                let w = if x.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(beta * (1.0 - x * x).sqrt()) / i0_beta
                };
                *r = cutoff * sinc(cutoff * t) * w;
                sum += *r;
            }
            for (j, r) in row.iter().enumerate() {
                table[p * taps + j] = (r / sum) as f32;
            }
        }
        let cap = cap_frames.max(4 * half + 64);
        let mut rs = FracResampler {
            ch,
            half,
            table,
            buf: vec![0.0; cap * ch],
            cap,
            len: 0,
            pos: 0.0,
        };
        rs.reset();
        rs
    }

    pub fn channels(&self) -> usize {
        self.ch
    }

    pub fn half(&self) -> usize {
        self.half
    }

    /// 清空历史，用静音填好左侧 half-1 帧。
    pub fn reset(&mut self) {
        let h = self.half - 1;
        self.buf[..h * self.ch].fill(0.0);
        self.len = h;
        self.pos = h as f64;
    }

    /// 已缓冲、还没被读位置越过的输入帧数（用于计算总水位）。
    pub fn buffered_ahead(&self) -> f64 {
        (self.len as f64 - self.pos - self.half as f64).max(0.0)
    }

    /// 要产生 `out_frames` 个输出帧，还需要再喂多少输入帧。
    pub fn input_needed(&self, out_frames: usize, step: f64) -> usize {
        if out_frames == 0 {
            return 0;
        }
        let last = self.pos + (out_frames - 1) as f64 * step;
        let need_len = last.floor() as usize + self.half + 1;
        need_len.saturating_sub(self.len)
    }

    fn compact(&mut self) {
        let i = self.pos.floor() as usize;
        let keep_from = (i + 1).saturating_sub(self.half);
        if keep_from > 0 {
            self.buf.copy_within(keep_from * self.ch..self.len * self.ch, 0);
            self.len -= keep_from;
            self.pos -= keep_from as f64;
        }
    }

    /// 剩余可写入的帧数（会先压缩缓冲）。
    pub fn free_frames(&mut self) -> usize {
        self.compact();
        self.cap - self.len
    }

    /// 追加交错输入。返回实际写入的帧数（超出容量的部分被丢弃）。
    pub fn push_input(&mut self, samples: &[f32]) -> usize {
        let frames = samples.len() / self.ch;
        if self.len + frames > self.cap {
            self.compact();
        }
        let n = frames.min(self.cap - self.len);
        let dst = self.len * self.ch;
        self.buf[dst..dst + n * self.ch].copy_from_slice(&samples[..n * self.ch]);
        self.len += n;
        n
    }

    /// 产生最多 `frames` 个交错输出帧，输入不够就提前停下。返回实际产生的帧数。
    pub fn process(&mut self, out: &mut [f32], frames: usize, step: f64) -> usize {
        let taps = 2 * self.half;
        let ch = self.ch;
        let mut h = [0f32; 2 * MAX_HALF];
        let mut produced = 0;
        while produced < frames {
            let i = self.pos.floor() as usize;
            if i + self.half >= self.len {
                break;
            }
            let pf = (self.pos - i as f64) * PHASES as f64;
            let p = (pf as usize).min(PHASES - 1);
            let a = (pf - p as f64) as f32;
            let r0 = &self.table[p * taps..(p + 1) * taps];
            let r1 = &self.table[(p + 1) * taps..(p + 2) * taps];
            for j in 0..taps {
                h[j] = r0[j] + a * (r1[j] - r0[j]);
            }
            let base = (i + 1 - self.half) * ch;
            let o = produced * ch;
            for c in 0..ch {
                let mut acc = 0f32;
                let mut idx = base + c;
                for &hj in &h[..taps] {
                    acc += self.buf[idx] * hj;
                    idx += ch;
                }
                out[o + c] = acc;
            }
            produced += 1;
            self.pos += step;
        }
        produced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, freq: f64, rate: f64) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin() as f32)
            .collect()
    }

    #[test]
    fn unity_step_is_pure_delay() {
        let mut rs = FracResampler::new(1, 32, 0.92, 4096);
        let x = sine(2000, 1000.0, 48000.0);
        rs.push_input(&x);
        let mut y = vec![0f32; 2000];
        let n = rs.process(&mut y, 2000, 1.0);
        assert_eq!(n, 2000 - 32);
        // 输出第 k 帧 ≈ 输入第 k 帧（左侧 half-1 帧静音历史抵消了 half-1 的位置偏移）
        for k in 100..n {
            assert!((y[k] - x[k]).abs() < 1e-3, "k={k} {} vs {}", y[k], x[k]);
        }
    }

    #[test]
    fn fractional_step_tracks_sine() {
        // 以 1.0001 的比例读取，检查输出仍是干净的正弦（与理想值比较）
        let rate = 48000.0;
        let f = 3000.0;
        let step = 1.0001;
        let mut rs = FracResampler::new(2, 32, 0.92, 8192);
        let mono = sine(6000, f, rate);
        let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, -s]).collect();
        rs.push_input(&stereo);
        let mut y = vec![0f32; 2 * 5000];
        let n = rs.process(&mut y, 5000, step);
        assert!(n > 4000);
        let mut max_err = 0f64;
        for k in 200..n {
            let t = k as f64 * step; // 输出 k 对应输入位置
            let ideal = (2.0 * std::f64::consts::PI * f * t / rate).sin();
            max_err = max_err.max((y[2 * k] as f64 - ideal).abs());
            assert!((y[2 * k] + y[2 * k + 1]).abs() < 1e-6);
        }
        assert!(max_err < 2e-3, "max_err={max_err}");
    }

    #[test]
    fn input_needed_is_exact() {
        let mut rs = FracResampler::new(2, 16, 0.9, 4096);
        let need = rs.input_needed(128, 1.0002);
        rs.push_input(&vec![0.1; need * 2]);
        let mut out = vec![0f32; 256];
        assert_eq!(rs.process(&mut out, 128, 1.0002), 128);
        // 再要 128 帧必须再喂
        assert!(rs.input_needed(128, 1.0002) > 0);
    }
}
