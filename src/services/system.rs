//! Polling watchers: Wi-Fi, brightness, camera/mic in use, clipboard history,
//! downloads in progress, Bluetooth headset battery, and the launcher app index.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{Interface, BSTR, HSTRING, PCWSTR};
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::System::Wmi::*;

use crate::platform;
use crate::state::*;

pub fn spawn(shared: Arc<Shared>) {
    let s1 = shared.clone();
    std::thread::Builder::new()
        .name("system-watch".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            fast_loop(s1)
        })
        .unwrap();
    let s2 = shared.clone();
    std::thread::Builder::new()
        .name("bt-watch".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            bt_loop(s2)
        })
        .unwrap();
    std::thread::Builder::new()
        .name("app-index".into())
        .spawn(move || {
            let apps = index_apps();
            shared.misc.write().apps = apps;
        })
        .unwrap();
}

fn fast_loop(shared: Arc<Shared>) {
    let wmi = Brightness::new();
    let mut tick: u64 = 0;
    let mut last_wifi: Option<Option<String>> = None;
    let mut last_bright: Option<f32> = None;
    let mut clip_seq = platform::clipboard_seq();
    let mut dl: HashMap<PathBuf, (u64, Instant, f64)> = HashMap::new();
    loop {
        std::thread::sleep(Duration::from_millis(250));
        tick += 1;

        // brightness (every 250ms – laptop panels only)
        if let Some(w) = &wmi {
            if let Some(b) = w.get() {
                if let Some(pb) = last_bright {
                    if (pb - b).abs() > 0.5 {
                        shared.hud(Hud::Brightness(b / 100.0));
                    }
                }
                last_bright = Some(b);
                shared.misc.write().brightness = Some(b / 100.0);
            }
        }

        // clipboard
        let seq = platform::clipboard_seq();
        if seq != clip_seq {
            clip_seq = seq;
            if let Some(t) = platform::get_clipboard_text() {
                let t = t.trim().to_string();
                if !t.is_empty() && t.len() < 20000 {
                    let mut m = shared.misc.write();
                    m.clipboard.retain(|c| c != &t);
                    m.clipboard.insert(0, t);
                    m.clipboard.truncate(20);
                }
            }
        }

        if tick % 4 == 0 {
            // camera / microphone indicators
            let cam = capability_in_use("webcam");
            let mic = capability_in_use("microphone");
            let mut m = shared.misc.write();
            m.cam_in_use = cam;
            m.mic_in_use = mic;
        }

        if tick % 4 == 2 {
            // downloads in progress
            scan_downloads(&shared, &mut dl);
        }

        if tick % 8 == 0 {
            let wifi = wifi_name();
            if let Some(prev) = &last_wifi {
                if prev != &wifi {
                    shared.hud(Hud::Wifi(wifi.clone()));
                }
            }
            last_wifi = Some(wifi.clone());
            shared.misc.write().wifi = wifi;
        }
    }
}

fn wifi_name() -> Option<String> {
    use windows::Networking::Connectivity::NetworkInformation;
    let p = NetworkInformation::GetInternetConnectionProfile().ok()?;
    let name = p.ProfileName().ok()?.to_string();
    let wlan = p.IsWlanConnectionProfile().unwrap_or(false);
    Some(if wlan { name } else { format!("{name} (Ethernet)") })
}

struct Brightness {
    svc: IWbemServices,
}

impl Brightness {
    fn new() -> Option<Self> {
        unsafe {
            let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?;
            let svc = loc
                .ConnectServer(&BSTR::from("ROOT\\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)
                .ok()?;
            let _ = CoSetProxyBlanket(
                &svc,
                10, // RPC_C_AUTHN_WINNT
                0,  // RPC_C_AUTHZ_NONE
                None,
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            );
            let b = Brightness { svc };
            b.get()?;
            Some(b)
        }
    }
    fn get(&self) -> Option<f32> {
        unsafe {
            let e = self
                .svc
                .ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from("SELECT CurrentBrightness FROM WmiMonitorBrightness"),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
                .ok()?;
            let mut objs = [None];
            let mut n = 0u32;
            let _ = e.Next(1000, &mut objs, &mut n);
            let obj = objs[0].take()?;
            let mut v = VARIANT::default();
            let name = platform::wide("CurrentBrightness");
            obj.Get(PCWSTR(name.as_ptr()), 0, &mut v, None, None).ok()?;
            let b = v.Anonymous.Anonymous.Anonymous.bVal;
            Some(b as f32)
        }
    }
}

/// Returns the app name if any app currently uses the capability ("webcam"/"microphone").
fn capability_in_use(cap: &str) -> Option<String> {
    let base = format!("Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\{cap}");
    let mut found = None;
    let mut check = |path: &str, display: &str| {
        if let Some((start, stop)) = read_usage(path) {
            if start != 0 && stop == 0 {
                found = Some(display.to_string());
            }
        }
    };
    for sub in reg_subkeys(&base) {
        if sub == "NonPackaged" {
            let np = format!("{base}\\NonPackaged");
            for app in reg_subkeys(&np) {
                let disp = app.rsplit('#').next().unwrap_or(&app).trim_end_matches(".exe").to_string();
                check(&format!("{np}\\{app}"), &disp);
            }
        } else {
            let disp = sub.split('_').next().unwrap_or(&sub).rsplit('.').next().unwrap_or(&sub).to_string();
            check(&format!("{base}\\{sub}"), &disp);
        }
    }
    found
}

fn reg_subkeys(path: &str) -> Vec<String> {
    let mut out = vec![];
    unsafe {
        let mut key = HKEY::default();
        let p = platform::wide(path);
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), Some(0), KEY_READ, &mut key).is_err() {
            return out;
        }
        let mut i = 0;
        loop {
            let mut name = [0u16; 512];
            let mut len = name.len() as u32;
            let r = RegEnumKeyExW(key, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None);
            if r.is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
            i += 1;
        }
        let _ = RegCloseKey(key);
    }
    out
}

fn read_usage(path: &str) -> Option<(u64, u64)> {
    unsafe {
        let mut key = HKEY::default();
        let p = platform::wide(path);
        RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), Some(0), KEY_READ, &mut key).ok().ok()?;
        let q = |name: &str| -> Option<u64> {
            let n = platform::wide(name);
            let mut v = 0u64;
            let mut sz = 8u32;
            RegQueryValueExW(key, PCWSTR(n.as_ptr()), None, None, Some(&mut v as *mut u64 as *mut u8), Some(&mut sz))
                .ok()
                .ok()?;
            Some(v)
        };
        let r = (q("LastUsedTimeStart"), q("LastUsedTimeStop"));
        let _ = RegCloseKey(key);
        Some((r.0?, r.1?))
    }
}

fn downloads_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Downloads"))
}

fn scan_downloads(shared: &Shared, state: &mut HashMap<PathBuf, (u64, Instant, f64)>) {
    let Some(dir) = downloads_dir() else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let mut seen = vec![];
    for e in rd.flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
        if !matches!(ext.as_str(), "crdownload" | "part" | "partial" | "download" | "opdownload") {
            continue;
        }
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        let now = Instant::now();
        let entry = state.entry(p.clone()).or_insert((size, now, 0.0));
        let dt = now.duration_since(entry.1).as_secs_f64();
        if dt > 0.5 {
            let sp = (size.saturating_sub(entry.0)) as f64 / dt;
            entry.2 = entry.2 * 0.5 + sp * 0.5;
            entry.0 = size;
            entry.1 = now;
        }
        seen.push(p);
    }
    let gone: Vec<PathBuf> = state.keys().filter(|k| !seen.contains(k)).cloned().collect();
    for g in gone {
        state.remove(&g);
        let stem = g.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        // only announce if the finished file now exists
        let finished = dir.join(&stem);
        if finished.exists() {
            shared.hud(Hud::Download(stem));
        }
    }
    let list: Vec<Download> = state
        .iter()
        .map(|(p, (bytes, _, sp))| Download {
            name: p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string(),
            bytes: *bytes,
            speed: *sp,
        })
        .collect();
    shared.misc.write().downloads = list;
}

/// Bluetooth devices exposing a battery level (headphones, earbuds, controllers…).
fn bt_loop(shared: Arc<Shared>) {
    use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind};
    use windows_collections::IIterable;
    use windows::Foundation::IPropertyValue;
    const BATT: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";
    const CONNECTED: &str = "{83DA6326-97A6-4088-9453-A1923F573B29} 15";
    let mut known: HashMap<String, u8> = HashMap::new();
    let mut first = true;
    loop {
        let res = (|| -> windows::core::Result<Vec<BtDevice>> {
            let props: IIterable<HSTRING> = vec![HSTRING::from(BATT), HSTRING::from(CONNECTED)].into();
            let aqs = HSTRING::from("System.Devices.ClassGuid:=\"{e0cbf06c-cd8b-4647-bb8a-263b43f0f974}\" OR System.Devices.ClassGuid:=\"{4d36e96c-e325-11ce-bfc1-08002be10318}\"");
            let list =
                DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(&aqs, &props, DeviceInformationKind::Device)?
                    .join()?;
            let mut out = vec![];
            for d in list {
                let map = d.Properties()?;
                let key = HSTRING::from(BATT);
                if !map.HasKey(&key).unwrap_or(false) {
                    continue;
                }
                let Ok(v) = map.Lookup(&key) else { continue };
                let Ok(pv) = v.cast::<IPropertyValue>() else { continue };
                let Ok(b) = pv.GetUInt8() else { continue };
                let connected = map
                    .Lookup(&HSTRING::from(CONNECTED))
                    .ok()
                    .and_then(|v| v.cast::<IPropertyValue>().ok())
                    .and_then(|p| p.GetBoolean().ok())
                    .unwrap_or(true);
                if !connected {
                    continue;
                }
                let name = d.Name()?.to_string();
                if out.iter().any(|x: &BtDevice| x.name == name) {
                    continue;
                }
                out.push(BtDevice { name, battery: b });
            }
            Ok(out)
        })();
        if let Ok(devs) = res {
            for d in &devs {
                if !known.contains_key(&d.name) && !first {
                    shared.hud(Hud::Bluetooth { name: d.name.clone(), battery: Some(d.battery) });
                }
            }
            known = devs.iter().map(|d| (d.name.clone(), d.battery)).collect();
            shared.misc.write().bt_devices = devs;
            first = false;
        }
        std::thread::sleep(Duration::from_secs(6));
    }
}

fn index_apps() -> Vec<(String, String)> {
    let mut roots = vec![];
    if let Some(p) = std::env::var_os("ProgramData") {
        roots.push(PathBuf::from(p).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    if let Some(p) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(p).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    let mut out: Vec<(String, String)> = vec![];
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>, depth: u32) {
        if depth > 4 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out, depth + 1);
                continue;
            }
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
            if ext != "lnk" && ext != "url" {
                continue;
            }
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let low = name.to_lowercase();
            if low.contains("uninstall") || low.contains("readme") || low.contains("help") || low.contains("documentation") {
                continue;
            }
            if out.iter().any(|(n, _)| n == &name) {
                continue;
            }
            out.push((name, p.to_string_lossy().to_string()));
        }
    }
    for r in roots {
        walk(&r, &mut out, 0);
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out
}
