import CoreAudio
import Foundation

/// CoreAudio 设备信息。按 UID 锁定设备，不跟随系统默认设备变化。
public struct AudioDevice: Hashable, Identifiable {
    public let id: AudioObjectID
    public let uid: String
    public let name: String
    public let outputChannels: Int
    public let inputChannels: Int
    public let nominalRate: Double
}

private func address(_ sel: AudioObjectPropertySelector,
                     _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: sel, mScope: scope, mElement: kAudioObjectPropertyElementMain)
}

public func getValue<T>(_ obj: AudioObjectID, _ sel: AudioObjectPropertySelector,
                 _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal, _ fallback: T) -> T {
    var addr = address(sel, scope)
    var value = fallback
    var size = UInt32(MemoryLayout<T>.size)
    let st = withUnsafeMutableBytes(of: &value) { AudioObjectGetPropertyData(obj, &addr, 0, nil, &size, $0.baseAddress!) }
    return st == noErr ? value : fallback
}

@discardableResult
public func setValue<T>(_ obj: AudioObjectID, _ sel: AudioObjectPropertySelector,
                 _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal, _ value: T) -> OSStatus {
    var addr = address(sel, scope)
    var v = value
    return withUnsafeBytes(of: &v) { AudioObjectSetPropertyData(obj, &addr, 0, nil, UInt32($0.count), $0.baseAddress!) }
}

public func getString(_ obj: AudioObjectID, _ sel: AudioObjectPropertySelector) -> String {
    var addr = address(sel)
    var cf: Unmanaged<CFString>?
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    let st = AudioObjectGetPropertyData(obj, &addr, 0, nil, &size, &cf)
    guard st == noErr, let s = cf?.takeRetainedValue() else { return "" }
    return s as String
}

private func channelCount(_ dev: AudioObjectID, _ scope: AudioObjectPropertyScope) -> Int {
    var addr = address(kAudioDevicePropertyStreamConfiguration, scope)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(dev, &addr, 0, nil, &size) == noErr, size > 0 else { return 0 }
    let raw = UnsafeMutableRawPointer.allocate(byteCount: Int(size), alignment: 16)
    defer { raw.deallocate() }
    guard AudioObjectGetPropertyData(dev, &addr, 0, nil, &size, raw) == noErr else { return 0 }
    let abl = UnsafeMutableAudioBufferListPointer(raw.assumingMemoryBound(to: AudioBufferList.self))
    return abl.reduce(0) { $0 + Int($1.mNumberChannels) }
}

public func deviceInfo(_ id: AudioObjectID) -> AudioDevice {
    AudioDevice(
        id: id,
        uid: getString(id, kAudioDevicePropertyDeviceUID),
        name: getString(id, kAudioObjectPropertyName),
        outputChannels: channelCount(id, kAudioObjectPropertyScopeOutput),
        inputChannels: channelCount(id, kAudioObjectPropertyScopeInput),
        nominalRate: getValue(id, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, Float64(0))
    )
}

public func allDevices() -> [AudioDevice] {
    var addr = address(kAudioHardwarePropertyDevices)
    var size: UInt32 = 0
    let sys = AudioObjectID(kAudioObjectSystemObject)
    guard AudioObjectGetPropertyDataSize(sys, &addr, 0, nil, &size) == noErr else { return [] }
    var ids = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
    guard AudioObjectGetPropertyData(sys, &addr, 0, nil, &size, &ids) == noErr else { return [] }
    return ids.map(deviceInfo)
}

public func defaultOutputDevice() -> AudioDevice? {
    let id: AudioObjectID = getValue(AudioObjectID(kAudioObjectSystemObject),
                                     kAudioHardwarePropertyDefaultOutputDevice,
                                     kAudioObjectPropertyScopeGlobal, AudioObjectID(0))
    return id == 0 ? nil : deviceInfo(id)
}

/// 选择输出设备：指定 UID > 名称包含指定文字 > 名称含 "Studio Display" > 系统默认输出。
public func pickOutputDevice(uid: String?, nameContains: String?) -> AudioDevice? {
    let outs = allDevices().filter { $0.outputChannels > 0 }
    if let uid { return outs.first { $0.uid == uid } }
    if let n = nameContains { return outs.first { $0.name.localizedCaseInsensitiveContains(n) } }
    if let sd = outs.first(where: { $0.name.localizedCaseInsensitiveContains("Studio Display") }) { return sd }
    return defaultOutputDevice()
}

/// 选择输入设备：指定 UID > 名称包含指定文字 > 名称含 "Studio Display" > 系统默认输入。
public func pickInputDevice(uid: String?, nameContains: String?) -> AudioDevice? {
    let ins = allDevices().filter { $0.inputChannels > 0 }
    if let uid { return ins.first { $0.uid == uid } }
    if let n = nameContains { return ins.first { $0.name.localizedCaseInsensitiveContains(n) } }
    if let sd = ins.first(where: { $0.name.localizedCaseInsensitiveContains("Studio Display") }) { return sd }
    let id: AudioObjectID = getValue(AudioObjectID(kAudioObjectSystemObject),
                                     kAudioHardwarePropertyDefaultInputDevice,
                                     kAudioObjectPropertyScopeGlobal, AudioObjectID(0))
    return id == 0 ? nil : deviceInfo(id)
}

public struct OutputLatency {
    public var bufferFrames: UInt32
    public var safetyOffset: UInt32
    public var deviceLatency: UInt32
    public var streamLatency: UInt32
    public var rate: Double

    public var totalFrames: UInt32 { bufferFrames + safetyOffset + deviceLatency + streamLatency }
    public func ms(_ f: UInt32) -> Double { Double(f) * 1000.0 / rate }
}

public func outputLatency(_ dev: AudioDevice) -> OutputLatency {
    let out = kAudioObjectPropertyScopeOutput
    var streamLatency: UInt32 = 0
    var addr = address(kAudioDevicePropertyStreams, out)
    var size: UInt32 = 0
    if AudioObjectGetPropertyDataSize(dev.id, &addr, 0, nil, &size) == noErr, size >= 4 {
        var streams = [AudioStreamID](repeating: 0, count: Int(size) / 4)
        if AudioObjectGetPropertyData(dev.id, &addr, 0, nil, &size, &streams) == noErr, let s = streams.first {
            streamLatency = getValue(s, kAudioStreamPropertyLatency, kAudioObjectPropertyScopeGlobal, UInt32(0))
        }
    }
    return OutputLatency(
        bufferFrames: getValue(dev.id, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal, UInt32(0)),
        safetyOffset: getValue(dev.id, kAudioDevicePropertySafetyOffset, out, UInt32(0)),
        deviceLatency: getValue(dev.id, kAudioDevicePropertyLatency, out, UInt32(0)),
        streamLatency: streamLatency,
        rate: dev.nominalRate
    )
}

/// 设置设备 IO 缓冲帧数，会限制在设备支持的范围内。返回实际生效的值。
public func setBufferFrames(_ dev: AudioDevice, _ frames: UInt32) -> UInt32 {
    let range: AudioValueRange = getValue(dev.id, kAudioDevicePropertyBufferFrameSizeRange,
                                          kAudioObjectPropertyScopeGlobal, AudioValueRange(mMinimum: 0, mMaximum: 0))
    var f = frames
    if range.mMaximum > 0 {
        f = UInt32(min(max(Double(frames), range.mMinimum), range.mMaximum))
    }
    setValue(dev.id, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal, f)
    return getValue(dev.id, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal, UInt32(0))
}

/// 系统为这个设备设定的立体声左右声道（从 1 开始）。取不到时返回 [1, 2]。
public func preferredStereoChannels(_ dev: AudioDevice) -> [UInt32] {
    var addr = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyPreferredChannelsForStereo,
                                          mScope: kAudioObjectPropertyScopeOutput,
                                          mElement: kAudioObjectPropertyElementMain)
    var ch: [UInt32] = [1, 2]
    var size = UInt32(8)
    let st = AudioObjectGetPropertyData(dev.id, &addr, 0, nil, &size, &ch)
    if st != noErr || ch.contains(0) || ch.contains(where: { Int($0) > dev.outputChannels }) { return [1, 2] }
    return ch
}
