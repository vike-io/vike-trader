//! `vike_core::journal_view_from_store` — the THIRD leg of the reconcile cross-check, built from
//! the materialized exec log.
//!
//! # What this file exists for
//!
//! The walk it drives used to live inside `crates/vike-app/src/main.rs`'s `journal_view_provider`
//! closure — the workspace's ONE production `JournalViewHook`, in the one workspace member the
//! merge gate does not RUN. Nothing in this repository executed a line of it. Two defects were
//! sitting there, and each one has a test below that reddens on it:
//!
//!   * **the window disagreed with the leg it is compared against.** The closure scoped its store
//!     read with a hard-coded one hour while the VENUE leg of the same `diff` was scoped by
//!     `VIKE_RECONCILE_LOOKBACK_MS`. They agreed only while nobody set that variable, and the
//!     failure points counter-intuitively — see
//!     [`a_journal_window_narrower_than_the_venue_window_turns_an_alert_into_an_auto_fold`].
//!   * **a store failure was indistinguishable from a quiet account.** `if let Ok(store) = …` with
//!     no `else` and no log yielded an empty view, which `diff` cannot tell from a journal that
//!     genuinely recorded nothing — [`an_empty_view_is_indistinguishable_from_no_journal_at_all`]
//!     is that claim, asserted rather than assumed, and it is the whole reason the lifted function
//!     logs on every degrading path.
//!
//! ⚠ **No test here asserts on LOG OUTPUT, deliberately.** These tests link into the grouped
//! `recon` binary and run as THREADS in one process; `tracing` caches an `Interest` verdict per
//! callsite process-globally, so a sibling thread reaching a callsite with no subscriber can switch
//! it off for a later capturing test. What is asserted is the observable VALUE on each path; the
//! log lines beside them are argued at their own site in `crates/vike-core/src/journal_view.rs`.

use std::collections::HashSet;

use indexmap::IndexMap;
use vike_core::journal_view_from_store;
use vike_data::{DataError, ExecFillRow, ExecOrderRow, HistStore, MemHistStore, SeriesId, TsRange};
use vike_exec::recon::{diff, Divergence, DivergenceKind, LocalCash, LocalView, ReconPolicy};
use vike_exec::{ManagedOrder, OrderStatus};
use vike_model::events::LiquiditySide;
use vike_model::{
    Bar, BookUpdate, EquitySample, FillReport, QuoteTick, SymbolProperties, TradeTick,
};

/// An arbitrary but FIXED "now", so every window in this file is arithmetic rather than a clock
/// read. The lifted function takes `since_ms` as a parameter precisely so a test can do this.
const NOW_MS: i64 = 1_767_225_600_000;
const ONE_HOUR_MS: i64 = 3_600_000;

fn fill(trade_id: &str, ts: i64) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: trade_id.into(),
        client_order_id: "c-1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        px: 100.0,
        commission: 0.0,
        mark_price: None,
        liquidity_side: String::new(),
        commission_asset: String::new(),
    }
}

fn order_row(coid: &str, status: &str, ts: i64) -> ExecOrderRow {
    ExecOrderRow {
        ts,
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "LIMIT".into(),
        status: status.into(),
        price: Some(100.0),
        trigger_price: None,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

/// A venue fill report naming `trade_id` — the report side of the three-way comparison.
fn fill_report(trade_id: &'static str) -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: "v-1".into(),
        client_order_id: Some("c-1".into()),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: NOW_MS,
    }
}

// ───────────────────────────── DEFECT 1: the window is a PARAMETER ─────────────────────────────

/// The window the caller passes is the window that is read — the property the hard-coded literal
/// denied. Three fills an hour apart, three windows, three answers.
#[test]
fn the_view_is_scoped_to_the_since_ms_the_caller_passes() {
    let store = MemHistStore::new();
    store
        .append_exec_fills(
            "binance",
            "BTCUSDT",
            &[
                fill("t-3h", NOW_MS - 3 * ONE_HOUR_MS),
                fill("t-2h", NOW_MS - 2 * ONE_HOUR_MS),
                fill("t-30m", NOW_MS - ONE_HOUR_MS / 2),
            ],
            Some("k-fills"),
        )
        .expect("seed fills");

    let six_hours = journal_view_from_store(&store, "binance", NOW_MS - 6 * ONE_HOUR_MS);
    assert_eq!(six_hours.seen_trade_ids.len(), 3, "a six-hour window sees all three");

    let one_hour = journal_view_from_store(&store, "binance", NOW_MS - ONE_HOUR_MS);
    assert_eq!(
        one_hour.seen_trade_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["t-30m"],
        "a one-hour window sees only the fill inside it"
    );

    let future = journal_view_from_store(&store, "binance", NOW_MS + ONE_HOUR_MS);
    assert!(future.seen_trade_ids.is_empty(), "a floor past the last row sees nothing");
}

/// **THE DEFECT, end to end.** A fill recorded three hours ago, a venue leg configured with a
/// six-hour lookback (`VIKE_RECONCILE_LOOKBACK_MS=21600000`), and local in-memory state that has
/// LOST the fill — the restore/persistence bug this third leg exists to catch.
///
/// With the journal read over the venue's own window, `diff` raises `JournalDivergence` and an
/// operator is told. With the one-hour literal the closure used to carry, the same tree yields
/// `MissingFill` — and the assertion at the end is the part that makes this a live-money defect
/// rather than a missed log line: `MissingFill` is one of the two kinds the DEFAULT `hybrid` policy
/// auto-applies, so the narrow window does not merely fail to alert, it silently FOLDS.
#[test]
fn a_journal_window_narrower_than_the_venue_window_turns_an_alert_into_an_auto_fold() {
    let store = MemHistStore::new();
    store
        .append_exec_fills(
            "binance",
            "BTCUSDT",
            &[fill("t-lost", NOW_MS - 3 * ONE_HOUR_MS)],
            Some("k-fills"),
        )
        .expect("seed fill");

    let orders: IndexMap<String, ManagedOrder> = IndexMap::new();
    let seen: HashSet<String> = HashSet::new(); // local lost it — that IS the bug being detected
    let positions: IndexMap<(String, String), f64> = IndexMap::new();
    let local = LocalView {
        venue: "binance",
        orders: &orders,
        seen_trade_ids: &seen,
        positions: &positions,
        qty_tol: 1e-9,
        cash: LocalCash::default(),
    };
    let reports = [fill_report("t-lost")];

    // The venue leg's own window (six hours), which is what the operator configured.
    let aligned = journal_view_from_store(&store, "binance", NOW_MS - 6 * ONE_HOUR_MS);
    let d = diff(&[], &reports, &[], &local, Some(&aligned));
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(
        matches!(d[0], Divergence::JournalDivergence { .. }),
        "a fill the journal recorded and local has lost is the persistence-bug signal: {d:?}"
    );

    // The literal the closure used to carry, on the SAME tree.
    let narrow = journal_view_from_store(&store, "binance", NOW_MS - ONE_HOUR_MS);
    let d = diff(&[], &reports, &[], &local, Some(&narrow));
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(
        matches!(d[0], Divergence::MissingFill(_)),
        "the narrow window downgrades the same disagreement to an ordinary catch-up fill: {d:?}"
    );
    assert!(
        vike_exec::recon::mode_applies(&ReconPolicy::hybrid(), DivergenceKind::MissingFill),
        "…and that downgrade is not merely a quieter message: `hybrid` — the DEFAULT policy, and \
         what an operator gets from an unset VIKE_RECONCILE_POLICY — AUTO-APPLIES MissingFill, so \
         the narrow window folds the divergence with no operator in front of it"
    );
}

/// The venue filter still holds: another venue's series contribute nothing, so a wider window
/// cannot leak one venue's ids into another's cross-check.
#[test]
fn only_the_named_venues_series_are_read() {
    let store = MemHistStore::new();
    store
        .append_exec_fills("binance", "BTCUSDT", &[fill("t-binance", NOW_MS)], Some("k-b"))
        .expect("seed binance");
    let mut bybit = fill("t-bybit", NOW_MS);
    bybit.venue = "bybit".into();
    store.append_exec_fills("bybit", "BTCUSDT", &[bybit], Some("k-y")).expect("seed bybit");

    let view = journal_view_from_store(&store, "binance", 0);
    assert!(view.seen_trade_ids.contains("t-binance"));
    assert!(!view.seen_trade_ids.contains("t-bybit"), "another venue's series is not read");
}

/// The order leg: the highest-ts snapshot per coid wins, the window applies to it too, and an
/// unparseable status is skipped without taking the rest of the series with it.
#[test]
fn the_order_leg_takes_the_latest_parseable_status_per_coid() {
    let store = MemHistStore::new();
    store
        .append_exec_orders(
            "binance",
            "BTCUSDT",
            &[
                order_row("c-live", "SUBMITTED", NOW_MS - 2 * ONE_HOUR_MS),
                order_row("c-live", "ACCEPTED", NOW_MS - ONE_HOUR_MS),
                order_row("c-done", "FILLED", NOW_MS - ONE_HOUR_MS),
                // A status a newer writer might emit that this build has no variant for. The
                // forward-compat guard skips it; the two rows above must survive.
                order_row("c-future", "TELEPORTED", NOW_MS - ONE_HOUR_MS),
            ],
            Some("k-orders"),
        )
        .expect("seed orders");

    let view = journal_view_from_store(&store, "binance", 0);
    assert_eq!(view.orders.get("c-live"), Some(&OrderStatus::Accepted), "highest-ts row wins");
    assert_eq!(view.orders.get("c-done"), Some(&OrderStatus::Filled));
    assert!(!view.orders.contains_key("c-future"), "an unknown status is skipped, not guessed");
    assert_eq!(view.orders.len(), 2, "…and skipping it does not drop the rest: {:?}", view.orders);

    // The same window rule as the fill leg.
    let narrow = journal_view_from_store(&store, "binance", NOW_MS - ONE_HOUR_MS / 2);
    assert!(narrow.orders.is_empty(), "every snapshot is older than this floor");
}

// ─────────────────────── DEFECT 2: a store failure must not read as silence ───────────────────────

/// A `HistStore` whose reads REFUSE. Two poses, one impl:
///
///   * [`StubMode::CatalogRefuses`] — `list_series` errors. This is the shape a `RemoteHistStore`
///     with a dead server has, and also what EVERY store that cannot enumerate returns from
///     `HistStore::list_series`'s own default body.
///   * [`StubMode::ScansRefuse`] — the catalog answers with a real `exec_fill` and `exec_order`
///     series for the venue under test, and the per-series scans then fail. That is the arm a
///     catalog-only stub cannot reach: the walk gets past `list_series` and dies one level in.
enum StubMode {
    CatalogRefuses,
    ScansRefuse,
}

struct RefusingStore(StubMode);

/// The planted reason, so an assertion could prove the STORE's own words travel if one ever needs
/// to — and so the stub never returns a bare `Ok` that would make a test vacuous.
const REFUSAL: &str = "planted: the store refused";

fn refused(verb: &str) -> DataError {
    DataError::Query(format!("{REFUSAL} ({verb})"))
}

impl HistStore for RefusingStore {
    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        match self.0 {
            StubMode::CatalogRefuses => Err(refused("list_series")),
            StubMode::ScansRefuse => Ok(vec![
                SeriesId::per_symbol("exec_fill", "binance", "BTCUSDT", None),
                SeriesId::per_symbol("exec_order", "binance", "BTCUSDT", None),
            ]),
        }
    }

    fn scan_exec_fills(&self, _venue: &str, _symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Err(refused("scan_exec_fills"))
    }

    fn scan_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, DataError> {
        Err(refused("scan_exec_orders"))
    }

    // ---- everything below is off this function's walk and says so if reached ----
    fn load_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        Err(refused("load_bars"))
    }
    fn scan_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        Err(refused("scan_quotes"))
    }
    fn scan_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        Err(refused("scan_trades"))
    }
    fn append_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _bars: &[Bar],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("append_bars"))
    }
    fn append_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[QuoteTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("append_quotes"))
    }
    fn append_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[TradeTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("append_trades"))
    }
    fn append_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _updates: &[BookUpdate],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("append_book_updates"))
    }
    fn scan_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Err(refused("scan_book_updates"))
    }
    fn append_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[(i64, SymbolProperties)],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("append_symbol_properties"))
    }
    fn scan_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Err(refused("scan_symbol_properties"))
    }
    fn append_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[EquitySample],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("append_equity"))
    }
    fn scan_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        Err(refused("scan_equity"))
    }
    fn append_exec_fills(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecFillRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("append_exec_fills"))
    }
    fn append_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecOrderRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("append_exec_orders"))
    }
    fn resample_quotes_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("resample_quotes_to_bars"))
    }
    fn resample_trades_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(refused("resample_trades_to_bars"))
    }
}

/// A catalog that refuses degrades to an EMPTY view rather than panicking or hanging — and the
/// control beside it is what makes that mean anything: the same call over a SEEDED store is
/// non-empty, so "empty" here is the refusal and not the function reporting nothing on every input.
#[test]
fn a_store_whose_catalog_refuses_degrades_to_an_empty_view() {
    let view = journal_view_from_store(&RefusingStore(StubMode::CatalogRefuses), "binance", 0);
    assert!(view.seen_trade_ids.is_empty());
    assert!(view.orders.is_empty());

    let seeded = MemHistStore::new();
    seeded
        .append_exec_fills("binance", "BTCUSDT", &[fill("t-1", NOW_MS)], Some("k"))
        .expect("seed fill");
    assert!(
        !journal_view_from_store(&seeded, "binance", 0).seen_trade_ids.is_empty(),
        "the control: a store that ANSWERS produces a non-empty view, so the assertions above are \
         about the refusal rather than about this function always returning nothing"
    );
}

/// The arm past the catalog: the inventory lists the venue's two exec series and every per-series
/// scan then fails. Both legs degrade independently and neither takes the pass down with it.
#[test]
fn a_store_whose_scans_refuse_degrades_to_an_empty_view() {
    let view = journal_view_from_store(&RefusingStore(StubMode::ScansRefuse), "binance", 0);
    assert!(view.seen_trade_ids.is_empty(), "the fill scan failed, so no trade ids");
    assert!(view.orders.is_empty(), "the order scan failed, so no statuses");
}

/// **WHY the failure paths have to LOG**, stated as an assertion instead of as a comment: an empty
/// journal view produces the SAME `diff` verdict as passing no journal at all. The value carries no
/// signal a caller could branch on, so if the builder does not say it out loud, a store outage and
/// a quiet account are the same event to everything downstream.
#[test]
fn an_empty_view_is_indistinguishable_from_no_journal_at_all() {
    let orders: IndexMap<String, ManagedOrder> = IndexMap::new();
    let seen: HashSet<String> = HashSet::new();
    let positions: IndexMap<(String, String), f64> = IndexMap::new();
    let local = LocalView {
        venue: "binance",
        orders: &orders,
        seen_trade_ids: &seen,
        positions: &positions,
        qty_tol: 1e-9,
        cash: LocalCash::default(),
    };
    let reports = [fill_report("t-lost")];

    let degraded = journal_view_from_store(&RefusingStore(StubMode::CatalogRefuses), "binance", 0);
    let with_empty = diff(&[], &reports, &[], &local, Some(&degraded));
    let without = diff(&[], &reports, &[], &local, None);

    assert_eq!(with_empty.len(), without.len(), "{with_empty:?} vs {without:?}");
    assert!(matches!(with_empty[0], Divergence::MissingFill(_)));
    assert!(matches!(without[0], Divergence::MissingFill(_)));
}
