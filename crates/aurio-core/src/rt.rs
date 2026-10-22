//! 线程优先级。网络收发线程被晚唤醒几毫秒，就等于凭空多了几毫秒抖动。

/// 把当前线程提到适合实时网络收发的优先级。失败时返回 false，不影响功能。
pub fn promote_current_thread() -> bool {
    imp::promote()
}

#[cfg(target_os = "macos")]
mod imp {
    const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    extern "C" {
        fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
    }
    pub fn promote() -> bool {
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) == 0 }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> *mut c_void;
        fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
    }
    pub fn promote() -> bool {
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) != 0 }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    pub fn promote() -> bool {
        false
    }
}
