use super::*;
use vike_data::SeriesCoverage;

/// One headless frame whose texture uploads are deliberately discarded, and whose geometry is
/// asserted sane.
///
/// egui 0.36 made `TexturesDelta` PANIC on drop while it still holds unapplied deltas —
/// "Deltas need to be handled. If you want to drop this intentionally call `clear` before
/// dropping." Every harness below renders into no backend at all: they run a frame to read a
/// widget rect, an action, or the emitted shapes. Discarding the uploads IS the intent, so it
/// is stated once here rather than at eight call sites.
///
/// Every frame also goes through `vike_ui_theme::frame_sanity::assert_frame_sane` — the ONE
/// shared geometry invariant, behind that crate's `test-support` feature. It is stated here
/// (once) for the same reason the clear is: every test that renders a frame gets it, including
/// the ones added after this comment.
///
/// ⚠ The assertion runs AFTER the clear, and the order is load-bearing. `clear` touches
/// `textures_delta` and never `shapes`, so it cannot hide anything the assertion reads — while
/// asserting first leaves the deltas unapplied, so dropping the frame during the assertion's
/// unwind panics a SECOND time in the destructor and the process ABORTS with `SIGABRT`,
/// printing a backtrace instead of the coordinate that was wrong.
fn run_frame(
    ctx: &egui::Context,
    raw: egui::RawInput,
    f: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut out = ctx.run_ui(raw, f);
    out.textures_delta.clear();
    vike_ui_theme::frame_sanity::assert_frame_sane(&out);
    out
}

fn cov(rows: u64, bytes: u64, first: i64, last: i64) -> SeriesCoverage {
    SeriesCoverage { first_ts: first, last_ts: last, rows, bytes, parts: 1, dates: 1 }
}

fn row(symbol: &str, kind: &str, iv: Option<&str>, c: SeriesCoverage) -> FlatRow {
    FlatRow {
        symbol: symbol.into(),
        kind: kind.into(),
        interval: iv.map(Into::into),
        cov: c,
        grouped: false,
    }
}

#[test]
fn coverage_label_shows_rows_and_span() {
    let cov = SeriesCoverage {
        first_ts: 0,
        last_ts: 86_400_000,
        rows: 1234,
        bytes: 0,
        parts: 1,
        dates: 2,
    };
    let s = coverage_label(&cov);
    assert!(s.contains("1234") || s.contains("1,234"));
    assert!(s.contains("1970")); // epoch-derived date rendered
    assert!(!s.contains("panicked"));
}

#[test]
fn coverage_label_zero_rows_is_safe() {
    let cov = SeriesCoverage { first_ts: 0, last_ts: 0, rows: 0, bytes: 0, parts: 0, dates: 0 };
    let _ = coverage_label(&cov); // must not panic on an empty series
}

#[test]
fn fmt_bytes_scales_units() {
    assert_eq!(fmt_bytes(0), "0 B");
    assert_eq!(fmt_bytes(512), "512 B");
    assert_eq!(fmt_bytes(1536), "1.5 KB");
    assert_eq!(fmt_bytes(1_572_864), "1.5 MB"); // 1.5 * 1024 * 1024
    assert_eq!(fmt_bytes(1024u64.pow(3) * 2), "2.0 GB");
}

#[test]
fn fmt_count_adds_thousands_separators() {
    assert_eq!(fmt_count(0), "0");
    assert_eq!(fmt_count(7), "7");
    assert_eq!(fmt_count(999), "999");
    assert_eq!(fmt_count(1000), "1,000");
    assert_eq!(fmt_count(1_234_567), "1,234,567");
}

#[test]
fn fmt_count_compact_scales_to_k_m_b() {
    assert_eq!(fmt_count_compact(500), "500");
    assert_eq!(fmt_count_compact(2_145_000_000), "2.1B");
    assert_eq!(fmt_count_compact(430_000), "430.0K");
}

#[test]
fn is_stale_flags_far_behind_global_max() {
    // global window is 0..1000; a series ending at 100 is 90% behind -> stale.
    assert!(is_stale(100, 0, 1000));
    // a series ending at 950 is only 5% behind -> not stale.
    assert!(!is_stale(950, 0, 1000));
    // degenerate window (no span) never flags stale.
    assert!(!is_stale(0, 0, 0));
}

#[test]
fn sort_by_rows_desc_orders_largest_first() {
    let mut rows = vec![
        row("AAA", "bar", Some("1m"), cov(100, 10, 0, 10)),
        row("BBB", "bar", Some("1m"), cov(300, 10, 0, 10)),
        row("CCC", "bar", Some("1m"), cov(200, 10, 0, 10)),
    ];
    sort_flat_rows(&mut rows, SortState { column: SortColumn::Rows, ascending: false });
    let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["BBB", "CCC", "AAA"]);
}

#[test]
fn sort_by_symbol_asc_orders_alphabetically() {
    let mut rows = vec![
        row("ZZZ", "bar", None, cov(1, 1, 0, 1)),
        row("AAA", "bar", None, cov(1, 1, 0, 1)),
        row("MMM", "bar", None, cov(1, 1, 0, 1)),
    ];
    sort_flat_rows(&mut rows, SortState { column: SortColumn::Symbol, ascending: true });
    let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["AAA", "MMM", "ZZZ"]);
}

#[test]
fn sort_by_updated_desc_orders_most_recent_first() {
    let mut rows = vec![
        row("AAA", "bar", Some("1m"), cov(1, 1, 0, 500)),
        row("BBB", "bar", Some("1m"), cov(1, 1, 0, 9000)),
        row("CCC", "bar", Some("1m"), cov(1, 1, 0, 4000)),
    ];
    sort_flat_rows(&mut rows, SortState { column: SortColumn::Updated, ascending: false });
    let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["BBB", "CCC", "AAA"]);
}

#[test]
fn sort_ties_break_deterministically_by_symbol_then_kind() {
    let mut rows = vec![
        row("AAA", "trade", None, cov(50, 1, 0, 1)),
        row("AAA", "bar", Some("1m"), cov(50, 1, 0, 1)),
    ];
    sort_flat_rows(&mut rows, SortState { column: SortColumn::Rows, ascending: true });
    // equal Rows -> falls back to symbol (equal) then kind_label ("bar/1m" < "trade").
    assert_eq!(rows[0].kind, "bar");
    assert_eq!(rows[1].kind, "trade");
}

#[test]
fn flatten_venue_filters_by_query_and_flattens_series() {
    use crate::model::{RollUp, SeriesRow, SymbolNode};
    let venue = VenueNode {
        venue: "binance".into(),
        symbols: vec![
            SymbolNode {
                symbol: "BTCUSDT".into(),
                grouped: false,
                series: vec![
                    SeriesRow {
                        kind: "bar".into(),
                        interval: Some("1m".into()),
                        cov: cov(100, 10, 0, 10),
                    },
                    SeriesRow { kind: "trade".into(), interval: None, cov: cov(50, 5, 0, 10) },
                ],
                total: RollUp::default(),
            },
            SymbolNode {
                symbol: "ETHUSDT".into(),
                grouped: false,
                series: vec![],
                total: RollUp::default(),
            },
        ],
        total: RollUp::default(),
    };
    let all = flatten_venue(&venue, "");
    assert_eq!(all.len(), 2); // BTCUSDT's two series; ETHUSDT has none

    let filtered = flatten_venue(&venue, "eth");
    assert!(filtered.is_empty()); // ETHUSDT matches the query but has no series rows

    let filtered = flatten_venue(&venue, "btc");
    assert_eq!(filtered.len(), 2);
}

fn venue_fixture(venue: &str, symbols: &[(&str, u64, i64, i64)]) -> VenueNode {
    use crate::model::{RollUp, SeriesRow, SymbolNode};
    let symbols = symbols
        .iter()
        .map(|(sym, rows, first, last)| SymbolNode {
            symbol: (*sym).to_string(),
            grouped: false,
            series: vec![SeriesRow {
                kind: "bar".into(),
                interval: Some("1m".into()),
                cov: cov(*rows, 1, *first, *last),
            }],
            total: RollUp::default(),
        })
        .collect();
    VenueNode { venue: venue.to_string(), symbols, total: RollUp::default() }
}

fn key(venue: &str, symbol: &str) -> SeriesKey {
    SeriesKey {
        venue: venue.into(),
        symbol: symbol.into(),
        kind: "bar".into(),
        interval: Some("1m".into()),
    }
}

// ============================ GridState selection (commit 1) ============================

#[test]
fn set_selected_inserts_and_removes() {
    let mut selected = BTreeSet::new();
    let k = key("binance", "BTCUSDT");

    set_selected(&mut selected, k.clone(), true);
    assert!(selected.contains(&k));

    set_selected(&mut selected, k.clone(), false);
    assert!(!selected.contains(&k), "toggling off must remove the key");
}

#[test]
fn set_selected_is_idempotent() {
    let mut selected = BTreeSet::new();
    let k = key("okx", "BTC-USDT");
    set_selected(&mut selected, k.clone(), true);
    set_selected(&mut selected, k.clone(), true);
    assert_eq!(selected.len(), 1, "inserting the same key twice must not duplicate it");
}

#[test]
fn select_all_over_filtered_set_only_selects_matching_rows() {
    let tree = vec![
        venue_fixture("binance", &[("BTCUSDT", 10, 0, 10), ("ETHUSDT", 5, 0, 10)]),
        venue_fixture("okx", &[("BTC-USDT", 7, 0, 10)]),
    ];

    // "btc" matches BTCUSDT (binance) and BTC-USDT (okx), not ETHUSDT.
    let keys = visible_keys(&tree, "btc");
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|k| k.symbol.to_lowercase().contains("btc")));

    let mut selected = BTreeSet::new();
    apply_select_all(&mut selected, &keys, true);
    assert_eq!(selected.len(), 2);
    assert!(selected.contains(&key("binance", "BTCUSDT")));
    assert!(selected.contains(&key("okx", "BTC-USDT")));
    assert!(!selected.contains(&key("binance", "ETHUSDT")));

    // narrowing the query and re-running select-all(false) only clears what's now visible.
    apply_select_all(&mut selected, &visible_keys(&tree, "binance"), false);
    assert!(!selected.contains(&key("binance", "BTCUSDT")));
    assert!(
        selected.contains(&key("okx", "BTC-USDT")),
        "clearing a narrower filtered set must not touch keys outside it"
    );
}

#[test]
fn visible_keys_empty_query_covers_every_row() {
    let tree = vec![venue_fixture("binance", &[("BTCUSDT", 10, 0, 10), ("ETHUSDT", 5, 0, 10)])];
    assert_eq!(visible_keys(&tree, "").len(), 2);
}

// ============================ bulk action bar (commit 1) ============================
// Real egui interaction test: renders `bulk_action_bar`, reads back the Delete button's
// actual screen rect from a probe frame, then drives a real pointer press+release over it —
// proving the click really does resolve to `BulkAction::Delete`, not just that the enum
// exists.

fn bulk_bar_screen() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0))
}

#[test]
fn bulk_bar_delete_button_click_sets_delete_action() {
    let ctx = egui::Context::default();
    let screen = bulk_bar_screen();

    // Frame 0: probe — discover the Delete button's rect (no pointer interaction).
    let mut rects = [egui::Rect::NOTHING; 3];
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    let _ = run_frame(&ctx, raw, |ui| {
        let (_, r) = bulk_action_bar(ui, 3, None);
        rects = r;
    });
    let delete_pos = rects[2].center();

    // Frame 1: move onto + press the Delete button.
    let mut raw =
        egui::RawInput { screen_rect: Some(screen), time: Some(1.0 / 60.0), ..Default::default() };
    raw.events.push(egui::Event::PointerMoved(delete_pos));
    raw.events.push(egui::Event::PointerButton {
        pos: delete_pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    let _ = run_frame(&ctx, raw, |ui| {
        let _ = bulk_action_bar(ui, 3, None);
    });

    // Frame 2: release over the same position -> a real click.
    let mut raw =
        egui::RawInput { screen_rect: Some(screen), time: Some(2.0 / 60.0), ..Default::default() };
    raw.events.push(egui::Event::PointerButton {
        pos: delete_pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    let mut action = None;
    let _ = run_frame(&ctx, raw, |ui| {
        let (a, _) = bulk_action_bar(ui, 3, None);
        action = a;
    });

    assert_eq!(action, Some(BulkAction::Delete));
}

/// The remote-mode gate (the #1378 seam close): with `delete_disabled = Some(reason)` the
/// Delete button is GRAYED, so the same press+release that resolves to `BulkAction::Delete`
/// above must yield NO action — a delete on a remote grid would act on the local store the
/// grid is not showing, and a clickable button that silently no-ops is the dishonest variant.
///
/// The `reason` string is arbitrary here — this test drives the GATE, not the wording — but it
/// is kept in step with `vike_app_core::data::stored_mode`'s `DELETE_LOCAL_ONLY` so a reader does not
/// meet a retired sentence. It used to read "delete is a local-store operation", which the
/// 2026-09-07 wire delete verb made false; see `bulk_action_bar`'s doc for the correction.
#[test]
fn bulk_bar_disabled_delete_click_yields_no_action() {
    const REASON: Option<&str> =
        Some("Delete is local-store only here — the wire's delete verb needs a keyed datahub");
    let ctx = egui::Context::default();
    let screen = bulk_bar_screen();

    // Frame 0: probe — the disabled button still owns a rect to aim the click at.
    let mut rects = [egui::Rect::NOTHING; 3];
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    let _ = run_frame(&ctx, raw, |ui| {
        let (_, r) = bulk_action_bar(ui, 3, REASON);
        rects = r;
    });
    let delete_pos = rects[2].center();
    assert!(rects[2] != egui::Rect::NOTHING, "the grayed Delete button still renders");

    // Frame 1: move onto + press the Delete button.
    let mut raw =
        egui::RawInput { screen_rect: Some(screen), time: Some(1.0 / 60.0), ..Default::default() };
    raw.events.push(egui::Event::PointerMoved(delete_pos));
    raw.events.push(egui::Event::PointerButton {
        pos: delete_pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    let _ = run_frame(&ctx, raw, |ui| {
        let _ = bulk_action_bar(ui, 3, REASON);
    });

    // Frame 2: release over the same position — the click that must NOT resolve.
    let mut raw =
        egui::RawInput { screen_rect: Some(screen), time: Some(2.0 / 60.0), ..Default::default() };
    raw.events.push(egui::Event::PointerButton {
        pos: delete_pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    let mut action = None;
    let _ = run_frame(&ctx, raw, |ui| {
        let (a, _) = bulk_action_bar(ui, 3, REASON);
        action = a;
    });

    assert_eq!(action, None, "a disabled Delete must not produce an action");
}

#[test]
fn bulk_bar_backfill_button_click_sets_backfill_action() {
    let ctx = egui::Context::default();
    let screen = bulk_bar_screen();

    let mut rects = [egui::Rect::NOTHING; 3];
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    let _ = run_frame(&ctx, raw, |ui| {
        let (_, r) = bulk_action_bar(ui, 1, None);
        rects = r;
    });
    let backfill_pos = rects[0].center();

    let mut raw =
        egui::RawInput { screen_rect: Some(screen), time: Some(1.0 / 60.0), ..Default::default() };
    raw.events.push(egui::Event::PointerMoved(backfill_pos));
    raw.events.push(egui::Event::PointerButton {
        pos: backfill_pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    let _ = run_frame(&ctx, raw, |ui| {
        let _ = bulk_action_bar(ui, 1, None);
    });

    let mut raw =
        egui::RawInput { screen_rect: Some(screen), time: Some(2.0 / 60.0), ..Default::default() };
    raw.events.push(egui::Event::PointerButton {
        pos: backfill_pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    let mut action = None;
    let _ = run_frame(&ctx, raw, |ui| {
        let (a, _) = bulk_action_bar(ui, 1, None);
        action = a;
    });

    assert_eq!(action, Some(BulkAction::Backfill));
}

#[test]
fn bulk_bar_no_click_reports_no_action() {
    let ctx = egui::Context::default();
    let screen = bulk_bar_screen();
    let mut action = None;
    for f in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let _ = run_frame(&ctx, raw, |ui| {
            let (a, _) = bulk_action_bar(ui, 2, None);
            action = a;
        });
    }
    assert_eq!(action, None);
}

// ============================ filter_tree / ViewFilter (commit 1) ============================

/// A tree built via `build_tree` (real rollups, not the zero-filled `venue_fixture` totals
/// above) so `global_span`/`is_stale` behave exactly as they do in production. Whole-tree span
/// is `[0, 1000]` (from okx's `BTC-USDT` at 1000): binance/FRESH ends at 990 (1% behind, not
/// stale), binance/STALE ends at 100 (90% behind, stale), okx/BTC-USDT ends at 1000 (not
/// stale).
fn tree_fixture() -> Vec<VenueNode> {
    use vike_data::SeriesId;
    let sid =
        |venue: &str, symbol: &str| SeriesId::per_symbol("bar", venue, symbol, Some("1m".into()));
    let inv = vec![
        (sid("binance", "FRESH"), cov(10, 1, 0, 990)),
        (sid("binance", "STALE"), cov(10, 1, 0, 100)),
        (sid("okx", "BTC-USDT"), cov(5, 1, 0, 1000)),
    ];
    crate::model::build_tree(inv)
}

// ======================= the cross-kind Partial column (dm-partial-cell) =====================
//
// Real egui frames, same shape as the `bulk_bar_*` interaction tests above: render the actual
// grid and read the glyphs back out of the frame's shapes, so these prove the marker REACHES
// THE SCREEN — not merely that `partial_days_label` returns a string.

/// Every piece of text painted in one frame of `stored_catalog_grid` over `tree`/`partials`.
fn grid_frame_text(tree: &[VenueNode], partials: &PartialDayMap) -> Vec<String> {
    let ctx = egui::Context::default();
    // The app's type, not egui's: the partial column draws an icon, and the icon family exists
    // only in the bundled set — egui's default fonts do not bind it, and epaint panics.
    vike_ui_theme::appearance::install_type(&ctx, vike_ui_theme::type_scale::TextSize::default());
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    let mut state = GridState::default();
    let out = run_frame(&ctx, raw, |ui| {
        let _ = stored_catalog_grid(ui, tree, &mut state, &GapMap::new(), partials, None);
    });
    out.shapes
        .into_iter()
        .filter_map(|cs| match cs.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

/// The warning icon as painted text — what the partial column's header and markers draw.
fn warning() -> String {
    vike_ui_theme::icons::WARNING.accessible_label("")
}

fn partial_fixture(venue: &str, symbol: &str, missing: &[&str]) -> PartialDayMap {
    let key =
        vike_data::InstrumentKey { venue: venue.into(), label: symbol.into(), grouped: false };
    let day = vike_data::PartialDay {
        day: 20_666, // ~2026-08-01; the column shows a COUNT, so the exact day is irrelevant
        missing_kinds: missing.iter().map(|s| s.to_string()).collect(),
    };
    PartialDayMap::from([(key, vec![day])])
}

/// The column header exists whether or not anything is partial — it is the affordance that
/// tells you the check was RUN, which a blank column alone would not.
#[test]
fn partial_column_header_renders_with_an_empty_map() {
    let text = grid_frame_text(&tree_fixture(), &PartialDayMap::new());
    assert!(text.iter().any(|t| *t == warning()), "header icon missing: {text:?}");
    assert!(text.iter().any(|t| t == "Coverage"), "grid did not render at all: {text:?}");
}

/// An empty map paints the header icon and NOTHING else — one warning total, no row markers.
#[test]
fn no_partial_days_marks_no_rows() {
    let text = grid_frame_text(&tree_fixture(), &PartialDayMap::new());
    assert_eq!(text.iter().filter(|t| **t == warning()).count(), 1, "{text:?}");
}

/// **The point of the column.** `binance/FRESH` has one partial day, so its row is marked —
/// and only its row: `STALE` and okx's `BTC-USDT` are untouched. The fixture's symbols each
/// have ONE series, so one marked instrument is exactly one marked row (plus the header).
#[test]
fn a_partial_instrument_marks_its_row_and_no_other() {
    let partials = partial_fixture("binance", "FRESH", &["quote"]);
    let text = grid_frame_text(&tree_fixture(), &partials);
    assert_eq!(
        text.iter().filter(|t| **t == warning()).count(),
        2,
        "expected header + exactly one row marker: {text:?}"
    );
}

/// **The distinction #1009 preserved, now load-bearing at the pixel level.** A grouped series
/// and a per-symbol one can share a label; the map here is keyed `grouped: false`, so a tree of
/// GROUPED series must not match it. Keying on `(venue, label)` alone would mark this row.
#[test]
fn a_grouped_row_does_not_match_a_per_symbol_key() {
    use vike_data::SeriesId;
    let tree = crate::model::build_tree(vec![(
        SeriesId::grouped("trade", "binance", "FRESH"),
        cov(10, 1, 0, 990),
    )]);
    let text = grid_frame_text(&tree, &partial_fixture("binance", "FRESH", &["quote"]));
    assert_eq!(
        text.iter().filter(|t| **t == warning()).count(),
        1,
        "a grouped row matched a per-symbol partial key: {text:?}"
    );
}

#[test]
fn filter_tree_all_keeps_everything() {
    let tree = tree_fixture();
    let filtered = filter_tree(&tree, &ViewFilter::All, &GapMap::new());
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].symbols.len() + filtered[1].symbols.len(), 3);
}

#[test]
fn filter_tree_venue_keeps_only_that_venue() {
    let tree = tree_fixture();
    let filtered = filter_tree(&tree, &ViewFilter::Venue("binance".to_string()), &GapMap::new());
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].venue, "binance");
    assert_eq!(filtered[0].symbols.len(), 2);
}

#[test]
fn filter_tree_stale_keeps_only_stale_rows() {
    let tree = tree_fixture();
    let filtered = filter_tree(&tree, &ViewFilter::Stale, &GapMap::new());
    // Only binance/STALE lags more than 35% of the tree-wide span behind the global max;
    // binance/FRESH and okx/BTC-USDT both drop out, and okx's venue disappears entirely.
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].venue, "binance");
    assert_eq!(filtered[0].symbols.len(), 1);
    assert_eq!(filtered[0].symbols[0].symbol, "STALE");
}

#[test]
fn filter_tree_watchlist_and_asset_class_are_still_placeholders() {
    let tree = tree_fixture();
    let gaps = GapMap::new();
    assert_eq!(filter_tree(&tree, &ViewFilter::Watchlist("crypto".into()), &gaps).len(), 2);
    assert_eq!(filter_tree(&tree, &ViewFilter::AssetClass("spot".into()), &gaps).len(), 2);
}

// ============================ gap viz (dm-gap-viz) ============================

#[test]
fn has_gaps_true_for_nonempty_list() {
    assert!(has_gaps(&[(0, 100)]));
}

#[test]
fn has_gaps_false_for_empty_list() {
    assert!(!has_gaps(&[]));
}

#[test]
fn filter_tree_has_gaps_empty_map_keeps_nothing() {
    let tree = tree_fixture();
    // No series has an entry in the gap map at all -> HasGaps keeps nothing.
    let filtered = filter_tree(&tree, &ViewFilter::HasGaps, &GapMap::new());
    assert!(filtered.is_empty(), "an empty GapMap must not accidentally keep every row");
}

#[test]
fn filter_tree_has_gaps_keeps_only_gapped_series() {
    let tree = tree_fixture();
    let mut gaps = GapMap::new();
    // binance/FRESH has a recorded gap; binance/STALE and okx/BTC-USDT do not (absent from
    // the map at all, mirroring "never fetched" / "no gaps found").
    gaps.insert(
        SeriesKey {
            venue: "binance".into(),
            symbol: "FRESH".into(),
            kind: "bar".into(),
            interval: Some("1m".into()),
        },
        vec![(10, 20)],
    );
    let filtered = filter_tree(&tree, &ViewFilter::HasGaps, &gaps);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].venue, "binance");
    assert_eq!(filtered[0].symbols.len(), 1);
    assert_eq!(filtered[0].symbols[0].symbol, "FRESH");
}

#[test]
fn filter_tree_has_gaps_ignores_series_with_empty_gap_list() {
    let tree = tree_fixture();
    let mut gaps = GapMap::new();
    // Present in the map but with an empty Vec -> same as "no gaps" (`has_gaps` is false).
    gaps.insert(
        SeriesKey {
            venue: "binance".into(),
            symbol: "FRESH".into(),
            kind: "bar".into(),
            interval: Some("1m".into()),
        },
        vec![],
    );
    let filtered = filter_tree(&tree, &ViewFilter::HasGaps, &gaps);
    assert!(filtered.is_empty());
}

#[test]
fn ts_range_to_x_fraction_maps_within_window() {
    let (f0, f1) = ts_range_to_x_fraction(200, 300, 0, 1000).unwrap();
    assert!((f0 - 0.2).abs() < 1e-6);
    assert!((f1 - 0.3).abs() < 1e-6);
}

#[test]
fn ts_range_to_x_fraction_clamps_outside_window() {
    let (f0, f1) = ts_range_to_x_fraction(-500, 2000, 0, 1000).unwrap();
    assert_eq!(f0, 0.0);
    assert_eq!(f1, 1.0);
}

#[test]
fn ts_range_to_x_fraction_degenerate_window_is_none() {
    assert_eq!(ts_range_to_x_fraction(0, 10, 0, 0), None);
}

// ============================ the design system (step 7) ============================

use vike_ui_theme::appearance::{self, Appearance};
use vike_ui_theme::components::{Status, Tokens};
use vike_ui_theme::metrics::Density;
use vike_ui_theme::theme::{Theme, ThemeId};

/// A context with `a` installed and one empty pass behind it, so the fonts are live.
fn installed(a: &Appearance) -> egui::Context {
    let ctx = egui::Context::default();
    appearance::install(&ctx, a);
    let _ = run_frame(&ctx, egui::RawInput::default(), |_| {});
    ctx
}

fn wide() -> egui::RawInput {
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
    egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() }
}

fn flatten(shape: egui::Shape, out: &mut Vec<egui::Shape>) {
    match shape {
        egui::Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, out)),
        s => out.push(s),
    }
}

/// Every shape one pass of `add` paints.
fn shapes_of(ctx: &egui::Context, add: impl FnMut(&mut egui::Ui)) -> Vec<egui::Shape> {
    let out = run_frame(ctx, wide(), add);
    let mut flat = Vec::new();
    for c in out.shapes {
        flatten(c.shape, &mut flat);
    }
    flat
}

fn fills(shapes: &[egui::Shape]) -> Vec<egui::Color32> {
    shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) => Some(r.fill),
            _ => None,
        })
        .collect()
}

fn texts(shapes: &[egui::Shape]) -> Vec<String> {
    shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

/// The values decision 4 chose: covered is the info blue, stale the warning amber, and a gap the
/// background it is a hole in. The bar is half a table row tall.
#[test]
fn coverage_style_is_info_blue_warning_amber_and_background_holes() {
    let ctx = installed(&Appearance::default());
    let t = Tokens::of(&ctx);
    let s = CoverageStyle::of(&t);
    assert_eq!(s.covered, Status::Info.color());
    assert_eq!(s.stale, Status::Warning.color());
    assert_eq!((s.gap, s.track), (t.theme.bg, t.theme.bg));
    assert_eq!(s.bar_h, (t.metrics.row_h * 0.5).round());
}

/// The bars paint covered and stale in `CoverageStyle`'s colours, and the legend paints its
/// swatches in the SAME two. So the legend cannot describe a bar the grid does not draw — which it
/// did on `main`: it said covered was blue and stale grey, while the bars were hover grey and amber.
#[test]
fn the_bar_and_its_legend_share_one_style() {
    for theme in ThemeId::ALL {
        let ctx = installed(&Appearance { theme, ..Appearance::default() });
        let style = CoverageStyle::of(&Tokens::of(&ctx));
        let (fresh, stale) = (cov(10, 1, 0, 990), cov(10, 1, 0, 100));
        let bars = fills(&shapes_of(&ctx, |ui| {
            paint_coverage_bar(ui, 140.0, style.bar_h, &fresh, 0, 1000, &[]);
            paint_coverage_bar(ui, 140.0, style.bar_h, &stale, 0, 1000, &[]);
        }));
        for (what, c) in
            [("track", style.track), ("covered", style.covered), ("stale", style.stale)]
        {
            assert!(bars.contains(&c), "{theme:?} bars carry no {what}: {bars:?}");
        }
        let legend = fills(&shapes_of(&ctx, coverage_legend));
        for (what, c) in [("covered", style.covered), ("stale", style.stale)] {
            assert!(legend.contains(&c), "{theme:?} legend has no {what} swatch: {legend:?}");
        }
    }
}

/// Which colours and words the coverage bar and its legend wear, pinned to what `CoverageStyle::of` and
/// `coverage_legend` painted before they moved to `ui-theme.toml`'s `coverage` map: the track and the gaps
/// the theme's background, covered the info blue, stale and the partial-day mark the warning amber — on every
/// theme — and the legend says "covered", "stale" and "partial day". Changing a row of the table changes the
/// bar, and this test is what says so by name.
#[test]
fn the_coverage_parts_wear_the_colours_and_words_the_coverage_map_gives_them() {
    for theme in ThemeId::ALL {
        let ctx = installed(&Appearance { theme, ..Appearance::default() });
        let t = Tokens::of(&ctx);
        let s = CoverageStyle::of(&t);
        assert_eq!((s.track, s.gap), (t.theme.bg, t.theme.bg), "{theme:?}: the track and the gaps");
        assert_eq!(s.covered, Status::Info.color(), "{theme:?}: covered");
        assert_eq!(s.stale, Status::Warning.color(), "{theme:?}: stale");
        let mark = maps::coverage::PARTIAL_DAY.colour.resolve(&t);
        assert_eq!(mark, Status::Warning.color(), "{theme:?}: the partial-day mark");
        let legend = texts(&shapes_of(&ctx, coverage_legend));
        for word in ["covered", "stale", "partial day", warning().as_str()] {
            assert!(
                legend.iter().any(|w| w == word),
                "{theme:?}: the legend says {word:?}: {legend:?}"
            );
        }
    }
}

/// The `coverage` map holds exactly the five parts the view draws — the bar's track, covered span, stale
/// span and gap cut-outs, and the partial-day mark — each with a colour (a part with none is not drawn),
/// and the mark is the warning icon.
#[test]
fn the_coverage_map_holds_the_five_parts_the_view_draws() {
    let keys: Vec<&str> = maps::coverage::ALL.iter().map(|r| r.key).collect();
    assert_eq!(keys, ["TRACK", "COVERED", "STALE", "GAP", "PARTIAL_DAY"]);
    for row in maps::coverage::ALL {
        assert_ne!(row.colour, vike_ui_theme::roles::ColourRole::None, "{}: no colour", row.key);
    }
    assert_eq!(maps::coverage::PARTIAL_DAY.icon(), Some(vike_ui_theme::icons::WARNING));
}

/// Rich-grid rows are the density's control height, because each holds a checkbox. The Studio's
/// plain picker's rows are the row height (owner decision 5). Measured as the pitch between the
/// coverage bars' tracks, one per row, in a venue of three rows.
#[test]
fn grid_rows_follow_the_density() {
    let tree = crate::model::build_tree(
        ["AAA", "BBB", "CCC"]
            .map(|s| {
                (
                    vike_data::SeriesId::per_symbol("bar", "binance", s, Some("1m".into())),
                    cov(10, 1, 0, 990),
                )
            })
            .into(),
    );
    for d in Density::ALL {
        let ctx = installed(&Appearance { density: d, ..Appearance::default() });
        let t = Tokens::of(&ctx);
        let style = CoverageStyle::of(&t);
        let pitch = |shapes: Vec<egui::Shape>| -> Vec<f32> {
            let mut tops: Vec<f32> = shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Rect(r)
                        if r.fill == style.track
                            && (r.rect.height() - style.bar_h).abs() < 0.01 =>
                    {
                        Some(r.rect.top())
                    }
                    _ => None,
                })
                .collect();
            tops.sort_by(f32::total_cmp);
            tops.windows(2).map(|w| w[1] - w[0]).collect()
        };
        let mut state = GridState::default();
        let rich = pitch(shapes_of(&ctx, |ui| {
            let _ = stored_catalog_grid(
                ui,
                &tree,
                &mut state,
                &GapMap::new(),
                &PartialDayMap::new(),
                None,
            );
        }));
        assert_eq!(rich, vec![t.metrics.control_h; 2], "{d:?} rich grid");
        let mut query = String::new();
        let plain = pitch(shapes_of(&ctx, |ui| {
            let _ = stored_catalog_ui(ui, &tree, &mut query);
        }));
        assert_eq!(plain, vec![t.metrics.row_h; 2], "{d:?} plain picker");
    }
}

/// An empty VIEW over a full store is not an empty store (the one-rail design's hazard 2; decision
/// 7). The grid says the view matched nothing — never that nothing is stored.
#[test]
fn an_empty_view_over_a_full_store_does_not_say_the_store_is_empty() {
    let ctx = installed(&Appearance::default());
    let mut state = GridState { active_view: ViewFilter::HasGaps, ..GridState::default() };
    let t = texts(&shapes_of(&ctx, |ui| {
        let _ = stored_catalog_grid(
            ui,
            &tree_fixture(),
            &mut state,
            &GapMap::new(),
            &PartialDayMap::new(),
            None,
        );
    }));
    assert!(t.iter().any(|s| s == EMPTY_VIEW), "{t:?}");
    assert!(!t.iter().any(|s| s == EMPTY_STORE), "{t:?}");
}

/// Save is the proxy row's primary action (spec §4.3): filled with the installed theme's accent.
#[test]
fn the_proxy_rows_save_is_primary() {
    for theme in ThemeId::ALL {
        let ctx = installed(&Appearance { theme, ..Appearance::default() });
        let mut edit = ProxyEdit::default();
        let f = fills(&shapes_of(&ctx, |ui| {
            let _ = polymarket_proxy_ui(ui, &mut edit);
        }));
        assert!(f.contains(&Theme::of(theme).accent), "{theme:?}: {f:?}");
    }
}
