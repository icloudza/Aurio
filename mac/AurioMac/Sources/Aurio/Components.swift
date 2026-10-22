import AppKit
import SwiftUI

/// 设计稿 M1（原生控制中心风格）的颜色和小部件。
enum Theme {
    static let accent = Color(hex: 0x0A84FF)
    static let orange = Color(hex: 0xFF9500)
    static let green = Color(hex: 0x34C759)
    static let yellow = Color(hex: 0xFFCC00)
    static let red = Color(hex: 0xFF3B30)
    static let gray = Color(hex: 0x8E8E93)
    static let ink = Color(hex: 0x1D1D1F)
    static let ink2 = Color(hex: 0x3A3A3C)
    static let muted = Color(hex: 0x6E6E73)
    static let panel = Color(hex: 0xF0F0F3, alpha: 0.86)
    static let tile = Color.white.opacity(0.62)
    static let tileHover = Color.white.opacity(0.82)
    static let separator = Color.black.opacity(0.14)
    static let danger = Color(hex: 0xD70015)
}

extension Color {
    init(hex: UInt32, alpha: Double = 1) {
        self.init(.sRGB,
                  red: Double((hex >> 16) & 0xFF) / 255,
                  green: Double((hex >> 8) & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255,
                  opacity: alpha)
    }
}

/// 半透明白色圆角块（控制中心的模块）
struct Tile<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Theme.tile))
    }
}

/// 彩色圆形图标。symbol 为 `CircleIcon.aurio` 时画应用标志（五根竖条）。
struct CircleIcon: View {
    static let aurio = "aurio.bars"
    let symbol: String
    let color: Color
    var size: CGFloat = 30

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .overlay {
                if symbol == CircleIcon.aurio {
                    AurioBars()
                        .stroke(.white, style: StrokeStyle(lineWidth: size * 0.055, lineCap: .round))
                        .frame(width: size * 0.56, height: size * 0.56)
                } else {
                    Image(systemName: symbol)
                        .font(.system(size: size * 0.48, weight: .semibold))
                        .foregroundStyle(.white)
                }
            }
    }
}

/// 应用标志的五根竖条（viewBox 24 等比缩放）
struct AurioBars: Shape {
    func path(in r: CGRect) -> Path {
        let k = min(r.width, r.height) / 24
        var p = Path()
        for (x, y0, y1) in MenuBarIcon.bars {
            p.move(to: CGPoint(x: r.minX + x * k, y: r.minY + y0 * k))
            p.addLine(to: CGPoint(x: r.minX + x * k, y: r.minY + y1 * k))
        }
        return p
    }
}

/// 点击整块切换的方块按钮（麦克风 / 回声消除）
struct ToggleTile: View {
    let symbol: String
    let color: Color
    let title: String
    let detail: String
    let action: () -> Void
    @State private var hover = false

    var body: some View {
        Button(action: action) {
            VStack(alignment: .leading, spacing: 10) {
                CircleIcon(symbol: symbol, color: color)
                VStack(alignment: .leading, spacing: 0) {
                    Text(title).font(.system(size: 13, weight: .semibold)).foregroundStyle(Theme.ink)
                    Text(detail).font(.system(size: 11.5)).foregroundStyle(Theme.muted).lineLimit(1)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(hover ? Theme.tileHover : Theme.tile))
            .contentShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
        .buttonStyle(PressStyle())
        .onHover { hover = $0 }
        .accessibilityLabel(title)
        .accessibilityValue(detail)
    }
}

/// 按下时轻微缩小
struct PressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

/// 32×18 的开关
struct NativeSwitch: View {
    @Binding var isOn: Bool

    var body: some View {
        ZStack(alignment: isOn ? .trailing : .leading) {
            Capsule().fill(isOn ? Theme.accent : Color.black.opacity(0.12))
            Circle()
                .fill(.white)
                .shadow(color: .black.opacity(0.25), radius: 1, y: 1)
                .frame(width: 14, height: 14)
                .padding(2)
        }
        .frame(width: 32, height: 18)
        .animation(.easeOut(duration: 0.15), value: isOn)
    }
}

/// 分段选择（底色浅灰，选中项为白色浮起）
struct SegmentedChoice<T: Hashable>: View {
    let options: [(T, String)]
    @Binding var selection: T
    @Namespace private var ns

    var body: some View {
        HStack(spacing: 2) {
            ForEach(options, id: \.0) { value, label in
                let on = value == selection
                Text(label)
                    .font(.system(size: 12))
                    .foregroundStyle(on ? Theme.ink : Theme.ink2)
                    .frame(maxWidth: .infinity, minHeight: 24)
                    .background {
                        if on {
                            RoundedRectangle(cornerRadius: 6, style: .continuous)
                                .fill(.white)
                                .shadow(color: .black.opacity(0.15), radius: 1, y: 1)
                                .matchedGeometryEffect(id: "sel", in: ns)
                        }
                    }
                    .contentShape(Rectangle())
                    .onTapGesture { withAnimation(.easeOut(duration: 0.18)) { selection = value } }
                    .accessibilityAddTraits(on ? [.isButton, .isSelected] : .isButton)
            }
        }
        .padding(2)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Color.black.opacity(0.07)))
    }
}

/// 控制中心样式的音量条：26px 高的胶囊，白色部分是音量，左边嵌着扬声器图标；在条上任意位置拖动。
struct VolumeBar: View {
    @Binding var value: Double   // 0…1

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width
            let h = geo.size.height
            let fill = h + CGFloat(value) * (w - h)
            ZStack(alignment: .leading) {
                Capsule().fill(Color.black.opacity(0.09))
                Capsule()
                    .fill(.white)
                    .shadow(color: .black.opacity(0.18), radius: 1.5, y: 1)
                    .frame(width: fill)
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.muted)
                    .frame(width: h, height: h)
            }
            .contentShape(Capsule())
            .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                value = min(1, max(0, Double((g.location.x - h / 2) / (w - h))))
            })
        }
        .frame(height: 26)
        .accessibilityElement()
        .accessibilityLabel("音量")
        .accessibilityValue("\(Int(value * 100))%")
        .accessibilityAdjustableAction { dir in
            switch dir {
            case .increment: value = min(1, value + 0.05)
            case .decrement: value = max(0, value - 0.05)
            @unknown default: break
            }
        }
    }

    private var icon: String {
        switch value {
        case 0: return "speaker.slash.fill"
        case ..<0.34: return "speaker.wave.1.fill"
        case ..<0.67: return "speaker.wave.2.fill"
        default: return "speaker.wave.3.fill"
        }
    }
}

/// 「电平」细条。-60…0 dBFS 线性映射到宽度。
struct LevelBar: View {
    let db: Float
    var active = true

    var body: some View {
        let f = active ? CGFloat(max(0, min(1, (db + 60) / 60))) : 0
        HStack(spacing: 8) {
            Text("电平").font(.system(size: 11)).foregroundStyle(Theme.muted).frame(width: 28, alignment: .leading)
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.black.opacity(0.08))
                    Capsule().fill(db > -3 ? Theme.orange : Theme.green).frame(width: geo.size.width * f)
                }
            }
            .frame(height: 4)
            .animation(.linear(duration: 0.05), value: f)
        }
        .accessibilityElement()
        .accessibilityLabel("电平")
        .accessibilityValue(active && db > -60 ? "\(Int(db.rounded())) dB" : "无信号")
    }
}

/// 「Studio Display >」：点击弹出原生菜单选设备。
/// （SwiftUI 的 Menu 在 macOS 上会把自定义标签压平成系统样式，所以自己弹 NSMenu。）
struct DeviceChooser: View {
    let current: String
    let selectedUID: String?
    let devices: [(uid: String, name: String)]
    let onSelect: (String?) -> Void
    @State private var anchor = MenuAnchor()
    @State private var hover = false

    var body: some View {
        Button(action: popUp) {
            HStack(spacing: 6) {
                Text(current).font(.system(size: 12)).foregroundStyle(hover ? Theme.ink2 : Theme.muted).lineLimit(1)
                Image(systemName: "chevron.right").font(.system(size: 9.5, weight: .semibold)).foregroundStyle(Theme.gray)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
        .background(AnchorView(anchor: anchor))
        .accessibilityLabel("选择设备，当前 \(current)")
    }

    private func popUp() {
        guard let view = anchor.view else { return }
        let menu = NSMenu()
        func add(_ title: String, _ uid: String?) {
            let target = MenuAction { onSelect(uid) }
            let item = NSMenuItem(title: title, action: #selector(MenuAction.fire), keyEquivalent: "")
            item.target = target
            item.representedObject = target   // 让菜单项持有它
            item.state = selectedUID == uid ? .on : .off
            menu.addItem(item)
        }
        add("自动（优先 Studio Display）", nil)
        menu.addItem(.separator())
        for d in devices { add(d.name, d.uid) }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: view.isFlipped ? view.bounds.height + 4 : -4), in: view)
    }
}

final class MenuAnchor { weak var view: NSView? }

private struct AnchorView: NSViewRepresentable {
    let anchor: MenuAnchor
    func makeNSView(context: Context) -> NSView {
        let v = NSView()
        anchor.view = v
        return v
    }
    func updateNSView(_ nsView: NSView, context: Context) { anchor.view = nsView }
}

private final class MenuAction: NSObject {
    let run: () -> Void
    init(_ run: @escaping () -> Void) { self.run = run }
    @objc func fire() { run() }
}
