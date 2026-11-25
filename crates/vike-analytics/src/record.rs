//! The one trading record type. Unifies the former `RunResult` / `MultiSymbolResult` /
//! `FastResult` / `PortfolioFastResult` — a flat superset where each producer fills what it has and
//! leaves the rest empty/0 (decision A in the engine-unification spec) — and says WHO produced it:
//! [`TradingRecordSource`], a backtest (with the simulator's own counters,
//! [`TradingRecordSimulator`]), one walk-forward step, or one live account at its tier.

use vike_model::{ForecastRow, OrderDrop, Trade};

/// **Who produced a [`TradingRecord`]**: the three sources a set of results can come from. Live
/// trading is a source, not a check (the owner, 2026-11-17).
#[derive(Debug, Clone, PartialEq)]
pub enum TradingRecordSource {
    /// The simulator's own run, with the counters only a simulator keeps.
    Backtest(TradingRecordSimulator),
    /// One step of a walk-forward: its out-of-sample `window` as `[start, end)` bar indices and the
    /// parameters the step's optimization chose (`None` when the step ran fixed parameters).
    WalkforwardStep {
        window: (usize, usize),
        params: Option<Vec<(String, vike_model::params::ParamValue)>>,
    },
    /// One account of one venue, at the tier it runs at (decision 0156).
    Live { account: vike_model::accounts::account_keys::AccountRef, tier: vike_model::AccountTier },
}

/// **The simulator's own record**: [`TradingRecord::default`]'s source, so the literal construction
/// sites that spell `..Default::default()` are simulator records unless they say otherwise.
impl Default for TradingRecordSource {
    fn default() -> Self {
        Self::Backtest(TradingRecordSimulator::default())
    }
}

/// The span a record covers, in epoch-ms: its first and last equity sample ([`TradingRecord::period`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradingPeriod {
    pub from_ms: i64,
    pub to_ms: i64,
}

/// One epoch-ms instant as a source label prints it: UTC, to the minute, or the bare number when it is
/// out of chrono's range.
fn label_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map_or_else(|| ms.to_string(), |t| t.format("%Y-%m-%d %H:%M").to_string())
}

impl TradingRecordSource {
    /// **THE label of a set of results**, so every view names its source and period in the same words:
    /// `backtest <file> <from> – <to> UTC`, `walk-forward step [a, b)`, or
    /// `<venue>/<LABEL> (<tier>) <from> – <to> UTC`. `file` is the backtest's strategy file (or its
    /// name); a `None` period leaves the span out. `crate::results::TradingResults::source_label` is
    /// this over a report's own source, period and file.
    #[must_use]
    pub fn label(&self, file: Option<&str>, period: Option<TradingPeriod>) -> String {
        let span = |head: String| match period {
            Some(p) => format!("{head} {} – {} UTC", label_time(p.from_ms), label_time(p.to_ms)),
            None => head,
        };
        match self {
            Self::Backtest(_) => span(format!("backtest {}", file.unwrap_or("(unnamed)"))),
            Self::WalkforwardStep { window: (start, end), .. } => {
                format!("walk-forward step [{start}, {end})")
            }
            Self::Live { account, tier } => {
                span(format!("{}/{} ({tier})", account.venue, account.label))
            }
        }
    }
}

/// The counters only a simulator keeps, carried by [`TradingRecordSource::Backtest`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TradingRecordSimulator {
    /// Event engines only (SL+TP both-hit count); kernels leave 0.
    pub intrabar_both_hit: u32,
    /// How many times the opt-in stale-price wait discipline
    /// (`vike_simulator::StrategyEngineConfig::max_price_staleness_ms`) DEFERRED a market order instead of
    /// filling it against stale data. Always `0` when that knob is `None` (the default) and for
    /// every vector-kernel run. Carried on the result — not just on the engine — so the
    /// `harness`/`backtest`-bin entry points, which drop the engine, can still tell "zero trades
    /// because the tape never printed" apart from "zero trades because the strategy never
    /// traded". See `vike_fills::staleness`.
    pub stale_deferrals: u64,
    /// Fills at which a CONFIGURED impact model (a slippage table's `market_impact`) charged
    /// nothing, because the market
    /// context it needs (`average_volume`, `sigma`) could not be measured from the series.
    ///
    /// The mirror of `vike_simulator::OrderBrokerSimulator::impact_unpriced`, and it exists because the skip is
    /// otherwise invisible: the engine falls back to the flat slippage, so a run configured with an
    /// impact model comes back priced without one — no error, no warning, and a number
    /// indistinguishable from a priced one.
    ///
    /// ⚠ The usual cause is the DATA, not the config: the context folds through a mean traded size,
    /// so a bar series carrying `volume = 0` disables impact at every coefficient. A non-zero count
    /// is not automatically a fault — the opening fills of a run legitimately precede a measurable
    /// window — but a count equal to the fill count means impact was never applied at all.
    pub impact_unpriced: u64,
    /// How many times the opt-in session gate (`vike_simulator::StrategyEngineConfig::session_gate`) refused a
    /// fill because the venue was CLOSED — resting orders deferred plus tagged-maker and armed
    /// protective-stop passes skipped. Always `0` when the gate is off (the default) and for every
    /// vector-kernel run. Carried on the result for the same reason as `stale_deferrals`: the
    /// `harness`/`backtest`-bin entry points drop the engine, and "few trades because the venue
    /// was shut most of the tape" must stay distinguishable from "the strategy never traded".
    /// See `vike_venues::session`.
    pub session_deferrals: u64,
    /// The engine's gate-drop diagnostics channel (`vike_simulator::OrderBrokerSimulator::dropped`) mirrored off the
    /// engine at end-of-run: a `vike_model::OrderDrop` for every order a gate REFUSED — the
    /// live `RiskGate`'s own reason strings (`"insufficient-margin"`, `"below-min-qty"`, …),
    /// order-kind cash-gate drops, `"volume_cap"`, and `"latency_reject"`. Empty for every
    /// vector-kernel run and for any run where nothing was dropped. Carried on the result — a single
    /// `clone` AFTER the fold loop, NOT new hot-path counting — for the same reason as the two
    /// deferral counters above: the `harness`/`backtest`-bin entry points drop the engine, and
    /// "zero trades because every order was denied" must stay distinguishable from "the strategy
    /// never traded". Consumed by [`crate::zero_trade`].
    pub dropped: Vec<OrderDrop>,
    /// When each of [`Self::dropped`] was refused, aligned by index: the time of the step the engine
    /// was in (`vike_simulator::OrderBrokerSimulator::dropped_at`), epoch ms. Empty when nothing stamped
    /// them (a vector kernel, a record decoded off the wire); a report's denial groups then carry no times.
    pub dropped_at: Vec<i64>,
    /// ⚠ **Below-min REVERSALS this backtest split, where the LIVE gate denies the whole order.**
    /// Carried the same way as the counters above: cloned after the fold loop, never counted on the
    /// hot path. The refused halves are in [`Self::dropped`].
    ///
    /// The condition: the order opposes the position, EXCEEDS it (so it flips through flat and
    /// `is_covered_reduce` is false), and falls below a venue floor. `OrderBrokerSimulator::fill_terms` fills
    /// the closing half and refuses the opening half; the live `RiskGate` refuses it all. So neither
    /// opens the far side.
    ///
    /// ⚠ **It is expected to stay ZERO above dust sizes.** A flip must exceed the position, so the
    /// condition needs `|position| < flip < min_qty` — the position must already be below the floor,
    /// which an opening order cannot produce. Against the real venue floors of a BTC perpetual
    /// (binance/bybit `min_notional` 5.0, okx 1.0, as the venues listed them) that means
    /// reversing a position worth **under ~$5**. Any strategy sizing above a few dollars per order cannot reach it.
    ///
    /// The counter exists so that reasoning is FALSIFIABLE rather than asserted: a non-zero reading
    /// on a real profile means the argument above is wrong. `OrderBrokerSimulator::below_min_reversals` is the
    /// source.
    pub below_min_reversals: u64,
    /// The warm-up requirement the run actually GATED on, in bars/ticks, captured once at
    /// end-of-run. `0` for a strategy with no warm-up (the default) and for every vector-kernel run.
    /// Carried so a zero-trade run can tell "the strategy stayed in warm-up the whole window
    /// (lookback > bars)" apart from a strategy that ran but never traded. Consumed by
    /// [`crate::zero_trade`].
    ///
    /// ⚠ **The EFFECTIVE number, not `Strategy::warmup()` — and this doc said the latter.** The
    /// event engines resolve `max(StrategyEngineConfig::warmup, strategy.warmup())` once at construction
    /// and gate on that, so a profile-declared floor (the harness's `data.warmup` key) reads here
    /// too. The distinction matters precisely for [`crate::zero_trade`]: the `warmup-shortfall`
    /// cause compares this against the step count, and reporting the strategy's own smaller number
    /// would diagnose a run as "never signalled" when the true cause was a floor the operator set.
    pub warmup: usize,
    /// ORDER FILLS this run booked as MAKER fills, and the count is the only way a reader can tell
    /// a maker/taker fee schedule that MATTERED from one that was resolved and then never touched.
    ///
    /// ⚠ **It is the engine's OWN classification, not a re-derivation** — `OrderBrokerSimulator::apply_fill`'s
    /// `is_maker` argument, which the dispatch sites set from the order KIND rather than from
    /// crossing aggressiveness (that site carries the caveat in its own source). So a marketable
    /// limit counts here, and an `OrderKind::LimitClose` counts as a taker fill. Reporting the
    /// engine's own answer is the point: a second derivation would be a second opinion about a
    /// number the fee was actually charged under.
    ///
    /// Settlements are excluded — `OrderBrokerSimulator::settle_at_payout` is a venue event with no order
    /// behind it and no fee, so counting it would inflate the taker side of a mix nobody traded.
    /// `0` for every vector-kernel run.
    pub maker_fills: u64,
    /// ORDER FILLS this run booked as TAKER fills — the other half of [`Self::maker_fills`], under
    /// the identical rules. `0` for every vector-kernel run.
    pub taker_fills: u64,
}

#[derive(Debug, Clone, Default)]
pub struct TradingRecord {
    /// Who produced this record. `Default` is the simulator's own ([`TradingRecordSource::default`]).
    pub source: TradingRecordSource,
    /// Closed trades (empty when a kernel runs with `build_trades = false`).
    pub trades: Vec<Trade>,
    /// Equity after each bar.
    pub equity_curve: Vec<f64>,
    pub final_equity: f64,
    pub n_trades: usize,
    /// Multi-symbol event runs only; single-symbol / vector leave empty.
    pub per_symbol_pnl: Vec<(String, f64)>,
    /// Multi-symbol event runs only; cumulative PnL per symbol.
    pub per_symbol_curves: Vec<(String, Vec<f64>)>,
    /// Bar timestamps aligned to `equity_curve` (empty when not tracked).
    pub equity_timestamp: Vec<i64>,
    /// NET perp funding cashflow over the run (received-positive / paid-negative) — the backtest
    /// twin of `vike_execution::Account.funding_paid`, broken out of `final_equity` for observability
    /// (a funding-carry strategy's P&L is mostly funding, so seeing it separately matters). `0.0`
    /// when no funding was charged (no `Bar.funding` / no `funding_source`), for the tick path
    /// (`vike_simulator::StrategyEngine::run_ticks` does not yet accrue funding), and for a run whose
    /// funding netted to zero. Summed as `Σ MirrorFunding.amount` (`= -Σ ContractValue::funding`), the same
    /// quantity R4 parity reconciles against the live `Account`.
    pub funding_paid: f64,
    /// The TOTAL per-fill commission this run paid, in cash units (positive = paid; a
    /// rebate-bearing schedule can make a fill's contribution NEGATIVE and the sum can be too).
    ///
    /// The same quantity `OrderBrokerSimulator::fold_fill` already debits from cash, summed rather than
    /// recomputed — so a reader comparing it against a claimed fee schedule is reading what the run
    /// charged, not what a report says it should have. Distinct from
    /// [`crate::TradingRecord::funding_paid`], which is a carry cashflow and not a commission, and
    /// from `Trade::fees`, which apportions across CLOSED round-trips only and is empty when a
    /// kernel ran with `build_trades = false`. `0.0` for every vector-kernel run.
    pub fees_paid: f64,
    /// `Some(price)` when the run is a COIN LEDGER (a coin-margined contract, decision 0258): every money
    /// field above is then an amount of the contract's settle coin, and this is the contract's last
    /// price, the dollars ONE coin is worth (0258's `r`), which values them for a dollar reader. `None`
    /// for every other run, whose money is already its quote currency.
    pub settle_price: Option<f64>,
    /// What this run found wrong with its DATA and ran anyway, one sentence each: an armed
    /// `data.on_gap = "warn"` gate's findings, `data.universe = "covered"`'s, a tick symbol with no
    /// rows (the run covers the others only), a funding history that held nothing, and the engine's
    /// own: an expired option whose underlying had no price to settle it against. Never counted in the
    /// fold: the harness appends its findings after the engine returns. Empty for a clean run.
    pub data_warnings: Vec<String>,
    /// Closed-trade accounting's statistics ([`vike_trade_ledger`]), folded from every fill of the run when it
    /// armed the mode (`vike_simulator::StrategyEngineConfig::closed_trades`); `None` for every other run and every vector-kernel
    /// run. Nothing above reads it: [`Self::trades`] stays the net-position fold.
    pub closed_trades: Option<vike_trade_ledger::TradeClosedMetrics>,
    /// Every forecast the strategy published (`vike_model::OrderBroker::publish_forecast`, book W35), in
    /// publishing order, each at the step it was published at: the simulator's record, mirrored after
    /// the fold loop as [`Self::trades`] is. Empty for a strategy that publishes none and for every
    /// vector-kernel run, so such a run's record is unchanged.
    pub forecasts: Vec<ForecastRow>,
    /// `Some` when [`Self::equity_curve`] is kept by TIME ([`TradingRecordSampling`], a tick run since 2026-11-20);
    /// `None` when it holds one point a step: a bar, or every event of a tick run that asked for that.
    pub sampling: Option<TradingRecordSampling>,
    /// A PORTFOLIO backtest's mounts, one row each (`crate::results::TradingResultsMounts`): filled by the harness
    /// from the simulator's per-mount attribution after the run, `None` for every other record. Nothing above
    /// reads it: every account figure is the mounts' together.
    pub mounts: Option<crate::results::TradingResultsMounts>,
    /// What the strategy's hooks FAILED with over the run (`vike_model::Strategy::failures`, read by the engine
    /// after `on_stop`; a portfolio's mounts together). Empty for a strategy that never failed and for every
    /// record no engine run produced.
    pub strategy_failures: vike_model::StrategyFailures,
}

/// **A TICK run's curve kept by TIME** (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s
/// amendment of 2026-11-20): [`TradingRecord::equity_curve`] holds the run's first priced event and then the close of
/// each [`Self::interval_ms`] step that saw one, the last priced event last, so the return statistics read equity at
/// equal times as a bar run's do. What the closes cannot say of the path between them is folded here over EVERY event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TradingRecordSampling {
    /// The step, in ms, the curve keeps one close per; its returns are annualized at it
    /// (`crate::results::periods_per_year_for_step_ms`).
    pub interval_ms: i64,
    /// The maximum drawdown over every priced event's equity (`crate::metrics::DrawdownWalk`): the closes miss a
    /// trough between two of them.
    pub max_drawdown: f64,
    /// The highest equity of any priced event (the path's peak, which a close can miss): with [`Self::min_equity`],
    /// what a walk-forward stitching runs kept by time reads its path's drawdown across two windows from. Like
    /// every field here it lives in memory only: nothing serializes this record.
    pub max_equity: f64,
    /// The lowest equity of any priced event (the path's trough, which a close can miss).
    pub min_equity: f64,
    /// When equity first reached zero or below on any priced event, epoch ms ([`TradingRecord::equity_reached_zero`]).
    pub reached_zero_at: Option<i64>,
    /// How many priced events the run folded: the steps a curve of every event holds (the zero-trade diagnosis's
    /// count, [`crate::zero_trade::ZeroTradeReport::analyze`]).
    pub events: usize,
    /// Whether every event's equity equalled the first's: nothing ever moved the account (the zero-trade trigger).
    pub flat: bool,
}

impl TradingRecord {
    /// The simulator's counters, or `None` for a record no simulator produced.
    #[must_use]
    pub fn simulator(&self) -> Option<&TradingRecordSimulator> {
        match &self.source {
            TradingRecordSource::Backtest(simulator) => Some(simulator),
            _ => None,
        }
    }

    /// The span this record covers: its first and last [`Self::equity_timestamp`], or `None` when it
    /// kept no timestamps.
    #[must_use]
    pub fn period(&self) -> Option<TradingPeriod> {
        Some(TradingPeriod {
            from_ms: *self.equity_timestamp.first()?,
            to_ms: *self.equity_timestamp.last()?,
        })
    }

    /// **Why this run's return metrics are INVALID, or `None`**: its equity reached zero or went below at
    /// some point (the owner, 2026-11-16), so every return, ratio and probability over the curve (a Sharpe, a
    /// drawdown, a PSR) describes an account that no longer existed. The words name the first such point by
    /// its UTC day ([`Self::equity_timestamp`]), or by its index where the run kept no timestamps. Read by
    /// [`crate::results::TradingResults::from_record`] and the Studio's Validation and Distribution tabs.
    #[must_use]
    pub fn equity_reached_zero(&self) -> Option<String> {
        let day = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms).map(|t| t.format("%Y-%m-%d").to_string())
        };
        let when = match self.sampling {
            // A curve kept by time holds closes: the sampler saw every event, a dip between two closes too.
            Some(sampling) => {
                let at = sampling.reached_zero_at?;
                day(at).map_or_else(|| format!("at {at} ms"), |day| format!("on {day}"))
            }
            None => {
                let at = self.equity_curve.iter().position(|e| *e <= 0.0)?;
                let day = self.equity_timestamp.get(at).and_then(|ms| day(*ms));
                day.map_or_else(|| format!("at equity sample {at}"), |day| format!("on {day}"))
            }
        };
        Some(format!("equity reached 0 {when}: metrics are not meaningful"))
    }

    /// The maximum drawdown of the run's PATH: [`TradingRecordSampling::max_drawdown`] when the curve is kept by time,
    /// else the curve's own (`crate::metrics::max_drawdown`).
    #[must_use]
    pub fn max_drawdown(&self) -> f64 {
        self.sampling
            .map_or_else(|| crate::metrics::max_drawdown(&self.equity_curve), |s| s.max_drawdown)
    }

    /// The annualization of the curve's returns: its own step's when it is kept by time
    /// ([`TradingRecordSampling::interval_ms`]), else the caller's `periods_per_year` (one point a bar).
    #[must_use]
    pub fn periods_per_year(&self, periods_per_year: f64) -> f64 {
        self.sampling.map_or(periods_per_year, |s| {
            crate::results::periods_per_year_for_step_ms(s.interval_ms)
        })
    }
}
