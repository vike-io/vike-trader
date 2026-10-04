//! The Vike brand (design system spec §6): the mark, its orange, the name, and the app icon.
//!
//! The mark is vike.io's own — an orange V made of a circle and a round-capped diagonal — measured
//! off the site's `static/v.png` into a 312-unit box. The spec's six refinements live here as code:
//! the vector master ([`MARK`]), one orange ([`ORANGE`]), a small-size mark ([`MARK_SMALL`]), a
//! darker orange for light backgrounds ([`ORANGE_ON_LIGHT`]), single-colour versions (the small
//! mark in white or black, `assets/brand/`), and the name ([`APP_NAME`]).
//!
//! Everything else is derived from those constants: the mark in the app's own title bar
//! ([`paint_mark`]), the window icon ([`window_icon`]), and every file under `assets/brand/`,
//! which `crates/vike-ui-theme/tests/brand_assets.rs` regenerates and compares.
//!
//! The orange is the LOGO's colour and never a UI accent: it sits 22° of hue from the classic
//! "down" red and 19° from the warning amber (§6).

use egui::Color32;

use crate::theme::{Theme, ThemeId};

/// The brand orange, `#FF6A00` — vike.io's `--color-brand`. The site's PNG is painted `#FF751F`;
/// that one is not used (refinement 2).
pub const ORANGE: Color32 = Color32::from_rgb(0xFF, 0x6A, 0x00);

/// The orange on a LIGHT background, `#D95400`: [`ORANGE`] on white is under the 3:1 a graphic
/// needs (refinement 4).
pub const ORANGE_ON_LIGHT: Color32 = Color32::from_rgb(0xD9, 0x54, 0x00);

/// The name every OS surface shows: the window title, the About item, the Linux launcher. Spelled
/// "Vike" (refinement 6). Owner decision D2 of the PR 5 plan.
pub const APP_NAME: &str = "Vike Trader";

/// The application id: the Wayland app id, the X11 window class, and the base name of the Linux
/// desktop entry and its icons. Owner decision D3.
///
/// ⚠ PERMANENT once shipped. A desktop pins launchers, groups windows and keys per-app settings on
/// it, so changing it later strands every user's pinned icon.
pub const APP_ID: &str = "io.vike.Trader";

/// The side of the box every mark coordinate is in: `static/v.png` is 312 × 312.
pub const BOX: f32 = 312.0;

/// One drawing of the mark: a filled circle and a round-capped diagonal, in [`BOX`] units, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    /// The circle's centre.
    pub centre: (f32, f32),
    /// The circle's radius.
    pub radius: f32,
    /// One end of the diagonal; the end is a round cap.
    pub from: (f32, f32),
    /// The other end of the diagonal, likewise capped.
    pub to: (f32, f32),
    /// The diagonal's width.
    pub stroke: f32,
}

/// The vector master (refinement 1), measured on `static/v.png`. The diagonal is exactly as wide
/// as the circle.
pub const MARK: Mark = Mark {
    centre: (74.5, 88.5),
    radius: 62.0,
    from: (238.0, 87.0),
    to: (155.0, 229.0),
    stroke: 124.0,
};

/// The small-size mark (refinement 3): a smaller circle, moved away from a lighter diagonal, so the
/// gap between them is about 41 units instead of 16 — about two pixels at 16 px, where the master's
/// closes to under one.
pub const MARK_SMALL: Mark = Mark {
    centre: (64.0, 94.0),
    radius: 52.0,
    from: (238.0, 87.0),
    to: (155.0, 229.0),
    stroke: 108.0,
};

/// The largest drawing, in physical pixels, that uses [`MARK_SMALL`] (§6: "32 px and below").
pub const SMALL_MAX_PX: f32 = 32.0;

/// Whether a drawing `px` physical pixels on a side is small enough for [`MARK_SMALL`].
pub fn is_small(px: f32) -> bool {
    px <= SMALL_MAX_PX
}

/// The mark for a drawing `px` physical pixels on a side.
pub fn mark_for_px(px: f32) -> &'static Mark {
    if is_small(px) { &MARK_SMALL } else { &MARK }
}

impl Mark {
    /// The mark's bounding box, `[x0, y0, x1, y1]` in box units: the circle and the diagonal's
    /// caps, which are discs of half the stroke at each end.
    pub fn bbox(&self) -> [f64; 4] {
        let (cx, cy, r) =
            (f64::from(self.centre.0), f64::from(self.centre.1), f64::from(self.radius));
        let h = f64::from(self.stroke) / 2.0;
        let (ax, ay) = (f64::from(self.from.0), f64::from(self.from.1));
        let (bx, by) = (f64::from(self.to.0), f64::from(self.to.1));
        [
            (cx - r).min(ax.min(bx) - h),
            (cy - r).min(ay.min(by) - h),
            (cx + r).max(ax.max(bx) + h),
            (cy + r).max(ay.max(by) + h),
        ]
    }

    /// The centre of [`Mark::bbox`] — the point the app icon puts at its tile's centre.
    pub fn bbox_centre(&self) -> (f64, f64) {
        let [x0, y0, x1, y1] = self.bbox();
        ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
    }

    /// The clear space between the circle and the diagonal, in box units.
    pub fn gap(&self) -> f64 {
        let (cx, cy) = (f64::from(self.centre.0), f64::from(self.centre.1));
        let (ax, ay) = (f64::from(self.from.0), f64::from(self.from.1));
        let (dx, dy) = (f64::from(self.to.0) - ax, f64::from(self.to.1) - ay);
        let t = (((cx - ax) * dx + (cy - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        let (ex, ey) = (cx - ax - t * dx, cy - ay - t * dy);
        (ex * ex + ey * ey).sqrt() - f64::from(self.radius) - f64::from(self.stroke) / 2.0
    }
}

/// The app icon's tile (§6: "the orange mark on a Graphite tile"). Owner decision D1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    /// Corner radius, in box units.
    pub radius: f32,
    /// The mark's scale on the tile above [`SMALL_MAX_PX`]: 1.0 lets the mark's own box fill the
    /// tile.
    pub mark_scale: f32,
    /// The mark's scale at [`SMALL_MAX_PX`] and below, where it grows so it stays legible.
    pub mark_scale_small: f32,
    /// Fill with Graphite's chart gradient (`grad_top` at the top, `bg` at the bottom) instead of
    /// a flat `bg`.
    pub gradient: bool,
    /// Width, in box units, of a hairline in Graphite's `border` colour inside the tile's edge; 0
    /// for none. Never drawn at [`SMALL_MAX_PX`] and below, where it would be a sub-pixel smear.
    pub hairline: f32,
}

/// The tile: variant B of decision D1 — the chart gradient, a hairline, the mark's box at 11/16 of
/// the tile and 13/16 when small.
pub const TILE: Tile = Tile {
    radius: 64.0,
    mark_scale: 0.6875,
    mark_scale_small: 0.8125,
    gradient: true,
    hairline: 4.0,
};

/// Where the tile sits in the icon's square canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grid {
    /// The tile fills the canvas: Windows, Linux, and the window icon off macOS.
    FullBleed,
    /// The tile sits in Apple's content square, 824 of 1024 units and centred, as every macOS app
    /// icon does: the Dock and the `.icns`. No drop shadow is drawn.
    AppleInset,
}

impl Grid {
    /// The share of the canvas's side the tile spans.
    pub fn span(self) -> f64 {
        match self {
            Grid::FullBleed => 1.0,
            Grid::AppleInset => 824.0 / 1024.0,
        }
    }
}

/// Where everything sits in an app icon's [`BOX`]-unit canvas: the one placement the rasterizer
/// and the SVG writer both use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileLayout {
    /// The canvas centre, on both axes; the tile is centred on it.
    pub centre: f64,
    /// Half the tile's side.
    pub half: f64,
    /// The tile's corner radius.
    pub radius: f64,
    /// The hairline's width, when one is drawn.
    pub hairline: Option<f64>,
    /// The mark drawn.
    pub mark: &'static Mark,
    /// A mark point `p` lands at `offset + scale * p`.
    pub scale: f64,
    /// See [`TileLayout::scale`].
    pub offset: (f64, f64),
}

/// The layout of the app icon on `grid`, with the small mark (and no hairline) when `small`. The
/// mark's bounding box is centred on the tile.
pub fn tile_layout(grid: Grid, small: bool) -> TileLayout {
    let g = grid.span();
    let mark = if small { &MARK_SMALL } else { &MARK };
    let scale = f64::from(if small { TILE.mark_scale_small } else { TILE.mark_scale }) * g;
    let centre = f64::from(BOX) / 2.0;
    let (bx, by) = mark.bbox_centre();
    let hairline = f64::from(TILE.hairline) * g;
    TileLayout {
        centre,
        half: centre * g,
        radius: f64::from(TILE.radius) * g,
        hairline: (!small && hairline > 0.0).then_some(hairline),
        mark,
        scale,
        offset: (centre - scale * bx, centre - scale * by),
    }
}

/// Samples per pixel along each axis where a pixel straddles an edge: 16 × 16 = 256 samples, so
/// coverage has 257 levels — finer than the 8-bit alpha it becomes.
const SAMPLES: u32 = 16;

/// A square with rounded corners centred at (`c`, `c`).
#[derive(Clone, Copy)]
struct RoundedSquare {
    c: f64,
    half: f64,
    r: f64,
}

impl RoundedSquare {
    /// Signed distance to the edge, negative inside. Exact, so a whole pixel can be classified.
    fn sd(self, x: f64, y: f64) -> f64 {
        let qx = (x - self.c).abs() - (self.half - self.r);
        let qy = (y - self.c).abs() - (self.half - self.r);
        let (ox, oy) = (qx.max(0.0), qy.max(0.0));
        (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - self.r
    }
}

/// What one point of an app icon shows. The discriminant indexes [`blend`]'s counts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Out = 0,
    Fill = 1,
    Ring = 2,
    Mark = 3,
}

/// An app icon's shapes, in canvas units.
struct Scene {
    circle: (f64, f64, f64),
    from: (f64, f64),
    to: (f64, f64),
    half_stroke: f64,
    tile: RoundedSquare,
    inner: Option<RoundedSquare>,
}

impl Scene {
    fn new(l: &TileLayout) -> Scene {
        let at = |(x, y): (f32, f32)| {
            (l.offset.0 + l.scale * f64::from(x), l.offset.1 + l.scale * f64::from(y))
        };
        let (cx, cy) = at(l.mark.centre);
        Scene {
            circle: (cx, cy, l.scale * f64::from(l.mark.radius)),
            from: at(l.mark.from),
            to: at(l.mark.to),
            half_stroke: l.scale * f64::from(l.mark.stroke) / 2.0,
            tile: RoundedSquare { c: l.centre, half: l.half, r: l.radius },
            inner: l.hairline.map(|h| RoundedSquare {
                c: l.centre,
                half: l.half - h,
                r: l.radius - h,
            }),
        }
    }

    /// Signed distance to the mark (circle ∪ diagonal). Exact outside; inside, at least as deep as
    /// the point really is — all [`Scene::whole`] needs.
    fn sd_mark(&self, x: f64, y: f64) -> f64 {
        let (cx, cy, r) = self.circle;
        let circle = ((x - cx) * (x - cx) + (y - cy) * (y - cy)).sqrt() - r;
        let ((ax, ay), (bx, by)) = (self.from, self.to);
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        let (ex, ey) = (x - ax - t * dx, y - ay - t * dy);
        circle.min((ex * ex + ey * ey).sqrt() - self.half_stroke)
    }

    fn class(&self, x: f64, y: f64) -> Class {
        if self.tile.sd(x, y) > 0.0 {
            Class::Out
        } else if self.sd_mark(x, y) <= 0.0 {
            Class::Mark
        } else if self.inner.is_some_and(|i| i.sd(x, y) > 0.0) {
            Class::Ring
        } else {
            Class::Fill
        }
    }

    /// Whether every point within `h` of (`x`, `y`) is in the same class as (`x`, `y`).
    fn whole(&self, x: f64, y: f64, h: f64) -> bool {
        self.tile.sd(x, y).abs() > h
            && self.sd_mark(x, y).abs() > h
            && self.inner.is_none_or(|i| i.sd(x, y).abs() > h)
    }
}

/// The tile's colour on canvas row `y`: Graphite's chart gradient, or its flat background.
fn tile_fill(l: &TileLayout, t: &Theme, y: f64) -> Color32 {
    if !TILE.gradient {
        return t.bg;
    }
    let f = ((y - (l.centre - l.half)) / (2.0 * l.half)).clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * f).round() as u8;
    Color32::from_rgb(
        mix(t.grad_top.r(), t.bg.r()),
        mix(t.grad_top.g(), t.bg.g()),
        mix(t.grad_top.b(), t.bg.b()),
    )
}

/// One pixel from its sample counts (indexed by [`Class`]): straight alpha, integer rounding only.
/// `colours` are the fill, the hairline and the mark.
fn blend(counts: [u32; 4], colours: [Color32; 3]) -> [u8; 4] {
    let inside = counts[1] + counts[2] + counts[3];
    if inside == 0 {
        return [0; 4];
    }
    let channel = |pick: fn(Color32) -> u8| {
        let sum: u32 = (0..3).map(|k| counts[k + 1] * u32::from(pick(colours[k]))).sum();
        ((sum + inside / 2) / inside) as u8
    };
    let total = SAMPLES * SAMPLES;
    [
        channel(|c| c.r()),
        channel(|c| c.g()),
        channel(|c| c.b()),
        ((255 * inside + total / 2) / total) as u8,
    ]
}

/// The app icon, `side` × `side`, RGBA8 with straight alpha, rows top to bottom. `shortcut` lets a
/// pixel far from every edge take its class from its centre; [`app_icon_rgba`] always takes it,
/// and a test proves it changes no pixel.
fn rasterize(side: u32, grid: Grid, small: bool, shortcut: bool) -> Vec<u8> {
    let l = tile_layout(grid, small);
    let scene = Scene::new(&l);
    let theme = Theme::of(ThemeId::Graphite);
    let u = f64::from(BOX) / f64::from(side);
    // Just over half a pixel's diagonal: a centre farther than this from every edge decides the
    // whole pixel.
    let h = u * 0.7072;
    let n = f64::from(SAMPLES);
    let mut out = Vec::with_capacity((side * side * 4) as usize);
    for py in 0..side {
        let fill = tile_fill(&l, theme, (f64::from(py) + 0.5) * u);
        for px in 0..side {
            let (xc, yc) = ((f64::from(px) + 0.5) * u, (f64::from(py) + 0.5) * u);
            let mut counts = [0u32; 4];
            if shortcut && scene.whole(xc, yc, h) {
                counts[scene.class(xc, yc) as usize] = SAMPLES * SAMPLES;
            } else {
                for j in 0..SAMPLES {
                    let y = (f64::from(py) + (f64::from(j) + 0.5) / n) * u;
                    for i in 0..SAMPLES {
                        let x = (f64::from(px) + (f64::from(i) + 0.5) / n) * u;
                        counts[scene.class(x, y) as usize] += 1;
                    }
                }
            }
            out.extend_from_slice(&blend(counts, [fill, theme.border, ORANGE]));
        }
    }
    out
}

/// The app icon at `side` × `side` pixels on `grid`: RGBA8, straight alpha, rows top to bottom. At
/// [`SMALL_MAX_PX`] and below it is drawn with [`MARK_SMALL`].
///
/// Coverage is counted on a 16 × 16 grid of samples per pixel, from `+ − × ÷` and `sqrt` alone
/// (decision 0032), so every platform produces the same bytes.
pub fn app_icon_rgba(side: u32, grid: Grid) -> Vec<u8> {
    rasterize(side, grid, is_small(side as f32), true)
}

/// The window's own icon: what the Windows taskbar and Alt-Tab show, and X11's window list; eframe
/// hands the same image to the macOS Dock. Wayland ignores it — its taskbar finds the icon through
/// the desktop entry named after [`APP_ID`].
///
/// ⚠ ONE image serves every size, and on Windows eframe RESIZES it (Lanczos3) to the system's big
/// and small icon sizes — 32 and 16 px at 100% scaling. So off macOS it is the SMALL mark drawn at
/// 64 px, which halves and quarters cleanly into both. The macOS Dock shows up to 512 px, so there
/// it is the master in Apple's content square at 512 px.
pub fn window_icon() -> egui::IconData {
    let (side, grid, small) = if cfg!(target_os = "macos") {
        (512, Grid::AppleInset, false)
    } else {
        (64, Grid::FullBleed, true)
    };
    egui::IconData { rgba: rasterize(side, grid, small, true), width: side, height: side }
}

/// Paint the orange mark into `rect`: the mark's box fitted to the rect's shorter side and centred.
/// It is [`MARK_SMALL`] when that side is at most [`SMALL_MAX_PX`] physical pixels. The same on
/// every theme (§6).
pub fn paint_mark(painter: &egui::Painter, rect: egui::Rect) {
    let side = rect.width().min(rect.height());
    let mark = mark_for_px(side * painter.ctx().pixels_per_point());
    let s = side / BOX;
    let origin = rect.center() - egui::vec2(side, side) / 2.0;
    let at = |(x, y): (f32, f32)| origin + egui::vec2(x, y) * s;
    let (a, b) = (at(mark.from), at(mark.to));
    painter.circle_filled(at(mark.centre), mark.radius * s, ORANGE);
    painter.line_segment([a, b], egui::Stroke::new(mark.stroke * s, ORANGE));
    // egui strokes end square; the mark's diagonal ends round.
    painter.circle_filled(a, mark.stroke * s / 2.0, ORANGE);
    painter.circle_filled(b, mark.stroke * s / 2.0, ORANGE);
}

/// The Linux desktop entry, launching `exec` (already quoted for the `Exec` key). A Wayland
/// taskbar matches a window to the entry whose file name is the window's app id, and X11 through
/// `StartupWMClass`; both are [`APP_ID`].
pub fn desktop_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={APP_NAME}\n\
         GenericName=Trading terminal\n\
         Comment=Charts, order entry and backtests on your Vike backend\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Office;Finance;\n\
         StartupWMClass={APP_ID}\n"
    )
}

/// The desktop entry's file name: the app id, so a Wayland compositor can find it.
pub fn desktop_file_name() -> String {
    format!("{APP_ID}.desktop")
}

/// The hicolor sizes the Linux icons ship at (§6: 16 to 512 px). Each is
/// `assets/brand/png/app-icon-<size>.png`.
pub const LINUX_ICON_PX: [u32; 9] = [16, 22, 24, 32, 48, 64, 128, 256, 512];

#[cfg(test)]
mod tests {
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
        for (side, grid) in [(256, Grid::FullBleed), (32, Grid::FullBleed), (256, Grid::AppleInset)]
        {
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
            assert!(
                (a.min(b)..=a.max(b)).contains(&p[i]),
                "channel {i}: {} not in {a}..={b}",
                p[i]
            );
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
        for (side, grid, small) in [
            (48, Grid::FullBleed, false),
            (24, Grid::FullBleed, true),
            (64, Grid::AppleInset, false),
        ] {
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
}
