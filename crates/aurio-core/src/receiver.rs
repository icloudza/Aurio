//! 网络线程这一半：解析音频包，按 sample_index 对齐后写进环形缓冲。

use crate::proto::{self, Format, Header};
use crate::stats::RxStats;
use rtrb::Producer;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 两个包间隔小于这个值就算同一批到达。
const BURST_GAP_US: u64 = 1500;
const BURST_WINDOW_US: u64 = 2_000_000;

pub struct Depacketizer {
    fmt: Format,
    stream_id: u8,
    producer: Producer<f32>,
    expected: Option<u64>,
    scratch: Vec<f32>,
    stats: Arc<RxStats>,
    last_arrival_us: u64,
    burst_acc: u32,
    burst_max_cur: u32,
    burst_max_prev: u32,
    burst_window_start: u64,
    gap_max_cur: u64,
    gap_max_prev: u64,
    zeros: Vec<f32>,
}

impl Depacketizer {
    pub fn new(fmt: Format, stream_id: u8, producer: Producer<f32>, stats: Arc<RxStats>) -> Self {
        Depacketizer {
            fmt,
            stream_id,
            producer,
            expected: None,
            scratch: vec![0.0; proto::MAX_PACKET],
            stats,
            last_arrival_us: 0,
            burst_acc: 0,
            burst_max_cur: 0,
            burst_max_prev: 0,
            burst_window_start: 0,
            gap_max_cur: 0,
            gap_max_prev: 0,
            zeros: vec![0.0; 1024],
        }
    }

    pub fn format(&self) -> Format {
        self.fmt
    }

    /// 处理一个收到的 UDP 包。`now_us` 是单调时钟（微秒），只用于统计成批到达的大小。
    pub fn handle(&mut self, pkt: &[u8], now_us: u64) {
        let h = match Header::parse(pkt) {
            Ok(h) => h,
            Err(_) => {
                self.stats.bad_packets.fetch_add(1, Relaxed);
                return;
            }
        };
        if h.ptype == proto::ptype::HEARTBEAT {
            // 心跳用 max(1) 避免和「没收到过」的 0 混淆
            let t = now_us.max(1);
            self.stats.last_heartbeat_us.store(t, Relaxed);
            if h.flags & proto::flags::MIC_REQUEST != 0 {
                self.stats.mic_request_us.store(t, Relaxed);
            }
            return;
        }
        if h.ptype != proto::ptype::AUDIO {
            return;
        }
        let payload = &pkt[proto::HEADER_LEN..];
        let frames = h.frames as usize;
        if h.stream_id != self.stream_id
            || h.format_id != self.fmt.id
            || payload.len() < frames * self.fmt.bytes_per_frame()
        {
            self.stats.bad_packets.fetch_add(1, Relaxed);
            return;
        }
        self.stats.packets.fetch_add(1, Relaxed);
        self.stats.bytes.fetch_add(pkt.len() as u64, Relaxed);
        self.stats.last_packet_us.store(now_us, Relaxed);
        self.track_burst(frames as u32, now_us);

        let rate = self.fmt.rate as i64;
        if let Some(exp) = self.expected {
            let d = h.sample_index as i64 - exp as i64;
            if d < 0 && d >= -rate {
                // 迟到或重复。局域网里很少见，直接丢。
                self.stats.late_packets.fetch_add(1, Relaxed);
                return;
            } else if d > 0 && d <= rate / 2 {
                // 中间丢了包：补静音，保持时间轴连续
                self.stats.lost_frames.fetch_add(d as u64, Relaxed);
                self.write_zeros(d as usize);
            } else if d != 0 {
                // 跳变太大（发送端重启等），重新对齐
                self.stats.resyncs.fetch_add(1, Relaxed);
            }
        }
        self.expected = Some(h.sample_index + frames as u64);

        let ch = self.fmt.channels as usize;
        let n = proto::decode_samples(
            self.fmt.sample,
            &payload[..frames * self.fmt.bytes_per_frame()],
            &mut self.scratch[..frames * ch],
        );
        self.write(n);
    }

    fn track_burst(&mut self, frames: u32, now_us: u64) {
        let gap = now_us.saturating_sub(self.last_arrival_us);
        if self.stats.packets.load(Relaxed) > 1 {
            self.gap_max_cur = self.gap_max_cur.max(gap);
        }
        if gap < BURST_GAP_US {
            self.burst_acc += frames;
        } else {
            self.burst_acc = frames;
        }
        self.last_arrival_us = now_us;
        self.burst_max_cur = self.burst_max_cur.max(self.burst_acc);
        if now_us.saturating_sub(self.burst_window_start) > BURST_WINDOW_US {
            self.burst_max_prev = self.burst_max_cur;
            self.burst_max_cur = self.burst_acc;
            self.burst_window_start = now_us;
            self.gap_max_prev = self.gap_max_cur;
            self.gap_max_cur = 0;
        }
        let g = self.gap_max_prev.max(self.gap_max_cur).min(u32::MAX as u64) as u32;
        self.stats.max_gap_us.store(g, Relaxed);
        self.stats
            .burst_frames
            .store(self.burst_max_prev.max(self.burst_max_cur), Relaxed);
    }

    fn write(&mut self, samples: usize) {
        let (_, rest) = self.producer.push_partial_slice(&self.scratch[..samples]);
        if !rest.is_empty() {
            let ch = self.fmt.channels as u64;
            self.stats.overflow_frames.fetch_add(rest.len() as u64 / ch, Relaxed);
        }
    }

    fn write_zeros(&mut self, frames: usize) {
        let mut left = frames * self.fmt.channels as usize;
        while left > 0 {
            let n = left.min(self.zeros.len());
            let (_, rest) = self.producer.push_partial_slice(&self.zeros[..n]);
            if !rest.is_empty() {
                let ch = self.fmt.channels as u64;
                self.stats.overflow_frames.fetch_add((left as u64) / ch, Relaxed);
                return;
            }
            left -= n;
        }
    }
}

/// UDP 接收线程。
pub struct UdpReceiver {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    local_addr: SocketAddr,
    peer: Arc<Mutex<Option<SocketAddr>>>,
    epoch: Instant,
    sock: UdpSocket,
    local_flags: Arc<AtomicU8>,
}

impl UdpReceiver {
    pub fn spawn(bind: SocketAddr, mut depack: Depacketizer) -> io::Result<UdpReceiver> {
        let sock = UdpSocket::bind(bind)?;
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        let local_addr = sock.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let peer = Arc::new(Mutex::new(None));
        let stop2 = stop.clone();
        let peer2 = peer.clone();
        let epoch = Instant::now();
        let send_sock = sock.try_clone()?;
        let local_flags = Arc::new(AtomicU8::new(0));
        let local2 = local_flags.clone();
        let join = std::thread::Builder::new()
            .name("aurio-net-rx".into())
            .spawn(move || {
                crate::rt::promote_current_thread();
                let start = epoch;
                let mut buf = [0u8; 2048];
                while !stop2.load(Relaxed) {
                    match sock.recv_from(&mut buf) {
                        Ok((n, from)) => {
                            let now_us = start.elapsed().as_micros() as u64;
                            // 收到对方的心跳（不是回声）就立刻回一个回声，让对方知道我们收到了
                            if let Ok(h) = Header::parse(&buf[..n]) {
                                if h.ptype == proto::ptype::HEARTBEAT && h.flags & proto::flags::HEARTBEAT_ACK == 0 {
                                    let mut ack = [0u8; proto::HEADER_LEN];
                                    let f = h.flags | proto::flags::HEARTBEAT_ACK | local2.load(Relaxed);
                                    Header { flags: f, ..h }.write(&mut ack);
                                    let _ = sock.send_to(&ack, from);
                                }
                            }
                            depack.handle(&buf[..n], now_us);
                            if let Ok(mut p) = peer2.try_lock() {
                                if *p != Some(from) {
                                    *p = Some(from);
                                }
                            }
                        }
                        Err(e)
                            if e.kind() == io::ErrorKind::WouldBlock
                                || e.kind() == io::ErrorKind::TimedOut => {}
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            })?;
        Ok(UdpReceiver { stop, join: Some(join), local_addr, peer, epoch, sock: send_sock, local_flags })
    }

    /// 本端状态标志（如 flags::AEC_ACTIVE），会附在每个心跳回声里告诉对方。
    pub fn set_local_flags(&self, flags: u8) {
        self.local_flags.store(flags, Relaxed);
    }

    /// 统计里的时间戳（微秒）都以这个时刻为起点。
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// 同一个 socket 的副本，用来往对端回发（麦克风上行走同一个端口）。
    pub fn socket(&self) -> io::Result<UdpSocket> {
        self.sock.try_clone()
    }

    pub fn peer_handle(&self) -> Arc<Mutex<Option<SocketAddr>>> {
        self.peer.clone()
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 最近一个发来音频的地址。
    pub fn peer(&self) -> Option<SocketAddr> {
        *self.peer.lock().unwrap()
    }
}

impl Drop for UdpReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{stream, Packetizer, FORMAT_48K_S24_STEREO};

    #[test]
    fn fills_gaps_and_drops_late() {
        let fmt = FORMAT_48K_S24_STEREO;
        let (prod, mut cons) = rtrb::RingBuffer::new(48000);
        let stats = Arc::new(RxStats::default());
        let mut d = Depacketizer::new(fmt, stream::PLAYBACK, prod, stats.clone());
        let mut p = Packetizer::new(fmt, stream::PLAYBACK, 120);
        let mut pkts = Vec::new();
        p.push(&vec![0.5f32; 120 * 2 * 4], |b| pkts.push(b.to_vec()));
        d.handle(&pkts[0], 0);
        d.handle(&pkts[2], 10); // 丢了 1 号
        d.handle(&pkts[1], 20); // 1 号迟到
        d.handle(&pkts[3], 30);
        assert_eq!(cons.slots(), 120 * 2 * 4);
        assert_eq!(stats.lost_frames.load(Relaxed), 120);
        assert_eq!(stats.late_packets.load(Relaxed), 1);
        let mut v = vec![0f32; 120 * 2 * 4];
        cons.pop_entire_slice(&mut v).unwrap();
        assert!((v[0] - 0.5).abs() < 1e-6);
        assert_eq!(v[240], 0.0); // 补的静音
        assert!((v[480] - 0.5).abs() < 1e-6);
        assert_eq!(stats.burst_frames.load(Relaxed), 480);
    }
}
