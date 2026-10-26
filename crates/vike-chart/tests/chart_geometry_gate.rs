//! The kill proofs for `tests/common/geometry.rs` — the reader and the checkers the size / DPI matrix
//! (`tests/chart_sizes.rs`) judges every frame with. A matrix that has not been shown to go red on
//! the defects it exists for is decoration (the standard `tests/frame_sanity_gate.rs` sets): each
//! checker below is handed a PLANTED defect and must fail on it, and a clean layout must pass — with
//! the leaf counts asserted, because an empty frame passes every checker trivially.
//!
//! The reader is proved on REAL galleys (a `Shape::Text` laid out by a real `Context`, the same
//! `TextShape` egui_plot emits), the checkers on hand-built rects, and the assembled `violations`
//! on the clean layout with one field of its `Geometry` bent at a time.

mod common;

use common::geometry::{
    Chip, ChipKind, Expect, Geometry, Kind, Pane, TOL, cross_overlaps, duplicate_strings,
    gap_errors, outside_rect, outside_y_band, overlapping_pairs, read_geometry, spread,
    tick_count_inversions, violations,
};
use egui::epaint::{ClippedShape, Shape};
use egui::{Color32, Pos2, Rect, Vec2, pos2, vec2};

const TAG_BG: Color32 = Color32::from_rgb(40, 40, 60);
/// The window: 972 wide (900 data + 72 gutter), the stack ends at 402 and the 18pt time axis under
/// it ends at 420.
const SCREEN: Rect = Rect { min: Pos2 { x: 0.0, y: 0.0 }, max: Pos2 { x: 972.0, y: 420.0 } };

fn r(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::from_min_max(pos2(x0, y0), pos2(x1, y1))
}

fn panes() -> Vec<Pane> {
    vec![
        Pane { name: "price".to_owned(), frame: r(0.0, 0.0, 900.0, 300.0) },
        Pane { name: "volume".to_owned(), frame: r(0.0, 305.0, 900.0, 402.0) },
    ]
}

fn kinds(v: &[common::geometry::Violation]) -> Vec<Kind> {
    let mut k: Vec<Kind> = v.iter().map(|x| x.kind).collect();
    k.sort();
    k.dedup();
    k
}

// ============================ planting real shapes ============================

/// A REAL laid-out galley (see `frame_sanity_gate.rs`'s `galley`): fonts exist only inside a pass.
fn galley(ctx: &egui::Context, text: &str) -> std::sync::Arc<egui::Galley> {
    let raw = egui::RawInput { screen_rect: Some(SCREEN), time: Some(0.0), ..Default::default() };
    let mut g = None;
    let mut out = ctx.run_ui(raw, |ui| {
        g = Some(ui.painter().layout_no_wrap(
            text.to_owned(),
            egui::FontId::monospace(10.0),
            Color32::WHITE,
        ));
    });
    out.textures_delta.clear();
    g.expect("the pass runs its closure exactly once")
}

/// The shapes of one planted frame, built in the order `draw` paints them.
struct Plant {
    ctx: egui::Context,
    shapes: Vec<Shape>,
}

impl Plant {
    fn new() -> Self {
        Plant { ctx: egui::Context::default(), shapes: Vec::new() }
    }

    /// A tick label whose tick sits at `center_y`, x as egui_plot places a right axis' label.
    fn y_label(&mut self, text: &str, right: f32, center_y: f32) {
        let g = galley(&self.ctx, text);
        let pos = pos2(right + 4.0, center_y - g.size().y / 2.0);
        self.shapes.push(Shape::galley(pos, g, Color32::WHITE));
    }

    /// A time label centred on `cx` directly under the bottom pane.
    fn x_label(&mut self, text: &str, cx: f32, bottom: f32) {
        let g = galley(&self.ctx, text);
        let pos = pos2(cx - g.size().x / 2.0, bottom);
        self.shapes.push(Shape::galley(pos, g, Color32::WHITE));
    }

    /// An axis chip as `overlay.rs` paints one: a radius-3 rect, then its text at `min + (6, 2)`.
    fn chip(&mut self, text: &str, min: Pos2, fill: Color32) -> Rect {
        let g = galley(&self.ctx, text);
        let rect = Rect::from_min_size(min, g.size() + vec2(12.0, 4.0));
        self.shapes.push(Shape::rect_filled(rect, 3.0, fill));
        self.shapes.push(Shape::galley(min + vec2(6.0, 2.0), g, Color32::WHITE));
        rect
    }

    fn read(self, panes: Vec<Pane>, last_chip: bool) -> Geometry {
        let out = egui::FullOutput {
            shapes: self
                .shapes
                .into_iter()
                .map(|shape| ClippedShape { clip_rect: SCREEN, shape })
                .collect(),
            pixels_per_point: 1.0,
            ..Default::default()
        };
        read_geometry(&out, SCREEN, panes, TAG_BG, last_chip)
    }
}

/// A frame that breaks no rule: price ticks 100/200, a volume tick 50, time labels 10:00/11:00, the
/// last-price chip between two price ticks, a volume value tag clear of the volume tick, a crosshair
/// price tag inside the price pane and a time tag inside the window. Returns the geometry and the
/// `Expect` that places the last close under the last-price chip.
fn clean() -> (Geometry, Expect) {
    let mut p = Plant::new();
    p.y_label("100.00", 900.0, 100.0);
    p.y_label("200.00", 900.0, 200.0);
    p.y_label("50", 900.0, 350.0);
    p.x_label("10:00", 300.0, 402.0);
    p.x_label("11:00", 600.0, 402.0);
    let last = p.chip("150.00", pos2(901.0, 140.0), Color32::GREEN);
    p.chip("180.00", pos2(901.0, 230.0), TAG_BG);
    p.chip("7", pos2(901.0, 360.0), Color32::GREEN);
    p.chip("10-24 17:52", pos2(400.0, 403.0), TAG_BG);
    (p.read(panes(), true), Expect { last_close_y: Some(last.center().y) })
}

// ============================ the reader ============================

#[test]
fn the_reader_finds_every_leaf_of_a_clean_frame() {
    // Guards the failure that would make every test below meaningless: a reader that finds nothing
    // judges nothing. Counts, not just "no violations".
    let (g, _) = clean();
    assert_eq!(g.panes.len(), 2);
    assert_eq!(
        g.y_labels.iter().map(Vec::len).collect::<Vec<_>>(),
        [2, 1],
        "price carries two y labels, volume one"
    );
    assert_eq!(g.x_labels.len(), 2);
    let by_kind = |k: ChipKind| g.chips.iter().filter(|c| c.kind == k).count();
    assert_eq!(by_kind(ChipKind::LastPrice), 1, "{:?}", g.chips);
    assert_eq!(by_kind(ChipKind::CrosshairPrice), 1);
    assert_eq!(by_kind(ChipKind::Value), 1);
    assert_eq!(by_kind(ChipKind::Time), 1);
    // The chip texts were paired with their rects, and kept OUT of the tick labels.
    let text_of = |k: ChipKind| g.chips.iter().find(|c| c.kind == k).map(|c| c.text.as_str());
    assert_eq!(text_of(ChipKind::LastPrice), Some("150.00"));
    assert_eq!(text_of(ChipKind::CrosshairPrice), Some("180.00"));
    assert_eq!(text_of(ChipKind::Value), Some("7"));
    assert_eq!(text_of(ChipKind::Time), Some("10-24 17:52"));
}

#[test]
fn a_clean_frame_passes_every_check() {
    let (g, ex) = clean();
    let v = violations(&g, &ex);
    assert!(v.is_empty(), "a clean frame must pass: {v:#?}");
}

#[test]
fn a_label_on_the_pane_boundary_belongs_to_the_pane_whose_tick_it_is() {
    // The Volume `0` tick sits ON the volume pane's bottom edge (y = 402): it must be read as a
    // VOLUME label (so the out-of-pane check can see it), not handed to a neighbour.
    let mut p = Plant::new();
    p.y_label("0", 900.0, 402.0);
    p.y_label("100.00", 900.0, 0.0);
    let g = p.read(panes(), false);
    assert_eq!(g.y_labels[1].len(), 1, "the 0 tick belongs to volume: {:?}", g.y_labels);
    assert_eq!(g.y_labels[1][0].text, "0");
    assert_eq!(g.y_labels[0].len(), 1);
}

#[test]
fn a_time_label_painted_past_the_window_edge_is_still_a_time_label() {
    // The shared x spacer hands egui_plot every hour mark, in view or not: a label for a mark past
    // the right edge lands beside the gutter, where a y label would — but under the bottom pane.
    let mut p = Plant::new();
    p.x_label("02:00", 1100.0, 402.0);
    p.y_label("100.00", 900.0, 100.0);
    let g = p.read(panes(), false);
    assert_eq!(g.x_labels.len(), 1, "{:?}", g.x_labels);
    assert_eq!(g.x_labels[0].text, "02:00");
    assert_eq!(g.y_labels[0].len(), 1);
    assert!(g.y_labels[1].is_empty(), "the 02:00 label was taken for a volume tick");
}

#[test]
fn a_gutter_chip_in_the_tag_fill_is_the_crosshair_tag_wherever_it_sits() {
    // The home2 defect: a crosshair price tag painted in the VOLUME pane's gutter. The reader must
    // still call it a crosshair tag (by its fill), or the misplacement check could never see it.
    let mut p = Plant::new();
    p.chip("78,946.02", pos2(901.0, 340.0), TAG_BG);
    let g = p.read(panes(), false);
    assert_eq!(g.chips.len(), 1);
    assert_eq!(g.chips[0].kind, ChipKind::CrosshairPrice);
    assert_eq!(g.chips[0].text, "78,946.02");
}

#[test]
fn a_rect_that_is_not_an_axis_chip_is_not_read_as_one() {
    // A corner-radius-3 rect that sits nowhere near a gutter or the time axis (a nav button, say)
    // must not become a chip; neither may a chip-looking rect with the wrong radius.
    let mut p = Plant::new();
    p.shapes.push(Shape::rect_filled(r(100.0, 100.0, 140.0, 120.0), 3.0, Color32::RED));
    p.shapes.push(Shape::rect_filled(r(901.0, 100.0, 960.0, 118.0), 6.0, Color32::RED));
    let g = p.read(panes(), false);
    assert!(g.chips.is_empty(), "{:?}", g.chips);
}

// ============================ the pure checkers ============================

#[test]
fn overlapping_pairs_flags_a_planted_overlap_and_passes_a_clean_column() {
    // Two 10pt-tall labels whose boxes share 4pt vertically: the planted defect.
    let planted = [r(904.0, 100.0, 950.0, 110.0), r(904.0, 106.0, 950.0, 116.0)];
    let hit = overlapping_pairs(&planted);
    assert_eq!(hit.len(), 1);
    assert_eq!((hit[0].0, hit[0].1), (0, 1));
    assert!((hit[0].2.x - 46.0).abs() < 1e-3 && (hit[0].2.y - 4.0).abs() < 1e-3, "{hit:?}");
    // A clean column: separated, and merely touching (a 0pt overlap is not an overlap).
    let clean = [
        r(904.0, 100.0, 950.0, 110.0),
        r(904.0, 110.0, 950.0, 120.0),
        r(904.0, 140.0, 950.0, 150.0),
    ];
    assert!(overlapping_pairs(&clean).is_empty());
    // Overlap on ONE axis only (side by side) is not an overlap.
    let beside = [r(0.0, 0.0, 10.0, 10.0), r(20.0, 2.0, 30.0, 12.0)];
    assert!(overlapping_pairs(&beside).is_empty());
}

#[test]
fn cross_overlaps_flags_a_chip_planted_over_a_label() {
    let chips = [r(901.0, 95.0, 960.0, 113.0)];
    let labels = [r(904.0, 100.0, 950.0, 110.0), r(904.0, 200.0, 950.0, 210.0)];
    let hit = cross_overlaps(&chips, &labels);
    assert_eq!(hit.len(), 1);
    assert_eq!((hit[0].0, hit[0].1), (0, 0));
    let clear = [r(901.0, 120.0, 960.0, 138.0)];
    assert!(cross_overlaps(&clear, &labels).is_empty());
}

#[test]
fn duplicate_strings_flags_a_repeated_text_once() {
    assert_eq!(duplicate_strings(["0.00", "0.00", "0.00", "0.01"]), ["0.00"]);
    assert!(duplicate_strings(["1", "2", "3"]).is_empty());
    assert!(duplicate_strings(std::iter::empty::<&str>()).is_empty());
}

#[test]
fn outside_y_band_flags_a_label_planted_half_out_of_its_pane() {
    // A tick label centred on the pane's bottom edge (y = 402): its lower half is outside.
    let planted = [r(904.0, 397.0, 912.0, 407.0)];
    let hit = outside_y_band(&planted, 305.0, 402.0);
    assert_eq!(hit.len(), 1);
    assert!((hit[0].1 - 5.0).abs() < 1e-3, "{hit:?}");
    // Above the top edge is just as outside.
    assert_eq!(outside_y_band(&[r(904.0, 300.0, 912.0, 310.0)], 305.0, 402.0).len(), 1);
    // Inside, and inside-within-slack, pass.
    let inside = [r(904.0, 310.0, 912.0, 320.0), r(904.0, 392.0, 912.0, 402.0 + TOL * 0.5)];
    assert!(outside_y_band(&inside, 305.0, 402.0).is_empty());
}

#[test]
fn outside_rect_flags_a_chip_planted_across_the_window_edge() {
    let window = r(0.0, 0.0, 972.0, 420.0);
    // Right edge cut (a wide price chip), left edge cut (a time tag at the left edge), bottom cut.
    let planted = [
        r(901.0, 100.0, 980.0, 118.0),
        r(-20.0, 403.0, 80.0, 419.0),
        r(400.0, 410.0, 500.0, 428.0),
    ];
    let hit = outside_rect(&planted, window);
    assert_eq!(hit.iter().map(|h| h.0).collect::<Vec<_>>(), [0, 1, 2]);
    assert!(
        (hit[0].1 - 8.0).abs() < 1e-3
            && (hit[1].1 - 20.0).abs() < 1e-3
            && (hit[2].1 - 8.0).abs() < 1e-3
    );
    assert!(outside_rect(&[r(901.0, 100.0, 960.0, 118.0), window], window).is_empty());
}

#[test]
fn spread_and_gap_errors_and_tick_inversions_flag_their_planted_defects() {
    assert!(spread(&[900.0, 900.2, 899.9]) <= TOL);
    assert!((spread(&[900.0, 872.0]) - 28.0).abs() < 1e-3, "a pane 28pt narrower");
    assert_eq!(spread(&[900.0]), 0.0);

    let stack =
        [r(0.0, 0.0, 900.0, 300.0), r(0.0, 305.0, 900.0, 400.0), r(0.0, 405.0, 900.0, 500.0)];
    assert!(gap_errors(&stack, 5.0).is_empty());
    let torn =
        [r(0.0, 0.0, 900.0, 300.0), r(0.0, 311.0, 900.0, 400.0), r(0.0, 405.0, 900.0, 500.0)];
    let hit = gap_errors(&torn, 5.0);
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].0, 0);
    assert!((hit[0].1 - 6.0).abs() < 1e-3);

    // 600pt shows 9 labels, 400pt shows 11: a taller pane with FEWER ticks.
    let inverted = [(400.0, 11), (600.0, 9), (200.0, 5)];
    assert_eq!(tick_count_inversions(&inverted), [(400.0, 11, 600.0, 9)]);
    assert!(tick_count_inversions(&[(200.0, 5), (400.0, 7), (400.4, 7), (600.0, 7)]).is_empty());
}

// ============================ the assembled checks, one defect at a time ============================

fn with(f: impl FnOnce(&mut Geometry, &mut Expect)) -> Vec<Kind> {
    let (mut g, mut ex) = clean();
    f(&mut g, &mut ex);
    kinds(&violations(&g, &ex))
}

fn chip_of(g: &mut Geometry, k: ChipKind) -> &mut Chip {
    g.chips.iter_mut().find(|c| c.kind == k).expect("the clean frame has every chip kind")
}

#[test]
fn two_overlapping_tick_labels_redden_label_overlap() {
    let k = with(|g, _| {
        // A second label 2pt below the first: their boxes share most of their height.
        let mut shifted = g.y_labels[0][0].clone();
        shifted.rect = shifted.rect.translate(vec2(0.0, 2.0));
        shifted.text = "100.50".to_owned();
        g.y_labels[0].push(shifted);
    });
    assert_eq!(k, [Kind::LabelOverlap]);
}

#[test]
fn the_same_tick_text_twice_reddens_duplicate_label() {
    let k = with(|g, _| {
        // The first label's text again, 60pt further down (clear of every chip and label).
        let mut far = g.y_labels[0][1].clone();
        far.rect = far.rect.translate(vec2(0.0, 60.0));
        far.text = g.y_labels[0][0].text.clone();
        g.y_labels[0].push(far);
    });
    assert_eq!(k, [Kind::DuplicateLabel]);
}

#[test]
fn a_chip_over_a_tick_label_reddens_chip_over_label() {
    let k = with(|g, _| {
        // Slide the volume value tag up onto the volume tick label.
        let target = g.y_labels[1][0].rect.center();
        let c = chip_of(g, ChipKind::Value);
        c.rect = Rect::from_center_size(target, c.rect.size());
    });
    assert_eq!(k, [Kind::ChipOverLabel]);
}

#[test]
fn a_label_half_outside_its_pane_reddens_label_outside_pane() {
    let k = with(|g, _| {
        // The volume tick label dragged down onto the pane's bottom edge.
        let edge = g.panes[1].frame.bottom();
        let l = &mut g.y_labels[1][0];
        l.rect = Rect::from_center_size(pos2(l.rect.center().x, edge), l.rect.size());
    });
    assert_eq!(k, [Kind::LabelOutsidePane]);
}

#[test]
fn a_crosshair_price_tag_in_another_pane_reddens_crosshair_tag_misplaced() {
    // The home2 defect: the tag in the VOLUME pane's gutter, below the price frame.
    let k = with(|g, _| {
        let c = chip_of(g, ChipKind::CrosshairPrice);
        c.rect = c.rect.translate(vec2(0.0, 70.0)); // y ~300..317: in volume, clear of its tick
    });
    assert_eq!(k, [Kind::CrosshairTagMisplaced]);
    // ...and one parked off the window altogether (the pointer at (5000, 5000)).
    let off = with(|g, _| {
        let c = chip_of(g, ChipKind::CrosshairPrice);
        c.rect = c.rect.translate(vec2(0.0, 4700.0));
    });
    assert!(off.contains(&Kind::CrosshairTagMisplaced), "{off:?}");
}

#[test]
fn a_time_tag_past_the_window_edge_reddens_time_tag_outside_window() {
    let k = with(|g, _| {
        let c = chip_of(g, ChipKind::Time);
        c.rect = c.rect.translate(vec2(-420.0, 0.0)); // the tag centred on x = 0
    });
    assert_eq!(k, [Kind::TimeTagOutsideWindow]);
}

#[test]
fn a_last_price_chip_off_its_pane_or_off_its_close_reddens_last_chip_outside_pane() {
    // Panned: the last close is far below the price pane and the chip followed it.
    let panned = with(|g, ex| {
        let c = chip_of(g, ChipKind::LastPrice);
        c.rect = c.rect.translate(vec2(0.0, 250.0));
        ex.last_close_y = Some(c.rect.center().y);
    });
    assert_eq!(panned, [Kind::LastChipOutsidePane]);
    // In the pane but 12pt from the close it should mark.
    let drifted = with(|g, ex| {
        ex.last_close_y = Some(chip_of(g, ChipKind::LastPrice).rect.center().y + 12.0);
    });
    assert_eq!(drifted, [Kind::LastChipOutsidePane]);
    // A frame that has bars but painted no chip at all.
    let missing = with(|g, _| g.chips.retain(|c| c.kind != ChipKind::LastPrice));
    assert_eq!(missing, [Kind::LastChipOutsidePane]);
    // No last close expected (an empty chart): nothing to say.
    let empty = with(|g, ex| {
        g.chips.retain(|c| c.kind != ChipKind::LastPrice);
        ex.last_close_y = None;
    });
    assert!(empty.is_empty(), "{empty:?}");
}

#[test]
fn a_price_chip_cut_by_the_window_reddens_chip_outside_window() {
    let k = with(|g, _| {
        let c = chip_of(g, ChipKind::Value);
        c.rect = Rect::from_min_size(c.rect.min, Vec2::new(90.0, c.rect.height())); // 901+90 > 972
    });
    assert_eq!(k, [Kind::ChipOutsideWindow]);
}

#[test]
fn a_time_label_cut_by_the_window_edge_reddens_time_label_cut() {
    // Centred on x = 0: half of it is in the window, half is not.
    let cut = with(|g, _| {
        let l = &mut g.x_labels[0];
        l.rect = Rect::from_center_size(pos2(0.0, l.rect.center().y), l.rect.size());
    });
    assert_eq!(cut, [Kind::TimeLabelCut]);
    // Wholly past the edge it is clipped away, not cut: nothing to see, nothing to say.
    let gone = with(|g, _| {
        let l = &mut g.x_labels[0];
        l.rect = Rect::from_center_size(pos2(-2000.0, l.rect.center().y), l.rect.size());
    });
    assert!(gone.is_empty(), "{gone:?}");
}

#[test]
fn panes_with_different_right_edges_redden_pane_right_edges() {
    let k = with(|g, _| g.panes[1].frame.max.x -= 28.0);
    assert_eq!(k, [Kind::PaneRightEdges]);
}

#[test]
fn a_torn_stack_or_off_screen_time_labels_redden_pane_stack() {
    // A 9pt gap where the 5pt separator belongs.
    let torn = with(|g, _| {
        g.panes[1].frame.min.y += 4.0;
    });
    assert_eq!(torn, [Kind::PaneStack]);
    // The stack running past the window: the panes' floors overflowing a short window.
    let overflow = with(|g, _| {
        g.screen.max.y = 300.0;
    });
    assert!(overflow.contains(&Kind::PaneStack), "{overflow:?}");
    // A window TALLER than the stack is not a defect: `draw` keeps its time-axis reserve clear.
    let roomy = with(|g, _| g.screen.max.y += 14.0);
    assert!(roomy.is_empty(), "{roomy:?}");
    // Time labels below the window while the stack itself tiles.
    let off = with(|g, _| {
        let l = &mut g.x_labels[0];
        l.rect = l.rect.translate(vec2(0.0, 30.0));
    });
    assert_eq!(off, [Kind::PaneStack]);
}
