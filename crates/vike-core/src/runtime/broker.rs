//! `LiveBroker` — the live `Broker`/`HftBroker` the strategy mount drives — split out of the runtime fold module (behavior byte-identical; the
//! block moved verbatim). `use super::*` re-exports the parent runtime module's full
//! import set + items, so nothing about resolution changes.

use super::*;

/// The live strategy seam (R7): the unified [`Strategy`] trait's `on_bar` fires ONCE per
/// CLOSED bar of the mounted series, AFTER the client's paper fills for that bar
/// (next-open discipline: orders submitted on bar i fill during bar i+1) and after the
/// bar joined the cache. Orders go through the ONE live path — ClientOrderIdGenerator → RiskGate →
/// client (paper or venue). [`LiveBroker`] is the live [`Broker`]: verbs BUFFER submissions
/// (drained after the handler returns) — the deferred twin of the sim broker's direct
/// mutation. Handlers are skipped until `index >= warmup()` (the R2 gate).
///
/// Strategy-facing view + order intake for one `on_bar` call.
pub struct LiveBroker {
    /// signed position in the mounted symbol (the engine's BOTH leg)
    pub position: f64,
    /// the bar's close (the engine's post-fill `price` mark)
    pub price: f64,
    /// resolver-priced account equity at the pre-strategy state, **CAPPED** by
    /// `vike_config::Policy::max_sizing_equity` where one is armed — this is a SPENDING lane, so it
    /// reads `ExecutionEngine::sizing_equity` (under `CoreConfig::price_cfg`) and not
    /// `resolved_equity`, which is what the snapshot/sampler display keep. ⚠ On the `on_fill` lane
    /// it arrives as `AppliedFill::equity_after`, capped at the fold through the shared
    /// `ExecutionEngine::cap_sizing_equity` so the two spellings cannot disagree; this doc named
    /// `resolved_equity` until the ceiling landed and was stale for both.
    pub equity: f64,
    /// every closed bar of the mounted series up to and INCLUDING this one (Arc clone)
    pub bars: Arc<Vec<Bar>>,
    /// bars.len() - 1
    pub index: usize,
    /// this bar's ts (epoch ms)
    pub now: i64,
    /// contract multiplier of the mounted symbol (Account::multiplier_of; 1.0 default) —
    /// the order_target_* sizing law needs it (Phase C)
    pub multiplier: f64,
    /// the gate's lot grid (RiskLimits::lot_size; 0.0 = no grid) — set_holdings lands on it
    pub lot_size: f64,
    // `pub` (was `pub(crate)`) so the vike-mm maker crate's white-box tests can construct a
    // `LiveBroker` by struct literal and assert on the buffered verbs — a TEST-only surface
    // (vike-mm dev-deps vike-core). The live drain still owns/empties these internally.
    pub submissions: Vec<BufferedSubmit>,
    pub modifications: Vec<BufferedModify>,
    /// per-tag cancels (pull ONE quote) — resolved tag→coid at drain time, like modifications
    pub cancels: Vec<String>,
    /// Per-symbol position/mark for every instrument the mount declared
    /// ([`crate::StrategyMount::symbols`]) plus its OWN symbol, MINUS the one series that
    /// dispatched — which the scalars above already answer for, with the fresher number (this bar's
    /// close, this tick's price, this fill's `position_after`). `CoreThread::declared_views` is the
    /// authority on that partition and on which ENGINE each row is read from.
    /// EMPTY for every single-symbol mount — the reads then fall through to the `position`/`price`
    /// scalars above, byte-identically. Owned (not a borrow of engine state) so the strategy call
    /// still cannot alias the rest of the core.
    ///
    /// A Vec rather than a map: a declaration is a handful of symbols, so a linear scan beats a
    /// hash, and this is rebuilt per strategy dispatch.
    pub positions: Vec<(String, f64)>,
    pub prices: Vec<(String, f64)>,
    /// Per-symbol CLOSED-BAR history — the [`Broker::bars`] twin of `positions`/`prices`, built by
    /// the same `CoreThread::declared_views` pass, off the same per-leg venue rule, so a leg's
    /// history, its position and its orders all name ONE book.
    ///
    /// ⚠ Unlike those two it carries EVERY declared instrument, the DISPATCHING one INCLUDED. There
    /// is no per-fill freshness to protect for a bar series (the `bars` scalar is the MOUNT's own
    /// history on every lane but bar/tick, not a per-dispatch number), and that completeness is what
    /// makes this table the membership oracle all three per-symbol reads consult — see
    /// [`LiveBroker::carries`].
    ///
    /// EMPTY for every single-symbol mount (`MountSpec::legs` is empty on every mount `vike-mount`
    /// builds), which is what keeps the ordinary dispatch byte-identical: empty ⇒ every read falls
    /// through to the scalars above exactly as it did before this table existed.
    ///
    /// `Arc` clones (a refcount bump), never a borrow of engine state — the same rule the rest of
    /// the ctx follows.
    pub bar_views: Vec<(String, Arc<Vec<Bar>>)>,
    pub brackets: Vec<BufferedBracket>,
    pub conditionals: Vec<BufferedConditional>,
    pub mass_cancel: bool,
}

impl LiveBroker {
    /// The symbol a `Broker` verb named, or `None` when it named nothing usable.
    ///
    /// EMPTY IS `None` ON PURPOSE: live bars carry `Bar::symbol == None`, so a strategy doing
    /// `bar.symbol.clone().unwrap_or_default()` passes `""` and means "my mount" — see
    /// `r7_gate.rs` / `crates/vike-core/src/runtime/tests/safe_state/mount_budget.rs`'s
    /// `BudgetSubmit` / vike-script's live bar lane, which all rely on it.
    /// Treating `""` as a symbol would send it to a venue.
    fn named(symbol: &str) -> Option<String> {
        (!symbol.is_empty()).then(|| symbol.to_string())
    }

    /// A per-symbol read, or `None` for "this table cannot answer" — which [`Broker::position`] /
    /// [`Broker::price`] then resolve through [`LiveBroker::carries`], NOT by falling back blindly.
    /// Empty table (every single-symbol mount) never answers.
    ///
    /// ⚠ A miss is one of exactly THREE things, and they do not get the same answer. (1) The
    /// DISPATCHING symbol on the fill lane, which `CoreThread::declared_views` deliberately leaves
    /// out so the scalar — this fill's `position_after` — stays the answer. (2) A declared leg whose
    /// PRICE the resolver could not produce (the position row still landed; see
    /// `CoreThread::push_view`). (3) A symbol this mount never declared, which gets the EMPTY answer
    /// rather than the dispatching series' number.
    fn declared_read(&self, table: &[(String, f64)], symbol: &str) -> Option<f64> {
        if table.is_empty() || symbol.is_empty() {
            return None;
        }
        table.iter().find(|(s, _)| s == symbol).map(|(_, v)| *v)
    }

    /// **Is `symbol` an instrument this broker can answer about at all?**
    ///
    /// This is the one predicate that separates "the mount carries it, the tables just did not have
    /// a row this dispatch" from "the mount does not carry it", and it reads [`Self::bar_views`]
    /// because that table alone is COMPLETE for the declared set (`positions` drops the fill lane's
    /// dispatching symbol; `prices` drops anything the resolver could not price).
    ///
    /// Two shapes answer TRUE without a lookup, and both are load-bearing:
    ///
    /// - an EMPTY table ⇒ a single-symbol mount, where [`Broker::bars`]' own contract licenses
    ///   ignoring the argument, `resolve_intent_symbol` likewise routes every intent to the mount's
    ///   own symbol, and shipped strategies pass the mount's symbol meaning "mine". Answering
    ///   "not carried" there would break every strategy running today to fix a mount shape that does
    ///   not exist yet.
    /// - an EMPTY symbol ⇒ "my mount", the `""` convention [`LiveBroker::named`] documents (live bars
    ///   carry `Bar::symbol == None`, so `bar.symbol.clone().unwrap_or_default()` passes `""`).
    fn carries(&self, symbol: &str) -> bool {
        self.bar_views.is_empty()
            || symbol.is_empty()
            || self.bar_views.iter().any(|(s, _)| s == symbol)
    }

    /// Rebalance to an absolute SIGNED position size — exact port of
    /// `exec/live_portfolio_engine.py::order_target`: submit the market delta when
    /// `|target - position| > 1e-12` (the oracle's dead-band), else no-op.
    pub fn order_target(&mut self, target_size: f64) {
        let delta = target_size - self.position;
        if delta.abs() > 1e-12 {
            self.submissions.push(BufferedSubmit {
                symbol: None,
                side: if delta > 0.0 { 1 } else { -1 },
                qty: delta.abs(),
                order_type: "market".into(),
                price: None,
                reduce_only: false,
                tag: None,
            });
        }
    }

    /// Rebalance to a target NOTIONAL value — port of `order_target_value`: no-op without
    /// a usable price (oracle guard `px <= 0.0`), else `units_from_value` → order_target.
    pub fn order_target_value(&mut self, value: f64) {
        if self.price <= 0.0 {
            return;
        }
        self.order_target(units_from_value(value, self.price, self.multiplier));
    }

    /// Rebalance to a fraction of current equity — port of `order_target_percent`:
    /// `units_from_percent(pct, equity, price, multiplier)` → order_target.
    pub fn order_target_percent(&mut self, pct: f64) {
        if self.price <= 0.0 {
            return;
        }
        self.order_target(units_from_percent(pct, self.equity, self.price, self.multiplier));
    }

    /// LEAN `SetHoldings` twin — the margin-aware upgrade over [`Self::order_target_percent`]:
    /// sizes with the Phase B `amount_to_order` walk so the position lands AT or UNDER the
    /// target on the lot grid, making repeated `set_holdings(x)` a no-op once filled (the
    /// LEAN idempotence law). Without a configured lot grid it falls back to the oracle
    /// `order_target_percent` path.
    pub fn set_holdings(&mut self, pct: f64) {
        if self.price <= 0.0 {
            return;
        }
        if self.lot_size > 0.0 {
            let target_notional = pct * self.equity;
            let unit = self.price * self.multiplier;
            let delta = amount_to_order(self.position, target_notional, unit, self.lot_size);
            if delta.abs() > 1e-12 {
                self.submissions.push(BufferedSubmit {
                    symbol: None,
                    side: if delta > 0.0 { 1 } else { -1 },
                    qty: delta.abs(),
                    order_type: "market".into(),
                    price: None,
                    reduce_only: false,
                    tag: None,
                });
            }
        } else {
            self.order_target_percent(pct);
        }
    }

    /// Arm a protective STOP in the core-owned ConditionalBook (port of
    /// `LiveEngine.submit_stop` → `ConditionalBook.add_stop`). Checked per closed bar
    /// BEFORE `on_bar` (oracle firing order); fires as a plain market through the gate.
    pub fn submit_stop(&mut self, side: i32, qty: f64, price: f64) {
        self.conditionals.push(BufferedConditional {
            symbol: None,
            side,
            qty,
            price: Some(price),
            trail: None,
        });
    }

    /// Arm a TRAILING stop (port of `LiveEngine.submit_trailing`): the extreme seeds from
    /// the current mark at drain time; refused (surfaced in recent-events) without a mark.
    pub fn submit_trailing(&mut self, side: i32, qty: f64, trail: f64) {
        self.conditionals.push(BufferedConditional {
            symbol: None,
            side,
            qty,
            price: None,
            trail: Some(trail),
        });
    }

    /// Reduce-only market order (live-only concept; not on the portable [`Broker`] surface).
    pub fn submit_market_reduce(&mut self, side: i32, qty: f64) {
        self.submissions.push(BufferedSubmit {
            symbol: None,
            side,
            qty,
            order_type: "market".into(),
            price: None,
            reduce_only: true,
            tag: None,
        });
    }

    /// Submit a TAGGED limit order (HFT modify surface). `tag` is a strategy-chosen stable id for
    /// this resting order; a later [`LiveBroker::modify`] on the same tag re-prices it in place.
    /// Live-only (no Python twin) — a strategy using tags is `impl Strategy<LiveBroker>`, which the
    /// type system documents as not backtest-portable.
    pub fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        self.submissions.push(BufferedSubmit {
            symbol: None,
            side,
            qty,
            order_type: "limit".into(),
            price: Some(price),
            reduce_only: false,
            tag: Some(tag.to_string()),
        });
    }

    /// Submit a TAGGED market order (HFT modify surface). See [`LiveBroker::submit_limit_tagged`].
    pub fn submit_market_tagged(&mut self, tag: &str, side: i32, qty: f64) {
        self.submissions.push(BufferedSubmit {
            symbol: None,
            side,
            qty,
            order_type: "market".into(),
            price: None,
            reduce_only: false,
            tag: Some(tag.to_string()),
        });
    }

    /// Modify a previously-tagged resting order by tag (RUST-NATIVE; no Python twin). No-op if the
    /// tag is unknown or its order is terminal (resolved at drain time against the runtime's
    /// tag→coid registry, then gated by [`ExecutionEngine::modify_order`]).
    pub fn modify(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modifications.push(BufferedModify { tag: tag.to_string(), new_qty, new_price });
    }

    /// Cancel ONE previously-tagged resting order by tag (RUST-NATIVE; no Python twin) — the
    /// per-side twin of [`LiveBroker::mass_cancel`]. Used to PULL a single quote (e.g. a market
    /// maker suppressing one side under adverse selection) without disturbing the other side. No-op
    /// if the tag is unknown or its order is already terminal (resolved at drain time against the
    /// runtime's tag→coid registry, then gated by [`ExecutionEngine::cancel_order`]).
    pub fn cancel_tagged(&mut self, tag: &str) {
        self.cancels.push(tag.to_string());
    }

    /// Cancel ALL of this engine's live orders (pull-all-quotes) after the handler returns (HFT;
    /// no Python twin). Drained AFTER this call's submits/modifications.
    pub fn mass_cancel(&mut self) {
        self.mass_cancel = true;
    }

    /// Submit a BRACKET (entry + protective stop-loss + take-profit) — RUST-NATIVE, no Python twin.
    /// `entry_price` None = market entry. The runtime mints the three coids at drain time and wires
    /// the OTO/OCO contingency via [`vike_model::build_bracket`]; the exits are reduce-only,
    /// opposite-side, sized to `qty`.
    pub fn submit_bracket(
        &mut self,
        side: i32,
        qty: f64,
        entry_price: Option<f64>,
        stop_loss: f64,
        take_profit: f64,
    ) {
        self.brackets.push(BufferedBracket {
            symbol: None,
            side,
            qty,
            entry_price,
            stop_loss,
            take_profit,
        });
    }
}

/// The live [`Broker`].
///
/// ⚠ The `symbol` argument is NOT ignored any more — that sentence stood here through #916/#924/#997
/// and was the exact claim the two-leg routing bug hid behind. Today:
///
/// - `submit_market`/`submit_limit` RECORD the argument ([`LiveBroker::named`]); the runtime's
///   `drain_broker` decides whether to honour it, per the mount's `StrategyMount::symbols`
///   declaration. An undeclared mount ignores it exactly as before; a declared one honours a
///   declared symbol and REFUSES anything else.
/// - `position`/`price` answer per symbol out of the tables `CoreThread::declared_views` builds
///   (again empty, hence scalar, for an undeclared mount) — on EVERY strategy-hook lane, not just
///   the bar/tick/reference-quote ones, and resolved against the leg's OWN venue's engine.
/// - `bars` answers per symbol too, out of [`LiveBroker::bar_views`], off that same pass and that
///   same per-leg venue rule. It was the last read that discarded the argument outright.
/// - a symbol a DECLARED mount does not carry gets the EMPTY answer — `0.0` / `0.0` / `&[]` — and
///   never the dispatching series' numbers. See [`LiveBroker::carries`] for the partition and the
///   header of `Broker::position` below for why empty is the right shape.
impl Broker for LiveBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.submissions.push(BufferedSubmit {
            symbol: Self::named(symbol),
            side,
            qty,
            order_type: "market".into(),
            price: None,
            reduce_only: false,
            tag: None,
        });
    }

    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, price: f64) {
        self.submissions.push(BufferedSubmit {
            symbol: Self::named(symbol),
            side,
            qty,
            order_type: "limit".into(),
            price: Some(price),
            reduce_only: false,
            tag: None,
        });
    }

    /// ## What an UNCARRIED symbol answers, and why it is `0.0` rather than the mount's number
    ///
    /// A mount that DECLARED its instruments has said which book it trades. A symbol outside that
    /// set is one this broker cannot address: `resolve_intent_symbol` REFUSES an order for it (a
    /// `Denied` lifecycle event plus an operator ring line), no fill can ever carry it, and no
    /// engine row is read for it. `0.0` is therefore the literal truth about THIS MOUNT — it holds
    /// none of that instrument, and by construction never can.
    ///
    /// The rejected alternatives, in order of how tempting they are:
    ///
    /// - **the dispatching series' position** (what shipped until this method got its `carries`
    ///   arm): a number about a DIFFERENT instrument, indistinguishable from a real one. This is the
    ///   whole defect — `vike_strategy::strategies::pairs::PairsZScore` closing leg A with leg B's signed size
    ///   is what it costs.
    /// - **`0.0` unconditionally**, i.e. for a single-symbol mount too: that breaks every strategy
    ///   running today, which passes the mount's own symbol (or `""`) to mean "mine" and would start
    ///   reading flat. Hence [`LiveBroker::carries`]' empty-table arm.
    /// - **panic, as `SimBroker::idx` does** on an unknown symbol: a backtest may die naming the
    ///   symbol — nothing is at risk but the run. The live core thread folds venue events for every
    ///   mount on the process; killing it because one strategy asked a question flattens nothing,
    ///   cancels nothing, and strands real resting orders at the venue.
    ///
    /// The parity that matters is preserved, and it is not "the same number": NEITHER engine answers
    /// about a different instrument. `crates/vike-sim/tests/multi_symbol_read_parity.rs` is the
    /// gate for the carried half, where the two engines genuinely must agree.
    fn position(&self, symbol: &str) -> f64 {
        match self.declared_read(&self.positions, symbol) {
            Some(pos) => pos,
            None if self.carries(symbol) => self.position,
            None => 0.0,
        }
    }

    /// Per symbol, with [`Broker::position`]'s partition — plus one RESIDUAL, stated rather than
    /// glossed: a declared leg the price resolver could not answer for (no mark, no quote, no trade,
    /// no bar — `CoreThread::push_view` writes no price row for it) still falls through to the
    /// scalar, i.e. to the DISPATCHING series' price. It is carried, so `carries` says nothing about
    /// it; the tables cannot tell that case apart from the fill lane's dispatching symbol, whose
    /// scalar IS its own price. Closing it means teaching `LiveBroker` which symbol dispatched, a
    /// second field on a struct built per market message — a separate change with its own latency
    /// argument. The uncarried case, which is what this PR is about, is unaffected: it never reaches
    /// the scalar at all.
    fn price(&self, symbol: &str) -> f64 {
        match self.declared_read(&self.prices, symbol) {
            Some(px) => px,
            None if self.carries(symbol) => self.price,
            None => 0.0,
        }
    }

    fn equity(&self) -> f64 {
        self.equity
    }

    /// Closed bars of `symbol` — the per-symbol table [`LiveBroker::bar_views`], falling back to the
    /// DISPATCHING series only where that is the contract rather than an accident.
    ///
    /// This read used to discard its argument outright (it was spelled `_symbol`). [`Broker::bars`]'
    /// own contract licenses that — "single-symbol live brokers may ignore `symbol`" — and for a
    /// single-symbol mount it is still exactly what happens here, byte-identically, because the
    /// table is empty. A mount that DECLARED a second leg is not a single-symbol broker, and there
    /// the licence lapsed: `bars(leg_b)` during a leg-A bar handed back leg A's history, so a
    /// strategy computing a spread from it read one leg twice — silently, and only when live, since
    /// `SimBroker::bars` indexes by symbol.
    ///
    /// A DECLARED mount asked about a symbol it does not carry gets an EMPTY slice: the only shape a
    /// `&[Bar]` return can express for "I have no history of that", and the one a strategy already
    /// handles (`bars(x).last()` is `None`, every indicator warmup gate is unmet). It cannot be
    /// mistaken for another instrument's history the way the old answer could. See
    /// [`Broker::position`] for the full argument over the alternatives.
    ///
    /// ⚠ COST, since this struct is built once per SUBSCRIBED MOUNT per market message on
    /// `CoreThread::drive_strategy_tick` — the fold the `p99 < 10µs` gate protects: a single-symbol
    /// mount pays one `Vec::new()` and one more drop, no allocation and no engine read, because
    /// `CoreThread::declared_views` early-returns `Default` for it. Only a declared-multi mount
    /// materializes rows, and `crates/vike-core/tests/runtime_latency.rs`'s `mounted-multi` variant
    /// is the lane that measures them.
    fn bars(&self, symbol: &str) -> &[Bar] {
        if self.bar_views.is_empty() || symbol.is_empty() {
            return self.bars.as_slice();
        }
        match self.bar_views.iter().find(|(s, _)| s == symbol) {
            Some((_, series)) => series.as_slice(),
            None => &[],
        }
    }

    fn index(&self) -> usize {
        self.index
    }

    fn now(&self) -> i64 {
        self.now
    }
}

/// The live HFT broker: routes the tagged-order verbs through [`LiveBroker`]'s existing inherent
/// methods (each BUFFERS into the same drain queues as the portable [`Broker`] verbs), and exposes
/// the signed position off the strategy-facing field. This is the seam that lets `SpreadMaker` (and
/// any future maker) be `impl<B: HftBroker> Strategy<B>` while still mounting as
/// `Strategy<LiveBroker>`. Pure delegation — no behavior change vs. calling the inherent methods
/// directly.
impl HftBroker for LiveBroker {
    fn position(&self) -> f64 {
        self.position
    }

    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        LiveBroker::submit_limit_tagged(self, tag, side, qty, price);
    }

    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modify(tag, new_qty, new_price);
    }

    fn cancel_tagged(&mut self, tag: &str) {
        LiveBroker::cancel_tagged(self, tag);
    }
}

#[path = "tests/broker.rs"]
#[cfg(test)]
mod broker_tests;
