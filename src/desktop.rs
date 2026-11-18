//! Desktop integration for the dock: Windows taskbar pins, shell icons, Start / Task View /
//! show desktop, master volume and the wallpaper.

use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// How a pinned item recognises its running windows.
#[derive(Clone, Debug, PartialEq)]
pub enum Match {
    /// Executable file name without ".exe" (lowercase).
    Stem(String),
    /// Substring of the executable path (Store apps: "\\<package name>_").
    PathPart(String),
}

impl Match {
    pub fn hits(&self, exe: &str) -> bool {
        match self {
            Match::Stem(s) => {
                let stem = exe.rsplit(['\\', '/']).next().unwrap_or(exe).trim_end_matches(".exe");
                !s.is_empty() && stem == s
            }
            Match::PathPart(p) => exe.contains(p.as_str()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Pin {
    /// Stable id: the shortcut path or "aumid:<id>" (lowercase).
    pub key: String,
    pub name: String,
    /// What to open to launch the app.
    pub launch: String,
    /// Shell parsing name used for the icon.
    pub icon_src: String,
    pub matcher: Match,
    /// Comes from the Windows taskbar (vs. pinned only on the dock).
    pub from_windows: bool,
}

fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

fn pinned_dir() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_default();
    base.join("Microsoft\\Internet Explorer\\Quick Launch\\User Pinned\\TaskBar")
}

/// Raw bytes of the taskbar's pin list (changes whenever pins change).
pub fn taskband_blob() -> Vec<u8> {
    use windows::Win32::System::Registry::*;
    let key = HSTRING::from("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Taskband");
    let val = HSTRING::from("Favorites");
    unsafe {
        let mut size = 0u32;
        if RegGetValueW(HKEY_CURRENT_USER, &key, &val, RRF_RT_REG_BINARY, None, None, Some(&mut size)).is_err() {
            return vec![];
        }
        let mut buf = vec![0u8; size as usize];
        if RegGetValueW(HKEY_CURRENT_USER, &key, &val, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size))
            .is_err()
        {
            return vec![];
        }
        buf.truncate(size as usize);
        buf
    }
}

/// Printable ASCII runs (>= 5 chars) found in the blob as UTF-16 at either byte alignment,
/// in file order.
fn blob_strings(b: &[u8]) -> Vec<(usize, String)> {
    let mut out = vec![];
    for al in 0..2 {
        let mut cur = String::new();
        let mut start = al;
        let mut i = al;
        while i + 1 < b.len() {
            let u = u16::from_le_bytes([b[i], b[i + 1]]);
            if (0x20..0x7f).contains(&u) {
                if cur.is_empty() {
                    start = i;
                }
                cur.push(u as u8 as char);
            } else {
                if cur.len() >= 5 {
                    out.push((start, std::mem::take(&mut cur)));
                }
                cur.clear();
            }
            i += 2;
        }
        if cur.len() >= 5 {
            out.push((start, cur));
        }
    }
    out.sort_by_key(|x| x.0);
    out
}

fn is_aumid(s: &str) -> bool {
    let Some((fam, app)) = s.split_once('!') else { return false };
    let Some((name, hash)) = fam.rsplit_once('_') else { return false };
    !app.is_empty()
        && !name.is_empty()
        && hash.len() == 13
        && hash.chars().all(|c| c.is_ascii_alphanumeric())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || ".!_-".contains(c))
}

/// Target exe + arguments of a .lnk file.
fn lnk_target(path: &str) -> (String, String) {
    com_init();
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else {
            return Default::default();
        };
        let Ok(pf) = link.cast::<IPersistFile>() else { return Default::default() };
        if pf.Load(&HSTRING::from(path), STGM_READ).is_err() {
            return Default::default();
        }
        let mut buf = [0u16; 520];
        let _ = link.GetPath(&mut buf, std::ptr::null_mut(), 0);
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        let target = String::from_utf16_lossy(&buf[..n]);
        let mut abuf = [0u16; 1024];
        let _ = link.GetArguments(&mut abuf);
        let n = abuf.iter().position(|&c| c == 0).unwrap_or(0);
        (target, String::from_utf16_lossy(&abuf[..n]))
    }
}

fn display_name(parsing: &str) -> Option<String> {
    com_init();
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(parsing), None).ok()?;
        let p = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// The apps pinned to the Windows taskbar, in the taskbar's order.
pub fn taskbar_pins(blob: &[u8]) -> Vec<Pin> {
    let dir = pinned_dir();
    let files: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.to_lowercase().ends_with(".lnk")).collect())
        .unwrap_or_default();
    let mut order: Vec<String> = vec![]; // "lnk:<file>" or "aumid:<id>"
    for (_, s) in blob_strings(blob) {
        let tok = if s.to_lowercase().ends_with(".lnk") {
            // longest pinned file name that the string ends with (strings can carry a junk prefix)
            files.iter().filter(|f| s.ends_with(f.as_str())).max_by_key(|f| f.len()).map(|f| format!("lnk:{f}"))
        } else if is_aumid(&s) {
            Some(format!("aumid:{s}"))
        } else {
            None
        };
        if let Some(t) = tok {
            if !order.contains(&t) {
                order.push(t);
            }
        }
    }
    // shortcuts in the folder that the blob didn't mention go last
    for f in &files {
        let t = format!("lnk:{f}");
        if !order.contains(&t) {
            order.push(t);
        }
    }
    let mut out = vec![];
    for t in order {
        if let Some(f) = t.strip_prefix("lnk:") {
            let path = dir.join(f).to_string_lossy().to_string();
            let (target, args) = lnk_target(&path);
            let name = f.trim_end_matches(".lnk").trim_end_matches(".LNK").to_string();
            // Squirrel apps (Discord, …) launch "Update.exe --processStart App.exe"
            let stem = if let Some(i) = args.find("--processStart") {
                args[i + 14..].trim().trim_matches('"').split_whitespace().next().unwrap_or("").to_lowercase()
            } else {
                target.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase()
            };
            let mut stem = stem.trim_end_matches(".exe").to_string();
            if stem.is_empty() && name.to_lowercase().contains("explorer") {
                stem = "explorer".into();
            }
            out.push(Pin {
                key: path.to_lowercase(),
                name,
                launch: path.clone(),
                icon_src: path,
                matcher: Match::Stem(stem),
                from_windows: true,
            });
        } else if let Some(id) = t.strip_prefix("aumid:") {
            let parsing = format!("shell:AppsFolder\\{id}");
            let fam = id.split('!').next().unwrap_or(id);
            let pkg = fam.rsplit_once('_').map(|x| x.0).unwrap_or(fam).to_lowercase();
            let name = display_name(&parsing).unwrap_or_else(|| pkg.rsplit('.').next().unwrap_or(&pkg).to_string());
            out.push(Pin {
                key: format!("aumid:{}", id.to_lowercase()),
                name,
                launch: parsing.clone(),
                icon_src: parsing,
                matcher: Match::PathPart(format!("\\{pkg}_")),
                from_windows: true,
            });
        }
    }
    out
}

/// A dock-only pin for an executable.
pub fn exe_pin(exe: &str) -> Pin {
    let stem = exe.rsplit(['\\', '/']).next().unwrap_or(exe).trim_end_matches(".exe").to_lowercase();
    Pin {
        key: exe.to_lowercase(),
        name: stem.clone(),
        launch: exe.to_string(),
        icon_src: exe.to_string(),
        matcher: Match::Stem(stem),
        from_windows: false,
    }
}

/// 48px icon of any shell item (file, shortcut, "shell:AppsFolder\\<aumid>"), as RGBA.
pub fn shell_icon(parsing: &str) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::*;
    com_init();
    unsafe {
        let item: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(parsing), None).ok()?;
        let hbm = item.GetImage(SIZE { cx: 64, cy: 64 }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let mut bm = BITMAP::default();
        if GetObjectW(hbm.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            let _ = DeleteObject(hbm.into());
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
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
        let n = GetDIBits(dc, hbm, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        let _ = DeleteObject(hbm.into());
        if n == 0 {
            return None;
        }
        let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            if !has_alpha {
                p[3] = 255;
            } else if p[3] > 0 && p[3] < 255 {
                // premultiplied -> straight
                let a = p[3] as f32 / 255.0;
                for c in &mut p[..3] {
                    *c = ((*c as f32 / a).min(255.0)) as u8;
                }
            }
        }
        Some((w as u32, h as u32, px))
    }
}

/// Hide or show the Windows taskbar on every monitor. Windows shows it again by itself
/// when Explorer restarts, so callers re-apply this periodically.
pub fn set_windows_taskbar_visible(visible: bool) {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> windows::core::BOOL {
        unsafe {
            let mut cls = [0u16; 64];
            let n = GetClassNameW(hwnd, &mut cls);
            let name = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
            if name == "Shell_TrayWnd" || name == "Shell_SecondaryTrayWnd" {
                let show = lp.0 != 0;
                if IsWindowVisible(hwnd).as_bool() != show {
                    let _ = ShowWindow(hwnd, if show { SW_SHOWNA } else { SW_HIDE });
                }
            }
        }
        TRUE
    }
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(visible as isize));
    }
}

// ------------------------------------------------------------- control center

/// Wi-Fi / Bluetooth radio switches, read and set on a background thread
/// (the WinRT calls block).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Radios {
    /// None = this PC has no such radio (or access was denied).
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
}

pub enum RadioCmd {
    Set { wifi: bool, on: bool },
}

pub fn spawn_radios() -> (std::sync::Arc<parking_lot::Mutex<Radios>>, std::sync::mpsc::Sender<RadioCmd>) {
    use windows::Devices::Radios::{Radio, RadioKind, RadioState};
    let state = std::sync::Arc::new(parking_lot::Mutex::new(Radios::default()));
    let (tx, rx) = std::sync::mpsc::channel::<RadioCmd>();
    let st = state.clone();
    let _ = std::thread::Builder::new().name("radios".into()).spawn(move || {
        com_init();
        let _ = Radio::RequestAccessAsync().and_then(|op| op.join());
        loop {
            let radios: Vec<Radio> = Radio::GetRadiosAsync()
                .and_then(|op| op.join())
                .map(|list| list.into_iter().collect())
                .unwrap_or_default();
            let find = |k: RadioKind| radios.iter().find(|r| r.Kind().map(|x| x == k).unwrap_or(false)).cloned();
            let (wifi, bt) = (find(RadioKind::WiFi), find(RadioKind::Bluetooth));
            let on = |r: &Option<Radio>| r.as_ref().and_then(|r| r.State().ok()).map(|s| s == RadioState::On);
            *st.lock() = Radios { wifi: on(&wifi), bluetooth: on(&bt) };
            // wait for a command (or re-read every 2 s)
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(RadioCmd::Set { wifi: w, on: v }) => {
                    let target = if w { &wifi } else { &bt };
                    if let Some(r) = target {
                        let _ = r.SetStateAsync(if v { RadioState::On } else { RadioState::Off }).and_then(|op| op.join());
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
    });
    (state, tx)
}

/// Set master volume (0..1).
pub fn set_volume(v: f32) {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::*;
    com_init();
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return };
        let Ok(dev) = en.GetDefaultAudioEndpoint(eRender, eConsole) else { return };
        let Ok(vol) = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { return };
        let _ = vol.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null());
        if v > 0.0 {
            let _ = vol.SetMute(false, std::ptr::null());
        }
    }
}

pub fn toggle_mute() {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::*;
    com_init();
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return };
        let Ok(dev) = en.GetDefaultAudioEndpoint(eRender, eConsole) else { return };
        let Ok(vol) = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { return };
        let m = vol.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        let _ = vol.SetMute(!m, std::ptr::null());
    }
}

/// Set laptop screen brightness (0..1) in the background.
pub fn set_brightness(v: f32) {
    use std::os::windows::process::CommandExt;
    let pct = (v.clamp(0.0, 1.0) * 100.0).round() as u32;
    let _ = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods | Select-Object -First 1 | Invoke-CimMethod -MethodName WmiSetBrightness -Arguments @{{Timeout=1;Brightness={pct}}}) | Out-Null"),
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

/// Create (Some(exe)) or remove (None) the "Aurolo" shortcut in the user's Startup folder.
pub fn set_startup_shortcut(exe: Option<&std::path::Path>) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let lnk = std::path::PathBuf::from(appdata).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup\\Aurolo.lnk");
    let Some(exe) = exe else {
        let _ = std::fs::remove_file(&lnk);
        return;
    };
    com_init();
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { return };
        let _ = link.SetPath(&HSTRING::from(exe.to_string_lossy().as_ref()));
        if let Some(dir) = exe.parent() {
            let _ = link.SetWorkingDirectory(&HSTRING::from(dir.to_string_lossy().as_ref()));
        }
        let _ = link.SetDescription(&HSTRING::from("Aurolo"));
        if let Ok(pf) = link.cast::<IPersistFile>() {
            let _ = pf.Save(&HSTRING::from(lnk.to_string_lossy().as_ref()), true);
        }
    }
}

/// Open the Start menu (asks the taskbar, no simulated key presses).
pub fn open_start() {
    unsafe {
        if let Ok(tray) = FindWindowW(&HSTRING::from("Shell_TrayWnd"), PCWSTR::null()) {
            let _ = PostMessageW(Some(tray), WM_SYSCOMMAND, WPARAM(SC_TASKLIST as usize), LPARAM(0));
        }
    }
}

pub fn task_view() {
    crate::platform::open("shell:::{3080F90E-D7AD-11D9-BD98-0000947B0257}");
}

pub fn show_desktop() {
    crate::platform::open("shell:::{3080F90D-D7AD-11D9-BD98-0000947B0257}");
}

/// Change master volume by `delta` (0..1 scale), unmuting when turning it up.
pub fn nudge_volume(delta: f32) {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::*;
    com_init();
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return };
        let Ok(dev) = en.GetDefaultAudioEndpoint(eRender, eConsole) else { return };
        let Ok(vol) = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { return };
        let cur = vol.GetMasterVolumeLevelScalar().unwrap_or(0.5);
        let v = (cur + delta).clamp(0.0, 1.0);
        let _ = vol.SetMasterVolumeLevelScalar(v, std::ptr::null());
        if delta > 0.0 {
            let _ = vol.SetMute(false, std::ptr::null());
        }
    }
}

// ------------------------------------------------------------------ wallpapers

pub fn wallpaper_dir(cfg_dir: &str) -> std::path::PathBuf {
    if !cfg_dir.trim().is_empty() {
        return std::path::PathBuf::from(cfg_dir.trim());
    }
    let pics = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_default().join("Pictures");
    pics.join("Wallpapers")
}

/// Image files in the wallpaper folder (one level deep), sorted by name.
pub fn list_wallpapers(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn scan(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: u32) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth == 0 {
                    scan(&p, out, 1);
                }
                continue;
            }
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
            if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "jfif") {
                out.push(p);
            }
        }
    }
    let mut out = vec![];
    scan(dir, &mut out, 0);
    out.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
    out
}

/// Current desktop wallpaper path.
pub fn current_wallpaper() -> String {
    let mut buf = [0u16; 520];
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr() as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    String::from_utf16_lossy(&buf[..n])
}

/// Set the desktop wallpaper (all monitors, "Fill").
pub fn set_wallpaper(path: &std::path::Path) {
    com_init();
    let p = HSTRING::from(path.to_string_lossy().as_ref());
    unsafe {
        if let Ok(dw) = CoCreateInstance::<_, IDesktopWallpaper>(&DesktopWallpaper, None, CLSCTX_ALL) {
            let _ = dw.SetPosition(DWPOS_FILL);
            if dw.SetWallpaper(PCWSTR::null(), &p).is_ok() {
                return;
            }
        }
        let w = crate::platform::wide(&path.to_string_lossy());
        let _ = SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(w.as_ptr() as *mut _),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        );
    }
}

/// Windows sometimes shows a black desktop after sign-in (it couldn't read the wallpaper
/// yet, e.g. from a second drive). Apply the current wallpaper again so it repaints;
/// if that file is gone, use the first image in the wallpaper folder.
pub fn reapply_wallpaper(cfg_dir: &str) {
    let cur = current_wallpaper();
    let path = if !cur.is_empty() && std::path::Path::new(&cur).exists() {
        Some(std::path::PathBuf::from(cur))
    } else {
        list_wallpapers(&wallpaper_dir(cfg_dir)).into_iter().next()
    };
    let Some(p) = path else { return };
    let _ = std::thread::Builder::new().name("wallpaper-fix".into()).spawn(move || {
        let w = crate::platform::wide(&p.to_string_lossy());
        unsafe {
            let _ = SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, Some(w.as_ptr() as *mut _), SPIF_UPDATEINIFILE | SPIF_SENDCHANGE);
        }
        crate::gfx::log(&format!("wallpaper re-applied: {}", p.display()));
    });
}

/// Handle of the desktop window; it changes when Windows Explorer restarts.
pub fn desktop_window_id() -> isize {
    unsafe { FindWindowW(&HSTRING::from("Progman"), PCWSTR::null()).map(|h| h.0 as isize).unwrap_or(0) }
}

/// Small thumbnail (longest side `max`) of an image file, as RGBA.
/// Keep `shared.wall_tint` in step with the desktop wallpaper: every couple of seconds,
/// if the wallpaper (path or file) changed, sample the colour where the island sits.
pub fn spawn_wall_tint(shared: std::sync::Arc<crate::state::Shared>) {
    std::thread::spawn(move || {
        let mut last: Option<(String, std::time::SystemTime)> = None;
        loop {
            let path = current_wallpaper();
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let key = (path.clone(), mtime);
            if last.as_ref() != Some(&key) {
                last = Some(key);
                let tint = if path.is_empty() { None } else { wall_tint(std::path::Path::new(&path)) };
                crate::gfx::log(&format!("island tint from wallpaper {path}: {tint:?}"));
                *shared.wall_tint.write() = tint;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

/// Average colour of the wallpaper behind the island (top centre of the screen, with the
/// image cropped to fill the screen), darkened so white text on it stays readable.
fn wall_tint(path: &std::path::Path) -> Option<crate::gfx::Rgba> {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let img = image::open(path).ok()?.thumbnail(480, 480).to_rgb8();
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN) as f32, GetSystemMetrics(SM_CYSCREEN) as f32) };
    let screen_aspect = if sw > 0.0 && sh > 0.0 { sw / sh } else { 16.0 / 9.0 };
    // "fill": the image covers the screen, cropped evenly on the long side
    let (cw, ch) = if iw / ih > screen_aspect { (ih * screen_aspect, ih) } else { (iw, iw / screen_aspect) };
    let (ox, oy) = ((iw - cw) / 2.0, (ih - ch) / 2.0);
    let (x0, x1) = ((ox + cw * 0.36) as u32, (ox + cw * 0.64) as u32);
    let (y0, y1) = (oy as u32, (oy + ch * 0.10).max(oy + 2.0) as u32);
    let (mut sum, mut n) = ([0f64; 3], 0f64);
    for y in y0..y1.min(img.height()) {
        for x in x0..x1.min(img.width()) {
            let p = img.get_pixel(x, y).0;
            for c in 0..3 {
                // average in linear light so bright specks don't dominate
                sum[c] += (p[c] as f64 / 255.0).powf(2.2);
            }
            n += 1.0;
        }
    }
    if n == 0.0 {
        return None;
    }
    let mut lin = [sum[0] / n, sum[1] / n, sum[2] / n];
    // keep it a dark glass: cap brightness, keep the hue
    let lum = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
    let max_lum = 0.03;
    if lum > max_lum {
        let k = max_lum / lum;
        lin.iter_mut().for_each(|v| *v *= k);
    }
    let g = |v: f64| (v.min(1.0).powf(1.0 / 2.2)) as f32;
    Some([g(lin[0]), g(lin[1]), g(lin[2]), 1.0])
}

pub fn thumbnail(path: &std::path::Path, max: u32) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::open(path).ok()?;
    let t = img.thumbnail(max, max).to_rgba8();
    Some((t.width(), t.height(), t.into_raw()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn dump_pins() {
        for p in super::taskbar_pins(&super::taskband_blob()) {
            let icon = super::shell_icon(&p.icon_src).map(|i| (i.0, i.1));
            println!("{:<22} {:?} icon={:?}", p.name, p.matcher, icon);
        }
        println!("wallpaper: {}", super::current_wallpaper());
    }
}
