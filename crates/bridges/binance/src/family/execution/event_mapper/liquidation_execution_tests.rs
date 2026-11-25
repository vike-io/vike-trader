//! A venue LIQUIDATION on the perp user-data stream ([`map_perp`], shared by binance and aster): the
//! venue documents `x` = `CALCULATED` as "Liquidation Execution" and marks the venue's own order by its
//! client id (`autoclose-…` liquidation, `adl_autoclose` ADL). Such an execution is ONE
//! `PositionLiquidated` (deduped by the engine's `seen_liq_ids` on `t`) and never a fill, and a frame
//! that executed nothing books nothing. Frames use the documented field letters of `ORDER_TRADE_UPDATE`.
use super::*;
use serde_json::json;
use vike_model::events::PositionSide;

/// An `ORDER_TRADE_UPDATE` of the venue's liquidation order: execution `x`, client id `c`, last
/// filled `l` (absent when `None`), trade id 12345.
fn liquidation_frame(x: &str, c: &str, l: Option<&str>) -> Value {
    let mut frame = json!({
        "e": "ORDER_TRADE_UPDATE", "E": 1_596_107_620_090_i64, "T": 1_596_107_620_087_i64,
        "o": {
            "s": "BTCUSDT", "c": c, "S": "SELL", "o": "LIMIT", "f": "IOC", "q": "0.001",
            "p": "10871.09", "ap": "10913.21", "x": x, "X": "FILLED", "i": 6_071_832_819_i64,
            "z": "0.001", "L": "10913.21", "N": "USDT", "n": "0.1", "t": 12345, "m": false,
            "R": false, "ps": "BOTH", "rp": "-1.5"
        }
    });
    if let Some(l) = l {
        frame["o"]["l"] = json!(l);
    }
    frame
}

/// `CALCULATED` is a liquidation execution whatever the order's id, and a TRADE of an `autoclose-` /
/// `adl_autoclose` order is one too: each is exactly one `PositionLiquidated` of what it executed, on
/// the series label, and no fill or fill wrap, on both venues' stream entry points.
#[test]
fn a_liquidation_execution_is_one_position_liquidated_and_never_a_fill() {
    let cases = [
        ("CALCULATED", "autoclose-1596107620040000020"),
        ("CALCULATED", "adl_autoclose"),
        ("CALCULATED", "vtr6perp1"),
        ("TRADE", "autoclose-1596107620040000020"),
        ("TRADE", "adl_autoclose"),
    ];
    for venue in ["binance", "aster"] {
        for (x, c) in cases {
            let frame = liquidation_frame(x, c, Some("0.001"));
            for events in [
                map_perp(&frame, venue, "BTCUSDT.P", None),
                map_perp_stream(&frame, venue, "BTCUSDT.P", None),
            ] {
                let [Event::PositionLiquidated(liquidation)] = events.as_slice() else {
                    panic!("{venue} {x} {c}: one PositionLiquidated and nothing else: {events:?}")
                };
                assert_eq!(liquidation.venue.as_str(), venue);
                assert_eq!(liquidation.symbol.as_str(), "BTCUSDT.P", "{x} {c}: the series label");
                assert_eq!(liquidation.position_side, PositionSide::Both);
                assert_eq!(
                    (liquidation.qty, liquidation.liquidation_price, liquidation.fee),
                    (0.001, 10913.21, 0.1),
                    "{x} {c}"
                );
                assert_eq!(
                    liquidation.trade_id.as_str(),
                    "12345",
                    "{x} {c}: `t`, the seen_liq_ids key"
                );
                assert_eq!(liquidation.timestamp, 1_596_107_620_087);
            }
        }
    }
}

/// The same execution reported as `CALCULATED` and as `TRADE` carries ONE liquidation id, so the
/// engine's `seen_liq_ids` folds it once; and a foreign symbol's id is qualified as a fill's is.
#[test]
fn the_same_execution_under_either_word_carries_one_liquidation_id() {
    let id = |x: &str, mounted: &str| match map_perp(
        &liquidation_frame(x, "autoclose-1", Some("0.001")),
        "binance",
        mounted,
        None,
    )
    .as_slice()
    {
        [Event::PositionLiquidated(liquidation)] => liquidation.trade_id.to_string(),
        other => panic!("{x}: {other:?}"),
    };
    assert_eq!(id("CALCULATED", "BTCUSDT.P"), id("TRADE", "BTCUSDT.P"));
    assert_eq!(id("CALCULATED", "ETHUSDT.P"), "BTCUSDT.P:12345", "foreign: `SYMBOL:t`");
}

/// A liquidation frame that executed NOTHING (no `l`, or `l` = 0: the venue's notice while its
/// order is still NEW) books nothing: `apply_liquidation` would read the zero as "close the WHOLE
/// position" at the frame's `L`.
#[test]
fn a_liquidation_frame_that_executed_nothing_books_nothing() {
    for x in ["CALCULATED", "TRADE"] {
        for l in [None, Some("0"), Some("0.000")] {
            let mut frame = liquidation_frame(x, "autoclose-1596107620040000020", l);
            frame["o"]["X"] = json!("NEW");
            assert_eq!(map_perp(&frame, "binance", "BTCUSDT.P", None), Vec::new(), "{x} l={l:?}");
        }
    }
}
