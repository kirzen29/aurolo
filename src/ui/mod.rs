//! Immediate-mode UI helpers shared by all overlay windows.

pub mod edge;
pub mod fullscreen;
pub mod island;
pub mod lyrics_view;
pub mod widget;

use std::path::PathBuf;

use crate::gfx::*;
use crate::state::Art;

/// Segoe Fluent Icons / MDL2 code points.
pub mod ic {
    pub const PLAY: char = '\u{E768}';
    pub const PAUSE: char = '\u{E769}';
    pub const NEXT: char = '\u{E893}';
    pub const PREV: char = '\u{E892}';
    pub const VOLUME: char = '\u{E767}';
    pub const VOLUME1: char = '\u{E993}';
    pub const MUTE: char = '\u{E74F}';
    pub const BRIGHT: char = '\u{E706}';
    pub const WIFI: char = '\u{E701}';
    pub const WIFI_OFF: char = '\u{EB55}';
    pub const BT: char = '\u{E702}';
    pub const SEARCH: char = '\u{E721}';
    pub const SETTINGS: char = '\u{E713}';
    pub const FULLSCREEN: char = '\u{E740}';
    pub const CLOSE: char = '\u{E711}';
    pub const MUSIC: char = '\u{EC4F}';
    pub const CLIPBOARD: char = '\u{E77F}';
    pub const FOLDER: char = '\u{E8B7}';
    pub const DOWNLOAD: char = '\u{E896}';
    pub const CALENDAR: char = '\u{E787}';
    pub const STOPWATCH: char = '\u{E916}';
    pub const TIMER: char = '\u{E823}';
    pub const MOON: char = '\u{E708}';
    pub const CHAT: char = '\u{E8BD}';
    pub const APP: char = '\u{ECAA}';
    pub const GLOBE: char = '\u{E774}';
    pub const CALC: char = '\u{E8EF}';
    pub const CMD: char = '\u{E756}';
    pub const CAMERA: char = '\u{E722}';
    pub const MIC: char = '\u{E720}';
    pub const CHIP: char = '\u{E950}';
    pub const LYRICS: char = '\u{E8D2}';
    pub const STYLE: char = '\u{E790}';
    pub const PIN: char = '\u{E718}';
    pub const ADD: char = '\u{E710}';
    pub const MINUS: char = '\u{E738}';
    pub const STOP: char = '\u{E71A}';
    pub const BELL: char = '\u{EA8F}';
    pub const POWER: char = '\u{E7E8}';
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Esc,
    Back,
    Up,
    Down,
    Left,
    Right,
    Space,
    Tab,
    BracketL,
    BracketR,
}

#[derive(Default)]
pub struct Input {
    pub mouse: Option<(f32, f32)>,
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub press_pos: Option<(f32, f32)>,
    pub right_pressed: bool,
    pub scroll: f32,
    pub text: String,
    pub keys: Vec<Key>,
    pub dropped: Vec<PathBuf>,
    pub hovering_file: bool,
    pub moved: bool,
    pub focused: bool,
}

impl Input {
    pub fn over(&self, r: [f32; 4]) -> bool {
        self.mouse.map(|p| hit(r, p)).unwrap_or(false)
    }
    pub fn clicked(&self, r: [f32; 4]) -> bool {
        self.released && self.press_pos.map(|p| hit(r, p)).unwrap_or(false) && self.over(r)
    }
    pub fn key(&self, k: Key) -> bool {
        self.keys.contains(&k)
    }
    /// Clears per-frame edges after the frame is built.
    pub fn end_frame(&mut self) {
        self.pressed = false;
        self.released = false;
        self.right_pressed = false;
        self.scroll = 0.0;
        self.text.clear();
        self.keys.clear();
        self.dropped.clear();
        self.moved = false;
        if !self.down {
            self.press_pos = None;
        }
    }
}

pub fn hit(r: [f32; 4], p: (f32, f32)) -> bool {
    p.0 >= r[0] && p.0 <= r[0] + r[2] && p.1 >= r[1] && p.1 <= r[1] + r[3]
}

pub struct Frame<'a> {
    pub gpu: &'a Gpu,
    pub painter: &'a mut Painter,
    pub scene: Scene,
    pub w: f32,
    pub h: f32,
    pub t: f32,
    pub dt: f32,
}

impl<'a> Frame<'a> {
    pub fn measure(&mut self, t: &Text) -> (f32, f32) {
        self.painter.measure(self.gpu, t)
    }

    /// Truncate with an ellipsis to fit `max_w`.
    pub fn fit(&mut self, t: Text, max_w: f32) -> Text {
        let (w, _) = self.measure(&t);
        if w <= max_w || t.s.is_empty() {
            return t;
        }
        let chars: Vec<char> = t.s.chars().collect();
        let mut n = ((chars.len() as f32) * max_w / w) as usize;
        loop {
            n = n.min(chars.len()).max(1);
            let s: String = chars[..n].iter().collect::<String>().trim_end().to_string() + "…";
            let mut c = t.clone();
            c.s = s;
            let (cw, _) = self.measure(&c);
            if cw <= max_w || n <= 1 {
                return c;
            }
            n -= 1.max(n / 10);
        }
    }

    pub fn icon(&mut self, g: char, cx: f32, cy: f32, size: f32, color: Rgba) {
        let t = Text::new(g.to_string(), cx, cy - size * 0.62, size, color).family(Family::Icons).center().lh(1.25);
        self.scene.text(t);
    }

    /// Round icon button. Returns true when clicked.
    pub fn icon_button(&mut self, input: &Input, g: char, cx: f32, cy: f32, size: f32, color: Rgba) -> bool {
        let r = size * 0.95;
        let rect = [cx - r, cy - r, r * 2.0, r * 2.0];
        let hov = input.over(rect);
        if hov {
            let a = if input.down { 0.22 } else { 0.13 };
            self.scene.shape(Shape::rect(rect[0], rect[1], rect[2], rect[3], r, [1.0, 1.0, 1.0, a]));
        }
        self.icon(g, cx, cy, size, color);
        input.clicked(rect)
    }

    /// Capsule button with text, returns (clicked, width).
    pub fn pill(&mut self, input: &Input, label: &str, x: f32, y: f32, h: f32, active: bool, accent: Rgba) -> (bool, f32) {
        let size = h * 0.42;
        let t = Text::new(label, 0.0, 0.0, size, WHITE).weight(if active { 600 } else { 500 });
        let (tw, _) = self.measure(&t);
        let w = tw + h * 0.9;
        let rect = [x, y, w, h];
        let hov = input.over(rect);
        let bg = if active {
            with_a(accent, 0.9)
        } else if hov {
            [1.0, 1.0, 1.0, 0.14]
        } else {
            [1.0, 1.0, 1.0, 0.07]
        };
        self.scene.shape(Shape::rect(x, y, w, h, h / 2.0, bg));
        let tc = if active { [0.04, 0.03, 0.06, 1.0] } else { [1.0, 1.0, 1.0, 0.86] };
        self.scene.text(Text { x: x + w / 2.0, y: y + (h - size * 1.25) / 2.0, color: tc, ..t }.center());
        (input.clicked(rect), w)
    }

    pub fn bar(&mut self, x: f32, y: f32, w: f32, h: f32, frac: f32, c0: Rgba, c1: Rgba) {
        self.scene.shape(Shape::rect(x, y, w, h, h / 2.0, [1.0, 1.0, 1.0, 0.13]));
        let fw = (w * frac.clamp(0.0, 1.0)).max(h);
        if frac > 0.001 {
            self.scene.shape(Shape::rect(x, y, fw, h, h / 2.0, c0).grad(c1, 0.0));
        }
    }

    /// Upload album art (sharp + blurred) into this window's painter if needed.
    pub fn ensure_art(&mut self, art: &Art) -> (u64, u64) {
        let k = art.id;
        let kb = art.id ^ 0x5bd1_e995_0000_0001;
        if !self.painter.has_image(k) {
            self.painter.upload_image(self.gpu, k, art.w, art.h, &art.rgba);
            self.painter.upload_image(self.gpu, kb, art.blur_w, art.blur_h, &art.blur);
        }
        self.painter.touch_image(k);
        self.painter.touch_image(kb);
        (k, kb)
    }
}

/// Brand-ish colours for social badges.
pub fn app_colors(name: &str) -> (Rgba, Rgba) {
    let n = name.to_lowercase();
    if n.contains("discord") {
        (rgba(88, 101, 242, 1.0), rgba(130, 140, 255, 1.0))
    } else if n.contains("instagram") {
        (rgba(253, 29, 29, 1.0), rgba(131, 58, 180, 1.0))
    } else if n.contains("whatsapp") {
        (rgba(37, 211, 102, 1.0), rgba(18, 140, 126, 1.0))
    } else if n.contains("telegram") {
        (rgba(42, 171, 238, 1.0), rgba(34, 158, 217, 1.0))
    } else {
        let h = crate::anim::hash01(name.bytes().fold(7u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64)));
        let c = hsv(h * 360.0, 0.55, 0.95);
        (c, hsv((h * 360.0 + 40.0) % 360.0, 0.6, 0.85))
    }
}

pub fn hsv(h: f32, s: f32, v: f32) -> Rgba {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match ((h / 60.0) as i32).rem_euclid(6) {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m, 1.0]
}

pub fn fmt_bytes_rate(b: f64) -> String {
    if b >= 1048576.0 {
        format!("{:.1} MB/s", b / 1048576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB/s", b / 1024.0)
    } else {
        format!("{:.0} B/s", b)
    }
}

pub fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1073741824.0 {
        format!("{:.2} GB", b / 1073741824.0)
    } else if b >= 1048576.0 {
        format!("{:.1} MB", b / 1048576.0)
    } else {
        format!("{:.0} KB", b / 1024.0)
    }
}
