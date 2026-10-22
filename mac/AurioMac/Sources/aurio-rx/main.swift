// aurio-rx：Mac 端。收 Windows 发来的音频从 Studio Display 播放；
// Windows 有应用要用麦克风时，把 Studio Display 的麦克风发回去。

import AVFoundation
import AurioKit
import CAurio
import CoreAudio
import Foundation

setvbuf(stdout, nil, _IOLBF, 0)

func usage() -> Never {
    print("""
    用法：
      aurio-rx list                       列出输出和输入设备
      aurio-rx run [选项]                 接收并播放，按需回传麦克风

    run 的选项：
      --device 名称      按名称匹配输出设备（默认找 "Studio Display"，找不到用系统默认输出）
      --uid UID          按 UID 精确指定设备
      --port N           UDP 端口（默认 47800）
      --buffer N         设备 IO 缓冲帧数（默认 128）
      --target-ms N      初始目标最低水位（默认 2）
      --taps N           重采样单侧抽头数（默认 32）
      --volume N         音量 0..1（默认 1）
      --mic 模式         麦克风上行：auto = Windows 有应用在用时才开（默认）；on = 一直开；
                         off = 不开；tone = 不开麦克风，发 440 Hz 测试音（测链路用）
      --mic-device 名称  按名称匹配输入设备（默认找 "Studio Display"）
      --seconds N        运行多久后退出（默认一直运行）
      --quiet            不打印每秒统计
    """)
    exit(2)
}

var args = Array(CommandLine.arguments.dropFirst())
guard let cmd = args.first else { usage() }
args.removeFirst()

func opt(_ name: String) -> String? {
    guard let i = args.firstIndex(of: "--" + name) else { return nil }
    return i + 1 < args.count ? args[i + 1] : ""
}
func flag(_ name: String) -> Bool { args.contains("--" + name) }
func num(_ name: String, _ def: Double) -> Double {
    guard let s = opt(name) else { return def }
    guard let v = Double(s) else { print("错误：--\(name) 需要数字"); exit(2) }
    return v
}

switch cmd {
case "list":
    let def = defaultOutputDevice()?.id
    print(String(format: "%-6@ %-8@ %-6@ %-34@ %@", "ID", "采样率", "输出", "名称", "UID"))
    for d in allDevices() where d.outputChannels > 0 {
        let mark = d.id == def ? "*" : " "
        print(String(format: "%@%-5d %-8.0f %-6d %-34@ %@", mark, d.id, d.nominalRate, d.outputChannels, d.name, d.uid))
    }
    print("（* 为系统默认输出）\n")
    print(String(format: "%-6@ %-8@ %-6@ %-34@ %@", "ID", "采样率", "输入", "名称", "UID"))
    for d in allDevices() where d.inputChannels > 0 {
        print(String(format: " %-5d %-8.0f %-6d %-34@ %@", d.id, d.nominalRate, d.inputChannels, d.name, d.uid))
    }
    exit(0)
case "run":
    break
default:
    usage()
}

guard let dev = pickOutputDevice(uid: opt("uid"), nameContains: opt("device")) else {
    print("错误：找不到输出设备，用 aurio-rx list 查看")
    exit(1)
}
let outChannels = max(1, min(dev.outputChannels, 2))
let actualBuffer = setBufferFrames(dev, UInt32(num("buffer", 128)))

var cfg = AurioRxConfig()
aurio_rx_default_config(&cfg)
cfg.port = UInt16(num("port", Double(cfg.port)))
cfg.out_rate = dev.nominalRate
cfg.out_channels = UInt32(outChannels)
cfg.target_ms = num("target-ms", cfg.target_ms)
cfg.half_taps = UInt32(num("taps", Double(cfg.half_taps)))
cfg.volume = Float(num("volume", 1.0))

guard let rx = aurio_rx_create(&cfg) else {
    print("错误：" + String(cString: aurio_last_error()))
    exit(1)
}

let lat = outputLatency(dev)
print("Aurio 接收端 \(String(cString: aurio_version()))")
print("设备：\(dev.name)（UID \(dev.uid)）")
print(String(format: "采样率 %.0f Hz，输出 %d 声道（设备共 %d 声道）", dev.nominalRate, outChannels, dev.outputChannels))
if dev.nominalRate != 48000 {
    print("注意：设备不是 48 kHz，会在接收端重采样（不去改设备设置，避免影响其他软件）")
}
print(String(format: "设备输出延迟：缓冲 %u 帧（%.2f ms）+ 安全偏移 %u（%.2f ms）+ 设备 %u（%.2f ms）+ 流 %u（%.2f ms）= %.2f ms",
             lat.bufferFrames, lat.ms(lat.bufferFrames), lat.safetyOffset, lat.ms(lat.safetyOffset),
             lat.deviceLatency, lat.ms(lat.deviceLatency), lat.streamLatency, lat.ms(lat.streamLatency),
             lat.ms(lat.totalFrames)))
if actualBuffer != UInt32(num("buffer", 128)) {
    print("注意：设备不支持请求的缓冲大小，实际为 \(actualBuffer) 帧")
}
let output: HALOutput
do {
    output = try HALOutput(device: dev, rx: rx, channels: outChannels)
    try output.start()
} catch {
    print("错误：\(error)")
    aurio_rx_destroy(rx)
    exit(1)
}
if !output.channelMap.isEmpty {
    print("声道映射：" + output.channelMap.enumerated().map { "\($0.offset + 1)←\($0.element < 0 ? "-" : String($0.element + 1))" }.joined(separator: " "))
}

let micMode = opt("mic") ?? "auto"
guard ["auto", "on", "off", "tone"].contains(micMode) else { print("错误：--mic 只能是 auto / on / off / tone"); exit(2) }
let micDevice: AudioDevice? = (micMode == "off" || micMode == "tone") ? nil
    : pickInputDevice(uid: nil, nameContains: opt("mic-device"))
switch micMode {
case "off": print("麦克风上行：关闭")
case "tone": print("麦克风上行：测试音模式（不打开麦克风）")
default:
    if let m = micDevice {
        print(String(format: "麦克风：%@，%.0f Hz，%d 声道，%@", m.name, m.nominalRate, m.inputChannels,
                     micMode == "on" ? "一直开" : "Windows 有应用在用时才开"))
        if AVCaptureDevice.authorizationStatus(for: .audio) == .notDetermined {
            let sem = DispatchSemaphore(value: 0)
            AVCaptureDevice.requestAccess(for: .audio) { _ in sem.signal() }
            sem.wait()
        }
        if AVCaptureDevice.authorizationStatus(for: .audio) != .authorized {
            print("注意：没有麦克风权限。到「系统设置 → 隐私与安全性 → 麦克风」里允许终端（或你运行本程序的 App）")
        }
    } else {
        print("注意：找不到输入设备，麦克风上行不可用")
    }
}
print("监听 UDP 端口 \(cfg.port)，Ctrl-C 退出\n")

// 麦克风上行的开关状态（只在主队列上访问）
var mic: OpaquePointer?
var micInput: HALInput?
var micRetryAfter = Date.distantPast

func startMic() {
    guard mic == nil, Date() >= micRetryAfter else { return }
    if micMode == "tone" {
        mic = aurio_mic_create(rx, 48000, 1, 1)
        if mic == nil { print("错误：" + String(cString: aurio_last_error())) }
        return
    }
    guard let m = micDevice else { return }
    // Studio Display 的麦克风阵列已经在设备内做了波束成形，取前一路即可；上行线程会把多声道下混成单声道
    let ch = max(1, min(m.inputChannels, 2))
    _ = setBufferFrames(m, UInt32(num("buffer", 128)))
    guard let up = aurio_mic_create(rx, m.nominalRate, UInt32(ch), 0) else {
        print("错误：" + String(cString: aurio_last_error()))
        micRetryAfter = Date().addingTimeInterval(5)
        return
    }
    do {
        let input = try HALInput(device: m, mic: up, channels: ch)
        try input.start()
        mic = up
        micInput = input
        print("麦克风已打开")
    } catch {
        print("错误：打开麦克风失败：\(error)，5 秒后重试")
        aurio_mic_destroy(up)
        micRetryAfter = Date().addingTimeInterval(5)
    }
}

func stopMic() {
    guard let m = mic else { return }
    micInput?.stop()   // 先停回调，再销毁上行对象
    micInput = nil
    aurio_mic_destroy(m)
    mic = nil
    print("麦克风已关闭")
}

// 防止 App Nap 和空闲睡眠打断推流
let activity = ProcessInfo.processInfo.beginActivity(
    options: [.latencyCritical, .userInitiated, .idleSystemSleepDisabled],
    reason: "Aurio 正在播放网络音频")

func shutdown(_ code: Int32) -> Never {
    stopMic()
    output.stop()
    var s = AurioRxStats()
    aurio_rx_get_stats(rx, &s)
    aurio_rx_destroy(rx)
    ProcessInfo.processInfo.endActivity(activity)
    print(String(format: "\n结束：收包 %llu，欠载 %llu，丢帧 %llu，丢包补零 %llu 帧，迟到 %llu，坏包 %llu",
                 s.packets, s.underruns, s.skips, s.lost_frames, s.late_packets, s.bad_packets))
    exit(code)
}

signal(SIGINT, SIG_IGN)
signal(SIGTERM, SIG_IGN)
let sigs = [SIGINT, SIGTERM].map { sig -> DispatchSourceSignal in
    let src = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    src.setEventHandler { shutdown(0) }
    src.resume()
    return src
}

let stateNames = ["空闲", "缓冲", "播放"]
let quiet = flag("quiet")
let seconds = num("seconds", 0)
let startTime = Date()
var lastPackets: UInt64 = 0
var lastBytes: UInt64 = 0

let timer = DispatchSource.makeTimerSource(queue: .main)
timer.schedule(deadline: .now() + 1, repeating: 1)
timer.setEventHandler {
    var s = AurioRxStats()
    aurio_rx_get_stats(rx, &s)
    let pps = s.packets - lastPackets
    let mbps = Double(s.bytes - lastBytes) * 8 / 1e6
    lastPackets = s.packets
    lastBytes = s.bytes

    let wantMic = micMode == "on" || micMode == "tone" || (micMode == "auto" && s.mic_requested != 0)
    if wantMic { startMic() } else { stopMic() }
    var micText = "麦克风 关"
    if let m = mic {
        var ms = AurioMicStats()
        aurio_mic_get_stats(m, &ms)
        micText = String(format: "麦克风 开 已发 %llu 包%@", ms.sent_packets,
                         ms.dropped_frames > 0 ? String(format: " 丢 %llu 帧", ms.dropped_frames) : "")
    }

    if !quiet {
        let st = Int(s.state) < stateNames.count ? stateNames[Int(s.state)] : "?"
        // 接收端总延迟 ≈ 平均水位 + 重采样 + 设备输出
        let rxLatency = Double(s.fill_ms) + Double(s.resampler_ms) + lat.ms(lat.totalFrames)
        let silent = s.since_last_packet_s < 0 ? "（还没收到数据）" :
            (s.since_last_packet_s > 1 ? String(format: "（%.0f 秒没收到数据）", s.since_last_packet_s) : "")
        print(String(format: "%@ 水位 %5.2f ms 最低 %5.2f 目标 %5.2f | 修正 %+6.1f ppm | 批 %5.2f 最大间隔 %5.2f ms | %4llu 包/秒 %.2f Mbps | 欠载 %llu 丢帧 %llu 丢包 %llu | 接收端延迟≈%.1f ms | %@ %@",
                     st, s.fill_ms, s.window_min_ms, s.target_ms, s.ppm, s.burst_ms, s.max_gap_ms, pps, mbps,
                     s.underruns, s.skips, s.lost_frames, rxLatency, micText, silent))
    }
    if seconds > 0 && Date().timeIntervalSince(startTime) >= seconds { shutdown(0) }
}
timer.resume()

withExtendedLifetime((sigs, timer, output)) {
    dispatchMain()
}
