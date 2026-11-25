//! Tests for the López de Prado imbalance and runs bar builders: hand-counted closes, the forming
//! bar, a dollar weight priced by `ContractValue` on a linear and an inverse contract, and streaming
//! equal to batch bit for bit for every weight.
use super::{ImbalanceBarBuilder, RunsBarBuilder};
use crate::bars::OrderflowBar;
use crate::bars::bars_tests::bits;
use vike_marketdata::TickTrade;
use vike_marketdata::test_support::trade;
use vike_model::ContractValue;

/// The tick flavour: every trade weighs 1.
fn tick(_: &TickTrade) -> f64 {
    1.0
}

/// The volume flavour: a trade weighs its size.
fn volume(t: &TickTrade) -> f64 {
    t.size
}

/// The dollar flavour on a linear contract of face 1, priced the way every caller prices a trade.
fn linear_dollars(t: &TickTrade) -> f64 {
    ContractValue::linear(1.0).order_notional(t.size, t.price)
}

/// A signed stream that closes several bars for both builders with every weight.
fn mixed() -> Vec<TickTrade> {
    vec![
        trade(1, 100.0, 1.0, false), // buy 1@100
        trade(2, 101.0, 2.0, false), // buy 2@101
        trade(3, 99.0, 2.0, true),   // sell 2@99
        trade(4, 98.0, 2.0, true),   // sell 2@98
        trade(5, 100.0, 3.0, false), // buy 3@100
        trade(6, 100.0, 1.0, true),  // sell 1@100
    ]
}

fn bar(
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    buy: f64,
    sell: f64,
    count: u32,
    span: (i64, i64),
) -> OrderflowBar {
    OrderflowBar {
        o,
        h,
        l,
        c,
        buy_vol: buy,
        sell_vol: sell,
        delta: buy - sell,
        trade_count: count,
        first_timestamp: span.0,
        last_timestamp: span.1,
    }
}

// Volume imbalance theta = sum(+-size). Threshold 3: +1, +3 (close), -2, -4 (close): two bars, each
// closing on the crossing trade, which is included whole (not split).
#[test]
fn imbalance_bar_volume_closes_on_abs_threshold() {
    let trades = &mixed()[..4];
    let bars = ImbalanceBarBuilder::from_trades(trades, 3.0, volume);
    assert_eq!(
        bars,
        [
            bar(100.0, 101.0, 100.0, 101.0, 3.0, 0.0, 2, (1, 2)),
            bar(99.0, 99.0, 98.0, 98.0, 0.0, 4.0, 2, (3, 4)),
        ]
    );
}

// Tick imbalance theta = sum(+-1). Threshold 3, every size 1: the sell at t3 pulls theta back, so
// the bar closes only at t5 (net +3): the tick imbalance is the SIGNED count.
#[test]
fn imbalance_bar_tick_counts_signed_ticks() {
    let trades = [
        trade(1, 100.0, 1.0, false), // +1
        trade(2, 100.0, 1.0, false), // +2
        trade(3, 100.0, 1.0, true),  // +1 (sell)
        trade(4, 100.0, 1.0, false), // +2
        trade(5, 100.0, 1.0, false), // +3: close
    ];
    let bars = ImbalanceBarBuilder::from_trades(&trades, 3.0, tick);
    assert_eq!(bars, [bar(100.0, 100.0, 100.0, 100.0, 4.0, 1.0, 5, (1, 5))]);
}

// Volume runs theta = max(buy run, sell run). Threshold 3: the sell run reaches 4 at t3 (close), then
// the lone buy 3@100 reaches 3 at t4 (close).
#[test]
fn runs_bar_volume_closes_on_max_side() {
    let trades = [
        trade(1, 100.0, 1.0, false), // buy run 1
        trade(2, 101.0, 2.0, true),  // sell run 2
        trade(3, 99.0, 2.0, true),   // sell run 4: close
        trade(4, 100.0, 3.0, false), // buy run 3: close
    ];
    let bars = RunsBarBuilder::from_trades(&trades, 3.0, volume);
    assert_eq!(
        bars,
        [
            bar(100.0, 101.0, 99.0, 99.0, 1.0, 4.0, 3, (1, 3)),
            bar(100.0, 100.0, 100.0, 100.0, 3.0, 0.0, 1, (4, 4)),
        ]
    );
}

// Runs and imbalance are DIFFERENT statistics: an alternating stream keeps |imbalance| <= 1 while
// each side's run grows. Threshold 3: runs closes at t5 (buy run 3); imbalance never closes.
#[test]
fn runs_bar_distinct_from_imbalance() {
    let trades: Vec<TickTrade> = (1..=5).map(|i| trade(i, 100.0, 1.0, i % 2 == 0)).collect();
    let runs = RunsBarBuilder::from_trades(&trades, 3.0, volume);
    assert_eq!(runs, [bar(100.0, 100.0, 100.0, 100.0, 3.0, 2.0, 5, (1, 5))]);
    assert!(ImbalanceBarBuilder::from_trades(&trades, 3.0, volume).is_empty());
}

// Dollar imbalance on a linear contract: buy 1@100 (+100), buy 2@101 (+202: theta 302 >= 300, close).
#[test]
fn imbalance_bar_dollar_weighs_the_priced_notional() {
    let bars = ImbalanceBarBuilder::from_trades(&mixed(), 300.0, linear_dollars);
    assert_eq!(bars[0], bar(100.0, 101.0, 100.0, 101.0, 3.0, 0.0, 2, (1, 2)));
}

// Dollar imbalance on an INVERSE contract (size in US dollars, `ContractValue::inverse(1.0)`): the
// notional IS the size, so buy 300 then buy 300 reaches 600 and closes, whatever the price; priced as
// `price * size` the first trade alone would be 19,500,000 and close at once.
#[test]
fn imbalance_bar_dollar_on_an_inverse_contract_weighs_the_size() {
    let trades = [trade(1, 65_000.0, 300.0, false), trade(2, 66_000.0, 300.0, false)];
    let inverse = |t: &TickTrade| ContractValue::inverse(1.0).order_notional(t.size, t.price);
    let bars = ImbalanceBarBuilder::from_trades(&trades, 600.0, inverse);
    assert_eq!(bars, [bar(65_000.0, 66_000.0, 65_000.0, 66_000.0, 600.0, 0.0, 2, (1, 2))]);
    assert_eq!(ImbalanceBarBuilder::from_trades(&trades, 600.0, linear_dollars).len(), 2);
}

#[test]
fn imbalance_runs_forming_and_zero_size_skipped() {
    let mut imbalance = ImbalanceBarBuilder::new(3.0);
    assert_eq!(imbalance.forming(), None);
    let t = trade(0, 100.0, 0.0, false);
    assert_eq!(imbalance.push(&t, volume(&t)), None); // zero size: skipped
    assert_eq!(imbalance.forming(), None); // never opened a bar
    let t = trade(1, 100.0, 1.0, false);
    assert_eq!(imbalance.push(&t, volume(&t)), None); // theta 1, forming
    let forming = imbalance.forming().unwrap();
    assert_eq!((forming.buy_vol, forming.sell_vol, forming.trade_count), (1.0, 0.0, 1));
    let t = trade(2, 101.0, 2.0, false);
    assert!(imbalance.push(&t, volume(&t)).is_some()); // theta 3: closes
    assert_eq!(imbalance.forming(), None); // reset after the close

    let mut runs = RunsBarBuilder::new(3.0);
    let t = trade(0, 100.0, 0.0, true);
    assert_eq!(runs.push(&t, volume(&t)), None); // zero size: skipped
    assert_eq!(runs.forming(), None);
    let t = trade(1, 100.0, 1.0, true);
    assert_eq!(runs.push(&t, volume(&t)), None); // sell run 1, forming
    let t = trade(2, 100.0, 2.0, true);
    assert!(runs.push(&t, volume(&t)).is_some()); // sell run 3: closes
    assert_eq!(runs.forming(), None);
}

// Streaming `push` equals batch `from_trades`, bit for bit, for both builders with every weight.
#[test]
fn imbalance_runs_streaming_equals_batch() {
    type Weight = fn(&TickTrade) -> f64;
    let trades = mixed();
    let weights: [(Weight, f64); 3] = [(tick, 2.0), (volume, 3.0), (linear_dollars, 100.0)];
    for (weight, threshold) in weights {
        let batch = ImbalanceBarBuilder::from_trades(&trades, threshold, weight);
        let mut builder = ImbalanceBarBuilder::new(threshold);
        let stream: Vec<OrderflowBar> =
            trades.iter().filter_map(|t| builder.push(t, weight(t))).collect();
        assert!(!batch.is_empty());
        assert_eq!(
            stream.iter().map(bits).collect::<Vec<_>>(),
            batch.iter().map(bits).collect::<Vec<_>>()
        );

        let batch = RunsBarBuilder::from_trades(&trades, threshold, weight);
        let mut builder = RunsBarBuilder::new(threshold);
        let stream: Vec<OrderflowBar> =
            trades.iter().filter_map(|t| builder.push(t, weight(t))).collect();
        assert!(!batch.is_empty());
        assert_eq!(
            stream.iter().map(bits).collect::<Vec<_>>(),
            batch.iter().map(bits).collect::<Vec<_>>()
        );
    }
}
