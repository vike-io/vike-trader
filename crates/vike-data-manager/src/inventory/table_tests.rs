//! Unit tests of the shared rows and cells: the coverage label, the sort and the bar's x mapping, and,
//! over real egui frames, the coverage bar, its legend and one row's cells.

use std::collections::HashMap;

use vike_model::StoreKindId;
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::components::ThemeStatus;
use vike_ui_theme::theme::ThemePaletteId;

use super::*;
use crate::inventory::filter::{DataManagerInventoryGapMap, gaps_unknown, has_gaps};
use crate::test_support::{coverage, fills, installed, key, shapes_of, texts, warning};

fn row(symbol: &str, kind: &str, iv: Option<&str>, c: SeriesCoverage) -> DataManagerInventoryRow {
    DataManagerInventoryRow {
        symbol: symbol.into(),
        kind: kind.parse().unwrap(),
        interval: iv.map(Into::into),
        coverage: c,
        grouped: false,
        source: None,
    }
}

#[test]
fn coverage_label_shows_rows_and_span() {
    let coverage = SeriesCoverage {
        first_timestamp: 0,
        last_timestamp: 86_400_000,
        rows: 1234,
        bytes: 0,
        parts: 1,
        dates: 2,
    };
    let s = coverage_label(&coverage);
    assert!(s.contains("1234") || s.contains("1,234"));
    assert!(s.contains("1970")); // epoch-derived date rendered
    assert!(!s.contains("panicked"));
}

#[test]
fn coverage_label_zero_rows_is_safe() {
    let coverage = SeriesCoverage {
        first_timestamp: 0,
        last_timestamp: 0,
        rows: 0,
        bytes: 0,
        parts: 0,
        dates: 0,
    };
    let _ = coverage_label(&coverage); // must not panic on an empty series
}

// `format_count`/`format_count_compact` are `vike_ui_theme::fmt`'s, pinned by that module's own tests.

#[test]
fn sort_by_rows_descending_orders_largest_first() {
    let mut rows = vec![
        row("AAA", "bar", Some("1m"), coverage(100, 10, 0, 10)),
        row("BBB", "bar", Some("1m"), coverage(300, 10, 0, 10)),
        row("CCC", "bar", Some("1m"), coverage(200, 10, 0, 10)),
    ];
    sort_flat_rows(
        &mut rows,
        DataManagerInventorySort { column: DataManagerInventorySortColumn::Rows, ascending: false },
    );
    let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["BBB", "CCC", "AAA"]);
}

#[test]
fn sort_by_symbol_asc_orders_alphabetically() {
    let mut rows = vec![
        row("ZZZ", "bar", None, coverage(1, 1, 0, 1)),
        row("AAA", "bar", None, coverage(1, 1, 0, 1)),
        row("MMM", "bar", None, coverage(1, 1, 0, 1)),
    ];
    sort_flat_rows(
        &mut rows,
        DataManagerInventorySort {
            column: DataManagerInventorySortColumn::Symbol,
            ascending: true,
        },
    );
    let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["AAA", "MMM", "ZZZ"]);
}

#[test]
fn sort_by_updated_descending_orders_most_recent_first() {
    let mut rows = vec![
        row("AAA", "bar", Some("1m"), coverage(1, 1, 0, 500)),
        row("BBB", "bar", Some("1m"), coverage(1, 1, 0, 9000)),
        row("CCC", "bar", Some("1m"), coverage(1, 1, 0, 4000)),
    ];
    sort_flat_rows(
        &mut rows,
        DataManagerInventorySort {
            column: DataManagerInventorySortColumn::Updated,
            ascending: false,
        },
    );
    let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["BBB", "CCC", "AAA"]);
}

#[test]
fn sort_ties_break_deterministically_by_symbol_then_kind() {
    let mut rows = vec![
        row("AAA", "trade", None, coverage(50, 1, 0, 1)),
        row("AAA", "bar", Some("1m"), coverage(50, 1, 0, 1)),
    ];
    sort_flat_rows(
        &mut rows,
        DataManagerInventorySort { column: DataManagerInventorySortColumn::Rows, ascending: true },
    );
    // equal Rows -> falls back to symbol (equal) then kind_label ("bar/1m" < "trade").
    assert_eq!(rows[0].kind, StoreKindId::Bar);
    assert_eq!(rows[1].kind, StoreKindId::Trade);
}

#[test]
fn timestamp_range_to_x_fraction_maps_within_window() {
    let (f0, f1) = timestamp_range_to_x_fraction(200, 300, 0, 1000).unwrap();
    assert!((f0 - 0.2).abs() < 1e-6);
    assert!((f1 - 0.3).abs() < 1e-6);
}

#[test]
fn timestamp_range_to_x_fraction_clamps_outside_window() {
    let (f0, f1) = timestamp_range_to_x_fraction(-500, 2000, 0, 1000).unwrap();
    assert_eq!(f0, 0.0);
    assert_eq!(f1, 1.0);
}

#[test]
fn timestamp_range_to_x_fraction_degenerate_window_is_none() {
    assert_eq!(timestamp_range_to_x_fraction(0, 10, 0, 0), None);
}

// ============================ the design system (step 7) ============================

/// The values decision 4 chose: covered is the info blue, stale the warning amber, and a gap the
/// background it is a hole in. The bar is half a table row tall.
#[test]
fn coverage_style_is_info_blue_warning_amber_and_background_holes() {
    let context = installed(&ThemeAppearance::default());
    let t = ThemeTokens::of(&context);
    let s = DataManagerInventoryCoverageStyle::of(&t);
    assert_eq!(s.covered, ThemeStatus::Info.color());
    assert_eq!(s.stale, ThemeStatus::Warning.color());
    assert_eq!((s.gap, s.track), (t.theme.background, t.theme.background));
    assert_eq!(s.bar_h, (t.metrics.row_height * 0.5).round());
}

/// ⚠ A series whose gap probe failed is not gap-free: its row marks the bar, the count says how many,
/// and it is not a series with gaps either. A series the map does not list, or that lists real holes,
/// has no mark.
#[test]
fn a_failed_gap_probe_never_reads_as_gap_free() {
    let context = installed(&ThemeAppearance::default());
    let marks = |gaps: Option<&[(i64, i64)]>| {
        let r = row("BTCUSDT", "bar", Some("1m"), coverage(10, 1, 0, 990));
        let shapes =
            shapes_of(&context, |ui| data_row_cells(ui, &r, 0, 1000, gaps, None, (false, false)));
        shapes
            .iter()
            .filter(|s| matches!(s, egui::Shape::Text(t) if t.galley.text() == "?"))
            .count()
    };
    assert_eq!(marks(Some(&[])), 1, "the probe failed");
    assert_eq!(marks(None), 0, "answered whole");
    assert_eq!(marks(Some(&[(100, 200)])), 0, "real holes are painted as holes");

    let gaps: DataManagerInventoryGapMap =
        HashMap::from([(key("binance", "A"), Vec::new()), (key("binance", "B"), vec![(100, 200)])]);
    assert_eq!(gaps_unknown(&gaps), 1);
    assert!(!has_gaps(&gaps[&key("binance", "A")]), "not known is not a hole either");
}

/// A row whose gap probe or coverage report has not answered yet carries [`CHECKING_MARK`] in that
/// cell, and the cells after it sit exactly where they sit once the answer lands.
#[test]
fn a_pending_answer_marks_its_cell_and_moves_nothing() {
    let context = installed(&ThemeAppearance::default());
    let r = row("BTCUSDT", "bar", Some("1m"), coverage(10, 1, 0, 990));
    // Drawn as `inventory_grid` draws a row: a rect of the control height, its cells laid left to
    // right in a child `Ui` over it. Each state is drawn twice and the settled second pass is read.
    let row_h = ThemeTokens::of(&context).metrics.control_height;
    let frame = |checking: (bool, bool)| {
        let mut draw = |ui: &mut egui::Ui| {
            let size = egui::vec2(ui.available_width(), row_h);
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            let mut row_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            data_row_cells(&mut row_ui, &r, 0, 1000, None, Some(""), checking);
        };
        let _ = shapes_of(&context, &mut draw);
        let shapes = shapes_of(&context, &mut draw);
        let marks = texts(&shapes).iter().filter(|s| *s == CHECKING_MARK).count();
        let rows_at = shapes.iter().find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == "10" => Some(t.pos),
            _ => None,
        });
        (marks, rows_at)
    };
    let (both, rows_pending) = frame((true, true));
    let (none, rows_answered) = frame((false, false));
    assert_eq!((both, none), (2, 0), "one mark per pending cell, none once answered");
    assert_eq!(frame((true, false)).0, 1);
    assert!(rows_pending.is_some(), "the Rows cell is painted");
    assert_eq!(rows_pending, rows_answered, "the answer lands without moving the row's cells");
}

/// The bars paint covered and stale in `DataManagerInventoryCoverageStyle`'s colours, and the legend paints its
/// swatches in the SAME two. So the legend cannot describe a bar the grid does not draw — which it
/// did on `main`: it said covered was blue and stale grey, while the bars were hover grey and amber.
#[test]
fn the_bar_and_its_legend_share_one_style() {
    for theme in ThemePaletteId::ALL {
        let context = installed(&ThemeAppearance { theme, ..ThemeAppearance::default() });
        let style = DataManagerInventoryCoverageStyle::of(&ThemeTokens::of(&context));
        let (fresh, stale) = (coverage(10, 1, 0, 990), coverage(10, 1, 0, 100));
        let bars = fills(&shapes_of(&context, |ui| {
            paint_coverage_bar(ui, 140.0, style.bar_h, &fresh, 0, 1000, &[]);
            paint_coverage_bar(ui, 140.0, style.bar_h, &stale, 0, 1000, &[]);
        }));
        for (what, c) in
            [("track", style.track), ("covered", style.covered), ("stale", style.stale)]
        {
            assert!(bars.contains(&c), "{theme:?} bars carry no {what}: {bars:?}");
        }
        let legend = fills(&shapes_of(&context, coverage_legend));
        for (what, c) in [("covered", style.covered), ("stale", style.stale)] {
            assert!(legend.contains(&c), "{theme:?} legend has no {what} swatch: {legend:?}");
        }
    }
}

/// Which colours and words the coverage bar and its legend wear, pinned to what `DataManagerInventoryCoverageStyle::of` and
/// `coverage_legend` painted before they moved to `ui-theme.toml`'s `coverage` map: the track and the gaps
/// the theme's background, covered the info blue, stale and the partial-day mark the warning amber — on every
/// theme — and the legend says "covered", "stale" and "partial day". Changing a row of the table changes the
/// bar, and this test is what says so by name.
#[test]
fn the_coverage_parts_wear_the_colours_and_words_the_coverage_map_gives_them() {
    for theme in ThemePaletteId::ALL {
        let context = installed(&ThemeAppearance { theme, ..ThemeAppearance::default() });
        let t = ThemeTokens::of(&context);
        let s = DataManagerInventoryCoverageStyle::of(&t);
        assert_eq!(
            (s.track, s.gap),
            (t.theme.background, t.theme.background),
            "{theme:?}: the track and the gaps"
        );
        assert_eq!(s.covered, ThemeStatus::Info.color(), "{theme:?}: covered");
        assert_eq!(s.stale, ThemeStatus::Warning.color(), "{theme:?}: stale");
        let mark = maps::coverage::PARTIAL_DAY.colour.resolve(&t);
        assert_eq!(mark, ThemeStatus::Warning.color(), "{theme:?}: the partial-day mark");
        let legend = texts(&shapes_of(&context, coverage_legend));
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
        assert_ne!(
            row.colour,
            vike_ui_theme::roles::ThemeColourRole::None,
            "{}: no colour",
            row.key
        );
    }
    assert_eq!(maps::coverage::PARTIAL_DAY.icon(), Some(vike_ui_theme::icons::WARNING));
}
