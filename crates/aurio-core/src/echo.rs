//! 回声分析：比较「播放出去的声音」（参照）和「麦克风录到的声音」，
//! 找出参照有没有经过 扬声器 → 空气 → 麦克风 漏回来，漏回来多少、晚了多久。
//!
//! 做法：两路都降到 8 kHz，在 0..max_delay 范围内算归一化互相关，取峰值。
//! 峰值处的相关系数 ρ 反映「录音里有多少是参照的回声」（ρ² 约等于回声能量占比），
//! 最小二乘增益 g 给出回声相对参照的强度。房间混响会把能量摊到多个延迟上，
//! 所以单个峰值是偏保守的估计，用来判断有没有回声、大概多强足够了。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 播放的声音太小，测不出来
    NoReference,
    /// 录音几乎是静音（麦克风没开或没数据）
    MicSilent,
    None,
    Slight,
    Obvious,
}

#[derive(Clone, Debug)]
pub struct EchoAnalysis {
    pub mic_rms_db: f32,
    pub mic_peak_db: f32,
    pub ref_rms_db: f32,
    /// 回声延迟（毫秒）
    pub delay_ms: f32,
    /// 峰值处的相关系数（0..1）
    pub correlation: f32,
    /// 回声相对于参照的强度（dB）
    pub echo_db: f32,
    /// 录音能量里回声大约占多少（0..1）
    pub echo_share: f32,
    pub verdict: Verdict,
}

fn db(x: f64) -> f32 {
    if x <= 1e-9 {
        -180.0
    } else {
        (20.0 * x.log10()) as f32
    }
}

fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

/// 降采样到约 8 kHz：块平均（顺带做了粗略的低通），再去掉直流。
fn decimate(x: &[f32], factor: usize) -> Vec<f64> {
    let mut out: Vec<f64> = x
        .chunks_exact(factor)
        .map(|c| c.iter().map(|&v| v as f64).sum::<f64>() / factor as f64)
        .collect();
    let mean = out.iter().sum::<f64>() / out.len().max(1) as f64;
    for v in &mut out {
        *v -= mean;
    }
    out
}

/// `mic`、`reference` 都是单声道、同一采样率、大致同时开始录制。
pub fn analyze(mic: &[f32], reference: &[f32], rate: f64, max_delay_s: f64) -> EchoAnalysis {
    let mic_rms_db = db(rms(mic));
    let ref_rms_db = db(rms(reference));
    let mic_peak_db = db(mic.iter().fold(0f32, |m, v| m.max(v.abs())) as f64);
    let mut a = EchoAnalysis {
        mic_rms_db,
        mic_peak_db,
        ref_rms_db,
        delay_ms: 0.0,
        correlation: 0.0,
        echo_db: -180.0,
        echo_share: 0.0,
        verdict: Verdict::None,
    };
    if ref_rms_db < -50.0 {
        a.verdict = Verdict::NoReference;
        return a;
    }
    if mic_rms_db < -65.0 {
        a.verdict = Verdict::MicSilent;
        return a;
    }

    let factor = ((rate / 8000.0).round() as usize).max(1);
    let low_rate = rate / factor as f64;
    let n = mic.len().min(reference.len());
    let m = decimate(&mic[..n], factor);
    let r = decimate(&reference[..n], factor);
    let n = m.len();
    let max_lag = ((max_delay_s * low_rate) as usize).min(n.saturating_sub(n / 4));

    // 前缀能量和，用来给每个延迟做归一化
    let mut pm = vec![0f64; n + 1];
    let mut pr = vec![0f64; n + 1];
    for i in 0..n {
        pm[i + 1] = pm[i] + m[i] * m[i];
        pr[i + 1] = pr[i] + r[i] * r[i];
    }

    let mut best = (0usize, 0f64, 0f64, 0f64); // (lag, ρ, 互相关, 参照能量)
    for lag in 0..=max_lag {
        let len = n - lag;
        let mut c = 0f64;
        for i in 0..len {
            c += m[i + lag] * r[i];
        }
        let em = pm[n] - pm[lag];
        let er = pr[len];
        if em <= 0.0 || er <= 0.0 {
            continue;
        }
        let rho = c / (em * er).sqrt();
        if rho.abs() > best.1.abs() {
            best = (lag, rho, c, er);
        }
    }

    let (lag, rho, c, er) = best;
    a.delay_ms = (lag as f64 * 1000.0 / low_rate) as f32;
    a.correlation = rho.abs() as f32;
    a.echo_share = (rho * rho) as f32;
    a.echo_db = db((c / er).abs());
    a.verdict = if a.correlation < 0.08 {
        Verdict::None
    } else if a.correlation < 0.25 {
        Verdict::Slight
    } else {
        Verdict::Obvious
    };
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn noise(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        }
    }

    fn signals(seconds: f64, delay_ms: f64, echo_gain: f32, noise_gain: f32) -> (Vec<f32>, Vec<f32>) {
        let rate = 48000.0;
        let n = (seconds * rate) as usize;
        let d = (delay_ms * rate / 1000.0) as usize;
        let mut rng = Rng(0x1234_5678_9abc_def1);
        // 参照：低通后的噪声，近似音乐的频谱倾斜
        let mut lp = 0f32;
        let reference: Vec<f32> = (0..n)
            .map(|_| {
                lp = 0.9 * lp + 0.1 * rng.noise();
                lp * 1.5
            })
            .collect();
        let mic: Vec<f32> = (0..n)
            .map(|i| {
                let echo = if i >= d { reference[i - d] * echo_gain } else { 0.0 };
                echo + rng.noise() * noise_gain
            })
            .collect();
        (mic, reference)
    }

    #[test]
    fn finds_echo_delay_and_level() {
        let (mic, reference) = signals(6.0, 150.0, 0.3, 0.02);
        let a = analyze(&mic, &reference, 48000.0, 1.0);
        assert_eq!(a.verdict, Verdict::Obvious, "{a:?}");
        assert!((a.delay_ms - 150.0).abs() < 2.0, "{a:?}");
        assert!((a.echo_db - 20.0 * 0.3f32.log10()).abs() < 1.5, "{a:?}");
    }

    #[test]
    fn no_echo_when_independent() {
        let (_, reference) = signals(6.0, 0.0, 0.0, 0.0);
        let (mic, _) = signals(6.0, 0.0, 0.0, 0.05);
        let a = analyze(&mic, &reference, 48000.0, 1.0);
        assert_eq!(a.verdict, Verdict::None, "{a:?}");
    }

    #[test]
    fn reports_silence() {
        let (mic, reference) = signals(3.0, 100.0, 0.3, 0.0);
        assert_eq!(analyze(&mic, &vec![0.0; reference.len()], 48000.0, 1.0).verdict, Verdict::NoReference);
        assert_eq!(analyze(&vec![0.0; mic.len()], &reference, 48000.0, 1.0).verdict, Verdict::MicSilent);
    }
}
