//! Aurio Windows 图形界面（Tauri）。界面在 ui/index.html，这里提供给界面调用的命令。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app {
    use aurio_win::win::discover::{discover, FoundMac};
    use aurio_win::win::driver::{self, DriverJob, DriverStatus, DriverTask};
    use aurio_win::win::echotest::{EchoProgress, EchoTest, EchoTestOptions};
    use aurio_win::win::engine::{Engine, EngineConfig, EngineStatus};
    use aurio_win::win::setup::{check, fix, open_sound_panel, SetupReport};
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use aurio_win::win::theme;
    use tauri::image::Image;
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

    /// 保存在 %APPDATA%\com.aurio.win\settings.json
    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    #[serde(default)]
    pub struct Settings {
        pub engine: EngineConfig,
        /// 上次选的 Mac 的名字（地址变了也能按名字重新找到）
        pub mac_name: String,
        /// 打开 Aurio 时自动开始
        pub auto_connect: bool,
        /// 点关闭时怎么做："tray" 缩到托盘，"quit" 退出，其他（含空）每次询问
        pub close_action: String,
    }

    pub struct Inner {
        engine: Mutex<Option<Engine>>,
        echo: Mutex<Option<EchoTest>>,
        driver: Mutex<Option<DriverTask>>,
        settings: Mutex<Settings>,
        path: PathBuf,
    }

    type AppState = Arc<Inner>;

    impl Inner {
        fn save(&self) {
            if let Some(dir) = self.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let s = self.settings.lock().unwrap().clone();
            if let Ok(json) = serde_json::to_string_pretty(&s) {
                let _ = std::fs::write(&self.path, json);
            }
        }
    }

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
        tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
    }

    #[tauri::command]
    fn get_settings(state: State<'_, AppState>) -> Settings {
        state.settings.lock().unwrap().clone()
    }

    #[tauri::command]
    fn save_settings(state: State<'_, AppState>, settings: Settings) {
        *state.settings.lock().unwrap() = settings;
        state.save();
    }

    #[tauri::command]
    async fn check_setup(state: State<'_, AppState>) -> Result<SetupReport, String> {
        let (r, d) = {
            let s = state.settings.lock().unwrap();
            (s.engine.mic_render.clone(), s.engine.mic_detect.clone())
        };
        blocking(move || check(&r, &d).map_err(|e| e.to_string())).await
    }

    /// 一键修复默认设备。返回做了哪些改动。
    #[tauri::command]
    async fn fix_setup(state: State<'_, AppState>, playback: String) -> Result<Vec<String>, String> {
        let (r, d) = {
            let s = state.settings.lock().unwrap();
            (s.engine.mic_render.clone(), s.engine.mic_detect.clone())
        };
        blocking(move || fix(&r, &d, &playback).map_err(|e| e.to_string())).await
    }

    #[tauri::command]
    async fn discover_macs() -> Result<Vec<FoundMac>, String> {
        blocking(|| discover(Duration::from_millis(2500)).map_err(|e| e.to_string())).await
    }

    #[tauri::command]
    async fn start(state: State<'_, AppState>, settings: Settings) -> Result<EngineStatus, String> {
        *state.settings.lock().unwrap() = settings.clone();
        state.save();
        let st = state.inner().clone();
        blocking(move || {
            if let Some(old) = st.engine.lock().unwrap().take() {
                old.stop();
            }
            let engine = Engine::start(&settings.engine).map_err(|e| e.to_string())?;
            let status = engine.status();
            *st.engine.lock().unwrap() = Some(engine);
            Ok(status)
        })
        .await
    }

    #[tauri::command]
    async fn stop(state: State<'_, AppState>) -> Result<(), String> {
        let st = state.inner().clone();
        blocking(move || {
            if let Some(e) = st.engine.lock().unwrap().take() {
                e.stop();
            }
            Ok(())
        })
        .await
    }

    #[tauri::command]
    fn status(state: State<'_, AppState>) -> Option<EngineStatus> {
        state.engine.lock().unwrap().as_ref().map(|e| e.status())
    }

    /// 开始录音回放测试。要求已经开始（麦克风方向在工作）。
    #[tauri::command]
    fn echo_test_start(state: State<'_, AppState>, seconds: f32) -> Result<(), String> {
        let status = state.engine.lock().unwrap().as_ref().map(|e| e.status());
        let Some(status) = status else { return Err("先点「连接」，连上 Mac 再测".into()) };
        if !status.mic.available {
            return Err(if status.mic.mode == "关闭" {
                "麦克风模式是「关闭」，改成「自动」或「常开」后重新开始".into()
            } else {
                "麦克风方向不可用（需要 VB-Cable）".into()
            });
        }
        let mut echo = state.echo.lock().unwrap();
        if echo.as_ref().is_some_and(|e| e.busy()) {
            return Err("测试还在进行".into());
        }
        let s = state.settings.lock().unwrap();
        *echo = Some(EchoTest::start(EchoTestOptions {
            seconds: seconds.clamp(3.0, 60.0),
            mic_device: s.engine.mic_detect.clone(),
            reference: s.engine.loopback.clone(),
        }));
        Ok(())
    }

    #[tauri::command]
    fn echo_test_progress(state: State<'_, AppState>) -> Option<EchoProgress> {
        state.echo.lock().unwrap().as_ref().map(|e| e.progress())
    }

    #[tauri::command]
    fn echo_test_play(state: State<'_, AppState>) -> Result<(), String> {
        let device = state.settings.lock().unwrap().engine.loopback.clone();
        match state.echo.lock().unwrap().as_ref() {
            Some(e) => e.play(&device).map_err(|e| e.to_string()),
            None => Err("还没有录音".into()),
        }
    }

    #[tauri::command]
    fn echo_test_stop_play(state: State<'_, AppState>) {
        if let Some(e) = state.echo.lock().unwrap().as_ref() {
            e.stop_play();
        }
    }

    /// 在资源管理器里选中这个文件
    #[tauri::command]
    fn reveal_file(path: String) {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn();
    }

    fn mic_devices(state: &AppState) -> (String, String) {
        let s = state.settings.lock().unwrap();
        (s.engine.mic_render.clone(), s.engine.mic_detect.clone())
    }

    #[tauri::command]
    async fn driver_status(state: State<'_, AppState>) -> Result<DriverStatus, String> {
        let (r, d) = mic_devices(&state);
        blocking(move || Ok(driver::status(&r, &d))).await
    }

    /// 在线安装 / 卸载驱动。会先停掉正在进行的连接（卸载时虚拟线不能被占用）。
    #[tauri::command]
    async fn driver_action(state: State<'_, AppState>, action: String, playback: String) -> Result<(), String> {
        let st = state.inner().clone();
        blocking(move || {
            let mut task = st.driver.lock().unwrap();
            if task.as_ref().is_some_and(|t| t.busy()) {
                return Err("上一个操作还没完成".into());
            }
            if let Some(e) = st.engine.lock().unwrap().take() {
                e.stop();
            }
            let (r, d) = mic_devices(&st);
            *task = Some(match action.as_str() {
                "install" => driver::start_install(r, d, playback),
                "uninstall" => driver::start_uninstall(r, d),
                _ => return Err("未知操作".into()),
            });
            Ok(())
        })
        .await
    }

    #[tauri::command]
    fn driver_progress(state: State<'_, AppState>) -> Option<DriverJob> {
        state.driver.lock().unwrap().as_ref().map(|t| t.job())
    }

    #[tauri::command]
    fn restart_computer() -> Result<(), String> {
        driver::restart_computer().map_err(|e| e.to_string())
    }

    #[tauri::command]
    fn open_sound_settings() {
        open_sound_panel();
    }

    // ———————————————————— 托盘 ————————————————————

    /// 托盘图标：32×32 RGBA，白色给深色任务栏、黑色给浅色任务栏（scripts/make-icons.py 生成）
    const TRAY_DARK: &[u8] = include_bytes!("../icons/tray-dark.rgba");
    const TRAY_LIGHT: &[u8] = include_bytes!("../icons/tray-light.rgba");

    /// 没连上 Mac 时图标变淡（和 Mac 菜单栏一致）
    fn tray_image(light: bool, linked: bool) -> Image<'static> {
        let mut px = if light { TRAY_LIGHT } else { TRAY_DARK }.to_vec();
        if !linked {
            for a in px.iter_mut().skip(3).step_by(4) {
                *a = (*a as u16 * 115 / 255) as u8;
            }
        }
        Image::new_owned(px, 32, 32)
    }

    fn tray_tooltip(status: Option<&EngineStatus>) -> &'static str {
        match status {
            Some(s) if s.peer.responding => "Aurio · 已连接 Mac",
            Some(_) => "Aurio · Mac 没有响应",
            None => "Aurio · 未开始",
        }
    }

    fn show_main(app: &AppHandle) {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }

    /// 真正退出：先停引擎，再关程序
    fn quit(app: &AppHandle) {
        if let Some(st) = app.try_state::<AppState>() {
            if let Some(e) = st.engine.lock().unwrap().take() {
                e.stop();
            }
        }
        app.exit(0);
    }

    /// 关闭窗口的询问框里选了以后调用。remember 为真时记下来，以后不再问。
    #[tauri::command]
    fn close_window(app: AppHandle, state: State<'_, AppState>, action: String, remember: bool) {
        if remember {
            state.settings.lock().unwrap().close_action = action.clone();
            state.save();
        }
        if action == "quit" {
            quit(&app);
        } else if let Some(w) = app.get_webview_window("main") {
            let _ = w.hide();
        }
    }

    fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
        let open = MenuItem::with_id(app, "open", "打开 Aurio", true, None::<&str>)?;
        let sep = PredefinedMenuItem::separator(app)?;
        let exit = MenuItem::with_id(app, "quit", "退出 Aurio", true, None::<&str>)?;
        let menu = Menu::with_items(app, &[&open, &sep, &exit])?;
        let light = theme::taskbar_is_light();
        TrayIconBuilder::with_id("main")
            .icon(tray_image(light, false))
            .tooltip(tray_tooltip(None))
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, ev| match ev.id().as_ref() {
                "open" => show_main(app),
                "quit" => quit(app),
                _ => {}
            })
            .on_tray_icon_event(|tray, ev| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                    show_main(tray.app_handle());
                }
            })
            .build(app)?;

        // 每 2 秒看一次任务栏主题和连接状态，变了才换图标
        let handle = app.clone();
        std::thread::spawn(move || {
            let mut last = (light, false, tray_tooltip(None));
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let light = theme::taskbar_is_light();
                let status = handle
                    .try_state::<AppState>()
                    .and_then(|st| st.engine.lock().unwrap().as_ref().map(|e| e.status()));
                let linked = status.as_ref().is_some_and(|s| s.peer.responding);
                let now = (light, linked, tray_tooltip(status.as_ref()));
                if now != last {
                    last = now;
                    if let Some(tray) = handle.tray_by_id("main") {
                        let _ = tray.set_icon(Some(tray_image(light, linked)));
                        let _ = tray.set_tooltip(Some(now.2));
                    }
                }
            }
        });
        Ok(())
    }

    pub fn run() {
        // 开机自启时带 --tray：不弹窗口，直接待在托盘里
        let start_in_tray = std::env::args().any(|a| a == "--tray");
        tauri::Builder::default()
            // 已经在运行（可能藏在托盘里）时再打开 Aurio.exe：把原来的窗口叫出来，不再开第二个
            .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
            .setup(move |app| {
                let path = app
                    .path()
                    .app_config_dir()
                    .unwrap_or_else(|_| std::env::temp_dir().join("Aurio"))
                    .join("settings.json");
                let settings = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
                    .unwrap_or_default();
                app.manage(Arc::new(Inner { engine: Mutex::new(None), echo: Mutex::new(None), driver: Mutex::new(None), settings: Mutex::new(settings), path }));
                setup_tray(app.handle())?;
                // 窗口在配置里默认隐藏，免得 --tray 启动时闪一下
                if !start_in_tray {
                    show_main(app.handle());
                }
                Ok(())
            })
            .on_window_event(|window, event| match event {
                // 点关闭（标题栏 ×、Alt+F4、任务栏右键关闭）：按设置缩到托盘或退出；没设置过就让界面弹框问
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let action = window
                        .try_state::<AppState>()
                        .map(|st| st.settings.lock().unwrap().close_action.clone())
                        .unwrap_or_default();
                    match action.as_str() {
                        "tray" => {
                            let _ = window.hide();
                        }
                        "quit" => quit(window.app_handle()),
                        _ => {
                            let _ = window.show();
                            let _ = window.set_focus();
                            let _ = window.emit("close-requested", ());
                        }
                    }
                }
                // 程序退出时窗口销毁：停掉引擎
                WindowEvent::Destroyed => {
                    if let Some(st) = window.try_state::<AppState>() {
                        if let Some(e) = st.engine.lock().unwrap().take() {
                            e.stop();
                        }
                    }
                }
                _ => {}
            })
            .invoke_handler(tauri::generate_handler![
                get_settings,
                save_settings,
                check_setup,
                fix_setup,
                discover_macs,
                start,
                stop,
                status,
                open_sound_settings,
                echo_test_start,
                echo_test_progress,
                echo_test_play,
                echo_test_stop_play,
                reveal_file,
                driver_status,
                driver_action,
                driver_progress,
                restart_computer,
                close_window
            ])
            .run(tauri::generate_context!())
            .expect("Aurio 启动失败");
    }
}

fn main() {
    // 以管理员身份运行的辅助进程：只做驱动的安装 / 卸载，不打开界面
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(|a| a.as_str()) == Some("--driver-helper") {
            std::process::exit(aurio_win::win::driver::helper_main(&args[1..]));
        }
    }
    #[cfg(windows)]
    app::run();
    #[cfg(not(windows))]
    eprintln!("Aurio 的这个界面只在 Windows 上运行");
}
