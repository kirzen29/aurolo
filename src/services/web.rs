//! Network-backed info: weather (Open-Meteo, no API key) and calendar (any iCal/ICS
//! link, e.g. Google Calendar's "secret address in iCal format").
//! Also mirrors Windows toast notifications when the OS grants listener access.

use std::sync::Arc;
use std::time::Duration;

use crate::platform;
use crate::state::*;

pub fn spawn(shared: Arc<Shared>, city: String, ics: String) {
    if !city.trim().is_empty() {
        let s = shared.clone();
        std::thread::Builder::new().name("weather".into()).spawn(move || weather_loop(s, city)).unwrap();
    }
    if !ics.trim().is_empty() {
        let s = shared.clone();
        std::thread::Builder::new().name("calendar".into()).spawn(move || calendar_loop(s, ics)).unwrap();
    }
    std::thread::Builder::new()
        .name("notifications".into())
        .spawn(move || {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            if let Err(e) = notif_loop(&shared) {
                crate::gfx::log(&format!("notification listener unavailable: {e:?}"));
                shared.misc.write().notif_access = Some(false);
            }
        })
        .unwrap();
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn weather_loop(shared: Arc<Shared>, city: String) {
    let a = agent();
    let mut coords: Option<(f64, f64, String)> = None;
    loop {
        if coords.is_none() {
            let url = format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=1", enc(city.trim()));
            if let Ok(mut r) = a.get(&url).call() {
                if let Ok(v) = r.body_mut().read_json::<serde_json::Value>() {
                    if let Some(x) = v["results"].get(0) {
                        coords = Some((
                            x["latitude"].as_f64().unwrap_or(0.0),
                            x["longitude"].as_f64().unwrap_or(0.0),
                            x["name"].as_str().unwrap_or(&city).to_string(),
                        ));
                    }
                }
            }
        }
        if let Some((lat, lon, name)) = &coords {
            let url = format!(
                "https://api.open-meteo.com/v1/forecast?latitude={lat:.3}&longitude={lon:.3}&current=temperature_2m,weather_code,is_day&daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1"
            );
            if let Ok(mut r) = a.get(&url).call() {
                if let Ok(v) = r.body_mut().read_json::<serde_json::Value>() {
                    let w = Weather {
                        city: name.clone(),
                        temp: v["current"]["temperature_2m"].as_f64().unwrap_or(0.0) as f32,
                        code: v["current"]["weather_code"].as_i64().unwrap_or(0) as i32,
                        is_day: v["current"]["is_day"].as_i64().unwrap_or(1) == 1,
                        hi: v["daily"]["temperature_2m_max"][0].as_f64().unwrap_or(0.0) as f32,
                        lo: v["daily"]["temperature_2m_min"][0].as_f64().unwrap_or(0.0) as f32,
                    };
                    shared.misc.write().weather = Some(w);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(if coords.is_some() { 900 } else { 120 }));
    }
}

pub fn weather_desc(code: i32) -> &'static str {
    match code {
        0 => "Clear",
        1 | 2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51..=57 => "Drizzle",
        61..=67 => "Rain",
        71..=77 => "Snow",
        80..=82 => "Showers",
        85 | 86 => "Snow showers",
        95..=99 => "Thunderstorm",
        _ => "—",
    }
}

fn calendar_loop(shared: Arc<Shared>, url: String) {
    let a = agent();
    let url = url.replacen("webcal://", "https://", 1);
    loop {
        if let Ok(mut r) = a.get(&url).call() {
            if let Ok(text) = r.body_mut().read_to_string() {
                let evs = parse_ics(&text);
                shared.misc.write().events = evs;
            }
        }
        std::thread::sleep(Duration::from_secs(600));
    }
}

/// Minimal ICS parser: upcoming VEVENTs in the next 7 days (no recurrence expansion).
fn parse_ics(text: &str) -> Vec<Event> {
    // unfold lines
    let mut lines: Vec<String> = vec![];
    for l in text.lines() {
        if (l.starts_with(' ') || l.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&l[1..]);
        } else {
            lines.push(l.to_string());
        }
    }
    let now = platform::unix_now();
    let off = platform::utc_offset_secs();
    let mut out = vec![];
    let mut cur: Option<(Option<i64>, String, bool)> = None;
    for l in lines {
        if l == "BEGIN:VEVENT" {
            cur = Some((None, String::new(), false));
        } else if l == "END:VEVENT" {
            if let Some((Some(t), title, all_day)) = cur.take() {
                if t >= now - 3600 && t <= now + 7 * 86400 {
                    out.push(Event { start: t, title, all_day });
                }
            }
        } else if let Some(c) = cur.as_mut() {
            if let Some((k, v)) = l.split_once(':') {
                let key = k.split(';').next().unwrap_or(k);
                match key {
                    "SUMMARY" => c.1 = v.replace("\\,", ",").replace("\\n", " "),
                    "DTSTART" => {
                        let all_day = k.contains("VALUE=DATE") && !v.contains('T');
                        c.2 = all_day;
                        c.0 = parse_ics_time(v, off, k.contains("TZID"));
                    }
                    _ => {}
                }
            }
        }
    }
    out.sort_by_key(|e| e.start);
    out.truncate(12);
    out
}

fn parse_ics_time(v: &str, local_off: i64, _tzid: bool) -> Option<i64> {
    let v = v.trim();
    let y: i64 = v.get(0..4)?.parse().ok()?;
    let m: i64 = v.get(4..6)?.parse().ok()?;
    let d: i64 = v.get(6..8)?.parse().ok()?;
    let (hh, mm, ss) = if v.len() >= 15 {
        (v.get(9..11)?.parse().ok()?, v.get(11..13)?.parse().ok()?, v.get(13..15)?.parse().ok()?)
    } else {
        (0i64, 0i64, 0i64)
    };
    let t = platform::days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss;
    // "Z" = UTC; floating/TZID times are treated as local time.
    if v.ends_with('Z') { Some(t) } else { Some(t - local_off) }
}

fn notif_loop(shared: &Arc<Shared>) -> windows::core::Result<()> {
    use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
    use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds};
    let listener = UserNotificationListener::Current()?;
    let status = listener.RequestAccessAsync()?.join()?;
    if status != UserNotificationListenerAccessStatus::Allowed {
        shared.misc.write().notif_access = Some(false);
        return Ok(());
    }
    shared.misc.write().notif_access = Some(true);
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut first = true;
    loop {
        if let Ok(list) = listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|o| o.join()) {
            for n in list {
                let Ok(id) = n.Id() else { continue };
                if !seen.insert(id) {
                    continue;
                }
                let app = n
                    .AppInfo()
                    .and_then(|a| a.DisplayInfo())
                    .and_then(|d| d.DisplayName())
                    .map(|h| h.to_string())
                    .unwrap_or_default();
                let mut texts = vec![];
                if let Ok(b) = n
                    .Notification()
                    .and_then(|x| x.Visual())
                    .and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric()?))
                {
                    if let Ok(els) = b.GetTextElements() {
                        for e in els {
                            if let Ok(t) = e.Text() {
                                texts.push(t.to_string());
                            }
                        }
                    }
                }
                let title = texts.first().cloned().unwrap_or_default();
                let body = texts.get(1..).map(|t| t.join(" ")).unwrap_or_default();
                let nf = Notif { app: app.clone(), title: title.clone(), body: body.clone(), at: std::time::Instant::now() };
                {
                    let mut m = shared.misc.write();
                    m.notifications.insert(0, nf);
                    m.notifications.truncate(30);
                }
                if !first {
                    shared.hud(Hud::Notification { app, title, body });
                }
            }
        }
        first = false;
        std::thread::sleep(Duration::from_millis(1500));
    }
}
