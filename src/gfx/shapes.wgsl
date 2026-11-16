// SDF shape renderer. All coordinates are logical pixels; g.scale converts to physical.
struct Globals {
    size: vec2<f32>,   // logical viewport size
    scale: f32,
    time: f32,
};
@group(0) @binding(0) var<uniform> g: Globals;

struct Inst {
    @location(0) rect: vec4<f32>,  // x, y, w, h  (shape box)
    @location(1) c0: vec4<f32>,
    @location(2) c1: vec4<f32>,
    @location(3) p: vec4<f32>,     // radius, width, softness, kind
    @location(4) q: vec4<f32>,     // kind specific
    @location(5) r: vec4<f32>,     // additive, gradient mode, gradient angle, pad
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,      // logical px relative to box top-left
    @location(1) @interpolate(flat) rect: vec4<f32>,
    @location(2) @interpolate(flat) c0: vec4<f32>,
    @location(3) @interpolate(flat) c1: vec4<f32>,
    @location(4) @interpolate(flat) p: vec4<f32>,
    @location(5) @interpolate(flat) q: vec4<f32>,
    @location(6) @interpolate(flat) r: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Inst) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let c = corners[vi];
    let kind = inst.p.w;
    var pad = inst.p.z + 2.0;
    if (kind > 5.5 && kind < 6.5) { pad = 0.0; }
    let mn = inst.rect.xy - vec2(pad);
    let sz = inst.rect.zw + vec2(pad * 2.0);
    let world = mn + c * sz;
    var o: VOut;
    let ndc = world / g.size * 2.0 - 1.0;
    o.pos = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    o.local = world - inst.rect.xy;
    o.rect = inst.rect;
    o.c0 = inst.c0;
    o.c1 = inst.c1;
    o.p = inst.p;
    o.q = inst.q;
    o.r = inst.r;
    return o;
}

fn sd_rrect(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let rr = min(r, min(b.x, b.y));
    let q = abs(p) - b + vec2(rr);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.0001), 0.0, 1.0);
    return vec2(length(pa - ba * h), h);
}

const PI: f32 = 3.14159265;

fn coverage(d: f32, soft: f32) -> f32 {
    let e = max(soft, 1.0 / g.scale);
    return 1.0 - smoothstep(-e * 0.5, e * 0.5, d);
}

fn grad_t(local: vec2<f32>, size: vec2<f32>, mode: f32, angle: f32) -> f32 {
    let uv = local / max(size, vec2(1.0));
    if (mode < 0.5) {
        let dir = vec2(cos(angle), sin(angle));
        return clamp(dot(uv - 0.5, dir) + 0.5, 0.0, 1.0);
    } else if (mode < 1.5) {
        return clamp(length(uv - 0.5) * 2.0, 0.0, 1.0);
    }
    // conic, rotating with time * angle
    let d = local - size * 0.5;
    let a = atan2(d.y, d.x) + g.time * angle;
    return 0.5 + 0.5 * sin(a);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let kind = i.p.w;
    let size = i.rect.zw;
    let half = size * 0.5;
    let pc = i.local - half;
    var a = 0.0;
    var t = grad_t(i.local, size, i.r.y, i.r.z);

    if (kind < 0.5) {
        // filled rounded rect
        a = coverage(sd_rrect(pc, half, i.p.x), i.p.z);
    } else if (kind < 1.5) {
        // stroked rounded rect (inner stroke)
        let d = sd_rrect(pc, half, i.p.x);
        let w = i.p.y;
        a = coverage(abs(d + w * 0.5) - w * 0.5, i.p.z);
    } else if (kind < 2.5) {
        // radial glow / soft dot. q.x = falloff exponent
        let d = length(pc) / max(half.x, 0.001);
        a = pow(clamp(1.0 - d, 0.0, 1.0), max(i.q.x, 0.1));
        t = clamp(d, 0.0, 1.0);
    } else if (kind < 3.5) {
        // line segment from q.xy to q.zw (absolute coords)
        let p = i.local + i.rect.xy;
        let s = sd_segment(p, i.q.xy, i.q.zw);
        a = coverage(s.x - i.p.y * 0.5, i.p.z);
        t = s.y;
    } else if (kind < 4.5) {
        // progress arc ring: q.x progress 0..1, q.y start angle (radians, 0 = top)
        let rad = half.x - i.p.y * 0.5;
        let d = abs(length(pc) - rad) - i.p.y * 0.5;
        var ang = atan2(pc.x, -pc.y) - i.q.y;
        ang = ang - floor(ang / (2.0 * PI)) * 2.0 * PI;
        let frac = ang / (2.0 * PI);
        var m = 0.0;
        if (frac <= i.q.x) { m = 1.0; }
        // round caps
        let a0 = i.q.y;
        let a1 = i.q.y + i.q.x * 2.0 * PI;
        let e0 = vec2(sin(a0), -cos(a0)) * rad;
        let e1 = vec2(sin(a1), -cos(a1)) * rad;
        let cap = min(length(pc - e0), length(pc - e1)) - i.p.y * 0.5;
        var dd = d;
        if (m < 0.5) { dd = cap; }
        if (i.q.x <= 0.0) { dd = 1e5; }
        a = coverage(dd, i.p.z);
        t = frac;
    } else if (kind < 5.5) {
        // star sparkle: soft core + cross spikes. q.x = spike strength
        let d = length(pc) / max(half.x, 0.001);
        let core = exp(-d * d * 18.0);
        let halo = pow(clamp(1.0 - d, 0.0, 1.0), 3.0) * 0.35;
        let nx = abs(pc.x) / half.x;
        let ny = abs(pc.y) / half.y;
        let spike = (exp(-ny * 60.0) * pow(clamp(1.0 - nx, 0.0, 1.0), 2.0)
                   + exp(-nx * 60.0) * pow(clamp(1.0 - ny, 0.0, 1.0), 2.0)) * i.q.x;
        a = clamp(core + halo + spike, 0.0, 1.0);
        t = clamp(d, 0.0, 1.0);
    } else if (kind < 6.5) {
        // screen edge glow: p.x corner radius, p.y glow width, q.x intensity, q.y rotation speed
        let d = -sd_rrect(pc, half, i.p.x);
        let w = max(i.p.y, 1.0);
        a = exp(-d / w) * i.q.x;
        let ang = atan2(pc.y * (size.x / size.y), pc.x) + g.time * i.q.y;
        t = 0.5 + 0.5 * sin(ang * 2.0);
        a = clamp(a, 0.0, 1.0);
    }

    let col = mix(i.c0, i.c1, t);
    let alpha = col.a * a;
    return vec4(col.rgb * alpha, alpha * (1.0 - i.r.x));
}
