//! Floating lyrics widget. Two layouts:
//! * full        – album art, title/artist, playback controls and lyrics
//! * lyrics only – just the lyrics on a slim glass card
//! Drag anywhere to move; drag the bottom-right grip to resize.

use crate::anim::*;
use crate::config::{LyricStyle, WidgetBg};
use crate::gfx::*;
use crate::platform;
use crate::state::*;
use crate::ui::lyrics_view::LyricView;
use crate::ui::{ic, Frame, Input};

pub enum WidgetAction {
    None,
    Fullscreen,
    Close,
    Drag,
    StyleChanged(LyricStyle),
    ToggleLayout,
    BgChanged(WidgetBg),
    Resize(f32, f32),
    ResizeEnd(f32, f32),
}

pub struct Widget {
    pub view: LyricView,
    pub lyrics_only: bool,
    pub bg: WidgetBg,
    /// True while the cursor is over the resize grip (app shows a resize cursor).
    pub cursor_resize: bool,
    hover: f32,
    resizing: Option<((i32, i32), (f32, f32))>,
    last_size: (f32, f32),
}

pub const MIN_FULL: (f32, f32) = (300.0, 118.0);
pub const MIN_LYRICS: (f32, f32) = (200.0, 46.0);
pub const MAX: (f32, f32) = (1600.0, 700.0);
const GRIP: f32 = 22.0;

impl Widget {
    pub fn new(style: LyricStyle, lyrics_only: bool, bg: WidgetBg) -> Self {
        Widget {
            view: LyricView::new(style),
            lyrics_only,
            bg,
            cursor_resize: false,
            hover: 0.0,
            resizing: None,
            last_size: (0.0, 0.0),
        }
    }

    pub fn min_size(lyrics_only: bool) -> (f32, f32) {
        if lyrics_only { MIN_LYRICS } else { MIN_FULL }
    }

    pub fn draw(&mut self, f: &mut Frame, input: &Input, shared: &Shared) -> WidgetAction {
        let media = shared.media.read().clone();
        let ly = shared.lyrics.read().clone();
        let audio = shared.audio.lock().clone();
        let pal = media.palette();
        let pos = shared.lyric_pos() + 0.06;
        let (w, h) = (f.w, f.h);
        let card = [0.0, 0.0, w, h];
        let r = if self.lyrics_only { (h * 0.32).clamp(16.0, 26.0) } else { 26.0 };
        let over = input.over(card) || self.resizing.is_some();
        self.hover = approach(self.hover, if over { 1.0 } else { 0.0 }, 10.0, f.dt);
        let mut action = WidgetAction::None;

        // ---- resize (tracks the global cursor so it keeps working as the window grows)
        let grip = [w - GRIP, h - GRIP, GRIP, GRIP];
        let on_grip = input.over(grip);
        self.cursor_resize = on_grip || self.resizing.is_some();
        if input.pressed && on_grip {
            self.resizing = Some((platform::cursor_pos(), (w, h)));
        }
        if let Some(((sx, sy), (sw, sh))) = self.resizing {
            let (cx, cy) = platform::cursor_pos();
            let s = f.painter.scale;
            let min = Self::min_size(self.lyrics_only);
            let nw = (sw + (cx - sx) as f32 / s).clamp(min.0, MAX.0);
            let nh = (sh + (cy - sy) as f32 / s).clamp(min.1, MAX.1);
            if !input.down {
                self.resizing = None;
                action = WidgetAction::ResizeEnd(nw, nh);
            } else if (nw, nh) != self.last_size {
                self.last_size = (nw, nh);
                action = WidgetAction::Resize(nw, nh);
            }
        }
        // ---- mouse wheel over the widget: smaller / bigger (keeps proportions)
        if self.resizing.is_none() && input.scroll.abs() > 0.01 && input.over(card) {
            let k = (1.0 + 0.08 * input.scroll).clamp(0.7, 1.4);
            let min = Self::min_size(self.lyrics_only);
            let k = k.max(min.0 / w).max(min.1 / h).min(MAX.0 / w).min(MAX.1 / h);
            action = WidgetAction::ResizeEnd(w * k, h * k);
        }

        // ---- card
        let d = pal.dark;
        let tint = [d[0] * 0.7 + 0.02, d[1] * 0.7 + 0.02, d[2] * 0.7 + 0.03];
        let rim = mix(pal.primary, WHITE, 0.6);
        match self.bg {
            WidgetBg::Solid => {
                f.scene.shape(Shape::rect(0.0, 0.0, w, h, r, [tint[0], tint[1], tint[2], 0.96]));
                if let Some(art) = &media.art {
                    let (_, kb) = f.ensure_art(art);
                    let s = w.max(h) * 1.2;
                    f.scene.image(ImageDraw::new(kb, (w - s) / 2.0, (h - s) / 2.0, s, s, r).opacity(0.32).saturation(1.3));
                    f.scene.layer();
                }
                f.scene.shape(
                    Shape::rect(0.0, 0.0, w, h, r, [0.0, 0.0, 0.0, 0.0])
                        .grad([0.0, 0.0, 0.0, 0.35], std::f32::consts::FRAC_PI_2),
                );
                f.scene.shape(Shape::stroke(0.0, 0.0, w, h, r, 1.0, with_a(rim, 0.22)).grad(with_a(WHITE, 0.05), 1.2));
            }
            WidgetBg::Glass => {
                // translucent tinted pane: what's behind stays visible
                f.scene.shape(Shape::rect(0.0, 0.0, w, h, r, [tint[0], tint[1], tint[2], 0.42]));
                // light album-colour wash
                f.scene.shape(Shape::rect(0.0, 0.0, w, h, r, with_a(pal.primary, 0.10)).grad(with_a(pal.secondary, 0.06), 0.4));
                // top sheen like real glass
                f.scene.shape(
                    Shape::rect(0.0, 0.0, w, h, r, with_a(WHITE, 0.10)).grad(with_a(WHITE, 0.0), std::f32::consts::FRAC_PI_2 * 0.9),
                );
                // bright rim, strongest top-left
                f.scene.shape(Shape::stroke(0.0, 0.0, w, h, r, 1.2, with_a(WHITE, 0.38)).grad(with_a(rim, 0.10), 0.9));
            }
            WidgetBg::None => {
                // invisible until hovered, so it can still be grabbed and moved
                let a = self.hover;
                if a > 0.01 {
                    f.scene.shape(Shape::rect(0.0, 0.0, w, h, r, [0.0, 0.0, 0.0, 0.16 * a]));
                    f.scene.shape(Shape::stroke(0.0, 0.0, w, h, r, 1.0, with_a(WHITE, 0.28 * a)));
                }
            }
        }

        let pad = 16.0;
        let lyric_area;
        // "No background" is pure floating lyrics: never show art / title / artist.
        let header = !self.lyrics_only && self.bg != WidgetBg::None;
        if !header {
            lyric_area = [pad, 8.0, w - pad * 2.0, h - 16.0];
        } else {
            // header: art + title
            let art_s = 40.0;
            if let Some(art) = &media.art {
                let (k, _) = f.ensure_art(art);
                f.scene.shape(Shape::glow(pad + art_s / 2.0, pad + art_s / 2.0, art_s * 0.9, with_a(pal.primary, 0.35), 2.0));
                f.scene.image(ImageDraw::new(k, pad, pad, art_s, art_s, 10.0));
            } else {
                f.scene.shape(Shape::rect(pad, pad, art_s, art_s, 10.0, [1.0, 1.0, 1.0, 0.08]));
                f.icon(ic::MUSIC, pad + art_s / 2.0, pad + art_s / 2.0, 16.0, with_a(WHITE, 0.6));
            }
            let tx = pad + art_s + 12.0;
            let title_w = w - tx - pad - 230.0 * self.hover;
            if media.active {
                let t = f.fit(Text::new(&media.title, tx, pad + 1.0, 15.0, WHITE).weight(600), title_w.max(40.0));
                f.scene.text(t);
                let a = f.fit(Text::new(&media.artist, tx, pad + 21.0, 12.5, with_a(WHITE, 0.6)), title_w.max(40.0));
                f.scene.text(a);
            } else {
                f.scene.text(Text::new("Aurolo Lyrics", tx, pad + 1.0, 15.0, WHITE).weight(600));
                f.scene.text(Text::new("Waiting for music", tx, pad + 21.0, 12.5, with_a(WHITE, 0.6)));
            }
            lyric_area = [pad, pad + art_s + 8.0, w - pad * 2.0, h - (pad + art_s + 8.0) - 14.0];
        }

        // ---- lyrics (with a soft drop shadow when there's little or no card behind them)
        let layer0 = f.scene.layers.len() - 1;
        let text0 = f.scene.layers[layer0].texts.len();
        self.view.minimal = !header;
        self.view.draw(f, lyric_area, &ly, &media, &pal, &audio, pos, false);
        let shadow = match self.bg {
            WidgetBg::Solid => 0.0,
            WidgetBg::Glass => 0.35,
            WidgetBg::None => 0.6,
        };
        if shadow > 0.0 {
            for (li, layer) in f.scene.layers.iter_mut().enumerate().skip(layer0) {
                let start = if li == layer0 { text0 } else { 0 };
                for t in layer.texts.iter_mut().skip(start) {
                    t.shadow = shadow;
                }
            }
        }
        f.scene.layer();

        // ---- progress hairline
        if media.duration > 0.0 && (self.bg != WidgetBg::None || self.hover > 0.05) {
            let frac = (media.pos_now() / media.duration) as f32;
            let bw = w - r * 2.0;
            f.scene.shape(Shape::rect(r, h - 5.0, bw, 2.0, 1.0, with_a(WHITE, 0.1)));
            f.scene.glow_shape(Shape::rect(r, h - 5.0, bw * frac, 2.0, 1.0, pal.primary).grad(pal.secondary, 0.0));
        }

        // ---- hover controls (top-right)
        let mut buttons_rect = [0.0f32; 4];
        if self.hover > 0.02 {
            let a = self.hover;
            let col = with_a(WHITE, 0.92 * a);
            let (y, sz, step) = if !header { (20.0, 11.0, 26.0) } else { (pad + 20.0, 12.5, 30.0) };
            let n = 8.0;
            let x_end = w - 14.0 - sz;
            let x_start = x_end - step * (n - 1.0);
            buttons_rect = [x_start - sz * 1.4, y - sz * 1.4, x_end - x_start + sz * 2.8, sz * 2.8];
            // dark backing so buttons stay readable over lyrics
            f.scene.shape(Shape::rect(
                buttons_rect[0],
                buttons_rect[1],
                buttons_rect[2],
                buttons_rect[3],
                buttons_rect[3] / 2.0,
                [0.0, 0.0, 0.0, 0.45 * a],
            ));
            let mut x = x_end;
            if f.icon_button(input, ic::CLOSE, x, y, sz, col) {
                action = WidgetAction::Close;
            }
            x -= step;
            if f.icon_button(input, ic::FULLSCREEN, x, y, sz, col) {
                action = WidgetAction::Fullscreen;
            }
            x -= step;
            // layout toggle: chevron up = collapse to lyrics only, chevron down = show song info
            let g = if self.lyrics_only { '\u{E70D}' } else { '\u{E70E}' };
            if f.icon_button(input, g, x, y, sz, col) {
                action = WidgetAction::ToggleLayout;
            }
            x -= step;
            // background: glass -> solid -> none
            if f.icon_button(input, '\u{E81E}', x, y, sz, col) {
                self.bg = self.bg.next();
                action = WidgetAction::BgChanged(self.bg);
            }
            x -= step;
            if f.icon_button(input, ic::STYLE, x, y, sz, col) {
                self.view.style = self.view.style.next();
                action = WidgetAction::StyleChanged(self.view.style);
            }
            x -= step;
            if f.icon_button(input, ic::NEXT, x, y, sz, col) {
                shared.media_cmd(MediaCmd::Next);
            }
            x -= step;
            let pp = if media.playing { ic::PAUSE } else { ic::PLAY };
            if f.icon_button(input, pp, x, y, sz * 1.15, col) {
                shared.media_cmd(MediaCmd::PlayPause);
            }
            x -= step;
            if f.icon_button(input, ic::PREV, x, y, sz, col) {
                shared.media_cmd(MediaCmd::Prev);
            }

            // resize grip: three diagonal dots
            let ga = if on_grip || self.resizing.is_some() { 0.9 } else { 0.4 } * a;
            for k in 0..3 {
                let o = k as f32 * 4.5;
                f.scene.shape(Shape::glow(w - 8.0 - o, h - 8.0, 1.6, with_a(WHITE, ga), 0.3));
                if k > 0 {
                    f.scene.shape(Shape::glow(w - 8.0, h - 8.0 - o, 1.6, with_a(WHITE, ga), 0.3));
                }
            }
            f.scene.shape(Shape::glow(w - 12.5, h - 12.5, 1.6, with_a(WHITE, ga), 0.3));
        }

        // ---- drag to move from anywhere that isn't a control or the grip
        if matches!(action, WidgetAction::None)
            && self.resizing.is_none()
            && input.pressed
            && input.over(card)
            && !on_grip
            && !input.over(buttons_rect)
        {
            action = WidgetAction::Drag;
        }
        if input.key(crate::ui::Key::Esc) {
            action = WidgetAction::Close;
        }
        action
    }
}
