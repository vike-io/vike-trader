//! The data-slice picker: enumerates the store's runnable series (via `HistStore::list_series` —
//! the TRAIT, so a remote RPC store lists exactly like the local DataFusion one; split-plane B12)
//! and produces a `DataSlice` for the Run.
//!
//! Two families are PICKABLE, in this order:
//! - **bars** — every `kind=bar` `(venue, symbol, interval)` (`vike_studio_core::bar_series`),
//!   picked as a `SliceKind::Bars` slice replayed through `StrategyEngine`;
//! - **ticks** — every `(venue, symbol)` with a recorded series in
//!   `vike_studio_core::REPLAYABLE_TICK_KINDS` (`vike_studio_core::tick_series`), picked as a
//!   `SliceKind::Ticks` slice replayed through `vike_backtest::hist_replay::replay_ticks`. Before
//!   this, tick series were listed nowhere and the Studio had no tick path at all.
//!
//! ...and a third family is DISCLOSED without being pickable: the instruments recorded ONLY as
//! `kind=depth` (`vike_studio_core::depth_only_series`), which no slice can replay — that
//! function's doc carries the argument, [`DEPTH_NOT_REPLAYABLE`] carries what the operator is
//! told. They are rendered because a picker is read as the store's complete runnable inventory,
//! so an instrument that is ON DISK and unrunnable dropping out of it silently is the same defect
//! as the scan failure below wearing different clothes.
//!
//! MVP loads the whole series range (`TsRange::all()`); a date-range sub-picker is a follow-up.
//! A tick pick can carry EXTRA symbols (the comma-separated box next to the combo) so a
//! cross-instrument tick strategy gets all its series in one `DataSlice::ticks` — the engine has
//! always accepted several tick series (`run_ticks` merges them k-way and routes by payload symbol).
//!
//! ⚠ **An enumeration that FAILED and a store that is genuinely EMPTY are different facts, and this
//! picker renders them differently.** [`SlicePicker::refresh`] used to `unwrap_or_default()` both
//! lists, so a store that could not be read drew the same empty combo — and the same *"no data in
//! store"* text — as one holding nothing: the operator reads "backfill some history" off a surface
//! whose real problem is that the datahub went away. [`crate::DataBrowserPane`], the sibling pane
//! whose own doc has said since split-plane B12 that it mirrors this one's maintenance-walk
//! pattern, already carried the cure; the mirror was one-way until now.
//!
//! ⚠ **The Refresh button's walk no longer happens on the paint thread**, and the split that buys
//! it is [`SlicePicker::refresh`] (walk + fold, blocking) vs [`SlicePicker::apply`] (fold alone).
//! [`crate::catalog::load_catalog`] performs the walk on a worker thread and `StudioState::poll`
//! calls `apply` with what it brings back, so this pane keeps ONE definition of what a catalog
//! answer MEANS whichever thread produced it. That module's doc carries the argument.
use vike_data::{HistStore, TsRange};
use vike_studio_core::DataSlice;

/// The combo's selected-text when the last catalog walk (refresh or apply — [`SlicePicker::apply`]
/// records it for both paths) could not enumerate the store.
///
/// The picker's twin of `crates/vike-app-core/src/stored_mode.rs`'s `PARTIALS_UNSERVED`, and it
/// exists for that constant's reason: a surface that could not get an answer SAYS SO, and never
/// borrows the rendering of the answer it would have given had the store been empty. The two send
/// an operator to opposite places — "nothing is recorded here" to a backfill, "I could not ask" to
/// the store itself — so a shared rendering is not a small imprecision, it is the wrong
/// instruction.
pub const SERIES_SCAN_FAILED: &str = "series scan failed";

/// What to do about a [`SERIES_SCAN_FAILED`] picker — the hover/expanded half of the disclosure.
///
/// Named separately from the reason the store reported because the two answer different questions:
/// the store's `DataError` says WHAT broke, this says where to look. The datahub hint is not
/// decoration — since split-plane B12 the Studio's store handle may be a `RemoteHistStore`, and an
/// unreachable server is the failure this path exists for.
pub const SERIES_SCAN_ADVICE: &str = "Could not list the store's series. Fix the store (a remote datahub: is the server up?), then \
     ⟳ Refresh.";

/// Why an instrument the store HOLDS is offered by no row — the hover behind the `depth-only`
/// chip and the dropdown's grayed entries.
///
/// The reason belongs on the surface rather than only in `vike_studio_core::tick_series`'s doc,
/// because the person who needs it is the one staring at a picker that does not list an
/// instrument they know they recorded. The action is in the last sentence: this is a recording
/// choice, and it is fixable.
pub const DEPTH_NOT_REPLAYABLE: &str = "Recorded, but not replayable. `kind=depth` is the CONFLATING L2 lane — periodic full \
     snapshots with every intermediate book state discarded — and a backtest over teleporting \
     depth reports fills it could never have got, so the tick replay reads the lossless \
     quote/trade/book lanes only. Record one of those for this instrument to make it runnable.";

/// One pickable series row. `interval: None` marks a TICK row (no bar step) — the same convention
/// `vike_data::SeriesId` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesRow {
    pub venue: String,
    pub symbol: String,
    pub interval: Option<String>,
}

impl SeriesRow {
    /// True for a tick (quote/trade/book) row.
    pub fn is_ticks(&self) -> bool {
        self.interval.is_none()
    }

    /// The combo's display text, e.g. `binance · BTCUSDT · 1m` / `polymarket · TKN · ticks`.
    pub fn label(&self) -> String {
        format!(
            "{} · {} · {}",
            self.venue,
            self.symbol,
            self.interval.as_deref().unwrap_or("ticks")
        )
    }
}

#[derive(Default)]
pub struct SlicePicker {
    /// Every runnable series in the store: bar rows first (sorted), then tick rows (sorted).
    available: Vec<SeriesRow>,
    selected: Option<usize>,
    /// `Some(reason)` when the last catalog walk (refresh or apply — [`Self::apply`] records it
    /// for both) could not enumerate the store's series — rendered by [`Self::ui`] instead of the
    /// "no data in store" text an EMPTY store earns.
    /// The same field, holding the same kind of value, as [`crate::DataBrowserPane`]'s.
    error: Option<String>,
    /// `(venue, symbol)` pairs the store holds ONLY as `kind=depth`
    /// (`vike_studio_core::depth_only_series`). Not in `available` — no slice can replay them —
    /// but rendered by [`Self::ui`] all the same, because an instrument that is on disk and
    /// unrunnable must not read as an instrument that was never recorded. [`DEPTH_NOT_REPLAYABLE`]
    /// is the reason shown beside them.
    depth_only: Vec<(String, String)>,
    /// Comma-separated EXTRA symbols folded into a tick pick alongside the selected row's symbol.
    /// Ignored for bar rows (multi-symbol bar runs need aligned bar counts, which a free-text box
    /// cannot promise — `load_slice_bars` rejects a misaligned set with a readable error).
    pub extra_symbols: String,
}

impl SlicePicker {
    /// Re-scan the store's series (a one-shot maintenance walk — call on open / a Refresh button,
    /// not per frame) and fold the answer in.
    ///
    /// ⚠ **This BLOCKS on the store**, which is why the Refresh button no longer calls it: on a
    /// `RemoteHistStore` the walk is a TCP connect, and the Studio's toolbar runs on the egui paint
    /// thread. `crate::catalog::load_catalog` is the same walk performed on a worker thread, and
    /// [`Self::apply`] is the fold both paths share — so a synchronous caller (this crate's tests,
    /// and `StudioState::new_with_qa`'s one-shot construction walk) and the off-thread caller can
    /// never disagree about what the answer MEANS.
    pub fn refresh(&mut self, store: &dyn HistStore) {
        self.apply(vike_studio_core::series_lists(store).map_err(|e| e.to_string()));
    }

    /// Fold a catalog walk's answer in — the pure half of [`Self::refresh`], and what
    /// `StudioState::poll` calls when the off-thread walk lands.
    ///
    /// A scan failure CLEARS the lists and records the reason ([`Self::error`], rendered by
    /// [`Self::ui`]) rather than degrading to empty ones. Every filter folds the same
    /// `HistStore::list_series` call — literally one call since `vike_studio_core::series_lists`
    /// (it was three, once per filter) — so a mixed outcome, bars listed and ticks refused, is
    /// unreachable by construction; and a PARTIAL list would be the same defect wearing a smaller
    /// hat, since the combo is read as the store's complete inventory either way.
    ///
    /// ⚠ The selection survives by INDEX, not by identity (it is re-validated against the new
    /// list's length, and clamped to row 0 when it falls off), and the off-thread path opens a
    /// window the blocking one never had: between `spawn_catalog_refresh` and the frame this fold
    /// lands, the operator can still change the combo, and a store whose series set moved
    /// underneath them can then land that index on a DIFFERENT row. Nothing is silently run on
    /// it — the combo shows whichever row the index now names — but the pick is not pinned across
    /// a refresh, and a caller that needs it pinned must compare `SeriesRow`s, not indices.
    pub fn apply(&mut self, listed: Result<vike_studio_core::SeriesLists, String>) {
        // All three filters live once in vike-studio-core (see `bar_series`/`tick_series`/
        // `depth_only_series` there, and `series_lists`, which folds one catalog read three ways)
        // — including the last, which is `tick_series`'s complement and must not be re-derived
        // from a second reading of the same rule.
        let lists = match listed {
            Ok(lists) => lists,
            Err(e) => {
                self.available.clear();
                self.depth_only.clear();
                self.selected = None;
                self.error = Some(e);
                return;
            }
        };
        self.error = None;
        self.depth_only = lists.depth_only;
        self.available = lists
            .bars
            .into_iter()
            .map(|(venue, symbol, interval)| SeriesRow { venue, symbol, interval: Some(interval) })
            .chain(lists.ticks.into_iter().map(|(venue, symbol)| SeriesRow {
                venue,
                symbol,
                interval: None,
            }))
            .collect();
        if self.selected.is_none_or(|i| i >= self.available.len()) {
            self.selected = (!self.available.is_empty()).then_some(0);
        }
    }

    pub fn available(&self) -> &[SeriesRow] {
        &self.available
    }

    /// Why the last [`Self::refresh`] could not list the store's series, if it could not.
    ///
    /// `None` covers BOTH a populated picker and one that scanned an empty store successfully —
    /// so a caller deciding what to tell the user must ask this FIRST and only then fall back to
    /// `available().is_empty()`, which on its own cannot tell the two apart. `StudioState`'s
    /// empty-results placeholder is the one such caller.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The `(venue, symbol)` pairs the store holds only as `kind=depth` — recorded, listed by no
    /// pickable row, and disclosed by [`Self::ui`] rather than dropped. See
    /// [`DEPTH_NOT_REPLAYABLE`].
    pub fn depth_only(&self) -> &[(String, String)] {
        &self.depth_only
    }

    pub fn select(&mut self, i: usize) {
        if i < self.available.len() {
            self.selected = Some(i);
        }
    }

    /// Select by `(venue, symbol, interval)` key — e.g. a row clicked in the Data-browser pane.
    /// No-op (returns `false`) if the key isn't in `available` (a stale list before the next
    /// `refresh`). BAR rows only: the Data browser hands tick rows back as `None` already.
    pub fn select_by_key(&mut self, venue: &str, symbol: &str, interval: &str) -> bool {
        match self.available.iter().position(|r| {
            r.venue == venue && r.symbol == symbol && r.interval.as_deref() == Some(interval)
        }) {
            Some(i) => {
                self.selected = Some(i);
                true
            }
            None => false,
        }
    }

    /// The currently-selected row, if any.
    pub fn selected_row(&self) -> Option<&SeriesRow> {
        self.available.get(self.selected?)
    }

    pub fn selected(&self) -> Option<DataSlice> {
        let row = self.selected_row()?;
        Some(match &row.interval {
            Some(interval) => DataSlice::bars(&row.venue, &row.symbol, interval, TsRange::all()),
            None => {
                let mut symbols = vec![row.symbol.clone()];
                for extra in self.extra_symbols.split(',') {
                    let extra = extra.trim();
                    if !extra.is_empty() && !symbols.iter().any(|s| s == extra) {
                        symbols.push(extra.to_string());
                    }
                }
                DataSlice::ticks(&row.venue, symbols, TsRange::all())
            }
        })
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let cur = self.selected_row().cloned();
        // FOUR states, not two. A failed scan, a store holding only unreplayable series, an empty
        // store and an unselected combo each read differently — and the middle two are the ones
        // this picker used to collapse into "no data in store", which is a claim about the STORE
        // made from a fact about the LIST. A failure cannot reach the `Some` arm (`refresh` drops
        // the selection with the list), so the order below is total.
        let nothing_recorded = self.available.is_empty() && self.depth_only.is_empty();
        let label = match (&cur, self.error.is_some(), self.available.is_empty()) {
            (Some(row), _, _) => row.label(),
            (None, true, _) => SERIES_SCAN_FAILED.to_string(),
            (None, false, true) if nothing_recorded => "no data in store".to_string(),
            (None, false, true) => "nothing replayable".to_string(),
            (None, false, false) => "select data…".to_string(),
        };
        ui.label(egui::RichText::new("Data").weak());
        egui::ComboBox::from_id_salt("studio-data-slice")
            .width(230.0)
            .selected_text(label)
            .show_ui(ui, |ui| {
                for i in 0..self.available.len() {
                    let text = self.available[i].label();
                    if ui.selectable_label(self.selected == Some(i), text).clicked() {
                        self.selected = Some(i);
                    }
                }
                // Named, one row each, and NOT selectable: a slice over them would load nothing.
                // They sit below the pickable rows so the list still reads top-down as "what you
                // can run", with what you cannot run — and why — under it rather than absent.
                for (venue, symbol) in &self.depth_only {
                    ui.weak(format!("{venue} · {symbol} · depth (not replayable)"))
                        .on_hover_text(DEPTH_NOT_REPLAYABLE);
                }
                if let Some(err) = &self.error {
                    ui.colored_label(crate::theme::ERR, format!("Series scan failed: {err}"));
                    ui.weak(SERIES_SCAN_ADVICE);
                } else if self.available.is_empty() && self.depth_only.is_empty() {
                    ui.weak("Store is empty — backfill history, then ⟳ Refresh.");
                }
            });
        // ...and again OUTSIDE the dropdown, because a disclosure that only exists inside a closed
        // popup is barely louder than the silence it replaced: the combo's own text says "series
        // scan failed", and this chip is what a glance at the toolbar catches.
        if let Some(err) = &self.error {
            crate::theme::chip(ui, crate::theme::ERR, "⚠ scan failed")
                .on_hover_text(format!("{SERIES_SCAN_ADVICE}\n\n{err}"));
        }
        // ...and the same argument for the depth-only instruments: a grayed row inside a closed
        // popup is not much louder than the silence it replaced, and the reader who needs this is
        // the one wondering why an instrument they KNOW they recorded is not in the list.
        if !self.depth_only.is_empty() {
            let named = self
                .depth_only
                .iter()
                .map(|(venue, symbol)| format!("{venue} · {symbol}"))
                .collect::<Vec<_>>()
                .join("\n");
            crate::theme::chip(ui, crate::theme::WARN, "⚠ depth-only")
                .on_hover_text(format!("{DEPTH_NOT_REPLAYABLE}\n\n{named}"));
        }
        // Tick picks only: a multi-symbol tick replay is the one place a free-text symbol list is
        // safe (no bar-length alignment to honor), and it is exactly what a cross-instrument tick
        // strategy needs.
        if cur.as_ref().is_some_and(SeriesRow::is_ticks) {
            ui.add_sized(
                [140.0, 20.0],
                egui::TextEdit::singleline(&mut self.extra_symbols).hint_text("+ symbols (a, b)"),
            )
            .on_hover_text(
                "Extra tick symbols to replay alongside the selected one, comma-separated",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::{DataFusionHist, HistStore, TsRange};
    use vike_model::{Bar, QuoteTick};
    use vike_studio_core::SliceKind;

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

    fn quote(ts: i64, symbol: &str) -> QuoteTick {
        QuoteTick {
            ts,
            local_ts: ts,
            bid: 1.0,
            ask: 1.1,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: symbol.to_string(),
        }
    }

    #[test]
    fn refresh_lists_bar_series_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(0), bar(60_000)], None).unwrap();
        store.append_bars("binance", "ETHUSDT", "1h", &[bar(0)], None).unwrap();

        let mut p = SlicePicker::default();
        p.refresh(&store);
        // exactly the two seeded (venue,symbol,interval) bar series
        assert_eq!(p.available().len(), 2);
        assert!(p.available().iter().any(|r| r.venue == "binance"
            && r.symbol == "BTCUSDT"
            && r.interval.as_deref() == Some("1m")));
        assert!(p.available().iter().any(|r| r.venue == "binance"
            && r.symbol == "ETHUSDT"
            && r.interval.as_deref() == Some("1h")));

        // selecting the first yields a DataSlice over the whole range
        p.select(0);
        let sl = p.selected().unwrap();
        assert_eq!(sl.venue, "binance");
        assert_eq!(sl.kind, SliceKind::Bars);
        assert!(matches!(sl.range, TsRange { start: None, end: None }));
    }

    /// Tick series are listed AFTER the bar rows and pick as a `SliceKind::Ticks` slice — the
    /// path that did not exist before (a tick series was invisible to the Studio entirely).
    #[test]
    fn refresh_lists_tick_series_after_the_bar_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(0)], None).unwrap();
        store.append_quotes("polymarket", "TKN", &[quote(1, "TKN")], None).unwrap();

        let mut p = SlicePicker::default();
        p.refresh(&store);
        assert_eq!(p.available().len(), 2);
        assert!(!p.available()[0].is_ticks(), "bar rows come first");
        assert!(p.available()[1].is_ticks());
        assert_eq!(p.available()[1].label(), "polymarket · TKN · ticks");

        p.select(1);
        let sl = p.selected().unwrap();
        assert_eq!(sl.kind, SliceKind::Ticks);
        assert_eq!(sl.venue, "polymarket");
        assert_eq!(sl.symbols, vec!["TKN".to_string()]);
    }

    /// The extra-symbols box folds additional symbols into a TICK slice (deduped, blanks
    /// dropped) and is inert for a bar slice.
    #[test]
    fn extra_symbols_widen_a_tick_pick_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(0)], None).unwrap();
        store.append_quotes("polymarket", "UP", &[quote(1, "UP")], None).unwrap();

        let mut p = SlicePicker::default();
        p.refresh(&store);
        p.extra_symbols = " DOWN , , UP , SPOT ".to_string();

        p.select(0); // the bar row
        assert_eq!(p.selected().unwrap().symbols, vec!["BTCUSDT".to_string()]);

        p.select(1); // the tick row
        assert_eq!(
            p.selected().unwrap().symbols,
            vec!["UP".to_string(), "DOWN".to_string(), "SPOT".to_string()],
            "blanks dropped, the selected symbol deduped, order preserved"
        );
    }

    #[test]
    fn select_by_key_finds_and_selects_a_known_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(0)], None).unwrap();
        store.append_bars("binance", "ETHUSDT", "1h", &[bar(0)], None).unwrap();

        let mut p = SlicePicker::default();
        p.refresh(&store);
        p.select(0); // start on some other row than the one we'll key-select

        assert!(p.select_by_key("binance", "ETHUSDT", "1h"));
        let sl = p.selected().unwrap();
        assert_eq!(sl.venue, "binance");
        assert_eq!(sl.symbol(), "ETHUSDT");
        assert_eq!(sl.interval, "1h");
    }

    #[test]
    fn select_by_key_is_a_no_op_for_an_unknown_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(0)], None).unwrap();

        let mut p = SlicePicker::default();
        p.refresh(&store);
        p.select(0);
        let before = p.selected().map(|s| (s.venue, s.symbols, s.interval));

        assert!(!p.select_by_key("okx", "NOPE", "1d"));
        let after = p.selected().map(|s| (s.venue, s.symbols, s.interval));
        assert_eq!(after, before, "an unknown key must not change the selection");
    }
}
