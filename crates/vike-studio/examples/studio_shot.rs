//! Headless Studio → PNG capture harness — the vike-studio twin of vike-chart's
//! `examples/export_png.rs`, and a lighter alternative to the `VIKE_SHOT` flow (which needs a full
//! `vike-app` build: eframe + winit + every bridge crate).
//!
//! Renders the REAL `StudioState::ui` off-screen — no window, no eframe — into `studio-<tab>.png`:
//!
//!   1. seed a temp `DataFusionHist` with a bar series AND a tick series so the picker has both
//!      row families, build a `StudioState` over it, and drive `ui()` through
//!      `egui::Context::run_ui` for a few frames (egui needs a second frame for combo/panel layout
//!      to settle);
//!   2. `ctx.tessellate(...)` → `Vec<ClippedPrimitive>`;
//!   3. paint those with the SAME production renderer eframe uses — `egui_wgpu::Renderer` — into an
//!      off-screen `Rgba8UnormSrgb` texture on a headless wgpu device (WARP/DX12 on Windows);
//!   4. copy the texture back and save it as a PNG via `image`;
//!   5. gate the readback with the shared non-golden pixel-liveness floors
//!      (`vike_ui_theme::pixel_liveness`), so a run that saved a blank frame reddens the
//!      `png-export` lane instead of exiting 0.
//!
//! Everything here is dev-only (the `png-export` feature); the library stays GPU-free.
//!
//! ```sh
//! cargo run -p vike-studio --features png-export --example studio_shot            # every tab
//! cargo run -p vike-studio --features png-export --example studio_shot -- strategy
//! ```

use std::sync::Arc;

use vike_data::{DataFusionHist, HistStore};
use vike_model::{Bar, QuoteTick, TradeTick};
use vike_studio::{ChatApiKeys, QaAutorun, RightTab, SavedStrategy, StrategySource, StudioState};

type TexDelta = (egui::TextureId, egui::epaint::ImageDelta);

// width*4 must be a 256-multiple to skip row padding on readback; 1600 is.
const PPP: f32 = 1.5;
const W_PX: u32 = 1600;
const H_PX: u32 = 1000;

/// Liveness floors for the three Studio poses. The mechanism and the rebaseline-free argument
/// live in `vike_ui_theme::pixel_liveness`; the numbers are facts about THESE scenes, so they
/// live beside them, and every margin is chosen to hold on both rasterizers that run this harness
/// (lavapipe in CI, DX12/WARP or a real driver on the dev box).
///
/// - `min_distinct_colors: 64` — every pose carries the two-row toolbar, the expanded left
///   editor full of syntax-highlighted Rhai (the default template), the tool rail's icons and a
///   POSED tools pane (native strategy form / sweep grid / two saved entries), so a healthy
///   frame's anti-aliased text alone measures distinct colours in the hundreds-to-thousands on
///   either rasterizer; 64 keeps over an order of magnitude of margin while a collapsed frame
///   measures 1.
/// - `max_dominant_share: 0.98` — bounds the one-colour-ate-the-frame class without any claim
///   about layout: even in the worst legitimate case where every panel shares one background
///   fill, the editor's glyph coverage plus toolbar/rail/tools chrome keeps any single colour
///   around 90% of a healthy frame, against 100.00% for a dead one.
/// - `min_quadrant_distinct: None`, DELIBERATELY (the chart harness argues the opposite for its
///   scene): the central panel is a mostly-flat getting-started block by design — no run result
///   exists in a capture — panel fills may legitimately share one colour, and which side of a
///   quadrant boundary a panel seam lands on is a LAYOUT accident that a resize or a default
///   panel-width change would flip. A quadrant floor here would gate on that accident, i.e. a
///   false-fire generator rather than a liveness signal.
const LIVENESS: vike_ui_theme::pixel_liveness::LivenessSpec =
    vike_ui_theme::pixel_liveness::LivenessSpec {
        min_distinct_colors: 64,
        max_dominant_share: 0.98,
        min_quadrant_distinct: None,
    };

fn main() {
    let want: Vec<String> = std::env::args().skip(1).collect();
    let tabs: Vec<(&str, RightTab)> =
        [("strategy", RightTab::Strategy), ("sweep", RightTab::Sweep), ("saved", RightTab::Saved)]
            .into_iter()
            .filter(|(n, _)| want.is_empty() || want.iter().any(|w| w == n))
            .collect();
    assert!(!tabs.is_empty(), "no known tab in {want:?} (strategy|sweep|saved)");

    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(seeded_store(dir.path()));

    for (name, tab) in tabs {
        // The pairing above is the compile-time half — it pins that each capture name still refers
        // to a live `RightTab` variant. `render_frames` takes the NAME rather than the variant
        // because the name is what arms `new_with_qa`'s workspace guard; this asserts the two
        // halves still agree before a frame is painted.
        assert_eq!(RightTab::from_qa_str(name), Some(tab), "capture name/variant pairing drifted");
        let (ctx, tex_deltas, out) = render_frames(&store, dir.path(), name);
        let img = rasterize(&ctx, tex_deltas, out, [W_PX, H_PX], PPP);
        let path = format!("studio-{name}.png");
        img.save(&path).expect("write PNG");
        // Liveness AFTER the save, so a red run leaves its PNG on disk as the diagnostic; the
        // lane fails on the panic's exit code either way. Same treatment as `export_png.rs`.
        let report =
            vike_ui_theme::pixel_liveness::pixel_report(img.as_raw(), img.width(), img.height());
        println!("pixel liveness [{name}]: {report}");
        vike_ui_theme::pixel_liveness::assert_pixels_live(&report, &LIVENESS);
        println!("wrote {path} ({W_PX}x{H_PX})");
    }
}

/// A store with BOTH families the picker now lists: 400 `binance/BTCUSDT 1m` bars (so the Rhai
/// default script and `buy_hold` both have something to trade) and a `polymarket/TKN` quote+trade
/// tick series (so the tick picker row is visible and pickable).
fn seeded_store(root: &std::path::Path) -> DataFusionHist {
    let store = DataFusionHist::open(root).expect("open hist store");
    let base: i64 = 1_767_225_600_000;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            let t = i as f64;
            let close = 60_000.0 + 2.0 * t + 400.0 * (t / 14.3).sin();
            let open = 60_000.0 + 2.0 * (t - 1.0) + 400.0 * ((t - 1.0) / 14.3).sin();
            Bar {
                ts: base + 60_000 * i,
                open,
                high: open.max(close) + 25.0,
                low: open.min(close) - 25.0,
                close,
                volume: 10.0 + (t / 5.0).cos().abs() * 90.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("append bars");

    let quotes: Vec<QuoteTick> = (0..300)
        .map(|i| QuoteTick {
            ts: base + 1_000 * i,
            local_ts: base + 1_000 * i,
            bid: 0.30 + (i % 11) as f64 * 0.001,
            ask: 0.31 + (i % 11) as f64 * 0.001,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: "TKN".into(),
        })
        .collect();
    let trades: Vec<TradeTick> = (0..300)
        .map(|i| TradeTick {
            ts: base + 1_000 * i,
            local_ts: base + 1_000 * i,
            price: 0.305 + (i % 11) as f64 * 0.001,
            size: 5.0,
            is_buyer_maker: i % 2 == 0,
            symbol: "TKN".into(),
        })
        .collect();
    store.append_quotes("polymarket", "TKN", &quotes, None).expect("append quotes");
    store.append_trades("polymarket", "TKN", &trades, None).expect("append trades");
    store
}

/// Build a `StudioState` posed for the tab named `qa_tab` and run a few `ui()` frames, keeping the
/// last frame's `FullOutput` plus every frame's texture deltas.
///
/// ⚠ The pose is set THROUGH `crates/vike-studio/src/studio.rs`'s `new_with_qa`, never by assigning
/// `right_tab`/`tools_collapsed` afterwards, and that is a correctness rule rather than a style
/// one. `new_with_qa` derives `qa_workspace_readonly` from the tab argument alone
/// (`qa_tab.is_some()`), and that flag is the ONLY thing standing between a capture run and the
/// operator's real Studio workspace: `maybe_persist_workspace` runs at the end of every `ui()`
/// frame and writes the current pose to `<project>/settings/state` the moment it drifts from what
/// was loaded. Posing by field assignment leaves the flag FALSE, so each of the three frames below
/// persisted the FORCED tab and expanded state over the user's own file — and `scripts/qa_shots.sh`
/// cannot prevent it from outside, because it isolates `VIKE_SETTINGS_DIR` while
/// `vike_model::state_path::project_state_dir` resolves by walking up from the working directory
/// instead. Pass the name and let the constructor arm the guard.
fn render_frames(
    store: &Arc<DataFusionHist>,
    state_dir: &std::path::Path,
    qa_tab: &str,
) -> (egui::Context, Vec<TexDelta>, egui::FullOutput) {
    let ctx = egui::Context::default();
    bind_named_font_families(&ctx);
    ctx.set_pixels_per_point(PPP);

    // state_dir = the store's root, the same pairing vike-app passes for a local store.
    // No provider keys: a capture harness must never open the credential store (`just qa-shots`
    // runs against a throwaway credential-free settings root for exactly that reason).
    // `QaAutorun::Off` — the capture poses the shell, it never kicks off a run.
    let mut st = StudioState::new_with_qa(
        store.clone(),
        state_dir.to_path_buf(),
        ChatApiKeys::default(),
        Some(qa_tab),
        QaAutorun::Off,
    );
    assert!(
        st.right_tab == RightTab::from_qa_str(qa_tab).expect("main() only passes known tab names"),
        "`new_with_qa` did not adopt the posed tab — the readonly guard keys off the same \
         argument, so a silent miss here is a capture run writing over the user's workspace",
    );
    // Pose the new surfaces so the capture actually SHOWS them rather than an empty default.
    st.strategy_source = StrategySource::Native;
    st.native_idx = vike_studio::native_strategies()
        .iter()
        .position(|n| *n == "buy_hold")
        .expect("buy_hold is registered");
    st.native_params = vec![("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())];
    st.grid = vec![("size".to_string(), "1, 2, 3".to_string())];
    st.saved.strategies = vec![
        SavedStrategy::rhai("sma-cross", vike_studio::EditorPane::default().source),
        SavedStrategy::native("hold-2", "buy_hold", st.native_params.clone()),
    ];

    let screen = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(W_PX as f32 / PPP, H_PX as f32 / PPP),
    );

    // The font atlas texture is emitted only in the FIRST frame's `textures_delta.set` and never
    // resent, so accumulate every frame's deltas IN ORDER and apply them all before rendering the
    // last frame (same reasoning as vike-chart's harness).
    let mut tex_deltas: Vec<TexDelta> = Vec::new();
    let mut last = None;
    for f in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| st.ui(ui));
        // egui 0.36 turned `textures_delta.set` from a `Vec<(TextureId, ImageDelta)>` into a map
        // whose value is a SmallVec — one texture id can carry several deltas in a frame. Flatten
        // back into the (id, delta) pairs `update_texture` takes. Mirrors `export_png.rs`.
        tex_deltas.extend(
            out.textures_delta
                .set
                .iter()
                .flat_map(|(id, deltas)| deltas.iter().map(move |d| (*id, d.clone()))),
        );
        // Harvested above into `tex_deltas`, so this frame's own list is handled and must be
        // cleared: egui 0.36 panics on a dropped `TexturesDelta` that still holds entries, and
        // every frame but the last IS dropped here. Same as `export_png.rs`.
        out.textures_delta.clear();
        last = Some(out);
    }
    (ctx.clone(), tex_deltas, last.expect("rendered at least one frame"))
}

/// vike-app registers custom `"light"`/`"semibold"` font families at startup; a bare `Context`
/// lacks them and epaint PANICS on first use. Bind both to the default proportional font.
fn bind_named_font_families(ctx: &egui::Context) {
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

/// Paint a tessellated egui frame into an off-screen wgpu texture and read it back as an RGBA
/// image — the same `egui_wgpu::Renderer` eframe drives, aimed at a texture instead of a surface.
/// Blocking (pollster) since this is a one-shot CLI. Lifted from vike-chart's `export_png`.
fn rasterize(
    ctx: &egui::Context,
    tex_deltas: Vec<TexDelta>,
    out: egui::FullOutput,
    size_px: [u32; 2],
    ppp: f32,
) -> image::RgbaImage {
    let paint_jobs = ctx.tessellate(out.shapes, ppp);

    pollster::block_on(async move {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                force_fallback_adapter: false,
                compatible_surface: None,
                // wgpu 30 added `apply_limit_buckets`; the default suits a headless offscreen
                // render. Same treatment as `vike-chart`'s `export_png.rs`.
                ..Default::default()
            })
            .await
            .expect("no wgpu adapter (need DX12/Vulkan or the WARP software fallback)");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("studio_shot"),
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
            let mut rpass = rpass.forget_lifetime();
            renderer.render(&mut rpass, &paint_jobs, &screen);
        }

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

        // wgpu 30 made `get_mapped_range` fallible; the map above already succeeded, so an error
        // here is a bug in this harness rather than a runtime condition.
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
