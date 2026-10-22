import AudioToolbox
import CAurio
import CoreAudio

/// 回调里用到的状态，放在手动分配的内存里，实时线程里不碰 Swift 引用计数。
struct VoiceContext {
    var unit: AudioUnit?
    /// 播放来源：rx 不为空时从 Rust 渲染 Windows 传来的声音；否则播放 testPlay（测试用）
    var rx: OpaquePointer?
    var testPlay: UnsafeMutablePointer<Float>?
    var testPlayFrames: Int
    var playPos: Int
    var outChannels: UInt32
    /// 采集去向：mic 不为空时交给 Rust 上行；否则录进 testRec（测试用）
    var mic: OpaquePointer?
    var testRec: UnsafeMutablePointer<Float>?
    var testRecFrames: Int
    var recPos: Int
    var inBuffer: UnsafeMutablePointer<Float>
    var inCapacity: UInt32
    /// 测试用：第一次进采集回调时已经渲染了多少帧（用来对齐参照和录音）
    var firstInputPlayPos: Int
}

/// 播放回调：语音处理单元要播什么，就从这里取。它播出的内容同时也是回声消除的参照信号。
private let voiceRender: AURenderCallback = { refCon, _, _, _, frames, ioData in
    let ctx = refCon.assumingMemoryBound(to: VoiceContext.self)
    guard let ioData, let out = UnsafeMutableAudioBufferListPointer(ioData).first?.mData else { return noErr }
    let buf = out.assumingMemoryBound(to: Float.self)
    let ch = Int(ctx.pointee.outChannels)
    if let rx = ctx.pointee.rx {
        aurio_rx_render(rx, buf, frames)
    } else if let play = ctx.pointee.testPlay {
        for f in 0..<Int(frames) {
            let p = ctx.pointee.playPos
            let v: Float = p < ctx.pointee.testPlayFrames ? play[p] : 0
            for c in 0..<ch { buf[f * ch + c] = v }
            ctx.pointee.playPos = p + 1
        }
    } else {
        buf.update(repeating: 0, count: Int(frames) * ch)
    }
    return noErr
}

/// 采集回调：取出已经做过回声消除的麦克风声音（单声道）。
private let voiceInput: AURenderCallback = { refCon, flags, timeStamp, _, frames, _ in
    let ctx = refCon.assumingMemoryBound(to: VoiceContext.self)
    guard let unit = ctx.pointee.unit, frames <= ctx.pointee.inCapacity else { return noErr }
    var abl = AudioBufferList(
        mNumberBuffers: 1,
        mBuffers: AudioBuffer(mNumberChannels: 1, mDataByteSize: frames * 4,
                              mData: UnsafeMutableRawPointer(ctx.pointee.inBuffer)))
    let st = AudioUnitRender(unit, flags, timeStamp, 1, frames, &abl)
    guard st == noErr else { return st }
    if let mic = ctx.pointee.mic {
        aurio_mic_push(mic, ctx.pointee.inBuffer, frames)
    } else if let rec = ctx.pointee.testRec {
        if ctx.pointee.recPos == 0 && ctx.pointee.firstInputPlayPos < 0 {
            ctx.pointee.firstInputPlayPos = ctx.pointee.playPos
        }
        let n = min(Int(frames), ctx.pointee.testRecFrames - ctx.pointee.recPos)
        if n > 0 {
            (rec + ctx.pointee.recPos).update(from: ctx.pointee.inBuffer, count: n)
            ctx.pointee.recPos += n
        }
    }
    return noErr
}

/// 苹果的语音处理单元（和 FaceTime 同一套回声消除、降噪、自动增益）。
/// 同一个单元既负责播放又负责采集：Windows 传来的声音经它播放，它就知道扬声器放了什么，
/// 能从麦克风里把这部分减掉。
public final class VoiceIO {
    private var unit: AudioUnit?
    let ctx: UnsafeMutablePointer<VoiceContext>
    public let rate: Double
    public let outChannels: Int
    /// 语音处理单元自己报告的额外延迟（秒）
    public private(set) var latency: Double = 0

    /// - rate：播放和采集给我们的数据都用这个采样率（应与 rx 的 out_rate 一致）
    /// - bypass：关掉回声消除等处理，直接拿原始麦克风（对比测试用）
    public init(output: AudioDevice, input: AudioDevice, rate: Double, outChannels: Int = 2,
                rx: OpaquePointer?, mic: OpaquePointer?, bypass: Bool = false) throws {
        self.rate = rate
        self.outChannels = outChannels
        let capacity: UInt32 = 8192
        ctx = UnsafeMutablePointer<VoiceContext>.allocate(capacity: 1)
        ctx.initialize(to: VoiceContext(
            unit: nil, rx: rx, testPlay: nil, testPlayFrames: 0, playPos: 0, outChannels: UInt32(outChannels),
            mic: mic, testRec: nil, testRecFrames: 0, recPos: 0,
            inBuffer: UnsafeMutablePointer<Float>.allocate(capacity: Int(capacity)), inCapacity: capacity,
            firstInputPlayPos: -1))

        var desc = AudioComponentDescription(
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_VoiceProcessingIO,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0, componentFlagsMask: 0)
        guard let comp = AudioComponentFindNext(nil, &desc) else {
            throw AudioError.osStatus("查找语音处理单元", -1)
        }
        var u: AudioUnit?
        try check(AudioComponentInstanceNew(comp, &u), "创建语音处理单元")
        guard let unit = u else { throw AudioError.osStatus("创建语音处理单元", -1) }
        self.unit = unit
        ctx.pointee.unit = unit

        // 指定 Studio Display 的扬声器和麦克风（不跟随系统默认设备）
        var outID = output.id, inID = input.id
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Output, 0,
                                       &outID, 4), "绑定输出设备")
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Input, 1,
                                       &inID, 4), "绑定麦克风")

        // 我们给它的播放数据：立体声交错 Float32；它给我们的麦克风：单声道 Float32
        var outFmt = AudioStreamBasicDescription(
            mSampleRate: rate, mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: UInt32(4 * outChannels), mFramesPerPacket: 1, mBytesPerFrame: UInt32(4 * outChannels),
            mChannelsPerFrame: UInt32(outChannels), mBitsPerChannel: 32, mReserved: 0)
        try check(AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, 0,
                                       &outFmt, UInt32(MemoryLayout<AudioStreamBasicDescription>.size)), "设置播放格式")
        var inFmt = outFmt
        inFmt.mChannelsPerFrame = 1
        inFmt.mBytesPerFrame = 4
        inFmt.mBytesPerPacket = 4
        try check(AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Output, 1,
                                       &inFmt, UInt32(MemoryLayout<AudioStreamBasicDescription>.size)), "设置麦克风格式")

        // 回声消除期间 Mac 上其他 App 的声音会被压低，这里压到最小（macOS 14 起支持）
        if #available(macOS 14.0, *) {
            var duck = AUVoiceIOOtherAudioDuckingConfiguration(mEnableAdvancedDucking: false, mDuckingLevel: .min)
            _ = AudioUnitSetProperty(unit, kAUVoiceIOProperty_OtherAudioDuckingConfiguration, kAudioUnitScope_Global, 0,
                                     &duck, UInt32(MemoryLayout<AUVoiceIOOtherAudioDuckingConfiguration>.size))
        }
        var bp: UInt32 = bypass ? 1 : 0
        _ = AudioUnitSetProperty(unit, kAUVoiceIOProperty_BypassVoiceProcessing, kAudioUnitScope_Global, 0, &bp, 4)
        // 自动增益：让说话音量稳定（游戏里离麦远近不一）
        var agc: UInt32 = 1
        _ = AudioUnitSetProperty(unit, kAUVoiceIOProperty_VoiceProcessingEnableAGC, kAudioUnitScope_Global, 0, &agc, 4)

        var render = AURenderCallbackStruct(inputProc: voiceRender, inputProcRefCon: UnsafeMutableRawPointer(ctx))
        try check(AudioUnitSetProperty(unit, kAudioUnitProperty_SetRenderCallback, kAudioUnitScope_Input, 0,
                                       &render, UInt32(MemoryLayout<AURenderCallbackStruct>.size)), "设置播放回调")
        var input = AURenderCallbackStruct(inputProc: voiceInput, inputProcRefCon: UnsafeMutableRawPointer(ctx))
        try check(AudioUnitSetProperty(unit, kAudioOutputUnitProperty_SetInputCallback, kAudioUnitScope_Global, 0,
                                       &input, UInt32(MemoryLayout<AURenderCallbackStruct>.size)), "设置采集回调")

        try check(AudioUnitInitialize(unit), "初始化语音处理单元")
        var lat = Float64(0)
        var sz = UInt32(8)
        if AudioUnitGetProperty(unit, kAudioUnitProperty_Latency, kAudioUnitScope_Global, 0, &lat, &sz) == noErr {
            latency = lat
        }
    }

    /// 测试用：播放这段单声道信号、同时录下同样长度的麦克风。调用方负责两块内存的生命周期。
    func setTestBuffers(play: UnsafeMutablePointer<Float>, playFrames: Int,
                        record: UnsafeMutablePointer<Float>, recordFrames: Int) {
        ctx.pointee.testPlay = play
        ctx.pointee.testPlayFrames = playFrames
        ctx.pointee.playPos = 0
        ctx.pointee.testRec = record
        ctx.pointee.testRecFrames = recordFrames
        ctx.pointee.recPos = 0
        ctx.pointee.firstInputPlayPos = -1
    }

    var recordedFrames: Int { ctx.pointee.recPos }
    var firstInputPlayPos: Int { ctx.pointee.firstInputPlayPos }

    public func start() throws {
        guard let unit else { return }
        try check(AudioOutputUnitStart(unit), "启动语音处理单元")
    }

    /// 停止并释放。调用后才能安全地销毁 Rust 那边的 rx / mic。
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
        ctx.pointee.inBuffer.deallocate()
        ctx.deinitialize(count: 1)
        ctx.deallocate()
    }
}
