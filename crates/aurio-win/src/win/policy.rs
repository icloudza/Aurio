//! 设置 Windows 默认音频设备。
//!
//! Windows 没有公开这个 API。这里用系统内部的 IPolicyConfig 接口：
//! Windows 7 起就有，SoundSwitch、EarTrumpet、AudioDeviceCmdlets 等工具都在用。

use super::common::Res;
use std::ffi::c_void;
use windows::core::{IUnknown, Interface, GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
/// 虚函数表里 SetDefaultEndpoint 的位置：IUnknown 3 个 + 前面 10 个方法
const SET_DEFAULT_ENDPOINT: usize = 13;
const RELEASE: usize = 2;

type SetDefaultEndpointFn = unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> HRESULT;
type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;

/// 把设备设为默认设备（控制台、多媒体、通讯三种用途都设）。device_id 来自 IMMDevice::GetId。
pub fn set_default_endpoint(device_id: &str) -> Res<()> {
    let _ = wasapi::initialize_mta();
    unsafe {
        let unk: IUnknown = CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;
        let mut pc: *mut c_void = std::ptr::null_mut();
        unk.query(&IID_POLICY_CONFIG, &mut pc).ok()?;
        let vtbl = *(pc as *const *const usize);
        let set: SetDefaultEndpointFn = std::mem::transmute(*vtbl.add(SET_DEFAULT_ENDPOINT));
        let release: ReleaseFn = std::mem::transmute(*vtbl.add(RELEASE));
        let id = HSTRING::from(device_id);
        let mut result: Res<()> = Ok(());
        for role in 0..3 {
            let hr = set(pc, PCWSTR(id.as_ptr()), role);
            if hr.is_err() {
                result = Err(format!("设置默认设备失败（{hr:?}）").into());
            }
        }
        release(pc);
        result
    }
}
