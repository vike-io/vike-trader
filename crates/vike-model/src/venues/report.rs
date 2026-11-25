//! Normalized, venue-agnostic reconciliation report rows — the report-level granularity the
//! reconciliation engine (`vike-execution::recon`) diffs against local state. Wire-serde like `Event`
//! so fixtures and journal cross-checks round-trip. A superset of `ReconcileSnapshot`'s net-position
//! view: reports carry per-order and per-fill history, which net-position alone cannot reconstruct.

use crate::events::{LiquiditySide, PositionSide, TradeId};
use crate::{ContractValue, MarginMode, OrderStatus, OrderType, PositionLeg};
use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// One open/closed order as the venue currently reports it (REST order-status query).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderStatusReport {
    pub venue: String,
    pub symbol: String,
    pub venue_order_id: CompactString,
    /// None when the venue did not echo our client id (externally-placed order).
    pub order_client_id: Option<String>,
    pub side: i32,
    /// The order kind, mapped by each venue's parser from its own word. A venue kind with no twin
    /// among ours stays [`OrderType::Unrecognized`] carrying the parser's spelling of it, so an
    /// adopted order (`OrderRecord::from_status_report`) keeps the venue's text and the
    /// capability preflight never re-sends it. Serialized as [`OrderType`]'s own word.
    pub order_type: OrderType,
    pub qty: f64,
    pub filled_qty: f64,
    pub average_price: f64,
    /// Venue status mapped by each venue's parser onto the FSM. Serialized as the FSM's uppercase
    /// word ([`OrderStatus::as_str`]: `"FILLED"`, `"PARTIALLY_FILLED"`), NOT the enum's own
    /// PascalCase serde spelling (which `OrderRecord` persists): report rows already written keep
    /// their bytes, and a word outside the FSM is refused on read rather than guessed.
    #[serde(with = "state_machine_status_word")]
    pub status: OrderStatus,
    pub timestamp: i64,
    /// The order's TERMS as the venue's row states them, in [`crate::OrderRequest`]'s meaning, so
    /// [`crate::OrderRecord::from_status_report`] copies them onto the rebuilt request: the limit
    /// price, the trigger price (a trailing kind's ACTIVATION price, never its moving stop), the
    /// trailing distance in price and the trailing distance as a PERCENT. A bridge fills only what
    /// its venue's row carries; `None` is "not stated", never zero. All four are absent from the
    /// JSON when `None`, so a report written before them, and every report that states none, keeps
    /// its bytes, and a reader older than them ignores the keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_percent: Option<f64>,
}

/// The serde form of [`OrderStatusReport::status`]: the FSM's uppercase word both ways, through
/// [`OrderStatus::as_str`] and [`OrderStatus::parse`]. An unknown word is a deserialize ERROR.
mod state_machine_status_word {
    use crate::OrderStatus;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(s: &OrderStatus, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(s.as_str())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<OrderStatus, D::Error> {
        let word = String::deserialize(de)?;
        OrderStatus::parse(&word).ok_or_else(|| {
            serde::de::Error::custom(format_args!("unknown order status word {word:?}"))
        })
    }
}

/// One execution as the venue reports it (REST trade/fill history query).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillReport {
    pub venue: String,
    pub symbol: String,
    pub trade_id: TradeId,
    pub venue_order_id: CompactString,
    pub order_client_id: Option<String>,
    pub side: i32,
    pub last_qty: f64,
    pub last_price: f64,
    pub commission: f64,
    pub commission_asset: String,
    pub liquidity_side: LiquiditySide,
    pub timestamp: i64,
}

/// One net position as the venue reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionStatusReport {
    pub venue: String,
    pub symbol: String,
    pub position_side: PositionSide,
    /// signed net quantity
    pub qty: f64,
    pub average_price: f64,
    pub timestamp: i64,
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
    /// The synthesized FLAT position row — `qty: 0.0`, `average_price: 0.0`, one-way [`PositionSide::Both`]
    /// with the sign (there is none) carried in `qty`, default [`MarginMode`] (Cross), `ts: 0`, no
    /// isolated margin. Load-bearing for reconciliation: a venue that reports NO open position for a
    /// mounted symbol (many venues OMIT a flat symbol from `/positions` entirely) still needs a row
    /// PRESENT so `vike_execution::recon::diff` can detect a stale LOCAL position the venue has since
    /// closed. Every venue's `ReconcileClient` synthesizes byte-identical rows for that case — this is
    /// the ONE constructor they all call (ig/oanda/fxcm/alpaca/okx/ctrader/ibkr).
    #[must_use]
    pub fn flat(venue: &str, symbol: &str) -> Self {
        PositionStatusReport {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            position_side: PositionSide::Both,
            qty: 0.0,
            average_price: 0.0,
            timestamp: 0,
            margin_mode: MarginMode::default(),
            isolated_margin: None,
            delta: None,
        }
    }

    /// **One symbol's position rows from the pieces a venue keeps it in** (deals, trades, orders: a
    /// [`crate::PositionRows::FromLegs`] venue), under the locked-pair rule (owner, 2026-11-11):
    ///
    /// - legs in ONE direction are one [`PositionSide::Both`] row: `qty` the signed sum, `average_price` the
    ///   size-weighted average of the legs' own prices ([`ContractValue::add_quantity_price`] over `value`, so
    ///   harmonic on an inverse contract), `ts` the newest leg's;
    /// - legs in BOTH directions (a locked or partly hedged pair, which one net number would hide) are
    ///   a [`PositionSide::Long`] row and a [`PositionSide::Short`] row, each summed the same way. Only
    ///   legs of one direction are ever averaged together, so the price is each side's own entry; the
    ///   two rows still sum to the net;
    /// - a zero-size leg (the empty position a pending order makes) carries nothing and is dropped,
    ///   and no leg left is the [`Self::flat`] row, which reconcile needs present;
    /// - `average_price` is `0.0`, "no usable price", when any leg of a row has none (not finite, or not
    ///   positive): reconcile then holds that row and books nothing at it.
    ///
    /// The rest of each row is [`Self::flat`]'s (Cross, no isolated margin, no delta). One golden file,
    /// `crates/bridges/dukascopy/data/position_legs.json`, pins this and the Dukascopy sidecar's
    /// `NetPosition.java` to the same answers.
    #[must_use]
    pub fn from_legs(
        venue: &str,
        symbol: &str,
        legs: &[PositionLeg],
        value: ContractValue,
    ) -> Vec<Self> {
        let (longs, shorts): (Vec<&PositionLeg>, Vec<&PositionLeg>) =
            legs.iter().filter(|leg| leg.size != 0.0).partition(|leg| leg.size > 0.0);
        let row = |side, legs: &[&PositionLeg]| {
            let (mut qty, mut held, mut held_quantity_price, mut priced, mut ts) =
                (0.0, 0.0, 0.0, true, 0_i64);
            for leg in legs {
                held_quantity_price =
                    value.add_quantity_price(held, held_quantity_price, leg.size.abs(), leg.price);
                held += leg.size.abs();
                qty += leg.size;
                priced &= leg.price.is_finite() && leg.price > 0.0;
                ts = ts.max(leg.timestamp_ms);
            }
            let avg_px = if priced { held_quantity_price / held } else { 0.0 };
            PositionStatusReport {
                position_side: side,
                qty,
                average_price: avg_px,
                timestamp: ts,
                ..Self::flat(venue, symbol)
            }
        };
        match (longs.is_empty(), shorts.is_empty()) {
            (true, true) => vec![Self::flat(venue, symbol)],
            (false, false) => {
                vec![row(PositionSide::Long, &longs), row(PositionSide::Short, &shorts)]
            }
            _ => vec![row(PositionSide::Both, &[longs, shorts].concat())],
        }
    }

    /// **A spot pair's wallet holding as its position row** (the spot rule, owner 2026-11-11): `held`
    /// is the BASE coin's balance as the wallet states it, `0.0` for a coin the answer does not list.
    /// Under ONE size `step` of the pair (dust a fee or a transfer left), or not a number, it is a FLAT
    /// row: like a derivative's, the row must be present for reconcile to see a local position the
    /// wallet no longer holds. Booked [`MarginMode::Cash`] (fully funded, never liquidated) and one-way
    /// [`PositionSide::Both`]; `average_price` is `0.0`, because a wallet's cost basis is not the strategy's,
    /// so reconcile books nothing at it. How a venue reads the balance and the step stays its bridge's.
    #[must_use]
    pub fn spot_holding(venue: &str, symbol: &str, held: f64, step: f64, ts: i64) -> Self {
        PositionStatusReport {
            qty: if held >= step { held } else { 0.0 },
            timestamp: ts,
            margin_mode: MarginMode::Cash,
            ..Self::flat(venue, symbol)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An order report keeps the FSM's UPPERCASE status word on the wire (not the enum's
    /// PascalCase serde form), round-trips, and REFUSES a word outside the FSM rather than guess.
    #[test]
    fn order_report_status_is_the_state_machine_word_on_the_wire() {
        let r = OrderStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            venue_order_id: "v1".into(),
            order_client_id: Some("c1".into()),
            side: 1,
            order_type: OrderType::Limit,
            qty: 1.0,
            filled_qty: 0.5,
            average_price: 100.0,
            status: OrderStatus::PartiallyFilled,
            timestamp: 7,
            price: None,
            trigger_price: None,
            trail: None,
            trail_percent: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""status":"PARTIALLY_FILLED""#), "{json}");
        assert!(json.contains(r#""order_type":"limit""#), "{json}");
        let back: OrderStatusReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        let unknown = json.replace("PARTIALLY_FILLED", "PartiallyFilled");
        assert!(serde_json::from_str::<OrderStatusReport>(&unknown).is_err(), "{unknown}");
    }

    /// The four terms are additive: a report that states none has the bytes it had before them (and a
    /// report written before them reads back with none), and one that states them round-trips.
    #[test]
    fn order_report_terms_are_absent_when_unstated_and_round_trip_when_stated() {
        let legacy = r#"{"venue":"binance","symbol":"BTCUSDT","venue_order_id":"v1","order_client_id":"c1","side":-1,"order_type":"stop_limit","qty":1.0,"filled_qty":0.0,"average_price":0.0,"status":"ACCEPTED","timestamp":7}"#;
        let r: OrderStatusReport = serde_json::from_str(legacy).unwrap();
        assert_eq!((r.price, r.trigger_price, r.trail, r.trail_percent), (None, None, None, None));
        assert_eq!(serde_json::to_string(&r).unwrap(), legacy, "no term: the old bytes");

        let stated = OrderStatusReport {
            price: Some(99.5),
            trigger_price: Some(100.0),
            trail: Some(2.5),
            trail_percent: Some(1.5),
            ..r
        };
        let json = serde_json::to_string(&stated).unwrap();
        assert!(
            json.ends_with(
                r#""timestamp":7,"price":99.5,"trigger_price":100.0,"trail":2.5,"trail_percent":1.5}"#
            ),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<OrderStatusReport>(&json).unwrap(), stated);
    }

    #[test]
    fn fill_report_roundtrips_through_json() {
        let r = FillReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade_id: "t1".into(),
            venue_order_id: "v1".into(),
            order_client_id: Some("c1".into()),
            side: 1,
            last_qty: 0.5,
            last_price: 30000.0,
            commission: 0.1,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            timestamp: 123,
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
        let legacy = r#"{"venue":"binance","symbol":"BTCUSDT","position_side":"BOTH","qty":1.0,"average_price":100.0,"timestamp":1}"#;
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
    /// `ReconcileClient`s hand-rolled before the dedup: one-way `BOTH`, zero qty/px/ts, default (Cross)
    /// margin, no isolated margin — and it serializes to the same legacy-compatible JSON (a cross
    /// flat row emits neither margin key), so swapping the call sites changes no wire bytes.
    #[test]
    fn flat_builds_the_canonical_flat_row() {
        let f = PositionStatusReport::flat("ig", "CS.D.EURUSD.MINI.IP");
        assert_eq!(f.venue, "ig");
        assert_eq!(f.symbol, "CS.D.EURUSD.MINI.IP");
        assert_eq!(f.position_side, PositionSide::Both);
        assert_eq!(f.qty, 0.0);
        assert_eq!(f.average_price, 0.0);
        assert_eq!(f.timestamp, 0);
        assert_eq!(f.margin_mode, MarginMode::default());
        assert_eq!(f.isolated_margin, None);
        // byte-identical to the hand-rolled literal every venue used
        let hand_rolled = PositionStatusReport {
            venue: "ig".to_string(),
            symbol: "CS.D.EURUSD.MINI.IP".to_string(),
            position_side: PositionSide::Both,
            qty: 0.0,
            average_price: 0.0,
            timestamp: 0,
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

    fn leg(size: f64, price: f64, timestamp_ms: i64) -> PositionLeg {
        PositionLeg { size, price, timestamp_ms }
    }

    fn netted(legs: &[PositionLeg]) -> Vec<(PositionSide, f64, f64, i64)> {
        PositionStatusReport::from_legs("ig", "EURUSD", legs, ContractValue::default())
            .into_iter()
            .map(|r| (r.position_side, r.qty, r.average_price, r.timestamp))
            .collect()
    }

    /// One direction is ONE `Both` row: the signed sum, the size-weighted average of the legs' own
    /// prices, the newest timestamp, and otherwise the flat row's fields. A short book weights the
    /// same as a long one (the venue copies' signed formula `Σ s·p / Σ s` gives these exact bits).
    #[test]
    fn one_direction_is_one_both_row_at_the_weighted_entry() {
        let rows = PositionStatusReport::from_legs(
            "ig",
            "EURUSD",
            &[leg(2.0, 100.0, 5), leg(1.0, 130.0, 9)],
            ContractValue::default(),
        );
        assert_eq!(
            rows,
            vec![PositionStatusReport {
                qty: 3.0,
                average_price: 110.0,
                timestamp: 9,
                ..PositionStatusReport::flat("ig", "EURUSD")
            }]
        );
        let signed = (-3.0 * 1.1 + -7.0 * 1.13) / (-3.0 + -7.0);
        assert_eq!(
            netted(&[leg(-3.0, 1.1, 0), leg(-7.0, 1.13, 0)]),
            vec![(PositionSide::Both, -10.0, signed, 0)]
        );
    }

    /// The locked-pair rule: long 10 @ 1.1000 and short 4 @ 1.1100 are TWO rows, each at its own entry,
    /// summing to the net 6; a fully hedged pair is two rows too, never a flat one.
    #[test]
    fn both_directions_are_a_long_and_a_short_row() {
        let pair = [leg(10.0, 1.1, 1), leg(-4.0, 1.11, 2)];
        let rows = netted(&pair);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].0, rows[0].1, rows[0].3), (PositionSide::Long, 10.0, 1));
        assert_eq!((rows[1].0, rows[1].1, rows[1].3), (PositionSide::Short, -4.0, 2));
        assert!((rows[0].2 - 1.1).abs() < 1e-12 && (rows[1].2 - 1.11).abs() < 1e-12, "{rows:?}");
        assert_eq!(rows[0].1 + rows[1].1, 6.0, "the rows still sum to the net");
        let hedged = netted(&[leg(1.0, 100.0, 0), leg(-1.0, 110.0, 0)]);
        assert_eq!(
            hedged,
            vec![(PositionSide::Long, 1.0, 100.0, 0), (PositionSide::Short, -1.0, 110.0, 0)]
        );
    }

    /// A zero-size leg is dropped (so a long beside an empty short stays one `Both` row), and no leg
    /// left, or none at all, is exactly the flat row.
    #[test]
    fn zero_legs_drop_and_nothing_left_is_the_flat_row() {
        assert_eq!(
            netted(&[leg(2.0, 100.0, 0), leg(0.0, 90.0, 7)]),
            vec![(PositionSide::Both, 2.0, 100.0, 0)]
        );
        let flat = vec![PositionStatusReport::flat("ig", "EURUSD")];
        let none = |legs: &[PositionLeg]| {
            PositionStatusReport::from_legs("ig", "EURUSD", legs, ContractValue::default())
        };
        assert_eq!(none(&[]), flat);
        assert_eq!(none(&[leg(0.0, 100.0, 3)]), flat);
    }

    /// A row with any leg the venue gave no usable price is `0.0`, "no usable price"; the other
    /// direction's row keeps its own.
    #[test]
    fn a_row_with_an_unpriced_leg_has_no_price() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let rows = netted(&[leg(1.0, 100.0, 0), leg(1.0, bad, 0), leg(-1.0, 110.0, 0)]);
            assert_eq!(
                rows,
                vec![(PositionSide::Long, 2.0, 0.0, 0), (PositionSide::Short, -1.0, 110.0, 0)],
                "{bad}"
            );
        }
    }

    /// A wallet's holding is a `Cash`, one-way row at no price; at or past ONE step it is the balance,
    /// under it (dust), an unlisted coin or not a number it is flat. Byte-identical to the row bybit's
    /// `spot_holding` writes by hand.
    #[test]
    fn a_spot_holding_is_a_cash_row_and_dust_is_flat() {
        let row = |held| PositionStatusReport::spot_holding("bybit", "DOGEUSDT.S", held, 1.0, 7);
        assert_eq!(
            row(12.5),
            PositionStatusReport {
                venue: "bybit".into(),
                symbol: "DOGEUSDT.S".into(),
                position_side: PositionSide::Both,
                qty: 12.5,
                average_price: 0.0,
                timestamp: 7,
                margin_mode: MarginMode::Cash,
                isolated_margin: None,
                delta: None,
            }
        );
        assert_eq!(row(1.0).qty, 1.0, "one step is a holding");
        for dust in [0.999, 0.0, -3.0, f64::NAN] {
            assert_eq!(row(dust).qty, 0.0, "{dust}");
            assert_eq!((row(dust).margin_mode, row(dust).timestamp), (MarginMode::Cash, 7));
        }
    }

    /// The averaging law is the contract's: two 1,000-USD legs at 50,000 and 60,000 of an inverse
    /// contract average harmonically, 54,545.45, not 55,000.
    #[test]
    fn an_inverse_contract_averages_harmonically() {
        let rows = PositionStatusReport::from_legs(
            "deribit",
            "BTC-PERPETUAL",
            &[leg(1000.0, 50_000.0, 0), leg(1000.0, 60_000.0, 0)],
            ContractValue::inverse(1.0),
        );
        assert_eq!(rows.len(), 1);
        assert!((rows[0].average_price - 600_000.0 / 11.0).abs() < 1e-9, "{rows:?}");
    }
}
