//! 在线更新的公共部分，两端和打包工具共用，不依赖操作系统。
//!
//! 发布时 CI 往 GitHub Release 上传：
//! - 安装包本身（Windows 的 `Aurio-Setup-x.y.z.exe`、macOS 的 `Aurio-x.y.z.dmg`）
//! - `latest.json`：版本清单，列出每个平台的文件、大小、SHA-256 和 Ed25519 签名
//! - `latest.json.sig`：清单本身的 Ed25519 签名（防止有人拿旧清单冒充新版本）
//! - `appcast.xml`：给 macOS 的 Sparkle 用，签名格式和这里相同
//!
//! 客户端通过 `releases/latest/download/<文件>` 这个固定地址取最新版，先走国内镜像，
//! 失败了再走 GitHub 官方。镜像是第三方运营的，所以下载的每样东西都要验签：
//! 公钥内置在程序里，镜像改动任何一个字节都会被发现。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

#[cfg(feature = "client")]
pub mod client;

/// 发布用的 GitHub 仓库
pub const REPO: &str = "icloudza/Aurio";

/// 更新签名公钥（Ed25519，base64）。私钥只在 CI 的 Secrets 里（UPDATE_SIGNING_KEY）。
/// macOS 的 Info.plist 里 SUPublicEDKey 必须是同一个值。
pub const PUBLIC_KEY: &str = "DssE8uUdmQ2exfmQWbQsxQ0euPzjuAknXS/mKsxZmpo=";

/// 内置的国内加速镜像（前缀式：镜像地址 + 完整的 GitHub 地址），按顺序尝试，最后是官方源。
/// 清单里的 `mirrors` 字段可以在不发新版的情况下替换这个列表。
pub const DEFAULT_MIRRORS: &[&str] = &["https://gh-proxy.com/", "https://gh.llkk.cc/", "https://ghfast.top/"];

/// 清单文件名
pub const MANIFEST: &str = "latest.json";
pub const MANIFEST_SIG: &str = "latest.json.sig";

/// 平台键
pub const PLATFORM_WINDOWS: &str = "windows-x86_64";
pub const PLATFORM_MACOS: &str = "macos-arm64";

// ———————————————————— 清单 ————————————————————

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// 新版本号，例如 "0.2.7"
    pub version: String,
    /// 发布时间（RFC 3339）
    #[serde(default)]
    pub pub_date: String,
    /// 更新说明（Markdown，取自 CHANGELOG.md）
    #[serde(default)]
    pub notes: String,
    /// 可选：替换内置镜像列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
    pub platforms: BTreeMap<String, Asset>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    /// 文件名
    pub file: String,
    /// GitHub 官方下载地址
    pub url: String,
    pub size: u64,
    /// 小写十六进制
    pub sha256: String,
    /// 对文件内容的 Ed25519 签名（base64）
    pub signature: String,
}

impl Manifest {
    /// 解析并验证清单：签名不对就拒绝。
    pub fn parse_verified(json: &[u8], sig: &str, public_key: &str) -> Result<Manifest, Error> {
        verify(public_key, json, sig.trim())?;
        serde_json::from_slice(json).map_err(|e| Error::Format(e.to_string()))
    }

    /// 实际使用的镜像列表：清单里给了就用清单的
    pub fn mirrors(&self) -> Vec<String> {
        if self.mirrors.is_empty() {
            DEFAULT_MIRRORS.iter().map(|s| s.to_string()).collect()
        } else {
            self.mirrors.clone()
        }
    }
}

impl Asset {
    /// 校验下载到的文件：大小、SHA-256、签名都要对上。
    pub fn verify(&self, data: &[u8], public_key: &str) -> Result<(), Error> {
        if data.len() as u64 != self.size {
            return Err(Error::Mismatch(format!("大小不对：应为 {} 字节，实际 {} 字节", self.size, data.len())));
        }
        let hash = sha256_hex(data);
        if !hash.eq_ignore_ascii_case(&self.sha256) {
            return Err(Error::Mismatch("SHA-256 不对".into()));
        }
        verify(public_key, data, &self.signature)
    }
}

// ———————————————————— 下载来源 ————————————————————

/// 一个下载来源：镜像（前缀）或官方源
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// 前缀，例如 "https://gh-proxy.com/"；官方源为空
    pub prefix: String,
}

impl Source {
    pub fn official() -> Source {
        Source { prefix: String::new() }
    }

    pub fn is_official(&self) -> bool {
        self.prefix.is_empty()
    }

    /// 给界面看的名字
    pub fn name(&self) -> String {
        if self.is_official() {
            return "GitHub 官方".into();
        }
        self.prefix
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string()
    }

    /// 把 GitHub 地址换成经过这个来源的地址
    pub fn url(&self, github_url: &str) -> String {
        format!("{}{}", self.prefix, github_url)
    }
}

/// 按顺序尝试的来源：先镜像，最后官方。`preferred` 是上次成功的来源，排到最前面。
pub fn sources(mirrors: &[String], preferred: Option<&Source>) -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();
    let mut push = |s: Source| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    if let Some(p) = preferred {
        push(p.clone());
    }
    for m in mirrors {
        let m = m.trim();
        // 只接受 https；本机地址（测试用）允许 http
        let local = m.starts_with("http://127.0.0.1") || m.starts_with("http://localhost");
        if m.starts_with("https://") || local {
            let prefix = if m.ends_with('/') { m.to_string() } else { format!("{m}/") };
            push(Source { prefix });
        }
    }
    push(Source::official());
    out
}

/// 最新版某个文件的固定地址
pub fn latest_url(file: &str) -> String {
    format!("https://github.com/{REPO}/releases/latest/download/{file}")
}

/// 某个版本某个文件的地址
pub fn release_url(tag: &str, file: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{tag}/{file}")
}

// ———————————————————— 版本号 ————————————————————

/// 比较 "x.y.z"：candidate 比 current 新返回 true。非数字部分（例如 -beta）忽略。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

fn parse_version(v: &str) -> Vec<u64> {
    let mut parts: Vec<u64> = v
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map_while(|p| p.parse().ok())
        .collect();
    while parts.last() == Some(&0) {
        parts.pop();
    }
    parts
}

// ———————————————————— 签名 ————————————————————

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// 签名不对，或者密钥格式不对
    Signature(String),
    /// 大小或哈希对不上
    Mismatch(String),
    /// 清单格式不对
    Format(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Signature(s) => write!(f, "签名校验失败：{s}"),
            Error::Mismatch(s) => write!(f, "文件校验失败：{s}"),
            Error::Format(s) => write!(f, "清单格式不对：{s}"),
        }
    }
}

impl std::error::Error for Error {}

fn decode32(b64: &str, what: &str) -> Result<[u8; 32], Error> {
    let bytes = B64.decode(b64.trim()).map_err(|_| Error::Signature(format!("{what}不是有效的 base64")))?;
    bytes.try_into().map_err(|_| Error::Signature(format!("{what}长度不对")))
}

/// 用公钥验证签名（标准 Ed25519，和 Sparkle 的 EdDSA 签名相同）
pub fn verify(public_key: &str, data: &[u8], signature: &str) -> Result<(), Error> {
    let key = VerifyingKey::from_bytes(&decode32(public_key, "公钥")?)
        .map_err(|_| Error::Signature("公钥无效".into()))?;
    let sig_bytes = B64
        .decode(signature.trim())
        .map_err(|_| Error::Signature("签名不是有效的 base64".into()))?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|_| Error::Signature("签名长度不对".into()))?;
    key.verify(data, &sig).map_err(|_| Error::Signature("签名和内容不匹配".into()))
}

/// 用私钥（32 字节种子的 base64）签名，返回 base64 签名
pub fn sign(secret_key: &str, data: &[u8]) -> Result<String, Error> {
    let key = SigningKey::from_bytes(&decode32(secret_key, "私钥")?);
    Ok(B64.encode(key.sign(data).to_bytes()))
}

/// 从 32 字节随机种子生成密钥对，返回 (私钥, 公钥)，都是 base64
pub fn keypair_from_seed(seed: [u8; 32]) -> (String, String) {
    let key = SigningKey::from_bytes(&seed);
    (B64.encode(seed), B64.encode(key.verifying_key().to_bytes()))
}

/// 私钥对应的公钥
pub fn public_key_of(secret_key: &str) -> Result<String, Error> {
    let key = SigningKey::from_bytes(&decode32(secret_key, "私钥")?);
    Ok(B64.encode(key.verifying_key().to_bytes()))
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (String, String) {
        keypair_from_seed([7u8; 32])
    }

    #[test]
    fn sign_and_verify() {
        let (sk, pk) = keys();
        let sig = sign(&sk, b"hello").unwrap();
        assert!(verify(&pk, b"hello", &sig).is_ok());
        assert!(verify(&pk, b"hellO", &sig).is_err());
        assert_eq!(public_key_of(&sk).unwrap(), pk);
    }

    /// RFC 8032 测试向量 1：确认是标准 Ed25519（Sparkle 能验证）
    #[test]
    fn rfc8032_vector() {
        let seed: [u8; 32] = hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let (sk, pk) = keypair_from_seed(seed);
        assert_eq!(B64.decode(&pk).unwrap(), hex::<32>("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"));
        let sig = B64.decode(sign(&sk, b"").unwrap()).unwrap();
        assert_eq!(
            sig,
            hex::<64>("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b")
        );
    }

    fn hex<const N: usize>(s: &str) -> [u8; N] {
        let v: Vec<u8> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
        v.try_into().unwrap()
    }

    #[test]
    fn manifest_roundtrip_and_tamper() {
        let (sk, pk) = keys();
        let data = b"installer bytes".to_vec();
        let asset = Asset {
            file: "Aurio-Setup-0.2.7.exe".into(),
            url: release_url("v0.2.7", "Aurio-Setup-0.2.7.exe"),
            size: data.len() as u64,
            sha256: sha256_hex(&data),
            signature: sign(&sk, &data).unwrap(),
        };
        let m = Manifest {
            version: "0.2.7".into(),
            pub_date: String::new(),
            notes: "- 修复".into(),
            mirrors: vec![],
            platforms: [(PLATFORM_WINDOWS.to_string(), asset.clone())].into_iter().collect(),
        };
        let json = serde_json::to_vec_pretty(&m).unwrap();
        let sig = sign(&sk, &json).unwrap();
        assert_eq!(Manifest::parse_verified(&json, &sig, &pk).unwrap(), m);

        // 清单被改：拒绝
        let mut bad = json.clone();
        let i = bad.iter().position(|&b| b == b'7').unwrap();
        bad[i] = b'9';
        assert!(Manifest::parse_verified(&bad, &sig, &pk).is_err());

        // 文件被改：拒绝
        assert!(asset.verify(&data, &pk).is_ok());
        let mut d2 = data.clone();
        d2[0] ^= 1;
        assert!(asset.verify(&d2, &pk).is_err());
        assert!(asset.verify(&data[1..], &pk).is_err());
    }

    #[test]
    fn versions() {
        assert!(is_newer("0.2.7", "0.2.6"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0", "0.99.99"));
        assert!(!is_newer("0.2.6", "0.2.6"));
        assert!(!is_newer("0.2.6.0", "0.2.6"));
        assert!(!is_newer("0.2.5", "0.2.6"));
    }

    #[test]
    fn source_order() {
        let mirrors = vec![
            "https://a.example".to_string(),
            "https://b.example/".to_string(),
            "http://insecure.example/".to_string(),
        ];
        let s = sources(&mirrors, Some(&Source { prefix: "https://b.example/".into() }));
        let names: Vec<String> = s.iter().map(|s| s.name()).collect();
        assert_eq!(names, ["b.example", "a.example", "GitHub 官方"]);
        assert_eq!(s[0].url("https://github.com/x"), "https://b.example/https://github.com/x");
        assert_eq!(Source::official().url("https://github.com/x"), "https://github.com/x");
        // 本机测试地址允许 http
        let s = sources(&["http://127.0.0.1:8000".to_string()], None);
        assert_eq!(s[0].prefix, "http://127.0.0.1:8000/");
    }
}
