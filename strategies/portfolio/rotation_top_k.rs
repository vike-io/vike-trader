//! Rotation top-k: a top-k momentum rotation over the run's universe (k = 2, every 10 bars). Score
//! = last/first of the seen closes; the winners get 1/k of the equity each, held drop-outs 0 — the
//! CrossSectionalStrategy pattern.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! ```
//!
//! On the ORDER SEAM (`impl<B: OrderBroker> Strategy<B>`), and trades in the simulator alone: it
//! ranks the run's universe ([`OrderBroker::symbols`]), which the live core does not answer (a
//! mount trades one series), so live it would mount and never trade. It counts its own bars to each
//! rebalance and sizes each target itself through [`vike_model::ContractValue::units_worth`].
//!
//! ⚠ Pinned BIT-FOR-BIT by `crates/vike-simulator/tests/parity/r4_parity.rs`'s `mse_runs_bit_parity`
//! (the library's twin of this file, `vike_simulator::RotationTopK`). Changing which verbs it calls is a
//! parity question, never a tidiness one.

use vike_marketdata::{Bar, Symbol};
use vike_model::params::Params;
use vike_model::{OrderBroker, OrderPlacement, Strategy};

/// The strategy this file runs. It reads no params: a profile naming a key is refused, since the
/// header declares none.
pub fn build<B: OrderBroker + 'static>(_params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(RotationTopK::default())
}

fn symbol_key(instrument: &Option<Symbol>) -> String {
    // Strategy._sym_key: strip ".VENUE" (exporter runs with default_venue=None → bare already)
    let s = instrument.as_deref().unwrap_or("_");
    match s.split_once('.') {
        Some((symbol, _)) => symbol.to_string(),
        None => s.to_string(),
    }
}

/// Top-k momentum rotation (k = 2, every 10 bars): score = last/first of the seen closes; winners
/// get 1/k of the equity, held drop-outs 0 — the CrossSectionalStrategy pattern.
///
/// On the order seam, and trades in the simulator alone: it ranks the run's universe
/// ([`OrderBroker::symbols`]), which the live core does not answer (a mount trades one series). Its
/// three former engine reads are its own now, each byte-identical to the one it replaced:
///
/// * the **universe**: [`OrderBroker::symbols`], the run's symbols in the run's order (the
///   `OrderBrokerSimulator::symbols` field it read);
/// * the **every-10-bars rebalance**: counted here (the owner's answer Q2 of 2026-11-10), at the
///   step's LAST symbol, when every bar of the step is in, on a step whose index
///   ([`vike_model::OrderBroker::index`]) is a multiple of 10 — the steps `ScheduleDateRule::every_n_bars(10)`
///   fired on, warm-up included. It now runs INSIDE the step's decide rather than after it, so the
///   simulator's in-bar rules (`InBarRules`, all off by default) see its orders as any order placed
///   on the bar: under `fill_at_close` they fill at that bar's close;
/// * the **target percent**: this file's `order_target_percent`,
///   `OrderBrokerSimulator::strategy_order_target_percent`'s arithmetic in seam reads, each order an UNTAGGED
///   market order (`place(symbol, "", ..)`), the lane `OrderBrokerSimulator::submit`'s raw arm takes.
#[derive(Default)]
pub struct RotationTopK {
    hist: Vec<(String, Vec<f64>)>,
    /// The step whose bars are arriving ([`vike_model::OrderBroker::index`]), and how many have.
    step: Option<usize>,
    seen: usize,
}

impl RotationTopK {
    /// Winners held.
    const K: usize = 2;
    /// The rebalance cadence, in steps.
    const EVERY: usize = 10;

    fn hist_mut(&mut self, key: &str) -> &mut Vec<f64> {
        if let Some(i) = self.hist.iter().position(|(k, _)| k == key) {
            &mut self.hist[i].1
        } else {
            self.hist.push((key.to_string(), Vec::new()));
            &mut self.hist.last_mut().unwrap().1
        }
    }

    fn rebalance<B: OrderBroker>(&self, b: &mut B) {
        let universe = b.symbols().to_vec();
        // scores in the run's symbol order (Python: dict insertion order = symbols order)
        let mut scores: Vec<(String, f64)> = Vec::new();
        for symbol in &universe {
            if let Some((_, h)) = self.hist.iter().find(|(k, _)| k == symbol)
                && !h.is_empty()
            {
                scores.push((symbol.clone(), h[h.len() - 1] / h[0]));
            }
        }
        if scores.len() < Self::K {
            return;
        }
        // stable sort desc by score (ties keep insertion order — Python sorted semantics)
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let winners: Vec<String> = scores.iter().take(Self::K).map(|(s, _)| s.clone()).collect();
        let w = 1.0 / winners.len() as f64;
        let mut target: Vec<(String, f64)> = winners.iter().map(|s| (s.clone(), w)).collect();
        for symbol in universe {
            if b.position(&symbol) != 0.0 && !target.iter().any(|(s, _)| *s == symbol) {
                target.push((symbol, 0.0));
            }
        }
        for (symbol, pct) in target {
            order_target_percent(b, &symbol, pct);
        }
    }
}

impl<B: OrderBroker> Strategy<B> for RotationTopK {
    fn on_bar(&mut self, b: &mut B, bar: &Bar) {
        self.hist_mut(&symbol_key(&bar.symbol)).push(bar.close);
        let step = b.index();
        if self.step != Some(step) {
            self.step = Some(step);
            self.seen = 0;
        }
        self.seen += 1;
        if self.seen == b.symbols().len() && step.is_multiple_of(Self::EVERY) {
            self.rebalance(b);
        }
    }
}

/// Holds `pct` of the equity in `symbol` by an untagged market order for the difference:
/// `OrderBrokerSimulator::strategy_order_target_percent`'s arithmetic in seam reads, bit for bit — the target
/// is [`vike_model::ContractValue::units_worth`] of `pct` of the equity in the symbol's quote
/// currency (`ContractValue::in_quote`: a coin ledger's coin at this price), at par; a difference
/// inside 1e-12 sends nothing. With no usable price, or no contract value, it HOLDS: read as a
/// target of 0 units it would close the position on a zero or negative print.
///
/// ⚠ The position read is the strategy's ([`vike_model::OrderBroker::position`]), the response-latency
/// shadow while the opt-in latency gate is armed; that gate arms in the tick lane alone, where no
/// `on_bar` fires, so on the bar lane this strategy runs on it is exchange truth, the read it replaced.
fn order_target_percent<B: OrderBroker>(b: &mut B, symbol: &str, pct: f64) {
    let price = b.price(symbol);
    if price <= 0.0 {
        return;
    }
    let Some(value) = b.contract_value(symbol) else { return };
    let equity = value.in_quote(b.equity(), price);
    let target = value.units_worth(pct * equity, price, 1.0).unwrap_or(0.0);
    let delta = target - b.position(symbol);
    if delta.abs() > 1e-12 {
        let side = if delta > 0.0 { 1 } else { -1 };
        b.place(symbol, "", side, delta.abs(), OrderPlacement::Market, false);
    }
}

// Copied from the simulator's former `ref_strategies.rs` tests (the rotation's two; that file is
// deleted, plan 2b). Run by
// `crates/vike-strategy-lab` (`cargo test -p vike-strategy-lab`); a build of this file
// compiles no test.
#[cfg(test)]
mod tests {
    use super::*;
    use vike_simulator::OrderBrokerSimulator;

    /// COMPILE-TIME proof that [`RotationTopK`] is on the order seam, not merely spelled so: a
    /// generic function's body is type-checked at its DEFINITION, so `RotationTopK: Strategy<B>`
    /// must hold for EVERY `B: OrderBroker`. An `OrderBrokerSimulator`-only verb narrows it back and this stops
    /// compiling; instantiating at `OrderBrokerSimulator` merely gives the test something to run.
    #[test]
    fn rotation_top_k_mounts_on_any_order_broker() {
        fn mounts<B: OrderBroker, S: Strategy<B>>(_s: S) {}
        fn probe<B: OrderBroker>() {
            mounts::<B, RotationTopK>(RotationTopK::default());
        }
        probe::<OrderBrokerSimulator>();
    }

    /// The rebalance runs INSIDE the step's decide (the strategy counts its own bars, the owner's
    /// answer Q2 of 2026-11-10), so `[engine.in_bar] fill_at_close` takes its market orders as it
    /// takes any order placed on the bar: they fill at THAT bar's close. While a bar-count schedule
    /// fired the rebalance after the in-bar pass, they waited for the next open.
    #[test]
    fn the_rebalance_is_placed_on_its_bar_so_fill_at_close_takes_it() {
        use vike_simulator::{InBarRules, StrategyEngine, StrategyEngineConfig};
        let series = |symbol: &str, base: f64| -> (String, Vec<Bar>) {
            let bars = (0..3)
                .map(|i| {
                    let close = base + i as f64;
                    Bar {
                        timestamp: 60_000 * i,
                        open: close - 0.5,
                        high: close,
                        low: close - 0.5,
                        close,
                        volume: 1.0,
                        funding: None,
                        bid: None,
                        ask: None,
                        symbol: Some(symbol.into()),
                    }
                })
                .collect();
            (symbol.to_string(), bars)
        };
        let params = StrategyEngineConfig {
            cash: 10_000.0,
            mirror: true,
            in_bar: InBarRules { fill_at_close: true, ..Default::default() },
            ..Default::default()
        };
        let mut engine = StrategyEngine::new(
            vec![series("A", 100.0), series("B", 50.0)],
            RotationTopK::default(),
            params,
        );
        engine.run();
        let fills = engine.core.mirror_fills.take().expect("the mirror is armed");
        let first = fills.first().expect("the first rebalance buys both winners");
        assert_eq!((first.timestamp, first.price), (0, 100.0), "bar 0's close: {fills:?}");
    }
}
