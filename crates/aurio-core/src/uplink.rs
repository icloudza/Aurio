//! 上行发送（Mac 麦克风 → Windows）。
//!
//! 音频回调线程只调用 `push`，把采样写进无锁环形缓冲；
//! 另一个线程负责下混成单声道、按需重采样到 48 kHz、打包，然后发给对端。
//! 对端地址取自接收线程看到的最近一个发送方，所以和播放方向走同一对 socket。

use crate::proto::{self, Packetizer, FORMAT_48K_S24_MONO};
use crate::resampler::FracResampler;
use rtrb::{Consumer, Producer, RingBuffer};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct UplinkStats {
    pub sent_packets: AtomicU64,
    pub send_errors: AtomicU64,
    /// 环形缓冲满了丢掉的帧（发送线程跟不上）
    pub dropped_frames: AtomicU64,
    /// 没有对端地址时丢掉的帧
    pub no_peer_frames: AtomicU64,
}

pub struct Uplink {
    prod: Producer<f32>,
    in_ch: usize,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    stats: Arc<UplinkStats>,
}

/// 测试用：不读麦克风，直接生成 440 Hz 正弦（-20 dBFS）。
pub enum Source {
    Ring,
    TestTone,
}

impl Uplink {
    pub fn spawn(
        sock: UdpSocket,
        peer: Arc<Mutex<Option<SocketAddr>>>,
        in_rate: f64,
        in_ch: usize,
        source: Source,
    ) -> io::Result<Uplink> {
        let in_ch = in_ch.max(1);
        // 0.5 秒容量，发送线程每毫秒取一次，正常情况下远远用不满
        let (prod, cons) = RingBuffer::new((in_rate as usize / 2).max(4096) * in_ch);
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(UplinkStats::default());
        let (stop2, stats2) = (stop.clone(), stats.clone());
        let join = std::thread::Builder::new()
            .name("aurio-uplink".into())
            .spawn(move || run(sock, peer, cons, in_rate, in_ch, source, stop2, stats2))?;
        Ok(Uplink { prod, in_ch, stop, join: Some(join), stats })
    }

    /// 实时线程调用：写入交错采样，不分配、不加锁、不做系统调用。
    pub fn push(&mut self, samples: &[f32]) {
        let n = samples.len() - samples.len() % self.in_ch;
        let (_, rest) = self.prod.push_partial_slice(&samples[..n]);
        if !rest.is_empty() {
            self.stats.dropped_frames.fetch_add((rest.len() / self.in_ch) as u64, Relaxed);
        }
    }

    pub fn channels(&self) -> usize {
        self.in_ch
    }

    pub fn stats(&self) -> &Arc<UplinkStats> {
        &self.stats
    }
}

impl Drop for Uplink {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    sock: UdpSocket,
    peer: Arc<Mutex<Option<SocketAddr>>>,
    mut cons: Consumer<f32>,
    in_rate: f64,
    in_ch: usize,
    source: Source,
    stop: Arc<AtomicBool>,
    stats: Arc<UplinkStats>,
) {
    crate::rt::promote_current_thread();
    let fmt = FORMAT_48K_S24_MONO;
    let out_rate = fmt.rate as f64;
    let mut pk = Packetizer::new(fmt, proto::stream::MIC, proto::DEFAULT_FRAMES_PER_PACKET);
    pk.set_start_index(proto::time_based_start_index(fmt.rate));

    let need_resample = (in_rate - out_rate).abs() > 0.5;
    let step = in_rate / out_rate;
    let mut rs = FracResampler::new(1, 32, 0.92 * (out_rate / in_rate).min(1.0), 16384);
    let mut raw = vec![0f32; 4096 * in_ch];
    let mut mono = vec![0f32; 4096];
    let mut out = vec![0f32; 8192];

    let start = Instant::now();
    let mut tone_frames: u64 = 0;
    let mut phase = 0f64;

    while !stop.load(Relaxed) {
        std::thread::sleep(Duration::from_millis(1));
        let dest = *peer.lock().unwrap();

        // 1) 取出一批单声道采样（输入采样率）
        let frames = match source {
            Source::Ring => {
                let avail = (cons.slots() / in_ch).min(raw.len() / in_ch);
                if avail == 0 {
                    continue;
                }
                let n = avail * in_ch;
                if cons.pop_entire_slice(&mut raw[..n]).is_err() {
                    continue;
                }
                for f in 0..avail {
                    let s = &raw[f * in_ch..(f + 1) * in_ch];
                    mono[f] = s.iter().sum::<f32>() / in_ch as f32;
                }
                avail
            }
            Source::TestTone => {
                let due = (start.elapsed().as_secs_f64() * in_rate) as u64;
                let n = ((due - tone_frames.min(due)) as usize).min(mono.len());
                for m in mono.iter_mut().take(n) {
                    *m = (phase * std::f64::consts::TAU).sin() as f32 * 0.1;
                    phase = (phase + 440.0 / in_rate).fract();
                }
                tone_frames += n as u64;
                n
            }
        };
        if frames == 0 {
            continue;
        }

        // 2) 必要时重采样到 48 kHz
        let samples: &[f32] = if need_resample {
            rs.push_input(&mono[..frames]);
            let want = ((frames as f64 / step) as usize + 4).min(out.len());
            let n = rs.process(&mut out, want, step);
            &out[..n]
        } else {
            &mono[..frames]
        };

        // 3) 打包发送
        match dest {
            Some(d) => {
                let (mut ok, mut err) = (0, 0);
                pk.push(samples, |b| match sock.send_to(b, d) {
                    Ok(_) => ok += 1,
                    Err(_) => err += 1,
                });
                stats.sent_packets.fetch_add(ok, Relaxed);
                stats.send_errors.fetch_add(err, Relaxed);
            }
            None => {
                // 还不知道发给谁：时间轴照样往前走，接收端以后会正常对齐
                pk.skip_frames(samples.len() as u64);
                stats.no_peer_frames.fetch_add(samples.len() as u64, Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{flags, heartbeat, stream, HEADER_LEN};
    use crate::receiver::{Depacketizer, UdpReceiver};
    use crate::stats::RxStats;

    #[test]
    fn tone_reaches_receiver_and_heartbeat_is_seen() {
        let stats = Arc::new(RxStats::default());
        let (prod, mut cons) = RingBuffer::new(48000);
        let d = Depacketizer::new(FORMAT_48K_S24_MONO, stream::MIC, prod, stats.clone());
        let rx = UdpReceiver::spawn("127.0.0.1:0".parse().unwrap(), d).unwrap();

        // 心跳：带麦克风请求，接收端应该回一个带 ACK 的回声（seq 不变）
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut hb = [0u8; HEADER_LEN];
        heartbeat(stream::PLAYBACK, flags::MIC_REQUEST, 7, &mut hb);
        sock.send_to(&hb, rx.local_addr()).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut ack = [0u8; 64];
        let (n, _) = sock.recv_from(&mut ack).unwrap();
        let h = crate::proto::Header::parse(&ack[..n]).unwrap();
        assert_eq!(h.seq, 7);
        assert!(h.flags & flags::HEARTBEAT_ACK != 0 && h.flags & flags::MIC_REQUEST != 0);

        // 上行：44.1 kHz 立体声测试音，需要下混 + 重采样
        let peer = Arc::new(Mutex::new(Some(rx.local_addr())));
        let up = Uplink::spawn(sock.try_clone().unwrap(), peer, 44100.0, 2, Source::TestTone).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        drop(up);
        std::thread::sleep(Duration::from_millis(50));

        assert!(stats.mic_request_us.load(Relaxed) > 0);
        let frames = cons.slots();
        // 约 0.4 秒 × 48000，允许调度误差
        assert!(frames > 12000 && frames < 24000, "frames={frames}");
        let mut v = vec![0f32; frames];
        cons.pop_entire_slice(&mut v).unwrap();
        let peak = v.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.05 && peak < 0.12, "peak={peak}");
        assert_eq!(stats.bad_packets.load(Relaxed), 0);
    }
}
