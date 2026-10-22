# dmgbuild 的配置：macOS 安装盘的内容和 Finder 窗口布局。
# 由 scripts/build-mac-dmg.sh 调用：dmgbuild -s installer/dmg-settings.py -D app=<Aurio.app> <卷名> <输出.dmg>
# 图标位置要和 scripts/make-dmg-background.py 里画的底板对齐。
import os.path

app = defines.get("app", "dist/Aurio.app")  # noqa: F821（dmgbuild 注入 defines）
app_name = os.path.basename(app)

# 内容
files = [app]
# 链接名用中文，和背景上的提示文字一致
symlinks = {"应用程序": "/Applications"}
icon = "mac/AurioMac/App/AppIcon.icns"          # 挂载后卷的图标

# 压缩格式：ULFO（lzfse）体积小、解压快，macOS 10.11 起支持
format = "ULFO"
filesystem = "HFS+"

# 窗口
background = "assets/dmg/background.tiff"
# 高度含标题栏（约 28pt），内容区正好是背景图的 660×400
window_rect = ((200, 160), (660, 428))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False

# 图标
icon_size = 128
text_size = 13
icon_locations = {
    app_name: (170, 190),
    "应用程序": (490, 190),
}
arrange_by = None
hide_extension = [app_name]
