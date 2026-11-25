//! **The Data Manager draws its series list the moment the inventory lands**, and the answers that
//! follow fill their cells in without moving a row.
//!
//! Each state is the shell's own fold: a `DataManagerInventory` fed the load's steps through `apply`, in
//! the order a slow link delivers them — the inventory, the gap probes one by one, the coverage
//! report, the whole load — and read by the window through `DataManagerInventoryContext::of`, as the desktop
//! reads it. The socket half (a scripted datahub whose probes are held) is
//! `crates/vike-app-core/src/data/data_manager/load_tests.rs`'s `the_tree_lands_before_any_gap_probe_answers`.

use std::collections::BTreeSet;

use egui::epaint::Shape;
use vike_app_core::backend::connection::BackendAction;
use vike_app_core::backend::editor::{BackendEditorState, BackendRegistryUpdate};
use vike_app_core::backend::registry::BackendRegistry;
use vike_app_core::data::data_manager::mode::DatahubCoverage;
use vike_app_core::data::data_manager::{
    DataManagerInventory, DataManagerInventoryLoadEvent, DataManagerInventoryLoadOutcome,
};
use vike_app_core::ui::windows::WindowView;
use vike_app_core::ui::windows::data_manager::rail::DataManagerRailCounts;
use vike_app_core::ui::windows::{
    BackendPickerSources, BackendSettingsEditState, BackendSettingsState,
    BackendSettingsWriteRequest, DataManagerDestination, DataManagerInventoryContext,
    data_tool_content,
};
use vike_data_manager::{CHECKING_MARK, DataManagerInventorySeriesKey};
use vike_ui_theme::appearance::{self, ThemeAppearance};

use crate::data_manager_screens::credentials_grids;
use crate::tool_context::Owned;

/// The datahub the window reads, as the shell resolves one.
const ADDRESS: &str = "127.0.0.1:17878";

/// The seeded store's three series, by the key a probe answers.
fn keys(owned: &Owned) -> BTreeSet<DataManagerInventorySeriesKey> {
    let mut out = BTreeSet::new();
    for v in &owned.tree {
        for s in &v.symbols {
            for r in &s.series {
                out.insert(DataManagerInventorySeriesKey::of(&v.venue, s, r));
            }
        }
    }
    out
}

/// A window on `destination` drawn over `catalog`, two passes (the second, once the layout settled, is the
/// one read), on one context and one window across every state, as the desktop draws it.
struct Window {
    context: egui::Context,
    tv: WindowView,
    owned: Owned,
    frame: u32,
}

impl Window {
    fn on(destination: DataManagerDestination) -> Self {
        let context = egui::Context::default();
        appearance::install(&context, &ThemeAppearance::default());
        // epaint panics on dropping an unapplied texture upload; a headless pass applies none.
        context.run_ui(raw(0), |_| {}).textures_delta.clear();
        let epoch = vike_app_core::backend::registry::device_key_epoch();
        let tv = WindowView {
            data_destination: destination,
            stored_auto_requested: Some(epoch),
            ..Default::default()
        };
        Window { context, tv, owned: Owned::seeded(), frame: 0 }
    }

    fn draw(&mut self, catalog: &DataManagerInventory) -> Vec<Shape> {
        let backends = BackendRegistry::default();
        let grids = credentials_grids();
        let settings = BackendSettingsState::Idle;
        let picker = BackendPickerSources { backends: &backends, active: None, reported: None };
        let mut action: Option<BackendAction> = None;
        let mut editor = BackendEditorState::Closed;
        let mut registry: Option<BackendRegistryUpdate> = None;
        let (mut refresh, mut edit, mut write) =
            (false, BackendSettingsEditState::Idle, None::<BackendSettingsWriteRequest>);
        let mut shapes = Vec::new();
        for _ in 0..2 {
            self.frame += 1;
            let mut tool_context = self.owned.context();
            tool_context.stored = DataManagerInventoryContext::of(catalog, Some(ADDRESS), "");
            let mut full = self.context.run_ui(raw(self.frame), |ui| {
                data_tool_content(
                    ui,
                    &tool_context,
                    &mut self.tv,
                    &grids,
                    &picker,
                    &mut action,
                    &mut editor,
                    &mut registry,
                    &settings,
                    &mut refresh,
                    &mut edit,
                    &mut write,
                )
            });
            full.textures_delta.clear();
            vike_ui_theme::frame_sanity::assert_frame_sane(&full);
            shapes = vike_ui_theme::painted::flatten(std::mem::take(&mut full.shapes));
        }
        shapes
    }
}

fn raw(frame: u32) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1500.0, 1000.0))),
        time: Some(f64::from(frame) / 60.0),
        ..Default::default()
    }
}

/// Where each series row's symbol and its Rows cell were painted, in paint order. The Rows cell sits
/// after the gap and Partial cells, so a cell that changed width as its answer landed moves it.
fn rows(owned: &Owned, shapes: &[Shape]) -> Vec<(String, egui::Pos2)> {
    let row_count =
        vike_ui_theme::fmt::format_count(owned.tree[0].symbols[0].series[0].coverage.rows);
    let mut cells: BTreeSet<&str> =
        owned.tree.iter().flat_map(|v| &v.symbols).map(|s| s.symbol.as_str()).collect();
    cells.insert(&row_count);
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Text(t) if cells.contains(t.galley.text()) => {
                Some((t.galley.text().to_string(), t.pos))
            }
            _ => None,
        })
        .collect()
}

/// How many cells carry the checking mark.
fn checking(shapes: &[Shape]) -> usize {
    shapes
        .iter()
        .filter(|s| matches!(s, Shape::Text(t) if t.galley.text() == CHECKING_MARK))
        .count()
}

/// Every word painted, joined.
fn said(shapes: &[Shape]) -> String {
    let texts = shapes.iter().filter_map(|s| match s {
        Shape::Text(t) => Some(t.galley.text().to_string()),
        _ => None,
    });
    texts.collect::<Vec<_>>().join(" ")
}

/// A running load whose inventory has just landed: the tree, every probe still out.
fn listed(owned: &Owned) -> DataManagerInventory {
    let mut catalog = DataManagerInventory::default();
    catalog.loading = true;
    catalog.apply(DataManagerInventoryLoadEvent::Listed(owned.tree.clone(), keys(owned)));
    catalog
}

/// The whole load, as `load_stored` returns it.
fn whole(
    owned: &Owned,
    gaps: vike_data_manager::DataManagerInventoryGapMap,
    coverage: DatahubCoverage,
) -> DataManagerInventoryLoadOutcome {
    let partials = match coverage {
        DatahubCoverage::Served => owned.partials.clone(),
        _ => Default::default(),
    };
    DataManagerInventoryLoadOutcome {
        tree: owned.tree.clone(),
        gaps,
        partials,
        coverage,
        error: None,
        history: None,
    }
}

/// **The list draws before any probe answers, and the answers fill it in without moving a row.**
/// Right after the inventory, All series lists every series with each gap cell and each Partial cell
/// marked `checking…`, and the Has-gaps count is not drawn; probe by probe, then the coverage report,
/// then the whole load, the marks go and every row stays exactly where it was first drawn.
#[test]
fn the_list_draws_at_the_inventory_and_no_row_moves_as_the_answers_land() {
    let mut window = Window::on(DataManagerDestination::AllSeries);
    let owned = Owned::seeded();
    let series = keys(&owned);
    let mut catalog = listed(&owned);

    let first = window.draw(&catalog);
    let drawn = rows(&owned, &first);
    // Two cells a row: its symbol and its Rows cell.
    assert_eq!(drawn.len(), 2 * series.len(), "every series is listed before any probe answers");
    assert_eq!(
        checking(&first),
        2 * series.len(),
        "each row's gap and Partial cells say checking…"
    );
    assert!(said(&first).contains("checking 3 series for gaps"), "the foot says what is still out");
    let mut listed_context = window.owned.context();
    listed_context.stored = DataManagerInventoryContext::of(&catalog, Some(ADDRESS), "");
    assert!(listed_context.stored.gaps_counting() && listed_context.stored.partials_counting());
    let counts = DataManagerRailCounts::from_context(&listed_context);
    assert!(counts.gaps_counting, "the rail draws no Has-gaps count while it is counted");
    assert_eq!(counts.series, series.len(), "…and the series count at once");

    let mut marks = Vec::new();
    for key in &series {
        catalog.apply(DataManagerInventoryLoadEvent::Probed(
            key.clone(),
            owned.gaps.get(key).cloned(),
            None,
        ));
        let shapes = window.draw(&catalog);
        assert_eq!(rows(&owned, &shapes), drawn, "a probe's answer moved a row");
        marks.push(checking(&shapes));
    }
    assert_eq!(marks, [5, 4, 3], "one gap cell fills in per answer; the Partial cells still check");
    assert!(
        !DataManagerInventoryContext::of(&catalog, Some(ADDRESS), "").gaps_counting(),
        "the gap count is final"
    );

    catalog.apply(DataManagerInventoryLoadEvent::Covered(
        owned.partials.clone(),
        DatahubCoverage::Served,
        None,
    ));
    let covered = window.draw(&catalog);
    assert_eq!(rows(&owned, &covered), drawn, "the coverage report moved a row");
    assert_eq!(checking(&covered), 0, "every cell is answered");

    catalog.apply(DataManagerInventoryLoadEvent::Done(whole(
        &owned,
        owned.gaps.clone(),
        DatahubCoverage::Served,
    )));
    assert!(!catalog.loading);
    let done = window.draw(&catalog);
    assert_eq!(rows(&owned, &done), drawn, "the whole load moved a row");
    assert_eq!(checking(&done), 0);
}

/// **A failed gap probe and an unread coverage report cost their columns, never the list**: the
/// rows stay where they were drawn, the failed series reads not known, the strip names each
/// column that could not be filled, and the inspector gives the cause.
#[test]
fn a_failed_pass_marks_its_column_and_leaves_the_list_in_place() {
    let mut window = Window::on(DataManagerDestination::AllSeries);
    let owned = Owned::seeded();
    let series = keys(&owned);
    let failed = series.iter().next().expect("the seeded store holds series").clone();
    window.tv.stored_last_selection = Some(vike_data_manager::DataManagerInventorySeriesKey {
        venue: failed.venue.clone(),
        symbol: failed.symbol.clone(),
        kind: failed.kind,
        interval: failed.interval.clone(),
        grouped: failed.grouped,
        source: failed.source.clone(),
    });
    let mut catalog = listed(&owned);
    let drawn = rows(&owned, &window.draw(&catalog));

    let mut gaps = vike_data_manager::DataManagerInventoryGapMap::new();
    for key in &series {
        let (entry, cause) = if *key == failed {
            (Some(Vec::new()), Some("planted probe refusal".to_string()))
        } else {
            (None, None)
        };
        if let Some(ranges) = &entry {
            gaps.insert(key.clone(), ranges.clone());
        }
        catalog.apply(DataManagerInventoryLoadEvent::Probed(key.clone(), entry, cause));
    }
    let cause = Some("planted report failure".to_string());
    catalog.apply(DataManagerInventoryLoadEvent::Covered(
        Default::default(),
        DatahubCoverage::Unserved,
        cause,
    ));
    let answered = window.draw(&catalog);
    catalog.apply(DataManagerInventoryLoadEvent::Done(whole(
        &owned,
        gaps,
        DatahubCoverage::Unserved,
    )));
    let done = window.draw(&catalog);

    for shapes in [&answered, &done] {
        assert_eq!(rows(&owned, shapes), drawn, "a failed pass moved or dropped a row");
        assert_eq!(checking(shapes), 0, "a failed answer is an answer: nothing still checks");
        let words = said(shapes);
        assert!(words.contains("Partial column unavailable"), "the strip: {words}");
        assert!(words.contains("Gaps not known for 1 series"), "the strip: {words}");
        assert!(words.contains("Cause: planted probe refusal"), "the inspector: {words}");
    }
    assert_eq!(catalog.progress.coverage_cause.as_deref(), Some("planted report failure"));
}

/// **The counts that need a pass are not drawn until it is done**: Overview's gap and partial tiles
/// say `counting…` and hold the attention rows back, so no row lands above another under the pointer.
#[test]
fn the_overview_counts_wait_for_their_pass() {
    let mut window = Window::on(DataManagerDestination::Overview);
    let owned = Owned::seeded();
    let mut catalog = listed(&owned);
    let counting = said(&window.draw(&catalog));
    assert!(counting.contains("counting\u{2026}"), "Overview: {counting}");
    assert!(counting.contains("Still counting"), "the attention list waits: {counting}");

    for key in keys(&owned) {
        catalog.apply(DataManagerInventoryLoadEvent::Probed(
            key.clone(),
            owned.gaps.get(&key).cloned(),
            None,
        ));
    }
    catalog.apply(DataManagerInventoryLoadEvent::Covered(
        owned.partials.clone(),
        DatahubCoverage::Served,
        None,
    ));
    let counted = said(&window.draw(&catalog));
    assert!(!counted.contains("counting\u{2026}"), "every count is final: {counted}");
    assert!(counted.contains("SERIES WITH GAPS"), "{counted}");
}
