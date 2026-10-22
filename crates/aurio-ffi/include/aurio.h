// Aurio 核心库的 C 接口。Swift 通过 module.modulemap 引入。
#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct AurioRx AurioRx;

typedef struct {
    uint16_t port;          // 监听的 UDP 端口，默认 47800
    uint8_t  stream_id;     // 0 = 播放方向
    uint16_t format_id;     // 1 = 48k/24bit/立体声
    double   out_rate;      // 本地输出设备采样率
    uint32_t out_channels;  // 本地输出设备声道数
    double   target_ms;     // 初始目标最低水位
    double   max_ppm;       // 漂移修正上限
    uint32_t half_taps;     // 重采样单侧抽头数
    float    volume;        // 线性音量 0..1
} AurioRxConfig;

typedef struct {
    uint32_t state;         // 0 空闲 1 缓冲中 2 播放中
    float    fill_ms;
    float    window_min_ms;
    float    target_ms;
    float    ppm;
    float    burst_ms;      // 成批到达的最大时长，约等于发送端采集周期
    float    max_gap_ms;    // 最近两个包之间的最大间隔，明显大于采集周期说明发送端或网络卡顿
    float    resampler_ms;  // 重采样固定延迟
    uint64_t underruns;
    uint64_t skips;
    uint64_t packets;
    uint64_t bytes;
    uint64_t lost_frames;
    uint64_t late_packets;
    uint64_t bad_packets;
    uint64_t resyncs;
    uint64_t overflow_frames;
    uint64_t rendered_frames;
    double   since_last_packet_s;     // 距离上一个音频包多久，负数表示还没收到过
    double   since_last_heartbeat_s;  // 距离上一个心跳多久，负数表示还没收到过
    uint32_t mic_requested;           // 对端最近 3 秒内请求过麦克风
    uint32_t has_peer;                // 已经知道对端地址（收到过对端的包）
} AurioRxStats;

void aurio_rx_default_config(AurioRxConfig *cfg);

// 绑定 UDP 端口并启动网络线程。失败返回 NULL，原因用 aurio_last_error() 取。
AurioRx *aurio_rx_create(const AurioRxConfig *cfg);

// 只能在音频实时回调里调用（同一时间只有一个线程调用）。
// out 为交错 float，长度 frames * out_channels。
void aurio_rx_render(AurioRx *rx, float *out, uint32_t frames);

void aurio_rx_get_stats(const AurioRx *rx, AurioRxStats *out);
void aurio_rx_set_volume(AurioRx *rx, float volume);
// 上次调用以来的输出峰值（线性，已乘音量），取完清零。界面画电平用。
float aurio_rx_take_peak(const AurioRx *rx);

// 最近一个对端的 IP（不含端口），写进 buf 并以 NUL 结尾。有对端返回 1，否则返回 0。
uint32_t aurio_rx_peer(const AurioRx *rx, char *buf, uint32_t len);

// 本端状态，附在心跳回声里告诉 Windows。bit0 = 正在做回声消除。
void aurio_rx_set_status_flags(AurioRx *rx, uint32_t flags);

// 必须先停掉音频回调再调用。
void aurio_rx_destroy(AurioRx *rx);

// ———————— 麦克风上行（复用 rx 的 socket，发给最近一个对端） ————————

typedef struct AurioMic AurioMic;

typedef struct {
    uint64_t sent_packets;
    uint64_t send_errors;
    uint64_t dropped_frames;   // 发送线程跟不上丢掉的帧
    uint64_t no_peer_frames;   // 还不知道对端地址时丢掉的帧
} AurioMicStats;

// test_tone 非 0 时改发 440 Hz 测试音（不需要麦克风权限，用来测链路）。
AurioMic *aurio_mic_create(const AurioRx *rx, double in_rate, uint32_t in_channels, uint32_t test_tone);
// 只能在音频输入回调里调用。data 为交错 float，frames * in_channels 个。
void aurio_mic_push(AurioMic *mic, const float *data, uint32_t frames);
void aurio_mic_get_stats(const AurioMic *mic, AurioMicStats *out);
// 上次调用以来麦克风的峰值（线性），取完清零。
float aurio_mic_take_peak(const AurioMic *mic);
// 必须先停掉音频输入回调再调用；rx 要在它之后销毁。
void aurio_mic_destroy(AurioMic *mic);

// ———————— 回声分析（测试工具用） ————————

typedef struct {
    float    mic_rms_db;
    float    mic_peak_db;
    float    ref_rms_db;
    float    delay_ms;      // 回声延迟
    float    correlation;   // 峰值相关系数 0..1
    float    echo_db;       // 回声相对参照的强度
    float    echo_share;    // 录音能量里回声的占比 0..1
    uint32_t verdict;       // 0 播放太小 1 录音静音 2 没有回声 3 轻微 4 明显
} AurioEchoResult;

// mic、reference：单声道、同采样率、同时开始，各 frames 帧。
void aurio_echo_analyze(const float *mic, const float *reference, uint64_t frames,
                        double rate, double max_delay_s, AurioEchoResult *out);

// 最近一次错误信息（UTF-8，线程局部），没有错误时返回空字符串。
const char *aurio_last_error(void);

const char *aurio_version(void);

#ifdef __cplusplus
}
#endif
