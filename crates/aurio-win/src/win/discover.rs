//! 用 mDNS（Bonjour）发现局域网里的 Mac。Mac 端 App 广播 `_aurio._udp`。

use super::common::Res;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::Serialize;
use std::time::{Duration, Instant};

pub const SERVICE: &str = "_aurio._udp.local.";

#[derive(Clone, Debug, Serialize)]
pub struct FoundMac {
    /// 电脑名，比如「Mac Studio」
    pub name: String,
    /// 首选地址「IP:端口」
    pub addr: String,
    /// 所有 IPv4 地址
    pub ips: Vec<String>,
    pub port: u16,
}

/// 浏览 `timeout` 时长，返回找到的 Mac（按名称排序）。
pub fn discover(timeout: Duration) -> Res<Vec<FoundMac>> {
    let mdns = ServiceDaemon::new()?;
    let rx = mdns.browse(SERVICE)?;
    let deadline = Instant::now() + timeout;
    let mut found: Vec<FoundMac> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let mut ips: Vec<String> = info.get_addresses_v4().iter().map(|a| a.to_string()).collect();
                ips.sort();
                if ips.is_empty() {
                    continue;
                }
                let name = info
                    .get_fullname()
                    .strip_suffix(&format!(".{SERVICE}"))
                    .unwrap_or(info.get_fullname())
                    .replace("\\032", " ");
                let port = info.get_port();
                found.retain(|f| f.name != name);
                found.push(FoundMac { addr: format!("{}:{port}", ips[0]), name, ips, port });
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = mdns.stop_browse(SERVICE);
    let _ = mdns.shutdown();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}
