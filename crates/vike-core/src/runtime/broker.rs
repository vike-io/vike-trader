//! `LiveBroker` — the live `OrderBroker` the strategy mount drives — split out of the runtime fold module (behavior byte-identical; the
//! block moved verbatim). `use super::*` re-exports the parent runtime module's full
//! import set + items, so nothing about resolution changes.

use super::*;

/// One row of a declared mount's leg table ([`LiveBroker::leg_values`]): a symbol, the value one unit
/// of it is worth as its leg's account books it, the grid its engine's gate holds for it (`None`
/// without a row of its own), and the delay label its `(venue, symbol)` stream last reported
/// (decision 0140; `None` = none stated). Named for clippy's `type_complexity`.
pub(crate) type LegFacts =
    (String, vike_model::ContractValue, Option<vike_model::Grid>, Option<vike_model::StreamDelay>);

/// The live strategy seam (R7): the unified [`Strategy`] trait's `on_bar` fires ONCE per
/// CLOSED bar of the mounted series, AFTER the client's paper fills for that bar
/// (next-open discipline: orders submitted on bar i fill during bar i+1) and after the
/// bar joined the cache. Orders go through the ONE live path — OrderClientIdGenerator → RiskGate →
/// client (paper or venue). [`LiveBroker`] is the live [`OrderBroker`]: verbs BUFFER submissions
/// (drained after the handler returns) — the deferred twin of the sim broker's direct
/// mutation. Handlers are skipped until `index >= warmup()` (the R2 gate).
///
/// Strategy-facing view + order intake for one `on_bar` call.
pub struct LiveBroker {
    /// signed position in the mounted symbol (the engine's BOTH leg)
    pub position: f64,
    /// that position's average entry price (`0.0` when flat), read with `position` from ONE lookup
    /// (`Engine::position_and_average_of`; on the fill lane `AppliedFill::average_after`, the state
    /// after THIS fill): what `vike_model::OrderBroker::average_price` answers.
    pub average_price: f64,
    /// the bar's close (the engine's post-fill `price` mark)
    pub price: f64,
    /// resolver-priced account equity at the pre-strategy state, **CAPPED** by
    /// `vike_config::Policy::max_sizing_equity` where one is armed — this is a SPENDING lane, so it
    /// reads `Engine::sizing_equity` (under `CoreConfig::price_config`) and not
    /// `resolved_equity`, which is what the snapshot/sampler display keep. ⚠ On the `on_fill` lane
    /// it arrives as `AppliedFill::equity_after`, capped at the fold through the shared
    /// `Engine::cap_sizing_equity` so the two spellings cannot disagree; this doc named
    /// `resolved_equity` until the ceiling landed and was stale for both.
    pub equity: f64,
    /// every closed bar of the mounted series up to and INCLUDING this one (Arc clone)
    pub bars: Arc<Vec<Bar>>,
    /// bars.len() - 1
    pub index: usize,
    /// this bar's ts (epoch ms)
    pub now: i64,
    /// What one unit of the mounted symbol's quantity is worth IN ITS QUOTE CURRENCY
    /// (`Account::base_value_of`; `Linear` 1.0 default, decision 0170): the kind and per-unit factor the
    /// sizing verbs read, through `vike_model::ContractValue::unit_value`, with [`Self::quote_rate`].
    pub contract_value: vike_model::ContractValue,
    /// Decision 0183's factor for the mounted symbol (`Account::quote_rate_of`): account-currency units per
    /// ONE unit of its quote currency, `1.0` when it is quoted in the account's own currency, `NaN` when it
    /// is foreign-quoted and the engine holds no rate yet (the sizing verbs then size nothing).
    pub quote_rate: f64,
    /// the gate's lot grid (RiskLimits::lot_size; 0.0 = no grid) — set_holdings lands on it
    pub lot_size: f64,
    /// the gate's price grid and venue floors (RiskLimits::tick_size / min_qty / min_notional; 0.0 =
    /// none) — `OrderBroker::instrument_grid` answers them with `lot_size`
    pub tick_size: f64,
    pub min_qty: f64,
    pub min_notional: f64,
    // `pub` (was `pub(crate)`) so the maker files' white-box tests can construct a `LiveBroker` by
    // struct literal and assert on the buffered verbs — a TEST-only surface (the check crate that
    // compiles them dev-deps vike-core). The live drain still owns/empties these internally.
    pub submissions: Vec<BufferedSubmit>,
    pub modifications: Vec<BufferedModify>,
    /// per-tag cancels (pull ONE quote) — resolved tag→order client id at drain time, like modifications
    pub cancels: Vec<String>,
    /// Per-symbol position/mark for every instrument the mount declared
    /// ([`crate::Mount::symbols`]) plus its OWN symbol, MINUS the one series that
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
    /// Per-symbol CLOSED-BAR history — the [`OrderBroker::bars`] twin of `positions`/`prices`, built by
    /// the same `CoreThread::declared_views` pass, off the same per-leg venue rule, so a leg's
    /// history, its position and its orders all name ONE book.
    ///
    /// ⚠ Unlike those two it carries EVERY declared instrument, the DISPATCHING one INCLUDED. There
    /// is no per-fill freshness to protect for a bar series (the `bars` scalar is the MOUNT's own
    /// history on every lane but bar/tick, not a per-dispatch number), and that completeness is what
    /// makes this table the membership oracle all three per-symbol reads consult — see
    /// [`LiveBroker::carries`].
    ///
    /// EMPTY for every single-symbol mount (one whose strategy declares no `vike_model::Strategy::symbols`,
    /// so its `MountSpec::legs` is empty), which is what keeps the ordinary dispatch byte-identical:
    /// empty ⇒ every read falls through to the scalars above exactly as it did before this table
    /// existed. Its FIRST row is the mount's own symbol, which is what `""` reads on a declared mount.
    ///
    /// `Arc` clones (a refcount bump), never a borrow of engine state — the same rule the rest of
    /// the ctx follows.
    pub bar_views: Vec<(String, Arc<Vec<Bar>>)>,
    /// What one unit of each DECLARED symbol (the mount's own and every leg) is worth, in its quote
    /// currency, as the leg's own ACCOUNT books it (`Account::base_value_of`): the table
    /// [`OrderBroker::contract_value`] answers a named symbol from on a declared mount, beside the grid the
    /// leg's engine's gate holds for it, which [`OrderBroker::instrument_grid`] answers a LEG from (`None`
    /// when the gate holds no row of its own for the symbol), and the leg's stream label, which
    /// [`OrderBroker::data_delay`] answers a LEG from. Resolved cold, when the mount's symbols, an
    /// account's values or grids, or a stream's label change (`CoreThread::rebuild_leg_values`), and
    /// lent here by `Arc::clone`, so a dispatch looks nothing up. `None` for every single-symbol mount.
    pub leg_values: Option<Arc<[LegFacts]>>,
    pub brackets: Vec<BufferedBracket>,
    /// [`Self::submit_group`], [`Self::submit_oco`], [`Self::submit_ladder`]
    pub order_lists: Vec<BufferedOrderList>,
    /// [`Self::submit_combo`]
    pub combos: Vec<BufferedCombo>,
    pub order_emulation_arms: Vec<BufferedConditional>,
    pub mass_cancel: bool,
    /// The delay label of the mount's OWN `(venue, symbol)` stream (decision 0140), COPIED from
    /// `CoreThread::mount_delays` at dispatch — one `Copy` field, no lookup. `None` = no label
    /// stated. [`OrderBroker::data_delay`] answers it; information, never a gate.
    pub data_delay: Option<vike_model::StreamDelay>,
    /// The mount's INPUTS as of this dispatch's decision (decision 0271, `CoreThread::mount_inputs` lent by
    /// `Arc::clone`): what [`OrderBroker::input`] answers. `None` for a mount that reads no input.
    pub inputs: Option<InputsAt>,
    /// The mount's CLOSED-TRADE ledger as of this dispatch (decision 0271 item 8, `CoreThread::mount_closed`
    /// lent by `Arc::clone`): what [`OrderBroker::closed_trades`] answers. `None` is an empty ledger (the mount's
    /// first fill has not come), never "keeps none": a live mount always keeps one.
    pub closed_trades: Option<Arc<vike_analytics::closed_trades::TradeClosedLedger>>,
}

impl LiveBroker {
    /// The symbol an `OrderBroker` verb named, or `None` when it named nothing usable.
    ///
    /// EMPTY IS `None` ON PURPOSE: live bars carry `Bar::symbol == None`, so a strategy doing
    /// `bar.symbol.clone().unwrap_or_default()` passes `""` and means "my mount" — see
    /// `r7_gate.rs` / `crates/vike-core/src/runtime/tests/safe_state/mount_budget.rs`'s
    /// `BudgetSubmit` / vike-rhai's live bar lane, which all rely on it.
    /// Treating `""` as a symbol would send it to a venue.
    fn named(symbol: &str) -> Option<String> {
        (!symbol.is_empty()).then(|| symbol.to_string())
    }

    /// The symbol a READ names: `""` is the mount's OWN symbol (decision 0277's rule 1). On a
    /// DECLARED mount that is [`Self::bar_views`]' first row (`CoreThread::declared_views` writes the
    /// mount's own first), whichever series dispatched, since there the scalars describe the
    /// DISPATCHING series, which may be a leg's. On every other mount the table is empty and `""`
    /// stays `""`: the scalars, which are the mount's own there.
    fn read_name<'a>(&'a self, symbol: &'a str) -> &'a str {
        match self.bar_views.first() {
            Some((own, _)) if symbol.is_empty() => own.as_str(),
            _ => symbol,
        }
    }

    /// Whether a read naming `symbol` asks about the mount's OWN stream (its delay label and the
    /// gate's grid, which the ctx carries for the mount's own symbol alone): `""`, any symbol on an
    /// undeclared mount, or the own symbol's name on a declared one.
    fn names_own(&self, symbol: &str) -> bool {
        symbol.is_empty() || self.bar_views.first().is_none_or(|(own, _)| own == symbol)
    }

    /// A per-symbol read, or `None` for "this table cannot answer" — which [`OrderBroker::position`] /
    /// [`OrderBroker::price`] then resolve through [`LiveBroker::carries`], NOT by falling back blindly.
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
    /// - an EMPTY table ⇒ a single-symbol mount, where [`OrderBroker::bars`]' own contract licenses
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
                order_type: vike_model::OrderType::Market,
                price: None,
                trigger: None,
                trail: None,
                limit_offset: None,
                reduce_only: false,
                tag: None,
                time_in_force: vike_model::TimeInForce::Gtc,
                gtd_expiry: None,
                release: None,
                ..Default::default()
            });
        }
    }

    /// A sizing answer of the one law (`vike_model::ContractValue::unit_value`, decision 0170 item 9 as
    /// amended 2026-11-08), passed through; `None` (the symbol is quoted in a currency the engine holds
    /// no rate for) is said ONCE per process at `error!`, and the verb then sizes nothing, never at par.
    fn said<T>(answer: Option<T>, verb: &str) -> Option<T> {
        if answer.is_none() {
            Self::refuse_unrated(verb);
        }
        answer
    }

    #[cold]
    fn refuse_unrated(verb: &str) {
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::error!(
                target: "vike_core::broker",
                verb,
                "REFUSED: the mounted symbol is quoted in a currency this engine holds no rate for \
                 yet, so a value or a percent of equity has no size; size it in units with \
                 order_target (said once per process)"
            );
        }
    }

    /// Rebalance to a target VALUE — port of `order_target_value`. `value` is in the ACCOUNT's money,
    /// the money [`OrderBroker::equity`] reports (a coin ledger's equity too is its coin valued in that money,
    /// decision 0258), and the size is `value` over what one unit is worth there
    /// (`vike_model::ContractValue::units_worth`): a linear contract's `price · per_unit`, an inverse
    /// one's face (1,000 on Deribit's `BTC-PERPETUAL` is 1,000 contracts at any price), each times
    /// decision 0183's [`Self::quote_rate`]. No-op without a usable price (oracle guard `px <= 0.0`)
    /// or without the rate.
    pub fn order_target_value(&mut self, value: f64) {
        if self.price <= 0.0 {
            return;
        }
        let units = self.contract_value.units_worth(value, self.price, self.quote_rate);
        if let Some(units) = Self::said(units, "order_target_value") {
            self.order_target(units);
        }
    }

    /// Rebalance to a fraction of current equity — port of `order_target_percent`: the value
    /// `pct · equity` sized as [`Self::order_target_value`] sizes it.
    pub fn order_target_percent(&mut self, pct: f64) {
        if self.price <= 0.0 {
            return;
        }
        let units = self.contract_value.units_worth(pct * self.equity, self.price, self.quote_rate);
        if let Some(units) = Self::said(units, "order_target_percent") {
            self.order_target(units);
        }
    }

    /// LEAN `SetHoldings` twin — the margin-aware upgrade over [`Self::order_target_percent`]:
    /// sizes with the Phase B `amount_to_order` walk so the position lands AT or UNDER the
    /// target on the lot grid, making repeated `set_holdings(x)` a no-op once filled (the
    /// LEAN idempotence law). Without a configured lot grid it falls back to the oracle
    /// `order_target_percent` path. The walk's unit is one unit's worth by the same law
    /// (`vike_model::ContractValue::unit_value`).
    pub fn set_holdings(&mut self, pct: f64) {
        if self.price <= 0.0 {
            return;
        }
        if self.lot_size > 0.0 {
            let unit = self.contract_value.unit_value(self.price, self.quote_rate);
            let Some(unit) = Self::said(unit, "set_holdings") else { return };
            let target_notional = pct * self.equity;
            let delta = amount_to_order(self.position, target_notional, unit, self.lot_size);
            if delta.abs() > 1e-12 {
                self.submissions.push(BufferedSubmit {
                    symbol: None,
                    side: if delta > 0.0 { 1 } else { -1 },
                    qty: delta.abs(),
                    order_type: vike_model::OrderType::Market,
                    price: None,
                    trigger: None,
                    trail: None,
                    limit_offset: None,
                    reduce_only: false,
                    tag: None,
                    time_in_force: vike_model::TimeInForce::Gtc,
                    gtd_expiry: None,
                    release: None,
                    ..Default::default()
                });
            }
        } else {
            self.order_target_percent(pct);
        }
    }

    /// Arm a protective STOP in the core-owned ConditionalArms (port of
    /// `LiveEngine.submit_stop` → `ConditionalArms.add_stop`). Checked per closed bar
    /// BEFORE `on_bar` (oracle firing order); fires as a market through the gate, flagged
    /// `reduce_only` for what the position covers (`submit_fired`), plain for what can open. Only ONE
    /// stop closes a position: what the closes already in flight cover is opened plain instead, so of
    /// two stops of the position's size the first closes it and the second opens the opposite position.
    pub fn submit_stop(&mut self, side: i32, qty: f64, price: f64) {
        self.order_emulation_arms.push(BufferedConditional {
            symbol: None,
            side,
            qty,
            price: Some(price),
            trail: None,
            limit_offset: None,
        });
    }

    /// Arm a TRAILING stop (port of `LiveEngine.submit_trailing`): the extreme seeds from
    /// the current mark at drain time; refused (surfaced in recent-events) without a mark.
    pub fn submit_trailing(&mut self, side: i32, qty: f64, trail: f64) {
        self.order_emulation_arms.push(BufferedConditional {
            symbol: None,
            side,
            qty,
            price: None,
            trail: Some(trail),
            limit_offset: None,
        });
    }

    /// Arm a TRAILING-LIMIT stop in the core-owned book, as [`Self::submit_trailing`] arms its trailing
    /// stop (the extreme seeds from the mark at drain time; refused without one): it fires by the
    /// trailing law at its stop level P and releases a LIMIT at P − `limit_offset` for a sell, P +
    /// `limit_offset` for a buy, never a market. Plain, the stop twin's flagging at the fire included.
    /// `OrderBrokerSimulator::submit_trailing_limit` is the backtest twin.
    pub fn submit_trailing_limit(&mut self, side: i32, qty: f64, trail: f64, limit_offset: f64) {
        self.order_emulation_arms.push(BufferedConditional {
            symbol: None,
            side,
            qty,
            price: None,
            trail: Some(trail),
            limit_offset: Some(limit_offset),
        });
    }

    /// Reduce-only TRAILING stop (live-only; not on the portable [`OrderBroker`] surface), the trailing twin
    /// of [`Self::submit_stop_reduce`]: `trail` behind the best price since `activation` was reached
    /// (`None`: since now), and with a `limit_offset` a trailing-LIMIT whose fire is a limit that far past
    /// its stop level. An ORDER through the capability row: a row that takes the kind reduce-only rests it
    /// natively, one that refuses it has the CORE hold it (its fire capped to the position), and a refusal
    /// reaches the strategy as an `OrderRejected` in the capability preflight's words. A trail that is not
    /// a finite number above zero, an activation not above zero or a negative offset is refused by the
    /// drain (`vike_model::order_terms_refusal`).
    pub fn submit_trailing_reduce(
        &mut self,
        side: i32,
        qty: f64,
        trail: f64,
        activation: Option<f64>,
        limit_offset: Option<f64>,
    ) {
        self.submissions.push(BufferedSubmit {
            symbol: None,
            side,
            qty,
            order_type: if limit_offset.is_some() {
                vike_model::OrderType::TrailingLimit
            } else {
                vike_model::OrderType::Trailing
            },
            price: None,
            trigger: activation,
            trail: Some(trail),
            limit_offset,
            reduce_only: true,
            tag: None,
            time_in_force: vike_model::TimeInForce::Gtc,
            gtd_expiry: None,
            release: None,
            ..Default::default()
        });
    }

    /// Reduce-only market order (live-only concept; not on the portable [`OrderBroker`] surface).
    pub fn submit_market_reduce(&mut self, side: i32, qty: f64) {
        self.submissions.push(BufferedSubmit {
            symbol: None,
            side,
            qty,
            order_type: vike_model::OrderType::Market,
            price: None,
            trigger: None,
            trail: None,
            limit_offset: None,
            reduce_only: true,
            tag: None,
            time_in_force: vike_model::TimeInForce::Gtc,
            gtd_expiry: None,
            release: None,
            ..Default::default()
        });
    }

    /// Reduce-only STOP at `trigger` (live-only; not on the portable [`OrderBroker`] surface): it may only
    /// shrink the position it meets, so one that fires after the position was closed another way
    /// cannot OPEN a new one. Unlike [`LiveBroker::submit_stop`] (always the core's own book, a plain
    /// market at the cross) this goes through the order path as an operator's `--type stop
    /// --reduce-only` does: a venue whose capability row lists `stop` among its `reduce_only_kinds`
    /// rests a native reduce-only stop, one whose row refuses it (IG, polymarket) has the CORE hold it
    /// and fire a reduce-only market capped to the position, and a paper account's book holds it as
    /// the simulator does. A refusal reaches the strategy as an `OrderRejected` lifecycle event in the
    /// capability preflight's words; the core's own hold is said on the operator's ring only.
    /// `OrderBrokerSimulator::submit_stop_reduce` is the backtest twin.
    pub fn submit_stop_reduce(&mut self, side: i32, qty: f64, trigger: f64) {
        self.submissions.push(BufferedSubmit {
            symbol: None,
            side,
            qty,
            order_type: vike_model::OrderType::Stop,
            price: None,
            trigger: Some(trigger),
            trail: None,
            limit_offset: None,
            reduce_only: true,
            tag: None,
            time_in_force: vike_model::TimeInForce::Gtc,
            gtd_expiry: None,
            release: None,
            ..Default::default()
        });
    }

    /// A STOP-LIMIT: once `trigger` trades it rests as a limit at `limit` (live-only; not on the portable
    /// [`OrderBroker`] surface, decision 0237's shape). It is an ORDER through the capability row, as
    /// [`LiveBroker::submit_stop_reduce`] is: a venue whose row lists `stop_limit` rests it natively, owned by
    /// the mount; a row that does not refuses it by name (`TRIGGER_UNSUPPORTED: kind=stop_limit venue=…`, an
    /// `OrderRejected` the strategy hears), never a plain stop or a market; a stated `paper` engine sends it to
    /// the simulator. A trigger or limit that is not a finite number above zero is refused by the drain
    /// (`vike_model::order_terms_refusal`) before anything is minted. Plain, not reduce-only.
    /// `OrderBrokerSimulator::submit_stop_limit` is the backtest twin.
    pub fn submit_stop_limit(&mut self, side: i32, qty: f64, trigger: f64, limit: f64) {
        self.push_trigger_order(vike_model::OrderType::StopLimit, side, qty, trigger, Some(limit));
    }

    /// A TAKE-PROFIT (if-touched) at `trigger`: with a `limit` it rests as a limit there once touched
    /// (limit-if-touched), without one it is a market once touched (market-if-touched). Routed and refused
    /// exactly as [`LiveBroker::submit_stop_limit`]. Plain, not reduce-only. `OrderBrokerSimulator::submit_take_profit` is
    /// the backtest twin.
    pub fn submit_take_profit(&mut self, side: i32, qty: f64, trigger: f64, limit: Option<f64>) {
        self.push_trigger_order(vike_model::OrderType::TakeProfit, side, qty, trigger, limit);
    }

    /// The one buffered row of the two trigger-kind verbs above: untagged, on the mount's own symbol, `Gtc`.
    fn push_trigger_order(
        &mut self,
        order_type: vike_model::OrderType,
        side: i32,
        qty: f64,
        trigger: f64,
        limit: Option<f64>,
    ) {
        self.submissions.push(BufferedSubmit {
            symbol: None,
            side,
            qty,
            order_type,
            price: limit,
            trigger: Some(trigger),
            trail: None,
            limit_offset: None,
            reduce_only: false,
            tag: None,
            time_in_force: vike_model::TimeInForce::Gtc,
            gtd_expiry: None,
            release: None,
            ..Default::default()
        });
    }

    /// A LIMIT with order ATTRIBUTES (all-or-none, sweep-to-fill, a discretionary amount: decision 0267),
    /// on the mount's own symbol, `Gtc`. An ORDER through the capability row in decision 0237's shape: a
    /// venue whose row lists every attribute it carries rests it natively, owned by the mount; any other
    /// refuses it BY NAME (`ATTRIBUTE_UNSUPPORTED: venue=… attribute=…`, an `OrderRejected` the strategy
    /// hears), never a plain limit. Terms `vike_model::attributes_refusal` refuses (a discretion that is not
    /// a finite number above zero) are a `Denied` from the drain. Live-only; `OrderBrokerSimulator::submit_limit_attributed`
    /// is the backtest twin.
    pub fn submit_limit_attributed(
        &mut self,
        side: i32,
        qty: f64,
        price: f64,
        attributes: vike_model::OrderAttributes,
    ) {
        self.submissions.push(BufferedSubmit {
            side,
            qty,
            order_type: vike_model::OrderType::Limit,
            price: Some(price),
            attributes: Some(attributes),
            ..Default::default()
        });
    }

    /// A TRAILING TAKE-PROFIT (IB `TRAIL MIT`; with a `limit_offset`, `TRAIL LIT`): a sell tracks the lowest
    /// price since it rests and fires when the price rises `trail` off it, a buy mirrors it; the fire is a
    /// market, or a limit `limit_offset` past the trigger. Through the capability row as
    /// [`LiveBroker::submit_stop_limit`] is; `reduce_only`, a row that refuses the kind has the CORE hold it
    /// as the mount's own exit (its fire capped to the position, a limit fire only where the row takes a
    /// reduce-only limit). Not cover for a position (it closes a winner). Live-only;
    /// `OrderBrokerSimulator::submit_trailing_take_profit` is the backtest twin.
    pub fn submit_trailing_take_profit(
        &mut self,
        side: i32,
        qty: f64,
        trail: f64,
        limit_offset: Option<f64>,
        reduce_only: bool,
    ) {
        self.submissions.push(BufferedSubmit {
            side,
            qty,
            order_type: vike_model::OrderType::TrailingTakeProfit,
            trail: Some(trail),
            limit_offset,
            reduce_only,
            ..Default::default()
        });
    }

    /// A trailing order whose distance is `percent` of its extreme (above 0, below 100): `order_type` is one
    /// of the three trailing kinds, as `OrderBrokerSimulator::submit_trailing_percent` takes it (its backtest twin) — a
    /// `trailing` stop, a `trailing_limit` with its `limit_offset`, or a `trailing_take_profit` with an
    /// optional one — from `activation` once reached (`None`: from now; a trailing take-profit takes none).
    /// Any other kind, or terms the order-terms rule refuses, is a `Denied` from the drain. Through the
    /// capability row's `percent_trail_kinds` (a row that lists the kind there rests it natively; a price
    /// trail is [`LiveBroker::submit_trailing_reduce`]'s or [`LiveBroker::submit_trailing_take_profit`]'s);
    /// `reduce_only`, a row that refuses it has the CORE hold it. Live-only.
    pub fn submit_trailing_percent(
        &mut self,
        order_type: vike_model::OrderType,
        side: i32,
        qty: f64,
        percent: f64,
        activation: Option<f64>,
        limit_offset: Option<f64>,
        reduce_only: bool,
    ) {
        self.submissions.push(BufferedSubmit {
            side,
            qty,
            order_type,
            trigger: activation,
            limit_offset,
            reduce_only,
            trail_percent: Some(percent),
            ..Default::default()
        });
    }

    /// An ADJUSTABLE STOP at `trigger` (with a `limit`, a stop-limit): once the price reaches `adjust.at`
    /// it becomes `adjust.to` in place, once (another stop, a stop-limit, or a trailing stop: the auto
    /// trailing stop). Through the capability row (the attribute word `adjust`); `reduce_only`, a row that
    /// refuses it has the CORE hold it. Terms `vike_model::stop_adjust_refusal` refuses are a `Denied` from the
    /// drain. Live-only; `OrderBrokerSimulator::submit_stop_adjustable` is the backtest twin.
    pub fn submit_stop_adjustable(
        &mut self,
        side: i32,
        qty: f64,
        trigger: f64,
        limit: Option<f64>,
        adjust: vike_model::OrderStopAdjust,
        reduce_only: bool,
    ) {
        self.submissions.push(BufferedSubmit {
            side,
            qty,
            order_type: if limit.is_some() {
                vike_model::OrderType::StopLimit
            } else {
                vike_model::OrderType::Stop
            },
            price: limit,
            trigger: Some(trigger),
            reduce_only,
            adjust: Some(adjust),
            ..Default::default()
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
            order_type: vike_model::OrderType::Limit,
            price: Some(price),
            trigger: None,
            trail: None,
            limit_offset: None,
            reduce_only: false,
            tag: Some(tag.to_string()),
            time_in_force: vike_model::TimeInForce::Gtc,
            gtd_expiry: None,
            release: None,
            ..Default::default()
        });
    }

    /// Submit a TAGGED **pegged** order (`vike_model::OrderType::Pegged`): it rests at `peg`'s anchor moved by
    /// its offset. The venue holds it where its row says so (`vike_venues::emulation_of`); everywhere else
    /// the core EMULATES it (decision 0275): one plain limit at a time, re-priced on every move of the
    /// anchor, and the strategy hears ONE order under `tag` (its `Accepted`, every fill, one end). The
    /// simulator's twin is `OrderBrokerSimulator::submit_pegged_tagged`. An amend of it is refused in words.
    pub fn submit_pegged_tagged(
        &mut self,
        tag: &str,
        side: i32,
        qty: f64,
        peg: vike_model::OrderPeg,
    ) {
        self.submissions.push(BufferedSubmit {
            side,
            qty,
            order_type: vike_model::OrderType::Pegged,
            tag: Some(tag.to_string()),
            peg: Some(peg),
            ..Default::default()
        });
    }

    /// Submit a TAGGED limit with a VISIBLE size (`OrderRequest::display_quantity`, an iceberg): the venue shows
    /// `display` of `qty` where its row wires the field; everywhere else the core EMULATES it (decision
    /// 0275): one plain limit of `display` at a time at `price`, and the strategy hears ONE order under
    /// `tag`. The simulator's twin is `OrderBrokerSimulator::submit_limit_display_tagged`.
    pub fn submit_limit_display_tagged(
        &mut self,
        tag: &str,
        side: i32,
        qty: f64,
        price: f64,
        display: f64,
    ) {
        self.submissions.push(BufferedSubmit {
            side,
            qty,
            order_type: vike_model::OrderType::Limit,
            price: Some(price),
            tag: Some(tag.to_string()),
            display_quantity: Some(display),
            ..Default::default()
        });
    }

    /// `symbol` (as [`vike_model::OrderBroker::place`]'s) and `reduce_only` onto the submission a verb
    /// that states neither just buffered.
    fn carry_symbol_and_reduce_only(&mut self, symbol: &str, reduce_only: bool) {
        if let Some(s) = self.submissions.last_mut() {
            s.symbol = Self::named(symbol);
            s.reduce_only = reduce_only;
        }
    }

    /// A limit order with a LIFETIME other than the default `Gtc` (`Day`, `Ioc`, `Fok`, `PostOnly`; `Gtd`
    /// with its `gtd_expiry`, epoch ms). `symbol` as in [`vike_model::OrderBroker::place`] (`""` = this
    /// mount's own).
    ///
    /// The order goes through the core's preflight like any other: a lifetime the venue's capability row does
    /// not carry is refused BY NAME (`TIF_UNSUPPORTED: tif=… venue=…`, a terminal `OrderRejected` the strategy
    /// hears as `Rejected`), never sent as a plain GTC limit. Before that the drain refuses, as a `Denied`, a `Gtd`
    /// with no expiry or one not after the dispatch's `now` and an expiry on any other lifetime
    /// (`vike_model::lifetime_refusal`); how near a future expiry may be is the venue's bridge's question
    /// (binance's perp lane refuses one under ten minutes). Live-only, like the tagged verbs: the simulator's
    /// twin is `OrderBrokerSimulator::submit_limit_post_only` (the one lifetime the backtest models).
    pub fn submit_limit_time_in_force(
        &mut self,
        symbol: &str,
        side: i32,
        qty: f64,
        price: f64,
        tif: vike_model::TimeInForce,
        gtd_expiry: Option<i64>,
    ) {
        self.submissions.push(BufferedSubmit {
            symbol: Self::named(symbol),
            side,
            qty,
            order_type: vike_model::OrderType::Limit,
            price: Some(price),
            trigger: None,
            trail: None,
            limit_offset: None,
            reduce_only: false,
            tag: None,
            time_in_force: tif,
            gtd_expiry,
            release: None,
            ..Default::default()
        });
    }

    /// A MARKET order the core HOLDS until `release` holds, then sends (a time release, and with a
    /// `vike_model::OrderReleasePrice` a conditional order). `symbol` as in [`vike_model::OrderBroker::place`].
    /// Held by the core and not the venue (`held_release.rs` in this crate): the capability row judges it
    /// now, the pre-trade gate at the release; it survives a restart; the strategy hears nothing until the
    /// venue answers the released order. A release not after the dispatch's `now` is refused by the drain
    /// (`vike_model::lifetime_refusal`) as a `Denied`. Live-only.
    pub fn submit_market_at(
        &mut self,
        symbol: &str,
        side: i32,
        qty: f64,
        release: vike_model::OrderRelease,
    ) {
        self.submissions.push(BufferedSubmit {
            symbol: Self::named(symbol),
            side,
            qty,
            order_type: vike_model::OrderType::Market,
            price: None,
            trigger: None,
            trail: None,
            limit_offset: None,
            reduce_only: false,
            tag: None,
            time_in_force: vike_model::TimeInForce::Gtc,
            gtd_expiry: None,
            release: Some(release),
            ..Default::default()
        });
    }

    /// A LIMIT order held until `release` holds, as [`LiveBroker::submit_market_at`], with the lifetime
    /// [`LiveBroker::submit_limit_time_in_force`] takes: a `Day` order's day starts at its release, a `Gtd` must
    /// expire after it, and one whose expiry passes while it is held expires instead of being sent.
    pub fn submit_limit_at(
        &mut self,
        symbol: &str,
        side: i32,
        qty: f64,
        price: f64,
        tif: vike_model::TimeInForce,
        gtd_expiry: Option<i64>,
        release: vike_model::OrderRelease,
    ) {
        self.submit_limit_time_in_force(symbol, side, qty, price, tif, gtd_expiry);
        if let Some(s) = self.submissions.last_mut() {
            s.release = Some(release);
        }
    }

    /// A MAKER-ONLY limit order: it rests, or the venue REFUSES it (a terminal `OrderRejected` whose cause
    /// is `RejectCause::PostOnlyWouldCross`, or an `OrderCanceled` with
    /// `vike_model::events::POST_ONLY_CANCEL_REASON` where the venue accepts then cancels) — it is never
    /// executed as a taker. A venue with no post-only order refuses it by name. See
    /// [`LiveBroker::submit_limit_time_in_force`].
    pub fn submit_limit_post_only(&mut self, symbol: &str, side: i32, qty: f64, price: f64) {
        self.submit_limit_time_in_force(
            symbol,
            side,
            qty,
            price,
            vike_model::TimeInForce::PostOnly,
            None,
        );
    }

    /// Submit a TAGGED market order (HFT modify surface). See [`LiveBroker::submit_limit_tagged`].
    pub fn submit_market_tagged(&mut self, tag: &str, side: i32, qty: f64) {
        self.submissions.push(BufferedSubmit {
            symbol: None,
            side,
            qty,
            order_type: vike_model::OrderType::Market,
            price: None,
            trigger: None,
            trail: None,
            limit_offset: None,
            reduce_only: false,
            tag: Some(tag.to_string()),
            time_in_force: vike_model::TimeInForce::Gtc,
            gtd_expiry: None,
            release: None,
            ..Default::default()
        });
    }

    /// Modify a previously-tagged resting order by tag (RUST-NATIVE; no Python twin). No-op if the
    /// tag is unknown or its order is terminal (resolved at drain time against the runtime's
    /// tag→order client id registry, then gated by [`Engine::modify_order`]).
    pub fn modify(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modifications.push(BufferedModify {
            tag: tag.to_string(),
            new_qty,
            new_price,
            new_trigger_price: None,
        });
    }

    /// [`LiveBroker::modify`] that also moves a trigger order's TRIGGER to `new_trigger_price`
    /// (`OrderIntent::Modify::new_trigger_price`; where each price lands is `vike_model::modified_prices`).
    /// A held order is edited in place; a resting stop takes it natively, a stop-limit's or a
    /// take-profit's trigger moves by cancel-then-new where its venue amends that way and is refused in
    /// words elsewhere (`OrderModifyRejected`, the order keeps its terms).
    pub fn modify_trigger(
        &mut self,
        tag: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) {
        self.modifications.push(BufferedModify {
            tag: tag.to_string(),
            new_qty,
            new_price,
            new_trigger_price: Some(new_trigger_price),
        });
    }

    /// Cancel ONE previously-tagged resting order by tag (RUST-NATIVE; no Python twin) — the
    /// per-side twin of [`LiveBroker::mass_cancel`]. Used to PULL a single quote (e.g. a market
    /// maker suppressing one side under adverse selection) without disturbing the other side. No-op
    /// if the tag is unknown or its order is already terminal (resolved at drain time against the
    /// runtime's tag→order client id registry, then gated by [`Engine::cancel_order`]).
    pub fn cancel_tagged(&mut self, tag: &str) {
        self.cancels.push(tag.to_string());
    }

    /// Cancel ALL of this engine's live orders (pull-all-quotes) after the handler returns (HFT;
    /// no Python twin). Drained AFTER this call's submits/modifications.
    pub fn mass_cancel(&mut self) {
        self.mass_cancel = true;
    }

    /// Submit a BRACKET (entry + protective stop-loss + take-profit) — RUST-NATIVE, no Python twin.
    /// `entry_price` None = market entry. The runtime mints the three order client ids at drain time and wires
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

    /// Submit an ORDER LIST on the mount's own symbol: `legs` linked by index as
    /// `vike_model::OrderListLeg` states (OTO through `parent`, OCO or OUO through `contingency` and
    /// `linked`) — RUST-NATIVE, no Python twin. The drain mints one order client id per leg and lowers the list
    /// as ONE `OrderIntent::OrderList` (decision 0254: every leg on the mount's one account); a leg's
    /// terms or lifetime the drain's rules refuse (`vike_model::order_terms_refusal`,
    /// `vike_model::lifetime_refusal`), or links `vike_model::build_order_list` cannot resolve, refuse
    /// the WHOLE list as a `Denied` the mount hears, before anything is minted.
    /// `OrderBrokerSimulator::submit_group` is the backtest twin.
    pub fn submit_group(&mut self, legs: Vec<vike_model::OrderListLeg>) {
        self.order_lists.push(BufferedOrderList { legs: Ok(legs), tags: Vec::new(), symbol: None });
    }

    /// Submit `a` and `b` as ONE-CANCELS-OTHER: the first fill of either cancels the other. Their own
    /// links are replaced (no parent, OCO, each linked to the other); the rest is
    /// [`Self::submit_group`]. `OrderBrokerSimulator::submit_oco` is the backtest twin.
    pub fn submit_oco(&mut self, a: vike_model::OrderListLeg, b: vike_model::OrderListLeg) {
        let link = |mut leg: vike_model::OrderListLeg, other: usize| {
            leg.parent = None;
            leg.contingency = Some(vike_model::ContingencyType::Oco);
            leg.linked = vec![other];
            leg
        };
        self.submit_group(vec![link(a, 1), link(b, 0)]);
    }

    /// Submit a BRACKET LADDER: [`Self::submit_bracket`]'s entry and protective stop for the whole
    /// `qty`, and one reduce-only limit take-profit per target, the exits linked OUO
    /// (`vike_model::build_ladder`, whose refusal in words the mount hears as a `Denied`). The rest is
    /// [`Self::submit_group`]. `OrderBrokerSimulator::submit_ladder` is the backtest twin.
    pub fn submit_ladder(
        &mut self,
        side: i32,
        qty: f64,
        entry_price: Option<f64>,
        stop_loss: f64,
        targets: Vec<vike_model::LadderTarget>,
    ) {
        // the drain stamps the venue and symbol; `build_ladder` only carries them
        let spec = vike_model::LadderSpec {
            venue: String::new(),
            symbol: String::new(),
            side,
            qty,
            entry_price,
            stop_loss,
            targets,
            account: None,
        };
        let legs = vike_model::build_ladder(&spec).map(|list| list.legs);
        self.order_lists.push(BufferedOrderList { legs, tags: Vec::new(), symbol: None });
    }

    /// Submit a COMBO on the mount's own account: `legs` (two or more, each an instrument and its SIGNED ratio,
    /// with its own limit when the combo is priced by its legs) traded as ONE order, `side` +1 buying it and -1
    /// selling it, `qty` combo units, `net_limit` the signed net per unit or `None` (a market combo, or one priced
    /// by its legs) — RUST-NATIVE, no Python twin. The drain stamps the mount's venue, routes the combo by the mount
    /// (its one account, decision 0254, the bracket's way) and lowers it as ONE `OrderIntent::Combo`, whose one order client id
    /// the runtime mints; a combo `vike_model::OrderCombo::validate` refuses is refused WHOLE as a `Denied` the mount
    /// hears, before anything is minted. Whether its venue takes it is the core's verdict (`lower_combo`).
    /// `OrderBrokerSimulator::submit_combo` is the backtest twin.
    pub fn submit_combo(
        &mut self,
        side: i32,
        qty: f64,
        legs: Vec<vike_model::OrderComboLeg>,
        net_limit: Option<f64>,
    ) {
        self.combos.push(BufferedCombo { side, qty, legs, net_limit });
    }
}

/// The live ORDER seam (`vike_model::OrderBroker`, decision 0269): every verb BUFFERS into the drain
/// queues its inherent twins fill. A tagged order is a submission in the ORDER lane carrying its tag
/// (as [`LiveBroker::submit_limit_tagged`]'s), `Gtc`, so it meets the drain's order-terms and lifetime
/// rules and the venue's capability row as the twin of its kind does (a stop in the order lane, as
/// [`LiveBroker::submit_stop_reduce`]'s, never the core's own conditional book), and the drain files the
/// tag against the order client id it mints; a group is [`LiveBroker::submit_group`]'s with each leg's tag; amend and
/// cancel are [`LiveBroker::modify`] / [`LiveBroker::modify_trigger`] and [`LiveBroker::cancel_tagged`],
/// resolved by tag at the drain. A paper engine is this path too.
///
/// The symbol a verb names rides the submission ([`LiveBroker::named`]: `""` is `None` and allocates
/// nothing), so the drain's one rule resolves it (`resolve_intent_symbol`: a mount that declared no
/// symbols trades its own and REFUSES an order naming another; a declared one the named symbol, `""` its
/// own, refusing one it did not declare). The tag registry stays keyed on the mount's own series
/// (`CoreThread::tag_key`), so one tag names one order of the mount. An order list carries the symbol
/// `place_group` names and the drain resolves it the same way (one broker plan, step 12; decision 0277's
/// item 6 left it on the dispatching series).
///
/// ⚠ The READS do not ignore the `symbol` argument either — the claim "the symbol argument is ignored"
/// stood on this impl through #916/#924/#997 and was the exact claim the two-leg routing bug hid behind:
///
/// - `position`/`price` answer per symbol out of the tables `CoreThread::declared_views` builds
///   (again empty, hence scalar, for an undeclared mount) — on EVERY strategy-hook lane, not just
///   the bar/tick/reference-quote ones, and resolved against the leg's OWN venue's engine.
/// - `bars` answers per symbol too, out of [`LiveBroker::bar_views`], off that same pass and that
///   same per-leg venue rule. It was the last read that discarded the argument outright.
/// - a symbol a DECLARED mount does not carry gets the EMPTY answer — `0.0` / `0.0` / `&[]` — and
///   never the dispatching series' numbers. See [`LiveBroker::carries`] for the partition and the
///   header of `OrderBroker::position` below for why empty is the right shape.
impl OrderBroker for LiveBroker {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: vike_model::OrderPlacement,
        reduce_only: bool,
    ) {
        use vike_model::OrderPlacement;
        match order {
            // worked by their tag, so an untagged one is refused in words (the drain denies it)
            OrderPlacement::Pegged { .. } | OrderPlacement::Iceberg { .. } if tag.is_empty() => {
                let why = "an untagged pegged order or iceberg has no lane: give it a tag";
                return self.submissions.push(BufferedSubmit {
                    symbol: Self::named(symbol),
                    qty,
                    refused: Some(why.to_string()),
                    ..Default::default()
                });
            }
            // the two working kinds take their verbs' path (native, or emulated by the core: decision
            // 0275), which states no reduce-only flag nor symbol, so both are carried onto what they
            // buffered
            OrderPlacement::Pegged { peg } => {
                self.submit_pegged_tagged(tag, side, qty, peg);
                return self.carry_symbol_and_reduce_only(symbol, reduce_only);
            }
            OrderPlacement::Iceberg { price, display } => {
                self.submit_limit_display_tagged(tag, side, qty, price, display);
                return self.carry_symbol_and_reduce_only(symbol, reduce_only);
            }
            _ => {}
        }
        // its kind, its limit (`price`), its trigger (a trailing kind's activation) and its trail;
        // untagged (`""`), the row the deleted `Broker::submit_market` / `submit_limit` and the
        // trigger-kind verbs buffer, field for field (decision 0277's rule 2)
        let (order_type, price, trigger, trail) = order.request_terms();
        self.submissions.push(BufferedSubmit {
            symbol: Self::named(symbol),
            side,
            qty,
            order_type,
            price,
            trigger,
            trail,
            reduce_only,
            tag: (!tag.is_empty()).then(|| tag.to_string()),
            ..Default::default()
        });
    }

    fn place_group(&mut self, symbol: &str, legs: Vec<(String, vike_model::OrderListLeg)>) {
        let (tags, legs) = legs.into_iter().unzip();
        let symbol = Self::named(symbol);
        self.order_lists.push(BufferedOrderList { legs: Ok(legs), tags, symbol });
    }

    fn amend(
        &mut self,
        tag: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    ) {
        match new_trigger {
            Some(trigger) => self.modify_trigger(tag, new_qty, new_price, trigger),
            None => self.modify(tag, new_qty, new_price),
        }
    }

    fn cancel(&mut self, tag: &str) {
        LiveBroker::cancel_tagged(self, tag);
    }

    /// Buffered in the order lane like any submission, so it keeps its place among them; the drain
    /// denies it in `reason`'s words (`BufferedSubmit::refused`, `deny_intent`).
    fn deny(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str) {
        self.submissions.push(BufferedSubmit {
            symbol: Self::named(symbol),
            qty,
            tag: Some(tag.to_string()),
            refused: Some(reason.to_string()),
            ..Default::default()
        });
    }

    /// With [`OrderBroker::position`]'s partition: the dispatching series' average (any symbol on a mount
    /// that declared none), `0.0` flat or for a symbol a declared mount does not carry, and NaN for a
    /// declared symbol with a row in the table (the mount's own, `""`, included), whose average the
    /// core does not carry to a dispatch; the fill lane's own symbol answers the fill's average.
    fn average_price(&self, symbol: &str) -> f64 {
        let symbol = self.read_name(symbol);
        match self.declared_read(&self.positions, symbol) {
            Some(_) => f64::NAN,
            None if self.carries(symbol) && self.position != 0.0 => self.average_price,
            None => 0.0,
        }
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
    ///   whole defect — the file `strategies/pairs/pairs_zscore.rs`'s `PairsZScore` closing leg A
    ///   with leg B's signed size is what it costs.
    /// - **`0.0` unconditionally**, i.e. for a single-symbol mount too: that breaks every strategy
    ///   running today, which passes the mount's own symbol (or `""`) to mean "mine" and would start
    ///   reading flat. Hence [`LiveBroker::carries`]' empty-table arm.
    /// - **panic, as `OrderBrokerSimulator::index` does** on an unknown symbol: a backtest may die naming the
    ///   symbol — nothing is at risk but the run. The live core thread folds venue events for every
    ///   mount on the process; killing it because one strategy asked a question flattens nothing,
    ///   cancels nothing, and strands real resting orders at the venue.
    ///
    /// The parity that matters is preserved, and it is not "the same number": NEITHER engine answers
    /// about a different instrument. `crates/vike-simulator/tests/parity/multi_symbol_read_parity.rs` is the
    /// gate for the carried half, where the two engines genuinely must agree.
    fn position(&self, symbol: &str) -> f64 {
        let symbol = self.read_name(symbol);
        match self.declared_read(&self.positions, symbol) {
            Some(pos) => pos,
            None if self.carries(symbol) => self.position,
            None => 0.0,
        }
    }

    /// Per symbol, with [`OrderBroker::position`]'s partition — plus one RESIDUAL, stated rather than
    /// glossed: a declared leg the price resolver could not answer for (no mark, no quote, no trade,
    /// no bar — `CoreThread::push_view` writes no price row for it) still falls through to the
    /// scalar, i.e. to the DISPATCHING series' price. It is carried, so `carries` says nothing about
    /// it; the tables cannot tell that case apart from the fill lane's dispatching symbol, whose
    /// scalar IS its own price. Closing it means teaching `LiveBroker` which symbol dispatched, a
    /// second field on a struct built per market message — a separate change with its own latency
    /// argument. The uncarried case, which is what this PR is about, is unaffected: it never reaches
    /// the scalar at all.
    fn price(&self, symbol: &str) -> f64 {
        let symbol = self.read_name(symbol);
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
    /// This read used to discard its argument outright (it was spelled `_symbol`). [`OrderBroker::bars`]'
    /// own contract licenses that — "single-symbol live brokers may ignore `symbol`" — and for a
    /// single-symbol mount it is still exactly what happens here, byte-identically, because the
    /// table is empty. A mount that DECLARED a second leg is not a single-symbol broker, and there
    /// the licence lapsed: `bars(leg_b)` during a leg-A bar handed back leg A's history, so a
    /// strategy computing a spread from it read one leg twice — silently, and only when live, since
    /// `OrderBrokerSimulator::bars` indexes by symbol.
    ///
    /// A DECLARED mount asked about a symbol it does not carry gets an EMPTY slice: the only shape a
    /// `&[Bar]` return can express for "I have no history of that", and the one a strategy already
    /// handles (`bars(x).last()` is `None`, every indicator warmup gate is unmet). It cannot be
    /// mistaken for another instrument's history the way the old answer could. See
    /// [`OrderBroker::position`] for the full argument over the alternatives.
    ///
    /// ⚠ COST, since this struct is built once per SUBSCRIBED MOUNT per market message on
    /// `CoreThread::drive_strategy_tick` — the fold the `p99 < 10µs` gate protects: a single-symbol
    /// mount pays one `Vec::new()` and one more drop, no allocation and no engine read, because
    /// `CoreThread::declared_views` early-returns `Default` for it. Only a declared-multi mount
    /// materializes rows, and `crates/vike-core/tests/runtime_latency.rs`'s `mounted-multi` variant
    /// is the lane that measures them.
    fn bars(&self, symbol: &str) -> &[Bar] {
        let symbol = self.read_name(symbol);
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

    /// The mount's OWN stream label (decision 0140). Answered for `""` and, on a single-symbol mount
    /// (the empty `bar_views` table, where [`OrderBroker::bars`] already licenses ignoring the name), for
    /// any symbol; on a multi-symbol mount for its own symbol's name too. A declared LEG reads its
    /// own stream's label from [`LiveBroker::leg_values`]; a symbol the mount did not declare, `None`.
    fn data_delay(&self, symbol: &str) -> Option<vike_model::StreamDelay> {
        if self.names_own(symbol) {
            return self.data_delay;
        }
        self.leg_values
            .as_deref()?
            .iter()
            .find(|(s, ..)| s == symbol)
            .and_then(|(.., delay)| *delay)
    }

    /// The input's bars that had CLOSED by this dispatch's decision (decision 0271), oldest first, from
    /// the mount's view: the backtest's as-of rule (`OrderBroker::input` is the last of them). Empty before
    /// its first bar closes, for a name the strategy did not declare, and on a mount that reads no
    /// input. The view holds what the mount read at its start (the tradehub reads as deep as a backtest:
    /// a longer input's whole stored history, any other its last `INPUT_HISTORY_BARS`) and every bar since.
    fn input_bars(&self, name: &str) -> &[Bar] {
        self.inputs.as_ref().map_or(&[], |inputs| inputs.closed_bars(name))
    }

    /// The mount's closed trades, booked by the simulator's ledger from every fill delivered to it, a fill's
    /// rows before the strategy hears it (`crates/vike-core/src/runtime/closed_trades.rs`'s module doc): ALWAYS
    /// `Some`, an empty slice before the first close. Rows from before a restart are not in it.
    fn closed_trades(&self) -> Option<&[vike_model::TradeClosedRow]> {
        Some(self.closed_trades.as_deref().map_or(&[], |ledger| ledger.rows()))
    }

    /// The gate's grid for the mount's OWN symbol, answered as [`Self::data_delay`] is; a declared
    /// LEG's from [`LiveBroker::leg_values`] (the row its engine's gate rounds the leg's orders on,
    /// `vike_model::RiskLimits::grid_for`); `None` when the gate holds no grid at all, or none of its
    /// own for the leg, and for a symbol the mount did not declare.
    fn instrument_grid(&self, symbol: &str) -> Option<vike_model::Grid> {
        if !self.names_own(symbol) {
            let legs = self.leg_values.as_deref()?;
            return legs.iter().find(|(s, ..)| s == symbol).and_then(|(_, _, grid, _)| *grid);
        }
        let nz = vike_model::nz_step;
        let grid = vike_model::Grid {
            tick_size: nz(self.tick_size),
            lot_size: nz(self.lot_size),
            min_notional: nz(self.min_notional),
            min_qty: nz(self.min_qty),
        };
        [grid.tick_size, grid.lot_size, grid.min_notional, grid.min_qty]
            .iter()
            .any(Option::is_some)
            .then_some(grid)
    }

    /// The `contract_value` field on a single-symbol mount (the empty `bar_views` table, as
    /// [`Self::data_delay`] answers); on a declared mount every symbol, `""` being the mount's own,
    /// reads [`LiveBroker::leg_values`] (the mount's own row first), the value its leg's account books
    /// it by, so a strategy's PnL fold and the ledger cannot disagree. A symbol the mount did not
    /// declare is `None`.
    fn contract_value(&self, symbol: &str) -> Option<vike_model::ContractValue> {
        let symbol = self.read_name(symbol);
        if symbol.is_empty() || self.bar_views.is_empty() {
            return Some(self.contract_value);
        }
        self.leg_values.as_deref()?.iter().find(|(s, ..)| s == symbol).map(|(_, v, ..)| *v)
    }
}

#[path = "tests/broker.rs"]
#[cfg(test)]
mod broker_tests;

#[path = "tests/strategy_trigger_kinds.rs"]
#[cfg(test)]
mod strategy_trigger_kinds_tests;

#[path = "tests/strategy_order_lists.rs"]
#[cfg(test)]
mod strategy_order_lists_tests;

#[path = "tests/strategy_order_broker.rs"]
#[cfg(test)]
mod strategy_order_broker_tests;
