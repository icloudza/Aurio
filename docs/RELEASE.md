# 发布

Aurio 用 GitHub Actions 自动打包两端，工作流是 [`.github/workflows/release.yml`](../.github/workflows/release.yml)。

## 1. 产物

| 文件 | 平台 | 说明 |
|---|---|---|
| `Aurio-<版本>.dmg` | macOS | 安装盘：打开后把 Aurio 拖到「应用程序」。App 和安装盘都经过 Developer ID 签名和苹果公证，并装订了公证票据 |
| `Aurio-Setup-<版本>.exe` | Windows | 安装包（NSIS） |
| `Aurio-<版本>-Windows.zip` | Windows | 免安装版 |
| `latest.json`、`latest.json.sig` | Windows | 签名的更新清单，已安装的 Aurio 据此检查和下载新版本 |
| `appcast.xml` | macOS | Sparkle 的更新清单 |
| `SHA256SUMS.txt` | — | 以上文件的 SHA-256 校验和 |

macOS 版目前只编译 Apple Silicon（arm64）。

## 2. 工作流

| 触发方式 | 行为 |
|---|---|
| 推送 `v*` 标签（例如 `v0.2.6`） | 核对版本号 → 单元测试 → 并行打包两端 → 创建 **GitHub Release 草稿**并上传产物 |
| 手动运行（Actions →「打包发布」→ Run workflow） | 同样打包，但不创建 Release，产物在本次运行的 Artifacts 里下载 |

各步骤：

1. **检查版本号**：标签必须等于 `Cargo.toml` 的版本，`tauri.conf.json`、`Info.plist` 和 Windows 设置页的版本号也必须一致，否则直接失败。
2. **核心库测试**：`cargo test --release -p aurio-core`。
3. **Windows**：在 Ubuntu 上用 MinGW 交叉编译，运行 `scripts/build-win.sh`，和本地打包的工具链相同。
4. **macOS**：在 macOS 上导入签名证书到临时钥匙串，运行 `scripts/build-mac-dmg.sh`：编译签名 App → 公证并装订 App → 生成 DMG → 签名 DMG → 公证并装订 DMG。结束后删除临时钥匙串和密钥文件。
5. **发布**：从 [CHANGELOG.md](../CHANGELOG.md) 提取本版本的说明，用 `aurio-sign` 给两个安装包签名并生成更新清单，生成校验和，创建 Release 草稿。确认无误后，在 GitHub 网页上手动点「Publish release」。

**草稿和预发布版本不会推送给用户。** 在线更新读取的是 `releases/latest/download/…`，GitHub 只把已发布、且不是预发布的最新版本算作 latest。点「Publish release」之后，已安装的 Aurio 才能检查到这个版本。

## 3. 配置 Secrets（只需一次）

macOS 签名和公证需要以下 Secrets。没有配置时，macOS 版仍会打包，但只做临时签名、不公证，别人下载后会被 Gatekeeper 拦截。

| Secret | 内容 |
|---|---|
| `MACOS_CERTIFICATE_P12` | Developer ID Application 证书（含私钥）导出的 `.p12` 文件，base64 编码 |
| `MACOS_CERTIFICATE_PASSWORD` | 导出 `.p12` 时设置的密码 |
| `APPLE_API_KEY_P8` | App Store Connect API 密钥文件（`AuthKey_XXXXXXXXXX.p8`）的内容 |
| `APPLE_API_KEY_ID` | 密钥 ID（文件名里 `AuthKey_` 后面那串） |
| `APPLE_API_ISSUER_ID` | Issuer ID：App Store Connect →「用户和访问」→「集成」→「App Store Connect API」页面顶部 |
| `UPDATE_SIGNING_KEY` | 在线更新的 Ed25519 签名私钥（base64）。没有配置时 CI 不生成更新清单，已安装的 Aurio 收不到这次更新 |

用脚本一次配置（需要已登录的 `gh`，并且仓库已经推送到 GitHub）：

```bash
./scripts/setup-ci-secrets.sh <证书.p12> <AuthKey_XXXXXXXXXX.p8> [更新签名私钥]
```

更新签名私钥默认读取 `~/.config/aurio/update_signing.key`。写入前，脚本会核对这把私钥和程序内置的公钥是否配对。

脚本会提示输入证书密码和 Issuer ID（不回显、不落盘）。它会先在临时钥匙串里验证密码是否正确、证书里是否有 Developer ID Application 身份，再写入 Secrets。

**准备凭据**
- **证书**：在「钥匙串访问」中找到「Developer ID Application: …」，展开后连同私钥一起选中，右键「导出」为 `.p12`，并设置密码。
- **API 密钥**：App Store Connect →「用户和访问」→「集成」→「App Store Connect API」，新建一个「开发者」权限的密钥，下载 `.p8` 文件（只能下载一次）。

- **更新签名密钥**：用 `./target/release/aurio-sign keygen ~/.config/aurio/update_signing.key` 生成（需先 `cargo build --release -p aurio-tools`），私钥写入文件，终端打印的公钥要内置到两端程序里（见下文「更换密钥」）。仓库里内置的公钥对应发布者手里的那把私钥。

请妥善保管 `.p12`、`.p8` 和更新签名私钥，不要提交到仓库（`.gitignore` 已经屏蔽了 `*.p12`、`*.p8`）。

### 更新签名密钥

- **一定要备份私钥。** 私钥丢失后，已安装的 Aurio 无法再验证任何新版本，只能让用户手动下载安装一个内置了新公钥的版本。
- **更换密钥**：用 `aurio-sign keygen <新路径>` 生成新密钥对，把公钥同时写进 `crates/aurio-update/src/lib.rs` 的 `PUBLIC_KEY` 和 `mac/AurioMac/App/Info.plist` 的 `SUPublicEDKey`，再更新 Secret。注意：旧版本的用户必须先装上一个用旧私钥签名的过渡版本，才能收到之后用新私钥签名的版本。

## 4. 发布一个版本

1. 按 [DEVELOPMENT.md](DEVELOPMENT.md#5-版本号) 修改版本号，并在 [CHANGELOG.md](../CHANGELOG.md) 新增一节 `## [x.y.z] - 日期`。
2. 本地验证：`cargo test --release -p aurio-core`，按需本地打包并按 [TESTING.md](TESTING.md) 验收。
3. 提交并推送标签：

   ```bash
   git tag v0.2.6
   ```

   ```bash
   git push origin main v0.2.6
   ```

4. 等 Actions 跑完（公证通常需要几分钟），到 GitHub 的 Releases 页面检查草稿，然后发布。

重新运行失败的工作流时，如果 Release 草稿已经存在，产物会覆盖上传。

## 5. 本地打包

不经过 CI 也可以在本机打出相同的产物：

| 命令 | 产物 |
|---|---|
| `./scripts/build-mac-dmg.sh` | `dist/Aurio-<版本>.dmg`。需要 `pip3 install dmgbuild`；设置了 `APPLE_API_KEY_PATH`、`APPLE_API_KEY_ID`、`APPLE_API_ISSUER_ID` 三个环境变量时会同时公证 |
| `./scripts/build-win.sh` | Windows 安装包和免安装版 |

DMG 的窗口布局在 `installer/dmg-settings.py`，背景图由 `scripts/make-dmg-background.py` 生成到 `assets/dmg/`。两者的图标位置需要保持一致。

本地也可以生成更新清单（和 CI 相同）：

```bash
./target/release/aurio-sign release --key-file ~/.config/aurio/update_signing.key --version 0.2.7 --notes notes.md --windows dist/Aurio-Setup-0.2.7.exe --macos dist/Aurio-0.2.7.dmg --out dist
```

## 6. 维护镜像列表

公益镜像随时可能失效。内置列表在两处，需要同时修改：

- `crates/aurio-update/src/lib.rs` 的 `DEFAULT_MIRRORS`（Windows）
- `mac/AurioMac/Sources/Aurio/UpdateController.swift` 的 `mirrors`（macOS）

修改只对之后发布的版本生效。所有镜像都失效时，两端会自动回退到 GitHub 官方，不影响更新，只是在国内可能较慢。如果需要更稳定的国内速度，可以在 CI 中把安装包同步到自己的国内对象存储，再把地址加进镜像列表。
