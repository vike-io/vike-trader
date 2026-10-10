//! `BacktestProfile`'s load-time validation: the first-error and the accumulating door, one walk.

use std::collections::BTreeSet;

use super::{BacktestProfile, DataKind, WindowForm, decide_mode};
use crate::harness::HarnessError;
use crate::hist_replay::SeriesKind;
use crate::walkforward::runner::WindowSearch;
use vike_model::Diagnostic;

#[cfg(doc)]
use super::{ImpactCfg, SizerCfg, WalkforwardCfg};

impl BacktestProfile {
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
    /// remote route each of those reruns is also a dial to a compute daemon that then re-reads the
    /// profile it was handed, so the round trips are neither free nor parallel. Nothing here is new
    /// validation — the rules, their order and their sentences are [`Self::validate`]'s — and the
    /// FIRST element is always the diagnostic `validate` itself would have returned. The two doors
    /// cannot disagree about which mistake governs, because they are one walk.
    ///
    /// Every diagnostic this produces today is [`vike_model::Severity::Error`]: a load-time
    /// refusal is all this validator has to say, and a profile that collects none of them is a
    /// profile that loads.
    ///
    /// # What it does NOT accumulate, said here rather than left to be discovered
    ///
    /// - **An unknown KEY — and that is a property of serde's error model, not a gap here.**
    ///   [`BacktestProfile`] is `#[serde(deny_unknown_fields)]`, and serde stops at the FIRST
    ///   undeclared key: by the time any rule below could run, deserialization has already failed
    ///   naming one key, and there is no profile to validate at all. So a file with five typo'd
    ///   key NAMES still reports one of them, and every diagnostic in this list is about a key
    ///   that parsed. Enumerating all five needs a `toml::Value` walk against the schema — a
    ///   different mechanism, checking a different thing, and deliberately not attempted from
    ///   inside a validator whose input is the already-deserialized struct.
    /// - **A second mistake inside ONE delegated rule.** A rule that calls out to
    ///   [`Self::validate_data_slice`], [`ImpactCfg::build`], [`SizerCfg::build`] or
    ///   [`WalkforwardCfg::window_form`] gets that function's own first error, so two mistakes in
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
    /// catch it: `crates/vike-backtest/src/profile_surface/render.rs` sorts its refusal rows by
    /// message text, so a source reorder moves nothing in the asset. This file's own tests are the
    /// only thing that would.
    ///
    /// ⚠ **The refusal SENTENCES live here, and their home is a published fact rather than a
    /// private one.** `profile_surface`'s `parse_refusals` reads this file as TEXT, harvests every
    /// `HarnessError::Validation(` literal, and publishes each one with the ENCLOSING `fn` as its
    /// evidence symbol into `crates/vike-backtest/tests/fixtures/profile.json`, which is compared
    /// in-process on every PR. So moving a sentence between functions rewrites that asset and
    /// rewording one rewrites the operator-facing reference: both are real edits with a re-render
    /// attached, never incidental tidying.
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
            let e = HarnessError::Validation(format!(
                "engine.cash must be > 0, got {}",
                self.engine.cash
            ));
            out.push(("engine.cash", e));
        }
        // `risk.max_orders_per_window` is a WALL-CLOCK order-rate throttle; sim time is not wall
        // time (a month of ticks can replay in under a second), so it is REJECTED at load rather
        // than silently ignored — `SimBroker::build_risk_gate` always disarms it regardless
        // (`limits.max_orders_per_window = None`), and a limit an operator believes is armed but
        // never actually checked is exactly the failure class this `[risk]` wiring exists to kill.
        // Every other `risk.*` limit is honored.
        if let Some(r) = &self.risk
            && r.max_orders_per_window.is_some()
        {
            let e = HarnessError::Validation(
                "risk.max_orders_per_window is a wall-clock order-rate throttle — meaningless \
                     in backtest sim time (a replayed month can execute in under a second, which \
                     would spuriously rate-limit every order; SimBroker::build_risk_gate always \
                     disarms this field for the identical reason on the live-parity gate it \
                     mounts). Remove it from `[risk]` — every other risk.* limit is honored"
                    .to_string(),
            );
            out.push(("risk.max_orders_per_window", e));
        }
        // Fail on a bad model name / horizon at PARSE time, not silently at fill time.
        //
        // ⚠ There is deliberately NO mode rejection here any more. This block used to refuse
        // `data.kind = "tick"` outright ("the tick lane replays a real book, which already
        // prices size better than any model"). The premise was only half true: an L1 tick
        // tape fills ANY size at the quote, and an L2 replay walks a RECORDING that never
        // reacts to the order, so the permanent term is missing on both. The lane now charges
        // exactly what its own price law has not already paid
        // (`crates/vike-sim/src/engine/sim_broker.rs`'s `impact_terms`), which is a
        // decision the engine can make per FILL and this validator could only ever have made
        // per RUN — the L2 tier degrades to the L1 tier on any event with no book, so even
        // one profile has both answers in it.
        if let Some(imp) = &self.engine.impact
            && let Err(e) = imp.build()
        {
            out.push(("engine.impact", e));
        }
        if self.engine.feed_latency && self.data.kind == DataKind::Bar {
            let e = HarnessError::Validation(
                "engine.feed_latency is tick-mode only: it re-orders recorded ticks by their \
                 machine receive stamp (local_ts), which a bar series does not carry"
                    .to_string(),
            );
            out.push(("engine.feed_latency", e));
        }
        // The queue-position fill model is consulted exclusively by the `run_ticks` tick/book replay
        // lanes, so it is tick-mode only (the mirror of the `feed_latency` rule). A bar-mode profile
        // setting it would silently do nothing — rejected here rather than mislead. Also validates
        // the string so a typo fails at load, not silently.
        if self.engine.queue_model.is_some() && self.data.kind == DataKind::Bar {
            let e = HarnessError::Validation(
                "engine.queue_model is tick-mode only: the FIFO queue lane runs in the tick/book \
                 replay path, which a bar series does not drive"
                    .to_string(),
            );
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
        if self.engine.equity_sample_every.is_some() && self.data.kind == DataKind::Bar {
            let e = HarnessError::Validation(
                "engine.equity_sample_every is tick-mode only: the bar lane records one equity \
                 sample per BAR (bounded by the bar count), so there is nothing to thin"
                    .to_string(),
            );
            out.push(("engine.equity_sample_every", e));
        }
        // Same rule for the tick-lane fill-model override: resolve it here so a typo ("L2Book",
        // "l2_book") fails the profile at LOAD with the valid set named, never a silent
        // optimistic-`Tick` fallback that would quietly undo the depth-cap realism knob (#819).
        if let Err(e) = self.engine.fill_model_kind() {
            out.push(("engine.fill_model", e));
        }
        // `attach_funding` window-joins the market funding series onto the price bars, and the
        // accrual it feeds runs ONLY in the bar loop — the tick lane has no per-interval funding
        // fold — so it is bar-mode only (the mirror of the tick-only `feed_latency` rule).
        if self.engine.attach_funding && self.data.kind == DataKind::Tick {
            let e = HarnessError::Validation(
                "engine.attach_funding is bar-mode only: the per-interval funding accrual runs in \
                 the bar loop; the tick lane carries no funding fold to feed"
                    .to_string(),
            );
            out.push(("engine.attach_funding", e));
        }
        // `cash_gate` is bar-mode only. `StrategyEngine::run` is its ONLY reader — the `granular`
        // mask and the `fill_step_gated` branch — and `StrategyEngine::fill_step_gated`'s single
        // caller sits inside that bar loop; `StrategyEngine::run_ticks` never consults it, and
        // neither does any `SimBroker` fill lane the tick path drives. So `kind = "tick"` +
        // `cash_gate = true` would parse, validate, reach `EngineParams` and change nothing — the
        // exact defect every other lane-asymmetric key here already refuses (`feed_latency`,
        // `queue_model`, `equity_sample_every`, `attach_funding`, `timeframes`).
        if self.engine.cash_gate && self.data.kind == DataKind::Tick {
            let e = HarnessError::Validation(
                "engine.cash_gate is bar-mode only: the shared-cash admission phase is \
                 StrategyEngine::run's own bar step (fill_step_gated), and run_ticks never \
                 consults it — a tick profile setting it would change nothing"
                    .to_string(),
            );
            out.push(("engine.cash_gate", e));
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
            // for a simultaneous cross-section would parse, validate, reach `EngineParams` and
            // change nothing.
            if self.data.kind == DataKind::Tick {
                let e = HarnessError::Validation(
                    "engine.decide = \"simultaneous\" is bar-mode only: the cross-sectional \
                     allocator it orders is StrategyEngine::fill_step_gated, reached only from \
                     StrategyEngine::run — run_ticks consults neither it nor cash_gate, so a tick \
                     profile setting it would change nothing"
                        .to_string(),
                );
                out.push(("engine.decide", e));
            }
            // (2) The DECLARED GAP. With a pre-trade gate armed (`[risk]`, or `engine.leverage`
            // through `SimBroker::build_risk_gate`'s leverage→initial-margin mapping),
            // `SimBroker::gate_order` folds the `pending` set — which ACCUMULATES across the
            // `on_bar` fan-out — so the symbol judged FIRST faces the step's whole budget and each
            // later one faces the remainder. Ordering the allocator cannot repair that: by the
            // time the fill phase ranks anything, the loser's order was already denied or shrunk
            // at submit. The fold is not the bug (a pre-trade cap must model the local view — see
            // `SimBroker::margin_in_use_pending_aware`); the missing piece is a step-boundary
            // budget snapshot, and it is NOT BUILT. Refused rather than accepted, because a mode
            // whose DECISION half is still taken in list order while its name says otherwise is
            // worse than no mode at all.
            //
            // ⚠ `clamp_to_leverage` does not escape this: it routes to
            // `SimBroker::cap_to_leverage`, whose own pending fold has the same shape.
            if self.risk.is_some() || self.engine.leverage.is_some() {
                let e = HarnessError::Validation(
                    "engine.decide = \"simultaneous\" is not built for a run with a pre-trade \
                     risk gate: remove [risk] and engine.leverage, or keep engine.decide \
                     unset. With a gate armed, SimBroker::gate_order folds the pending set as it \
                     ACCUMULATES during the on_bar fan-out, so the symbol judged first faces the \
                     whole step budget and later ones face the remainder — the DECISION is still \
                     taken in symbol-list order and no fill-phase ranking can repair it. Closing \
                     it needs a step-boundary budget snapshot, which does not exist"
                        .to_string(),
                );
                out.push(("engine.decide", e));
            }
            // (3) `detail_interval`, refused for exactly the reason `cash_gate` refuses it below:
            // this mode IMPLIES the gated lane, and `StrategyEngine::run`'s granular mask is
            // `!gated && !sub[i].is_empty()`, so the detail tape would be loaded, paid for and
            // then ignored.
            if self.data.detail_interval.is_some() {
                let e = HarnessError::Validation(
                    "data.detail_interval and engine.decide = \"simultaneous\" are mutually \
                     exclusive: the simultaneous mode implies the shared-cash admission phase, \
                     which routes the whole bar step through fill_step_gated and disables the \
                     granular sub-bar lane — the detail tape would be loaded, paid for and ignored"
                        .to_string(),
                );
                out.push(("engine.decide", e));
            }
        }
        // `timeframes` is bar-mode only (the coarse series are resampled from the base BAR
        // stream, and a tick profile has none), and every entry must be a valid,
        // strictly-coarser-than-base interval — left to the engine it is
        // `parse_timeframe(tf).expect(..)`, a process abort where every neighbouring key gives a
        // named refusal.
        if !self.engine.timeframes.is_empty() {
            if self.data.kind == DataKind::Tick {
                let e = HarnessError::Validation(
                    "engine.timeframes is bar-mode only: the coarse series are resampled from the \
                     base BAR stream, and a tick profile has none"
                        .to_string(),
                );
                out.push(("engine.timeframes", e));
            }
            // The base interval is the DENOMINATOR of the coarser-than-base rule below, so a
            // profile whose own `data.interval` does not parse gets that refusal and then the
            // COMPARISON is skipped — while each entry is still checked on its own terms, which
            // is the half that needs no denominator.
            let base = vike_model::time::interval_ms(&self.data.interval);
            if base.is_none() {
                let e = HarnessError::Validation(format!(
                    "data.interval {:?} is not a valid interval, so engine.timeframes cannot be \
                     checked against it",
                    self.data.interval
                ));
                out.push(("data.interval", e));
            }
            for tf in &self.engine.timeframes {
                let Some(ms) = vike_model::time::interval_ms(tf) else {
                    let e = HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} is not a valid interval (want a count then one \
                         of s/m/h/d, e.g. \"4h\")"
                    ));
                    out.push(("engine.timeframes", e));
                    continue;
                };
                if ms <= 0 {
                    let e = HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} resolves to {ms}ms — a zero-length window makes \
                         every boundary the same instant"
                    ));
                    out.push(("engine.timeframes", e));
                    // A zero-length window is also "not coarser than the base", so this entry has
                    // said its one useful thing — the second sentence would only repeat it.
                    continue;
                }
                if let Some(base) = base
                    && ms <= base
                {
                    let e = HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} ({ms}ms) must be coarser than data.interval {:?} \
                         ({base}ms) — a finer window cannot be synthesised from the base stream \
                         and would return the base bars re-labelled",
                        self.data.interval
                    ));
                    out.push(("engine.timeframes", e));
                }
            }
        }
        if self.engine.maint_margin < 0.0 {
            let e = HarnessError::Validation(format!(
                "engine.maint_margin must be >= 0, got {} — 0 means the liquidation watchdog is \
                 off, and a negative rate has no meaning",
                self.engine.maint_margin
            ));
            out.push(("engine.maint_margin", e));
        }
        if self.engine.liq_buffer < 0.0 {
            let e = HarnessError::Validation(format!(
                "engine.liq_buffer must be >= 0, got {}",
                self.engine.liq_buffer
            ));
            out.push(("engine.liq_buffer", e));
        }
        if let Some(v) = self.engine.volume_limit
            && !(v > 0.0 && v <= 1.0)
        {
            let e = HarnessError::Validation(format!(
                "engine.volume_limit must be in (0, 1], got {v} — it is the fraction of the \
                 EVENT's own volume one fill may take (a bar's volume in bar mode, ONE PRINT's \
                 in tick mode), so 1.0 is all of it"
            ));
            out.push(("engine.volume_limit", e));
        }
        if self.engine.multiplier <= 0.0 {
            let e = HarnessError::Validation(format!(
                "engine.multiplier must be > 0, got {} — it scales every position's notional, so \
                 zero makes every trade worth nothing",
                self.engine.multiplier
            ));
            out.push(("engine.multiplier", e));
        }
        for (sym, m) in &self.engine.multipliers {
            if *m <= 0.0 {
                let e = HarnessError::Validation(format!(
                    "engine.multipliers.{sym} must be > 0, got {m}"
                ));
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
            for sym in self.engine.multipliers.keys() {
                if !slice.contains(sym) {
                    let known: Vec<&str> = slice.iter().map(String::as_str).collect();
                    let e = HarnessError::Validation(format!(
                        "engine.multipliers.{sym} names a symbol this run does not load — the \
                         engine matches a multiplier row by EXACT symbol name and otherwise falls \
                         back to the global engine.multiplier, so this row would change nothing. \
                         The data slice is: {}",
                        known.join(", ")
                    ));
                    out.push(("engine.multipliers", e));
                }
            }
        }
        if let Some(l) = self.engine.leverage
            && l <= 0.0
        {
            let e = HarnessError::Validation(format!(
                "engine.leverage must be > 0, got {l} — omit the key for unlevered"
            ));
            out.push(("engine.leverage", e));
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
        // The mechanism: `SimBroker::build_risk_gate` returns `None` on `p.clamp_to_leverage`
        // BEFORE it so much as reads `p.risk_limits`, so the clamp disarms the WHOLE pre-trade
        // gate — every `[risk]` limit, not merely `max_leverage`. `[risk]` is the discarded side
        // here, which is the OPPOSITE of what the leverage/`[risk]` rule below describes for the
        // clamp-off case.
        if self.engine.clamp_to_leverage && self.risk.is_some() {
            let e = HarnessError::Validation(
                "engine.clamp_to_leverage is unavailable to a profile carrying [risk]: \
                 SimBroker::build_risk_gate returns None on clamp_to_leverage BEFORE it reads \
                 risk_limits, so the clamp disarms the pre-trade gate entirely and EVERY [risk] \
                 limit — not just max_leverage — is discarded. Drop clamp_to_leverage to keep \
                 [risk]'s gate, or remove [risk] and clamp against engine.leverage alone"
                    .to_string(),
            );
            out.push(("engine.clamp_to_leverage", e));
        }
        if self.engine.clamp_to_leverage && self.engine.leverage.is_none() {
            let e = HarnessError::Validation(
                "engine.clamp_to_leverage needs engine.leverage: there is nothing to clamp to"
                    .to_string(),
            );
            out.push(("engine.clamp_to_leverage", e));
        }
        // Two margin sources, one gate. `SimBroker::build_risk_gate` matches
        // `(&p.risk_limits, p.leverage)` and its `(Some(l), _)` arm takes `risk_limits`
        // UNCONDITIONALLY, so with the clamp OFF (the default) `engine.leverage` is the silently
        // discarded side — whether or not `[risk]` sets `max_leverage`. Refuse rather than let
        // that precedence pick for the operator, the same shape as the `engine.fee` /
        // `engine.fee_rate` refusal below. ⚠ The clamp-ON case is NOT this one and is refused
        // above: there the early `None` return discards `[risk]` instead.
        if self.engine.leverage.is_some() && self.risk.is_some() {
            let e = HarnessError::Validation(
                "engine.leverage and [risk] are two different margin sources for the same gate — \
                 set exactly one. With clamp_to_leverage off (the default) there are three \
                 configurations and [risk] wins all of them: SimBroker::build_risk_gate's \
                 (Some(l), _) arm takes risk_limits unconditionally, so a [risk] that sets \
                 max_leverage answers with ITS number, a [risk] that does not sets no margin \
                 requirement at all, and either way engine.leverage's 1/L im_requirement is \
                 silently discarded. (With clamp_to_leverage on, neither reaches a gate — that \
                 combination is refused separately.) Set risk.max_leverage instead, or remove \
                 [risk] to use engine.leverage's 1/L margin mapping"
                    .to_string(),
            );
            out.push(("engine.leverage", e));
        }
        if let Some(ms) = self.engine.settlement_period_ms
            && ms <= 0
        {
            let e = HarnessError::Validation(format!(
                "engine.settlement_period_ms must be > 0, got {ms} — omit the key to never \
                 settle"
            ));
            out.push(("engine.settlement_period_ms", e));
        }
        // Two cost models, one fee: refuse rather than let a silent precedence rule pick.
        if let Some(fee) = &self.engine.fee {
            if let Err(e) = fee.build() {
                out.push(("engine.fee", e));
            }
            if self.engine.fee_rate != 0.0 {
                let e = HarnessError::Validation(format!(
                    "engine.fee and a non-zero engine.fee_rate ({}) are two different cost \
                     models — set exactly one",
                    self.engine.fee_rate
                ));
                out.push(("engine.fee", e));
            }
        }
        // Fail on a bad `kind` / missing knob / missing wrapped base at PARSE time, the same
        // load-fast rule as `fee`/`impact` above — never a silent fallback to "no sizing", which
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
        if let Some(wf) = &self.walkforward {
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
            let form = match wf.window_form() {
                Ok(form) => Some(form),
                Err(e) => {
                    out.push(("walkforward", e));
                    None
                }
            };
            if form == Some(WindowForm::Splits) && wf.n_splits == Some(0) {
                let e = HarnessError::Validation(
                    "walkforward.n_splits must be >= 1, got 0".to_string(),
                );
                out.push(("walkforward.n_splits", e));
            }
            // Every duration is RESOLVED here so a typo or a zero fails at load, exactly as the
            // string knobs below are; the values are discarded because the driver reads them
            // again from the same resolvers.
            for (key, parsed) in [
                ("walkforward.train", wf.train_span()),
                ("walkforward.test", wf.test_span()),
                ("walkforward.step", wf.step_span()),
                ("walkforward.purge", wf.purge_span()),
                ("walkforward.embargo", wf.embargo_span()),
            ] {
                if let Err(e) = parsed {
                    out.push((key, e));
                }
            }
            // Every string knob is RESOLVED here so a typo fails at load; only `search`'s value is
            // wanted (the grid rule below reads it), and the other two are called for the parse
            // alone — the drivers read them again from the same resolvers.
            let search = match wf.window_search() {
                Ok(search) => Some(search),
                Err(e) => {
                    out.push(("walkforward.search", e));
                    None
                }
            };
            if let Err(e) = wf.walk_mode() {
                out.push(("walkforward.mode", e));
            }
            if let Err(e) = wf.rank_metric() {
                out.push(("walkforward.rank_by", e));
            }
            if search == Some(WindowSearch::Sweep) && !self.is_paramscan() {
                let e = HarnessError::Validation(
                    "walkforward.search = \"sweep\" needs a [sweep] table to search — this \
                     profile has none, so every window would 'select' the one parameter set it \
                     already has. Add the grid, or drop the key for the no-search control."
                        .to_string(),
                );
                out.push(("walkforward.search", e));
            }
        }
        // Fail on a bad resolution kind/window at PARSE time; the winners file + coverage check
        // need the profile's base_dir and run late, in `ResolutionCfg::build`.
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
        // to a compute daemon that then reads the profile it was handed. Each one decides whether a
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
        // ...and the ARMING rule for the two that are meaningless alone. A disposition or a
        // tolerance without `require_coverage` is a key an operator wrote, a reader will believe,
        // and nothing consults — `vike_config::CONSUMPTION`'s whole subject, and worse here than
        // there because what it appears to configure is a REFUSAL: somebody reads
        // `on_gap = "refuse"` and believes a run over an uncovered window cannot happen.
        if self.data.on_gap.is_some() && !self.data.require_coverage {
            let e = HarnessError::Validation(
                "data.on_gap is set without data.require_coverage = true, so the gate it \
                 configures is not armed and this key is read by nothing — while it reads as \
                 though a run over an uncovered window would be stopped. Arm the gate, or remove \
                 the disposition"
                    .to_string(),
            );
            out.push(("data.on_gap", e));
        }
        if self.data.max_gap.is_some() && !self.data.require_coverage {
            let e = HarnessError::Validation(
                "data.max_gap is set without data.require_coverage = true, so the gate whose \
                 tolerance it names is not armed and this key is read by nothing. Arm the gate, or \
                 remove the tolerance"
                    .to_string(),
            );
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
                let e = HarnessError::Validation(
                    "data.detail_interval and engine.cash_gate are mutually exclusive: \
                     cash_gate routes the whole bar step through the gated fill lane and \
                     UNCONDITIONALLY disables the granular sub-bar lane, so the detail tape would \
                     be loaded, scanned and then ignored while every order was reported as \
                     resolved against it. Set one."
                        .to_string(),
                );
                out.push(("data.detail_interval", e));
            }
            // ⚠ This one is LOOK-AHEAD, not waste. A walk hands each window a SLICED coarse series
            // while nothing slices the detail tape, and a sub-bar is bucketed by
            // `partition_point(|e| e <= sub.ts) - 1` — so every window's LAST coarse step would
            // absorb the whole remaining tape and fill against bars from AFTER the window ended.
            if self.walkforward.is_some() {
                let e = HarnessError::Validation(
                    "data.detail_interval is unavailable to a [walkforward] profile: the walk \
                     hands each window a sliced coarse series while nothing slices the detail \
                     tape, so every window's last step would fill against bars from AFTER the \
                     window ended. That is look-ahead, not merely waste. Run the profile without \
                     [walkforward], or without the detail tape."
                        .to_string(),
                );
                out.push(("data.detail_interval", e));
            }
        }
        // The per-share shapes model an EQUITIES book, where a contract multiplier is 1.0. The fee
        // path multiplies `FeeSchedule::commission`'s answer by the symbol's multiplier, which is
        // right for a notional cap and WRONG for the per-share term and the floor: a $1.00
        // per-order minimum is one dollar, not one dollar per contract, so a multiplier of 50
        // charges a $50 minimum. `FeeCfg` cannot see `[engine]`, so the rule is here.
        if let Some(fee) = &self.engine.fee
            && fee.kind == "per_share_with_floor"
        {
            let multiplied = self.engine.multiplier != 1.0
                || self.engine.multipliers.values().any(|m| *m != 1.0);
            if multiplied {
                let e = HarnessError::Validation(
                    "engine.fee.kind = \"per_share_with_floor\" needs engine.multiplier = 1.0 \
                     (the default) and no engine.multipliers row other than 1.0: the fee path \
                     scales the schedule's answer by the contract multiplier, so a per-ORDER \
                     minimum would be charged per CONTRACT. This shape models an equities book. \
                     Drop the multiplier, or price a multiplied contract with percent_maker_taker."
                        .to_string(),
                );
                out.push(("engine.fee.kind", e));
            }
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
                    let e = HarnessError::Validation(format!(
                        "data.from ({start}) must be <= data.to ({end})"
                    ));
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
                return Err(HarnessError::Validation(
                    "data.venue/data.symbols and [[data.series]] are two ways to name the SAME \
                     slice — set exactly one (the [[data.series]] form is the cross-venue one)"
                        .to_string(),
                ));
            }
            (false, false) => {
                return Err(HarnessError::Validation(
                    "data.symbols must not be empty (or use the cross-venue [[data.series]] form)"
                        .to_string(),
                ));
            }
            (true, false) => {
                if self.data.venue.is_none() {
                    return Err(HarnessError::Validation(
                        "data.venue is required alongside data.symbols".to_string(),
                    ));
                }
                if self.data.symbols.is_empty() {
                    return Err(HarnessError::Validation(
                        "data.symbols must not be empty".to_string(),
                    ));
                }
            }
            (false, true) => {}
        }

        // `run_ticks`/`StrategyEngine` resolve a payload to a symbol SLOT by name, so a repeated
        // symbol (even across venues) would silently route both series into one slot.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for s in self.data.resolved_series() {
            if !seen.insert(s.symbol.clone()) {
                return Err(HarnessError::Validation(format!(
                    "duplicate symbol {:?} in the data slice — the engine routes ticks to a \
                     symbol slot BY NAME, so two series cannot share one symbol",
                    s.symbol
                )));
            }
        }

        if self.data.kind == DataKind::Bar
            && let Some(s) = self.data.series.iter().find(|s| s.kind != SeriesKind::Tick)
        {
            return Err(HarnessError::Validation(format!(
                "data.series[{:?}].kind = {:?} is tick-mode only: a bar series has no \
                     quote/trade/book lanes to filter",
                s.symbol, s.kind
            )));
        }

        // The PIT grid is looked up under ONE `default_venue`, so snapping a cross-venue slice
        // would read the wrong venue's grid for every series but the first.
        if self.engine.snap_to_properties && self.data.is_cross_venue() {
            return Err(HarnessError::Validation(
                "engine.snap_to_properties is single-venue only: the point-in-time instrument \
                 grid is looked up under one default_venue, so a cross-venue [[data.series]] \
                 slice would snap every series to the first series' venue"
                    .to_string(),
            ));
        }
        Ok(())
    }
}
