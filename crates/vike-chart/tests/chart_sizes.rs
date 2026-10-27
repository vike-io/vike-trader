//! The chart SIZE / DPI matrix: `chart::draw` rendered headless at nine window sizes, four device
//! pixel ratios and five data layouts, every frame judged by what it must NEVER do — tick labels
//! that overlap or repeat, an axis tag painted over a tick label or outside its pane or the window,
//! panes that do not line up or tile the window. Property assertions only: no goldens, no pixels, so
//! a frame is judged by the rules and not by what it once looked like.
//!
//! A CELL is a window in physical pixels, a `pixels_per_point` and a layout; the logical size is
//! `px / ppp` (1366 / 1.25 = 1092.8 on purpose). The nine sizes are ONE `#[test]` each so a failure
//! names the size; each loops its ppp x layout cells. The extra families — 10,000 bars, a pointer
//! parked in every pane, four price magnitudes, a panned view, four and six indicator panes, one to
//! 100,000 bars, a window resized between frames, the tiny window that drops a pane — are one
//! `#[test]` each. `tests/common/geometry.rs` reads a frame (`read_geometry`) and holds the checkers;
//! `tests/chart_geometry_gate.rs` is their kill proof.
//!
//! EVERY assertion is enforced. The eight that first failed on the real chart were behaviour choices
//! (hide the tick under a tag chip? clamp the chip? drop a pane?) and waited as `#[ignore]`d tests for
//! the owner's ruling; the rulings are in (TradingView style: the chip wins and the tick under it is
//! not drawn, a tick label never sticks out of its pane, a tag or chip never leaves the window or its
//! pane, narrow ranges get decimals, a chip wider than the gutter shortens its text, a window too
//! short for its panes drops the lowest) and the chart implements them, so no kind is held back.
//! Never weaken a checker to make a cell pass: fix the chart.

mod common;

use std::collections::BTreeMap;

use common::geometry::{
    ChipKind, Expect, Geometry, Kind, Pane, Violation, plot_transform, read_geometry,
    tick_count_inversions, violations,
};
use common::{Case, make_state, wave_bars_scaled};
use egui::{Pos2, Rect, pos2, vec2};
use indexmap::IndexMap;
use vike_chart::{Active, Nav, chart::style::ChartStyle, model::Bar, panes::PaneKey};
use vike_ui_theme::value::chart;

/// Frames per cell: the plot memory, the last-frame widget rects and egui_plot's axis thickness
/// (which lags one frame) all need a few passes to settle; only the LAST frame is judged.
const FRAMES: usize = 4;
/// The bars of an ordinary cell: four hours of one-minute bars, so the time axis has hour marks.
const BARS: usize = 240;
/// The price level `wave_bars` is centred on; a cell's `price` rescales it.
const WAVE_CENTRE: f64 = 100.0;
/// A window this short (logical points) may drop its lowest panes: four panes at their 44pt floors
/// need a window 227pt tall (209pt of panes + separators + the 18pt time-axis reserve).
const SHORT_WINDOW_PT: f32 = 227.0;

/// The four device pixel ratios every size is run at.
const PPPS: [f32; 4] = [1.0, 1.25, 1.5, 2.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// Price + Volume + RSI + MACD: the four-pane stack of the home screen.
    Panes,
    /// Candles, price pane only (the time axis moves under the price pane).
    Candles,
    Line,
    /// No bars at all.
    Empty,
    /// One bar, with a Volume pane.
    OneBar,
    /// Price + Volume + this many study panes (RSI, MACD, Stochastic, ATR, CCI, ROC in that order):
    /// the "timeline disappears with 4+ panes" shape.
    Studies(usize),
}

/// The oscillators `Layout::Panes` (the first two) and `Layout::Studies` stack, by registry name;
/// study `n` (1-based) has uid `n` and sits in `PaneKey::Study(n)`.
const STUDIES: [&str; 6] = ["rsi", "macd", "stochastic", "atr", "cci", "roc"];

/// How many study panes a layout stacks.
fn study_count(layout: Layout) -> usize {
    match layout {
        Layout::Panes => 2,
        Layout::Studies(n) => n,
        _ => 0,
    }
}

const LAYOUTS: [Layout; 5] =
    [Layout::Panes, Layout::Candles, Layout::Line, Layout::Empty, Layout::OneBar];

/// Where the pointer is parked, resolved against the cell's own pane frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerAt {
    None,
    /// Inside the price pane, left of centre.
    Price,
    /// Inside the Volume pane (the home2 collision).
    Volume,
    /// Inside the bottom (MACD) pane.
    Macd,
    /// Far outside the window.
    OffScreen,
    /// On the first bar, hard against the window's left edge.
    LeftEdge,
    /// On the x of the first time label.
    TimeLabelX,
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    /// The window in PHYSICAL pixels.
    px: (u32, u32),
    ppp: f32,
    layout: Layout,
    bars: usize,
    /// The price level the wave is centred on.
    price: f64,
    pointer: PointerAt,
    /// `Nav::PanLeft` on the last frame.
    pan_left: bool,
    /// A steady climb under the wave (see [`climbing_bars`]).
    climb: bool,
    /// The window in physical pixels on the LAST frame when it is resized after `px` has settled
    /// (same `Context`, a different `screen_rect`); the frame judged is that one.
    resize_to: Option<(u32, u32)>,
}

/// [`wave_bars_scaled`] with a steady climb of 0.5 (times `scale`) per bar added to every price, so
/// the LAST close sits above everything a panned-back view shows: the one data shape in which the
/// last-price chip's y leaves the price pane.
fn climbing_bars(n: usize, scale: f64) -> Vec<Bar> {
    wave_bars_scaled(n, scale)
        .into_iter()
        .enumerate()
        .map(|(i, b)| {
            let d = i as f64 * 0.5 * scale;
            Bar { o: b.o + d, h: b.h + d, l: b.l + d, c: b.c + d, ..b }
        })
        .collect()
}

impl Cell {
    fn new(px: (u32, u32), ppp: f32, layout: Layout) -> Self {
        let bars = match layout {
            Layout::Empty => 0,
            Layout::OneBar => 1,
            _ => BARS,
        };
        Cell {
            px,
            ppp,
            layout,
            bars,
            price: WAVE_CENTRE,
            pointer: PointerAt::None,
            pan_left: false,
            climb: false,
            resize_to: None,
        }
    }

    /// The cell without its ppp: what a tick-density comparison groups by.
    fn key(&self) -> String {
        let resized = self.resize_to.map_or(String::new(), |(w, h)| format!(" resized to {w}x{h}"));
        format!(
            "{}x{} {:?} bars={} price={} pointer={:?} pan_left={} climb={}{resized}",
            self.px.0,
            self.px.1,
            self.layout,
            self.bars,
            self.price,
            self.pointer,
            self.pan_left,
            self.climb
        )
    }

    fn label(&self) -> String {
        format!("[{} @{}]", self.key(), self.ppp)
    }

    /// A window of `px` physical pixels, in logical points.
    fn logical_of(&self, px: (u32, u32)) -> egui::Vec2 {
        vec2(px.0 as f32 / self.ppp, px.1 as f32 / self.ppp)
    }

    fn logical(&self) -> egui::Vec2 {
        self.logical_of(self.px)
    }

    /// The window of the frame that is judged: the resized one, else the cell's own.
    fn final_logical(&self) -> egui::Vec2 {
        self.logical_of(self.resize_to.unwrap_or(self.px))
    }
}

// ============================ running a cell ============================

/// One frame's reading: the geometry, what the shapes cannot say, and the price pane's transform
/// (a pointer placement needs it).
struct Observed {
    geom: Geometry,
    expect: Expect,
    price_tr: Option<egui_plot::PlotTransform>,
}

struct Run {
    cell: Cell,
    obs: Observed,
}

/// The plot id salts `draw` gives each pane of a layout, top to bottom (see the module doc of
/// `tests/common/geometry.rs`).
fn pane_ids(layout: Layout) -> Vec<String> {
    let study = |n: u64| format!("pane_{:?}", PaneKey::Study(n));
    match layout {
        Layout::Panes | Layout::Studies(_) => ["price".to_owned(), "volume".to_owned()]
            .into_iter()
            .chain((1..=study_count(layout) as u64).map(study))
            .collect(),
        Layout::OneBar => vec!["price".to_owned(), "volume".to_owned()],
        Layout::Candles | Layout::Line | Layout::Empty => vec!["price".to_owned()],
    }
}

fn run_once(cell: &Cell, pointer: Option<Pos2>) -> Observed {
    let scale = cell.price / WAVE_CENTRE;
    let st = make_state(if cell.climb {
        climbing_bars(cell.bars, scale)
    } else {
        wave_bars_scaled(cell.bars, scale)
    });
    let n_studies = study_count(cell.layout);
    let indicators: Vec<Active> = (1..=n_studies as u64)
        .map(|uid| {
            let name = STUDIES[uid as usize - 1];
            let spec = vike_chart::indicators::get(name)
                .unwrap_or_else(|| panic!("{name} indicator registered"));
            Active::new(uid, spec, &st.bars)
        })
        .collect();
    let panes_authored: Vec<PaneKey> =
        [PaneKey::Volume].into_iter().chain((1..=n_studies as u64).map(PaneKey::Study)).collect();
    let volume_only = [PaneKey::Volume];
    let study_pane_of: IndexMap<u64, PaneKey> =
        (1..=n_studies as u64).map(|uid| (uid, PaneKey::Study(uid))).collect();

    let mut case = Case::new(&st);
    case.screen = Rect::from_min_size(Pos2::ZERO, cell.logical());
    case.resize = cell.resize_to.map(|px| Rect::from_min_size(Pos2::ZERO, cell.logical_of(px)));
    case.ppp = cell.ppp;
    case.frames = FRAMES;
    case.pointer = pointer;
    case.nav = cell.pan_left.then_some(Nav::PanLeft);
    match cell.layout {
        Layout::Panes | Layout::Studies(_) => {
            case.indicators = &indicators;
            case.sub_panes = &panes_authored;
            case.study_pane_of = &study_pane_of;
        }
        Layout::Candles => case.options.show_volume = false,
        Layout::Line => {
            case.style = ChartStyle::Line;
            case.options.show_volume = false;
        }
        Layout::Empty => {}
        Layout::OneBar => case.sub_panes = &volume_only,
    }

    let ids = pane_ids(cell.layout);
    let mut panes: Vec<Pane> = Vec::new();
    let mut price_tr = None;
    let (_actions, out, ctx) = case.run_probed(&mut |ui: &mut egui::Ui| {
        panes = ids
            .iter()
            .map_while(|id| {
                plot_transform(ui, id).map(|tr| Pane { name: id.clone(), frame: *tr.frame() })
            })
            .collect();
        price_tr = plot_transform(ui, "price");
    });
    // A window too short for every pane drops the LOWEST ones (`sub_panes_that_fit`); anywhere
    // else a missing plot means its id salt drifted, which must stay loud, not an empty frame.
    assert!(
        panes.len() == ids.len() || cell.final_logical().y < SHORT_WINDOW_PT,
        "{}: only {} of the plots {ids:?} exist after draw: an id salt drifted",
        cell.label(),
        panes.len()
    );
    assert!(!panes.is_empty(), "{}: no price plot after draw: its id salt drifted", cell.label());
    let appearance = vike_ui_theme::appearance::current(&ctx);
    let tag_bg = vike_chart::colors::ChartColors::resolve(&case.options, &appearance).tag_bg;
    let last_close_y = match (st.bars.last(), price_tr.as_ref()) {
        (Some(b), Some(tr)) => Some(tr.position_from_point_y(b.c)),
        _ => None,
    };
    let screen = case.resize.unwrap_or(case.screen);
    let geom = read_geometry(&out, screen, panes, tag_bg, !st.bars.is_empty());
    Observed { geom, expect: Expect { last_close_y }, price_tr }
}

/// Where `at` lands in the frame `o` measured (a pointer placement needs the pane frames, so the
/// cell is run once without a pointer first).
fn pointer_for(at: PointerAt, o: &Observed, label: &str) -> Option<Pos2> {
    let g = &o.geom;
    let price = g.panes[0].frame;
    let across = |f: Rect| f.left() + 0.45 * f.width();
    let middle = |f: Rect| pos2(across(f), f.center().y);
    match at {
        PointerAt::None => None,
        // Clear of the strip along the top of the pane where the price tag is deliberately hidden.
        PointerAt::Price => Some(pos2(
            across(price),
            price.top() + (0.5 * price.height()).max(chart::PRICE_TAG_HIDDEN_STRIP_H + 4.0),
        )),
        PointerAt::Volume => {
            Some(middle(g.panes.get(1).unwrap_or_else(|| panic!("{label}: no volume pane")).frame))
        }
        PointerAt::Macd => Some(middle(g.panes.last().expect("a bottom pane").frame)),
        PointerAt::OffScreen => Some(pos2(5000.0, 5000.0)),
        PointerAt::LeftEdge => {
            let tr = o.price_tr.as_ref().expect("the price transform");
            Some(pos2(tr.position_from_point_x(0.0).max(price.left() + 0.5), price.center().y))
        }
        PointerAt::TimeLabelX => {
            let l = g.x_labels.first().unwrap_or_else(|| panic!("{label}: no time label to hover"));
            Some(pos2(l.rect.center().x, price.center().y))
        }
    }
}

/// Run a cell: once to measure its panes, and again with the pointer parked where the cell says.
fn run_cell(cell: &Cell) -> Observed {
    let first = run_once(cell, None);
    match pointer_for(cell.pointer, &first, &cell.label()) {
        None => first,
        Some(p) => {
            let o = run_once(cell, Some(p));
            // A pointer cell that did not hover anything would test nothing: the time tag follows
            // the pointer's x wherever it is in the window, so it must exist for every in-window one.
            let timed = o.geom.chips.iter().any(|c| c.kind == ChipKind::Time);
            let want_time = !matches!(cell.pointer, PointerAt::OffScreen) && cell.bars > 0;
            assert_eq!(
                timed,
                want_time,
                "{}: the pointer cell hovered nothing: {:?}",
                cell.label(),
                o.geom.chips
            );
            o
        }
    }
}

fn run_all(cells: &[Cell]) -> Vec<Run> {
    cells.iter().map(|c| Run { cell: *c, obs: run_cell(c) }).collect()
}

// ============================ judging the runs ============================

struct Finding {
    cell: String,
    v: Violation,
}

/// Every assertion over every run, plus the cross-cell one (a taller price pane never shows fewer
/// ticks): cells that differ only by ppp are compared on their logical price-pane height.
fn findings(runs: &[Run]) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut groups: BTreeMap<String, Vec<(f32, usize)>> = BTreeMap::new();
    for run in runs {
        for v in violations(&run.obs.geom, &run.obs.expect) {
            out.push(Finding { cell: run.cell.label(), v });
        }
        let g = &run.obs.geom;
        groups
            .entry(run.cell.key())
            .or_default()
            .push((g.panes[0].frame.height(), g.y_labels[0].len()));
    }
    for (key, samples) in groups {
        for (h0, n0, h1, n1) in tick_count_inversions(&samples) {
            out.push(Finding {
                cell: key.clone(),
                v: Violation {
                    kind: Kind::TickCountInversion,
                    detail: format!(
                        "the price pane shows {n0} y labels at {h0:.1}pt but only {n1} at {h1:.1}pt"
                    ),
                    magnitude: (n0 - n1) as f32,
                },
            });
        }
    }
    out
}

/// The failure text: a per-kind headline (count, cells, worst), then every finding.
fn report(found: &[Finding]) -> String {
    let mut by_kind: BTreeMap<Kind, Vec<&Finding>> = BTreeMap::new();
    for f in found {
        by_kind.entry(f.v.kind).or_default().push(f);
    }
    let mut text = String::new();
    for (kind, fs) in &by_kind {
        let cells: std::collections::BTreeSet<&str> = fs.iter().map(|f| f.cell.as_str()).collect();
        let worst = fs.iter().max_by(|a, b| a.v.magnitude.total_cmp(&b.v.magnitude));
        text.push_str(&format!(
            "\n== {kind:?}: {} findings in {} cells; worst {:.1}pt at {}",
            fs.len(),
            cells.len(),
            worst.map_or(0.0, |w| w.v.magnitude),
            worst.map_or("-", |w| w.cell.as_str())
        ));
    }
    text.push('\n');
    for f in found {
        text.push_str(&format!("\n{} {:?}: {}", f.cell, f.v.kind, f.v.detail));
    }
    text
}

/// A matrix whose frames held nothing would pass every assertion: the cells must have painted
/// labels, and a real pane must carry price ticks. The per-pane half is asked only of an ordinary
/// frame (the default price, no pointer): a tag or chip legitimately takes the label under it, and a
/// pane with one label (a 305pt pane at 800x600 shows just `100.00`, its strongest tick) or none
/// but the one the crosshair covers is then label-free by the owner's ruling.
fn assert_not_vacuous(runs: &[Run]) {
    let y: usize =
        runs.iter().map(|r| r.obs.geom.y_labels.iter().map(Vec::len).sum::<usize>()).sum();
    assert!(y > 0, "no y tick label was read in any cell: the reader found nothing");
    let chips: usize = runs.iter().map(|r| r.obs.geom.chips.len()).sum();
    assert!(chips > 0, "no axis chip was read in any cell: the reader found nothing");
    for r in runs {
        let g = &r.obs.geom;
        let ordinary = r.cell.pointer == PointerAt::None && r.cell.price == WAVE_CENTRE;
        if ordinary && r.cell.bars > 1 && g.panes[0].frame.height() >= 100.0 {
            assert!(
                !g.y_labels[0].is_empty(),
                "{}: a {}pt price pane with no y labels",
                r.cell.label(),
                g.panes[0].frame.height()
            );
        }
    }
}

/// Every assertion over every run must be GREEN.
fn assert_clean(runs: &[Run]) {
    assert_not_vacuous(runs);
    let bad = findings(runs);
    assert!(bad.is_empty(), "{} findings:{}", bad.len(), report(&bad));
}

fn check_enforced(cells: Vec<Cell>) {
    assert_clean(&run_all(&cells));
}

// ============================ the cell lists ============================

fn size_cells(px: (u32, u32)) -> Vec<Cell> {
    PPPS.iter().flat_map(|&ppp| LAYOUTS.map(|l| Cell::new(px, ppp, l))).collect()
}

/// 10,000 bars (the whole series fits the default view, so the decimation path runs).
fn big_cells() -> Vec<Cell> {
    let mut cells = Vec::new();
    for px in [(1366, 768), (1920, 1080), (3840, 2160)] {
        for ppp in [1.0, 2.0] {
            cells.push(Cell { bars: 10_000, ..Cell::new(px, ppp, Layout::Panes) });
        }
    }
    cells
}

/// The pointer parked in every pane, off the window, at the left edge and on a time label — 3 sizes
/// x 2 ppp, plus the in-price-pane baseline.
fn pointer_cells() -> Vec<Cell> {
    let mut cells = Vec::new();
    for px in [(800, 600), (1366, 768), (1920, 1080)] {
        for ppp in [1.0, 2.0] {
            for pointer in [
                PointerAt::Price,
                PointerAt::Volume,
                PointerAt::Macd,
                PointerAt::OffScreen,
                PointerAt::LeftEdge,
                PointerAt::TimeLabelX,
            ] {
                cells.push(Cell { pointer, ..Cell::new(px, ppp, Layout::Panes) });
            }
        }
    }
    cells
}

/// Prices from 1e-5 to 750,000: how the y labels and the chips format and fit.
fn magnitude_cells() -> Vec<Cell> {
    let mut cells = Vec::new();
    for px in [(1366, 768), (1920, 1080)] {
        for ppp in [1.0, 2.0] {
            for price in [1.15, 80_000.0, 750_000.0, 1e-5] {
                cells.push(Cell { price, ..Cell::new(px, ppp, Layout::Panes) });
            }
        }
    }
    cells
}

/// A climbing series in the follow view and panned left (`Nav::PanLeft`): panned, the last bar and
/// its last close are off the right edge, and the close is above everything the view still shows.
fn pan_cells() -> Vec<Cell> {
    let mut cells = Vec::new();
    for px in [(1366, 768), (1920, 1080)] {
        for ppp in [1.0, 2.0] {
            for pan_left in [false, true] {
                cells.push(Cell { pan_left, climb: true, ..Cell::new(px, ppp, Layout::Panes) });
            }
        }
    }
    cells
}

/// Price + Volume + 4 and + 6 indicator panes (eight panes in all) in windows that fit every one at
/// its 44pt floor: the "timeline disappears with 4+ panes" shape. 1366x768 at ppp 2 (684x384pt) is
/// left out on purpose: it fits only six sub-panes, so the lowest study is dropped there.
fn many_pane_cells() -> Vec<Cell> {
    let mut cells = Vec::new();
    for (px, ppp) in
        [((1366, 768), 1.0), ((1920, 1080), 1.0), ((1920, 1080), 2.0), ((3840, 2160), 2.0)]
    {
        for studies in [4, 6] {
            cells.push(Cell::new(px, ppp, Layout::Studies(studies)));
        }
    }
    cells
}

/// One bar, two bars (indicators still warming up, a one-bar x range) and 100,000 bars (the
/// decimation path, ~1,700 hour marks handed to the time axis) at one size.
fn bar_count_cells() -> Vec<Cell> {
    [1, 2, 100_000].map(|bars| Cell { bars, ..Cell::new((1366, 768), 1.0, Layout::Panes) }).to_vec()
}

/// A window resized between frames on the same `Context`: three frames settle at `from`, the one
/// judged is the first at `to`. Every `to` fits all four panes, except the last pair's FIRST window
/// (640x360 at ppp 2 shows three panes), which grows into four.
fn resize_cells() -> Vec<Cell> {
    let at = |from, to, ppp| Cell { resize_to: Some(to), ..Cell::new(from, ppp, Layout::Panes) };
    vec![
        at((1920, 1080), (800, 600), 1.0),
        at((1920, 1080), (800, 600), 2.0),
        at((800, 600), (1920, 1080), 1.0),
        at((800, 600), (1920, 1080), 2.0),
        at((1366, 768), (640, 360), 1.0),
        at((1366, 768), (640, 360), 1.5),
        at((1366, 768), (1366, 420), 1.0),
        at((640, 360), (1366, 768), 1.5),
        at((640, 360), (1366, 768), 2.0),
    ]
}

/// The window the four-pane stack cannot fit: it drops its lowest pane.
fn tiny_cell() -> Cell {
    Cell::new((480, 200), 1.0, Layout::Panes)
}

macro_rules! sizes {
    ($($name:ident = $w:literal x $h:literal;)*) => {
        /// The nine sizes, in physical pixels.
        const SIZES: &[(u32, u32)] = &[$(($w, $h)),*];
        $(
            #[test]
            fn $name() {
                check_enforced(size_cells(($w, $h)));
            }
        )*
    };
}

sizes! {
    size_640x360 = 640 x 360;
    size_800x600 = 800 x 600;
    size_1280x720 = 1280 x 720;
    size_1366x768 = 1366 x 768;
    size_1920x1080 = 1920 x 1080;
    size_2560x1440 = 2560 x 1440;
    size_3440x1440 = 3440 x 1440;
    size_3840x2160 = 3840 x 2160;
    size_600x1000 = 600 x 1000;
}

/// Every cell of every family except the tiny one, for a ruling test to sweep.
fn all_cells() -> Vec<Cell> {
    let mut cells: Vec<Cell> = SIZES.iter().flat_map(|&px| size_cells(px)).collect();
    cells.extend(big_cells());
    cells.extend(pointer_cells());
    cells.extend(magnitude_cells());
    cells.extend(pan_cells());
    cells
}

// ============================ the extra families ============================

#[test]
fn ten_thousand_bars() {
    check_enforced(big_cells());
}

#[test]
fn pointer_variants() {
    check_enforced(pointer_cells());
}

#[test]
fn price_magnitudes() {
    check_enforced(magnitude_cells());
}

#[test]
fn pan_left() {
    check_enforced(pan_cells());
}

#[test]
fn a_window_too_short_for_four_panes_drops_the_lowest_and_stays_clean() {
    // 480x200: four 44pt-floored panes cannot fit. The MACD pane (the lowest) is dropped, the
    // other three tile the window, and every enforced assertion holds.
    // (Not `check_enforced`: its vacuity guard wants labels, and these 44pt panes are too short for
    // a tick label that no chip or edge takes.)
    let runs = run_all(&[tiny_cell()]);
    assert_eq!(runs[0].obs.geom.panes.len(), 3);
    let bad = findings(&runs);
    assert!(bad.is_empty(), "{} findings:{}", bad.len(), report(&bad));
}

/// The 640x360 window at ppp 2 (320x180pt) is where the matrix first measured the overflow: three
/// panes at ppp 2, all four at ppp 1 (360pt tall).
#[test]
fn a_short_window_drops_exactly_the_panes_that_do_not_fit() {
    let at = |ppp| run_all(&[Cell::new((640, 360), ppp, Layout::Panes)])[0].obs.geom.panes.len();
    assert_eq!((at(1.0), at(1.25), at(1.5), at(2.0)), (4, 4, 4, 3));
}

/// The "timeline disappears with 4+ panes" shape (`chart/mod.rs`'s `draw`: egui's implicit
/// `item_spacing.y` leaked ~4pt per sub-pane until the bottom pane's time axis slid under the status
/// bar): four and six indicator panes under Volume keep every pane at its floor, the time axis in
/// the window and every assertion green.
#[test]
fn four_and_six_indicator_panes_keep_every_pane_and_the_time_axis() {
    let runs = run_all(&many_pane_cells());
    for r in &runs {
        let (g, label) = (&r.obs.geom, r.cell.label());
        assert_eq!(g.panes.len(), 2 + study_count(r.cell.layout), "{label}: a pane was dropped");
        assert!(!g.x_labels.is_empty(), "{label}: the time axis painted no label");
        for p in &g.panes {
            let h = p.frame.height();
            assert!(h >= chart::MIN_PANE_PX - 0.5, "{label}: pane {} is {h}pt tall", p.name);
        }
    }
    assert_clean(&runs);
}

/// A one-bar and a two-bar series (warming-up indicators, a degenerate x range) and a 100,000-bar
/// one at 1366x768. Every frame the harness renders also runs `vike_ui_theme::frame_sanity`, so a
/// NaN or infinite coordinate fails here too.
#[test]
fn one_two_and_a_hundred_thousand_bars() {
    assert_clean(&run_all(&bar_count_cells()));
}

/// The window is resized between frames on one `Context` (the plots keep their remembered frames
/// and bounds): the FIRST frame at the new size must already be clean and finite, which a guard
/// that learned its pixel scale one frame late, or a pane stack sized from last frame's height,
/// would fail.
#[test]
fn a_window_resized_between_frames_is_clean_on_the_first_frame_at_the_new_size() {
    let runs = run_all(&resize_cells());
    for r in &runs {
        assert_eq!(r.obs.geom.panes.len(), 4, "{}: a pane was dropped", r.cell.label());
    }
    assert_clean(&runs);
}

#[test]
fn the_size_list_is_the_designed_nine() {
    assert_eq!(SIZES.len(), 9);
    // 9 sizes x 4 ppp x 5 layouts, 10,000 bars (3 x 2), pointers (3 x 2 x 6), magnitudes (2 x 2 x 4),
    // pans (2 x 2 x 2).
    assert_eq!(all_cells().len(), 9 * 4 * 5 + 6 + 36 + 16 + 8);
}

// ============================ the regression test for the crosshair tag ============================

/// The crosshair price tag follows a pointer inside the PRICE pane only: parked in Volume or MACD
/// (or off the window) it must not be painted at all — it used to land in the price gutter at the
/// pointer's y, below the price pane, on top of the Volume ticks (`home2.png`).
#[test]
fn the_crosshair_price_tag_is_painted_only_for_a_pointer_in_the_price_pane() {
    let mut checked = 0;
    for px in [(800, 600), (1366, 768), (1920, 1080)] {
        for ppp in [1.0, 2.0] {
            for pointer in [PointerAt::Volume, PointerAt::Macd, PointerAt::OffScreen] {
                let cell = Cell { pointer, ..Cell::new(px, ppp, Layout::Panes) };
                let o = run_cell(&cell);
                let tags: Vec<_> =
                    o.geom.chips.iter().filter(|c| c.kind == ChipKind::CrosshairPrice).collect();
                assert!(
                    tags.is_empty(),
                    "{}: a crosshair price tag outside the price pane: {tags:?}",
                    cell.label()
                );
                checked += 1;
            }
            let cell = Cell { pointer: PointerAt::Price, ..Cell::new(px, ppp, Layout::Panes) };
            let o = run_cell(&cell);
            let price = o.geom.panes[0].frame;
            let tags: Vec<_> =
                o.geom.chips.iter().filter(|c| c.kind == ChipKind::CrosshairPrice).collect();
            assert_eq!(
                tags.len(),
                1,
                "{}: the price pane's own pointer lost its tag",
                cell.label()
            );
            assert!(
                price.y_range().contains(tags[0].rect.center().y),
                "{}: {:?}",
                cell.label(),
                tags[0]
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 24);
}
