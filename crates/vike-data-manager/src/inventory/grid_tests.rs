//! Unit tests of the rich grid: the selection and its "select all (filtered)", the bulk bar driven by
//! a real pointer, the cross-kind Partial column read off the frame's shapes, the row pitch per
//! density and the empty-view sentence.

use vike_model::StoreKindId;
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::metrics::ThemeDensity;

use super::*;
use crate::inventory::list::inventory_list;
use crate::inventory::table::DataManagerInventoryCoverageStyle;
use crate::test_support::{
    coverage, installed, key, run_frame, shapes_of, texts, tree_fixture, warning,
};

fn venue_fixture(venue: &str, symbols: &[(&str, u64, i64, i64)]) -> DataManagerInventoryVenue {
    use crate::inventory::{
        DataManagerInventoryRollup, DataManagerInventorySeries, DataManagerInventorySymbol,
    };
    let symbols = symbols
        .iter()
        .map(|(symbol_name, rows, first, last)| DataManagerInventorySymbol {
            symbol: (*symbol_name).to_string(),
            grouped: false,
            series: vec![DataManagerInventorySeries {
                kind: StoreKindId::Bar,
                interval: Some("1m".into()),
                source: None,
                coverage: coverage(*rows, 1, *first, *last),
            }],
            total: DataManagerInventoryRollup::default(),
        })
        .collect();
    DataManagerInventoryVenue {
        venue: venue.to_string(),
        symbols,
        total: DataManagerInventoryRollup::default(),
    }
}

/// A grouped and a per-symbol series that share a label, kind and interval are TWO rows with two
/// keys: one checkbox does not tick both, and a gap recorded for one is not painted on the other
/// nor lists it in Has gaps. Their key used to drop `grouped` (and `source`), so both rows answered
/// to one key.
#[test]
fn a_grouped_and_a_per_symbol_series_with_one_label_keep_two_keys() {
    use vike_data::SeriesId;
    let grouped = SeriesId::grouped(StoreKindId::Quote, "polymarket", "btc-5m");
    let single = SeriesId::per_symbol(StoreKindId::Quote, "polymarket", "btc-5m", None);
    let tree = crate::inventory::build_inventory_tree(vec![
        (grouped, coverage(10, 1, 0, 10)),
        (single, coverage(20, 1, 0, 10)),
    ]);
    let keys = visible_keys(&tree, "");
    assert_eq!(keys.len(), 2, "two rows");
    assert_ne!(keys[0], keys[1], "two keys, so two checkboxes: {keys:?}");

    let mut selected = BTreeSet::new();
    set_selected(&mut selected, keys[0].clone(), true);
    assert!(!selected.contains(&keys[1]), "ticking one row selects only that row");

    let gapped = keys.iter().find(|k| k.grouped).expect("the grouped row").clone();
    let gaps = DataManagerInventoryGapMap::from([(gapped, vec![(2, 4)])]);
    let filtered = filter_tree(&tree, &DataManagerInventoryFilter::HasGaps, &gaps);
    let kept: Vec<bool> = filtered[0].symbols.iter().map(|n| n.grouped).collect();
    assert_eq!(kept, vec![true], "only the grouped series has the gap");
}

// ============================ DataManagerInventoryGridState selection (commit 1) ============================

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
// proving the click really does resolve to `DataManagerInventoryGridBulkAction::Delete`, not just that the enum
// exists.

fn bulk_bar_screen() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0))
}

/// Press and release the bulk bar's button `button` (an index into the rects it returns) over three
/// real frames, with `selected` rows chosen and `delete_disabled` as given: frame 0 probes the
/// button's rect (no pointer interaction), frame 1 moves onto it and presses, frame 2 releases over
/// the same position. Returns the probe frame's rects and the action the release frame resolved.
fn click_bulk_button(
    selected: usize,
    delete_disabled: Option<&str>,
    button: usize,
) -> ([egui::Rect; 3], Option<DataManagerInventoryGridBulkAction>) {
    let context = egui::Context::default();
    let screen = bulk_bar_screen();

    // Frame 0: probe — a disabled button still owns a rect to aim the click at.
    let mut rects = [egui::Rect::NOTHING; 3];
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    let _ = run_frame(&context, raw, |ui| {
        let (_, r) = bulk_action_bar(ui, selected, delete_disabled);
        rects = r;
    });
    let screen_position = rects[button].center();

    // Frame 1: move onto + press the button.
    let mut raw =
        egui::RawInput { screen_rect: Some(screen), time: Some(1.0 / 60.0), ..Default::default() };
    raw.events.push(egui::Event::PointerMoved(screen_position));
    raw.events.push(egui::Event::PointerButton {
        pos: screen_position,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    let _ = run_frame(&context, raw, |ui| {
        let _ = bulk_action_bar(ui, selected, delete_disabled);
    });

    // Frame 2: release over the same position -> a real click (disabled: one that must not resolve).
    let mut raw =
        egui::RawInput { screen_rect: Some(screen), time: Some(2.0 / 60.0), ..Default::default() };
    raw.events.push(egui::Event::PointerButton {
        pos: screen_position,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    let mut action = None;
    let _ = run_frame(&context, raw, |ui| {
        let (a, _) = bulk_action_bar(ui, selected, delete_disabled);
        action = a;
    });
    (rects, action)
}

#[test]
fn bulk_bar_delete_button_click_sets_delete_action() {
    let (_, action) = click_bulk_button(3, None, 2);
    assert_eq!(action, Some(DataManagerInventoryGridBulkAction::Delete));
}

/// The mode gate (the #1378 seam close): with `delete_disabled = Some(reason)` the Delete button
/// is GRAYED, so the same press+release that resolves to `DataManagerInventoryGridBulkAction::Delete` above must yield NO
/// action — this seat sends no delete of its own and has no store of its own, and a
/// clickable button that silently no-ops is the dishonest variant.
///
/// The `reason` string is arbitrary here — this test drives the GATE, not the wording — but it
/// is kept in step with `vike_app_core::data::data_manager::mode`'s `DELETE_LOCAL_ONLY` so a reader does not
/// meet a retired sentence. It read "delete is a local-store operation" until the 2026-09-07 wire
/// delete verb made that false (see `bulk_action_bar`'s doc), and "Delete is local-store only here"
/// until 2026-11-12, when the no-datahub arm stopped granting Delete too, and "this app holds no datahub
/// keys" until 2026-11-16, when every datahub dial had signed with the device key for a day.
#[test]
fn bulk_bar_disabled_delete_click_yields_no_action() {
    const REASON: Option<&str> = Some(
        "Delete is not built into the desktop: remove a series on the server with `vike-cli data history rm`",
    );
    let (rects, action) = click_bulk_button(3, REASON, 2);
    assert!(rects[2] != egui::Rect::NOTHING, "the grayed Delete button still renders");
    assert_eq!(action, None, "a disabled Delete must not produce an action");
}

#[test]
fn bulk_bar_backfill_button_click_sets_backfill_action() {
    let (_, action) = click_bulk_button(1, None, 0);
    assert_eq!(action, Some(DataManagerInventoryGridBulkAction::Backfill));
}

#[test]
fn bulk_bar_no_click_reports_no_action() {
    let context = egui::Context::default();
    let screen = bulk_bar_screen();
    let mut action = None;
    for f in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let _ = run_frame(&context, raw, |ui| {
            let (a, _) = bulk_action_bar(ui, 2, None);
            action = a;
        });
    }
    assert_eq!(action, None);
}

// ======================= the cross-kind Partial column (data-manager-partial-cell) =====================
//
// Real egui frames, same shape as the `bulk_bar_*` interaction tests above: render the actual
// grid and read the glyphs back out of the frame's shapes, so these prove the marker REACHES
// THE SCREEN — not merely that `partial_days_label` returns a string.

/// Every piece of text painted in one frame of `inventory_grid` over `tree`/`partials`.
fn grid_frame_text(
    tree: &[DataManagerInventoryVenue],
    partials: &DataManagerInventoryPartialDayMap,
) -> Vec<String> {
    let context = egui::Context::default();
    // The app's type, not egui's: the partial column draws an icon, and the icon family exists
    // only in the bundled set — egui's default fonts do not bind it, and epaint panics.
    vike_ui_theme::appearance::install_type(
        &context,
        vike_ui_theme::type_scale::ThemeTextSize::default(),
    );
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    let mut state = DataManagerInventoryGridState::default();
    let out = run_frame(&context, raw, |ui| {
        let _ = inventory_grid(
            ui,
            tree,
            &mut state,
            &DataManagerInventoryGapMap::new(),
            partials,
            None,
            &BTreeSet::new(),
            false,
        );
    });
    out.shapes
        .into_iter()
        .filter_map(|cs| match cs.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

fn partial_fixture(
    venue: &str,
    symbol: &str,
    missing: &[&str],
) -> DataManagerInventoryPartialDayMap {
    let key = vike_data::InstrumentCoverageKey {
        venue: venue.into(),
        label: symbol.into(),
        grouped: false,
    };
    let day = vike_data::InstrumentCoveragePartialDay {
        day: 20_666, // ~2026-08-01; the column shows a COUNT, so the exact day is irrelevant
        missing_kinds: missing.iter().map(|s| s.parse().unwrap()).collect(),
    };
    DataManagerInventoryPartialDayMap::from([(key, vec![day])])
}

/// The column header exists whether or not anything is partial — it is the affordance that
/// tells you the check was RUN, which a blank column alone would not.
#[test]
fn partial_column_header_renders_with_an_empty_map() {
    let text = grid_frame_text(&tree_fixture(), &DataManagerInventoryPartialDayMap::new());
    assert!(text.iter().any(|t| *t == warning()), "header icon missing: {text:?}");
    assert!(text.iter().any(|t| t == "Coverage"), "grid did not render at all: {text:?}");
}

/// An empty map paints the header icon and NOTHING else — one warning total, no row markers.
#[test]
fn no_partial_days_marks_no_rows() {
    let text = grid_frame_text(&tree_fixture(), &DataManagerInventoryPartialDayMap::new());
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
    let tree = crate::inventory::build_inventory_tree(vec![(
        SeriesId::grouped(StoreKindId::Trade, "binance", "FRESH"),
        coverage(10, 1, 0, 990),
    )]);
    let text = grid_frame_text(&tree, &partial_fixture("binance", "FRESH", &["quote"]));
    assert_eq!(
        text.iter().filter(|t| **t == warning()).count(),
        1,
        "a grouped row matched a per-symbol partial key: {text:?}"
    );
}

/// Rich-grid rows are the density's control height, because each holds a checkbox. The Studio's
/// plain picker's rows are the row height (owner decision 5). Measured as the pitch between the
/// coverage bars' tracks, one per row, in a venue of three rows.
#[test]
fn grid_rows_follow_the_density() {
    let tree = crate::inventory::build_inventory_tree(
        ["AAA", "BBB", "CCC"]
            .map(|s| {
                (
                    vike_data::SeriesId::per_symbol(
                        StoreKindId::Bar,
                        "binance",
                        s,
                        Some("1m".into()),
                    ),
                    coverage(10, 1, 0, 990),
                )
            })
            .into(),
    );
    for d in ThemeDensity::ALL {
        let context = installed(&ThemeAppearance { density: d, ..ThemeAppearance::default() });
        let t = ThemeTokens::of(&context);
        let style = DataManagerInventoryCoverageStyle::of(&t);
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
        let mut state = DataManagerInventoryGridState::default();
        let rich = pitch(shapes_of(&context, |ui| {
            let _ = inventory_grid(
                ui,
                &tree,
                &mut state,
                &DataManagerInventoryGapMap::new(),
                &DataManagerInventoryPartialDayMap::new(),
                None,
                &BTreeSet::new(),
                false,
            );
        }));
        assert_eq!(rich, vec![t.metrics.control_height; 2], "{d:?} rich grid");
        let mut query = String::new();
        let plain = pitch(shapes_of(&context, |ui| {
            let _ = inventory_list(ui, &tree, &mut query);
        }));
        assert_eq!(plain, vec![t.metrics.row_height; 2], "{d:?} plain picker");
    }
}

/// An empty VIEW over a full store is not an empty store (the one-rail design's hazard 2; decision
/// 7). The grid says the view matched nothing — never that nothing is stored.
#[test]
fn an_empty_view_over_a_full_store_does_not_say_the_store_is_empty() {
    let context = installed(&ThemeAppearance::default());
    let mut state = DataManagerInventoryGridState {
        active_view: DataManagerInventoryFilter::HasGaps,
        ..DataManagerInventoryGridState::default()
    };
    let t = texts(&shapes_of(&context, |ui| {
        let _ = inventory_grid(
            ui,
            &tree_fixture(),
            &mut state,
            &DataManagerInventoryGapMap::new(),
            &DataManagerInventoryPartialDayMap::new(),
            None,
            &BTreeSet::new(),
            false,
        );
    }));
    assert!(t.iter().any(|s| s == EMPTY_VIEW), "{t:?}");
    assert!(!t.iter().any(|s| s == EMPTY_STORE), "{t:?}");
}
