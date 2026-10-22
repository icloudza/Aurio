# 开发指南

两端都在一台 Mac 上编译：Mac 版本地编译，Windows 版用 MinGW 交叉编译。

## 1. 环境准备

| 用途 | 需要 |
|---|---|
| 共同 | [rustup](https://rustup.rs/)，Rust 稳定版（1.89 及以上） |
| Mac 版 | Xcode 或 Xcode 命令行工具（Swift 5.9 及以上） |
| Windows 版 | `brew install mingw-w64 makensis`；`rustup toolchain install 1.91.0 --target x86_64-pc-windows-gnu` |
| 重新生成图标 | `brew install librsvg`；Python 3 和 Pillow（`pip3 install pillow`） |
| macOS 安装盘 | `pip3 install dmgbuild` |

Windows 图形界面基于 Tauri 2，要求 Rust 1.90 及以上，所以 Windows 版固定使用 1.91.0 工具链编译，不影响系统默认的 Rust 版本。交叉编译所需的链接器和静态链接选项已经写在 `.cargo/config.toml` 中。

## 2. 编译与打包

| 命令 | 产物 |
|---|---|
| `./scripts/build-mac-app.sh` | `dist/Aurio.app`：菜单栏 App，已签名 |
| `./scripts/build-mac-dmg.sh` | `dist/Aurio-<版本>.dmg`：安装盘，可选公证（见 [RELEASE.md](RELEASE.md#5-本地打包)） |
| `./scripts/build-mac.sh` | `mac/AurioMac/.build/release/aurio-rx`（命令行版）、`target/release/aurio-send`、`aurio-sim` |
| `./scripts/build-win.sh` | `dist/Aurio-Setup-<版本>.exe`（安装包）、`dist/Aurio-Windows.zip`（免安装版） |
| `python3 scripts/make-icons.py` | 重新生成两端的应用图标和 Windows 托盘图标 |
| `python3 scripts/make-dmg-background.py` | 重新生成安装盘的窗口背景 |

### Mac 签名

`build-mac-app.sh` 按以下顺序自动选择签名身份：Developer ID Application → Apple Development → ad-hoc。也可以用环境变量 `AURIO_SIGN_IDENTITY` 指定。签名启用了 hardened runtime，授权文件 `mac/AurioMac/App/Aurio.entitlements` 只申请了麦克风权限（`com.apple.security.device.audio-input`）。

本地运行 `build-mac-app.sh` 只签名、不公证，适合在自己的设备上使用。要分发给他人，请用 `build-mac-dmg.sh` 或 CI 打出经过公证的安装盘，见 [RELEASE.md](RELEASE.md)。

### Windows 安装包

安装脚本是 `installer/aurio.nsi`（NSIS，UTF-8 带 BOM），由 `build-win.sh` 调用，版本号取自 `Cargo.toml`。安装包包含：

- `Aurio.exe`（图形界面）
- `aurio-win.exe`（命令行版）
- `WebView2Loader.dll`
- 开始菜单快捷方式、自定义卸载流程（卸载前先检测并卸载虚拟声卡驱动，结束后提示重启）
- 可选的桌面快捷方式和开机自启（`Aurio.exe --tray`）

安装包不包含 VB-Cable 驱动，驱动由软件在线下载安装。Windows 程序没有代码签名。

## 3. 代码结构

```
crates/aurio-core/src/
  proto.rs       包头、采样格式、编解码
  receiver.rs    网络线程：解析、按 sample_index 对齐、写入环形缓冲
  playout.rs     渲染线程：取数据、漂移补偿、重采样、淡入
  drift.rs       PI 控制器与自适应目标水位
  resampler.rs   Kaiser 窗 sinc 可变比例重采样
  sender.rs      打包与发送
  uplink.rs      麦克风上行（下混、重采样、打包）
  echo.rs        回声分析（互相关）
  sim.rs         离线仿真
  stats.rs、rt.rs、wav.rs
crates/aurio-ffi/          C 接口，头文件 include/aurio.h
crates/aurio-update/src/
  lib.rs         更新清单格式、版本比较、镜像地址、Ed25519 签名与验签（不依赖操作系统）
  client.rs      取清单、带回退和续传的下载（client 功能，Windows 端使用）
crates/aurio-tools/src/bin/
  aurio-sign.rs  生成更新签名密钥、签名文件、生成发布用的 latest.json 和 appcast.xml
crates/aurio-win/src/win/
  capture.rs     WASAPI 回环采集与静音补包
  mic.rs         麦克风接收、写入 CABLE Input、心跳与录音检测
  engine.rs      把采集、发送和麦克风组合成一个引擎，汇总状态
  setup.rs       设置检查与一键修复
  policy.rs      设置默认设备（IPolicyConfig）
  driver.rs      VB-Cable 下载、校验、提权安装与卸载
  discover.rs    mDNS 发现
  echotest.rs    录音回放测试
  theme.rs       任务栏浅色 / 深色检测
  update.rs      在线更新：状态、下载、更新助手（静默安装后重新打开）
apps/win-tauri/
  src/main.rs    Tauri 命令、托盘、窗口生命周期、驱动辅助进程入口
  ui/index.html  界面（单文件，无构建步骤）
mac/AurioMac/Sources/
  AurioKit/      AurioEngine（引擎与状态）、HALOutput / HALInput、VoiceIO（回声消除）、Devices
  Aurio/         菜单栏 App：AurioApp（入口、设置、菜单栏图标）、MenuView（面板）、Components（控件）、
                 UpdateController（Sparkle 在线更新与镜像选择）
  aurio-rx/      命令行版
```

## 4. 测试与调试工具

**单元测试**

```bash
cargo test --release -p aurio-core
```

**在线更新**：签名、清单，以及模拟各种镜像故障的下载测试。

```bash
cargo test --release -p aurio-update --features client
```

测试时可以用环境变量 `AURIO_UPDATE_MIRRORS` 替换 Windows 端的镜像列表（逗号分隔；本机地址允许 http）。

**离线仿真**：模拟发送端时钟偏差、采集周期、抖动和丢包，几秒钟就能跑完 24 小时。

```bash
./target/release/aurio-sim --hours 24 --ppm 80 --jitter-ms 2
```

选项：`--seconds N | --hours N`、`--ppm N`、`--period-ms N`、`--jitter-ms N`、`--loss 0..1`、`--buffer N`、`--target-ms N`、`--taps N`。

**测试发送端**：没有 Windows 时，可以在 Mac 上向 Aurio 发送测试信号。

```bash
./target/release/aurio-send --to 127.0.0.1:47800 --signal sine --period-ms 10 --ppm 150
```

选项：`--signal sine|click|noise|wav`（`wav` 需要配合 `--file`）、`--freq`、`--level-db`、`--period-ms`、`--fpp`、`--ppm`、`--jitter-ms`、`--seconds`。加上 `--mic-request` 可以模拟 Windows 请求麦克风，并统计收到的麦克风数据。

**Mac 命令行版**：每秒打印一行水位、修正量、包间隔和欠载统计，便于观察控制器行为。

```bash
mac/AurioMac/.build/release/aurio-rx run --device "Studio Display"
```

**Mac App 的开发用环境变量**

| 变量 | 作用 |
|---|---|
| `AURIO_SNAPSHOT=<目录>` | 用假数据把菜单栏面板和菜单栏图标渲染成 PNG 后退出，不打开音频和网络。用于检查界面 |
| `AURIO_AEC_TEST=<结果文件>` | 运行回声消除对比测试，把结果写入文件后退出 |
| `AURIO_PORT`、`AURIO_MUTE`、`AURIO_MIC_TONE` | 见 [USAGE.md 附录](USAGE.md#附录命令行与环境变量) |

## 5. 版本号

发布新版本时需要同时修改以下 5 处，CI 会核对它们是否一致：

- `Cargo.toml`：workspace 的 `version`
- `apps/win-tauri/tauri.conf.json`：`version`
- `mac/AurioMac/App/Info.plist`：`CFBundleShortVersionString` 和 `CFBundleVersion`
- `installer/aurio.nsi`：`VERSION` 的默认值（`build-win.sh` 会传入实际版本）
- `apps/win-tauri/ui/index.html`：设置页底部的版本号

打包、签名、公证和发布的完整流程见 [RELEASE.md](RELEASE.md)。

## 6. 编码约定

- 实时音频回调中不分配内存、不加锁、不做系统调用，跨线程只用无锁环形缓冲和原子变量。
- 新的跨平台逻辑放进 `aurio-core`，保持它不依赖操作系统，便于单元测试和仿真。
- 界面文字和代码注释使用简体中文，面向用户的文案直接说明结果和操作步骤。
