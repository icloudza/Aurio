//! 发送端：把采集到的 f32 采样打包后经 UDP 发出。

use crate::proto::{Format, Packetizer};
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{SocketAddr, UdpSocket};

/// DSCP EF（46），写进 IP 头的 TOS 字段要左移 2 位。
const DSCP_EF_TOS: u32 = 46 << 2;

pub struct UdpSender {
    sock: UdpSocket,
    dest: SocketAddr,
    pk: Packetizer,
    pub sent_packets: u64,
    pub send_errors: u64,
}

impl UdpSender {
    pub fn new(dest: SocketAddr, fmt: Format, stream_id: u8, frames_per_packet: usize) -> io::Result<Self> {
        let domain = if dest.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
        let s = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        if dest.is_ipv4() {
            // 家用路由器多半不认，Windows 上没有组策略时也会被忽略，失败不影响使用
            let _ = s.set_tos_v4(DSCP_EF_TOS);
        }
        let bind: SocketAddr = if dest.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() };
        s.bind(&bind.into())?;
        let sock: UdpSocket = s.into();
        let mut pk = Packetizer::new(fmt, stream_id, frames_per_packet);
        pk.set_start_index(crate::proto::time_based_start_index(fmt.rate));
        Ok(UdpSender { sock, dest, pk, sent_packets: 0, send_errors: 0 })
    }

    pub fn packetizer(&mut self) -> &mut Packetizer {
        &mut self.pk
    }

    pub fn socket(&self) -> &UdpSocket {
        &self.sock
    }

    pub fn dest(&self) -> SocketAddr {
        self.dest
    }

    /// 发送交错 f32 采样，凑满一包发一包。
    pub fn send(&mut self, samples: &[f32]) {
        let sock = &self.sock;
        let dest = self.dest;
        let mut ok = 0;
        let mut err = 0;
        self.pk.push(samples, |b| match sock.send_to(b, dest) {
            Ok(_) => ok += 1,
            Err(_) => err += 1,
        });
        self.sent_packets += ok;
        self.send_errors += err;
    }
}
