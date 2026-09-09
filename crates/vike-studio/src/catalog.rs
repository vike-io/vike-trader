//! The Studio's catalog REFRESH, walked OFF the egui paint thread.
//!
//! The toolbar's ⟳ Refresh used to do this inline, in the frame, and it was FOUR full catalog
//! walks deep: `SlicePicker::refresh` called `vike_studio_core::bar_series`, then `tick_series`,
//! then `depth_only_series` — three separate `HistStore::list_series` calls over one catalog — and
//! then `DataBrowserPane::refresh` called `inventory()`, which is one manifest open per series
//! plus a `std::fs::metadata` per part. Every one of them ran between two frames.
//!
//! ⚠ **It is not merely local file I/O.** Since split-plane B12 the Studio holds its store as
//! `StoreHandle = Arc<dyn HistStore + Send + Sync>`, and `crates/vike-app/src/main.rs`'s
//! `open_studio_store` makes that a `vike_datahub_client::RemoteHistStore` whenever a datahub
//! address resolves — a store that is connect-per-read. So on a remote session those four walks
//! were four fresh TCP connects, serially, on the thread that has to paint the next frame; an
//! unreachable server paid each one's connect timeout before the UI moved again.
//!
//! **The pattern here is not new and is deliberately not invented.**
//! `crates/vike-app-core/src/stored_load.rs`'s `load_stored_tree` is the Data Manager's twin of this
//! module — the walk, spelled over the trait, with the spawn and the latch in
//! `crates/vike-app/src/app_methods.rs`'s `refresh_stored` and the "Loading…" state on
//! `crates/vike-app/src/app_ui.rs`'s `stored_loading`. This is the same discipline for the
//! Studio: [`load_catalog`] is the walk (pure of egui, so CI runs it), [`spawn_catalog_load`] puts
//! it on a worker thread, and `StudioState::poll` folds the single [`CatalogLoad`] message in
//! through the two panes' `apply` methods on a later frame.
//!
//! **What it deliberately does NOT do:**
//!
//! - It does **not cache**. Refresh means re-read; a cache would be a second answer that can
//!   disagree with the store, which is the defect the ⟳ button exists to fix.
//! - It does **not merge the two failures**. `series` and `inventory` are separate `Result`s in
//!   one message because they are separate reads of separate verbs and each pane renders its own
//!   outcome. A mixed outcome is REAL, not hypothetical: the `RemoteHistStore` above connects per
//!   read, so a server that goes away between the two verbs answers `list_series` and refuses
//!   `inventory`; and `crates/vike-data/src/datafusion_hist.rs`'s `inventory` folds a
//!   `series_coverage` per listed series, so one unreadable manifest refuses the inventory of a
//!   store whose series list just answered. Collapsing the two would render a populated combo
//!   beside a Data pane that claims the whole scan failed — or the reverse. (⚠ The fixture
//!   `crates/vike-studio/tests/common/mod.rs`'s `RefusingCatalogStore` is NOT such a store: it
//!   overrides `list_series` only, and `crates/vike-data/src/hist.rs`'s defaulted `inventory`
//!   REFUSES too — deliberately, that trait doc says, so no caller is handed a fabricated empty
//!   inventory. Over that double BOTH halves are `Err`, with different words.)
//! - It does **not cancel**. A walk in flight is left to finish (its `send` on a dropped receiver
//!   is a silently-ignored `Err`, exactly like every other worker in `studio.rs`), and the
//!   in-flight latch is the RECEIVER's presence — so the second click the shell refuses is
//!   refused because the first walk is still owed an answer, not because a flag was set.
//! - It does **not** move `StudioState::new_with_qa`'s construction-time walk off-thread. That one
//!   runs once, before the first frame is drawn, and the shell's posed-state tests (and the QA
//!   autorun hook) read a populated picker straight after the constructor returns.

use vike_data::HistStore;
use vike_data_manager::VenueNode;
use vike_studio_core::{SeriesLists, StoreHandle};

/// What `StudioState::poll` shows in both panes when the walk's thread died without answering.
///
/// The Studio's other workers each phrase this for themselves ("run failed (worker terminated)",
/// "sweep failed (worker terminated)"); this is the catalog's, named because BOTH panes render it
/// and a sentence spelled twice drifts. It reads as a scan failure on purpose: from the operator's
/// side a walk that cannot answer is a walk that cannot answer, and ⟳ Refresh is the same next
/// action either way.
pub const CATALOG_WALK_LOST: &str = "catalog scan failed (worker terminated)";

/// One catalog walk's answer: everything the Studio's two store-reading panes need, in one
/// message, so a refresh is one thread and one channel rather than two of each.
///
/// Each half carries its OWN `Result` — see the module doc for why they are not merged.
#[derive(Debug, Clone)]
pub struct CatalogLoad {
    /// `vike_studio_core::series_lists`'s answer — the slice picker's three lists, folded from
    /// ONE `list_series` call. `Err` is the store's own words, which
    /// `crate::picker::SlicePicker::apply` renders rather than an empty combo.
    pub series: Result<SeriesLists, String>,
    /// `HistStore::inventory` folded through `vike_data_manager::build_tree` — the Data pane's
    /// display tree. `Err` is again the store's own words, kept distinct from an empty tree.
    pub inventory: Result<Vec<VenueNode>, String>,
}

impl CatalogLoad {
    /// The load a DEAD worker leaves behind: both halves refused with [`CATALOG_WALK_LOST`].
    ///
    /// A disconnect is a terminal outcome, not silence — the same rule `StudioState::poll`'s
    /// run/sweep/walk-forward arms follow. Leaving the panes on their previous lists would claim
    /// the refresh had found the store unchanged.
    pub fn worker_terminated() -> Self {
        Self {
            series: Err(CATALOG_WALK_LOST.to_string()),
            inventory: Err(CATALOG_WALK_LOST.to_string()),
        }
    }
}

/// Walk `store`'s catalog into both panes' inputs — the whole of what the ⟳ Refresh button used to
/// do inline, and nothing else. Blocking, egui-free, and therefore testable in CI.
///
/// TWO store reads, down from four: `series_lists` folds `list_series` three ways in one call, and
/// `inventory` is its own verb (a different question — every kind, with coverage and byte counts —
/// which no fold of `list_series` can answer).
pub fn load_catalog(store: &dyn HistStore) -> CatalogLoad {
    CatalogLoad {
        series: vike_studio_core::series_lists(store).map_err(|e| e.to_string()),
        inventory: store.inventory().map(vike_data_manager::build_tree).map_err(|e| e.to_string()),
    }
}

/// Run [`load_catalog`] on a worker thread and return the receiver the shell polls.
///
/// `ctx.request_repaint()` after the send is what makes the answer VISIBLE: egui is a reactive
/// loop, so without it the load would sit in the channel until some other input happened to wake
/// a frame. The send itself is allowed to fail — a receiver dropped while the walk was in flight
/// (the shell was closed) is not an error, exactly as in `vike_studio_core::spawn_run`.
pub fn spawn_catalog_load(
    store: StoreHandle,
    ctx: &egui::Context,
) -> std::sync::mpsc::Receiver<CatalogLoad> {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let load = load_catalog(store.as_ref());
        let _ = tx.send(load);
        ctx.request_repaint();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vike_data::DataFusionHist;
    use vike_model::Bar;

    fn bar(ts: i64) -> Bar {
        Bar {
            ts,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn seeded() -> (tempfile::TempDir, StoreHandle) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(0), bar(60_000)], None).unwrap();
        (dir, Arc::new(store))
    }

    /// The walk answers BOTH panes from one call over a real seeded store — the property the
    /// single-message shape rests on.
    #[test]
    fn one_walk_answers_both_panes() {
        let (_dir, store) = seeded();
        let load = load_catalog(store.as_ref());
        let series = load.series.expect("a seeded store lists its series");
        assert_eq!(series.bars.len(), 1, "the one seeded bar series");
        assert!(series.ticks.is_empty() && series.depth_only.is_empty());
        let tree = load.inventory.expect("a seeded store answers inventory");
        assert_eq!(tree.len(), 1, "one venue node");
        assert_eq!(tree[0].venue, "binance");
    }

    /// The spawned walk delivers over the channel and its answer folds into the panes exactly as
    /// the synchronous `refresh` would — the whole point of sharing the `apply` fold.
    ///
    /// `recv()` rather than a poll loop, because there is no shell here to poll: this test holds
    /// the receiver itself, and a blocking wait is the honest way to consume it (a worker that
    /// never sends drops its sender, so `recv` returns `Err` instead of hanging). The shell-level
    /// twin in `studio.rs` — `a_spawned_catalog_refresh_populates_the_picker_through_poll` — has
    /// to drive `StudioState::poll` per frame, so it sleep-polls the way that file's other worker
    /// tests do; the two waits differ because the two surfaces do, not because one is right.
    #[test]
    fn a_spawned_walk_delivers_the_same_lists_the_blocking_walk_produces() {
        let (_dir, store) = seeded();
        let ctx = egui::Context::default();
        let rx = spawn_catalog_load(store.clone(), &ctx);
        let load = rx.recv().expect("the worker must answer or drop its sender");

        let mut spawned = crate::picker::SlicePicker::default();
        spawned.apply(load.series);
        let mut blocking = crate::picker::SlicePicker::default();
        blocking.refresh(store.as_ref());
        assert_eq!(spawned.available(), blocking.available(), "one walk, two threads, one answer");

        let mut pane = crate::data_browser::DataBrowserPane::default();
        pane.apply(load.inventory);
        // The pane keeps its tree private; its rendered failure text is the observable, and a
        // successful walk must leave none.
        assert!(pane.error().is_none(), "a seeded store's inventory must not read as a failure");
    }

    /// A dead worker is a terminal outcome in BOTH panes, never a silent keep-the-old-lists — the
    /// same rule `StudioState::poll`'s other disconnect arms follow.
    #[test]
    fn a_terminated_worker_refuses_both_halves_with_the_named_reason() {
        let load = CatalogLoad::worker_terminated();
        let mut picker = crate::picker::SlicePicker::default();
        picker.apply(load.series);
        assert_eq!(picker.error(), Some(CATALOG_WALK_LOST));
        assert!(picker.available().is_empty(), "a failed walk clears the list, never keeps it");

        let mut pane = crate::data_browser::DataBrowserPane::default();
        pane.apply(load.inventory);
        assert_eq!(pane.error(), Some(CATALOG_WALK_LOST));
    }
}
