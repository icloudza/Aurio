#!/bin/sh
# 编译 Mac 端：先编 Rust 静态库，再编 Swift 命令行工具。
set -e
cd "$(dirname "$0")/.."
cargo build --release -p aurio-ffi -p aurio-tools
cd mac/AurioMac
swift build -c release
echo
echo "完成：mac/AurioMac/.build/release/aurio-rx"
echo "      target/release/aurio-send、aurio-sim"
