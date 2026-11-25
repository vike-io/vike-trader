//! **Every refusal of a backtest profile, as DATA**: one typed variant per refusal, each with its stable
//! id and its ONE text (`vike_utility::refusal_enum!`, `crates/vike-utility/src/errors.rs`).
//!
//! [`crate::HarnessError::Validation`] carries a [`ProfileRefusal`], so nothing refuses a profile without a
//! variant here, and the published schema (`profile_surface`, feature `profile-schema`) lists every refusal by
//! walking [`ProfileRefusalId::ALL`] rather than by reading source text. A check names its variant; the text is
//! for a person and the id is what code and a reader decide on (decision 0301).
//!
//! Each text is the sentence the check printed before it moved here, byte for byte. A hole names the variant's
//! field (the compiler checks each against the fields) or a const in scope here (`{COMMISSION_NOW}`).
//!
//! The variants under "carriers" forward a text ANOTHER surface owns (vike-fills' cost grammar, the strategy
//! file's own build, the sandbox, vike-analytics' bar permutation): they carry that text unchanged and are not
//! rows of the profile's published refusals (`profile_surface`'s `raised_at` says so per id). Those marked
//! `carries refusal` show the forwarded refusal's id when it has one (`vike_utility::errors`'s module doc,
//! "A carrier shows ONE id").
//!
//! An id is part of the published contract: keep it when the text is reworded, and add a variant rather than
//! repurpose one.

use std::fmt;

use vike_backtesthub_client::summary::PARAMSCAN_POINTS_MAX;
use vike_model::run_request::decide::DecideMode;

use crate::run::costs::{COMMISSION_NOW, SLIPPAGE_NOW};
use crate::run::profile::MAX_SIZER_DEPTH;
use crate::search::euler::MAX_AXES;
use crate::walkforward::combinatorial::MAX_OUT_OF_SAMPLE_RUNS;

/// A roster's words joined by ` | `, as a refusal's "want" list names them: rendered from the roster itself,
/// so the refusal cannot fall behind it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WordList(&'static [&'static str]);

impl fmt::Display for WordList {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.join(" | "))
    }
}

/// `engine.decide`'s words, [`DecideMode::WORDS`].
pub(crate) const DECIDE_WORDS: WordList = WordList(&DecideMode::WORDS);

vike_utility::refusal_enum! {
    /// A refused backtest profile: one variant per refusal, its id and its one text.
    #[derive(Debug, Clone, PartialEq)]
    pub enum ProfileRefusal with ids ProfileRefusalId {
        EngineCashNotPositive { cash: f64 } => "profile.engine.cash.not_positive":
            "engine.cash must be > 0, got {cash}",
        RiskMaximumOrdersPerWindowWallClock =>
            "profile.risk.max_orders_per_window.wall_clock_throttle":
            "risk.max_orders_per_window is a wall-clock order-rate throttle — meaningless \
             in backtest sim time (a replayed month can execute in under a second, which \
             would spuriously rate-limit every order; OrderBrokerSimulator::build_risk_gate always \
             disarms this field for the identical reason on the live-parity gate it \
             mounts). Remove it from `[risk]` — every other risk.* limit is honored",
        EngineMarginCallBarModeOnly => "profile.engine.margin_call.bar_mode_only":
            "engine.margin_call is bar-mode only: the call is judged at a bar's open and \
             extremes, which a tick does not have, and run_ticks refuses it",
        EngineMarginCallWithLiquidation => "profile.engine.margin_call.with_liquidation":
            "engine.margin_call replaces the engine.maint_margin liquidation (and its \
             venue_style_liquidation), so a profile states one of the two: remove \
             maint_margin and venue_style_liquidation, or the [engine.margin_call] table",
        EngineWholeEntryFeeNeedsClosedTrades =>
            "profile.engine.whole_entry_fee.needs_closed_trades":
            "engine.whole_entry_fee books the closed-trade block's entry commission, and \
             engine.closed_trades = false leaves that block out: turn closed_trades on, or \
             remove whole_entry_fee",
        EngineFeedLatencyTickModeOnly => "profile.engine.feed_latency.tick_mode_only":
            "engine.feed_latency is tick-mode only: it re-orders recorded ticks by their \
             machine receive stamp (local_timestamp), which a bar series does not carry",
        EngineQueueModelTickModeOnly => "profile.engine.queue_model.tick_mode_only":
            "engine.queue_model is tick-mode only: the FIFO queue lane runs in the tick/book \
             replay path, which a bar series does not drive",
        EngineEquitySampleEveryTickModeOnly =>
            "profile.engine.equity_sample_every.tick_mode_only":
            "engine.equity_sample_every is tick-mode only: the bar lane records one equity \
             sample per BAR (bounded by the bar count), so there is nothing to thin",
        EngineCashGateBarModeOnly => "profile.engine.cash_gate.bar_mode_only":
            "engine.cash_gate is bar-mode only: the shared-cash admission phase is \
             StrategyEngine::run's own bar step (fill_step_gated), and run_ticks never \
             consults it — a tick profile setting it would change nothing",
        EngineInBarBarModeOnly => "profile.engine.in_bar.bar_mode_only":
            "[engine.in_bar] is bar-mode only: its rules decide how a BAR's walk meets the \
             resting orders, and run_ticks reads none of them — a tick profile setting one \
             would change nothing",
        EngineInBarWalkWithSharedCash => "profile.engine.in_bar.walk_with_shared_cash":
            "engine.in_bar.path_walk, price_order and committed_stops do not go with \
             engine.cash_gate or engine.decide = \"simultaneous\": the shared-cash admission \
             phase fills each bar whole in fill_step_gated and walks no path, so these rules \
             would be set and ignored",
        EngineDecideSimultaneousBarModeOnly =>
            "profile.engine.decide.simultaneous_bar_mode_only":
            "engine.decide = \"simultaneous\" is bar-mode only: the cross-sectional \
             allocator it orders is StrategyEngine::fill_step_gated, reached only from \
             StrategyEngine::run — run_ticks consults neither it nor cash_gate, so a tick \
             profile setting it would change nothing",
        EngineDecideSimultaneousWithRiskGate =>
            "profile.engine.decide.simultaneous_with_risk_gate":
            "engine.decide = \"simultaneous\" is not built for a run with a pre-trade \
             risk gate: remove [risk] and engine.leverage, or keep engine.decide \
             unset. With a gate armed, OrderBrokerSimulator::gate_order folds the pending set as it \
             ACCUMULATES during the on_bar fan-out, so the symbol judged first faces the \
             whole step budget and later ones face the remainder — the DECISION is still \
             taken in symbol-list order and no fill-phase ranking can repair it. Closing \
             it needs a step-boundary budget snapshot, which does not exist",
        EngineDecideSimultaneousWithDetailInterval =>
            "profile.engine.decide.simultaneous_with_detail_interval":
            "data.detail_interval and engine.decide = \"simultaneous\" are mutually \
             exclusive: the simultaneous mode implies the shared-cash admission phase, \
             which routes the whole bar step through fill_step_gated and disables the \
             granular sub-bar lane — the detail tape would be loaded, paid for and ignored",
        EngineTimeframesBarModeOnly => "profile.engine.timeframes.bar_mode_only":
            "engine.timeframes is bar-mode only: the coarse series are resampled from the \
             base BAR stream, and a tick profile has none",
        DataIntervalInvalidForTimeframes { interval: String } =>
            "profile.data.interval.invalid_for_timeframes":
            "data.interval {interval:?} is not a valid interval, so engine.timeframes cannot be \
             checked against it",
        EngineTimeframesZeroLength { tf: String, ms: i64 } =>
            "profile.engine.timeframes.zero_length":
            "engine.timeframes {tf:?} resolves to {ms}ms — a zero-length window makes \
             every boundary the same instant",
        EngineTimeframesNotAnInterval { tf: String } =>
            "profile.engine.timeframes.not_an_interval":
            "engine.timeframes {tf:?} is not a valid interval (want a count then one \
             of s/m/h/d, e.g. \"4h\")",
        EngineTimeframesNotCoarser { tf: String, ms: i64, interval: String, base: i64 } =>
            "profile.engine.timeframes.not_coarser":
            "engine.timeframes {tf:?} ({ms}ms) must be coarser than data.interval {interval:?} \
             ({base}ms) — a finer window cannot be synthesised from the base stream \
             and would return the base bars re-labelled",
        EngineMaintenanceMarginNegative { maint_margin: f64 } =>
            "profile.engine.maint_margin.negative":
            "engine.maint_margin must be >= 0, got {maint_margin} — 0 means the liquidation watchdog is \
             off, and a negative rate has no meaning",
        EngineLiquidationBufferNegative { liquidation_buffer: f64 } =>
            "profile.engine.liquidation_buffer.negative":
            "engine.liquidation_buffer must be >= 0, got {liquidation_buffer}",
        EngineVolumeLimitOutOfRange { v: f64 } => "profile.engine.volume_limit.out_of_range":
            "engine.volume_limit must be in (0, 1], got {v} — it is the fraction of the \
             EVENT's own volume one fill may take (a bar's volume in bar mode, ONE PRINT's \
             in tick mode), so 1.0 is all of it",
        EngineMultiplierNotPositive { multiplier: f64 } =>
            "profile.engine.multiplier.not_positive":
            "engine.multiplier must be > 0, got {multiplier} — it scales every position's notional, so \
             zero makes every trade worth nothing",
        EngineMultipliersNotPositive { symbol: String, m: f64 } =>
            "profile.engine.multipliers.not_positive":
            "engine.multipliers.{symbol} must be > 0, got {m}",
        EngineMultipliersNotLoaded { symbol: String, known: String } =>
            "profile.engine.multipliers.symbol_not_loaded":
            "engine.multipliers.{symbol} names a symbol this run does not load — the \
             engine matches a multiplier row by EXACT symbol name and otherwise falls \
             back to the global engine.multiplier, so this row would change nothing. \
             The data slice is: {known}",
        /// A stated multiplier (`engine.multipliers` row or the global `engine.multiplier`) that is not the catalog's unit.
        EngineMultiplierWrongUnit { key: String, symbol: String, venue: String, stated: f64, per_unit: f64 } =>
            "profile.engine.multiplier.not_the_catalog_unit":
            "{key} sizes {symbol} on {venue} at {stated}, and the catalog values one unit of it at {per_unit} \
             (the unit a paper book and the venue's live mount book it by), so this run would \
             book every notional, margin, fee and PnL of it at {stated} where live books {per_unit}. \
             Drop its engine.multipliers row (or the engine.multiplier it falls back to), or \
             state {per_unit}",
        EngineRiskFreeRateInvalid { r: f64 } => "profile.engine.risk_free_rate.invalid":
            "engine.risk_free_rate must be a finite yearly fraction above -1 (0.02 is 2%), got {r}",
        EngineLeverageNotPositive { l: f64 } => "profile.engine.leverage.not_positive":
            "engine.leverage must be > 0, got {l} — omit the key for unlevered",
        EngineMountLeverageCapInvalid { cap: f64 } => "profile.engine.max_mount_leverage.invalid":
            "engine.max_mount_leverage must be a finite figure of at least 1 (the mount's gross \
             notional over its sizing equity), got {cap}: omit the key for no cap",
        EngineClampToLeverageWithRisk => "profile.engine.clamp_to_leverage.with_risk":
            "engine.clamp_to_leverage is unavailable to a profile carrying [risk]: \
             OrderBrokerSimulator::build_risk_gate returns None on clamp_to_leverage BEFORE it reads \
             risk_limits, so the clamp disarms the pre-trade gate entirely and EVERY [risk] \
             limit — not just max_leverage — is discarded. Drop clamp_to_leverage to keep \
             [risk]'s gate, or remove [risk] and clamp against engine.leverage alone",
        EngineClampToLeverageNeedsLeverage => "profile.engine.clamp_to_leverage.needs_leverage":
            "engine.clamp_to_leverage needs engine.leverage: there is nothing to clamp to",
        EngineLeverageWithRisk => "profile.engine.leverage.with_risk":
            "engine.leverage and [risk] are two different margin sources for the same gate — \
             set exactly one. With clamp_to_leverage off (the default) there are three \
             configurations and [risk] wins all of them: OrderBrokerSimulator::build_risk_gate's \
             (Some(l), _) arm takes risk_limits unconditionally, so a [risk] that sets \
             max_leverage answers with ITS number, a [risk] that does not sets no margin \
             requirement at all, and either way engine.leverage's 1/L initial_margin_requirement is \
             silently discarded. (With clamp_to_leverage on, neither reaches a gate — that \
             combination is refused separately.) Set risk.max_leverage instead, or remove \
             [risk] to use engine.leverage's 1/L margin mapping",
        EngineSettlementPeriodNotPositive { ms: i64 } =>
            "profile.engine.settlement_period_ms.not_positive":
            "engine.settlement_period_ms must be > 0, got {ms} — omit the key to never \
             settle",
        WalkforwardSplitsBelowOne => "profile.walkforward.n_splits_below_one": "walkforward.n_splits must be >= 1, got 0",
        WalkforwardSearchNeedsParamscan => "profile.walkforward.search.needs_paramscan":
            "walkforward.search needs a [paramscan] table to search — this \
             profile has none, so every window would 'select' the one parameter set it \
             already has. Add the grid, or drop the key for the no-search control.",
        PermutationPermutationsBelowOne => "profile.permutation.permutations.below_one":
            "permutation.permutations must be >= 1, got 0 — a permutation test with no \
             permuted history has no p-value",
        PermutationWithWalkforward => "profile.permutation.with_walkforward":
            "[permutation] and [walkforward] are two programs for one run, and a \
             permutation test of a walk-forward is not built — run them as two profiles",
        PermutationTrainWithoutParamscan => "profile.permutation.train.without_paramscan":
            "permutation.train = true re-runs the [paramscan] grid on every history, and \
             this profile has none. Add the grid, or drop train for the plain test.",
        PermutationParamscanWithoutTrain => "profile.permutation.train.paramscan_without_train":
            "a [paramscan] grid beside [permutation] is searched only by permutation \
             training — set permutation.train = true, or drop the grid for the plain test",
        DataOnGapWithoutRequireCoverage => "profile.data.on_gap.without_require_coverage":
            "data.on_gap is set without data.require_coverage = true, so the gate it \
             configures is not armed and this key is read by nothing — while it reads as \
             though a run over an uncovered window would be stopped. Arm the gate, or remove \
             the disposition",
        DataMaxGapWithoutRequireCoverage => "profile.data.max_gap.without_require_coverage":
            "data.max_gap is set without data.require_coverage = true, so the gate whose \
             tolerance it names is not armed and this key is read by nothing. Arm the gate, or \
             remove the tolerance",
        DataDetailIntervalWithCashGate => "profile.data.detail_interval.with_cash_gate":
            "data.detail_interval and engine.cash_gate are mutually exclusive: \
             cash_gate routes the whole bar step through the gated fill lane and \
             UNCONDITIONALLY disables the granular sub-bar lane, so the detail tape would \
             be loaded, scanned and then ignored while every order was reported as \
             resolved against it. Set one.",
        DataDetailIntervalWithWalkforward => "profile.data.detail_interval.with_walkforward":
            "data.detail_interval is unavailable to a [walkforward] profile: the walk \
             hands each window a sliced coarse series while nothing slices the detail \
             tape, so every window's last step would fill against bars from AFTER the \
             window ended. That is look-ahead, not merely waste. Run the profile without \
             [walkforward], or without the detail tape.",
        /// An `[inputs]` row whose name or read is not in the input grammar (decision 0271); `e` is the grammar's own refusal.
        InputsRowUnparsed { name: String, read: String, e: String } => "profile.inputs.row_unparsed":
            "[inputs] {name:?} = {read:?}: {e}",
        DataOptionsLedgerNotDollar => "profile.data.options.ledger_not_dollar":
            "[data.options] books its legs at the chain's marks, which are dollars, so the run's \
             ledger must be the dollar: drop engine.account_currency or set it to \"USD\"",
        StrategySdkWithRhaiFile { sdk: u32, file: String } => "profile.strategy.sdk.with_rhai_file":
            "strategy.sdk = {sdk} names a Rust strategy SDK, and strategy.file {file:?} is a .rhai \
             file, which runs on this vike's own code: remove strategy.sdk",
        DataFromAfterTo { start: i64, end: i64 } => "profile.data.from.after_to":
            "data.from ({start}) must be <= data.to ({end})",
        DataTwoSliceForms => "profile.data.two_slice_forms":
            "data.venue/data.symbols and [[data.series]] are two ways to name the SAME \
             slice — set exactly one (the [[data.series]] form is the cross-venue one)",
        DataSymbolsEmptyWithoutSeries => "profile.data.symbols.empty_without_series":
            "data.symbols must not be empty (or use the cross-venue [[data.series]] form)",
        DataVenueMissing => "profile.data.venue.missing":
            "data.venue is required alongside data.symbols",
        DataSymbolsEmpty => "profile.data.symbols.empty": "data.symbols must not be empty",
        DataSeriesForeignAccount { symbol: String, account: String, venue: String } =>
            "profile.data.series.account_of_another_venue":
            "data.series[{symbol:?}].account = {account:?} must be an account of the \
             series' own venue {venue:?} (`{venue}/<LABEL>`, `{venue}/DEFAULT` or `{venue}`): the series is \
             then the leg `SYMBOL@account` an order is placed on",
        DataDuplicateSymbol { leg: String } => "profile.data.duplicate_symbol":
            "duplicate symbol {leg:?} in the data slice — the engine routes ticks to a \
             symbol slot BY NAME, so two series cannot share one symbol; one instrument on \
             two venues marks the series of the other venue by its account \
             (`account = \"<venue>/<LABEL>\"`, the leg `SYMBOL@<venue>/<LABEL>`)",
        DataSeriesKindTickModeOnly { symbol: String, kind: crate::run::tick_replay::SeriesKind } =>
            "profile.data.series.kind_tick_mode_only":
            "data.series[{symbol:?}].kind = {kind:?} is tick-mode only: a bar series has no \
             quote/trade/book lanes to filter",
        EngineSnapToPropertiesCrossVenue => "profile.engine.snap_to_properties.cross_venue":
            "engine.snap_to_properties is single-venue only: the point-in-time instrument \
             grid is looked up under one default_venue, so a cross-venue [[data.series]] \
             slice would snap every series to the first series' venue",

        DataOptionsUnderlyingIntervalInvalid { raw: String } =>
            "profile.data.options.underlying_interval_invalid":
            "[data.options] underlying_interval {raw:?} is not an interval the store keeps bars \
             of (want a count then one of s/m/h/d, e.g. \"1h\")",
        DataFileFormatWithoutFile => "profile.data.file_format.without_file":
            "data.file_format is set without data.file, so the form it describes is read by \
             nothing: name the file, or remove the format",
        DataSourceAndFileBothSet => "profile.data.source_and_file_both_set":
            "data.source and data.file both name where the bars come from — set one: a \
             lane of the store, or a file read without importing it",
        /// `vike_data::store::store_kind::refuse_an_unknown_lane`'s sentence, prefixed with the key.
        DataSourceLaneUnknown { e: String } => "profile.data.source.lane_unknown":
            "data.source: {e}",
        DataFileEmpty => "profile.data.file.empty":
            "data.file is empty: name the CSV file whose bars this run reads",
        DataSourceAndFileBarModeOnly => "profile.data.source_and_file_bar_mode_only":
            "data.source and data.file are bar-mode only: a lane or a file is read as bars, \
             and the tick replay reads the venue's own tape — set data.kind = \"bar\"",
        DataFileSeriesNotOne { series: usize } => "profile.data.file.series_not_one":
            "data.file holds ONE series and this profile names {series}: name one symbol",
        DataFileDetailIntervalUnavailable => "profile.data.file.detail_interval_unavailable":
            "data.file holds one interval, so data.detail_interval has no finer series to \
             read: drop it, or import both files and read their lanes with data.source",
        DataFileNotInStore => "profile.data.file.not_in_store":
            "data.file is not in the store, so data.require_coverage, data.universe and \
             data.explain, which plan over the store, would judge a series this run does \
             not read: drop them, or import the file (`csv-bars`) and read its lane with \
             data.source",
        /// `vike_data::csv_bars::CsvBarsFormat::parse`'s sentence, prefixed with the key.
        DataFileFormatInvalid { e: String } => "profile.data.file_format.invalid":
            "data.file_format: {e}",
        DataOptionsTickModeOnly => "profile.data.options.tick_mode_only":
            "[data.options] is tick-mode only: an option leg has a price only in the hours its \
             chain holds a row of it, and the bar lane needs one row per symbol per step, so it \
             would trade a leg at a price the chain never held. Set data.kind = \"tick\"",
        DataOptionsVenueAndChainMissing => "profile.data.options.venue_and_chain_missing":
            "[data.options] needs venue and chain: the stored chain series the legs are read \
             from (kind=chain/venue=<venue>/symbol=<chain>)",
        DataOptionsLegsEmpty => "profile.data.options.legs_empty":
            "[data.options] legs must not be empty: name each option instrument to trade as the \
             chain names it",
        DataOptionsLegEmptyOrRepeated { leg: String } => "profile.data.options.leg_empty_or_repeated":
            "[data.options] leg {leg:?} is empty or repeated: each leg is one symbol slot of \
             the run, which routes events by name",
        DataOptionsLegIsRunSeries { leg: String } => "profile.data.options.leg_is_run_series":
            "[data.options] leg {leg:?} is also a series of the run: a symbol has one slot, \
             so a leg is priced by its chain alone",
        DataOptionsUnderlyingNotRunSeries { underlying: String } =>
            "profile.data.options.underlying_not_run_series":
            "[data.options] underlying {underlying:?} is not a series of this run: the legs settle at \
             expiry against its price, so add it to data.symbols or [[data.series]]",
        DataOptionsAssumedHalfSpreadOutOfRange { half: f64 } =>
            "profile.data.options.assumed_half_spread_out_of_range":
            "[data.options] assumed_half_spread {half} is not a fraction of the mark above 0 and below 1: \
             0.05 sells a leg at 95% of its mark and buys it at 105%, and 0 would be the bare mark \
             this key exists to refuse",
        DataHeardStreamsTickModeOnly => "profile.data.heard_streams_tick_mode_only":
            "data.reference, data.mark and data.feed_status are tick-mode only: a bar run \
             hears its bars, and these are heard between ticks",
        DataReferenceOnOwnVenue { symbol: String, venue: String } =>
            "profile.data.reference.on_own_venue":
            "data.reference {symbol:?} on {venue:?} is on the run's own venue: a reference is another \
             venue's touch; list a series of the run's venue in the data slice instead",
        DataReferenceKindTrade { symbol: String } => "profile.data.reference.kind_trade":
            "data.reference {symbol:?}: kind = \"trade\" hears nothing, since a reference is heard \
             as its touch: use \"quote\", \"book\" or \"tick\"",
        DataReferenceListedTwice { symbol: String, venue: String } =>
            "profile.data.reference.listed_twice":
            "data.reference {symbol:?} on {venue:?} is listed twice",
        /// `vike_model::time::parse_span`'s sentence, prefixed with the key.
        DataWarmupSpanInvalid { e: String } => "profile.data.warmup.span_invalid":
            "data.warmup: {e}",
        DataWarmupCalendarSpan { raw: String } => "profile.data.warmup.calendar_span":
            "data.warmup {raw:?} is a CALENDAR span, and a calendar month is not a fixed \
             number of steps (one month from 31 January is 28 days, from 31 March 30) — so \
             its length would depend on where data.from happens to sit, which is the \
             irreproducibility this key exists to remove. Write it in bars (\"200bars\") or \
             in fixed time (\"90d\")",
        DataWarmupDurationOnTicks { raw: String } => "profile.data.warmup.duration_on_ticks":
            "data.warmup {raw:?} is a DURATION, which is bar-mode only: a tick tape has no \
             fixed cadence to divide it by, so no number of ticks corresponds to it. The \
             engine gates run_ticks on an EVENT index, so write the requirement as a count \
             — e.g. \"200bars\" means 200 ticks here",
        DataWarmupIntervalInvalid { interval: String, raw: String } =>
            "profile.data.warmup.interval_invalid":
            "data.interval {interval:?} is not a valid interval, so the duration data.warmup \
             {raw:?} cannot be converted to a step count",
        DataWarmupIntervalNotPositive { interval: String, base: i64, raw: String } =>
            "profile.data.warmup.interval_not_positive":
            "data.interval {interval:?} resolves to {base}ms, so the duration data.warmup \
             {raw:?} names no number of steps",
        DataDetailIntervalBarModeOnly { raw: String } => "profile.data.detail_interval.bar_mode_only":
            "data.detail_interval {raw:?} is bar-mode only: the intrabar detail tape is \
             bucketed into COARSE BAR steps and consulted by StrategyEngine::run's bar fill \
             phase, and run_ticks never reads those buckets — a tick replay already resolves \
             every order against the tape it is replaying",
        DataDetailIntervalInvalid { raw: String } => "profile.data.detail_interval.invalid":
            "data.detail_interval {raw:?} is not a valid interval (want a count then one of \
             s/m/h/d, e.g. \"1m\") — it is handed straight to the hist store as a series \
             interval, so it must name a series the store can hold",
        DataDetailIntervalNotPositive { raw: String, ms: i64 } =>
            "profile.data.detail_interval.not_positive":
            "data.detail_interval {raw:?} resolves to {ms}ms — a zero-length window makes \
             every sub-bar boundary the same instant",
        DataDetailIntervalBaseIntervalInvalid { interval: String, raw: String } =>
            "profile.data.detail_interval.base_interval_invalid":
            "data.interval {interval:?} is not a valid interval, so data.detail_interval {raw:?} \
             cannot be checked as finer than it",
        DataDetailIntervalNotFiner { raw: String, ms: i64, interval: String, base: i64 } =>
            "profile.data.detail_interval.not_finer":
            "data.detail_interval {raw:?} ({ms}ms) must be strictly FINER than data.interval \
             {interval:?} ({base}ms) — at this resolution each coarse step buckets at most one \
             sub-bar, which resolves no stop-versus-target ambiguity while paying for a \
             second whole store scan. This is the inverse of the engine.timeframes rule, \
             where a synthesised series must be COARSER than the base",
        DataOnGapUnknown { raw: String, roster: String } => "profile.data.on_gap.unknown":
            "data.on_gap {raw:?} is not one of {roster}. It says what data.require_coverage DOES \
             about a window the store does not cover: `refuse` stops the run (the default, \
             because a gate that only warns is invisible on every run nobody reads), `warn` \
             logs the findings and runs unchanged, `run` leaves the gate inert while keeping \
             the file's statement of what this run is supposed to require",
        /// `vike_model::time::parse_span`'s sentence, prefixed with the key.
        DataMaxGapSpanInvalid { e: String } => "profile.data.max_gap.span_invalid":
            "data.max_gap: {e}",
        DataMaxGapNotPositive { raw: String, ms: i64 } => "profile.data.max_gap.not_positive":
            "data.max_gap {raw:?} resolves to {ms}ms — a non-positive tolerance is what an \
             ABSENT data.max_gap already means (tolerate nothing), so this spelling can only \
             mislead. Remove the key, or name a real duration",
        DataMaxGapBarCount { raw: String, n: usize } => "profile.data.max_gap.bar_count":
            "data.max_gap {raw:?} is a BAR COUNT, and a gap in this store is a wall-clock span \
             in epoch-ms: converting {n} bars to a duration needs an interval, and a tick \
             profile has none. Write the tolerance as fixed time — \"4h\", \"1d\"",
        DataMaxGapCalendarSpan { raw: String } => "profile.data.max_gap.calendar_span":
            "data.max_gap {raw:?} is a CALENDAR span, and a calendar month is not a fixed \
             number of milliseconds (one month from 31 January is 28 days, from 31 March 30) \
             — so the tolerance would depend on where data.from happens to sit, and two runs \
             naming the same tolerance would accept different amounts. Write it in fixed time \
             — \"30d\"",
        DataUniverseUnknown { raw: String, roster: String } => "profile.data.universe.unknown":
            "data.universe {raw:?} is not one of {roster}. It is the POINT-IN-TIME membership \
             rule: `declared` takes the symbol list verbatim (the default), `covered` names \
             every member whose tape does not span the window and runs anyway, `strict` \
             refuses that run. None of the three ever drops a member — a universe a run \
             narrowed silently would not be the one the profile names",
        /// A `[[data.reference]]` or `[[data.mark]]` table's `delay_ms` that is not a whole number 0 or more.
        DataStreamDelayInvalid { table: String, symbol: String, delay: toml::Value } =>
            "profile.data.stream_delay_invalid":
            "data.{table} {symbol:?} states delay_ms = {delay}, and a delay is a whole number of \
             milliseconds, 0 or more: the time added to when each of its events reaches this box \
             (its recorded receive time, else its venue's stamp)",
        EngineResolutionKindUnknown { kind: String } => "profile.engine.resolution.kind_unknown":
            "unknown engine.resolution.kind {kind:?} (known: \"binary_outcome\")",
        EngineResolutionWindowSecondsNotPositive { window_secs: i64 } =>
            "profile.engine.resolution.window_secs.not_positive":
            "engine.resolution.window_secs must be > 0, got {window_secs}",
        EngineResolutionWinnerNotBinary { slug: String, wi: i64 } =>
            "profile.engine.resolution.winners.not_binary":
            "engine.resolution.winners[{slug:?}] = {wi} is not a binary outcome index \
             (0 or 1). Non-binary and unresolved (-1) values are real on-chain facts and \
             are never coerced to \"both sides lose\" — drop the window from the run \
             instead.",
        EngineResolutionWindowsWithoutWinner { missing: usize, names: Vec<String> } =>
            "profile.engine.resolution.windows_without_winner":
            "engine.resolution: {missing} window slug(s) in data.series have no binary \
             winning_index (first: {names:?}). A window with no payout would end the run \
             marked at its last traded price — remove it from the slice or supply its \
             resolution.",
        PermutationRankByUnknown { raw: String } => "profile.permutation.rank_by.unknown":
            "unknown permutation.rank_by {raw:?} (want sharpe | return | max_dd | equity; \
             absent = sharpe)",
        InputsKeyNotDeclared { key: String, reads: String } => "profile.inputs.key_not_declared":
            "[inputs] {key:?} names no input this strategy reads, so it would remap nothing ({reads}). \
             An [inputs] row is keyed by the name the strategy's code writes",
        /// `crate::run::data_plan::inputs`'s `parse_series_name` sentence for a name the strategy reads, or one
        /// an `[inputs]` row maps it to.
        InputsNameRefused { whose: String, read: String, e: String } => "profile.inputs.name_refused":
            "{whose} {read:?}: {e}",

        MountsRouteRunsOneStrategy => "profile.mounts.route_runs_one_strategy":
            "this route runs one [strategy], and the profile names [[mounts]]: a portfolio runs \
             as one backtest (vike-cli backtest run), never as a walk-forward, a search or a \
             permutation test",
        StrategyOrMountsMissing => "profile.strategy.strategy_or_mounts_missing":
            "a profile names what it trades: a [strategy] table, or a [[mounts]] list whose \
             entries each carry one",
        StrategyBesideMounts => "profile.strategy.beside_mounts":
            "[strategy] and [[mounts]] are two ways to name what a profile trades: keep one \
             (a portfolio names each strategy in its mount's [mounts.strategy] table)",
        MountsStrategyFileRefused { i: usize, refusal: String } =>
            "profile.mounts.strategy.file.refused":
            "mounts[{i}].{refusal}",
        MountsStrategySdkBesideRhaiFile { i: usize, sdk: u32, file: String } =>
            "profile.mounts.strategy.sdk.beside_rhai_file":
            "mounts[{i}].strategy.sdk = {sdk} names a Rust strategy SDK, and its file {file:?} is a \
             .rhai file, which runs on this vike's own code: remove the mount's strategy.sdk",
        MountsSymbolsEmpty { i: usize } => "profile.mounts.symbols.empty":
            "mounts[{i}].symbols is empty: a mount trades at least one slot of the data slice",
        MountsSymbolsRepeated { i: usize, symbol: String } => "profile.mounts.symbols.repeated":
            "mounts[{i}].symbols names {symbol:?} twice: a mount trades a slot once",
        MountsMaxLeverageBelowOneOrNotFinite { i: usize, cap: f64 } =>
            "profile.mounts.max_leverage.below_one_or_not_finite":
            "mounts[{i}].max_leverage must be a finite figure of at least 1 (the mount's \
             gross notional over the account's sizing equity), got {cap}: omit the key for \
             engine.max_mount_leverage",
        MountsMaxLeverageAboveEngine { i: usize, cap: f64, engine: f64 } =>
            "profile.mounts.max_leverage.above_engine_max_mount_leverage":
            "mounts[{i}].max_leverage = {cap} is above engine.max_mount_leverage = \
             {engine}: a mount's figure may only narrow the run's, and a higher one is \
             refused, never clamped. Lower it, or raise engine.max_mount_leverage",
        MountsMaxLossNotPositiveOrNotFinite { i: usize, loss: f64 } =>
            "profile.mounts.max_loss.not_positive_or_not_finite":
            "mounts[{i}].max_loss must be a finite amount above 0 in the run's ledger \
             currency, got {loss}: omit the key for no loss budget",
        MountsSymbolsNotInDataSlice { i: usize, symbol: String, slots: String } =>
            "profile.mounts.symbols.not_in_data_slice":
            "mounts[{i}].symbols names {symbol:?}, which is no slot of the data slice: a \
             mount trades the slice's own series, named as the slice names them ({slots})",
        MountsSlotTradedByNoMount { slot: String } => "profile.mounts.slot_traded_by_no_mount":
            "the data slice's {slot:?} is traded by no mount: name it in a mount's \
             symbols, or drop it from [data]",
        MountsBesideWalkforward => "profile.mounts.beside_walkforward":
            "[[mounts]] beside [walkforward] is not built: the walk splits one series and runs \
             one strategy on each window (walkforward::runner's one_series_preflight). Run the \
             portfolio as one backtest, or walk each mount's strategy alone",
        MountsBesideParamscan => "profile.mounts.beside_paramscan":
            "[[mounts]] beside a [paramscan] grid is not built: a grid varies one \
             [strategy.params] table. Search each mount's strategy alone",
        MountsBesidePermutation => "profile.mounts.beside_permutation":
            "[[mounts]] beside [permutation] is not built: the test permutes one run's held bars \
             (walkforward::runner's held_bars_preflight)",
        MountsBesideSimultaneousDecide => "profile.mounts.beside_simultaneous_decide":
            "[[mounts]] beside engine.decide = \"simultaneous\" is not built: on each symbol's \
             closed bar the mounts that trade it decide in list order, each order judged as it \
             is placed, and the step-boundary budget snapshot a simultaneous decision needs \
             does not exist",
        MountsOnTickLane => "profile.mounts.on_tick_lane":
            "[[mounts]] on the tick lane is not built: the tick replay dispatches no mounts. Run \
             the portfolio over bars (data.kind = \"bar\")",
        EngineInBarFillAtCloseWithMounts => "profile.engine.in_bar.fill_at_close_with_mounts":
            "[engine.in_bar] fill_at_close with two or more mounts is not built: the simulator \
             fills a bar's close orders after every mount has decided, where live fills each \
             mount's before the next mount steps",
        EngineInBarCommittedStopsWithMounts =>
            "profile.engine.in_bar.committed_stops_with_mounts":
            "[engine.in_bar] committed_stops with two or more mounts is not built: a stop is \
             committed only while no market order rests on its symbol, and that book is every \
             mount's, so one mount's market order would decide another mount's stop",
        EngineClosedTradesOffForPortfolio => "profile.engine.closed_trades.off_for_portfolio":
            "engine.closed_trades = false leaves out the closed-trade ledger each mount's trades \
             and win % are read from: a portfolio keeps it on",
        MountsFileFillsAtClose { file: String } => "profile.mounts.file_fills_at_close":
            "mount {file:?}'s file says fill = \"close\", and a portfolio of two or more mounts does \
             not fill at a bar's close: the simulator fills a bar's close orders after every \
             mount has decided, where live fills each mount's before the next mount steps",
        MountsOptimisticQuotesDiffer { first: String, other: String } =>
            "profile.mounts.optimistic_quotes_differ":
            "mounts {first:?} and {other:?} fill a tagged plain limit by different laws (their headers' \
             optimistic_quotes): one run fills by one law, so state it for all with \
             [engine.quotes] optimistic, or run them apart",
        MountsCostsAskedWithoutMount => "profile.mounts.costs_asked_without_mount":
            "a portfolio's costs were asked of a profile with no mount",
        MountsCostsDiffer { first: String, other: String } => "profile.mounts.costs_differ":
            "mounts {first:?} and {other:?} charge different commissions or slippage: one run charges \
             every fill one cost, so set the same commission and slippage params in every \
             mount's [mounts.strategy.params], or run them apart",
        MountsInputReadsDiffer { file: String } => "profile.mounts.input_reads_differ":
            "mount {file:?} is a backtest-only file, whose inputs are read whole, and another mount's \
             are read as live reads them: one run reads its inputs one way, so run them apart",
        MountsProfileNamesNoStrategyTable => "profile.mounts.names_no_strategy_table":
            "a profile with [[mounts]] names no [strategy] table: each mount carries its own \
             [mounts.strategy]. The CLI's --preset, --script, --param and --strategy flags, a --set \
             of strategy.*, and the MCP script argument all write [strategy], so edit the mount's own \
             table instead",
        MountsPreparedAsOneStrategyTable => "profile.mounts.prepared_as_one_strategy_table":
            "a portfolio's `.rs` mounts are each made ready by its mount \
             (`StrategyFiles::prepare_mount`), never as one strategy table",
        StrategyFilePathRefused { e: String } => "profile.strategy.file.path_refused":
            "strategy.file: {e}",
        StrategyFileIsLink { file: String } => "profile.strategy.file.is_link":
            "strategy.file {file:?} is a link, and a link is not read: copy the file itself into \
             the strategies folder",
        StrategyFileUnreadable { file: String, path: String, e: String } =>
            "profile.strategy.file.unreadable":
            "strategy.file {file:?}: cannot read {path}: {e}",
        /// The file's header did not parse (`StrategySource::read`), in the header's words.
        StrategyFileHeaderRefused { file: String, e: String } =>
            "profile.strategy.file.header_refused":
            "strategy.file {file:?}: {e}",
        StrategyParamsRefused { file: String, refused: String } => "profile.strategy.params.refused":
            "strategy.file {file:?}: [strategy.params] refused: {refused}",
        StrategyFileNotRead { file: String } => "profile.strategy.file.not_read":
            "strategy.file {file:?} was not read: a door that runs a profile reads its strategy file \
             from the project's strategies/ folder first",
        StrategyFileRhaiCompileError { file: String, error: String } =>
            "profile.strategy.file.rhai_compile_error":
            "strategy.file {file:?}: rhai compile error: {error}",
        StrategyFileNoSandboxRoute { file: String } => "profile.strategy.file.no_sandbox_route":
            "strategy.file {file:?} is a Rust strategy file: it runs only in a sandboxed child process, \
             and this door has no route to one",
        StrategyNameRhaiNoLongerRead => "profile.strategy.name.rhai_no_longer_read":
            "[strategy] name = \"rhai\" is no longer read: save the script as a .rhai file \
             under the project's strategies/ folder and name it, [strategy] file = \
             \"<folder>/<script>.rhai\"",
        StrategyNameNoLongerRead { name: String, file: String } =>
            "profile.strategy.name.no_longer_read":
            "[strategy] name = {name:?} is no longer read: a profile names its strategy's file. \
             Write [strategy] file = {file:?}",
        StrategyRhaiNoLongerRead { path: String } => "profile.strategy.rhai.no_longer_read":
            "[strategy] rhai = {path:?} is no longer read: write [strategy] file = \"<the script's \
             path under the project's strategies/ folder>\"",
        EngineStudioFeeNoLongerRead => "profile.engine.studio_fee_no_longer_read":
            "a Studio run's fee is no longer read: {COMMISSION_NOW}",
        EngineStudioSlippageNoLongerRead => "profile.engine.studio_slippage_no_longer_read":
            "a Studio run's slippage is no longer read: {SLIPPAGE_NOW}",
        EngineDecideUnknown { other: String } => "profile.engine.decide.unknown":
            "unknown engine.decide {other:?} (want {DECIDE_WORDS}; absent = sequential, \
             the per-symbol walk in the order data.symbols lists them)",
        EngineMarginModeIsolatedNotRun => "profile.engine.margin_mode.isolated_not_run":
            "engine.margin_mode \"isolated\": vike does not run isolated margin yet",
        EngineMarginModeUnknown { other: String } => "profile.engine.margin_mode.unknown":
            "unknown engine.margin_mode {other:?} (want cross | cash; absent = cross)",
        StrategyParamsNotSerialized { e: String } => "profile.strategy.params.not_serialized":
            "serializing the strategy file's params table to TOML failed: {e}",
        StrategyFileMountNotLoaded { file: String, mount: usize, loaded: usize } =>
            "profile.strategy.file.mount_not_loaded":
            "strategy.file {file:?} is mount {mount}, and this sandboxed child loaded {loaded} \
             mount file(s)",
        StrategyArtifactNoLongerRun { name: String } => "profile.strategy.artifact_no_longer_run":
            "strategy `{name}`: a built artifact is no longer run by its name and sha. Run the \
             strategy FILE it was built from (`[strategy] file = \"<folder>/{name}.rs\"`): a `.rs` \
             file runs in a sandboxed child, and no hub loads an artifact",

        /// A `[walkforward]` duration key (`train`, `test`, `step`, `purge`, `embargo`) the span grammar cannot read.
        WalkforwardDurationUnreadable { key: String, e: String } =>
            "profile.walkforward.duration_unreadable":
            "walkforward.{key}: {e}",
        WalkforwardSplitsBesideDurations => "profile.walkforward.splits_beside_durations":
            "walkforward.n_splits and walkforward.train/test/step are two PROTOCOLS, \
             not two spellings — a split count is defined BY the range and moves when \
             the range moves, a duration does not. Set exactly one.",
        WalkforwardNoWindowForm => "profile.walkforward.no_window_form":
            "[walkforward] declares no window form — set n_splits = N, or a train/test \
             pair (e.g. train = \"12mo\", test = \"3mo\")",
        WalkforwardGapNeedsDurationForm => "profile.walkforward.gap_needs_duration_form":
            "walkforward.purge / walkforward.embargo need the DURATION window \
             form (train/test) — the split-count walk stays gap-free \
             (docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md). \
             Replace n_splits with a train/test pair, or drop the key.",
        WalkforwardDurationFormHalfDeclared => "profile.walkforward.duration_form_half_declared":
            "the duration window form needs BOTH walkforward.train and \
             walkforward.test (step defaults to test) — a half-declared window \
             has no shape",
        WalkforwardCombinatorialBesideOtherForm =>
            "profile.walkforward.combinatorial_beside_other_form":
            "walkforward.groups/test_groups is a third PROTOCOL beside n_splits and \
             train/test/step — the combinatorial walk answers paths, the other two one \
             stitched walk. Set exactly one form.",
        WalkforwardCombinatorialFormHalfDeclared =>
            "profile.walkforward.combinatorial_form_half_declared":
            "the combinatorial window form needs BOTH walkforward.groups and \
             walkforward.test_groups — a half-declared cut has no splits",
        WalkforwardGroupsCutNoSplit { groups: usize, test_groups: usize } =>
            "profile.walkforward.groups_cut_no_split":
            "walkforward.groups = {groups} with walkforward.test_groups = {test_groups} cuts \
             no split: the combinatorial walk needs groups >= 2 and 1 <= test_groups < groups",
        WalkforwardCombinatorialConfiguresNoModeOrPurge =>
            "profile.walkforward.combinatorial_configures_no_mode_or_purge":
            "walkforward.mode / walkforward.purge configure nothing on the combinatorial \
             form: each split trains on every group it does not test, so there is no train \
             shape to pick and no gap before a test group. Drop the key; walkforward.embargo \
             is the bars dropped from training after each test group.",
        WalkforwardSearchGridRenamed => "profile.walkforward.search.grid_renamed":
            "walkforward.search = \"grid\" was renamed to \"paramscan\" — one name for one \
             knob, and it is the table it expands ([paramscan]). Write search = \"paramscan\".",
        WalkforwardSearchUnknown { other: String } => "profile.walkforward.search.unknown":
            "unknown walkforward.search {other:?} (want none | paramscan; absent = none, the \
             no-search control)",
        WalkforwardModeUnknown { other: String } => "profile.walkforward.mode.unknown":
            "unknown walkforward.mode {other:?} (want anchored | rolling; absent = anchored)",
        WalkforwardRankByUnknown { raw: String } => "profile.walkforward.rank_by.unknown":
            "unknown walkforward.rank_by {raw:?} (want sharpe | return | max_dd | equity; \
             absent = sharpe)",
        /// `[engine.margin_call]`'s numbers refused by `vike_simulator::ShortfallCover::validate`, whose text opens with the key.
        EngineMarginCallRefused { e: String } => "profile.engine.margin_call.refused":
            "engine.margin_call.{e}",
        EngineAccountCurrencyUnknown { raw: String } => "profile.engine.account_currency.unknown":
            "unknown engine.account_currency {raw:?} (want a fiat currency code such as USD, EUR, GBP \
             or JPY; absent = USD, the dollar ledger)",
        EngineConversionNotACurrencyCode { key: String } =>
            "profile.engine.conversion.not_a_currency_code":
            "engine.conversion {key:?} is not a currency code (the quote currency the row \
             converts into the ledger, such as JPY or USDT)",
        EngineConversionLedgerOwnCurrency { quote: String, ledger: String } =>
            "profile.engine.conversion.ledger_own_currency":
            "engine.conversion.{quote} names a pair for the ledger's own currency ({ledger}, \
             engine.account_currency), which converts into itself: drop the row",
        EngineConversionQuoteTwice { quote: String } => "profile.engine.conversion.quote_twice":
            "engine.conversion names {quote} twice (keys are read in any case): keep one row",
        /// A conversion row's series name the series grammar (`crate::run::data_plan::parse_series_name`) cannot read.
        EngineConversionSeriesUnreadable { quote: String, series: String, e: String } =>
            "profile.engine.conversion.series_unreadable":
            "engine.conversion.{quote} = {series:?}: {e}",
        EngineConversionPairDirectionUnreadable {
            quote: String,
            series: String,
            symbol: String,
            ledger: String,
        } => "profile.engine.conversion.pair_direction_unreadable":
            "engine.conversion.{quote} = {series:?}: the pair {symbol} is neither {quote}{ledger} (a \
             {quote} priced in {ledger}) nor {ledger}{quote} (a {ledger} priced in {quote}), so \
             which way its price points against the {ledger} ledger cannot be read off its name. \
             Name a series of one of the two (any of - / _ . between them)",
        EngineQueueModelPowerNotANumber { tail: String } =>
            "profile.engine.queue_model.power_not_a_number":
            "queue_model \"prob_power:{tail}\": {tail:?} is not a number",
        EngineQueueModelUnknown { other: String } => "profile.engine.queue_model.unknown":
            "unknown queue_model {other:?} (want risk_averse | prob_power[:n] | prob_log)",
        EngineQueueModelRiskAverseRespelled { old: String } =>
            "profile.engine.queue_model.risk_averse_respelled":
            "queue_model {old:?} is spelled \"risk_averse\" now",
        EngineFillModelUnknown { other: String } => "profile.engine.fill_model.unknown":
            "unknown fill_model {other:?} (want l2book; absent = the default Tick model)",
        EngineSizerNestingTooDeep { depth: usize, bases: String } =>
            "profile.engine.sizer.nesting_too_deep":
            "engine.sizer nesting is {depth} levels deep ([engine.sizer{bases}]) — \
             MAX_SIZER_DEPTH is {MAX_SIZER_DEPTH}; flatten the base chain (no composition of \
             the two wrapping kinds needs to nest this deep)",
        EngineSizerKnobRequired { field: String, kind: String } =>
            "profile.engine.sizer.knob_required":
            "engine.sizer.{field} is required when engine.sizer.kind = {kind:?}",
        EngineSizerPortfolioHeatNeedsBase => "profile.engine.sizer.portfolio_heat_needs_base":
            "engine.sizer.kind = \"portfolio_heat\" wraps a base sizer — add an \
             [engine.sizer.base] table naming it",
        EngineSizerDrawdownThrottleNeedsBase =>
            "profile.engine.sizer.drawdown_throttle_needs_base":
            "engine.sizer.kind = \"drawdown_throttle\" wraps a base sizer — add an \
             [engine.sizer.base] table naming it",
        EngineSizerKindUnknown { other: String } => "profile.engine.sizer.kind.unknown":
            "unknown engine.sizer.kind {other:?} (want pass_through | fixed_dollar | \
             fixed_shares | percent_equity | percent_volatility | max_risk_percent | portfolio_heat \
             | drawdown_throttle)",
        EngineSizerBaseUnderScalarKind { kind: String, bases: String } =>
            "profile.engine.sizer.base_under_scalar_kind":
            "engine.sizer.kind = {kind:?} does not wrap a base sizer, but \
             [engine.sizer{bases}.base] is set — only \"portfolio_heat\" and \"drawdown_throttle\" \
             read it, so the chain would run {kind:?} alone. Remove the base table, or name a \
             wrapping kind",
        EngineFeeRateNoLongerRead { rate: toml::Value } => "profile.engine.fee_rate.no_longer_read":
            "engine.fee_rate = {rate} is no longer read: {COMMISSION_NOW} (a fee_rate fraction f is \
             commission = 100 x f in percent)",
        EngineFeeNoLongerRead => "profile.engine.fee.no_longer_read":
            "[engine.fee] is no longer read: {COMMISSION_NOW}",
        /// `[engine.slippage]` or the older `[engine.impact]` (`key` names which).
        EngineSlippageNoLongerRead { key: String } => "profile.engine.slippage.no_longer_read":
            "{key} is no longer read: {SLIPPAGE_NOW}",
        PermutationTableMissing => "profile.permutation.table_missing":
            "profile has no [permutation] table — a permutation test reads \
             permutation.permutations, e.g. `[permutation]\npermutations = 1000`",
        PermutationPermutationsZero => "profile.permutation.permutations.zero":
            "a permutation test needs at least one permuted history; zero gives no p-value",
        PermutationBarModeOnly => "profile.permutation.bar_mode_only":
            "a permutation test is bar-mode only: it permutes the bars of the history, which a \
             tick replay does not produce — set data.kind = \"bar\"",
        PermutationTrainingNeedsParamscan => "profile.permutation.training_needs_paramscan":
            "permutation training re-runs the optimisation on every history and needs a \
             [paramscan] table to search — this profile has none. Add the grid, or run the plain \
             permutation test.",

        WalkforwardStepShorterThanTest { step: usize, test: usize } =>
            "profile.walkforward.step_shorter_than_test":
            "walkforward.step resolves to {step} bar(s) and walkforward.test to {test} — \
             a step shorter than the validation window makes consecutive windows OVERLAP, \
             and the stitched out-of-sample curve would compound the same bars more than \
             once, reporting a return and a Sharpe roughly the overlap factor too high. \
             Set step >= test: equal tiles the windows edge to edge (that is also what an \
             absent step means), and larger leaves a gap between them.",
        WalkforwardCombinatorialNotStitched => "profile.walkforward.combinatorial_not_stitched":
            "walkforward.groups/test_groups is the combinatorial walk, whose answer is its paths \
             rather than one window list: it runs through the combinatorial driver \
             (`vike-cli backtest run` routes it there), not through the stitched walk",
        WalkforwardSplitsTooMany {
            k: usize,
            n: usize,
            chunks: usize,
            chunk_bars: usize,
            why: String,
            fix: String,
        } => "profile.walkforward.n_splits_too_many":
            "walkforward.n_splits = {k} cuts the {n} bar(s) of this range into {chunks} equal chunks of \
             {chunk_bars} bar(s) (the first trains the first window, then one out-of-sample window each), and {why}. \
             {fix}.",
        WalkforwardCombinatorialNeedsGroups => "profile.walkforward.combinatorial_needs_groups":
            "the combinatorial walk needs walkforward.groups and walkforward.test_groups; this \
             [walkforward] table declares another window form",
        WalkforwardSpanMonthsTooLong { key: String, k: i64, years: i64 } =>
            "profile.walkforward.span_months_too_long":
            "walkforward.{key} is {k} months, longer than {years} years and than any series; \
             shorten it",
        WalkforwardSpanNeedsBarInterval { key: String, interval: String } =>
            "profile.walkforward.span_needs_bar_interval":
            "walkforward.{key} is a duration, so it needs a bar interval to convert against — \
             data.interval {interval:?} is not one (want e.g. \"1m\", \"1h\", \"1d\"), or spell \
             the window with the `bars` suffix",
        WalkforwardSpanMillisecondsTooLong { key: String, ms: i64, interval: String } =>
            "profile.walkforward.span_milliseconds_too_long":
            "walkforward.{key} is {ms} ms, too long to count in {interval} bars and longer \
             than any series; shorten it",
        WalkforwardSpanShorterThanOneBar { key: String, interval: String } =>
            "profile.walkforward.span_shorter_than_one_bar":
            "walkforward.{key} is shorter than one {interval} bar — it would resolve to zero \
             bars and change nothing; widen it or use the `bars` suffix",
        WalkforwardTickSeriesNotOne { series: usize } => "profile.walkforward.tick_series_not_one":
            "a tick walk-forward runs over ONE series — this profile resolves {series}; name a single \
             symbol",
        WalkforwardTickEventsCrossWindowEdge => "profile.walkforward.tick_events_cross_window_edge":
            "a tick walk-forward cuts ONE series' tape in venue order, and [data.options] legs, \
             heard streams ([[data.reference]], [[data.mark]]) and engine.feed_latency each \
             order or make events across a window's edge otherwise than a run of that window \
             alone would, so a window would not be its range's answer: walk without them",
        WalkforwardTickSeedBarIntervalMissing =>
            "profile.walkforward.tick_seed_bar_interval_missing":
            "a tick walk-forward counts its windows in seed bars, and cuts them on a bar's edge \
             so each window seeds from its own events: set engine.seed_bar_interval_ms (60000 \
             cuts on the minute)",
        WalkforwardTickRangeOpenEnded => "profile.walkforward.tick_range_open_ended":
            "a tick walk-forward cuts its windows in time, so its range must have two ends: \
             set data.from and data.to",
        WalkforwardTickNoWindows { seed_bars: usize, symbol: String } =>
            "profile.walkforward.tick_no_windows":
            "the [walkforward] window form yields NO windows over the {seed_bars} seed bar(s) of {symbol} in this \
             range — a walk that evaluates nothing must not return a report that looks like one",
        WalkforwardTickWindowShorterThanWarmup {
            i: usize,
            seed_bars: usize,
            symbol: String,
            test: usize,
            training_clause: String,
            warmup: usize,
            at_point: String,
            train_clause: String,
        } => "profile.walkforward.tick_window_shorter_than_warmup":
            "window {i} of the [walkforward] windows over the {seed_bars} seed bar(s) of {symbol} holds {test} \
             priced event(s) out of sample{training_clause}, and a window's engine starts afresh and decides \
             nothing before its warm-up of {warmup} (data.warmup, else the strategy's own{at_point}, \
             counted in quotes and trades on a tick tape) has passed. Lengthen walkforward.test{train_clause}, \
             cut fewer windows, or widen data.from/data.to.",
        WalkforwardTickGroupsMoreThanSeedBars { groups: usize, symbol: String, seed_bars: usize } =>
            "profile.walkforward.tick_groups_more_than_seed_bars":
            "{groups} groups need at least {groups} seed bars; {symbol} has {seed_bars} in this range",
        WalkforwardTickGroupsShorterThanWarmup {
            groups: usize,
            seed_bars: usize,
            symbol: String,
            fewest: usize,
            warmup: usize,
            at_point: String,
        } => "profile.walkforward.tick_groups_shorter_than_warmup":
            "{groups} groups cut the {seed_bars} seed bar(s) of {symbol} into groups holding {fewest} priced \
             event(s) or more, and a group's engine starts afresh and decides nothing before its \
             warm-up of {warmup} (data.warmup, else the strategy's own{at_point}, counted in quotes and \
             trades on a tick tape) has passed. Cut fewer groups, or widen data.from/data.to.",
        WalkforwardNoWindows { bars: usize, symbol: String } => "profile.walkforward.no_windows":
            "the [walkforward] window form yields NO windows over the {bars} bar(s) of {symbol} in \
             this range — a walk that evaluates nothing must not return a report that looks like \
             one",
        WalkforwardWindowsShorterThanWarmup {
            bars: usize,
            symbol: String,
            warmup: usize,
            at_point: String,
            test: usize,
            training_clause: String,
            train_clause: String,
        } => "profile.walkforward.windows_shorter_than_warmup":
            "the [walkforward] windows over the {bars} bar(s) of {symbol} are too short for the \
             warm-up of {warmup} bar(s) (data.warmup, else the strategy's own{at_point}): the \
             shortest out-of-sample window is {test} bar(s){training_clause}, and a window's engine starts \
             afresh and decides nothing before its warm-up has passed. Lengthen \
             walkforward.test{train_clause} past {warmup} bars, or widen data.from/data.to.",
        WalkforwardTableMissing => "profile.walkforward.table_missing":
            "profile has no [walkforward] table — a walk-forward needs a window form, e.g. \
             `[walkforward]\nn_splits = 4` or `[walkforward]\ntrain = \"12mo\"\ntest = \"3mo\"`",
        WalkforwardBarModeOnly => "profile.walkforward.bar_mode_only":
            "walk-forward is bar-mode only: the splitter divides ONE bar series by index, which a \
             tick replay does not produce — set data.kind = \"bar\"",
        WalkforwardSeriesNotOne { series: usize } => "profile.walkforward.series_not_one":
            "walk-forward runs over ONE bar series — this profile resolves {series}; name a single \
             symbol",
        WalkforwardSearchWithoutGrid => "profile.walkforward.search_without_grid":
            "walk-forward optimization needs a [paramscan] table to search — this profile has none, \
             so every window would 'select' the one parameter set it already has. Add the grid, \
             or run the fixed-parameter walk instead.",
        WalkforwardTableMissingForCombinatorial =>
            "profile.walkforward.table_missing_for_combinatorial":
            "profile has no [walkforward] table — the combinatorial walk reads \
             walkforward.groups and walkforward.test_groups",
        WalkforwardGroupsMoreThanBars { groups: usize, symbol: String, bars: usize } =>
            "profile.walkforward.groups_more_than_bars":
            "{groups} groups need at least {groups} bars; {symbol} has {bars} in this range",
        WalkforwardGroupsShorterThanWarmup {
            groups: usize,
            bars: usize,
            symbol: String,
            shortest: usize,
            warmup: usize,
            at_point: String,
            fix: String,
        } => "profile.walkforward.groups_shorter_than_warmup":
            "{groups} groups cut the {bars} bar(s) of {symbol} into groups of {shortest} bar(s) or more, \
             and a group's engine starts afresh and decides nothing in its first {warmup} warm-up \
             bar(s) (data.warmup, else the strategy's own{at_point}). {fix}.",
        WalkforwardCombinatorialSearchWithoutGrid =>
            "profile.walkforward.combinatorial_search_without_grid":
            "a searched combinatorial walk-forward needs a [paramscan] table to search — this \
             profile has none. Add the grid, or walk the control.",
        WalkforwardCombinatorialCutUnrunnable { groups: usize, test_groups: usize } =>
            "profile.walkforward.combinatorial_cut_unrunnable":
            "a combinatorial walk-forward needs at least 2 groups and 1 <= test_groups < groups; \
             got groups = {groups}, test_groups = {test_groups}",
        WalkforwardCombinatorialTooManyRuns { groups: usize, test_groups: usize, runs: usize } =>
            "profile.walkforward.combinatorial_too_many_runs":
            "groups = {groups} with test_groups = {test_groups} is at least {runs} out-of-sample \
             group runs (paths x groups); a combinatorial walk-forward makes at most \
             {MAX_OUT_OF_SAMPLE_RUNS}, so its answer fits one frame. Cut fewer groups or test \
             fewer of them: groups = 12 with test_groups = 6 is 5,544",
        WalkforwardEmbargoLeavesNoTraining {
            embargo_bars: usize,
            split: usize,
            groups: usize,
            test_groups: usize,
            bars: usize,
        } => "profile.walkforward.embargo_leaves_no_training":
            "walkforward.embargo resolves to {embargo_bars} bar(s), which leaves split {split} of \
             {groups} groups ({test_groups} tested) over {bars} bar(s) no training bar to search on. \
             Shorten the embargo below a group's length, or cut fewer groups.",
        ParamscanGridTooLarge { sizes: String } => "profile.paramscan.grid_too_large":
            "the paramscan grid ({sizes}) is more than the {PARAMSCAN_POINTS_MAX} points one grid \
             may run: narrow an axis or widen its step",
        ParamscanAxisNotAnArray { key: String, value: vike_model::params::ParamValue } =>
            "profile.paramscan.axis_not_an_array":
            "paramscan.{key} must be an array of values to scan over, got {value:?}",
        ParamscanAxisEmpty { key: String } => "profile.paramscan.axis_empty":
            "paramscan.{key} must be a non-empty array",
        ParamscanAxisNamesEngineKey { key: String } => "profile.paramscan.axis_names_engine_key":
            "paramscan.{key}: a grid varies strategy params only; the run's commission and slippage \
             are strategy params, so scan commission or slippage (with their _unit beside them in \
             [strategy.params])",
        ParamscanSearchNeedsTable { search: String } => "profile.paramscan.search_needs_table":
            "{search} search needs a [paramscan] table (a single-point profile has nothing to search)",
        ParamscanAxisMixesIntegerAndFloat { key: String, search: String, grid: String } =>
            "profile.paramscan.axis_mixes_integer_and_float":
            "paramscan.{key} mixes integer and float values; {search} search must render every \
             coordinate as one TOML type, which would change what the strategy receives — \
             make the axis all-integer or all-float, or use the {grid} optimizer (--optimizer {grid})",
        ParamscanAxisNotNumeric {
            search: String,
            key: String,
            v: vike_model::params::ParamValue,
            grid: String,
        } => "profile.paramscan.axis_not_numeric":
            "{search} search needs numeric paramscan axes, but paramscan.{key} contains {v:?} — \
             use the {grid} optimizer (--optimizer {grid}) for non-numeric axes",
        ParamscanEulerTooManyAxes { axes: usize, grid: String } =>
            "profile.paramscan.euler_too_many_axes":
            "euler search supports at most {MAX_AXES} paramscan axes, got {axes} — the 3^d refinement \
             neighbourhood stops being cheaper than the grid past that; use the {grid} optimizer (--optimizer {grid})",

        // Carriers: a text another surface owns, forwarded unchanged.
        /// A test's stand-in refused to build its file (`StrategyBuilder::stand_in`), in its own words.
        StrategyStandInRefused { refusal: String } => "profile.strategy.stand_in_refused":
            "{refusal}",
        /// A loaded file's header refused a model key's run id (`StrategyFileHeader::with_model_texts`).
        StrategyModelTextsRefused { refusal: String } =>
            "profile.strategy.params.model_texts_refused":
            "{refusal}",
        /// A loaded `.rs` file refused its params or failed to build, in the file's words.
        StrategyFileBuildRefused { refusal: String } => "profile.strategy.file.build_refused":
            "{refusal}",
        /// The sandboxed child could not load a mount's `.rs` file: the jail's refusal (its own id), or the
        /// loader's words (no id: this one shows).
        StrategyFileLoadRefused { refusal: vike_utility::errors::CarriedRefusal } => carries refusal,
            "profile.strategy.file.load_refused": "{refusal}",
        /// `vike_fills::costs::costs_of` refused the strategy's commission/slippage params (the cost's id shows).
        StrategyCostRefused { refusal: vike_fills::refusals::CostRefusal } => carries refusal,
            "profile.strategy.cost_refused": "{refusal}",
        /// `vike_fills::market_impact::slippage_terms` refused the run's slippage.
        StrategySlippageRefused { refusal: String } => "profile.strategy.slippage_refused": "{refusal}",
        /// `vike_analytics::permutation::permute_bars` refused the held series (a misaligned set).
        PermutationBarsRefused { refusal: String } => "profile.permutation.bars_refused": "{refusal}",
    }
}
