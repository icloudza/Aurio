import AudioToolbox
import CAurio
import CoreAudio

/// 输入回调用到的状态。放在手动分配的内存里，实时线程里不会碰到 Swift 的引用计数。
private struct InputContext {
    var unit: AudioUnit?
    var mic: OpaquePointer?
    var buffer: UnsafeMutablePointer<Float>
    var capacityFrames: UInt32
    var channels: UInt32
}

/// 实时输入回调：从 AUHAL 取出这一批采样，直接交给 Rust 的上行环形缓冲。
private let inputProc: AURenderCallback = { refCon, flags, timeStamp, _, frames, _ in
    let ctx = refCon.assumingMemoryBound(to: InputContext.self)
    guard let unit = ctx.pointee.unit, frames <= ctx.pointee.capacityFrames else { return noErr }
    let ch = ctx.pointee.channels
    var abl = AudioBufferList(
        mNumberBuffers: 1,
        mBuffers: AudioBuffer(mNumberChannels: ch, mDataByteSize: frames * 4 * ch,
                              mData: UnsafeMutableRawPointer(ctx.pointee.buffer)))
    let st = AudioUnitRender(unit, flags, timeStamp, 1, frames, &abl)
    if st == noErr {
        aurio_mic_push(ctx.pointee.mic, ctx.pointee.buffer, frames)
    }
    return st
}

/// 用 AUHAL 从指定输入设备采集（不跟随系统默认输入）。
public final class HALInput {
    private var unit: AudioUnit?
    private let ctx: UnsafeMutablePointer<InputContext>
    public let channels: Int

    public init(device: AudioDevice, mic: OpaquePointer, channels: Int) throws {
        self.channels = channels
        let capacity: UInt32 = 8192
        ctx = UnsafeMutablePointer<InputContext>.allocate(capacity: 1)
        ctx.initialize(to: InputContext(
            unit: nil, mic: mic,
            buffer: UnsafeMutablePointer<Float>.allocate(capacity: Int(capacity) * channels),
            capacityFrames: capacity, channels: UInt32(channels)))

        var desc = AudioComponentDescription(
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_HALOutput,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0, componentFlagsMask: 0)
        guard let comp = AudioComponentFindNext(nil, &desc) else {
            throw AudioError.osStatus("查找 AUHAL", -1)
        }
        var u: AudioUnit?
        try check(AudioComponentInstanceNew(comp, &u), "创建输入 AUHAL")
        guard let unit = u else { throw AudioError.osStatus("创建输入 AUHAL", -1) }
        self.unit = unit
        ctx.pointee.unit = unit

        // 只要输入：element 1 输入开、element 0 输出关
        var on: UInt32 = 1, off: UInt32 = 0
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Input, 1,
                                       &on, 4), "启用输入")
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Output, 0,
                                       &off, 4), "关闭输出")

        var devID = device.id
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0,
                                       &devID, UInt32(MemoryLayout<AudioObjectID>.size)), "绑定输入设备")

        // 我们要的数据格式：设备采样率、交错 Float32
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
        try check(AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Output, 1,
                                       &fmt, UInt32(MemoryLayout<AudioStreamBasicDescription>.size)), "设置输入格式")

        var cb = AURenderCallbackStruct(inputProc: inputProc, inputProcRefCon: UnsafeMutableRawPointer(ctx))
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_SetInputCallback, kAudioUnitScope_Global, 0,
                                       &cb, UInt32(MemoryLayout<AURenderCallbackStruct>.size)), "设置输入回调")

        try check(AudioUnitInitialize(unit), "初始化输入 AUHAL")
    }

    public func start() throws {
        guard let unit else { return }
        try check(AudioOutputUnitStart(unit), "启动输入")
    }

    /// 停止并释放 AUHAL。调用后才能安全地销毁 Rust 那边的上行对象。
    public func stop() {
        guard let unit else { return }
        AudioOutputUnitStop(unit)
        AudioUnitUninitialize(unit)
        AudioComponentInstanceDispose(unit)
        self.unit = nil
        ctx.pointee.unit = nil
    }

    deinit {
        stop()
        ctx.pointee.buffer.deallocate()
        ctx.deinitialize(count: 1)
        ctx.deallocate()
    }
}
