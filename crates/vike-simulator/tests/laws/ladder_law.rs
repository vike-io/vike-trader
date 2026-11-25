//! The bracket-ladder law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_ladder`] lowers a
//! `vike_model::LadderSpec` through `vike_model::build_ladder` to an entry, a reduce-only stop for its
//! whole size and N partial targets, the stop and each target linked OUO (`ouo_law.rs` pins the OUO
//! spend itself). Pinned:
//!
//! 1. the exits are HELD until the entry fills: nothing but the entry rests, and a bar that crosses
//!    the stop before the entry has filled fires nothing;
//! 2. the entry's fill arms them at their full size, from the next event (the bar it filled on does
//!    not fill a target it also crossed);
//! 3. each target's fill shrinks the stop to the position, and the last one cancels it;
//! 4. a bar that crosses the stop and a target fills the existing one-bar law, the stop first and the
//!    sum capped to the position, and the stop's fill cancels every target;
//! 5. a ladder the builder refuses is dropped in its words, and nothing rests;
//! 6. an entry that ends WITHOUT a fill (here a bar with no volume under the volume cap) cancels its
//!    held exits, each in words (the paper book's `parent-expired` cascade), and nothing rests.

use vike_model::{LadderSpec, LadderTarget, OrderKind};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

use crate::stop_limit_law::{FLAT, Once, SYM, bar, params, run_bars};

/// Long 3 (a market entry, or a limit at `entry`), the stop at 95, targets 1 at 110 and 2 at 120.
fn ladder(entry: Option<f64>) -> LadderSpec {
    LadderSpec {
        venue: String::new(),
        symbol: SYM.into(),
        side: 1,
        qty: 3.0,
        entry_price: entry,
        stop_loss: 95.0,
        targets: vec![
            LadderTarget { price: 110.0, qty: 1.0 },
            LadderTarget { price: 120.0, qty: 2.0 },
        ],
        account: None,
    }
}

fn place(entry: Option<f64>) -> Once {
    Once::new(move |b: &mut OrderBrokerSimulator| {
        let handles = b.submit_ladder(&ladder(entry));
        assert_eq!(handles, ["g1", "g2", "g3", "g4"].map(String::from).to_vec());
    })
}

/// What rests, as `(kind, side, price, size)`.
fn book(engine: &StrategyEngine<Once>) -> Vec<(OrderKind, i32, Option<f64>, f64)> {
    engine.core.pending_of(SYM).iter().map(|o| (o.kind, o.side, o.price, o.size)).collect()
}

fn fills(engine: &StrategyEngine<Once>) -> Vec<(i32, f64, f64)> {
    engine.strategy.fills.iter().map(|f| (f.side, f.price, f.size)).collect()
}

/// The three exits as they rest once armed, the stop at `stop` of size.
fn exits(stop: f64) -> Vec<(OrderKind, i32, Option<f64>, f64)> {
    vec![
        (OrderKind::Stop, -1, Some(95.0), stop),
        (OrderKind::Limit, -1, Some(110.0), 1.0),
        (OrderKind::Limit, -1, Some(120.0), 2.0),
    ]
}

#[test]
fn the_exits_are_held_until_the_entry_fills() {
    let engine = run_bars(params(), vec![bar(1_000, FLAT), bar(2_000, FLAT)], place(Some(99.0)));
    assert_eq!(book(&engine), vec![(OrderKind::Limit, 1, Some(99.0), 3.0)], "the entry alone");
    // the bar that fills the limit entry at 99 also crosses the stop: not armed yet, so it rests
    let bars = vec![bar(1_000, FLAT), bar(2_000, [100.0, 101.0, 94.0, 100.0])];
    let engine = run_bars(params(), bars, place(Some(99.0)));
    assert_eq!(fills(&engine), vec![(1, 99.0, 3.0)], "the entry, and no stop on its own bar");
    assert_eq!(book(&engine), exits(3.0), "armed at full size by the entry's fill");
}

#[test]
fn each_targets_fill_shrinks_the_stop_and_the_last_one_cancels_it() {
    let bars = |n: usize| {
        let all = [
            bar(1_000, FLAT),
            bar(2_000, [100.0, 112.0, 99.0, 100.0]), // the entry at 100; 110 crossed, not yet armed
            bar(3_000, [100.0, 112.0, 99.0, 111.0]), // the 110 target
            bar(4_000, [111.0, 121.0, 110.0, 120.0]), // the 120 target
        ];
        all[..n].to_vec()
    };
    let engine = run_bars(params(), bars(2), place(None));
    assert_eq!(fills(&engine), vec![(1, 100.0, 3.0)]);
    assert_eq!(book(&engine), exits(3.0));
    let engine = run_bars(params(), bars(3), place(None));
    assert_eq!(fills(&engine)[1..], [(-1, 110.0, 1.0)]);
    assert_eq!(book(&engine), vec![exits(2.0)[0], exits(2.0)[2]], "the stop is the position: 2");
    let engine = run_bars(params(), bars(4), place(None));
    assert_eq!(fills(&engine)[1..], [(-1, 110.0, 1.0), (-1, 120.0, 2.0)]);
    assert!(book(&engine).is_empty(), "the stop reduced to nothing is cancelled");
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
}

#[test]
fn a_bar_crossing_the_stop_and_a_target_fills_the_stop_first_and_cancels_the_targets() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, FLAT),
        bar(3_000, [100.0, 112.0, 94.0, 100.0]), // the 110 target and the 95 stop
    ];
    let engine = run_bars(params(), bars, place(None));
    assert_eq!(fills(&engine), vec![(1, 100.0, 3.0), (-1, 95.0, 3.0)], "the stop, capped first");
    assert!(book(&engine).is_empty(), "every target cancelled by the stop's fill");
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
    assert_eq!(engine.core.intrabar_both_hit, 1);
}

#[test]
fn a_ladder_the_builder_refuses_is_dropped_in_its_words() {
    let engine = run_bars(
        params(),
        vec![bar(1_000, FLAT), bar(2_000, FLAT)],
        Once::new(|b| {
            let mut spec = ladder(None);
            spec.targets[1].qty = 3.0;
            assert!(b.submit_ladder(&spec).is_empty());
        }),
    );
    let why: Vec<&str> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(why, vec!["the targets sum to 4, past the entry's size 3"]);
    assert!(book(&engine).is_empty() && fills(&engine).is_empty());
}

#[test]
fn an_entry_that_ends_without_a_fill_cancels_its_held_exits_in_words() {
    let p = StrategyEngineConfig { volume_limit: Some(1.0), ..params() };
    let dry = vike_simulator::test_support::bar(2_000, 100.0, 101.0, 99.0, 100.0, 0.0);
    let bars = vec![bar(1_000, FLAT), dry, bar(3_000, [100.0, 112.0, 94.0, 100.0])];
    let engine = run_bars(p, bars, place(None));
    assert!(fills(&engine).is_empty(), "the entry met no volume, and no exit ever armed");
    assert!(book(&engine).is_empty(), "nothing rests");
    let dropped: Vec<(&str, f64)> =
        engine.core.dropped.iter().map(|d| (d.reason.as_str(), d.size)).collect();
    let ended = "parent-ended: the order that arms it ended without a fill";
    assert_eq!(dropped, vec![("volume_cap", 3.0), (ended, 3.0), (ended, 1.0), (ended, 2.0)]);
}
