//! 任务栏用的是浅色还是深色。托盘图标按它选黑色或白色。
//!
//! 注意和窗口主题不是一回事：「Windows 模式」（任务栏、开始菜单）存在
//! SystemUsesLightTheme，「应用模式」存在 AppsUseLightTheme，两者可以不同。

use windows::core::w;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

/// 任务栏是浅色时返回 true。读不到时按深色处理（Windows 10 的默认值）。
pub fn taskbar_is_light() -> bool {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok() && value != 0
}
