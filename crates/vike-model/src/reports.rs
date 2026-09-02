//! Normalized, venue-agnostic reconciliation report rows — the report-level granularity the
//! reconciliation engine (`vike-exec::recon`) diffs against local state. Wire-serde like `Event`
//! so fixtures and journal cross-checks round-trip. A superset of `ReconcileSnapshot`'s net-position
//! view: reports carry per-order and per-fill history, which net-position alone cannot reconstruct.

use crate::events::{LiquiditySide, PositionSide, TradeId};
use crate::MarginMode;
use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// One open/closed order as the venue currently reports it (REST order-status query).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderStatusReport {
    pub venue: String,
    pub symbol: String,
    pub venue_order_id: CompactString,
    /// None when the venue did not echo our client id (externally-placed order).
    pub client_order_id: Option<String>,
    pub side: i32,
    pub order_type: String,
    pub qty: f64,
    pub filled_qty: f64,
    pub avg_px: f64,
    /// Venue status normalized to our FSM vocabulary (e.g. "FILLED", "PARTIALLY_FILLED").
    pub status: String,
    pub ts: i64,
}

/// One execution as the venue reports it (REST trade/fill history query).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillReport {
    pub venue: String,
    pub symbol: String,
    pub trade_id: TradeId,
    pub venue_order_id: CompactString,
    pub client_order_id: Option<String>,
    pub side: i32,
    pub last_qty: f64,
    pub last_px: f64,
    pub commission: f64,
    pub commission_asset: String,
    pub liquidity_side: LiquiditySide,
    pub ts: i64,
}

/// One net position as the venue reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionStatusReport {
    pub venue: String,
    pub symbol: String,
    pub position_side: PositionSide,
    /// signed net quantity
    pub qty: f64,
    pub avg_px: f64,
    pub ts: i64,
    /// The venue-REPORTED margin mode of this position row (margin-mode step-2: read-side only —
    /// vike still SENDS what it sent before). Parsed only where the venue payload demonstrably
    /// carries one (binance/aster `marginType`, bybit `tradeMode`, okx `mgnMode`, hyperliquid
    /// `leverage.type`); absent/unrecognized ⇒ [`MarginMode::Cross`] — fail-safe, byte-identical
    /// to pre-field behavior. `skip_serializing_if` keeps a cross row's JSON identical to
    /// pre-field records (the same discipline as `PositionEntry.margin_mode`, #487).
    #[serde(default, skip_serializing_if = "MarginMode::is_cross")]
    pub margin_mode: MarginMode,
    /// Isolated-wallet balance where the venue reports one (binance `isolatedWallet` /
    /// `isolatedMargin`, okx `margin`). `None` = cross, or the venue doesn't surface it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolated_margin: Option<f64>,
    /// Venue-REPORTED per-position DELTA in COIN units, where the venue surfaces one (Deribit
    /// `get_positions.delta`). Correct for an INVERSE contract, whose `qty`/`size` above is USD
    /// notional rather than coin — so `delta` is the robust coin-exposure source a linear
    /// perp/future hedge leg folds into net greeks (`coin_delta × spot`). `None` for every venue
    /// that does not report one (all but Deribit today) — additive and byte-identical there:
    /// `skip_serializing_if` keeps a no-delta row's JSON identical to pre-field records, same
    /// discipline as `margin_mode`/`isolated_margin` above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
}

impl PositionStatusReport {
    /// The synthesized FLAT position row — `qty: 0.0`, `avg_px: 0.0`, one-way [`PositionSide::Both`]
    /// with the sign (there is none) carried in `qty`, default [`MarginMode`] (Cross), `ts: 0`, no
    /// isolated margin. Load-bearing for reconciliation: a venue that reports NO open position for a
    /// mounted symbol (many venues OMIT a flat symbol from `/positions` entirely) still needs a row
    /// PRESENT so `vike_exec::recon::diff` can detect a stale LOCAL position the venue has since
    /// closed. Every venue's `ReconClient` synthesizes byte-identical rows for that case — this is
    /// the ONE constructor they all call (ig/oanda/fxcm/alpaca/okx/ctrader/ibkr).
    #[must_use]
    pub fn flat(venue: &str, symbol: &str) -> Self {
        PositionStatusReport {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            position_side: PositionSide::Both,
            qty: 0.0,
            avg_px: 0.0,
            ts: 0,
            margin_mode: MarginMode::default(),
            isolated_margin: None,
            delta: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_report_roundtrips_through_json() {
        let r = FillReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade_id: "t1".into(),
            venue_order_id: "v1".into(),
            client_order_id: Some("c1".into()),
            side: 1,
            last_qty: 0.5,
            last_px: 30000.0,
            commission: 0.1,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 123,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: FillReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    /// Margin-mode step-2 compat: a pre-field position report (no `margin_mode`/`isolated_margin`
    /// keys) deserializes to the Cross/None defaults, a cross row serializes WITHOUT the keys
    /// (byte-identical to pre-field records), and an isolated row round-trips.
    #[test]
    fn position_report_margin_fields_default_and_roundtrip() {
        let legacy = r#"{"venue":"binance","symbol":"BTCUSDT","position_side":"BOTH","qty":1.0,"avg_px":100.0,"ts":1}"#;
        let r: PositionStatusReport = serde_json::from_str(legacy).unwrap();
        assert_eq!(r.margin_mode, MarginMode::Cross);
        assert_eq!(r.isolated_margin, None);
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("margin_mode") && !json.contains("isolated_margin"),
            "cross serializes to nothing: {json}"
        );

        let iso = PositionStatusReport {
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(75.0),
            ..r
        };
        let json = serde_json::to_string(&iso).unwrap();
        assert!(json.contains(r#""margin_mode":"Isolated""#), "{json}");
        let back: PositionStatusReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, iso);
    }

    /// `PositionStatusReport::flat` builds exactly the byte-identical flat row the seven venue
    /// `ReconClient`s hand-rolled before the dedup: one-way `BOTH`, zero qty/px/ts, default (Cross)
    /// margin, no isolated margin — and it serializes to the same legacy-compatible JSON (a cross
    /// flat row emits neither margin key), so swapping the call sites changes no wire bytes.
    #[test]
    fn flat_builds_the_canonical_flat_row() {
        let f = PositionStatusReport::flat("ig", "CS.D.EURUSD.MINI.IP");
        assert_eq!(f.venue, "ig");
        assert_eq!(f.symbol, "CS.D.EURUSD.MINI.IP");
        assert_eq!(f.position_side, PositionSide::Both);
        assert_eq!(f.qty, 0.0);
        assert_eq!(f.avg_px, 0.0);
        assert_eq!(f.ts, 0);
        assert_eq!(f.margin_mode, MarginMode::default());
        assert_eq!(f.isolated_margin, None);
        // byte-identical to the hand-rolled literal every venue used
        let hand_rolled = PositionStatusReport {
            venue: "ig".to_string(),
            symbol: "CS.D.EURUSD.MINI.IP".to_string(),
            position_side: PositionSide::Both,
            qty: 0.0,
            avg_px: 0.0,
            ts: 0,
            margin_mode: MarginMode::default(),
            isolated_margin: None,
            delta: None,
        };
        assert_eq!(f, hand_rolled);
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            !json.contains("margin_mode") && !json.contains("isolated_margin"),
            "cross flat row stays legacy-compatible: {json}"
        );
    }
}
