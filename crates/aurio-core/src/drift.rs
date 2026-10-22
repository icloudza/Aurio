//! 漂移补偿：用 PI 控制器调节重采样比例，让缓冲的「最低水位」稳定在目标值。
//!
//! 为什么看最低水位而不是平均水位：发送端按采集周期（例如 10 ms）成批发包，
//! 水位本身是锯齿波。窗口内的最低点才反映离欠载还有多远，而且不受批大小影响。
//!
//! 控制量 `adj` 是相对修正：实际 step = 名义 step × (1 + adj)。
//! 水位偏高 → adj > 0 → 消耗得更快 → 水位下降。

#[derive(Clone, Copy, Debug)]
pub struct DriftConfig {
    /// 初始目标最低水位（毫秒）
    pub target_ms: f64,
    /// 自适应时目标的下限 / 上限（毫秒）
    pub min_target_ms: f64,
    pub max_target_ms: f64,
    /// 控制窗口长度（毫秒）
    pub window_ms: f64,
    /// 比例增益，单位 1/s：水位偏差按这个速率被拉回
    pub kp_hz: f64,
    /// 积分增益，单位 1/s²
    pub ki_hz2: f64,
    /// 修正量上限（ppm）
    pub max_ppm: f64,
    /// 每次欠载后目标水位增加多少（毫秒）
    pub underrun_bump_ms: f64,
    /// 抖动峰值的衰减半衰期（秒）。越长越保守。
    pub jitter_halflife_s: f64,
    /// 目标水位 = 下限 + 安全系数 × 抖动峰值
    pub jitter_safety: f64,
    /// 目标水位每秒最多下降多少（毫秒），上升不限速
    pub target_fall_ms_per_s: f64,
    /// 最低水位超过目标多少时，直接丢弃多余数据（毫秒）
    pub skip_excess_ms: f64,
}

impl Default for DriftConfig {
    fn default() -> Self {
        DriftConfig {
            target_ms: 2.0,
            min_target_ms: 1.0,
            max_target_ms: 40.0,
            window_ms: 100.0,
            kp_hz: 0.2,
            ki_hz2: 0.01,
            max_ppm: 500.0,
            underrun_bump_ms: 1.0,
            jitter_halflife_s: 600.0,
            jitter_safety: 1.5,
            target_fall_ms_per_s: 0.02,
            skip_excess_ms: 15.0,
        }
    }
}

pub struct DriftController {
    cfg: DriftConfig,
    in_rate: f64,
    out_rate: f64,
    target: f64,
    adj: f64,
    integ: f64,
    win_min: f64,
    win_elapsed: usize,
    win_len: usize,
    /// 窗口最低水位低于其自身慢速均值的最大偏离（输入帧），带慢速衰减。
    /// 不能拿目标值当基准：目标一升高水位跟不上，偏离变大，会正反馈冲到上限。
    jitter_peak: f64,
    /// 窗口最低水位的慢速均值（时间常数 2 秒）
    win_min_avg: Option<f64>,
    /// 重新开始播放或丢帧后，忽略接下来的一个窗口（数据还在过渡）
    settle: bool,
    last_win_min: f64,
}

impl DriftController {
    pub fn new(cfg: DriftConfig, in_rate: f64, out_rate: f64) -> Self {
        DriftController {
            cfg,
            in_rate,
            out_rate,
            target: cfg.target_ms * in_rate / 1000.0,
            adj: 0.0,
            integ: 0.0,
            win_min: f64::INFINITY,
            win_elapsed: 0,
            win_len: (cfg.window_ms * out_rate / 1000.0).max(1.0) as usize,
            jitter_peak: 0.0,
            win_min_avg: None,
            settle: true,
            last_win_min: 0.0,
        }
    }

    pub fn adj(&self) -> f64 {
        self.adj
    }

    pub fn ppm(&self) -> f64 {
        self.adj * 1e6
    }

    pub fn target_frames(&self) -> f64 {
        self.target
    }

    pub fn last_window_min(&self) -> f64 {
        self.last_win_min
    }

    /// 播放重新开始（缓冲完成）时调用。保留积分项：时钟偏差是两台机器的固有属性。
    pub fn restart(&mut self) {
        self.win_min = f64::INFINITY;
        self.win_elapsed = 0;
        self.settle = true;
        self.win_min_avg = None;
    }

    pub fn jitter_peak_frames(&self) -> f64 {
        self.jitter_peak
    }

    /// 网络侧观测到的迟到量（输入帧）：包到达最大间隔减去采集周期。
    /// 比等水位掉下来再反应更早，能在欠载发生前就把目标抬上去。
    pub fn note_lateness(&mut self, frames: f64) {
        if frames > self.jitter_peak {
            self.jitter_peak = frames;
            let ms = self.in_rate / 1000.0;
            let desired = (self.cfg.min_target_ms * ms + self.cfg.jitter_safety * self.jitter_peak)
                .min(self.cfg.max_target_ms * ms);
            self.target = self.target.max(desired);
        }
    }

    pub fn on_underrun(&mut self) {
        let max = self.cfg.max_target_ms * self.in_rate / 1000.0;
        let bump = self.cfg.underrun_bump_ms * self.in_rate / 1000.0;
        self.jitter_peak += bump;
        self.target = (self.target + bump).min(max);
    }

    fn update_target(&mut self, win_min: f64, dt: f64) {
        let ms = self.in_rate / 1000.0;
        let avg = self.win_min_avg.unwrap_or(win_min);
        let excursion = (avg - win_min).max(0.0);
        self.win_min_avg = Some(avg + (win_min - avg) * (dt / 2.0).min(1.0));
        let decay = 0.5f64.powf(dt / self.cfg.jitter_halflife_s);
        self.jitter_peak = (self.jitter_peak * decay).max(excursion);
        let desired = (self.cfg.min_target_ms * ms + self.cfg.jitter_safety * self.jitter_peak)
            .min(self.cfg.max_target_ms * ms);
        if desired > self.target {
            self.target = desired;
        } else {
            let fall = self.cfg.target_fall_ms_per_s * ms * dt;
            self.target = (self.target - fall).max(desired);
        }
    }

    /// 每次渲染回调结束后调用。`fill` 是当前水位（输入帧），`out_frames` 是本次输出的帧数。
    /// 返回 Some(n) 表示建议丢掉 n 个输入帧。
    pub fn observe(&mut self, fill: f64, out_frames: usize) -> Option<usize> {
        self.win_min = self.win_min.min(fill);
        self.win_elapsed += out_frames;
        if self.win_elapsed < self.win_len {
            return None;
        }
        let dt = self.win_elapsed as f64 / self.out_rate;
        let win_min = self.win_min;
        self.win_min = f64::INFINITY;
        self.win_elapsed = 0;
        self.last_win_min = win_min;

        if self.settle {
            self.settle = false;
            return None;
        }
        self.update_target(win_min, dt);

        let err = win_min - self.target;
        let skip_excess = self.cfg.skip_excess_ms * self.in_rate / 1000.0;
        if err > skip_excess {
            self.settle = true;
            return Some(err as usize);
        }

        let kp = self.cfg.kp_hz / self.in_rate;
        let ki = self.cfg.ki_hz2 / self.in_rate;
        let max = self.cfg.max_ppm * 1e-6;
        let p = kp * err;
        let candidate = p + ki * (self.integ + err * dt);
        // 条件积分：输出没饱和、或者误差在把输出往回拉时才累积，防止积分饱和
        if candidate.abs() < max || candidate.signum() != err.signum() {
            self.integ += err * dt;
        }
        let integ_max = max / ki;
        self.integ = self.integ.clamp(-integ_max, integ_max);
        self.adj = (p + ki * self.integ).clamp(-max, max);
        None
    }
}
