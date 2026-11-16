//! Lyric renderers shared by the floating widget and the fullscreen view.
//!
//! * Karaoke  – Apple-Music-style spring-scrolling list, words fill + glow as sung.
//! * Constellation – each word is a star; sung words ignite and get linked into a
//!   constellation that ascends into the starfield when the line ends.
//! * Drift – lines float in 3D depth and fly past the camera, warp-speed particles.

use std::collections::HashMap;

use crate::anim::*;
use crate::config::LyricStyle;
use crate::gfx::*;
use crate::state::*;
use crate::ui::Frame;

#[derive(Clone)]
struct WordBox {
    x: f32, // relative to line centre
    y: f32, // row top relative to line top
    w: f32,
}

#[derive(Clone)]
struct LineLayout {
    words: Vec<WordBox>,
    h: f32,
}

#[derive(Clone)]
struct ConstLayout {
    stars: Vec<(f32, f32)>,  // relative to group centre
    labels: Vec<(f32, f32)>, // label centre-x, top-y relative to group centre
    widths: Vec<f32>,
}

struct Remnant {
    x: f32,
    y: f32,
    born: f32,
    seed: f32,
    color: Rgba,
}

struct Warp {
    ang: f32,
    rad: f32,
    z: f32,
    hue: f32,
}

pub struct LyricView {
    pub style: LyricStyle,
    track: u64,
    size_key: u32,
    layouts: HashMap<usize, LineLayout>,
    cum: Vec<f32>,
    consts: HashMap<usize, ConstLayout>,
    scroll: Spring,
    last_line: Option<usize>,
    remnants: Vec<Remnant>,
    exited: std::collections::HashSet<usize>,
    shooting: Option<(f32, f32, f32, f32, f32)>,
    next_shoot: f32,
    warp: Vec<Warp>,
    font_size: f32,
    /// Lyrics-only mode: hide song title/artist from status messages.
    pub minimal: bool,
}

const FADE_TOP: f32 = 1.4;

impl LyricView {
    pub fn new(style: LyricStyle) -> Self {
        LyricView {
            style,
            track: 0,
            size_key: 0,
            layouts: HashMap::new(),
            cum: vec![],
            consts: HashMap::new(),
            scroll: Spring::with(0.0, 90.0, 17.0),
            last_line: None,
            remnants: vec![],
            exited: Default::default(),
            shooting: None,
            next_shoot: 4.0,
            warp: vec![],
            font_size: 0.0,
            minimal: false,
        }
    }

    fn reset(&mut self) {
        self.layouts.clear();
        self.consts.clear();
        self.cum.clear();
        self.last_line = None;
        self.exited.clear();
    }

    fn base_size(&self, rect: [f32; 4], big: bool) -> f32 {
        match (self.style, big) {
            (LyricStyle::Karaoke, true) => (rect[3] * 0.068).clamp(28.0, 66.0),
            (LyricStyle::Karaoke, false) => (rect[3] * 0.22).clamp(15.0, 46.0),
            (LyricStyle::Constellation, true) => (rect[3] * 0.04).clamp(20.0, 42.0),
            (LyricStyle::Constellation, false) => (rect[3] * 0.12).clamp(11.0, 26.0),
            (LyricStyle::Drift, true) => (rect[3] * 0.06).clamp(26.0, 58.0),
            (LyricStyle::Drift, false) => (rect[3] * 0.18).clamp(14.0, 40.0),
        }
    }

    fn word_text(&self, s: &str, size: f32) -> Text {
        Text::new(s, 0.0, 0.0, size, WHITE).family(Family::Lyric).weight(650).lh(1.2)
    }

    fn layout_line(&mut self, f: &mut Frame, line: &Line, size: f32, max_w: f32) -> LineLayout {
        let space = size * 0.3;
        let mut rows: Vec<Vec<(usize, f32)>> = vec![vec![]];
        let mut row_w = 0.0;
        let words: Vec<String> = if line.words.is_empty() {
            line.text.split_whitespace().map(|s| s.to_string()).collect()
        } else {
            line.words.iter().map(|w| w.text.clone()).collect()
        };
        for (i, w) in words.iter().enumerate() {
            let (ww, _) = f.measure(&self.word_text(w, size));
            let add = if rows.last().unwrap().is_empty() { ww } else { ww + space };
            if row_w + add > max_w && !rows.last().unwrap().is_empty() {
                rows.push(vec![]);
                row_w = 0.0;
                rows.last_mut().unwrap().push((i, ww));
                row_w = ww;
            } else {
                rows.last_mut().unwrap().push((i, ww));
                row_w += add;
            }
        }
        let lh = size * 1.22;
        let mut boxes = vec![WordBox { x: 0.0, y: 0.0, w: 0.0 }; words.len()];
        for (r, row) in rows.iter().enumerate() {
            let total: f32 = row.iter().map(|(_, w)| *w).sum::<f32>() + space * (row.len().max(1) - 1) as f32;
            let mut x = -total / 2.0;
            for (i, w) in row {
                boxes[*i] = WordBox { x, y: r as f32 * lh, w: *w };
                x += w + space;
            }
        }
        let n_rows = if words.is_empty() { 1 } else { rows.len() };
        LineLayout { words: boxes, h: n_rows as f32 * lh }
    }

    fn ensure_layouts(&mut self, f: &mut Frame, ly: &LyricsState, size: f32, max_w: f32) {
        let key = (size * 10.0) as u32 ^ ((max_w as u32) << 12) ^ (self.style as u32) << 28;
        if ly.track_id != self.track || key != self.size_key || self.layouts.len() != ly.lines.len() {
            if ly.track_id != self.track {
                self.remnants.clear();
            }
            self.track = ly.track_id;
            self.size_key = key;
            self.reset();
            let gap = size * 0.55;
            let mut acc = 0.0;
            for (i, line) in ly.lines.iter().enumerate() {
                let l = self.layout_line(f, line, size, max_w);
                self.cum.push(acc);
                acc += l.h + gap;
                self.layouts.insert(i, l);
            }
            self.cum.push(acc);
        }
    }

    /// Fractional y offset into the stacked lines for a fractional line index.
    fn cum_at(&self, v: f32) -> f32 {
        if self.cum.is_empty() {
            return 0.0;
        }
        let v = v.clamp(0.0, (self.cum.len() - 1) as f32);
        let i = v.floor() as usize;
        let j = (i + 1).min(self.cum.len() - 1);
        lerp(self.cum[i], self.cum[j], v - i as f32)
    }

    // ------------------------------------------------------------------ public

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        f: &mut Frame,
        rect: [f32; 4],
        ly: &LyricsState,
        media: &MediaState,
        pal: &Palette,
        audio: &AudioState,
        pos: f64,
        big: bool,
    ) {
        let size = self.base_size(rect, big);
        self.font_size = size;
        if big {
            self.background(f, rect, pal, audio, media);
        } else if self.style == LyricStyle::Constellation {
            self.starfield(f, rect, pal, audio, 40, 0.5);
        } else if self.style == LyricStyle::Drift {
            self.warp_field(f, rect, pal, audio, 50);
        }

        // In minimal (lyrics-only) mode, status messages never show the song name.
        let title = if self.minimal { "" } else { media.title.as_str() };
        if !media.active {
            let sub = if self.minimal { "" } else { "Spotify, YouTube, Apple Music… lyrics appear here" };
            self.message(f, rect, "Play something", sub, big);
            return;
        }
        match ly.status {
            LyricStatus::Loading | LyricStatus::Idle => {
                if self.minimal {
                    // just breathing dots while lyrics load
                    let c = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
                    let prog = (f.t * 0.8).fract();
                    self.dots(f, c.0, c.1, self.font_size, prog, pal, audio);
                } else {
                    let a = 0.45 + 0.25 * (f.t * 3.0).sin();
                    self.message_a(f, rect, "Finding lyrics…", title, big, a);
                }
                return;
            }
            LyricStatus::NotFound => {
                if self.minimal {
                    self.message(f, rect, "No lyrics for this song", "", big);
                } else {
                    self.message(f, rect, title, "No synced lyrics found for this track", big);
                }
                return;
            }
            LyricStatus::Instrumental => {
                self.message(f, rect, "♪ Instrumental", title, big);
                return;
            }
            _ => {}
        }
        if ly.lines.is_empty() {
            return;
        }
        match self.style {
            LyricStyle::Karaoke => self.karaoke(f, rect, ly, media, pal, audio, pos, big),
            LyricStyle::Constellation => self.constellation(f, rect, ly, pal, audio, pos, big),
            LyricStyle::Drift => self.drift(f, rect, ly, media, pal, audio, pos, big),
        }
    }

    fn message(&mut self, f: &mut Frame, rect: [f32; 4], title: &str, sub: &str, big: bool) {
        self.message_a(f, rect, title, sub, big, 0.95);
    }

    fn message_a(&mut self, f: &mut Frame, rect: [f32; 4], title: &str, sub: &str, big: bool, a: f32) {
        let s = if big { 40.0 } else { 18.0 };
        let cx = rect[0] + rect[2] / 2.0;
        let cy = rect[1] + rect[3] / 2.0;
        let ty = if sub.is_empty() { cy - s * 0.62 } else { cy - s * 0.9 };
        if !title.is_empty() {
            let t = Text::new(title, cx, ty, s, [1.0, 1.0, 1.0, a]).family(Family::Lyric).weight(650).center();
            let t = f.fit(t, rect[2] * 0.9);
            f.scene.glow_text(t);
        }
        if sub.is_empty() {
            return;
        }
        let st = Text::new(sub, cx, cy + s * 0.45, s * 0.45, [1.0, 1.0, 1.0, a * 0.55]).center();
        let st = f.fit(st, rect[2] * 0.9);
        f.scene.text(st);
    }

    // ------------------------------------------------------------ backgrounds

    fn background(&mut self, f: &mut Frame, rect: [f32; 4], pal: &Palette, audio: &AudioState, media: &MediaState) {
        let [x, y, w, h] = rect;
        let dark = pal.dark;
        f.scene.shape(Shape::rect(x, y, w, h, 0.0, [dark[0] * 0.6, dark[1] * 0.6, dark[2] * 0.6, 1.0]).grad(
            [dark[0] * 1.4, dark[1] * 1.4, dark[2] * 1.6, 1.0],
            std::f32::consts::FRAC_PI_2,
        ));
        if let Some(art) = &media.art {
            let (_, kb) = f.ensure_art(art);
            let op = match self.style {
                LyricStyle::Karaoke => 0.55,
                LyricStyle::Constellation => 0.22,
                LyricStyle::Drift => 0.3,
            };
            // oversize + slow pan so the blur feels alive
            let s = w.max(h) * 1.25;
            let px = (f.t * 0.05).sin() * w * 0.04;
            let py = (f.t * 0.037).cos() * h * 0.04;
            f.scene.image(
                ImageDraw::new(kb, x + (w - s) / 2.0 + px, y + (h - s) / 2.0 + py, s, s, 0.0)
                    .opacity(op)
                    .saturation(1.35),
            );
            f.scene.layer();
            // vignette
            f.scene.shape(Shape::rect(x, y, w, h, 0.0, [0.0, 0.0, 0.0, 0.0]).radial([0.0, 0.0, 0.0, 0.7]));
        }
        let pulse = audio.pulse();
        // drifting colour nebulae
        let blobs = [(pal.primary, 0.13, 0.0), (pal.secondary, 0.09, 2.1), (pal.primary, 0.07, 4.2)];
        for (i, (c, sp, ph)) in blobs.iter().enumerate() {
            let bx = x + w * (0.5 + 0.32 * (f.t * sp + ph).sin());
            let by = y + h * (0.5 + 0.28 * (f.t * sp * 1.3 + ph * 1.7).cos());
            let r = w.max(h) * (0.32 + 0.04 * i as f32) * (1.0 + pulse * 0.06);
            let a = match self.style {
                LyricStyle::Karaoke => 0.22,
                _ => 0.16,
            };
            f.scene.shape(Shape::glow(bx, by, r, with_a(*c, a + pulse * 0.05), 1.6).additive());
        }
        match self.style {
            LyricStyle::Constellation => self.starfield(f, rect, pal, audio, 260, 1.0),
            LyricStyle::Drift => self.warp_field(f, rect, pal, audio, 170),
            LyricStyle::Karaoke => {}
        }
    }

    fn starfield(&mut self, f: &mut Frame, rect: [f32; 4], pal: &Palette, audio: &AudioState, n: usize, k: f32) {
        let [x, y, w, h] = rect;
        let pulse = audio.pulse();
        for i in 0..n {
            let s = i as u64 * 7919 + 13;
            let depth = 0.3 + hash01(s ^ 0xabc) * 0.7;
            let sx = (hash01(s) * w + f.t * 2.5 * depth) % w;
            let sy = (hash01(s ^ 0x55) * h + f.t * 0.6 * depth) % h;
            let tw = 0.55 + 0.45 * (f.t * (0.6 + hash01(s ^ 3) * 2.2) + hash01(s ^ 9) * 6.28).sin();
            let r = (0.6 + depth * 1.5) * k.max(0.6);
            let tint = if hash01(s ^ 77) > 0.8 { pal.light } else { WHITE };
            let a = (0.18 + 0.55 * depth) * tw * (1.0 + pulse * 0.4) * k.min(1.0);
            f.scene.shape(Shape::glow(x + sx, y + sy, r * 2.2, with_a(tint, a.min(1.0)), 2.2).additive());
        }
        // remnants of past constellations
        let t = f.t;
        self.remnants.retain(|r| t - r.born < 30.0);
        for r in &self.remnants {
            let age = t - r.born;
            let a = (1.0 - age / 30.0) * 0.55 * (0.6 + 0.4 * (t * 1.3 + r.seed * 6.0).sin());
            let ry = r.y - age * 3.0;
            if ry < y || ry > y + h {
                continue;
            }
            f.scene.shape(Shape::glow(r.x, ry, 3.2, with_a(r.color, a), 2.0).additive());
        }
        // occasional shooting star
        if k >= 1.0 {
            if self.shooting.is_none() && f.t > self.next_shoot {
                let sx = x + w * (0.2 + hash01(f.t.to_bits() as u64) * 0.6);
                let sy = y + h * (0.05 + hash01(f.t.to_bits() as u64 ^ 1) * 0.25);
                let dir = if hash01(f.t.to_bits() as u64 ^ 2) > 0.5 { 1.0 } else { -1.0 };
                self.shooting = Some((f.t, sx, sy, dir * w * 0.35, h * 0.22));
                self.next_shoot = f.t + 6.0 + hash01(f.t.to_bits() as u64 ^ 4) * 9.0;
            }
            if let Some((t0, sx, sy, dx, dy)) = self.shooting {
                let p = (f.t - t0) / 0.9;
                if p >= 1.0 {
                    self.shooting = None;
                } else {
                    let e = ease_out_cubic(p);
                    let hx = sx + dx * e;
                    let hy = sy + dy * e;
                    let tail = 0.22;
                    let e0 = ease_out_cubic((p - tail).max(0.0));
                    let tx = sx + dx * e0;
                    let ty = sy + dy * e0;
                    let a = (1.0 - p) * 0.9;
                    let mut sh = Shape::line(tx, ty, hx, hy, 1.6, with_a(pal.light, 0.0));
                    sh.c1 = with_a(WHITE, a);
                    f.scene.glow_shape(sh);
                    f.scene.glow_shape(Shape::glow(hx, hy, 6.0, with_a(WHITE, a), 1.5));
                }
            }
        }
    }

    fn warp_field(&mut self, f: &mut Frame, rect: [f32; 4], pal: &Palette, audio: &AudioState, n: usize) {
        if self.warp.len() != n {
            self.warp = (0..n)
                .map(|i| Warp {
                    ang: hash01(i as u64 * 31 + 1) * std::f32::consts::TAU,
                    rad: 0.15 + hash01(i as u64 * 31 + 2) * 0.85,
                    z: hash01(i as u64 * 31 + 3) * 4.0,
                    hue: hash01(i as u64 * 31 + 4),
                })
                .collect();
        }
        let [x, y, w, h] = rect;
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let speed = 0.35 + audio.level * 2.2 + audio.pulse() * 1.2;
        let dt = f.dt.min(0.05);
        let scale = w.max(h) * 0.5;
        for p in self.warp.iter_mut() {
            let z0 = p.z;
            p.z -= speed * dt;
            if p.z < 0.08 {
                p.z += 4.0;
                continue;
            }
            let proj = |z: f32| (p.ang.cos() * p.rad / z, p.ang.sin() * p.rad / z);
            let (ax, ay) = proj(z0.max(p.z + 0.02));
            let (bx, by) = proj(p.z);
            let (x0, y0) = (cx + ax * scale * 0.35, cy + ay * scale * 0.35);
            let (x1, y1) = (cx + bx * scale * 0.35, cy + by * scale * 0.35);
            if x1 < x || x1 > x + w || y1 < y || y1 > y + h {
                continue;
            }
            let a = ((4.0 - p.z) / 4.0).powf(1.6) * 0.8;
            let c = if p.hue > 0.5 { pal.primary } else { pal.secondary };
            let c = mix(c, WHITE, 0.35);
            let mut s = Shape::line(x0, y0, x1, y1, 1.0 + (1.0 / p.z).min(2.5), with_a(c, 0.0));
            s.c1 = with_a(c, a);
            f.scene.shape(s.additive());
        }
    }

    // ---------------------------------------------------------------- karaoke

    #[allow(clippy::too_many_arguments)]
    fn karaoke(
        &mut self,
        f: &mut Frame,
        rect: [f32; 4],
        ly: &LyricsState,
        media: &MediaState,
        pal: &Palette,
        audio: &AudioState,
        pos: f64,
        big: bool,
    ) {
        let size = self.font_size;
        let max_w = rect[2] * if big { 0.78 } else { 0.92 };
        self.ensure_layouts(f, ly, size, max_w);
        let plain = ly.status == LyricStatus::Plain;
        let cur = if plain {
            let frac = if media.duration > 0.0 { pos / media.duration } else { 0.0 };
            Some(((frac * ly.lines.len() as f64) as usize).min(ly.lines.len() - 1))
        } else {
            ly.line_at(pos)
        };
        let target = cur.map(|c| c as f32).unwrap_or(-0.6);
        if self.last_line != cur {
            self.last_line = cur;
        }
        self.scroll.set(target);
        let sv = self.scroll.step(f.dt.min(0.05));
        let focus_y = rect[1] + rect[3] * if big { 0.40 } else { 0.30 };
        let cx = rect[0] + rect[2] / 2.0;
        let off = self.cum_at(sv.max(0.0)) + if sv < 0.0 { sv * size * 1.8 } else { 0.0 };
        let clip = [rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]];

        // intro / instrumental gap dots
        let cur_empty = cur.map(|c| ly.lines[c].text.trim().is_empty()).unwrap_or(true);
        if !plain && cur_empty {
            let next_t = match cur {
                Some(c) => ly.lines.get(c + 1).map(|l| l.t),
                None => ly.lines.first().map(|l| l.t),
            };
            let start = cur.map(|c| ly.lines[c].t).unwrap_or(0.0);
            if let Some(nt) = next_t {
                if nt - pos > 1.2 || cur.is_none() {
                    let prog = ((pos - start) / (nt - start).max(0.1)).clamp(0.0, 1.0) as f32;
                    let dy = focus_y + if cur.is_none() { -size * 1.6 } else { size * 0.4 };
                    self.dots(f, cx, dy, size, prog, pal, audio);
                }
            }
        }

        let first = cur.unwrap_or(0).saturating_sub(4);
        let last = (cur.unwrap_or(0) + 7).min(ly.lines.len());
        for i in first..last {
            let line = &ly.lines[i];
            let Some(lay) = self.layouts.get(&i) else { continue };
            let d = i as f32 - sv;
            let s = 1.0 - 0.16 * d.abs().min(1.0);
            let y_top = focus_y + self.cum[i] - off;
            if y_top > rect[1] + rect[3] || y_top + lay.h < rect[1] {
                continue;
            }
            let fade_edge = smooth(rect[1], rect[1] + size * FADE_TOP, y_top)
                * smooth(rect[1] + rect[3], rect[1] + rect[3] - size * FADE_TOP, y_top + lay.h * 0.6);
            let is_cur = Some(i) == cur && !plain;
            let base_a = if plain {
                if Some(i) == cur { 0.95 } else { 0.42 }
            } else if is_cur {
                0.34
            } else {
                (0.40 - 0.07 * d.abs()).max(0.08)
            };
            for (j, wb) in lay.words.iter().enumerate() {
                let text = if line.words.is_empty() {
                    line.text.split_whitespace().nth(j).unwrap_or("").to_string()
                } else {
                    line.words[j].text.clone()
                };
                let wx = cx + wb.x * s;
                let mut wy = y_top + wb.y * s;
                let mut p = 0.0f32;
                if is_cur {
                    let w = &line.words[j];
                    p = ((pos - w.t) / (w.end - w.t).max(0.05)).clamp(0.0, 1.0) as f32;
                    let bump = if p > 0.0 && p < 1.0 { (p * std::f32::consts::PI).sin() } else { 0.0 };
                    wy -= bump * size * 0.06;
                }
                let base = Text::new(text, wx, wy, size, with_a(WHITE, base_a * fade_edge))
                    .family(Family::Lyric)
                    .weight(650)
                    .lh(1.2)
                    .scale(s)
                    .clip(clip);
                f.scene.text(base.clone());
                if is_cur && p > 0.0 {
                    let ww = wb.w * s;
                    let fill_x = wx + ww * p;
                    let lit = Text { color: with_a(WHITE, fade_edge), clip: Some([wx - 2.0, clip[1], fill_x + 1.0, clip[3]]), ..base.clone() };
                    f.scene.text(lit);
                    let glow_c = mix(pal.primary, WHITE, 0.25);
                    let ga = if p < 1.0 { 0.95 } else { 0.55 } * (1.0 + audio.pulse() * 0.4);
                    f.scene.halo_text(Text {
                        color: with_a(glow_c, (ga * fade_edge).min(1.0)),
                        clip: Some([wx - size, clip[1], fill_x + size * 0.3, clip[3]]),
                        ..base
                    });
                }
            }
        }
    }

    fn dots(&mut self, f: &mut Frame, cx: f32, cy: f32, size: f32, prog: f32, pal: &Palette, audio: &AudioState) {
        let r = size * 0.16;
        let gap = size * 0.62;
        let breathe = 1.0 + 0.12 * (f.t * 2.4).sin() + audio.pulse() * 0.15;
        for k in 0..3 {
            let lit = ((prog * 3.0) - k as f32).clamp(0.0, 1.0);
            let x = cx + (k as f32 - 1.0) * gap;
            let c = mix(with_a(WHITE, 0.25), mix(pal.primary, WHITE, 0.4), lit);
            f.scene.glow_shape(Shape::glow(x, cy, r * 2.0 * breathe, c, 1.0));
        }
    }

    // --------------------------------------------------------- constellation

    fn const_layout(&mut self, f: &mut Frame, i: usize, line: &Line, size: f32, max_w: f32) -> ConstLayout {
        if let Some(c) = self.consts.get(&i) {
            return c.clone();
        }
        let words: Vec<&str> = line.words.iter().map(|w| w.text.as_str()).collect();
        let widths: Vec<f32> = words.iter().map(|w| f.measure(&self.word_text(w, size)).0).collect();
        let gap = size * 1.25;
        let total: f32 = widths.iter().sum::<f32>() + gap * (widths.len().max(1) - 1) as f32;
        let rows = ((total / max_w).ceil() as usize).clamp(1, 4);
        let per = (words.len() + rows - 1) / rows.max(1);
        let row_h = size * 3.6;
        let mut stars = vec![];
        let mut labels = vec![];
        for r in 0..rows {
            let lo = r * per;
            let hi = ((r + 1) * per).min(words.len());
            if lo >= hi {
                continue;
            }
            let rw: f32 = widths[lo..hi].iter().sum::<f32>() + gap * (hi - lo - 1) as f32;
            let mut x = -rw / 2.0;
            let ry = (r as f32 - (rows as f32 - 1.0) / 2.0) * row_h;
            for j in lo..hi {
                let seed = (self.track ^ (i as u64 * 1_000_003) ^ (j as u64 * 7919)) as u64;
                let jit = (hash01(seed) - 0.5) * size * 2.0;
                // zig-zag so the links make shapes rather than a flat line
                let zig = if j % 2 == 0 { -1.0 } else { 1.0 } * size * 0.55;
                let sy = ry + jit + zig;
                let sx = x + widths[j] / 2.0;
                stars.push((sx, sy));
                let above = sy > ry;
                let ly = if above { sy - size * 1.95 } else { sy + size * 0.75 };
                labels.push((sx, ly));
                x += widths[j] + gap;
            }
        }
        let c = ConstLayout { stars, labels, widths };
        self.consts.insert(i, c.clone());
        c
    }

    /// Full vertical extent of a constellation (stars and labels), in px at scale 1.
    fn const_height(lay: &ConstLayout, size: f32) -> f32 {
        let (mut y0, mut y1) = (f32::MAX, f32::MIN);
        for (k, s) in lay.stars.iter().enumerate() {
            let lab = lay.labels[k].1;
            y0 = y0.min(s.1 - size * 0.4).min(lab);
            y1 = y1.max(s.1 + size * 0.4).max(lab + size * 1.25);
        }
        if y0 > y1 { size * 2.0 } else { (y1 - y0).max(size * 2.0) }
    }

    #[allow(clippy::too_many_arguments)]
    fn constellation(
        &mut self,
        f: &mut Frame,
        rect: [f32; 4],
        ly: &LyricsState,
        pal: &Palette,
        audio: &AudioState,
        pos: f64,
        big: bool,
    ) {
        if ly.track_id != self.track {
            self.track = ly.track_id;
            self.reset();
            self.remnants.clear();
        }
        let size = self.font_size;
        let max_w = rect[2] * if big { 0.74 } else { 0.9 };
        let center = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] * if big { 0.47 } else { 0.5 });
        let clip = [rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]];
        let cur = ly.line_at(pos);
        let next_idx = cur.map(|c| c + 1).unwrap_or(0);
        let mut to_draw: Vec<usize> = vec![];
        if let Some(c) = cur {
            if c > 0 {
                to_draw.push(c - 1);
            }
            to_draw.push(c);
        }
        if next_idx < ly.lines.len() {
            to_draw.push(next_idx);
        }
        let pulse = audio.pulse();
        let mut any_visible = false;
        for i in to_draw {
            let line = &ly.lines[i];
            if line.words.is_empty() {
                continue;
            }
            let lay = self.const_layout(f, i, line, size, max_w);
            // Handover between lines: the outgoing line drifts up while the incoming one rises
            // from below, over the same window, each travelling at least its own height, so the
            // two constellations never share the same space.
            let trans = 0.45f64; // half the handover duration (s)
            let lead = 0.25f64; // finish the handover just after the first word starts
            let gap_close = 2.5f64; // lines closer than this hand over directly
            let next = ly.lines.get(i + 1).filter(|n| !n.words.is_empty());
            let exit_mid = match next {
                Some(n) if n.t - line.end < gap_close => n.t - lead,
                _ => line.end + 1.6,
            };
            let prev = if i > 0 { ly.lines.get(i - 1).filter(|p| !p.words.is_empty()) } else { None };
            let enter_mid = match prev {
                Some(p) if line.t - p.end < gap_close => line.t - lead,
                _ => line.t - 0.5,
            };
            // Same easing for both sides of a handover, so at any moment the outgoing line has
            // moved up exactly as far as the incoming one still has to rise.
            let enter = ease_in_out(smooth((enter_mid - trans) as f32, (enter_mid + trans) as f32, pos as f32));
            let exit = ease_in_out(smooth((exit_mid - trans) as f32, (exit_mid + trans) as f32, pos as f32));
            let ga = enter * (1.0 - exit);
            if ga <= 0.003 {
                continue;
            }
            any_visible = true;
            // Travel distance is shared by the two lines of a handover and covers the taller
            // of them plus a gap, so their bounding boxes stay apart throughout.
            let h_self = Self::const_height(&lay, size);
            let h_prev = match prev {
                Some(p) => {
                    let l = self.const_layout(f, i - 1, p, size, max_w);
                    Self::const_height(&l, size)
                }
                None => h_self,
            };
            let h_next = match next {
                Some(n) => {
                    let l = self.const_layout(f, i + 1, n, size, max_w);
                    Self::const_height(&l, size)
                }
                None => h_self,
            };
            let travel_in = h_self.max(h_prev) + size * 2.5;
            let travel_out = h_self.max(h_next) + size * 2.5;
            let scale = 0.92 + 0.08 * enter - 0.08 * exit;
            let gy = center.1 + (1.0 - enter) * travel_in - exit * travel_out;
            let gx = center.0;
            let tf = |p: (f32, f32)| (gx + p.0 * scale, gy + p.1 * scale);

            // when the exit starts, leave faint stars behind
            if exit > 0.02 && !self.exited.contains(&i) {
                self.exited.insert(i);
                for (k, s) in lay.stars.iter().enumerate() {
                    let (sx, sy) = tf(*s);
                    self.remnants.push(Remnant {
                        x: sx,
                        y: sy,
                        born: f.t,
                        seed: hash01(k as u64 + i as u64 * 31),
                        color: if k % 2 == 0 { mix(pal.primary, WHITE, 0.5) } else { mix(pal.secondary, WHITE, 0.5) },
                    });
                }
                if self.remnants.len() > 400 {
                    let n = self.remnants.len() - 400;
                    self.remnants.drain(..n);
                }
            }

            let lit_of = |w: &Word| smooth((w.t - 0.06) as f32, (w.t + 0.2) as f32, pos as f32);
            // links
            for j in 1..lay.stars.len() {
                let w = &line.words[j];
                let prog = ease_out_cubic(smooth((w.t - 0.12) as f32, (w.t + 0.28) as f32, pos as f32));
                if prog <= 0.0 {
                    continue;
                }
                let (ax, ay) = tf(lay.stars[j - 1]);
                let (bx, by) = tf(lay.stars[j]);
                let ex = lerp(ax, bx, prog);
                let ey = lerp(ay, by, prog);
                let mut l = Shape::line(ax, ay, ex, ey, (size * 0.055).max(1.0), with_a(pal.primary, 0.75 * ga));
                l.c1 = with_a(pal.secondary, 0.75 * ga);
                f.scene.glow_shape(l.soft(0.6));
            }
            // closing link back to the first star once the line is complete (a "shape")
            if lay.stars.len() >= 4 {
                let lw = line.words.last().unwrap();
                let prog = ease_out_cubic(smooth(lw.end as f32, lw.end as f32 + 0.6, pos as f32));
                if prog > 0.0 && hash01(i as u64 ^ self.track) > 0.45 {
                    let (ax, ay) = tf(*lay.stars.last().unwrap());
                    let (bx, by) = tf(lay.stars[lay.stars.len() / 2 - 1]);
                    let mut l = Shape::line(ax, ay, lerp(ax, bx, prog), lerp(ay, by, prog), (size * 0.04).max(0.8), with_a(pal.secondary, 0.35 * ga));
                    l.c1 = with_a(pal.primary, 0.35 * ga);
                    f.scene.glow_shape(l);
                }
            }
            // stars + labels
            for (j, w) in line.words.iter().enumerate() {
                if j >= lay.stars.len() {
                    break;
                }
                let lit = lit_of(w);
                let active = pos >= w.t && pos < w.end;
                let (sx, sy) = tf(lay.stars[j]);
                let tw = 0.85 + 0.15 * (f.t * 3.0 + j as f32).sin();
                let r = size * (0.09 + 0.13 * lit) * (1.0 + pulse * 0.25 * lit) * tw;
                let c = if j % 2 == 0 { pal.primary } else { pal.secondary };
                f.scene.shape(Shape::glow(sx, sy, r * 5.5, with_a(c, 0.35 * lit * ga), 1.8).additive());
                f.scene.glow_shape(Shape::glow(sx, sy, r * 1.6, with_a(mix(c, WHITE, 0.7), (0.3 + 0.7 * lit) * ga), 1.2));
                if active || lit > 0.0 && (pos - w.t) < 0.5 {
                    let flare = if active { 1.0 } else { 1.0 - ((pos - w.t) as f32 / 0.5) };
                    f.scene.glow_shape(Shape::star(sx, sy, r * 6.0, with_a(WHITE, 0.9 * flare * ga), 0.9));
                }
                // label
                let (lx, lyy) = tf(lay.labels[j]);
                let la = (0.22 + 0.78 * lit) * ga;
                let txt = Text::new(w.text.clone(), lx, lyy, size, with_a(WHITE, la))
                    .family(Family::Lyric)
                    .weight(600)
                    .lh(1.2)
                    .center()
                    .scale(scale)
                    .clip(clip);
                if lit > 0.0 {
                    f.scene.halo_text(Text { color: with_a(mix(c, WHITE, 0.2), 0.7 * lit * ga), ..txt.clone() });
                }
                f.scene.text(txt);
            }
        }
        if !any_visible {
            // between lines: gentle dots toward next line
            if let Some(n) = ly.lines.get(next_idx) {
                let start = cur.map(|c| ly.lines[c].end).unwrap_or(0.0);
                if n.t - pos > 1.5 {
                    let prog = ((pos - start) / (n.t - start).max(0.1)).clamp(0.0, 1.0) as f32;
                    self.dots(f, center.0, center.1, size * 1.3, prog, pal, audio);
                }
            }
        }
    }

    // ----------------------------------------------------------------- drift

    #[allow(clippy::too_many_arguments)]
    fn drift(
        &mut self,
        f: &mut Frame,
        rect: [f32; 4],
        ly: &LyricsState,
        media: &MediaState,
        pal: &Palette,
        audio: &AudioState,
        pos: f64,
        big: bool,
    ) {
        let size = self.font_size;
        let max_w = rect[2] * if big { 0.7 } else { 0.9 };
        self.ensure_layouts(f, ly, size, max_w);
        let plain = ly.status == LyricStatus::Plain;
        let cur = if plain {
            let frac = if media.duration > 0.0 { pos / media.duration } else { 0.0 };
            Some(((frac * ly.lines.len() as f64) as usize).min(ly.lines.len() - 1))
        } else {
            ly.line_at(pos)
        };
        self.scroll.set(cur.map(|c| c as f32).unwrap_or(-1.0));
        let sv = self.scroll.step(f.dt.min(0.05));
        let (cx, cy) = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] * 0.47);
        let clip = [rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]];
        let spread = (rect[2] * 0.22, rect[3] * 0.2);
        let off = |i: f32| -> (f32, f32) {
            let a = i.floor();
            let b = a + 1.0;
            let fa = |k: f32| {
                let s = (k as i64 as u64).wrapping_mul(2654435761) ^ 0x9e37;
                ((hash01(s) - 0.5) * 2.0, (hash01(s ^ 0xff) - 0.5) * 2.0)
            };
            let (pa, pb) = (fa(a), fa(b));
            let t = ease_in_out(i - a);
            (lerp(pa.0, pb.0, t), lerp(pa.1, pb.1, t))
        };
        let cam = off(sv);
        let sway = ((f.t * 0.23).sin() * 0.06, (f.t * 0.17).cos() * 0.05);
        let first = cur.unwrap_or(0).saturating_sub(2);
        let last = (cur.unwrap_or(0) + 6).min(ly.lines.len());
        let pulse = audio.pulse();
        for i in (first..last).rev() {
            let z = (i as f32 - sv) * 1.0;
            if z < -0.85 || z > 5.0 {
                continue;
            }
            let persp = 1.0 / (1.0 + z * 0.62);
            let o = off(i as f32);
            let px = cx + ((o.0 - cam.0) + sway.0) * spread.0 * persp * 2.0;
            let py = cy + ((o.1 - cam.1) + sway.1) * spread.1 * persp * 2.0;
            let alpha = if z >= 0.0 { (1.0 - z / 5.0).powf(1.6) } else { (1.0 + z * 1.25).max(0.0) };
            let Some(lay) = self.layouts.get(&i) else { continue };
            let line = &ly.lines[i];
            let is_cur = Some(i) == cur && !plain;
            let qs = ((persp * 50.0).round() / 50.0).max(0.1);
            let top = py - lay.h * qs / 2.0;
            let dim = if is_cur { 0.36 } else { 0.55 };
            for (j, wb) in lay.words.iter().enumerate() {
                let text = if line.words.is_empty() {
                    line.text.split_whitespace().nth(j).unwrap_or("").to_string()
                } else {
                    line.words[j].text.clone()
                };
                let mut p = 0.0;
                let mut pop = 1.0;
                if is_cur {
                    let w = &line.words[j];
                    p = ((pos - w.t) / (w.end - w.t).max(0.05)).clamp(0.0, 1.0) as f32;
                    if p > 0.0 && p < 1.0 {
                        pop = 1.0 + 0.07 * (p * std::f32::consts::PI).sin();
                    }
                }
                let sc = qs * pop;
                let wx = px + wb.x * qs - wb.w * (sc - qs) * 0.5;
                let wy = top + wb.y * qs - size * (sc - qs) * 0.5;
                let base = Text::new(text, wx, wy, size, with_a(WHITE, dim * alpha))
                    .family(Family::Lyric)
                    .weight(700)
                    .lh(1.2)
                    .scale(sc)
                    .clip(clip);
                f.scene.text(base.clone());
                if is_cur && p > 0.0 {
                    let fill_x = wx + wb.w * sc * p;
                    f.scene.text(Text { color: with_a(WHITE, alpha), clip: Some([wx - 2.0, clip[1], fill_x + 1.0, clip[3]]), ..base.clone() });
                    let gc = mix(if j % 2 == 0 { pal.primary } else { pal.secondary }, WHITE, 0.2);
                    f.scene.halo_text(Text {
                        color: with_a(gc, (0.85 + pulse * 0.3).min(1.0) * alpha),
                        clip: Some([wx - size, clip[1], fill_x + size * 0.3, clip[3]]),
                        ..base
                    });
                }
            }
        }
    }
}
