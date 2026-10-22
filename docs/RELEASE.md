# 发布

Aurio 用 GitHub Actions 自动打包两端，工作流是 [`.github/workflows/release.yml`](../.github/workflows/release.yml)。

## 1. 产物

| 文件 | 平台 | 说明 |
|---|---|---|
| `Aurio-<版本>.dmg` | macOS | 安装盘：打开后把 Aurio 拖到「应用程序」。App 和安装盘都经过 Developer ID 签名和苹果公证，并装订了公证票据 |
| `Aurio-Setup-<版本>.exe` | Windows | 安装包（NSIS） |
| `Aurio-<版本>-Windows.zip` | Windows | 免安装版 |
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
5. **发布**：生成校验和，从 [CHANGELOG.md](../CHANGELOG.md) 提取本版本的说明，创建 Release 草稿。确认无误后，在 GitHub 网页上手动点「Publish release」。

## 3. 配置 Secrets（只需一次）

macOS 签名和公证需要以下 Secrets。没有配置时，macOS 版仍会打包，但只做临时签名、不公证，别人下载后会被 Gatekeeper 拦截。

| Secret | 内容 |
|---|---|
| `MACOS_CERTIFICATE_P12` | Developer ID Application 证书（含私钥）导出的 `.p12` 文件，base64 编码 |
| `MACOS_CERTIFICATE_PASSWORD` | 导出 `.p12` 时设置的密码 |
| `APPLE_API_KEY_P8` | App Store Connect API 密钥文件（`AuthKey_XXXXXXXXXX.p8`）的内容 |
| `APPLE_API_KEY_ID` | 密钥 ID（文件名里 `AuthKey_` 后面那串） |
| `APPLE_API_ISSUER_ID` | Issuer ID：App Store Connect →「用户和访问」→「集成」→「App Store Connect API」页面顶部 |

用脚本一次配置（需要已登录的 `gh`，并且仓库已经推送到 GitHub）：

```bash
./scripts/setup-ci-secrets.sh <证书.p12> <AuthKey_XXXXXXXXXX.p8>
```

脚本会提示输入证书密码和 Issuer ID（不回显、不落盘）。它会先在临时钥匙串里验证密码是否正确、证书里是否有 Developer ID Application 身份，再写入 Secrets。

**准备凭据**
- **证书**：在「钥匙串访问」中找到「Developer ID Application: …」，展开后连同私钥一起选中，右键「导出」为 `.p12`，并设置密码。
- **API 密钥**：App Store Connect →「用户和访问」→「集成」→「App Store Connect API」，新建一个「开发者」权限的密钥，下载 `.p8` 文件（只能下载一次）。

请妥善保管 `.p12` 和 `.p8` 文件，不要提交到仓库。

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
