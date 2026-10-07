//! `run_backtest` — the harness run dispatcher: loads a [`super::BacktestProfile`]'s
//! data slice from an `Arc<dyn HistStore>` (any backend: `DataFusionHist` in prod, a
//! read-in-place archive-Parquet store under `backtest --archive`, `MemHistStore` in tests) and runs
//! the resolved strategy in bar-mode
//! (`StrategyEngine`) or tick-mode (`replay_ticks`), depending on `profile.data.kind`.
//!
//! The two modes wire `EngineParams.properties` (the `snap_to_properties` point-in-time instrument
//! grid, PR-2a) DIFFERENTLY, and that difference is load-bearing:
//! - Bar mode never goes through `replay_ticks`, so nothing else would wire `params.properties` —
//!   this function sets it directly when `profile.engine.snap_to_properties` is on.
//! - Tick mode hands `snap_to_properties` straight through to `TickReplayConfig`; `replay_ticks`
//!   itself sets `params.properties` in that case (see `hist_replay::replay_ticks`). Setting it here
//!   too would be redundant (and diverge from the loader's own `properties_source` closure), so tick
//!   mode leaves `EngineParams.properties` untouched.
//!
//! Both modes load the profile's RESOLVED series (`DataCfg::resolved_series`), so a cross-venue
//! `[[data.series]]` slice and the frozen single-venue `venue` + `symbols` pair take the same
//! code path — the latter is just the former's whole-lane expansion (port backlog G4). The
//! opt-in `[engine.resolution]` settlement source (G6) and `[engine.fee]` schedule (G7) are
//! built once, here, and handed to `EngineParams` for both modes.
//!
//! ⚠ Resolution and fee are named above because they are BUILT here rather than copied across —
//! **they are not a roster of the `[engine]` surface this file wires, and nothing in this header
//! is.** Every `EngineCfg` field reaches `EngineParams` at one or both of the two construction
//! sites below; the authority for that set is `super::profile::EngineCfg`'s own field list, and
//! `crates/vike-ops/tests/wiring/engine_cfg_reaches_the_engine.rs` is what holds the two sites to it. A
//! prose list here would rot the way the one on `bar_engine_params` did (16 fields short inside
//! one branch), so this file keeps none.
//!
//! ⚠ **`EngineParams` is not fed by `[engine]` ALONE, and that gate cannot see the other feeder.**
//! Two `[data]` keys reach it here: `data.warmup` becomes `EngineParams::warmup` at both
//! construction sites (the warm-up FLOOR — it is a statement about how much HISTORY the slice must
//! spend before the first tradeable event, so it lives with the slice), and `data.detail_interval`
//! becomes `EngineParams::granular_by_symbol` through [`load_profile_detail_bars`] in
//! `run_backtest`'s bar arm. Both are named because a reader auditing "what configures the engine"
//! would otherwise check `EngineCfg` and the gate above and conclude the set was complete — it is
//! not, and nothing mechanical holds these two. Read `super::profile::DataCfg` for their rules.

use std::sync::Arc;

use vike_data::HistStore;
use vike_exec::RiskLimits;
use vike_model::Bar;

use super::{BacktestProfile, DataKind, HarnessError, strategy_by_name};
use crate::hist_replay::{TickReplayConfig, properties_source, replay_ticks};
use vike_analytics::BacktestResult;
use vike_sim::{EngineParams, StrategyEngine};

/// The optional pre-trade `RiskLimits` this profile's `[risk]` section describes
/// (runprofile-wiring-step2) — `None` EXACTLY when the section is absent, so a profile written
/// before this field existed maps to a `None` `EngineParams.risk_limits` byte-identically
/// (`SimBroker::build_risk_gate`'s `(None, None) => None` arm never mounts a gate). `Some`
/// delegates to [`vike_exec::ProfileRisk::to_risk_limits`] — the SAME compile-checked converter
/// paper/live use, so a limit proven in a backtest run carries unchanged into those modes. Shared
/// by both the bar-mode and tick-mode `EngineParams` construction below (one mapping, not two).
fn risk_limits_for(profile: &BacktestProfile) -> Option<RiskLimits> {
    profile.risk.as_ref().map(|r| r.to_risk_limits())
}

/// The BAR-mode [`EngineParams`] a profile describes, built in one place.
///
/// ⚠ This doc used to enumerate that surface — "`cash`/`fee_rate`/the `fee` SCHEDULE/`slippage`/
/// `[risk]`/`[engine.resolution]`/`[engine.impact]`/`snap_to_properties`" — and call the list "the
/// whole `[engine]` surface the bar lane honors". It was sixteen fields short by the end of the
/// branch that added them, and any list written here rots the same way. **The authority is
/// `super::profile::EngineCfg`'s own field set**, and
/// `crates/vike-ops/tests/wiring/engine_cfg_reaches_the_engine.rs` is what holds this function and its
/// tick-arm twin to it: every declared field must be read at BOTH sites unless a row in that file
/// states why one site (or both) is correct. Read the struct, not this paragraph.
///
/// Extracted from [`run_backtest`]'s bar arm (a pure code move) so the walk-forward runner
/// ([`crate::walkforward::runner::run_walkforward`]) gets IDENTICAL engine params instead of
/// re-deriving its own — the divergent-default bug shape this workspace keeps paying for. It is a
/// FACTORY, not a cached value, because [`EngineParams`] is not `Clone` (it can carry a
/// `Box<dyn PositionSizer>` and a properties closure), and the walk-forward runner needs a fresh
/// one per OOS window.
pub(crate) fn bar_engine_params(
    profile: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Result<EngineParams, HarnessError> {
    let venue = profile.data.default_venue();
    let series = profile.data.resolved_series();
    let symbols: Vec<String> = series.iter().map(|s| s.symbol.clone()).collect();

    // Opt-in fee SCHEDULE (G7). `None` leaves the flat `fee_rate` chain untouched; `validate`
    // already refused the two together.
    //
    // ⚠ `build_for(&series)`, NOT `build()`: `[engine.fee] kind = "venue"` resolves the venue's
    // fee LANE from the run's OWN series (a `.P` symbol is priced on the perp row, not the spot
    // one), and the slice is the only thing that knows which lane this run trades. `build()` is
    // the load-time shape door `BacktestProfile::refusals` drives, which cannot see the slice —
    // see `FeeCfg::build_for`. Every non-`venue` kind answers identically through either door.
    let fee_schedule = profile.engine.fee.as_ref().map(|f| f.build_for(&series)).transpose()?;
    // Opt-in binary-resolution settlement (G6). Built here (not in `validate`) because it reads
    // the winners sidecar relative to the profile's own directory and cross-checks the run's
    // actual symbols.
    let resolution = profile
        .engine
        .resolution
        .as_ref()
        .map(|r| r.build(profile.base_dir.as_deref(), &symbols))
        .transpose()?;
    let (resolution, resolution_end_ts) = match resolution {
        Some((src, end)) => (Some(src), end),
        None => (None, None),
    };
    // Opt-in position sizer (Task 12). `None` leaves `StrategyEngine::new`'s own
    // `PassThroughSizer` fallback untouched; `validate` already refused an unbuildable kind.
    let sizer = profile.engine.sizer.as_ref().map(|s| s.build()).transpose()?;

    // `default_venue` re-tags each bar's `symbol` to `SYMBOL.VENUE` (StrategyEngine::new's
    // frozen R2 behavior), which a symbol-inferring strategy (e.g. `buy_hold` reading
    // `bar.symbol`) can't route on. In bar mode the venue tag is ONLY needed as the
    // `properties_as_of` venue key, so set it just when snapping — otherwise leave bars on
    // their bare symbol so the common (no-snap) backtest "just works" without a magic
    // `strategy.params.symbol`.
    let mut params = EngineParams {
        cash: profile.engine.cash,
        fee_rate: profile.engine.fee_rate,
        fee_schedule,
        slippage: profile.engine.slippage,
        default_venue: profile.engine.snap_to_properties.then(|| venue.clone()),
        resolution,
        resolution_end_ts,
        // Optional pre-trade RiskGate limits (runprofile-wiring-step2): the SAME
        // `[risk]` → `RiskLimits` converter paper/live use. Absent `[risk]` ⇒ `None` ⇒
        // `SimBroker::build_risk_gate` never mounts a gate — byte-identical to before this
        // field existed. `validate` already refused a profile that sets
        // `max_orders_per_window` (meaningless wall-clock throttle in sim time), so no
        // further filtering is needed here.
        risk_limits: risk_limits_for(profile),
        timeframes: profile.engine.timeframes.clone(),
        cash_gate: profile.engine.cash_gate,
        // WHEN the cross-section is decided (`vike_sim::DecideMode`). `"simultaneous"` IMPLIES
        // `cash_gate`, and the engine reads the disjunction — so this line can turn the gated
        // admission phase on without the older flag being set, which is deliberate: an allocator
        // is what a cross-section is ordered IN. `refusals` has already refused the three
        // combinations that would make the mode a claim it cannot meet (tick lane, an armed
        // pre-trade gate, a detail tape).
        decide: super::profile::decide_mode(profile.engine.decide.as_deref())?,
        maint_margin: profile.engine.maint_margin,
        liq_buffer: profile.engine.liq_buffer,
        venue_style_liquidation: profile.engine.venue_style_liquidation,
        volume_limit: profile.engine.volume_limit,
        max_open_positions: profile.engine.max_open_positions,
        max_open_long: profile.engine.max_open_long,
        max_open_short: profile.engine.max_open_short,
        multiplier: profile.engine.multiplier,
        multipliers: profile.engine.multipliers.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        leverage: profile.engine.leverage,
        clamp_to_leverage: profile.engine.clamp_to_leverage,
        settlement_period_ms: profile.engine.settlement_period_ms,
        session_gate: profile.engine.session_gate,
        sizer,
        // See `EngineCfg::emulator_release_stops`'s doc: the harness default is `false` (matching
        // `EngineParams::default()`), and this is one of the two construction sites that must
        // assign it for a profile's explicit `true` to reach the engine.
        emulator_release_stops: profile.engine.emulator_release_stops,
        // The declared warm-up FLOOR (`data.warmup`). `None` leaves the gate at exactly
        // `Strategy::warmup()`, byte-identical; `Some(n)` raises it and can never lower it (see
        // `EngineParams::warmup`). Resolved through `DataCfg::warmup_steps`, which is also where
        // the refusals live — so a `"3mo"` or a duration over an unparseable `data.interval`
        // fails HERE rather than being silently rounded to something.
        //
        // ⚠ It is assigned at this site, which `run_backtest_over_bars` shares, so a walk-forward
        // window warms up per window — the right answer: each OOS window is a fresh engine over a
        // fresh slice, and its indicators are as cold at its first bar as the run's were at the
        // range start.
        warmup: profile.data.warmup_steps()?,
        ..Default::default()
    };
    if profile.engine.snap_to_properties {
        params.properties = Some(properties_source(store.clone()));
    }
    // Opt-in market-impact slippage. Absent = `params.impact` stays `None` = the flat-slippage
    // path, unchanged. The tick arm of `run_backtest` wires the SAME `ImpactCfg::build` — this
    // used to be the only wire-in because `validate` refused the knob in tick mode.
    if let Some(imp) = &profile.engine.impact {
        params.impact = Some(imp.build()?);
        params.impact_window = imp.window;
    }
    Ok(params)
}

/// Refuse a multi-symbol universe whose series are not the same length.
///
/// `StrategyEngine::new` carries `assert!(lengths.len() <= 1, ...)` — a plain `assert!`, so it
/// fires in release — and nothing between the store and it aligns anything. Left alone, a universe
/// whose members have different listing dates aborts the process AFTER the whole store scan is
/// paid for, with a message naming neither symbol.
///
/// This is the pre-flight the two existing lanes already perform: `hist_replay`'s
/// `replay_ticks_core` returns `ReplayError::MisalignedSeededBars`, and `vike_studio_core`'s
/// `load_slice_bars` returns `RunError::Data`. ⚠ It refuses rather than repairing: forward-fill and
/// intersection are a profile key in a later plan, and picking one here would silently change what
/// a run means.
fn refuse_ragged_series(loaded: &[(String, Vec<Bar>)]) -> Result<(), HarnessError> {
    if loaded.len() < 2 {
        return Ok(());
    }
    let first_len = loaded[0].1.len();
    if loaded.iter().all(|(_, s)| s.len() == first_len) {
        return Ok(());
    }
    let mut rows: Vec<String> =
        loaded.iter().map(|(sym, s)| format!("{sym}={}", s.len())).collect();
    rows.sort();
    Err(HarnessError::Data(format!(
        "the requested symbols returned series of different lengths ({}) — the bar engine requires \
         one row per symbol per step and refuses rather than guessing which rows to invent. A \
         member listed later than the window start, or a gap the store never filled, is the usual \
         cause; narrow the window to the range every symbol covers, or run them separately",
        rows.join(", ")
    )))
}

/// Load this profile's BAR series from `store`, one `load_bars` per RESOLVED series (a cross-venue
/// `[[data.series]]` slice reads each series from its own venue; the single-venue form expands to
/// the same thing), window-joining the stored market `funding` series when `attach_funding` is on.
///
/// Extracted from [`run_backtest`]'s bar arm (a pure code move) so the walk-forward runner loads
/// the SAME bars — funding join included — as a plain run of the same profile.
pub(crate) fn load_profile_bars(
    profile: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Result<Vec<(String, Vec<Bar>)>, HarnessError> {
    let range = profile.range()?;
    let loaded: Vec<(String, Vec<Bar>)> = profile
        .data
        .resolved_series()
        .iter()
        .map(|s| {
            let mut price = store
                .load_bars(&s.venue, &s.symbol, &profile.data.interval, range)
                .map_err(|e| HarnessError::Data(e.to_string()))?;
            // When `attach_funding` is on, the stored market `funding` series for that same
            // `(venue, symbol)` is loaded and WINDOW-JOINED onto the price bars — attaching each
            // rate only to the bar whose window contains the funding event, so the bar-loop accrual
            // charges once per interval, not once per bar (see `window_join_funding`).
            if profile.engine.attach_funding {
                attach_funding_series(store.as_ref(), &s.venue, &s.symbol, range, &mut price)?;
            }
            Ok((s.symbol.clone(), price))
        })
        .collect::<Result<Vec<_>, HarnessError>>()?;
    refuse_ragged_series(&loaded)?;
    Ok(loaded)
}

/// Load the opt-in INTRABAR DETAIL TAPE (`data.detail_interval`) — one finer series per resolved
/// symbol, over the SAME range as the coarse bars — in the shape
/// [`vike_sim::EngineParams::granular_by_symbol`] takes. EMPTY when the key is absent, which is the
/// byte-identical no-op: `StrategyEngine::new` buckets nothing and every symbol fills on the
/// coarse lane.
///
/// # What this door unlocks, and why the engine half already existed
///
/// The granular sub-bar lane (`StrategyEngine::fill_pending_granular`, and the per-coarse-step
/// bucketing in `StrategyEngine::new`) has been in the engine since the port and could be reached
/// by NO profile: nothing in `super::profile` assigned `granular_by_symbol`, so the finest fill
/// tier this engine has was live code with no door. That is why this is a loader rather than a
/// second replay engine — the ambiguity resolution item 2 asks for is what
/// `fill_pending_granular` already does by walking sub-bars in time order instead of handing the
/// triggered pair to `vike_fills::resolve_intrabar_fills` to order adverse-first.
///
/// # Three properties that are load-bearing rather than incidental
///
/// 1. **The symbol list is `DataCfg::resolved_series`, the same list the coarse load walks.**
///    `StrategyEngine::new` `expect`s that every `granular_by_symbol` key is a symbol of the run
///    (the third undocumented-abort panic recorded on that function), and building the two lists
///    from one source makes that panic unreachable from this door BY CONSTRUCTION rather than by
///    a check that could rot.
/// 2. **[`refuse_ragged_series`] is deliberately NOT applied.** The coarse series must be
///    length-aligned because the bar engine takes one row per symbol per step; the detail series
///    are bucketed per symbol INDEPENDENTLY and never indexed in lockstep, so two symbols whose
///    finer tapes have different lengths are correct rather than ragged.
/// 3. **An EMPTY detail series is refused, not skipped.** `StrategyEngine::new` has
///    `if subs.is_empty() { continue; }`, so a symbol the store holds no finer series for would
///    silently fall back to the coarse guessing lane while the operator believed the realism knob
///    was on — the same silence the lane-asymmetric `[engine]` refusals exist to stop, landing on
///    a per-symbol basis where it is harder to notice. It is an I/O-time refusal because nothing
///    load-time can know what the store holds (the shape `ResolutionCfg::build`'s winners-coverage
///    check already uses).
pub(super) fn load_profile_detail_bars(
    profile: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Result<Vec<(String, Vec<Bar>)>, HarnessError> {
    let Some(interval) = profile.data.detail_interval.as_deref() else {
        return Ok(Vec::new());
    };
    // The raw string is what the store is asked for, but the value is only legitimate if it
    // RESOLVES: the parse, the bar-mode-only rule and the strictly-finer-than-base comparison all
    // live on `DataCfg::detail_interval_ms`, and this call is where they are raised on the run
    // path. Discarding the millisecond answer is deliberate — the engine buckets by timestamp
    // against the coarse bars' own edges and needs no step size of its own.
    profile.data.detail_interval_ms()?;
    let range = profile.range()?;
    profile
        .data
        .resolved_series()
        .iter()
        .map(|s| {
            let subs = store
                .load_bars(&s.venue, &s.symbol, interval, range)
                .map_err(|e| HarnessError::Data(e.to_string()))?;
            if subs.is_empty() {
                return Err(HarnessError::Data(format!(
                    "data.detail_interval = {interval:?} but the store holds no {interval} bars \
                     for ({:?}, {:?}) in this range — the intrabar detail tape would silently \
                     fall back to the coarse stop-versus-target guess for that symbol alone, so \
                     the run is refused instead. Backfill the finer series, or drop the key",
                    s.venue, s.symbol
                )));
            }
            Ok((s.symbol.clone(), subs))
        })
        .collect()
}

/// Run one backtest profile end-to-end: resolve the strategy, load the configured data slice
/// from `store`, and run it (bar-mode `StrategyEngine` or tick-mode `replay_ticks`).
pub fn run_backtest(
    profile: &BacktestProfile,
    // `Arc<dyn HistStore>`, not the concrete `DataFusionHist`, so any store implementing the trait
    // can drive a single run — the `backtest` bin's `Arc::new(DataFusionHist)` coerces at the call
    // site, and its `--archive` arm feeds an archive-Parquet store through the SAME dispatcher.
    // (It was written for vike-backfill's `poly_ch_backtest` bin and that bin's
    // `ClickHousePolyHistStore`, the #691 ClickHouse-reading backtest bridge — both deleted, the bin
    // by #2030 on 2026-09-20; the seam is what outlived them, and an archive-Parquet store dropped
    // into it unchanged.) The sweep entrypoints
    // (`sweep`/`euler`) take the SAME `Arc<dyn HistStore + Send + Sync>` — their rayon fan-out only
    // clones the `Arc` and calls this dispatcher, needing nothing beyond `Send + Sync`, which the
    // trait object provides — so the whole harness compiles against the trait with no DataFusion
    // at DEFAULT features (the trait-only/`datafusion-store` split).
    store: Arc<dyn HistStore + Send + Sync>,
) -> Result<BacktestResult, HarnessError> {
    let strategy = strategy_by_name(&profile.strategy.name, &profile.strategy.params)?;
    let range = profile.range()?;
    let venue = profile.data.default_venue();
    let series = profile.data.resolved_series();
    let symbols: Vec<String> = series.iter().map(|s| s.symbol.clone()).collect();

    // ⚠ THE DATA PRE-FLIGHT, and it is placed here rather than first on purpose. Every refusal
    // above still fires before it, so this lane's existing error ORDER is untouched (the property
    // `run_backtest_over_bars`'s doc calls a contract) — and it cannot change what any profile
    // written before `data.require_coverage`/`data.universe` existed does, because
    // `crate::data_plan::enforce` reads NOTHING from the store unless one of them is armed.
    //
    // What an armed gate ends: a window with a complete trade tape and no book at all loads,
    // runs to completion and REPORTS FILLS. Nothing between the store and the engine notices —
    // per series both manifests look unremarkable, and `vike_data::find_gaps` is blind to the
    // commonest shape of it by construction (its holes are strictly inside a recorded span, so a
    // window that opens before the tape does has no gap). `crates/vike-data/src/store/coverage.rs`'s
    // module doc carries the measurement.
    //
    // `WARN` reaches the operator through `tracing::warn!` rather than a return value: this
    // function's contract is a `BacktestResult`, the walk-forward runner and every sweep trial call
    // it, and widening the signature to carry diagnostics would touch every one of them to deliver
    // a line that the log already delivers on both routes. The REFUSAL is the return value, which
    // is what a refusal should be.
    for line in crate::data_plan::enforce(profile, store.as_ref(), "the store this run opened")? {
        tracing::warn!("{line}");
    }

    match profile.data.kind {
        DataKind::Bar => {
            // Both halves of the bar lane live in shared helpers so the walk-forward runner
            // (`crate::walkforward::runner`) builds the SAME engine params and loads the SAME bars.
            // ⚠ Deliberately NOT delegating to `run_backtest_over_bars` below — see its doc: the
            // delegation would reorder this lane's errors.
            let mut params = bar_engine_params(profile, &store)?;
            let bars = load_profile_bars(profile, &store)?;
            // Opt-in intrabar detail tape (`data.detail_interval`) — EMPTY unless the key is set,
            // which is the byte-identical no-op. Assigned AFTER the two calls above so this lane's
            // existing error ORDER is untouched (see `run_backtest_over_bars`'s doc on why that
            // order is a contract), and assigned HERE rather than inside `bar_engine_params`
            // deliberately: that factory is shared with `run_backtest_over_bars`, whose caller is
            // a walk-forward window holding a SLICED coarse series. `StrategyEngine::new` buckets
            // a sub-bar by `partition_point(|e| e <= sub.ts) - 1`, so every sub-bar past the last
            // coarse bar lands in the LAST bucket — a whole-range tape against a windowed slice
            // would let the final step of every training window fill against the future. The
            // pairing is refused in `BacktestProfile::refusals` rather than relied on here, but
            // the wiring is kept where it cannot happen at all.
            params.granular_by_symbol = load_profile_detail_bars(profile, &store)?;
            Ok(StrategyEngine::new(bars, strategy, params).run())
        }
        DataKind::Tick => {
            // Opt-in fee SCHEDULE (G7). `None` leaves the flat `fee_rate` chain untouched;
            // `validate` already refused the two together. `build_for(&series)` for the same
            // reason the bar arm above uses it: the `kind = "venue"` LANE comes from this run's
            // own series.
            let fee_schedule =
                profile.engine.fee.as_ref().map(|f| f.build_for(&series)).transpose()?;
            // Opt-in binary-resolution settlement (G6). Built here (not in `validate`) because it
            // reads the winners sidecar relative to the profile's own directory and cross-checks
            // the run's actual symbols.
            let resolution = profile
                .engine
                .resolution
                .as_ref()
                .map(|r| r.build(profile.base_dir.as_deref(), &symbols))
                .transpose()?;
            let (resolution, resolution_end_ts) = match resolution {
                Some((src, end)) => (Some(src), end),
                None => (None, None),
            };
            // Opt-in position sizer (Task 12). Same wire-in as the bar arm above.
            let sizer = profile.engine.sizer.as_ref().map(|s| s.build()).transpose()?;
            let mut params = EngineParams {
                cash: profile.engine.cash,
                fee_rate: profile.engine.fee_rate,
                fee_schedule,
                slippage: profile.engine.slippage,
                resolution,
                resolution_end_ts,
                // See the bar-mode branch above for the rationale — same field, same converter.
                risk_limits: risk_limits_for(profile),
                // Opt-in FIFO queue-position fill (tick lane only — `validate` rejects it on bars).
                // When set, `StrategyEngine::new` builds a `QueueTracker` and `run_ticks` routes
                // resting limits — including tagged maker quotes — through the queue gate, so a quote
                // fills only once a taker trade consumes the size ahead of it. Absent ⇒ `None` ⇒ the
                // frozen simple-crossing fill, byte-identical.
                queue_model: profile.engine.queue_model_kind()?,
                queue_seed_depth: profile.engine.queue_seed_depth.unwrap_or(0.0),
                queue_min_hold_ms: profile.engine.queue_min_hold_ms.unwrap_or(0),
                // Opt-in ORDER latency (two legs): `order_latency_ms` is the ENTRY leg — every
                // strategy order action reaches the matching engine that late; `fill_latency_ms` is
                // the RESPONSE leg — the strategy LEARNS of a fill that late (its shadow position
                // lags), so it can't react inside the gap. Either > 0 arms the model; both `0` ⇒
                // `None` ⇒ zero-latency, byte-identical.
                latency_model: (profile.engine.order_latency_ms > 0
                    || profile.engine.fill_latency_ms > 0)
                    .then(|| {
                        vike_sim::LatencyModelKind::constant(
                            profile.engine.order_latency_ms.saturating_mul(1_000_000),
                            profile.engine.fill_latency_ms.saturating_mul(1_000_000),
                        )
                    }),
                // Equity-curve density (see `EngineCfg::equity_sample_every`): absent ⇒
                // `EveryTick` ⇒ the frozen per-tick curve, byte-identical. A sweep over a very
                // long tape sets it to stop paying 16 bytes/tick per run for samples it never
                // reads. `validate` already rejected it in bar mode.
                equity_sampling: profile.engine.equity_sampling(),
                // `timeframes` is deliberately absent: `validate` refuses it on this lane.
                // ⚠ `cash_gate` is refused on this lane too (`validate`), so the value forwarded
                // here is always `false` and `run_ticks` would not read it anyway — the gate is
                // `StrategyEngine::run`'s bar step alone. It is assigned rather than omitted so
                // `crates/vike-ops/tests/wiring/engine_cfg_reaches_the_engine.rs` keeps REQUIRING it at
                // both sites: an exemption row would stop checking this field here forever, and
                // forwarding a constant `false` costs nothing.
                cash_gate: profile.engine.cash_gate,
                // ...and `decide` for the SAME two reasons, spelled once next to its twin:
                // `"simultaneous"` is refused on this lane (`refusals`), so the value forwarded
                // here is always `DecideMode::Sequential` and `run_ticks` reads neither field
                // anyway — and it is assigned rather than omitted so the gate above keeps
                // REQUIRING it at both sites instead of taking a permanent exemption row.
                decide: super::profile::decide_mode(profile.engine.decide.as_deref())?,
                maint_margin: profile.engine.maint_margin,
                liq_buffer: profile.engine.liq_buffer,
                venue_style_liquidation: profile.engine.venue_style_liquidation,
                volume_limit: profile.engine.volume_limit,
                max_open_positions: profile.engine.max_open_positions,
                max_open_long: profile.engine.max_open_long,
                max_open_short: profile.engine.max_open_short,
                multiplier: profile.engine.multiplier,
                multipliers: profile
                    .engine
                    .multipliers
                    .iter()
                    .map(|(k, v)| (k.clone(), *v))
                    .collect(),
                leverage: profile.engine.leverage,
                clamp_to_leverage: profile.engine.clamp_to_leverage,
                settlement_period_ms: profile.engine.settlement_period_ms,
                session_gate: profile.engine.session_gate,
                sizer,
                // See `EngineCfg::emulator_release_stops`'s doc: the harness default is `false`
                // (matching `EngineParams::default()`), and this is the second of the two
                // construction sites that must assign it for a profile's explicit `true` to reach
                // the engine.
                emulator_release_stops: profile.engine.emulator_release_stops,
                // The declared warm-up FLOOR (`data.warmup`), assigned on BOTH lanes because
                // `run_ticks` gates on the same expression `run` does — its index counts EVENTS,
                // so `warmup = "200bars"` means 200 ticks here. `DataCfg::warmup_steps` refuses a
                // DURATION spelling on this lane rather than dividing it by a cadence a tick tape
                // does not have.
                warmup: profile.data.warmup_steps()?,
                ..Default::default()
            };

            // Opt-in depth-capped fill model (`fill_model = "l2book"`): a resting order fills only up
            // to the DISPLAYED book depth (insufficient depth ⇒ it rests), vs the default L1
            // spread-crossing model (fills the full size at the quote). Resolved STRICTLY by
            // `EngineCfg::fill_model_kind` — absent ⇒ `Tick`, an unknown spelling is a Validation
            // error (already caught by `validate` at load), never a silent optimistic-`Tick`
            // fallback. The tick-replay loader respects an explicit `L2Book` choice and forces
            // `Tick` otherwise, so assigning the resolved `Tick` default here is byte-identical.
            params.fill_model = profile.engine.fill_model_kind()?;

            // Opt-in market-impact slippage on the TICK lane (`validate` no longer refuses it
            // here — see the `[engine.impact]` block there for why the old "the tick lane replays
            // a real book" premise was only half true). Absent = `params.impact` stays `None` =
            // the flat-slippage path, byte-identical. The SAME `ImpactCfg::build` the bar arm
            // calls, so the two lanes cannot end up holding differently-configured models; which
            // TERMS each fill is charged is decided per fill by
            // `crates/vike-sim/src/engine/sim_broker.rs`'s `impact_terms`, not here.
            if let Some(imp) = &profile.engine.impact {
                params.impact = Some(imp.build()?);
                params.impact_window = imp.window;
            }

            let cfg = TickReplayConfig {
                venue: venue.clone(),
                symbols: symbols.clone(),
                series: (!profile.data.series.is_empty()).then(|| series.clone()),
                range,
                seed_bar_interval_ms: profile.engine.seed_bar_interval_ms,
                params,
                snap_to_properties: profile.engine.snap_to_properties,
                // Opt-in feed-latency delivery (tick lane only — `validate` rejects it on the bar
                // lane). Absent = `false` = today's venue-ordered replay, byte-identical.
                feed_latency: profile.engine.feed_latency,
            };

            replay_ticks(store, strategy, cfg).map_err(|e| HarnessError::Data(e.to_string()))
        }
    }
}

/// [`run_backtest`]'s BAR lane over bars the CALLER already holds — the seam a walk-forward window
/// needs, and the reason the search apparatus can be reused per window at all.
///
/// # Why this exists
///
/// Every candidate a search evaluates goes through `super::sweep`'s `point_row`, which calls
/// [`run_backtest`], which calls [`load_profile_bars`] — so scoring `G` candidates costs `G` loads
/// of the profile's whole range. A walk-forward window needs the opposite: score each candidate on
/// the TRAIN slice it is already holding. It cannot express that slice as a store range either,
/// because [`crate::walkforward::walk_forward_strategy`] splits by INDEX and deliberately reads no
/// `Bar.ts` at all — asking it for a range would mean teaching it a calendar it is built to avoid.
///
/// What this removes is the LOAD, not the store: `bar_engine_params` still takes it, because
/// `snap_to_properties` reads a properties grid from it. That read is skipped unless the profile
/// asks for it, so the per-candidate cost of this path is the engine run and nothing else.
///
/// # ⚠ Why [`run_backtest`]'s bar arm does NOT delegate here
///
/// It would reorder that lane's errors. `run_backtest` resolves the strategy FIRST, before it ever
/// touches the store, so a bad strategy name on a bar profile fails before a store failure can. A
/// delegating arm would have to load the bars before calling this, inverting that pair. The three
/// lines are assembly over shared helpers — `strategy_by_name`, `bar_engine_params`,
/// `StrategyEngine::new` — not a second implementation of anything, and `run_backtest` staying
/// byte-identical is what lets the walk-forward work claim it moved no existing number.
pub(crate) fn run_backtest_over_bars(
    profile: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    bars: Vec<(String, Vec<Bar>)>,
) -> Result<BacktestResult, HarnessError> {
    let strategy = strategy_by_name(&profile.strategy.name, &profile.strategy.params)?;
    let params = bar_engine_params(profile, store)?;
    Ok(StrategyEngine::new(bars, strategy, params).run())
}

/// Summary of one [`window_join_funding`] pass, for caller logging and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FundingJoinStats {
    /// price bars that received a funding rate (one per bar, even when several events collided).
    pub placed: usize,
    /// funding events that fell into a price bar window ALREADY holding an earlier event this pass
    /// — i.e. the price interval is COARSER than the funding cadence, so charges would collapse.
    /// Only the LAST event in such a window is kept; each extra one is counted here.
    pub collisions: usize,
    /// funding events whose ts predates the FIRST price bar's window (no home) — dropped.
    pub dropped_before: usize,
}

/// Window-join a sorted market `funding` event series onto the replayed PRICE bars: attach each
/// funding event's rate to the ONE price bar whose window `[bar.ts, next_bar.ts)` contains the
/// event ts (the last bar's window is unbounded above — `bar.ts <= f_ts`). A price bar with no
/// event in its window is left with `funding = None`.
///
/// **Never forward-fills.** The `SimBroker` bar-loop accrual (`engine.rs`) charges funding on
/// EVERY bar whose `Bar.funding.is_some()`, so spreading one rate across every bar until the next
/// event would charge every bar instead of once per 8h/1h funding interval — the exact
/// live-vs-backtest over-charge this join exists to prevent.
///
/// Both inputs MUST be sorted ascending by ts; the walk is a single two-pointer merge — O(n+m),
/// never a nested scan. `funding` is `(ts, rate)`. Mutates `price_bars` in place and returns a
/// [`FundingJoinStats`] the caller logs from.
///
/// # Price interval must be no coarser than the funding cadence
/// The join assumes at most one funding event per price bar window. If MORE than one lands in a
/// window (a price interval coarser than the funding cadence — e.g. 1d price bars over 8h
/// funding), only the LAST is kept (an earlier one would otherwise vanish silently) and each extra
/// one is counted in [`FundingJoinStats::collisions`] so the caller can warn loudly. Use a price
/// interval at least as fine as the funding cadence to avoid collapsing charges.
pub fn window_join_funding(price_bars: &mut [Bar], funding: &[(i64, f64)]) -> FundingJoinStats {
    let mut stats = FundingJoinStats::default();
    let mut fi = 0usize; // two-pointer cursor into `funding`, advanced monotonically
    let n = price_bars.len();
    for pi in 0..n {
        let bar_ts = price_bars[pi].ts;
        // `None` for the last bar => its window catches every remaining event (unbounded above).
        let next_ts = price_bars.get(pi + 1).map(|b| b.ts);

        // Drop any events sitting BEFORE this bar's window start. Only reachable at `pi == 0`:
        // an event earlier than the first price bar belongs to no window. For `pi > 0` every
        // earlier event was already consumed by a prior (contiguous) window, so this never fires
        // past the head.
        while fi < funding.len() && funding[fi].0 < bar_ts {
            stats.dropped_before += 1;
            fi += 1;
        }

        // Collect every event in `[bar_ts, next_ts)` (or `[bar_ts, ∞)` for the last bar). The LAST
        // one wins; any earlier collision is counted, not silently overwritten.
        let mut chosen: Option<f64> = None;
        while fi < funding.len() {
            let (f_ts, rate) = funding[fi];
            let in_window = match next_ts {
                Some(nts) => f_ts < nts,
                None => true,
            };
            if !in_window {
                break;
            }
            if chosen.is_some() {
                stats.collisions += 1;
            }
            chosen = Some(rate);
            fi += 1;
        }
        if let Some(rate) = chosen {
            price_bars[pi].funding = Some(rate);
            stats.placed += 1;
        }
    }
    stats
}

/// Load the stored market `funding` series for `(venue, symbol)` and window-join it onto `price`
/// (see [`window_join_funding`]). The funding series is read under the cadence-agnostic `"funding"`
/// interval label the #761 market funding-rate backfill writes; each returned `Bar` carries
/// `funding = Some(rate)` at its funding-event ts. An empty/absent funding series leaves `price`
/// unchanged (a warning, never an error). Collisions / pre-first-bar drops are logged loudly.
fn attach_funding_series(
    store: &(dyn HistStore + Send + Sync),
    venue: &str,
    symbol: &str,
    range: vike_data::TsRange,
    price: &mut [Bar],
) -> Result<(), HarnessError> {
    let fbars = store
        .load_bars(venue, symbol, "funding", range)
        .map_err(|e| HarnessError::Data(e.to_string()))?;
    // Each source bar's `funding` IS the rate at its ts; the store returns them ts-ascending, the
    // same sorted contract `window_join_funding` and the price load both rely on. Element type
    // `(i64, f64)` is inferred from the `window_join_funding` call below.
    let events: Vec<_> = fbars.iter().filter_map(|b| b.funding.map(|r| (b.ts, r))).collect();
    if events.is_empty() {
        tracing::warn!(
            venue,
            symbol,
            "engine.attach_funding is on but the stored `funding` series is empty/absent — bars \
             left unchanged (no funding accrual for this series)"
        );
        return Ok(());
    }
    let stats = window_join_funding(price, &events);
    if stats.collisions > 0 {
        tracing::warn!(
            venue,
            symbol,
            collisions = stats.collisions,
            "engine.attach_funding: {} funding event(s) collided into a coarser price-bar window \
             and only the last was kept — use a price interval at least as fine as the funding \
             cadence to avoid collapsing charges",
            stats.collisions
        );
    }
    if stats.dropped_before > 0 {
        tracing::warn!(
            venue,
            symbol,
            dropped = stats.dropped_before,
            "engine.attach_funding: {} funding event(s) predated the first price bar and were \
             dropped (no window to attach to)",
            stats.dropped_before
        );
    }
    Ok(())
}

/// `risk_limits_for` needs no store at all — a plain DEFAULT build (no `datafusion-store`)
/// still proves the struct-level half of the byte-identical-default claim: absent `[risk]` maps
/// to `None`, present maps to the SAME `ProfileRisk::to_risk_limits` conversion the field-level
/// `harness::profile` tests already pin. The behavioral half (a real `SimBroker` denial) lives in
/// `tests/harness_risk_wiring.rs`, which needs only a fixture `HistStore`, not `DataFusionHist`.
#[cfg(test)]
mod risk_limits_for_tests;

/// `refuse_ragged_series` needs no store at all — a plain DEFAULT build (no
/// `datafusion-store`) still proves this guard, unlike the store-backed `tests` module below.
#[cfg(test)]
mod align_tests;

// Every test here builds a concrete `DataFusionHist` fixture, so the whole module is behind
// `datafusion-store` — the DEFAULT (trait-only) build compiles `run_backtest` (against the seam)
// but none of these store-backed tests.
#[cfg(all(test, feature = "datafusion-store"))]
mod tests;
