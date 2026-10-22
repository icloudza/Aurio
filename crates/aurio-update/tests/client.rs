//! 下载端的集成测试：本机起一个 HTTP 服务器，模拟各种镜像。
//!
//! 镜像前缀 http://127.0.0.1:<端口>/<模式>/，模式：
//! - good    正常（支持 Range）
//! - error   一律返回 500
//! - badsig  清单签名不对
//! - corrupt 安装包内容被改了一个字节
//! - slow    每秒只给 4 KB
#![cfg(feature = "client")]

use aurio_update::client::Client;
use aurio_update::{self as up, Asset, Manifest};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FILE: &str = "Aurio-Setup-9.9.9.exe";

struct Fixture {
    installer: Vec<u8>,
    manifest: Vec<u8>,
    sig: String,
    log: Mutex<Vec<String>>,
}

fn fixture() -> (Arc<Fixture>, String) {
    let (sk, pk) = up::keypair_from_seed([3u8; 32]);
    let installer: Vec<u8> = (0..300_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let asset = Asset {
        file: FILE.into(),
        url: up::release_url("v9.9.9", FILE),
        size: installer.len() as u64,
        sha256: up::sha256_hex(&installer),
        signature: up::sign(&sk, &installer).unwrap(),
    };
    let m = Manifest {
        version: "9.9.9".into(),
        pub_date: String::new(),
        notes: "测试".into(),
        mirrors: vec![],
        platforms: [(up::PLATFORM_WINDOWS.to_string(), asset)].into_iter().collect(),
    };
    let manifest = serde_json::to_vec(&m).unwrap();
    let sig = up::sign(&sk, &manifest).unwrap();
    (Arc::new(Fixture { installer, manifest, sig, log: Mutex::new(Vec::new()) }), pk)
}

fn serve(fx: Arc<Fixture>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let fx = fx.clone();
            std::thread::spawn(move || handle(s, &fx));
        }
    });
    port
}

fn handle(mut s: TcpStream, fx: &Fixture) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let mut range: Option<u64> = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
            range = v.trim().trim_end_matches('-').parse().ok();
        }
    }
    let mode = path.trim_start_matches('/').split('/').next().unwrap_or("").to_string();
    fx.log.lock().unwrap().push(format!("{mode} {} {:?}", path.rsplit('/').next().unwrap_or(""), range));
    let reply = |s: &mut TcpStream, code: &str, body: &[u8]| {
        let _ = write!(s, "HTTP/1.1 {code}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        let _ = s.write_all(body);
    };
    if mode == "error" {
        return reply(&mut s, "500 Internal Server Error", b"no");
    }
    if path.ends_with("/latest.json") {
        return reply(&mut s, "200 OK", &fx.manifest);
    }
    if path.ends_with("/latest.json.sig") {
        let sig = if mode == "badsig" { up::sign(&up::keypair_from_seed([9u8; 32]).0, &fx.manifest).unwrap() } else { fx.sig.clone() };
        return reply(&mut s, "200 OK", sig.as_bytes());
    }
    if path.ends_with(FILE) {
        let mut data = fx.installer.clone();
        if mode == "corrupt" {
            data[1000] ^= 0xff;
        }
        if mode == "slow" {
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", data.len());
            for chunk in data.chunks(4096) {
                if s.write_all(chunk).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            return;
        }
        if let Some(from) = range {
            let body = &data[from as usize..];
            let _ = write!(
                s,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{}\r\nConnection: close\r\n\r\n",
                body.len(),
                data.len() - 1,
                data.len()
            );
            let _ = s.write_all(body);
            return;
        }
        return reply(&mut s, "200 OK", &data);
    }
    reply(&mut s, "404 Not Found", b"")
}

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aurio-update-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// 所有场景放在一个测试里：用到了进程级的环境变量
#[test]
fn mirrors_fallback_verify_resume() {
    let (fx, pk) = fixture();
    let port = serve(fx.clone());
    let m = |mode: &str| format!("http://127.0.0.1:{port}/{mode}/");
    // 连不上的端口
    let dead = "http://127.0.0.1:9/".to_string();

    // 1. 清单：连不上、500、签名不对都要跳过，落到 good
    std::env::set_var("AURIO_UPDATE_MIRRORS", [dead.clone(), m("error"), m("badsig"), m("good")].join(","));
    let c = Client::new("Aurio-test", &pk).unwrap();
    let (manifest, src) = c.fetch_manifest(None).unwrap();
    assert_eq!(manifest.version, "9.9.9");
    assert_eq!(src.prefix, m("good"));

    // 2. 下载：被篡改的安装包要被丢弃，换到 good
    std::env::set_var("AURIO_UPDATE_MIRRORS", [m("corrupt"), m("good")].join(","));
    let c = Client::new("Aurio-test", &pk).unwrap();
    let asset = manifest.platforms[up::PLATFORM_WINDOWS].clone();
    let dir = tmp_dir("fallback");
    let (path, src) = c.download(&manifest, &asset, &dir, None, &|_| {}).unwrap();
    assert_eq!(src.prefix, m("good"));
    assert_eq!(std::fs::read(&path).unwrap(), fx.installer);
    assert!(!dir.join(format!("{FILE}.part")).exists());

    // 3. 续传：.part 里已有前一半，good 只应该收到从中间开始的 Range 请求
    let dir = tmp_dir("resume");
    std::fs::create_dir_all(&dir).unwrap();
    let half = fx.installer.len() / 2;
    std::fs::write(dir.join(format!("{FILE}.part")), &fx.installer[..half]).unwrap();
    std::env::set_var("AURIO_UPDATE_MIRRORS", m("good"));
    let c = Client::new("Aurio-test", &pk).unwrap();
    fx.log.lock().unwrap().clear();
    let (path, _) = c.download(&manifest, &asset, &dir, None, &|_| {}).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), fx.installer);
    let log = fx.log.lock().unwrap().clone();
    assert!(log.iter().any(|l| l == &format!("good {FILE} Some({half})")), "没有续传：{log:?}");

    // 4. 已经下载并验证过的文件：不再联网
    fx.log.lock().unwrap().clear();
    c.download(&manifest, &asset, &dir, None, &|_| {}).unwrap();
    assert!(fx.log.lock().unwrap().is_empty());

    // 5. 慢镜像：热身期过后放弃，换到 good
    let dir = tmp_dir("slow");
    std::env::set_var("AURIO_UPDATE_MIRRORS", [m("slow"), m("good")].join(","));
    let c = Client::new("Aurio-test", &pk).unwrap();
    let seen = Mutex::new(Vec::<String>::new());
    let (path, src) = c
        .download(&manifest, &asset, &dir, None, &|p| seen.lock().unwrap().push(p.source.clone()))
        .unwrap();
    assert_eq!(src.prefix, m("good"));
    assert_eq!(std::fs::read(&path).unwrap(), fx.installer);
    assert!(seen.lock().unwrap().iter().any(|s| s.contains("/slow")), "进度里应该先出现慢镜像");

    // 6. 全部失败：报错里列出每个来源的原因（官方源会因为测试版本不存在而失败）
    std::env::set_var("AURIO_UPDATE_MIRRORS", [dead, m("error")].join(","));
    let c = Client::new("Aurio-test", &pk).unwrap();
    let err = c.download(&manifest, &asset, &tmp_dir("fail"), None, &|_| {}).unwrap_err();
    assert!(err.contains("连不上") && err.contains("HTTP 500"), "{err}");
}
