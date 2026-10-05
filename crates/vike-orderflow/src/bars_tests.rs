use super::*;
use vike_marketdata::test_support::trade;

// 4 trades, n=2 → two bars. Bar1: buy 1@100, sell 2@101 → o100 h101 l100 c101, buy1 sell2 delta-1.
fn seq() -> Vec<TradeTick> {
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
            first_ts: 1,
            last_ts: 2
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
            first_ts: 3,
            last_ts: 4
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

fn bits(b: &OrderflowBar) -> (u64, u64, u64, u64, u64, u64, u64, u32, i64, i64) {
    (
        b.o.to_bits(),
        b.h.to_bits(),
        b.l.to_bits(),
        b.c.to_bits(),
        b.buy_vol.to_bits(),
        b.sell_vol.to_bits(),
        b.delta.to_bits(),
        b.trade_count,
        b.first_ts,
        b.last_ts,
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
                first_ts: 1,
                last_ts: 1
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
            first_ts: 1,
            last_ts: 2
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

// threshold $200: buy 1@100 ($100) then sell 2@100 ($200) → bar1 takes 1 of the sell
// ($100, exactly filling the room) and closes; the remaining 1 sell opens bar2.
#[test]
fn dollar_bar_splits_at_notional_boundary() {
    let trades = vec![trade(1, 100.0, 1.0, false), trade(2, 100.0, 2.0, true)];
    let bars = DollarBarBuilder::from_trades(&trades, 200.0);
    assert_eq!(bars.len(), 1);
    assert_eq!(
        bars[0],
        OrderflowBar {
            o: 100.0,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            buy_vol: 1.0,
            sell_vol: 1.0,
            delta: 0.0,
            trade_count: 2,
            first_ts: 1,
            last_ts: 2
        }
    );
}

// threshold $100: one buy 5@50 = $250 → two full bars (2 units = $100 each) +
// remainder 1 unit ($50) forming. The dollar twin of volume_bar_splits_one_big_trade.
#[test]
fn dollar_bar_splits_one_big_trade() {
    let trades = vec![trade(1, 50.0, 5.0, false)];
    let bars = DollarBarBuilder::from_trades(&trades, 100.0);
    assert_eq!(bars.len(), 2);
    for b in &bars {
        assert_eq!(
            b,
            &OrderflowBar {
                o: 50.0,
                h: 50.0,
                l: 50.0,
                c: 50.0,
                buy_vol: 2.0,
                sell_vol: 0.0,
                delta: 2.0,
                trade_count: 1,
                first_ts: 1,
                last_ts: 1
            }
        );
    }
    let mut d = DollarBarBuilder::new(100.0);
    assert_eq!(d.push(&trades[0]).len(), 2);
    let f = d.forming().unwrap();
    assert_eq!((f.buy_vol, f.sell_vol, f.trade_count), (1.0, 0.0, 1));
}

#[test]
fn dollar_bar_partial_not_emitted_and_zero_size_skipped() {
    // $50 of a $100 threshold → no bar; zero-size trades are skipped entirely.
    let trades = vec![trade(0, 100.0, 0.0, false), trade(1, 50.0, 1.0, false)];
    assert!(DollarBarBuilder::from_trades(&trades, 100.0).is_empty());
    let mut d = DollarBarBuilder::new(100.0);
    assert!(d.push(&trades[0]).is_empty());
    assert_eq!(d.forming(), None); // zero-size never opened a bar
    assert!(d.push(&trades[1]).is_empty());
    assert_eq!(d.forming().unwrap().buy_vol, 1.0);
}

#[test]
fn dollar_bar_streaming_equals_batch() {
    let trades =
        vec![trade(1, 100.0, 5.0, false), trade(2, 101.0, 2.0, true), trade(3, 99.0, 4.0, false)];
    let batch = DollarBarBuilder::from_trades(&trades, 250.0);
    let mut b = DollarBarBuilder::new(250.0);
    let mut stream = Vec::new();
    for t in &trades {
        stream.extend(b.push(t));
    }
    assert_eq!(stream.len(), batch.len());
    assert!(!batch.is_empty());
    for (s, x) in stream.iter().zip(&batch) {
        assert_eq!(bits(s), bits(x));
    }
}

// A price<=0 trade can never close a dollar bar (no notional) — it must be absorbed
// without looping, and a later real trade still closes on cumulative notional.
#[test]
fn dollar_bar_zero_price_absorbed() {
    let mut d = DollarBarBuilder::new(100.0);
    assert!(d.push(&trade(1, 0.0, 3.0, true)).is_empty());
    assert_eq!(d.forming().unwrap().sell_vol, 3.0);
    let closed = d.push(&trade(2, 100.0, 1.0, false)); // $100 → closes
    assert_eq!(closed.len(), 1);
    assert_eq!((closed[0].buy_vol, closed[0].sell_vol), (1.0, 3.0));
}

// ---- López de Prado information-driven bars: imbalance + runs ----

// A richer signed stream that closes several bars for both builders across all units.
fn mixed() -> Vec<TradeTick> {
    vec![
        trade(1, 100.0, 1.0, false), // buy 1@100
        trade(2, 101.0, 2.0, false), // buy 2@101
        trade(3, 99.0, 2.0, true),   // sell 2@99
        trade(4, 98.0, 2.0, true),   // sell 2@98
        trade(5, 100.0, 3.0, false), // buy 3@100
        trade(6, 100.0, 1.0, true),  // sell 1@100
    ]
}

// Volume imbalance θ = Σ(±size). threshold 3: +1,+3(close),−2,−4(close) → two bars, each
// closing exactly on the boundary-crossing trade (which is fully included, not split).
#[test]
fn imbalance_bar_volume_closes_on_abs_threshold() {
    let trades = vec![
        trade(1, 100.0, 1.0, false),
        trade(2, 101.0, 2.0, false),
        trade(3, 99.0, 2.0, true),
        trade(4, 98.0, 2.0, true),
    ];
    let bars = ImbalanceBarBuilder::from_trades(&trades, FlowUnit::Volume, 3.0);
    assert_eq!(bars.len(), 2);
    assert_eq!(
        bars[0],
        OrderflowBar {
            o: 100.0,
            h: 101.0,
            l: 100.0,
            c: 101.0,
            buy_vol: 3.0,
            sell_vol: 0.0,
            delta: 3.0,
            trade_count: 2,
            first_ts: 1,
            last_ts: 2
        }
    );
    assert_eq!(
        bars[1],
        OrderflowBar {
            o: 99.0,
            h: 99.0,
            l: 98.0,
            c: 98.0,
            buy_vol: 0.0,
            sell_vol: 4.0,
            delta: -4.0,
            trade_count: 2,
            first_ts: 3,
            last_ts: 4
        }
    );
}

// Tick imbalance θ = Σ(±1). threshold 3, all size 1: the sell at t3 pushes θ back down so
// the bar only closes at t5 (net +3) — proves tick imbalance is the SIGNED count.
#[test]
fn imbalance_bar_tick_counts_signed_ticks() {
    let trades = vec![
        trade(1, 100.0, 1.0, false), // +1
        trade(2, 100.0, 1.0, false), // +2
        trade(3, 100.0, 1.0, true),  // +1 (sell)
        trade(4, 100.0, 1.0, false), // +2
        trade(5, 100.0, 1.0, false), // +3 → close
    ];
    let bars = ImbalanceBarBuilder::from_trades(&trades, FlowUnit::Tick, 3.0);
    assert_eq!(bars.len(), 1);
    assert_eq!(
        bars[0],
        OrderflowBar {
            o: 100.0,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            buy_vol: 4.0,
            sell_vol: 1.0,
            delta: 3.0,
            trade_count: 5,
            first_ts: 1,
            last_ts: 5
        }
    );
}

// Volume runs θ = max(buy-run, sell-run). threshold 3: the sell side reaches 4 at t3
// (close), then the lone buy 3@100 reaches 3 at t4 (close) → two bars.
#[test]
fn runs_bar_volume_closes_on_max_side() {
    let trades = vec![
        trade(1, 100.0, 1.0, false), // buy_run 1
        trade(2, 101.0, 2.0, true),  // sell_run 2
        trade(3, 99.0, 2.0, true),   // sell_run 4 → close
        trade(4, 100.0, 3.0, false), // buy_run 3 → close
    ];
    let bars = RunsBarBuilder::from_trades(&trades, FlowUnit::Volume, 3.0);
    assert_eq!(bars.len(), 2);
    assert_eq!(
        bars[0],
        OrderflowBar {
            o: 100.0,
            h: 101.0,
            l: 99.0,
            c: 99.0,
            buy_vol: 1.0,
            sell_vol: 4.0,
            delta: -3.0,
            trade_count: 3,
            first_ts: 1,
            last_ts: 3
        }
    );
    assert_eq!(
        bars[1],
        OrderflowBar {
            o: 100.0,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            buy_vol: 3.0,
            sell_vol: 0.0,
            delta: 3.0,
            trade_count: 1,
            first_ts: 4,
            last_ts: 4
        }
    );
}

// Runs and imbalance are DIFFERENT statistics: an alternating stream keeps |imbalance| ≤ 1
// yet grows the per-side runs. threshold 3 → runs closes one bar at t5 (buy-run hits 3);
// imbalance never closes over the same stream.
#[test]
fn runs_bar_distinct_from_imbalance() {
    let trades = vec![
        trade(1, 100.0, 1.0, false), // buy
        trade(2, 100.0, 1.0, true),  // sell
        trade(3, 100.0, 1.0, false), // buy
        trade(4, 100.0, 1.0, true),  // sell
        trade(5, 100.0, 1.0, false), // buy → buy_run 3
    ];
    let runs = RunsBarBuilder::from_trades(&trades, FlowUnit::Volume, 3.0);
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0],
        OrderflowBar {
            o: 100.0,
            h: 100.0,
            l: 100.0,
            c: 100.0,
            buy_vol: 3.0,
            sell_vol: 2.0,
            delta: 1.0,
            trade_count: 5,
            first_ts: 1,
            last_ts: 5
        }
    );
    // same stream, imbalance threshold 3 → |θ| peaks at 1, no bar closes.
    assert!(ImbalanceBarBuilder::from_trades(&trades, FlowUnit::Volume, 3.0).is_empty());
}

#[test]
fn imbalance_runs_forming_and_zero_size_skipped() {
    let mut ib = ImbalanceBarBuilder::new(FlowUnit::Volume, 3.0);
    assert_eq!(ib.forming(), None);
    assert_eq!(ib.push(&trade(0, 100.0, 0.0, false)), None); // zero-size skipped
    assert_eq!(ib.forming(), None); // never opened a bar
    assert_eq!(ib.push(&trade(1, 100.0, 1.0, false)), None); // θ=1, forming
    let f = ib.forming().unwrap();
    assert_eq!((f.buy_vol, f.sell_vol, f.trade_count), (1.0, 0.0, 1));
    assert!(ib.push(&trade(2, 101.0, 2.0, false)).is_some()); // θ=3 → closes
    assert_eq!(ib.forming(), None); // reset after close

    let mut rb = RunsBarBuilder::new(FlowUnit::Volume, 3.0);
    assert_eq!(rb.push(&trade(0, 100.0, 0.0, true)), None); // zero-size skipped
    assert_eq!(rb.forming(), None);
    assert_eq!(rb.push(&trade(1, 100.0, 1.0, true)), None); // sell_run=1, forming
    assert!(rb.push(&trade(2, 100.0, 2.0, true)).is_some()); // sell_run=3 → closes
    assert_eq!(rb.forming(), None);
}

// Streaming push == batch from_trades, bit-for-bit, for both builders across all units.
#[test]
fn imbalance_runs_streaming_equals_batch() {
    let trades = mixed();
    for (kind, th) in [(FlowUnit::Tick, 2.0), (FlowUnit::Volume, 3.0), (FlowUnit::Dollar, 100.0)] {
        let ibatch = ImbalanceBarBuilder::from_trades(&trades, kind, th);
        let mut ib = ImbalanceBarBuilder::new(kind, th);
        let istream: Vec<OrderflowBar> = trades.iter().filter_map(|t| ib.push(t)).collect();
        assert!(!ibatch.is_empty());
        assert_eq!(istream.len(), ibatch.len());
        for (s, x) in istream.iter().zip(&ibatch) {
            assert_eq!(bits(s), bits(x));
        }

        let rbatch = RunsBarBuilder::from_trades(&trades, kind, th);
        let mut rb = RunsBarBuilder::new(kind, th);
        let rstream: Vec<OrderflowBar> = trades.iter().filter_map(|t| rb.push(t)).collect();
        assert!(!rbatch.is_empty());
        assert_eq!(rstream.len(), rbatch.len());
        for (s, x) in rstream.iter().zip(&rbatch) {
            assert_eq!(bits(s), bits(x));
        }
    }
}

// OFF / additive: the LdP builders are new, explicitly-constructed types with their own
// state — running them over a stream cannot perturb the tick/volume/dollar builders. Pin
// the existing tick bars bit-for-bit (identical to `tick_bar_expected_values`) to prove the
// additive change left the shipped bar paths byte-identical.
#[test]
fn additive_builders_leave_existing_bars_byte_identical() {
    let trades = seq();
    let _imb = ImbalanceBarBuilder::from_trades(&trades, FlowUnit::Volume, 2.0);
    let _run = RunsBarBuilder::from_trades(&trades, FlowUnit::Volume, 2.0);
    let tick = TickBarBuilder::from_trades(&trades, 2);
    assert_eq!(tick.len(), 2);
    let want = [
        OrderflowBar {
            o: 100.0,
            h: 101.0,
            l: 100.0,
            c: 101.0,
            buy_vol: 1.0,
            sell_vol: 2.0,
            delta: -1.0,
            trade_count: 2,
            first_ts: 1,
            last_ts: 2,
        },
        OrderflowBar {
            o: 99.0,
            h: 100.0,
            l: 99.0,
            c: 100.0,
            buy_vol: 3.0,
            sell_vol: 1.0,
            delta: 2.0,
            trade_count: 2,
            first_ts: 3,
            last_ts: 4,
        },
    ];
    for (g, w) in tick.iter().zip(&want) {
        assert_eq!(bits(g), bits(w));
    }
    // volume/dollar builders stay deterministic and unaffected too.
    assert_eq!(
        VolumeBarBuilder::from_trades(&trades, 3.0),
        VolumeBarBuilder::from_trades(&trades, 3.0)
    );
    assert_eq!(
        DollarBarBuilder::from_trades(&trades, 250.0),
        DollarBarBuilder::from_trades(&trades, 250.0)
    );
}
