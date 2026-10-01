//! ONE offscreen rasterizer for the `png-export` harnesses.
//!
//! `crates/vike-chart/examples/export_png.rs` and `crates/vike-studio/examples/studio_shot.rs`
//! each held a private copy of this function: 121 code lines, of which 120 were identical and the
//! 121st was the wgpu device label. That parameter is the whole of what differed, and it is now an
//! argument.
//!
//! ⚠ **The copies were not merely redundant — they had already DRIFTED, and the drift shipped.**
//! The two crates each pinned `pollster` independently and the pins came apart (1.0 against 0.4);
//! a Dependabot bump touching exactly that dependency went green because, at the time, CI built
//! neither harness. `scripts/ci_feature_suite.sh`'s `png-export` arm closed the half that let the
//! bump pass unbuilt. This module closes the other half: with the renderer here, `wgpu`,
//! `pollster` and `egui-wgpu` are pinned ONCE instead of twice, so there is no longer a second
//! pin for a bump to move on its own.
//!
//! Its sibling `pixel_liveness` already took the READBACK half for these same two callers and
//! names both of them by path. This module is the other end of the same seam: that one judges the
//! bytes, this one produces them.
//!
//! ⚠ Behind the `offscreen` feature and NOT behind `test-support`, deliberately: that feature's
//! doc states it "pulls in NO new dependency", and this one pulls four. A default build of this
//! crate, and every consumer that only wants the theme, compiles none of it.

/// The accumulated font-atlas uploads a frame loop collects, handed straight to the GPU renderer.
///
/// A type alias rather than a struct: it is `egui`'s own pair, and naming it is what lets both
/// callers' frame loops stay readable without this module inventing a vocabulary it does not own.
/// Each harness previously declared this line for itself.
pub type TexDelta = (egui::TextureId, egui::epaint::ImageDelta);

/// Paint a tessellated egui frame into an off-screen wgpu texture and read it back as an RGBA PNG
/// image — the same `egui_wgpu::Renderer` eframe drives, just aimed at a texture instead of a
/// surface. Blocking (pollster) since this is a one-shot CLI.
pub fn rasterize(
    ctx: &egui::Context,
    tex_deltas: Vec<TexDelta>,
    out: egui::FullOutput,
    size_px: [u32; 2],
    ppp: f32,
    label: &'static str,
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
            .request_device(&wgpu::DeviceDescriptor { label: Some(label), ..Default::default() })
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
