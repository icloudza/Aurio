//! 传输协议：24 字节固定包头 + PCM 负载。全部字段小端序。
//!
//! | 偏移 | 长度 | 字段         |
//! |------|------|--------------|
//! | 0    | 2    | magic `ND`   |
//! | 2    | 1    | version      |
//! | 3    | 1    | type         |
//! | 4    | 1    | stream_id    |
//! | 5    | 1    | flags        |
//! | 6    | 2    | reserved     |
//! | 8    | 4    | seq          |
//! | 12   | 8    | sample_index |
//! | 20   | 2    | frames       |
//! | 22   | 2    | format_id    |

pub const MAGIC: [u8; 2] = *b"ND";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 24;
pub const DEFAULT_PORT: u16 = 47800;
/// 以太网 MTU 1500 减去 IP/UDP 头后的安全上限。
pub const MAX_PACKET: usize = 1472;
pub const DEFAULT_FRAMES_PER_PACKET: usize = 120;

pub mod ptype {
    pub const AUDIO: u8 = 1;
    pub const HEARTBEAT: u8 = 2;
    pub const CONTROL: u8 = 3;
}

pub mod stream {
    /// Windows → Mac（播放方向）
    pub const PLAYBACK: u8 = 0;
    /// Mac → Windows（麦克风方向）
    pub const MIC: u8 = 1;
}

pub mod flags {
    pub const MUTED: u8 = 1 << 0;
    pub const FORMAT_CHANGED: u8 = 1 << 1;
    /// 发送端的采集出现了断档（例如 WASAPI 报告 discontinuity）。
    pub const DISCONTINUITY: u8 = 1 << 2;
    /// 心跳包：Windows 端有应用在用麦克风，请 Mac 打开麦克风上行。
    pub const MIC_REQUEST: u8 = 1 << 3;
    /// 心跳回声：收到心跳后原样回给对方（seq 不变），对方据此判断连通并测往返时间。
    pub const HEARTBEAT_ACK: u8 = 1 << 4;
    /// 心跳回声里：回应方（Mac）正在做回声消除
    pub const AEC_ACTIVE: u8 = 1 << 5;
}

/// 用当前时间生成流的起始 sample_index。
/// 发送端重启后序号一定比之前大，接收端会正常重新对齐，不会把新数据当成迟到包丢掉。
pub fn time_based_start_index(rate: u32) -> u64 {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ms * rate as u64 / 1000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub ptype: u8,
    pub stream_id: u8,
    pub flags: u8,
    pub seq: u32,
    pub sample_index: u64,
    pub frames: u16,
    pub format_id: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    TooShort,
    BadMagic,
    BadVersion(u8),
    BadLength,
}

impl Header {
    pub fn write(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&MAGIC);
        buf[2] = VERSION;
        buf[3] = self.ptype;
        buf[4] = self.stream_id;
        buf[5] = self.flags;
        buf[6..8].copy_from_slice(&[0, 0]);
        buf[8..12].copy_from_slice(&self.seq.to_le_bytes());
        buf[12..20].copy_from_slice(&self.sample_index.to_le_bytes());
        buf[20..22].copy_from_slice(&self.frames.to_le_bytes());
        buf[22..24].copy_from_slice(&self.format_id.to_le_bytes());
    }

    pub fn parse(buf: &[u8]) -> Result<Header, ParseError> {
        if buf.len() < HEADER_LEN {
            return Err(ParseError::TooShort);
        }
        if buf[0..2] != MAGIC {
            return Err(ParseError::BadMagic);
        }
        if buf[2] != VERSION {
            return Err(ParseError::BadVersion(buf[2]));
        }
        Ok(Header {
            ptype: buf[3],
            stream_id: buf[4],
            flags: buf[5],
            seq: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            sample_index: u64::from_le_bytes(buf[12..20].try_into().unwrap()),
            frames: u16::from_le_bytes(buf[20..22].try_into().unwrap()),
            format_id: u16::from_le_bytes(buf[22..24].try_into().unwrap()),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    S16,
    S24,
}

impl SampleFormat {
    pub const fn bytes(self) -> usize {
        match self {
            SampleFormat::S16 => 2,
            SampleFormat::S24 => 3,
        }
    }
}

/// 握手前先用固定格式表；以后协商时 format_id 指向协商结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub id: u16,
    pub rate: u32,
    pub channels: u16,
    pub sample: SampleFormat,
}

impl Format {
    pub const fn bytes_per_frame(&self) -> usize {
        self.channels as usize * self.sample.bytes()
    }

    pub const fn max_frames_per_packet(&self) -> usize {
        (MAX_PACKET - HEADER_LEN) / self.bytes_per_frame()
    }
}

pub const FORMATS: &[Format] = &[
    Format { id: 1, rate: 48000, channels: 2, sample: SampleFormat::S24 },
    Format { id: 2, rate: 48000, channels: 1, sample: SampleFormat::S24 },
    Format { id: 3, rate: 48000, channels: 2, sample: SampleFormat::S16 },
    Format { id: 4, rate: 48000, channels: 1, sample: SampleFormat::S16 },
    Format { id: 5, rate: 44100, channels: 2, sample: SampleFormat::S24 },
];

pub const FORMAT_48K_S24_STEREO: Format = FORMATS[0];
pub const FORMAT_48K_S24_MONO: Format = FORMATS[1];

pub fn format_by_id(id: u16) -> Option<Format> {
    FORMATS.iter().copied().find(|f| f.id == id)
}

const S24_MAX: f32 = 8_388_607.0;
const S16_MAX: f32 = 32_767.0;

/// f32 交错采样 → 线上格式。返回写入的字节数。
pub fn encode_samples(sample: SampleFormat, src: &[f32], dst: &mut [u8]) -> usize {
    match sample {
        SampleFormat::S24 => {
            for (s, d) in src.iter().zip(dst.chunks_exact_mut(3)) {
                let v = (s.clamp(-1.0, 1.0) * S24_MAX).round() as i32;
                let b = v.to_le_bytes();
                d.copy_from_slice(&b[0..3]);
            }
            src.len() * 3
        }
        SampleFormat::S16 => {
            for (s, d) in src.iter().zip(dst.chunks_exact_mut(2)) {
                let v = (s.clamp(-1.0, 1.0) * S16_MAX).round() as i16;
                d.copy_from_slice(&v.to_le_bytes());
            }
            src.len() * 2
        }
    }
}

/// 线上格式 → f32 交错采样。返回解出的采样数。
pub fn decode_samples(sample: SampleFormat, src: &[u8], dst: &mut [f32]) -> usize {
    match sample {
        SampleFormat::S24 => {
            let n = (src.len() / 3).min(dst.len());
            for (d, s) in dst[..n].iter_mut().zip(src.chunks_exact(3)) {
                let v = i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8;
                *d = v as f32 / 8_388_608.0;
            }
            n
        }
        SampleFormat::S16 => {
            let n = (src.len() / 2).min(dst.len());
            for (d, s) in dst[..n].iter_mut().zip(src.chunks_exact(2)) {
                *d = i16::from_le_bytes([s[0], s[1]]) as f32 / 32_768.0;
            }
            n
        }
    }
}

/// 把连续的 f32 采样切成固定帧数的音频包。
pub struct Packetizer {
    fmt: Format,
    stream_id: u8,
    frames_per_packet: usize,
    seq: u32,
    sample_index: u64,
    pending: Vec<f32>,
    pending_frames: usize,
    next_flags: u8,
    buf: Vec<u8>,
}

impl Packetizer {
    pub fn new(fmt: Format, stream_id: u8, frames_per_packet: usize) -> Self {
        let fpp = frames_per_packet.clamp(1, fmt.max_frames_per_packet());
        Packetizer {
            fmt,
            stream_id,
            frames_per_packet: fpp,
            seq: 0,
            sample_index: 0,
            pending: vec![0.0; fpp * fmt.channels as usize],
            pending_frames: 0,
            next_flags: 0,
            buf: vec![0; HEADER_LEN + fpp * fmt.bytes_per_frame()],
        }
    }

    pub fn format(&self) -> Format {
        self.fmt
    }

    pub fn frames_per_packet(&self) -> usize {
        self.frames_per_packet
    }

    pub fn sample_index(&self) -> u64 {
        self.sample_index
    }

    /// 设置下一个包的 sample_index（只应在开始发送前调用）。
    pub fn set_start_index(&mut self, index: u64) {
        self.sample_index = index;
    }

    /// 下一个发出的包带上 flags（例如 DISCONTINUITY）。
    pub fn set_next_flags(&mut self, f: u8) {
        self.next_flags |= f;
    }

    /// 发送端漏掉了 n 帧（例如采集断档）。sample_index 往前跳，接收端会补静音。
    pub fn skip_frames(&mut self, n: u64) {
        self.sample_index += n;
        self.next_flags |= flags::DISCONTINUITY;
    }

    /// 输入交错 f32 采样（长度应为声道数的整数倍，多出的半帧会被丢弃），
    /// 每凑满一个包就调用一次 `emit`。
    pub fn push(&mut self, samples: &[f32], mut emit: impl FnMut(&[u8])) {
        let ch = self.fmt.channels as usize;
        let mut src = &samples[..samples.len() - samples.len() % ch];
        while !src.is_empty() {
            let room_frames = self.frames_per_packet - self.pending_frames;
            let n = room_frames.min(src.len() / ch) * ch;
            let start = self.pending_frames * ch;
            self.pending[start..start + n].copy_from_slice(&src[..n]);
            self.pending_frames += n / ch;
            src = &src[n..];
            if self.pending_frames == self.frames_per_packet {
                let len = self.build_packet();
                emit(&self.buf[..len]);
            }
        }
    }

    fn build_packet(&mut self) -> usize {
        let frames = self.pending_frames;
        let h = Header {
            ptype: ptype::AUDIO,
            stream_id: self.stream_id,
            flags: self.next_flags,
            seq: self.seq,
            sample_index: self.sample_index,
            frames: frames as u16,
            format_id: self.fmt.id,
        };
        h.write(&mut self.buf);
        let ch = self.fmt.channels as usize;
        let n = encode_samples(
            self.fmt.sample,
            &self.pending[..frames * ch],
            &mut self.buf[HEADER_LEN..],
        );
        self.seq = self.seq.wrapping_add(1);
        self.sample_index += frames as u64;
        self.pending_frames = 0;
        self.next_flags = 0;
        HEADER_LEN + n
    }
}

/// 构造一个心跳包（不带负载）。
pub fn heartbeat(stream_id: u8, flags: u8, seq: u32, buf: &mut [u8; HEADER_LEN]) {
    Header {
        ptype: ptype::HEARTBEAT,
        stream_id,
        flags,
        seq,
        sample_index: 0,
        frames: 0,
        format_id: 0,
    }
    .write(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = Header {
            ptype: ptype::AUDIO,
            stream_id: 1,
            flags: flags::MUTED,
            seq: 0xdead_beef,
            sample_index: 0x0123_4567_89ab_cdef,
            frames: 120,
            format_id: 1,
        };
        let mut b = [0u8; HEADER_LEN];
        h.write(&mut b);
        assert_eq!(&b[0..2], b"ND");
        assert_eq!(Header::parse(&b).unwrap(), h);
        b[0] = b'X';
        assert_eq!(Header::parse(&b), Err(ParseError::BadMagic));
    }

    #[test]
    fn s24_roundtrip() {
        let src = [0.0f32, 0.5, -0.5, 0.999, -1.0, 1e-6];
        let mut bytes = [0u8; 18];
        assert_eq!(encode_samples(SampleFormat::S24, &src, &mut bytes), 18);
        let mut dst = [0f32; 6];
        decode_samples(SampleFormat::S24, &bytes, &mut dst);
        for (a, b) in src.iter().zip(dst.iter()) {
            assert!((a - b).abs() < 2.0 / 8_388_608.0, "{a} vs {b}");
        }
    }

    #[test]
    fn packetizer_splits_and_counts() {
        let fmt = FORMAT_48K_S24_STEREO;
        let mut p = Packetizer::new(fmt, stream::PLAYBACK, 120);
        let input = vec![0.25f32; 480 * 2 + 10];
        let mut pkts = Vec::new();
        p.push(&input, |b| pkts.push(b.to_vec()));
        assert_eq!(pkts.len(), 4);
        for (i, pk) in pkts.iter().enumerate() {
            let h = Header::parse(pk).unwrap();
            assert_eq!(h.seq, i as u32);
            assert_eq!(h.sample_index, i as u64 * 120);
            assert_eq!(h.frames, 120);
            assert_eq!(pk.len(), HEADER_LEN + 120 * 6);
        }
        // 剩下 5 帧留在 pending 里
        p.push(&vec![0.0; 115 * 2], |b| pkts.push(b.to_vec()));
        assert_eq!(pkts.len(), 5);
        assert_eq!(Header::parse(&pkts[4]).unwrap().sample_index, 480);
    }
}
