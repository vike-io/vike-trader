//! What strategy files share: a signal rule's intent per bar ([`Signal`], [`SignalSource`]) and the rule
//! it makes ([`SignalRule`]), the crosses most rules read ([`Cross`], [`CrossBands`]), and a flip-only rule's
//! side ([`flip_side`]). Copied from the strategy gallery (plan 2b, C3); a registry indicator fed per
//! bar is `vike_indicators::strategy_kit::IndicatorLine`, as this crate names no indicator (decision 0269).

use vike_marketdata::Bar;

use crate::{Rule, RuleDefaults, Tactics};
use vike_model::OrderSide::{Long, Short};

/// The intent of one bar. `Long` and `Short` hold one position of size 1, reversing an opposite
/// position and ignoring the same side already held. `Close` flattens.
#[derive(Clone, Copy)]
pub enum Signal {
    Hold,
    Long,
    Short,
    Close,
}

/// A rule that only says, bar by bar, which way to be ([`SignalRule`] trades it). It reads every bar
/// it is fed, so its indicators are warm when trading starts.
pub trait SignalSource: Send {
    fn signal(&mut self, bar: &Bar) -> Signal;
}

/// A signal rule as a rule: market orders of the header's size, one position at a time (pyramiding
/// 1), no costs of its own. A long or short entry reverses what the other side holds and is refused
/// on the side already held; a close flattens.
pub struct SignalRule<S>(pub S);

/// The entry names the two sides trade under.
const LONG: &str = "long";
const SHORT: &str = "short";

impl<S: SignalSource> Rule for SignalRule<S> {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        match self.0.signal(bar) {
            Signal::Long => {
                strategy.entry(LONG, Long);
            }
            Signal::Short => {
                strategy.entry(SHORT, Short);
            }
            Signal::Close => {
                strategy.close_all();
            }
            Signal::Hold => {}
        }
    }
}

/// Crossing up is `a > b` now with `a <= b` on the previous bar, crossing down the mirror. A NaN
/// compares false, so nothing fires while either side is still warming up.
pub struct Cross {
    a: f64,
    b: f64,
}

impl Default for Cross {
    fn default() -> Self {
        Cross::new()
    }
}

impl Cross {
    pub fn new() -> Self {
        Cross { a: f64::NAN, b: f64::NAN }
    }
    pub fn step(&mut self, a: f64, b: f64) -> Signal {
        let signal = if a > b && self.a <= self.b {
            Signal::Long
        } else if a < b && self.a >= self.b {
            Signal::Short
        } else {
            Signal::Hold
        };
        (self.a, self.b) = (a, b);
        signal
    }
}

/// Long on crossing up through one level, short on crossing down through another.
pub struct CrossBands {
    up: Cross,
    down: Cross,
    up_level: f64,
    down_level: f64,
}

impl CrossBands {
    pub fn new(up_level: f64, down_level: f64) -> Self {
        CrossBands { up: Cross::new(), down: Cross::new(), up_level, down_level }
    }
    pub fn step(&mut self, v: f64) -> Signal {
        // Both sides step on every bar, or a side skipped this bar would compare against a stale previous value.
        let up = self.up.step(v, self.up_level);
        let down = self.down.step(v, self.down_level);
        match (up, down) {
            (Signal::Long, _) => Signal::Long,
            (_, Signal::Short) => Signal::Short,
            _ => Signal::Hold,
        }
    }
}

/// Turns the two entry triggers of a flip-only rule into a signal, tracking the side in `held` because a signal sees no broker.
/// Only a bar firing both needs it: the side held decides which one is allowed. Three strategy files share it
/// (`pivot_liquidity_sweep`, `pressure_gauge`, `three_ema_bias_score`).
// shortcut: a bar firing both while truly flat takes the long alone, though the rules then fill both; nothing can be flat after
// the first trade, so this only matters before it. A warm-up `held` cannot misfile a bar firing one trigger.
pub fn flip_side(long: bool, short: bool, held: &mut i8) -> Signal {
    if long && (!short || *held <= 0) {
        *held = 1;
        Signal::Long
    } else if short {
        *held = -1;
        Signal::Short
    } else {
        Signal::Hold
    }
}
