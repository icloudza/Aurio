//! WASAPI 公共部分：找设备、打开采集流、格式转换、MMCSS、QPC。

use std::error::Error;
use wasapi::*;
use windows::core::w;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;

pub type Res<T> = Result<T, Box<dyn Error>>;

pub const RATE: usize = 48000;

/// 把当前线程注册到 MMCSS 的 "Pro Audio" 任务，获得实时调度。
pub fn mmcss_pro_audio() -> bool {
    let mut idx = 0u32;
    unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut idx).is_ok() }
}

/// 当前 QPC 时间，换算成 100 ns 单位（与 WASAPI 缓冲时间戳同一时基）。
pub fn qpc_100ns() -> i64 {
    let mut c = 0i64;
    let mut f = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    (c as i128 * 10_000_000 / f.max(1) as i128) as i64
}

pub fn find_device(direction: Direction, name: &str) -> Res<Device> {
    let en = DeviceEnumerator::new()?;
    if name.is_empty() {
        return Ok(en.get_default_device(&direction)?);
    }
    let coll = en.get_device_collection(&direction)?;
    let needle = name.to_lowercase();
    let mut names = Vec::new();
    for d in &coll {
        let d = d?;
        let n = d.get_friendlyname().unwrap_or_default();
        if n.to_lowercase().contains(&needle) {
            return Ok(d);
        }
        names.push(n);
    }
    let kind = if direction == Direction::Capture { "录音" } else { "播放" };
    Err(format!("找不到名称包含 \"{name}\" 的{kind}设备。现有：{}", names.join("、")).into())
}

/// 线性峰值 → dBFS，静音返回 -90。
pub fn peak_db(peak: f32) -> f32 {
    if peak <= 0.0000316 {
        -90.0
    } else {
        20.0 * peak.log10()
    }
}

/// 这个播放设备是不是虚拟线（新版 VB-Cable 会装出 "CABLE Input" 和 "CABLE In 16ch" 两个，都通往同一根线）。
pub fn is_virtual_cable(name: &str, mic_render: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("vb-audio virtual cable") || n.contains("cable in") || n.contains(&mic_render.to_lowercase())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Shared,
    LowLatency,
    Exclusive,
}

impl Mode {
    pub fn parse(s: &str) -> Res<Mode> {
        match s {
            "shared" | "" => Ok(Mode::Shared),
            "lowlat" => Ok(Mode::LowLatency),
            "exclusive" => Ok(Mode::Exclusive),
            _ => Err(format!("未知模式 {s}，可选 shared / lowlat / exclusive").into()),
        }
    }
}

/// 把设备给出的原始字节转成交错 f32 立体声。
pub struct Converter {
    st: SampleType,
    store_bytes: usize,
    ch: usize,
}

impl Converter {
    pub fn new(fmt: &WaveFormat) -> Res<Converter> {
        Ok(Converter {
            st: fmt.get_subformat()?,
            store_bytes: fmt.get_bitspersample() as usize / 8,
            ch: fmt.get_nchannels() as usize,
        })
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.store_bytes * self.ch
    }

    fn sample(&self, b: &[u8]) -> f32 {
        match (self.st, self.store_bytes) {
            (SampleType::Float, 4) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (SampleType::Int, 2) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            (SampleType::Int, 3) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            (SampleType::Int, 4) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            _ => 0.0,
        }
    }

    /// 追加 `frames` 帧立体声到 out。
    pub fn convert(&self, data: &[u8], frames: usize, out: &mut Vec<f32>) {
        let bpf = self.bytes_per_frame();
        let sb = self.store_bytes;
        for f in 0..frames {
            let fr = &data[f * bpf..(f + 1) * bpf];
            let l = self.sample(&fr[0..sb]);
            let r = if self.ch > 1 { self.sample(&fr[sb..2 * sb]) } else { l };
            out.push(l);
            out.push(r);
        }
    }
}

pub struct Stream {
    pub client: AudioClient,
    pub event: Option<Handle>,
    pub conv: Converter,
    pub buffer_frames: u32,
    pub desc: String,
}

fn periods_desc(client: &AudioClient) -> String {
    match client.get_device_period() {
        Ok((def, min)) => format!("设备周期 默认 {:.2} ms / 最小 {:.2} ms", def as f64 / 1e4, min as f64 / 1e4),
        Err(_) => String::new(),
    }
}

/// 打开一个 48 kHz 流。`loopback` 为 true 时 device 是播放设备，以回环方式采集。
pub fn open_stream(device: &Device, direction: Direction, mode: Mode, loopback: bool) -> Res<Stream> {
    let mut client = device.get_iaudioclient()?;
    let pd = periods_desc(&client);
    let (def_period, min_period) = client.get_device_period()?;
    let f32_stereo = WaveFormat::new(32, 32, &SampleType::Float, RATE, 2, None);

    let (fmt, desc, events) = match mode {
        Mode::Shared => {
            // 回环采集在部分系统上不发事件，用轮询更稳
            let sm = if loopback {
                StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: def_period }
            } else {
                StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_period }
            };
            client.initialize_client(&f32_stereo, &direction, &sm)?;
            (f32_stereo, format!("共享模式（自动转换为 48 kHz float），{pd}"), !loopback)
        }
        Mode::LowLatency => {
            if loopback {
                return Err("lowlat 模式不支持回环采集".into());
            }
            let mix = client.get_mixformat()?;
            if mix.get_samplespersec() as usize != RATE {
                return Err(format!(
                    "设备混音格式是 {} Hz，lowlat 模式要求 48000 Hz。请在「声音设置 → 设备属性 → 高级」里改成 48000 Hz",
                    mix.get_samplespersec()
                )
                .into());
            }
            let p = client.get_shared_mode_engine_period(&mix)?;
            client.initialize_shared_audio_stream(&mix, TimingMode::Events, p.min)?;
            let d = format!(
                "低延迟共享模式（IAudioClient3），引擎周期 {} 帧（{:.2} ms），可选 {}..{} 帧（基本单位 {}）",
                p.min, p.min as f64 * 1000.0 / RATE as f64, p.min, p.max, p.fundamental
            );
            (mix, d, true)
        }
        Mode::Exclusive => {
            if loopback {
                return Err("独占模式不支持回环采集".into());
            }
            let candidates = [
                WaveFormat::new(32, 32, &SampleType::Float, RATE, 2, None),
                WaveFormat::new(32, 32, &SampleType::Int, RATE, 2, None),
                WaveFormat::new(32, 24, &SampleType::Int, RATE, 2, None),
                WaveFormat::new(24, 24, &SampleType::Int, RATE, 2, None),
                WaveFormat::new(16, 16, &SampleType::Int, RATE, 2, None),
            ];
            let fmt = candidates
                .iter()
                .find_map(|f| client.is_supported_exclusive_with_quirks(f).ok())
                .ok_or("设备不支持 48 kHz 立体声独占模式")?;
            let period = client.calculate_aligned_period_near(min_period, Some(128), &fmt)?;
            client.initialize_client(&fmt, &direction, &StreamMode::EventsExclusive { period_hns: period })?;
            let d = format!(
                "独占模式，{} 位 {:?}，周期 {:.2} ms，{pd}",
                fmt.get_validbitspersample(),
                fmt.get_subformat()?,
                period as f64 / 1e4
            );
            (fmt, d, true)
        }
    };
    let event = if events { Some(client.set_get_eventhandle()?) } else { None };
    let buffer_frames = client.get_buffer_size()?;
    let conv = Converter::new(&fmt)?;
    Ok(Stream { client, event, conv, buffer_frames, desc })
}

impl Stream {
    /// 等下一批数据：事件模式等事件，轮询模式睡 1 ms。
    pub fn wait(&self) -> Res<()> {
        match &self.event {
            Some(h) => h.wait_for_event(2000).map_err(|_| "等待音频事件超时（设备被拔掉或被占用？）".into()),
            None => {
                std::thread::sleep(std::time::Duration::from_millis(1));
                Ok(())
            }
        }
    }
}
