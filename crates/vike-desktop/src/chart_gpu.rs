//! GPU candle layer — the wgpu renderer behind vike-chart's `GpuCandleItem` seam
//! (GPU Candle Layer Phase 2, Task 2). Ports NOTHING from the Python app; this is a
//! greenfield egui_wgpu integration.
//!
//! Data flow: vike-chart's `GpuCandleItem::shapes` (running inside `Plot::show`) maps
//! every visible bar through the current-frame `PlotTransform` into a screen-space
//! [`vike_chart::render::CandleInstance`] (egui POINTS) and hands the `Vec` + the plot
//! `Rect` to a vike-app closure (wired in Task 3). That closure wraps them in a
//! [`CandleCallback`] via `egui_wgpu::Callback::new_paint_callback(rect, cb)`, producing
//! an `egui::Shape::Callback`. egui then drives this module's [`CallbackTrait`]:
//! `prepare` uploads the instances + a plot-rect uniform; `paint` issues one instanced
//! draw of candle bodies + wicks straight into egui's own render pass.
//!
//! Pipeline lifetime: [`CandlePipeline`] is built ONCE in `App::new` and stored in
//! `render_state.renderer.write().callback_resources` (a `TypeMap`); `prepare`/`paint`
//! fetch it back out by type. Per-frame state (the instance vec + rect) rides on the
//! `CandleCallback` value itself, rebuilt every frame.
//!
//! ## Coordinate mapping (px → clip)
//! egui sets the render-pass VIEWPORT to the callback's `rect` — the plot frame, in
//! physical px (see egui-wgpu `renderer.rs`: `Primitive::Callback` → `set_viewport`).
//! So clip space `[-1, 1]` spans exactly the plot rect, and the candle instance points
//! (absolute egui points) are mapped RELATIVE to that rect:
//! `clip.x = (pt.x - rect.min.x) / rect.w * 2 - 1`, `clip.y = 1 - (pt.y - rect.min.y) / rect.h * 2`
//! (y flipped: egui screen-y grows downward). `pixels_per_point` cancels out (the viewport
//! is `rect * ppp` uniformly in both axes), so the uniform carries only the rect in points.
//! Bonus: the viewport clips candle geometry to the plot rect for free — bodies/wicks never
//! bleed into the axis gutter or neighbouring panes, matching egui_plot's own clipping.
//!
//! ## No bytemuck / no unsafe
//! The workspace enforces `unsafe_code = "forbid"` (root manifest); bytemuck's `Pod`/
//! `Zeroable` derives expand to a bare `unsafe impl`, which `forbid` rejects (and `allow`
//! cannot lift). So instead of a `bytemuck::Pod` cast, the GPU byte buffers are built with
//! safe `f32::to_ne_bytes`/`u32::to_ne_bytes` in the exact `#[repr(C)]` field order the
//! WGSL vertex layout expects — verified by the `gpu_candle_layout` unit test.

// Task 2 delivered this module as the GPU render surface; Task 3 wires it into vike-chart's
// `GpuCandleItem` build closure (`main.rs`'s `gpu_build`) + the global render toggle
// (`App::gpu_render`, ANDed with `App::gpu_ok`). `CandlePipeline::new` is registered in
// `App::new`; `CandleCallback` is constructed per-frame by `gpu_build` when the toggle is on.

use eframe::egui;
use eframe::egui_wgpu;
use eframe::wgpu;

use vike_chart::render::CandleInstance;

/// Vertices emitted per candle instance by the vertex shader: 6 (body fill) + 6 (wick) +
/// 4×6 (body border edges: top/bottom/left/right) = 36. See `CANDLE_WGSL`.
const VERTS_PER_INSTANCE: u32 = 36;

/// The GPU instance mirror of [`CandleInstance`], in the `#[repr(C)]` byte layout the WGSL
/// `Inst` vertex input expects. Grouped so every attribute sits at a naturally-aligned,
/// validation-safe offset with ZERO padding (16 + 16 + 8 + 4 = 44 bytes, all fields 4-byte
/// aligned) — see `INSTANCE_ATTRS` and the `gpu_candle_layout` test.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct GpuCandle {
    /// body rect in screen POINTS: `[x_lo, x_hi, body_top (min y), body_bot (max y)]` — WGSL `@location(0)`, offset 0.
    body: [f32; 4],
    /// rgba, already 0..1 (Task 1 divided by 255) — WGSL `@location(3)`, offset 16.
    color: [f32; 4],
    /// wick span in screen POINTS: `[wick_top (min y), wick_bot (max y)]` — WGSL `@location(1)`, offset 32.
    wick: [f32; 2],
    /// `1` = solid fill, `0` = hollow (border only) — WGSL `@location(2)`, offset 40.
    filled: u32,
}

/// Size of one [`GpuCandle`] in the vertex buffer (== `array_stride`). 44 bytes, no padding.
const GPU_CANDLE_SIZE: usize = std::mem::size_of::<GpuCandle>();

/// Per-instance vertex attributes, indexed by shader location. Offsets MUST equal the
/// [`GpuCandle`] field offsets (asserted in `gpu_candle_layout`). All offsets are multiples
/// of 8 (or 4 for the `Uint32`), so they satisfy wgpu's vertex-attribute alignment rules on
/// every backend.
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 4] = [
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 }, // body
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 32, shader_location: 1 }, // wick
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 40, shader_location: 2 }, // filled
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 3 }, // color
];

impl From<&CandleInstance> for GpuCandle {
    fn from(c: &CandleInstance) -> Self {
        GpuCandle {
            body: [c.x_lo, c.x_hi, c.body_top, c.body_bot],
            color: c.color,
            wick: [c.wick_top, c.wick_bot],
            filled: c.filled,
        }
    }
}

impl GpuCandle {
    /// Append this instance's 44 bytes to `buf` in `#[repr(C)]` field order (native-endian,
    /// which is what wgpu buffers expect). Safe stand-in for a `bytemuck` cast.
    fn write_ne(&self, buf: &mut Vec<u8>) {
        for v in self.body {
            buf.extend_from_slice(&v.to_ne_bytes());
        }
        for v in self.color {
            buf.extend_from_slice(&v.to_ne_bytes());
        }
        for v in self.wick {
            buf.extend_from_slice(&v.to_ne_bytes());
        }
        buf.extend_from_slice(&self.filled.to_ne_bytes());
    }
}

/// The screen-mapping uniform: the plot frame rect in egui POINTS `[min_x, min_y, w, h]`
/// (see the module-level "Coordinate mapping" note). 16 bytes — the minimum uniform size,
/// and 16-byte aligned as WGSL requires for a `vec4<f32>`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct ScreenUniform {
    rect: [f32; 4],
}

impl ScreenUniform {
    fn to_ne_bytes(self) -> [u8; 16] {
        let mut b = [0u8; 16];
        for (i, v) in self.rect.iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
        }
        b
    }
}

/// The candle render pipeline + its GPU buffers. Built once (`App::new`) and stored in
/// egui-wgpu's `callback_resources`.
pub struct CandlePipeline {
    pipeline: wgpu::RenderPipeline,
    /// Per-instance [`GpuCandle`] vertex buffer (`step_mode = Instance`). Grown on demand.
    instances: wgpu::Buffer,
    /// Capacity of `instances`, in whole [`GpuCandle`]s.
    cap: usize,
    /// The 16-byte [`ScreenUniform`]; rewritten each frame, never reallocated (so `bind_group`
    /// stays valid).
    screen: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// Initial instance-buffer capacity (candles). A comfortable visible-window count; grows if
/// a frame exceeds it.
const INITIAL_CAP: usize = 4096;

impl CandlePipeline {
    /// Build the pipeline against egui's target format. Callers guard this (`App::new` wraps it
    /// in `catch_unwind`) so a driver/shader failure degrades to the egui fallback rather than
    /// aborting startup.
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vike_candle_shader"),
            source: wgpu::ShaderSource::Wgsl(CANDLE_WGSL.into()),
        });

        let screen = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vike_candle_screen_uniform"),
            size: std::mem::size_of::<ScreenUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vike_candle_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(
                        std::mem::size_of::<ScreenUniform>() as u64,
                    ),
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vike_candle_bind_group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &screen,
                    offset: 0,
                    size: None,
                }),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vike_candle_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        // Mirror egui-wgpu's own sRGB-vs-UNorm branch (egui-wgpu-0.35.0 renderer.rs:
        // `if output_color_format.is_srgb() { "fs_main_linear_framebuffer" } else {
        // "fs_main_gamma_framebuffer" }`) so candles match the egui-painted elements on BOTH
        // surface types. The common non-sRGB case keeps using `fs_main` — unchanged.
        let fs_entry = if target.is_srgb() { "fs_main_srgb" } else { "fs_main" };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vike_candle_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                // wgpu 30 made each vertex-buffer slot OPTIONAL (`&[Option<VertexBufferLayout>]`)
                // so a pipeline can leave a slot unbound; 29 took the layout directly. One slot,
                // always bound, so this is a wrap and not a behaviour change.
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: GPU_CANDLE_SIZE as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &INSTANCE_ATTRS,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::default(),
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::default(),
                conservative: false,
            },
            // egui's main render pass carries no depth attachment (eframe defaults
            // depth_buffer/stencil_buffer = 0), so a depth-less pipeline is compatible.
            depth_stencil: None,
            // eframe's default `multisampling = 0` ⇒ egui's render pass is single-sampled
            // (`msaa_samples.max(1)` = 1). This MUST match or `paint` fails validation.
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(fs_entry),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target,
                    // Premultiplied-alpha "over", matching egui's own color blend. The fragment
                    // shader emits premultiplied rgb; opaque candles (a = 1) pass through.
                    blend: Some(wgpu::BlendState {
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
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vike_candle_instances"),
            size: (INITIAL_CAP * GPU_CANDLE_SIZE) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { pipeline, instances, cap: INITIAL_CAP, screen, bind_group }
    }
}

/// One frame's candle draw, produced by vike-app's `GpuCandleItem` build closure (Task 3) and
/// wrapped via `egui_wgpu::Callback::new_paint_callback(rect, cb)`. `rect` is the plot frame in
/// egui points — the SAME value egui uses for the callback viewport — carried here so `prepare`
/// can write the mapping uniform (which only `paint` would otherwise see, via `info.viewport`).
pub struct CandleCallback {
    pub instances: Vec<CandleInstance>,
    pub rect: egui::Rect,
}

impl egui_wgpu::CallbackTrait for CandleCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Absent pipeline (GPU build degraded) ⇒ no-op; Task 3 also gates the toggle on `gpu_ok`.
        let Some(pipe) = resources.get_mut::<CandlePipeline>() else {
            return Vec::new();
        };

        // Screen-mapping uniform: the plot rect in points (see module "Coordinate mapping").
        let uni = ScreenUniform {
            rect: [self.rect.min.x, self.rect.min.y, self.rect.width(), self.rect.height()],
        };
        queue.write_buffer(&pipe.screen, 0, &uni.to_ne_bytes());

        if self.instances.is_empty() {
            return Vec::new();
        }

        // Grow the instance buffer if this frame overflows capacity (reallocate; the bind group
        // does not reference it, so nothing dangles).
        if self.instances.len() > pipe.cap {
            let new_cap = self.instances.len().next_power_of_two();
            pipe.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vike_candle_instances"),
                size: (new_cap * GPU_CANDLE_SIZE) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            pipe.cap = new_cap;
        }

        let mut bytes = Vec::with_capacity(self.instances.len() * GPU_CANDLE_SIZE);
        for ci in &self.instances {
            GpuCandle::from(ci).write_ne(&mut bytes);
        }
        queue.write_buffer(&pipe.instances, 0, &bytes);

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let n = self.instances.len() as u32;
        if n == 0 {
            return;
        }
        let Some(pipe) = resources.get::<CandlePipeline>() else {
            return;
        };
        render_pass.set_pipeline(&pipe.pipeline);
        render_pass.set_bind_group(0, &pipe.bind_group, &[]);
        // Bind the whole buffer; only instances 0..n are drawn (and were written this frame).
        render_pass.set_vertex_buffer(0, pipe.instances.slice(..));
        render_pass.draw(0..VERTS_PER_INSTANCE, 0..n);
    }
}

/// The candle shader. One instanced draw: per instance the vertex shader emits a body (filled
/// quad when solid, else collapsed), a 1pt wick, and a 1pt body border (4 edge quads) — the
/// border is drawn for BOTH solid and hollow candles, exactly mirroring egui's candle, which
/// always paints a 1px stroke and fills conditionally. The border is CENTERED on the body
/// outline (not inset), matching egui_plot's `Polygon` stroke (epaint tessellates rect strokes
/// as centered + miter-joined — see the vendored `egui_plot`/`epaint` `polygon.rs`/
/// `tessellator.rs`). `vertex_index` (0..36) selects the piece.
///
/// Two fragment entry points, chosen in `CandlePipeline::new` by `target.is_srgb()` — the exact
/// branch egui-wgpu's own `renderer.rs` uses to pick between `fs_main_gamma_framebuffer` and
/// `fs_main_linear_framebuffer`: `fs_main` (gamma-out; the common non-sRGB UNorm target, where
/// the stored bytes ARE the gamma values — unchanged by this fix) and `fs_main_srgb` (linear-out;
/// an sRGB target has the hardware apply linear->gamma on write, so the shader must hand it
/// linear values instead).
const CANDLE_WGSL: &str = r#"
// Plot-rect mapping uniform: [min_x, min_y, width, height] in egui POINTS. egui set the
// render-pass viewport to this same rect (in px), so clip [-1,1] spans it; ppp cancels out.
struct Screen {
    rect: vec4<f32>,
};
@group(0) @binding(0) var<uniform> screen: Screen;

struct Inst {
    @location(0) body: vec4<f32>,   // x_lo, x_hi, body_top(min y), body_bot(max y)
    @location(1) wick: vec2<f32>,   // wick_top(min y), wick_bot(max y)
    @location(2) filled: u32,       // 1 = solid, 0 = hollow (border only)
    @location(3) color: vec4<f32>,  // rgba, already 0..1
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

// egui points -> clip, relative to the plot-rect viewport (y grows downward on screen).
fn to_clip(pt: vec2<f32>) -> vec4<f32> {
    let x = (pt.x - screen.rect.x) / screen.rect.z * 2.0 - 1.0;
    let y = 1.0 - (pt.y - screen.rect.y) / screen.rect.w * 2.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

// One axis-aligned quad (two triangles), corner c in 0..6. cull_mode = none ⇒ winding is free.
fn quad(x0: f32, y0: f32, x1: f32, y1: f32, c: u32) -> vec2<f32> {
    var xs = array<f32, 6>(x0, x1, x1, x0, x1, x0);
    var ys = array<f32, 6>(y0, y0, y1, y0, y1, y1);
    return vec2<f32>(xs[c], ys[c]);
}

const STROKE: f32 = 1.0;   // border/wick thickness in POINTS (egui Stroke/Line width 1.0)
// Border half-width: the body border straddles the outline (centered), like egui_plot's
// `Polygon` stroke — NOT inset. See the vertex-shader border branches below. Derived from
// STROKE (not a bare 0.5) so the two stay locked together and STROKE is a live reference.
const HALF_STROKE: f32 = STROKE * 0.5;
const WICK_HW: f32 = 0.5;  // wick half-width in POINTS (1pt total)

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Inst) -> VsOut {
    let x_lo = inst.body.x;
    let x_hi = inst.body.y;
    let b_top = inst.body.z;   // smaller y (higher price)
    let b_bot = inst.body.w;   // larger y (lower price)
    let w_top = inst.wick.x;
    let w_bot = inst.wick.y;
    let xc = (x_lo + x_hi) * 0.5;

    var p: vec2<f32>;
    if (vi < 6u) {
        // BODY FILL — solid only; hollow collapses to a zero-area point.
        if (inst.filled == 1u) {
            p = quad(x_lo, b_top, x_hi, b_bot, vi);
        } else {
            p = vec2<f32>(x_lo, b_top);
        }
    } else if (vi < 12u) {
        // WICK — thin vertical quad at the body x-center.
        p = quad(xc - WICK_HW, w_top, xc + WICK_HW, w_bot, vi - 6u);
    } else if (vi < 18u) {
        // BODY BORDER — top edge, CENTERED on b_top (straddles it by HALF_STROKE each way,
        // matching egui_plot's Polygon stroke) and extended HALF_STROKE past x_lo/x_hi so the
        // mitered corners are fully covered (epaint tessellates rect strokes with miter joins).
        // At a doji (b_top == b_bot) this exactly overlaps the bottom edge below into a single
        // ~1px line, instead of the old inset edges' adjacent-but-non-overlapping ~2px line.
        p = quad(
            x_lo - HALF_STROKE, b_top - HALF_STROKE, x_hi + HALF_STROKE, b_top + HALF_STROKE, vi - 12u
        );
    } else if (vi < 24u) {
        // BODY BORDER — bottom edge, centered on b_bot.
        p = quad(
            x_lo - HALF_STROKE, b_bot - HALF_STROKE, x_hi + HALF_STROKE, b_bot + HALF_STROKE, vi - 18u
        );
    } else if (vi < 30u) {
        // BODY BORDER — left edge, centered on x_lo (extended into the top/bottom corners too).
        p = quad(
            x_lo - HALF_STROKE, b_top - HALF_STROKE, x_lo + HALF_STROKE, b_bot + HALF_STROKE, vi - 24u
        );
    } else {
        // BODY BORDER — right edge, centered on x_hi.
        p = quad(
            x_hi - HALF_STROKE, b_top - HALF_STROKE, x_hi + HALF_STROKE, b_bot + HALF_STROKE, vi - 30u
        );
    }

    var vsout: VsOut;
    vsout.pos = to_clip(p);
    vsout.color = inst.color;
    return vsout;
}

// 0-1 linear from 0-1 sRGB gamma. Verbatim from egui-wgpu-0.35.0's egui.wgsl
// (`linear_from_gamma_rgb`) — used only by fs_main_srgb below.
fn linear_from_gamma_rgb(srgb: vec3<f32>) -> vec3<f32> {
    let cutoff = srgb < vec3<f32>(0.04045);
    let lower = srgb / vec3<f32>(12.92);
    let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    // Premultiplied output for egui's (One, OneMinusSrcAlpha) target blend. For a plain UNorm
    // (non-sRGB) target — the common case — the stored bytes ARE the gamma values directly,
    // exactly like egui's own `fs_main_gamma_framebuffer`. Selected whenever `!target.is_srgb()`;
    // unchanged by the sRGB fix (see `fs_main_srgb` below).
    return vec4<f32>(v.color.rgb * v.color.a, v.color.a);
}

@fragment
fn fs_main_srgb(v: VsOut) -> @location(0) vec4<f32> {
    // An sRGB-format target (e.g. Bgra8UnormSrgb) has the hardware apply a linear->gamma encode
    // on every write, so the shader must hand it LINEAR values for the stored bytes to end up
    // gamma-correct — exactly egui's `fs_main_linear_framebuffer` trick (egui-wgpu-0.35.0
    // egui.wgsl), picked by the identical `target_format.is_srgb()` branch (egui-wgpu-0.35.0
    // renderer.rs). Linearize rgb, THEN premultiply by alpha (alpha itself carries no gamma
    // curve, so it stays untouched — same as fs_main).
    let lin = linear_from_gamma_rgb(v.color.rgb);
    return vec4<f32>(lin * v.color.a, v.color.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The GPU byte layout the WGSL `Inst` input decodes MUST match `GpuCandle`'s `#[repr(C)]`
    /// field offsets and the `INSTANCE_ATTRS` table. Pinned here so a field reorder / stray pad
    /// can't silently desync the CPU upload from the shader (no GPU needed).
    #[test]
    fn gpu_candle_layout() {
        // Size + per-field offsets (must equal the WGSL vertex-attribute offsets).
        assert_eq!(size_of::<GpuCandle>(), 44);
        assert_eq!(GPU_CANDLE_SIZE, 44);
        assert_eq!(offset_of!(GpuCandle, body), 0);
        assert_eq!(offset_of!(GpuCandle, color), 16);
        assert_eq!(offset_of!(GpuCandle, wick), 32);
        assert_eq!(offset_of!(GpuCandle, filled), 40);

        // The attribute table (indexed by shader_location) points at the matching field offset.
        assert_eq!(INSTANCE_ATTRS[0].shader_location, 0);
        assert_eq!(INSTANCE_ATTRS[0].offset, offset_of!(GpuCandle, body) as u64);
        assert_eq!(INSTANCE_ATTRS[1].shader_location, 1);
        assert_eq!(INSTANCE_ATTRS[1].offset, offset_of!(GpuCandle, wick) as u64);
        assert_eq!(INSTANCE_ATTRS[2].shader_location, 2);
        assert_eq!(INSTANCE_ATTRS[2].offset, offset_of!(GpuCandle, filled) as u64);
        assert_eq!(INSTANCE_ATTRS[3].shader_location, 3);
        assert_eq!(INSTANCE_ATTRS[3].offset, offset_of!(GpuCandle, color) as u64);

        // Uniform is exactly one vec4 (16 bytes, the WGSL/std140 minimum).
        assert_eq!(size_of::<ScreenUniform>(), 16);
    }

    /// `From<&CandleInstance>` maps every field into the right GPU slot, and `write_ne` emits
    /// them in `#[repr(C)]` order (native-endian) — the bytes the shader reads back.
    #[test]
    fn candle_instance_maps_and_serializes() {
        let ci = CandleInstance {
            x_lo: 1.0,
            x_hi: 2.0,
            body_top: 3.0,
            body_bot: 4.0,
            wick_top: 5.0,
            wick_bot: 6.0,
            color: [0.1, 0.2, 0.3, 0.4],
            filled: 1,
        };
        let g = GpuCandle::from(&ci);
        assert_eq!(g.body, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(g.color, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(g.wick, [5.0, 6.0]);
        assert_eq!(g.filled, 1);

        let mut buf = Vec::new();
        g.write_ne(&mut buf);
        assert_eq!(buf.len(), GPU_CANDLE_SIZE);
        // body @0, color @16, wick @32, filled @40 — spot-check each region's first scalar.
        assert_eq!(&buf[0..4], &1.0f32.to_ne_bytes()); // body.x_lo
        assert_eq!(&buf[16..20], &0.1f32.to_ne_bytes()); // color.r
        assert_eq!(&buf[32..36], &5.0f32.to_ne_bytes()); // wick_top
        assert_eq!(&buf[40..44], &1u32.to_ne_bytes()); // filled

        // Hollow candle carries filled = 0.
        let hollow = CandleInstance { filled: 0, ..ci };
        assert_eq!(GpuCandle::from(&hollow).filled, 0);
    }

    #[test]
    fn screen_uniform_serializes_rect_in_order() {
        let u = ScreenUniform { rect: [10.0, 20.0, 300.0, 150.0] };
        let b = u.to_ne_bytes();
        assert_eq!(&b[0..4], &10.0f32.to_ne_bytes());
        assert_eq!(&b[4..8], &20.0f32.to_ne_bytes());
        assert_eq!(&b[8..12], &300.0f32.to_ne_bytes());
        assert_eq!(&b[12..16], &150.0f32.to_ne_bytes());
    }
}
