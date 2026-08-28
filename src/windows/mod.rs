pub mod explorer_sel;
pub mod shell_menu;

use eframe::egui;
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, WS_OVERLAPPEDWINDOW};

pub fn centered_outer_position(inner_size: egui::Vec2) -> Option<egui::Pos2> {
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut cursor) == 0 {
            return None;
        }
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        let dpi = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) == 0
            && dpi_x > 0
        {
            dpi_x
        } else {
            96
        };
        let scale = dpi as f32 / 96.0;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: (inner_size.x * scale).round() as i32,
            bottom: (inner_size.y * scale).round() as i32,
        };
        if AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, 0, 0, dpi) == 0 {
            return None;
        }
        let outer_w = rect.right - rect.left;
        let outer_h = rect.bottom - rect.top;
        let work = info.rcWork;
        let x = work.left + (work.right - work.left - outer_w) / 2;
        let y = work.top + (work.bottom - work.top - outer_h) / 2;
        let max_x = work.right.saturating_sub(outer_w).max(work.left);
        let max_y = work.bottom.saturating_sub(outer_h).max(work.top);
        Some(egui::pos2(
            x.clamp(work.left, max_x) as f32 / scale,
            y.clamp(work.top, max_y) as f32 / scale,
        ))
    }
}

pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    const CF_UNICODETEXT: u32 = 13;
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * 2;
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("OpenClipboard".to_owned());
        }
        let _ = EmptyClipboard();
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if handle.is_null() {
            CloseClipboard();
            return Err("GlobalAlloc".to_owned());
        }
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            CloseClipboard();
            return Err("GlobalLock".to_owned());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), pointer.cast::<u8>(), bytes);
        GlobalUnlock(handle);
        if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
            CloseClipboard();
            return Err("SetClipboardData".to_owned());
        }
        CloseClipboard();
    }
    Ok(())
}
