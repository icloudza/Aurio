//! 给 Swift（以及以后的其他宿主）用的 C 接口。头文件见 include/aurio.h。

use aurio_core::playout::PlayoutConfig;
use aurio_core::proto::{self, format_by_id};
use aurio_core::stats::AtomicF32;
use aurio_core::{new_rx_stream, Playout, RxStats, UdpReceiver};
use std::cell::{RefCell, UnsafeCell};
use std::ffi::{c_char, CString};
use std::net::SocketAddr;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use aurio_core::uplink::{Source, Uplink, UplinkStats};

#[repr(C)]
pub struct AurioRxConfig {
    pub port: u16,
    pub stream_id: u8,
    pub format_id: u16,
    pub out_rate: f64,
    pub out_channels: u32,
    pub target_ms: f64,
    pub max_ppm: f64,
    pub half_taps: u32,
    pub volume: f32,
}

#[repr(C)]
#[derive(Default)]
pub struct AurioRxStats {
    pub state: u32,
    pub fill_ms: f32,
    pub window_min_ms: f32,
    pub target_ms: f32,
    pub ppm: f32,
    pub burst_ms: f32,
    pub max_gap_ms: f32,
    pub resampler_ms: f32,
    pub underruns: u64,
    pub skips: u64,
    pub packets: u64,
    pub bytes: u64,
    pub lost_frames: u64,
    pub late_packets: u64,
    pub bad_packets: u64,
    pub resyncs: u64,
    pub overflow_frames: u64,
    pub rendered_frames: u64,
    pub since_last_packet_s: f64,
    pub since_last_heartbeat_s: f64,
    pub mic_requested: u32,
    pub has_peer: u32,
}

pub struct AurioRx {
    playout: UnsafeCell<Playout>,
    out_channels: usize,
    in_rate: f64,
    resampler_ms: f32,
    stats: Arc<RxStats>,
    volume: Arc<AtomicF32>,
    net: UdpReceiver,
    /// 上次读取以来输出的峰值（f32 的位表示；非负浮点数按位比较和按值比较一致）
    out_peak: AtomicU32,
}

/// 求峰值并累计到 `acc` 里（取最大）。实时线程里调用，只用原子操作。
fn accumulate_peak(acc: &AtomicU32, data: &[f32]) {
    let mut p = 0f32;
    for &x in data {
        p = p.max(x.abs());
    }
    if p.is_finite() && p > 0.0 {
        acc.fetch_max(p.to_bits(), Relaxed);
    }
}

/// 「需要麦克风」心跳的有效期：超过这么久没收到就认为不再需要
const MIC_REQUEST_TIMEOUT_S: f64 = 3.0;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(msg: String) {
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::new(msg).unwrap_or_default());
}

#[no_mangle]
pub extern "C" fn aurio_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn aurio_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// # Safety
/// `cfg` 必须指向有效内存。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_default_config(cfg: *mut AurioRxConfig) {
    let d = PlayoutConfig::default();
    *cfg = AurioRxConfig {
        port: proto::DEFAULT_PORT,
        stream_id: proto::stream::PLAYBACK,
        format_id: 1,
        out_rate: 48000.0,
        out_channels: 2,
        target_ms: d.drift.target_ms,
        max_ppm: d.drift.max_ppm,
        half_taps: d.half_taps as u32,
        volume: 1.0,
    };
}

/// # Safety
/// `cfg` 必须指向有效的配置。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_create(cfg: *const AurioRxConfig) -> *mut AurioRx {
    let cfg = &*cfg;
    let Some(fmt) = format_by_id(cfg.format_id) else {
        set_error(format!("未知的格式编号 {}", cfg.format_id));
        return std::ptr::null_mut();
    };
    if cfg.out_channels == 0 || cfg.out_rate < 8000.0 {
        set_error(format!("输出参数不合法：{} Hz，{} 声道", cfg.out_rate, cfg.out_channels));
        return std::ptr::null_mut();
    }
    let mut pc = PlayoutConfig::default();
    pc.drift.target_ms = cfg.target_ms;
    pc.drift.max_ppm = cfg.max_ppm;
    pc.half_taps = cfg.half_taps as usize;
    let (depack, playout, stats) =
        new_rx_stream(fmt, cfg.stream_id, cfg.out_channels as usize, cfg.out_rate, pc);
    let volume = playout.volume_handle();
    volume.store(cfg.volume);
    let bind = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    let net = match UdpReceiver::spawn(bind, depack) {
        Ok(n) => n,
        Err(e) => {
            set_error(format!("绑定 UDP 端口 {} 失败：{e}", cfg.port));
            return std::ptr::null_mut();
        }
    };
    let rx = AurioRx {
        resampler_ms: playout.resampler_latency_ms() as f32,
        in_rate: playout.in_rate(),
        playout: UnsafeCell::new(playout),
        out_channels: cfg.out_channels as usize,
        stats,
        volume,
        net,
        out_peak: AtomicU32::new(0),
    };
    set_error(String::new());
    Box::into_raw(Box::new(rx))
}

/// # Safety
/// 只能由音频回调线程调用；`out` 至少有 frames × out_channels 个 float。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_render(rx: *mut AurioRx, out: *mut f32, frames: u32) {
    if rx.is_null() || out.is_null() {
        return;
    }
    let rx = &*rx;
    let buf = std::slice::from_raw_parts_mut(out, frames as usize * rx.out_channels);
    (*rx.playout.get()).render(buf, frames as usize);
    accumulate_peak(&rx.out_peak, buf);
}

/// 取出上次调用以来的输出峰值（线性，已乘音量）并清零。界面画电平用。
///
/// # Safety
/// 指针必须有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_take_peak(rx: *const AurioRx) -> f32 {
    rx.as_ref().map_or(0.0, |r| f32::from_bits(r.out_peak.swap(0, Relaxed)))
}

/// # Safety
/// 指针必须有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_get_stats(rx: *const AurioRx, out: *mut AurioRxStats) {
    if rx.is_null() || out.is_null() {
        return;
    }
    let rx = &*rx;
    let s = rx.stats.snapshot(rx.in_rate);
    let now = rx.net.epoch().elapsed().as_secs_f64();
    let since_us = |t: u64| if t == 0 { -1.0 } else { (now - t as f64 / 1e6).max(0.0) };
    let since = if s.packets == 0 { -1.0 } else { since_us(rx.stats.last_packet_us.load(Relaxed)) };
    let mic_since = since_us(rx.stats.mic_request_us.load(Relaxed));
    *out = AurioRxStats {
        state: s.state,
        fill_ms: s.fill_ms,
        window_min_ms: s.window_min_ms,
        target_ms: s.target_ms,
        ppm: s.ppm,
        burst_ms: s.burst_ms,
        max_gap_ms: s.max_gap_ms,
        resampler_ms: rx.resampler_ms,
        underruns: s.underruns,
        skips: s.skips,
        packets: s.packets,
        bytes: s.bytes,
        lost_frames: s.lost_frames,
        late_packets: s.late_packets,
        bad_packets: s.bad_packets,
        resyncs: s.resyncs,
        overflow_frames: s.overflow_frames,
        rendered_frames: s.rendered_frames,
        since_last_packet_s: since,
        since_last_heartbeat_s: since_us(rx.stats.last_heartbeat_us.load(Relaxed)),
        mic_requested: (mic_since >= 0.0 && mic_since < MIC_REQUEST_TIMEOUT_S) as u32,
        has_peer: rx.net.peer().is_some() as u32,
    };
}

/// 把最近一个对端的 IP（不含端口）写进 buf，以 NUL 结尾。有对端返回 1，否则返回 0 并写空串。
///
/// # Safety
/// `buf` 至少有 `len` 字节。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_peer(rx: *const AurioRx, buf: *mut c_char, len: u32) -> u32 {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let ip = rx.as_ref().and_then(|r| r.net.peer()).map(|a| a.ip().to_string()).unwrap_or_default();
    let bytes = ip.as_bytes();
    let n = bytes.len().min(len as usize - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
    *buf.add(n) = 0;
    (n > 0) as u32
}

/// # Safety
/// 指针必须有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_set_volume(rx: *mut AurioRx, volume: f32) {
    if let Some(rx) = rx.as_ref() {
        rx.volume.store(volume.clamp(0.0, 1.0));
    }
}

/// 本端状态，附在心跳回声里告诉 Windows。bit0 = 正在做回声消除。
///
/// # Safety
/// 指针必须有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_set_status_flags(rx: *mut AurioRx, flags: u32) {
    if let Some(rx) = rx.as_ref() {
        let mut f = 0u8;
        if flags & 1 != 0 {
            f |= proto::flags::AEC_ACTIVE;
        }
        rx.net.set_local_flags(f);
    }
}

/// # Safety
/// 调用前必须停掉音频回调；之后不能再使用该指针。
#[no_mangle]
pub unsafe extern "C" fn aurio_rx_destroy(rx: *mut AurioRx) {
    if !rx.is_null() {
        drop(Box::from_raw(rx));
    }
}

// ———————————————————— 麦克风上行 ————————————————————

pub struct AurioMic {
    up: UnsafeCell<Uplink>,
    stats: Arc<UplinkStats>,
    peak: AtomicU32,
}

#[repr(C)]
#[derive(Default)]
pub struct AurioMicStats {
    pub sent_packets: u64,
    pub send_errors: u64,
    pub dropped_frames: u64,
    pub no_peer_frames: u64,
}

/// 创建麦克风上行，复用 rx 的 socket 和对端地址。
/// `test_tone` 非 0 时不读 push 进来的数据，改发 440 Hz 测试音。
///
/// # Safety
/// `rx` 必须有效，且在 AurioMic 销毁前保持有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_mic_create(
    rx: *const AurioRx,
    in_rate: f64,
    in_channels: u32,
    test_tone: u32,
) -> *mut AurioMic {
    let Some(rx) = rx.as_ref() else {
        set_error("rx 为空".into());
        return std::ptr::null_mut();
    };
    let sock = match rx.net.socket() {
        Ok(s) => s,
        Err(e) => {
            set_error(format!("复制 socket 失败：{e}"));
            return std::ptr::null_mut();
        }
    };
    let source = if test_tone != 0 { Source::TestTone } else { Source::Ring };
    match Uplink::spawn(sock, rx.net.peer_handle(), in_rate, in_channels as usize, source) {
        Ok(up) => {
            let stats = up.stats().clone();
            Box::into_raw(Box::new(AurioMic { up: UnsafeCell::new(up), stats, peak: AtomicU32::new(0) }))
        }
        Err(e) => {
            set_error(format!("启动上行线程失败：{e}"));
            std::ptr::null_mut()
        }
    }
}

/// 只能在音频输入回调里调用。`data` 为交错 float，frames × in_channels 个。
///
/// # Safety
/// 指针必须有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_mic_push(mic: *mut AurioMic, data: *const f32, frames: u32) {
    let Some(mic) = mic.as_ref() else { return };
    if data.is_null() {
        return;
    }
    let up = &mut *mic.up.get();
    let n = frames as usize * up.channels();
    let data = std::slice::from_raw_parts(data, n);
    accumulate_peak(&mic.peak, data);
    up.push(data);
}

/// 取出上次调用以来麦克风的峰值（线性）并清零。
///
/// # Safety
/// 指针必须有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_mic_take_peak(mic: *const AurioMic) -> f32 {
    mic.as_ref().map_or(0.0, |m| f32::from_bits(m.peak.swap(0, Relaxed)))
}

/// # Safety
/// 指针必须有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_mic_get_stats(mic: *const AurioMic, out: *mut AurioMicStats) {
    let (Some(mic), false) = (mic.as_ref(), out.is_null()) else { return };
    *out = AurioMicStats {
        sent_packets: mic.stats.sent_packets.load(Relaxed),
        send_errors: mic.stats.send_errors.load(Relaxed),
        dropped_frames: mic.stats.dropped_frames.load(Relaxed),
        no_peer_frames: mic.stats.no_peer_frames.load(Relaxed),
    };
}

/// 必须先停掉音频输入回调再调用。
///
/// # Safety
/// 之后不能再使用该指针。
#[no_mangle]
pub unsafe extern "C" fn aurio_mic_destroy(mic: *mut AurioMic) {
    if !mic.is_null() {
        drop(Box::from_raw(mic));
    }
}

// ———————————————————— 回声分析（测试工具用） ————————————————————

#[repr(C)]
#[derive(Default)]
pub struct AurioEchoResult {
    pub mic_rms_db: f32,
    pub mic_peak_db: f32,
    pub ref_rms_db: f32,
    pub delay_ms: f32,
    pub correlation: f32,
    pub echo_db: f32,
    pub echo_share: f32,
    /// 0 播放太小 1 录音静音 2 没有回声 3 轻微 4 明显
    pub verdict: u32,
}

/// # Safety
/// mic、reference 各至少有 frames 个 float，out 必须有效。
#[no_mangle]
pub unsafe extern "C" fn aurio_echo_analyze(
    mic: *const f32,
    reference: *const f32,
    frames: u64,
    rate: f64,
    max_delay_s: f64,
    out: *mut AurioEchoResult,
) {
    use aurio_core::echo::{analyze, Verdict};
    if mic.is_null() || reference.is_null() || out.is_null() {
        return;
    }
    let m = std::slice::from_raw_parts(mic, frames as usize);
    let r = std::slice::from_raw_parts(reference, frames as usize);
    let a = analyze(m, r, rate, max_delay_s);
    *out = AurioEchoResult {
        mic_rms_db: a.mic_rms_db,
        mic_peak_db: a.mic_peak_db,
        ref_rms_db: a.ref_rms_db,
        delay_ms: a.delay_ms,
        correlation: a.correlation,
        echo_db: a.echo_db,
        echo_share: a.echo_share,
        verdict: match a.verdict {
            Verdict::NoReference => 0,
            Verdict::MicSilent => 1,
            Verdict::None => 2,
            Verdict::Slight => 3,
            Verdict::Obvious => 4,
        },
    };
}
