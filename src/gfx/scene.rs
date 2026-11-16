//! Retained-per-frame draw list. UI code fills a `Scene`, the `Painter` renders it.

pub type Rgba = [f32; 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Ui,
    Display,
    Serif,
    Mono,
    Icons,
    Lyric,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Shape {
    pub rect: [f32; 4],
    pub c0: Rgba,
    pub c1: Rgba,
    pub p: [f32; 4],
    pub q: [f32; 4],
    pub r: [f32; 4],
}

pub const K_RECT: f32 = 0.0;
pub const K_STROKE: f32 = 1.0;
pub const K_GLOW: f32 = 2.0;
pub const K_LINE: f32 = 3.0;
pub const K_ARC: f32 = 4.0;
pub const K_STAR: f32 = 5.0;
pub const K_EDGE: f32 = 6.0;

impl Shape {
    fn base(kind: f32, rect: [f32; 4], c: Rgba) -> Self {
        Shape { rect, c0: c, c1: c, p: [0.0, 0.0, 0.0, kind], q: [0.0; 4], r: [0.0; 4] }
    }
    pub fn rect(x: f32, y: f32, w: f32, h: f32, radius: f32, c: Rgba) -> Self {
        let mut s = Self::base(K_RECT, [x, y, w, h], c);
        s.p[0] = radius;
        s
    }
    pub fn stroke(x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, c: Rgba) -> Self {
        let mut s = Self::base(K_STROKE, [x, y, w, h], c);
        s.p[0] = radius;
        s.p[1] = width;
        s
    }
    /// Soft radial glow centred on (cx, cy).
    pub fn glow(cx: f32, cy: f32, radius: f32, c: Rgba, falloff: f32) -> Self {
        let mut s = Self::base(K_GLOW, [cx - radius, cy - radius, radius * 2.0, radius * 2.0], c);
        s.c1 = [c[0], c[1], c[2], 0.0];
        s.q[0] = falloff;
        s
    }
    pub fn line(x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) -> Self {
        let pad = width;
        let mn = [x0.min(x1) - pad, y0.min(y1) - pad];
        let mx = [x0.max(x1) + pad, y0.max(y1) + pad];
        let mut s = Self::base(K_LINE, [mn[0], mn[1], mx[0] - mn[0], mx[1] - mn[1]], c);
        s.p[1] = width;
        s.q = [x0, y0, x1, y1];
        s
    }
    /// Progress ring centred on (cx, cy). progress 0..1 clockwise from top.
    pub fn arc(cx: f32, cy: f32, radius: f32, thickness: f32, progress: f32, c: Rgba) -> Self {
        let mut s = Self::base(K_ARC, [cx - radius, cy - radius, radius * 2.0, radius * 2.0], c);
        s.p[1] = thickness;
        s.q[0] = progress.clamp(0.0, 1.0);
        s
    }
    pub fn star(cx: f32, cy: f32, radius: f32, c: Rgba, spikes: f32) -> Self {
        let mut s = Self::base(K_STAR, [cx - radius, cy - radius, radius * 2.0, radius * 2.0], c);
        s.q[0] = spikes;
        s
    }
    pub fn edge(w: f32, h: f32, corner: f32, width: f32, intensity: f32, spin: f32, c0: Rgba, c1: Rgba) -> Self {
        let mut s = Self::base(K_EDGE, [0.0, 0.0, w, h], c0);
        s.c1 = c1;
        s.p[0] = corner;
        s.p[1] = width;
        s.q = [intensity, spin, 0.0, 0.0];
        s
    }
    pub fn soft(mut self, s: f32) -> Self {
        self.p[2] = s;
        self
    }
    /// Linear gradient c0 -> c1 along `angle` radians (0 = left to right).
    pub fn grad(mut self, c1: Rgba, angle: f32) -> Self {
        self.c1 = c1;
        self.r[1] = 0.0;
        self.r[2] = angle;
        self
    }
    pub fn radial(mut self, c1: Rgba) -> Self {
        self.c1 = c1;
        self.r[1] = 1.0;
        self
    }
    /// Conic gradient that rotates at `speed` rad/s.
    pub fn conic(mut self, c1: Rgba, speed: f32) -> Self {
        self.c1 = c1;
        self.r[1] = 2.0;
        self.r[2] = speed;
        self
    }
    pub fn additive(mut self) -> Self {
        self.r[0] = 1.0;
        self
    }
    pub fn start_angle(mut self, a: f32) -> Self {
        self.q[1] = a;
        self
    }
}

#[derive(Clone, Debug)]
pub struct Text {
    pub s: String,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub color: Rgba,
    pub family: Family,
    pub weight: u16,
    pub italic: bool,
    pub align: Align,
    pub max_w: Option<f32>,
    /// clip rect x0,y0,x1,y1 in logical px
    pub clip: Option<[f32; 4]>,
    pub scale: f32,
    pub line_height: f32,
    /// Opacity of a soft dark drop shadow drawn under the text (0 = none).
    pub shadow: f32,
}

impl Text {
    pub fn new(s: impl Into<String>, x: f32, y: f32, size: f32, color: Rgba) -> Self {
        Text {
            s: s.into(),
            x,
            y,
            size,
            color,
            family: Family::Ui,
            weight: 400,
            italic: false,
            align: Align::Left,
            max_w: None,
            clip: None,
            scale: 1.0,
            line_height: 1.25,
            shadow: 0.0,
        }
    }
    pub fn weight(mut self, w: u16) -> Self {
        self.weight = w;
        self
    }
    pub fn family(mut self, f: Family) -> Self {
        self.family = f;
        self
    }
    pub fn italic(mut self, i: bool) -> Self {
        self.italic = i;
        self
    }
    pub fn center(mut self) -> Self {
        self.align = Align::Center;
        self
    }
    pub fn right(mut self) -> Self {
        self.align = Align::Right;
        self
    }
    pub fn max_w(mut self, w: f32) -> Self {
        self.max_w = Some(w);
        self
    }
    pub fn clip(mut self, c: [f32; 4]) -> Self {
        self.clip = Some(c);
        self
    }
    pub fn scale(mut self, s: f32) -> Self {
        self.scale = s;
        self
    }
    pub fn lh(mut self, l: f32) -> Self {
        self.line_height = l;
        self
    }
}

#[derive(Clone, Debug)]
pub struct ImageDraw {
    pub key: u64,
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub radius: f32,
    pub opacity: f32,
    pub saturation: f32,
    pub tint: Rgba,
}

impl ImageDraw {
    pub fn new(key: u64, x: f32, y: f32, w: f32, h: f32, radius: f32) -> Self {
        ImageDraw {
            key,
            rect: [x, y, w, h],
            uv: [0.0, 0.0, 1.0, 1.0],
            radius,
            opacity: 1.0,
            saturation: 1.0,
            tint: [0.0; 4],
        }
    }
    pub fn opacity(mut self, o: f32) -> Self {
        self.opacity = o;
        self
    }
    pub fn saturation(mut self, s: f32) -> Self {
        self.saturation = s;
        self
    }
    pub fn tint(mut self, t: Rgba) -> Self {
        self.tint = t;
        self
    }
}

#[derive(Default)]
pub struct Layer {
    pub shapes: Vec<Shape>,
    pub images: Vec<ImageDraw>,
    pub texts: Vec<Text>,
}

/// One frame of drawing. Items flagged `glow` are also blurred into the bloom buffer.
pub struct Scene {
    pub layers: Vec<Layer>,
    pub glow_shapes: Vec<Shape>,
    pub glow_texts: Vec<Text>,
    pub bloom: f32,
}

impl Default for Scene {
    fn default() -> Self {
        Scene { layers: vec![Layer::default()], glow_shapes: vec![], glow_texts: vec![], bloom: 1.0 }
    }
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }
    fn top(&mut self) -> &mut Layer {
        self.layers.last_mut().unwrap()
    }
    pub fn layer(&mut self) {
        self.layers.push(Layer::default());
    }
    pub fn shape(&mut self, s: Shape) {
        self.top().shapes.push(s);
    }
    pub fn text(&mut self, t: Text) {
        self.top().texts.push(t);
    }
    pub fn image(&mut self, i: ImageDraw) {
        self.top().images.push(i);
    }
    /// Drawn sharp AND bloomed.
    pub fn glow_shape(&mut self, s: Shape) {
        self.glow_shapes.push(s);
        self.top().shapes.push(s);
    }
    /// Bloom only (invisible halo).
    pub fn halo_shape(&mut self, s: Shape) {
        self.glow_shapes.push(s);
    }
    pub fn glow_text(&mut self, t: Text) {
        self.glow_texts.push(t.clone());
        self.top().texts.push(t);
    }
    pub fn halo_text(&mut self, t: Text) {
        self.glow_texts.push(t);
    }
}

pub fn rgba(r: u8, g: u8, b: u8, a: f32) -> Rgba {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a]
}

pub fn with_a(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], a]
}

pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

pub const WHITE: Rgba = [1.0, 1.0, 1.0, 1.0];
pub const BLACK: Rgba = [0.0, 0.0, 0.0, 1.0];
