//! The Data pane: browse what historical data the store actually holds via
//! [`vike_data_manager::inventory_list`] — the SAME venue → symbol → series catalog view the
//! app's Data Manager "Stored" tab renders (`vike-data-manager`, the de-duplicated extraction).
//! [`StudioDataBrowserPane`] is a thin Studio-side wrapper: [`StudioDataBrowserPane::refresh`] walks the
//! store's inventory into a [`vike_data_manager::DataManagerInventoryVenue`] tree (mirrors
//! [`crate::panes::slice_picker::StudioSlicePicker::refresh`]'s one-shot maintenance-walk pattern — never per frame);
//! [`StudioDataBrowserPane::ui`] renders that tree and maps a clicked row's
//! [`vike_data_manager::DataManagerInventorySeriesKey`] down to the `(venue, symbol, interval)` triple the
//! Studio's [`crate::panes::slice_picker::StudioSlicePicker`] picker contract expects (bar series only — a clicked
//! tick-series row, which carries no `interval`, yields `None`; there is no `DataSlice` for it
//! yet).
//!
//! ⚠ **The walk and the FOLD are separate entry points**, for the reason
//! [`crate::backend::catalog`] exists: `inventory()` is the most expensive of the Studio's catalog reads
//! (one manifest open per series, one `metadata` per part — or a fresh connect on a
//! `HistoryStoreDatahub`), and the toolbar's ⟳ Refresh used to run it inline on the egui paint thread.
//! [`StudioDataBrowserPane::refresh`] is still walk+fold for a synchronous caller;
//! [`StudioDataBrowserPane::apply`] is the fold alone, which is what the worker thread's answer goes
//! through.

use vike_data::HistoryStore;
use vike_data_manager::DataManagerInventoryVenue;
use vike_ui_theme::components::ThemeStatus;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::studio;

/// Studio pane state: the last-scanned inventory tree + the search box text, and the Import CSV form
/// drawn above them. Owns no store access outside [`Self::refresh`].
#[derive(Default)]
pub struct StudioDataBrowserPane {
    pub(crate) search: String,
    /// The Import CSV form (decision 0272): `StudioState` takes its request after [`Self::ui`] and
    /// hands it the upload's progress and answer.
    pub import: crate::panes::csv_upload::StudioCsvUploadForm,
    tree: Vec<DataManagerInventoryVenue>,
    /// `Some(reason)` when the last [`Self::refresh`] could not scan the inventory — surfaced by
    /// [`Self::ui`] instead of the misleading "no data yet" hint. Load-bearing for a REMOTE store
    /// (split-plane B12): there "the server is unreachable" and "the server holds nothing" must
    /// never render the same, so a scan failure is never a silent empty.
    error: Option<String>,
}

impl StudioDataBrowserPane {
    /// Re-scan the store's full inventory (every kind, not just bars — unlike `StudioSlicePicker`,
    /// which only lists `kind=bar`) and rebuild the display tree. Takes the TRAIT
    /// ([`HistoryStore::inventory`] — manifest-only/catalog reads, no data scan; the remote store
    /// serves it over RPC), still a one-shot walk — call on open / a Refresh action, not per
    /// frame.
    ///
    /// ⚠ **This BLOCKS**, and on this pane's walk that is the expensive one: `inventory()` is one
    /// manifest open per series plus a `metadata` call per part locally, and a fresh connect over
    /// the wire. The Refresh button calls [`crate::backend::catalog::load_catalog`] on a worker thread
    /// instead and hands the answer to [`Self::apply`]; this entry point stays for the
    /// construction-time walk and for tests.
    pub(crate) fn refresh(&mut self, store: &dyn HistoryStore) {
        self.apply(
            store
                .inventory()
                .map(vike_data_manager::build_inventory_tree)
                .map_err(|e| e.to_string()),
        );
    }

    /// Fold an inventory walk's answer in — the pure half of [`Self::refresh`], shared with the
    /// off-thread path so the two cannot drift.
    ///
    /// A scan failure clears the pane AND records the reason (it used to degrade to a silent
    /// `unwrap_or_default` empty, indistinguishable from a genuinely empty store).
    pub(crate) fn apply(&mut self, tree: Result<Vec<DataManagerInventoryVenue>, String>) {
        match tree {
            Ok(tree) => {
                self.tree = tree;
                self.error = None;
            }
            Err(e) => {
                self.tree = Vec::new();
                self.error = Some(e);
            }
        }
    }

    /// Why the last walk could not scan the store's inventory, if it could not — the twin of
    /// [`crate::panes::slice_picker::StudioSlicePicker::error`], and read for the same reason: `None` covers BOTH a
    /// populated pane and one that scanned an empty store successfully, so a caller deciding what
    /// to TELL the user must ask this first. [`Self::ui`] already does; this accessor exists so a
    /// test can observe the distinction without reaching into the tree.
    #[cfg(test)]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The first and last timestamp (epoch-ms) the last walk found for `venue · symbol` at
    /// `interval` (bars), or across its replayable tick lanes (`interval: None`, the earliest first
    /// and the latest last); `None` when the walk listed no such rows. The toolbar's run window reads
    /// it: where the data starts, and the day the presets count back from.
    pub(crate) fn coverage_of(
        &self,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
    ) -> Option<(i64, i64)> {
        let wanted = |kind: vike_model::StoreKindId, at: Option<&str>| match interval {
            Some(_) => kind == vike_model::StoreKindId::Bar && at == interval,
            None => vike_model::StoreKindId::TICK_LANES.contains(&kind),
        };
        self.tree
            .iter()
            .filter(|v| v.venue == venue)
            .flat_map(|v| &v.symbols)
            .filter(|s| s.symbol == symbol && !s.grouped)
            .flat_map(|s| &s.series)
            .filter(|s| s.source.is_none() && s.coverage.rows > 0)
            .filter(|s| wanted(s.kind, s.interval.as_deref()))
            .map(|s| (s.coverage.first_timestamp, s.coverage.last_timestamp))
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
    }

    /// Render the shared stored-catalog tree view. Returns `Some((venue, symbol, interval))` the
    /// frame a bar-series row is clicked — the caller (`StudioState::ui`) applies it to the
    /// `StudioSlicePicker`. A clicked tick-series row (`interval: None`) yields `None`: no `DataSlice`
    /// (bar-only) exists for it. `upload_to` is the datahub the Import CSV form sends to, `None` when
    /// the Studio reads none.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        upload_to: Option<&str>,
    ) -> Option<(String, String, String)> {
        crate::studio::pane_header(ui, icons::DATA, "Stored data");
        self.import.ui(ui, upload_to);
        ui.add_space(space::XS);
        ui.label(egui::RichText::new("Click a bar series to select it as the run slice.").weak());
        ui.add_space(space::XS);
        // The shared catalog grid lays out ~690px of fixed-width columns (symbol/kind/coverage/
        // rows/size/updated — see vike-data-manager's W_* consts), far wider than the Studio's
        // 340px tools panel. Give it its natural width inside a horizontal ScrollArea so the
        // grid renders intact and the pane h-scrolls, instead of egui squeezing the fixed cells
        // into a broken clip (symbol column half-vanished, coverage bar jammed into the rail).
        let selection = egui::ScrollArea::horizontal()
            .id_salt("studio-data-hscroll")
            .show(ui, |ui| {
                ui.set_min_width(studio::DATA_BROWSER_MIN_WIDTH);
                vike_data_manager::inventory_list(ui, &self.tree, &mut self.search)
            })
            .inner;
        if let Some(err) = &self.error {
            // A failed scan must never wear the "empty store" costume — for a remote store this
            // is typically "the datahub server went away", which Refresh CAN fix.
            ui.colored_label(ThemeStatus::Error.color(), format!("Inventory scan failed: {err}"));
            ui.weak("Fix the store (a remote datahub: is the server up?), then Refresh.");
        } else if self.tree.is_empty() {
            // NOTE: no "$VIKE_HIST_STORE, then Refresh" advice here — the store root is resolved
            // once at startup (vike-desktop's studio_store_root caches the open for the process
            // lifetime), so changing the env var and clicking Refresh would do nothing.
            ui.weak(
                "No historical data in this store yet. Backfill history (the Data Manager's \
                 Backfill, or vike-backfill), then Refresh.",
            );
        }
        selection.and_then(|s| s.interval.map(|iv| (s.venue, s.symbol, iv)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data_manager::DataManagerInventorySeriesKey;
    use vike_marketdata::Bar;
    use vike_model::StoreKindId;
    use vike_store::HistoryStoreDataFusion;

    fn bar(timestamp: i64) -> Bar {
        vike_marketdata::test_support::flat_bar_zero_volume(timestamp, 1.0)
    }

    #[test]
    fn refresh_populates_tree_from_a_real_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        store
            .append_bars("binance", "BTCUSDT", "1m", &[bar(0), bar(60_000), bar(120_000)], None)
            .unwrap();

        let mut pane = StudioDataBrowserPane::default();
        pane.refresh(&store);

        assert_eq!(pane.tree.len(), 1, "one venue node");
        let venue = &pane.tree[0];
        assert_eq!(venue.venue, "binance");
        assert_eq!(venue.symbols.len(), 1, "one symbol node");
        let symbol = &venue.symbols[0];
        assert_eq!(symbol.symbol, "BTCUSDT");
        assert_eq!(symbol.series.len(), 1, "one series row");
        let series = &symbol.series[0];
        assert_eq!((series.kind, series.interval.as_deref()), (StoreKindId::Bar, Some("1m")));
        assert_eq!(series.coverage.rows, 3);
        assert_eq!(series.coverage.first_timestamp, 0);
        assert_eq!(series.coverage.last_timestamp, 120_000);
        // ...which is where the toolbar's run window says the series starts, for that interval only.
        assert_eq!(pane.coverage_of("binance", "BTCUSDT", Some("1m")), Some((0, 120_000)));
        assert_eq!(pane.coverage_of("binance", "BTCUSDT", Some("1h")), None);
        assert_eq!(pane.coverage_of("binance", "BTCUSDT", None), None, "no tick lane is held");
    }

    #[test]
    fn refresh_on_an_empty_store_yields_no_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let mut pane = StudioDataBrowserPane::default();
        pane.refresh(&store);
        assert!(pane.tree.is_empty());
    }

    #[test]
    fn bar_selection_maps_to_venue_symbol_interval_tuple() {
        let selection = DataManagerInventorySeriesKey {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            kind: StoreKindId::Bar,
            interval: Some("1m".to_string()),
            grouped: false,
            source: None,
        };
        let mapped = selection
            .interval
            .clone()
            .map(|iv| (selection.venue.clone(), selection.symbol.clone(), iv));
        assert_eq!(mapped, Some(("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string())));
    }

    #[test]
    fn tick_selection_with_no_interval_maps_to_none() {
        let selection = DataManagerInventorySeriesKey {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            kind: StoreKindId::Trade,
            interval: None,
            grouped: false,
            source: None,
        };
        let mapped: Option<(String, String, String)> =
            selection.interval.map(|iv| (selection.venue, selection.symbol, iv));
        assert_eq!(mapped, None);
    }
}
