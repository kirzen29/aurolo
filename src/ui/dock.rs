//! Taskbar dock: a floating glass bar along the bottom of the screen.
//!
//! * left   – search orb (opens the island launcher), pinned + open apps with live
//!            indicators; the focused app expands to show its window title
//! * centre – now playing (art, title, equalizer, controls) or today's next event
//! * right  – focus timer, CPU / RAM rings, weather, network / volume / battery, clock
//!
//! Click an app to switch to it (again to minimize, or to cycle its windows),
//! right-click to pin / unpin, hover for its window title.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::anim::*;
use crate::gfx::*;
use crate::platform;
use crate::shell::{self, TaskWin};
use crate::state::*;
use crate::ui::{ic, Frame, Input};

/// Height of the bar strip (logical px) that is reserved at the bottom of the screen.
pub const DOCK_H: f32 = 62.0;
/// Transparent, click-through space above the bar for hover tooltips.
pub const TIP_H: f32 = 40.0;
const BAR_H: f32 = 46.0;
const SLOT: f32 = 42.0;
const ICON: f32 = 26.0;

pub enum DockAction {
    None,
    Launcher,
    Lyrics,
    TogglePin(String),
}

struct Slot {
    exe: String,
    name: String,
    wins: Vec<TaskWin>,

}

type Px = Arc<(u32, u32, Vec<u8>)>;

pub struct Dock {
    wins: Vec<TaskWin>,
    last_scan: Option<Instant>,
    /// Last foreground window that isn't one of ours.
    fg: isize,
    icons: HashMap<String, Option<Px>>,
    hover: HashMap<String, f32>,
    hover_since: Option<(String, f32)>,
    expand: HashMap<String, f32>,
    timer_end: Option<Instant>,
    timer_total: f32,
    pub hits: Vec<[f32; 4]>,
    /// Anything on the bar under the mouse or animating (app renders faster).
    pub busy: bool,
}

fn key_of(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)) ^ 0xd0c4_0000_0000_0000
}

fn app_name(exe: &str, fallback: &str) -> String {
    let stem = exe.rsplit(['\\', '/']).next().unwrap_or(exe).trim_end_matches(".exe");
    let known = [
        ("explorer", "File Explorer"),
        ("msedge", "Microsoft Edge"),
        ("chrome", "Google Chrome"),
        ("firefox", "Firefox"),
        ("code", "VS Code"),
        ("windowsterminal", "Terminal"),
        ("wt", "Terminal"),
        ("spotify", "Spotify"),
        ("discord", "Discord"),
        ("whatsapp", "WhatsApp"),
        ("notepad", "Notepad"),
        ("claude", "Claude"),
        ("taskmgr", "Task Manager"),
        ("systemsettings", "Settings"),
        ("winword", "Word"),
        ("excel", "Excel"),
        ("powerpnt", "PowerPoint"),
        ("steam", "Steam"),
        ("obs64", "OBS Studio"),
    ];
    for (k, n) in known {
        if stem == k {
            return n.to_string();
        }
    }
    if stem.is_empty() {
        return fallback.to_string();
    }
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => fallback.to_string(),
    }
}

impl Dock {
    pub fn new() -> Self {
        Dock {
            wins: vec![],
            last_scan: None,
            fg: 0,
            icons: HashMap::new(),
            hover: HashMap::new(),
            hover_since: None,
            expand: HashMap::new(),
            timer_end: None,
            timer_total: 0.0,
            hits: vec![],
            busy: false,
        }
    }

    fn scan(&mut self, force: bool) {
        let due = self.last_scan.map(|t| t.elapsed() > Duration::from_millis(400)).unwrap_or(true);
        if !due && !force {
            return;
        }
        self.last_scan = Some(Instant::now());
        self.wins = shell::task_windows();
        let fg = shell::foreground_hwnd();
        if self.wins.iter().any(|w| w.hwnd == fg) {
            self.fg = fg;
        } else if !self.wins.iter().any(|w| w.hwnd == self.fg) {
            self.fg = 0;
        }
    }

    fn slots(&self, pins: &[String]) -> Vec<Slot> {
        let mut out: Vec<Slot> = pins
            .iter()
            .map(|p| {
                let exe = p.to_lowercase();
                Slot { name: app_name(&exe, "App"), exe, wins: vec![] }
            })
            .collect();
        // open apps, in order of first appearance (stable while you work)
        let mut order: Vec<&TaskWin> = self.wins.iter().collect();
        order.sort_by_key(|w| w.hwnd);
        for w in order {
            let key = if w.exe.is_empty() { format!("hwnd:{}", w.hwnd) } else { w.exe.clone() };
            if let Some(s) = out.iter_mut().find(|s| s.exe == key) {
                s.wins.push(w.clone());
            } else {
                out.push(Slot { name: app_name(&w.exe, &w.title), exe: key, wins: vec![w.clone()] });
            }
        }
        // inside each app, most recently used window first
        for s in &mut out {
            s.wins.sort_by_key(|w| self.wins.iter().position(|x| x.hwnd == w.hwnd).unwrap_or(usize::MAX));
        }
        out
    }

    fn icon_for(&mut self, f: &mut Frame, slot: &Slot) -> Option<u64> {
        let k = key_of(&slot.exe);
        if !self.icons.contains_key(&slot.exe) {
            let px = if !slot.exe.starts_with("hwnd:") { shell::file_icon(&slot.exe) } else { None }
                .or_else(|| slot.wins.first().and_then(|w| shell::window_icon(w.hwnd)));
            self.icons.insert(slot.exe.clone(), px.map(Arc::new));
        }
        let px = self.icons.get(&slot.exe)?.as_ref()?.clone();
        if !f.painter.has_image(k) {
            f.painter.upload_image(f.gpu, k, px.0, px.1, &px.2);
        }
        f.painter.touch_image(k);
        Some(k)
    }

    fn timer_remaining(&self) -> Option<f32> {
        self.timer_end.map(|e| e.saturating_duration_since(Instant::now()).as_secs_f32())
    }

    pub fn draw(&mut self, f: &mut Frame, input: &Input, shared: &Shared, pins: &[String], focus_mins: u32) -> DockAction {
        self.scan(false);
        self.hits.clear();
        self.busy = false;
        let media = shared.media.read().clone();
        let audio = shared.audio.lock().clone();
        let sys = shared.sys.read().clone();
        let (wifi, volume, muted, weather, notifs, next_event) = {
            let m = shared.misc.read();
            let now = platform::unix_now();
            let ev = m.events.iter().filter(|e| e.start > now - 1800).min_by_key(|e| e.start).cloned();
            (m.wifi.clone(), m.volume, m.muted, m.weather.clone(), m.notifications.len(), ev)
        };
        let pal = if media.active { media.palette() } else { Palette::default() };
        let mut action = DockAction::None;

        // ---- focus timer
        if let Some(rem) = self.timer_remaining() {
            if rem <= 0.0 {
                self.timer_end = None;
                shared.hud(Hud::TimerDone("Focus session complete".into()));
                platform::beep();
            }
        }

        let y = TIP_H + (DOCK_H - BAR_H) / 2.0;
        let cy = y + BAR_H / 2.0;
        let margin = 10.0;
        let mut tip: Option<(String, f32)> = None;

        // ================= left: orb + apps =================
        let slots = self.slots(pins);
        let fg_exe = slots.iter().find(|s| s.wins.iter().any(|w| w.hwnd == self.fg)).map(|s| s.exe.clone());
        // widths (the focused app grows to show its title)
        let mut widths = vec![];
        let mut titles = vec![];
        for s in &slots {
            let target = if Some(&s.exe) == fg_exe.as_ref() { 1.0 } else { 0.0 };
            let e = self.expand.entry(s.exe.clone()).or_insert(target);
            *e = approach(*e, target, 9.0, f.dt);
            if (*e - target).abs() > 0.01 {
                self.busy = true;
            }
            let ev = *e;
            let title = s.wins.iter().find(|w| w.hwnd == self.fg).or(s.wins.first()).map(|w| w.title.clone()).unwrap_or_default();
            let tt = f.fit(Text::new(&title, 0.0, 0.0, 12.0, WHITE).weight(600), 150.0);
            let tw = if title.is_empty() { 0.0 } else { f.measure(&tt).0 + 10.0 };
            widths.push(SLOT + tw * ease_out_cubic(ev));
            titles.push((tt, ev));
        }
        let orb_w = 46.0;
        let left_w = 6.0 + orb_w + 10.0 + widths.iter().sum::<f32>() + 6.0;
        let left = [margin, y, left_w, BAR_H];
        glass(f, left, &pal);
        self.hits.push(left);

        // search orb
        let ocx = left[0] + 6.0 + orb_w / 2.0;
        let orb = [ocx - 17.0, cy - 17.0, 34.0, 34.0];
        let ohov = input.over(orb);
        f.scene.shape(Shape::glow(ocx, cy, 22.0, with_a(pal.primary, if ohov { 0.55 } else { 0.32 }), 1.6));
        f.scene.shape(Shape::rect(orb[0], orb[1], 34.0, 34.0, 17.0, pal.primary).conic(pal.secondary, 0.6));
        f.scene.shape(Shape::rect(orb[0] + 1.5, orb[1] + 1.5, 31.0, 31.0, 15.5, [0.04, 0.03, 0.08, if ohov { 0.35 } else { 0.55 }]));
        f.icon(ic::SEARCH, ocx, cy, 14.0, WHITE);
        if input.clicked(orb) {
            action = DockAction::Launcher;
        }
        if ohov {
            tip = Some(("Search apps, web & math".into(), ocx));
        }
        // divider
        let dx = left[0] + 6.0 + orb_w + 5.0;
        f.scene.shape(Shape::rect(dx, cy - 12.0, 1.0, 24.0, 0.5, with_a(WHITE, 0.14)));

        let mut x = dx + 5.0;
        for (i, s) in slots.iter().enumerate() {
            let sw = widths[i];
            let r = [x, y + 3.0, sw, BAR_H - 6.0];
            let hov = input.over(r);
            let h = self.hover.entry(s.exe.clone()).or_insert(0.0);
            *h = approach(*h, if hov { 1.0 } else { 0.0 }, 14.0, f.dt);
            let hv = *h;
            if hv > 0.01 && hv < 0.99 {
                self.busy = true;
            }
            let (tt, ev) = &titles[i];
            let focused = Some(&s.exe) == fg_exe.as_ref();
            let running = !s.wins.is_empty();
            // background: focused app gets a tinted capsule
            if *ev > 0.01 {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 13.0, with_a(pal.primary, 0.20 * ev)).grad(with_a(pal.secondary, 0.10 * ev), 0.0));
                f.scene.shape(Shape::stroke(r[0], r[1], r[2], r[3], 13.0, 1.0, with_a(WHITE, 0.16 * ev)));
            }
            if hv > 0.01 {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 13.0, with_a(WHITE, 0.09 * hv)));
            }
            // icon (slight lift on hover)
            let icx = x + SLOT / 2.0;
            let icy = cy - 2.0 - 2.0 * hv;
            let sz = ICON * (1.0 + 0.10 * hv);
            match self.icon_for(f, s) {
                Some(k) => {
                    let op = if running || hv > 0.5 { 1.0 } else { 0.78 };
                    f.scene.image(ImageDraw::new(k, icx - sz / 2.0, icy - sz / 2.0, sz, sz, 5.0).opacity(op));
                }
                None => {
                    let (c0, c1) = crate::ui::app_colors(&s.name);
                    f.scene.shape(Shape::rect(icx - sz / 2.0, icy - sz / 2.0, sz, sz, 8.0, c0).grad(c1, 0.8));
                    let ini = s.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                    f.scene.text(Text::new(ini, icx, icy - sz * 0.36, sz * 0.55, WHITE).center().weight(700));
                }
            }
            // title of the focused window
            if *ev > 0.05 && !tt.s.is_empty() {
                let a = smooth(0.35, 1.0, *ev);
                f.scene.text(Text { x: x + SLOT - 2.0, y: cy - 8.5, color: with_a(WHITE, 0.92 * a), clip: Some([x, y, x + sw - 6.0, y + BAR_H]), ..tt.clone() });
            }
            // running indicator
            let iy = y + BAR_H - 6.0;
            if focused {
                f.scene.glow_shape(Shape::rect(icx - 8.0, iy, 16.0, 3.0, 1.5, pal.primary).grad(pal.secondary, 0.0));
            } else if running {
                let n = s.wins.len().min(3);
                for k in 0..n {
                    let ox = (k as f32 - (n as f32 - 1.0) / 2.0) * 6.0;
                    f.scene.shape(Shape::rect(icx + ox - 2.0, iy, 4.0, 3.0, 1.5, with_a(WHITE, 0.55)));
                }
            }
            // tooltip after a short hover (not for the focused one: its title is on show)
            if hov {
                let t = f.t;
                match &self.hover_since {
                    Some((e, _)) if *e == s.exe => {}
                    _ => self.hover_since = Some((s.exe.clone(), t)),
                }
                let since = self.hover_since.as_ref().map(|h| t - h.1).unwrap_or(0.0);
                if since > 0.35 && !focused {
                    let label = match s.wins.len() {
                        0 => format!("{}  ·  click to open", s.name),
                        1 => s.wins[0].title.clone(),
                        n => format!("{}  ·  {n} windows", s.name),
                    };
                    tip = Some((label, icx));
                } else if since <= 0.35 {
                    self.busy = true;
                }
            }
            // clicks
            if input.clicked(r) {
                if s.wins.is_empty() {
                    if !s.exe.starts_with("hwnd:") {
                        platform::open(&s.exe);
                    }
                } else if focused && s.wins.len() == 1 {
                    shell::minimize_window(s.wins[0].hwnd);
                } else if focused {
                    // windows are MRU-sorted: index 1 is the one used before this,
                    // so repeated clicks cycle through the app's windows
                    shell::activate_window(s.wins[1].hwnd);
                } else {
                    shell::activate_window(s.wins[0].hwnd);
                }
                self.last_scan = None;
            }
            if hov && input.right_pressed && !s.exe.starts_with("hwnd:") {
                action = DockAction::TogglePin(s.exe.clone());
            }
            x += sw;
        }

        // ================= right: tools + status + clock =================
        let lt = platform::local_time();
        let time_t = Text::new(format!("{}:{:02}", lt.hour, lt.minute), 0.0, 0.0, 15.0, WHITE).weight(650);
        let date_s = format!(
            "{}, {} {}",
            &platform::WEEKDAYS[lt.weekday as usize % 7][..3],
            lt.day,
            platform::MONTHS[(lt.month as usize).saturating_sub(1) % 12]
        );
        let date_t = Text::new(date_s, 0.0, 0.0, 10.5, with_a(WHITE, 0.6)).weight(500);
        let clock_w = f.measure(&time_t).0.max(f.measure(&date_t).0) + 28.0;
        let timer_rem = self.timer_remaining();
        let timer_w = if timer_rem.is_some() { 92.0 } else { 40.0 };
        let stats_w = 84.0;
        let weather_w = if weather.is_some() { 62.0 } else { 0.0 };
        let status_w = if sys.battery.is_some() { 118.0 } else { 78.0 };
        let right_w = 8.0 + timer_w + stats_w + weather_w + status_w + clock_w + 4.0;
        let right = [f.w - margin - right_w, y, right_w, BAR_H];
        glass(f, right, &pal);
        self.hits.push(right);
        let mut rx = right[0] + 8.0;

        // focus timer
        {
            let r = [rx, y + 5.0, timer_w - 6.0, BAR_H - 10.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 12.0, with_a(WHITE, 0.09)));
            }
            let tcx = rx + 17.0;
            if let Some(rem) = timer_rem {
                let p = rem / self.timer_total.max(1.0);
                f.scene.shape(Shape::arc(tcx, cy, 11.0, 2.4, 1.0, with_a(WHITE, 0.12)));
                f.scene.glow_shape(Shape::arc(tcx, cy, 11.0, 2.4, p, rgba(255, 120, 90, 1.0)).conic(rgba(255, 190, 90, 1.0), 0.0));
                f.icon(ic::TIMER, tcx, cy, 9.5, with_a(WHITE, 0.85));
                f.scene.text(Text::new(fmt_time(rem as f64), rx + 33.0, cy - 9.0, 13.0, WHITE).weight(600).family(Family::Mono));
                if hov {
                    tip = Some(("Stop focus timer".into(), tcx));
                }
            } else {
                f.icon(ic::TIMER, tcx, cy, 14.0, with_a(WHITE, if hov { 1.0 } else { 0.78 }));
                if hov {
                    tip = Some((format!("Start a {focus_mins}-minute focus session"), tcx));
                }
            }
            if input.clicked(r) {
                if self.timer_end.is_some() {
                    self.timer_end = None;
                } else {
                    let secs = focus_mins.max(1) as f32 * 60.0;
                    self.timer_end = Some(Instant::now() + Duration::from_secs_f32(secs));
                    self.timer_total = secs;
                }
            }
            rx += timer_w;
        }
        sep(f, rx - 3.0, cy);

        // CPU / RAM rings — click for Task Manager
        {
            let r = [rx, y + 5.0, stats_w - 6.0, BAR_H - 10.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 12.0, with_a(WHITE, 0.09)));
            }
            let cpu = (sys.cpu / 100.0).clamp(0.0, 1.0);
            let ram = if sys.ram_total > 0.0 { (sys.ram_used / sys.ram_total) as f32 } else { 0.0 };
            let ring = |f: &mut Frame, cx: f32, v: f32, label: &str, c0: Rgba, c1: Rgba| {
                f.scene.shape(Shape::arc(cx, cy, 13.0, 2.6, 1.0, with_a(WHITE, 0.12)));
                let hot = if v > 0.85 { rgba(255, 90, 90, 1.0) } else { c1 };
                f.scene.glow_shape(Shape::arc(cx, cy, 13.0, 2.6, v.max(0.02), c0).conic(hot, 0.0));
                f.scene.text(Text::new(format!("{:.0}", v * 100.0), cx, cy - 8.0, 10.0, WHITE).center().weight(650));
                f.scene.text(Text::new(label, cx, cy + 0.5, 6.5, with_a(WHITE, 0.55)).center().weight(700));
            };
            ring(f, rx + 19.0, cpu, "CPU", rgba(90, 200, 255, 1.0), rgba(140, 120, 255, 1.0));
            ring(f, rx + 55.0, ram, "RAM", rgba(120, 255, 190, 1.0), rgba(90, 200, 255, 1.0));
            if hov {
                let t = format!(
                    "CPU {:.0}%{}  ·  RAM {:.1} / {:.0} GB  ·  ↓ {}",
                    sys.cpu,
                    sys.cpu_temp.map(|t| format!(" {t:.0}°C")).unwrap_or_default(),
                    sys.ram_used,
                    sys.ram_total,
                    crate::ui::fmt_bytes_rate(sys.net_down)
                );
                tip = Some((t, rx + 37.0));
            }
            if input.clicked(r) {
                platform::open("taskmgr.exe");
            }
            rx += stats_w;
        }
        sep(f, rx - 3.0, cy);

        // weather
        if let Some(wt) = &weather {
            weather_glyph(f, rx + 15.0, cy - 1.0, 8.5, wt.code, wt.is_day);
            f.scene.text(Text::new(format!("{:.0}°", wt.temp), rx + 29.0, cy - 9.0, 13.5, WHITE).weight(600));
            let r = [rx, y, weather_w, BAR_H];
            if input.over(r) {
                tip = Some((format!("{}  ·  H {:.0}°  L {:.0}°", wt.city, wt.hi, wt.lo), rx + 28.0));
            }
            rx += weather_w;
            sep(f, rx - 3.0, cy);
        }

        // network / volume / battery — click for quick settings
        {
            let r = [rx, y + 5.0, status_w - 6.0, BAR_H - 10.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 12.0, with_a(WHITE, 0.09)));
            }
            let col = with_a(WHITE, 0.88);
            let wg = if wifi.is_some() { ic::WIFI } else { ic::WIFI_OFF };
            f.icon(wg, rx + 18.0, cy, 13.0, col);
            let vg = if muted || volume <= 0.001 { ic::MUTE } else if volume < 0.4 { ic::VOLUME1 } else { ic::VOLUME };
            f.icon(vg, rx + 45.0, cy, 13.0, col);
            if let Some(b) = &sys.battery {
                let frac = (b.percent / 100.0).clamp(0.0, 1.0);
                let bx = rx + 64.0;
                let bc = if b.charging || b.on_ac {
                    rgba(120, 255, 160, 1.0)
                } else if frac < 0.2 {
                    rgba(255, 90, 90, 1.0)
                } else {
                    WHITE
                };
                f.scene.shape(Shape::stroke(bx, cy - 5.5, 20.0, 11.0, 3.5, 1.2, with_a(WHITE, 0.6)));
                f.scene.shape(Shape::rect(bx + 20.5, cy - 2.0, 2.0, 4.0, 1.0, with_a(WHITE, 0.6)));
                f.scene.shape(Shape::rect(bx + 2.0, cy - 3.5, (16.0 * frac).max(1.5), 7.0, 2.0, bc));
                f.scene.text(Text::new(format!("{:.0}", b.percent), bx + 26.0, cy - 7.5, 10.5, with_a(WHITE, 0.8)).weight(600));
            }
            if hov {
                let mut t = format!("{}  ·  Volume {:.0}%", wifi.clone().unwrap_or_else(|| "Offline".into()), volume * 100.0);
                if let Some(b) = &sys.battery {
                    t += &format!("  ·  Battery {:.0}%{}", b.percent, if b.charging { " charging" } else { "" });
                }
                tip = Some((t, rx + status_w / 2.0));
            }
            if input.clicked(r) {
                platform::open("ms-actioncenter:controlcenter/");
            }
            rx += status_w;
        }
        sep(f, rx - 3.0, cy);

        // clock + notification badge — click for notifications & calendar
        {
            let r = [rx, y + 5.0, clock_w - 4.0, BAR_H - 10.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 12.0, with_a(WHITE, 0.09)));
            }
            let tx = rx + clock_w / 2.0 - 2.0;
            f.scene.text(Text { x: tx, y: cy - 17.0, ..time_t }.center());
            f.scene.text(Text { x: tx, y: cy + 2.0, ..date_t }.center());
            if notifs > 0 {
                let bx = r[0] + r[2] - 7.0;
                f.scene.glow_shape(Shape::glow(bx, y + 11.0, 4.0, pal.primary, 0.4));
                f.scene.shape(Shape::glow(bx, y + 11.0, 2.6, WHITE, 0.2));
            }
            if hov {
                let n = if notifs == 1 { "1 notification".to_string() } else { format!("{notifs} notifications") };
                tip = Some((format!("{}  ·  {n}", platform::WEEKDAYS[lt.weekday as usize % 7]), tx));
            }
            if input.clicked(r) {
                platform::open("ms-actioncenter:");
            }
        }

        // ================= centre: now playing / next event =================
        let gap_l = left[0] + left[2] + 14.0;
        let gap_r = right[0] - 14.0;
        let mid = f.w / 2.0;
        let avail = ((mid - gap_l).min(gap_r - mid) * 2.0).min(420.0);
        if media.active && avail >= 240.0 {
            let cw = avail;
            let c = [mid - cw / 2.0, y, cw, BAR_H];
            glass(f, c, &pal);
            self.hits.push(c);
            // art
            let a_s = 32.0;
            let ax = c[0] + 8.0;
            let art_r = [ax, cy - a_s / 2.0, a_s, a_s];
            if let Some(art) = &media.art {
                let (k, _) = f.ensure_art(art);
                f.scene.shape(Shape::glow(ax + a_s / 2.0, cy, a_s * 0.8, with_a(pal.primary, 0.35 + 0.3 * audio.pulse()), 2.0));
                f.scene.image(ImageDraw::new(k, art_r[0], art_r[1], a_s, a_s, 9.0));
            } else {
                f.scene.shape(Shape::rect(art_r[0], art_r[1], a_s, a_s, 9.0, with_a(WHITE, 0.1)));
                f.icon(ic::MUSIC, ax + a_s / 2.0, cy, 13.0, with_a(WHITE, 0.7));
            }
            if input.clicked(art_r) {
                action = DockAction::Lyrics;
            }
            if input.over(art_r) {
                tip = Some(("Fullscreen lyrics".into(), ax + a_s / 2.0));
            }
            // controls (right side)
            let bx = c[0] + cw - 20.0;
            let col = with_a(WHITE, 0.9);
            if f.icon_button(input, ic::NEXT, bx, cy, 11.0, col) {
                shared.media_cmd(MediaCmd::Next);
            }
            let pp = if media.playing { ic::PAUSE } else { ic::PLAY };
            if f.icon_button(input, pp, bx - 28.0, cy, 13.0, WHITE) {
                shared.media_cmd(MediaCmd::PlayPause);
            }
            if f.icon_button(input, ic::PREV, bx - 56.0, cy, 11.0, col) {
                shared.media_cmd(MediaCmd::Prev);
            }
            // equalizer
            let ex = bx - 92.0;
            let n = 5;
            for k in 0..n {
                let v = if media.playing { audio.bands[(k * 7 + 2).min(BANDS - 1)].clamp(0.0, 1.0) } else { 0.0 };
                let bh = 3.0 + 15.0 * v;
                let c0 = mix(pal.primary, pal.secondary, k as f32 / (n - 1) as f32);
                f.scene.glow_shape(Shape::rect(ex + k as f32 * 4.5, cy - bh / 2.0, 2.6, bh, 1.3, c0));
            }
            // text
            let tx = ax + a_s + 10.0;
            let tw = (ex - 10.0 - tx).max(30.0);
            let t1 = f.fit(Text::new(&media.title, tx, cy - 16.0, 12.5, WHITE).weight(650), tw);
            f.scene.text(t1);
            let t2 = f.fit(Text::new(&media.artist, tx, cy + 1.0, 11.0, with_a(WHITE, 0.6)), tw);
            f.scene.text(t2);
            // progress hairline
            if media.duration > 0.0 {
                let frac = (media.pos_now() / media.duration) as f32;
                let pw = cw - 36.0;
                f.scene.shape(Shape::rect(c[0] + 18.0, y + BAR_H - 3.0, pw, 1.5, 0.75, with_a(WHITE, 0.10)));
                f.scene.glow_shape(Shape::rect(c[0] + 18.0, y + BAR_H - 3.0, pw * frac.clamp(0.0, 1.0), 1.5, 0.75, pal.primary).grad(pal.secondary, 0.0));
            }
        } else if let Some(ev) = next_event.filter(|_| avail >= 200.0) {
            let label = Text::new(&ev.title, 0.0, 0.0, 12.0, WHITE).weight(600);
            let when = event_when(ev.start, ev.all_day);
            let wt = Text::new(&when, 0.0, 0.0, 11.0, with_a(WHITE, 0.6)).weight(500);
            let lw = f.measure(&label).0.min(avail - 120.0);
            let ww = f.measure(&wt).0;
            let cw = (lw + ww + 64.0).min(avail);
            let c = [mid - cw / 2.0, y + 6.0, cw, BAR_H - 12.0];
            glass(f, c, &pal);
            self.hits.push(c);
            f.icon(ic::CALENDAR, c[0] + 20.0, cy, 12.0, with_a(pal.light, 0.9));
            let t = f.fit(Text { x: c[0] + 36.0, y: cy - 8.0, ..label }, lw);
            f.scene.text(t);
            f.scene.text(Text { x: c[0] + 46.0 + lw, y: cy - 7.0, ..wt });
        }

        // ================= tooltip =================
        if let Some((s, cx)) = tip {
            let t = Text::new(s, 0.0, 0.0, 11.5, WHITE).weight(550);
            let t = f.fit(t, 420.0);
            let tw = f.measure(&t).0 + 22.0;
            let th = 26.0;
            let tx = (cx - tw / 2.0).clamp(6.0, f.w - tw - 6.0);
            let ty = TIP_H - th - 4.0;
            f.scene.shape(Shape::rect(tx, ty + 2.0, tw, th, th / 2.0, [0.0, 0.0, 0.0, 0.25]).soft(8.0));
            f.scene.shape(Shape::rect(tx, ty, tw, th, th / 2.0, [0.06, 0.06, 0.09, 0.92]));
            f.scene.shape(Shape::stroke(tx, ty, tw, th, th / 2.0, 1.0, with_a(WHITE, 0.16)));
            f.scene.text(Text { x: tx + tw / 2.0, y: ty + 5.0, ..t }.center());
        }

        if self.hits.iter().any(|r| input.over(*r)) {
            self.busy = true;
        }
        action
    }
}

/// Floating glass capsule, tinted by the current album palette.
fn glass(f: &mut Frame, r: [f32; 4], pal: &Palette) {
    let [x, y, w, h] = r;
    let rad = h / 2.0;
    f.scene.shape(Shape::rect(x, y + 3.0, w, h, rad, [0.0, 0.0, 0.0, 0.28]).soft(12.0));
    f.scene.shape(Shape::rect(x, y, w, h, rad, [0.06, 0.06, 0.09, 0.62]));
    f.scene.shape(Shape::rect(x, y, w, h, rad, with_a(pal.primary, 0.10)).grad(with_a(pal.secondary, 0.06), 0.3));
    f.scene.shape(Shape::rect(x, y, w, h, rad, with_a(WHITE, 0.10)).grad(with_a(WHITE, 0.0), std::f32::consts::FRAC_PI_2 * 0.85));
    f.scene.shape(Shape::stroke(x, y, w, h, rad, 1.0, with_a(WHITE, 0.30)).grad(with_a(WHITE, 0.06), std::f32::consts::FRAC_PI_2));
}

fn sep(f: &mut Frame, x: f32, cy: f32) {
    f.scene.shape(Shape::rect(x, cy - 11.0, 1.0, 22.0, 0.5, with_a(WHITE, 0.12)));
}

fn weather_glyph(f: &mut Frame, cx: f32, cy: f32, r: f32, code: i32, day: bool) {
    let sun = rgba(255, 204, 0, 1.0);
    let cloud = with_a(WHITE, 0.9);
    if code <= 1 {
        if day {
            f.scene.glow_shape(Shape::glow(cx, cy, r, sun, 0.4));
        } else {
            f.scene.glow_shape(Shape::glow(cx, cy, r * 0.9, rgba(220, 220, 255, 1.0), 0.5));
        }
        return;
    }
    if code == 2 && day {
        f.scene.glow_shape(Shape::glow(cx + r * 0.5, cy - r * 0.45, r * 0.7, sun, 0.4));
    }
    f.scene.shape(Shape::rect(cx - r, cy - r * 0.1, r * 2.0, r * 0.8, r * 0.4, cloud));
    f.scene.shape(Shape::glow(cx - r * 0.25, cy - r * 0.1, r * 0.62, cloud, 0.05));
    f.scene.shape(Shape::glow(cx + r * 0.35, cy, r * 0.5, cloud, 0.05));
    if (51..=67).contains(&code) || (80..=99).contains(&code) {
        for k in 0..3 {
            let dx = (k as f32 - 1.0) * r * 0.6;
            f.scene.shape(Shape::line(cx + dx, cy + r * 0.9, cx + dx - r * 0.2, cy + r * 1.4, 1.4, rgba(100, 180, 255, 1.0)));
        }
    }
}

fn event_when(start: i64, all_day: bool) -> String {
    if all_day {
        return "today".into();
    }
    let d = start - platform::unix_now();
    if d <= 0 {
        "now".into()
    } else if d < 3600 {
        format!("in {} min", (d / 60).max(1))
    } else {
        let local = start + platform::utc_offset_secs();
        let s = local.rem_euclid(86400);
        format!("at {}:{:02}", s / 3600, (s / 60) % 60)
    }
}
