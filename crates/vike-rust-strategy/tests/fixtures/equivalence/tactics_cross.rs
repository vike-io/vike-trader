//! `tactics_cross` — the ORDER LANGUAGE fixture `vike-rust-strategy`'s `tests/equivalence.rs`
//! drives down both mechanisms (`ABI_VERSION` 5, decision 0269's later wave: built-ins, Rhai and
//! Rust strategy files write orders the same way).
//!
//! An ordinary user strategy written as a `vike_tactics::Rule`, its `build` bounding
//! `B: vike_model::OrderBroker` (the bound `vike_tactics::RuleRun` takes, the one broker interface). It
//! sat in the build-time tier's fixture tree until that tier was deleted (plan 2b), and moved here
//! unchanged. The equivalence test compiles THESE BYTES twice, as a module of the test binary (its
//! `build` called directly, the mechanism that tier generated) and through the real builder into a
//! strategy file, runs both over
//! the same bars and compares them bit for bit, so each verb below is a path across the boundary
//! that must land as it does compiled in:
//!
//! | what the rule does | what crosses |
//! |---|---|
//! | an entry and its exit in one pass | `OrderBroker::place_group` (one OTO list, its legs as lent `repr(C)` rows) where the broker refuses a reduce-only order while flat; `place_exit_leg` where it rests one |
//! | a stop-limit entry with percent exits and a trail | `place` of a `StopLimit`; the exits wait for its fill, then `place_exit_leg` of a `Stop`, a `Limit` and a `Trailing` |
//! | a touch trail on the long side | `place_exit_leg` of a `TakeProfit` once the entry fills |
//! | naming an exit again while held | `OrderBroker::amend` |
//! | a close, at the next fill or at this bar's close | `place` (reduce-only `Market`) and `place_at_close` |
//! | a free limit, cancelled five bars on | `place` of a `Limit` and `OrderBroker::cancel` |
//! | the fills-a-day cap | `place_close_from_fill` |
//! | every pass | `rests_reduce_only_while_flat`, `OrderBroker::instrument_grid` (the tick levels snap to) |
//! | an entry's size, from the input it declares and the closed net (`ABI_VERSION` 6) | the `inputs` export, `OrderBroker::input_bars`, `OrderBroker::closed_trades` |
//! | with `other` set: a hedge on a SECOND symbol (`Tactics::on`), its bars read as they close (`ABI_VERSION` 8) | every order verb with the symbol it names, the `symbols` export (`RuleRun::trades`) |
//!
//! and every fill and order end comes back with its tag (`FillAbi::tag`, `OrderEventAbi::tag`),
//! which is how `RuleRun` books what it sent.

use vike_marketdata::Bar;
use vike_model::params::{ParamValue, Params};
use vike_model::{Commission, OrderSide};
use vike_tactics::{Rule, RuleDefaults, RuleRun, Tactics, TacticsTrail};

/// The most fills in one UTC day before the cap closes what is held. The series spans two days.
const FILLS_A_DAY: usize = 13;

/// A dual-SMA crossover on the bar's close, written in the order language.
pub struct Cross {
    fast: usize,
    slow: usize,
    closes: Vec<f64>,
    /// Bars seen, so the rule's periodic verbs land on fixed bars.
    n: usize,
    /// The stored series the rule reads beside its bars (`Rule::inputs`), named by the params;
    /// `None` declares none.
    input: Option<String>,
    /// How hard the run's closed net moves an entry's size; `0.0`, the fallback, reads it for nothing.
    ledger_gain: f64,
    /// A second symbol the rule hedges on (`RuleRun::trades`); `None`, the fallback, trades its own
    /// alone, every bar of which is then the rule's.
    other: Option<String>,
}

impl Cross {
    /// An entry's size: the header's 3, tilted by the input (its last close against the mean of its
    /// last 10, `Tactics::input`) and by the run's closed net (`Tactics::closed_net`), bounded so
    /// it never reaches 0 (an entry of size 0 only closes).
    fn size(&self, strategy: &Tactics<'_>) -> f64 {
        let tilt = self.input.as_deref().map_or(0.0, |name| {
            let input = strategy.input(name);
            let window = input.bars(10);
            match input.close() {
                Some(last) if !window.is_empty() => {
                    let mean = window.iter().map(|b| b.close).sum::<f64>() / window.len() as f64;
                    last / mean - 1.0
                }
                _ => 0.0,
            }
        });
        let ledger =
            strategy.closed_net().map_or(0.0, |net| (self.ledger_gain * net).clamp(-0.5, 0.5));
        3.0 * (1.0 + tilt) + ledger
    }

    fn sma(&self, period: usize) -> Option<f64> {
        let tail = self.closes.len().checked_sub(period).map(|from| &self.closes[from..])?;
        Some(tail.iter().sum::<f64>() / period as f64)
    }
}

impl Rule for Cross {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { size: 3.0, pyramiding: 1, fee: Commission::None, ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        self.input.iter().cloned().collect()
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if let Some(other) = self.other.as_deref()
            && bar.symbol.as_deref() == Some(other)
        {
            // The second symbol's bar: hold one unit against the run's own position there.
            let (held, hedge) = (strategy.position(), strategy.position_of(other));
            if held > 0.0 && hedge >= 0.0 {
                strategy.on(other).entry("hedge", OrderSide::Short).size(1.0);
            } else if held < 0.0 && hedge <= 0.0 {
                strategy.on(other).entry("hedge", OrderSide::Long).size(1.0);
            }
            return;
        }
        self.closes.push(bar.close);
        self.n += 1;
        let (Some(fast), Some(slow)) = (self.sma(self.fast), self.sma(self.slow)) else {
            return;
        };
        let px = bar.close;
        let held = strategy.position();
        let size = self.size(strategy);
        if fast > slow && held <= 0.0 {
            strategy.entry("long", OrderSide::Long).size(size);
            strategy.exit("long guard").from("long").stop(px - 2.0).take(px + 3.0);
            strategy
                .exit("long touch")
                .from("long")
                .trail(TacticsTrail { after_ticks: 30.0, by_ticks: 0.0 });
        } else if fast < slow && held >= 0.0 {
            strategy.entry("short", OrderSide::Short).size(size).stop(px - 0.5).limit(px - 1.0);
            strategy.exit("short guard").from("short").stop_pct(1.5).take_pct(2.5);
            strategy
                .exit("short trail")
                .from("short")
                .trail(TacticsTrail { after_ticks: 20.0, by_ticks: 10.0 });
        } else if held > 0.0 && self.n.is_multiple_of(4) {
            strategy.exit("long guard").from("long").stop(px - 2.0).take(px + 3.0);
        } else if held < 0.0 && px > fast {
            if self.n.is_multiple_of(2) {
                strategy.close("short").here();
            } else {
                strategy.close("short");
            }
        }
        match self.n % 10 {
            0 => {
                strategy.order("dip", OrderSide::Long).limit(px - 3.0);
            }
            5 => strategy.cancel("dip"),
            _ => {}
        }
    }
}

/// The entry contract, bound by the order seam: `RuleRun` makes the rule a `Strategy<B>` for every
/// `B: OrderBroker`, a strategy file's `HostBroker` included.
pub fn build<B: vike_model::OrderBroker + 'static>(
    params: &Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let window = |key: &str, default: usize| -> usize {
        params
            .get(key)
            .and_then(ParamValue::as_integer)
            .map(|i| i.max(1) as usize)
            .unwrap_or(default)
    };
    let symbol = params.get("symbol").and_then(ParamValue::as_str).unwrap_or("BTCUSDT");
    let rule = Cross {
        fast: window("fast", 5),
        slow: window("slow", 20),
        closes: Vec::new(),
        n: 0,
        input: params.get("input").and_then(ParamValue::as_str).map(str::to_string),
        ledger_gain: params.get("ledger_gain").and_then(ParamValue::as_float).unwrap_or(0.0),
        other: params.get("other").and_then(ParamValue::as_str).map(str::to_string),
    };
    let traded: Vec<String> = rule.other.iter().cloned().collect();
    Box::new(RuleRun::new(rule, symbol).trades(traded.as_slice()).fills_a_day(FILLS_A_DAY))
}
