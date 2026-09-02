//! `run_backtest` — the harness run dispatcher (Task 3): loads a [`super::BacktestProfile`]'s
//! data slice from an `Arc<dyn HistStore>` (any backend: `DataFusionHist` in prod, a ClickHouse
//! store in `poly_ch_backtest`, `MemHistStore` in tests) and runs the resolved strategy in bar-mode
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

use std::sync::Arc;

use vike_data::HistStore;
use vike_exec::RiskLimits;
use vike_model::Bar;

use super::{strategy_by_name, BacktestProfile, DataKind, HarnessError};
use crate::{
    properties_source, replay_ticks, BacktestResult, EngineParams, StrategyEngine, TickReplayConfig,
};

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

/// The BAR-mode [`EngineParams`] a profile describes — the whole `[engine]` surface the bar lane
/// honors (`cash`/`fee_rate`/the `fee` SCHEDULE/`slippage`/`[risk]`/`[engine.resolution]`/
/// `[engine.impact]`/`snap_to_properties`), built in one place.
///
/// Extracted from [`run_backtest`]'s bar arm (a pure code move) so the walk-forward runner
/// ([`super::walkforward::run_walkforward`]) gets IDENTICAL engine params instead of re-deriving
/// its own — the divergent-default bug shape this workspace keeps paying for. It is a FACTORY, not
/// a cached value, because [`EngineParams`] is not `Clone` (it can carry a `Box<dyn PositionSizer>`
/// and a properties closure), and the walk-forward runner needs a fresh one per OOS window.
pub(super) fn bar_engine_params(
    profile: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Result<EngineParams, HarnessError> {
    let venue = profile.data.default_venue();
    let symbols: Vec<String> =
        profile.data.resolved_series().iter().map(|s| s.symbol.clone()).collect();

    // Opt-in fee SCHEDULE (G7). `None` leaves the flat `fee_rate` chain untouched; `validate`
    // already refused the two together.
    let fee_schedule = profile.engine.fee.as_ref().map(|f| f.build()).transpose()?;
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
        ..Default::default()
    };
    if profile.engine.snap_to_properties {
        params.properties = Some(properties_source(store.clone()));
    }
    // Opt-in market-impact slippage (bar mode only — `validate` rejects it on the tick
    // lane). Absent = `params.impact` stays `None` = the flat-slippage path, unchanged.
    if let Some(imp) = &profile.engine.impact {
        params.impact = Some(imp.build()?);
        params.impact_window = imp.window;
    }
    Ok(params)
}

/// Load this profile's BAR series from `store`, one `load_bars` per RESOLVED series (a cross-venue
/// `[[data.series]]` slice reads each series from its own venue; the single-venue form expands to
/// the same thing), window-joining the stored market `funding` series when `attach_funding` is on.
///
/// Extracted from [`run_backtest`]'s bar arm (a pure code move) so the walk-forward runner loads
/// the SAME bars — funding join included — as a plain run of the same profile.
pub(super) fn load_profile_bars(
    profile: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Result<Vec<(String, Vec<Bar>)>, HarnessError> {
    let range = profile.range()?;
    profile
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
        .collect()
}

/// Run one backtest profile end-to-end: resolve the strategy, load the configured data slice
/// from `store`, and run it (bar-mode `StrategyEngine` or tick-mode `replay_ticks`).
pub fn run_backtest(
    profile: &BacktestProfile,
    // `Arc<dyn HistStore>`, not the concrete `DataFusionHist`, so any store implementing the trait
    // can drive a single run — the `backtest` bin's `Arc::new(DataFusionHist)` coerces at the call
    // site, and vike-backfill's `poly_ch_backtest` bin feeds a `ClickHousePolyHistStore` (the #691
    // ClickHouse-reading backtest bridge) through the SAME dispatcher. The sweep entrypoints
    // (`sweep`/`euler`) take the SAME `Arc<dyn HistStore + Send + Sync>` — their rayon fan-out only
    // clones the `Arc` and calls this dispatcher, needing nothing beyond `Send + Sync`, which the
    // trait object provides — so the whole harness compiles against the trait with no DataFusion
    // (the `hist-replay`/`datafusion-store` split).
    store: Arc<dyn HistStore + Send + Sync>,
) -> Result<BacktestResult, HarnessError> {
    let strategy = strategy_by_name(&profile.strategy.name, &profile.strategy.params)?;
    let range = profile.range()?;
    let venue = profile.data.default_venue();
    let series = profile.data.resolved_series();
    let symbols: Vec<String> = series.iter().map(|s| s.symbol.clone()).collect();

    match profile.data.kind {
        DataKind::Bar => {
            // Both halves of the bar lane live in shared helpers so the walk-forward runner
            // (`super::walkforward`) builds the SAME engine params and loads the SAME bars.
            let params = bar_engine_params(profile, &store)?;
            let bars = load_profile_bars(profile, &store)?;
            Ok(StrategyEngine::new(bars, strategy, params).run())
        }
        DataKind::Tick => {
            // Opt-in fee SCHEDULE (G7). `None` leaves the flat `fee_rate` chain untouched;
            // `validate` already refused the two together.
            let fee_schedule = profile.engine.fee.as_ref().map(|f| f.build()).transpose()?;
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
                        crate::latency::LatencyModelKind::constant(
                            profile.engine.order_latency_ms.saturating_mul(1_000_000),
                            profile.engine.fill_latency_ms.saturating_mul(1_000_000),
                        )
                    }),
                // Equity-curve density (see `EngineCfg::equity_sample_every`): absent ⇒
                // `EveryTick` ⇒ the frozen per-tick curve, byte-identical. A sweep over a very
                // long tape sets it to stop paying 16 bytes/tick per run for samples it never
                // reads. `validate` already rejected it in bar mode.
                equity_sampling: profile.engine.equity_sampling(),
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

/// `risk_limits_for` needs no store at all — a plain `hist-replay` build (no `datafusion-store`)
/// still proves the struct-level half of the byte-identical-default claim: absent `[risk]` maps
/// to `None`, present maps to the SAME `ProfileRisk::to_risk_limits` conversion the field-level
/// `harness::profile` tests already pin. The behavioral half (a real `SimBroker` denial) lives in
/// `tests/harness_risk_wiring.rs`, which needs only a fixture `HistStore`, not `DataFusionHist`.
#[cfg(test)]
mod risk_limits_for_tests {
    use super::*;

    const BASE_TOML: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
"#;

    #[test]
    fn none_when_risk_section_absent() {
        let profile = BacktestProfile::from_toml_str(BASE_TOML).unwrap();
        assert!(profile.risk.is_none());
        assert!(
            risk_limits_for(&profile).is_none(),
            "absent `[risk]` must map to `None`, not an armed-but-empty gate"
        );
    }

    #[test]
    fn some_matching_to_risk_limits_when_risk_section_present() {
        let toml =
            format!("{BASE_TOML}\n[risk]\nmax_notional_per_order = 5000.0\nmax_leverage = 3.0\n");
        let profile = BacktestProfile::from_toml_str(&toml).unwrap();
        let risk = profile.risk.as_ref().expect("`[risk]` configured");
        let got = risk_limits_for(&profile).expect("Some when `[risk]` is present");
        assert_eq!(got, risk.to_risk_limits(), "must use the SAME converter, not a re-derived one");
        assert_eq!(got.max_notional_per_order, Some(5000.0));
        assert_eq!(got.max_leverage, Some(3.0));
        assert_eq!(got.im_requirement, Some(1.0 / 3.0), "3x arms the buying-power check at 1/3");
    }
}

// Every test here builds a concrete `DataFusionHist` fixture, so the whole module is behind
// `datafusion-store` — a trait-only `hist-replay` build compiles `run_backtest` (against the seam)
// but none of these store-backed tests.
#[cfg(all(test, feature = "datafusion-store"))]
mod tests {
    use super::*;

    // The concrete store the tests build; the non-test code drives `run_backtest` through the
    // `Arc<dyn HistStore>` seam, so this import belongs to the tests alone.
    use vike_data::DataFusionHist;
    use vike_model::{QuoteTick, SymbolProperties};

    const VENUE: &str = "binance";
    const SYMBOL: &str = "BTCUSDT";

    fn bar(ts: i64, price: f64) -> Bar {
        Bar {
            ts,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn quote(ts: i64, bid: f64, ask: f64) -> QuoteTick {
        QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: String::new() }
    }

    /// `strategy.params.symbol` is set explicitly (rather than relying on `BuyHold`'s `bar.symbol`
    /// fallback): bar-mode always sets `EngineParams.default_venue`, which re-tags each bar's
    /// `symbol` field as `"SYMBOL.VENUE"` (`format_instrument`) for filter-grid lookups —a
    /// different string from the bare symbol key `SimBroker` indexes positions by (see
    /// `tests/filters_fills.rs`'s `OpenClose` doc comment). An explicit `symbol` param sidesteps
    /// that mismatch identically to how a real strategy would.
    fn bar_profile(kind: &str, extra_engine: &str) -> String {
        format!(
            r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "{kind}"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0
{extra_engine}

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "{SYMBOL}"
"#
        )
    }

    #[test]
    fn bar_mode_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0)];
        store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

        let profile = BacktestProfile::from_toml_str(&bar_profile("bar", "")).unwrap();
        let result = run_backtest(&profile, store).unwrap();

        // BuyHold opens a 1.0-unit position on the first bar and holds — no CLOSED trade, but
        // final_equity tracks the mark against the last bar's close (100.0 cash spent -> equity
        // re-marked at 102.0), so equity has moved off the starting cash.
        assert_ne!(result.final_equity, 1000.0, "buy_hold should open a position and move equity");
    }

    #[test]
    fn bar_mode_no_symbol_param_runs_without_panic() {
        // The footgun fix: a plain bar-mode profile (no `snap_to_properties`) must NOT set
        // `default_venue`, so bars keep their BARE symbol and `buy_hold` routes on `bar.symbol`
        // with no magic `strategy.params.symbol`. Before the fix this panicked ("unknown symbol").
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0)];
        store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

        let toml = format!(
            r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
"#
        );
        let profile = BacktestProfile::from_toml_str(&toml).unwrap();
        let result = run_backtest(&profile, store).unwrap();
        assert_ne!(
            result.final_equity, 1000.0,
            "buy_hold ran and opened a position (no symbol param)"
        );
    }

    #[test]
    fn tick_mode_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let quotes = vec![quote(0, 10.0, 11.0), quote(1000, 12.0, 13.0)];
        store.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();

        let profile = BacktestProfile::from_toml_str(&bar_profile("tick", "")).unwrap();
        let result = run_backtest(&profile, store).unwrap();

        assert_ne!(result.final_equity, 1000.0, "buy_hold should open a position and move equity");
    }

    #[test]
    fn tick_snap_to_properties_gates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let quotes = vec![quote(0, 10.0, 11.0), quote(1000, 12.0, 13.0)];
        store.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();
        store
            .append_symbol_properties(
                VENUE,
                SYMBOL,
                &[(0, SymbolProperties { min_qty: 1e9, ..Default::default() })],
                None,
            )
            .unwrap();

        // snap ON: the opening buy (size 1.0) is below the recorded min_qty -> gated -> no fill.
        let profile_on =
            BacktestProfile::from_toml_str(&bar_profile("tick", "snap_to_properties = true"))
                .unwrap();
        let result_on = run_backtest(&profile_on, store.clone()).unwrap();
        assert_eq!(result_on.final_equity, 1000.0, "snap-on must gate the sub-min_qty opening buy");

        // snap OFF (default): raw replay, the buy fills.
        let profile_off =
            BacktestProfile::from_toml_str(&bar_profile("tick", "snap_to_properties = false"))
                .unwrap();
        let result_off = run_backtest(&profile_off, store).unwrap();
        assert_ne!(result_off.final_equity, 1000.0, "snap-off must fill the opening buy");
    }

    #[test]
    fn bar_snap_to_properties_gates() {
        // Bar-mode snapping is a DISTINCT path from tick: `run_backtest` sets `params.properties`
        // directly (bars bypass replay_ticks) AND sets `default_venue` (needed as the properties
        // venue key), so `bar_profile` supplies the explicit `strategy.params.symbol`.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0)];
        store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();
        store
            .append_symbol_properties(
                VENUE,
                SYMBOL,
                &[(0, SymbolProperties { min_qty: 1e9, ..Default::default() })],
                None,
            )
            .unwrap();

        // snap ON: the opening buy (size 1.0) is below the recorded min_qty → gated → no fill.
        let profile_on =
            BacktestProfile::from_toml_str(&bar_profile("bar", "snap_to_properties = true"))
                .unwrap();
        let result_on = run_backtest(&profile_on, store.clone()).unwrap();
        assert_eq!(result_on.final_equity, 1000.0, "bar snap-on must gate the sub-min_qty open");

        // snap OFF: raw fill.
        let profile_off =
            BacktestProfile::from_toml_str(&bar_profile("bar", "snap_to_properties = false"))
                .unwrap();
        let result_off = run_backtest(&profile_off, store).unwrap();
        assert_ne!(result_off.final_equity, 1000.0, "bar snap-off must fill the open");
    }
}
