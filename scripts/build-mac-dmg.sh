#!/bin/sh
# 打包 macOS 安装盘：dist/Aurio-<版本>.dmg
#
# 流程：编译并签名 Aurio.app →（可选）公证并装订 App → 生成 DMG → 签名 DMG →（可选）公证并装订 DMG
#
# 需要：pip 安装的 dmgbuild（pip3 install dmgbuild，可用环境变量 DMGBUILD 指定可执行文件）。
#
# 公证（可选）：设置以下三个环境变量就会提交苹果公证，否则跳过：
#   APPLE_API_KEY_PATH   App Store Connect API 密钥文件（AuthKey_XXXXXXXXXX.p8）的路径
#   APPLE_API_KEY_ID     密钥 ID
#   APPLE_API_ISSUER_ID  Issuer ID（App Store Connect →「用户和访问」→「集成」页面顶部）
#
# 其他环境变量：
#   AURIO_SIGN_IDENTITY  指定签名身份（默认自动选择 Developer ID Application）
#   SKIP_APP_BUILD=1     不重新编译 App，直接用 dist/Aurio.app
set -e
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Aurio.app
DMG="dist/Aurio-$VERSION.dmg"
DMGBUILD="${DMGBUILD:-dmgbuild}"

command -v "$DMGBUILD" >/dev/null 2>&1 || { echo "找不到 dmgbuild：先运行 pip3 install dmgbuild" >&2; exit 1; }

NOTARIZE=
if [ -n "${APPLE_API_KEY_PATH:-}" ] && [ -n "${APPLE_API_KEY_ID:-}" ] && [ -n "${APPLE_API_ISSUER_ID:-}" ]; then
    NOTARIZE=1
    # 公证要求签名带可信时间戳，不允许退回无时间戳签名
    export AURIO_REQUIRE_TIMESTAMP=1
fi

if [ "${SKIP_APP_BUILD:-}" != 1 ]; then
    ./scripts/build-mac-app.sh
fi
[ -d "$APP" ] || { echo "找不到 $APP" >&2; exit 1; }

# 签名身份：和 build-mac-app.sh 相同的选择规则
ID="${AURIO_SIGN_IDENTITY:-}"
if [ -z "$ID" ]; then
    ID="$(security find-identity -v -p codesigning 2>/dev/null | grep '"Developer ID Application' | head -1 | sed -E 's/.*"(.*)".*/\1/')"
fi

# 提交公证并等待结果；失败时打印苹果返回的日志
notarize() {
    echo "提交公证：$1"
    OUT="$(xcrun notarytool submit "$1" \
        --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" \
        --wait --timeout 30m --output-format json)"
    STATUS="$(printf '%s' "$OUT" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')"
    SUB_ID="$(printf '%s' "$OUT" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')"
    echo "公证结果：$STATUS（$SUB_ID）"
    if [ "$STATUS" != "Accepted" ]; then
        xcrun notarytool log "$SUB_ID" \
            --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" || true
        exit 1
    fi
}

# 1. 先公证 App 并装订票据：用户把 App 拷出 DMG 后，离线首次打开也能通过 Gatekeeper
if [ -n "$NOTARIZE" ]; then
    ZIP="$(mktemp -d)/Aurio.zip"
    ditto -c -k --keepParent "$APP" "$ZIP"
    notarize "$ZIP"
    rm -f "$ZIP"
    xcrun stapler staple "$APP"
fi

# 2. 生成 DMG
rm -f "$DMG"
"$DMGBUILD" -s installer/dmg-settings.py -D app="$APP" "Aurio" "$DMG"

# 3. 签名 DMG
if [ -n "$ID" ]; then
    echo "签名 DMG：$ID"
    if [ -n "$NOTARIZE" ]; then
        codesign --force --timestamp --sign "$ID" "$DMG"
    else
        codesign --force --timestamp --sign "$ID" "$DMG" 2>/dev/null || codesign --force --timestamp=none --sign "$ID" "$DMG"
    fi
else
    echo "没有找到 Developer ID 证书，DMG 不签名"
fi

# 4. 公证 DMG 并装订
if [ -n "$NOTARIZE" ]; then
    notarize "$DMG"
    xcrun stapler staple "$DMG"
    spctl -a -t open --context context:primary-signature -v "$DMG"
else
    echo "没有设置公证用的环境变量，跳过公证（分发给他人前需要公证）"
fi

echo
echo "完成：$DMG"
