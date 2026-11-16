//! Wi-Fi and Bluetooth control for the dock's control center.
//!
//! * Wi-Fi uses the native WLAN API: scan, list, connect (saved, open or with a
//!   password), disconnect.
//! * Bluetooth lists paired devices (classic + LE) with their connection state. Audio
//!   devices (headphones, earbuds, speakers) can be connected / disconnected through
//!   their audio driver's one-shot reconnect / disconnect property, the same mechanism
//!   Windows uses for its own "Connect" button.
//!
//! Everything blocking runs on background threads; the UI reads shared state and sends
//! commands.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use windows::core::{GUID, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::NetworkManagement::WiFi::*;

// ------------------------------------------------------------------------- Wi-Fi

#[derive(Clone, Debug)]
pub struct WifiNet {
    pub ssid: String,
    /// Saved profile name, if Windows remembers this network.
    pub profile: Option<String>,
    /// 0..100
    pub signal: u32,
    pub secured: bool,
    pub connected: bool,
    /// WPA3 / enterprise etc.: we can't build a profile for these, Settings can.
    pub supported_new: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WifiState {
    pub networks: Vec<WifiNet>,
    /// SSID being connected to.
    pub connecting: Option<String>,
    pub error: Option<String>,
    pub scanned: bool,
}

pub enum WifiCmd {
    Scan,
    Connect { ssid: String, profile: Option<String>, password: Option<String>, secured: bool },
    Disconnect,
}

struct Wlan {
    h: HANDLE,
    iface: GUID,
}

impl Wlan {
    fn open() -> Option<Self> {
        unsafe {
            let mut ver = 0u32;
            let mut h = HANDLE::default();
            if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
                return None;
            }
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            if WlanEnumInterfaces(h, None, &mut list) != 0 || list.is_null() {
                WlanCloseHandle(h, None);
                return None;
            }
            let n = (*list).dwNumberOfItems;
            let iface = if n > 0 { Some((*list).InterfaceInfo[0].InterfaceGuid) } else { None };
            WlanFreeMemory(list as *const _);
            match iface {
                Some(iface) => Some(Wlan { h, iface }),
                None => {
                    WlanCloseHandle(h, None);
                    None
                }
            }
        }
    }

    fn scan(&self) {
        unsafe {
            let _ = WlanScan(self.h, &self.iface, None, None, None);
        }
    }

    fn networks(&self) -> Vec<WifiNet> {
        let mut out: Vec<WifiNet> = vec![];
        unsafe {
            let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
            if WlanGetAvailableNetworkList(self.h, &self.iface, 0, None, &mut list) != 0 || list.is_null() {
                return out;
            }
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).Network.as_ptr(), n);
            for it in items {
                let len = (it.dot11Ssid.uSSIDLength as usize).min(32);
                let ssid = String::from_utf8_lossy(&it.dot11Ssid.ucSSID[..len]).to_string();
                if ssid.is_empty() {
                    continue; // hidden network
                }
                let pn = it.strProfileName.iter().position(|&c| c == 0).unwrap_or(it.strProfileName.len());
                let profile = String::from_utf16_lossy(&it.strProfileName[..pn]);
                let connected = it.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0;
                let auth = it.dot11DefaultAuthAlgorithm;
                let supported_new = !it.bSecurityEnabled.as_bool()
                    || auth == DOT11_AUTH_ALGO_RSNA_PSK
                    || auth == DOT11_AUTH_ALGO_WPA_PSK;
                let net = WifiNet {
                    ssid: ssid.clone(),
                    profile: if profile.is_empty() { None } else { Some(profile) },
                    signal: it.wlanSignalQuality,
                    secured: it.bSecurityEnabled.as_bool(),
                    connected,
                    supported_new,
                };
                // the same network can be listed twice (with and without a profile)
                if let Some(e) = out.iter_mut().find(|e| e.ssid == ssid) {
                    e.connected |= net.connected;
                    e.signal = e.signal.max(net.signal);
                    if e.profile.is_none() {
                        e.profile = net.profile;
                    }
                } else {
                    out.push(net);
                }
            }
            WlanFreeMemory(list as *const _);
        }
        out.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)));
        out
    }

    fn connect_profile(&self, profile: &str) -> bool {
        let p = HSTRING::from(profile);
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(p.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_infrastructure,
            dwFlags: 0,
        };
        unsafe { WlanConnect(self.h, &self.iface, &params, None) == 0 }
    }

    /// Save a profile for a new network (open or WPA2/WPA personal), then connect.
    fn connect_new(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;");
        let hex: String = ssid.bytes().map(|b| format!("{b:02X}")).collect();
        let security = match password {
            Some(pw) => format!(
                "<authEncryption><authentication>WPA2PSK</authentication><encryption>AES</encryption><useOneX>false</useOneX></authEncryption>\
                 <sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{}</keyMaterial></sharedKey>",
                esc(pw)
            ),
            None => "<authEncryption><authentication>open</authentication><encryption>none</encryption><useOneX>false</useOneX></authEncryption>".into(),
        };
        let xml = format!(
            "<?xml version=\"1.0\"?><WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
             <name>{n}</name><SSIDConfig><SSID><hex>{hex}</hex><name>{n}</name></SSID></SSIDConfig>\
             <connectionType>ESS</connectionType><connectionMode>auto</connectionMode>\
             <MSM><security>{security}</security></MSM></WLANProfile>",
            n = esc(ssid)
        );
        let x = HSTRING::from(xml);
        let mut reason = 0u32;
        let r = unsafe { WlanSetProfile(self.h, &self.iface, 0, &x, PCWSTR::null(), true, None, &mut reason) };
        if r != 0 {
            return Err("Couldn't save this network".into());
        }
        if self.connect_profile(ssid) { Ok(()) } else { Err("Couldn't connect".into()) }
    }

    fn disconnect(&self) {
        unsafe {
            let _ = WlanDisconnect(self.h, &self.iface, None);
        }
    }

    fn delete_profile(&self, name: &str) {
        let n = HSTRING::from(name);
        unsafe {
            let _ = WlanDeleteProfile(self.h, &self.iface, &n, None);
        }
    }
}

impl Drop for Wlan {
    fn drop(&mut self) {
        unsafe {
            WlanCloseHandle(self.h, None);
        }
    }
}

pub fn spawn_wifi() -> (Arc<Mutex<WifiState>>, Sender<WifiCmd>) {
    let state = Arc::new(Mutex::new(WifiState::default()));
    let (tx, rx): (Sender<WifiCmd>, Receiver<WifiCmd>) = channel();
    let st = state.clone();
    let _ = std::thread::Builder::new().name("wifi".into()).spawn(move || {
        let Some(w) = Wlan::open() else {
            st.lock().error = Some("No Wi-Fi adapter".into());
            return;
        };
        // after a connect attempt, watch for it to succeed
        let mut pending: Option<(String, std::time::Instant, bool)> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(1500)) {
                Ok(WifiCmd::Scan) => {
                    w.scan();
                    std::thread::sleep(Duration::from_millis(2500));
                }
                Ok(WifiCmd::Connect { ssid, profile, password, secured }) => {
                    {
                        let mut s = st.lock();
                        s.connecting = Some(ssid.clone());
                        s.error = None;
                    }
                    let res = match (&profile, &password) {
                        (Some(p), None) => {
                            if w.connect_profile(p) { Ok(()) } else { Err("Couldn't connect".into()) }
                        }
                        _ => w.connect_new(&ssid, if secured { password.as_deref() } else { None }),
                    };
                    match res {
                        Ok(()) => pending = Some((ssid, std::time::Instant::now(), profile.is_none())),
                        Err(e) => {
                            let mut s = st.lock();
                            s.connecting = None;
                            s.error = Some(e);
                        }
                    }
                }
                Ok(WifiCmd::Disconnect) => w.disconnect(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let nets = w.networks();
            let mut s = st.lock();
            if let Some((ssid, t, new_profile)) = &pending {
                let ok = nets.iter().any(|n| &n.ssid == ssid && n.connected);
                if ok {
                    s.connecting = None;
                    pending = None;
                } else if t.elapsed() > Duration::from_secs(15) {
                    s.connecting = None;
                    s.error = Some("Couldn't connect. Check the password and try again".into());
                    // don't keep a profile with a wrong password around
                    if *new_profile {
                        w.delete_profile(ssid);
                    }
                    pending = None;
                }
            }
            s.networks = nets;
            s.scanned = true;
        }
    });
    (state, tx)
}

// --------------------------------------------------------------------- Bluetooth

#[derive(Clone, Debug)]
pub struct BtDev {
    pub name: String,
    pub connected: bool,
    /// Headphones / speakers: we can connect and disconnect these.
    pub audio: bool,
}

#[derive(Clone, Debug, Default)]
pub struct BtState {
    pub devices: Vec<BtDev>,
    pub busy: Option<String>,
    pub loaded: bool,
}

pub enum BtCmd {
    Refresh,
    Toggle { name: String, connect: bool },
}

/// {7FA06C40-B8F6-4C7E-8556-E8C33A12E54D}: Bluetooth audio driver property set.
const KSPROPSETID_BTAUDIO: GUID = GUID::from_u128(0x7fa06c40_b8f6_4c7e_8556_e8c33a12e54d);
const KSPROPERTY_ONESHOT_RECONNECT: u32 = 0;
const KSPROPERTY_ONESHOT_DISCONNECT: u32 = 1;

struct BtAudio {
    name: String,
    connected: bool,
    controls: Vec<windows::Win32::Media::KernelStreaming::IKsControl>,
}

/// Bluetooth audio devices found through their audio endpoints.
fn bt_audio_devices() -> Vec<BtAudio> {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_DeviceInterface_FriendlyName;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::Media::KernelStreaming::IKsControl;
    use windows::Win32::System::Com::*;
    let mut out: Vec<BtAudio> = vec![];
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return out };
        let Ok(col) = en.EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_UNPLUGGED.0)) else { return out };
        let n = col.GetCount().unwrap_or(0);
        for i in 0..n {
            let Ok(ep) = col.Item(i) else { continue };
            let state = ep.GetState().unwrap_or(DEVICE_STATE_UNPLUGGED);
            let r = (|| -> windows::core::Result<(String, IKsControl)> {
                let topo: IDeviceTopology = ep.Activate(CLSCTX_ALL, None)?;
                let conn = topo.GetConnector(0)?;
                let other = conn.GetConnectedTo()?;
                let part: IPart = windows::core::Interface::cast(&other)?;
                let topo2 = part.GetTopologyObject()?;
                let id = topo2.GetDeviceId()?;
                let id_s = id.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id.0 as *const _));
                if !id_s.to_lowercase().contains("bth") {
                    return Err(windows::core::Error::from(E_FAIL));
                }
                let dev = en.GetDevice(&HSTRING::from(id_s.as_str()))?;
                let ks: IKsControl = dev.Activate(CLSCTX_ALL, None)?;
                let store = ep.OpenPropertyStore(STGM_READ)?;
                let v = store.GetValue(&PKEY_DeviceInterface_FriendlyName)?;
                let name = v.to_string();
                Ok((name, ks))
            })();
            if let Ok((name, ks)) = r {
                if name.is_empty() {
                    continue;
                }
                let connected = state == DEVICE_STATE_ACTIVE;
                match out.iter_mut().find(|d| d.name == name) {
                    Some(d) => {
                        d.connected |= connected;
                        d.controls.push(ks);
                    }
                    None => out.push(BtAudio { name, connected, controls: vec![ks] }),
                }
            }
        }
    }
    out
}

fn bt_audio_set(dev: &BtAudio, connect: bool) {
    use windows::Win32::Media::KernelStreaming::{KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0, KSPROPERTY_TYPE_GET};
    let prop = KSIDENTIFIER {
        Anonymous: KSIDENTIFIER_0 {
            Anonymous: KSIDENTIFIER_0_0 {
                Set: KSPROPSETID_BTAUDIO,
                Id: if connect { KSPROPERTY_ONESHOT_RECONNECT } else { KSPROPERTY_ONESHOT_DISCONNECT },
                Flags: KSPROPERTY_TYPE_GET,
            },
        },
    };
    for ks in &dev.controls {
        let mut ret = 0u32;
        unsafe {
            let _ = ks.KsProperty(&prop, std::mem::size_of::<KSIDENTIFIER>() as u32, std::ptr::null_mut(), 0, &mut ret);
        }
    }
}

/// Paired Bluetooth devices (classic + LE) with their connected state. Reads the
/// devices' own nodes (fast) rather than querying the radio.
fn bt_paired() -> Vec<(String, bool)> {
    use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind};
    use windows::Foundation::IPropertyValue;
    use windows_collections::IIterable;
    // DEVPKEY_Device_IsConnected-style flag Windows keeps on Bluetooth device nodes
    const CONNECTED: &str = "{83DA6326-97A6-4088-9453-A1923F573B29} 15";
    const INSTANCE: &str = "System.Devices.DeviceInstanceId";
    let r = (|| -> windows::core::Result<Vec<(String, bool)>> {
        let props: IIterable<HSTRING> = vec![HSTRING::from(CONNECTED), HSTRING::from(INSTANCE)].into();
        let aqs = HSTRING::from("System.Devices.ClassGuid:=\"{e0cbf06c-cd8b-4647-bb8a-263b43f0f974}\"");
        let list = DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(&aqs, &props, DeviceInformationKind::Device)?.join()?;
        let mut out: Vec<(String, bool)> = vec![];
        for d in list {
            let map = d.Properties()?;
            let inst = map
                .Lookup(&HSTRING::from(INSTANCE))
                .ok()
                .and_then(|v| windows::core::Interface::cast::<IPropertyValue>(&v).ok())
                .and_then(|p| p.GetString().ok())
                .map(|s| s.to_string().to_uppercase())
                .unwrap_or_default();
            // the device itself, not its services / the radio
            let is_dev = (inst.starts_with(r"BTHENUM\DEV_") || inst.starts_with(r"BTHLE\DEV_")) && !inst.contains('{');
            if !is_dev {
                continue;
            }
            let name = d.Name()?.to_string();
            if name.is_empty() {
                continue;
            }
            let connected = map
                .Lookup(&HSTRING::from(CONNECTED))
                .ok()
                .and_then(|v| windows::core::Interface::cast::<IPropertyValue>(&v).ok())
                .and_then(|p| p.GetBoolean().ok())
                .unwrap_or(false);
            match out.iter_mut().find(|x| x.0 == name) {
                Some(x) => x.1 |= connected,
                None => out.push((name, connected)),
            }
        }
        Ok(out)
    })();
    r.unwrap_or_default()
}

pub fn spawn_bluetooth() -> (Arc<Mutex<BtState>>, Sender<BtCmd>) {
    let state = Arc::new(Mutex::new(BtState::default()));
    let (tx, rx): (Sender<BtCmd>, Receiver<BtCmd>) = channel();
    let st = state.clone();
    let _ = std::thread::Builder::new().name("bluetooth".into()).spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let mut active = false;
        loop {
            let cmd = rx.recv_timeout(Duration::from_secs(if active { 2 } else { 3600 }));
            let mut audio = bt_audio_devices();
            match cmd {
                Ok(BtCmd::Refresh) => active = true,
                Ok(BtCmd::Toggle { name, connect }) => {
                    st.lock().busy = Some(name.clone());
                    if let Some(d) = audio.iter().find(|d| d.name == name || name.contains(&d.name) || d.name.contains(&name)) {
                        bt_audio_set(d, connect);
                    }
                    // give the device a moment, then re-read
                    for _ in 0..10 {
                        std::thread::sleep(Duration::from_millis(600));
                        audio = bt_audio_devices();
                        let now = audio
                            .iter()
                            .find(|d| d.name == name || name.contains(&d.name) || d.name.contains(&name))
                            .map(|d| d.connected);
                        if now == Some(connect) {
                            break;
                        }
                    }
                    st.lock().busy = None;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let paired = bt_paired();
            let mut devs: Vec<BtDev> = vec![];
            for (name, connected) in paired {
                let a = audio.iter().find(|d| d.name == name || name.contains(&d.name) || d.name.contains(&name));
                devs.push(BtDev { connected: connected || a.map(|a| a.connected).unwrap_or(false), audio: a.is_some(), name });
            }
            // audio devices Windows lists under a different name than the paired entry
            for a in &audio {
                if !devs.iter().any(|d| d.name == a.name || d.name.contains(&a.name) || a.name.contains(&d.name)) {
                    devs.push(BtDev { name: a.name.clone(), connected: a.connected, audio: true });
                }
            }
            devs.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            let mut s = st.lock();
            s.devices = devs;
            s.loaded = true;
        }
    });
    (state, tx)
}

#[cfg(test)]
mod tests {
    #[test]
    fn dump_net() {
        let t = std::time::Instant::now();
        let w = super::Wlan::open().expect("wlan");
        w.scan();
        std::thread::sleep(std::time::Duration::from_secs(4));
        let nets = w.networks();
        println!("wifi count={} in {:?}", nets.len(), t.elapsed());
        for n in nets.iter().take(5) {
            println!("wifi {:<28} sig={:>3} sec={} saved={} conn={}", n.ssid, n.signal, n.secured, n.profile.is_some(), n.connected);
        }
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let t = std::time::Instant::now();
        let a = super::bt_audio_devices();
        println!("bt-audio {} in {:?}", a.len(), t.elapsed());
        let t = std::time::Instant::now();
        let p = super::bt_paired();
        println!("bt-paired {} in {:?}", p.len(), t.elapsed());
        for (n, c) in p {
            println!("bt-paired {n} connected={c}");
        }
    }
}
