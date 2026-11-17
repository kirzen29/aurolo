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
}

impl Fullscreen {
    pub fn new(style: LyricStyle) -> Self {
        Fullscreen { view: LyricView::new(style), ui_vis: 1.0, last_move: 0.0, last_scroll: -9.0, toast: None, eq: vec![] }
    }

    /// Soft equalizer along the bottom edge: thin bars rising from the bottom, tinted by
    /// the album colours, mirrored from the centre so bass sits in the middle.
    fn equalizer(&mut self, f: &mut Frame, pal: &Palette, audio: &AudioState, playing: bool) {
        let n = 72usize;
        if self.eq.len() != n {
            self.eq = vec![0.0; n];
        }
        let (w, h) = (f.w, f.h);
        let half = n / 2;
        for i in 0..n {
            // distance from the centre picks the band: low frequencies in the middle
            let d = (i as i32 - half as i32).unsigned_abs() as usize;
            let band = (d * (BANDS - 1) / half.max(1)).min(BANDS - 1);
            let target = if playing { audio.bands[band].clamp(0.0, 1.0) } else { 0.0 };
            let cur = self.eq[i];
            // fast rise, slow fall, like a real meter
            let rate = if target > cur { 22.0 } else { 5.0 };
            self.eq[i] = approach(cur, target, rate, f.dt);
        }
        let gap = 3.0;
        let bw = (w - gap * (n as f32 - 1.0)) / n as f32;
        let max_h = h * 0.11;
        for (i, v) in self.eq.iter().enumerate() {
            let bh = 2.0 + max_h * v.powf(0.85);
            let x = i as f32 * (bw + gap);
            let t = i as f32 / (n - 1) as f32;
            let c = mix(pal.primary, pal.secondary, (t * 2.0 - 1.0).abs());
            let c = mix(c, WHITE, 0.25);
            f.scene.shape(Shape::rect(x, h - bh, bw, bh + 4.0, bw.min(4.0) / 2.0, with_a(c, 0.0)).grad(
                with_a(c, 0.30),
                std::f32::consts::FRAC_PI_2,
            ));
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
        if input.key(Key::BracketL) {
            action = FsAction::Offset(-0.25);
            self.toast = Some(("Lyrics earlier".into(), f.t));
        }
        if input.key(Key::BracketR) {
            action = FsAction::Offset(0.25);
            self.toast = Some(("Lyrics later".into(), f.t));
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
                Text::new("Scroll to change style  ·  Esc to close", w - 40.0, h - 30.0, 11.0, with_a(WHITE, 0.32 * v)).right(),
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
