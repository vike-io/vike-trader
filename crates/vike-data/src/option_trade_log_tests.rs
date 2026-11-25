use super::*;

/// 2024-06-03 00:00:00 UTC.
const DAY: i64 = 1_717_372_800_000;

fn trade(
    ts: i64,
    instrument: &str,
    mark: Option<f64>,
    iv: Option<f64>,
    amount: f64,
) -> OptionTradeRow {
    OptionTradeRow {
        ts,
        underlying: "BTC".into(),
        instrument: instrument.into(),
        expiry_ms: 1_718_956_800_000,
        strike: 94_000.0,
        kind: OptionKind::Call,
        trade_id: format!("{ts}"),
        price: 0.0007,
        amount,
        is_buyer_maker: true,
        iv,
        mark_price: mark,
        index_price: 67_000.0,
        premium_in_coin: true,
    }
}

/// Three trades of one instrument in hour 00 and one in hour 01: hour 00's row is stamped 00:59:59.999 and
/// carries the LAST mark (in USD: coin mark x index) and iv, and the hour's summed amount.
#[test]
fn one_row_per_hour_and_instrument_with_the_hours_last_values() {
    let a = "BTC-21JUN24-94000-C";
    let rows = hourly_chain_rows(&[
        trade(DAY + 1_000, a, Some(0.001), Some(0.60), 1.0),
        trade(DAY + 2_000, a, Some(0.002), Some(0.62), 2.5),
        trade(DAY + 3_599_999, a, Some(0.003), Some(0.64), 0.5),
        trade(DAY + 3_600_000, a, Some(0.004), Some(0.66), 4.0),
    ]);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].ts, DAY + 3_599_999);
    assert_eq!(rows[0].mark, Some(0.003 * 67_000.0));
    assert_eq!(rows[0].iv, Some(0.64));
    assert_eq!(rows[0].volume, Some(4.0));
    assert_eq!(rows[1].ts, DAY + 2 * 3_600_000 - 1);
    assert_eq!(rows[1].mark, Some(0.004 * 67_000.0));
    assert_eq!(rows[1].volume, Some(4.0));
    // what a trade cannot know stays absent, never zero
    for row in &rows {
        assert_eq!((row.bid, row.ask, row.open_interest), (None, None, None));
        assert_eq!((row.delta, row.gamma, row.theta, row.vega), (None, None, None, None));
        assert_eq!((row.instrument.as_str(), row.underlying.as_str()), (a, "BTC"));
        assert_eq!(
            (row.strike, row.kind, row.expiry_ms),
            (94_000.0, OptionKind::Call, 1_718_956_800_000)
        );
    }
}

/// Input order is not assumed, two instruments sort by name inside an hour, and a value the latest trade
/// left out (Deribit's 2017 `mark_price: null`) keeps the hour's earlier one.
#[test]
fn unordered_input_sorts_by_hour_then_instrument_and_an_absent_value_keeps_the_earlier_one() {
    let (a, b) = ("BTC-21JUN24-94000-C", "BTC-21JUN24-90000-C");
    let rows = hourly_chain_rows(&[
        trade(DAY + 5_000, a, None, None, 1.0),
        trade(DAY + 4_000, b, Some(0.01), Some(0.5), 1.0),
        trade(DAY + 1_000, a, Some(0.002), Some(0.7), 1.0),
    ]);
    let names: Vec<&str> = rows.iter().map(|r| r.instrument.as_str()).collect();
    assert_eq!(names, [b, a]);
    assert_eq!(rows[1].mark, Some(0.002 * 67_000.0));
    assert_eq!(rows[1].iv, Some(0.7));
    assert_eq!(rows[1].volume, Some(2.0));
}

/// A linear book's premium is already in the settlement currency: its mark is not scaled by the index.
/// An hour with no mark at all leaves the row's mark absent.
#[test]
fn a_linear_premium_is_not_scaled_and_a_markless_hour_stays_markless() {
    let mut linear = trade(DAY, "SOL_USDC-27JUN25-160-C", Some(11.72), Some(0.7767), 100.0);
    linear.premium_in_coin = false;
    linear.underlying = "SOL".into();
    let markless = trade(DAY, "BTC-31MAR17-940-P", None, Some(0.2973), 1.0);
    let rows = hourly_chain_rows(&[linear, markless]);
    assert_eq!(rows[0].instrument, "BTC-31MAR17-940-P");
    assert_eq!(rows[0].mark, None);
    assert_eq!(rows[1].underlying, "SOL");
    assert_eq!(rows[1].mark, Some(11.72));
}

/// A trade before the epoch still lands in the hour that holds it (`div_euclid`), and no trades give no rows.
#[test]
fn edges() {
    assert!(hourly_chain_rows(&[]).is_empty());
    let rows = hourly_chain_rows(&[trade(-1, "X", Some(1.0), None, 1.0)]);
    assert_eq!(rows[0].ts, -1);
}
