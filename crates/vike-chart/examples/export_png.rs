//! Headless chart → PNG export harness (investigation prototype).
//!
//! Renders the REAL `vike_chart::chart::draw` off-screen — no window, no eframe — and writes
//! `chart.png`: authentic engine pixels for the educational site, not a mock-up. The route:
//!
//!   1. drive `draw` through `egui::Context::run_ui` exactly like `tests/draw_characterization.rs`
//!      (a few warm-up frames so the plot's deferred `set_plot_bounds` settle to the full-series
//!      view), keeping the last frame's `egui::FullOutput`;
//!   2. `ctx.tessellate(out.shapes, ppp)` → `Vec<ClippedPrimitive>`;
//!   3. paint those with the SAME production renderer eframe uses — `egui_wgpu::Renderer` — into an
//!      off-screen `Rgba8UnormSrgb` texture on a headless wgpu device (WARP/DX12 on Windows);
//!   4. copy the texture to a buffer and save it as a PNG via `image`;
//!   5. gate the readback with the shared non-golden pixel-liveness floors
//!      (`vike_ui_theme::pixel_liveness`), so a run that saved a blank frame — the silent twin of
//!      the texture-delta panic below — reddens the `png-export` lane instead of exiting 0.
//!
//! Everything here is dev-only (see this crate's `Cargo.toml`); the library stays GPU-free.
//!
//! Run:  `cargo run -p vike-chart --example export_png`   → writes `chart.png` in the cwd.

use indexmap::IndexMap;
use vike_chart::scale::ScaleAssign;
use vike_chart::{
    Active, Bar, ChartInputs, ChartOptions, ChartState, ChartStyle, FollowLive, IndicatorDialog,
    PaneFractions, PaneKey, ScaleMode, SettingsDialog, draw,
};

// ---- output geometry. width*4 must be a 256-multiple to skip row-padding on readback; 1280 is. ----
const PPP: f32 = 2.0; // pixels per point (crisp text)
const W_PX: u32 = 1280;
const H_PX: u32 = 800;

/// Liveness floors for THIS scene. The mechanism and the rebaseline-free argument live in
/// `vike_ui_theme::pixel_liveness`; the numbers are facts about the scene `build_state` poses, so
/// they live beside it. Every margin below is chosen to hold on BOTH rasterizers that run this
/// harness — Mesa lavapipe in the CI lane, DX12/WARP or a real driver on the dev box — whose AA
/// arithmetic differs enough to move exact counts but not to cross gaps this wide.
///
/// - `min_distinct_colors: 64` — the frame is ~30 candles (two body fills plus feathered edge
///   blends), wicks, an EMA(10) overlay, a hammer glyph, a volume pane, gridlines and axis label
///   text at ppp 2. Feathering is on for every shape edge and each text glyph edge alone blends
///   foreground over background through dozens of coverage levels, so a healthy frame measures
///   distinct colours in the THOUSANDS on either rasterizer; 64 sits over an order of magnitude
///   below that, while the frames this gate exists for measure 1 (nothing drawn but the clear
///   colour — e.g. an un-uploaded font atlas makes egui-wgpu skip every mesh) to ~a dozen (bare
///   panel skeleton).
/// - `max_dominant_share: 0.98` — the plot background is the biggest flat region, and candles +
///   volume bars + gridlines + labels keep it under roughly 90% of the frame; a dead frame is
///   100.00% by definition. 98% leaves real margin on the healthy side and none for a dead one.
/// - `min_quadrant_distinct: Some(8)` — TRUE of this scene, argued quadrant by quadrant (the
///   studio harness argues the opposite for its own): gridlines cross all four, the rise's
///   candles top the left half, the recovery and the late-pullback highs cross the upper right,
///   and the trough-and-hammer bars sit in the lower right (the hammer's low, 91.0, is the
///   series minimum). The lower LEFT is the one quadrant no candle reaches — the price pane
///   takes ~79% of the height (`crates/vike-chart/src/panes.rs`'s `default_share`), which puts
///   the screen midline near the bottom of the left half's price range — so it is carried by
///   the volume pane and the time-axis labels spanning the whole bottom edge instead. 8 catches
///   a render that stopped partway — a frame whose lower or right half never painted — which the
///   two GLOBAL floors can miss.
const LIVENESS: vike_ui_theme::pixel_liveness::LivenessSpec =
    vike_ui_theme::pixel_liveness::LivenessSpec {
        min_distinct_colors: 64,
        max_dominant_share: 0.98,
        min_quadrant_distinct: Some(8),
    };

fn main() {
    let state = build_state();

    // EMA(10) overlay — a real `Active` folded over the (all-closed) series, index-aligned to the
    // bars exactly as vike-app would hand it to `draw`.
    let ema_spec = vike_chart::indicators::get("ema").expect("ema indicator registered");
    let mut ema = Active::new(1, ema_spec, &state.bars);
    ema.set_params(vec![10.0], &state.bars);

    // Hammer pattern — a real `Active` drawn as a price-pane overlay. Its series is a per-bar
    // SIGNAL (+100/-100/0), so `render_overlay`'s `Category::Pattern` arm anchors a glyph to the
    // flagged bar's extreme rather than plotting the raw ±100 on the price axis.
    let hammer_spec = vike_chart::indicators::get("hammer").expect("hammer indicator registered");
    let hammer = Active::new(2, hammer_spec, &state.bars);
    let hits: Vec<usize> = hammer.outputs[0]
        .series
        .iter()
        .enumerate()
        .filter(|(_, v)| v.abs() > 0.5)
        .map(|(i, _)| i)
        .collect();
    println!("hammer indicator fired at bar index(es): {hits:?}");

    let (ctx, tex_deltas, out) = render_frames(&state, &[ema, hammer]);
    let img = rasterize(&ctx, tex_deltas, out, [W_PX, H_PX], PPP);
    let path = "chart.png";
    img.save(path).expect("write PNG");
    // Liveness is gated AFTER the save, deliberately: on a red run the PNG is already on disk, so
    // the artifact that explains the failure survives it (the lane fails on the panic's exit code
    // either way — `scripts/ci_feature_suite.sh`'s `png-export` arm). Printed on green too, so
    // the lane log carries the measured margins over time.
    let report =
        vike_ui_theme::pixel_liveness::pixel_report(img.as_raw(), img.width(), img.height());
    println!("pixel liveness: {report}");
    vike_ui_theme::pixel_liveness::assert_pixels_live(&report, &LIVENESS);
    println!("wrote {path} ({W_PX}x{H_PX})");
}

/// ~30 one-minute OHLC bars: a rise, a pullback that bottoms on a textbook HAMMER at index 22
/// (small body near the top, long lower shadow, tiny upper wick), then a recovery.
fn build_state() -> ChartState {
    let mut bars: Vec<Bar> = Vec::new();
    let mut push = |i: usize, o: f64, h: f64, l: f64, c: f64, v: f64| {
        bars.push(Bar { t: i as f64, ot: 1_700_000_000_000 + i as i64 * 60_000, o, h, l, c, v });
    };
    // rise
    push(0, 100.0, 101.2, 99.6, 100.9, 12.0);
    push(1, 100.9, 102.1, 100.5, 101.8, 14.0);
    push(2, 101.8, 103.0, 101.4, 102.6, 16.0);
    push(3, 102.6, 103.4, 102.0, 102.3, 11.0);
    push(4, 102.3, 103.9, 102.1, 103.7, 18.0);
    push(5, 103.7, 104.6, 103.3, 104.2, 15.0);
    push(6, 104.2, 105.1, 103.8, 104.0, 13.0);
    push(7, 104.0, 104.5, 102.9, 103.1, 17.0);
    // pullback
    push(8, 103.1, 103.6, 101.8, 102.0, 19.0);
    push(9, 102.0, 102.4, 100.6, 100.9, 21.0);
    push(10, 100.9, 101.3, 99.7, 100.1, 20.0);
    push(11, 100.1, 100.8, 98.9, 99.2, 22.0);
    push(12, 99.2, 99.9, 98.2, 99.5, 16.0);
    push(13, 99.5, 100.2, 98.6, 98.9, 18.0);
    push(14, 98.9, 99.4, 97.4, 97.7, 24.0);
    push(15, 97.7, 98.1, 96.5, 96.9, 23.0);
    push(16, 96.9, 97.6, 96.0, 97.3, 17.0);
    push(17, 97.3, 97.8, 96.1, 96.4, 20.0);
    push(18, 96.4, 96.9, 95.2, 95.6, 26.0);
    push(19, 95.6, 96.0, 94.4, 94.8, 28.0);
    push(20, 94.8, 95.2, 93.9, 94.2, 25.0);
    push(21, 94.2, 94.7, 93.1, 93.4, 27.0);
    // HAMMER at index 22: body 93.6->94.1 near the top, long lower shadow to 91.0, tiny upper wick.
    push(22, 93.6, 94.3, 91.0, 94.1, 34.0);
    // recovery
    push(23, 94.1, 95.6, 94.0, 95.4, 22.0);
    push(24, 95.4, 96.8, 95.1, 96.5, 20.0);
    push(25, 96.5, 97.9, 96.2, 97.6, 18.0);
    push(26, 97.6, 98.4, 96.9, 97.2, 16.0);
    push(27, 97.2, 98.6, 97.0, 98.3, 17.0);
    push(28, 98.3, 99.7, 98.1, 99.4, 19.0);
    push(29, 99.4, 100.6, 99.1, 100.2, 21.0);

    let mut s = ChartState::default();
    let n = bars.len();
    s.bars = bars;
    s.closed_len = n; // all closed — no forming bar
    s.refresh_caches();
    s
}

/// Drive `draw` for a few frames on one persistent `Context` and return the LAST frame's
/// `FullOutput`. Multiple frames are required: `draw`'s bounds writes are DEFERRED
/// (`set_plot_bounds` applies after the closure), so the full-series view only settles from the
/// second frame on — same reason `tests/draw_characterization.rs` runs >=2 frames.
type TexDelta = (egui::TextureId, egui::epaint::ImageDelta);

fn render_frames(
    state: &ChartState,
    indicators: &[Active],
) -> (egui::Context, Vec<TexDelta>, egui::FullOutput) {
    let ctx = egui::Context::default();
    bind_chart_font_families(&ctx);
    ctx.set_pixels_per_point(PPP);

    let mut follow = FollowLive::default();
    let mut settings = SettingsDialog::default();
    let mut indicator_dialog = IndicatorDialog::default();
    let mut panes = PaneFractions::default();
    let study_pane_of: IndexMap<u64, PaneKey> = IndexMap::new();
    let series_pane_of: IndexMap<String, PaneKey> = IndexMap::new();
    let series_scale: IndexMap<String, ScaleAssign> = IndexMap::new();
    let sub_panes = [PaneKey::Volume]; // authentic candles + volume pane

    let screen = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(W_PX as f32 / PPP, H_PX as f32 / PPP),
    );

    // The font atlas texture is emitted only in the FIRST frame's `textures_delta.set` and never
    // resent (static content), so accumulate every frame's deltas IN ORDER and apply them all
    // before rendering the last frame — otherwise every mesh (even solid candles use WHITE_UV into
    // the atlas) references an un-uploaded `Managed(0)` and paints nothing.
    let mut tex_deltas: Vec<TexDelta> = Vec::new();
    let mut last = None;
    for f in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = draw(
                ui,
                ChartInputs {
                    state,
                    style: ChartStyle::Candles,
                    nav: None,
                    indicators,
                    studies: &[],
                    follow: &mut follow,
                    options: &ChartOptions::default(),
                    settings: &mut settings,
                    indicator_dialog: &mut indicator_dialog,
                    scale: ScaleMode::Linear,
                    invert: false,
                    panes: &mut panes,
                    sync: None,
                    footprint: None,
                    footprint_gen: 0,
                    cvd_on: false,
                    profile_on: false,
                    of_tick_size: 0.0,
                    sub_panes: &sub_panes,
                    study_pane_of: &study_pane_of,
                    overlays: &[],
                    series_panes: &[],
                    series_pane_of: &series_pane_of,
                    series_scale: &series_scale,
                    gpu_candles: None,
                },
            );
        });
        // egui 0.36 turned `textures_delta.set` from a `Vec<(TextureId, ImageDelta)>` into a map
        // whose value is a SmallVec — one texture id can now carry several deltas in a frame. Flatten
        // it back into the (id, delta) pairs the renderer's `update_texture` takes, in map order.
        tex_deltas.extend(
            out.textures_delta
                .set
                .iter()
                .flat_map(|(id, deltas)| deltas.iter().map(move |d| (*id, d.clone()))),
        );
        // HARVESTED above into `tex_deltas`, which is what actually reaches `update_texture` — so
        // this frame's own delta list has been handled and must be cleared. egui 0.36 panics on a
        // dropped `TexturesDelta` that still holds entries, and every frame but the last IS
        // dropped here, on the next iteration's `last = Some(out)`. Compile-checking this file
        // cannot see that: it took a real GPU run to surface it.
        out.textures_delta.clear();
        last = Some(out);
    }
    (ctx.clone(), tex_deltas, last.expect("rendered at least one frame"))
}

/// `draw` lays out labels in the custom `"light"`/`"semibold"` font families vike-app registers at
/// startup; a bare `Context` lacks them and epaint PANICS on first use. Bind both to the default
/// proportional font (identical to the characterization harness's prerequisite).
fn bind_chart_font_families(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default FontDefinitions define Proportional");
    for name in ["light", "semibold"] {
        fonts.families.insert(egui::FontFamily::Name(name.into()), proportional.clone());
    }
    ctx.set_fonts(fonts);
}

/// Paint a tessellated egui frame into an off-screen wgpu texture and read it back as an RGBA PNG
/// image — the same `egui_wgpu::Renderer` eframe drives, just aimed at a texture instead of a
/// surface. Blocking (pollster) since this is a one-shot CLI.
fn rasterize(
    ctx: &egui::Context,
    tex_deltas: Vec<TexDelta>,
    out: egui::FullOutput,
    size_px: [u32; 2],
    ppp: f32,
) -> image::RgbaImage {
    // Tessellate on the SAME context that ran the frames — its font atlas is realized (a fresh
    // Context has "no fonts loaded" until a frame runs). `tex_deltas` (accumulated across frames)
    // carries that atlas to the GPU renderer below.
    let paint_jobs = ctx.tessellate(out.shapes, ppp);

    pollster::block_on(async move {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                force_fallback_adapter: false,
                compatible_surface: None,
                // wgpu 30 added `apply_limit_buckets`; the default is what a headless offscreen
                // render wants, and spelling the three fields above keeps them under review.
                ..Default::default()
            })
            .await
            .expect("no wgpu adapter (need DX12/Vulkan or the WARP software fallback)");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("export_png"),
                ..Default::default()
            })
            .await
            .expect("request_device");

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d {
                width: size_px[0],
                height: size_px[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut renderer =
            egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        for (id, delta) in &tex_deltas {
            renderer.update_texture(&device, &queue, *id, delta);
        }
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: size_px, pixels_per_point: ppp };

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let cmds = renderer.update_buffers(&device, &queue, &mut encoder, &paint_jobs, &screen);

        {
            let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.03,
                            b: 0.05,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // egui-wgpu 0.35 renders into a 'static-lifetime pass (wgpu 29 lifetime model).
            let mut rpass = rpass.forget_lifetime();
            renderer.render(&mut rpass, &paint_jobs, &screen);
        }

        // Copy the texture into a readback buffer (rows padded to 256).
        let unpadded = size_px[0] * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * size_px[1]) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(size_px[1]),
                },
            },
            wgpu::Extent3d { width: size_px[0], height: size_px[1], depth_or_array_layers: 1 },
        );

        queue.submit(cmds.into_iter().chain([encoder.finish()]));

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        rx.recv().expect("map channel").expect("map buffer");

        // wgpu 30 made `get_mapped_range` fallible (it can now report a bad range) instead of
        // panicking internally. The map above already succeeded, so an error here is a bug in this
        // harness rather than a runtime condition — expect, do not swallow.
        let data = slice.get_mapped_range().expect("mapped range");
        let mut pixels = Vec::with_capacity((unpadded * size_px[1]) as usize);
        for row in 0..size_px[1] {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        buffer.unmap();

        image::RgbaImage::from_raw(size_px[0], size_px[1], pixels).expect("image from raw")
    })
}
