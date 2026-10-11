//! Symbol lookups, the resolution latch, order-intake plumbing and the strategy-facing reads.

#[cfg(doc)]
use vike_model::HftBroker;
use vike_model::{Fill, OptionKind, Position, SymbolProperties, WorkingOrder, py_sum};

#[cfg(doc)]
use super::knobs::DecideMode;
#[cfg(doc)]
use super::params::EngineParams;
use super::state::SimBroker;

impl SimBroker {
    pub(super) fn idx(&self, symbol: &str) -> usize {
        self.symbols
            .iter()
            .position(|s| s == symbol)
            .unwrap_or_else(|| panic!("unknown symbol {symbol:?}"))
    }

    /// The PIT grid for `symbols[si]` at `ts`, or `None` when there is no filter source, no venue
    /// key, or no record — in which case the fill path stays unconstrained.
    pub(super) fn grid_for(&self, si: usize, ts: i64) -> Option<SymbolProperties> {
        let (venue, f) = (self.venue.as_deref()?, self.properties.as_ref()?);
        f(venue, &self.symbols[si], ts)
    }

    /// The per-symbol VENUE HOLD table in NANOSECONDS the latency gate arms with, indexed like
    /// [`Self::symbols`] — each market's `SymbolProperties::taker_hold_ms` at `ts`, resolved ONCE
    /// through the same PIT [`EngineParams::properties`] seam the fill grid uses (so a replay reads
    /// the hold the venue declared for that market, not a global constant).
    ///
    /// `ts` is the RUN START, not each action's stamp: a venue hold is a standing property of the
    /// market, and re-resolving it per submitted action would put a PIT lookup on the order path
    /// and let the in-flight queue's ordering drift mid-run.
    ///
    /// Returns EMPTY — the byte-identical, frozen path — with no properties source, no venue key,
    /// or when no symbol declares a hold (which is every venue but Polymarket).
    pub(crate) fn taker_hold_table_ns(&self, ts: i64) -> Vec<i64> {
        if self.venue.is_none() || self.properties.is_none() {
            return Vec::new();
        }
        let table: Vec<i64> = (0..self.symbols.len())
            .map(|si| {
                self.grid_for(si, ts)
                    .map_or(0, |p| i64::from(p.taker_hold_ms).saturating_mul(1_000_000))
            })
            .collect();
        if table.iter().all(|&h| h == 0) { Vec::new() } else { table }
    }

    /// The perp funding rate for `symbols[si]` at `ts` from the optional
    /// [`EngineParams::funding_source`] seam, or `None` when there is no source, no venue key, or
    /// no funding event at this ts — in which case no funding beyond `Bar.funding` is charged.
    pub(crate) fn funding_rate_for(&self, si: usize, ts: i64) -> Option<f64> {
        let (venue, f) = (self.venue.as_deref()?, self.funding_source.as_ref()?);
        f(venue, &self.symbols[si], ts)
    }

    // --- membership / caps ---

    pub fn is_active(&self, symbol: &str) -> bool {
        self.is_active_idx(self.idx(symbol))
    }

    pub(crate) fn is_active_idx(&self, si: usize) -> bool {
        match &self.active_mask {
            None => true,
            Some(mask) => {
                let sym = &self.symbols[si];
                mask.iter().find(|(s, _)| s == sym).map(|(_, m)| m[self.step]).unwrap_or(true)
            }
        }
    }

    // --- binary-resolution latch (the opt-in `EngineParams::resolution` lane) ---

    /// Has `symbol` already resolved in this run (see [`EngineParams::resolution`])? Once true it
    /// stays true — the market is settled, so the engine REFUSES every further order for it on
    /// both lanes. Always `false` when no resolution source is configured. A strategy can read
    /// this before quoting to avoid pushing orders that would be dropped.
    ///
    /// Total: an UNKNOWN symbol answers `false` rather than panicking (unlike the write verbs'
    /// `idx()`) — see the body for why a predicate differs from a write verb here.
    pub fn is_resolved(&self, symbol: &str) -> bool {
        // TOTAL on purpose — an unknown symbol is `false`, not a panic. This is public read-only
        // getter surface a strategy calls to decide whether to quote, typically from inside
        // `on_fill` / `on_bar` where a panic aborts the whole backtest. The engine's other
        // `idx()` callers are WRITE verbs, where an unknown symbol is a genuine strategy bug
        // worth failing loudly on; a predicate has an honest answer for a symbol this run never
        // registered — it has not resolved here — so it returns that instead.
        match self.symbols.iter().position(|s| s == symbol) {
            Some(si) => self.is_resolved_idx(si),
            None => false,
        }
    }

    /// `resolved` is EMPTY unless a resolution source is configured, so the default (parity-gated)
    /// order-intake path costs exactly one `is_empty` branch and never indexes.
    #[inline]
    pub(crate) fn is_resolved_idx(&self, si: usize) -> bool {
        !self.resolved.is_empty() && self.resolved[si]
    }

    /// Latch `symbols[si]` as resolved (idempotent). Called by the engine's resolution sweep
    /// BEFORE it settles, so the settlement's own `on_fill` re-entry is already refused.
    pub(crate) fn mark_resolved(&mut self, si: usize) {
        if !self.resolved.is_empty() {
            self.resolved[si] = true;
        }
    }

    // --- option-expiry payout (the opt-in `EngineParams::option_specs` lane) ---

    /// The PER-CONTRACT intrinsic settlement value for `symbols[si]` when it is a configured option
    /// that has expired at/before `probe_ts`, else `None`. CALL → `max(0, u − strike)`,
    /// PUT → `max(0, strike − u)`, where `u` is the engine's own mark for the option's `underlying`
    /// symbol ([`Self::price_of`] — the same last-seen-price field `equity_now` marks against).
    ///
    /// Returns `None` — the option is NOT settled — when: there is no source; the symbol is not an
    /// option (the source returns `None`); the spec names NO underlying (`underlying: None`, also
    /// reported once per symbol with a `tracing` warning — the rule is
    /// `crates/vike-sim/src/engine/settlement.rs`'s module doc); the option has not yet expired
    /// (`probe_ts < expiry_ms`); or the `underlying` names no registered symbol (no mark to settle
    /// against). The per-contract value is intentionally NOT scaled by the contract size here —
    /// the shared [`Self::settle_at_payout`] fold applies the option symbol's multiplier (its
    /// contract size) to `qty × payout`, exactly as it did on the entry fill, so
    /// `max(0, u − K)·contract_size·qty` falls out with no second scaling and no risk of
    /// double-applying it.
    pub(crate) fn option_payout(&mut self, si: usize, probe_ts: i64) -> Option<f64> {
        let source = self.option_specs.as_ref()?;
        let spec = source(&self.symbols[si])?;
        let Some(underlying) = spec.underlying.as_deref() else {
            self.report_option_without_underlying(si);
            return None;
        };
        if probe_ts < spec.expiry_ms {
            return None;
        }
        let under_si = self.symbols.iter().position(|s| s == underlying)?;
        let u = self.sym[under_si].price;
        Some(match spec.right {
            OptionKind::Call => (u - spec.strike).max(0.0),
            OptionKind::Put => (spec.strike - u).max(0.0),
        })
    }

    /// Warn — once per symbol, through the `option_no_underlying_reported` latch — that
    /// `symbols[si]`'s option spec names no underlying, so a held position in it is never settled
    /// at expiry. Called on every probe of such a spec; only the first one logs.
    fn report_option_without_underlying(&mut self, si: usize) {
        let Some(reported) = self.option_no_underlying_reported.get_mut(si) else {
            return;
        };
        if *reported {
            return;
        }
        *reported = true;
        tracing::warn!(
            symbol = %self.symbols[si],
            "option spec names no underlying: the option has no engine mark to settle against, \
             so a position in it is NOT settled at expiry and stays held (marked at its own last \
             price); give the spec an `underlying` that is a registered symbol of the run"
        );
    }

    /// The ONE order-intake choke point for the untagged `pending` lane: push `o` onto
    /// `symbols[si]`'s book UNLESS that symbol has resolved, in which case the order is REFUSED.
    /// Resolution is terminal — the market cannot trade again — so a strategy that submits from
    /// inside `on_fill` (the classic re-hedge / re-enter pattern) can never re-open a position in
    /// a market that settled in this very step, which would otherwise fill against the resolution
    /// bar's stale price and be settled again on the next step. The tagged maker lane is gated the
    /// same way in [`HftBroker::submit_limit_tagged`].
    ///
    /// Refusals are deliberately NOT recorded in `dropped`: a maker re-quoting on every tick after
    /// resolution would grow that diagnostics vec without bound. [`Self::is_resolved`] is the read
    /// a strategy or test uses instead, and the settlement itself is in [`Self::settlements`].
    #[inline]
    pub(super) fn push_pending(&mut self, si: usize, o: WorkingOrder) {
        if self.is_resolved_idx(si) {
            return;
        }
        // Opt-in latency gate (tick replay only; `None` everywhere else = the frozen path).
        if self.latency.is_some() {
            let desc = crate::latency::LatencyOrder::new(o.side, o.size, o.price);
            let (now, size, weight) = (self.now, o.size, o.weight);
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(now, &desc, crate::latency::InFlightAction::Push { si, order: o });
            if !sent {
                self.record_latency_reject(si, size, weight);
            }
            return;
        }
        self.sym[si].pending.push(o);
    }

    /// Record an order-path action the latency model REJECTED (a negative entry latency: the
    /// request never reached the matching engine) in the existing `dropped` diagnostics channel,
    /// under the kind `"latency_reject"` — the same convention `"volume_cap"` uses.
    ///
    /// Without this a calibrated series with a rejection burst silently swallows a large fraction
    /// of a strategy's orders and the run reports nothing, leaving "why did my maker stop quoting"
    /// undiagnosable. Bounded by the number of actions the strategy issues, unlike the
    /// post-resolution refusals `push_pending` deliberately does not record.
    pub(super) fn record_latency_reject(&mut self, si: usize, size: f64, weight: f64) {
        let sym = self.symbols[si].clone();
        self.dropped.push((sym, "latency_reject".into(), size, weight));
    }

    /// Apply ONE delivered in-flight action — the drain's per-action dispatch. Each arm is the
    /// zero-latency body of the matching strategy verb, so the delayed path and the immediate
    /// path share their semantics verbatim (including the resolution refusal, re-checked HERE
    /// because a market can resolve while an action is still in flight).
    pub(crate) fn apply_in_flight(&mut self, action: crate::latency::InFlightAction) {
        use crate::latency::InFlightAction as A;
        match action {
            A::Push { si, order } => {
                if !self.is_resolved_idx(si) {
                    self.sym[si].pending.push(order);
                }
            }
            A::CancelAll { si } => self.sym[si].pending.clear(),
            A::SubmitTagged { tag, order } => {
                if !self.is_resolved_idx(Self::HFT_SI) {
                    self.sym[Self::HFT_SI].tagged.insert(tag, order);
                }
            }
            A::ModifyTagged { tag, new_qty, new_price } => {
                if let Some(o) = self.sym[Self::HFT_SI].tagged.get_mut(&tag) {
                    if let Some(q) = new_qty {
                        if q > o.size {
                            o.qid = 0; // amend UP forfeits queue priority (see modify_tagged)
                        }
                        o.size = q;
                    }
                    if let Some(p) = new_price {
                        o.price = Some(p);
                    }
                }
            }
            A::CancelTagged { tag } => {
                self.sym[Self::HFT_SI].tagged.shift_remove(&tag);
            }
        }
    }

    // The three open-position caps below count POSITIONS (`pos.size != 0.0`), never resting or
    // in-flight orders, and are consulted at FILL time (`dispatch_fill`), not at submit. The
    // opt-in latency gate is therefore inert for them by construction: an order held in flight is
    // no more a position than the resting `pending` order it would otherwise have been, and by the
    // time it can breach a cap it has been delivered and filled through the same check. This is
    // NOT the shape `cap_to_leverage` has — that one is a pre-trade check that sums PENDING
    // notional, so it does have to see the in-flight queue (see there).
    pub(crate) fn at_open_cap(&self) -> bool {
        let cap = self.max_open_positions;
        cap != 0 && self.sym.iter().filter(|st| st.pos.size != 0.0).count() >= cap
    }

    pub(crate) fn at_long_cap(&self) -> bool {
        let cap = self.max_open_long;
        cap != 0 && self.sym.iter().filter(|st| st.pos.size > 0.0).count() >= cap
    }

    pub(crate) fn at_short_cap(&self) -> bool {
        let cap = self.max_open_short;
        cap != 0 && self.sym.iter().filter(|st| st.pos.size < 0.0).count() >= cap
    }

    // --- ATR helper ---

    /// Mean true range over the last `n` bars up to (and including) the current step.
    pub(super) fn atr(&self, si: usize, n: usize) -> f64 {
        let bars = &self.bars[si];
        if bars.is_empty() {
            return 0.0;
        }
        let end = self.step + 1;
        let start = 1.max(end.saturating_sub(n));
        if end < 2 {
            return 0.0;
        }
        let mut trs: Vec<f64> = Vec::new();
        for i in start..end {
            let bar = &bars[i];
            let prev_close = bars[i - 1].close;
            let tr = (bar.high - bar.low)
                .max((bar.high - prev_close).abs())
                .max((bar.low - prev_close).abs());
            trs.push(tr);
        }
        if trs.is_empty() { 0.0 } else { py_sum(trs.iter().copied()) / trs.len() as f64 }
    }

    // --- reads ---

    pub fn position_of(&self, symbol: &str) -> Position {
        self.sym[self.idx(symbol)].pos
    }

    pub fn price_of(&self, symbol: &str) -> f64 {
        self.sym[self.idx(symbol)].price
    }

    pub fn multiplier_of(&self, symbol: &str) -> f64 {
        self.mult.iter().find(|(s, _)| s == symbol).map(|(_, m)| *m).unwrap_or(self.multiplier)
    }

    pub(crate) fn multiplier_of_idx(&self, si: usize) -> f64 {
        self.mult_by_si[si] // O(1) index; byte-identical to multiplier_of(symbols[si])
    }

    /// THE cross-sectional walk order — every fold over the whole book goes through here rather
    /// than over `0..symbols.len()` directly, so there is ONE place that decides whether a
    /// portfolio read is a property of the instrument SET or of the list the caller typed.
    ///
    /// ⚠ Under [`DecideMode::Sequential`] this is the identity permutation, so a fold routed
    /// through it visits the same indices in the same sequence as the raw range it replaced and
    /// the result is bit-for-bit the frozen one. That equality is the whole reason the indirection
    /// is safe to add to a parity-frozen path: nothing about the default answer changes, only
    /// where the order comes from.
    #[inline]
    pub(crate) fn cross_section(&self) -> &[usize] {
        &self.cross_section
    }

    /// cash + Σ pos·price·mult (Python builtin sum → py_sum, cross-section order — the symbols
    /// order under the default [`DecideMode::Sequential`]).
    pub fn equity_now(&self) -> f64 {
        self.cash
            + py_sum(self.cross_section().iter().map(|&si| {
                vike_model::signed_notional(
                    self.sym[si].pos.size,
                    self.sym[si].price,
                    self.multiplier_of_idx(si),
                )
            }))
    }

    pub fn drawdown_now(&self) -> f64 {
        let eq = self.equity_now();
        let peak = self.equity_peak;
        if peak <= 0.0 {
            return 0.0;
        }
        f64::max(0.0, (peak - eq) / peak)
    }

    /// Resting untagged orders the MATCHING ENGINE holds for `symbol`.
    ///
    /// HAZARD under the opt-in latency gate: an order the strategy has submitted but that is still
    /// in flight is in NEITHER this slice nor the position — it does not exist anywhere the
    /// strategy can see it except [`Self::in_flight_of`]. The common idiom
    /// `if ctx.pending_of(s).is_empty() { ctx.submit_limit(..) }` therefore re-fires on every tick
    /// of the entry-latency window and can rest N duplicate orders; write it as
    /// `if ctx.pending_of(s).is_empty() && ctx.in_flight_of(s) == 0 { .. }` instead. (The TAGGED
    /// maker lane is immune — an insert under an existing tag replaces it.)
    pub fn pending_of(&self, symbol: &str) -> &[WorkingOrder] {
        &self.sym[self.idx(symbol)].pending
    }

    /// The armed protective stop level for `symbol`, or `None` when nothing is armed. Read-only
    /// view of the implicit bracket `submit(.., stop)` arms — the level `check_stop` breaches
    /// against, and the OCO sibling a closing-side limit fill disarms.
    pub fn protective_stop_of(&self, symbol: &str) -> Option<f64> {
        self.sym[self.idx(symbol)].stop
    }

    /// How many NEW-ORDER actions for `symbol` are held in flight by the opt-in latency gate —
    /// submitted by the strategy, not yet visible to the matching engine. Always `0` when the
    /// gate is not armed, which is every path but a `run_ticks` replay with
    /// `EngineParams::latency_model = Some(..)`. Pair it with [`Self::pending_of`] (see the
    /// duplicate-submit hazard documented there).
    ///
    /// Total on an unknown symbol (`0`) rather than a panic, like [`Self::is_resolved`]: it is a
    /// read a strategy calls to decide whether to quote.
    pub fn in_flight_of(&self, symbol: &str) -> usize {
        let (Some(g), Some(si)) =
            (self.latency.as_ref(), self.symbols.iter().position(|s| s == symbol))
        else {
            return 0;
        };
        g.in_flight_orders_for(si)
    }

    /// Advance the strategy-visible shadow position by ONE fill the response leg has DELIVERED.
    /// No-op (and no symbol lookup) unless the gate is armed — `shadow_pos` is empty otherwise.
    /// `Fill::symbol` is always `symbols[si]` (see `apply_fill`), so the lookup always resolves;
    /// an unrecognized symbol is skipped rather than panicking inside a delivery loop.
    pub(crate) fn advance_shadow(&mut self, fill: &Fill) {
        if self.shadow_pos.is_empty() {
            return;
        }
        if let Some(si) = self.symbols.iter().position(|s| *s == fill.symbol) {
            self.shadow_pos[si] += fill.side as f64 * fill.size;
        }
    }
}
