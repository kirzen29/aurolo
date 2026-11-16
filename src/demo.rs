//! Demo mode for screenshots: `AUROLO_DEMO=1`.
//!
//! Replaces personal data (notifications, Wi-Fi name, app usage, reminders, clipboard)
//! with sample data, draws a wallpaper-style backdrop behind the overlays, and never
//! writes to the user's settings or reminders.
//!
//! Optional: `AUROLO_DEMO_LAYOUT=full|lyrics`, `AUROLO_DEMO_BG=glass|solid|none`,
//! `AUROLO_DEMO_FULLSCREEN=1`, `AUROLO_DEBUG_TAB=media|system|tools|daily|social|settings`.

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
    cfg.island_glass = false;
    cfg.lyrics_widget = true;
    cfg.widget_pos = None;
    cfg.widget_lyrics_only = std::env::var("AUROLO_DEMO_LAYOUT").map(|v| v == "lyrics").unwrap_or(false);
    cfg.widget_background = match std::env::var("AUROLO_DEMO_BG").unwrap_or_default().as_str() {
        "solid" => WidgetBg::Solid,
        "none" => WidgetBg::None,
        _ => WidgetBg::Glass,
    };
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
    m.weather = Some(Weather { city: "Kyoto".into(), temp: 18.0, hi: 21.0, lo: 12.0, code: 2, is_day: true });
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
