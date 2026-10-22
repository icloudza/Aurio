//! 命令行工具共用的小工具。

use std::collections::HashMap;

/// 极简参数解析：`--key value` 或 `--flag`。
pub struct Args {
    map: HashMap<String, String>,
    pub positional: Vec<String>,
}

impl Args {
    pub fn parse() -> Args {
        let mut map = HashMap::new();
        let mut positional = Vec::new();
        let v: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < v.len() {
            if let Some(k) = v[i].strip_prefix("--") {
                if let Some((k, val)) = k.split_once('=') {
                    map.insert(k.to_string(), val.to_string());
                } else if i + 1 < v.len() && !v[i + 1].starts_with("--") {
                    map.insert(k.to_string(), v[i + 1].clone());
                    i += 1;
                } else {
                    map.insert(k.to_string(), "true".to_string());
                }
            } else {
                positional.push(v[i].clone());
            }
            i += 1;
        }
        Args { map, positional }
    }

    pub fn has(&self, k: &str) -> bool {
        self.map.contains_key(k)
    }

    pub fn str(&self, k: &str, default: &str) -> String {
        self.map.get(k).cloned().unwrap_or_else(|| default.to_string())
    }

    pub fn f64(&self, k: &str, default: f64) -> f64 {
        match self.map.get(k) {
            Some(v) => v.parse().unwrap_or_else(|_| die(&format!("--{k} 需要一个数字，收到 {v}"))),
            None => default,
        }
    }

    pub fn usize(&self, k: &str, default: usize) -> usize {
        self.f64(k, default as f64) as usize
    }
}

pub fn die(msg: &str) -> ! {
    eprintln!("错误：{msg}");
    std::process::exit(2)
}
