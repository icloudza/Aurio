//! 录音回放测试：录一段应用收到的麦克风声音（从 CABLE Output 录），同时录下 Windows 正在播放的
//! 声音作为参照，分析播放的声音有没有漏进麦克风（回声）。录完可以回放、存成 WAV。
//!
//! 录 CABLE Output 本身就会让「自动」模式检测到有应用在用麦克风，Mac 随即打开麦克风，
//! 所以开头先等一会儿（准备阶段），等麦克风数据到了再正式计时。

use super::common::*;
use aurio_core::echo::{analyze, EchoAnalysis, Verdict};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wasapi::*;

pub struct EchoTestOptions {
    pub seconds: f32,
    /// 应用录音用的设备（CABLE Output）
    pub mic_device: String,
    /// 回环采集的播放设备，空 = 系统默认播放设备
    pub reference: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct EchoReport {
    /// 一句话结论
    pub verdict: String,
    /// 补充说明
    pub detail: String,
    pub delay_ms: f32,
    pub echo_db: f32,
    pub echo_share_pct: f32,
    pub correlation: f32,
    pub mic_rms_db: f32,
    pub ref_rms_db: f32,
    pub wav_path: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct EchoProgress {
    /// 准备 / 录音 / 分析 / 完成 / 出错
    pub phase: String,
    pub elapsed_s: f32,
    pub total_s: f32,
    pub mic_db: f32,
    pub ref_db: f32,
    pub playing: bool,
    pub result: Option<EchoReport>,
    pub error: Option<String>,
}

pub struct EchoTest {
    progress: Arc<Mutex<EchoProgress>>,
    recording: Arc<Mutex<Vec<f32>>>,
    stop_play: Arc<AtomicBool>,
}

const PREROLL_MAX: Duration = Duration::from_secs(4);
const PREROLL_MIN: Duration = Duration::from_millis(800);

impl EchoTest {
    pub fn start(opts: EchoTestOptions) -> EchoTest {
        let progress = Arc::new(Mutex::new(EchoProgress {
            phase: "准备".into(),
            total_s: opts.seconds,
            mic_db: -90.0,
            ref_db: -90.0,
            ..Default::default()
        }));
        let recording = Arc::new(Mutex::new(Vec::new()));
        let (p, r) = (progress.clone(), recording.clone());
        std::thread::spawn(move || {
            let _ = initialize_mta();
            let res = record(&opts, &p).map(|(mic, reference)| {
                p.lock().unwrap().phase = "分析".into();
                let a = analyze(&mic, &reference, RATE as f64, 1.5);
                let wav = save_wav(&mic);
                *r.lock().unwrap() = mic;
                report(&a, wav)
            });
            let mut g = p.lock().unwrap();
            match res {
                Ok(rep) => {
                    g.phase = "完成".into();
                    g.result = Some(rep);
                }
                Err(e) => {
                    g.phase = "出错".into();
                    g.error = Some(e.to_string());
                }
            }
        });
        EchoTest { progress, recording, stop_play: Arc::new(AtomicBool::new(false)) }
    }

    pub fn progress(&self) -> EchoProgress {
        self.progress.lock().unwrap().clone()
    }

    pub fn busy(&self) -> bool {
        let p = self.progress.lock().unwrap();
        matches!(p.phase.as_str(), "准备" | "录音" | "分析") || p.playing
    }

    /// 在指定播放设备（空 = 系统默认）上回放录音。
    pub fn play(&self, device: &str) -> Res<()> {
        let samples = self.recording.lock().unwrap().clone();
        if samples.is_empty() {
            return Err("还没有录音".into());
        }
        self.stop_play.store(false, Relaxed);
        let (stop, progress, device) = (self.stop_play.clone(), self.progress.clone(), device.to_string());
        progress.lock().unwrap().playing = true;
        std::thread::spawn(move || {
            let _ = initialize_mta();
            if let Err(e) = play_samples(&device, &samples, &stop) {
                progress.lock().unwrap().error = Some(format!("回放失败：{e}"));
            }
            progress.lock().unwrap().playing = false;
        });
        Ok(())
    }

    pub fn stop_play(&self) {
        self.stop_play.store(true, Relaxed);
    }
}

/// 读出这一批的所有包，转成单声道追加到 out，返回帧数和峰值。
fn drain(st: &Stream, cap: &AudioCaptureClient, raw: &mut [u8], tmp: &mut Vec<f32>, out: &mut Vec<f32>) -> Res<(usize, f32)> {
    let mut frames = 0;
    let mut peak = 0f32;
    loop {
        if cap.get_next_packet_size()? == Some(0) {
            break;
        }
        let (n, info) = cap.read_from_device(raw)?;
        if n == 0 {
            break;
        }
        tmp.clear();
        if info.flags.silent {
            tmp.resize(n as usize * 2, 0.0);
        } else {
            st.conv.convert(raw, n as usize, tmp);
        }
        for f in tmp.chunks_exact(2) {
            let v = (f[0] + f[1]) * 0.5;
            peak = peak.max(v.abs());
            out.push(v);
        }
        frames += n as usize;
    }
    Ok((frames, peak))
}

fn record(opts: &EchoTestOptions, p: &Mutex<EchoProgress>) -> Res<(Vec<f32>, Vec<f32>)> {
    let mic_dev = find_device(Direction::Capture, &opts.mic_device)?;
    let ref_dev = find_device(Direction::Render, &opts.reference)?;
    let mic = open_stream(&mic_dev, Direction::Capture, Mode::Shared, false)?;
    let refs = open_stream(&ref_dev, Direction::Capture, Mode::Shared, true)?;
    let mic_cap = mic.client.get_audiocaptureclient()?;
    let ref_cap = refs.client.get_audiocaptureclient()?;

    mic.client.start_stream()?;
    refs.client.start_stream()?;
    let mut raw = vec![0u8; (mic.buffer_frames.max(refs.buffer_frames) as usize + 64) * 8];
    let mut tmp = Vec::new();
    let mut mic_buf: Vec<f32> = Vec::new();
    let mut ref_buf: Vec<f32> = Vec::new();

    let start = Instant::now();
    let mut rec_start: Option<Instant> = None;
    let mut last_ref = Instant::now();
    let mut last_ui = Instant::now();
    let (mut mic_peak, mut ref_peak) = (0f32, 0f32);
    let total = Duration::from_secs_f32(opts.seconds);

    loop {
        std::thread::sleep(Duration::from_millis(2));
        let (_, mp) = drain(&mic, &mic_cap, &mut raw, &mut tmp, &mut mic_buf)?;
        let (rf, rp) = drain(&refs, &ref_cap, &mut raw, &mut tmp, &mut ref_buf)?;
        mic_peak = mic_peak.max(mp);
        ref_peak = ref_peak.max(rp);
        // 回环采集在系统没放声音时不给数据，按时间补静音，保证两路时间轴对齐
        if rf > 0 {
            last_ref = Instant::now();
        } else {
            let idle = last_ref.elapsed();
            if idle >= Duration::from_millis(20) {
                let frames = ((idle.as_secs_f64() - 0.01) * RATE as f64) as usize;
                ref_buf.resize(ref_buf.len() + frames, 0.0);
                last_ref += Duration::from_secs_f64(frames as f64 / RATE as f64);
            }
        }

        match rec_start {
            None => {
                // 准备阶段：等 Mac 打开麦克风、数据到达后再正式开始
                let waited = start.elapsed();
                let mic_live = mic_peak > 0.0005;
                if (waited >= PREROLL_MIN && mic_live) || waited >= PREROLL_MAX {
                    mic_buf.clear();
                    ref_buf.clear();
                    last_ref = Instant::now();
                    rec_start = Some(Instant::now());
                    p.lock().unwrap().phase = "录音".into();
                }
            }
            Some(t) if t.elapsed() >= total => break,
            Some(_) => {}
        }

        if last_ui.elapsed() >= Duration::from_millis(100) {
            let mut g = p.lock().unwrap();
            g.elapsed_s = rec_start.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
            g.mic_db = peak_db(mic_peak);
            g.ref_db = peak_db(ref_peak);
            drop(g);
            mic_peak = 0.0;
            ref_peak = 0.0;
            last_ui = Instant::now();
        }
    }
    let _ = mic.client.stop_stream();
    let _ = refs.client.stop_stream();
    Ok((mic_buf, ref_buf))
}

fn save_wav(mic: &[f32]) -> Option<PathBuf> {
    let dir = std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Downloads")).filter(|d| d.is_dir())?;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("Aurio-录音测试-{secs}.wav"));
    aurio_core::wav::write_mono16(&path, mic, RATE as u32).ok().map(|_| path)
}

fn report(a: &EchoAnalysis, wav: Option<PathBuf>) -> EchoReport {
    let note = "会议软件和微信自带回声消除，一般能把这类回声去掉；这里测的是没有经过处理的原始声音。";
    let (verdict, detail) = match a.verdict {
        Verdict::NoReference => (
            "播放的声音太小，测不出回声".to_string(),
            "录音时 Windows 几乎没有在放声音。先在 Windows 上放一首歌，再点「开始录音」。".to_string(),
        ),
        Verdict::MicSilent => (
            "录到的声音几乎是静音".to_string(),
            "Mac 的麦克风可能没打开：看 Mac 菜单栏里 Aurio 的麦克风状态和权限，或者把麦克风模式改成「常开」再试。".to_string(),
        ),
        Verdict::None => ("基本没有回声".to_string(), note.to_string()),
        Verdict::Slight => (
            format!("有轻微回声（延迟约 {:.0} ms）", a.delay_ms),
            format!("播放的声音有一小部分漏进了麦克风。{note}"),
        ),
        Verdict::Obvious => (
            format!("回声明显（延迟约 {:.0} ms）", a.delay_ms),
            format!("Studio Display 外放的声音被它自己的麦克风收进去了。可以调低 Mac 上的音量再测。{note}"),
        ),
    };
    EchoReport {
        verdict,
        detail,
        delay_ms: a.delay_ms,
        echo_db: a.echo_db,
        echo_share_pct: a.echo_share * 100.0,
        correlation: a.correlation,
        mic_rms_db: a.mic_rms_db,
        ref_rms_db: a.ref_rms_db,
        wav_path: wav.map(|p| p.display().to_string()).unwrap_or_default(),
    }
}

fn play_samples(device: &str, samples: &[f32], stop: &AtomicBool) -> Res<()> {
    let dev = find_device(Direction::Render, device)?;
    let mut client = dev.get_iaudioclient()?;
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, RATE, 2, None);
    client.initialize_client(
        &fmt,
        &Direction::Render,
        &StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: 1_000_000 },
    )?;
    let render = client.get_audiorenderclient()?;
    client.start_stream()?;
    let mut pos = 0;
    let mut bytes = Vec::new();
    while pos < samples.len() && !stop.load(Relaxed) {
        let n = (client.get_available_space_in_frames()? as usize).min(samples.len() - pos);
        if n > 0 {
            bytes.clear();
            for &v in &samples[pos..pos + n] {
                bytes.extend_from_slice(&v.to_le_bytes());
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            render.write_to_device(n, &bytes, None)?;
            pos += n;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // 等缓冲里剩下的放完
    while !stop.load(Relaxed) && client.get_current_padding()? > 0 {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = client.stop_stream();
    Ok(())
}
