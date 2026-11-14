//! The one backtest result type. Unifies the former `RunResult` / `MultiSymbolResult` /
//! `FastResult` / `PortfolioFastResult` — a flat superset where each producer fills what it has and
//! leaves the rest empty/0 (decision A in the engine-unification spec).

use vike_model::{OrderDrop, Trade};

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
    pub equity_timestamp: Vec<i64>,
    /// How many times the opt-in stale-price wait discipline
    /// (`vike_simulator::StrategyEngineConfig::max_price_staleness_ms`) DEFERRED a market order instead of
    /// filling it against stale data. Always `0` when that knob is `None` (the default) and for
    /// every vector-kernel run. Carried on the result — not just on the engine — so the
    /// `harness`/`backtest`-bin entry points, which drop the engine, can still tell "zero trades
    /// because the tape never printed" apart from "zero trades because the strategy never
    /// traded". See `vike_fills::staleness`.
    pub stale_deferrals: u64,
    /// Fills at which a CONFIGURED impact model (`[engine.slippage]`'s `market_impact`) charged
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
    /// NET perp funding cashflow over the run (received-positive / paid-negative) — the backtest
    /// twin of `vike_execution::Account.funding_paid`, broken out of `final_equity` for observability
    /// (a funding-carry strategy's P&L is mostly funding, so seeing it separately matters). `0.0`
    /// when no funding was charged (no `Bar.funding` / no `funding_source`), for the tick path
    /// (`vike_simulator::StrategyEngine::run_ticks` does not yet accrue funding), and for a run whose
    /// funding netted to zero. Summed as `Σ MirrorFunding.amount` (`= -Σ ContractValue::funding`), the same
    /// quantity R4 parity reconciles against the live `Account`.
    pub funding_paid: f64,
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
    /// The TOTAL per-fill commission this run paid, in cash units (positive = paid; a
    /// rebate-bearing schedule can make a fill's contribution NEGATIVE and the sum can be too).
    ///
    /// The same quantity `OrderBrokerSimulator::fold_fill` already debits from cash, summed rather than
    /// recomputed — so a reader comparing it against a claimed fee schedule is reading what the run
    /// charged, not what a report says it should have. Distinct from
    /// [`crate::BacktestResult::funding_paid`], which is a carry cashflow and not a commission, and
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
    /// rows (the run covers the others only), and a funding history that held nothing. Never counted
    /// in the fold: the harness writes it after the engine returns. Empty for a clean run.
    pub data_warnings: Vec<String>,
    /// Closed-trade accounting's statistics ([`crate::closed_trades`]), folded from every fill of the run when it
    /// armed the mode (`vike_simulator::StrategyEngineConfig::closed_trades`); `None` for every other run and every vector-kernel
    /// run. Nothing above reads it: [`Self::trades`] stays the net-position fold.
    pub closed_trades: Option<crate::closed_trades::TradeClosedMetrics>,
}

/// One out-of-sample window's outcome.
///
/// `Serialize`, so the compute-to-data `RunWalkforwardProfile` verb can return the report as JSON
/// TEXT, exactly as `BacktestReport`/`ParamscanReport` already cross the datahub wire. And
/// `Deserialize` since 2026-11-11: the Studio's `RunWalkforward` answer
/// (`vike_backtesthub_client::wire_studio::WalkforwardReportWire`) carries these windows AS
/// THEMSELVES, where it carried a field-for-field mirror with the same bytes. A field added here is
/// therefore a field of that wire, and `crates/vike-backtesthub-client/tests/backtest_plane_wire_pin.rs`
/// fails until its frozen line says so.
/// ⚠ NOT `Copy` any more. It became a window that can carry the parameters its own search chose,
/// and an owned `Vec` cannot be `Copy`. Every consumer already took `&WalkforwardWindow`, so the loss costs
/// nothing today — but a future caller that expected an implicit copy gets a move instead.
///
/// Here, beside [`BacktestResult`], since 2026-11-10: the walk-forward fold that produces it is
/// `vike_backtest::walkforward`'s, and the desktop's Studio renders it without linking that harness
/// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s amendment of that day).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WalkforwardWindow {
    /// The `[start, end)` bar-index range of this OOS window.
    pub test_range: (usize, usize),
    /// The window's out-of-sample return (a fraction, e.g. `0.05` = +5%).
    pub out_of_sample_return: f64,
    /// The parameter overrides this window SELECTED, when the caller searched inside it — `None`
    /// on the fixed-parameter walk, which is every caller that does not optimize.
    ///
    /// This field is the whole diagnostic value of a walk-forward OPTIMIZATION and the reason the
    /// report grew rather than forking: a stitched OOS number tells you the procedure's result,
    /// while the per-window winners tell you whether the procedure found anything STABLE or
    /// wandered. Our own cohort study is the cautionary case — per-fold in-sample↔out-of-sample
    /// R² came out ≈ 0.01, which is only visible if each fold's choice was recorded.
    ///
    /// ⚠ `default` + `skip_serializing_if` are load-bearing on the Studio wire: a fixed walk's frame
    /// stays byte-identical to the pre-field one, and an OLD server's frame still decodes for a NEW
    /// client (decision 0112, verdict 3).
    ///
    /// # Why each value crosses as a `ParamValue`, under the key it always had
    ///
    /// The client reads back the kind the strategy was handed, with no text round trip. It used to
    /// cross as its rendered TOML text (`"8"`, `"1.5"`), parsed back on the client.
    ///
    /// ⚠ **That retyped a field IN PLACE, and decision 0112 calls a changed field type a change an
    /// old peer cannot decode.** The field is populated ONLY on a SEARCHING walk
    /// (`vike_backtesthub_client::wire_studio::WalkforwardWire::search`), and no client this
    /// workspace ships asks for one: the Studio sends `WalkforwardWire::fixed` alone, so its windows
    /// carry `None`, which is absent on the wire and decodes the same under either type. A
    /// third-party client that DID send a search would fail to decode a new server's searching
    /// answer (a number where it expected a string), and a new client searching against an older
    /// server would read each rendered value as a `ParamValue::Str`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_params: Option<Vec<(String, vike_model::params::ParamValue)>>,
}

/// Stitched walk-forward result. `Serialize`-only: it crosses as JSON TEXT (`RunWalkforwardProfile`,
/// [`WalkforwardWindow`]'s doc), and the Studio wire carries it in a type of its own
/// (`vike_backtesthub_client::wire_studio::WalkforwardReportWire`, another key order and other skip
/// rules) holding these same windows.
#[derive(Clone, Debug, serde::Serialize)]
pub struct WalkforwardReport {
    pub windows: Vec<WalkforwardWindow>,
    pub out_of_sample_equity_curve: Vec<f64>,
    pub out_of_sample_return: f64,
    pub out_of_sample_sharpe: f64,
    /// Fraction of windows profitable out-of-sample (the `overfit_verdict` input).
    pub walkforward_consistency: f64,
    /// NET perp funding over the out-of-sample windows (received-positive), the sum of each window's
    /// [`BacktestResult::funding_paid`] as run fresh at `cash` (summed, NOT rebased like the stitched
    /// curve). A search's training runs are in no number here.
    pub funding_paid: f64,
    /// The currency the run's money is in WHEN IT IS NOT THE DOLLAR (`JPY` for a profile that stated
    /// `engine.account_currency = "JPY"`): the equity curve here is an amount of it, and `out_of_sample_return` and
    /// `out_of_sample_sharpe` are ratios and need no unit. `None` for the dollar, which is also what every
    /// producer but the profile-driven walk (`vike_backtest::walkforward::runner::run_walkforward`)
    /// leaves: a dollar report is byte-identical to one written before this field existed, because
    /// `None` is not serialized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_currency: Option<String>,
    /// What the walk found wrong with its data and ran anyway ([`BacktestResult`]'s field of the same
    /// name). Empty, and so not serialized, for every clean walk; the fold leaves it empty and its
    /// drivers fill it (the profile runner, the Studio's slice walk).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub data_warnings: Vec<String>,
    /// The strategy file the walk ran and the sha256 of its text as read
    /// ([`crate::report::BacktestReport`]'s two fields of the same names). Empty, and so not
    /// serialized, where none was recorded; the fold leaves them empty and the profile runner fills
    /// them.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub strategy_file: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub strategy_sha256: String,
}
