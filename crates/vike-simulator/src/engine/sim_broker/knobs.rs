//! The engine's mode knobs: the fill model, the decide mode, equity sampling.

use serde::Deserialize;

#[cfg(doc)]
use super::params::StrategyEngineConfig;

/// Which fill model the engine uses (Bar = default, Tick = L1 spread-crossing, L2Book = walk the
/// resting book). Lives here (with `OrderBrokerSimulator`/`StrategyEngineConfig`, its only users) so the module graph
/// stays acyclic — engine.rs re-exports it, and the crate root names it `vike_simulator::FillModelKind`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FillModelKind {
    #[default]
    Bar,
    Tick,
    /// [`vike_fills::fill_model::FillModelL2Book`] — a taker is priced by CONSUMING the replayed L2
    /// book for its own size, not by reading one tick price. Requires a tape carrying
    /// `Tick::Book` events (it degrades to [`FillModelKind::Tick`] wherever no book exists), and
    /// is only ever reached by asking for it, so selecting `Bar`/`Tick` is byte-identical to the
    /// engine before this variant existed.
    ///
    /// Pair it with `slippage: 0.0`: the walk IS the slippage, and stacking a flat
    /// `adverse_fill_price` haircut on top would double-charge it.
    L2Book,
}

/// How densely the TICK lane records its equity curve ([`StrategyEngineConfig::equity_sampling`]).
///
/// The curve is two `Vec`s (`equity_curve` + `equity_timestamp`) grown by 16 bytes per priced tick,
/// unconditionally — on a 100M-tick tape that is 1.6 GB of resident memory per run, paid by every
/// point of a parameter sweep even when nothing ever reads the curve. This knob is the opt-out.
///
/// [`Self::EveryTick`] is the DEFAULT and is byte-identical to the engine before this type
/// existed: a run that does not set it produces the identical curve — same length, same values,
/// same timestamps. The other two variants are a deliberate fidelity trade — see each.
///
/// TICK LANE ONLY (`StrategyEngine::run_ticks`). The bar lane records one sample per BAR, which is
/// bounded by the bar count and needs no thinning, so `run` ignores this field entirely.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EquitySampling {
    /// One sample per priced tick — the frozen default.
    #[default]
    EveryTick,
    /// Keep one sample per `n` priced ticks (ticks `0, n, 2n, …`), plus a closing sample at the
    /// run's LAST priced tick so `equity_curve.last()` still agrees with `final_equity` and the
    /// end-of-run settlement sweeps still patch the right row.
    ///
    /// `n <= 1` names the same sampling as [`Self::EveryTick`] and is treated as it.
    ///
    /// Fidelity: every curve-DERIVED statistic (max drawdown, sharpe, the return series) is then
    /// computed over the subsample and is an approximation — a drawdown that opens and closes
    /// between two kept samples is invisible. Nothing else moves: `final_equity`, `trades`,
    /// `n_trades`, `per_symbol_pnl` and every fill are unaffected, because none of them is derived
    /// from the curve.
    EveryN(usize),
    /// Record nothing — both curve vectors stay EMPTY. For a sweep that ranks on
    /// `final_equity`/`n_trades` only. Every curve-derived statistic degenerates (a metric over an
    /// empty series), so never combine this with a drawdown- or sharpe-ranked sweep.
    Off,
    /// Keep the curve by TIME, steps of this many ms (decision 0078's amendment of 2026-11-20; a tick profile's
    /// default): the run's first priced tick, then the LAST priced tick of each step that saw one, so the run's last
    /// tick ends the curve and the end-of-run sweeps patch it as they patch `EveryTick`'s. A step never goes back:
    /// under feed latency a tick that arrives with an earlier venue stamp joins the step already open, so the closes
    /// stay one a step and in time order. The return statistics then read equity at equal times, as a bar run's do. Every priced tick's equity is still folded into
    /// `vike_analytics::TradingRecordSampling` (the record's `sampling`): the maximum drawdown and the first time
    /// equity reached zero stay the whole path's, to the bit. A step of `0` or less is [`Self::EveryTick`].
    ByTime(i64),
}

/// An opt-in margin model that cuts a position the account cannot carry by `cover` times its
/// shortfall ([`StrategyEngineConfig::shortfall_cover`]; `None`, the default, leaves the bar path's margin
/// call and every fill as they were).
///
/// The account at a mark is `equity = cash + Σ size·mark·mult` against
/// `margin = Σ |size|·mark·mult·rate`, `rate` being [`Self::margin_long`] or [`Self::margin_short`]
/// by the position's side, and it is short when `available = equity - margin < 0`. The commission
/// an open position paid stays out of it, as it is out of the cash: giving it back took three
/// commission-paying rows of the strategy lab further from their reference.
///
/// The margin call, on each bar a position is held, after the bar's fills and before the strategy
/// runs, at two checkpoints in turn: the bar's OPEN, then each position's ADVERSE extreme (the low
/// for a long, the high for a short), every held position at that checkpoint's mark.
///
/// A bar that has a WALK (`StrategyEngineConfig::granular_by_symbol`: the open, then the stretch to the extreme nearer
/// the open, the stretch to the other, the stretch to the close) is judged as it fills instead: after each
/// sub-bar's fills, every position held then is judged with the symbol at that sub-bar's close and the others at
/// their last price, in the same order and by the same rule. A position is therefore judged where the walk had
/// it: not at an extreme that came before its entry, and at the extremes that came before a later fill of the
/// same bar closed it (a target met on the way back does not hide the call at the extreme the price went to
/// first). A step with no walk, and every run on the cash-gated lane, keeps the two checkpoints above.
///
/// - The just-enough size is what brings the account back to its margin at that mark:
///   `|available| / rate / (mark·mult)`, floored to [`Self::step`]. The position is cut by
///   [`Self::cover`] times that; a shortfall below one step cuts one whole unit instead (`1.0`, or
///   the step when that is larger). Never more than the position holds.
/// - The cut fills at the checkpoint's mark as any forced close does (`OrderBrokerSimulator::apply_fill`: the
///   slippage and [`StrategyEngineConfig::slippage_amount`] against the position, and the commission), so it
///   is a closed trade of its own, and `on_fill` sees it. `OrderBrokerSimulator::shortfall_cover_fills` counts
///   it first, which is how a strategy tells it from a fill of its own orders.
/// - With several symbols held, each is judged in `cross_section()` order against the whole
///   account, after the cuts before it.
///
/// The fill gate, on every fill lane: a fill that leaves a position on its own side whose margin,
/// at the fill's price, exceeds the account's equity at that moment (as above, the positions at
/// their last price, so on the bar path the previous close until a fill moves the cash) is refused
/// WHOLE, a flip's closing half with it, recorded in `OrderBrokerSimulator::dropped` as
/// `"insufficient-margin"`. Not when a fill earlier in the same step reduced the position on that
/// same side: that is the entry of a close-then-enter reversal, which fills and is margin-called.
/// Nor when an earlier fill of the step changed the position and this fill fits against the position
/// and the equity the step began with: an entry stacked on one that filled before it in the step
/// (two entries at one open, each a whole account) fills, and the call cuts the stack.
///
/// `cover: 4.0` is the size measured on a reference that margin-calls this way, and the open
/// checkpoint, the whole unit and the gate were each measured on it too; `cover: 1.0` closes just
/// enough.
///
/// ⚠ BAR PATH ONLY: `StrategyEngine::run_ticks` refuses a run that sets it. The call is judged at a
/// bar's open and extreme, which a tick does not have, and judging every print with a cut of four
/// times the shortfall is another rule, with nothing measured behind it.
///
/// It IS a backtest profile's `[engine.margin_call]` table, field for field (`vike-backtest` reads the
/// TOML straight into it, and refuses what [`Self::validate`] refuses, naming the key):
///
/// ```toml
/// [engine.margin_call]
/// cover = 4.0          # a call closes this many times the just-enough size
/// margin_long = 0.2    # the margin a long carries, a fraction of its value (20%)
/// margin_short = 0.2
/// step = 0.00001       # the quantity step the just-enough size floors to (absent: none)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ShortfallCover {
    /// The multiple of the just-enough size a call closes: `4.0` is what the measured reference
    /// does, `1.0` closes just enough.
    pub cover: f64,
    /// The margin a long must carry, a fraction of its value at the mark (`0.2` is 20%, `1.0` all of
    /// it). `0` never calls or refuses a long.
    pub margin_long: f64,
    /// [`Self::margin_long`] for a short.
    pub margin_short: f64,
    /// The instrument's quantity step the just-enough size floors to (`0.00001` for BTCUSDT).
    /// Absent: `0`, which floors nothing.
    #[serde(default)]
    pub step: f64,
}

impl ShortfallCover {
    /// The rate a position of `size` is margined at.
    pub(crate) fn rate_for(&self, size: f64) -> f64 {
        if size > 0.0 { self.margin_long } else { self.margin_short }
    }

    /// `Err` naming the first number that cannot be one: [`Self::cover`] must be finite and above
    /// `0`, the two margins and [`Self::step`] finite and `0` or more. The message opens with the
    /// field's name, which is its profile key too. `StrategyEngine::new` panics on a rule that fails
    /// it, so a caller that builds one by hand is checked as a profile is.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.cover.is_finite() && self.cover > 0.0) {
            return Err(format!("cover must be > 0, got {}", self.cover));
        }
        for (key, v) in [
            ("margin_long", self.margin_long),
            ("margin_short", self.margin_short),
            ("step", self.step),
        ] {
            if !(v.is_finite() && v >= 0.0) {
                return Err(format!("{key} must be a number >= 0, got {v}"));
            }
        }
        Ok(())
    }
}
