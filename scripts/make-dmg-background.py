#!/usr/bin/env python3
"""生成 macOS 安装盘（DMG）的窗口背景。

    python3 scripts/make-dmg-background.py

产物：assets/dmg/background.tiff（含 1x 和 2x 两种分辨率，Retina 屏上清晰）
以及 background.png / background@2x.png 预览图。

需要 rsvg-convert（brew install librsvg）和 macOS 自带的 tiffutil。
图标位置要和 installer/dmg-settings.py 的 icon_locations 一致。
"""
import os
import subprocess
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "assets", "dmg")

W, H = 660, 400                 # Finder 窗口内容区大小（点）
APP_X, APPS_X, ICON_Y = 170, 490, 190   # 图标中心，和 dmg-settings.py 一致
ACCENT = "#14a08e"


def svg():
    # 箭头：画在两块白底之间，两侧各留 18pt
    x0, x1 = APP_X + 92 + 18, APPS_X - 92 - 18
    y = ICON_Y
    bars = [(4, 10, 14), (8, 7, 17), (12, 4, 20), (16, 7, 17), (20, 10, 14)]
    logo = "".join(
        f'<path d="M{24 + x * 0.75:.2f} {26 + y0 * 0.75:.2f}V{26 + y1 * 0.75:.2f}"/>' for x, y0, y1 in bars)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}">
<defs>
  <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#fbfbfa"/><stop offset="1" stop-color="#f1f1ee"/>
  </linearGradient>
</defs>
<rect width="{W}" height="{H}" fill="url(#bg)"/>

<!-- 左上角：标志和名称 -->
<rect x="20" y="22" width="26" height="26" rx="7" fill="{ACCENT}"/>
<g fill="none" stroke="#ffffff" stroke-width="1.9" stroke-linecap="round">{logo}</g>
<text x="56" y="41" font-family="PingFang SC, -apple-system, Helvetica Neue, sans-serif" font-size="15" font-weight="600" fill="#1f2937">Aurio</text>

<!-- 图标下方的浅色底板，让两个图标有落点；下沿留出 Finder 画文件名的位置 -->
<rect x="{APP_X - 92}" y="{ICON_Y - 84}" width="184" height="196" rx="28" fill="#ffffff" fill-opacity="0.65" stroke="#111827" stroke-opacity="0.05"/>
<rect x="{APPS_X - 92}" y="{ICON_Y - 84}" width="184" height="196" rx="28" fill="#ffffff" fill-opacity="0.65" stroke="#111827" stroke-opacity="0.05"/>

<!-- 箭头 -->
<g fill="none" stroke="{ACCENT}" stroke-width="3" stroke-linecap="round" stroke-linejoin="round">
  <path d="M{x0} {y}H{x1 - 4}" stroke-dasharray="1 9"/>
  <path d="M{x1 - 12} {y - 10}L{x1} {y}L{x1 - 12} {y + 10}"/>
</g>

<!-- 提示文字（Finder 会在图标下方画文件名，这里放在更下面） -->
<text x="{W / 2}" y="{H - 52}" text-anchor="middle" font-family="PingFang SC, -apple-system, Helvetica Neue, sans-serif" font-size="14" fill="#4b5563">把 Aurio 拖到「应用程序」文件夹即可完成安装</text>
</svg>
'''


def main():
    os.makedirs(OUT, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        src = os.path.join(tmp, "bg.svg")
        open(src, "w").write(svg())
        p1 = os.path.join(OUT, "background.png")
        p2 = os.path.join(OUT, "background@2x.png")
        subprocess.run(["rsvg-convert", "-w", str(W), "-h", str(H), src, "-o", p1], check=True)
        subprocess.run(["rsvg-convert", "-w", str(W * 2), "-h", str(H * 2), src, "-o", p2], check=True)
        tiff = os.path.join(OUT, "background.tiff")
        subprocess.run(["tiffutil", "-cathidpicheck", p1, p2, "-out", tiff], check=True)
    print("写入", os.path.relpath(tiff, ROOT))


if __name__ == "__main__":
    main()
