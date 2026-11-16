//! GPU renderer: SDF shapes, images, glyphon text and a two-level bloom, onto
//! per-pixel-transparent windows (DX12 + DirectComposition).

pub mod scene;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, ColorMode, Family as GFamily, FontSystem, Metrics,
    Resolution, Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport, Weight,
};
use wgpu::util::DeviceExt;
use winit::window::Window;

pub use scene::*;

pub static ICON_FONT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
pub static LYRIC_FONT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn has_family(fs: &FontSystem, name: &str) -> bool {
    fs.db().faces().any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
}

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub fonts: RefCell<FontSystem>,
    pub swash: RefCell<SwashCache>,
    pub glyph_cache: Cache,
}

impl Gpu {
    pub fn new(lyric_font: &str) -> anyhow_lite::Result<Self> {
        let mut backend_options = wgpu::BackendOptions::default();
        backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::DX12;
        desc.flags = wgpu::InstanceFlags::empty();
        desc.backend_options = backend_options;
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .map_err(|e| anyhow_lite::err(format!("no GPU adapter: {e}")))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aurolo"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(|e| anyhow_lite::err(format!("device: {e}")))?;
        let glyph_cache = Cache::new(&device);
        let mut fonts = FontSystem::new();
        fonts.db_mut().set_sans_serif_family("Segoe UI");
        let icon = if has_family(&fonts, "Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        let _ = ICON_FONT.set(icon.to_string());
        let want = lyric_font.trim();
        let lyric = if !want.is_empty() && has_family(&fonts, want) {
            want.to_string()
        } else if has_family(&fonts, "Segoe UI Variable Display") {
            "Segoe UI Variable Display".to_string()
        } else {
            "Segoe UI".to_string()
        };
        let _ = LYRIC_FONT.set(lyric);
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            fonts: RefCell::new(fonts),
            swash: RefCell::new(SwashCache::new()),
            glyph_cache,
        })
    }
}

/// Minimal error helper so we don't pull in anyhow.
pub mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, String>;
    pub fn err(s: String) -> String {
        s
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TextKey {
    s: String,
    size: u32,
    family: Family,
    weight: u16,
    italic: bool,
    max_w: Option<u32>,
    align: Align,
    lh: u32,
}

struct TextEntry {
    buf: Buffer,
    w: f32,
    h: f32,
    frame: u64,
}

struct ImgTex {
    bind: wgpu::BindGroup,
    frame: u64,
    _tex: wgpu::Texture,
}

struct Targets {
    glow: wgpu::TextureView,
    passes: Vec<(wgpu::BindGroup, wgpu::TextureView, [u32; 2])>, // (source bind, target view, target size)
    comp: Vec<wgpu::BindGroup>,
    _ubufs: Vec<wgpu::Buffer>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    size: [f32; 2],
    scale: f32,
    time: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImgInst {
    rect: [f32; 4],
    uv: [f32; 4],
    p: [f32; 4],
    tint: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurU {
    dir: [f32; 2],
    gain: f32,
    _pad: f32,
}

pub struct Painter {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pub scale: f32,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    shape_pipe: wgpu::RenderPipeline,
    img_pipe: wgpu::RenderPipeline,
    blur_pipe: wgpu::RenderPipeline,
    add_pipe: wgpu::RenderPipeline,
    img_bgl: wgpu::BindGroupLayout,
    blur_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    shape_buf: wgpu::Buffer,
    shape_cap: usize,
    img_buf: wgpu::Buffer,
    img_cap: usize,
    images: HashMap<u64, ImgTex>,
    targets: Option<Targets>,
    atlas: TextAtlas,
    viewport: Viewport,
    renderers: Vec<TextRenderer>,
    glow_renderer: TextRenderer,
    texts: HashMap<TextKey, TextEntry>,
    frame: u64,
    pub configured: bool,
}

const SHAPE_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4
];
const IMG_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4];

const PREMUL: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};
const ADD: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
};

impl Painter {
    pub fn new(gpu: &Gpu, window: Arc<Window>) -> Self {
        let device = &gpu.device;
        let surface = gpu.instance.create_surface(window.clone()).expect("surface");
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm]
            .into_iter()
            .find(|f| caps.formats.contains(f))
            .unwrap_or(caps.formats[0]);
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            log(&format!("warning: premultiplied alpha unsupported: {:?}", caps.alpha_modes));
            caps.alpha_modes[0]
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
            color_space: Default::default(),
        };

        let globals_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("globals"),
            contents: bytemuck::bytes_of(&Globals { size: [1.0, 1.0], scale: 1.0, time: 0.0 }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() }],
        });
        let tex_entry = |b: u32| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let samp_entry = |b: u32| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let img_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img"),
            entries: &[tex_entry(0), samp_entry(1)],
        });
        let blur_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur"),
            entries: &[
                tex_entry(0),
                samp_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let shapes_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shapes"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shapes.wgsl").into()),
        });
        let post_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("post"),
            source: wgpu::ShaderSource::Wgsl(include_str!("post.wgsl").into()),
        });

        let shape_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shape"),
            bind_group_layouts: &[Some(&globals_bgl)],
            immediate_size: 0,
        });
        let img_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("img"),
            bind_group_layouts: &[Some(&globals_bgl), Some(&img_bgl)],
            immediate_size: 0,
        });
        let blur_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blur"),
            bind_group_layouts: &[Some(&blur_bgl)],
            immediate_size: 0,
        });

        let make = |label: &str,
                    layout: &wgpu::PipelineLayout,
                    module: &wgpu::ShaderModule,
                    vs: &str,
                    fs: &str,
                    buffers: &[wgpu::VertexBufferLayout],
                    blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some(vs),
                    buffers: &buffers.iter().cloned().map(Some).collect::<Vec<_>>(),
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let shape_pipe = make(
            "shapes",
            &shape_layout,
            &shapes_mod,
            "vs",
            "fs",
            &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Shape>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &SHAPE_ATTRS,
            }],
            Some(PREMUL),
        );
        let img_pipe = make(
            "img",
            &img_layout,
            &post_mod,
            "vs_img",
            "fs_img",
            &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<ImgInst>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &IMG_ATTRS,
            }],
            Some(PREMUL),
        );
        let blur_pipe = make("blur", &blur_layout, &post_mod, "vs_full", "fs_blur", &[], None);
        let add_pipe = make("add", &blur_layout, &post_mod, "vs_full", "fs_blur", &[], Some(ADD));

        let shape_cap = 1024;
        let shape_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shapes"),
            size: (shape_cap * std::mem::size_of::<Shape>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let img_cap = 32;
        let img_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("imgs"),
            size: (img_cap * std::mem::size_of::<ImgInst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut atlas = TextAtlas::with_color_mode(device, &gpu.queue, &gpu.glyph_cache, format, ColorMode::Web);
        let viewport = Viewport::new(device, &gpu.glyph_cache);
        let glow_renderer = TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);

        let scale = window.scale_factor() as f32;
        let mut p = Painter {
            window,
            surface,
            config,
            scale,
            globals_buf,
            globals_bg,
            shape_pipe,
            img_pipe,
            blur_pipe,
            add_pipe,
            img_bgl,
            blur_bgl,
            sampler,
            shape_buf,
            shape_cap,
            img_buf,
            img_cap,
            images: HashMap::new(),
            targets: None,
            atlas,
            viewport,
            renderers: vec![],
            glow_renderer,
            texts: HashMap::new(),
            frame: 0,
            configured: false,
        };
        p.resize(gpu);
        p
    }

    pub fn resize(&mut self, gpu: &Gpu) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            self.configured = false;
            return;
        }
        self.scale = self.window.scale_factor() as f32;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&gpu.device, &self.config);
        self.targets = None;
        self.configured = true;
    }

    /// Logical size of the window.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.config.width as f32 / self.scale, self.config.height as f32 / self.scale)
    }

    fn make_targets(&self, device: &wgpu::Device) -> Targets {
        let (w, h) = (self.config.width.max(1), self.config.height.max(1));
        let mk = |w: u32, h: u32| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("rt"),
                    size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.config.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let glow = mk(w, h);
        let (w2, h2) = (w / 2, h / 2);
        let (w4, h4) = (w / 6, h / 6);
        let h1 = mk(w2, h2);
        let v1 = mk(w2, h2);
        let h2t = mk(w4, h4);
        let v2 = mk(w4, h4);
        let mut ubufs = vec![];
        let mut bg = |src: &wgpu::TextureView, dir: [f32; 2], gain: f32| {
            let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur-u"),
                contents: bytemuck::bytes_of(&BlurU { dir, gain, _pad: 0.0 }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let g = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blur"),
                layout: &self.blur_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: ub.as_entire_binding() },
                ],
            });
            ubufs.push(ub);
            g
        };
        let s2 = [1.0 / w2.max(1) as f32, 1.0 / h2.max(1) as f32];
        let s4 = [1.0 / w4.max(1) as f32, 1.0 / h4.max(1) as f32];
        let passes = vec![
            (bg(&glow, [s2[0], 0.0], 1.0), h1.clone(), [w2, h2]),
            (bg(&h1, [0.0, s2[1]], 1.0), v1.clone(), [w2, h2]),
            (bg(&v1, [s4[0], 0.0], 1.0), h2t.clone(), [w4, h4]),
            (bg(&h2t, [0.0, s4[1]], 1.0), v2.clone(), [w4, h4]),
        ];
        let comp = vec![bg(&v1, [0.0, 0.0], 1.0), bg(&v2, [0.0, 0.0], 1.0)];
        Targets { glow, passes, comp, _ubufs: ubufs }
    }

    pub fn has_image(&self, key: u64) -> bool {
        self.images.contains_key(&key)
    }

    pub fn upload_image(&mut self, gpu: &Gpu, key: u64, w: u32, h: u32, rgba: &[u8]) {
        let tex = gpu.device.create_texture_with_data(
            &gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("image"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            rgba,
        );
        let view = tex.create_view(&Default::default());
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.img_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.images.insert(key, ImgTex { bind, frame: self.frame, _tex: tex });
    }

    fn key_of(t: &Text) -> TextKey {
        TextKey {
            s: t.s.clone(),
            size: t.size.to_bits(),
            family: t.family,
            weight: t.weight,
            italic: t.italic,
            max_w: t.max_w.map(|w| w.to_bits()),
            align: t.align,
            lh: t.line_height.to_bits(),
        }
    }

    fn ensure_text(&mut self, gpu: &Gpu, t: &Text) -> TextKey {
        let key = Self::key_of(t);
        let frame = self.frame;
        if let Some(e) = self.texts.get_mut(&key) {
            e.frame = frame;
            return key;
        }
        let mut fs = gpu.fonts.borrow_mut();
        let mut buf = Buffer::new(&mut fs, Metrics::new(t.size, t.size * t.line_height));
        buf.set_size(t.max_w, None);
        let fam = match t.family {
            Family::Ui => GFamily::Name("Segoe UI Variable Text"),
            Family::Display => GFamily::Name("Segoe UI Variable Display"),
            Family::Serif => GFamily::Name("Georgia"),
            Family::Mono => GFamily::Name("Cascadia Mono"),
            Family::Icons => GFamily::Name(ICON_FONT.get().map(|s| s.as_str()).unwrap_or("Segoe MDL2 Assets")),
            Family::Lyric => GFamily::Name(LYRIC_FONT.get().map(|s| s.as_str()).unwrap_or("Segoe UI Variable Display")),
        };
        let attrs = Attrs::new()
            .family(fam)
            .weight(Weight(t.weight))
            .style(if t.italic { Style::Italic } else { Style::Normal });
        let align = match t.align {
            Align::Left => glyphon::cosmic_text::Align::Left,
            Align::Center => glyphon::cosmic_text::Align::Center,
            Align::Right => glyphon::cosmic_text::Align::Right,
        };
        buf.set_text(&t.s, &attrs, Shaping::Advanced, t.max_w.map(|_| align));
        buf.shape_until_scroll(&mut fs, false);
        let mut w: f32 = 0.0;
        let mut lines = 0;
        for run in buf.layout_runs() {
            w = w.max(run.line_w);
            lines += 1;
        }
        let h = lines.max(1) as f32 * t.size * t.line_height;
        self.texts.insert(key.clone(), TextEntry { buf, w, h, frame });
        key
    }

    /// Measure text in logical px (unscaled by `Text::scale`).
    pub fn measure(&mut self, gpu: &Gpu, t: &Text) -> (f32, f32) {
        let k = self.ensure_text(gpu, t);
        let e = &self.texts[&k];
        (e.w, e.h)
    }

    fn text_areas<'a>(texts: &'a HashMap<TextKey, TextEntry>, list: &[Text], scale: f32) -> Vec<TextArea<'a>> {
        let mut out = Vec::with_capacity(list.len());
        for t in list {
            if t.shadow > 0.0 {
                let a = t.shadow * t.color[3];
                for (dx, dy, k) in [(0.0, 1.5, 1.0), (1.0, 2.0, 0.6), (-1.0, 2.0, 0.6)] {
                    let mut s = t.clone();
                    s.x += dx;
                    s.y += dy;
                    s.color = [0.0, 0.0, 0.0, a * k];
                    out.push(Self::text_area(texts, &s, scale));
                }
            }
            out.push(Self::text_area(texts, t, scale));
        }
        out
    }

    fn text_area<'a>(texts: &'a HashMap<TextKey, TextEntry>, t: &Text, scale: f32) -> TextArea<'a> {
        let e = &texts[&Self::key_of(t)];
        let s = t.scale;
        let width = t.max_w.unwrap_or(e.w);
        let left = match t.align {
            Align::Left => t.x,
            Align::Center => t.x - width * s * 0.5,
            Align::Right => t.x - width * s,
        };
        let c = t.color;
        let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let bounds = match t.clip {
            Some([x0, y0, x1, y1]) => TextBounds {
                left: (x0 * scale) as i32,
                top: (y0 * scale) as i32,
                right: (x1 * scale).ceil() as i32,
                bottom: (y1 * scale).ceil() as i32,
            },
            None => TextBounds::default(),
        };
        TextArea {
            buffer: &e.buf,
            left: left * scale,
            top: t.y * scale,
            scale: scale * s,
            bounds,
            default_color: GColor::rgba(to8(c[0]), to8(c[1]), to8(c[2]), to8(c[3])),
            custom_glyphs: &[],
        }
    }

    pub fn render(&mut self, gpu: &Gpu, scene: &Scene, time: f32) {
        if !self.configured {
            return;
        }
        self.frame += 1;
        let device = &gpu.device;
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.resize(gpu);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let (lw, lh) = self.logical_size();
        let scale = self.scale;
        gpu.queue.write_buffer(
            &self.globals_buf,
            0,
            bytemuck::bytes_of(&Globals { size: [lw, lh], scale, time }),
        );

        // ---- shapes & images into instance buffers
        let mut shapes: Vec<Shape> = Vec::new();
        let mut shape_ranges = vec![];
        let mut imgs: Vec<ImgInst> = Vec::new();
        let mut img_ranges = vec![];
        for layer in &scene.layers {
            let s0 = shapes.len();
            shapes.extend_from_slice(&layer.shapes);
            shape_ranges.push(s0 as u32..shapes.len() as u32);
            let i0 = imgs.len();
            for im in &layer.images {
                if self.images.contains_key(&im.key) {
                    imgs.push(ImgInst {
                        rect: im.rect,
                        uv: im.uv,
                        p: [im.radius, im.opacity, 0.0, im.saturation],
                        tint: im.tint,
                    });
                }
            }
            img_ranges.push(i0 as u32..imgs.len() as u32);
        }
        let g0 = shapes.len() as u32;
        shapes.extend_from_slice(&scene.glow_shapes);
        let glow_range = g0..shapes.len() as u32;
        if shapes.len() > self.shape_cap {
            self.shape_cap = shapes.len().next_power_of_two();
            self.shape_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("shapes"),
                size: (self.shape_cap * std::mem::size_of::<Shape>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !shapes.is_empty() {
            gpu.queue.write_buffer(&self.shape_buf, 0, bytemuck::cast_slice(&shapes));
        }
        if imgs.len() > self.img_cap {
            self.img_cap = imgs.len().next_power_of_two();
            self.img_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("imgs"),
                size: (self.img_cap * std::mem::size_of::<ImgInst>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !imgs.is_empty() {
            gpu.queue.write_buffer(&self.img_buf, 0, bytemuck::cast_slice(&imgs));
        }
        let img_keys: Vec<Vec<u64>> = scene
            .layers
            .iter()
            .map(|l| l.images.iter().filter(|i| self.images.contains_key(&i.key)).map(|i| i.key).collect())
            .collect();

        // ---- text
        for layer in &scene.layers {
            for t in &layer.texts {
                self.ensure_text(gpu, t);
            }
        }
        for t in &scene.glow_texts {
            self.ensure_text(gpu, t);
        }
        while self.renderers.len() < scene.layers.len() {
            self.renderers.push(TextRenderer::new(&mut self.atlas, device, wgpu::MultisampleState::default(), None));
        }
        self.viewport.update(&gpu.queue, Resolution { width: self.config.width, height: self.config.height });
        {
            let mut fs = gpu.fonts.borrow_mut();
            let mut sw = gpu.swash.borrow_mut();
            for (i, layer) in scene.layers.iter().enumerate() {
                let areas = Self::text_areas(&self.texts, &layer.texts, scale);
                let _ = self.renderers[i].prepare(device, &gpu.queue, &mut fs, &mut self.atlas, &self.viewport, areas, &mut sw);
            }
            let areas: Vec<TextArea> = scene.glow_texts.iter().map(|t| Self::text_area(&self.texts, t, scale)).collect();
            let _ = self.glow_renderer.prepare(device, &gpu.queue, &mut fs, &mut self.atlas, &self.viewport, areas, &mut sw);
        }

        let use_bloom = scene.bloom > 0.0 && (!glow_range.is_empty() || !scene.glow_texts.is_empty());
        if use_bloom && self.targets.is_none() {
            self.targets = Some(self.make_targets(device));
        }

        let mut enc = device.create_command_encoder(&Default::default());

        if use_bloom {
            let t = self.targets.as_ref().unwrap();
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("glow"),
                    color_attachments: &[att(&t.glow)],
                    ..Default::default()
                });
                pass.set_bind_group(0, &self.globals_bg, &[]);
                if !glow_range.is_empty() {
                    pass.set_pipeline(&self.shape_pipe);
                    pass.set_vertex_buffer(0, self.shape_buf.slice(..));
                    pass.draw(0..6, glow_range.clone());
                }
                let _ = self.glow_renderer.render(&self.atlas, &self.viewport, &mut pass);
            }
            for (bg, target, _) in &t.passes {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("blur"),
                    color_attachments: &[att(target)],
                    ..Default::default()
                });
                pass.set_pipeline(&self.blur_pipe);
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..3, 0..1);
            }
        }

        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main"),
                color_attachments: &[att(&view)],
                ..Default::default()
            });
            for (i, _layer) in scene.layers.iter().enumerate() {
                pass.set_bind_group(0, &self.globals_bg, &[]);
                let r = shape_ranges[i].clone();
                if !r.is_empty() {
                    pass.set_pipeline(&self.shape_pipe);
                    pass.set_vertex_buffer(0, self.shape_buf.slice(..));
                    pass.draw(0..6, r);
                }
                let r = img_ranges[i].clone();
                if !r.is_empty() {
                    pass.set_pipeline(&self.img_pipe);
                    pass.set_vertex_buffer(0, self.img_buf.slice(..));
                    for (j, idx) in r.enumerate() {
                        let key = img_keys[i][j];
                        if let Some(img) = self.images.get(&key) {
                            pass.set_bind_group(1, &img.bind, &[]);
                            pass.draw(0..6, idx..idx + 1);
                        }
                    }
                }
                let _ = self.renderers[i].render(&self.atlas, &self.viewport, &mut pass);
            }
            if use_bloom {
                let t = self.targets.as_ref().unwrap();
                pass.set_pipeline(&self.add_pipe);
                for bg in &t.comp {
                    pass.set_bind_group(0, bg, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }
        gpu.queue.submit([enc.finish()]);
        self.window.pre_present_notify();
        gpu.queue.present(frame);

        // housekeeping
        self.atlas.trim();
        if self.frame % 120 == 0 {
            let f = self.frame;
            self.texts.retain(|_, e| f - e.frame < 240);
        }
    }

    pub fn touch_image(&mut self, key: u64) {
        let f = self.frame;
        if let Some(i) = self.images.get_mut(&key) {
            i.frame = f;
        }
    }

    pub fn drop_image(&mut self, key: u64) {
        self.images.remove(&key);
    }
}

fn att(v: &wgpu::TextureView) -> Option<wgpu::RenderPassColorAttachment<'_>> {
    Some(wgpu::RenderPassColorAttachment {
        view: v,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
            store: wgpu::StoreOp::Store,
        },
    })
}

pub fn log(s: &str) {
    use std::io::Write;
    let path = crate::config::data_dir().join("aurolo.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{s}");
    }
    eprintln!("{s}");
}
