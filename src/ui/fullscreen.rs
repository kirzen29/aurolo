//! Fullscreen lyrics: constellation / karaoke / 3D drift with auto-hiding controls.

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
    toast: Option<(String, f32)>,
}

impl Fullscreen {
    pub fn new(style: LyricStyle) -> Self {
        Fullscreen { view: LyricView::new(style), ui_vis: 1.0, last_move: 0.0, toast: None }
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
        let show = f.t - self.last_move < 2.8;
        self.ui_vis = approach(self.ui_vis, if show { 1.0 } else { 0.0 }, 8.0, f.dt);

        self.view.draw(f, [0.0, 0.0, w, h], &ly, &media, &pal, &audio, pos, true);
        f.scene.layer();

        // keyboard
        if input.key(Key::Esc) {
            action = FsAction::Close;
        }
        if input.key(Key::Space) {
            shared.media_cmd(MediaCmd::PlayPause);
        }
        if input.key(Key::Right) || input.key(Key::Tab) {
            self.view.style = self.view.style.next();
            action = FsAction::StyleChanged(self.view.style);
            self.toast = Some((self.view.style.name().into(), f.t));
        }
        if input.key(Key::Left) {
            self.view.style = self.view.style.next().next();
            action = FsAction::StyleChanged(self.view.style);
            self.toast = Some((self.view.style.name().into(), f.t));
        }
        if input.key(Key::BracketL) {
            action = FsAction::Offset(-0.25);
            self.toast = Some(("Lyrics earlier".into(), f.t));
        }
        if input.key(Key::BracketR) {
            action = FsAction::Offset(0.25);
            self.toast = Some(("Lyrics later".into(), f.t));
        }

        // track info (always, subtle) bottom-left
        let a = 0.55 + 0.45 * self.ui_vis;
        let base_y = h - 92.0;
        let mut tx = 44.0;
        if let Some(art) = &media.art {
            let (k, _) = f.ensure_art(art);
            f.scene.shape(Shape::glow(44.0 + 28.0, base_y + 28.0, 50.0, with_a(pal.primary, 0.3 * a), 2.0));
            f.scene.image(ImageDraw::new(k, 44.0, base_y, 56.0, 56.0, 12.0).opacity(a));
            tx += 70.0;
        }
        if media.active {
            let t = f.fit(Text::new(&media.title, tx, base_y + 6.0, 18.0, with_a(WHITE, a)).weight(650), w * 0.35);
            f.scene.text(t);
            let ar = f.fit(Text::new(&media.artist, tx, base_y + 30.0, 14.0, with_a(WHITE, 0.6 * a)), w * 0.35);
            f.scene.text(ar);
        }

        if self.ui_vis > 0.01 {
            let v = self.ui_vis;
            // progress
            if media.duration > 0.0 {
                let bw = w * 0.36;
                let bx = (w - bw) / 2.0;
                let by = h - 54.0;
                let frac = (media.pos_now() / media.duration) as f32;
                let r = [bx, by - 8.0, bw, 20.0];
                f.bar(bx, by, bw, 4.0, frac, with_a(pal.primary, v), with_a(pal.secondary, v));
                if input.clicked(r) {
                    if let Some((mx, _)) = input.mouse {
                        shared.media_cmd(MediaCmd::Seek(((mx - bx) / bw).clamp(0.0, 1.0) as f64 * media.duration));
                    }
                }
                f.scene.text(Text::new(fmt_time(media.pos_now()), bx - 12.0, by - 7.0, 12.0, with_a(WHITE, 0.6 * v)).right());
                f.scene.text(Text::new(fmt_time(media.duration), bx + bw + 12.0, by - 7.0, 12.0, with_a(WHITE, 0.6 * v)));
                // controls
                let cy = by - 34.0;
                let cx = w / 2.0;
                if f.icon_button(input, ic::PREV, cx - 54.0, cy, 18.0, with_a(WHITE, v)) {
                    shared.media_cmd(MediaCmd::Prev);
                }
                let pp = if media.playing { ic::PAUSE } else { ic::PLAY };
                if f.icon_button(input, pp, cx, cy, 24.0, with_a(WHITE, v)) {
                    shared.media_cmd(MediaCmd::PlayPause);
                }
                if f.icon_button(input, ic::NEXT, cx + 54.0, cy, 18.0, with_a(WHITE, v)) {
                    shared.media_cmd(MediaCmd::Next);
                }
            }
            // style switcher top-centre
            let names = LyricStyle::ALL;
            let ph = 34.0;
            let mut widths = vec![];
            for s in names {
                let t = Text::new(s.name(), 0.0, 0.0, ph * 0.42, WHITE).weight(600);
                widths.push(f.measure(&t).0 + ph * 0.9);
            }
            let total: f32 = widths.iter().sum::<f32>() + 8.0 * (names.len() - 1) as f32 + 12.0;
            let mut x = (w - total) / 2.0;
            let y = 28.0;
            f.scene.shape(Shape::rect(x - 6.0, y - 6.0, total, ph + 12.0, (ph + 12.0) / 2.0, [0.0, 0.0, 0.0, 0.35 * v]));
            for (i, s) in names.iter().enumerate() {
                let (c, bw) = f.pill(input, s.name(), x, y, ph, *s == self.view.style, with_a(WHITE, v));
                if c {
                    self.view.style = *s;
                    action = FsAction::StyleChanged(*s);
                }
                x += bw + 8.0;
                let _ = i;
            }
            // close + offset
            if f.icon_button(input, ic::CLOSE, w - 44.0, 45.0, 16.0, with_a(WHITE, v)) {
                action = FsAction::Close;
            }
            let off = *shared.lyrics_offset.read();
            let oy = h - 62.0;
            let ox = w - 160.0;
            if f.icon_button(input, ic::MINUS, ox, oy, 12.0, with_a(WHITE, 0.8 * v)) {
                action = FsAction::Offset(-0.25);
            }
            f.scene.text(Text::new(format!("sync {:+.2}s", off), ox + 50.0, oy - 8.0, 12.0, with_a(WHITE, 0.7 * v)).center());
            if f.icon_button(input, ic::ADD, ox + 100.0, oy, 12.0, with_a(WHITE, 0.8 * v)) {
                action = FsAction::Offset(0.25);
            }
            f.scene.text(
                Text::new("Esc close · ←/→ style · Space play · [ ] sync", w - 44.0, h - 30.0, 11.0, with_a(WHITE, 0.4 * v))
                    .right(),
            );
        }
        if let Some((msg, t0)) = &self.toast {
            let age = f.t - t0;
            if age > 1.6 {
                self.toast = None;
            } else {
                let a = smooth(0.0, 0.15, age) * (1.0 - smooth(1.2, 1.6, age));
                let t = Text::new(msg.clone(), w / 2.0, h * 0.84, 15.0, with_a(WHITE, a)).weight(600).center();
                let (tw, _) = f.measure(&t);
                f.scene.shape(Shape::rect(w / 2.0 - tw / 2.0 - 16.0, h * 0.84 - 8.0, tw + 32.0, 34.0, 17.0, [0.0, 0.0, 0.0, 0.45 * a]));
                f.scene.text(t);
            }
        }
        action
    }
}
