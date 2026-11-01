//! Store-backed gate for `default_equity_curve` (decision 0243): the stored `portfolio/TOTAL` equity
//! series is plotted when the journal records one currency, and NOT when it records a ledger that is
//! not the dollar's, because the series is one currency's and states none (the sheet would read a
//! euro curve as dollars beside dollar trades).
//!
//! Seeds the shared `vike_data::MemHistStore` double through `append_equity`, keyed on
//! `("portfolio", "TOTAL")` exactly as the code under test reads it.
//!
//! ⚠ Whole-file gated on the `tearsheet` feature, like `tests/mtm_store.rs`.
#![cfg(feature = "tearsheet")]

use vike_analytics::reconstruct_trades;
use vike_backend::tearsheet_cli::store::default_equity_curve;
use vike_data::{HistStore, MemHistStore};
use vike_model::EquitySample;
use vike_model::events::{FillEvent, PositionSide, TradeId};

fn fill(side: i32, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("t", ts),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: 1.0,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: Some(px),
        position_side: PositionSide::Both,
    }
}

/// A store holding a three-point `portfolio/TOTAL` series (5000, 5100, 5200) that no realized-only
/// curve over [`fills`] can equal.
fn store_with_total() -> MemHistStore {
    let store = MemHistStore::new();
    let rows: Vec<EquitySample> = [(1, 5_000.0), (2, 5_100.0), (3, 5_200.0)]
        .into_iter()
        .map(|(ts, equity)| EquitySample {
            ts,
            venue: "TOTAL".into(),
            equity,
            realized: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
        })
        .collect();
    store.append_equity("portfolio", "TOTAL", &rows, None).expect("MemHistStore accepts the seed");
    store
}

/// One round trip, +10: the realized-only curve is `[seed, seed + 10]`.
fn fills() -> Vec<FillEvent> {
    vec![fill(1, 100.0, 10), fill(-1, 110.0, 20)]
}

#[test]
fn a_dollar_journal_plots_the_stored_total_series() {
    let trades = reconstruct_trades(&fills());
    let (eq, ts) = default_equity_curve(&store_with_total(), &trades, 1_000.0, None);
    assert_eq!(eq, vec![5_000.0, 5_100.0, 5_200.0], "the stored series, unchanged");
    assert_eq!(ts, vec![1, 2, 3]);
}

#[test]
fn a_journal_with_a_non_dollar_ledger_does_not_plot_the_stored_total_series() {
    let trades = reconstruct_trades(&fills());
    let (eq, ts) = default_equity_curve(&store_with_total(), &trades, 1_000.0, Some("fxcm EUR"));
    assert_eq!(
        eq,
        vec![1_000.0, 1_010.0],
        "the realized-only curve of the dollar fills: the stored TOTAL may be euros"
    );
    assert_eq!(ts, vec![20, 20], "the seed point is stamped with the first exit");
}

#[test]
fn a_dollar_journal_with_no_stored_series_falls_back_as_before() {
    let trades = reconstruct_trades(&fills());
    let (eq, _) = default_equity_curve(&MemHistStore::new(), &trades, 1_000.0, None);
    assert_eq!(eq, vec![1_000.0, 1_010.0]);
}
