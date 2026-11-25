//! `ProfileBacktest`'s load-time validation: the first-error and the accumulating door, one walk.

use std::collections::BTreeSet;

use super::{
    ProfileBacktest, WalkforwardWindowForm, checked_margin_call, decide_mode, margin_mode,
    resolve_conversions, resolve_ledger_currency,
};
use crate::HarnessError;
use crate::run::refusals::ProfileRefusal;
use crate::run::tick_replay::SeriesKind;
use crate::walkforward::runner::WalkforwardWindowSearch;
use vike_model::Diagnostic;
use vike_model::run_request::replay::ReplayKind;
use vike_strategy::file::scan::StrategyFileLanguage;

#[cfg(doc)]
use super::{ProfileEngineSizer, ProfileWalkforward};

impl ProfileBacktest {
    /// Semantic validation beyond what serde's field/type checking already enforces: exactly
    /// one data-slice form with non-empty, unique series, `from <= to`, `cash > 0`, and the
    /// mode-compatibility rules for the opt-in engine knobs.
    ///
    /// ⚠ **This is the FIRST-ERROR ADAPTER over [`Self::refusals`] now, and nothing else.** It
    /// keeps the signature, the error TYPE and the error VALUE every existing caller and test
    /// reads: the rules accumulate the real [`HarnessError`]s they have always raised and this
    /// hands back the first of them verbatim — so an unparseable `data.from` still comes back as a
    /// [`HarnessError::Parse`] rather than being re-wrapped as a `Validation`, which a rebuilt
    /// error would have silently done. Rule ORDER is what makes that equivalence hold, and
    /// [`Self::refusals`] carries why it may not be reshuffled.
    ///
    /// [`Self::validate_all`] is the door for a caller that wants every mistake in one pass.
    pub fn validate(&self) -> Result<(), HarnessError> {
        match self.refusals().into_iter().next() {
            Some((_, err)) => Err(err),
            None => Ok(()),
        }
    }

    /// Every rule [`Self::validate`] runs, reported as a LIST — so somebody learns about five
    /// mistakes in one pass instead of paying a round trip per mistake.
    ///
    /// # Why an accumulating door exists
    ///
    /// A first-error validator charges one edit-and-rerun per mistake, and a profile is one file
    /// with eighty-odd keys in it: a freshly written one genuinely carries several at once. On the
    /// remote route each of those reruns is also a dial to a backtesthub that then re-reads the
    /// profile it was handed, so the round trips are neither free nor parallel. Nothing here is new
    /// validation — the rules, their order and their sentences are [`Self::validate`]'s — and the
    /// FIRST element is always the diagnostic `validate` itself would have returned. The two doors
    /// cannot disagree about which mistake governs, because they are one walk.
    ///
    /// Every diagnostic this produces today is [`vike_model::DiagnosticSeverity::Error`]: a load-time
    /// refusal is all this validator has to say, and a profile that collects none of them is a
    /// profile that loads.
    ///
    /// # What it does NOT accumulate, said here rather than left to be discovered
    ///
    /// - **An unknown KEY — and that is a property of serde's error model, not a gap here.**
    ///   [`ProfileBacktest`] is `#[serde(deny_unknown_fields)]`, and serde stops at the FIRST
    ///   undeclared key: by the time any rule below could run, deserialization has already failed
    ///   naming one key, and there is no profile to validate at all. So a file with five typo'd
    ///   key NAMES still reports one of them, and every diagnostic in this list is about a key
    ///   that parsed. Enumerating all five needs a `toml::Value` walk against the schema — a
    ///   different mechanism, checking a different thing, and deliberately not attempted from
    ///   inside a validator whose input is the already-deserialized struct.
    /// - **A second mistake inside ONE delegated rule.** A rule that calls out to
    ///   [`Self::validate_data_slice`], [`ProfileEngineSizer::build`] or
    ///   [`ProfileWalkforward::window_form`] gets that function's own first error, so two mistakes in
    ///   one `[data]` table still report as one. The gain here is across TABLES, which is where a
    ///   new profile's mistakes actually sit; pushing accumulation down into those functions is a
    ///   wider change, because each is also called on paths that want a `Result` and each owns
    ///   refusal sentences whose evidence symbol is PUBLISHED (see [`Self::refusals`]).
    pub fn validate_all(&self) -> Vec<Diagnostic> {
        self.refusals()
            .into_iter()
            .map(|(key, err)| Diagnostic::error(key, err.message()))
            .collect()
    }

    /// The accumulating core both doors read: one row per rule that refused, each carrying the key
    /// a reader should look at and the [`HarnessError`] that rule actually raised.
    ///
    /// ⚠ **ORDER IS THE CONTRACT.** [`Self::validate`] returns element zero, so the sequence below
    /// is the short-circuit sequence this function replaced, unchanged — every `return Err(…)`
    /// became a push and the walk carries on. Reordering two rules changes which refusal every
    /// existing caller and test sees from an invalid profile, and the published surface would NOT
    /// catch it: `crates/vike-backtest/src/run/profile_surface/render.rs` sorts its refusal rows by
    /// message text, so a source reorder moves nothing in the asset. This file's own tests are the
    /// only thing that would.
    ///
    /// ⚠ **Where a refusal is raised is a published fact rather than a private one.** Each one is a
    /// `crate::run::refusals::ProfileRefusal` variant, published with its id, its text and the
    /// function that raises it (`profile_surface`'s `refusal_sites`) into
    /// `crates/vike-backtest/tests/fixtures/profile.json`, which is compared in-process on every PR.
    /// So moving a check between functions moves its site row, and rewording one rewrites the
    /// operator-facing reference: both are real edits with a re-render attached.
    ///
    /// ⚠ **A rule whose input an earlier rule already refused is SKIPPED, never run on wreckage**,
    /// and each of those sites says so where it sits rather than being counted here: the
    /// `engine.multipliers` KEY check needs a resolved data slice, the coarser-than-base comparison
    /// needs a `data.interval` that parsed, the split-count and grid rules need a `[walkforward]`
    /// window form and a `search` value that resolved, and `from <= to` needs a range. What makes
    /// accumulating safe at all is that every OTHER rule reads only its own key, so nothing it says
    /// depends on a question already answered wrong.
    fn refusals(&self) -> Vec<(&'static str, HarnessError)> {
        let mut out: Vec<(&'static str, HarnessError)> = Vec::new();
        // The `[data]` half first, exactly as before — and its verdict is REMEMBERED, because one
        // later rule reads the slice this one may have just refused.
        let slice_ok = match self.validate_data_slice() {
            Ok(()) => true,
            Err(e) => {
                out.push(("data", e));
                false
            }
        };
        if self.engine.cash <= 0.0 {
            let e = HarnessError::from(ProfileRefusal::EngineCashNotPositive {
                cash: self.engine.cash,
            });
            out.push(("engine.cash", e));
        }
        // `risk.max_orders_per_window` is a WALL-CLOCK order-rate throttle; sim time is not wall
        // time (a month of ticks can replay in under a second), so it is REJECTED at load rather
        // than silently ignored — `OrderBrokerSimulator::build_risk_gate` always disarms it regardless
        // (`limits.max_orders_per_window = None`), and a limit an operator believes is armed but
        // never actually checked is exactly the failure class this `[risk]` wiring exists to kill.
        // Every other `risk.*` limit is honored.
        if let Some(r) = &self.risk
            && r.max_orders_per_window.is_some()
        {
            let e = HarnessError::from(ProfileRefusal::RiskMaximumOrdersPerWindowWallClock);
            out.push(("risk.max_orders_per_window", e));
        }
        // The margin call: its numbers, then the lane and the liquidation it replaces.
        if let Some(call) = self.engine.margin_call {
            if let Err(e) = checked_margin_call(call) {
                out.push(("engine.margin_call", e));
            }
            if self.data.kind == ReplayKind::Tick {
                let e = HarnessError::from(ProfileRefusal::EngineMarginCallBarModeOnly);
                out.push(("engine.margin_call", e));
            }
            if self.engine.maint_margin > 0.0 || self.engine.venue_style_liquidation {
                let e = HarnessError::from(ProfileRefusal::EngineMarginCallWithLiquidation);
                out.push(("engine.margin_call", e));
            }
        }
        if self.engine.whole_entry_fee && !self.engine.closed_trades {
            let e = HarnessError::from(ProfileRefusal::EngineWholeEntryFeeNeedsClosedTrades);
            out.push(("engine.whole_entry_fee", e));
        }
        if self.engine.feed_latency && self.data.kind == ReplayKind::Bar {
            let e = HarnessError::from(ProfileRefusal::EngineFeedLatencyTickModeOnly);
            out.push(("engine.feed_latency", e));
        }
        // The queue-position fill model is consulted exclusively by the `run_ticks` tick/book replay
        // lanes, so it is tick-mode only (the mirror of the `feed_latency` rule). A bar-mode profile
        // setting it would silently do nothing — rejected here rather than mislead. Also validates
        // the string so a typo fails at load, not silently.
        if self.engine.queue_model.is_some() && self.data.kind == ReplayKind::Bar {
            let e = HarnessError::from(ProfileRefusal::EngineQueueModelTickModeOnly);
            out.push(("engine.queue_model", e));
        }
        // Fail-fast on an unrecognized queue_model string.
        if let Err(e) = self.engine.queue_model_kind() {
            out.push(("engine.queue_model", e));
        }
        // The equity-curve density knob is read only by `run_ticks` (the bar lane's curve is one
        // sample per BAR — bounded by the bar count, nothing to thin), so it is tick-mode only.
        // A bar-mode profile setting it would silently do nothing and quietly leave the operator
        // believing the run's drawdown/sharpe were computed over a subsample they chose.
        if self.engine.equity_sample_every.is_some() && self.data.kind == ReplayKind::Bar {
            let e = HarnessError::from(ProfileRefusal::EngineEquitySampleEveryTickModeOnly);
            out.push(("engine.equity_sample_every", e));
        }
        // Same rule for the tick-lane fill-model override: resolve it here so a typo ("L2Book",
        // "l2_book") fails the profile at LOAD with the valid set named, never a silent
        // optimistic-`Tick` fallback that would quietly undo the depth-cap realism knob (#819).
        if let Err(e) = self.engine.fill_model_kind() {
            out.push(("engine.fill_model", e));
        }
        // (`attach_funding` was refused on the tick lane until 2026-11-08: the tick loop now charges
        // the same funding schedule, `vike_simulator::StrategyEngineConfig::funding_schedule`.)
        // `cash_gate` is bar-mode only. `StrategyEngine::run` is its ONLY reader — the `granular`
        // mask and the `fill_step_gated` branch — and `StrategyEngine::fill_step_gated`'s single
        // caller sits inside that bar loop; `StrategyEngine::run_ticks` never consults it, and
        // neither does any `OrderBrokerSimulator` fill lane the tick path drives. So `kind = "tick"` +
        // `cash_gate = true` would parse, validate, reach `StrategyEngineConfig` and change nothing — the
        // exact defect every other lane-asymmetric key here already refuses (`feed_latency`,
        // `queue_model`, `equity_sample_every`, `timeframes`).
        if self.engine.cash_gate && self.data.kind == ReplayKind::Tick {
            let e = HarnessError::from(ProfileRefusal::EngineCashGateBarModeOnly);
            out.push(("engine.cash_gate", e));
        }
        // `[engine.in_bar]` is the bar lane's: `StrategyEngine::run` alone reads `StrategyEngineConfig::in_bar`,
        // and the rules that walk a bar (the path, the price order, the committed stop) act on the granular
        // sub-bar lane, which the shared-cash lane disables. This close is not one of them: the shared-cash
        // phase meets it at the head of the next step (`StrategyEngine::fill_step_gated_at_close`).
        if let Some(rules) = self.engine.in_bar {
            let any = rules.path_walk
                || rules.fill_at_close
                || rules.price_order
                || rules.ties_in_placement_order
                || rules.queue_order
                || rules.committed_stops
                || rules.trail_from_fill
                || rules.reduce_only_at_fill;
            if any && self.data.kind == ReplayKind::Tick {
                let e = HarnessError::from(ProfileRefusal::EngineInBarBarModeOnly);
                out.push(("engine.in_bar", e));
            }
            let walks = rules.path_walk || rules.price_order || rules.committed_stops;
            let gated = self.engine.cash_gate
                || decide_mode(self.engine.decide.as_deref()).unwrap_or_default().is_simultaneous();
            if walks && gated {
                let e = HarnessError::from(ProfileRefusal::EngineInBarWalkWithSharedCash);
                out.push(("engine.in_bar", e));
            }
        }
        // `engine.decide`'s three refusals. The KEY itself parses in either lane (an unknown
        // spelling is `decide_mode`'s own refusal); what is checked here is the combinations, all
        // three of which would otherwise let a run BELIEVE it computed a cross-section it did not.
        //
        // ⚠ The resolver is called rather than the raw string matched, so a future spelling
        // cannot be silently exempted from any of the three by being spelled differently. An
        // unparseable value falls through to `Sequential` here and is refused by the resolver at
        // the construction site, so it is reported once rather than twice.
        if decide_mode(self.engine.decide.as_deref()).unwrap_or_default().is_simultaneous() {
            // (1) BAR MODE ONLY, for the same reason `cash_gate` above is: the allocator the mode
            // orders lives in `StrategyEngine::fill_step_gated`, reached only from
            // `StrategyEngine::run`. `run_ticks` consults neither field, so a tick profile asking
            // for a simultaneous cross-section would parse, validate, reach `StrategyEngineConfig` and
            // change nothing.
            if self.data.kind == ReplayKind::Tick {
                let e = HarnessError::from(ProfileRefusal::EngineDecideSimultaneousBarModeOnly);
                out.push(("engine.decide", e));
            }
            // (2) The DECLARED GAP. With a pre-trade gate armed (`[risk]`, or `engine.leverage`
            // through `OrderBrokerSimulator::build_risk_gate`'s leverage→initial-margin mapping),
            // `OrderBrokerSimulator::gate_order` folds the `pending` set — which ACCUMULATES across the
            // `on_bar` fan-out — so the symbol judged FIRST faces the step's whole budget and each
            // later one faces the remainder. Ordering the allocator cannot repair that: by the
            // time the fill phase ranks anything, the loser's order was already denied or shrunk
            // at submit. The fold is not the bug (a pre-trade cap must model the local view — see
            // `OrderBrokerSimulator::margin_in_use_pending_aware`); the missing piece is a step-boundary
            // budget snapshot, and it is NOT BUILT. Refused rather than accepted, because a mode
            // whose DECISION half is still taken in list order while its name says otherwise is
            // worse than no mode at all.
            //
            // ⚠ `clamp_to_leverage` does not escape this: it routes to
            // `OrderBrokerSimulator::cap_to_leverage`, whose own pending fold has the same shape.
            if self.risk.is_some() || self.engine.leverage.is_some() {
                let e = HarnessError::from(ProfileRefusal::EngineDecideSimultaneousWithRiskGate);
                out.push(("engine.decide", e));
            }
            // (3) `detail_interval`, refused for exactly the reason `cash_gate` refuses it below:
            // this mode IMPLIES the gated lane, and `StrategyEngine::run`'s granular mask is
            // `!gated && !sub[i].is_empty()`, so the detail tape would be loaded, paid for and
            // then ignored.
            if self.data.detail_interval.is_some() {
                let e =
                    HarnessError::from(ProfileRefusal::EngineDecideSimultaneousWithDetailInterval);
                out.push(("engine.decide", e));
            }
        }
        // `timeframes` is bar-mode only (the coarse series are resampled from the base BAR
        // stream, and a tick profile has none), and every entry must be a valid,
        // strictly-coarser-than-base interval — left to the engine it is
        // `parse_timeframe(tf).expect(..)`, a process abort where every neighbouring key gives a
        // named refusal.
        if !self.engine.timeframes.is_empty() {
            if self.data.kind == ReplayKind::Tick {
                let e = HarnessError::from(ProfileRefusal::EngineTimeframesBarModeOnly);
                out.push(("engine.timeframes", e));
            }
            // The base interval is the DENOMINATOR of the coarser-than-base rule below, so a
            // profile whose own `data.interval` does not parse gets that refusal and then the
            // COMPARISON is skipped — while each entry is still checked on its own terms, which
            // is the half that needs no denominator.
            let base = vike_marketdata::interval::interval_ms(&self.data.interval);
            if base.is_none() {
                let e = HarnessError::from(ProfileRefusal::DataIntervalInvalidForTimeframes {
                    interval: self.data.interval.clone(),
                });
                out.push(("data.interval", e));
            }
            for tf in &self.engine.timeframes {
                // The sentence is a released `profile.json` refusal, `{ms}` included.
                if let Some(ms) = vike_marketdata::interval::interval_ms(tf).filter(|ms| *ms == 0) {
                    let e = HarnessError::from(ProfileRefusal::EngineTimeframesZeroLength {
                        tf: tf.clone(),
                        ms,
                    });
                    out.push(("engine.timeframes", e));
                    // A zero-length window is also "not coarser than the base", so this entry has
                    // said its one useful thing — the second sentence would only repeat it.
                    continue;
                }
                // A count too long for an `i64` is refused here too: the engine resamples at this
                // width, so a wrapped one would be coarse bars of the wrong size under this label.
                let Some(ms) = vike_marketdata::interval::bar_step_ms(tf) else {
                    let e = HarnessError::from(ProfileRefusal::EngineTimeframesNotAnInterval {
                        tf: tf.clone(),
                    });
                    out.push(("engine.timeframes", e));
                    continue;
                };
                if let Some(base) = base
                    && ms <= base
                {
                    let e = HarnessError::from(ProfileRefusal::EngineTimeframesNotCoarser {
                        tf: tf.clone(),
                        ms,
                        interval: self.data.interval.clone(),
                        base,
                    });
                    out.push(("engine.timeframes", e));
                }
            }
        }
        if self.engine.maint_margin < 0.0 {
            let e = HarnessError::from(ProfileRefusal::EngineMaintenanceMarginNegative {
                maint_margin: self.engine.maint_margin,
            });
            out.push(("engine.maint_margin", e));
        }
        if self.engine.liquidation_buffer < 0.0 {
            let e = HarnessError::from(ProfileRefusal::EngineLiquidationBufferNegative {
                liquidation_buffer: self.engine.liquidation_buffer,
            });
            out.push(("engine.liquidation_buffer", e));
        }
        if let Some(v) = self.engine.volume_limit
            && !(v > 0.0 && v <= 1.0)
        {
            let e = HarnessError::from(ProfileRefusal::EngineVolumeLimitOutOfRange { v });
            out.push(("engine.volume_limit", e));
        }
        if self.engine.multiplier <= 0.0 {
            let e = HarnessError::from(ProfileRefusal::EngineMultiplierNotPositive {
                multiplier: self.engine.multiplier,
            });
            out.push(("engine.multiplier", e));
        }
        for (symbol, m) in &self.engine.multipliers {
            if *m <= 0.0 {
                let e = HarnessError::from(ProfileRefusal::EngineMultipliersNotPositive {
                    symbol: symbol.clone(),
                    m: *m,
                });
                out.push(("engine.multipliers", e));
            }
        }
        // ...and the KEY half of the same rule. `StrategyEngine::new` resolves each loaded
        // symbol's multiplier by EXACT name (`p.multipliers.iter().find(|(name, _)| name == s)`)
        // and falls back to the global `p.multiplier`, so a row naming a symbol this run does not
        // load is read by nobody: the run sizes at 1.0 (or whatever the global says) while the
        // operator reads the profile as contract-sized. A typo is the whole failure mode — which
        // makes this the `[engine]` surface's own instance of the defect class this file exists
        // to refuse, and the reason the valid set is named back in the message.
        if slice_ok && !self.engine.multipliers.is_empty() {
            // Gated on `slice_ok` rather than on `validate_data_slice` having merely RUN: this
            // rule names the loaded symbols back, so it is only answerable once the slice is
            // well-formed and its symbols are known unique. On a refused slice it is skipped —
            // a "the data slice is: " listing built from wreckage would be a second, wrong
            // refusal chasing the first one.
            let slice: BTreeSet<String> =
                self.data.resolved_series().into_iter().map(|s| s.symbol).collect();
            for symbol in self.engine.multipliers.keys() {
                if !slice.contains(symbol) {
                    let known: Vec<&str> = slice.iter().map(String::as_str).collect();
                    let e = HarnessError::from(ProfileRefusal::EngineMultipliersNotLoaded {
                        symbol: symbol.clone(),
                        known: known.join(", "),
                    });
                    out.push(("engine.multipliers", e));
                }
            }
        }
        // ...and the VALUE against the catalog (decision 0170's catalog amendment): where the name fixes one
        // unit (`vike_catalog::kept_contract_value`, the value a paper book and the venue's live mount book, its
        // KEPT row deciding whether it is inverse where the run holds one), a
        // stated multiplier must be that unit. Any other sizes every notional, margin, fee and PnL of the run
        // by the wrong factor against live, so it is refused, never silently preferred to the catalog's. A
        // name whose unit only its row states (an ibkr future, an fxcm CFD) keeps the profile's figure; an
        // inverse value is the coin ledger's own (`crate::run::profile_run_ledger_coin`, which refuses a
        // stated multiplier on such a run), not this one.
        if slice_ok {
            for s in self.data.resolved_series() {
                let row = self.kept.row(&s.venue, &s.symbol);
                let Some(cv) = vike_catalog::kept_contract_value(&s.venue, &s.symbol, row)
                    .filter(|cv| cv.kind == vike_model::NotionalKind::Linear)
                else {
                    continue;
                };
                let (key, stated) = match self.engine.multipliers.get(&s.symbol) {
                    Some(m) => ("engine.multipliers", *m),
                    None => ("engine.multiplier", self.engine.multiplier),
                };
                if stated > 0.0 && stated != cv.per_unit {
                    let e = HarnessError::from(ProfileRefusal::EngineMultiplierWrongUnit {
                        key: key.to_string(),
                        symbol: s.symbol.clone(),
                        venue: s.venue.clone(),
                        stated,
                        per_unit: cv.per_unit,
                    });
                    out.push((key, e));
                }
            }
        }
        if let Some(r) = self.engine.risk_free_rate
            && !(r.is_finite() && r > -1.0)
        {
            let e = HarnessError::from(ProfileRefusal::EngineRiskFreeRateInvalid { r });
            out.push(("engine.risk_free_rate", e));
        }
        if let Some(l) = self.engine.leverage
            && l <= 0.0
        {
            let e = HarnessError::from(ProfileRefusal::EngineLeverageNotPositive { l });
            out.push(("engine.leverage", e));
        }
        // The judge reads anything else as no cap (`vike_execution::risk::mount_leverage_refusal`);
        // a figure written here is refused, never read as off.
        if let Some(cap) = self.engine.max_mount_leverage
            && !(cap.is_finite() && cap >= 1.0)
        {
            let e = HarnessError::from(ProfileRefusal::EngineMountLeverageCapInvalid { cap });
            out.push(("engine.max_mount_leverage", e));
        }
        // ⚠ ORDER IS LOAD-BEARING, and this block used to have it backwards. The real end state
        // — `clamp_to_leverage` is unavailable to ANY profile carrying `[risk]` — was stated
        // NOWHERE, so a profile with `[risk]` and `clamp_to_leverage = true` was told by the
        // "needs engine.leverage" rule to add a key that the leverage/`[risk]` rule then refused.
        // Two refusals, one dead end, and the constraint that actually governs never named. It is
        // named FIRST now.
        //
        // ⚠ An accumulating door shows BOTH of that pair at once where the first-error door
        // showed only the governing one, and that is an improvement rather than the old dead end:
        // the reader sees "drop clamp_to_leverage" and "clamp needs engine.leverage" together, and
        // the first answer dissolves both. What must not change is WHICH comes first.
        //
        // The mechanism: `OrderBrokerSimulator::build_risk_gate` returns `None` on `p.clamp_to_leverage`
        // BEFORE it so much as reads `p.risk_limits`, so the clamp disarms the WHOLE pre-trade
        // gate — every `[risk]` limit, not merely `max_leverage`. `[risk]` is the discarded side
        // here, which is the OPPOSITE of what the leverage/`[risk]` rule below describes for the
        // clamp-off case.
        if self.engine.clamp_to_leverage && self.risk.is_some() {
            let e = HarnessError::from(ProfileRefusal::EngineClampToLeverageWithRisk);
            out.push(("engine.clamp_to_leverage", e));
        }
        if self.engine.clamp_to_leverage && self.engine.leverage.is_none() {
            let e = HarnessError::from(ProfileRefusal::EngineClampToLeverageNeedsLeverage);
            out.push(("engine.clamp_to_leverage", e));
        }
        // Two margin sources, one gate. `OrderBrokerSimulator::build_risk_gate` matches
        // `(&p.risk_limits, p.leverage)` and its `(Some(l), _)` arm takes `risk_limits`
        // UNCONDITIONALLY, so with the clamp OFF (the default) `engine.leverage` is the silently
        // discarded side — whether or not `[risk]` sets `max_leverage`. Refuse rather than let
        // that precedence pick for the operator. ⚠ The clamp-ON case is NOT this one and is refused
        // above: there the early `None` return discards `[risk]` instead.
        if self.engine.leverage.is_some() && self.risk.is_some() {
            let e = HarnessError::from(ProfileRefusal::EngineLeverageWithRisk);
            out.push(("engine.leverage", e));
        }
        if let Some(ms) = self.engine.settlement_period_ms
            && ms <= 0
        {
            let e = HarnessError::from(ProfileRefusal::EngineSettlementPeriodNotPositive { ms });
            out.push(("engine.settlement_period_ms", e));
        }
        // Fail on a bad `kind` / missing knob / missing wrapped base at PARSE time — never a silent fallback to "no sizing", which
        // would run the whole backtest unsized while the operator read the report as a sized one.
        if let Some(sizer) = &self.engine.sizer
            && let Err(e) = sizer.build()
        {
            out.push(("engine.sizer", e));
        }
        // What fails at LOAD in this section, and what deliberately does not. A `[walkforward]`
        // value that is wrong ON ITS OWN TERMS — independent of the store, of the data slice and
        // of which verb is running — fails here: a zero split count (there is no window to test
        // on), an unrecognized `search`/`mode`/`rank_by` string (a typo must never degrade into a
        // silently different protocol), `search = "sweep"` on a profile carrying no `[sweep]`
        // table (a walk that says it optimizes with nothing to search is a contradiction that no
        // data can resolve, and it is the same shape as the zero-split hole above), and — since
        // the duration window form landed — an incoherent WINDOW FORM or a duration that is not
        // one (the block below argues each).
        //
        // The bar-mode and single-symbol rules are NOT checked here, and the difference is what
        // they depend on: those are questions about what the profile RESOLVES against a store,
        // while `[walkforward]` is inert for a plain `run_backtest`/`run_paramscan` run — so
        // rejecting a profile that merely CARRIES the section would fail runs that never ask for
        // a walk-forward. `crate::walkforward::runner` enforces those where they apply, and
        // re-enforces the rules below too, because a hand-built profile never passed through here
        // at all.
        if let Some(walkforward) = &self.walkforward {
            // The WINDOW FORM is the first question, and every incoherent combination of it is
            // wrong on its own terms too: a form that is absent, doubled or half-declared (a table
            // with no window shape is not a walk), and `purge`/`embargo` under the split-count
            // form (`docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md`). What is NOT
            // checked here is how many BARS a duration resolves to: that needs `data.interval` AND
            // the series, so it belongs to `crate::walkforward::windows::resolve_windows`, which
            // the drivers call.
            //
            // An unresolvable form leaves the split-count rule below UNASKED rather than guessed
            // at: `n_splits = 0` beside a `train`/`test` pair is already refused BY the form rule,
            // and answering it twice would name two mistakes where the profile has one.
            let form = match walkforward.window_form() {
                Ok(form) => Some(form),
                Err(e) => {
                    out.push(("walkforward", e));
                    None
                }
            };
            if form == Some(WalkforwardWindowForm::Splits) && walkforward.n_splits == Some(0) {
                let e = HarnessError::from(ProfileRefusal::WalkforwardSplitsBelowOne);
                out.push(("walkforward.n_splits", e));
            }
            // Every duration is RESOLVED here so a typo or a zero fails at load, exactly as the
            // string knobs below are; the values are discarded because the driver reads them
            // again from the same resolvers.
            for (key, parsed) in [
                ("walkforward.train", walkforward.train_span()),
                ("walkforward.test", walkforward.test_span()),
                ("walkforward.step", walkforward.step_span()),
                ("walkforward.purge", walkforward.purge_span()),
                ("walkforward.embargo", walkforward.embargo_span()),
            ] {
                if let Err(e) = parsed {
                    out.push((key, e));
                }
            }
            // Every string knob is RESOLVED here so a typo fails at load; only `search`'s value is
            // wanted (the grid rule below reads it), and the other two are called for the parse
            // alone — the drivers read them again from the same resolvers.
            let search = match walkforward.window_search() {
                Ok(search) => Some(search),
                Err(e) => {
                    out.push(("walkforward.search", e));
                    None
                }
            };
            if let Err(e) = walkforward.walk_mode() {
                out.push(("walkforward.mode", e));
            }
            if let Err(e) = walkforward.rank_metric() {
                out.push(("walkforward.rank_by", e));
            }
            if search == Some(WalkforwardWindowSearch::Sweep) && !self.is_paramscan() {
                let e = HarnessError::from(ProfileRefusal::WalkforwardSearchNeedsParamscan);
                out.push(("walkforward.search", e));
            }
        }
        // `[permutation]` is the run's PROGRAM when present, so what is wrong with it on its own
        // terms fails here: no history to compare with, an unknown `rank_by`, a second program
        // beside it, training with nothing to search, and a grid that nothing would search. The
        // bar-mode rule is the driver's, as the walk's is (`crate::permutation`).
        if let Some(permutation) = &self.permutation {
            if permutation.permutations == 0 {
                let e = HarnessError::from(ProfileRefusal::PermutationPermutationsBelowOne);
                out.push(("permutation.permutations", e));
            }
            if let Err(e) = permutation.rank_metric() {
                out.push(("permutation.rank_by", e));
            }
            if self.walkforward.is_some() {
                let e = HarnessError::from(ProfileRefusal::PermutationWithWalkforward);
                out.push(("permutation", e));
            }
            if permutation.train && !self.is_paramscan() {
                let e = HarnessError::from(ProfileRefusal::PermutationTrainWithoutParamscan);
                out.push(("permutation.train", e));
            }
            if !permutation.train && self.is_paramscan() {
                let e = HarnessError::from(ProfileRefusal::PermutationParamscanWithoutTrain);
                out.push(("permutation.train", e));
            }
        }
        // Fail on a bad resolution kind/window at PARSE time; the winners file + coverage check
        // need the profile's base_dir and run late, in `ProfileEngineResolution::build`.
        if let Some(res) = &self.engine.resolution
            && let Err(e) = res.validate_shape()
        {
            out.push(("engine.resolution", e));
        }
        // ── the two `[data]` time resolvers, promoted from RUN time to LOAD time ────────────────
        // Both are already called on the run path — `warmup_steps` at both `bar_engine_params`
        // lanes, `detail_interval_ms` in the tape loader — so every refusal here already fired.
        // What moves is WHEN: calling them from this function makes them answerable before a store
        // is opened, which is the whole difference between a typo costing a round trip and a typo
        // costing nothing.
        if let Err(e) = self.data.warmup_steps() {
            out.push(("data.warmup", e));
        }
        if let Err(e) = self.data.detail_interval_ms() {
            out.push(("data.detail_interval", e));
        }
        // The three coverage/universe key SPELLINGS, answered before a store is opened for the same
        // reason the two above are: a typo in a disposition should cost nothing, not a round trip
        // to a backtesthub that then reads the profile it was handed. Each one decides whether a
        // run happens at all, so none of them falls back.
        if let Err(e) = self.data.on_gap() {
            out.push(("data.on_gap", e));
        }
        if let Err(e) = self.data.max_gap_ms() {
            out.push(("data.max_gap", e));
        }
        if let Err(e) = self.data.universe_mode() {
            out.push(("data.universe", e));
        }
        // Where the bars come from (`data.source` / `data.file`), answered before a store or a file is
        // opened, for the same reason. `ProfileData::bar_source` carries every refusal.
        if let Err(e) = self.data.bar_source() {
            let key = match (self.data.file.is_some(), self.data.file_format.is_some()) {
                (true, _) => "data.file",
                (false, true) => "data.file_format",
                (false, false) => "data.source",
            };
            out.push((key, e));
        }
        // ...and the ARMING rule for the two that are meaningless alone. A disposition or a
        // tolerance without `require_coverage` is a key an operator wrote, a reader will believe,
        // and nothing consults — `vike_config::CONSUMPTION`'s whole subject, and worse here than
        // there because what it appears to configure is a REFUSAL: somebody reads
        // `on_gap = "refuse"` and believes a run over an uncovered window cannot happen.
        if self.data.on_gap.is_some() && !self.data.require_coverage {
            let e = HarnessError::from(ProfileRefusal::DataOnGapWithoutRequireCoverage);
            out.push(("data.on_gap", e));
        }
        if self.data.max_gap.is_some() && !self.data.require_coverage {
            let e = HarnessError::from(ProfileRefusal::DataMaxGapWithoutRequireCoverage);
            out.push(("data.max_gap", e));
        }
        // ── the three rules that read TWO tables, which is why they live here and not on a cfg ──
        // A cfg struct sees its own table. Each rule below compares two, so this function is the
        // only place that can ask the question at all.
        if self.data.detail_interval.is_some() {
            // ⚠ The dangerous one: the run SUCCEEDS and the number looks plausible.
            // `StrategyEngine::run`'s granular mask is `!cash_gate && !sub[i].is_empty()`, so the
            // detail tape would be loaded, scanned and then ignored while the operator believed
            // every order was resolved against it.
            if self.engine.cash_gate {
                let e = HarnessError::from(ProfileRefusal::DataDetailIntervalWithCashGate);
                out.push(("data.detail_interval", e));
            }
            // ⚠ This one is LOOK-AHEAD, not waste. A walk hands each window a SLICED coarse series
            // while nothing slices the detail tape, and a sub-bar is bucketed by
            // `partition_point(|e| e <= sub.ts) - 1` — so every window's LAST coarse step would
            // absorb the whole remaining tape and fill against bars from AFTER the window ended.
            if self.walkforward.is_some() {
                let e = HarnessError::from(ProfileRefusal::DataDetailIntervalWithWalkforward);
                out.push(("data.detail_interval", e));
            }
        }
        // A ledger currency this build cannot read is refused here, at load, so the symbol guard
        // (`refuse_foreign_quoted_symbols`) only ever compares names against a currency it knows.
        // The symbols themselves are checked at run time against the series each door resolved.
        if let Err(e) = resolve_ledger_currency(self.engine.account_currency.as_deref()) {
            out.push(("engine.account_currency", e));
        } else if let Err(e) = resolve_conversions(self) {
            // The conversion pairs read against that ledger (decision 0243 phase 2), at load; whether
            // the store holds each one is asked when the run reads it. Skipped on a ledger refused above.
            out.push(("engine.conversion", e));
        }
        // The account's margin mode: a word this build does not run is refused at load, never
        // read as the default.
        if let Err(e) = margin_mode(self.engine.margin_mode.as_deref()) {
            out.push(("engine.margin_mode", e));
        }
        // The strategy file's path: under the `strategies/` folder, `.rs` or `.rhai`.
        if let Some(Err(e)) = self.strategy.as_ref().map(super::ProfileStrategy::language) {
            out.push(("strategy.file", e));
        }
        // What the profile trades, one way, and every `[[mounts]]` rule (`mounts_config`).
        out.extend(self.mount_refusals(slice_ok));
        // Each `[inputs]` row's two names in the input grammar (decision 0271), at load. Whether a
        // row names an input the strategy reads is asked when the run builds the strategy.
        // A one-part name (`"1d"`) is the run's own symbol at that bar length, the grammar the run reads.
        // The check is the daemon profile's too (`unparsed_input_rows`); the sentence is this profile's.
        let own = crate::run::data_plan::own_series(self);
        for (name, read, e) in vike_data::store::series::unparsed_input_rows(&self.inputs, own) {
            let e = HarnessError::from(ProfileRefusal::InputsRowUnparsed {
                name: name.to_string(),
                read: read.to_string(),
                e,
            });
            out.push(("inputs", e));
        }
        // `[data.options]`: its own keys, then its ledger. A leg is priced at its chain's DOLLAR mark,
        // so a run that books in another currency would read every premium as that currency's.
        if let Err(e) = self.data.options_check() {
            out.push(("data.options", e));
        } else if self.data.options.is_some()
            && self
                .engine
                .account_currency
                .as_deref()
                .is_some_and(|c| !vike_model::same_currency(c, vike_model::DEFAULT_CURRENCY))
        {
            let e = HarnessError::from(ProfileRefusal::DataOptionsLedgerNotDollar);
            out.push(("data.options", e));
        }
        // An SDK is a Rust file's (decision 0296): a `.rhai` file runs on this vike's own code, so the
        // override would be read by nothing.
        if let Some(strategy) = &self.strategy
            && let Some(sdk) = strategy.sdk
            && matches!(strategy.language(), Ok(StrategyFileLanguage::Rhai))
        {
            let e = HarnessError::from(ProfileRefusal::StrategySdkWithRhaiFile {
                sdk,
                file: strategy.file.clone(),
            });
            out.push(("strategy.sdk", e));
        }
        // The range is the LAST rule, as it was. `from <= to` needs a range that PARSED, so an
        // unparseable stamp is reported and the comparison skipped rather than asked of the
        // `i64::MIN`/`MAX` fallbacks, which would answer a question about values nobody wrote.
        match self.range() {
            Err(e) => out.push(("data", e)),
            Ok(range) => {
                let start = range.start.unwrap_or(i64::MIN);
                let end = range.end.unwrap_or(i64::MAX);
                if start > end {
                    let e = HarnessError::from(ProfileRefusal::DataFromAfterTo { start, end });
                    out.push(("data.from", e));
                }
            }
        }
        out
    }

    /// The `[data]` half of [`Self::validate`]: exactly one slice form, non-empty, symbols
    /// unique, and the lane filter only where a lane exists.
    fn validate_data_slice(&self) -> Result<(), HarnessError> {
        let legacy = self.data.venue.is_some() || !self.data.symbols.is_empty();
        let series = !self.data.series.is_empty();
        match (legacy, series) {
            (true, true) => {
                return Err(HarnessError::from(ProfileRefusal::DataTwoSliceForms));
            }
            (false, false) => {
                return Err(HarnessError::from(ProfileRefusal::DataSymbolsEmptyWithoutSeries));
            }
            (true, false) => {
                if self.data.venue.is_none() {
                    return Err(HarnessError::from(ProfileRefusal::DataVenueMissing));
                }
                if self.data.symbols.is_empty() {
                    return Err(HarnessError::from(ProfileRefusal::DataSymbolsEmpty));
                }
            }
            (false, true) => {}
        }

        // `run_ticks`/`StrategyEngine` resolve a payload to a symbol SLOT by name, so a repeated
        // slot (even across venues) would silently route both series into one. A series naming its
        // ACCOUNT is the slot `SYMBOL@account` (decision 0293): one user name on two venues is two
        // slots once the leg on the other venue names its account.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for s in self.data.resolved_series() {
            if let Some(account) = s.account.as_deref() {
                let leg = s.leg_name();
                let named = vike_model::accounts::account_keys::parse_leg_name(&leg);
                if !matches!(&named, Ok(Some(on)) if on.venue == s.venue) {
                    return Err(HarnessError::from(ProfileRefusal::DataSeriesForeignAccount {
                        symbol: s.symbol.clone(),
                        account: account.to_string(),
                        venue: s.venue.clone(),
                    }));
                }
            }
            if !seen.insert(s.leg_name()) {
                return Err(HarnessError::from(ProfileRefusal::DataDuplicateSymbol {
                    leg: s.leg_name(),
                }));
            }
        }

        if self.data.kind == ReplayKind::Bar
            && let Some(s) = self.data.series.iter().find(|s| s.kind != SeriesKind::Tick)
        {
            return Err(HarnessError::from(ProfileRefusal::DataSeriesKindTickModeOnly {
                symbol: s.symbol.clone(),
                kind: s.kind,
            }));
        }

        self.data.validate_heard()?;

        // The PIT grid is looked up under ONE `default_venue`, so snapping a cross-venue slice
        // would read the wrong venue's grid for every series but the first.
        if self.engine.snap_to_properties && self.data.is_cross_venue() {
            return Err(HarnessError::from(ProfileRefusal::EngineSnapToPropertiesCrossVenue));
        }
        Ok(())
    }
}
