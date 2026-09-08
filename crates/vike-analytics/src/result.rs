//! The one backtest result type. Unifies the former `RunResult` / `MultiSymbolResult` /
//! `FastResult` / `PortfolioFastResult` — a flat superset where each producer fills what it has and
//! leaves the rest empty/0 (decision A in the engine-unification spec).

use vike_model::Trade;

#[derive(Debug, Clone, Default)]
pub struct BacktestResult {
    /// Closed trades (empty when a kernel runs with `build_trades = false`).
    pub trades: Vec<Trade>,
    /// Equity after each bar.
    pub equity_curve: Vec<f64>,
    pub final_equity: f64,
    pub n_trades: usize,
    /// Event engines only (SL+TP both-hit count); kernels leave 0.
    pub intrabar_both_hit: u32,
    /// Multi-symbol event runs only; single-symbol / vector leave empty.
    pub per_symbol_pnl: Vec<(String, f64)>,
    /// Multi-symbol event runs only; cumulative PnL per symbol.
    pub per_symbol_curves: Vec<(String, Vec<f64>)>,
    /// Bar timestamps aligned to `equity_curve` (empty when not tracked).
    pub equity_ts: Vec<i64>,
    /// How many times the opt-in stale-price wait discipline
    /// (`vike_backtest::EngineParams::max_price_staleness_ms`) DEFERRED a market order instead of
    /// filling it against stale data. Always `0` when that knob is `None` (the default) and for
    /// every vector-kernel run. Carried on the result — not just on the engine — so the
    /// `harness`/`backtest`-bin entry points, which drop the engine, can still tell "zero trades
    /// because the tape never printed" apart from "zero trades because the strategy never
    /// traded". See `vike_backtest::staleness`.
    pub stale_deferrals: u64,
    /// Fills at which a CONFIGURED `[engine.impact]` model charged nothing, because the market
    /// context it needs (`avg_volume`, `sigma`) could not be measured from the series.
    ///
    /// The mirror of `vike_backtest::SimBroker::impact_unpriced`, and it exists because the skip is
    /// otherwise invisible: the engine falls back to the flat slippage, so a run configured with an
    /// impact model comes back priced without one — no error, no warning, and a number
    /// indistinguishable from a priced one.
    ///
    /// ⚠ The usual cause is the DATA, not the config: the context folds through a mean traded size,
    /// so a bar series carrying `volume = 0` disables impact at every coefficient. A non-zero count
    /// is not automatically a fault — the opening fills of a run legitimately precede a measurable
    /// window — but a count equal to the fill count means impact was never applied at all.
    pub impact_unpriced: u64,
    /// How many times the opt-in session gate (`vike_backtest::EngineParams::session_gate`) refused a
    /// fill because the venue was CLOSED — resting orders deferred plus tagged-maker and armed
    /// protective-stop passes skipped. Always `0` when the gate is off (the default) and for every
    /// vector-kernel run. Carried on the result for the same reason as `stale_deferrals`: the
    /// `harness`/`backtest`-bin entry points drop the engine, and "few trades because the venue
    /// was shut most of the tape" must stay distinguishable from "the strategy never traded".
    /// See [`vike_model::session`].
    pub session_deferrals: u64,
    /// The engine's gate-drop diagnostics channel (`vike_backtest::SimBroker::dropped`) mirrored off the
    /// engine at end-of-run: `(symbol, reason, size, weight)` for every order a gate REFUSED — the
    /// live `RiskGate`'s own reason strings (`"insufficient-margin"`, `"below-min-qty"`, …),
    /// order-kind cash-gate drops, `"volume_cap"`, and `"latency_reject"`. Empty for every
    /// vector-kernel run and for any run where nothing was dropped. Carried on the result — a single
    /// `clone` AFTER the fold loop, NOT new hot-path counting — for the same reason as the two
    /// deferral counters above: the `harness`/`backtest`-bin entry points drop the engine, and
    /// "zero trades because every order was denied" must stay distinguishable from "the strategy
    /// never traded". Consumed by [`crate::zero_trade`].
    pub dropped: Vec<(String, String, f64, f64)>,
    /// ⚠ **Fills this backtest executed that the LIVE gate would have DENIED** — a MEASUREMENT of
    /// the one known live-vs-backtest divergence still open, not a refusal channel. Carried the same
    /// way as the counters above: cloned after the fold loop, never counted on the hot path.
    ///
    /// The condition is a below-min REVERSAL — the order opposes the position, EXCEEDS it (so it
    /// flips through flat and `is_covered_reduce` is false), and falls below a venue floor.
    /// `SimBroker::apply_fill`'s opening/closing split is direction-ONLY, so it calls the whole
    /// thing closing and executes it entire; the live `RiskGate` requires coverage and refuses.
    ///
    /// ⚠ **It is expected to stay ZERO above dust sizes.** A flip must exceed the position, so the
    /// condition needs `|position| < flip < min_qty` — the position must already be below the floor,
    /// which an opening order cannot produce. Against the real venue grids in
    /// `crates/vike-mount/src/fallback.rs` (binance/bybit `min_notional` 5.0, okx 1.0) that means
    /// reversing a position worth **under ~$5**. Any strategy sizing above a few dollars per order
    /// cannot reach it.
    ///
    /// The counter exists so that reasoning is FALSIFIABLE rather than asserted: a non-zero reading
    /// on a real profile means the argument above is wrong. It is not a refusal channel, and it does
    /// not need fixing — the fix would change fill decisions and, worse, the obvious repair does not
    /// converge the two engines (live denies the whole order; splitting the flip makes the backtest
    /// flatten). `SimBroker::below_min_reversals` is the source; see `crates/vike-exec/src/risk.rs`'s
    /// "KNOWN RESIDUAL DIVERGENCE".
    pub below_min_reversals: u64,
    /// The strategy's warm-up requirement (`Strategy::warmup()`, in bars/ticks) captured once at
    /// end-of-run. `0` for a strategy with no warm-up (the default) and for every vector-kernel run.
    /// Carried so a zero-trade run can tell "the strategy stayed in warm-up the whole window
    /// (lookback > bars)" apart from a strategy that ran but never traded. Consumed by
    /// [`crate::zero_trade`].
    pub warmup: usize,
    /// NET perp funding cashflow over the run (received-positive / paid-negative) — the backtest
    /// twin of `vike_exec::Account.funding_paid`, broken out of `final_equity` for observability
    /// (a funding-carry strategy's P&L is mostly funding, so seeing it separately matters). `0.0`
    /// when no funding was charged (no `Bar.funding` / no `funding_source`), for the tick path
    /// (`vike_backtest::StrategyEngine::run_ticks` does not yet accrue funding), and for a run whose
    /// funding netted to zero. Summed as `Σ MirrorFunding.amount` (`= -Σ funding_charge`), the same
    /// quantity R4 parity reconciles against the live `Account`.
    pub funding_paid: f64,
}
