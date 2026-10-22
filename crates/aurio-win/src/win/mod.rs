pub mod cable;
pub mod capture;
pub mod common;
pub mod discover;
pub mod driver;
pub mod echotest;
pub mod engine;
pub mod list;
pub mod mic;
pub mod policy;
pub mod send;
pub mod setup;
pub mod theme;
pub mod update;

use aurio_tools::Args;

/// 命令行入口
pub fn cli_main() {
    let a = Args::parse();
    let cmd = a.positional.first().map(|s| s.as_str()).unwrap_or("");
    if wasapi::initialize_mta().is_err() {
        aurio_tools::die("COM 初始化失败");
    }
    let r = match cmd {
        "list" => list::run(&a),
        "send" => send::run(&a),
        "cable-latency" => cable::run(&a),
        "setup" => setup::run(&a),
        "discover" => discover::discover(std::time::Duration::from_secs(3)).map(|v| {
            if v.is_empty() {
                println!("没找到 Mac（确认 Mac 上的 Aurio 已经打开）");
            }
            for m in v {
                println!("{}  {}（全部地址：{}）", m.name, m.addr, m.ips.join("、"));
            }
        }),
        _ => {
            println!(
                "用法：\n  \
                 aurio-win setup                     检查 VB-Cable 和声音设置\n  \
                 aurio-win discover                  在局域网里找 Mac\n  \
                 aurio-win send [--to Mac的IP]       开始（不给 --to 就自动找 Mac）\n  \
                 aurio-win list                      列出设备、格式和引擎周期\n  \
                 aurio-win cable-latency             测量虚拟线延迟\n\n\
                 send 的选项：\n  \
                 --loopback 名称      回环采集哪个播放设备（默认：系统默认播放设备）\n  \
                 --device 名称        改为从录音设备采集（不用回环）\n  \
                 --mode 模式          shared（默认）/ lowlat / exclusive，只对 --device 有意义\n  \
                 --mic 模式           auto = 有应用在用 CABLE Output 录音时才开（默认）；on = 一直开；off = 不要麦克风\n  \
                 --mic-device 名称    麦克风写入的播放设备（默认 \"CABLE Input\"）\n  \
                 --mic-detect 名称    auto 模式检测的录音设备（默认 \"CABLE Output\"）\n  \
                 --mic-queue-ms N     麦克风 WASAPI 队列余量（默认 5）\n  \
                 --fpp N              每包帧数（默认 120）\n\n\
                 平时用图形界面 Aurio.exe 就行，命令行用于排查。"
            );
            return;
        }
    };
    if let Err(e) = r {
        aurio_tools::die(&e.to_string());
    }
}
