import CAurio
import Foundation

/// 回声消除效果测试：通过扬声器播放一段类似音乐/游戏声的测试信号，同时录麦克风，
/// 分别在「关闭处理（旁路）」和「打开回声消除」下各录一次，比较回声强度。
/// 两次用的是同一个语音处理单元、同一条路由，只差处理开关，对比才公平。
public struct EchoCancelReport {
    public var raw = AurioEchoResult()
    public var processed = AurioEchoResult()
    /// 录音电平（dBFS）：旁路 / 处理后
    public var rawLevelDb: Double = 0
    public var processedLevelDb: Double = 0
    public var seconds: Double = 0
    public var levelDb: Double = 0

    public var text: String {
        func line(_ name: String, _ r: AurioEchoResult, _ level: Double) -> String {
            let v = ["播放太小", "录音静音", "没检出回声", "轻微回声", "明显回声"][Int(min(r.verdict, 4))]
            return String(format: "%@：%@ | 回声强度 %.1f dB · 相关 %.2f · 回声占录音 %.0f%% · 延迟 %.0f ms · 录音电平 %.1f dB",
                          name, v, r.echo_db, r.correlation, r.echo_share * 100, r.delay_ms, level)
        }
        // 处理后的录音里，回声能量 ≈ 电平 + 10·log10(占比)；和旁路比，得到回声被压低了多少
        func echoEnergy(_ r: AurioEchoResult, _ level: Double) -> Double {
            level + 10 * log10(max(Double(r.echo_share), 1e-6))
        }
        let suppression = echoEnergy(raw, rawLevelDb) - echoEnergy(processed, processedLevelDb)
        return [
            String(format: "测试信号 %.0f 秒，电平 %.0f dBFS（实际响度取决于 Studio Display 的音量）", seconds, levelDb),
            line("关闭回声消除", raw, rawLevelDb),
            line("打开回声消除", processed, processedLevelDb),
            String(format: "回声被压低约 %.0f dB%@", suppression,
                   processed.verdict <= 2 ? "（处理后已检测不到回声，实际压低的可能更多）" : ""),
        ].joined(separator: "\n")
    }
}

/// 粉红噪声加随机起伏的包络：有强有弱、有停顿，接近音乐和游戏声。
func makeTestSignal(frames: Int, rate: Double, levelDb: Double) -> [Float] {
    var s: UInt64 = 0x9E37_79B9_7F4A_7C15
    func rnd() -> Float {
        s ^= s << 13; s ^= s >> 7; s ^= s << 17
        return Float(s >> 40) / Float(1 << 24)
    }
    var b0: Float = 0, b1: Float = 0, b2: Float = 0
    var out = [Float](repeating: 0, count: frames)
    var i = 0
    while i < frames {
        let on = Int((0.2 + Double(rnd()) * 0.6) * rate)
        let off = Int((0.05 + Double(rnd()) * 0.25) * rate)
        let gain = powf(10, (-6 * rnd()) / 20)
        for k in 0..<on where i + k < frames {
            let w = rnd() * 2 - 1
            b0 = 0.99765 * b0 + w * 0.0990460
            b1 = 0.96300 * b1 + w * 0.2965164
            b2 = 0.57000 * b2 + w * 1.0526913
            // 起止各 10 ms 淡入淡出，避免咔哒声
            let edge = min(Float(k), Float(on - k)) / Float(rate * 0.01)
            out[i + k] = (b0 + b1 + b2 + w * 0.1848) * gain * min(1, edge)
        }
        i += on + off
    }
    let rms = sqrt(out.reduce(0) { $0 + $1 * $1 } / Float(frames))
    let scale = powf(10, Float(levelDb) / 20) / max(rms, 1e-6)
    return out.map { $0 * scale }
}

/// 在当前线程跑完两次录音并分析（约 2 × (seconds + 1) 秒）。
public func runEchoCancelTest(output: AudioDevice, input: AudioDevice, seconds: Double = 10,
                              levelDb: Double = -24) throws -> EchoCancelReport {
    let rate = 48000.0
    let frames = Int(seconds * rate)
    let skip = Int(2 * rate)   // 前 2 秒是回声消除的收敛期，不计入
    var signal = makeTestSignal(frames: frames, rate: rate, levelDb: levelDb)
    var report = EchoCancelReport()
    report.seconds = seconds
    report.levelDb = levelDb

    for bypass in [true, false] {
        var rec = [Float](repeating: 0, count: frames + Int(rate))
        let v = try VoiceIO(output: output, input: input, rate: rate, outChannels: 2, rx: nil, mic: nil, bypass: bypass)
        signal.withUnsafeMutableBufferPointer { play in
            rec.withUnsafeMutableBufferPointer { r in
                v.setTestBuffers(play: play.baseAddress!, playFrames: frames, record: r.baseAddress!, recordFrames: r.count)
                try? v.start()
                Thread.sleep(forTimeInterval: seconds + 0.5)
                v.stop()
            }
        }
        // 对齐：参照从「第一次采集时已渲染的位置」往前 100 ms 开始，保证回声延迟为正
        let refStart = max(0, v.firstInputPlayPos - Int(0.1 * rate))
        let n = min(v.recordedFrames, frames - refStart) - skip
        guard n > Int(rate) else { throw AudioError.osStatus("录音太短（麦克风没有数据？）", -1) }
        var res = AurioEchoResult()
        rec.withUnsafeBufferPointer { r in
            signal.withUnsafeBufferPointer { p in
                aurio_echo_analyze(r.baseAddress! + skip, p.baseAddress! + refStart + skip, UInt64(n), rate, 1.0, &res)
            }
        }
        let level = 10 * log10(max(Double(rec[skip..<(skip + n)].reduce(0) { $0 + $1 * $1 }) / Double(n), 1e-12))
        if bypass {
            report.raw = res
            report.rawLevelDb = level
        } else {
            report.processed = res
            report.processedLevelDb = level
        }
        Thread.sleep(forTimeInterval: 0.5)
    }
    return report
}
