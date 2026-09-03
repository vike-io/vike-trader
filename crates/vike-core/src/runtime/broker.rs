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
    /// resolver-priced account equity (`ExecutionEngine::resolved_equity` under
    /// `CoreConfig::price_cfg` — the same source the snapshot/sampler display) at the
    /// pre-strategy state
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
    /// EMPTY for every single-symbol mount (`MountSpec::legs` is empty on every mount `vike-run`
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
    /// `r7_gate.rs` / `safe_state_tests.rs` / vike-script's live bar lane, which all rely on it.
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

    /// LEAN `SetHoldings` twin — the margin-aware upgrade over [`order_target_percent`]:
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
    ///   whole defect — `vike_strategy::pairs::PairsZScore` closing leg A with leg B's signed size
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
    /// about a different instrument. `crates/vike-backtest/tests/multi_symbol_read_parity.rs` is the
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
    /// ⚠ COST, since this struct is built once per MARKET MESSAGE on
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty-buffer `LiveBroker`, the shape `runtime::strategy_drive` hands a strategy.
    /// `bars`/`index` are unread by every verb driven below (none of them looks at history).
    fn ctx() -> LiveBroker {
        LiveBroker {
            position: 5.0,
            price: 100.0,
            equity: 10_000.0,
            bars: Arc::new(Vec::new()),
            index: 0,
            now: 0,
            multiplier: 1.0,
            lot_size: 0.0,
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            positions: Vec::new(),
            prices: Vec::new(),
            bar_views: Vec::new(),
            brackets: Vec::new(),
            conditionals: Vec::new(),
            mass_cancel: false,
        }
    }

    fn bar(close: f64) -> Bar {
        Bar {
            ts: 1,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// The shape `declared_views` hands a DECLARED-multi mount: a row per declared instrument in
    /// every table, and — the point of this fixture — nothing at all for `"SOMETHING_ELSE"`.
    fn declared_ctx() -> LiveBroker {
        LiveBroker {
            positions: vec![("ETHUSDT".into(), -7.0)],
            prices: vec![("ETHUSDT".into(), 50.0)],
            bar_views: vec![
                ("BTCUSDT".into(), Arc::new(vec![bar(100.0)])),
                ("ETHUSDT".into(), Arc::new(vec![bar(50.0)])),
            ],
            ..ctx()
        }
    }

    /// A DECLARED mount answers about the symbol NAMED — including `bars`, which discarded its
    /// argument until this method got its table.
    #[test]
    fn a_declared_mount_answers_per_symbol() {
        let b = declared_ctx();
        assert_eq!(Broker::position(&b, "ETHUSDT"), -7.0);
        assert_eq!(Broker::price(&b, "ETHUSDT"), 50.0);
        assert_eq!(Broker::bars(&b, "ETHUSDT").last().map(|x| x.close), Some(50.0));
        assert_eq!(Broker::bars(&b, "BTCUSDT").last().map(|x| x.close), Some(100.0));
    }

    /// **THE DECISION.** A symbol a DECLARED mount does not carry reads EMPTY — never the
    /// dispatching series' numbers, which is the defect, and never the account's, which this broker
    /// cannot see. `0.0` / `0.0` / `&[]` is the literal truth about a mount whose order path REFUSES
    /// that same symbol. `Broker::position`'s doc argues it against the alternatives.
    #[test]
    fn an_uncarried_symbol_reads_empty_on_a_declared_mount() {
        let b = declared_ctx();
        // The fixture's scalars are the dispatching series' — 5.0 and 100.0. Neither may leak out.
        assert_eq!(Broker::position(&b, "SOMETHING_ELSE"), 0.0);
        assert_eq!(Broker::price(&b, "SOMETHING_ELSE"), 0.0);
        assert!(Broker::bars(&b, "SOMETHING_ELSE").is_empty());
    }

    /// The other half of the partition, and the one every mount `vike-run` builds today
    /// (`MountSpec::legs` is empty on all of them): an UNDECLARED mount keeps the permissive
    /// single-symbol licence — the argument is ignored, every read answers about the mount, and a
    /// name it never heard of is NOT an uncarried symbol.
    #[test]
    fn an_undeclared_mount_still_ignores_the_symbol_argument() {
        let mut b = ctx();
        b.bars = Arc::new(vec![bar(100.0)]);
        for sym in ["", "BTCUSDT", "SOMETHING_ELSE"] {
            assert_eq!(Broker::position(&b, sym), 5.0, "position({sym:?})");
            assert_eq!(Broker::price(&b, sym), 100.0, "price({sym:?})");
            assert_eq!(Broker::bars(&b, sym).len(), 1, "bars({sym:?})");
        }
    }

    /// `""` means "my mount" on a DECLARED mount too — the [`LiveBroker::named`] convention, which
    /// live bars depend on (`Bar::symbol` is `None`, so `unwrap_or_default()` passes `""`). Reading
    /// it as an uncarried symbol would answer flat to every strategy written against a live bar.
    #[test]
    fn the_empty_symbol_is_the_mount_even_when_declared() {
        let b = declared_ctx();
        assert_eq!(Broker::position(&b, ""), 5.0);
        assert_eq!(Broker::price(&b, ""), 100.0);
        assert!(Broker::bars(&b, "").is_empty(), "the fixture's dispatching series is empty");
    }

    /// A buffered submission that carries a TAG never also carries a SYMBOL.
    ///
    /// ⚠ This test used to be the producer-side pin for a `debug_assert!` in
    /// `runtime::strategy_drive`'s `drain_broker`, on the theory that the tag→coid registry keyed on
    /// the MOUNT's symbol while the ORDER routed through `resolve_intent_symbol` — so a
    /// symbol-carrying tagged submit would insert under one key and be looked up under another.
    /// **That reasoning was wrong in its premise**: the registry keyed on the DRAIN's symbol, not
    /// the mount's, so the two disagreed whenever the dispatching series was not the mount's own,
    /// with no symbol-carrying submit needed. `CoreThread::tag_key` now keys on the mount's own
    /// series for the insert AND both lookups, which makes the key independent of a submission's
    /// `symbol` altogether — so the assert was removed rather than left standing over a hazard it
    /// no longer describes.
    ///
    /// What this still pins is the `HftBroker` CONTRACT the tag lane rests on: a tag names a quote
    /// and never an instrument, so `cancel_tagged(tag)`/`modify_tagged(tag)` are mount-scoped by
    /// construction. A tagged verb that started naming a symbol would be a new surface needing its
    /// own routing story (and would make one tag able to mean two orders on one mount, which the
    /// symbol-less verbs cannot express) — this sweep is what forces that conversation.
    ///
    /// EXHAUSTIVE over a CLOSED producer set: every `BufferedSubmit` in the workspace is pushed
    /// by one of the sites in THIS file, and all of them are driven below. A new
    /// submission-producing verb must be added to this sweep.
    #[test]
    fn tagged_orders_never_carry_a_symbol() {
        let mut b = ctx();

        // The inherent sizing verbs — `order_target_value`/`order_target_percent` funnel into
        // `order_target`, and `set_holdings` has TWO push arms (no lot grid → the oracle
        // `order_target_percent` path; a lot grid → its own `BufferedSubmit`). Drive both.
        b.order_target(50.0);
        b.order_target_value(1_000.0);
        b.order_target_percent(0.25);
        b.set_holdings(0.5);
        b.lot_size = 1.0;
        b.set_holdings(0.9);

        // The reduce-only market verb.
        b.submit_market_reduce(-1, 1.0);

        // The two TAGGED verbs — the only sites in the workspace that set `tag: Some`, both
        // through the inherent method and through the delegating `HftBroker` impl the makers use.
        b.submit_limit_tagged("bid", 1, 1.0, 99.0);
        b.submit_market_tagged("flat", -1, 1.0);
        HftBroker::submit_limit_tagged(&mut b, "ask", -1, 1.0, 101.0);

        // The portable `Broker` verbs, NAMING a symbol — so this sweep genuinely observes a
        // symbol-carrying row and the assertion below cannot pass vacuously.
        Broker::submit_market(&mut b, "ETHUSDT", 1, 1.0);
        Broker::submit_limit(&mut b, "ETHUSDT", -1, 1.0, 101.0);

        assert!(
            b.submissions.iter().any(|s| s.tag.is_some()),
            "sweep observed no TAGGED submission — the invariant below would hold vacuously"
        );
        assert!(
            b.submissions.iter().any(|s| s.symbol.is_some()),
            "sweep observed no symbol-carrying submission — `symbol` is evidently never set, so \
             the invariant below would hold vacuously"
        );

        for s in &b.submissions {
            assert!(
                s.tag.is_none() || s.symbol.is_none(),
                "a {} submission tagged {:?} also named symbol {:?}: the tag registry keys on the \
                 MOUNT's symbol, so this row would insert under one key and be looked up under \
                 another",
                s.order_type,
                s.tag,
                s.symbol,
            );
        }
    }
}
