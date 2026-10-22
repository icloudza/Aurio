import AurioKit
import ServiceManagement
import SwiftUI

@main
struct AurioApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        MenuBarExtra {
            MenuView()
                .environmentObject(model)
                .environmentObject(model.engine)
        } label: {
            MenuBarLabel(model: model, updates: model.updates)
        }
        .menuBarExtraStyle(.window)
    }
}

/// 菜单栏图标：连接状态 + 有新版本时的小圆点
private struct MenuBarLabel: View {
    @ObservedObject var model: AppModel
    @ObservedObject var updates: UpdateController

    var body: some View {
        Image(nsImage: MenuBarIcon.image(active: model.linked, badge: updates.availableVersion != nil))
    }
}

/// 保存设置、管理开机自启，持有引擎和更新控制器。
final class AppModel: ObservableObject {
    let engine: AurioEngine
    /// 在线更新（截图和测试模式下不启动，不联网）
    let updates: UpdateController
    private let defaults = UserDefaults.standard
    private var sub: Any?
    /// 测试用：环境变量 AURIO_MUTE=1 时音量固定为 0 且不保存
    private let forceMute: Bool

    /// 和 Windows 连着（正在播放或收到心跳）：菜单栏图标实心，否则变淡
    @Published var linked = false
    @Published var launchAtLogin: Bool

    init() {
        let env = ProcessInfo.processInfo.environment
        // 开发用：AURIO_SNAPSHOT=目录 时把面板渲染成 PNG 后退出（假数据，不开音频、不监听网络）
        if let dir = env["AURIO_SNAPSHOT"] {
            engine = AurioEngine(port: 0, outputUID: nil, inputUID: nil, micMode: .auto, volume: 0.72,
                                 echoCancel: env["AURIO_SNAPSHOT_AEC"] == "1")
            launchAtLogin = false
            forceMute = true
            updates = UpdateController(start: false)
            DispatchQueue.main.async { [self] in self.renderSnapshots(to: dir) }
            return
        }
        // 开发用：AURIO_AEC_TEST=结果文件路径 时只跑回声消除对比测试，写完结果就退出（不监听网络）
        if let out = env["AURIO_AEC_TEST"] {
            engine = AurioEngine(port: 0, outputUID: nil, inputUID: nil, micMode: .off, volume: 0)
            launchAtLogin = false
            forceMute = true
            updates = UpdateController(start: false)
            AppModel.runAecTest(outPath: out)
            return
        }
        let port = env["AURIO_PORT"].flatMap { UInt16($0) }
            ?? UInt16(exactly: defaults.integer(forKey: "port")).flatMap { $0 == 0 ? nil : $0 }
            ?? 47800
        forceMute = env["AURIO_MUTE"] == "1"
        let volume: Float = forceMute ? 0 : (defaults.object(forKey: "volume") as? Float ?? 1.0)
        engine = AurioEngine(
            port: port,
            outputUID: defaults.string(forKey: "outputUID"),
            inputUID: defaults.string(forKey: "inputUID"),
            micMode: MicMode(rawValue: defaults.string(forKey: "micMode") ?? "") ?? .auto,
            volume: volume,
            echoCancel: defaults.object(forKey: "echoCancel") as? Bool ?? false)
        engine.micTestTone = env["AURIO_MIC_TONE"] == "1"
        launchAtLogin = SMAppService.mainApp.status == .enabled
        updates = UpdateController(start: true)
        sub = engine.$state.sink { [weak self] st in
            switch st {
            case .playing, .connected: self?.linked = true
            default: self?.linked = false
            }
        }
        engine.start()
    }

    func setOutput(_ uid: String?) {
        defaults.set(uid, forKey: "outputUID")
        engine.setOutput(uid: uid)
    }

    func setInput(_ uid: String?) {
        defaults.set(uid, forKey: "inputUID")
        engine.setInput(uid: uid)
    }

    func setMicMode(_ m: MicMode) {
        defaults.set(m.rawValue, forKey: "micMode")
        engine.micMode = m
        objectWillChange.send()
    }

    func setEchoCancel(_ on: Bool) {
        defaults.set(on, forKey: "echoCancel")
        engine.echoCancel = on
        objectWillChange.send()
    }

    /// 拖动滑块时每帧都会调用：只改引擎音量（一次原子写），不触发界面重绘；停手 0.3 秒后再保存。
    func setVolume(_ v: Float) {
        engine.volume = forceMute ? 0 : v
        guard !forceMute else { return }
        saveVolume?.cancel()
        let item = DispatchWorkItem { [defaults] in defaults.set(v, forKey: "volume") }
        saveVolume = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: item)
    }
    private var saveVolume: DispatchWorkItem?

    func setLaunchAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
        } catch {
            NSLog("Aurio: 开机自启设置失败：\(error)")
        }
        launchAtLogin = SMAppService.mainApp.status == .enabled
    }

    private static func runAecTest(outPath: String) {
        DispatchQueue.global().async {
            var text: String
            if let out = pickOutputDevice(uid: nil, nameContains: nil), let inp = pickInputDevice(uid: nil, nameContains: nil) {
                do {
                    let r = try runEchoCancelTest(output: out, input: inp)
                    text = "扬声器：\(out.name)\n麦克风：\(inp.name)\n" + r.text
                } catch {
                    text = "测试失败：\(error)"
                }
            } else {
                text = "找不到扬声器或麦克风"
            }
            try? text.write(toFile: outPath, atomically: true, encoding: .utf8)
            DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
        }
    }

    /// 用离屏窗口里的 NSHostingView 渲染，原生控件（下拉菜单等）也能画出来。
    @MainActor private func renderSnapshots(to dir: String) {
        func save<V: View>(_ view: V, _ name: String) {
            let host = NSHostingView(rootView: view.environmentObject(self).environmentObject(engine))
            host.frame.size = host.fittingSize
            let win = NSWindow(contentRect: host.frame, styleMask: .borderless, backing: .buffered, defer: false)
            win.contentView = host
            win.appearance = NSAppearance(named: .aqua)
            host.layoutSubtreeIfNeeded()
            guard let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) else { return }
            host.cacheDisplay(in: host.bounds, to: rep)
            try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name))
        }
        for (active, badge) in [(true, false), (false, false), (true, true)] {
            // 菜单栏图标：模板图按黑色画，这里 4 倍放大存档
            let img = MenuBarIcon.image(active: active, badge: badge)
            let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 72, pixelsHigh: 72, bitsPerSample: 8,
                                       samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                                       bytesPerRow: 0, bitsPerPixel: 0)!
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
            img.draw(in: NSRect(x: 0, y: 0, width: 72, height: 72))
            NSGraphicsContext.restoreGraphicsState()
            try? rep.representation(using: .png, properties: [:])?
                .write(to: URL(fileURLWithPath: dir).appendingPathComponent(badge ? "menubar-update.png" : active ? "menubar-on.png" : "menubar-off.png"))
        }
        engine.applyPreviewState("playing")
        updates.showPreview(version: "0.2.7")
        save(MenuView(), "menu-playing.png")
        updates.showPreview(version: nil)
        engine.applyPreviewState("waiting")
        save(MenuView(), "menu-waiting.png")
        NSApplication.shared.terminate(nil)
    }

    func quit() {
        engine.stop()
        NSApplication.shared.terminate(nil)
    }
}

/// 菜单栏图标：和应用图标同一个「五根竖条」图形，单色模板图（系统按菜单栏颜色自动反色）。
enum MenuBarIcon {
    /// viewBox 24 里的竖条 (x, 上 y, 下 y)，和 scripts/make-icons.py 的 BARS5 一致
    static let bars: [(CGFloat, CGFloat, CGFloat)] = [(4, 10, 14), (8, 7, 17), (12, 4, 20), (16, 7, 17), (20, 10, 14)]
    private static var cache: [String: NSImage] = [:]

    /// active：已连接（实心）；badge：有新版本（右上角小圆点）
    static func image(active: Bool, badge: Bool = false) -> NSImage {
        let key = "\(active)-\(badge)"
        if let img = cache[key] { return img }
        let size: CGFloat = 18
        let img = NSImage(size: NSSize(width: size, height: size), flipped: true) { _ in
            let k = size / 24
            let path = NSBezierPath()
            for (x, y0, y1) in bars {
                path.move(to: NSPoint(x: x * k, y: y0 * k))
                path.line(to: NSPoint(x: x * k, y: y1 * k))
            }
            path.lineWidth = 2 * k
            path.lineCapStyle = .round
            NSColor.black.withAlphaComponent(active ? 1 : 0.45).setStroke()
            path.stroke()
            if badge {
                NSColor.black.setFill()
                NSBezierPath(ovalIn: NSRect(x: 13.6, y: 0.6, width: 4.4, height: 4.4)).fill()
            }
            return true
        }
        img.isTemplate = true
        img.accessibilityDescription = (active ? "Aurio（已连接）" : "Aurio（未连接）") + (badge ? "，有新版本" : "")
        cache[key] = img
        return img
    }
}
