use super::*;
use crate::model::Bar;
use egui::Color32;

const AVAIL: f32 = 600.0;

fn wave(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let base = 100.0 + (i as f64 * 0.30).sin() * 6.0;
            Bar {
                t: i as f64,
                ot: 1_700_000_000_000 + i as i64 * 60_000,
                o: base,
                h: base + 2.5,
                l: base - 2.5,
                c: base + (i as f64 * 0.7).cos(),
                v: 10.0 + (i % 7) as f64,
            }
        })
        .collect()
}

fn st(bars: Vec<Bar>) -> ChartState {
    let mut s = ChartState::default();
    let n = bars.len();
    s.bars = bars;
    s.closed_len = n;
    s.refresh_caches();
    s
}

/// One visible oscillator (rsi) authored into `PaneKey::Study(uid)`.
fn osc(uid: u64, bars: &[Bar]) -> Active {
    let spec = crate::indicators::get("rsi").expect("rsi registered");
    let a = Active::new(uid, spec, bars);
    assert!(!a.is_overlay(), "rsi must be an oscillator (its own study pane)");
    a
}

/// A layout call with the empty-by-default series/overlay inputs — the common
/// case; individual tests override the study / series arguments.
fn layout<'a>(
    avail: f32,
    show_volume: bool,
    style: ChartStyle,
    state: &ChartState,
    indicators: &[Active],
    sub_panes: &[PaneKey],
    study_pane_of: &IndexMap<u64, PaneKey>,
    cvd_on: bool,
    has_footprint: bool,
    panes: &mut PaneFractions,
) -> PaneLayout<'a> {
    static EMPTY_SP_OF: std::sync::OnceLock<IndexMap<String, PaneKey>> = std::sync::OnceLock::new();
    resolve_pane_layout(
        avail,
        show_volume,
        style,
        state,
        indicators,
        sub_panes,
        study_pane_of,
        &[],
        cvd_on,
        has_footprint,
        &[],
        EMPTY_SP_OF.get_or_init(IndexMap::new),
        &[],
        panes,
        false,
    )
}

/// The [`layout`] twin that also feeds tick-driven microstructure studies — the
/// only inputs that differ, so every other argument is the common minimal case
/// (no volume/CVD/series, candles, `avail == AVAIL`).
fn layout_micro<'a>(
    state: &ChartState,
    sub_panes: &[PaneKey],
    micro: &[ActiveStudy],
    panes: &mut PaneFractions,
) -> PaneLayout<'a> {
    static EMPTY_SP_OF: std::sync::OnceLock<IndexMap<String, PaneKey>> = std::sync::OnceLock::new();
    static EMPTY_STUDY_OF: std::sync::OnceLock<IndexMap<u64, PaneKey>> = std::sync::OnceLock::new();
    resolve_pane_layout(
        AVAIL,
        false,
        ChartStyle::Candles,
        state,
        &[],
        sub_panes,
        EMPTY_STUDY_OF.get_or_init(IndexMap::new),
        micro,
        false,
        false,
        &[],
        EMPTY_SP_OF.get_or_init(IndexMap::new),
        &[],
        panes,
        false,
    )
}

#[test]
fn price_only_no_subpanes() {
    let s = st(wave(40));
    let mut pf = PaneFractions::default();
    let sp_of = IndexMap::new();
    let l = layout(AVAIL, false, ChartStyle::Candles, &s, &[], &[], &sp_of, false, false, &mut pf);
    assert_eq!(l.present, vec![PaneKey::Price]);
    assert_eq!(l.heights.len(), 1);
    assert_eq!(l.n_below, 0);
    assert_eq!(l.axis_h, 0.0);
    assert_eq!(l.chrome, 0.0);
    assert_eq!(l.avail_for_panes, AVAIL);
    assert_eq!(l.price_h, AVAIL); // price alone takes the whole budget
    assert_eq!(l.n_reorderable, 0);
}

#[test]
fn with_volume() {
    let s = st(wave(40));
    let mut pf = PaneFractions::default();
    let sp_of = IndexMap::new();
    // Volume must be present in the authored unified sub-order to appear.
    let sub = [PaneKey::Volume];
    let l = layout(AVAIL, true, ChartStyle::Candles, &s, &[], &sub, &sp_of, false, false, &mut pf);
    assert!(l.vol_on);
    assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Volume]);
    assert_eq!(l.heights.len(), 2);
    assert_eq!(l.n_below, 1);
    assert_eq!(l.n_reorderable, 1);
    assert_eq!(l.axis_h, chart::AXIS_LABEL_H);
    // No header rows now (overlay legend): chrome = n_below(1)*chart::PANE_SEP_H + axis_h
    assert_eq!(l.chrome, chart::PANE_SEP_H + chart::AXIS_LABEL_H);
}

#[test]
fn with_volume_and_cvd() {
    let s = st(wave(40));
    let mut pf = PaneFractions::default();
    let sp_of = IndexMap::new();
    // cvd_on AND footprint present ⇒ the CVD pane resolves; both authored.
    let sub = [PaneKey::Volume, PaneKey::Cvd];
    let l = layout(AVAIL, true, ChartStyle::Candles, &s, &[], &sub, &sp_of, true, true, &mut pf);
    assert!(l.cvd_pane_on);
    assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Volume, PaneKey::Cvd]);
    assert_eq!(l.heights.len(), 3);
    assert_eq!(l.n_below, 2);
    assert_eq!(l.n_reorderable, 2);
    // No header rows: chrome = 2*chart::PANE_SEP_H + axis_h
    assert_eq!(l.chrome, 2.0 * chart::PANE_SEP_H + chart::AXIS_LABEL_H);
}

#[test]
fn cvd_after_volume_order_is_honored() {
    // Chart single-max default: the authored order is verbatim — CVD ABOVE
    // Volume renders CVD above Volume (they are peers, no forced sequence).
    let s = st(wave(40));
    let mut pf = PaneFractions::default();
    let sp_of = IndexMap::new();
    let sub = [PaneKey::Cvd, PaneKey::Volume];
    let l = layout(AVAIL, true, ChartStyle::Candles, &s, &[], &sub, &sp_of, true, true, &mut pf);
    assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Cvd, PaneKey::Volume]);
}

#[test]
fn cvd_needs_footprint_data() {
    // cvd_on but NO footprint ⇒ no CVD pane (default-off gate).
    let s = st(wave(40));
    let mut pf = PaneFractions::default();
    let sp_of = IndexMap::new();
    let sub = [PaneKey::Volume, PaneKey::Cvd];
    let l = layout(AVAIL, true, ChartStyle::Candles, &s, &[], &sub, &sp_of, true, false, &mut pf);
    assert!(!l.cvd_pane_on, "cvd_on alone must not produce a pane without footprint data");
    assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Volume]);
}

#[test]
fn one_study_pane() {
    let s = st(wave(60));
    let ind = [osc(1, &s.bars)];
    let sub = [PaneKey::Volume, PaneKey::Study(1)];
    let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
    sp_of.insert(1, PaneKey::Study(1));
    let mut pf = PaneFractions::default();
    let l = layout(AVAIL, true, ChartStyle::Candles, &s, &ind, &sub, &sp_of, false, false, &mut pf);
    assert_eq!(l.visible_study_panes, vec![PaneKey::Study(1)]);
    assert_eq!(l.n_study_panes, 1);
    assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Volume, PaneKey::Study(1)]);
    assert_eq!(l.heights.len(), 3);
    assert_eq!(l.n_below, 2); // volume + study
    assert_eq!(l.n_reorderable, 2);
    // No header rows: chrome = n_below(2)*chart::PANE_SEP_H + axis_h
    assert_eq!(l.chrome, 2.0 * chart::PANE_SEP_H + chart::AXIS_LABEL_H);
}

#[test]
fn hidden_study_leaves_no_pane() {
    // A study whose indicator is NOT visible must be filtered out (no blank strip).
    let s = st(wave(60));
    let mut ind = [osc(1, &s.bars)];
    ind[0].visible = false;
    let sub = [PaneKey::Study(1)];
    let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
    sp_of.insert(1, PaneKey::Study(1));
    let mut pf = PaneFractions::default();
    let l =
        layout(AVAIL, false, ChartStyle::Candles, &s, &ind, &sub, &sp_of, false, false, &mut pf);
    assert!(l.visible_study_panes.is_empty(), "hidden study ⇒ its pane is dropped");
    assert_eq!(l.n_study_panes, 0);
    assert_eq!(l.present, vec![PaneKey::Price]);
}

#[test]
fn renko_suppresses_volume() {
    // Renko reindexes ⇒ style_preserves_volume(Renko) == false ⇒ no volume pane
    // even with show_volume = true.
    let s = st(wave(60));
    let mut pf = PaneFractions::default();
    let sp_of = IndexMap::new();
    let sub = [PaneKey::Volume];
    let l = layout(AVAIL, true, ChartStyle::Renko, &s, &[], &sub, &sp_of, false, false, &mut pf);
    assert!(!l.vol_on, "Renko volume is suppressed");
    assert_eq!(l.present, vec![PaneKey::Price]);
    assert_eq!(l.n_below, 0);
}

#[test]
fn one_series_pane_resolves_from_overlays() {
    // A compare symbol moved into its own pane: series_panes + series_pane_of +
    // a matching overlay ⇒ visible_series resolves and the pane appears last.
    let s = st(wave(40));
    let other = st(wave(20));
    let overlays = [SeriesInput { symbol: "ETHUSDT", state: &other, color: Color32::RED }];
    let mut sp_of_series: IndexMap<String, PaneKey> = IndexMap::new();
    sp_of_series.insert("ETHUSDT".to_string(), PaneKey::Series(7));
    let series_panes = [PaneKey::Series(7)];
    let empty_study: IndexMap<u64, PaneKey> = IndexMap::new();
    let mut pf = PaneFractions::default();
    let l = resolve_pane_layout(
        AVAIL,
        false, // no volume, to isolate the series-pane bookkeeping
        ChartStyle::Candles,
        &s,
        &[],
        &[],
        &empty_study,
        &[],
        false,
        false,
        &series_panes,
        &sp_of_series,
        &overlays,
        &mut pf,
        false,
    );
    assert_eq!(l.n_series_panes, 1);
    assert_eq!(l.visible_series.len(), 1);
    assert_eq!(l.visible_series[0].0, PaneKey::Series(7));
    assert_eq!(l.visible_series[0].1.symbol, "ETHUSDT");
    assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Series(7)]);
    assert_eq!(l.heights.len(), 2);
    assert_eq!(l.n_below, 1);
    // No volume/cvd/study ⇒ zero reorderable sub-panes; the series pane is
    // present at index 1 of `present`.
    assert_eq!(l.n_reorderable, 0);
    // No header rows: chrome = n_below(1)*chart::PANE_SEP_H + axis_h
    assert_eq!(l.chrome, chart::PANE_SEP_H + chart::AXIS_LABEL_H);
}

#[test]
fn price_maximized_suppresses_every_subpane() {
    // Feature #1b: with price_maximized set, volume + every study pane are
    // hidden and the price pane is the sole present pane at full height —
    // even though show_volume is ON and a study pane exists.
    let s = st(wave(40));
    let ind = [osc(1, &s.bars)];
    let sub = [PaneKey::Volume, PaneKey::Study(1)];
    let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
    sp_of.insert(1, PaneKey::Study(1));
    let empty_series: IndexMap<String, PaneKey> = IndexMap::new();
    let mut pf = PaneFractions::default();
    let l = resolve_pane_layout(
        AVAIL,
        true, // volume ON — maximize must override it
        ChartStyle::Candles,
        &s,
        &ind,
        &sub,
        &sp_of,
        &[],
        false,
        false,
        &[],
        &empty_series,
        &[],
        &mut pf,
        true, // price_maximized
    );
    assert_eq!(l.present, vec![PaneKey::Price]);
    assert_eq!(l.n_study_panes, 0);
    assert!(!l.vol_on);
    assert_eq!(l.n_below, 0);
    assert_eq!(l.heights.len(), 1);
    assert_eq!(l.chrome, 0.0); // no headers / separators / axis when maximized
}

#[test]
fn heights_sum_plus_chrome_equals_avail() {
    // The layout invariant across a rich pane set: study + volume + cvd.
    let s = st(wave(60));
    let ind = [osc(1, &s.bars)];
    let sub = [PaneKey::Volume, PaneKey::Cvd, PaneKey::Study(1)];
    let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
    sp_of.insert(1, PaneKey::Study(1));
    let mut pf = PaneFractions::default();
    let l = layout(AVAIL, true, ChartStyle::Candles, &s, &ind, &sub, &sp_of, true, true, &mut pf);
    assert_eq!(l.present.len(), 4); // Price, Volume, Cvd, Study(1)
    let sum: f32 = l.heights.iter().sum();
    assert!(
        (sum + l.chrome - AVAIL).abs() < 0.01,
        "heights.sum({sum}) + chrome({}) != {AVAIL}",
        l.chrome
    );
}

// ---- tick-driven microstructure studies (the `micro` population) ----

/// A VPIN study (trades-only, never book-gated) with one committed sample, so it
/// is visible AND non-empty — the "this pane has something to draw" case.
fn vpin(uid: u64) -> ActiveStudy {
    let spec = crate::studies::get_study("vpin").expect("vpin registered");
    // bucket volume 10 / window 2 so a single trade commits a bucket.
    let mut s = ActiveStudy::with_params(uid, spec, vec![10.0, 2.0, 1.0]);
    s.on_trade(
        &vike_marketdata::TradeTick {
            ts: 1,
            local_ts: 0,
            price: 100.0,
            size: 10.0,
            is_buyer_maker: false,
            symbol: String::new(),
        },
        None,
    );
    s
}

#[test]
fn no_studies_is_the_pre_studies_layout() {
    // The byte-identical guarantee, asserted directly: `micro == &[]` resolves the
    // SAME layout as the indicator-only path, including that a Study pane with no
    // indicator behind it stays absent.
    let s = st(wave(40));
    let sub = [PaneKey::Study(9)];
    let mut pf = PaneFractions::default();
    let l = layout_micro(&s, &sub, &[], &mut pf);
    assert_eq!(l.present, vec![PaneKey::Price]);
    assert!(l.visible_study_panes.is_empty());
    assert_eq!(l.n_below, 0);
}

#[test]
fn a_microstructure_study_keeps_its_pane_present() {
    let s = st(wave(40));
    let micro = [vpin(9)];
    let sub = [PaneKey::Study(9)];
    let mut pf = PaneFractions::default();
    let l = layout_micro(&s, &sub, &micro, &mut pf);
    assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Study(9)]);
    assert_eq!(l.visible_study_panes, vec![PaneKey::Study(9)]);
    assert_eq!(l.n_study_panes, 1);
    assert_eq!(l.n_below, 1);
}

#[test]
fn hidden_microstructure_study_leaves_no_pane() {
    let s = st(wave(40));
    let mut micro = [vpin(9)];
    micro[0].visible = false;
    let sub = [PaneKey::Study(9)];
    let mut pf = PaneFractions::default();
    let l = layout_micro(&s, &sub, &micro, &mut pf);
    assert_eq!(l.present, vec![PaneKey::Price], "a hidden study must not leave a blank strip");
}

#[test]
fn book_gated_study_with_no_l2_feed_leaves_no_pane() {
    // `book_imbalance` needs a book; none was fed, so `is_empty()` holds and the
    // pane must not open at all (rather than opening on a fabricated flat line).
    let s = st(wave(40));
    let spec = crate::studies::get_study("book_imbalance").expect("registered");
    let micro = [ActiveStudy::new(9, spec)];
    assert!(micro[0].is_empty());
    let sub = [PaneKey::Study(9)];
    let mut pf = PaneFractions::default();
    let l = layout_micro(&s, &sub, &micro, &mut pf);
    assert_eq!(l.present, vec![PaneKey::Price]);
}

#[test]
fn an_indicator_and_a_study_can_share_one_pane() {
    // Merged pane: the indicator predicate alone would already keep it present, so
    // this pins that adding the study population does not disturb that resolution.
    let s = st(wave(60));
    let ind = [osc(1, &s.bars)];
    let micro = [vpin(2)];
    let sub = [PaneKey::Study(1)];
    let mut sp_of: IndexMap<u64, PaneKey> = IndexMap::new();
    sp_of.insert(1, PaneKey::Study(1));
    let mut pf = PaneFractions::default();
    static EMPTY_SP_OF: std::sync::OnceLock<IndexMap<String, PaneKey>> = std::sync::OnceLock::new();
    let l = resolve_pane_layout(
        AVAIL,
        false,
        ChartStyle::Candles,
        &s,
        &ind,
        &sub,
        &sp_of,
        &micro,
        false,
        false,
        &[],
        EMPTY_SP_OF.get_or_init(IndexMap::new),
        &[],
        &mut pf,
        false,
    );
    assert_eq!(l.present, vec![PaneKey::Price, PaneKey::Study(1)]);
    assert_eq!(l.n_study_panes, 1);
}

#[test]
fn price_maximize_hides_study_panes_too() {
    let s = st(wave(40));
    let micro = [vpin(9)];
    let sub = [PaneKey::Study(9)];
    let mut pf = PaneFractions::default();
    static EMPTY_SP_OF: std::sync::OnceLock<IndexMap<String, PaneKey>> = std::sync::OnceLock::new();
    static EMPTY_STUDY_OF: std::sync::OnceLock<IndexMap<u64, PaneKey>> = std::sync::OnceLock::new();
    let l = resolve_pane_layout(
        AVAIL,
        false,
        ChartStyle::Candles,
        &s,
        &[],
        &sub,
        EMPTY_STUDY_OF.get_or_init(IndexMap::new),
        &micro,
        false,
        false,
        &[],
        EMPTY_SP_OF.get_or_init(IndexMap::new),
        &[],
        &mut pf,
        true,
    );
    assert_eq!(l.present, vec![PaneKey::Price]);
}
