#!/bin/sh
# 把发布要用的凭据写进 GitHub 仓库的 Actions Secrets：macOS 签名与公证、在线更新签名私钥。
#
#   ./scripts/setup-ci-secrets.sh <Developer ID 证书.p12> <AuthKey_XXXXXXXXXX.p8> [更新签名私钥]
#
# 更新签名私钥默认取 ~/.config/aurio/update_signing.key（aurio-sign keygen 生成的），
# 写入前会核对它和程序里内置的公钥是否配对。
#
# 运行时会提示输入证书密码和 Issuer ID（不回显、不落盘）。需要已登录的 gh（gh auth login），
# 并且当前仓库已经关联到 GitHub。写入的 Secrets：
#   MACOS_CERTIFICATE_P12       证书（base64）
#   MACOS_CERTIFICATE_PASSWORD  证书密码
#   APPLE_API_KEY_P8            App Store Connect API 密钥内容
#   APPLE_API_KEY_ID            密钥 ID
#   APPLE_API_ISSUER_ID         Issuer ID
#   UPDATE_SIGNING_KEY          在线更新清单的签名私钥
set -e
cd "$(dirname "$0")/.."

fail() { echo "错误：$1" >&2; exit 1; }

P12="$1"
P8="$2"
UPDATE_KEY="${3:-$HOME/.config/aurio/update_signing.key}"
[ -n "$P12" ] && [ -n "$P8" ] || fail "用法：$0 <证书.p12> <AuthKey_XXXXXXXXXX.p8>"
[ -f "$P12" ] || fail "找不到证书文件：$P12"
[ -f "$P8" ] || fail "找不到密钥文件：$P8"
[ -f "$UPDATE_KEY" ] || fail "找不到更新签名私钥：$UPDATE_KEY（用 cargo run -p aurio-tools --bin aurio-sign -- keygen <路径> 生成）"
# 私钥必须和程序内置的公钥配对，否则已安装的 Aurio 会拒绝所有更新
cargo build --release -q -p aurio-tools --bin aurio-sign
EXPECTED="$(sed -n 's/^pub const PUBLIC_KEY: &str = "\(.*\)";/\1/p' crates/aurio-update/src/lib.rs)"
ACTUAL="$(./target/release/aurio-sign pubkey --key-file "$UPDATE_KEY")" || fail "更新签名私钥格式不对"
[ "$ACTUAL" = "$EXPECTED" ] || fail "更新签名私钥和程序内置的公钥不配对（内置 $EXPECTED，私钥对应 $ACTUAL）"
echo "更新签名私钥和内置公钥配对正确"
command -v gh >/dev/null 2>&1 || fail "需要 GitHub CLI：brew install gh"
gh auth status >/dev/null 2>&1 || fail "gh 还没登录：先运行 gh auth login"
REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null)" \
    || fail "当前目录没有关联 GitHub 仓库：先 git remote add origin … 并推送"

# 密钥 ID 默认取自文件名 AuthKey_<ID>.p8
KEY_ID="$(basename "$P8" | sed -n 's/^AuthKey_\(.*\)\.p8$/\1/p')"
printf "API 密钥 ID [%s]：" "$KEY_ID"
read -r INPUT
[ -n "$INPUT" ] && KEY_ID="$INPUT"
[ -n "$KEY_ID" ] || fail "需要密钥 ID"

echo "Issuer ID 在 App Store Connect →「用户和访问」→「集成」→「App Store Connect API」页面顶部。"
printf "Issuer ID："
read -r ISSUER_ID
[ -n "$ISSUER_ID" ] || fail "需要 Issuer ID"

printf "证书 %s 的密码（不回显）：" "$(basename "$P12")"
trap 'stty echo; echo; exit 130' INT TERM
stty -echo
read -r P12_PASSWORD
stty echo
echo

# 在临时钥匙串里试导入，确认密码正确、证书里有 Developer ID Application 身份
TMP="$(mktemp -d)"
trap 'stty echo 2>/dev/null; security delete-keychain "$TMP/check.keychain-db" >/dev/null 2>&1; rm -rf "$TMP"' EXIT
security create-keychain -p check "$TMP/check.keychain-db"
security import "$P12" -k "$TMP/check.keychain-db" -P "$P12_PASSWORD" -f pkcs12 >/dev/null 2>&1 \
    || fail "证书导入失败：密码不对，或者文件不是 .p12 证书"
IDENTITY="$(security find-identity -v -p codesigning "$TMP/check.keychain-db" | grep '"Developer ID Application' | head -1 | sed -E 's/.*"(.*)".*/\1/')"
[ -n "$IDENTITY" ] || fail "证书里没有 Developer ID Application 身份（公证需要这种证书）"
echo "证书有效：$IDENTITY"

echo "写入 $REPO 的 Secrets…"
base64 -i "$P12" | gh secret set MACOS_CERTIFICATE_P12 --repo "$REPO"
printf '%s' "$P12_PASSWORD" | gh secret set MACOS_CERTIFICATE_PASSWORD --repo "$REPO"
gh secret set APPLE_API_KEY_P8 --repo "$REPO" < "$P8"
printf '%s' "$KEY_ID" | gh secret set APPLE_API_KEY_ID --repo "$REPO"
printf '%s' "$ISSUER_ID" | gh secret set APPLE_API_ISSUER_ID --repo "$REPO"
tr -d '\n' < "$UPDATE_KEY" | gh secret set UPDATE_SIGNING_KEY --repo "$REPO"

echo
echo "完成。推送 v* 标签（例如 git tag v0.2.7 && git push origin v0.2.7）就会自动打包、签名、公证并生成更新清单。"
echo "请备份 $UPDATE_KEY：丢失后，已安装的 Aurio 将无法验证新版本。"
