//! Thin Win32 helpers: shell open, clipboard, autostart, window styles, local time.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn open(target: &str) {
    let t = HSTRING::from(target);
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &t, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn open_with_args(exe: &str, args: &str) {
    unsafe {
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &HSTRING::from(exe),
            &HSTRING::from(args),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

pub fn reveal(path: &str) {
    open_with_args("explorer.exe", &format!("/select,\"{path}\""));
}

pub fn set_clipboard(text: &str) {
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        let w = wide(text);
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(13, Some(HANDLE(h.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

pub fn get_clipboard_text() -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(13).is_err() {
            return None;
        }
        OpenClipboard(None).ok()?;
        let r = (|| {
            let h = GetClipboardData(13).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        r
    }
}

pub fn clipboard_seq() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

pub fn set_autostart(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        let k = wide(RUN_KEY);
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), Some(0), KEY_SET_VALUE, &mut key).is_err() {
            return;
        }
        let name = wide("Aurolo");
        // drop the entry from the project's old name, if any
        let legacy = wide("Aura");
        let _ = RegDeleteValueW(key, PCWSTR(legacy.as_ptr()));
        if on {
            if let Ok(exe) = std::env::current_exe() {
                let v = wide(&format!("\"{}\"", exe.display()));
                let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
                let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(bytes));
                // a Startup-folder shortcut as a second route (only one copy ever runs)
                crate::desktop::set_startup_shortcut(Some(&exe));
            }
        } else {
            let _ = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
            crate::desktop::set_startup_shortcut(None);
        }
        let _ = RegCloseKey(key);
        // Recent Windows builds skip startup entries that have no "enabled" mark in the
        // startup-approval list (what Task Manager's Startup tab edits). Add one, but never
        // override the user turning Aurolo off there.
        approve_startup("Run", "Aurolo", on);
        approve_startup("StartupFolder", "Aurolo.lnk", on);
    }
}

/// Mark a startup entry as enabled in Explorer's StartupApproved list (if it has no mark
/// yet), or drop the mark when autostart is turned off.
fn approve_startup(list: &str, name: &str, on: bool) {
    let path = wide(&format!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\{list}"));
    let n = wide(name);
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .is_err()
        {
            return;
        }
        if on {
            let mut size = 0u32;
            let exists = RegQueryValueExW(key, PCWSTR(n.as_ptr()), None, None, None, Some(&mut size)).is_ok();
            if !exists {
                // 02 = enabled (03 would be "disabled"), then an empty timestamp
                let v: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                let _ = RegSetValueExW(key, PCWSTR(n.as_ptr()), Some(0), REG_BINARY, Some(&v));
            }
        } else {
            let _ = RegDeleteValueW(key, PCWSTR(n.as_ptr()));
        }
        let _ = RegCloseKey(key);
    }
}

pub fn hwnd_of(w: &winit::window::Window) -> HWND {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => HWND(h.hwnd.get() as *mut _),
        _ => HWND::default(),
    }
}

/// Make a window ignore the mouse (clicks go to whatever is underneath).
pub fn set_click_through(hwnd: HWND, through: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = if through {
            ex | (WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0) as isize
        } else {
            ex & !(WS_EX_TRANSPARENT.0 as isize)
        };
        if want != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
            if through {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            }
        }
    }
}

/// Tool window that never steals focus and stays out of Alt+Tab.
pub fn set_overlay_style(hwnd: HWND, no_activate: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let mut want = ex | WS_EX_TOOLWINDOW.0 as isize;
        want &= !(WS_EX_APPWINDOW.0 as isize);
        if no_activate {
            want |= WS_EX_NOACTIVATE.0 as isize;
        } else {
            want &= !(WS_EX_NOACTIVATE.0 as isize);
        }
        if want != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        }
    }
}

pub fn force_foreground(hwnd: HWND) {
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn keep_topmost(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

/// Mouse position in physical screen pixels.
pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// Is the mouse pointer visible? Games hide it, so this tells a game apart from a
/// full-screen video.
pub fn cursor_visible() -> bool {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    unsafe { GetCursorInfo(&mut ci).is_ok() && (ci.flags.0 & CURSOR_SHOWING.0) != 0 && !ci.hCursor.is_invalid() }
}

pub fn mouse_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

/// Is a fullscreen app (game, video) covering the foreground monitor?
pub fn foreground_is_fullscreen() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        // our own windows (e.g. fullscreen lyrics) never count as "another app is fullscreen"
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == windows::Win32::System::Threading::GetCurrentProcessId() {
            return false;
        }
        let shell = GetShellWindow();
        let desk = GetDesktopWindow();
        if fg == shell || fg == desk {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls);
        let name = String::from_utf16_lossy(&cls[..n as usize]);
        if name == "WorkerW" || name == "Progman" {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let mon = windows::Win32::Graphics::Gdi::MonitorFromWindow(fg, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST);
        let mut mi = windows::Win32::Graphics::Gdi::MONITORINFO {
            cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
            ..Default::default()
        };
        if !windows::Win32::Graphics::Gdi::GetMonitorInfoW(mon, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub weekday: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

pub fn local_time() -> LocalTime {
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    LocalTime {
        year: st.wYear,
        month: st.wMonth,
        day: st.wDay,
        weekday: st.wDayOfWeek,
        hour: st.wHour,
        minute: st.wMinute,
        second: st.wSecond,
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Seconds to add to UTC to get local time.
pub fn utc_offset_secs() -> i64 {
    let lt = local_time();
    let st = unsafe { windows::Win32::System::SystemInformation::GetSystemTime() };
    let l = days_from_civil(lt.year as i64, lt.month as i64, lt.day as i64) * 86400
        + lt.hour as i64 * 3600
        + lt.minute as i64 * 60;
    let u = days_from_civil(st.wYear as i64, st.wMonth as i64, st.wDay as i64) * 86400
        + st.wHour as i64 * 3600
        + st.wMinute as i64 * 60;
    let d = l - u;
    (d as f64 / 900.0).round() as i64 * 900
}

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

pub fn beep() {
    unsafe {
        let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONASTERISK);
    }
}

/// Prevent (or allow again) system sleep and display timeout. Must be called from the
/// long-lived UI thread: Windows ties the request to the calling thread.
pub fn keep_awake(on: bool) {
    use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};
    unsafe {
        if on {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
        } else {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

pub fn lock_pc() {
    unsafe {
        let _ = windows::Win32::System::Shutdown::LockWorkStation();
    }
}

pub fn sleep_pc() {
    open_with_args("rundll32.exe", "powrprof.dll,SetSuspendState 0,1,0");
}

pub fn restart_pc() {
    open_with_args("shutdown.exe", "/r /t 0");
}

pub fn shutdown_pc() {
    open_with_args("shutdown.exe", "/s /t 0");
}

/// Seconds since the last keyboard/mouse input.
pub fn idle_secs() -> u64 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut li = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe {
        if !GetLastInputInfo(&mut li).as_bool() {
            return 0;
        }
        let now = windows::Win32::System::SystemInformation::GetTickCount();
        (now.wrapping_sub(li.dwTime) / 1000) as u64
    }
}

/// Executable name (without .exe) of the foreground window's process.
pub fn foreground_app() -> Option<String> {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
        Some(name)
    }
}

pub fn today_key() -> String {
    let t = local_time();
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.day)
}
