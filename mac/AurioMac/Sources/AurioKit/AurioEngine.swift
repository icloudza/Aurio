import AVFoundation
import CAurio
import Combine
import CoreAudio
import Foundation

/// 麦克风上行模式：自动 = Windows 有应用在用麦克风时才开。
public enum MicMode: String, CaseIterable, Identifiable {
    case auto, on, off
    public var id: String { rawValue }
    public var label: String {
        switch self {
        case .auto: return "自动"
        case .on: return "常开"
        case .off: return "关闭"
        }
    }
}

public enum ConnectionState: Equatable {
    case stopped
    case waiting
    /// 正在收到 Windows 的声音
    case playing(peer: String)
    /// 心跳还在（Windows 端开着），但没有音频
    case connected(peer: String)
    /// 连过，但心跳和音频都停了
    case disconnected(peer: String)
    case error(String)
}

public struct EngineMetrics: Equatable {
    public var latencyMs: Double = 0
    public var fillMs: Double = 0
    public var targetMs: Double = 0
    public var ppm: Double = 0
    public var underruns: UInt64 = 0
    public var packetsPerSecond: UInt64 = 0
    public var burstMs: Double = 0
    public var maxGapMs: Double = 0
    public init() {}
}

/// 接收端引擎：收 Windows 的音频从指定设备播放，按 Windows 的心跳开关麦克风上行，
/// 并用 Bonjour 广播 `_aurio._udp`，让 Windows 端自动发现。
/// 所有方法都在主线程调用。
public final class AurioEngine: NSObject, ObservableObject {
    public static let serviceType = "_aurio._udp."

    @Published public private(set) var state: ConnectionState = .stopped
    @Published public private(set) var metrics = EngineMetrics()
    @Published public private(set) var micActive = false
    @Published public private(set) var micRequested = false
    @Published public private(set) var micError: String?
    /// 正在用语音处理单元（回声消除）。同时通过心跳回声告诉 Windows。
    @Published public private(set) var echoCancelActive = false {
        didSet { if let r = rx { aurio_rx_set_status_flags(r, echoCancelActive ? 1 : 0) } }
    }
    @Published public private(set) var outputDevices: [AudioDevice] = []
    @Published public private(set) var inputDevices: [AudioDevice] = []
    @Published public private(set) var currentOutput: AudioDevice?
    @Published public private(set) var currentInput: AudioDevice?

    /// 实时电平（每秒刷新 30 次）。单独一个对象，免得整个面板跟着高频重绘。
    public let levels = LevelMeter()

    public let port: UInt16
    /// 选中的输出设备 UID；nil 表示自动（优先 Studio Display）
    public private(set) var outputUID: String?
    public private(set) var inputUID: String?
    public var micMode: MicMode
    /// 麦克风打开时用苹果的语音处理（回声消除、降噪）。改了以后，下次打开麦克风生效。
    public var echoCancel: Bool {
        didSet { if echoCancel != oldValue, mic != nil { stopMic() } }
    }
    /// 不开麦克风，改发 440 Hz 测试音（测链路用）
    public var micTestTone = false
    public var volume: Float {
        didSet { if let rx { aurio_rx_set_volume(rx, volume) } }
    }

    private var rx: OpaquePointer?
    private var output: HALOutput?
    private var voice: VoiceIO?
    private var rxRate: Double = 48000
    private var rxChannels = 2
    private var latency: OutputLatency?
    private var mic: OpaquePointer?
    private var micInput: HALInput?
    private var micRetryAfter = Date.distantPast
    private var timer: Timer?
    private var service: NetService?
    private var lastPackets: UInt64 = 0
    private var meterTimer: Timer?
    private var wantRunning = false

    public init(port: UInt16 = 47800, outputUID: String?, inputUID: String?, micMode: MicMode, volume: Float,
                echoCancel: Bool = false) {
        self.port = port
        self.outputUID = outputUID
        self.inputUID = inputUID
        self.micMode = micMode
        self.echoCancel = echoCancel
        self.volume = volume
        super.init()
        refreshDevices()
    }

    // MARK: - 公共接口

    public func start() {
        wantRunning = true
        startAudio()
        publishService()
        if timer == nil {
            let t = Timer(timeInterval: 1, repeats: true) { [weak self] _ in self?.tick() }
            RunLoop.main.add(t, forMode: .common)
            timer = t
        }
    }

    public func stop() {
        wantRunning = false
        timer?.invalidate()
        timer = nil
        service?.stop()
        service = nil
        stopAudio()
        state = .stopped
    }

    public func setOutput(uid: String?) {
        outputUID = uid
        if wantRunning {
            stopAudio()
            startAudio()
        }
    }

    public func setInput(uid: String?) {
        inputUID = uid
        refreshDevices()
        stopMic()   // 下一次 tick 按新设备重新打开
    }

    // MARK: - 设备

    private func refreshDevices() {
        let all = allDevices()
        outputDevices = all.filter { $0.outputChannels > 0 }
        inputDevices = all.filter { $0.inputChannels > 0 }
        currentInput = pickInputDevice(uid: inputUID, nameContains: nil)
            ?? (inputUID != nil ? nil : pickInputDevice(uid: nil, nameContains: nil))
    }

    private func resolveOutput() -> AudioDevice? {
        if let uid = outputUID { return outputDevices.first { $0.uid == uid } }
        return pickOutputDevice(uid: nil, nameContains: nil)
    }

    // MARK: - 音频

    private func startAudio() {
        guard rx == nil else { return }
        refreshDevices()
        guard let dev = resolveOutput() else {
            state = .error(outputUID != nil ? "选中的输出设备不在了，插回后会自动恢复" : "找不到输出设备")
            return
        }
        let ch = max(1, min(dev.outputChannels, 2))
        _ = setBufferFrames(dev, 128)
        var cfg = AurioRxConfig()
        aurio_rx_default_config(&cfg)
        cfg.port = port
        cfg.out_rate = dev.nominalRate
        cfg.out_channels = UInt32(ch)
        cfg.volume = volume
        guard let r = aurio_rx_create(&cfg) else {
            let msg = String(cString: aurio_last_error())
            state = .error(msg.contains("Address already in use")
                ? "端口 \(port) 被占用（是不是还开着命令行版 aurio-rx？）" : msg)
            return
        }
        do {
            let out = try HALOutput(device: dev, rx: r, channels: ch)
            try out.start()
            rx = r
            output = out
            rxRate = dev.nominalRate
            rxChannels = ch
            latency = outputLatency(dev)
            currentOutput = dev
            lastPackets = 0
            state = .waiting
        } catch {
            aurio_rx_destroy(r)
            state = .error("打开输出设备失败：\(error)")
        }
    }

    private func stopAudio() {
        stopMic()
        output?.stop()
        output = nil
        if let r = rx { aurio_rx_destroy(r) }
        rx = nil
    }

    // MARK: - 麦克风

    private func startMic() {
        guard mic == nil, let r = rx, Date() >= micRetryAfter else { return }
        if micTestTone {
            mic = aurio_mic_create(r, 48000, 1, 1)
            micActive = mic != nil
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .notDetermined:
            micRetryAfter = Date().addingTimeInterval(1)
            AVCaptureDevice.requestAccess(for: .audio) { _ in }
            return
        case .denied, .restricted:
            micError = "没有麦克风权限：系统设置 → 隐私与安全性 → 麦克风 → 打开 Aurio"
            micRetryAfter = Date().addingTimeInterval(5)
            return
        default:
            break
        }
        guard let m = currentInput else {
            micError = "找不到麦克风设备"
            return
        }
        if echoCancel, startVoice(input: m) { return }
        let ch = max(1, min(m.inputChannels, 2))
        _ = setBufferFrames(m, 128)
        guard let up = aurio_mic_create(r, m.nominalRate, UInt32(ch), 0) else {
            micError = String(cString: aurio_last_error())
            micRetryAfter = Date().addingTimeInterval(5)
            return
        }
        do {
            let input = try HALInput(device: m, mic: up, channels: ch)
            try input.start()
            mic = up
            micInput = input
            micActive = true
            micError = nil
        } catch {
            aurio_mic_destroy(up)
            micError = "打开麦克风失败：\(error)"
            micRetryAfter = Date().addingTimeInterval(5)
        }
    }

    /// 切到语音处理单元：它同时负责播放（Windows 的声音，也是回声参照）和采集（消除回声后的麦克风）。
    /// 失败时恢复普通输出并返回 false，由调用方改用普通麦克风。
    private func startVoice(input m: AudioDevice) -> Bool {
        guard let r = rx, let outDev = currentOutput else { return false }
        guard let up = aurio_mic_create(r, rxRate, 1, 0) else { return false }
        output?.stop()   // 同一时间只能有一个单元往 rx 要数据
        output = nil
        do {
            let v = try VoiceIO(output: outDev, input: m, rate: rxRate, outChannels: rxChannels, rx: r, mic: up)
            try v.start()
            voice = v
            mic = up
            micActive = true
            echoCancelActive = true
            micError = nil
            return true
        } catch {
            aurio_mic_destroy(up)
            restoreOutput()
            micError = "回声消除没能启动（\(error)），先用普通麦克风"
            return false
        }
    }

    /// 回到普通输出（原样播放，音乐音质）
    private func restoreOutput() {
        guard output == nil, voice == nil, let r = rx, let dev = currentOutput else { return }
        do {
            let out = try HALOutput(device: dev, rx: r, channels: rxChannels)
            try out.start()
            output = out
        } catch {
            state = .error("恢复输出失败：\(error)")
        }
    }

    private func stopMic() {
        guard let m = mic else { return }
        if let v = voice {
            v.stop()       // 先停回调，再销毁上行对象
            voice = nil
            echoCancelActive = false
            aurio_mic_destroy(m)
            mic = nil
            micActive = false
            restoreOutput()
            return
        }
        micInput?.stop()   // 先停回调，再销毁上行对象
        micInput = nil
        aurio_mic_destroy(m)
        mic = nil
        micActive = false
    }

    // MARK: - 每秒一次

    private func tick() {
        refreshDevices()

        // 输出设备拔掉了：停下来，等它回来
        if rx != nil, let cur = currentOutput, !outputDevices.contains(where: { $0.uid == cur.uid }) {
            stopAudio()
            currentOutput = nil
            state = .error("输出设备「\(cur.name)」不见了，插回后会自动恢复")
            return
        }
        if rx == nil {
            if wantRunning { startAudio() }
            if rx == nil { return }
        }
        guard let r = rx else { return }

        var s = AurioRxStats()
        aurio_rx_get_stats(r, &s)
        var m = EngineMetrics()
        m.fillMs = Double(s.fill_ms)
        m.targetMs = Double(s.target_ms)
        m.ppm = Double(s.ppm)
        m.underruns = s.underruns
        m.packetsPerSecond = s.packets &- lastPackets
        m.burstMs = Double(s.burst_ms)
        m.maxGapMs = Double(s.max_gap_ms)
        lastPackets = s.packets
        if let lat = latency {
            m.latencyMs = Double(s.fill_ms) + Double(s.resampler_ms) + lat.ms(lat.totalFrames)
                + (voice?.latency ?? 0) * 1000
        }
        metrics = m

        // 音频包和心跳只要有一个在 2 秒内到过，就算连着
        var buf = [CChar](repeating: 0, count: 64)
        _ = aurio_rx_peer(r, &buf, UInt32(buf.count))
        let peer = String(cString: buf)
        let fresh = { (t: Double) in t >= 0 && t <= 2 }
        if fresh(s.since_last_packet_s) {
            state = .playing(peer: peer)
        } else if fresh(s.since_last_heartbeat_s) {
            state = .connected(peer: peer)
        } else if s.since_last_packet_s < 0 && s.since_last_heartbeat_s < 0 {
            state = .waiting
        } else {
            state = .disconnected(peer: peer)
        }

        micRequested = s.mic_requested != 0
        let want = micTestTone || micMode == .on || (micMode == .auto && micRequested)
        if want { startMic() } else { stopMic() }
    }

    // MARK: - Bonjour

    private func publishService() {
        guard service == nil else { return }
        let name = Host.current().localizedName ?? ProcessInfo.processInfo.hostName
        let svc = NetService(domain: "", type: AurioEngine.serviceType, name: name, port: Int32(port))
        svc.setTXTRecord(NetService.data(fromTXTRecord: ["v": Data("1".utf8)]))
        svc.publish()
        service = svc
    }
}

// MARK: - 电平

/// 播放和麦克风的实时电平，单位 dBFS。没有声音或没在工作时为 `LevelMeter.floor`。
public final class LevelMeter: ObservableObject {
    public static let floor: Float = -60
    @Published public internal(set) var outputDb: Float = LevelMeter.floor
    @Published public internal(set) var micDb: Float = LevelMeter.floor

    /// 峰值立即跟上，回落按每秒 24 dB，读起来不闪。
    static func smooth(_ prev: Float, peak: Float, dt: Float) -> Float {
        let db = peak > 0 ? max(LevelMeter.floor, 20 * log10(peak)) : LevelMeter.floor
        return max(db, prev - 24 * dt)
    }
}

extension AurioEngine {
    /// 面板打开时开启电平刷新，关上时停掉。
    public func setMetering(_ on: Bool) {
        if !on {
            meterTimer?.invalidate()
            meterTimer = nil
            levels.outputDb = LevelMeter.floor
            levels.micDb = LevelMeter.floor
            return
        }
        guard meterTimer == nil else { return }
        let dt: TimeInterval = 1.0 / 30
        // 先清掉面板关着时攒下的峰值
        if let r = rx { _ = aurio_rx_take_peak(r) }
        if let m = mic { _ = aurio_mic_take_peak(m) }
        let t = Timer(timeInterval: dt, repeats: true) { [weak self] _ in self?.meterTick(Float(dt)) }
        RunLoop.main.add(t, forMode: .common)
        meterTimer = t
    }

    private func meterTick(_ dt: Float) {
        let out = rx.map { aurio_rx_take_peak($0) } ?? 0
        let mi = mic.map { aurio_mic_take_peak($0) } ?? 0
        let o = LevelMeter.smooth(levels.outputDb, peak: out, dt: dt)
        let m = LevelMeter.smooth(levels.micDb, peak: mi, dt: dt)
        if o != levels.outputDb { levels.outputDb = o }
        if m != levels.micDb { levels.micDb = m }
    }
}


// MARK: - 版本 / 界面预览

extension AurioEngine {
    public static var coreVersion: String { String(cString: aurio_version()) }

    /// 只给界面截图用：填一组假的状态（"playing" = 正在播放、麦克风回声消除中；其他 = 等待连接）。不碰音频和网络。
    public func applyPreviewState(_ kind: String) {
        currentOutput = outputDevices.first { $0.name.contains("Studio Display") } ?? outputDevices.first
        currentInput = inputDevices.first { $0.name.contains("Studio Display") } ?? inputDevices.first
        guard kind == "playing" else {
            state = .waiting
            metrics = EngineMetrics()
            micActive = false
            micRequested = false
            echoCancelActive = false
            levels.outputDb = LevelMeter.floor
            levels.micDb = LevelMeter.floor
            return
        }
        state = .playing(peer: "192.168.1.10")
        var m = EngineMetrics()
        m.latencyMs = 48; m.fillMs = 10.3; m.targetMs = 10; m.ppm = -20; m.packetsPerSecond = 500
        metrics = m
        micActive = true
        micRequested = true
        echoCancelActive = echoCancel
        levels.outputDb = -14
        levels.micDb = -30
    }
}
