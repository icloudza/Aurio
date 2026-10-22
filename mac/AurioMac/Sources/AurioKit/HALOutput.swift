import AudioToolbox
import CAurio
import CoreAudio

public enum AudioError: Error, CustomStringConvertible {
    case osStatus(String, OSStatus)

    public var description: String {
        switch self {
        case let .osStatus(what, st):
            let bytes = withUnsafeBytes(of: st.bigEndian) { Array($0) }
            let fourcc = bytes.allSatisfy { $0 >= 32 && $0 < 127 } ? " '\(String(decoding: bytes, as: UTF8.self))'" : ""
            return "\(what) 失败（OSStatus \(st)\(fourcc)）"
        }
    }
}

public func check(_ st: OSStatus, _ what: String) throws {
    if st != noErr { throw AudioError.osStatus(what, st) }
}

/// 实时渲染回调：直接把设备缓冲交给 Rust 填。这里不分配内存、不加锁、不打日志。
private let renderProc: AURenderCallback = { refCon, _, _, _, frames, ioData in
    guard let ioData else { return noErr }
    let abl = UnsafeMutableAudioBufferListPointer(ioData)
    guard let buf = abl.first?.mData else { return noErr }
    let rx = OpaquePointer(refCon)
    aurio_rx_render(rx, buf.assumingMemoryBound(to: Float.self), frames)
    return noErr
}

/// 用 AUHAL 直接驱动指定设备输出（不经过系统默认设备）。
public final class HALOutput {
    private var unit: AudioUnit?
    public let channels: Int
    public let rate: Double
    public private(set) var channelMap: [Int32] = []

    public init(device: AudioDevice, rx: OpaquePointer, channels: Int) throws {
        self.channels = channels
        self.rate = device.nominalRate

        var desc = AudioComponentDescription(
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_HALOutput,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0, componentFlagsMask: 0)
        guard let comp = AudioComponentFindNext(nil, &desc) else {
            throw AudioError.osStatus("查找 AUHAL", -1)
        }
        var u: AudioUnit?
        try check(AudioComponentInstanceNew(comp, &u), "创建 AUHAL")
        guard let unit = u else { throw AudioError.osStatus("创建 AUHAL", -1) }
        self.unit = unit

        // 只要输出：element 0 输出开、element 1 输入关
        var on: UInt32 = 1, off: UInt32 = 0
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Output, 0,
                                       &on, 4), "启用输出")
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Input, 1,
                                       &off, 4), "关闭输入")

        var devID = device.id
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0,
                                       &devID, UInt32(MemoryLayout<AudioObjectID>.size)), "绑定设备")

        // 我们提供的数据格式：设备采样率、交错 Float32
        var fmt = AudioStreamBasicDescription(
            mSampleRate: device.nominalRate,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: UInt32(4 * channels),
            mFramesPerPacket: 1,
            mBytesPerFrame: UInt32(4 * channels),
            mChannelsPerFrame: UInt32(channels),
            mBitsPerChannel: 32,
            mReserved: 0)
        try check(AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, 0,
                                       &fmt, UInt32(MemoryLayout<AudioStreamBasicDescription>.size)), "设置格式")

        // 设备声道多于我们提供的声道时，按系统的立体声首选声道映射，其余声道静音
        if device.outputChannels > channels {
            var map = [Int32](repeating: -1, count: device.outputChannels)
            let pref = preferredStereoChannels(device)
            for c in 0..<channels {
                map[Int(pref[min(c, pref.count - 1)]) - 1] = Int32(c)
            }
            try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_ChannelMap, kAudioUnitScope_Output, 0,
                                           &map, UInt32(map.count * 4)), "设置声道映射")
            channelMap = map
        }

        var cb = AURenderCallbackStruct(inputProc: renderProc, inputProcRefCon: UnsafeMutableRawPointer(rx))
        try check(AudioUnitSetProperty(unit, kAudioUnitProperty_SetRenderCallback, kAudioUnitScope_Input, 0,
                                       &cb, UInt32(MemoryLayout<AURenderCallbackStruct>.size)), "设置回调")

        try check(AudioUnitInitialize(unit), "初始化 AUHAL")
    }

    public func start() throws {
        guard let unit else { return }
        try check(AudioOutputUnitStart(unit), "启动输出")
    }

    public func stop() {
        guard let unit else { return }
        AudioOutputUnitStop(unit)
        AudioUnitUninitialize(unit)
        AudioComponentInstanceDispose(unit)
        self.unit = nil
    }

    deinit { stop() }
}
