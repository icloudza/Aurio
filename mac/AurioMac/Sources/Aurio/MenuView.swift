import AurioKit
import SwiftUI

/// 菜单栏面板（设计稿 M1 · 原生控制中心风格）
struct MenuView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var engine: AurioEngine
    @State private var panel = PanelWindow()

    var body: some View {
        VStack(spacing: 10) {
            statusTile
            HStack(spacing: 10) {
                micTile
                echoTile
            }
            SpeakerTile(model: model, engine: engine, levels: engine.levels)
            micSection
            footer
        }
        .padding(10)
        .frame(width: 340)
        .fixedSize(horizontal: false, vertical: true)
        // 量出内容高度，让窗口跟着变（菜单栏面板的窗口自己不会缩小，内容会悬在中间）
        .background(GeometryReader { geo in
            Color.clear
                .onAppear { panel.fit(height: geo.size.height) }
                .onChange(of: geo.size.height) { panel.fit(height: $0) }
        })
        .background(WindowReader { panel.attach($0, engine: engine) })
        .background(Theme.panel.ignoresSafeArea())
        .foregroundStyle(Theme.ink)
        .environment(\.colorScheme, .light)
    }

    // MARK: 状态

    private var statusTile: some View {
        Tile {
            HStack(spacing: 12) {
                CircleIcon(symbol: status.symbol, color: status.color, size: 34)
                VStack(alignment: .leading, spacing: 1) {
                    Text(status.title).font(.system(size: 13, weight: .semibold))
                    Text(status.detail)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.muted)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                        .help(status.detail)
                }
                Spacer(minLength: 0)
                Circle().fill(status.dot).frame(width: 8, height: 8)
            }
        }
    }

    private var status: (symbol: String, color: Color, title: String, detail: String, dot: Color) {
        switch engine.state {
        case let .playing(peer):
            let from = peer.isEmpty ? "已连接" : "来自 \(peer)"
            return (CircleIcon.aurio, Theme.accent, "正在播放 Windows 的声音",
                    String(format: "%@ · 延迟约 %.0f ms", from, engine.metrics.latencyMs), Theme.green)
        case let .connected(peer):
            return (CircleIcon.aurio, Theme.accent, "已连接 Windows",
                    peer.isEmpty ? "没有声音" : "来自 \(peer) · 没有声音", Theme.green)
        case .waiting:
            return ("dot.radiowaves.left.and.right", Theme.gray, "等待 Windows 连接",
                    "在 Windows 上打开 Aurio 即可自动连接", Theme.yellow)
        case let .disconnected(peer):
            return (CircleIcon.aurio, Theme.gray, "连接已断开",
                    peer.isEmpty ? "Windows 上的 Aurio 停止了" : "\(peer) · Windows 上的 Aurio 停止了", Theme.orange)
        case let .error(msg):
            return ("exclamationmark", Theme.red, "出错了", msg, Theme.red)
        case .stopped:
            return (CircleIcon.aurio, Theme.gray, "已停止", "", Theme.gray)
        }
    }

    // MARK: 两个方块

    private var micTile: some View {
        let on = engine.micMode != .off
        let detail: String
        switch engine.micMode {
        case .off: detail = "关闭"
        case .auto: detail = engine.micActive ? "自动 · 正在使用" : "自动 · 待命"
        case .on: detail = engine.micActive ? "常开 · 正在使用" : "常开 · 正在打开"
        }
        return ToggleTile(symbol: on ? "mic.fill" : "mic.slash.fill", color: on ? Theme.orange : Theme.gray,
                          title: "麦克风", detail: detail) {
            model.setMicMode(engine.micMode == .off ? .auto : .off)
        }
    }

    private var echoTile: some View {
        let on = engine.echoCancel
        let detail = !on ? "关" : (engine.echoCancelActive ? "开 · 正在消除" : "开 · 通话时生效")
        return ToggleTile(symbol: "waveform.path", color: on ? Theme.accent : Theme.gray,
                          title: "回声消除", detail: detail) {
            model.setEchoCancel(!engine.echoCancel)
        }
        .help("打开后，Windows 上录音时改用苹果的语音处理：队友听不到扬声器里的游戏声，但这期间播放会变成通话音质")
    }

    // MARK: 麦克风

    private var micSection: some View {
        Tile {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 6) {
                    Text("麦克风").font(.system(size: 13, weight: .semibold))
                    Spacer(minLength: 8)
                    DeviceChooser(current: engine.currentInput?.name ?? "未选择",
                                  selectedUID: engine.inputUID,
                                  devices: engine.inputDevices.map { ($0.uid, $0.name) },
                                  onSelect: model.setInput)
                }
                SegmentedChoice(options: MicMode.allCases.map { ($0, $0.label) },
                                selection: Binding(get: { engine.micMode }, set: { model.setMicMode($0) }))
                // 一直占位：麦克风关着时是空条，面板高度不跟着跳
                MicLevel(engine: engine, levels: engine.levels)
                Text(micHint)
                    .font(.system(size: 11.5))
                    .foregroundStyle(engine.micError != nil ? Theme.danger : Theme.muted)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .help(micHint)
            }
        }
    }

    private var micHint: String {
        if let err = engine.micError { return err }
        switch engine.micMode {
        case .auto: return engine.micActive ? "Windows 上有应用正在使用麦克风" : "Windows 上有应用用麦克风时自动打开"
        case .on: return "一直把麦克风发给 Windows"
        case .off: return "不会打开麦克风"
        }
    }

    // MARK: 底部

    private var footer: some View {
        let m = engine.metrics
        let live: Bool = {
            switch engine.state {
            case .playing, .connected: return true
            default: return false
            }
        }()
        return VStack(spacing: 0) {
            HStack(spacing: 0) {
                stat(live ? String(format: "%.0f ms", m.latencyMs) : "—", "延迟")
                stat(live ? String(format: "%.1f ms", m.fillMs) : "—", "缓冲")
                stat(live ? String(format: "%.0f", m.ppm).replacingOccurrences(of: "-", with: "−") : "—", "ppm")
                stat(live ? "\(m.underruns)" : "—", "欠载")
            }
            .padding(EdgeInsets(top: 4, leading: 6, bottom: 10, trailing: 6))
            .help(live ? String(format: "目标缓冲 %.1f ms · %llu 包/秒 · 最大间隔 %.1f ms",
                                m.targetMs, m.packetsPerSecond, m.maxGapMs) : "")
            Rectangle().fill(Theme.separator).frame(height: 0.5)
            ListRow(action: { model.setLaunchAtLogin(!model.launchAtLogin) }) {
                Text("登录时打开").font(.system(size: 13))
                Spacer()
                NativeSwitch(isOn: Binding(get: { model.launchAtLogin }, set: { model.setLaunchAtLogin($0) }))
            }
            Rectangle().fill(Theme.separator).frame(height: 0.5)
            ListRow(action: { model.quit() }) {
                Text("退出 Aurio").font(.system(size: 13))
                Spacer()
                Text("⌘Q").font(.system(size: 12)).foregroundStyle(Theme.gray)
            }
            .keyboardShortcut("q")
        }
        .padding(.horizontal, 4)
        .padding(.vertical, 2)
    }

    private func stat(_ value: String, _ label: String) -> some View {
        VStack(spacing: 0) {
            Text(value).font(.system(size: 13, weight: .semibold)).monospacedDigit()
            Text(label).font(.system(size: 11)).foregroundStyle(Theme.muted)
        }
        .frame(maxWidth: .infinity)
    }
}

/// 30px 高、悬停时带浅色底的列表行
private struct ListRow<Content: View>: View {
    let action: () -> Void
    @ViewBuilder var content: Content
    @State private var hover = false

    var body: some View {
        Button(action: action) {
            HStack { content }
                .padding(.horizontal, 6)
                .frame(height: 30)
                .background(RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hover ? Color.black.opacity(0.05) : .clear))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
    }
}

// MARK: - 扬声器

/// 单独成一个视图：电平每秒变 30 次、拖音量时每帧都变，只重绘这一块。
private struct SpeakerTile: View {
    let model: AppModel
    @ObservedObject var engine: AurioEngine
    @ObservedObject var levels: LevelMeter
    /// 滑块自己的状态：拖动时不牵动整个面板
    @State private var volume: Double

    init(model: AppModel, engine: AurioEngine, levels: LevelMeter) {
        self.model = model
        self.engine = engine
        self.levels = levels
        _volume = State(initialValue: Double(engine.volume))
    }

    var body: some View {
        Tile {
            VStack(spacing: 10) {
                HStack(spacing: 6) {
                    Text("扬声器").font(.system(size: 13, weight: .semibold))
                    Spacer(minLength: 8)
                    DeviceChooser(current: engine.currentOutput?.name ?? "未选择",
                                  selectedUID: engine.outputUID,
                                  devices: engine.outputDevices.map { ($0.uid, $0.name) },
                                  onSelect: model.setOutput)
                }
                VolumeBar(value: Binding(get: { volume }, set: { volume = $0; model.setVolume(Float($0)) }))
                LevelBar(db: levels.outputDb, active: engine.currentOutput != nil)
            }
        }
    }
}

private struct MicLevel: View {
    @ObservedObject var engine: AurioEngine
    @ObservedObject var levels: LevelMeter

    var body: some View {
        LevelBar(db: levels.micDb, active: engine.micActive)
    }
}

// MARK: - 面板窗口

/// 拿到面板所在的 NSWindow：
/// - 内容高度变了就把窗口改成同样高（顶边不动），免得内容悬在中间、露出背景的直角；
/// - 按窗口是否真的显示在屏幕上开关电平刷新（SwiftUI 的 onAppear 在菜单栏面板里不可靠）。
final class PanelWindow {
    private weak var window: NSWindow?
    private var observer: NSObjectProtocol?
    private var pendingHeight: CGFloat?

    func attach(_ w: NSWindow?, engine: AurioEngine) {
        guard let w, w !== window else { return }
        if let observer { NotificationCenter.default.removeObserver(observer) }
        window = w
        let update = { [weak w, weak engine] in
            guard let w, let engine else { return }
            engine.setMetering(w.occlusionState.contains(.visible))
        }
        observer = NotificationCenter.default.addObserver(
            forName: NSWindow.didChangeOcclusionStateNotification, object: w, queue: .main) { _ in update() }
        update()
        if let h = pendingHeight { fit(height: h) }
    }

    func fit(height: CGFloat) {
        guard let w = window else { pendingHeight = height; return }
        pendingHeight = nil
        let content = w.contentRect(forFrameRect: w.frame)
        guard height > 0, abs(content.height - height) > 0.5 else { return }
        let target = NSRect(x: content.minX, y: content.maxY - height, width: content.width, height: height)
        w.setFrame(w.frameRect(forContentRect: target), display: true)
    }

    deinit {
        if let observer { NotificationCenter.default.removeObserver(observer) }
    }
}

/// 把所在的 NSWindow 交给回调（加进窗口、换窗口时都会调用）
private struct WindowReader: NSViewRepresentable {
    let onWindow: (NSWindow?) -> Void

    func makeNSView(context: Context) -> NSView {
        let v = TrackingView()
        v.onWindow = onWindow
        return v
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        (nsView as? TrackingView)?.onWindow = onWindow
    }

    private final class TrackingView: NSView {
        var onWindow: ((NSWindow?) -> Void)?
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            let w = window
            DispatchQueue.main.async { [weak self] in self?.onWindow?(w) }
        }
    }
}
