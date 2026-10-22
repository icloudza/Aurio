# Aurio

Aurio 让 Windows 电脑通过局域网，把 Mac 上的 Studio Display（或任意音频设备）当作自己的扬声器和麦克风使用。

- **播放**：Windows 的系统声音实时传到 Mac，从 Studio Display 播放。
- **麦克风**：Studio Display 的麦克风传回 Windows，任何应用都可以把它当普通麦克风使用。
- **回声消除**：可选。外放时开麦，对方听不到扬声器里的声音。
- **低延迟、不漂移**：有线局域网下端到端约 35–45 ms，其中 22 ms 是 Studio Display 自身的硬件延迟。两台电脑的时钟偏差会被持续校正，长时间播放不会积累延迟或爆音。

当前版本：**0.2.6**（[更新记录](CHANGELOG.md)）

## 工作方式

```
播放    Windows 系统声音 → WASAPI 回环采集 → UDP → Mac 抖动缓冲 / 漂移补偿 / 重采样 → Studio Display 扬声器
麦克风  Studio Display 麦克风 → Mac 上行（按需开启）→ UDP → Windows → CABLE Input → CABLE Output → 应用
控制    Windows 每 0.5 秒发一次心跳（附带「需要麦克风」），Mac 原样回应，用于检测连通和测量往返时间
```

- 播放方向不需要在 Windows 上安装任何驱动。
- 麦克风方向借用 [VB-Cable](https://vb-audio.com/Cable/) 虚拟音频线：Aurio 把 Mac 传来的声音写入「CABLE Input」，应用从「CABLE Output」录音。驱动可以在 Aurio 的设置页一键在线安装或卸载。
- 两端通过 Bonjour（`_aurio._udp`）自动发现，也可以手动填写 IP。

设计细节见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## 系统要求

| | 要求 |
|---|---|
| Mac | macOS 13 或更高版本 |
| Windows | Windows 10 / 11（64 位），Microsoft WebView2 运行时（Windows 11 自带） |
| 网络 | 两台电脑在同一局域网。推荐有线连接 |

## 快速开始

1. 从 GitHub Releases 下载安装包，或在本机编译（见 [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)）：
   - **Mac**：`Aurio-<版本>.dmg`，打开后把 Aurio 拖到「应用程序」，然后打开它，菜单栏会出现 Aurio 图标。
   - **Windows**：`Aurio-Setup-<版本>.exe`，运行安装。
2. 打开 Windows 上的 Aurio，选择找到的 Mac，点「连接」。

完整的使用说明见 [docs/USAGE.md](docs/USAGE.md)，编译环境的准备见 [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)。

## 仓库结构

```
crates/
  aurio-core/    两端共用的核心库：协议、抖动缓冲、漂移补偿、重采样、回声分析、离线仿真
  aurio-ffi/     给 Swift 调用的 C 接口（静态库 + include/aurio.h）
  aurio-win/     Windows 音频与系统集成：WASAPI 采集、麦克风回放、设备设置、驱动管理、Bonjour 发现
  aurio-tools/   开发工具：aurio-send（测试发送端）、aurio-sim（离线仿真）
apps/win-tauri/  Windows 图形界面（Tauri 2）
mac/AurioMac/    Mac 端：AurioKit（CoreAudio 引擎）、Aurio（菜单栏 App）、aurio-rx（命令行版）
installer/       Windows 安装包脚本（NSIS）、macOS 安装盘布局（dmgbuild）
scripts/         编译、打包、图标生成和 CI 配置脚本
assets/          应用图标母版（icon/）、安装盘背景（dmg/）
.github/         GitHub Actions：自动打包、签名、公证和发布
docs/            文档
```

## 文档

| 文档 | 内容 |
|---|---|
| [docs/USAGE.md](docs/USAGE.md) | 安装、使用、界面说明、常见问题 |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 架构、传输协议、缓冲与漂移补偿、延迟构成 |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | 开发环境、编译打包、代码结构、调试工具 |
| [docs/TESTING.md](docs/TESTING.md) | 自动测试与真机验收清单 |
| [docs/RELEASE.md](docs/RELEASE.md) | 自动打包、签名公证、发布流程与 CI 配置 |
| [CHANGELOG.md](CHANGELOG.md) | 版本更新记录 |
