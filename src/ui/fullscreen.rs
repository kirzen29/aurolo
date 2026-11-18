//! Fullscreen lyrics: a calm, uncluttered view over the song's own artwork.
//! Mouse wheel (or ←/→) switches style; controls appear only while the mouse moves.

use crate::anim::*;
use crate::config::LyricStyle;
use crate::gfx::*;
use crate::state::*;
use crate::ui::lyrics_view::LyricView;
use crate::ui::{ic, Frame, Input, Key};

pub enum FsAction {
    None,
    Close,
    StyleChanged(LyricStyle),
    Offset(f64),
}

pub struct Fullscreen {
    pub view: LyricView,
    ui_vis: f32,
    last_move: f32,
    last_scroll: f32,
    toast: Option<(String, f32)>,
    eq: Vec<f32>,
    peaks: Vec<f32>,
}

impl Fullscreen {
    pub fn new(style: LyricStyle) -> Self {
        Fullscreen { view: LyricView::new(style), ui_vis: 1.0, last_move: 0.0, last_scroll: -9.0, toast: None, eq: vec![], peaks: vec![] }
    }

    /// Visualiser along the bottom edge: smooth rolling curves shaped by the music.
    /// The spectrum is gathered into a dozen soft control points per side (bass in the
    /// middle, highs toward the edges), levelled per band so the whole width moves, then
    /// drawn as a filled curve with a gentle crest line. A softer second curve in the
    /// other album colour drifts behind it. Both swell a little on the beat.
    fn equalizer(&mut self, f: &mut Frame, pal: &Palette, audio: &AudioState, playing: bool) {
        const K: usize = 12; // control points per half
        if self.eq.len() != K || self.peaks.len() != BANDS {
            self.eq = vec![0.0; K];
            self.peaks = vec![0.15; BANDS];
        }
        // level each band against its own slowly-decaying peak
        let mut lev = [0.0f32; BANDS];
        for i in 0..BANDS {
            let raw = audio.bands[i].clamp(0.0, 1.0);
            self.peaks[i] = (self.peaks[i] * (1.0 - 0.12 * f.dt)).max(raw).max(0.08);
            lev[i] = if playing { (raw / self.peaks[i]).clamp(0.0, 1.0) } else { 0.0 };
        }
        // gather bands into K log-spaced control points, smooth them over time
        for k in 0..K {
            let lo = 1 + ((k as f32 / K as f32).powf(1.6) * (BANDS - 2) as f32) as usize;
            let hi = (1 + (((k + 1) as f32 / K as f32).powf(1.6) * (BANDS - 2) as f32) as usize).max(lo + 1).min(BANDS);
            let avg = lev[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
            let target = avg.powf(1.5);
            let rate = if target > self.eq[k] { 10.0 } else { 3.5 };
            self.eq[k] = approach(self.eq[k], target, rate, f.dt);
        }
        let (w, h) = (f.w, f.h);
        let pulse = audio.pulse();
        let t = f.t;
        let cp = &self.eq;
        // smooth value across the width (Catmull-Rom through the mirrored control points)
        let at = |u: f32, shift: f32| -> f32 {
            let d = (((u - 0.5).abs() * 2.0) + shift).clamp(0.0, 1.0);
            let x = d * (K - 1) as f32;
            let i = x.floor() as i32;
            let fr = x - i as f32;
            let g = |j: i32| cp[j.clamp(0, K as i32 - 1) as usize];
            let (p0, p1, p2, p3) = (g(i - 1), g(i), g(i + 1), g(i + 2));
            let v = 0.5
                * (2.0 * p1
                    + (-p0 + p2) * fr
                    + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * fr * fr
                    + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * fr * fr * fr);
            // settle toward the corners
            v.max(0.0) * (1.0 - d.powf(2.5) * 0.7)
        };
        let max_h = h * 0.085;
        let base = 2.0 + 1.5 * (t * 1.1).sin().abs();
        let c_back = mix(pal.secondary, WHITE, 0.25);
        let c_front = mix(pal.primary, WHITE, 0.35);
        let cols = 240usize;
        let mut back = Vec::with_capacity(cols + 1);
        let mut front = Vec::with_capacity(cols + 1);
        for k in 0..=cols {
            let u = k as f32 / cols as f32;
            let sway = 0.06 * (u * 7.0 - t * 0.7).sin();
            let vb = at(u, sway) * 1.1;
            let vf = at(u, 0.0);
            back.push(h - (base + max_h * vb * (1.0 + 0.2 * pulse)));
            front.push(h - (base + max_h * vf * (1.0 + 0.3 * pulse)));
        }
        // filled bodies: pixel-snapped columns (no seams), fading upward
        let mut fill = |tops: &Vec<f32>, c: Rgba, a_top: f32, a_bot: f32| {
            for k in 0..cols {
                let x0 = (k as f32 * w / cols as f32).round();
                let x1 = ((k + 1) as f32 * w / cols as f32).round();
                let y = (tops[k] + tops[k + 1]) / 2.0;
                f.scene.shape(Shape::rect(x0, y, x1 - x0, h - y, 0.0, with_a(c, a_top)).grad(with_a(c, a_bot), std::f32::consts::FRAC_PI_2));
            }
        };
        fill(&back, c_back, 0.05, 0.14);
        fill(&front, c_front, 0.08, 0.22);
        // crest lines
        for k in 0..cols {
            let (x0, x1) = (k as f32 * w / cols as f32, (k + 1) as f32 * w / cols as f32);
            let mut lb = Shape::line(x0, back[k], x1, back[k + 1], 1.0, with_a(c_back, 0.30));
            lb.c1 = lb.c0;
            f.scene.shape(lb);
            let mut lf = Shape::line(x0, front[k], x1, front[k + 1], 1.6, with_a(c_front, 0.6));
            lf.c1 = lf.c0;
            f.scene.shape(lf);
        }
    }

    fn set_style(&mut self, s: LyricStyle, t: f32) -> FsAction {
        self.view.style = s;
        self.toast = Some((s.name().into(), t));
        FsAction::StyleChanged(s)
    }

    pub fn draw(&mut self, f: &mut Frame, input: &Input, shared: &Shared) -> FsAction {
        let media = shared.media.read().clone();
        let ly = shared.lyrics.read().clone();
        let audio = shared.audio.lock().clone();
        let pal = media.palette();
        let pos = shared.lyric_pos() + 0.06;
        let (w, h) = (f.w, f.h);
        let mut action = FsAction::None;

        if input.moved || input.pressed {
            self.last_move = f.t;
        }
        let show = f.t - self.last_move < 2.4;
        self.ui_vis = approach(self.ui_vis, if show { 1.0 } else { 0.0 }, 8.0, f.dt);

        self.view.draw(f, [0.0, 0.0, w, h], &ly, &media, &pal, &audio, pos, true);
        self.equalizer(f, &pal, &audio, media.playing);
        f.scene.layer();
        if self.view.style == LyricStyle::Constellation {
            corner_clock(f, &pal);
        }

        // ---- input: wheel / arrows switch style, Esc closes
        if input.scroll.abs() > 0.01 && f.t - self.last_scroll > 0.32 {
            self.last_scroll = f.t;
            let s = if input.scroll < 0.0 { self.view.style.next() } else { self.view.style.prev() };
            action = self.set_style(s, f.t);
        }
        if input.key(Key::Right) || input.key(Key::Tab) {
            action = self.set_style(self.view.style.next(), f.t);
        }
        if input.key(Key::Left) {
            action = self.set_style(self.view.style.prev(), f.t);
        }
        if input.key(Key::Esc) {
            action = FsAction::Close;
        }
        if input.key(Key::Space) {
            shared.media_cmd(MediaCmd::PlayPause);
        }
        // ---- lyrics sync: + pushes the lyrics ahead, − holds them back
        let offset = *shared.lyrics_offset.read();
        let mut nudge = 0.0;
        if input.key(Key::Plus) || input.key(Key::BracketR) {
            nudge += SYNC_STEP;
        }
        if input.key(Key::Minus) || input.key(Key::BracketL) {
            nudge -= SYNC_STEP;
        }

        if nudge != 0.0 {
            // snap to the step grid so repeated presses read cleanly (+0.1, +0.2 …)
            let target = ((offset + nudge) / SYNC_STEP).round() * SYNC_STEP;
            action = FsAction::Offset(target - offset);
            let hint = if target > 1e-6 { "  ·  lyrics ahead" } else if target < -1e-6 { "  ·  lyrics behind" } else { "" };
            self.toast = Some((format!("{}{hint}", sync_label(target)), f.t));
        }

        // ---- track info, bottom-left (quiet)
        let a = 0.5 + 0.45 * self.ui_vis;
        let base_y = h - 86.0;
        let mut tx = 44.0;
        if let Some(art) = &media.art {
            let (k, _) = f.ensure_art(art);
            f.scene.image(ImageDraw::new(k, 44.0, base_y, 48.0, 48.0, 10.0).opacity(a));
            tx += 62.0;
        }
        if media.active {
            let t = f.fit(Text::new(&media.title, tx, base_y + 4.0, 16.0, with_a(WHITE, 0.92 * a)).weight(650), w * 0.32);
            f.scene.text(t);
            let ar = f.fit(Text::new(&media.artist, tx, base_y + 26.0, 13.0, with_a(WHITE, 0.55 * a)), w * 0.32);
            f.scene.text(ar);
        }

        if self.ui_vis > 0.01 {
            let v = self.ui_vis;
            // progress + controls, bottom centre
            if media.duration > 0.0 {
                let bw = w * 0.30;
                let bx = (w - bw) / 2.0;
                let by = h - 50.0;
                let frac = (media.pos_now() / media.duration) as f32;
                let r = [bx, by - 8.0, bw, 20.0];
                f.scene.shape(Shape::rect(bx, by, bw, 3.0, 1.5, with_a(WHITE, 0.16 * v)));
                f.scene.shape(Shape::rect(bx, by, (bw * frac.clamp(0.0, 1.0)).max(3.0), 3.0, 1.5, with_a(WHITE, 0.85 * v)));
                if input.clicked(r) {
                    if let Some((mx, _)) = input.mouse {
                        shared.media_cmd(MediaCmd::Seek(((mx - bx) / bw).clamp(0.0, 1.0) as f64 * media.duration));
                    }
                }
                f.scene.text(Text::new(fmt_time(media.pos_now()), bx - 12.0, by - 7.0, 11.5, with_a(WHITE, 0.5 * v)).right());
                f.scene.text(Text::new(fmt_time(media.duration), bx + bw + 12.0, by - 7.0, 11.5, with_a(WHITE, 0.5 * v)));
                let cy = by - 32.0;
                let cx = w / 2.0;
                if f.icon_button(input, ic::PREV, cx - 50.0, cy, 16.0, with_a(WHITE, 0.85 * v)) {
                    shared.media_cmd(MediaCmd::Prev);
                }
                let pp = if media.playing { ic::PAUSE } else { ic::PLAY };
                if f.icon_button(input, pp, cx, cy, 21.0, with_a(WHITE, v)) {
                    shared.media_cmd(MediaCmd::PlayPause);
                }
                if f.icon_button(input, ic::NEXT, cx + 50.0, cy, 16.0, with_a(WHITE, 0.85 * v)) {
                    shared.media_cmd(MediaCmd::Next);
                }
            }
            // style: name + a row of dots (click a dot to jump), top centre
            let names = LyricStyle::ALL;
            f.scene.text(Text::new(self.view.style.name(), w / 2.0, 26.0, 13.0, with_a(WHITE, 0.7 * v)).weight(600).center());
            let gap = 14.0;
            let x0 = w / 2.0 - gap * (names.len() - 1) as f32 / 2.0;
            for (i, s) in names.iter().enumerate() {
                let dx = x0 + gap * i as f32;
                let on = *s == self.view.style;
                let r = if on { 3.4 } else { 2.4 };
                f.scene.shape(Shape::rect(dx - r, 54.0 - r, r * 2.0, r * 2.0, r, with_a(WHITE, if on { 0.9 } else { 0.3 } * v)));
                if input.clicked([dx - 7.0, 46.0, 14.0, 16.0]) && !on {
                    action = self.set_style(*s, f.t);
                }
            }
            if f.icon_button(input, ic::CLOSE, w - 40.0, 40.0, 14.0, with_a(WHITE, 0.75 * v)) {
                action = FsAction::Close;
            }
            f.scene.text(
                Text::new("Scroll to change style  ·  + / − to sync  ·  Esc to close",w - 40.0, h - 30.0, 11.0, with_a(WHITE, 0.32 * v)).right(),
            );
        }
        if let Some((msg, t0)) = &self.toast {
            let age = f.t - t0;
            if age > 1.4 {
                self.toast = None;
            } else {
                let a = smooth(0.0, 0.12, age) * (1.0 - smooth(1.0, 1.4, age));
                let t = Text::new(msg.clone(), w / 2.0, h * 0.86, 14.0, with_a(WHITE, a)).weight(600).center();
                let (tw, _) = f.measure(&t);
                f.scene.shape(Shape::rect(w / 2.0 - tw / 2.0 - 14.0, h * 0.86 - 7.0, tw + 28.0, 32.0, 16.0, [0.0, 0.0, 0.0, 0.35 * a]));
                f.scene.text(t);
            }
        }
        action
    }
}

/// One press of + / − moves the lyrics by this many seconds.
const SYNC_STEP: f64 = 0.1;

fn sync_label(offset: f64) -> String {
    if offset.abs() < 0.05 {
        "Sync 0.0s".into()
    } else {
        format!("Sync {:+.1}s", offset)
    }
}

/// Constellation view: a thin clock and the date in the top-left corner, nothing more.
fn corner_clock(f: &mut Frame, pal: &Palette) {
    let lt = crate::platform::local_time();
    let (x, y) = (46.0, 32.0);
    let tint = mix(pal.light, WHITE, 0.5);
    let time = Text::new(format!("{:02}:{:02}", lt.hour, lt.minute), x, y, 52.0, with_a(tint, 0.9))
        .family(Family::Display)
        .weight(250)
        .lh(1.1)
        .shadow_a(0.3);
    f.scene.text(time);
    let date = format!(
        "{} \u{B7} {} {}",
        crate::platform::WEEKDAYS[lt.weekday as usize % 7],
        lt.day,
        crate::platform::MONTHS[(lt.month as usize + 11) % 12]
    );
    f.scene.text(Text::new(date, x + 2.0, y + 64.0, 13.5, with_a(tint, 0.55)).weight(500).shadow_a(0.3));
}
