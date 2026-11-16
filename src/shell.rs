//! Shell helpers for the taskbar dock: open windows, app icons, and the appbar
//! (the screen strip Windows reserves so maximized windows stop above the dock).

use windows::core::{BOOL, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// A top-level window that Windows would show on the taskbar.
#[derive(Clone, Debug)]
pub struct TaskWin {
    pub hwnd: isize,
    pub title: String,
    /// Full path of the owning process' executable (lowercase), "" if unknown.
    pub exe: String,
}

fn exe_of_pid(pid: u32) -> String {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        if r.is_err() {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize]).to_lowercase()
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    let mut v: u32 = 0;
    unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut v as *mut u32 as *mut _, 4).is_ok() && v != 0 }
}

/// Windows that belong on a taskbar, in z-order (most recently used first).
pub fn task_windows() -> Vec<TaskWin> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let out = &mut *(lp.0 as *mut Vec<HWND>);
            out.push(hwnd);
        }
        TRUE
    }
    let mut all: Vec<HWND> = vec![];
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
    }
    let me = unsafe { windows::Win32::System::Threading::GetCurrentProcessId() };
    let mut out = vec![];
    for hwnd in all {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                continue;
            }
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let app = ex & WS_EX_APPWINDOW.0 != 0;
            if !app {
                if ex & WS_EX_TOOLWINDOW.0 != 0 || ex & WS_EX_NOACTIVATE.0 != 0 {
                    continue;
                }
                if GetWindow(hwnd, GW_OWNER).map(|o| !o.0.is_null()).unwrap_or(false) {
                    continue;
                }
            }
            if is_cloaked(hwnd) {
                continue;
            }
            let n = GetWindowTextLengthW(hwnd);
            if n <= 0 {
                continue;
            }
            let mut buf = vec![0u16; n as usize + 1];
            let got = GetWindowTextW(hwnd, &mut buf);
            let title = String::from_utf16_lossy(&buf[..got.max(0) as usize]);
            let mut cls = [0u16; 64];
            let cn = GetClassNameW(hwnd, &mut cls);
            let class = String::from_utf16_lossy(&cls[..cn.max(0) as usize]);
            if class == "Progman" || class == "WorkerW" || class == "Shell_TrayWnd" {
                continue;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == me {
                continue;
            }
            let mut exe = exe_of_pid(pid);
            // UWP apps live inside ApplicationFrameHost: use the hosted app's process
            if exe.ends_with("applicationframehost.exe") {
                if let Some(child) = uwp_child_exe(hwnd, pid) {
                    exe = child;
                }
            }
            out.push(TaskWin { hwnd: hwnd.0 as isize, title, exe });
        }
    }
    out
}

fn uwp_child_exe(host: HWND, host_pid: u32) -> Option<String> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let v = &mut *(lp.0 as *mut (u32, u32));
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != v.0 {
                v.1 = pid;
                return FALSE;
            }
        }
        TRUE
    }
    let mut v = (host_pid, 0u32);
    unsafe {
        let _ = EnumChildWindows(Some(host), Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    if v.1 == 0 { None } else { Some(exe_of_pid(v.1)).filter(|s| !s.is_empty()) }
}

pub fn foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// Bring a window to the front (restoring it if minimized).
pub fn activate_window(h: isize) {
    let hwnd = HWND(h as *mut _);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            SwitchToThisWindow(hwnd, true);
        }
    }
}

pub fn minimize_window(h: isize) {
    unsafe {
        let _ = ShowWindow(HWND(h as *mut _), SW_MINIMIZE);
    }
}

/// Is a Windows shell panel (quick settings, notification center, Start, search…) in
/// the foreground? `apps` are the normal app windows, which never count.
pub fn shell_panel_open(apps: &[TaskWin]) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() || apps.iter().any(|w| w.hwnd == fg.0 as isize) {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls);
        let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        let exe = exe_of_pid(pid);
        let stem = exe.rsplit('\\').next().unwrap_or("");
        matches!(
            stem,
            "explorer.exe" | "shellexperiencehost.exe" | "shellhost.exe" | "startmenuexperiencehost.exe" | "searchhost.exe"
        )
    }
}

/// Close an open shell panel the way the taskbar does: give focus back to an app (or the
/// desktop), and the panel dismisses itself.
pub fn dismiss_shell_panel(app: isize) {
    unsafe {
        let target = if app != 0 && IsWindow(Some(HWND(app as *mut _))).as_bool() {
            HWND(app as *mut _)
        } else {
            GetShellWindow()
        };
        if !SetForegroundWindow(target).as_bool() {
            SwitchToThisWindow(target, true);
        }
    }
}

/// Ask a window to close, as if its X button was clicked.
pub fn close_window(h: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}


/// Convert an HICON into straight RGBA pixels.
fn icon_to_rgba(hicon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::Graphics::Gdi::*;
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(hicon, &mut ii).ok()?;
        let cleanup = |ii: &ICONINFO| {
            if !ii.hbmColor.is_invalid() {
                let _ = DeleteObject(ii.hbmColor.into());
            }
            if !ii.hbmMask.is_invalid() {
                let _ = DeleteObject(ii.hbmMask.into());
            }
        };
        if ii.hbmColor.is_invalid() {
            cleanup(&ii);
            return None;
        }
        let mut bm = BITMAP::default();
        if GetObjectW(ii.hbmColor.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            cleanup(&ii);
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        if w <= 0 || h <= 0 || w > 512 || h > 512 {
            cleanup(&ii);
            return None;
        }
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let n = GetDIBits(dc, ii.hbmColor, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        cleanup(&ii);
        if n == 0 {
            return None;
        }
        let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            if !has_alpha {
                p[3] = 255;
            }
        }
        // the 48px system image list pads small icons; crop to the visible pixels
        Some(crop_to_content(w as u32, h as u32, px))
    }
}

fn crop_to_content(w: u32, h: u32, px: Vec<u8>) -> (u32, u32, Vec<u8>) {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            if px[((y * w + x) * 4 + 3) as usize] > 8 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return (w, h, px);
    }
    // keep it square and centred
    let s = (x1 - x0).max(y1 - y0);
    if s >= w.min(h) * 9 / 10 {
        return (w, h, px);
    }
    let cx = (x0 + x1) / 2;
    let cy = (y0 + y1) / 2;
    let sx = cx.saturating_sub(s / 2).min(w - s);
    let sy = cy.saturating_sub(s / 2).min(h - s);
    let mut out = vec![0u8; (s * s * 4) as usize];
    for y in 0..s {
        let src = (((sy + y) * w + sx) * 4) as usize;
        let dst = (y * s * 4) as usize;
        out[dst..dst + (s * 4) as usize].copy_from_slice(&px[src..src + (s * 4) as usize]);
    }
    (s, s, out)
}

/// Large (48px when available) icon of a file, as RGBA.
pub fn file_icon(path: &str) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
    use windows::Win32::UI::Shell::*;
    let wpath = HSTRING::from(path);
    unsafe {
        let mut sfi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            &wpath,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        );
        if r != 0 {
            if let Ok(il) = SHGetImageList::<IImageList>(SHIL_EXTRALARGE as i32) {
                if let Ok(hicon) = il.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0) {
                    let out = icon_to_rgba(hicon);
                    let _ = DestroyIcon(hicon);
                    if out.is_some() {
                        return out;
                    }
                }
            }
        }
        let mut sfi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            &wpath,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if r == 0 || sfi.hIcon.is_invalid() {
            return None;
        }
        let out = icon_to_rgba(sfi.hIcon);
        let _ = DestroyIcon(sfi.hIcon);
        out
    }
}

/// Icon a window shows in its title bar / taskbar button.
pub fn window_icon(h: isize) -> Option<(u32, u32, Vec<u8>)> {
    let hwnd = HWND(h as *mut _);
    unsafe {
        let mut res = 0usize;
        let _ = SendMessageTimeoutW(hwnd, WM_GETICON, WPARAM(ICON_BIG as usize), LPARAM(0), SMTO_ABORTIFHUNG, 60, Some(&mut res));
        let mut icon = res as isize;
        if icon == 0 {
            icon = GetClassLongPtrW(hwnd, GCLP_HICON) as isize;
        }
        if icon == 0 {
            return None;
        }
        icon_to_rgba(HICON(icon as *mut _))
    }
}

/// Monitor and work-area rectangles (physical px) for the monitor containing (x, y).
pub fn monitor_rects(x: i32, y: i32) -> (RECT, RECT) {
    use windows::Win32::Graphics::Gdi::*;
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        (mi.rcMonitor, mi.rcWork)
    }
}

const APPBAR_MSG: u32 = WM_APP + 0x51;

fn appbar_data(hwnd: HWND, rc: RECT) -> windows::Win32::UI::Shell::APPBARDATA {
    windows::Win32::UI::Shell::APPBARDATA {
        cbSize: std::mem::size_of::<windows::Win32::UI::Shell::APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: APPBAR_MSG,
        uEdge: windows::Win32::UI::Shell::ABE_BOTTOM,
        rc,
        lParam: LPARAM(0),
    }
}

/// Register `hwnd` as a bottom appbar so maximized windows stop above it.
pub fn appbar_register(hwnd: HWND) {
    use windows::Win32::UI::Shell::*;
    let mut abd = appbar_data(hwnd, RECT::default());
    unsafe {
        SHAppBarMessage(ABM_NEW, &mut abd);
    }
}

/// Ask the shell where a `height`-px bottom bar fits on monitor `mon` (stacking above
/// the Windows taskbar when that is visible). With `commit`, reserve that strip.
pub fn appbar_place(hwnd: HWND, mon: RECT, height: i32, commit: bool) -> RECT {
    use windows::Win32::UI::Shell::*;
    let rc = RECT { left: mon.left, right: mon.right, top: mon.bottom - height, bottom: mon.bottom };
    let mut abd = appbar_data(hwnd, rc);
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.top = abd.rc.bottom - height;
        if commit {
            SHAppBarMessage(ABM_SETPOS, &mut abd);
        }
    }
    abd.rc
}

pub fn appbar_remove(hwnd: HWND) {
    use windows::Win32::UI::Shell::*;
    let mut abd = appbar_data(hwnd, RECT::default());
    unsafe {
        SHAppBarMessage(ABM_REMOVE, &mut abd);
    }
}
