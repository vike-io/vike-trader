//! The Data pane: browse what historical data the store actually holds via
//! [`vike_data_manager::stored_catalog_ui`] — the SAME venue → symbol → series catalog view the
//! app's Data Manager "Stored" tab renders (`vike-data-manager`, the de-duplicated extraction).
//! [`DataBrowserPane`] is a thin Studio-side wrapper: [`DataBrowserPane::refresh`] walks the
//! store's inventory into a [`vike_data_manager::VenueNode`] tree (mirrors
//! [`crate::picker::SlicePicker::refresh`]'s one-shot maintenance-walk pattern — never per frame);
//! [`DataBrowserPane::ui`] renders that tree and maps a clicked row's
//! [`vike_data_manager::StoredSelection`] down to the `(venue, symbol, interval)` triple the
//! Studio's [`crate::picker::SlicePicker`] picker contract expects (bar series only — a clicked
//! tick-series row, which carries no `interval`, yields `None`; there is no `DataSlice` for it
//! yet).
//!
//! ⚠ **The walk and the FOLD are separate entry points**, for the reason
//! [`crate::catalog`] exists: `inventory()` is the most expensive of the Studio's catalog reads
//! (one manifest open per series, one `metadata` per part — or a fresh connect on a
//! `RemoteHistStore`), and the toolbar's ⟳ Refresh used to run it inline on the egui paint thread.
//! [`DataBrowserPane::refresh`] is still walk+fold for a synchronous caller;
//! [`DataBrowserPane::apply`] is the fold alone, which is what the worker thread's answer goes
//! through.

use vike_data::HistStore;
use vike_data_manager::VenueNode;

/// Studio pane state: the last-scanned inventory tree + the search box text. Owns no store access
/// outside [`Self::refresh`].
#[derive(Default)]
pub struct DataBrowserPane {
    pub search: String,
    tree: Vec<VenueNode>,
    /// `Some(reason)` when the last [`Self::refresh`] could not scan the inventory — surfaced by
    /// [`Self::ui`] instead of the misleading "no data yet" hint. Load-bearing for a REMOTE store
    /// (split-plane B12): there "the server is unreachable" and "the server holds nothing" must
    /// never render the same, so a scan failure is never a silent empty.
    error: Option<String>,
}

impl DataBrowserPane {
    /// Re-scan the store's full inventory (every kind, not just bars — unlike `SlicePicker`,
    /// which only lists `kind=bar`) and rebuild the display tree. Takes the TRAIT
    /// ([`HistStore::inventory`] — manifest-only/catalog reads, no data scan; the remote store
    /// serves it over RPC), still a one-shot walk — call on open / a Refresh action, not per
    /// frame.
    ///
    /// ⚠ **This BLOCKS**, and on this pane's walk that is the expensive one: `inventory()` is one
    /// manifest open per series plus a `metadata` call per part locally, and a fresh connect over
    /// the wire. The Refresh button calls [`crate::catalog::load_catalog`] on a worker thread
    /// instead and hands the answer to [`Self::apply`]; this entry point stays for the
    /// construction-time walk and for tests.
    pub fn refresh(&mut self, store: &dyn HistStore) {
        self.apply(store.inventory().map(vike_data_manager::build_tree).map_err(|e| e.to_string()));
    }

    /// Fold an inventory walk's answer in — the pure half of [`Self::refresh`], shared with the
    /// off-thread path so the two cannot drift.
    ///
    /// A scan failure clears the pane AND records the reason (it used to degrade to a silent
    /// `unwrap_or_default` empty, indistinguishable from a genuinely empty store).
    pub fn apply(&mut self, tree: Result<Vec<VenueNode>, String>) {
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
    /// [`crate::picker::SlicePicker::error`], and read for the same reason: `None` covers BOTH a
    /// populated pane and one that scanned an empty store successfully, so a caller deciding what
    /// to TELL the user must ask this first. [`Self::ui`] already does; this accessor exists so a
    /// test (and any future caller) can observe the distinction without reaching into the tree.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Render the shared stored-catalog tree view. Returns `Some((venue, symbol, interval))` the
    /// frame a bar-series row is clicked — the caller (`StudioState::ui`) applies it to the
    /// `SlicePicker`. A clicked tick-series row (`interval: None`) yields `None`: no `DataSlice`
    /// (bar-only) exists for it.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<(String, String, String)> {
        crate::theme::section_header(ui, "🗄", "Stored data");
        ui.label(egui::RichText::new("Click a bar series to select it as the run slice.").weak());
        ui.add_space(2.0);
        // The shared catalog grid lays out ~690px of fixed-width columns (symbol/kind/coverage/
        // rows/size/updated — see vike-data-manager's W_* consts), far wider than the Studio's
        // 340px tools panel. Give it its natural width inside a horizontal ScrollArea so the
        // grid renders intact and the pane h-scrolls, instead of egui squeezing the fixed cells
        // into a broken clip (symbol column half-vanished, coverage bar jammed into the rail).
        let selection = egui::ScrollArea::horizontal()
            .id_salt("studio-data-hscroll")
            .show(ui, |ui| {
                ui.set_min_width(700.0);
                vike_data_manager::stored_catalog_ui(ui, &self.tree, &mut self.search)
            })
            .inner;
        if let Some(err) = &self.error {
            // A failed scan must never wear the "empty store" costume — for a remote store this
            // is typically "the datahub server went away", which ⟳ Refresh CAN fix.
            ui.colored_label(
                egui::Color32::from_rgb(220, 110, 130),
                format!("Inventory scan failed: {err}"),
            );
            ui.weak("Fix the store (a remote datahub: is the server up?), then ⟳ Refresh.");
        } else if self.tree.is_empty() {
            // NOTE: no "$VIKE_HIST_STORE, then Refresh" advice here — the store root is resolved
            // once at startup (vike-app's studio_store_root caches the open for the process
            // lifetime), so changing the env var and clicking Refresh would do nothing.
            ui.weak(
                "No historical data in this store yet. Backfill history (the Data Manager's \
                 Backfill, or vike-backfill), then ⟳ Refresh.",
            );
        }
        selection.and_then(|s| s.interval.map(|iv| (s.venue, s.symbol, iv)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::DataFusionHist;
    use vike_data_manager::StoredSelection;
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

    #[test]
    fn refresh_populates_tree_from_a_real_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store
            .append_bars("binance", "BTCUSDT", "1m", &[bar(0), bar(60_000), bar(120_000)], None)
            .unwrap();

        let mut pane = DataBrowserPane::default();
        pane.refresh(&store);

        assert_eq!(pane.tree.len(), 1, "one venue node");
        let venue = &pane.tree[0];
        assert_eq!(venue.venue, "binance");
        assert_eq!(venue.symbols.len(), 1, "one symbol node");
        let symbol = &venue.symbols[0];
        assert_eq!(symbol.symbol, "BTCUSDT");
        assert_eq!(symbol.series.len(), 1, "one series row");
        let series = &symbol.series[0];
        assert_eq!((series.kind.as_str(), series.interval.as_deref()), ("bar", Some("1m")));
        assert_eq!(series.cov.rows, 3);
        assert_eq!(series.cov.first_ts, 0);
        assert_eq!(series.cov.last_ts, 120_000);
    }

    #[test]
    fn refresh_on_an_empty_store_yields_no_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let mut pane = DataBrowserPane::default();
        pane.refresh(&store);
        assert!(pane.tree.is_empty());
    }

    #[test]
    fn bar_selection_maps_to_venue_symbol_interval_tuple() {
        let sel = StoredSelection {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            kind: "bar".to_string(),
            interval: Some("1m".to_string()),
        };
        let mapped = sel.interval.clone().map(|iv| (sel.venue.clone(), sel.symbol.clone(), iv));
        assert_eq!(mapped, Some(("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string())));
    }

    #[test]
    fn tick_selection_with_no_interval_maps_to_none() {
        let sel = StoredSelection {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            kind: "trade".to_string(),
            interval: None,
        };
        let mapped: Option<(String, String, String)> =
            sel.interval.map(|iv| (sel.venue, sel.symbol, iv));
        assert_eq!(mapped, None);
    }
}
