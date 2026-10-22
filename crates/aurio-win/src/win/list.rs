use super::common::Res;
use aurio_tools::Args;
use wasapi::*;

pub fn run(_a: &Args) -> Res<()> {
    let en = DeviceEnumerator::new()?;
    for dir in [Direction::Render, Direction::Capture] {
        let def_id = en.get_default_device(&dir).and_then(|d| d.get_id()).unwrap_or_default();
        println!("{}设备：", if dir == Direction::Render { "播放" } else { "录音" });
        for d in &en.get_device_collection(&dir)? {
            let d = d?;
            let name = d.get_friendlyname().unwrap_or_default();
            let mark = if d.get_id().unwrap_or_default() == def_id { "*" } else { " " };
            let mut line = format!("{mark} {name}");
            if let Ok(client) = d.get_iaudioclient() {
                if let Ok(mix) = client.get_mixformat() {
                    line += &format!(
                        "\n      混音格式 {} Hz / {} 声道 / {} 位",
                        mix.get_samplespersec(),
                        mix.get_nchannels(),
                        mix.get_validbitspersample()
                    );
                    if let Ok((def, min)) = client.get_device_period() {
                        line += &format!("，设备周期 默认 {:.2} ms 最小 {:.2} ms", def as f64 / 1e4, min as f64 / 1e4);
                    }
                    match client.get_shared_mode_engine_period(&mix) {
                        Ok(p) => {
                            line += &format!(
                                "\n      共享引擎周期 默认 {} 帧 最小 {} 帧（{:.2} ms）{}",
                                p.default,
                                p.min,
                                p.min as f64 * 1000.0 / mix.get_samplespersec() as f64,
                                if p.min < p.default { "← 支持低延迟共享模式" } else { "" }
                            )
                        }
                        Err(_) => line += "\n      不支持 IAudioClient3",
                    }
                }
            }
            println!("{line}");
        }
        println!();
    }
    println!("（* 为系统默认设备）");
    Ok(())
}
