//! ⚠ **A post-only (maker-only) limit order never executes as a taker in the backtest.**
//!
//! `SimBroker::submit_limit_post_only` judges the order ONCE, at arrival, against the symbol's last
//! price with `vike_fills::fill_model::post_only_would_cross` — the same law the paper book applies
//! (`crates/vike-paper/src/client/post_only_tests.rs`), so backtest == paper holds for a maker-only
//! order too. A crossing order is refused (nothing rests, the run's `dropped` list says why); one
//! that would rest, rests and fills later as the limit it is.

use vike_marketdata::Bar;
use vike_model::Strategy;
use vike_sim::{EngineParams, SimBroker, StrategyEngine};

const SYM: &str = "SYM0";
const VENUE: &str = "TEST";
const PX: f64 = 100.0;

fn bars(n: usize) -> Vec<Bar> {
    (0..n).map(|i| Bar::ohlcv(60_000 * (i as i64 + 1), PX, PX, PX, PX, 1_000.0)).collect()
}

/// Submits ONE post-only order on the SECOND bar (the first one gives the symbol its price).
struct PostOnlyOnce {
    side: i32,
    price: f64,
    bars_seen: usize,
    handed_on: Option<bool>,
}

impl Strategy<SimBroker> for PostOnlyOnce {
    fn on_bar(&mut self, b: &mut SimBroker, _bar: &Bar) {
        self.bars_seen += 1;
        if self.bars_seen == 2 {
            self.handed_on =
                Some(b.submit_limit_post_only(SYM, self.side, 1.0, self.price, 1.0, true, None));
        }
    }
}

/// (handed on, orders resting, the run's drop reasons)
fn run(side: i32, price: f64) -> (Option<bool>, usize, Vec<String>) {
    let params = EngineParams {
        cash: 1_000_000.0,
        default_venue: Some(VENUE.to_string()),
        ..Default::default()
    };
    let strat = PostOnlyOnce { side, price, bars_seen: 0, handed_on: None };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars(3))], strat, params);
    let _ = e.run();
    let reasons = e.core.dropped.iter().map(|(_, r, _, _)| r.clone()).collect();
    (e.strategy.handed_on, e.core.sym[0].pending.len(), reasons)
}

#[test]
fn a_post_only_buy_through_the_last_price_is_refused() {
    let (handed_on, resting, reasons) = run(1, PX * 1.01);
    assert_eq!(handed_on, Some(false));
    assert_eq!(resting, 0, "a refused post-only order must not rest");
    assert!(reasons.iter().any(|r| r == "post_only_would_cross"), "{reasons:?}");
}

#[test]
fn a_post_only_sell_at_or_below_the_last_price_is_refused() {
    for price in [PX, PX * 0.99] {
        let (handed_on, resting, reasons) = run(-1, price);
        assert_eq!(handed_on, Some(false), "sell @ {price}");
        assert_eq!(resting, 0);
        assert!(reasons.iter().any(|r| r == "post_only_would_cross"), "{reasons:?}");
    }
}

/// NON-VACUOUS: the same engine, the same bars, an order on the maker side of the market rests.
#[test]
fn a_post_only_order_on_the_maker_side_rests() {
    let (handed_on, resting, reasons) = run(1, PX * 0.5);
    assert_eq!(handed_on, Some(true));
    assert_eq!(resting, 1, "{reasons:?}");
    assert!(!reasons.iter().any(|r| r == "post_only_would_cross"), "{reasons:?}");
    let (handed_on, resting, _) = run(-1, PX * 1.5);
    assert_eq!((handed_on, resting), (Some(true), 1));
}
