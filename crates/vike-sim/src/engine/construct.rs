//! Construction: `StrategyEngine::new` builds the `SimBroker` core, the per-symbol state and the opt-in lanes from `EngineParams`.

use std::rc::Rc;

use indexmap::IndexMap;
use vike_marketdata::Bar;
use vike_model::{FeeSchedule, Position, Strategy};
use vike_venues::SessionCalendar;

use super::{EngineParams, SimBroker, StrategyEngine, SymbolState, format_instrument};
use crate::queue_model::QueueTracker;
use crate::schedule::Schedule;
use crate::timeframe::{parse_timeframe, resample};
use vike_analytics::sizing::PassThroughSizer;

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    pub fn new(bars_by_symbol: Vec<(String, Vec<Bar>)>, strategy: S, p: EngineParams) -> Self {
        let symbols: Vec<String> = bars_by_symbol.iter().map(|(s, _)| s.clone()).collect();
        // Pre-tag each bar with its instrument id once at construction.
        let bars: Vec<Rc<Vec<Bar>>> = bars_by_symbol
            .iter()
            .map(|(s, series)| {
                Rc::new(
                    series
                        .iter()
                        .map(|b| {
                            let mut nb = b.clone();
                            nb.symbol = Some(format_instrument(p.default_venue.as_deref(), s));
                            nb
                        })
                        .collect(),
                )
            })
            .collect();
        let lengths: std::collections::BTreeSet<usize> = bars.iter().map(|v| v.len()).collect();
        // ⚠ This assert is a LAST line of defence, not the refusal an operator sees.
        // `harness::run::refuse_ragged_series` catches a ragged universe at load and names both
        // symbols and both lengths. Reaching HERE means a caller bypassed the harness loader, so
        // the message stays terse on purpose — it is addressed to a developer, not a user.
        assert!(lengths.len() <= 1, "all symbol series must have the same length (aligned)");
        let n = lengths.into_iter().next().unwrap_or(0);

        let mut sym: Vec<SymbolState> = bars_by_symbol
            .iter()
            .map(|(_, series)| SymbolState {
                pos: Position::default(),
                pending: Vec::new(),
                realized: 0.0,
                stop: None,
                entry_fee: 0.0,
                entry_ts: 0,
                price: if n > 0 { series[0].open } else { 0.0 },
                hi_since: 0.0,
                lo_since: f64::INFINITY,
                tf: Vec::new(),
                sub: vec![Vec::new(); n],
                tagged: IndexMap::new(),
                settled_profit: 0.0,
            })
            .collect();
        // per-symbol higher-TF aggregates
        for (si, st) in sym.iter_mut().enumerate() {
            for tf in &p.timeframes {
                let ms = parse_timeframe(tf).expect("valid timeframe");
                st.tf.push((tf.clone(), ms, resample(&bars[si], ms)));
            }
        }
        // granular sub-bars bucketed into their coarse step
        //
        // ⚠ A THIRD undocumented-abort panic lives here, the same shape as the two recorded on
        // `crates/vike-sim/src/engine/sim_broker.rs`'s module doc (`SimBroker::idx` on an
        // unknown symbol; `crates/vike-sim/src/bar_buffer.rs`'s `BarSeriesBuffer`): a
        // `p.granular_by_symbol` entry
        // naming a symbol not present in this run's `symbols` aborts the process at the
        // `expect("granular symbol unknown")` below, rather than returning a named error.
        //
        // ⚠ **A PROFILE CAN NOW REACH THIS FIELD, and this comment said it could not.** The
        // `data.detail_interval` key (the intrabar detail tape) loads it through
        // `crates/vike-backtest/src/harness/run.rs`'s `load_profile_detail_bars`. The panic stays
        // unreachable FROM THAT DOOR by CONSTRUCTION rather than by a check: that loader builds
        // its entries from `DataCfg::resolved_series`, the same list the coarse `load_profile_bars`
        // walks, so a key it emits is a symbol of the run by definition and no comparison exists to
        // rot. What is still open is a DIRECT `EngineParams` caller — a test, a bin, a future
        // loader that assembles the pair from two sources — for which this remains a process abort
        // where `timeframes` gives a named refusal. Converting it means `StrategyEngine::new`
        // returning a `Result`, which is a signature change across every construction site in the
        // tree; it has not been done, and a second profile key that reaches this field must not be
        // written on the assumption that it was.
        for (gs, subs) in &p.granular_by_symbol {
            if subs.is_empty() {
                continue;
            }
            let si = symbols.iter().position(|s| s == gs).expect("granular symbol unknown");
            let edges: Vec<i64> = bars[si].iter().map(|b| b.ts).collect();
            let mut sorted_subs: Vec<Bar> = subs.clone();
            sorted_subs.sort_by_key(|b| b.ts);
            for sub in sorted_subs {
                // bisect_right(edges, sub.ts) - 1
                let i = edges.partition_point(|&e| e <= sub.ts) as i64 - 1;
                if i < 0 {
                    continue; // precedes the first coarse bar
                }
                sym[si].sub[i as usize].push(sub);
            }
        }
        let now = if !symbols.is_empty() && n > 0 { bars[0][0].ts } else { 0 };
        // Opt-in queue-position tracker (tick/book replay only): built ONCE from the Copy
        // config fields; `None` (the default) means the tracker never exists — byte-identical.
        let queue = p
            .queue_model
            .map(|k| QueueTracker::new(k, p.queue_seed_depth, p.queue_min_hold_ms, symbols.len()));
        // Resolve each symbol's contract multiplier ONCE (index si -> f64) so hot paths (equity_now,
        // per-symbol curve, apply_fill, liquidation) index a Vec instead of scanning `mult` by string
        // on every call. Byte-identical to multiplier_of()'s first-match-else-default lookup — matters
        // for a mixed futures/stocks/tokens book with many per-symbol multipliers.
        let mult_by_si: Vec<f64> = symbols
            .iter()
            .map(|s| {
                p.multipliers
                    .iter()
                    .find(|(name, _)| name == s)
                    .map(|(_, m)| *m)
                    .unwrap_or(p.multiplier)
            })
            .collect();
        let n_sym_resolved = symbols.len(); // captured before `symbols` moves into the struct
        // Per-symbol session calendars for the opt-in gate (see `EngineParams::session_gate`).
        // EMPTY unless the gate is on, so the default fill path pays one `Vec::get`/`is_empty`
        // branch and never indexes (mirrors `last_print_ts`). The KEY is per SYMBOL — each resolves
        // to its per-symbol override, else the venue-only `default_venue` calendar, else the
        // fail-permissive always-open row — which is the fix for one `default_venue` per run.
        let session: Vec<Option<SessionCalendar>> = if p.session_gate {
            let venue_default = p.default_venue.as_deref().map(vike_venues::session_for);
            symbols.iter().map(|s| p.session_calendars.get(s).copied().or(venue_default)).collect()
        } else {
            Vec::new()
        };
        // THE ONE JUDGE (deny-vs-clamp phase 2): mount the literal live RiskGate when armed —
        // `None` (no leverage, no risk_limits, or the clamp escape hatch) keeps the submit path
        // byte-identical to the pre-phase-2 engine. See `SimBroker::build_risk_gate`.
        let risk_gate = SimBroker::build_risk_gate(&p);
        // Fee-schedule bridge (fee model follow-up 3): `Some` OVERRIDES the flat
        // `maker_fee`/`taker_fee`/`fee_rate` chain with the venue schedule's own
        // `maker_taker_rates()` fractions; `None` (default) is the frozen precedence, untouched.
        // See `EngineParams::fee_schedule`.
        let (maker_fee, taker_fee) = match p.fee_schedule {
            Some(schedule) => schedule.maker_taker_rates(),
            None => (p.maker_fee.unwrap_or(p.fee_rate), p.taker_fee.unwrap_or(p.fee_rate)),
        };
        // ...with the exceptions, and ⚠ **THE ROUTING IS THE OTHER WAY ROUND NOW**: the two shapes
        // that may FLATTEN are NAMED here, and every other shape is carried to the fill site
        // verbatim and applied through `FeeSchedule::commission` at the price the fill actually
        // transacted at. It read the other way — `ProbabilityScaled` named, everything else
        // flattened — and the inversion is a SAFETY property rather than a tidy-up, because
        // `maker_taker_rates()` answers `(0.0, 0.0)` for every shape it cannot express: under the
        // old routing a `PerShareWithFloor` schedule (the IBKR per-share + per-order MINIMUM +
        // notional cap) charged exactly ZERO while looking configured, which is the same
        // prediction-market misreport port backlog G7 named, wearing the equities shape. Naming
        // the flat side means a shape ADDED to `FeeSchedule` later defaults to being charged
        // exactly rather than to being charged nothing.
        //
        // What the two named arms are, and why flattening them is not a loss:
        //   * `PercentMakerTaker` — a flat fraction of notional IS the whole shape, and
        //     `commission` computes `((qty*px) * (bps/1e4))`, which is `fee(size, price, rate,
        //     mult)`'s own associativity with `rate = bps/1e4`. Identical arithmetic, one fewer
        //     per-fill branch.
        //   * `Free` — zero either way.
        // `PercentOfUnderlying` moved to the exact side and is byte-identical on every real
        // Deribit configuration: `commission` is `min(bps × premium, cap × premium)` and
        // `bps/1e4 ≪ cap`, so the `min` is a no-op there and the product is the same expression.
        // It matters only where the cap genuinely binds — which the flat fraction could not
        // express at all, and understating a fee is the dangerous direction.
        let fee_curve = match p.fee_schedule {
            None | Some(FeeSchedule::PercentMakerTaker { .. } | FeeSchedule::Free) => None,
            Some(exact) => Some(exact),
        };
        // The cross-section's walk order, resolved ONCE here (before `symbols` moves into the
        // struct) because `symbols` is fixed for the life of the engine. Identity under the
        // default `DecideMode::Sequential`, so every fold routed through
        // `SimBroker::cross_section` sums exactly the bits it summed before the field existed.
        let cross_section = p.decide.cross_section_order(&symbols);
        let core = SimBroker {
            symbols,
            bars,
            n,
            fee_rate: p.fee_rate,
            cash: p.cash,
            funding_paid: 0.0,
            slippage: p.slippage,
            maker_fee,
            taker_fee,
            fee_curve,
            multiplier: p.multiplier,
            mult: p.multipliers,
            mult_by_si,
            leverage: p.leverage,
            clamp_to_leverage: p.clamp_to_leverage,
            risk_gate,
            maint_margin: p.maint_margin,
            venue_style_liquidation: p.venue_style_liquidation,
            liq_buffer: p.liq_buffer,
            cash_gate: p.cash_gate,
            decide: p.decide,
            cross_section,
            active_mask: p.active_mask,
            max_open_positions: p.max_open_positions,
            max_open_long: p.max_open_long,
            max_open_short: p.max_open_short,
            sizer: p.sizer.unwrap_or_else(|| Box::new(PassThroughSizer)),
            volume_limit: p.volume_limit.filter(|v| *v != 0.0),
            equity_peak: p.cash,
            step: 0,
            dropped: Vec::new(),
            below_min_reversals: 0,
            trades: Vec::new(),
            intrabar_both_hit: 0,
            sym,
            now,
            fill_model: p.fill_model,
            emulator_release_stops: p.emulator_release_stops,
            index: 0,
            schedule: Schedule::new(),
            mirror_fills: if p.mirror { Some(Vec::new()) } else { None },
            mirror_funding: if p.mirror { Some(Vec::new()) } else { None },
            properties: p.properties.clone(),
            funding_source: p.funding_source.clone(),
            venue: p.default_venue.clone(),
            // The per-symbol resolved latch is allocated ONLY with a resolution OR option-expiry
            // source (both settle terminally and share the latch), so the default order-intake path
            // stays a single `is_empty` branch (see `push_pending`).
            resolved: if p.resolution.is_some() || p.option_specs.is_some() {
                vec![false; n_sym_resolved]
            } else {
                Vec::new()
            },
            resolution: p.resolution,
            resolution_end_ts: p.resolution_end_ts,
            // Allocate-only-when-configured, like `resolved` above: no source, no latch.
            option_no_underlying_reported: if p.option_specs.is_some() {
                vec![false; n_sym_resolved]
            } else {
                Vec::new()
            },
            option_specs: p.option_specs,
            option_expiry_end_ts: p.option_expiry_end_ts,
            settlements: Vec::new(),
            settlement_period_ms: p.settlement_period_ms,
            settle_bucket: None,
            variation_settlements: Vec::new(),
            // Same allocate-only-when-configured trick as `resolved` above: with the wait
            // discipline off this stays EMPTY and the fill lanes never index it (see
            // `SimBroker::defer_stale`), keeping the default path byte-identical.
            last_print_ts: if p.max_price_staleness_ms.is_some() {
                vec![None; n_sym_resolved]
            } else {
                Vec::new()
            },
            max_price_staleness_ms: p.max_price_staleness_ms,
            stale_deferrals: 0,
            // Resolved ONCE per symbol at construction (see the `session` let-binding above); the
            // fill lanes then pay a single `Vec::get` per event, no string match — see
            // `EngineParams::session_gate` and `SimBroker::session_closed`.
            session,
            session_deferrals: 0,
            impact: p.impact,
            impact_window: p.impact_window,
            // sized by `run_ticks` only when an impact model is configured (see there); EMPTY
            // here and on every bar/vector path, which is what keeps `slippage_for`'s bar-window
            // read the frozen line
            impact_ticks: Vec::new(),
            slippage_saturations: 0,
            impact_unpriced: 0,
            // The realised maker/taker mix and the realised commission total — see the field docs
            // on `SimBroker`; both start at zero and are folded at the charge site.
            maker_fills: 0,
            taker_fills: 0,
            fees_paid: 0.0,
            latency: None, // armed by `run_ticks` only (see `StrategyEngine::latency`)
            shadow_pos: Vec::new(), // sized at arm time, cleared at disarm (see `run_ticks`)
            books: Vec::new(), // sized by `run_ticks` only when the tape carries book events
        };
        // The warm-up gate, resolved ONCE (see the `warmup` field doc). `max`, not replace: an
        // outside floor may RAISE what the strategy declared and may never lower it, so a caller
        // passing a smaller number than `strategy.warmup()` changes nothing — which is what makes
        // `EngineParams::warmup` safe to hand an operator's profile key.
        let warmup = p.warmup.unwrap_or(0).max(strategy.warmup());
        StrategyEngine {
            core,
            strategy,
            queue,
            latency: p.latency_model,
            deferred_fills: Vec::new(),
            equity_sampling: p.equity_sampling,
            warmup,
        }
    }
}
