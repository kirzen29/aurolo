// Images, blur and bloom composite.
struct Globals {
    size: vec2<f32>,
    scale: f32,
    time: f32,
};

// ---------- textured rounded rect ----------
@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct ImgInst {
    @location(0) rect: vec4<f32>,
    @location(1) uv: vec4<f32>,      // u0 v0 u1 v1
    @location(2) p: vec4<f32>,       // radius, opacity, softness, saturation
    @location(3) tint: vec4<f32>,
};

struct IOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) rect: vec4<f32>,
    @location(3) @interpolate(flat) p: vec4<f32>,
    @location(4) @interpolate(flat) tint: vec4<f32>,
};

@vertex
fn vs_img(@builtin(vertex_index) vi: u32, inst: ImgInst) -> IOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let c = corners[vi];
    let world = inst.rect.xy + c * inst.rect.zw;
    var o: IOut;
    let ndc = world / g.size * 2.0 - 1.0;
    o.pos = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    o.local = c * inst.rect.zw;
    o.uv = mix(inst.uv.xy, inst.uv.zw, c);
    o.rect = inst.rect;
    o.p = inst.p;
    o.tint = inst.tint;
    return o;
}

fn sd_rrect(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let rr = min(r, min(b.x, b.y));
    let q = abs(p) - b + vec2(rr);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

@fragment
fn fs_img(i: IOut) -> @location(0) vec4<f32> {
    let half = i.rect.zw * 0.5;
    let d = sd_rrect(i.local - half, half, i.p.x);
    let e = max(i.p.z, 1.0 / g.scale);
    let cov = 1.0 - smoothstep(-e * 0.5, e * 0.5, d);
    var c = textureSample(tex, samp, i.uv);
    let lum = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    c = vec4(mix(vec3(lum), c.rgb, i.p.w), c.a);
    c = vec4(mix(c.rgb, i.tint.rgb, i.tint.a), c.a);
    let a = c.a * cov * i.p.y;
    return vec4(c.rgb * a, a);
}

// ---------- fullscreen passes ----------
struct BlurU {
    dir: vec2<f32>,   // texel step in source uv
    gain: f32,
    _pad: f32,
};
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var bsamp: sampler;
@group(0) @binding(2) var<uniform> bu: BlurU;

struct FOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> FOut {
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    var o: FOut;
    o.pos = vec4(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2(x, y);
    return o;
}

@fragment
fn fs_blur(i: FOut) -> @location(0) vec4<f32> {
    // 9-tap gaussian using linear sampling offsets
    let w0 = 0.2270270270;
    let w1 = 0.3162162162;
    let w2 = 0.0702702703;
    let o1 = 1.3846153846;
    let o2 = 3.2307692308;
    var c = textureSample(src, bsamp, i.uv) * w0;
    c += textureSample(src, bsamp, i.uv + bu.dir * o1) * w1;
    c += textureSample(src, bsamp, i.uv - bu.dir * o1) * w1;
    c += textureSample(src, bsamp, i.uv + bu.dir * o2) * w2;
    c += textureSample(src, bsamp, i.uv - bu.dir * o2) * w2;
    return c * bu.gain;
}
