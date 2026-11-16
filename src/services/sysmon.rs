//! PC performance: CPU load + live clock, RAM, GPU (PDH counters), network, disk,
//! battery, and temperatures / fan RPM / power from LibreHardwareMonitor's web server.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sysinfo::{Disks, Networks, ProcessesToUpdate, System};
use windows::core::PCWSTR;
use windows::Win32::System::Performance::*;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

use crate::state::*;

pub fn spawn(shared: Arc<Shared>, lhm_url: String) {
    std::thread::Builder::new()
        .name("sysmon".into())
        .spawn(move || run(shared, lhm_url))
        .unwrap();
}

struct Pdh {
    query: PDH_HQUERY,
    gpu: Option<PDH_HCOUNTER>,
    perf: Option<PDH_HCOUNTER>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Pdh {
    fn new() -> Option<Self> {
        unsafe {
            let mut q = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut q) != 0 {
                return None;
            }
            let add = |path: &str| {
                let w = wide(path);
                let mut c = PDH_HCOUNTER::default();
                if PdhAddEnglishCounterW(q, PCWSTR(w.as_ptr()), 0, &mut c) == 0 { Some(c) } else { None }
            };
            let gpu = add("\\GPU Engine(*engtype_3D)\\Utilization Percentage");
            let perf = add("\\Processor Information(_Total)\\% Processor Performance");
            let _ = PdhCollectQueryData(q);
            Some(Pdh { query: q, gpu, perf })
        }
    }
    fn sample(&self) -> (Option<f32>, Option<f32>) {
        unsafe {
            let _ = PdhCollectQueryData(self.query);
            let gpu = self.gpu.and_then(|c| {
                let mut size = 0u32;
                let mut count = 0u32;
                let _ = PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, None);
                if size == 0 {
                    return None;
                }
                let mut buf = vec![0u8; size as usize];
                let r = PdhGetFormattedCounterArrayW(
                    c,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    Some(buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
                );
                if r != 0 {
                    return None;
                }
                let items = std::slice::from_raw_parts(buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W, count as usize);
                let total: f64 = items.iter().map(|i| i.FmtValue.Anonymous.doubleValue).sum();
                Some(total.clamp(0.0, 100.0) as f32)
            });
            let perf = self.perf.and_then(|c| {
                let mut v = PDH_FMT_COUNTERVALUE::default();
                if PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut v) == 0 {
                    Some(v.Anonymous.doubleValue as f32)
                } else {
                    None
                }
            });
            (gpu, perf)
        }
    }
}

fn battery() -> Option<Battery> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s).ok()? };
    if s.BatteryFlag & 128 != 0 || s.BatteryLifePercent == 255 {
        return None;
    }
    Some(Battery {
        percent: s.BatteryLifePercent as f32,
        charging: s.BatteryFlag & 8 != 0,
        on_ac: s.ACLineStatus == 1,
    })
}

fn run(shared: Arc<Shared>, lhm_url: String) {
    let mut sys = System::new().expect("sysinfo");
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let mut nets = Networks::new_with_refreshed_list().expect("networks");
    let mut disks = Disks::new_with_refreshed_list().expect("disks");
    let pdh = Pdh::new();
    let base_mhz = sys.cpus().first().map(|c| c.frequency()).unwrap_or(0) as f32;
    {
        let mut s = shared.sys.write();
        s.cpu_name = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().replace("(R)", "").replace("(TM)", "").replace(" CPU", ""))
            .unwrap_or_default();
        s.cores = sys.cpus().len();
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(800)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut lhm_next = Instant::now();
    let mut last = Instant::now();
    let mut tick = 0u64;
    let mut prev_batt: Option<Battery> = None;
    let mut low_warned = 100.0f32;
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        tick += 1;
        let dt = last.elapsed().as_secs_f64().max(0.1);
        last = Instant::now();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        nets.refresh(true);
        let (mut rx, mut tx) = (0u64, 0u64);
        for (_, d) in nets.iter() {
            rx += d.received();
            tx += d.transmitted();
        }
        if tick % 10 == 1 {
            disks.refresh(true);
        }
        let (gpu, perf) = pdh.as_ref().map(|p| p.sample()).unwrap_or((None, None));
        let top = if tick % 3 == 0 {
            sys.refresh_processes(ProcessesToUpdate::All, true);
            let mut v: Vec<(String, f32)> = vec![];
            for p in sys.processes().values() {
                let name = p.name().to_string_lossy().trim_end_matches(".exe").to_string();
                if name == "System Idle Process" || name == "Idle" {
                    continue;
                }
                if let Some(e) = v.iter_mut().find(|e| e.0 == name) {
                    e.1 += p.cpu_usage();
                } else {
                    v.push((name, p.cpu_usage()));
                }
            }
            v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            v.truncate(4);
            let n = sys.cpus().len().max(1) as f32;
            Some(v.into_iter().map(|(a, b)| (a, b / n)).collect::<Vec<_>>())
        } else {
            None
        };

        let lhm = if !lhm_url.is_empty() && Instant::now() >= lhm_next {
            let r = agent
                .get(&lhm_url)
                .call()
                .ok()
                .and_then(|mut r| r.body_mut().read_json::<serde_json::Value>().ok());
            if r.is_none() {
                lhm_next = Instant::now() + Duration::from_secs(15);
            } else {
                lhm_next = Instant::now() + Duration::from_millis(1500);
            }
            Some(r)
        } else {
            None
        };

        let batt = battery();
        if let (Some(b), Some(pb)) = (&batt, &prev_batt) {
            if b.on_ac != pb.on_ac || b.charging != pb.charging {
                shared.hud(Hud::Battery { percent: b.percent, charging: b.on_ac });
            } else if !b.on_ac && b.percent <= 20.0 && low_warned > 20.0
                || !b.on_ac && b.percent <= 10.0 && low_warned > 10.0
            {
                low_warned = b.percent;
                shared.hud(Hud::Battery { percent: b.percent, charging: false });
            }
            if b.on_ac {
                low_warned = 100.0;
            }
        }
        prev_batt = batt.clone();

        let mut s = shared.sys.write();
        s.cpu = sys.global_cpu_usage();
        if let Some(p) = perf {
            s.cpu_ghz = base_mhz * p / 100.0 / 1000.0;
        } else if s.cpu_ghz == 0.0 {
            s.cpu_ghz = base_mhz / 1000.0;
        }
        s.ram_used = sys.used_memory() as f64 / 1073741824.0;
        s.ram_total = sys.total_memory() as f64 / 1073741824.0;
        s.gpu = gpu.or(s.gpu);
        s.net_down = rx as f64 / dt;
        s.net_up = tx as f64 / dt;
        if let Some(d) = disks.iter().find(|d| d.mount_point().to_string_lossy().starts_with("C:")) {
            s.disk_total = d.total_space() as f64 / 1073741824.0;
            s.disk_used = (d.total_space() - d.available_space()) as f64 / 1073741824.0;
        }
        s.battery = batt;
        s.uptime = System::uptime().unwrap_or(0);
        let cpu = s.cpu;
        s.cpu_hist.push_back(cpu);
        while s.cpu_hist.len() > 60 {
            s.cpu_hist.pop_front();
        }
        let nd = (s.net_down / 1048576.0) as f32;
        s.net_hist.push_back(nd);
        while s.net_hist.len() > 60 {
            s.net_hist.pop_front();
        }
        if let Some(t) = top {
            s.top_procs = t;
        }
        if let Some(r) = lhm {
            match r {
                Some(v) => {
                    s.lhm_ok = true;
                    let mut acc = LhmAcc::default();
                    walk(&v, Hw::Other, &mut acc);
                    s.cpu_temp = acc.cpu_pkg.or(acc.cpu_max);
                    s.gpu_temp = acc.gpu_core.or(acc.gpu_any);
                    s.fans = acc.fans;
                    s.cpu_power = acc.cpu_power;
                    if s.gpu.is_none() {
                        s.gpu = acc.gpu_load;
                    }
                    if !acc.gpu_name.is_empty() {
                        s.gpu_name = acc.gpu_name;
                    }
                }
                None => s.lhm_ok = false,
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Hw {
    Cpu,
    Gpu,
    Other,
}

#[derive(Default)]
struct LhmAcc {
    cpu_pkg: Option<f32>,
    cpu_max: Option<f32>,
    gpu_core: Option<f32>,
    gpu_any: Option<f32>,
    gpu_load: Option<f32>,
    gpu_name: String,
    fans: Vec<(String, f32)>,
    cpu_power: Option<f32>,
}

fn num(v: &str) -> Option<f32> {
    let n: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',' || *c == '-').collect();
    n.replace(',', ".").parse().ok()
}

fn walk(v: &serde_json::Value, mut hw: Hw, acc: &mut LhmAcc) {
    let text = v.get("Text").and_then(|t| t.as_str()).unwrap_or("");
    let img = v.get("ImageURL").and_then(|t| t.as_str()).unwrap_or("").to_lowercase();
    if img.contains("cpu") {
        hw = Hw::Cpu;
    } else if img.contains("nvidia") || img.contains("ati") || img.contains("amd") || img.contains("gpu") {
        hw = Hw::Gpu;
        if acc.gpu_name.is_empty() {
            acc.gpu_name = text.to_string();
        }
    }
    let value = v.get("Value").and_then(|t| t.as_str()).unwrap_or("");
    let has_children = v.get("Children").and_then(|c| c.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
    if !has_children && !value.is_empty() {
        let n = num(value);
        let lt = text.to_lowercase();
        if value.contains("°C") {
            if let Some(n) = n {
                match hw {
                    Hw::Cpu => {
                        if lt.contains("package") || lt.contains("tctl") || lt.contains("tdie") {
                            acc.cpu_pkg = Some(n);
                        }
                        acc.cpu_max = Some(acc.cpu_max.map_or(n, |m: f32| m.max(n)));
                    }
                    Hw::Gpu => {
                        if lt.contains("core") && acc.gpu_core.is_none() {
                            acc.gpu_core = Some(n);
                        }
                        acc.gpu_any = Some(acc.gpu_any.map_or(n, |m: f32| m.max(n)));
                    }
                    Hw::Other => {}
                }
            }
        } else if value.contains("RPM") {
            if let Some(n) = n {
                let label = match hw {
                    Hw::Gpu => format!("GPU {text}"),
                    _ => text.to_string(),
                };
                acc.fans.push((label, n));
            }
        } else if value.ends_with(" W") && hw == Hw::Cpu && lt.contains("package") {
            acc.cpu_power = n;
        } else if value.ends_with('%') && hw == Hw::Gpu && lt == "gpu core" {
            acc.gpu_load = n;
        }
    }
    if let Some(ch) = v.get("Children").and_then(|c| c.as_array()) {
        for c in ch {
            walk(c, hw, acc);
        }
    }
}
