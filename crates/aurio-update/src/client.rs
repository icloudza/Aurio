//! 下载端：取清单、下载安装包。按「镜像 → 官方」依次尝试，支持续传，慢了或错了就换来源。
//! 只在开启 `client` 功能时编译（Windows 端用；Mac 端的下载交给 Sparkle）。

use crate::{latest_url, sources, Asset, Manifest, Source, MANIFEST, MANIFEST_SIG};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 下载速度低于这个值（字节/秒）就换下一个来源
pub const SLOW_BPS: u64 = 32 * 1024;
/// 判断「慢」之前先给多少时间热身
pub const SLOW_GRACE: Duration = Duration::from_secs(8);

/// 下载进度
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub source: String,
    pub received: u64,
    pub total: u64,
    /// 字节/秒
    pub speed: f64,
}

pub struct Client {
    http: reqwest::blocking::Client,
    public_key: String,
    mirrors: Vec<String>,
}

impl Client {
    /// `user_agent` 例如 "Aurio/0.2.7 (Windows)"
    pub fn new(user_agent: &str, public_key: &str) -> Result<Client, String> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(Duration::from_secs(8))
            // 整个请求的上限；慢速由下面的速度检测提前放弃
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Client { http, public_key: public_key.to_string(), mirrors: mirrors_from_env() })
    }

    /// 依次从各来源取清单，返回第一个签名有效的清单和它的来源
    pub fn fetch_manifest(&self, preferred: Option<&Source>) -> Result<(Manifest, Source), String> {
        let mut errors = Vec::new();
        for src in sources(&self.mirrors, preferred) {
            let r = self.get_small(&src.url(&latest_url(MANIFEST))).and_then(|json| {
                let sig = self.get_small(&src.url(&latest_url(MANIFEST_SIG)))?;
                Manifest::parse_verified(&json, &String::from_utf8_lossy(&sig), &self.public_key).map_err(|e| e.to_string())
            });
            match r {
                Ok(m) => return Ok((m, src)),
                Err(e) => errors.push(format!("{}：{e}", src.name())),
            }
        }
        Err(format!("检查更新失败（{}）", errors.join("；")))
    }

    fn get_small(&self, url: &str) -> Result<Vec<u8>, String> {
        let r = self.http.get(url).timeout(Duration::from_secs(12)).send().map_err(|e| net_err(&e))?;
        let code = r.status().as_u16();
        if code != 200 {
            return Err(format!("HTTP {code}"));
        }
        let body = r.bytes().map_err(|e| net_err(&e))?;
        if body.len() > 1 << 20 {
            return Err("清单太大".into());
        }
        Ok(body.to_vec())
    }

    /// 下载并校验安装包，保存到 `dir/<文件名>`。返回文件路径和成功的来源。
    /// 清单里的镜像列表优先于内置列表；`preferred`（例如取清单时成功的来源）排在最前。
    pub fn download(
        &self,
        manifest: &Manifest,
        asset: &Asset,
        dir: &Path,
        preferred: Option<&Source>,
        progress: &dyn Fn(&Progress),
    ) -> Result<(PathBuf, Source), String> {
        fs::create_dir_all(dir).map_err(|e| format!("建不了下载目录：{e}"))?;
        let dest = dir.join(&asset.file);
        let part = dir.join(format!("{}.part", asset.file));
        // 之前已经下载并验证过
        if let Ok(data) = fs::read(&dest) {
            if asset.verify(&data, &self.public_key).is_ok() {
                return Ok((dest, preferred.cloned().unwrap_or_else(Source::official)));
            }
            let _ = fs::remove_file(&dest);
        }
        let mirrors = if std::env::var("AURIO_UPDATE_MIRRORS").is_ok() { self.mirrors.clone() } else { manifest.mirrors() };
        let mut errors = Vec::new();
        for src in sources(&mirrors, preferred) {
            match self.download_from(&src, asset, &part, progress) {
                Ok(()) => {
                    let data = fs::read(&part).map_err(|e| e.to_string())?;
                    match asset.verify(&data, &self.public_key) {
                        Ok(()) => {
                            fs::rename(&part, &dest).map_err(|e| format!("保存安装包失败：{e}"))?;
                            return Ok((dest, src));
                        }
                        Err(e) => {
                            // 这个来源给了不对的文件：丢掉，换下一个来源从头下
                            let _ = fs::remove_file(&part);
                            errors.push(format!("{}：{e}", src.name()));
                        }
                    }
                }
                Err(e) => errors.push(format!("{}：{e}", src.name())),
            }
        }
        Err(format!("下载失败（{}）", errors.join("；")))
    }

    fn download_from(&self, src: &Source, asset: &Asset, part: &Path, progress: &dyn Fn(&Progress)) -> Result<(), String> {
        let mut have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if have > asset.size {
            have = 0;
        }
        if have == asset.size {
            return Ok(());
        }
        let mut req = self.http.get(src.url(&asset.url));
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let mut resp = req.send().map_err(|e| net_err(&e))?;
        let code = resp.status().as_u16();
        let (mut file, mut got) = match code {
            206 if have > 0 => (OpenOptions::new().append(true).open(part).map_err(|e| e.to_string())?, have),
            200 => (File::create(part).map_err(|e| e.to_string())?, 0),
            _ => return Err(format!("HTTP {code}")),
        };
        let mut p = Progress { source: src.name(), received: got, total: asset.size, speed: 0.0 };
        progress(&p);
        let started = Instant::now();
        let mut window = Instant::now();
        let mut window_bytes = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = resp.read(&mut buf).map_err(|e| format!("连接中断：{e}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| format!("写文件失败：{e}"))?;
            got += n as u64;
            window_bytes += n as u64;
            if got > asset.size {
                return Err("返回的数据比预期多".into());
            }
            let w = window.elapsed();
            if w >= Duration::from_millis(1500) {
                let bps = window_bytes as f64 / w.as_secs_f64();
                p.received = got;
                p.speed = bps;
                progress(&p);
                if started.elapsed() > SLOW_GRACE && (bps as u64) < SLOW_BPS {
                    return Err(format!("速度太慢（{:.0} KB/s）", bps / 1024.0));
                }
                window = Instant::now();
                window_bytes = 0;
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        p.received = got;
        progress(&p);
        if got != asset.size {
            return Err(format!("下载不完整（{got} / {} 字节）", asset.size));
        }
        Ok(())
    }
}

/// 测试用：AURIO_UPDATE_MIRRORS=逗号分隔的镜像前缀，替换内置列表（本机地址允许 http）
fn mirrors_from_env() -> Vec<String> {
    match std::env::var("AURIO_UPDATE_MIRRORS") {
        Ok(v) => v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        Err(_) => crate::DEFAULT_MIRRORS.iter().map(|s| s.to_string()).collect(),
    }
}

fn net_err(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "超时".into()
    } else if e.is_connect() {
        "连不上".into()
    } else {
        e.to_string()
    }
}
