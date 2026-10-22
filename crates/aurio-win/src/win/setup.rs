//! 检查 VB-Cable 和 Windows 声音设置。命令行打印结果，图形界面显示同一份报告。
//!
//! VB-Cable 的许可不允许把它集成进别的软件的安装流程，所以这里不替用户运行它的安装程序，
//! 只负责找到安装包、检查结果、给出步骤。

use super::common::*;
use aurio_tools::Args;
use serde::Serialize;
use wasapi::*;

pub const PACK: &str = "VBCABLE_Driver_Pack45.zip";

#[derive(Clone, Debug, Default, Serialize)]
pub struct SetupReport {
    pub cable_installed: bool,
    pub cable_in: String,
    pub cable_out: String,
    /// 找到的 VB-Cable 安装包路径（没装时用）
    pub package: Option<String>,
    pub default_render: String,
    pub default_capture: String,
    pub comm_capture: String,
    /// 可以当播放来源的设备（排除了虚拟线）
    pub render_devices: Vec<String>,
    /// 需要用户去改的地方，每条一句话
    pub todo: Vec<String>,
    /// 有可以一键修复的项（默认播放设备是虚拟线 / 默认录音设备不是 CABLE Output）
    pub fixable: bool,
    /// 一键修复时会把默认播放设备换成这个（优先显示器的 HDMI 音频，本地不出声）
    pub suggested_playback: String,
}

/// 从可用的播放设备里挑一个做默认：优先显示器的 HDMI / DisplayPort 音频，本地不出声。
pub fn suggest_playback(devices: &[String]) -> Option<String> {
    let display = ["hdmi", "displayport", "high definition audio", "display audio", "nvidia", "amd", "intel(r) display"];
    devices
        .iter()
        .find(|n| {
            let l = n.to_lowercase();
            display.iter().any(|k| l.contains(k))
        })
        .or(devices.first())
        .cloned()
}

fn name_of(d: &Device) -> String {
    d.get_friendlyname().unwrap_or_default()
}

fn mix_rate(d: &Device) -> Option<u32> {
    d.get_iaudioclient().ok()?.get_mixformat().ok().map(|f| f.get_samplespersec())
}

/// 在 exe 所在目录（和下一级子目录）里找 VB-Cable 安装包或解压后的安装程序。
pub fn find_package() -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let mut dirs = vec![dir.clone()];
    if let Ok(rd) = std::fs::read_dir(&dir) {
        dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    for d in &dirs {
        for f in ["VBCABLE_Setup_x64.exe", PACK] {
            let p = d.join(f);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

pub fn check(mic_render: &str, mic_detect: &str) -> Res<SetupReport> {
    let _ = initialize_mta();
    let en = DeviceEnumerator::new()?;
    let mut r = SetupReport::default();
    let show = |d: &Option<Device>| d.as_ref().map(name_of).unwrap_or_default();

    let cable_in = find_device(Direction::Render, mic_render).ok();
    let cable_out = find_device(Direction::Capture, mic_detect).ok();
    r.cable_installed = cable_in.is_some() && cable_out.is_some();
    r.cable_in = show(&cable_in);
    r.cable_out = show(&cable_out);

    if let Ok(coll) = en.get_device_collection(&Direction::Render) {
        r.render_devices = (&coll)
            .into_iter()
            .flatten()
            .map(|d| name_of(&d))
            .filter(|n| !is_virtual_cable(n, mic_render))
            .collect();
    }

    let def_render = en.get_default_device(&Direction::Render).ok();
    let comm_render = en.get_default_device_for_role(&Direction::Render, &Role::Communications).ok();
    let def_capture = en.get_default_device(&Direction::Capture).ok();
    let comm_capture = en.get_default_device_for_role(&Direction::Capture, &Role::Communications).ok();
    r.default_render = show(&def_render);
    r.suggested_playback = suggest_playback(&r.render_devices).unwrap_or_default();
    r.default_capture = show(&def_capture);
    r.comm_capture = show(&comm_capture);

    if !r.cable_installed {
        r.package = find_package().map(|p| p.display().to_string());
        r.todo.push("麦克风功能需要虚拟声卡驱动：在下面的「虚拟声卡驱动」里点「安装驱动」，会自动下载并安装".to_string());
    }

    if is_virtual_cable(&r.default_render, mic_render) {
        r.fixable = true;
        r.todo.push(format!(
            "默认播放设备是「{}」（虚拟线）：系统声音会直接进麦克风。到声音设置 → 输出，换成显示器的 HDMI 音频之类没接喇叭的设备",
            r.default_render
        ));
    }
    let comm_render_name = show(&comm_render);
    if comm_render.is_some() && is_virtual_cable(&comm_render_name, mic_render) {
        r.fixable = true;
        r.todo.push(format!(
            "默认通讯播放设备是「{comm_render_name}」（虚拟线）：在声音控制面板的播放设备上右键 → 设为默认通讯设备，选一个别的"
        ));
    }

    if let (Some(cin), Some(cout)) = (&cable_in, &cable_out) {
        let cout_id = cout.get_id().unwrap_or_default();
        let id = |d: &Option<Device>| d.as_ref().and_then(|d| d.get_id().ok()).unwrap_or_default();
        if id(&def_capture) != cout_id || id(&comm_capture) != cout_id {
            r.fixable = true;
            r.todo.push(format!(
                "把「{}」设为默认录音设备（声音控制面板 → 录制 → 右键 → 设为默认设备），应用就会自动用 Studio Display 的麦克风。可以顺便改名叫「Studio Display 麦克风」",
                name_of(cout)
            ));
        }
        for (d, kind) in [(cin, "播放端"), (cout, "录音端")] {
            if let Some(rate) = mix_rate(d).filter(|&r| r != 48000) {
                r.todo.push(format!(
                    "「{}」（{kind}）的格式是 {rate} Hz，建议改成 48000 Hz：声音控制面板 → 属性 → 高级",
                    name_of(d)
                ));
            }
        }
    }
    Ok(r)
}

/// 一键修复：默认播放设备换成非虚拟线的设备，默认录音设备设成 CABLE Output。
/// `playback` 为空时用建议的设备。返回做了哪些改动。
pub fn fix(mic_render: &str, mic_detect: &str, playback: &str) -> Res<Vec<String>> {
    let r = check(mic_render, mic_detect)?;
    let mut done = Vec::new();
    let en = DeviceEnumerator::new()?;
    let comm_render = en
        .get_default_device_for_role(&Direction::Render, &Role::Communications)
        .ok()
        .map(|d| name_of(&d))
        .unwrap_or_default();
    if is_virtual_cable(&r.default_render, mic_render) || is_virtual_cable(&comm_render, mic_render) {
        let target = if !playback.is_empty() && r.render_devices.iter().any(|n| n == playback) {
            playback.to_string()
        } else {
            r.suggested_playback.clone()
        };
        if target.is_empty() {
            return Err("没有可以当默认播放设备的设备（除了虚拟线）".into());
        }
        let dev = find_device(Direction::Render, &target)?;
        super::policy::set_default_endpoint(&dev.get_id()?)?;
        done.push(format!("默认播放设备改成了「{target}」"));
    }
    if r.cable_installed {
        let cout = find_device(Direction::Capture, mic_detect)?;
        let cout_id = cout.get_id()?;
        let id = |role: &Role| en.get_default_device_for_role(&Direction::Capture, role).and_then(|d| d.get_id()).unwrap_or_default();
        if id(&Role::Console) != cout_id || id(&Role::Communications) != cout_id {
            super::policy::set_default_endpoint(&cout_id)?;
            done.push(format!("默认录音设备改成了「{}」", name_of(&cout)));
        }
    }
    Ok(done)
}

/// 打开 Windows 的声音控制面板（录制页）。
pub fn open_sound_panel() {
    let _ = std::process::Command::new("control").args(["mmsys.cpl,,1"]).spawn();
}

pub fn run(a: &Args) -> Res<()> {
    let r = check(&a.str("mic-device", "CABLE Input"), &a.str("mic-detect", "CABLE Output"))?;
    println!("VB-Cable：{}", if r.cable_installed { format!("已安装（{} / {}）", r.cable_in, r.cable_out) } else { "没有安装".into() });
    println!("默认播放设备：{}", r.default_render);
    println!("默认录音设备：{}", r.default_capture);
    println!("默认通讯录音设备：{}", r.comm_capture);
    println!("可用的播放来源：{}", r.render_devices.join("、"));
    println!();
    if r.todo.is_empty() {
        println!("设置都没问题。");
    } else {
        println!("还需要做：");
        for (i, t) in r.todo.iter().enumerate() {
            println!("  {}. {t}", i + 1);
        }
        if a.has("fix") && r.fixable {
            for d in fix(&a.str("mic-device", "CABLE Input"), &a.str("mic-detect", "CABLE Output"), &a.str("playback", ""))? {
                println!("已修复：{d}");
            }
        } else if a.has("open") {
            open_sound_panel();
        } else {
            println!("\n加 --fix 自动修复默认设备，加 --open 打开声音控制面板");
        }
    }
    Ok(())
}
