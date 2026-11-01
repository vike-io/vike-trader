//! Tests for `brand`: the mark, its orange, the names and the icon rasterizer.

use super::*;
use crate::color_math::contrast_ratio;

#[test]
fn the_brand_colours_and_names_are_the_ruled_ones() {
    assert_eq!(ORANGE, Color32::from_rgb(0xFF, 0x6A, 0x00));
    assert_eq!(ORANGE_ON_LIGHT, Color32::from_rgb(0xD9, 0x54, 0x00));
    assert_eq!(APP_NAME, "Vike Trader");
    assert_eq!(APP_ID, "io.vike.Trader");
}

/// Refinement 4's reason, and §6's "always orange, on every theme".
#[test]
fn the_orange_reads_on_every_theme_and_only_the_dark_orange_on_white() {
    for id in ThemeId::ALL {
        let r = contrast_ratio(ORANGE, Theme::of(id).bg);
        assert!(r >= 3.0, "{id:?}: {r:.2}");
    }
    assert!(contrast_ratio(ORANGE, Color32::WHITE) < 3.0);
    assert!(contrast_ratio(ORANGE_ON_LIGHT, Color32::WHITE) >= 3.0);
}

#[test]
fn the_masters_diagonal_is_as_wide_as_its_circle() {
    assert_eq!(MARK.stroke, 2.0 * MARK.radius);
}

/// Refinement 3, in box units: about 16 on the master, about 41 on the small mark.
#[test]
fn the_small_mark_opens_the_gap() {
    assert!((MARK.gap() - 16.4).abs() < 0.1, "{}", MARK.gap());
    assert!((MARK_SMALL.gap() - 40.7).abs() < 0.1, "{}", MARK_SMALL.gap());
}

#[test]
fn thirty_two_pixels_and_below_draw_the_small_mark() {
    assert_eq!(mark_for_px(16.0), &MARK_SMALL);
    assert_eq!(mark_for_px(32.0), &MARK_SMALL);
    assert_eq!(mark_for_px(32.5), &MARK);
}

#[test]
fn the_bounding_boxes_are_measured() {
    assert_eq!(MARK.bbox(), [12.5, 25.0, 300.0, 291.0]);
    assert_eq!(MARK_SMALL.bbox(), [12.0, 33.0, 292.0, 283.0]);
    assert_eq!(MARK.bbox_centre(), (156.25, 158.0));
    assert_eq!(MARK_SMALL.bbox_centre(), (152.0, 158.0));
}

/// Every layout keeps the mark inside the tile with clear space: 16 box units at full bleed,
/// shrunk with the tile in Apple's grid.
#[test]
fn the_mark_sits_inside_its_tile_with_clear_space() {
    for grid in [Grid::FullBleed, Grid::AppleInset] {
        for small in [false, true] {
            let l = tile_layout(grid, small);
            let [x0, y0, x1, y1] = l.mark.bbox();
            let (lo, hi) = (l.centre - l.half, l.centre + l.half);
            let space = 16.0 * grid.span() + l.hairline.unwrap_or(0.0);
            for v in [l.offset.0 + l.scale * x0, l.offset.1 + l.scale * y0] {
                assert!(v >= lo + space, "{grid:?} small={small}: {v} vs {}", lo + space);
            }
            for v in [l.offset.0 + l.scale * x1, l.offset.1 + l.scale * y1] {
                assert!(v <= hi - space, "{grid:?} small={small}: {v} vs {}", hi - space);
            }
        }
    }
}

fn pixel(rgba: &[u8], side: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * side + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

/// Where the mark's circle centre lands, in pixels.
fn circle_pixel(l: &TileLayout, side: u32) -> (u32, u32) {
    let u = f64::from(BOX) / f64::from(side);
    let x = (l.offset.0 + l.scale * f64::from(l.mark.centre.0)) / u;
    let y = (l.offset.1 + l.scale * f64::from(l.mark.centre.1)) / u;
    (x as u32, y as u32)
}

#[test]
fn a_raster_is_side_squared_rgba_and_the_same_every_time() {
    let a = app_icon_rgba(48, Grid::FullBleed);
    assert_eq!(a.len(), 48 * 48 * 4);
    assert_eq!(a, app_icon_rgba(48, Grid::FullBleed));
}

#[test]
fn the_corners_are_transparent_and_the_circle_is_brand_orange() {
    for (side, grid) in [(256, Grid::FullBleed), (32, Grid::FullBleed), (256, Grid::AppleInset)] {
        let rgba = app_icon_rgba(side, grid);
        assert_eq!(pixel(&rgba, side, 0, 0)[3], 0, "{grid:?} {side}: the top-left corner");
        let (x, y) = circle_pixel(&tile_layout(grid, is_small(side as f32)), side);
        assert_eq!(pixel(&rgba, side, x, y), [0xFF, 0x6A, 0x00, 255], "{grid:?} {side}");
    }
}

/// Inside the tile and away from the mark, a pixel is opaque Graphite: between the gradient's
/// two stops, channel by channel (equal to `bg` on a flat tile).
#[test]
fn the_tile_is_graphite() {
    let side = 256;
    let rgba = app_icon_rgba(side, Grid::FullBleed);
    let t = Theme::of(ThemeId::Graphite);
    let p = pixel(&rgba, side, side / 8, side * 7 / 8);
    assert_eq!(p[3], 255);
    let stops =
        [(t.grad_top.r(), t.bg.r()), (t.grad_top.g(), t.bg.g()), (t.grad_top.b(), t.bg.b())];
    for (i, (a, b)) in stops.into_iter().enumerate() {
        assert!((a.min(b)..=a.max(b)).contains(&p[i]), "channel {i}: {} not in {a}..={b}", p[i]);
    }
}

/// Refinement 3, on pixels: at 16 px the small mark still shows the tile between the circle and
/// the diagonal, and the master's gap is closed.
#[test]
fn at_sixteen_pixels_only_the_small_mark_keeps_its_gap() {
    let darkest = |small: bool| -> u8 {
        let side = 16;
        let rgba = rasterize(side, Grid::FullBleed, small, true);
        let l = tile_layout(Grid::FullBleed, small);
        let m = l.mark;
        let (cx, cy) = (f64::from(m.centre.0), f64::from(m.centre.1));
        let (ax, ay) = (f64::from(m.from.0), f64::from(m.from.1));
        let (dx, dy) = (f64::from(m.to.0) - ax, f64::from(m.to.1) - ay);
        let t = (((cx - ax) * dx + (cy - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        let (qx, qy) = (ax + t * dx, ay + t * dy);
        let u = f64::from(BOX) / f64::from(side);
        // Walk from the circle's centre to the nearest point of the diagonal; keep the darkest
        // red channel met (the tile's red is under 25, the orange's 255).
        (0..=64)
            .map(|i| {
                let f = f64::from(i) / 64.0;
                let (x, y) = (cx + (qx - cx) * f, cy + (qy - cy) * f);
                let (x, y) = ((l.offset.0 + l.scale * x) / u, (l.offset.1 + l.scale * y) / u);
                pixel(&rgba, side, x as u32, y as u32)[0]
            })
            .min()
            .expect("the walk has points")
    };
    let (small, master) = (darkest(true), darkest(false));
    assert!(small < 128, "the small mark's gap must read dark at 16 px (darkest red {small})");
    assert!(
        small < master,
        "the small mark ({small}) must open a darker gap than the master ({master})"
    );
}

/// The shortcut changes nothing: the same icons sampled at every pixel are byte-identical.
#[test]
fn classifying_whole_pixels_from_their_centre_changes_no_pixel() {
    for (side, grid, small) in
        [(48, Grid::FullBleed, false), (24, Grid::FullBleed, true), (64, Grid::AppleInset, false)]
    {
        assert_eq!(
            rasterize(side, grid, small, true),
            rasterize(side, grid, small, false),
            "{side}"
        );
    }
}

#[test]
fn the_window_icon_is_the_small_mark_at_64_px_off_macos() {
    let icon = window_icon();
    if cfg!(target_os = "macos") {
        assert_eq!((icon.width, icon.height), (512, 512));
    } else {
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.rgba, rasterize(64, Grid::FullBleed, true, true));
    }
}

#[test]
fn paint_mark_draws_the_small_mark_in_orange_inside_its_rect() {
    let ctx = egui::Context::default();
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 5.0), egui::vec2(24.0, 22.0));
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| paint_mark(ui.painter(), rect));
    let shapes: Vec<egui::Shape> =
        std::mem::take(&mut out.shapes).into_iter().map(|c| c.shape).collect();
    out.drop_without_applying_deltas();
    let s = 22.0 / BOX;
    let inside = rect.expand(0.01);
    let (mut radii, mut segments) = (Vec::new(), 0);
    for shape in &shapes {
        match shape {
            egui::Shape::Circle(c) => {
                assert_eq!(c.fill, ORANGE);
                assert!(inside.contains(c.center), "{:?} outside {rect:?}", c.center);
                radii.push(c.radius);
            }
            egui::Shape::LineSegment { points, stroke } => {
                assert_eq!(stroke.color, ORANGE);
                assert!(points.iter().all(|p| inside.contains(*p)), "{points:?}");
                segments += 1;
            }
            _ => {}
        }
    }
    assert_eq!(segments, 1);
    let cap = MARK_SMALL.stroke * s / 2.0;
    assert_eq!(radii, vec![MARK_SMALL.radius * s, cap, cap]);
}

#[test]
fn the_desktop_entry_names_the_app_id_wherever_a_desktop_matches_it() {
    let e = desktop_entry("/home/u/bin/vike-desktop");
    assert!(e.starts_with("[Desktop Entry]\n"), "{e}");
    for want in [
        format!("Name={APP_NAME}"),
        format!("Icon={APP_ID}"),
        format!("StartupWMClass={APP_ID}"),
        "Exec=/home/u/bin/vike-desktop".to_string(),
        "Type=Application".to_string(),
    ] {
        assert!(e.lines().any(|l| l == want), "missing `{want}` in:\n{e}");
    }
    assert_eq!(desktop_file_name(), format!("{APP_ID}.desktop"));
}
