#!/bin/sh
# 编译并打包 Mac 菜单栏 App：dist/Aurio.app
# 签名身份自动选择：Developer ID Application > Apple Development > ad-hoc。
# 也可以用环境变量 AURIO_SIGN_IDENTITY 指定。
set -e
cd "$(dirname "$0")/.."
ROOT="$PWD"

cargo build --release -p aurio-ffi
(cd mac/AurioMac && swift build -c release --product Aurio)
BIN="$(cd mac/AurioMac && swift build -c release --show-bin-path)/Aurio"

APP="$ROOT/dist/Aurio.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Aurio"
cp mac/AurioMac/App/Info.plist "$APP/Contents/Info.plist"
cp mac/AurioMac/App/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"

ID="${AURIO_SIGN_IDENTITY:-}"
if [ -z "$ID" ]; then
    IDS="$(security find-identity -v -p codesigning 2>/dev/null)"
    ID="$(echo "$IDS" | grep '"Developer ID Application' | head -1 | sed -E 's/.*"(.*)".*/\1/')"
    [ -z "$ID" ] && ID="$(echo "$IDS" | grep '"Apple Development' | head -1 | sed -E 's/.*"(.*)".*/\1/')"
    [ -z "$ID" ] && ID="-"
fi

ENT=mac/AurioMac/App/Aurio.entitlements
if [ "$ID" = "-" ]; then
    echo "签名：ad-hoc（没有找到开发者证书）"
    codesign --force --sign - --entitlements "$ENT" "$APP"
else
    echo "签名：$ID"
    # Developer ID 需要时间戳（要联网）。公证时（AURIO_REQUIRE_TIMESTAMP=1）必须带时间戳；
    # 平时失败了就退回不带时间戳，不影响本机使用
    if [ -n "${AURIO_REQUIRE_TIMESTAMP:-}" ]; then
        codesign --force --options runtime --timestamp --entitlements "$ENT" --sign "$ID" "$APP"
    else
        codesign --force --options runtime --timestamp --entitlements "$ENT" --sign "$ID" "$APP" 2>/dev/null \
            || codesign --force --options runtime --timestamp=none --entitlements "$ENT" --sign "$ID" "$APP"
    fi
fi
codesign --verify --strict "$APP"
echo
echo "完成：$APP"
echo "安装：把 Aurio.app 拖进「应用程序」文件夹，双击打开，菜单栏会出现扬声器图标。"
