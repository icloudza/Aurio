#!/bin/sh
# 编译并打包 Mac 菜单栏 App：dist/Aurio.app（内含 Sparkle.framework，用于在线更新）
#
# 签名身份自动选择：Developer ID Application > Apple Development > ad-hoc。
# 也可以用环境变量 AURIO_SIGN_IDENTITY 指定。
# AURIO_REQUIRE_TIMESTAMP=1（公证时由 build-mac-dmg.sh 设置）：签名必须带可信时间戳。
set -e
cd "$(dirname "$0")/.."
ROOT="$PWD"

cargo build --release -p aurio-ffi
(cd mac/AurioMac && swift build -c release --product Aurio)
BUILD="$(cd mac/AurioMac && swift build -c release --show-bin-path)"

APP="$ROOT/dist/Aurio.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
cp "$BUILD/Aurio" "$APP/Contents/MacOS/Aurio"
cp mac/AurioMac/App/Info.plist "$APP/Contents/Info.plist"
cp mac/AurioMac/App/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Sparkle：保留符号链接拷贝。Aurio 没有开沙盒，用不到它的 XPC 服务，删掉以减小体积、简化签名
ditto "$BUILD/Sparkle.framework" "$APP/Contents/Frameworks/Sparkle.framework"
rm -rf "$APP/Contents/Frameworks/Sparkle.framework/Versions/B/XPCServices" \
       "$APP/Contents/Frameworks/Sparkle.framework/XPCServices"

ID="${AURIO_SIGN_IDENTITY:-}"
if [ -z "$ID" ]; then
    IDS="$(security find-identity -v -p codesigning 2>/dev/null)"
    ID="$(echo "$IDS" | grep '"Developer ID Application' | head -1 | sed -E 's/.*"(.*)".*/\1/')"
    [ -z "$ID" ] && ID="$(echo "$IDS" | grep '"Apple Development' | head -1 | sed -E 's/.*"(.*)".*/\1/')"
    [ -z "$ID" ] && ID="-"
fi

# 签名一个目标。参数：路径 [codesign 的其他参数…]
sign() {
    TARGET="$1"
    shift
    if [ "$ID" = "-" ]; then
        codesign --force --sign - "$@" "$TARGET"
    elif [ -n "${AURIO_REQUIRE_TIMESTAMP:-}" ]; then
        codesign --force --options runtime --timestamp --sign "$ID" "$@" "$TARGET"
    else
        # Developer ID 需要时间戳（要联网）；平时失败了就退回不带时间戳，不影响本机使用
        codesign --force --options runtime --timestamp --sign "$ID" "$@" "$TARGET" 2>/dev/null \
            || codesign --force --options runtime --timestamp=none --sign "$ID" "$@" "$TARGET"
    fi
}

if [ "$ID" = "-" ]; then echo "签名：ad-hoc（没有找到开发者证书）"; else echo "签名：$ID"; fi
# 由内到外：Sparkle 的辅助程序 → Sparkle 框架 → App
FW="$APP/Contents/Frameworks/Sparkle.framework"
sign "$FW/Versions/B/Autoupdate"
sign "$FW/Versions/B/Updater.app"
sign "$FW"
sign "$APP" --entitlements mac/AurioMac/App/Aurio.entitlements
codesign --verify --strict --deep "$APP"
echo
echo "完成：$APP"
echo "安装：把 Aurio.app 拖进「应用程序」文件夹，双击打开，菜单栏会出现 Aurio 图标。"
