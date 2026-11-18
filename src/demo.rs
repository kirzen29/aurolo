//! Demo mode for screenshots: `AUROLO_DEMO=1`.
//!
//! Replaces personal data (notifications, Wi-Fi name, app usage, reminders, clipboard)
//! with sample data, draws a wallpaper-style backdrop behind the overlays, and never
//! writes to the user's settings or reminders.
//!
//! Optional: `AUROLO_DEMO_LAYOUT=full|lyrics`, `AUROLO_DEMO_BG=glass|solid|none`,
//! `AUROLO_DEMO_FULLSCREEN=1`, `AUROLO_DEMO_DOCK=1` (+ `AUROLO_DEMO_DOCK_DIAL=1` / `AUROLO_DEMO_DOCK_MENU=1`), `AUROLO_DEBUG_TAB=media|system|tools|daily|social|settings`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::config::{Config, WidgetBg};
use crate::gfx::*;
use crate::state::*;
use crate::ui::Frame;

static DEMO: AtomicBool = AtomicBool::new(false);

pub fn init() -> bool {
    let on = std::env::var("AUROLO_DEMO").map(|v| !v.is_empty() && v != "0").unwrap_or(false);
    DEMO.store(on, Ordering::Relaxed);
    on
}

pub fn is_demo() -> bool {
    DEMO.load(Ordering::Relaxed)
}

pub fn want_fullscreen() -> bool {
    is_demo() && std::env::var("AUROLO_DEMO_FULLSCREEN").is_ok()
}

pub fn apply_cfg(cfg: &mut Config) {
    cfg.island = true;
    cfg.lyrics_widget = true;
    // a demo runs next to the real Aurolo: leave the user's PC setup alone
    cfg.autostart = false;
    cfg.dock_reserve = false;
    cfg.hide_windows_taskbar = false;
    cfg.dock = std::env::var("AUROLO_DEMO_DOCK").is_ok();
    cfg.widget_pos = None;
    cfg.widget_lyrics_only = std::env::var("AUROLO_DEMO_LAYOUT").map(|v| v == "lyrics").unwrap_or(false);
    cfg.widget_background = match std::env::var("AUROLO_DEMO_BG").unwrap_or_default().as_str() {
        "solid" => WidgetBg::Solid,
        "none" => WidgetBg::None,
        _ => WidgetBg::Glass,
    };
    if let Ok(s) = std::env::var("AUROLO_DEMO_STYLE") {
        use crate::config::LyricStyle as L;
        let st = match s.as_str() {
            "karaoke" => Some(L::Karaoke),
            "constellation" => Some(L::Constellation),
            "drift" => Some(L::Drift),
            "banner" => Some(L::Banner),
            "pop" => Some(L::Pop),
            _ => None,
        };
        if let Some(st) = st {
            cfg.widget_style = st;
            cfg.fullscreen_style = st;
        }
    }
    cfg.widget_size_full = None;
    cfg.widget_size_lyrics = None;
    cfg.edge_glow = true;
    cfg.edge_only_when_playing = false;
    cfg.weather_city = String::new();
    cfg.calendar_ics_url = String::new();
}

pub fn seed(shared: &Shared) {
    let now = Instant::now();
    let unix = crate::platform::unix_now();
    let mut m = shared.misc.write();
    m.notifications = vec![
        Notif { app: "Discord".into(), title: "nova".into(), body: "are we still on for tonight? 🎧".into(), at: now },
        Notif { app: "Instagram".into(), title: "luna.wav".into(), body: "sent you a reel".into(), at: now },
        Notif { app: "WhatsApp".into(), title: "Studio group".into(), body: "Mix v3 is up, have a listen".into(), at: now },
        Notif { app: "Discord".into(), title: "kai".into(), body: "check the new playlist".into(), at: now },
    ];
    m.notif_access = Some(true);
    m.wifi = Some("Home Network".into());
    *shared.weather_city.write() = "Kyoto".into();
    m.weather = Some(Weather {
        city: "Kyoto".into(),
        temp: 18.0,
        hi: 21.0,
        lo: 12.0,
        code: 2,
        is_day: true,
        feels: 17.0,
        humidity: 62,
        wind: 9.0,
        sunrise: "06:21".into(),
        sunset: "16:51".into(),
        aqi: Some(38),
        hourly: (0..24)
            .map(|k| {
                let hr = (10 + k) % 24;
                let temp = 14.0 + 7.0 * (((hr as f32) - 8.0) / 24.0 * std::f32::consts::TAU).sin().max(-0.6);
                let code = if (15..19).contains(&hr) { 61 } else if hr % 5 == 0 { 3 } else { 1 };
                (hr, temp, code, if code == 61 { 70 } else { 10 }, (7..17).contains(&hr))
            })
            .collect(),
        daily: vec![(3, 21.0, 12.0, 2, 10), (4, 19.0, 11.0, 61, 80), (5, 17.0, 9.0, 3, 30), (6, 20.0, 10.0, 0, 0), (0, 22.0, 12.0, 1, 5)],
    });
    m.events = vec![
        Event { start: unix + 50 * 60, title: "Design review".into(), all_day: false },
        Event { start: unix + 5 * 3600, title: "Gym".into(), all_day: false },
        Event { start: unix + 26 * 3600, title: "Dinner with Sam".into(), all_day: false },
    ];
    m.clipboard = vec![
        "https://open.spotify.com/playlist/late-night-drive".into(),
        "Meeting moved to 4:30".into(),
        "#7c5cff".into(),
        "cargo build --release".into(),
    ];
    m.usage = vec![
        ("Spotify".into(), 2 * 3600 + 14 * 60),
        ("Code".into(), 1 * 3600 + 52 * 60),
        ("Chrome".into(), 1 * 3600 + 5 * 60),
        ("Discord".into(), 41 * 60),
        ("Figma".into(), 23 * 60),
    ];
    m.usage_total = m.usage.iter().map(|u| u.1).sum();
    m.bt_devices = vec![BtDevice { name: "Studio Buds".into(), battery: 82 }];
    m.volume = 0.6;
}

/// Wallpaper-style backdrop: deep gradient with slow colour nebulae.
pub fn draw_backdrop(f: &mut Frame) {
    let (w, h) = (f.w, f.h);
    f.scene.bloom = 0.0;
    f.scene.shape(Shape::rect(0.0, 0.0, w, h, 0.0, rgba(10, 8, 24, 1.0)).grad(rgba(6, 16, 30, 1.0), std::f32::consts::FRAC_PI_2));
    let blobs = [
        (0.18, 0.30, 0.55, rgba(124, 92, 255, 0.55)),
        (0.82, 0.22, 0.45, rgba(255, 120, 170, 0.38)),
        (0.70, 0.85, 0.60, rgba(40, 200, 220, 0.35)),
        (0.30, 0.90, 0.40, rgba(255, 170, 80, 0.25)),
    ];
    for (bx, by, r, c) in blobs {
        f.scene.shape(Shape::glow(w * bx, h * by, w.max(h) * r, c, 1.4).additive());
    }
    for i in 0..140u64 {
        let x = crate::anim::hash01(i * 31 + 7) * w;
        let y = crate::anim::hash01(i * 31 + 9) * h;
        let a = 0.15 + crate::anim::hash01(i * 31 + 11) * 0.5;
        f.scene.shape(Shape::glow(x, y, 1.6 + crate::anim::hash01(i) * 1.6, with_a(WHITE, a), 2.0).additive());
    }
}
