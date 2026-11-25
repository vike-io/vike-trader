//! Construction: `StrategyEngine::new` builds the `OrderBrokerSimulator` core, the per-symbol state and the opt-in lanes from `StrategyEngineConfig`.

use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use vike_marketdata::{Bar, resample};
use vike_model::{ContractValue, FeeSchedule, NotionalKind, Position, Strategy};
use vike_venues::SessionCalendar;

use super::{
    BarSeriesShared, InBar, InputView, OrderBrokerSimulator, OrderBrokerSimulatorSymbolState,
    QuoteConversionCursor, StrategyEngine, StrategyEngineConfig, path_of,
};
use crate::queue_model::QueueTracker;
use crate::schedule::Schedule;
use crate::timeframe::parse_timeframe;
use vike_portfolio::sizing::PositionSizerPassThrough;
use vike_trade_ledger::{TradeClosedAccountTerms, TradeClosedLedger};

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    pub fn new(
        bars_by_symbol: Vec<(String, Vec<Bar>)>,
        strategy: S,
        p: StrategyEngineConfig,
    ) -> Self {
        let shared: Vec<(String, BarSeriesShared)> =
            bars_by_symbol.into_iter().map(|(s, bars)| (s, bars.into())).collect();
        Self::new_shared(shared, strategy, p)
    }

    /// [`Self::new`] over [`BarSeriesShared`] views: a range of a series other runs hold too (a
    /// walk-forward window, a searched point), which the run reads as the same slice of bars and copies
    /// only if it must tag it (`BarSeriesShared::tagged`).
    pub fn new_shared(
        bars_by_symbol: Vec<(String, BarSeriesShared)>,
        strategy: S,
        p: StrategyEngineConfig,
    ) -> Self {
        // Each bar tagged with its instrument id once, IN the series handed over (no second copy of the
        // bars; a shared view already tagged is run as it is).
        let (symbols, bars): (Vec<String>, Vec<Rc<BarSeriesShared>>) = bars_by_symbol
            .into_iter()
            .map(|(s, series)| {
                let series = series.tagged(p.default_venue.as_deref(), &s);
                (s, Rc::new(series))
            })
            .unzip();
        // Each slot's foreign venue when it is a traded leg of one; none declared: empty.
        let leg_venues: Vec<Option<String>> = if p.reference_legs.is_empty() {
            Vec::new()
        } else {
            let venue_of = |s: &String| {
                p.reference_legs.iter().find(|(leg, _)| leg == s).map(|(_, v)| v.clone())
            };
            symbols.iter().map(venue_of).collect()
        };
        let lengths: std::collections::BTreeSet<usize> = bars.iter().map(|v| v.len()).collect();
        // ⚠ This assert is a LAST line of defence, not the refusal an operator sees.
        // `run::dispatch::refuse_ragged_series` catches a ragged universe at load and names both
        // symbols and both lengths. Reaching HERE means a caller bypassed the harness loader, so
        // the message stays terse on purpose — it is addressed to a developer, not a user.
        assert!(lengths.len() <= 1, "all symbol series must have the same length (aligned)");
        let n = lengths.into_iter().next().unwrap_or(0);
        // The same last line for a margin call whose numbers cannot be one: a profile is refused at
        // load naming the key (`vike-backtest`'s `checked_margin_call`); a direct caller stops here.
        if let Some(Err(e)) = p.shortfall_cover.map(|c| c.validate()) {
            panic!("StrategyEngineConfig::shortfall_cover: {e}");
        }

        let mut symbol: Vec<OrderBrokerSimulatorSymbolState> = bars
            .iter()
            .map(|series| OrderBrokerSimulatorSymbolState {
                pos: Position::default(),
                pending: Vec::new(),
                realized: 0.0,
                stop: None,
                entry_fee: 0.0,
                entry_timestamp: 0,
                price: if n > 0 { series[0].open } else { 0.0 },
                hi_since: 0.0,
                lo_since: f64::INFINITY,
                tf: Vec::new(),
                sub: vec![Vec::new(); n],
                quotes: Vec::new(),
                held: Vec::new(),
                emulated: IndexMap::new(),
                settled_profit: 0.0,
            })
            .collect();
        // per-symbol higher-TF aggregates
        for (si, st) in symbol.iter_mut().enumerate() {
            for tf in &p.timeframes {
                let ms = parse_timeframe(tf).expect("valid timeframe");
                st.tf.push((tf.clone(), ms, resample(&bars[si], ms)));
            }
        }
        // granular sub-bars bucketed into their coarse step
        //
        // ⚠ A THIRD undocumented-abort panic lives here, the same shape as the two recorded on
        // `crates/vike-simulator/src/engine/sim_broker.rs`'s module doc (`OrderBrokerSimulator::index` on an
        // unknown symbol; `crates/vike-simulator/src/bar_buffer.rs`'s `BarSeriesBuffer`): a
        // `p.granular_by_symbol` entry
        // naming a symbol not present in this run's `symbols` aborts the process at the
        // `expect("granular symbol unknown")` below, rather than returning a named error.
        //
        // ⚠ **A PROFILE CAN NOW REACH THIS FIELD, and this comment said it could not.** The
        // `data.detail_interval` key (the intrabar detail tape) loads it through
        // `crates/vike-backtest/src/run/dispatch.rs`'s `load_profile_detail_bars`. The panic stays
        // unreachable FROM THAT DOOR by CONSTRUCTION rather than by a check: that loader builds
        // its entries from `ProfileData::resolved_series`, the same list the coarse `load_profile_bars`
        // walks, so a key it emits is a symbol of the run by definition and no comparison exists to
        // rot. What is still open is a DIRECT `StrategyEngineConfig` caller — a test, a bin, a future
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
            let edges: Vec<i64> = bars[si].iter().map(|b| b.timestamp).collect();
            let mut sorted_subs: Vec<Bar> = subs.clone();
            sorted_subs.sort_by_key(|b| b.timestamp);
            for sub in sorted_subs {
                // bisect_right(edges, sub.ts) - 1
                let i = edges.partition_point(|&e| e <= sub.timestamp) as i64 - 1;
                if i < 0 {
                    continue; // precedes the first coarse bar
                }
                symbol[si].sub[i as usize].push(sub);
            }
        }
        // `InBarRules::path_walk`: every step walks its bar's path, or each detail bar's in turn,
        // all stamped with the step's bar.
        if p.in_bar.path_walk {
            for (si, st) in symbol.iter_mut().enumerate() {
                for (i, walk) in st.sub.iter_mut().enumerate() {
                    let bar = &bars[si][i];
                    *walk = if walk.is_empty() {
                        path_of(bar, bar.timestamp).to_vec()
                    } else {
                        walk.iter().flat_map(|d| path_of(d, bar.timestamp)).collect()
                    };
                }
            }
        }
        let now = if !symbols.is_empty() && n > 0 { bars[0][0].timestamp } else { 0 };
        // Opt-in queue-position tracker (tick/book replay only): built ONCE from the Copy
        // config fields; `None` (the default) means the tracker never exists — byte-identical.
        let queue = p
            .queue_model
            .map(|k| QueueTracker::new(k, p.queue_seed_depth, p.queue_min_hold_ms, symbols.len()));
        // Resolve each symbol's contract multiplier ONCE (index si -> f64), first match else the
        // default, so hot paths (equity_now, per-symbol curve, apply_fill, liquidation) index a Vec
        // instead of scanning `multipliers` by string on every call — matters for a mixed
        // futures/stocks/tokens book with many per-symbol multipliers. It only seeds the value below,
        // which is the engine's one store of the factor.
        let multiplier_by_si: Vec<f64> = symbols
            .iter()
            .map(|s| {
                p.multipliers
                    .iter()
                    .find(|(name, _)| name == s)
                    .map(|(_, m)| *m)
                    .unwrap_or(p.multiplier)
            })
            .collect();
        // ...and its CONTRACT VALUE (decision 0170): a listed one, else the linear value of that
        // multiplier, so an unlisted symbol books exactly the product it always did.
        let value_by_si: Vec<ContractValue> = symbols
            .iter()
            .zip(&multiplier_by_si)
            .map(|(s, &m)| {
                p.contract_values
                    .iter()
                    .find(|(name, _)| name == s)
                    .map_or(ContractValue::linear(m), |(_, v)| *v)
            })
            .collect();
        // ...and, for a symbol quoted in another currency than the ledger, the series that books it
        // (`StrategyEngineConfig::conversions`): EMPTY with none named, so the default path pays one
        // `is_empty` per event. Its value above is its QUOTE-currency value until a rate is in force.
        let quote_conversion_by_si: Vec<Option<QuoteConversionCursor>> = if p.conversions.is_empty()
        {
            Vec::new()
        } else {
            symbols
                .iter()
                .zip(&value_by_si)
                .map(|(s, v)| {
                    let mut of =
                        p.conversions.iter().enumerate().filter(|(_, c)| c.symbols.contains(s));
                    let (k, c) = of.next()?;
                    // last line of defence, as the asserts above: the harness books a symbol once
                    assert!(of.next().is_none(), "{s} is booked through two conversion series");
                    Some(QuoteConversionCursor::new(*v, k, c))
                })
                .collect()
        };
        // ...and whether its series counts volume in the settle coin (`StrategyEngineConfig::coin_volume`,
        // inverse symbols only).
        let coin_volume_by_si: Vec<bool> = symbols
            .iter()
            .zip(&value_by_si)
            .map(|(s, v)| v.kind == NotionalKind::Inverse && p.coin_volume.contains(s))
            .collect();
        // ...and whether it trades on a spot (cash) account (`StrategyEngineConfig::cash_symbols`).
        let cash_by_si: Vec<bool> = symbols.iter().map(|s| p.cash_symbols.contains(s)).collect();
        let n_symbol_resolved = symbols.len(); // captured before `symbols` moves into the struct
        // The funding schedule by symbol index (`StrategyEngineConfig::funding_schedule`), EMPTY with none
        // given so the default path allocates nothing.
        let funding_schedule: Vec<Option<Arc<_>>> = if p.funding_schedule.is_empty() {
            Vec::new()
        } else {
            symbols
                .iter()
                .map(|s| {
                    p.funding_schedule.iter().find(|(name, _)| name == s).map(|(_, c)| c.clone())
                })
                .collect()
        };
        let n_funding = funding_schedule.len();
        // Per-symbol session calendars for the opt-in gate (see `StrategyEngineConfig::session_gate`).
        // EMPTY unless the gate is on, so the default fill path pays one `Vec::get`/`is_empty`
        // branch and never indexes (mirrors `last_print_timestamp`). The KEY is per SYMBOL — each resolves
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
        // byte-identical to the pre-phase-2 engine. See `OrderBrokerSimulator::build_risk_gate`.
        let risk_gate = OrderBrokerSimulator::build_risk_gate(&p);
        // The tick a strategy reads, by precedence (`StrategyEngineConfig::kept_ticks`): what the gate's limits
        // state before any properties refresh, and each symbol's kept-row tick, EMPTY with none given.
        let stated_tick_by_si: Vec<Option<f64>> = risk_gate.as_ref().map_or_else(Vec::new, |g| {
            symbols.iter().map(|s| g.limits.grid_for(s).tick_size).collect()
        });
        let kept_tick_by_si: Vec<Option<f64>> = if p.kept_ticks.is_empty() {
            Vec::new()
        } else {
            let tick_of = |s: &String| p.kept_ticks.iter().find(|(name, _)| name == s);
            let positive = |t: f64| (t.is_finite() && t > 0.0).then_some(t);
            symbols.iter().map(|s| tick_of(s).and_then(|(_, t)| positive(*t))).collect()
        };
        let kept_tick_read = vec![std::cell::Cell::new(0); kept_tick_by_si.len()];
        // ...and, in a run that does not snap, the dated record's, asked once a symbol a day.
        let n_dated = if p.dated_ticks.is_some() { symbols.len() } else { 0 };
        let dated_tick_day = vec![std::cell::Cell::new((i64::MIN, None)); n_dated];
        // Fee-schedule bridge (fee model follow-up 3): `Some` OVERRIDES the flat
        // `maker_fee`/`taker_fee`/`fee_rate` chain with the venue schedule's own
        // `maker_taker_rates()` fractions; `None` (default) is the frozen precedence, untouched.
        // See `StrategyEngineConfig::fee_schedule`.
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
        // Each symbol's own schedule (`StrategyEngineConfig::fee_schedules`), each option's underlying
        // index (`StrategyEngineConfig::fee_underlyings`) and delivery fee (`StrategyEngineConfig::delivery_fees`),
        // resolved ONCE like `multiplier_by_si`; each EMPTY unless the run lists one, so a run that lists none
        // charges exactly as before.
        let by_symbol = |listed: &[(String, FeeSchedule)]| -> Vec<Option<FeeSchedule>> {
            if listed.is_empty() {
                return Vec::new();
            }
            let of = |s: &String| listed.iter().find(|(name, _)| name == s).map(|(_, f)| *f);
            symbols.iter().map(of).collect()
        };
        let fee_by_si = by_symbol(&p.fee_schedules);
        let delivery_fee_by_si = by_symbol(&p.delivery_fees);
        let fee_underlying_by_si: Vec<Option<usize>> = if p.fee_underlyings.is_empty() {
            Vec::new()
        } else {
            symbols
                .iter()
                .enumerate()
                .map(|(si, s)| {
                    let schedule = fee_by_si.get(si).copied().flatten().or(fee_curve);
                    if !matches!(schedule, Some(FeeSchedule::PercentOfUnderlying { .. })) {
                        return None;
                    }
                    let (_, under) = p.fee_underlyings.iter().find(|(opt, _)| opt == s)?;
                    symbols.iter().position(|x| x == under)
                })
                .collect()
        };
        // The cross-section's walk order, resolved ONCE here (before `symbols` moves into the
        // struct) because `symbols` is fixed for the life of the engine. Identity under the
        // default `DecideMode::Sequential`, so every fold routed through
        // `OrderBrokerSimulator::cross_section` sums exactly the bits it summed before the field existed.
        let cross_section = p.decide.cross_section_order(&symbols);
        let in_bar = InBar::new(p.in_bar, symbols.len());
        let mut core = OrderBrokerSimulator {
            symbols,
            bars,
            n,
            fee_rate: p.fee_rate,
            cash: p.cash,
            funding_paid: 0.0,
            slippage: p.slippage,
            slippage_amount: p.slippage_amount,
            maker_fee,
            taker_fee,
            fee_curve,
            fee_by_si,
            fee_underlying_by_si,
            delivery_fee_by_si,
            value_by_si,
            conversions: p.conversions,
            quote_conversion_by_si,
            coin_volume_by_si,
            cash_by_si,
            leverage: p.leverage,
            clamp_to_leverage: p.clamp_to_leverage,
            risk_gate,
            max_mount_leverage: p.max_mount_leverage,
            leverage_judged: false,
            maint_margin: p.maint_margin,
            venue_style_liquidation: p.venue_style_liquidation,
            liquidation_buffer: p.liquidation_buffer,
            shortfall_cover: p.shortfall_cover,
            shortfall_cover_fills: 0,
            shortfall_reduced: vec![None; n_symbol_resolved],
            shortfall_bar_size: vec![0.0; n_symbol_resolved],
            shortfall_bar_equity: 0.0,
            cash_gate: p.cash_gate,
            decide: p.decide,
            cross_section,
            active_mask: p.active_mask,
            max_open_positions: p.max_open_positions,
            max_open_long: p.max_open_long,
            max_open_short: p.max_open_short,
            sizer: p.sizer.unwrap_or_else(|| Box::new(PositionSizerPassThrough)),
            volume_limit: p.volume_limit.filter(|v| *v != 0.0),
            equity_peak: p.cash,
            step: 0,
            dropped: Vec::new(),
            dropped_at: Vec::new(),
            below_min_reversals: 0,
            trades: Vec::new(),
            forecasts: Vec::new(),
            intrabar_both_hit: 0,
            symbol,
            now,
            fill_model: p.fill_model,
            emulator_release_stops: p.emulator_release_stops,
            index: 0,
            schedule: Schedule::new(),
            mirror_fills: if p.mirror { Some(Vec::new()) } else { None },
            mirror_funding: if p.mirror { Some(Vec::new()) } else { None },
            ledger_events: None,
            // Kept on every run, for the strategy's read (`OrderBroker::closed_trades`); the terms arm its report.
            closed_trades: p.closed_trades.map_or_else(TradeClosedLedger::default, |t| {
                // The margin used is the engine's own margin model's, when the run states one.
                let (margin_long, margin_short) =
                    p.shortfall_cover.map_or((0.0, 0.0), |c| (c.margin_long, c.margin_short));
                let account = TradeClosedAccountTerms {
                    risk_free_rate: t.risk_free_rate,
                    margin_long,
                    margin_short,
                    ..TradeClosedAccountTerms::new(p.cash)
                };
                // One `mark_close` a step (`mark_closed_trades`), so its series are sized to the run.
                TradeClosedLedger::new(t.options).with_account(account).with_room_for_marks(n)
            }),
            closed_trade_report: p.closed_trades.is_some(),
            properties: p.properties.clone(),
            kept_tick_by_si,
            stated_tick_by_si,
            kept_tick_read,
            dated_ticks: p.dated_ticks.clone(),
            dated_tick_day,
            funding_source: p.funding_source.clone(),
            funding_schedule,
            funding_next: vec![0; n_funding],
            venue: p.default_venue.clone(),
            // The per-symbol resolved latch is allocated ONLY with a resolution OR option-expiry
            // source (both settle terminally and share the latch), so the default order-intake path
            // stays a single `is_empty` branch (see `push_pending`).
            resolved: if p.resolution.is_some() || p.option_specs.is_some() {
                vec![false; n_symbol_resolved]
            } else {
                Vec::new()
            },
            resolution: p.resolution,
            resolution_end_timestamp: p.resolution_end_timestamp,
            // Allocate-only-when-configured, like `resolved` above: no source, no latch.
            option_unsettled_reported: if p.option_specs.is_some() {
                vec![false; n_symbol_resolved]
            } else {
                Vec::new()
            },
            data_warnings: Vec::new(),
            option_specs: p.option_specs,
            option_expiry_end_timestamp: p.option_expiry_end_timestamp,
            settlements: Vec::new(),
            settlement_period_ms: p.settlement_period_ms,
            settle_bucket: None,
            variation_settlements: Vec::new(),
            // Same allocate-only-when-configured trick as `resolved` above: with the wait
            // discipline off this stays EMPTY and the fill lanes never index it (see
            // `OrderBrokerSimulator::defer_stale`), keeping the default path byte-identical.
            last_print_timestamp: if p.max_price_staleness_ms.is_some() {
                vec![None; n_symbol_resolved]
            } else {
                Vec::new()
            },
            max_price_staleness_ms: p.max_price_staleness_ms,
            stale_deferrals: 0,
            // Resolved ONCE per symbol at construction (see the `session` let-binding above); the
            // fill lanes then pay a single `Vec::get` per event, no string match — see
            // `StrategyEngineConfig::session_gate` and `OrderBrokerSimulator::session_closed`.
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
            // on `OrderBrokerSimulator`; both start at zero and are folded at the charge site.
            maker_fills: 0,
            taker_fills: 0,
            fees_paid: 0.0,
            order_fees: IndexMap::new(),
            optimistic_quotes: p.optimistic_quotes,
            queue_id_sequence: 1,
            latency: None, // armed by `run_ticks` only (see `StrategyEngine::latency`)
            shadow_pos: Vec::new(), // sized at arm time, cleared at disarm (see `run_ticks`)
            books: Vec::new(), // sized by `run_ticks` only when the tape carries book events
            releasing: None,
            held_terms: None,
            queue_model: p.queue_model.is_some(),
            groups: Default::default(),
            combos: Default::default(),
            order_tags: IndexMap::new(),
            seam_filled: Vec::new(),
            seam_ended: Vec::new(),
            seam_parents: Vec::new(),
            in_bar,
            inputs: InputView::new(p.inputs, p.input_whole),
            combo_mark_staleness_ms: 0,
            mounts: Vec::new(), // filled by `StrategyEngine::new_mounted` alone
            mount: 0,
            dispatching_slot: 0,
        };
        // The rates in force at the first event, so `Strategy::on_start` sizes and gates at them too.
        core.advance_quote_rates(core.now);
        // The warm-up gate, resolved ONCE (see the `warmup` field doc). `max`, not replace: an
        // outside floor may RAISE what the strategy declared and may never lower it, so a caller
        // passing a smaller number than `strategy.warmup()` changes nothing — which is what makes
        // `StrategyEngineConfig::warmup` safe to hand an operator's profile key.
        let warmup = p.warmup.unwrap_or(0).max(strategy.warmup());
        StrategyEngine {
            core,
            strategy,
            queue,
            latency: p.latency_model,
            deferred_fills: Vec::new(),
            equity_sampling: p.equity_sampling,
            warmup,
            leg_venues,
        }
    }
}
