//! Aurio 核心库：协议、抖动缓冲、漂移补偿、重采样。Windows 端和 Mac 端共用。

pub mod drift;
pub mod echo;
pub mod playout;
pub mod proto;
pub mod receiver;
pub mod resampler;
pub mod rt;
pub mod sim;
pub mod stats;
pub mod sender;
pub mod uplink;
pub mod wav;

use std::sync::Arc;

pub use playout::{Playout, PlayoutConfig};
pub use proto::Format;
pub use receiver::{Depacketizer, UdpReceiver};
pub use stats::{RxStats, Snapshot};

/// 创建一条接收流：返回网络线程用的解包器和渲染线程用的播放器。
/// 环形缓冲容量为 1 秒。
pub fn new_rx_stream(
    fmt: Format,
    stream_id: u8,
    out_channels: usize,
    out_rate: f64,
    cfg: PlayoutConfig,
) -> (Depacketizer, Playout, Arc<RxStats>) {
    let ch = fmt.channels as usize;
    let stats = Arc::new(RxStats::default());
    let (prod, cons) = rtrb::RingBuffer::new(fmt.rate as usize * ch);
    let depack = Depacketizer::new(fmt, stream_id, prod, stats.clone());
    let playout = Playout::new(cons, ch, fmt.rate as f64, out_channels, out_rate, cfg, stats.clone());
    (depack, playout, stats)
}
