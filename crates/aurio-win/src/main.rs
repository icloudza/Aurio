//! aurio-win：Windows 端命令行（排查用）。日常使用请用图形界面 Aurio.exe。

fn main() {
    #[cfg(windows)]
    aurio_win::win::cli_main();
    #[cfg(not(windows))]
    {
        eprintln!("aurio-win 只能在 Windows 上运行");
        std::process::exit(1);
    }
}
