//! Taskbar dock: a floating glass bar along the bottom of the screen.
//!
//! * left   – weather, CPU / RAM rings, focus timer, wallpaper dial
//! * centre – Start, search, Task View, then every app pinned to the Windows taskbar
//!            plus open apps, with real icons, running dots and notification badges;
//!            the focused app expands to show its window title
//! * right  – Wi-Fi / volume (scroll to change) / battery, clock, show desktop
//!
//! Click an app to switch to it (again to minimize, or to cycle its windows). Right-click
//! for its window list, new window, close and pin options. Music lives on the island.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::anim::*;
use crate::desktop::{self, Pin};
use crate::gfx::*;
use crate::platform;
use crate::shell::{self, TaskWin};
use crate::state::*;
use crate::ui::{ic, Frame, Input, Key};

/// Height of the bar strip (logical px) that is reserved at the bottom of the screen.
pub const DOCK_H: f32 = 62.0;
/// Transparent, click-through space above the bar for tooltips, menus and the dial.
pub const TIP_H: f32 = 330.0;
const BAR_H: f32 = 46.0;
const SLOT: f32 = 40.0;
const ICON: f32 = 25.0;
const TITLE_MAX: f32 = 140.0;

pub enum PinOp {
    /// Pin an executable to the dock.
    Pin(String),
    /// Remove a dock-only pin (executable path).
    Unpin(String),
    /// Hide a Windows taskbar pin from the dock (its key).
    Hide(String),
}

pub enum DockAction {
    None,
    Launcher,
    Pins(PinOp),
}

#[derive(Clone)]
struct Slot {
    key: String,
    name: String,
    pin: Option<Pin>,
    wins: Vec<TaskWin>,
    icon_src: String,
}

impl Slot {
    fn launch(&self) -> Option<String> {
        match &self.pin {
            Some(p) => Some(p.launch.clone()),
            None if !self.key.starts_with("hwnd:") => Some(self.key.clone()),
            None => None,
        }
    }
}

type Px = Arc<(u32, u32, Vec<u8>)>;

struct Menu {
    key: String,
    x: f32,
}

pub struct Dock {
    wins: Vec<TaskWin>,
    last_scan: Option<Instant>,
    /// Last foreground window that isn't one of ours.
    fg: isize,
    pins: Vec<Pin>,
    pin_blob: Vec<u8>,
    last_pin_check: Option<Instant>,
    icons: HashMap<String, Option<Px>>,
    hover: HashMap<String, f32>,
    hover_since: Option<(String, f32)>,
    expand: HashMap<String, f32>,
    menu: Option<Menu>,
    menu_a: f32,
    timer_end: Option<Instant>,
    timer_total: f32,
    // wallpaper dial
    walls: Vec<PathBuf>,
    walls_checked: Option<Instant>,
    dial_open: bool,
    // control center (our own quick settings: opens and closes reliably)
    cc_open: bool,
    cc_a: f32,
    radios: Arc<parking_lot::Mutex<desktop::Radios>>,
    radio_tx: Sender<desktop::RadioCmd>,
    /// Slider being dragged: 1 = volume, 2 = brightness; with its live value.
    cc_drag: Option<(u8, f32)>,
    /// Brightness shown right after a change (Windows reports it back with a delay).
    cc_bri: Option<(f32, Instant)>,
    dial_a: f32,
    dial_x: f32,
    dial_pos: f32,
    dial_target: i64,
    dial_applied: Option<PathBuf>,
    dial_moved: Option<Instant>,
    thumbs: HashMap<PathBuf, Option<Px>>,
    thumb_req: Sender<PathBuf>,
    thumb_rx: Receiver<(PathBuf, Option<Px>)>,
    thumb_pending: HashSet<PathBuf>,
    /// Windows panel the dock opened and that is (probably) still up.
    panel: Option<u8>,
    /// Set when a second click asks to close that panel; the app does the focus dance.
    pub dismiss_req: bool,
    /// Demo screenshots: open the right-click menu of the focused app once.
    debug_menu: bool,
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
        ("notepad", "Notepad"),
        ("taskmgr", "Task Manager"),
        ("systemsettings", "Settings"),
        ("winword", "Word"),
        ("excel", "Excel"),
        ("powerpnt", "PowerPoint"),
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

fn spawn_thumb_loader() -> (Sender<PathBuf>, Receiver<(PathBuf, Option<Px>)>) {
    let (req_tx, req_rx) = channel::<PathBuf>();
    let (res_tx, res_rx) = channel();
    let _ = std::thread::Builder::new().name("wall-thumbs".into()).spawn(move || {
        while let Ok(p) = req_rx.recv() {
            let t = desktop::thumbnail(&p, 360).map(Arc::new);
            if res_tx.send((p, t)).is_err() {
                break;
            }
        }
    });
    (req_tx, res_rx)
}

impl Dock {
    pub fn new() -> Self {
        let (thumb_req, thumb_rx) = spawn_thumb_loader();
        let (radios, radio_tx) = desktop::spawn_radios();
        Dock {
            wins: vec![],
            last_scan: None,
            fg: 0,
            pins: vec![],
            pin_blob: vec![],
            last_pin_check: None,
            icons: HashMap::new(),
            hover: HashMap::new(),
            hover_since: None,
            expand: HashMap::new(),
            menu: None,
            menu_a: 0.0,
            timer_end: None,
            timer_total: 0.0,
            walls: vec![],
            walls_checked: None,
            dial_open: crate::demo::is_demo() && std::env::var("AUROLO_DEMO_DOCK_DIAL").is_ok(),
            cc_open: crate::demo::is_demo() && std::env::var("AUROLO_DEMO_DOCK_CC").is_ok(),
            cc_a: 0.0,
            radios,
            radio_tx,
            cc_drag: None,
            cc_bri: None,
            dial_a: 0.0,
            dial_x: 0.0,
            dial_pos: 0.0,
            dial_target: 0,
            dial_applied: None,
            dial_moved: None,
            thumbs: HashMap::new(),
            thumb_req,
            thumb_rx,
            thumb_pending: HashSet::new(),
            hits: vec![],
            busy: false,
            panel: None,
            dismiss_req: false,
            debug_menu: crate::demo::is_demo() && std::env::var("AUROLO_DEMO_DOCK_MENU").is_ok(),
        }
    }

    /// True while a menu or the wallpaper dial is open (the app keeps the dock responsive).
    pub fn popup_open(&self) -> bool {
        self.menu.is_some() || self.dial_open || self.cc_open
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
        // Windows taskbar pins: re-read only when the pin list actually changed
        let pin_due = self.last_pin_check.map(|t| t.elapsed() > Duration::from_secs(3)).unwrap_or(true);
        if pin_due {
            self.last_pin_check = Some(Instant::now());
            let blob = desktop::taskband_blob();
            if blob != self.pin_blob || self.pins.is_empty() {
                self.pins = desktop::taskbar_pins(&blob);
                self.pin_blob = blob;
            }
        }
    }

    fn slots(&self, extra: &[String], hidden: &[String]) -> Vec<Slot> {
        let mut out: Vec<Slot> = vec![];
        let hidden: Vec<String> = hidden.iter().map(|h| h.to_lowercase()).collect();
        for p in &self.pins {
            if hidden.contains(&p.key) {
                continue;
            }
            out.push(Slot { key: p.key.clone(), name: p.name.clone(), pin: Some(p.clone()), wins: vec![], icon_src: p.icon_src.clone() });
        }
        for e in extra {
            let exe = e.to_lowercase();
            // already on the Windows taskbar? don't show it twice
            if out.iter().any(|s| s.pin.as_ref().map(|p| p.matcher.hits(&exe)).unwrap_or(false)) {
                continue;
            }
            let p = desktop::exe_pin(&exe);
            out.push(Slot { key: p.key.clone(), name: app_name(&exe, "App"), pin: Some(p), wins: vec![], icon_src: exe.clone() });
        }
        // open apps, in a stable order while you work
        let mut order: Vec<&TaskWin> = self.wins.iter().collect();
        order.sort_by_key(|w| w.hwnd);
        for w in order {
            let found = if w.exe.is_empty() {
                None
            } else {
                out.iter_mut().find(|s| s.key == w.exe || s.pin.as_ref().map(|p| p.matcher.hits(&w.exe)).unwrap_or(false))
            };
            match found {
                Some(s) => s.wins.push(w.clone()),
                None => {
                    let key = if w.exe.is_empty() { format!("hwnd:{}", w.hwnd) } else { w.exe.clone() };
                    out.push(Slot { name: app_name(&w.exe, &w.title), icon_src: w.exe.clone(), key, pin: None, wins: vec![w.clone()] });
                }
            }
        }
        // inside each app, most recently used window first
        for s in &mut out {
            s.wins.sort_by_key(|w| self.wins.iter().position(|x| x.hwnd == w.hwnd).unwrap_or(usize::MAX));
        }
        out
    }

    fn icon_for(&mut self, f: &mut Frame, slot: &Slot) -> Option<u64> {
        let id = if slot.icon_src.is_empty() { slot.key.clone() } else { slot.icon_src.clone() };
        let k = key_of(&id);
        if !self.icons.contains_key(&id) {
            let mut px = None;
            if !slot.icon_src.is_empty() {
                px = desktop::shell_icon(&slot.icon_src);
                if px.is_none() && slot.pin.is_none() {
                    px = shell::file_icon(&slot.icon_src);
                }
            }
            if px.is_none() {
                px = slot.wins.first().and_then(|w| shell::window_icon(w.hwnd));
            }
            self.icons.insert(id.clone(), px.map(Arc::new));
        }
        let px = self.icons.get(&id)?.as_ref()?.clone();
        if !f.painter.has_image(k) {
            f.painter.upload_image(f.gpu, k, px.0, px.1, &px.2);
        }
        f.painter.touch_image(k);
        Some(k)
    }

    /// Open a Windows panel, or close it if it's the one we opened and it's still up
    /// (a second click toggles, like the Windows taskbar).
    fn toggle_panel(&mut self, id: u8, open: impl FnOnce()) {
        if self.panel == Some(id) {
            self.panel = None;
            self.dismiss_req = true;
        } else {
            open();
            self.panel = Some(id);
        }
    }

    /// Last app window the user worked in (focus goes back there when a panel closes).
    pub fn last_app(&self) -> isize {
        self.fg
    }

    /// A press outside the dock usually closes the open Windows panel too, unless it
    /// lands on the panel itself (rough screen area per panel).
    fn track_outside_press(&mut self, input: &Input) {
        let Some(id) = self.panel else { return };
        if !(input.pressed && input.mouse.is_none()) {
            return;
        }
        let (cx, cy) = platform::cursor_pos();
        let (mon, _) = shell::monitor_rects(cx, cy);
        let w = (mon.right - mon.left).max(1) as f32;
        let fx = (cx - mon.left) as f32 / w;
        let on_panel = match id {
            2 => fx > 0.70,                    // notifications: right side
            3 => (0.25..0.75).contains(&fx),   // Start: centre
            _ => false,
        };
        let _ = cy;
        if !on_panel {
            self.panel = None;
        }
    }

    fn timer_remaining(&self) -> Option<f32> {
        self.timer_end.map(|e| e.saturating_duration_since(Instant::now()).as_secs_f32())
    }

    fn refresh_walls(&mut self, dir: &str) {
        let due = self.walls_checked.map(|t| t.elapsed() > Duration::from_secs(5)).unwrap_or(true);
        if !due {
            return;
        }
        self.walls_checked = Some(Instant::now());
        let mut list = desktop::list_wallpapers(&desktop::wallpaper_dir(dir));
        if list.is_empty() {
            // nothing collected yet: offer the wallpapers that ship with Windows
            list = desktop::list_wallpapers(std::path::Path::new("C:\\Windows\\Web\\Wallpaper"));
        }
        let first = self.walls.is_empty();
        if list != self.walls {
            self.walls = list;
            if first || self.dial_applied.is_none() {
                let cur = desktop::current_wallpaper().to_lowercase();
                let idx = self.walls.iter().position(|p| p.to_string_lossy().to_lowercase() == cur).unwrap_or(0);
                self.dial_target = idx as i64;
                self.dial_pos = idx as f32;
                self.dial_applied = self.walls.get(idx).cloned().filter(|p| p.to_string_lossy().to_lowercase() == cur);
            }
        }
    }

    fn thumb(&mut self, f: &mut Frame, p: &PathBuf) -> Option<u64> {
        while let Ok((path, t)) = self.thumb_rx.try_recv() {
            self.thumb_pending.remove(&path);
            self.thumbs.insert(path, t);
        }
        match self.thumbs.get(p) {
            Some(Some(px)) => {
                let k = key_of(&p.to_string_lossy()) ^ 0x7a11;
                if !f.painter.has_image(k) {
                    f.painter.upload_image(f.gpu, k, px.0, px.1, &px.2);
                }
                f.painter.touch_image(k);
                Some(k)
            }
            Some(None) => None,
            None => {
                if self.thumb_pending.insert(p.clone()) {
                    let _ = self.thumb_req.send(p.clone());
                }
                self.busy = true;
                None
            }
        }
    }

    fn turn_dial(&mut self, steps: i64) {
        if self.walls.is_empty() || steps == 0 {
            return;
        }
        self.dial_target += steps;
        self.dial_moved = Some(Instant::now());
    }

    pub fn draw(
        &mut self,
        f: &mut Frame,
        input: &Input,
        shared: &Shared,
        extra_pins: &[String],
        hidden: &[String],
        focus_mins: u32,
        wall_dir: &str,
    ) -> DockAction {
        self.scan(false);
        self.track_outside_press(input);
        self.refresh_walls(wall_dir);
        self.hits.clear();
        self.busy = false;
        let media = shared.media.read().clone();
        let sys = shared.sys.read().clone();
        let (wifi, volume, muted, weather, notifs) = {
            let m = shared.misc.read();
            (m.wifi.clone(), m.volume, m.muted, m.weather.clone(), m.notifications.iter().map(|n| n.app.to_lowercase()).collect::<Vec<_>>())
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
        // a press anywhere outside an open popup closes it (mouse state is global)
        let mut popup_rects: Vec<[f32; 4]> = vec![];

        // ================= right: status + clock + show desktop =================
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
        let status_w = if sys.battery.is_some() { 118.0 } else { 78.0 };
        let desk_w = 14.0;
        let right_w = 6.0 + status_w + clock_w + desk_w;
        let right = [f.w - margin - right_w, y, right_w, BAR_H];
        glass(f, right, &pal);
        self.hits.push(right);
        let mut rx = right[0] + 6.0;
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
                if input.scroll.abs() > 0.01 {
                    desktop::nudge_volume(0.04 * input.scroll.signum() * input.scroll.abs().ceil());
                }
                let mut t = format!(
                    "{}  ·  Volume {:.0}% (scroll to change)",
                    wifi.clone().unwrap_or_else(|| "Offline".into()),
                    volume * 100.0
                );
                if let Some(b) = &sys.battery {
                    t += &format!("  ·  Battery {:.0}%{}", b.percent, if b.charging { " charging" } else { "" });
                }
                tip = Some((t, rx + status_w / 2.0));
            }
            if input.clicked(r) {
                self.cc_open = !self.cc_open;
                self.menu = None;
                self.dial_open = false;
            }
            rx += status_w;
        }
        sep(f, rx - 3.0, cy);
        {
            let r = [rx, y + 5.0, clock_w - 4.0, BAR_H - 10.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 12.0, with_a(WHITE, 0.09)));
            }
            let tx = rx + clock_w / 2.0 - 2.0;
            f.scene.text(Text { x: tx, y: cy - 17.0, ..time_t }.center());
            f.scene.text(Text { x: tx, y: cy + 2.0, ..date_t }.center());
            if !notifs.is_empty() {
                let bx = r[0] + r[2] - 7.0;
                f.scene.glow_shape(Shape::glow(bx, y + 11.0, 4.0, pal.primary, 0.4));
                f.scene.shape(Shape::glow(bx, y + 11.0, 2.6, WHITE, 0.2));
            }
            if hov {
                let n = if notifs.len() == 1 { "1 notification".to_string() } else { format!("{} notifications", notifs.len()) };
                tip = Some((format!("{}  ·  {n}", platform::WEEKDAYS[lt.weekday as usize % 7]), tx));
            }
            if input.clicked(r) {
                self.toggle_panel(2, || platform::open("ms-actioncenter:"));
            }
            rx += clock_w;
        }
        {
            // show desktop: the sliver at the very end, like Windows
            let r = [rx - 2.0, y + 8.0, desk_w - 2.0, BAR_H - 16.0];
            let hov = input.over(r);
            f.scene.shape(Shape::rect(rx + 3.0, cy - 9.0, 1.2, 18.0, 0.6, with_a(WHITE, if hov { 0.7 } else { 0.25 })));
            if hov {
                tip = Some(("Show desktop".into(), rx + 4.0));
            }
            if input.clicked(r) {
                desktop::show_desktop();
            }
        }

        // ================= left: weather, system, focus, wallpaper =================
        let weather_w = if weather.is_some() { 62.0 } else { 0.0 };
        let timer_rem = self.timer_remaining();
        let timer_w = if timer_rem.is_some() { 92.0 } else { 40.0 };
        let stats_w = 84.0;
        let wall_w = 44.0;
        let left_w = 8.0 + weather_w + stats_w + timer_w + wall_w + 2.0;
        let left = [margin, y, left_w, BAR_H];
        glass(f, left, &pal);
        self.hits.push(left);
        let mut lx = left[0] + 8.0;
        if let Some(wt) = &weather {
            weather_glyph(f, lx + 15.0, cy - 1.0, 8.5, wt.code, wt.is_day);
            f.scene.text(Text::new(format!("{:.0}°", wt.temp), lx + 29.0, cy - 9.0, 13.5, WHITE).weight(600));
            if input.over([lx, y, weather_w, BAR_H]) {
                tip = Some((format!("{}  ·  H {:.0}°  L {:.0}°", wt.city, wt.hi, wt.lo), lx + 28.0));
            }
            lx += weather_w;
            sep(f, lx - 3.0, cy);
        }
        {
            let r = [lx, y + 5.0, stats_w - 6.0, BAR_H - 10.0];
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
            ring(f, lx + 19.0, cpu, "CPU", rgba(90, 200, 255, 1.0), rgba(140, 120, 255, 1.0));
            ring(f, lx + 55.0, ram, "RAM", rgba(120, 255, 190, 1.0), rgba(90, 200, 255, 1.0));
            if hov {
                let t = format!(
                    "CPU {:.0}%{}  ·  RAM {:.1} / {:.0} GB  ·  ↓ {}",
                    sys.cpu,
                    sys.cpu_temp.map(|t| format!(" {t:.0}°C")).unwrap_or_default(),
                    sys.ram_used,
                    sys.ram_total,
                    crate::ui::fmt_bytes_rate(sys.net_down)
                );
                tip = Some((t, lx + 37.0));
            }
            if input.clicked(r) {
                platform::open("taskmgr.exe");
            }
            lx += stats_w;
        }
        sep(f, lx - 3.0, cy);
        {
            let r = [lx, y + 5.0, timer_w - 6.0, BAR_H - 10.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 12.0, with_a(WHITE, 0.09)));
            }
            let tcx = lx + 17.0;
            if let Some(rem) = timer_rem {
                let p = rem / self.timer_total.max(1.0);
                f.scene.shape(Shape::arc(tcx, cy, 11.0, 2.4, 1.0, with_a(WHITE, 0.12)));
                f.scene.glow_shape(Shape::arc(tcx, cy, 11.0, 2.4, p, rgba(255, 120, 90, 1.0)).conic(rgba(255, 190, 90, 1.0), 0.0));
                f.icon(ic::TIMER, tcx, cy, 9.5, with_a(WHITE, 0.85));
                f.scene.text(Text::new(fmt_time(rem as f64), lx + 33.0, cy - 9.0, 13.0, WHITE).weight(600).family(Family::Mono));
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
            lx += timer_w;
        }
        sep(f, lx - 3.0, cy);
        {
            // wallpaper dial button: click to open the dial, scroll to flip wallpapers
            let r = [lx, y + 5.0, wall_w - 6.0, BAR_H - 10.0];
            let hov = input.over(r);
            let wcx = lx + (wall_w - 6.0) / 2.0;
            if hov || self.dial_open {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 12.0, with_a(WHITE, if self.dial_open { 0.14 } else { 0.09 })));
            }
            // a tiny dial: ring + notch that turns with the selection
            let n = self.walls.len().max(1) as f32;
            let ang = (self.dial_pos / n) * std::f32::consts::TAU;
            f.scene.shape(Shape::arc(wcx, cy, 11.0, 2.0, 1.0, with_a(WHITE, 0.22)));
            f.scene.glow_shape(Shape::arc(wcx, cy, 11.0, 2.0, 0.16, pal.primary).start_angle(ang));
            f.icon('\u{E91B}', wcx, cy, 10.0, with_a(WHITE, 0.9));
            if hov && input.scroll.abs() > 0.01 {
                self.turn_dial(if input.scroll > 0.0 { -1 } else { 1 });
                self.dial_open = true;
                self.dial_x = wcx;
            }
            if input.clicked(r) {
                self.cc_open = false;
                self.dial_open = !self.dial_open;
                self.dial_x = wcx;
                self.menu = None;
            }
            if hov && !self.dial_open {
                tip = Some(("Wallpapers  ·  click or scroll".into(), wcx));
            }
        }

        // ================= centre: start, search, task view, apps =================
        let slots = self.slots(extra_pins, hidden);
        let fg_key = slots.iter().find(|s| s.wins.iter().any(|w| w.hwnd == self.fg)).map(|s| s.key.clone());
        let lead_w = 3.0 * 38.0 + 10.0;
        let left_end = left[0] + left[2] + 12.0;
        let right_start = right[0] - 12.0;
        let base_w = 6.0 + lead_w + SLOT * slots.len() as f32 + 6.0;
        let title_room = (right_start - left_end - base_w).clamp(0.0, TITLE_MAX + 10.0);
        let mut widths = vec![];
        let mut titles = vec![];
        for s in &slots {
            let target = if Some(&s.key) == fg_key.as_ref() { 1.0 } else { 0.0 };
            let e = self.expand.entry(s.key.clone()).or_insert(target);
            *e = approach(*e, target, 9.0, f.dt);
            if (*e - target).abs() > 0.01 {
                self.busy = true;
            }
            let ev = *e;
            let title = s.wins.iter().find(|w| w.hwnd == self.fg).or(s.wins.first()).map(|w| w.title.clone()).unwrap_or_default();
            let tt = f.fit(Text::new(&title, 0.0, 0.0, 12.0, WHITE).weight(600), (title_room - 10.0).max(0.0));
            let tw = if title.is_empty() || title_room < 40.0 { 0.0 } else { f.measure(&tt).0 + 10.0 };
            widths.push(SLOT + tw * ease_out_cubic(ev));
            titles.push((tt, ev));
        }
        let mid_w = 6.0 + lead_w + widths.iter().sum::<f32>() + 6.0;
        let mx = (f.w / 2.0 - mid_w / 2.0).max(left_end).min((right_start - mid_w).max(left_end));
        let mid = [mx, y, mid_w, BAR_H];
        glass(f, mid, &pal);
        self.hits.push(mid);
        let mut x = mx + 6.0;
        // Start
        {
            let c = x + 19.0;
            let r = [c - 17.0, cy - 17.0, 34.0, 34.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 11.0, with_a(WHITE, 0.10)));
            }
            let s = 7.2;
            let g = 1.6;
            for (i, (dx, dy)) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)].iter().enumerate() {
                let qx = c + if *dx < 0.0 { -s - g / 2.0 } else { g / 2.0 };
                let qy = cy + if *dy < 0.0 { -s - g / 2.0 } else { g / 2.0 };
                let c0 = mix(pal.primary, pal.secondary, i as f32 / 3.0);
                f.scene.shape(Shape::rect(qx, qy, s, s, 1.8, mix(c0, WHITE, 0.25)).grad(c0, 0.8));
            }
            if hov {
                tip = Some(("Start".into(), c));
            }
            if input.clicked(r) {
                self.toggle_panel(3, desktop::open_start);
            }
            x += 38.0;
        }
        // Search (Aurolo launcher)
        {
            let c = x + 19.0;
            let r = [c - 17.0, cy - 17.0, 34.0, 34.0];
            let hov = input.over(r);
            f.scene.shape(Shape::glow(c, cy, 20.0, with_a(pal.primary, if hov { 0.5 } else { 0.25 }), 1.6));
            f.scene.shape(Shape::rect(r[0] + 2.0, r[1] + 2.0, 30.0, 30.0, 15.0, pal.primary).conic(pal.secondary, 0.6));
            f.scene.shape(Shape::rect(r[0] + 3.5, r[1] + 3.5, 27.0, 27.0, 13.5, [0.04, 0.03, 0.08, if hov { 0.35 } else { 0.55 }]));
            f.icon(ic::SEARCH, c, cy, 12.5, WHITE);
            if hov {
                tip = Some(("Search apps, web & math".into(), c));
            }
            if input.clicked(r) {
                action = DockAction::Launcher;
            }
            x += 38.0;
        }
        // Task View
        {
            let c = x + 19.0;
            let r = [c - 17.0, cy - 17.0, 34.0, 34.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 11.0, with_a(WHITE, 0.10)));
            }
            f.icon('\u{E7C4}', c, cy, 15.0, with_a(WHITE, 0.9));
            if hov {
                tip = Some(("Task View".into(), c));
            }
            if input.clicked(r) {
                self.toggle_panel(4, desktop::task_view);
            }
            x += 38.0;
        }
        f.scene.shape(Shape::rect(x + 4.0, cy - 12.0, 1.0, 24.0, 0.5, with_a(WHITE, 0.14)));
        x += 10.0;

        for (i, s) in slots.iter().enumerate() {
            let sw = widths[i];
            let r = [x, y + 3.0, sw, BAR_H - 6.0];
            let hov = input.over(r);
            let h = self.hover.entry(s.key.clone()).or_insert(0.0);
            *h = approach(*h, if hov { 1.0 } else { 0.0 }, 14.0, f.dt);
            let hv = *h;
            if hv > 0.01 && hv < 0.99 {
                self.busy = true;
            }
            let (tt, ev) = &titles[i];
            let focused = Some(&s.key) == fg_key.as_ref();
            let running = !s.wins.is_empty();
            if *ev > 0.01 {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 13.0, with_a(pal.primary, 0.20 * ev)).grad(with_a(pal.secondary, 0.10 * ev), 0.0));
                f.scene.shape(Shape::stroke(r[0], r[1], r[2], r[3], 13.0, 1.0, with_a(WHITE, 0.16 * ev)));
            }
            if hv > 0.01 {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 13.0, with_a(WHITE, 0.09 * hv)));
            }
            let icx = x + SLOT / 2.0;
            let icy = cy - 2.0 - 2.0 * hv;
            let sz = ICON * (1.0 + 0.10 * hv);
            match self.icon_for(f, s) {
                Some(k) => {
                    let op = if running || hv > 0.5 { 1.0 } else { 0.86 };
                    f.scene.image(ImageDraw::new(k, icx - sz / 2.0, icy - sz / 2.0, sz, sz, 5.0).opacity(op));
                }
                None => {
                    let (c0, c1) = crate::ui::app_colors(&s.name);
                    f.scene.shape(Shape::rect(icx - sz / 2.0, icy - sz / 2.0, sz, sz, 8.0, c0).grad(c1, 0.8));
                    let ini = s.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                    f.scene.text(Text::new(ini, icx, icy - sz * 0.36, sz * 0.55, WHITE).center().weight(700));
                }
            }
            // notification badge
            let nl = s.name.to_lowercase();
            let badge = notifs.iter().filter(|a| !a.is_empty() && (nl.contains(a.as_str()) || a.contains(nl.as_str()))).count();
            if badge > 0 {
                let bx = icx + sz * 0.42;
                let by = icy - sz * 0.42;
                let label = if badge > 9 { "9+".to_string() } else { badge.to_string() };
                let bw = if badge > 9 { 17.0 } else { 13.0 };
                f.scene.shape(Shape::rect(bx - bw / 2.0, by - 6.5, bw, 13.0, 6.5, rgba(255, 70, 95, 1.0)));
                f.scene.shape(Shape::stroke(bx - bw / 2.0, by - 6.5, bw, 13.0, 6.5, 1.2, [0.06, 0.06, 0.09, 0.9]));
                f.scene.text(Text::new(label, bx, by - 6.0, 8.5, WHITE).center().weight(700));
            }
            if *ev > 0.05 && !tt.s.is_empty() {
                let a = smooth(0.35, 1.0, *ev);
                f.scene.text(Text {
                    x: x + SLOT - 2.0,
                    y: cy - 8.5,
                    color: with_a(WHITE, 0.92 * a),
                    clip: Some([x, y, x + sw - 6.0, y + BAR_H]),
                    ..tt.clone()
                });
            }
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
            if hov && self.menu.is_none() {
                let t = f.t;
                match &self.hover_since {
                    Some((e, _)) if *e == s.key => {}
                    _ => self.hover_since = Some((s.key.clone(), t)),
                }
                let since = self.hover_since.as_ref().map(|h| t - h.1).unwrap_or(0.0);
                if since > 0.35 && !focused {
                    let label = match s.wins.len() {
                        0 => s.name.clone(),
                        1 => s.wins[0].title.clone(),
                        n => format!("{}  ·  {n} windows  ·  right-click for all", s.name),
                    };
                    tip = Some((label, icx));
                } else if since <= 0.35 {
                    self.busy = true;
                }
            }
            if input.clicked(r) && self.menu.is_none() {
                if s.wins.is_empty() {
                    if let Some(l) = s.launch() {
                        platform::open(&l);
                    }
                } else if focused && s.wins.len() == 1 {
                    shell::minimize_window(s.wins[0].hwnd);
                } else if focused {
                    // windows are MRU-sorted: index 1 is the one used before this, so
                    // repeated clicks cycle through the app's windows
                    shell::activate_window(s.wins[1].hwnd);
                } else {
                    shell::activate_window(s.wins[0].hwnd);
                }
                self.last_scan = None;
            }
            if hov && input.right_pressed {
                self.menu = Some(Menu { key: s.key.clone(), x: icx });
                self.menu_a = 0.0;
                self.dial_open = false;
            }
            x += sw;
        }

        if self.debug_menu {
            if let Some(i) = slots.iter().position(|s| s.wins.len() > 1).or_else(|| slots.iter().position(|s| !s.wins.is_empty())) {
                let xs: f32 = mx + 6.0 + lead_w + widths[..i].iter().sum::<f32>() + SLOT / 2.0;
                self.menu = Some(Menu { key: slots[i].key.clone(), x: xs });
                self.debug_menu = false;
            }
        }

        // ================= app menu (right-click) =================
        self.menu_a = approach(self.menu_a, if self.menu.is_some() { 1.0 } else { 0.0 }, 16.0, f.dt);
        if let Some(m) = &self.menu {
            let mkey = m.key.clone();
            let mxc = m.x;
            match slots.iter().find(|s| s.key == mkey).cloned() {
                None => self.menu = None,
                Some(s) => {
                    if self.menu_a < 0.99 {
                        self.busy = true;
                    }
                    if let Some((rect, act)) = self.draw_menu(f, input, &s, mxc, &pal) {
                        popup_rects.push(rect);
                        if let Some(a) = act {
                            action = a;
                        }
                    }
                }
            }
        }

        // ================= wallpaper dial =================
        self.dial_a = approach(self.dial_a, if self.dial_open { 1.0 } else { 0.0 }, 14.0, f.dt);
        let target = self.dial_target as f32;
        if (self.dial_pos - target).abs() > 0.001 {
            self.dial_pos = approach(self.dial_pos, target, 12.0, f.dt);
            self.busy = true;
        } else {
            self.dial_pos = target;
        }
        if self.dial_a > 0.01 {
            if let Some(rect) = self.draw_dial(f, input, &pal) {
                popup_rects.push(rect);
            }
            if self.dial_a < 0.99 {
                self.busy = true;
            }
        }
        // apply the chosen wallpaper once the dial has settled for a moment
        if let Some(t) = self.dial_moved {
            if t.elapsed() > Duration::from_millis(450) && !self.walls.is_empty() {
                self.dial_moved = None;
                let n = self.walls.len() as i64;
                let p = self.walls[self.dial_target.rem_euclid(n) as usize].clone();
                if self.dial_applied.as_ref() != Some(&p) {
                    self.dial_applied = Some(p.clone());
                    let _ = std::thread::Builder::new().name("set-wallpaper".into()).spawn(move || desktop::set_wallpaper(&p));
                }
            } else {
                self.busy = true;
            }
        }

        // ================= control center =================
        self.cc_a = approach(self.cc_a, if self.cc_open { 1.0 } else { 0.0 }, 16.0, f.dt);
        if self.cc_a > 0.01 {
            if self.cc_a < 0.99 {
                self.busy = true;
            }
            let rect = self.draw_cc(f, input, shared, &pal, right[0] + right[2]);
            popup_rects.push(rect);
        }

        // close popups on a press elsewhere or Esc
        if input.key(Key::Esc) {
            self.menu = None;
            self.dial_open = false;
            self.cc_open = false;
        } else if input.pressed && !popup_rects.iter().any(|r| input.over(*r)) {
            self.menu = None;
            // the buttons that open the dial / control center toggle them on release
            if !input.over(left) {
                self.dial_open = false;
            }
            if !input.over(right) {
                self.cc_open = false;
            }
        }
        self.hits.extend(popup_rects.iter().copied());

        // ================= tooltip =================
        if let Some((s, cx)) = tip.filter(|_| self.menu.is_none()) {
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

    /// Right-click menu for an app: its windows, new window, pin options, close.
    fn draw_menu(&mut self, f: &mut Frame, input: &Input, s: &Slot, anchor: f32, pal: &Palette) -> Option<([f32; 4], Option<DockAction>)> {
        let a = ease_out_cubic(self.menu_a);
        let row = 30.0;
        let w = 290.0;
        let wins: Vec<&TaskWin> = s.wins.iter().take(7).collect();
        let mut items: Vec<(char, String, u8)> = vec![]; // (icon, label, id)
        if s.launch().is_some() {
            items.push((ic::ADD, if s.wins.is_empty() { format!("Open {}", s.name) } else { "New window".into() }, 1));
        }
        match &s.pin {
            Some(p) if p.from_windows => items.push((ic::PIN, "Hide from dock".into(), 2)),
            Some(_) => items.push((ic::PIN, "Unpin from dock".into(), 3)),
            None if !s.key.starts_with("hwnd:") => items.push((ic::PIN, "Pin to dock".into(), 4)),
            None => {}
        }
        if !s.wins.is_empty() {
            items.push((ic::CLOSE, if s.wins.len() == 1 { "Close window".into() } else { format!("Close all {} windows", s.wins.len()) }, 5));
        }
        let head = 34.0;
        let sep_h = if wins.is_empty() { 0.0 } else { 9.0 };
        let h = head + wins.len() as f32 * row + sep_h + items.len() as f32 * row + 8.0;
        let x = (anchor - w / 2.0).clamp(8.0, f.w - w - 8.0);
        let y = TIP_H - h - 6.0 + 10.0 * (1.0 - a);
        let rect = [x, y, w, h];
        f.scene.shape(Shape::rect(x, y + 4.0, w, h, 16.0, [0.0, 0.0, 0.0, 0.3 * a]).soft(16.0));
        f.scene.shape(Shape::rect(x, y, w, h, 16.0, [0.07, 0.07, 0.10, 0.95 * a]));
        f.scene.shape(Shape::rect(x, y, w, h, 16.0, with_a(pal.primary, 0.08 * a)).grad(with_a(pal.secondary, 0.04 * a), 0.4));
        f.scene.shape(Shape::stroke(x, y, w, h, 16.0, 1.0, with_a(WHITE, 0.22 * a)));
        f.scene.text(Text::new(&s.name, x + 16.0, y + 10.0, 12.5, with_a(WHITE, 0.95 * a)).weight(700));
        let mut out: Option<DockAction> = None;
        let mut close_menu = false;
        let mut ry = y + head;
        for wdw in &wins {
            let r = [x + 6.0, ry, w - 12.0, row - 2.0];
            let hov = input.over(r);
            let active = wdw.hwnd == self.fg;
            if hov || active {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 9.0, with_a(WHITE, if hov { 0.10 } else { 0.05 } * a)));
            }
            if active {
                f.scene.shape(Shape::rect(r[0] + 3.0, r[1] + 8.0, 3.0, r[3] - 16.0, 1.5, with_a(pal.primary, a)));
            }
            let t = f.fit(Text::new(&wdw.title, x + 18.0, ry + 6.0, 12.0, with_a(WHITE, 0.9 * a)), w - 64.0);
            f.scene.text(t);
            // per-window close
            let cx = x + w - 24.0;
            let cr = [cx - 11.0, ry + 3.0, 22.0, 22.0];
            if hov {
                let ch = input.over(cr);
                if ch {
                    f.scene.shape(Shape::rect(cr[0], cr[1], cr[2], cr[3], 7.0, with_a(rgba(255, 80, 90, 1.0), 0.8 * a)));
                }
                f.icon(ic::CLOSE, cx, ry + 14.0, 9.0, with_a(WHITE, 0.9 * a));
            }
            if input.clicked(cr) {
                shell::close_window(wdw.hwnd);
                self.last_scan = None;
            } else if input.clicked(r) {
                shell::activate_window(wdw.hwnd);
                close_menu = true;
            }
            ry += row;
        }
        if !wins.is_empty() {
            f.scene.shape(Shape::rect(x + 14.0, ry + 4.0, w - 28.0, 1.0, 0.5, with_a(WHITE, 0.10 * a)));
            ry += sep_h;
        }
        for (g, label, id) in &items {
            let r = [x + 6.0, ry, w - 12.0, row - 2.0];
            let hov = input.over(r);
            if hov {
                f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 9.0, with_a(WHITE, 0.10 * a)));
            }
            f.icon(*g, x + 24.0, ry + 14.0, 11.0, with_a(WHITE, 0.8 * a));
            f.scene.text(Text::new(label, x + 42.0, ry + 6.0, 12.0, with_a(WHITE, 0.9 * a)));
            if input.clicked(r) {
                close_menu = true;
                match id {
                    1 => {
                        if let Some(l) = s.launch() {
                            platform::open(&l);
                        }
                    }
                    2 => out = Some(DockAction::Pins(PinOp::Hide(s.key.clone()))),
                    3 => {
                        let exe = s.pin.as_ref().map(|p| p.launch.clone()).unwrap_or_default();
                        out = Some(DockAction::Pins(PinOp::Unpin(exe)));
                    }
                    4 => out = Some(DockAction::Pins(PinOp::Pin(s.key.clone()))),
                    5 => {
                        for wdw in &s.wins {
                            shell::close_window(wdw.hwnd);
                        }
                        self.last_scan = None;
                    }
                    _ => {}
                }
            }
            ry += row;
        }
        if close_menu {
            self.menu = None;
        }
        Some((rect, out))
    }

    /// Control center: Wi-Fi / Bluetooth switches, volume and brightness, battery and
    /// shortcuts into Settings. Anchored above the right end of the dock.
    fn draw_cc(&mut self, f: &mut Frame, input: &Input, shared: &Shared, pal: &Palette, right_edge: f32) -> [f32; 4] {
        let a = ease_out_cubic(self.cc_a);
        let (wifi_name, volume, muted, brightness, bt_dev) = {
            let m = shared.misc.read();
            (m.wifi.clone(), m.volume, m.muted, m.brightness, m.bt_devices.first().map(|d| d.name.clone()))
        };
        let battery = shared.sys.read().battery.clone();
        let radios = *self.radios.lock();
        let w = 352.0;
        let tile_h = 56.0;
        let has_bri = brightness.is_some();
        let h = 16.0 + (tile_h + 24.0) * 2.0 + 6.0 + if has_bri { 42.0 } else { 0.0 } + 42.0 + 46.0;
        let x = (right_edge - w).max(8.0);
        let y = TIP_H - h - 6.0 + 12.0 * (1.0 - a);
        let rect = [x, y, w, h];
        f.scene.shape(Shape::rect(x, y + 4.0, w, h, 22.0, [0.0, 0.0, 0.0, 0.32 * a]).soft(18.0));
        f.scene.shape(Shape::rect(x, y, w, h, 22.0, [0.06, 0.06, 0.09, 0.95 * a]));
        f.scene.shape(Shape::rect(x, y, w, h, 22.0, with_a(pal.primary, 0.08 * a)).grad(with_a(pal.secondary, 0.04 * a), 0.4));
        f.scene.shape(Shape::stroke(x, y, w, h, 22.0, 1.0, with_a(WHITE, 0.22 * a)));
        let _ = pal;
        let accent = rgba(70, 140, 255, 1.0);

        // ---- tiles
        enum Act {
            Radio(bool),
            Open(&'static str),
        }
        let wifi_on = radios.wifi.or(Some(wifi_name.is_some()));
        let tiles: [(char, String, Option<bool>, Act, Option<&'static str>); 6] = [
            (
                if wifi_on == Some(false) { ic::WIFI_OFF } else { ic::WIFI },
                if wifi_on == Some(true) { wifi_name.clone().unwrap_or_else(|| "Wi-Fi".into()) } else { "Wi-Fi".into() },
                wifi_on,
                if radios.wifi.is_some() { Act::Radio(true) } else { Act::Open("ms-settings:network-wifi") },
                Some("ms-settings:network-wifi"),
            ),
            (
                ic::BT,
                match (radios.bluetooth, &bt_dev) {
                    (Some(true), Some(d)) => d.clone(),
                    _ => "Bluetooth".into(),
                },
                radios.bluetooth,
                if radios.bluetooth.is_some() { Act::Radio(false) } else { Act::Open("ms-settings:bluetooth") },
                Some("ms-settings:bluetooth"),
            ),
            ('\u{E709}', "Airplane mode".into(), None, Act::Open("ms-settings:network-airplanemode"), None),
            ('\u{E708}', "Night light".into(), None, Act::Open("ms-settings:nightlight"), None),
            ('\u{EBAA}', "Energy saver".into(), None, Act::Open("ms-settings:batterysaver"), None),
            ('\u{E7EF}', "Do not disturb".into(), None, Act::Open("ms-settings:notifications"), None),
        ];
        let tw = (w - 32.0 - 2.0 * 10.0) / 3.0;
        for (i, (g, label, on, act, more)) in tiles.into_iter().enumerate() {
            let tx = x + 16.0 + (i % 3) as f32 * (tw + 10.0);
            let ty = y + 16.0 + (i / 3) as f32 * (tile_h + 24.0);
            let r = [tx, ty, tw, tile_h];
            let hov = input.over(r);
            let active = on == Some(true);
            let bg = if active { with_a(accent, 0.92 * a) } else { with_a(WHITE, if hov { 0.12 } else { 0.07 } * a) };
            f.scene.shape(Shape::rect(tx, ty, tw, tile_h, 12.0, bg));
            if !active {
                f.scene.shape(Shape::stroke(tx, ty, tw, tile_h, 12.0, 1.0, with_a(WHITE, 0.08 * a)));
            }
            let fg = if active { with_a([0.03, 0.04, 0.08, 1.0], a) } else { with_a(WHITE, 0.92 * a) };
            let split = more.is_some();
            let icx = if split { tx + tw * 0.34 } else { tx + tw / 2.0 };
            f.icon(g, icx, ty + tile_h / 2.0, 15.0, fg);
            let mut main_r = r;
            if let Some(target) = more {
                // right third: chevron that opens the full settings page
                let cr = [tx + tw * 0.66, ty, tw * 0.34, tile_h];
                main_r = [tx, ty, tw * 0.66, tile_h];
                f.scene.shape(Shape::rect(cr[0], ty + 12.0, 1.0, tile_h - 24.0, 0.5, with_a(fg, 0.25)));
                f.icon('\u{E76C}', cr[0] + cr[2] / 2.0, ty + tile_h / 2.0, 10.0, fg);
                if input.clicked(cr) {
                    platform::open(target);
                    self.cc_open = false;
                }
            }
            let lt = f.fit(Text::new(label, tx + tw / 2.0, ty + tile_h + 5.0, 11.0, with_a(WHITE, 0.8 * a)).center(), tw + 6.0);
            f.scene.text(lt);
            if input.clicked(main_r) {
                match act {
                    Act::Radio(is_wifi) => {
                        let cur = if is_wifi { radios.wifi } else { radios.bluetooth }.unwrap_or(false);
                        let _ = self.radio_tx.send(desktop::RadioCmd::Set { wifi: is_wifi, on: !cur });
                        // show the new state right away
                        let mut st = self.radios.lock();
                        if is_wifi {
                            st.wifi = Some(!cur);
                        } else {
                            st.bluetooth = Some(!cur);
                        }
                    }
                    Act::Open(uri) => {
                        platform::open(uri);
                        self.cc_open = false;
                    }
                }
            }
        }

        // ---- sliders (brightness on laptops, volume)
        let mut sy = y + 16.0 + (tile_h + 24.0) * 2.0 + 6.0;
        if let Some((_, t)) = self.cc_bri {
            if t.elapsed() > Duration::from_secs(3) {
                self.cc_bri = None;
            }
        }
        let mut rows: Vec<(u8, char, f32)> = vec![];
        if let Some(b) = brightness {
            rows.push((2, ic::BRIGHT, self.cc_bri.map(|x| x.0).unwrap_or(b)));
        }
        let vg = if muted || volume <= 0.001 { ic::MUTE } else if volume < 0.4 { ic::VOLUME1 } else { ic::VOLUME };
        rows.push((1, vg, if muted { 0.0 } else { volume }));
        for (id, g, val) in rows {
            let val = match self.cc_drag {
                Some((d, v)) if d == id => v,
                _ => val,
            };
            let gx = x + 30.0;
            let cy = sy + 18.0;
            if f.icon_button(input, g, gx, cy, 13.0, with_a(WHITE, 0.9 * a)) && id == 1 {
                desktop::toggle_mute();
            }
            let bx = x + 56.0;
            let bw = w - 56.0 - 28.0;
            let track = [bx - 8.0, cy - 12.0, bw + 16.0, 24.0];
            f.scene.shape(Shape::rect(bx, cy - 2.0, bw, 4.0, 2.0, with_a(WHITE, 0.16 * a)));
            f.scene.glow_shape(Shape::rect(bx, cy - 2.0, (bw * val).max(4.0), 4.0, 2.0, with_a(accent, a)));
            let kx = bx + bw * val;
            f.scene.shape(Shape::glow(kx, cy, 9.0, with_a(BLACK, 0.4 * a), 1.0));
            f.scene.shape(Shape::rect(kx - 7.0, cy - 7.0, 14.0, 14.0, 7.0, with_a(WHITE, a)));
            f.scene.shape(Shape::rect(kx - 3.5, cy - 3.5, 7.0, 7.0, 3.5, with_a(accent, a)));
            if input.pressed && input.over(track) {
                self.cc_drag = Some((id, val));
            }
            if let Some((d, _)) = self.cc_drag {
                if d == id {
                    self.busy = true;
                    if let Some((mx, _)) = input.mouse {
                        let nv = ((mx - bx) / bw).clamp(0.0, 1.0);
                        if id == 1 && (nv - val).abs() > 0.005 {
                            desktop::set_volume(nv);
                        }
                        self.cc_drag = Some((id, nv));
                    }
                    if !input.down {
                        let v = self.cc_drag.map(|x| x.1).unwrap_or(val);
                        if id == 2 {
                            desktop::set_brightness(v);
                            self.cc_bri = Some((v, Instant::now()));
                        }
                        self.cc_drag = None;
                    }
                }
            }
            if input.over(track) && input.scroll.abs() > 0.01 && self.cc_drag.is_none() {
                let nv = (val + 0.05 * input.scroll.signum()).clamp(0.0, 1.0);
                if id == 1 {
                    desktop::set_volume(nv);
                } else {
                    desktop::set_brightness(nv);
                    self.cc_bri = Some((nv, Instant::now()));
                }
            }
            sy += 42.0;
        }

        // ---- footer: battery + settings
        let fy = y + h - 46.0;
        f.scene.shape(Shape::rect(x + 1.0, fy, w - 2.0, 1.0, 0.5, with_a(WHITE, 0.08 * a)));
        let cy = fy + 23.0;
        if let Some(b) = &battery {
            let frac = (b.percent / 100.0).clamp(0.0, 1.0);
            let bx = x + 20.0;
            let bc = if b.charging || b.on_ac {
                rgba(120, 255, 160, 1.0)
            } else if frac < 0.2 {
                rgba(255, 90, 90, 1.0)
            } else {
                WHITE
            };
            f.scene.shape(Shape::stroke(bx, cy - 5.5, 20.0, 11.0, 3.5, 1.2, with_a(WHITE, 0.6 * a)));
            f.scene.shape(Shape::rect(bx + 20.5, cy - 2.0, 2.0, 4.0, 1.0, with_a(WHITE, 0.6 * a)));
            f.scene.shape(Shape::rect(bx + 2.0, cy - 3.5, (16.0 * frac).max(1.5), 7.0, 2.0, with_a(bc, a)));
            let label = format!("{:.0}%{}", b.percent, if b.charging { "  ·  charging" } else { "" });
            f.scene.text(Text::new(label, bx + 30.0, cy - 8.0, 12.0, with_a(WHITE, 0.85 * a)).weight(600));
            if input.clicked([x + 10.0, fy + 4.0, 160.0, 38.0]) {
                platform::open("ms-settings:batterysaver");
                self.cc_open = false;
            }
        }
        if f.icon_button(input, ic::SETTINGS, x + w - 30.0, cy, 12.5, with_a(WHITE, 0.85 * a)) {
            platform::open("ms-settings:");
            self.cc_open = false;
        }
        rect
    }


    /// The wallpaper dial: a curved carousel of thumbnails above the dock.
    fn draw_dial(&mut self, f: &mut Frame, input: &Input, pal: &Palette) -> Option<[f32; 4]> {
        let a = ease_out_cubic(self.dial_a);
        let w = 600.0f32.min(f.w - 16.0);
        let h = 196.0;
        let x = (self.dial_x - 60.0).clamp(8.0, f.w - w - 8.0);
        let y = TIP_H - h - 6.0 + 12.0 * (1.0 - a);
        let rect = [x, y, w, h];
        f.scene.shape(Shape::rect(x, y + 4.0, w, h, 22.0, [0.0, 0.0, 0.0, 0.32 * a]).soft(18.0));
        f.scene.shape(Shape::rect(x, y, w, h, 22.0, [0.06, 0.06, 0.09, 0.93 * a]));
        f.scene.shape(Shape::rect(x, y, w, h, 22.0, with_a(pal.primary, 0.10 * a)).grad(with_a(pal.secondary, 0.05 * a), 0.4));
        f.scene.shape(Shape::stroke(x, y, w, h, 22.0, 1.0, with_a(WHITE, 0.22 * a)));
        f.scene.text(Text::new("Wallpapers", x + 18.0, y + 12.0, 13.0, with_a(WHITE, 0.95 * a)).weight(700));
        // open folder
        let fx = x + w - 28.0;
        if f.icon_button(input, ic::FOLDER, fx, y + 22.0, 11.0, with_a(WHITE, 0.85 * a)) {
            let d = desktop::wallpaper_dir("");
            let _ = std::fs::create_dir_all(&d);
            platform::open(&d.to_string_lossy());
        }
        if self.walls.is_empty() {
            f.scene.text(
                Text::new("Add images to Pictures\\Wallpapers", x + w / 2.0, y + h / 2.0 - 8.0, 12.5, with_a(WHITE, 0.7 * a)).center(),
            );
            return Some(rect);
        }
        let n = self.walls.len() as i64;
        if input.over(rect) && input.scroll.abs() > 0.01 {
            self.turn_dial(if input.scroll > 0.0 { -1 } else { 1 });
        }
        let cx = x + w / 2.0;
        let base_y = y + 112.0;
        let pos = self.dial_pos;
        let center_i = pos.round() as i64;
        // draw far ones first so the centre sits on top
        let mut order: Vec<i64> = (center_i - 3..=center_i + 3).collect();
        order.sort_by(|p, q| (*q as f32 - pos).abs().partial_cmp(&(*p as f32 - pos).abs()).unwrap());
        let mut clicked: Option<i64> = None;
        for i in order {
            let d = i as f32 - pos;
            // with only a few wallpapers, show each one once instead of repeating it
            let off = i - center_i;
            if d.abs() > 2.9 || off < -((n - 1) / 2) || off > n / 2 {
                continue;
            }
            let scale = 1.0 - 0.22 * d.abs().min(2.5);
            let tw = 168.0 * scale;
            let th = tw * 9.0 / 16.0;
            // laid out along a shallow arc, like a dial
            let tx = cx + d * 118.0 - d.signum() * d.abs().powf(1.5) * 8.0 - tw / 2.0;
            let ty = base_y - th / 2.0 + d * d * 7.0;
            let fade = (1.0 - d.abs() / 3.0).clamp(0.0, 1.0) * a;
            let p = self.walls[i.rem_euclid(n) as usize].clone();
            let r = [tx, ty, tw, th];
            if d.abs() < 0.5 {
                f.scene.shape(Shape::glow(cx, base_y, tw * 0.62, with_a(pal.primary, 0.30 * a), 2.0));
            }
            match self.thumb(f, &p) {
                Some(k) => f.scene.image(ImageDraw::new(k, tx, ty, tw, th, 10.0 * scale).opacity(fade)),
                None => f.scene.shape(Shape::rect(tx, ty, tw, th, 10.0 * scale, with_a(WHITE, 0.08 * fade))),
            }
            let hov = input.over(r);
            let ring = if d.abs() < 0.5 { 0.85 } else if hov { 0.5 } else { 0.14 };
            f.scene.shape(Shape::stroke(tx, ty, tw, th, 10.0 * scale, if d.abs() < 0.5 { 1.6 } else { 1.0 }, with_a(WHITE, ring * fade)));
            if input.clicked(r) {
                clicked = Some(i);
            }
        }
        if let Some(i) = clicked {
            self.turn_dial(i - self.dial_target);
        }
        // name + position
        let cur = &self.walls[self.dial_target.rem_euclid(n) as usize];
        let name = cur.file_stem().map(|s| s.to_string_lossy().replace(['-', '_'], " ")).unwrap_or_default();
        let t = f.fit(Text::new(name, cx, y + h - 30.0, 12.0, with_a(WHITE, 0.9 * a)).weight(600).center(), w - 140.0);
        f.scene.text(t);
        f.scene.text(
            Text::new(format!("{} / {}", self.dial_target.rem_euclid(n) + 1, n), x + 18.0, y + h - 29.0, 11.0, with_a(WHITE, 0.5 * a))
                .family(Family::Mono),
        );
        // arrows
        if f.icon_button(input, '\u{E76B}', x + 26.0, base_y, 11.0, with_a(WHITE, 0.85 * a)) {
            self.turn_dial(-1);
        }
        if f.icon_button(input, '\u{E76C}', x + w - 26.0, base_y, 11.0, with_a(WHITE, 0.85 * a)) {
            self.turn_dial(1);
        }
        // shuffle
        if f.icon_button(input, '\u{E8B1}', x + w - 30.0, y + h - 22.0, 10.5, with_a(WHITE, 0.75 * a)) && n > 1 {
            let r = (crate::anim::hash01(platform::unix_now() as u64 ^ self.dial_target as u64) * (n - 1) as f32) as i64 + 1;
            self.turn_dial(r);
        }
        Some(rect)
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
