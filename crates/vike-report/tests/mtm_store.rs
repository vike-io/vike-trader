//! Store-backed gate for `mtm_curve_from_store`: fold a synthetic journal-fill stream against a
//! seeded bar store and check the reconstructed mark-to-market equity/exposure — including a
//! sample with no as-of price (the missing-price gap is FLAGGED, not dropped).
//!
//! Uses a minimal DataFusion-free `HistStore` double (only `load_bars` carries behavior) — the
//! same pattern `src/excursions.rs`'s tests use, since `vike_data::MemHistStore::load_bars` is
//! inert and can never return prices.

use std::collections::HashMap;

use vike_data::{DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_model::events::{FillEvent, TradeId};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};
use vike_report::{mtm_curve_from_store, mtm_equity_curve, RuntimeStats};

/// A bars-only `HistStore` double: `load_bars` returns the seeded bars for a `(venue, symbol)` key
/// filtered to the requested range; every other method is an inert stub.
#[derive(Default)]
struct SeededBarStore {
    bars: HashMap<(String, String), Vec<Bar>>,
}

impl SeededBarStore {
    fn with_bars(venue: &str, symbol: &str, bars: Vec<Bar>) -> Self {
        let mut m = HashMap::new();
        m.insert((venue.to_string(), symbol.to_string()), bars);
        Self { bars: m }
    }
}

fn in_range(ts: i64, r: TsRange) -> bool {
    r.start.map(|s| ts >= s).unwrap_or(true) && r.end.map(|e| ts <= e).unwrap_or(true)
}

impl HistStore for SeededBarStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        _interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        Ok(self
            .bars
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|v| v.iter().filter(|b| in_range(b.ts, range)).cloned().collect())
            .unwrap_or_default())
    }
    fn scan_quotes(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        Ok(Vec::new())
    }
    fn scan_trades(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<TradeTick>, DataError> {
        Ok(Vec::new())
    }
    fn append_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[Bar],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_quotes(
        &self,
        _: &str,
        _: &str,
        _: &[QuoteTick],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_trades(
        &self,
        _: &str,
        _: &str,
        _: &[TradeTick],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_book_updates(
        &self,
        _: &str,
        _: &str,
        _: &[BookUpdate],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_book_updates(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Ok(Vec::new())
    }
    fn append_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: &[(i64, SymbolProperties)],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Ok(Vec::new())
    }
    fn append_equity(
        &self,
        _: &str,
        _: &str,
        _: &[EquitySample],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_equity(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<EquitySample>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_fills(
        &self,
        _: &str,
        _: &str,
        _: &[ExecFillRow],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_orders(
        &self,
        _: &str,
        _: &str,
        _: &[ExecOrderRow],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_orders(&self, _: &str, _: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        Ok(Vec::new())
    }
    fn resample_quotes_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TsRange,
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn resample_trades_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TsRange,
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
}

fn fill(side: i32, qty: f64, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        // minted here, not read off a wire — same `t<ts>` bytes as before
        trade_id: TradeId::prefixed("t", ts),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    }
}

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn mtm_curve_from_store_reconstructs_marked_equity() {
    // A long round-trip; the bar closes rise 100 -> 110 -> 120 across the hold.
    let store = SeededBarStore::with_bars(
        "binance",
        "BTCUSDT",
        vec![bar(1000, 100.0), bar(2000, 110.0), bar(3000, 120.0), bar(4000, 120.0)],
    );
    let fills = vec![fill(1, 1.0, 100.0, 1000), fill(-1, 1.0, 120.0, 3000)];
    let points = mtm_curve_from_store(&store, &fills, 1_000.0, "binance", "1m").expect("mtm");

    // Sample grid = union of the fill timestamps {1000,3000} and the bars inside the fill window
    // [1000,3000] = {1000,2000,3000}; the 4000 bar is outside the window, so it is not a mark.
    let (eq, ts) = mtm_equity_curve(&points);
    assert_eq!(ts, vec![1000, 2000, 3000]);
    // 1000: flat @ mark 100. 2000: long marked at 110 -> +10. 3000: closed +20, flat.
    assert_eq!(eq, vec![1_000.0, 1_010.0, 1_020.0]);
    assert!(points.iter().all(|p| p.missing_prices == 0), "every sample is priced");

    let rs = RuntimeStats::from_points(&points);
    assert_eq!(rs.traded_notional, 220.0, "|1|*100 + |1|*120");
    assert_eq!(rs.peak_margin_used, 110.0, "peak gross notional is the 110 mark at ts 2000");
}

#[test]
fn store_flags_missing_price_before_first_bar() {
    // The buy lands at ts 500, BEFORE the first bar (1000) — so the ts-500 sample has no as-of
    // price for the open position: it must be FLAGGED missing, not dropped.
    let store =
        SeededBarStore::with_bars("binance", "BTCUSDT", vec![bar(1000, 100.0), bar(2000, 110.0)]);
    let fills = vec![fill(1, 1.0, 100.0, 500), fill(-1, 1.0, 110.0, 2000)];
    let points = mtm_curve_from_store(&store, &fills, 1_000.0, "binance", "1m").expect("mtm");

    // grid = {500,2000} ∪ bars in [500,2000] {1000,2000} = {500,1000,2000}.
    assert_eq!(points.len(), 3, "three samples, including the pre-bar one");
    let at500 = points.iter().find(|p| p.ts == 500).expect("a sample at ts 500");
    assert_eq!(at500.missing_prices, 1, "no bar at/before 500 -> flagged, not dropped");
    assert_eq!(at500.unrealized, 0.0, "silent-zero for the unpriceable open position");
    assert_eq!(at500.gross_notional, 0.0, "unpriceable -> excluded from notional (LEAN skip)");
    assert_eq!(at500.equity, 1_000.0, "equity still resolves through the gap");
    let at1000 = points.iter().find(|p| p.ts == 1000).expect("a sample at ts 1000");
    assert_eq!(at1000.missing_prices, 0, "the 1000 bar prices the position");
}
