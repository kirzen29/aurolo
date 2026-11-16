//! Lumn-style ambient rim light around the screen, coloured by album art and pulsing to the beat.

use crate::anim::*;
use crate::gfx::*;
use crate::state::*;
use crate::ui::Frame;

pub struct EdgeGlow {
    level: f32,
    vis: f32,
    c0: Rgba,
    c1: Rgba,
}

impl EdgeGlow {
    pub fn new() -> Self {
        EdgeGlow { level: 0.0, vis: 0.0, c0: Palette::default().primary, c1: Palette::default().secondary }
    }

    /// Returns false once fully faded out (window can be hidden).
    pub fn draw(&mut self, f: &mut Frame, shared: &Shared, thickness: f32, intensity: f32, only_playing: bool) -> bool {
        let (playing, pal) = {
            let m = shared.media.read();
            (m.playing && m.active, m.palette())
        };
        let a = shared.audio.lock().clone();
        let want = if playing || !only_playing { 1.0 } else { 0.0 };
        self.vis = approach(self.vis, want, 2.5, f.dt);
        self.level = approach(self.level, a.level, 10.0, f.dt);
        self.c0 = mix(self.c0, pal.primary, (f.dt * 2.0).min(1.0));
        self.c1 = mix(self.c1, pal.secondary, (f.dt * 2.0).min(1.0));
        if self.vis < 0.004 {
            return false;
        }
        let pulse = a.pulse();
        let width = thickness * (0.85 + self.level * 0.9 + pulse * 0.7);
        let inten = intensity * self.vis * (0.45 + self.level * 0.6 + pulse * 0.45);
        f.scene.bloom = 0.0;
        f.scene.shape(Shape::edge(f.w, f.h, 10.0, width, inten.min(1.4), 0.35, self.c0, self.c1));
        // a crisp hairline at the very edge reads as "light source"
        f.scene.shape(
            Shape::stroke(0.0, 0.0, f.w, f.h, 10.0, 1.6, with_a(mix(self.c0, WHITE, 0.4), 0.55 * self.vis))
                .conic(with_a(mix(self.c1, WHITE, 0.4), 0.55 * self.vis), 0.35),
        );
        true
    }
}
