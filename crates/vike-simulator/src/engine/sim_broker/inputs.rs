//! The as-of view of a run's INPUTS (decision 0271): each a stored series the strategy reads by name
//! (`vike_model::Strategy::inputs`), answered through `vike_model::OrderBroker::input` as the last of its
//! bars that had CLOSED by the current event.
//!
//! # The rule
//!
//! A bar of interval `I` opened at `ts` closes at `ts + I`. An event at `t` sees the last bar with
//! `ts + I <= t`: a bar closing exactly at `t` is seen, the bar in progress at `t` never is, and
//! nothing is seen before the first bar closes. On the bar lane the strategy decides at its bar's
//! CLOSE, so `StrategyEngine::run` moves the view to `bar.ts + bar_ms` before the `on_bar` fan-out, and
//! to the bar's OPEN before the fills of the step (a fill's hook inside the bar sees no input bar that
//! closed after the bar opened). On the tick lane the event is the tick.
//!
//! # The whole series (decision 0271's amendment of 2026-11-14)
//!
//! `vike_model::OrderBroker::input_whole` reads an input's bars still to come: every bar the view holds on
//! the run's LAST event ([`InputView::end_at`], set once by each run loop before its first event), so no
//! bar the loader read past the run as a guard is ever shown. Served only when
//! `crate::StrategyEngineConfig::input_whole` says the run's strategy file is backtest-only; `None`
//! otherwise.
//!
//! # The cost
//!
//! Time only moves forward, so each input keeps how many of its bars have closed and steps past a bar
//! once: [`InputView::advance_to`] is O(1) per input BAR over the whole run, never a search per event or
//! per order, and [`InputView::closed_bars`] reads the count. A run with no input pays one empty loop
//! per event.

use vike_marketdata::Bar;

/// One input of [`Inputs::series`]: the name the strategy reads it by, the length of one of its bars
/// in ms, and its bars (sorted by `ts` at construction).
#[derive(Debug, Clone, PartialEq)]
pub struct InputSeries {
    pub name: String,
    pub interval_ms: i64,
    pub bars: Vec<Bar>,
}

/// `crate::StrategyEngineConfig::inputs`: the series a run's strategy reads beside its own bars. No series
/// for every strategy that declares none, which is every run before decision 0271.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inputs {
    /// The length of one bar of the run's OWN series, in ms: a bar step's decision is at
    /// `bar.ts + bar_ms`, which its inputs are read as of and its forecasts are stamped at. Read by the
    /// bar lane alone; a tick lane's event is the tick. The harness states it for every bar run, with or
    /// without inputs; `0` (unstated) decides at the open.
    pub bar_ms: i64,
    pub series: Vec<InputSeries>,
}

/// The view an `OrderBrokerSimulator` keeps: each input and how many of its bars had closed by the last
/// advance, and by the run's last event.
#[derive(Default)]
pub(crate) struct InputView {
    inputs: Inputs,
    /// Each input's name, in the series' order: `vike_model::OrderBroker::input_names`, which the index
    /// reads ([`InputView::closed_bars_at`]) index.
    names: Vec<String>,
    closed: Vec<usize>,
    /// How many of each input's bars close by the run's last event ([`InputView::end_at`]); `None` when
    /// the run serves no whole series (`crate::StrategyEngineConfig::input_whole` off).
    whole: Option<Vec<usize>>,
}

impl InputView {
    /// The view of `inputs`, serving each one's whole series when `whole`.
    pub(crate) fn new(mut inputs: Inputs, whole: bool) -> Self {
        for s in &mut inputs.series {
            s.bars.sort_by_key(|b| b.timestamp);
        }
        let closed = vec![0; inputs.series.len()];
        let whole = whole.then(|| closed.clone());
        let names = inputs.series.iter().map(|s| s.name.clone()).collect();
        InputView { inputs, names, closed, whole }
    }

    /// Each input's name, in the order [`Self::closed_bars_at`] and [`Self::whole_bars_at`] index them.
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }

    /// The close of the run's own bar opened at `bar_timestamp`: where a bar step decides.
    pub(crate) fn close_of(&self, bar_timestamp: i64) -> i64 {
        bar_timestamp.saturating_add(self.inputs.bar_ms)
    }

    /// Move every input to the event at `t`. Never backwards: a `t` earlier than the last is a no-op.
    pub(crate) fn advance_to(&mut self, t: i64) {
        for (s, n) in self.inputs.series.iter().zip(&mut self.closed) {
            // The one as-of rule, `Bar::closed_at`, which the live mount's view reads too.
            while s.bars.get(*n).is_some_and(|b| b.closed_at(s.interval_ms, t)) {
                *n += 1;
            }
        }
    }

    /// The run's last event is at `t`: each input's whole series is what [`Self::advance_to`] would hold
    /// there. A no-op when the run serves no whole series.
    pub(crate) fn end_at(&mut self, t: i64) {
        let Some(whole) = &mut self.whole else { return };
        for (s, n) in self.inputs.series.iter().zip(whole) {
            *n = s.bars.partition_point(|b| b.closed_at(s.interval_ms, t));
        }
    }

    /// The closed bars of the input `name`, oldest first: empty before its first bar closes or for a
    /// name the run loaded no series for.
    pub(crate) fn closed_bars(&self, name: &str) -> &[Bar] {
        self.names.iter().position(|n| n == name).map_or(&[], |k| self.closed_bars_at(k))
    }

    /// [`Self::closed_bars`] of the `k`th input ([`Self::names`]).
    pub(crate) fn closed_bars_at(&self, k: usize) -> &[Bar] {
        &self.inputs.series[k].bars[..self.closed[k]]
    }

    /// The whole series of the input `name` (the module doc), oldest first: empty for a name the run
    /// loaded no series for, `None` when the run serves no whole series.
    pub(crate) fn whole_bars(&self, name: &str) -> Option<&[Bar]> {
        self.whole.as_ref()?;
        let Some(k) = self.names.iter().position(|n| n == name) else { return Some(&[]) };
        self.whole_bars_at(k)
    }

    /// [`Self::whole_bars`] of the `k`th input ([`Self::names`]).
    pub(crate) fn whole_bars_at(&self, k: usize) -> Option<&[Bar]> {
        let whole = self.whole.as_ref()?;
        Some(&self.inputs.series[k].bars[..whole[k]])
    }
}
