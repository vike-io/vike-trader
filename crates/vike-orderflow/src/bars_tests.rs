use super::*;
use vike_marketdata::test_support::trade;

// 4 trades, n=2 → two bars. Bar1: buy 1@100, sell 2@101 → o100 h101 l100 c101, buy1 sell2 delta-1.
fn seq() -> Vec<TickTrade> {
    vec![
        trade(1, 100.0, 1.0, false),
        trade(2, 101.0, 2.0, true),
        trade(3, 99.0, 3.0, false),
        trade(4, 100.0, 1.0, true),
    ]
}

#[test]
fn tick_bar_expected_values() {
    let bars = TickBarBuilder::from_trades(&seq(), 2);
    assert_eq!(bars.len(), 2);
    assert_eq!(
        bars[0],
        OrderflowBar {
            o: 100.0,
            h: 101.0,
            l: 100.0,
            c: 101.0,
            buy_vol: 1.0,
            sell_vol: 2.0,
            delta: -1.0,
            trade_count: 2,
            first_timestamp: 1,
            last_timestamp: 2
        }
    );
    assert_eq!(
        bars[1],
        OrderflowBar {
            o: 99.0,
            h: 100.0,
            l: 99.0,
            c: 100.0,
            buy_vol: 3.0,
            sell_vol: 1.0,
            delta: 2.0,
            trade_count: 2,
            first_timestamp: 3,
            last_timestamp: 4
        }
    );
}

#[test]
fn tick_bar_partial_not_emitted_and_zero_size_skipped() {
    let mut trades = seq();
    trades.push(trade(5, 100.0, 5.0, false)); // 5th trade → partial, not emitted
    trades.insert(0, trade(0, 100.0, 0.0, false)); // zero-size → skipped entirely
    let bars = TickBarBuilder::from_trades(&trades, 2);
    assert_eq!(bars.len(), 2); // still exactly two full bars from the 4 non-zero trades before the 5th
}

#[test]
fn tick_bar_streaming_equals_batch() {
    let trades = seq();
    let batch = TickBarBuilder::from_trades(&trades, 2);
    let mut b = TickBarBuilder::new(2);
    let stream: Vec<OrderflowBar> = trades.iter().filter_map(|t| b.push(t)).collect();
    assert_eq!(stream.len(), batch.len());
    for (s, x) in stream.iter().zip(&batch) {
        assert_eq!(bits(s), bits(x)); // bit-identical
    }
}

pub(super) fn bits(b: &OrderflowBar) -> (u64, u64, u64, u64, u64, u64, u64, u32, i64, i64) {
    (
        b.o.to_bits(),
        b.h.to_bits(),
        b.l.to_bits(),
        b.c.to_bits(),
        b.buy_vol.to_bits(),
        b.sell_vol.to_bits(),
        b.delta.to_bits(),
        b.trade_count,
        b.first_timestamp,
        b.last_timestamp,
    )
}

#[test]
fn invariant_delta_and_nonneg() {
    // seq bar totals are 3 (buy1+sell2) and 4 (buy3+sell1); assert the real invariants.
    for b in TickBarBuilder::from_trades(&seq(), 2) {
        assert_eq!(b.delta, b.buy_vol - b.sell_vol);
        assert!(b.buy_vol >= 0.0 && b.sell_vol >= 0.0);
    }
}

#[test]
fn forming_exposes_open_accumulator() {
    let mut b = TickBarBuilder::new(2);
    assert_eq!(b.forming(), None);
    b.push(&trade(1, 100.0, 1.0, false));
    let f = b.forming().unwrap();
    assert_eq!((f.o, f.c, f.buy_vol, f.trade_count), (100.0, 100.0, 1.0, 1));
    b.push(&trade(2, 101.0, 2.0, true)); // closes the bar
    assert_eq!(b.forming(), None);

    let mut v = VolumeBarBuilder::new(2.0);
    let closed = v.push(&trade(1, 100.0, 5.0, false)); // 2 closes + remainder 1.0 open
    assert_eq!(closed.len(), 2);
    let vf = v.forming().unwrap();
    assert_eq!((vf.buy_vol, vf.sell_vol), (1.0, 0.0));
}

// threshold 2.0. One buy trade 5@100 → 5/2 = two full bars (2 each) + remainder 1 open.
#[test]
fn volume_bar_splits_one_big_trade() {
    let trades = vec![trade(1, 100.0, 5.0, false)];
    let bars = VolumeBarBuilder::from_trades(&trades, 2.0);
    assert_eq!(bars.len(), 2);
    for b in &bars {
        assert_eq!(
            b,
            &OrderflowBar {
                o: 100.0,
                h: 100.0,
                l: 100.0,
                c: 100.0,
                buy_vol: 2.0,
                sell_vol: 0.0,
                delta: 2.0,
                trade_count: 1,
                first_timestamp: 1,
                last_timestamp: 1
            }
        );
    }
}

// threshold 3.0: buy 2@100, sell 2@101 → bar1 = 2 buy + 1 sell(split) → o100 h101 l100 c101 buy2 sell1 delta1; remainder 1 sell opens bar2.
#[test]
fn volume_bar_splits_across_two_trades() {
    let trades = vec![trade(1, 100.0, 2.0, false), trade(2, 101.0, 2.0, true)];
    let bars = VolumeBarBuilder::from_trades(&trades, 3.0);
    assert_eq!(bars.len(), 1);
    assert_eq!(
        bars[0],
        OrderflowBar {
            o: 100.0,
            h: 101.0,
            l: 100.0,
            c: 101.0,
            buy_vol: 2.0,
            sell_vol: 1.0,
            delta: 1.0,
            trade_count: 2,
            first_timestamp: 1,
            last_timestamp: 2
        }
    );
}

#[test]
fn volume_bar_streaming_equals_batch() {
    let trades =
        vec![trade(1, 100.0, 5.0, false), trade(2, 101.0, 2.0, true), trade(3, 99.0, 4.0, false)];
    let batch = VolumeBarBuilder::from_trades(&trades, 2.0);
    let mut b = VolumeBarBuilder::new(2.0);
    let mut stream = Vec::new();
    for t in &trades {
        stream.extend(b.push(t));
    }
    assert_eq!(stream.len(), batch.len());
    for (s, x) in stream.iter().zip(&batch) {
        assert_eq!(bits(s), bits(x));
    }
}

fn flat(price: f64, buy: f64, sell: f64, count: u32, span: (i64, i64)) -> OrderflowBar {
    OrderflowBar {
        o: price,
        h: price,
        l: price,
        c: price,
        buy_vol: buy,
        sell_vol: sell,
        delta: buy - sell,
        trade_count: count,
        first_timestamp: span.0,
        last_timestamp: span.1,
    }
}

/// A trade's notional on a linear contract of face 1, priced the way every caller prices a trade:
/// this crate prices none (`bars.rs`'s module doc).
fn linear_dollars(t: &TickTrade) -> f64 {
    vike_model::ContractValue::linear(1.0).order_notional(t.size, t.price)
}

/// The same on an INVERSE contract whose size is US dollars (Deribit's `BTC-PERPETUAL`).
fn inverse_dollars(t: &TickTrade) -> f64 {
    vike_model::ContractValue::inverse(1.0).order_notional(t.size, t.price)
}

// Threshold $200: buy 1@100 ($100), then sell 2@100 ($200): bar 1 takes 1 of the sell ($100, exactly
// the room) and closes; the other sell opens bar 2.
#[test]
fn dollar_bar_splits_at_the_value_boundary() {
    let trades = vec![trade(1, 100.0, 1.0, false), trade(2, 100.0, 2.0, true)];
    let bars = DollarBarBuilder::from_trades(&trades, 200.0, linear_dollars);
    assert_eq!(bars, [flat(100.0, 1.0, 1.0, 2, (1, 2))]);
}

// Threshold $100: one buy 5@50 = $250: two full bars (2 units, $100 each) and 1 unit ($50) forming.
#[test]
fn dollar_bar_splits_one_big_trade() {
    let trades = vec![trade(1, 50.0, 5.0, false)];
    let want = flat(50.0, 2.0, 0.0, 1, (1, 1));
    assert_eq!(DollarBarBuilder::from_trades(&trades, 100.0, linear_dollars), [want, want]);
    let mut d = DollarBarBuilder::new(100.0);
    assert_eq!(d.push(&trades[0], linear_dollars(&trades[0])).len(), 2);
    let f = d.forming().unwrap();
    assert_eq!((f.buy_vol, f.sell_vol, f.trade_count), (1.0, 0.0, 1));
}

#[test]
fn dollar_bar_partial_not_emitted_and_zero_size_skipped() {
    // $50 of a $100 threshold: no bar; a zero-size trade is skipped entirely.
    let trades = vec![trade(0, 100.0, 0.0, false), trade(1, 50.0, 1.0, false)];
    assert!(DollarBarBuilder::from_trades(&trades, 100.0, linear_dollars).is_empty());
    let mut d = DollarBarBuilder::new(100.0);
    assert!(d.push(&trades[0], linear_dollars(&trades[0])).is_empty());
    assert_eq!(d.forming(), None);
    assert!(d.push(&trades[1], linear_dollars(&trades[1])).is_empty());
    assert_eq!(d.forming().unwrap().buy_vol, 1.0);
}

#[test]
fn dollar_bar_streaming_equals_batch() {
    let trades =
        vec![trade(1, 100.0, 5.0, false), trade(2, 101.0, 2.0, true), trade(3, 99.0, 4.0, false)];
    for notional in [linear_dollars as fn(&TickTrade) -> f64, inverse_dollars] {
        let batch = DollarBarBuilder::from_trades(&trades, 2.5, notional);
        let mut d = DollarBarBuilder::new(2.5);
        let stream: Vec<OrderflowBar> =
            trades.iter().flat_map(|t| d.push(t, notional(t))).collect();
        assert!(!batch.is_empty());
        assert_eq!(
            stream.iter().map(bits).collect::<Vec<_>>(),
            batch.iter().map(bits).collect::<Vec<_>>()
        );
    }
}

// A trade worth nothing adds no value, so it can never close a dollar bar: it is absorbed without
// looping, and a later real trade still closes on the cumulative value.
#[test]
fn dollar_bar_zero_price_absorbed() {
    let mut d = DollarBarBuilder::new(100.0);
    let free = trade(1, 0.0, 3.0, true);
    assert!(d.push(&free, linear_dollars(&free)).is_empty());
    assert_eq!(d.forming().unwrap().sell_vol, 3.0);
    let paid = trade(2, 100.0, 1.0, false);
    let closed = d.push(&paid, linear_dollars(&paid)); // $100: closes
    assert_eq!(closed.len(), 1);
    assert_eq!((closed[0].buy_vol, closed[0].sell_vol), (1.0, 3.0));
}

// An INVERSE contract's size is already dollars: a tape of 300, 500 and 200 USD contracts at about
// 65,000 makes two $500 bars ({buy 300, sell 200}, then {sell 300, buy 200}), whatever the price.
// `price * size` would read the first trade alone as $19,500,000 and close thousands of bars.
#[test]
fn dollar_bars_of_an_inverse_tape_count_its_dollars_not_price_times_size() {
    let trades = vec![
        trade(1, 65_000.0, 300.0, false), // $300
        trade(2, 66_000.0, 500.0, true),  // $500: $200 closes bar 1, $300 opens bar 2
        trade(3, 64_000.0, 200.0, false), // $200: closes bar 2
    ];
    let bars = DollarBarBuilder::from_trades(&trades, 500.0, inverse_dollars);
    let first = OrderflowBar {
        o: 65_000.0,
        h: 66_000.0,
        l: 65_000.0,
        c: 66_000.0,
        buy_vol: 300.0,
        sell_vol: 200.0,
        delta: 100.0,
        trade_count: 2,
        first_timestamp: 1,
        last_timestamp: 2,
    };
    let second = OrderflowBar {
        o: 66_000.0,
        h: 66_000.0,
        l: 64_000.0,
        c: 64_000.0,
        buy_vol: 200.0,
        sell_vol: 300.0,
        delta: -100.0,
        trade_count: 2,
        first_timestamp: 2,
        last_timestamp: 3,
    };
    assert_eq!(bars, [first, second]);
    let price_times_size = |t: &TickTrade| t.price * t.size;
    assert!(DollarBarBuilder::from_trades(&trades, 500.0, price_times_size).len() > 1_000);
}
