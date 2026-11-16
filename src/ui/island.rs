//! The Dynamic Island: a black pill at the top of the screen that morphs between
//! compact, peek, HUD, notification and an expanded control centre.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::anim::*;
use crate::config::Config;
use crate::gfx::*;
use crate::platform;
use crate::services::web::weather_desc;
use crate::state::*;
use crate::ui::{app_colors, fmt_bytes, fmt_bytes_rate, ic, Frame, Input, Key};

pub const WIN_W: f32 = 780.0;
pub const WIN_H: f32 = 440.0;
const TOP: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Compact,
    Peek,
    Expanded,
    Hud,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Media,
    System,
    Tools,
    Social,
}

pub enum IslandAction {
    None,
    ToggleWidget,
    Fullscreen,
    WantFocus(bool),
}

enum Res {
    Calc(String),
    App(String, String),
    Web(String),
    Run(String),
}

pub struct Island {
    w: Spring,
    h: Spring,
    r: Spring,
    yoff: Spring,
    bubble: Spring,
    mode: Mode,
    mode_since: f32,
    hover_since: Option<f32>,
    leave_since: Option<f32>,
    tab: Tab,
    hud: Option<(Hud, f32)>,
    pub launcher: bool,
    query: String,
    sel: usize,
    timer_end: Option<Instant>,
    timer_total: f32,
    timer_label: String,
    stopwatch: Option<Instant>,
    pub focus: bool,
    shelf: Vec<PathBuf>,
    social_seen: std::collections::HashMap<String, Instant>,
    /// Interactive area in logical window coords (for click-through toggling).
    pub hit: Option<[f32; 4]>,
    pub hidden: bool,
    pub lyrics_widget_on: bool,
    pub widget_style_name: &'static str,
    caret_t: f32,
}

impl Island {
    pub fn new() -> Self {
        Island {
            w: Spring::with(170.0, 420.0, 30.0),
            h: Spring::with(34.0, 420.0, 30.0),
            r: Spring::with(17.0, 420.0, 30.0),
            yoff: Spring::with(0.0, 200.0, 24.0),
            bubble: Spring::with(0.0, 300.0, 22.0),
            mode: Mode::Compact,
            mode_since: 0.0,
            hover_since: None,
            leave_since: None,
            tab: Tab::Media,
            hud: None,
            launcher: false,
            query: String::new(),
            sel: 0,
            timer_end: None,
            timer_total: 0.0,
            timer_label: String::new(),
            stopwatch: None,
            focus: false,
            shelf: vec![],
            social_seen: Default::default(),
            hit: None,
            hidden: false,
            lyrics_widget_on: true,
            widget_style_name: "",
            caret_t: 0.0,
        }
    }

    pub fn open_launcher(&mut self, t: f32) {
        self.set_mode(Mode::Expanded, t);
        self.launcher = true;
        self.query.clear();
        self.sel = 0;
    }

    pub fn close(&mut self, t: f32) {
        self.launcher = false;
        self.query.clear();
        self.set_mode(Mode::Compact, t);
    }

    fn set_mode(&mut self, m: Mode, t: f32) {
        if self.mode != m {
            self.mode = m;
            self.mode_since = t;
        }
    }

    fn timer_remaining(&self) -> Option<f32> {
        self.timer_end.map(|e| e.saturating_duration_since(Instant::now()).as_secs_f32())
    }

    fn start_timer(&mut self, mins: f32, label: &str) {
        self.timer_end = Some(Instant::now() + Duration::from_secs_f32(mins * 60.0));
        self.timer_total = mins * 60.0;
        self.timer_label = label.to_string();
        self.stopwatch = None;
    }

    pub fn draw(&mut self, f: &mut Frame, input: &Input, shared: &Shared, cfg: &Config) -> IslandAction {
        let mut action = IslandAction::None;
        let t = f.t;
        let media = shared.media.read().clone();
        let audio = shared.audio.lock().clone();
        let pal = media.palette();
        let was_launcher = self.launcher;

        // ---- timers
        if let Some(rem) = self.timer_remaining() {
            if rem <= 0.0 {
                self.timer_end = None;
                platform::beep();
                let label = if self.timer_label.is_empty() { "Timer".to_string() } else { self.timer_label.clone() };
                self.hud = Some((Hud::TimerDone(label), t + 5.0));
            }
        }

        // ---- HUD queue
        loop {
            let next = shared.huds.lock().pop_front();
            let Some(h) = next else { break };
            if self.focus && matches!(h, Hud::Notification { .. }) {
                continue;
            }
            let dur = match h {
                Hud::Notification { .. } => 5.0,
                Hud::Volume { .. } | Hud::Brightness(_) => 1.6,
                _ => 3.0,
            };
            self.hud = Some((h, t + dur));
        }
        if let Some((_, until)) = &self.hud {
            if t > *until {
                self.hud = None;
            }
        }

        // ---- dropped files go to the shelf
        for p in &input.dropped {
            if !self.shelf.contains(p) {
                self.shelf.insert(0, p.clone());
            }
            self.hud = Some((Hud::Message("Added to shelf".into(), file_name(p)), t + 2.0));
        }
        self.shelf.truncate(12);

        // ---- mode state machine
        let pill = self.pill_rect(f.w);
        let inside = input.over([pill[0] - 6.0, pill[1] - 8.0, pill[2] + 12.0, pill[3] + 16.0]);
        if self.mode == Mode::Expanded {
            if inside || input.down {
                self.leave_since = None;
            } else if self.leave_since.is_none() {
                self.leave_since = Some(t);
            }
            let typing = self.launcher && !self.query.is_empty();
            let left_long = self.leave_since.map(|l| t - l > if self.launcher { 9e9 } else { 0.55 }).unwrap_or(false);
            let clicked_outside = input.pressed && !inside;
            if (left_long && !typing) || clicked_outside || input.key(Key::Esc) {
                self.close(t);
            }
        } else {
            if inside {
                self.hover_since.get_or_insert(t);
            } else {
                self.hover_since = None;
            }
            let hovering = self.hover_since.map(|h| t - h > 0.16).unwrap_or(false);
            let m = if self.hud.is_some() && !hovering {
                Mode::Hud
            } else if hovering || input.hovering_file {
                Mode::Peek
            } else {
                Mode::Compact
            };
            self.set_mode(m, t);
        }
        if self.hidden && self.mode != Mode::Expanded {
            self.set_mode(Mode::Compact, t);
        }

        // ---- target geometry
        let (tw, th, tr) = self.target_size(f, &media, cfg);
        self.w.set(tw);
        self.h.set(th);
        self.r.set(tr);
        self.yoff.set(if self.hidden && self.mode != Mode::Expanded { -th - 20.0 } else { 0.0 });
        let dt = f.dt.min(0.05);
        let w = self.w.step(dt);
        let h = self.h.step(dt);
        let r = self.r.step(dt).min(h / 2.0);
        let yo = self.yoff.step(dt);
        let x = (f.w - w) / 2.0;
        let y = TOP + yo;

        // bubble (secondary live activity)
        let has_bubble = self.mode == Mode::Compact
            && (self.timer_end.is_some() && media.active
                || self.stopwatch.is_some() && media.active
                || !shared.misc.read().downloads.is_empty());
        self.bubble.set(if has_bubble && !self.hidden { 1.0 } else { 0.0 });
        let bs = self.bubble.step(dt).max(0.0);

        // ---- draw pill
        let rect = [x, y, w, h];
        f.scene.halo_shape(Shape::rect(x, y, w, h, r, with_a(pal.primary, if media.playing { 0.10 + audio.pulse() * 0.12 } else { 0.0 })));
        f.scene.shape(Shape::rect(x, y + 3.0, w, h, r, [0.0, 0.0, 0.0, 0.35]).soft(14.0));
        f.scene.shape(Shape::rect(x, y, w, h, r, [0.0, 0.0, 0.0, 1.0]));
        if self.mode == Mode::Expanded {
            f.scene.shape(Shape::stroke(x, y, w, h, r, 1.0, [1.0, 1.0, 1.0, 0.07]));
        }
        let content_a = smooth(0.05, 0.22, t - self.mode_since)
            * smooth(0.6, 0.95, (w / tw).min(tw / w) * (h / th).min(th / h));
        f.scene.layer();
        match self.mode {
            Mode::Compact => self.compact(f, input, shared, &media, &audio, &pal, rect, content_a.max(0.35)),
            Mode::Peek => {
                if input.hovering_file {
                    self.drop_hint(f, rect, content_a);
                } else if let Some(a) = self.peek(f, input, shared, &media, &pal, rect, content_a) {
                    action = a;
                }
            }
            Mode::Hud => self.hud_view(f, input, shared, &pal, rect, content_a, cfg),
            Mode::Expanded => {
                if let Some(a) = self.expanded(f, input, shared, &media, &audio, &pal, rect, content_a, cfg) {
                    action = a;
                }
            }
        }

        // click to expand
        if self.mode != Mode::Expanded && input.clicked(rect) && matches!(action, IslandAction::None) {
            if let Mode::Hud = self.mode {
                if let Some((Hud::Notification { app, .. }, _)) = &self.hud {
                    if let Some(s) = cfg.socials.iter().find(|s| app.to_lowercase().contains(&s.match_app)) {
                        platform::open(&s.url);
                        self.social_seen.insert(s.name.clone(), Instant::now());
                    }
                    self.hud = None;
                }
            } else {
                self.set_mode(Mode::Expanded, t);
                self.leave_since = None;
            }
        }

        // bubble
        let mut hit = rect;
        if bs > 0.01 {
            let d = 34.0 * bs;
            let bx = x + w + 7.0;
            let by = y + (h - d) / 2.0;
            f.scene.shape(Shape::rect(bx, by, d, d, d / 2.0, [0.0, 0.0, 0.0, 1.0]));
            let (cx, cy) = (bx + d / 2.0, by + d / 2.0);
            if let Some(rem) = self.timer_remaining() {
                let p = rem / self.timer_total.max(1.0);
                f.scene.glow_shape(Shape::arc(cx, cy, d * 0.36, 3.0, p, rgba(255, 159, 10, bs)));
            } else if let Some(sw) = self.stopwatch {
                let p = (sw.elapsed().as_secs_f32() % 60.0) / 60.0;
                f.scene.glow_shape(Shape::arc(cx, cy, d * 0.36, 3.0, p, rgba(255, 159, 10, bs)));
            } else {
                let p = (t * 0.8).fract();
                f.scene.shape(Shape::arc(cx, cy, d * 0.36, 3.0, 0.3, rgba(10, 132, 255, bs)).start_angle(p * std::f32::consts::TAU));
                f.icon(ic::DOWNLOAD, cx, cy, 11.0 * bs, rgba(10, 132, 255, bs));
            }
            hit = [x, y, w + 7.0 + d, h];
        }
        self.hit = if self.hidden && self.mode != Mode::Expanded {
            None
        } else {
            Some([hit[0] - 4.0, hit[1] - 6.0, hit[2] + 8.0, hit[3] + 12.0])
        };

        if self.launcher != was_launcher {
            action = IslandAction::WantFocus(self.launcher);
        }
        action
    }

    fn pill_rect(&self, win_w: f32) -> [f32; 4] {
        let w = self.w.value;
        [(win_w - w) / 2.0, TOP + self.yoff.value, w, self.h.value]
    }

    fn target_size(&mut self, f: &mut Frame, media: &MediaState, _cfg: &Config) -> (f32, f32, f32) {
        match self.mode {
            Mode::Compact => {
                let w = if media.active {
                    252.0
                } else if self.timer_end.is_some() || self.stopwatch.is_some() {
                    220.0
                } else {
                    176.0
                };
                (w, 34.0, 17.0)
            }
            Mode::Peek => {
                if media.active {
                    (450.0, 100.0, 34.0)
                } else {
                    (400.0, 92.0, 32.0)
                }
            }
            Mode::Expanded => (740.0, 400.0, 40.0),
            Mode::Hud => match &self.hud {
                Some((Hud::Notification { .. }, _)) => (440.0, 78.0, 30.0),
                Some((Hud::Volume { .. }, _)) | Some((Hud::Brightness(_), _)) => (340.0, 44.0, 22.0),
                Some((h, _)) => {
                    let (a, b) = hud_text(h);
                    let ta = Text::new(a, 0.0, 0.0, 13.0, WHITE).weight(600);
                    let tb = Text::new(b, 0.0, 0.0, 13.0, WHITE);
                    let w = f.measure(&ta).0 + f.measure(&tb).0 + 96.0;
                    (w.clamp(240.0, 520.0), 44.0, 22.0)
                }
                None => (176.0, 34.0, 17.0),
            },
        }
    }

    // ------------------------------------------------------------- compact

    #[allow(clippy::too_many_arguments)]
    fn compact(&mut self, f: &mut Frame, _input: &Input, shared: &Shared, media: &MediaState, audio: &AudioState, pal: &Palette, rect: [f32; 4], a: f32) {
        let [x, y, w, h] = rect;
        let cy = y + h / 2.0;
        let misc = shared.misc.read().clone();
        if media.active {
            if let Some(art) = &media.art {
                let (k, _) = f.ensure_art(art);
                f.scene.image(ImageDraw::new(k, x + 7.0, y + 6.0, 22.0, 22.0, 7.0).opacity(a));
            } else {
                f.icon(ic::MUSIC, x + 18.0, cy, 12.0, with_a(pal.primary, a));
            }
            if self.focus {
                f.icon(ic::MOON, x + 42.0, cy, 11.0, with_a(rgba(191, 90, 242, 1.0), a));
            }
            // visualiser
            let n = 5;
            let bw = 3.2;
            let gap = 2.6;
            let total = n as f32 * bw + (n - 1) as f32 * gap;
            let bx0 = x + w - 16.0 - total;
            for k in 0..n {
                let lo = k * BANDS / n / 2 + 1;
                let hi = (k + 1) * BANDS / n / 2 + 2;
                let v: f32 = audio.bands[lo..hi].iter().cloned().fold(0.0, f32::max);
                let v = if media.playing { v } else { 0.0 };
                let bh = 3.5 + v * 15.0;
                let bx = bx0 + k as f32 * (bw + gap);
                let c0 = mix(pal.primary, pal.secondary, k as f32 / (n - 1) as f32);
                f.scene.glow_shape(Shape::rect(bx, cy - bh / 2.0, bw, bh, bw / 2.0, with_a(c0, a)));
            }
            // timer text in the middle if there's room
        } else if let Some(rem) = self.timer_remaining() {
            f.icon(ic::TIMER, x + 20.0, cy, 13.0, with_a(rgba(255, 159, 10, 1.0), a));
            let p = rem / self.timer_total.max(1.0);
            f.scene.shape(Shape::arc(x + 20.0, cy, 11.0, 2.0, p, rgba(255, 159, 10, a * 0.8)));
            f.scene.text(Text::new(fmt_time(rem as f64), x + w - 16.0, cy - 9.0, 14.5, rgba(255, 159, 10, a)).weight(600).right().family(Family::Display));
        } else if let Some(sw) = self.stopwatch {
            f.icon(ic::STOPWATCH, x + 20.0, cy, 13.0, with_a(rgba(255, 159, 10, 1.0), a));
            f.scene.text(Text::new(fmt_time(sw.elapsed().as_secs_f64()), x + w - 16.0, cy - 9.0, 14.5, rgba(255, 159, 10, a)).weight(600).right());
        } else {
            let lt = platform::local_time();
            let left = if self.focus { 36.0 } else { 16.0 };
            if self.focus {
                f.icon(ic::MOON, x + 20.0, cy, 12.0, with_a(rgba(191, 90, 242, 1.0), a));
            }
            f.scene.text(
                Text::new(format!("{}:{:02}", lt.hour, lt.minute), x + left, cy - 9.5, 14.5, with_a(WHITE, a))
                    .weight(600)
                    .family(Family::Display),
            );
            let right = if let Some(wt) = &misc.weather {
                format!("{:.0}°", wt.temp)
            } else if let Some(b) = &shared.sys.read().battery {
                format!("{:.0}%", b.percent)
            } else {
                String::new()
            };
            f.scene.text(Text::new(right, x + w - 16.0, cy - 9.0, 13.5, with_a(WHITE, 0.7 * a)).right());
        }
        // privacy indicators
        let mut dx = x + w - 7.0;
        if misc.cam_in_use.is_some() {
            f.scene.glow_shape(Shape::glow(dx - 3.0, y + 8.0, 3.5, rgba(48, 209, 88, 1.0), 0.6));
            dx -= 9.0;
        }
        if misc.mic_in_use.is_some() {
            f.scene.glow_shape(Shape::glow(dx - 3.0, y + 8.0, 3.5, rgba(255, 159, 10, 1.0), 0.6));
        }
    }

    fn drop_hint(&mut self, f: &mut Frame, rect: [f32; 4], a: f32) {
        let [x, y, w, h] = rect;
        f.scene.shape(Shape::stroke(x + 10.0, y + 10.0, w - 20.0, h - 20.0, 22.0, 1.5, rgba(10, 132, 255, 0.8 * a)));
        f.icon(ic::FOLDER, x + 44.0, y + h / 2.0, 20.0, rgba(10, 132, 255, a));
        f.scene.text(Text::new("Drop to add to shelf", x + 70.0, y + h / 2.0 - 10.0, 15.0, with_a(WHITE, a)).weight(600));
    }

    // ---------------------------------------------------------------- peek

    #[allow(clippy::too_many_arguments)]
    fn peek(&mut self, f: &mut Frame, input: &Input, shared: &Shared, media: &MediaState, pal: &Palette, rect: [f32; 4], a: f32) -> Option<IslandAction> {
        let [x, y, w, h] = rect;
        if media.active {
            let art_s = 66.0;
            if let Some(art) = &media.art {
                let (k, _) = f.ensure_art(art);
                f.scene.image(ImageDraw::new(k, x + 17.0, y + 17.0, art_s, art_s, 15.0).opacity(a));
            } else {
                f.scene.shape(Shape::rect(x + 17.0, y + 17.0, art_s, art_s, 15.0, with_a(pal.primary, 0.3 * a)));
                f.icon(ic::MUSIC, x + 17.0 + art_s / 2.0, y + 17.0 + art_s / 2.0, 22.0, with_a(WHITE, a));
            }
            let tx = x + 17.0 + art_s + 14.0;
            let ctrl_w = 120.0;
            let tw = w - (tx - x) - ctrl_w - 10.0;
            let title = f.fit(Text::new(&media.title, tx, y + 20.0, 15.5, with_a(WHITE, a)).weight(600), tw);
            f.scene.text(title);
            let artist = f.fit(Text::new(&media.artist, tx, y + 41.0, 13.0, with_a(WHITE, 0.6 * a)), tw);
            f.scene.text(artist);
            // current lyric line as a teaser
            let ly = shared.lyrics.read();
            let lyric = ly.line_at(shared.lyric_pos()).map(|i| ly.lines[i].text.clone()).unwrap_or_default();
            drop(ly);
            if !lyric.is_empty() {
                let lt = f.fit(Text::new(lyric, tx, y + 60.0, 12.0, with_a(mix(pal.primary, WHITE, 0.35), a)).italic(true), tw);
                f.scene.text(lt);
            }
            // progress
            if media.duration > 0.0 {
                let bx = tx;
                let bw = w - (bx - x) - 22.0;
                let frac = (media.pos_now() / media.duration) as f32;
                f.bar(bx, y + h - 18.0, bw, 3.0, frac, with_a(pal.primary, a), with_a(pal.secondary, a));
            }
            let cy = y + 38.0;
            let cx = x + w - 66.0;
            if f.icon_button(input, ic::PREV, cx - 36.0, cy, 14.0, with_a(WHITE, a)) {
                shared.media_cmd(MediaCmd::Prev);
                return Some(IslandAction::None);
            }
            let pp = if media.playing { ic::PAUSE } else { ic::PLAY };
            if f.icon_button(input, pp, cx, cy, 18.0, with_a(WHITE, a)) {
                shared.media_cmd(MediaCmd::PlayPause);
                return Some(IslandAction::None);
            }
            if f.icon_button(input, ic::NEXT, cx + 36.0, cy, 14.0, with_a(WHITE, a)) {
                shared.media_cmd(MediaCmd::Next);
                return Some(IslandAction::None);
            }
            // swallow clicks on controls area so the pill doesn't expand
            if input.clicked([cx - 56.0, cy - 20.0, 112.0, 40.0]) {
                return Some(IslandAction::None);
            }
        } else {
            let lt = platform::local_time();
            f.scene.text(
                Text::new(format!("{}:{:02}", lt.hour, lt.minute), x + 26.0, y + 14.0, 34.0, with_a(WHITE, a))
                    .weight(600)
                    .family(Family::Display),
            );
            let date = format!("{}, {} {}", platform::WEEKDAYS[lt.weekday as usize % 7], platform::MONTHS[(lt.month as usize + 11) % 12], lt.day);
            f.scene.text(Text::new(date, x + 28.0, y + 58.0, 13.0, with_a(WHITE, 0.6 * a)));
            let misc = shared.misc.read().clone();
            let rx = x + w - 24.0;
            if let Some(wt) = &misc.weather {
                weather_icon(f, rx - 150.0, y + 34.0, 14.0, wt.code, wt.is_day, a);
                f.scene.text(Text::new(format!("{:.0}°", wt.temp), rx, y + 14.0, 26.0, with_a(WHITE, a)).right().weight(500));
                f.scene.text(Text::new(weather_desc(wt.code), rx, y + 50.0, 12.0, with_a(WHITE, 0.6 * a)).right());
            }
            if let Some(ev) = misc.events.first() {
                let s = format!("{} · {}", event_when(ev), ev.title);
                let t = f.fit(Text::new(s, rx, y + 68.0, 11.5, with_a(rgba(255, 69, 58, 1.0), 0.9 * a)).right(), w * 0.45);
                f.scene.text(t);
            }
        }
        None
    }

    // ----------------------------------------------------------------- HUD

    #[allow(clippy::too_many_arguments)]
    fn hud_view(&mut self, f: &mut Frame, _input: &Input, shared: &Shared, pal: &Palette, rect: [f32; 4], a: f32, cfg: &Config) {
        let [x, y, w, h] = rect;
        let cy = y + h / 2.0;
        let Some((hud, _)) = self.hud.clone() else { return };
        match hud {
            Hud::Volume { level, muted } => {
                let g = if muted || level < 0.01 { ic::MUTE } else if level < 0.4 { ic::VOLUME1 } else { ic::VOLUME };
                f.icon(g, x + 26.0, cy, 15.0, with_a(WHITE, a));
                let bx = x + 50.0;
                let bw = w - 50.0 - 58.0;
                let lv = if muted { 0.0 } else { level };
                f.bar(bx, cy - 3.0, bw, 6.0, lv, with_a(WHITE, a), with_a(WHITE, a));
                f.scene.text(Text::new(format!("{:.0}", lv * 100.0), x + w - 22.0, cy - 9.0, 14.0, with_a(WHITE, 0.8 * a)).right().weight(600));
            }
            Hud::Brightness(b) => {
                f.icon(ic::BRIGHT, x + 26.0, cy, 15.0, with_a(rgba(255, 214, 10, 1.0), a));
                let bx = x + 50.0;
                let bw = w - 50.0 - 58.0;
                f.bar(bx, cy - 3.0, bw, 6.0, b, rgba(255, 214, 10, a), rgba(255, 159, 10, a));
                f.scene.text(Text::new(format!("{:.0}", b * 100.0), x + w - 22.0, cy - 9.0, 14.0, with_a(WHITE, 0.8 * a)).right().weight(600));
            }
            Hud::Notification { app, title, body } => {
                let (c0, c1) = app_colors(&app);
                let bx = x + 20.0 + 21.0;
                f.scene.shape(Shape::rect(bx - 21.0, cy - 21.0, 42.0, 42.0, 21.0, with_a(c0, a)).grad(with_a(c1, a), 0.8));
                let ini = app.chars().next().unwrap_or('•').to_uppercase().to_string();
                f.scene.text(Text::new(ini, bx, cy - 12.0, 18.0, with_a(WHITE, a)).center().weight(700));
                let tx = x + 76.0;
                let tw = w - 76.0 - 22.0;
                let at = f.fit(Text::new(app.clone(), tx, y + 12.0, 11.5, with_a(WHITE, 0.5 * a)), tw);
                f.scene.text(at);
                let tt = f.fit(Text::new(title, tx, y + 27.0, 14.0, with_a(WHITE, a)).weight(600), tw);
                f.scene.text(tt);
                let bt = f.fit(Text::new(body, tx, y + 47.0, 12.5, with_a(WHITE, 0.7 * a)), tw);
                f.scene.text(bt);
                let _ = cfg;
            }
            Hud::Battery { percent, charging } => {
                let col = if charging { rgba(48, 209, 88, 1.0) } else if percent <= 20.0 { rgba(255, 69, 58, 1.0) } else { WHITE };
                battery_icon(f, x + 16.0, cy - 7.0, percent / 100.0, col, a, charging);
                let (l, r) = hud_text(&Hud::Battery { percent, charging });
                f.scene.text(Text::new(l, x + 58.0, cy - 9.0, 13.0, with_a(WHITE, a)).weight(600));
                f.scene.text(Text::new(r, x + w - 20.0, cy - 9.0, 13.0, with_a(col, a)).right().weight(600));
            }
            other => {
                let (glyph, col) = match &other {
                    Hud::Wifi(Some(_)) => (ic::WIFI, rgba(10, 132, 255, 1.0)),
                    Hud::Wifi(None) => (ic::WIFI_OFF, rgba(255, 69, 58, 1.0)),
                    Hud::Bluetooth { .. } => (ic::BT, rgba(10, 132, 255, 1.0)),
                    Hud::TimerDone(_) => (ic::TIMER, rgba(255, 159, 10, 1.0)),
                    Hud::Download(_) => (ic::DOWNLOAD, rgba(48, 209, 88, 1.0)),
                    Hud::Copied(_) => (ic::CLIPBOARD, pal.primary),
                    _ => (ic::BELL, pal.primary),
                };
                let pulse = if matches!(other, Hud::TimerDone(_)) { 0.5 + 0.5 * (f.t * 8.0).sin() } else { 1.0 };
                f.icon(glyph, x + 26.0, cy, 15.0, with_a(col, a * pulse));
                let (l, r) = hud_text(&other);
                f.scene.text(Text::new(l, x + 50.0, cy - 9.0, 13.0, with_a(WHITE, a)).weight(600));
                let rt = f.fit(Text::new(r, x + w - 20.0, cy - 9.0, 13.0, with_a(WHITE, 0.6 * a)).right(), w * 0.55);
                f.scene.text(rt);
            }
        }
        let _ = shared;
    }

    // ------------------------------------------------------------ expanded

    #[allow(clippy::too_many_arguments)]
    fn expanded(
        &mut self,
        f: &mut Frame,
        input: &Input,
        shared: &Shared,
        media: &MediaState,
        audio: &AudioState,
        pal: &Palette,
        rect: [f32; 4],
        a: f32,
        cfg: &Config,
    ) -> Option<IslandAction> {
        let [x, y, w, h] = rect;
        let mut act = None;
        // tabs
        let mut tx = x + 22.0;
        let ty = y + 18.0;
        for (tab, name) in [(Tab::Media, "Now Playing"), (Tab::System, "System"), (Tab::Tools, "Tools"), (Tab::Social, "Social")] {
            let (c, bw) = f.pill(input, name, tx, ty, 30.0, self.tab == tab && !self.launcher_typing(), with_a(WHITE, a));
            if c {
                self.tab = tab;
                self.launcher = false;
                self.query.clear();
            }
            tx += bw + 6.0;
        }
        // search box
        let sw = 230.0;
        let sx = x + w - 22.0 - sw;
        let srect = [sx, ty, sw, 30.0];
        let hov = input.over(srect);
        f.scene.shape(Shape::rect(sx, ty, sw, 30.0, 15.0, [1.0, 1.0, 1.0, if self.launcher { 0.14 } else if hov { 0.11 } else { 0.07 }]));
        if self.launcher {
            f.scene.shape(Shape::stroke(sx, ty, sw, 30.0, 15.0, 1.0, with_a(pal.primary, 0.7 * a)));
        }
        f.icon(ic::SEARCH, sx + 16.0, ty + 15.0, 12.0, with_a(WHITE, 0.6 * a));
        if input.clicked(srect) {
            self.launcher = true;
        }
        if self.launcher {
            self.caret_t += f.dt;
            for c in input.text.chars() {
                if !c.is_control() {
                    self.query.push(c);
                    self.sel = 0;
                    self.caret_t = 0.0;
                }
            }
            if input.key(Key::Back) {
                self.query.pop();
                self.sel = 0;
                self.caret_t = 0.0;
            }
        }
        let shown = if self.query.is_empty() { "Search apps, web, math, >cmd".to_string() } else { self.query.clone() };
        let qa = if self.query.is_empty() { 0.4 } else { 0.95 };
        let qt = Text::new(shown, sx + 32.0, ty + 7.0, 13.0, with_a(WHITE, qa * a));
        let qt = f.fit(qt, sw - 44.0);
        let qw = if self.query.is_empty() { 0.0 } else { f.measure(&qt).0 };
        f.scene.text(qt);
        if self.launcher && (self.caret_t * 2.0) as i32 % 2 == 0 {
            f.scene.shape(Shape::rect(sx + 32.0 + qw + 1.0, ty + 8.0, 1.5, 15.0, 0.7, with_a(WHITE, 0.9 * a)));
        }

        let area = [x + 22.0, y + 64.0, w - 44.0, h - 64.0 - 20.0];
        if self.launcher_typing() {
            if let Some(a2) = self.launcher_results(f, input, shared, area, a) {
                act = Some(a2);
            }
            return act;
        }
        match self.tab {
            Tab::Media => act = self.tab_media(f, input, shared, media, audio, pal, area, a),
            Tab::System => self.tab_system(f, shared, pal, area, a),
            Tab::Tools => self.tab_tools(f, input, shared, pal, area, a, cfg),
            Tab::Social => self.tab_social(f, input, shared, area, a, cfg),
        }
        act
    }

    fn launcher_typing(&self) -> bool {
        self.launcher && !self.query.is_empty()
    }

    fn results(&self, shared: &Shared) -> Vec<Res> {
        let q = self.query.trim();
        let mut out = vec![];
        if let Some(cmd) = q.strip_prefix('>') {
            out.push(Res::Run(cmd.trim().to_string()));
            return out;
        }
        if let Some(v) = calc(q) {
            let s = if (v - v.round()).abs() < 1e-9 && v.abs() < 1e15 { format!("{}", v.round() as i64) } else { format!("{:.6}", v).trim_end_matches('0').trim_end_matches('.').to_string() };
            out.push(Res::Calc(s));
        }
        let ql = q.to_lowercase();
        let misc = shared.misc.read();
        let mut scored: Vec<(i32, &(String, String))> = misc
            .apps
            .iter()
            .filter_map(|app| {
                let n = app.0.to_lowercase();
                let score = if n.starts_with(&ql) {
                    100
                } else if n.split_whitespace().any(|w| w.starts_with(&ql)) {
                    80
                } else if n.contains(&ql) {
                    60
                } else if subseq(&ql, &n) {
                    30
                } else {
                    return None;
                };
                Some((score - (n.len() as i32 / 4), app))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, app) in scored.into_iter().take(5) {
            out.push(Res::App(app.0.clone(), app.1.clone()));
        }
        out.push(Res::Web(q.to_string()));
        out
    }

    fn launcher_results(&mut self, f: &mut Frame, input: &Input, shared: &Shared, area: [f32; 4], a: f32) -> Option<IslandAction> {
        let res = self.results(shared);
        if input.key(Key::Down) {
            self.sel = (self.sel + 1).min(res.len().saturating_sub(1));
        }
        if input.key(Key::Up) {
            self.sel = self.sel.saturating_sub(1);
        }
        let mut run: Option<usize> = if input.key(Key::Enter) { Some(self.sel) } else { None };
        let rh = 42.0;
        for (i, r) in res.iter().enumerate() {
            let ry = area[1] + i as f32 * (rh + 4.0);
            if ry + rh > area[1] + area[3] {
                break;
            }
            let rr = [area[0], ry, area[2], rh];
            if input.over(rr) && input.moved {
                self.sel = i;
            }
            if i == self.sel {
                f.scene.shape(Shape::rect(rr[0], rr[1], rr[2], rr[3], 14.0, [1.0, 1.0, 1.0, 0.1 * a]));
            }
            let (g, title, sub) = match r {
                Res::Calc(v) => (ic::CALC, format!("= {v}"), "Enter to copy".to_string()),
                Res::App(n, _) => (ic::APP, n.clone(), "Application".to_string()),
                Res::Web(q) => (ic::GLOBE, format!("Search the web for “{q}”"), "Default browser".to_string()),
                Res::Run(c) => (ic::CMD, format!("Run: {c}"), "Command".to_string()),
            };
            f.icon(g, rr[0] + 24.0, ry + rh / 2.0, 15.0, with_a(WHITE, 0.85 * a));
            let tt = f.fit(Text::new(title, rr[0] + 50.0, ry + 6.0, 14.0, with_a(WHITE, a)).weight(600), rr[2] - 70.0);
            f.scene.text(tt);
            f.scene.text(Text::new(sub, rr[0] + 50.0, ry + 24.0, 11.0, with_a(WHITE, 0.5 * a)));
            if input.clicked(rr) {
                run = Some(i);
            }
        }
        if let Some(i) = run {
            if let Some(r) = res.get(i) {
                match r {
                    Res::Calc(v) => {
                        platform::set_clipboard(v);
                        shared.hud(Hud::Copied(v.clone()));
                    }
                    Res::App(_, p) => platform::open(p),
                    Res::Web(q) => platform::open(&format!("https://www.google.com/search?q={}", url_enc(q))),
                    Res::Run(c) => platform::open_with_args("cmd.exe", &format!("/c start \"\" {c}")),
                }
                self.close(f.t);
                return Some(IslandAction::WantFocus(false));
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn tab_media(
        &mut self,
        f: &mut Frame,
        input: &Input,
        shared: &Shared,
        media: &MediaState,
        audio: &AudioState,
        pal: &Palette,
        area: [f32; 4],
        a: f32,
    ) -> Option<IslandAction> {
        let [x, y, w, h] = area;
        let mut act = None;
        if !media.active {
            f.icon(ic::MUSIC, x + w / 2.0, y + h / 2.0 - 34.0, 34.0, with_a(WHITE, 0.4 * a));
            f.scene.text(Text::new("Nothing playing", x + w / 2.0, y + h / 2.0, 18.0, with_a(WHITE, 0.9 * a)).center().weight(600));
            f.scene.text(Text::new("Play music in Spotify, a browser or any player", x + w / 2.0, y + h / 2.0 + 28.0, 13.0, with_a(WHITE, 0.5 * a)).center());
            return None;
        }
        let art_s = 176.0;
        if let Some(art) = &media.art {
            let (k, kb) = f.ensure_art(art);
            f.scene.image(ImageDraw::new(kb, x - 10.0, y - 4.0, art_s + 20.0, art_s + 20.0, 40.0).opacity(0.7 * a));
            f.scene.halo_shape(Shape::glow(x + art_s / 2.0, y + art_s / 2.0, art_s * 0.75, with_a(pal.primary, 0.25 + audio.pulse() * 0.25), 1.5));
            f.scene.image(ImageDraw::new(k, x, y + 4.0, art_s, art_s, 22.0).opacity(a));
        } else {
            f.scene.shape(Shape::rect(x, y + 4.0, art_s, art_s, 22.0, with_a(pal.primary, 0.25 * a)));
        }
        f.scene.layer();
        let tx = x + art_s + 26.0;
        let tw = w - art_s - 26.0;
        let t = f.fit(Text::new(&media.title, tx, y + 4.0, 22.0, with_a(WHITE, a)).weight(650).family(Family::Display), tw);
        f.scene.text(t);
        let ar = f.fit(Text::new(&media.artist, tx, y + 36.0, 15.0, with_a(WHITE, 0.7 * a)), tw);
        f.scene.text(ar);
        let src = media.app.split(['!', '.']).next().unwrap_or("").to_string();
        let al = f.fit(Text::new(if media.album.is_empty() { src } else { media.album.clone() }, tx, y + 58.0, 12.5, with_a(WHITE, 0.45 * a)), tw);
        f.scene.text(al);

        // progress + seek
        let py = y + 92.0;
        if media.duration > 0.0 {
            let frac = (media.pos_now() / media.duration) as f32;
            let r = [tx, py - 8.0, tw, 20.0];
            let hv = input.over(r);
            f.bar(tx, py, tw, if hv { 6.0 } else { 4.0 }, frac, with_a(pal.primary, a), with_a(pal.secondary, a));
            if input.clicked(r) {
                if let Some((mx, _)) = input.mouse {
                    shared.media_cmd(MediaCmd::Seek(((mx - tx) / tw).clamp(0.0, 1.0) as f64 * media.duration));
                }
            }
            f.scene.text(Text::new(fmt_time(media.pos_now()), tx, py + 10.0, 11.0, with_a(WHITE, 0.5 * a)));
            f.scene.text(Text::new(format!("-{}", fmt_time(media.duration - media.pos_now())), tx + tw, py + 10.0, 11.0, with_a(WHITE, 0.5 * a)).right());
        }
        // controls
        let cy = y + 142.0;
        let cx = tx + tw / 2.0;
        if f.icon_button(input, ic::PREV, cx - 64.0, cy, 19.0, with_a(WHITE, a)) {
            shared.media_cmd(MediaCmd::Prev);
        }
        let pp = if media.playing { ic::PAUSE } else { ic::PLAY };
        if f.icon_button(input, pp, cx, cy, 27.0, with_a(WHITE, a)) {
            shared.media_cmd(MediaCmd::PlayPause);
        }
        if f.icon_button(input, ic::NEXT, cx + 64.0, cy, 19.0, with_a(WHITE, a)) {
            shared.media_cmd(MediaCmd::Next);
        }
        // lyric line
        let ly = shared.lyrics.read();
        let line = ly.line_at(shared.lyric_pos()).map(|i| ly.lines[i].text.clone()).unwrap_or_default();
        drop(ly);
        if !line.is_empty() {
            let lt = f.fit(Text::new(line, cx, y + 178.0, 13.5, with_a(mix(pal.primary, WHITE, 0.4), a)).center().italic(true), tw);
            f.scene.glow_text(lt);
        }
        // action pills
        let by = y + h - 34.0;
        let mut bx = x;
        let (c1, w1) = f.pill(input, if self.lyrics_widget_on { "Lyrics widget: on" } else { "Lyrics widget: off" }, bx, by, 30.0, self.lyrics_widget_on, with_a(pal.primary, a));
        if c1 {
            act = Some(IslandAction::ToggleWidget);
        }
        bx += w1 + 8.0;
        let (c2, w2) = f.pill(input, "Fullscreen lyrics", bx, by, 30.0, false, pal.primary);
        if c2 {
            act = Some(IslandAction::Fullscreen);
            self.close(f.t);
        }
        bx += w2 + 14.0;
        // spectrum
        let vw = x + w - bx;
        let n = 40;
        let bw = vw / n as f32;
        for k in 0..n {
            let v = audio.bands[k * BANDS / n];
            let bh = 2.0 + v * 26.0;
            let c = mix(pal.primary, pal.secondary, k as f32 / n as f32);
            f.scene.glow_shape(Shape::rect(bx + k as f32 * bw, by + 30.0 - bh, bw * 0.62, bh, bw * 0.3, with_a(c, 0.9 * a)));
        }
        act
    }

    fn tab_system(&mut self, f: &mut Frame, shared: &Shared, pal: &Palette, area: [f32; 4], a: f32) {
        let s = shared.sys.read().clone();
        let misc = shared.misc.read().clone();
        let [x, y, w, h] = area;
        let ring = |f: &mut Frame, cx: f32, cy: f32, frac: f32, label: &str, value: String, c0: Rgba, c1: Rgba| {
            let r = 40.0;
            f.scene.shape(Shape::arc(cx, cy, r, 7.0, 1.0, [1.0, 1.0, 1.0, 0.08 * a]));
            let mut arc = Shape::arc(cx, cy, r, 7.0, frac.clamp(0.0, 1.0), with_a(c0, a));
            arc.c1 = with_a(c1, a);
            f.scene.glow_shape(arc);
            f.scene.text(Text::new(value, cx, cy - 11.0, 17.0, with_a(WHITE, a)).center().weight(650).family(Family::Display));
            f.scene.text(Text::new(label, cx, cy + r + 8.0, 11.5, with_a(WHITE, 0.55 * a)).center());
        };
        let heat = |v: f32| -> (Rgba, Rgba) {
            if v > 0.85 {
                (rgba(255, 69, 58, 1.0), rgba(255, 159, 10, 1.0))
            } else if v > 0.6 {
                (rgba(255, 159, 10, 1.0), rgba(255, 214, 10, 1.0))
            } else {
                (pal.primary, pal.secondary)
            }
        };
        let rw = w * 0.56;
        let cols = 4.0;
        let ry = y + 50.0;
        let mut i = 0.0;
        let mut put = |f: &mut Frame, frac: f32, label: &str, value: String| {
            let cx = x + rw / cols * (i + 0.5);
            let (c0, c1) = heat(frac);
            ring(f, cx, ry, frac, label, value, c0, c1);
            i += 1.0;
        };
        put(f, s.cpu / 100.0, "CPU", format!("{:.0}%", s.cpu));
        let ram = if s.ram_total > 0.0 { (s.ram_used / s.ram_total) as f32 } else { 0.0 };
        put(f, ram, "Memory", format!("{:.0}%", ram * 100.0));
        match s.gpu {
            Some(g) => put(f, g / 100.0, "GPU", format!("{:.0}%", g)),
            None => put(f, 0.0, "GPU", "—".into()),
        }
        match s.cpu_temp {
            Some(tc) => put(f, tc / 100.0, "CPU temp", format!("{:.0}°", tc)),
            None => {
                let d = if s.disk_total > 0.0 { (s.disk_used / s.disk_total) as f32 } else { 0.0 };
                put(f, d, "Disk C:", format!("{:.0}%", d * 100.0));
            }
        }
        // CPU history sparkline under rings
        let sy = y + 128.0;
        let sh = 44.0;
        let sw = rw - 20.0;
        let hist: Vec<f32> = s.cpu_hist.iter().cloned().collect();
        if hist.len() > 1 {
            let n = hist.len();
            for k in 1..n {
                let x0 = x + 10.0 + sw * (k - 1) as f32 / 59.0;
                let x1 = x + 10.0 + sw * k as f32 / 59.0;
                let y0 = sy + sh - hist[k - 1] / 100.0 * sh;
                let y1 = sy + sh - hist[k] / 100.0 * sh;
                let mut l = Shape::line(x0, y0, x1, y1, 2.0, with_a(pal.primary, 0.9 * a));
                l.c1 = with_a(pal.secondary, 0.9 * a);
                f.scene.glow_shape(l);
            }
        }
        f.scene.text(Text::new("CPU · last 60s", x + 10.0, sy + sh + 6.0, 11.0, with_a(WHITE, 0.4 * a)));
        f.scene.text(Text::new(s.cpu_name.clone(), x + 10.0, sy + sh + 24.0, 11.0, with_a(WHITE, 0.4 * a)));
        if !s.lhm_ok {
            let t = Text::new(
                "Temps & fan speeds: run LibreHardwareMonitor → Options → Remote Web Server",
                x + 10.0,
                y + h - 18.0,
                11.0,
                rgba(255, 159, 10, 0.8 * a),
            );
            let t = f.fit(t, rw);
            f.scene.text(t);
        }

        // stats list on the right
        let lx = x + rw + 16.0;
        let lw = w - rw - 16.0;
        f.scene.shape(Shape::rect(lx - 2.0, y - 2.0, lw + 2.0, h + 4.0, 18.0, [1.0, 1.0, 1.0, 0.04 * a]));
        let mut rows: Vec<(char, String, String)> = vec![];
        rows.push((ic::CHIP, "Clock speed".into(), format!("{:.2} GHz", s.cpu_ghz)));
        rows.push((ic::CHIP, "RAM".into(), format!("{:.1} / {:.0} GB", s.ram_used, s.ram_total)));
        if let Some(t) = s.cpu_temp {
            rows.push((ic::CHIP, "CPU temp".into(), format!("{t:.0} °C")));
        }
        if let Some(t) = s.gpu_temp {
            rows.push((ic::CHIP, "GPU temp".into(), format!("{t:.0} °C")));
        }
        if let Some(p) = s.cpu_power {
            rows.push((ic::POWER, "CPU power".into(), format!("{p:.1} W")));
        }
        for (name, rpm) in s.fans.iter().take(3) {
            rows.push(('\u{E9CA}', name.clone(), format!("{rpm:.0} RPM")));
        }
        rows.push((ic::DOWNLOAD, "Network".into(), format!("↓ {}  ↑ {}", fmt_bytes_rate(s.net_down), fmt_bytes_rate(s.net_up))));
        if let Some(b) = &s.battery {
            rows.push((ic::POWER, "Battery".into(), format!("{:.0}%{}", b.percent, if b.on_ac { " · plugged in" } else { "" })));
        }
        for d in misc.bt_devices.iter().take(2) {
            rows.push((ic::BT, d.name.clone(), format!("{}%", d.battery)));
        }
        if let Some(wf) = &misc.wifi {
            rows.push((ic::WIFI, "Network".into(), wf.clone()));
        }
        for (name, cpu) in s.top_procs.iter().take(2) {
            rows.push((ic::APP, name.clone(), format!("{cpu:.0}% CPU")));
        }
        rows.push((ic::TIMER, "Uptime".into(), format!("{}h {}m", s.uptime / 3600, (s.uptime / 60) % 60)));
        let rh = 23.0;
        for (k, (g, l, v)) in rows.iter().enumerate() {
            let yy = y + 8.0 + k as f32 * rh;
            if yy + rh > y + h {
                break;
            }
            f.icon(*g, lx + 16.0, yy + 9.0, 11.0, with_a(WHITE, 0.45 * a));
            let lt = f.fit(Text::new(l.clone(), lx + 32.0, yy, 12.5, with_a(WHITE, 0.6 * a)), lw * 0.45);
            f.scene.text(lt);
            let vt = f.fit(Text::new(v.clone(), lx + lw - 12.0, yy, 12.5, with_a(WHITE, 0.95 * a)).right().weight(600), lw * 0.5);
            f.scene.text(vt);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tab_tools(&mut self, f: &mut Frame, input: &Input, shared: &Shared, pal: &Palette, area: [f32; 4], a: f32, cfg: &Config) {
        let [x, y, w, h] = area;
        let gap = 12.0;
        let cw = (w - gap * 3.0) / 4.0;
        let card = |f: &mut Frame, i: usize, title: &str, g: char| -> [f32; 4] {
            let cx = x + i as f32 * (cw + gap);
            f.scene.shape(Shape::rect(cx, y, cw, h, 20.0, [1.0, 1.0, 1.0, 0.055 * a]));
            f.icon(g, cx + 22.0, y + 22.0, 12.0, with_a(WHITE, 0.55 * a));
            f.scene.text(Text::new(title, cx + 38.0, y + 13.0, 12.5, with_a(WHITE, 0.7 * a)).weight(600));
            [cx, y, cw, h]
        };
        let orange = rgba(255, 159, 10, 1.0);

        // ---- Timer
        let r = card(f, 0, "Timer", ic::TIMER);
        let (ccx, ccy) = (r[0] + cw / 2.0, y + 92.0);
        let (txt, frac) = if let Some(rem) = self.timer_remaining() {
            (fmt_time(rem as f64), rem / self.timer_total.max(1.0))
        } else if let Some(sw) = self.stopwatch {
            (fmt_time(sw.elapsed().as_secs_f64()), (sw.elapsed().as_secs_f32() % 60.0) / 60.0)
        } else {
            ("0:00".into(), 0.0)
        };
        f.scene.shape(Shape::arc(ccx, ccy, 48.0, 5.0, 1.0, [1.0, 1.0, 1.0, 0.07 * a]));
        f.scene.glow_shape(Shape::arc(ccx, ccy, 48.0, 5.0, frac, with_a(orange, a)));
        f.scene.text(Text::new(txt, ccx, ccy - 13.0, 22.0, with_a(WHITE, a)).center().weight(600).family(Family::Display));
        let mut bx = r[0] + 12.0;
        let mut by = y + 156.0;
        for (label, mins) in [("5m", 5.0), ("15m", 15.0), (if cfg.pomodoro_minutes == 25 { "25m" } else { "Focus" }, cfg.pomodoro_minutes as f32)] {
            let (c, bw) = f.pill(input, label, bx, by, 26.0, false, orange);
            if c {
                self.start_timer(mins, if mins == cfg.pomodoro_minutes as f32 { "Pomodoro" } else { "Timer" });
            }
            bx += bw + 5.0;
        }
        by += 32.0;
        bx = r[0] + 12.0;
        let running = self.timer_end.is_some() || self.stopwatch.is_some();
        let (c, bw) = f.pill(input, if running { "Stop" } else { "Stopwatch" }, bx, by, 26.0, running, orange);
        if c {
            if running {
                self.timer_end = None;
                self.stopwatch = None;
            } else {
                self.stopwatch = Some(Instant::now());
            }
        }
        bx += bw + 5.0;
        let (c, _) = f.pill(input, "Focus", bx, by, 26.0, self.focus, rgba(191, 90, 242, 1.0));
        if c {
            self.focus = !self.focus;
            if self.focus && self.timer_end.is_none() {
                self.start_timer(cfg.pomodoro_minutes as f32, "Focus session");
            }
        }
        if self.focus {
            f.scene.text(Text::new("Notifications silenced", r[0] + cw / 2.0, y + h - 22.0, 11.0, rgba(191, 90, 242, a)).center());
        }

        // ---- Clipboard
        let r = card(f, 1, "Clipboard", ic::CLIPBOARD);
        let clip = shared.misc.read().clipboard.clone();
        if clip.is_empty() {
            f.scene.text(Text::new("Copy something…", r[0] + cw / 2.0, y + h / 2.0, 12.0, with_a(WHITE, 0.4 * a)).center());
        }
        for (k, c) in clip.iter().take(6).enumerate() {
            let ry = y + 42.0 + k as f32 * 36.0;
            let rr = [r[0] + 8.0, ry, cw - 16.0, 32.0];
            if input.over(rr) {
                f.scene.shape(Shape::rect(rr[0], rr[1], rr[2], rr[3], 10.0, [1.0, 1.0, 1.0, 0.08 * a]));
            }
            let one: String = c.split_whitespace().collect::<Vec<_>>().join(" ");
            let t = f.fit(Text::new(one, rr[0] + 8.0, ry + 8.0, 12.0, with_a(WHITE, 0.85 * a)), rr[2] - 16.0);
            f.scene.text(t);
            if input.clicked(rr) {
                platform::set_clipboard(c);
                shared.hud(Hud::Copied(c.chars().take(40).collect()));
            }
        }

        // ---- Shelf
        let r = card(f, 2, "Shelf", ic::FOLDER);
        if self.shelf.is_empty() {
            f.scene.text(Text::new("Drag files onto", r[0] + cw / 2.0, y + h / 2.0 - 18.0, 12.0, with_a(WHITE, 0.4 * a)).center());
            f.scene.text(Text::new("the island to keep", r[0] + cw / 2.0, y + h / 2.0, 12.0, with_a(WHITE, 0.4 * a)).center());
            f.scene.text(Text::new("them handy", r[0] + cw / 2.0, y + h / 2.0 + 18.0, 12.0, with_a(WHITE, 0.4 * a)).center());
        }
        let mut remove = None;
        for (k, p) in self.shelf.iter().take(6).enumerate() {
            let ry = y + 42.0 + k as f32 * 36.0;
            let rr = [r[0] + 8.0, ry, cw - 16.0, 32.0];
            let hov = input.over(rr);
            if hov {
                f.scene.shape(Shape::rect(rr[0], rr[1], rr[2], rr[3], 10.0, [1.0, 1.0, 1.0, 0.08 * a]));
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_uppercase();
            let (c0, _) = app_colors(&ext);
            f.scene.shape(Shape::rect(rr[0] + 6.0, ry + 6.0, 20.0, 20.0, 6.0, with_a(c0, 0.8 * a)));
            let t = f.fit(Text::new(file_name(p), rr[0] + 34.0, ry + 8.0, 12.0, with_a(WHITE, 0.85 * a)), rr[2] - 40.0);
            f.scene.text(t);
            if input.clicked(rr) {
                platform::open(&p.to_string_lossy());
            }
            if hov && input.right_pressed {
                remove = Some(k);
            }
        }
        if let Some(k) = remove {
            self.shelf.remove(k);
        }
        if !self.shelf.is_empty() {
            f.scene.text(Text::new("click open · right-click remove", r[0] + cw / 2.0, y + h - 20.0, 10.0, with_a(WHITE, 0.35 * a)).center());
        }

        // ---- Today
        let r = card(f, 3, "Today", ic::CALENDAR);
        let misc = shared.misc.read().clone();
        let lt = platform::local_time();
        f.scene.text(Text::new(platform::WEEKDAYS[lt.weekday as usize % 7], r[0] + 16.0, y + 40.0, 12.0, rgba(255, 69, 58, a)).weight(600));
        f.scene.text(Text::new(format!("{}", lt.day), r[0] + 16.0, y + 54.0, 34.0, with_a(WHITE, a)).weight(500).family(Family::Display));
        if let Some(wt) = &misc.weather {
            weather_icon(f, r[0] + cw - 34.0, y + 62.0, 13.0, wt.code, wt.is_day, a);
            f.scene.text(Text::new(format!("{:.0}°", wt.temp), r[0] + cw - 14.0, y + 82.0, 18.0, with_a(WHITE, a)).right().weight(600));
            f.scene.text(Text::new(format!("H{:.0}° L{:.0}°", wt.hi, wt.lo), r[0] + cw - 14.0, y + 104.0, 10.5, with_a(WHITE, 0.5 * a)).right());
        } else if cfg.weather_city.is_empty() {
            f.scene.text(Text::new("set weather_city", r[0] + cw - 14.0, y + 70.0, 10.5, with_a(WHITE, 0.35 * a)).right());
        }
        let mut ey = y + 126.0;
        if misc.events.is_empty() {
            let msg = if cfg.calendar_ics_url.is_empty() { "Add calendar_ics_url in config for events" } else { "No upcoming events" };
            let t = Text::new(msg, r[0] + 16.0, ey, 11.0, with_a(WHITE, 0.4 * a)).max_w(cw - 30.0);
            f.scene.text(t);
        }
        for ev in misc.events.iter().take(4) {
            f.scene.shape(Shape::rect(r[0] + 14.0, ey + 2.0, 3.0, 28.0, 1.5, with_a(pal.primary, a)));
            let tt = f.fit(Text::new(ev.title.clone(), r[0] + 24.0, ey, 12.0, with_a(WHITE, 0.9 * a)).weight(600), cw - 36.0);
            f.scene.text(tt);
            f.scene.text(Text::new(event_when(ev), r[0] + 24.0, ey + 16.0, 10.5, with_a(WHITE, 0.5 * a)));
            ey += 36.0;
        }
    }

    fn tab_social(&mut self, f: &mut Frame, input: &Input, shared: &Shared, area: [f32; 4], a: f32, cfg: &Config) {
        let [x, y, w, h] = area;
        let misc = shared.misc.read().clone();
        let n = cfg.socials.len().max(1) as f32;
        let gap = 12.0;
        let tw = (w - gap * (n - 1.0)) / n;
        let th = 124.0;
        for (i, s) in cfg.socials.iter().enumerate() {
            let tx = x + i as f32 * (tw + gap);
            let rr = [tx, y, tw, th];
            let hov = input.over(rr);
            let (c0, c1) = app_colors(&s.name);
            f.scene.shape(Shape::rect(tx, y, tw, th, 22.0, with_a(c0, if hov { 0.22 } else { 0.12 } * a)).grad(with_a(c1, 0.06 * a), 0.9));
            f.scene.shape(Shape::stroke(tx, y, tw, th, 22.0, 1.0, with_a(c0, 0.3 * a)));
            let (cx, cy) = (tx + 40.0, y + 42.0);
            f.scene.glow_shape(Shape::rect(cx - 22.0, cy - 22.0, 44.0, 44.0, 14.0, with_a(c0, a)).grad(with_a(c1, a), 0.8));
            social_glyph(f, &s.name, cx, cy, a);
            f.scene.text(Text::new(&s.name, tx + 18.0, y + 76.0, 15.0, with_a(WHITE, a)).weight(650));
            let seen = self.social_seen.get(&s.name).cloned();
            let unread = misc
                .notifications
                .iter()
                .filter(|n| n.app.to_lowercase().contains(&s.match_app) && seen.map(|t| n.at > t).unwrap_or(true))
                .count();
            let sub = if unread > 0 { format!("{unread} new") } else { "Open messages".into() };
            f.scene.text(Text::new(sub, tx + 18.0, y + 97.0, 11.5, with_a(WHITE, 0.55 * a)));
            if unread > 0 {
                let bx = tx + tw - 26.0;
                f.scene.glow_shape(Shape::rect(bx - 12.0, y + 14.0, 24.0, 22.0, 11.0, rgba(255, 69, 58, a)));
                f.scene.text(Text::new(format!("{}", unread.min(99)), bx, y + 17.0, 12.0, with_a(WHITE, a)).center().weight(700));
            }
            if input.clicked(rr) {
                platform::open(&s.url);
                self.social_seen.insert(s.name.clone(), Instant::now());
            }
        }
        // recent notifications
        let ly = y + th + 18.0;
        f.scene.text(Text::new("Recent", x + 4.0, ly, 12.0, with_a(WHITE, 0.55 * a)).weight(600));
        match misc.notif_access {
            Some(false) => {
                let t = Text::new(
                    "Windows didn't grant notification access to Aurolo — tiles still open your DMs. Unread counts need Settings › Privacy › Notifications.",
                    x + 4.0,
                    ly + 22.0,
                    11.5,
                    with_a(WHITE, 0.45 * a),
                )
                .max_w(w - 8.0);
                f.scene.text(t);
            }
            _ => {
                if misc.notifications.is_empty() {
                    f.scene.text(Text::new("No notifications yet", x + 4.0, ly + 22.0, 11.5, with_a(WHITE, 0.4 * a)));
                }
                for (k, nf) in misc.notifications.iter().take(3).enumerate() {
                    let ry = ly + 22.0 + k as f32 * 30.0;
                    let (c0, _) = app_colors(&nf.app);
                    f.scene.shape(Shape::rect(x + 4.0, ry + 2.0, 18.0, 18.0, 9.0, with_a(c0, a)));
                    let s = format!("{} — {}  {}", nf.app, nf.title, nf.body);
                    let t = f.fit(Text::new(s, x + 30.0, ry + 2.0, 12.0, with_a(WHITE, 0.8 * a)), w - 100.0);
                    f.scene.text(t);
                    let age = nf.at.elapsed().as_secs();
                    let ago = if age < 60 { "now".into() } else if age < 3600 { format!("{}m", age / 60) } else { format!("{}h", age / 3600) };
                    f.scene.text(Text::new(ago, x + w - 4.0, ry + 2.0, 11.0, with_a(WHITE, 0.4 * a)).right());
                }
            }
        }
        let _ = h;
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

fn hud_text(h: &Hud) -> (String, String) {
    match h {
        Hud::Battery { percent, charging } => {
            if *charging {
                ("Charging".into(), format!("{percent:.0}%"))
            } else if *percent <= 20.0 {
                ("Low battery".into(), format!("{percent:.0}%"))
            } else {
                ("On battery".into(), format!("{percent:.0}%"))
            }
        }
        Hud::Wifi(Some(n)) => ("Connected".into(), n.clone()),
        Hud::Wifi(None) => ("Disconnected".into(), "No internet".into()),
        Hud::Bluetooth { name, battery } => (name.clone(), battery.map(|b| format!("{b}%")).unwrap_or_else(|| "Connected".into())),
        Hud::TimerDone(l) => (l.clone(), "Done".into()),
        Hud::Download(n) => ("Downloaded".into(), n.clone()),
        Hud::Copied(s) => ("Copied".into(), s.clone()),
        Hud::Message(a, b) => (a.clone(), b.clone()),
        Hud::Notification { app, title, .. } => (app.clone(), title.clone()),
        Hud::Volume { level, .. } => ("Volume".into(), format!("{:.0}", level * 100.0)),
        Hud::Brightness(b) => ("Brightness".into(), format!("{:.0}", b * 100.0)),
    }
}

fn battery_icon(f: &mut Frame, x: f32, y: f32, frac: f32, col: Rgba, a: f32, charging: bool) {
    f.scene.shape(Shape::stroke(x, y, 30.0, 14.0, 4.5, 1.5, with_a(WHITE, 0.6 * a)));
    f.scene.shape(Shape::rect(x + 31.0, y + 4.5, 2.5, 5.0, 1.0, with_a(WHITE, 0.6 * a)));
    f.scene.glow_shape(Shape::rect(x + 2.5, y + 2.5, (25.0 * frac).max(2.0), 9.0, 2.5, with_a(col, a)));
    if charging {
        f.icon('\u{E945}', x + 15.0, y + 7.0, 9.0, with_a(BLACK, a));
    }
}

fn weather_icon(f: &mut Frame, cx: f32, cy: f32, r: f32, code: i32, day: bool, a: f32) {
    let sun = rgba(255, 204, 0, a);
    let cloud = with_a(WHITE, 0.9 * a);
    match code {
        0 | 1 => {
            if day {
                f.scene.glow_shape(Shape::glow(cx, cy, r * 1.0, sun, 0.4));
            } else {
                f.scene.glow_shape(Shape::glow(cx, cy, r * 0.9, rgba(220, 220, 255, a), 0.5));
                f.scene.shape(Shape::glow(cx + r * 0.35, cy - r * 0.25, r * 0.7, [0.0, 0.0, 0.0, a], 0.3));
            }
        }
        _ => {
            if code == 2 && day {
                f.scene.glow_shape(Shape::glow(cx + r * 0.5, cy - r * 0.45, r * 0.7, sun, 0.4));
            }
            f.scene.shape(Shape::rect(cx - r, cy - r * 0.1, r * 2.0, r * 0.8, r * 0.4, cloud));
            f.scene.shape(Shape::glow(cx - r * 0.25, cy - r * 0.1, r * 0.62, cloud, 0.05));
            f.scene.shape(Shape::glow(cx + r * 0.35, cy, r * 0.5, cloud, 0.05));
            if (51..=67).contains(&code) || (80..=99).contains(&code) {
                for k in 0..3 {
                    let dx = (k as f32 - 1.0) * r * 0.6;
                    f.scene.shape(Shape::line(cx + dx, cy + r * 0.9, cx + dx - r * 0.2, cy + r * 1.4, 1.6, rgba(100, 180, 255, a)));
                }
            }
            if (71..=77).contains(&code) || code == 85 || code == 86 {
                for k in 0..3 {
                    f.scene.shape(Shape::glow(cx + (k as f32 - 1.0) * r * 0.6, cy + r * 1.15, 2.2, with_a(WHITE, a), 0.5));
                }
            }
        }
    }
}

fn social_glyph(f: &mut Frame, name: &str, cx: f32, cy: f32, a: f32) {
    let n = name.to_lowercase();
    if n.contains("instagram") {
        f.scene.shape(Shape::stroke(cx - 11.0, cy - 11.0, 22.0, 22.0, 7.0, 2.2, with_a(WHITE, a)));
        f.scene.shape(Shape::stroke(cx - 5.0, cy - 5.0, 10.0, 10.0, 5.0, 2.2, with_a(WHITE, a)));
        f.scene.shape(Shape::glow(cx + 6.2, cy - 6.2, 1.8, with_a(WHITE, a), 0.1));
    } else if n.contains("whatsapp") {
        f.scene.shape(Shape::stroke(cx - 11.0, cy - 11.0, 22.0, 22.0, 11.0, 2.2, with_a(WHITE, a)));
        f.scene.shape(Shape::line(cx - 9.0, cy + 8.0, cx - 11.5, cy + 12.5, 2.4, with_a(WHITE, a)));
        f.icon('\u{E717}', cx, cy, 10.0, with_a(WHITE, a));
    } else if n.contains("discord") {
        f.scene.shape(Shape::rect(cx - 12.0, cy - 8.0, 24.0, 17.0, 8.0, with_a(WHITE, a)));
        f.scene.shape(Shape::glow(cx - 4.5, cy + 0.5, 2.6, with_a(rgba(88, 101, 242, 1.0), a), 0.1));
        f.scene.shape(Shape::glow(cx + 4.5, cy + 0.5, 2.6, with_a(rgba(88, 101, 242, 1.0), a), 0.1));
    } else {
        let ini = name.chars().next().unwrap_or('•').to_uppercase().to_string();
        f.scene.text(Text::new(ini, cx, cy - 12.0, 19.0, with_a(WHITE, a)).center().weight(700));
    }
}

fn event_when(ev: &Event) -> String {
    let now = platform::unix_now();
    let d = ev.start - now;
    if ev.all_day {
        return "All day".into();
    }
    if d <= 0 {
        "Now".into()
    } else if d < 3600 {
        format!("in {} min", d / 60)
    } else {
        let off = platform::utc_offset_secs();
        let local = ev.start + off;
        let hh = (local.rem_euclid(86400)) / 3600;
        let mm = (local.rem_euclid(3600)) / 60;
        let days = (local.div_euclid(86400)) - ((now + off).div_euclid(86400));
        let day = match days {
            0 => "Today".to_string(),
            1 => "Tomorrow".to_string(),
            n => format!("In {n} days"),
        };
        format!("{day} {hh}:{mm:02}")
    }
}

fn subseq(q: &str, s: &str) -> bool {
    let mut it = s.chars();
    q.chars().all(|c| it.any(|d| d == c))
}

fn url_enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Tiny calculator: + - * / % ^, parentheses, decimals. Returns None if not maths.
pub fn calc(s: &str) -> Option<f64> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() || !s.chars().any(|c| c.is_ascii_digit()) || !s.chars().any(|c| "+-*/%^(x".contains(c)) {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_digit() || "+-*/%^().x".contains(c)) {
        return None;
    }
    let s = s.replace('x', "*");
    let b = s.as_bytes();
    let mut i = 0;
    fn expr(b: &[u8], i: &mut usize) -> Option<f64> {
        let mut v = term(b, i)?;
        while *i < b.len() && (b[*i] == b'+' || b[*i] == b'-') {
            let op = b[*i];
            *i += 1;
            let r = term(b, i)?;
            v = if op == b'+' { v + r } else { v - r };
        }
        Some(v)
    }
    fn term(b: &[u8], i: &mut usize) -> Option<f64> {
        let mut v = power(b, i)?;
        while *i < b.len() && (b[*i] == b'*' || b[*i] == b'/' || b[*i] == b'%') {
            let op = b[*i];
            *i += 1;
            let r = power(b, i)?;
            v = match op {
                b'*' => v * r,
                b'/' => v / r,
                _ => v % r,
            };
        }
        Some(v)
    }
    fn power(b: &[u8], i: &mut usize) -> Option<f64> {
        let base = unary(b, i)?;
        if *i < b.len() && b[*i] == b'^' {
            *i += 1;
            let e = power(b, i)?;
            return Some(base.powf(e));
        }
        Some(base)
    }
    fn unary(b: &[u8], i: &mut usize) -> Option<f64> {
        if *i < b.len() && b[*i] == b'-' {
            *i += 1;
            return Some(-unary(b, i)?);
        }
        atom(b, i)
    }
    fn atom(b: &[u8], i: &mut usize) -> Option<f64> {
        if *i < b.len() && b[*i] == b'(' {
            *i += 1;
            let v = expr(b, i)?;
            if *i < b.len() && b[*i] == b')' {
                *i += 1;
            }
            return Some(v);
        }
        let st = *i;
        while *i < b.len() && (b[*i].is_ascii_digit() || b[*i] == b'.') {
            *i += 1;
        }
        std::str::from_utf8(&b[st..*i]).ok()?.parse().ok()
    }
    let v = expr(b, &mut i)?;
    if i != b.len() || !v.is_finite() {
        return None;
    }
    Some(v)
}

#[allow(dead_code)]
fn _unused(_: u64) -> String {
    fmt_bytes(0)
}
