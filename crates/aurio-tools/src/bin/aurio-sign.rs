//! 更新签名工具：生成密钥、签名文件、生成发布用的更新清单。
//!
//! 私钥从环境变量 AURIO_UPDATE_KEY（base64）或 --key-file 读取。

use aurio_tools::{die, Args};
use aurio_update::{self as up, Asset, Manifest};
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let a = Args::parse();
    let cmd = a.positional.first().map(|s| s.as_str()).unwrap_or("");
    match cmd {
        "keygen" => keygen(&a),
        "pubkey" => println!("{}", up::public_key_of(&secret(&a)).unwrap_or_else(|e| die(&e.to_string()))),
        "sign" => {
            let file = a.positional.get(1).unwrap_or_else(|| die("用法：aurio-sign sign <文件>"));
            println!("{}", sign_file(&secret(&a), Path::new(file)));
        }
        "verify" => verify(&a),
        "release" => release(&a),
        _ => {
            println!(
                "用法：\n  \
                 aurio-sign keygen <私钥文件>        生成新的签名密钥对，私钥写入文件，打印公钥\n  \
                 aurio-sign pubkey                   打印私钥对应的公钥\n  \
                 aurio-sign sign <文件>              签名文件，打印 base64 签名\n  \
                 aurio-sign verify <文件> <签名>     用内置公钥（或 --pubkey）验证签名\n  \
                 aurio-sign release [选项]           生成 latest.json、latest.json.sig、appcast.xml\n\n\
                 release 的选项：\n  \
                 --version x.y.z     版本号\n  \
                 --tag vx.y.z        Release 标签（默认 v + 版本号）\n  \
                 --notes 文件        更新说明（Markdown）\n  \
                 --date-iso 时间     发布时间，RFC 3339\n  \
                 --date-rfc2822 时间 发布时间，RFC 2822（appcast 用）\n  \
                 --windows 文件      Windows 安装包\n  \
                 --macos 文件        macOS 安装盘\n  \
                 --min-macos 版本    最低 macOS 版本（默认 13.0）\n  \
                 --out 目录          输出目录\n\n\
                 私钥从环境变量 AURIO_UPDATE_KEY 或 --key-file 读取。"
            );
        }
    }
}

fn secret(a: &Args) -> String {
    if a.has("key-file") {
        let path = a.str("key-file", "");
        return fs::read_to_string(&path).unwrap_or_else(|e| die(&format!("读不了私钥文件 {path}：{e}"))).trim().to_string();
    }
    std::env::var("AURIO_UPDATE_KEY").map(|s| s.trim().to_string()).unwrap_or_else(|_| die("没有私钥：设置 AURIO_UPDATE_KEY 或传 --key-file"))
}

fn keygen(a: &Args) {
    let path = a.positional.get(1).unwrap_or_else(|| die("用法：aurio-sign keygen <私钥文件>"));
    let path = Path::new(path);
    if path.exists() {
        die(&format!("{} 已经存在，不会覆盖（换一个文件名，或者先手动删除）", path.display()));
    }
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).unwrap_or_else(|e| die(&format!("取随机数失败：{e}")));
    let (sk, pk) = up::keypair_from_seed(seed);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    write_private(path, &format!("{sk}\n"));
    eprintln!("私钥已写入 {}（请备份，丢失后已安装的版本将无法验证新的更新）", path.display());
    println!("{pk}");
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap_or_else(|e| die(&format!("写不了 {}：{e}", path.display())));
    f.write_all(text.as_bytes()).unwrap_or_else(|e| die(&e.to_string()));
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) {
    fs::write(path, text).unwrap_or_else(|e| die(&format!("写不了 {}：{e}", path.display())));
}

fn sign_file(sk: &str, path: &Path) -> String {
    let data = fs::read(path).unwrap_or_else(|e| die(&format!("读不了 {}：{e}", path.display())));
    up::sign(sk, &data).unwrap_or_else(|e| die(&e.to_string()))
}

fn verify(a: &Args) {
    let (Some(file), Some(sig)) = (a.positional.get(1), a.positional.get(2)) else {
        die("用法：aurio-sign verify <文件> <签名>")
    };
    let pk = a.str("pubkey", up::PUBLIC_KEY);
    let data = fs::read(file).unwrap_or_else(|e| die(&format!("读不了 {file}：{e}")));
    match up::verify(&pk, &data, sig) {
        Ok(()) => println!("签名有效"),
        Err(e) => die(&e.to_string()),
    }
}

fn asset(sk: &str, path: &Path, tag: &str) -> Asset {
    let data = fs::read(path).unwrap_or_else(|e| die(&format!("读不了 {}：{e}", path.display())));
    let file = path.file_name().unwrap().to_string_lossy().to_string();
    Asset {
        url: up::release_url(tag, &file),
        file,
        size: data.len() as u64,
        sha256: up::sha256_hex(&data),
        signature: up::sign(sk, &data).unwrap_or_else(|e| die(&e.to_string())),
    }
}

fn release(a: &Args) {
    let sk = secret(a);
    let pk = up::public_key_of(&sk).unwrap_or_else(|e| die(&e.to_string()));
    // 私钥必须和程序里内置的公钥配对，否则已安装的程序会拒绝这次更新
    if pk != up::PUBLIC_KEY && !a.has("allow-key-mismatch") {
        die(&format!(
            "私钥对应的公钥是 {pk}，和程序内置的 {} 不一致：检查 UPDATE_SIGNING_KEY",
            up::PUBLIC_KEY
        ));
    }
    let version = a.str("version", "");
    if version.is_empty() {
        die("需要 --version");
    }
    let tag = a.str("tag", &format!("v{version}"));
    let notes = if a.has("notes") {
        let p = a.str("notes", "");
        fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("读不了 {p}：{e}"))).trim().to_string()
    } else {
        String::new()
    };
    let out = PathBuf::from(a.str("out", "."));
    fs::create_dir_all(&out).unwrap_or_else(|e| die(&e.to_string()));

    let mut platforms = std::collections::BTreeMap::new();
    let win = a.has("windows").then(|| asset(&sk, Path::new(&a.str("windows", "")), &tag));
    let mac = a.has("macos").then(|| asset(&sk, Path::new(&a.str("macos", "")), &tag));
    if let Some(w) = &win {
        platforms.insert(up::PLATFORM_WINDOWS.to_string(), w.clone());
    }
    if let Some(m) = &mac {
        platforms.insert(up::PLATFORM_MACOS.to_string(), m.clone());
    }
    if platforms.is_empty() {
        die("至少要给 --windows 或 --macos");
    }

    let manifest = Manifest { version: version.clone(), pub_date: a.str("date-iso", ""), notes: notes.clone(), mirrors: vec![], platforms };
    let json = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = up::sign(&sk, &json).unwrap();
    // 写之前自检一遍
    Manifest::parse_verified(&json, &sig, &pk).unwrap_or_else(|e| die(&format!("自检失败：{e}")));
    fs::write(out.join(up::MANIFEST), &json).unwrap();
    fs::write(out.join(up::MANIFEST_SIG), format!("{sig}\n")).unwrap();
    println!("写入 {}、{}", up::MANIFEST, up::MANIFEST_SIG);

    if let Some(m) = &mac {
        let xml = appcast(&version, &notes, &a.str("date-rfc2822", ""), &a.str("min-macos", "13.0"), m);
        fs::write(out.join("appcast.xml"), xml).unwrap();
        println!("写入 appcast.xml");
    }
}

/// Sparkle 的 appcast：只放最新一个版本
fn appcast(version: &str, notes: &str, date: &str, min_macos: &str, dmg: &Asset) -> String {
    let pub_date = if date.is_empty() { String::new() } else { format!("      <pubDate>{}</pubDate>\n", esc(date)) };
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Aurio</title>
    <item>
      <title>Aurio {v}</title>
{pub_date}      <sparkle:version>{v}</sparkle:version>
      <sparkle:shortVersionString>{v}</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>{min}</sparkle:minimumSystemVersion>
      <description><![CDATA[{html}]]></description>
      <enclosure url="{url}" length="{len}" type="application/octet-stream" sparkle:edSignature="{sig}"/>
    </item>
  </channel>
</rss>
"#,
        v = esc(version),
        min = esc(min_macos),
        html = markdown_html(notes).replace("]]>", "]]&gt;"),
        url = esc(&dmg.url),
        len = dmg.size,
        sig = esc(&dmg.signature),
    )
}

/// 够用的 Markdown → HTML：标题、列表、段落、`代码`、**粗体**
fn markdown_html(md: &str) -> String {
    let mut out = String::from("<div style=\"font-family:-apple-system,'PingFang SC',sans-serif;font-size:13px;line-height:1.55\">");
    let mut in_list = false;
    for line in md.lines() {
        let t = line.trim();
        let item = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "));
        if in_list && item.is_none() {
            out.push_str("</ul>");
            in_list = false;
        }
        if t.is_empty() {
            continue;
        }
        if let Some(item) = item {
            if !in_list {
                out.push_str("<ul style=\"padding-left:18px;margin:4px 0 10px\">");
                in_list = true;
            }
            out.push_str(&format!("<li>{}</li>", inline(item)));
        } else if let Some(h) = t.strip_prefix("### ").or_else(|| t.strip_prefix("## ")) {
            out.push_str(&format!("<h3 style=\"font-size:13px;margin:12px 0 4px\">{}</h3>", inline(h)));
        } else {
            out.push_str(&format!("<p style=\"margin:4px 0\">{}</p>", inline(t)));
        }
    }
    if in_list {
        out.push_str("</ul>");
    }
    out.push_str("</div>");
    out
}

fn inline(s: &str) -> String {
    let mut out = String::new();
    let mut code = false;
    let mut bold = false;
    let mut rest = esc(s);
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('`') {
            out.push_str(if code { "</code>" } else { "<code>" });
            code = !code;
            rest = r.to_string();
        } else if let Some(r) = rest.strip_prefix("**") {
            out.push_str(if bold { "</b>" } else { "<b>" });
            bold = !bold;
            rest = r.to_string();
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = rest[c.len_utf8()..].to_string();
        }
    }
    if code {
        out.push_str("</code>");
    }
    if bold {
        out.push_str("</b>");
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
