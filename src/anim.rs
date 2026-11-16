//! Springs, easing and small math helpers for buttery UI motion.

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub stiffness: f32,
    pub damping: f32,
}

impl Spring {
    pub fn new(v: f32) -> Self {
        Spring { value: v, velocity: 0.0, target: v, stiffness: 260.0, damping: 26.0 }
    }
    pub fn with(v: f32, stiffness: f32, damping: f32) -> Self {
        Spring { value: v, velocity: 0.0, target: v, stiffness, damping }
    }
    pub fn set(&mut self, t: f32) {
        self.target = t;
    }
    pub fn snap(&mut self, v: f32) {
        self.value = v;
        self.target = v;
        self.velocity = 0.0;
    }
    pub fn step(&mut self, dt: f32) -> f32 {
        // semi-implicit euler with substeps for stability
        let steps = ((dt / 0.004).ceil() as i32).clamp(1, 16);
        let h = dt / steps as f32;
        for _ in 0..steps {
            let f = -self.stiffness * (self.value - self.target) - self.damping * self.velocity;
            self.velocity += f * h;
            self.value += self.velocity * h;
        }
        self.value
    }
    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < 0.001 && self.velocity.abs() < 0.001
    }
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn clamp01(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = clamp01(t);
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_out(t: f32) -> f32 {
    let t = clamp01(t);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

pub fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = clamp01((x - a) / (b - a));
    t * t * (3.0 - 2.0 * t)
}

/// Exponential approach, frame-rate independent.
pub fn approach(cur: f32, target: f32, rate: f32, dt: f32) -> f32 {
    cur + (target - cur) * (1.0 - (-rate * dt).exp())
}

/// Deterministic hash -> 0..1
pub fn hash01(mut x: u64) -> f32 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
