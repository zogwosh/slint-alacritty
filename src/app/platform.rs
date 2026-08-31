//! 原生窗口平台适配。

#[cfg(target_os = "windows")]
/// 请求 Windows 11 DWM 为无边框窗口绘制原生圆角。
pub(super) fn apply_native_window_rounding(window: &slint::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;

    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut c_void,
            attribute: u32,
            value: *const c_void,
            value_size: u32,
        ) -> i32;
    }

    let handle = window.window_handle();
    let Ok(handle) = handle.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let preference = DWMWCP_ROUND;
    let result = unsafe {
        DwmSetWindowAttribute(
            handle.hwnd.get() as *mut c_void,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&preference as *const i32).cast(),
            std::mem::size_of_val(&preference) as u32,
        )
    };
    if result < 0 {
        eprintln!("failed to enable native window rounding: HRESULT 0x{result:08x}");
    }
}

#[cfg(not(target_os = "windows"))]
pub(super) fn apply_native_window_rounding(_window: &slint::Window) {}
