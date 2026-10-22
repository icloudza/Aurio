#!/usr/bin/env python3
"""生成 Aurio 的应用图标（方案 1 · 声柱）。

    python3 scripts/make-icons.py

产物：
- assets/icon/*.svg                     母版（macOS 1024、Windows 256 / 小尺寸）
- mac/AurioMac/App/AppIcon.icns         macOS 应用图标（iconutil 打包）
- apps/win-tauri/icons/icon.ico         Windows 多尺寸图标（16–256，PNG 压缩）
- apps/win-tauri/icons/icon.png、32x32.png
- apps/win-tauri/icons/tray-dark.rgba、tray-light.rgba  托盘图标（32×32 原始 RGBA，白色给深色任务栏、黑色给浅色任务栏）

需要 rsvg-convert（brew install librsvg）、macOS 自带的 iconutil，以及 Pillow（读托盘 PNG 的像素）。
菜单栏图标是单色模板图，在 Swift 代码里直接画（AurioApp.swift），不在这里生成。
"""
import math
import os
import shutil
import struct
import subprocess
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets", "icon")

TOP, BOT = "#22b9a4", "#0c8676"          # 底板渐变（上 → 下）
BARS5 = [(4, 10, 14), (8, 7, 17), (12, 4, 20), (16, 7, 17), (20, 10, 14)]   # viewBox 24 里的竖条 (x, y0, y1)


def squircle(cx, cy, a, n=5.0, steps=720):
    """超椭圆 |x/a|^n + |y/a|^n = 1，接近 macOS 图标的连续圆角外形。"""
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        x = cx + a * math.copysign(abs(c) ** (2 / n), c)
        y = cy + a * math.copysign(abs(s) ** (2 / n), s)
        pts.append(f"{x:.2f},{y:.2f}")
    return "M" + " L".join(pts) + " Z"


def bars(bars, cx, cy, glyph, stroke_units=2.0):
    """把 viewBox 24 的竖条放大到 glyph 像素宽，中心在 (cx, cy)。"""
    k = glyph / 24
    ox, oy = cx - glyph / 2, cy - glyph / 2
    d = " ".join(f"M{ox + x * k:.2f} {oy + y0 * k:.2f}V{oy + y1 * k:.2f}" for x, y0, y1 in bars)
    return (f'<path d="{d}" fill="none" stroke="#ffffff" stroke-width="{stroke_units * k:.2f}" '
            f'stroke-linecap="round"/>')


def mac_svg():
    # Apple 图标网格：1024 画布，图形 824，四周留 100 给阴影
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<defs>
  <linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{TOP}"/><stop offset="1" stop-color="{BOT}"/>
  </linearGradient>
  <linearGradient id="hl" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#ffffff" stop-opacity="0.22"/><stop offset="0.5" stop-color="#ffffff" stop-opacity="0"/>
  </linearGradient>
  <filter id="sh" x="-20%" y="-20%" width="140%" height="140%">
    <feGaussianBlur in="SourceAlpha" stdDeviation="14"/>
    <feOffset dy="12" result="b"/>
    <feComponentTransfer><feFuncA type="linear" slope="0.32"/></feComponentTransfer>
    <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
  </filter>
</defs>
<g filter="url(#sh)"><path d="{squircle(512, 512, 412)}" fill="url(#g)"/></g>
<path d="{squircle(512, 512, 410)}" fill="none" stroke="url(#hl)" stroke-width="4"/>
{bars(BARS5, 512, 512, 824 * 0.56)}
</svg>
'''


def win_svg(size):
    # Windows：底板几乎铺满，圆角 20%，不加阴影（任务栏上会显脏）
    m = 0 if size <= 48 else size * 0.03
    s = size - 2 * m
    r = s * 0.2
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#1aae9a"/><stop offset="1" stop-color="#0f9282"/>
</linearGradient></defs>
<rect x="{m:.2f}" y="{m:.2f}" width="{s:.2f}" height="{s:.2f}" rx="{r:.2f}" fill="url(#g)"/>
{bars(BARS5, size / 2, size / 2, s * 0.6)}
</svg>
'''


# 小尺寸手工对齐像素：每根竖条 2px 宽、落在整数像素上，底板边缘也在整数像素上。
# 格式：尺寸 → (底板边距, 圆角, [(竖条左边 x, 上 y, 下 y), ...])
PIXEL = {
    "mac": {
        16: (1, 3.2, [(4, 6, 10), (7, 3, 13), (10, 6, 10)]),
        32: (2, 6.4, [(7, 14, 18), (11, 11, 21), (15, 8, 24), (19, 11, 21), (23, 14, 18)]),
    },
    "win": {
        16: (0, 3.2, [(4, 5, 11), (7, 3, 13), (10, 5, 11)]),
        20: (0, 4, [(5, 7, 13), (9, 4, 16), (13, 7, 13)]),
        24: (0, 4.8, [(7, 8, 16), (11, 5, 19), (15, 8, 16)]),
        32: (0, 6.4, [(7, 14, 18), (11, 11, 21), (15, 8, 24), (19, 11, 21), (23, 14, 18)]),
    },
}


def pixel_svg(kind, size):
    m, r, bars_ = PIXEL[kind][size]
    top, bot = (TOP, BOT) if kind == "mac" else ("#1aae9a", "#0f9282")
    rects = "".join(f'<rect x="{x}" y="{y0}" width="2" height="{y1 - y0}" rx="1" fill="#ffffff"/>' for x, y0, y1 in bars_)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bot}"/>
</linearGradient></defs>
<rect x="{m}" y="{m}" width="{size - 2 * m}" height="{size - 2 * m}" rx="{r}" fill="url(#g)"/>
{rects}
</svg>
'''


def tray_svg(color):
    """托盘图标：只有图形没有底板。按 32px 画，每根竖条 4px（缩到 16px 正好 2px）。"""
    bars_ = [(2, 12, 20), (8, 7, 25), (14, 3, 29), (20, 7, 25), (26, 12, 20)]
    rects = "".join(f'<rect x="{x}" y="{y0}" width="4" height="{y1 - y0}" rx="2" fill="{color}"/>' for x, y0, y1 in bars_)
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32">{rects}</svg>'


def render(svg_text, out_png, px):
    with tempfile.NamedTemporaryFile("w", suffix=".svg", delete=False) as f:
        f.write(svg_text)
        path = f.name
    subprocess.run(["rsvg-convert", "-w", str(px), "-h", str(px), path, "-o", out_png], check=True)
    os.unlink(path)


def write_ico(pngs, out):
    """ICO 文件：每个尺寸直接存 PNG（Vista 起支持）。"""
    entries, blobs = [], []
    offset = 6 + 16 * len(pngs)
    for px, path in pngs:
        data = open(path, "rb").read()
        dim = 0 if px >= 256 else px
        entries.append(struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset))
        blobs.append(data)
        offset += len(data)
    with open(out, "wb") as f:
        f.write(struct.pack("<HHH", 0, 1, len(pngs)))
        f.write(b"".join(entries))
        f.write(b"".join(blobs))


def main():
    os.makedirs(ASSETS, exist_ok=True)
    full = mac_svg()
    open(os.path.join(ASSETS, "mac-1024.svg"), "w").write(full)
    open(os.path.join(ASSETS, "win-256.svg"), "w").write(win_svg(256))
    for kind in PIXEL:
        for px in PIXEL[kind]:
            open(os.path.join(ASSETS, f"{kind}-{px}.svg"), "w").write(pixel_svg(kind, px))

    tmp = tempfile.mkdtemp()
    try:
        # macOS：iconset → icns。16、32px 用像素对齐的手工版本
        iconset = os.path.join(tmp, "AppIcon.iconset")
        os.makedirs(iconset)
        for pt in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                px = pt * scale
                name = f"icon_{pt}x{pt}{'@2x' if scale == 2 else ''}.png"
                svg = pixel_svg("mac", px) if px in PIXEL["mac"] else full
                render(svg, os.path.join(iconset, name), px)
        icns = os.path.join(ROOT, "mac", "AurioMac", "App", "AppIcon.icns")
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", icns], check=True)
        print("写入", os.path.relpath(icns, ROOT))

        # Windows：多尺寸 ico。32px 及以下用像素对齐的手工版本
        win_dir = os.path.join(ROOT, "apps", "win-tauri", "icons")
        pngs = []
        for px in (16, 20, 24, 32, 40, 48, 64, 96, 128, 256):
            p = os.path.join(tmp, f"win-{px}.png")
            render(pixel_svg("win", px) if px in PIXEL["win"] else win_svg(px), p, px)
            pngs.append((px, p))
        write_ico(pngs, os.path.join(win_dir, "icon.ico"))
        shutil.copy(os.path.join(tmp, "win-32.png"), os.path.join(win_dir, "32x32.png"))
        render(win_svg(512), os.path.join(win_dir, "icon.png"), 512)
        print("写入 apps/win-tauri/icons/icon.ico、icon.png、32x32.png")

        # 托盘：Tauri 直接吃 RGBA 像素，省得在程序里解码 PNG
        from PIL import Image
        for name, color in (("tray-dark", "#ffffff"), ("tray-light", "#1b1b1b")):
            svg = tray_svg(color)
            open(os.path.join(ASSETS, f"{name}.svg"), "w").write(svg)
            png = os.path.join(tmp, f"{name}.png")
            render(svg, png, 32)
            data = Image.open(png).convert("RGBA").tobytes()
            assert len(data) == 32 * 32 * 4
            open(os.path.join(win_dir, f"{name}.rgba"), "wb").write(data)
        print("写入 apps/win-tauri/icons/tray-dark.rgba、tray-light.rgba")

        # 预览图（只给人看，不打包）
        render(full, os.path.join(ASSETS, "preview-mac-1024.png"), 1024)
    finally:
        shutil.rmtree(tmp)


if __name__ == "__main__":
    main()
