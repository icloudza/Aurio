//! 虚拟声卡驱动（VB-Cable）的检测、在线安装和彻底卸载。
//!
//! VB-Cable 是 VB-Audio 的捐赠软件，许可不允许把它的安装包集成进别的软件的安装流程，
//! 所以这里像包管理器一样工作：用户点「安装」时才从 VB-Audio 官网下载官方原版安装包，
//! 校验数字签名后运行它自带的安装程序；Aurio 本身不携带也不修改它。
//!
//! 流程：
//! - 普通权限：下载 → 解压 → 校验签名
//! - 管理员权限：以管理员身份重新启动 Aurio 自己（隐藏窗口，--driver-helper），
//!   由它静默运行安装 / 卸载，结果写到日志文件，普通权限这边等它退出后读取
//!
//! 管理员权限只能通过 Windows 的 UAC 确认框获得，这是系统强制的，任何软件都一样。

use super::common::*;
use serde::Serialize;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Com::Urlmon::URLDownloadToFileW;
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// VB-Audio 官网下载页上的官方安装包地址
pub const PACK_URL: &str = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";
const PACK_NAME: &str = "VBCABLE_Driver_Pack45.zip";
const SETUP_EXE: &str = "VBCABLE_Setup_x64.exe";
/// 安装程序签名证书里的作者名（VB-Cable 作者 Vincent Burel）
const SIGNER: &str = "BUREL VINCENT";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Default, Serialize)]
pub struct DriverStatus {
    pub installed: bool,
    pub cable_in: String,
    pub cable_out: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DriverJob {
    /// install / uninstall
    pub kind: String,
    /// 一句话描述当前在做什么
    pub step: String,
    pub done: bool,
    pub ok: bool,
    pub needs_reboot: bool,
    pub message: String,
}

pub fn status(mic_render: &str, mic_detect: &str) -> DriverStatus {
    let _ = wasapi::initialize_mta();
    let cin = find_device(wasapi::Direction::Render, mic_render).ok().and_then(|d| d.get_friendlyname().ok());
    let cout = find_device(wasapi::Direction::Capture, mic_detect).ok().and_then(|d| d.get_friendlyname().ok());
    DriverStatus {
        installed: cin.is_some() && cout.is_some(),
        cable_in: cin.unwrap_or_default(),
        cable_out: cout.unwrap_or_default(),
    }
}

/// %LOCALAPPDATA%\Aurio\vbcable
fn work_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Aurio").join("vbcable")
}

fn hidden(cmd: &str) -> Command {
    let mut c = Command::new(cmd);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

fn powershell(script: &str) -> Res<String> {
    let out = hidden("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn download(url: &str, to: &Path) -> Res<()> {
    let _ = std::fs::remove_file(to);
    unsafe {
        URLDownloadToFileW(None, &HSTRING::from(url), &HSTRING::from(to.as_os_str()), 0, None)
            .map_err(|e| format!("下载失败（{e}），检查网络后重试"))?;
    }
    let len = std::fs::metadata(to).map(|m| m.len()).unwrap_or(0);
    if len < 100_000 {
        return Err("下载到的文件不完整，稍后重试".into());
    }
    Ok(())
}

fn extract(zip: &Path, dir: &Path) -> Res<()> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    // Windows 10 1803 起自带 tar.exe（支持 zip）；不行再用 PowerShell
    let ok = hidden("tar.exe").arg("-xf").arg(zip).arg("-C").arg(dir).status().map(|s| s.success()).unwrap_or(false);
    if !ok || !dir.join(SETUP_EXE).exists() {
        let script = format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            zip.display(),
            dir.display()
        );
        powershell(&script)?;
    }
    if !dir.join(SETUP_EXE).exists() {
        return Err("安装包解压失败".into());
    }
    Ok(())
}

/// 校验安装程序的数字签名：必须有效，且签名者是 VB-Cable 的作者。
fn verify(exe: &Path) -> Res<()> {
    let script = format!(
        "$s = Get-AuthenticodeSignature -LiteralPath '{}'; \"$($s.Status)|$($s.SignerCertificate.Subject)\"",
        exe.display()
    );
    let out = powershell(&script)?;
    let (st, subject) = out.split_once('|').unwrap_or((out.as_str(), ""));
    if st != "Valid" || !subject.to_uppercase().contains(SIGNER) {
        return Err(format!("安装包签名校验没通过（{st}），为安全起见没有安装").into());
    }
    Ok(())
}

/// 以管理员身份运行（弹 UAC 确认框），隐藏窗口，等它结束并返回退出码。
pub(crate) fn run_elevated(exe: &Path, args: &str) -> Res<u32> {
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(args);
    let verb = HSTRING::from("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        if let Err(e) = ShellExecuteExW(&mut info) {
            // 0x800704C7 = ERROR_CANCELLED：用户在 UAC 框里点了「否」
            return Err(if e.code().0 as u32 == 0x8007_04C7 {
                "需要管理员权限才能继续（UAC 确认框里点了「否」）".into()
            } else {
                format!("没能获得管理员权限：{e}").into()
            });
        }
        if info.hProcess.is_invalid() {
            return Err("没能启动管理员进程".into());
        }
        let wait = WaitForSingleObject(info.hProcess, 10 * 60 * 1000);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        if wait != WAIT_OBJECT_0 {
            return Err("等待超时".into());
        }
        Ok(code)
    }
}

fn read_log(log: &Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default().trim().to_string()
}

fn wait_for<F: Fn() -> bool>(f: F, secs: u64) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    f()
}

/// 后台任务的句柄：界面每隔一会儿读一次进度
pub struct DriverTask {
    job: Arc<Mutex<DriverJob>>,
}

impl DriverTask {
    pub fn job(&self) -> DriverJob {
        self.job.lock().unwrap().clone()
    }
    pub fn busy(&self) -> bool {
        !self.job.lock().unwrap().done
    }
}

fn spawn(kind: &str, f: impl FnOnce(&dyn Fn(&str)) -> Res<(String, bool)> + Send + 'static) -> DriverTask {
    let job = Arc::new(Mutex::new(DriverJob { kind: kind.into(), ..Default::default() }));
    let j = job.clone();
    std::thread::spawn(move || {
        let _ = wasapi::initialize_mta();
        let set = |s: &str| j.lock().unwrap().step = s.to_string();
        let r = f(&set);
        let mut g = j.lock().unwrap();
        g.done = true;
        match r {
            Ok((msg, reboot)) => {
                g.ok = true;
                g.message = msg;
                g.needs_reboot = reboot;
            }
            Err(e) => g.message = e.to_string(),
        }
    });
    DriverTask { job }
}

/// 下载官方安装包、校验后返回解压目录（已经下载过且签名有效就复用）。
fn fetch(set: &dyn Fn(&str)) -> Res<PathBuf> {
    let dir = work_dir();
    let pack_dir = dir.join("pack");
    if pack_dir.join(SETUP_EXE).exists() && verify(&pack_dir.join(SETUP_EXE)).is_ok() {
        return Ok(pack_dir);
    }
    std::fs::create_dir_all(&dir)?;
    set("正在从 VB-Audio 官网下载…");
    let zip = dir.join(PACK_NAME);
    download(PACK_URL, &zip)?;
    set("正在解压…");
    extract(&zip, &pack_dir)?;
    set("正在校验数字签名…");
    verify(&pack_dir.join(SETUP_EXE))?;
    let _ = std::fs::remove_file(&zip);
    Ok(pack_dir)
}

/// 在线安装。完成后自动把默认设备设好（默认录音设备 = CABLE Output，默认播放设备不是虚拟线）。
pub fn start_install(mic_render: String, mic_detect: String, playback: String) -> DriverTask {
    spawn("install", move |set| {
        let pack = fetch(set)?;
        set("等待管理员授权（请在弹出的确认框里点「是」）…");
        let log = work_dir().join("helper.log");
        let _ = std::fs::remove_file(&log);
        let me = std::env::current_exe()?;
        let args = format!("--driver-helper install \"{}\" \"{}\"", pack.display(), log.display());
        set("正在安装驱动…");
        let code = run_elevated(&me, &args)?;
        if code != 0 {
            let msg = read_log(&log);
            return Err(if msg.is_empty() { format!("安装失败（代码 {code}）") } else { msg }.into());
        }
        set("正在等待系统识别新设备…");
        let appeared = wait_for(|| status(&mic_render, &mic_detect).installed, 20);
        if !appeared {
            return Ok(("驱动已安装，需要重启电脑后才能使用".into(), true));
        }
        set("正在设置默认设备…");
        let fixed = super::setup::fix(&mic_render, &mic_detect, &playback).unwrap_or_default();
        let mut msg = "驱动已安装".to_string();
        if !fixed.is_empty() {
            msg += &format!("；{}", fixed.join("；"));
        }
        Ok((msg, false))
    })
}

/// 彻底卸载：运行官方卸载，再删掉残留的设备、音频端点和驱动包。
pub fn start_uninstall(mic_render: String, mic_detect: String) -> DriverTask {
    spawn("uninstall", move |set| {
        // 有官方安装程序就先用它卸载；下载失败也继续，后面的清理一样能删干净
        let pack = fetch(set).ok();
        set("等待管理员授权（请在弹出的确认框里点「是」）…");
        let log = work_dir().join("helper.log");
        let _ = std::fs::remove_file(&log);
        let me = std::env::current_exe()?;
        let pack_arg = pack.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let args = format!("--driver-helper uninstall \"{pack_arg}\" \"{}\"", log.display());
        set("正在卸载驱动并清理残留…");
        let code = run_elevated(&me, &args)?;
        if code != 0 {
            let msg = read_log(&log);
            return Err(if msg.is_empty() { format!("卸载失败（代码 {code}）") } else { msg }.into());
        }
        let _ = std::fs::remove_dir_all(work_dir());
        let gone = wait_for(|| !status(&mic_render, &mic_detect).installed, 10);
        if gone {
            Ok(("驱动已卸载干净".into(), false))
        } else {
            Ok(("驱动已卸载，重启电脑后设备会完全消失".into(), true))
        }
    })
}

// ———————————— 管理员辅助进程（由 Aurio.exe --driver-helper 调用） ————————————

/// 卸载程序用：驱动装了返回 10，没装返回 11。
pub const STATUS_INSTALLED: i32 = 10;
pub const STATUS_NOT_INSTALLED: i32 = 11;

/// 有没有装 VB-Cable：音频端点在，或者系统里还有它的设备（比如装了没重启）。
pub fn detect_installed() -> bool {
    if status("CABLE Input", "CABLE Output").installed {
        return true;
    }
    powershell("@(Get-PnpDevice | Where-Object { $_.FriendlyName -like '*VB-Audio Virtual Cable*' }).Count")
        .ok()
        .and_then(|n| n.trim().parse::<u32>().ok())
        .is_some_and(|n| n > 0)
}

/// 管理员辅助进程入口：
/// - `install <安装包目录> <日志>` / `uninstall <安装包目录或空> <日志>`：返回 0 成功，1 失败（原因写进日志）
/// - `status`：驱动装了返回 10，没装返回 11
pub fn helper_main(args: &[String]) -> i32 {
    if args.first().map(|s| s.as_str()) == Some("status") {
        let _ = wasapi::initialize_mta();
        return if detect_installed() { STATUS_INSTALLED } else { STATUS_NOT_INSTALLED };
    }
    let (Some(kind), Some(pack), Some(log)) = (args.first(), args.get(1), args.get(2)) else {
        return 2;
    };
    let r = match kind.as_str() {
        "install" => helper_install(Path::new(pack)),
        "uninstall" => helper_uninstall(if pack.is_empty() { None } else { Some(Path::new(pack)) }),
        _ => Err("未知操作".into()),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            let _ = std::fs::write(log, e.to_string());
            1
        }
    }
}

fn run_setup(pack: &Path, flag: &str) -> Res<()> {
    let exe = pack.join(SETUP_EXE);
    verify(&exe)?;
    // -i 安装 / -u 卸载，-h 不显示界面
    let mut child = hidden(exe.to_str().unwrap_or_default()).current_dir(pack).args([flag, "-h"]).spawn()?;
    let t = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if t.elapsed() > Duration::from_secs(180) {
            let _ = child.kill();
            return Err("驱动安装程序运行超时".into());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn helper_install(pack: &Path) -> Res<()> {
    run_setup(pack, "-i")
}

/// 删掉所有 VB-Audio Virtual Cable 设备（含音频端点）和驱动商店里的 VB-Audio 驱动包。
const CLEANUP_PS: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
Get-PnpDevice | Where-Object { $_.FriendlyName -like '*VB-Audio Virtual Cable*' -or $_.Manufacturer -like '*VB-Audio*' } |
  ForEach-Object { pnputil /remove-device "$($_.InstanceId)" | Out-Null }
Get-WindowsDriver -Online | Where-Object { $_.ProviderName -like '*VB-Audio*' -or $_.OriginalFileName -like '*vbMmeCable*' } |
  ForEach-Object { pnputil /delete-driver $_.Driver /uninstall /force | Out-Null }
$left = @(Get-WindowsDriver -Online | Where-Object { $_.ProviderName -like '*VB-Audio*' -or $_.OriginalFileName -like '*vbMmeCable*' }).Count
"left=$left"
"#;

fn helper_uninstall(pack: Option<&Path>) -> Res<()> {
    if let Some(p) = pack {
        let _ = run_setup(p, "-u");
    }
    let out = powershell(CLEANUP_PS)?;
    if let Some(n) = out.lines().find_map(|l| l.strip_prefix("left=")).and_then(|n| n.trim().parse::<u32>().ok()) {
        if n > 0 {
            return Err(format!("还有 {n} 个驱动包没删掉，重启电脑后再点一次卸载").into());
        }
    }
    Ok(())
}

/// 5 秒后重启电脑（普通用户权限即可）。
pub fn restart_computer() -> Res<()> {
    hidden("shutdown.exe").args(["/r", "/t", "5", "/c", "Aurio：完成虚拟声卡驱动的安装或卸载"]).spawn()?;
    Ok(())
}
