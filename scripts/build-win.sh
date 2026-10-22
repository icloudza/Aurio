#!/bin/sh
# 在 Mac 上交叉编译 Windows 版：图形界面 Aurio.exe + 命令行 aurio-win.exe。
# 需要：mingw-w64、makensis（Mac：brew install mingw-w64 makensis；Ubuntu：apt install mingw-w64 nsis zip）
#       rustup toolchain install 1.91.0 --target x86_64-pc-windows-gnu
# （Tauri 要求 Rust 1.90+，这里单独用 1.91，不影响系统默认的 Rust 版本）
set -e
cd "$(dirname "$0")/.."
T=x86_64-pc-windows-gnu
cargo +1.91.0 build --release --target $T -p aurio-win -p aurio-win-app
OUT=dist/Aurio-Windows
rm -rf "$OUT" && mkdir -p "$OUT"
x86_64-w64-mingw32-strip -o "$OUT/Aurio.exe" target/$T/release/Aurio.exe
x86_64-w64-mingw32-strip -o "$OUT/aurio-win.exe" target/$T/release/aurio-win.exe
cp target/$T/release/WebView2Loader.dll "$OUT/"
# VB-Cable 不再随包附带：Aurio 设置里的「安装驱动」会从官网下载
# 安装包（NSIS，需要 brew install makensis）
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
SETUP="dist/Aurio-Setup-$VERSION.exe"
rm -f dist/Aurio-Setup-*.exe
# makensis 要在 UTF-8 区域设置下才能读中文脚本。Linux（CI）上用 NSIS_LOCALE=C.UTF-8
LC_ALL="${NSIS_LOCALE:-en_US.UTF-8}" makensis -V2 -INPUTCHARSET UTF8 -DVERSION="$VERSION" -DSRC="$PWD/$OUT" -DOUTFILE="$PWD/$SETUP" installer/aurio.nsi
# 免安装版（解压即用）
rm -f dist/Aurio-Windows.zip dist/aurio-win.exe dist/VBCABLE_Driver_Pack45.zip
(cd dist && zip -qr Aurio-Windows.zip Aurio-Windows)
echo
echo "完成："
echo "  $SETUP（安装包）"
echo "  dist/Aurio-Windows.zip（免安装版，解压后双击 Aurio.exe）"
