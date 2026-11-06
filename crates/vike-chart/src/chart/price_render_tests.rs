use super::*;
use crate::chart::SeriesInput;
use crate::model::ChartState;

/// A closed [`ChartState`] over `closes` on a fixed 1-minute ot grid starting at `base`.
/// `l`/`h` bracket each close by ±1.0 so the visible OHLC extent is distinguishable from
/// the close line.
fn st(base: i64, closes: &[f64]) -> ChartState {
    let mut s = ChartState::default();
    s.bars = closes
        .iter()
        .enumerate()
        .map(|(i, &c)| Bar {
            t: i as f64,
            ot: base + i as i64 * 60_000,
            o: c,
            h: c + 1.0,
            l: c - 1.0,
            c,
            v: 1.0,
        })
        .collect();
    s.closed_len = closes.len();
    s.refresh_caches();
    s
}

fn scale_map(sym: &str, a: ScaleAssign) -> IndexMap<String, ScaleAssign> {
    let mut m = IndexMap::new();
    m.insert(sym.to_string(), a);
    m
}

/// Linear mode: a SharedLinear compare's line = its ABSOLUTE closes (map is identity),
/// reindexed by ot onto the primary's index domain; raw_ext = the compare's visible
/// low/high (NOT the primary's).
#[test]
fn shared_axis_linear_maps_absolute_closes_and_reports_raw_extent() {
    let prim = st(1_700_000_000_000, &[100.0, 101.0, 102.0, 103.0]);
    // Same ot grid so reindex is identity; a HIGHER magnitude so the extent clearly
    // expands beyond the primary's [99, 104].
    let cmp = st(1_700_000_000_000, &[200.0, 205.0, 210.0, 208.0]);
    let overlays = [SeriesInput { symbol: "CMP", state: &cmp, color: Color32::RED }];
    let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
    let scales = scale_map("CMP", ScaleAssign::SharedLinear);
    let map = |y: f64| y; // Linear identity
    let mut legend = Vec::new();
    let out = compute_shared_axis_lines(
        &prim.bars,
        &overlays,
        &empty_panes,
        &scales,
        0.0,
        3.0,
        ScaleMode::Linear,
        &map,
        &mut legend,
    );
    assert_eq!(out.lines.len(), 1, "one shared line");
    let (color, pvs, base) = &out.lines[0];
    assert_eq!(*color, Color32::RED);
    assert_eq!(*base, 0);
    // absolute closes, mapped (identity)
    assert_eq!(pvs, &vec![200.0, 205.0, 210.0, 208.0]);
    // raw_ext = compare visible low (200-1) .. high (210+1)
    assert_eq!(out.raw_ext, Some((199.0, 211.0)));
    // legend carries the absolute LAST close (208.0) as a PRICE (is_pct=false)
    assert_eq!(legend, vec![(Color32::RED, "CMP".to_string(), 208.0, false)]);
}

/// Log mode: the line values are `log10(close)`, and a non-positive low is excluded from
/// the raw extent so it can never inject a NaN into the primary's autofit bounds.
#[test]
fn shared_axis_log_uses_log10_and_drops_nonpositive_low_from_extent() {
    let prim = st(1_700_000_000_000, &[100.0, 100.0, 100.0]);
    // A compare whose FIRST bar's low would be <= 0 (c=0.5 → l=-0.5): its mapped low is
    // NaN, so it must be dropped from raw_ext; its close line still plots log10(0.5).
    let cmp = st(1_700_000_000_000, &[0.5, 10.0, 100.0]);
    let overlays = [SeriesInput { symbol: "CMP", state: &cmp, color: Color32::GREEN }];
    let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
    let scales = scale_map("CMP", ScaleAssign::SharedLinear);
    let map = |y: f64| y.log10();
    let mut legend = Vec::new();
    let out = compute_shared_axis_lines(
        &prim.bars,
        &overlays,
        &empty_panes,
        &scales,
        0.0,
        2.0,
        ScaleMode::Log,
        &map,
        &mut legend,
    );
    let (_, pvs, _) = &out.lines[0];
    // closes mapped through log10 (bar 0 close = 0.5 → negative, still finite)
    assert!((pvs[0] - 0.5_f64.log10()).abs() < 1e-12);
    assert!((pvs[2] - 100.0_f64.log10()).abs() < 1e-12);
    // raw_ext excludes bar 0 (l=-0.5 maps to NaN); bars 1&2 give l in [9, 99], h in [11, 101]
    let (lo, hi) = out.raw_ext.unwrap();
    assert_eq!((lo, hi), (9.0, 101.0));
    assert!(lo.is_finite() && hi.is_finite());
}

/// Percent/Indexed primary mode: a SharedLinear compare renders NOTHING (absolute price
/// has no mapping onto a rebased axis) — empty lines, no extent, no legend.
#[test]
fn shared_axis_is_noop_in_percent_and_indexed_mode() {
    let prim = st(1_700_000_000_000, &[100.0, 101.0]);
    let cmp = st(1_700_000_000_000, &[200.0, 205.0]);
    let overlays = [SeriesInput { symbol: "CMP", state: &cmp, color: Color32::RED }];
    let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
    let scales = scale_map("CMP", ScaleAssign::SharedLinear);
    let map = |y: f64| y;
    for mode in [ScaleMode::Percent, ScaleMode::Indexed] {
        let mut legend = Vec::new();
        let out = compute_shared_axis_lines(
            &prim.bars,
            &overlays,
            &empty_panes,
            &scales,
            0.0,
            1.0,
            mode,
            &map,
            &mut legend,
        );
        assert!(out.lines.is_empty(), "{mode:?}: no shared line");
        assert_eq!(out.raw_ext, None);
        assert!(legend.is_empty());
    }
}

/// Non-SharedLinear pins (the default Percent, plus Right) are skipped by the shared path,
/// and a symbol moved to its own pane is skipped too ⇒ empty result (byte-identical default).
#[test]
fn shared_axis_skips_non_sharedlinear_and_own_pane_symbols() {
    let prim = st(1_700_000_000_000, &[100.0, 101.0]);
    let cmp = st(1_700_000_000_000, &[200.0, 205.0]);
    let overlays = [SeriesInput { symbol: "CMP", state: &cmp, color: Color32::RED }];
    let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
    let map = |y: f64| y;
    // default (absent) ⇒ Percent, Right ⇒ secondary — both skipped here.
    for a in [ScaleAssign::Percent, ScaleAssign::Right, ScaleAssign::Left] {
        let scales = scale_map("CMP", a);
        let mut legend = Vec::new();
        let out = compute_shared_axis_lines(
            &prim.bars,
            &overlays,
            &empty_panes,
            &scales,
            0.0,
            1.0,
            ScaleMode::Linear,
            &map,
            &mut legend,
        );
        assert!(out.lines.is_empty(), "{a:?} must not render on the shared axis");
        assert_eq!(out.raw_ext, None);
    }
    // Even a SharedLinear pin is skipped when the symbol has its own pane.
    let mut own_pane: IndexMap<String, PaneKey> = IndexMap::new();
    own_pane.insert("CMP".to_string(), PaneKey::Series(0));
    let scales = scale_map("CMP", ScaleAssign::SharedLinear);
    let mut legend = Vec::new();
    let out = compute_shared_axis_lines(
        &prim.bars,
        &overlays,
        &own_pane,
        &scales,
        0.0,
        1.0,
        ScaleMode::Linear,
        &map,
        &mut legend,
    );
    assert!(out.lines.is_empty(), "own-pane symbol is not a price-pane shared overlay");
}

/// Empty overlays ⇒ empty result (the default at every non-compare call site).
#[test]
fn shared_axis_empty_overlays_is_noop() {
    let prim = st(1_700_000_000_000, &[100.0, 101.0]);
    let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
    let empty_scales: IndexMap<String, ScaleAssign> = IndexMap::new();
    let map = |y: f64| y;
    let mut legend = Vec::new();
    let out = compute_shared_axis_lines(
        &prim.bars,
        &[],
        &empty_panes,
        &empty_scales,
        0.0,
        1.0,
        ScaleMode::Linear,
        &map,
        &mut legend,
    );
    assert!(out.lines.is_empty());
    assert_eq!(out.raw_ext, None);
    assert!(legend.is_empty());
}

/// The secondary-axis path must NOT emit a line for a SharedLinear symbol (the skip
/// tightening) — else the compare would double-render on both the primary and a secondary
/// axis. A Right pin on a DIFFERENT symbol still renders, proving the path still works.
#[test]
fn secondary_axis_skips_sharedlinear_but_still_handles_right() {
    let prim = st(1_700_000_000_000, &[100.0, 101.0, 102.0]);
    let cmp_shared = st(1_700_000_000_000, &[200.0, 205.0, 210.0]);
    let cmp_right = st(1_700_000_000_000, &[50.0, 55.0, 60.0]);
    let overlays = [
        SeriesInput { symbol: "SHARED", state: &cmp_shared, color: Color32::RED },
        SeriesInput { symbol: "RIGHT", state: &cmp_right, color: Color32::BLUE },
    ];
    let empty_panes: IndexMap<String, PaneKey> = IndexMap::new();
    let mut scales: IndexMap<String, ScaleAssign> = IndexMap::new();
    scales.insert("SHARED".to_string(), ScaleAssign::SharedLinear);
    scales.insert("RIGHT".to_string(), ScaleAssign::Right);
    let mut legend = Vec::new();
    let sec = Cell::new((0.0, 0.0, 0.0, 0.0));
    let out = compute_secondary_axis_lines(
        &prim.bars,
        &overlays,
        &empty_panes,
        &scales,
        0.0,
        2.0,
        0.0,
        10.0,
        &mut legend,
        &sec,
    );
    // exactly ONE secondary line — the Right symbol; SHARED (SharedLinear) is absent.
    assert_eq!(out.len(), 1, "only the Right pin rides the secondary axis");
    assert_eq!(out[0].0, Color32::BLUE);
    assert!(legend.iter().all(|(_, s, _, _)| s == "RIGHT"));
}
