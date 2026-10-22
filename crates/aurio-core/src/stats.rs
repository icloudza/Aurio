//! 接收端统计量。网络线程和渲染线程各写各的字段，界面线程只读。
//! 全部用 Relaxed 原子操作，渲染线程里可以安全写入。

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
}

pub mod state {
    pub const IDLE: u32 = 0;
    pub const BUFFERING: u32 = 1;
    pub const PLAYING: u32 = 2;
}

#[derive(Default)]
pub struct RxStats {
    // —— 网络线程 ——
    pub packets: AtomicU64,
    pub bytes: AtomicU64,
    /// 因丢包补的静音帧数
    pub lost_frames: AtomicU64,
    /// 迟到或重复的包
    pub late_packets: AtomicU64,
    /// 解析失败、流号或格式不对的包
    pub bad_packets: AtomicU64,
    /// 发送端重启或大跳变后的重新同步次数
    pub resyncs: AtomicU64,
    /// 环形缓冲写满而丢掉的帧数
    pub overflow_frames: AtomicU64,
    /// 最近观察到的成批到达的最大帧数（≈ 发送端采集周期）
    pub burst_frames: AtomicU32,
    /// 最近一个包到达的时间（微秒，相对接收线程启动）
    pub last_packet_us: AtomicU64,
    /// 最近一段时间里两个包之间的最大间隔（微秒）。明显大于采集周期说明发送端或网络卡了
    pub max_gap_us: AtomicU32,
    /// 最近一次收到心跳的时间（微秒），0 表示没收到过
    pub last_heartbeat_us: AtomicU64,
    /// 最近一次收到「需要麦克风」心跳的时间（微秒），0 表示没收到过
    pub mic_request_us: AtomicU64,

    // —— 渲染线程 ——
    pub state: AtomicU32,
    /// 最近一次回调后的水位（输入帧）
    pub fill_frames: AtomicF32,
    /// 最近一个控制窗口内的最低水位（输入帧）
    pub window_min_frames: AtomicF32,
    /// 目标最低水位（输入帧）
    pub target_frames: AtomicF32,
    /// 当前重采样修正量（ppm，正数表示消耗得更快）
    pub ppm: AtomicF32,
    pub underruns: AtomicU64,
    pub skips: AtomicU64,
    pub rendered_frames: AtomicU64,
}

/// 统计快照，单位已换算成毫秒。
#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub state: u32,
    pub fill_ms: f32,
    pub window_min_ms: f32,
    pub target_ms: f32,
    pub ppm: f32,
    pub underruns: u64,
    pub skips: u64,
    pub packets: u64,
    pub bytes: u64,
    pub lost_frames: u64,
    pub late_packets: u64,
    pub bad_packets: u64,
    pub resyncs: u64,
    pub overflow_frames: u64,
    pub burst_ms: f32,
    pub max_gap_ms: f32,
    pub rendered_frames: u64,
}

impl RxStats {
    pub fn snapshot(&self, in_rate: f64) -> Snapshot {
        let ms = |f: f32| (f as f64 * 1000.0 / in_rate) as f32;
        Snapshot {
            state: self.state.load(Relaxed),
            fill_ms: ms(self.fill_frames.load()),
            window_min_ms: ms(self.window_min_frames.load()),
            target_ms: ms(self.target_frames.load()),
            ppm: self.ppm.load(),
            underruns: self.underruns.load(Relaxed),
            skips: self.skips.load(Relaxed),
            packets: self.packets.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            lost_frames: self.lost_frames.load(Relaxed),
            late_packets: self.late_packets.load(Relaxed),
            bad_packets: self.bad_packets.load(Relaxed),
            resyncs: self.resyncs.load(Relaxed),
            overflow_frames: self.overflow_frames.load(Relaxed),
            burst_ms: ms(self.burst_frames.load(Relaxed) as f32),
            max_gap_ms: self.max_gap_us.load(Relaxed) as f32 / 1000.0,
            rendered_frames: self.rendered_frames.load(Relaxed),
        }
    }
}
