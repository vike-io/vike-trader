//! In-memory [`HistStore`] for tests — the test double venue bridges build a
//! [`crate::PropertiesRecorder`] over when asserting their opt-in properties-recording wiring, WITHOUT
//! pulling the heavy Arrow/DataFusion tree (the crate `vike-store`) into their test builds.
//!
//! Also home to the shared [`RecordingSink`]/[`NoopSink`] `LiveDataSink` doubles (testing-arch
//! Phase 4d) - one capturing sink instead of the ~13 inline copies the venue bridges carried.
//!
//! And home to `vike_data::hist_store_stubs!`, the one spelling of the required-verb stubs every
//! hand-written `HistStore` double used to carry (`test_support/stubs.rs`).
//!
//! Behind the `test-support` feature (off by default). The `bars`, `symbol_properties` and
//! `equity` methods carry real behavior — they honor the batch-level `commit_key` idempotency
//! contract (a repeated key is a no-op) and return rows ts-ascending, matching `DataFusionHist`.
//! Unlike `DataFusionHist` (which has no venue/symbol Parquet column and must re-inject
//! `EquitySample.venue` from the caller's `symbol` argument on scan — see `vike-store`'s
//! `datafusion_hist::codec::batch_to_equities`), this in-memory double holds the real domain value,
//! so `equity` rows come back with whatever `.venue` the caller originally passed in, untouched.
//! Bars take the OPPOSITE care for the same fidelity reason: [`persisted_bar`] erases on append the
//! fields the real bar schema never persists, so the round trip loses exactly what
//! `DataFusionHist`'s does. The still-inert remainder: quotes/trades/book read empty and append
//! zero rows, depth inherits the trait's refusing defaults, and the two `resample_*_to_bars` verbs
//! return `Ok(0)` HONESTLY rather than fabricating — this double never holds ticks, and resampling
//! zero ticks derives zero bars on the real store too. The catalog pair
//! (`list_series`/`inventory`) answers with a REAL fold over exactly the held seams — an honest
//! empty when nothing was seeded — rather than inheriting a default: the trait's old empty default
//! had a SEEDED double answering "no series" while holding rows, and the refusing default that
//! replaced it would deny a fold this double can genuinely perform.
//!
//! Empty-batch fidelity: every real append here returns `Ok(0)` for an empty `rows` slice WITHOUT
//! consuming the `commit_key`, matching `DataFusionHist::commit_rows` (which short-circuits on
//! `ts.is_empty()` before registering the key). A double that burned the key on an empty batch would
//! turn a subsequent real append under that key into a no-op that happens only in tests.

use vike_marketdata::Bar;

use crate::store::hist::HistStore;

mod mem_store;
mod sinks;
// `hist_store_stubs!`, exported at the crate root by `#[macro_export]` (so no `pub use` here).
mod stubs;
/// The citation readers the store-table text gates share (this crate's `tests/series_cadence_gate.rs`,
/// vike-store's `tests/store/live_rec_marker.rs`, vike-ops' `store_kind_gate`).
pub mod text;

pub use mem_store::MemHistStore;
pub use sinks::{NoopSink, RecordingSink, SinkCall};

/// What a bar looks like AFTER the real store's round trip: the bar schema persists
/// ts/OHLC/volume/funding and no bid/ask/symbol column, so
/// `crates/vike-store/src/datafusion_hist/codec/market.rs`'s `bars_from_batch` decodes those three as
/// `None` regardless of what was appended. The double erases at the same boundary (the write), so
/// a test asserting `symbol` survives `append_bars` → `load_bars` fails here exactly as it would
/// on `DataFusionHist`.
fn persisted_bar(b: &Bar) -> Bar {
    Bar { bid: None, ask: None, symbol: None, ..b.clone() }
}

/// One journalled order row: `coid`, a `1.0` LIMIT buy of binance `BTCUSDT` at `100.0`, in `status`
/// at `ts`, nothing filled, no trigger, venue id or account.
pub fn exec_order_row(coid: &str, status: &str, ts: i64) -> crate::ExecOrderRow {
    crate::ExecOrderRow {
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
        account: None,
    }
}

/// A book DATA event of 2 levels (`0.45 x 10` bid, `0.46 x 5` ask, tick `0.01`) at `(ts, seq)`, of
/// `kind`: what a series-quality score counts as data. `local_ts` is `ts`, the symbol empty.
pub fn book_event(
    ts: i64,
    seq: u64,
    kind: vike_marketdata::BookUpdateKind,
) -> vike_marketdata::BookUpdate {
    vike_marketdata::BookUpdate {
        ts,
        local_ts: ts,
        seq,
        kind,
        tick_size: 0.01,
        bids: vec![vike_marketdata::BookLevel::new(0.45, 10.0)],
        asks: vec![vike_marketdata::BookLevel::new(0.46, 5.0)],
        symbol: String::new(),
    }
}

/// A §B status MARKER (zero levels, seq 0, tick 0) at `ts`, of `kind` — exactly what
/// `live_rec::stream_status` records. `local_ts` is `ts`, the symbol empty.
pub fn status_marker(
    ts: i64,
    kind: vike_marketdata::BookUpdateKind,
) -> vike_marketdata::BookUpdate {
    vike_marketdata::BookUpdate {
        ts,
        local_ts: ts,
        seq: 0,
        kind,
        tick_size: 0.0,
        bids: Vec::new(),
        asks: Vec::new(),
        symbol: String::new(),
    }
}

/// The rows the grouped tick-paging suites plant — vike-store's `tests/reads/tick_scan_head.rs` (the
/// store's scan head) and vike-datahub's `tests/datafusion_serve/grouped_tick_paging.rs` (the same
/// paging over the wire): a tick of `sym` at `ts` carries its discriminator `d` as its price, so the
/// judge can tell every planted row apart.
pub mod grouped_ticks {
    use vike_marketdata::{BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

    /// `d` bid over `d + 0.5` ask, both sizes `1.0`, `local_ts` 0.
    pub fn quote(sym: &str, ts: i64, d: i64) -> QuoteTick {
        let (bid, ask) = (d as f64, d as f64 + 0.5);
        QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: sym.into() }
    }

    /// A taker BUY (`is_buyer_maker: false`) of `1.0` at `d`, `local_ts` 0.
    pub fn trade(sym: &str, ts: i64, d: i64) -> TradeTick {
        TradeTick {
            ts,
            local_ts: 0,
            price: d as f64,
            size: 1.0,
            is_buyer_maker: false,
            symbol: sym.into(),
        }
    }

    /// A `Delta` of one bid (`d`) and one ask (`d + 0.5`) level at seq `d`, tick `0.01`: ONE event is
    /// TWO stored rows (a row budget counts rows, and the judge counts them the same way).
    pub fn book(sym: &str, ts: i64, d: i64) -> BookUpdate {
        BookUpdate {
            ts,
            local_ts: 0,
            seq: d as u64,
            kind: BookUpdateKind::Delta,
            tick_size: 0.01,
            bids: vec![BookLevel { price: d as f64, qty: 1.0 }],
            asks: vec![BookLevel { price: d as f64 + 0.5, qty: 1.0 }],
            symbol: sym.into(),
        }
    }
}

// ===================================================================================================
// The double's own unit tests — the BAR verbs. The older seams are covered by the integration
// suites in `crates/vike-store/tests/series/` (equity_series.rs's `mem_tests`, exec_log_series.rs,
// funding_series.rs, chain_series.rs) and `crates/vike-store/tests/store/depth_and_budget.rs`'s
// seeded-catalog test; bars are tested HERE, beside the erasure helper whose behavior half of them
// pin. Runs in the plain `cargo test -p vike-data`
// roster lane: `lib.rs` compiles this module under `#[cfg(any(test, feature = "test-support"))]`.
// ===================================================================================================

#[path = "test_support_tests.rs"]
#[cfg(test)]
mod test_support_tests;
