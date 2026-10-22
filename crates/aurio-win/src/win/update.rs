//! 在线更新（Windows）。
//!
//! 流程：取签名清单 → 比较版本 → 下载安装包（镜像优先、续传、校验大小 / SHA-256 / 签名）→
//! 复制一份自己到临时目录当「更新助手」，主程序退出 → 助手等主程序退出后，以管理员身份
//! 静默运行安装包（/S /UPDATE），装完以普通权限重新打开 Aurio。
//!
//! 取清单和下载的细节在 aurio_update::client，这里只管状态和安装。

use aurio_update::client::{Client, Progress};
use aurio_update::{self as up, Manifest, Source};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前版本（和 Cargo.toml 的 workspace 版本一致）
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, Serialize)]
pub struct UpdateStatus {
    pub current: String,
    /// idle / checking / latest / available / downloading / ready / installing / error
    pub stage: String,
    pub latest: Option<String>,
    pub notes: String,
    pub size: u64,
    pub received: u64,
    /// 正在用 / 上次成功的下载来源，给界面看
    pub source: String,
    /// 字节/秒
    pub speed: f64,
    pub error: Option<String>,
    /// 上次检查完成的时间（Unix 秒），0 表示还没检查过
    pub checked_at: u64,
}

struct State {
    status: UpdateStatus,
    manifest: Option<Manifest>,
    preferred: Option<Source>,
    ready: Option<PathBuf>,
    busy: bool,
}

#[derive(Clone)]
pub struct Updater {
    st: Arc<Mutex<State>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 下载目录：%TEMP%\Aurio\update
pub fn work_dir() -> PathBuf {
    std::env::temp_dir().join("Aurio").join("update")
}

fn client() -> Result<Client, String> {
    Client::new(concat!("Aurio/", env!("CARGO_PKG_VERSION"), " (Windows)"), up::PUBLIC_KEY)
}

impl Updater {
    pub fn new() -> Updater {
        // 清掉上次更新留下的更新助手（如果它还在运行，删不掉也没关系；已下载的安装包保留）
        let _ = fs::remove_file(work_dir().join("aurio-updater.exe"));
        Updater {
            st: Arc::new(Mutex::new(State {
                status: UpdateStatus { current: CURRENT.into(), stage: "idle".into(), ..Default::default() },
                manifest: None,
                preferred: None,
                ready: None,
                busy: false,
            })),
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.st.lock().unwrap().status.clone()
    }

    /// 检查更新（阻塞，通常一两秒）。正在下载或安装时直接返回当前状态。
    pub fn check(&self) -> UpdateStatus {
        let preferred = {
            let mut st = self.st.lock().unwrap();
            if st.busy {
                return st.status.clone();
            }
            st.busy = true;
            st.status.stage = "checking".into();
            st.status.error = None;
            st.preferred.clone()
        };
        let result = client().and_then(|c| c.fetch_manifest(preferred.as_ref()));
        let mut st = self.st.lock().unwrap();
        st.busy = false;
        st.status.checked_at = now_secs();
        match result {
            Ok((m, src)) => {
                st.status.source = src.name();
                st.preferred = Some(src);
                st.status.latest = Some(m.version.clone());
                st.status.notes = m.notes.clone();
                let asset = m.platforms.get(up::PLATFORM_WINDOWS);
                st.status.size = asset.map(|a| a.size).unwrap_or(0);
                st.status.stage = if !up::is_newer(&m.version, CURRENT) || asset.is_none() {
                    "latest"
                } else if st.ready.as_ref().is_some_and(|p| p.exists())
                    && st.manifest.as_ref().is_some_and(|old| old.version == m.version)
                {
                    "ready"
                } else {
                    "available"
                }
                .into();
                st.manifest = Some(m);
            }
            Err(e) => {
                st.status.stage = "error".into();
                st.status.error = Some(e);
            }
        }
        st.status.clone()
    }

    /// 在后台下载新版本。用 `status()` 看进度。
    pub fn start_download(&self) -> Result<(), String> {
        let (manifest, preferred) = {
            let mut st = self.st.lock().unwrap();
            if st.busy {
                return Err("正在进行中".into());
            }
            let m = st.manifest.clone().ok_or("先检查更新")?;
            let asset = m.platforms.get(up::PLATFORM_WINDOWS).ok_or("这个版本没有 Windows 安装包")?;
            st.busy = true;
            st.status.stage = "downloading".into();
            st.status.error = None;
            st.status.received = 0;
            st.status.size = asset.size;
            (m, st.preferred.clone())
        };
        let me = self.clone();
        std::thread::Builder::new()
            .name("aurio-update".into())
            .spawn(move || {
                let asset = manifest.platforms[up::PLATFORM_WINDOWS].clone();
                // 清掉以前版本留下的安装包（同一个版本的半截文件保留，用于续传）
                if let Ok(entries) = fs::read_dir(work_dir()) {
                    for e in entries.flatten() {
                        let name = e.file_name().to_string_lossy().to_string();
                        if !name.starts_with(&asset.file) && name != "aurio-updater.exe" {
                            let _ = fs::remove_file(e.path());
                        }
                    }
                }
                let on_progress = |p: &Progress| {
                    let mut st = me.st.lock().unwrap();
                    st.status.source = p.source.clone();
                    st.status.received = p.received;
                    st.status.speed = p.speed;
                };
                let r = client().and_then(|c| c.download(&manifest, &asset, &work_dir(), preferred.as_ref(), &on_progress));
                let mut st = me.st.lock().unwrap();
                st.busy = false;
                st.status.speed = 0.0;
                match r {
                    Ok((path, src)) => {
                        st.ready = Some(path);
                        st.status.stage = "ready".into();
                        st.status.received = asset.size;
                        st.status.source = src.name();
                        st.preferred = Some(src);
                    }
                    Err(e) => {
                        st.status.stage = "error".into();
                        st.status.error = Some(e);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 启动更新助手。成功后调用方应立刻退出程序（安装包会替换正在运行的文件）。
    pub fn install(&self) -> Result<(), String> {
        let (setup, asset) = {
            let st = self.st.lock().unwrap();
            if st.status.stage != "ready" {
                return Err("新版本还没下载好".into());
            }
            let setup = st.ready.clone().ok_or("找不到安装包")?;
            let asset = st.manifest.as_ref().and_then(|m| m.platforms.get(up::PLATFORM_WINDOWS).cloned());
            (setup, asset)
        };
        // 运行前再验一次：下载完到现在之间文件可能被改过
        let asset = asset.ok_or("清单里没有 Windows 安装包")?;
        let data = fs::read(&setup).map_err(|e| e.to_string())?;
        asset.verify(&data, up::PUBLIC_KEY).map_err(|e| e.to_string())?;

        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        // 安装包会结束所有 Aurio.exe，所以助手换一个文件名
        let helper = work_dir().join("aurio-updater.exe");
        fs::copy(&me, &helper).map_err(|e| format!("准备更新助手失败：{e}"))?;
        let pid = std::process::id().to_string();
        spawn_detached(&helper, &["--update-helper", &setup.to_string_lossy(), &me.to_string_lossy(), &pid])
            .map_err(|e| format!("启动更新助手失败：{e}"))?;
        self.st.lock().unwrap().status.stage = "installing".into();
        Ok(())
    }
}

fn spawn_detached(exe: &Path, args: &[&str]) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new(exe)
        .args(args)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
}

/// 更新助手入口：`aurio-updater.exe --update-helper <安装包> <Aurio.exe> <主进程 PID>`。返回进程退出码。
pub fn helper_main(args: &[String]) -> i32 {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    let (Some(setup), Some(app)) = (args.first(), args.get(1)) else { return 2 };
    // 等主程序退出，免得安装包结束进程时它还没走
    if let Some(pid) = args.get(2).and_then(|p| p.parse::<u32>().ok()) {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let _ = WaitForSingleObject(h, 15_000);
                let _ = CloseHandle(h);
            }
        }
    }
    // /S 静默安装；/UPDATE 告诉安装包这是在线更新（沿用原来的安装选项）
    let code = match super::driver::run_elevated(Path::new(setup), "/S /UPDATE") {
        Ok(c) => c as i32,
        Err(_) => 1, // 用户在 UAC 框里点了「否」等：照样把原来的 Aurio 打开
    };
    // 助手是普通权限，这样重新打开的 Aurio 也是普通权限
    let _ = spawn_detached(Path::new(app), &[]);
    code
}
