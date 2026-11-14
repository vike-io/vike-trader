//! `lower_combo` — one `OrderIntent::Combo` lowered onto the engine apply surface.

use super::*;
use vike_model::orders::client_id::{OrderClientIdIntent, OrderClientIdLabels};

impl<C: ExecutionClient> CoreThread<C> {
    /// Lower one [`OrderIntent::Combo`] onto the engine apply surface — the PR-2 half of combo
    /// orders (PR-1 shipped the vocabulary; `vike_execution::RiskGate::check_combo` shipped the gate).
    ///
    /// `venue_supports_combo` is passed IN rather than read here so the static-registry lookup
    /// stays at the call site and this function is a pure function of the answer.
    /// [`vike_model::build_combo`] deliberately leaves `symbol` EMPTY for the venue adapter to
    /// resolve at submit, so a `true` caps row is a PROMISE the adapter really does that step —
    /// the registry keeps every other venue's combo on the terminal-reject path below, the
    /// protection that stops an empty-symbol order reaching a live venue. DERIBIT is the one
    /// `true` row (pinned by `vike_model`'s `combo_support_is_deribit_only`): its `submit_order`
    /// routes a non-empty `combo_legs` through `private/create_combo` + the orientation solve
    /// (`vike_deribit::execution::combo`), proven live by the gated `deribit_combo_smoke` order lifecycle.
    ///
    /// Shape, in order:
    /// 0. **The account the spec names** (`OrderCombo::account`) resolves first, `lower_bracket`'s
    ///    account step: an unheld account is refused by name and nothing is minted.
    /// 1. **Validate, THEN mint ONE order client id for the whole combo** — never one per leg — or take the one
    ///    its sender named (`OrderCombo::order_client_id`), refusing the combo whole, nothing sent,
    ///    when this node already holds that id (`lower_order_list`'s rule). A combo is ONE
    ///    order everywhere downstream (one order client id, one `OrderRecord`, one event stream) because the
    ///    VENUE is the group manager; `build_combo` carries every leg on that single request. An
    ///    invalid (hand-built) spec refuses BEFORE the mint — no order-client-id-sequence gap.
    /// 2. **Write-ahead journal** of the resolved request (`append_minted_submit`), exactly as the
    ///    `Submit`/`Bracket` arms do: the `Command` record for this intent carries the PRE-mint
    ///    `OrderCombo` with no order client id at all, so without this a combo that terminalizes without ever
    ///    filling has no `execution_order` seed for the materializer. Journaled BEFORE the capability
    ///    and risk refusals on purpose — a refused combo is precisely the audit trail worth
    ///    keeping. (The invalid-spec refusal precedes the mint, so there is nothing to journal.)
    /// 3. **Venue capability.** A venue that cannot take a combo gets a SYNTHESIZED TERMINAL
    ///    lifecycle — `OrderSubmitted` then `OrderRejected` — never a silent drop (the emitter-split
    ///    contract: no order may vanish, and a dead venue path must synthesize the terminal
    ///    itself). Registered first so the FSM has an entry for those two events to advance. A combo
    ///    priced by its legs (`OrderComboLeg::limit`) takes the same terminal reject, in its own words, on
    ///    every engine whose stated tier is not `paper`: no venue row takes a leg limit, and only the
    ///    paper book fills one (wave 6 of the order kinds; no capability axis until a venue does).
    /// 4. **Risk.** [`vike_execution::RiskGate::check_combo`] crosses every leg as a synthetic
    ///    single-leg order and ACCUMULATES the admitted legs' commitments, so N legs cannot each
    ///    fit inside the same unchanged free buying power. A veto surfaces as `OrderDenied` — the
    ///    same event a single-order veto produces, captured for `Strategy::on_order_event` exactly
    ///    as `gate_and_register` captures it — and the combo never enters the registry. Ahead of it,
    ///    the spot rule on every leg the combo sells (`Engine::combo_cash_denial`, a plain
    ///    sell's `cash_oversell_denial` per leg) denies the same way, naming the leg.
    /// 5. **Submit.** Admit every leg symbol into the engine's `extra_symbols` scope (leg fills
    ///    carry LEG symbols; without this the bare-`Fill` symbol filter drops them and the Account
    ///    stays flat), then register + hand the request to the client, mirroring
    ///    `Engine::submit_order`'s tail. The venue emits the rest of the lifecycle. Last, the
    ///    host hears the legs (`CoreConfig::on_combo_legs`): the half of an admission the core cannot
    ///    make, telling the venue's reconcile client to read them.
    ///
    /// Returns the order client id when the combo reached the venue AND when it was REFUSED after the mint: the
    /// RiskGate denial (an `OrderDenied`) and the no-combo-venue reject (`venue_supports_combo` false,
    /// or leg limits off paper: `OrderSubmitted` + `OrderRejected`) each name an order that was minted
    /// and journaled, exactly as `lower_submit`'s RiskGate and capability refusals do, so the mount that
    /// built it is attributed the event. The refusals BEFORE the mint (an unheld account, ambiguity, an
    /// invalid spec, a given id already in use) mint nothing and return empty.
    /// Validation runs BEFORE the mint (the `ArmConditional` mint-after-validation rule: a refused spec that
    /// burned an order client id would leave a sequence gap, and a gap is only diagnostic while an id that
    /// exists but names no order stays impossible); the capability and risk refusals mint FIRST
    /// because their refusal events must NAME an order — the same trade-off the single-order path
    /// makes when it mints ahead of the gate.
    ///
    /// COLD PATH: this runs per ORDER, never per market message, so the per-leg context snapshot
    /// and its `Vec` are off the measured p99 core hop. No logging happens on the fold path.
    pub(crate) fn lower_combo(
        &mut self,
        spec: vike_model::OrderCombo,
        now: i64,
        venue_supports_combo: bool,
        route: EngineRoute,
    ) -> Vec<String> {
        // **THE SPEC NAMES ITS ACCOUNT, resolved BEFORE the ambiguity gate below**: `lower_bracket`'s
        // account step, for its reason (decision 0254: a combo is ONE order on ONE account). An unheld
        // account is refused by name before anything is minted; `account: None` short-circuits, so a
        // spec written before the field takes the path it always took.
        let route = match spec.account.as_ref() {
            None => route,
            Some(account) => match self.route_for_payload_account(route, &spec.venue, account) {
                Some(resolved) => resolved,
                None => {
                    self.refuse_unheld_account("combo", &spec.venue, account);
                    self.pump_client();
                    return Vec::new();
                }
            },
        };
        // §4.2 row 1, the risk-INCREASING side of §4.5's law — refused FIRST, ahead of even the
        // spec validation, because a refusal that cannot name a book is not about the spec at all.
        // Empty return, matching every other refusal in this function.
        if let Some(candidates) = self.ambiguous_accounts(route, &spec.venue) {
            self.refuse_ambiguous("combo", &spec.venue, &candidates);
            self.pump_client();
            return Vec::new();
        }
        let engine_index = self.route_of(route, &spec.venue).unwrap_or(0);
        // Validate FIRST, mint SECOND. `OrderCombo` cannot normally be invalid (`validate` runs in
        // both its constructor and its `Deserialize`), so a failure here is a hand-built spec.
        // Refuse loudly, mint nothing, emit nothing: there is no order to name yet — and minting
        // one would burn an order client id into a sequence gap (see the doc above). `pump_client` still runs,
        // for uniformity with every other arm's tail.
        if let Err(e) = spec.validate() {
            self.note(format!("combo REFUSED: {e}"));
            self.pump_client();
            return Vec::new();
        }
        // A combo whose SENDER named its id (`OrderCombo::order_client_id`: a remote combo, retry-safe)
        // is placed under it, and an id this node already holds refuses the combo WHOLE before
        // anything is journaled or sent, in a single submit's words for a reused id, as
        // `lower_order_list` refuses a list. No id (or an empty one, as a submit's) is minted.
        let order_client_id = match spec.order_client_id.as_deref().filter(|c| !c.is_empty()) {
            Some(given) if self.order_id_in_use(engine_index, given) => {
                self.refuse_order_id_in_use(engine_index, given);
                self.note(format!(
                    "combo on {} REFUSED WHOLE, nothing was sent: its order id {given} is already \
                     used",
                    spec.venue
                ));
                self.pump_client();
                return Vec::new();
            }
            Some(given) => given.to_string(),
            // ONE id on the routed engine's plan. A combo holds no one position, so its intent is its side
            // read flat (`OL` or `OS`).
            None => {
                let (src, strat) = self.order_client_id_who(route);
                let intent = OrderClientIdIntent::of(spec.side, false, false, 0.0);
                self.order_client_id_plans[engine_index].mint(
                    &mut self.order_client_id_gen,
                    OrderClientIdLabels::new(src, strat, intent),
                )
            }
        };
        let Ok(mut req) = vike_model::build_combo(&spec, &order_client_id) else {
            // Unreachable: `validate` passed just above and `build_combo`'s only failure IS
            // `validate` on the same immutable spec. Refuse gracefully anyway — never panic in
            // the fold.
            self.note("combo REFUSED: spec failed re-validation".to_string());
            self.pump_client();
            return Vec::new();
        };
        req.timestamp = now;
        if self.order_client_id_row_needed(engine_index) {
            self.order_client_id_venue.insert(order_client_id.clone(), engine_index);
        }
        self.journal_minted_submit(engine_index, &req, now);
        // Stamp the engine clock BEFORE the gate, mirroring the single-order path (the `Submit`
        // arm stamps ahead of `submit_order`): a denied combo must not leave `now_ms` stale.
        self.engine_mut(engine_index).now_ms = now;

        // A STOPPED account refuses the combo (`refuse_for_stopped_account`), terminal, never sent.
        if self.refuse_for_stopped_account(engine_index, &req, now) {
            self.pump_client();
            return vec![order_client_id];
        }

        // A combo priced by its legs is filled only by a PAPER book (`vike_model::combo_legs_cross`):
        // no venue row takes a leg limit (deribit prices a combo by its net), so on any engine whose
        // STATED tier is not `paper` (the seed is no proof, decision 0201; a replay core rebuilt from a
        // snapshot carries the statement) it takes the no-combo-venue reject below, in its own words.
        let legs_off_paper = venue_supports_combo
            && spec.legs.iter().any(|l| l.limit.is_some())
            && self.engine(engine_index).stated_tier() != Some(vike_model::AccountTier::Paper);
        if !venue_supports_combo || legs_off_paper {
            let (log, reason, note) = if legs_off_paper {
                let why = format!(
                    "venue {} takes no limit per combo leg: no venue row does, only a paper book \
                     fills one",
                    spec.venue
                );
                ("combo REJECTED: no venue takes a limit per combo leg", why.clone(), why)
            } else {
                (
                    "combo REJECTED: venue declares no combo support",
                    format!("venue {} does not support combo orders", spec.venue),
                    format!("venue {} declares no combo support", spec.venue),
                )
            };
            // No order may silently vanish. There is no venue client on the other side of this to
            // emit the lifecycle, so synthesize the whole terminal sequence locally. Registered
            // FIRST so the FSM has an entry for `OrderSubmitted` (Initialized -> Submitted) and
            // then `OrderRejected` (-> Rejected) to advance through. Per-ORDER log, off the fold.
            tracing::warn!(target: "vike_core::core", order_client_id = %order_client_id, venue = %spec.venue, "{log}");
            let engine = self.engine_mut(engine_index);
            let mut mo = vike_model::OrderRecord::new(req.clone());
            mo.created_ms = Some(now);
            engine.registry.insert(order_client_id.clone(), mo);
            self.publish_to(
                engine_index,
                Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                    order_client_id: order_client_id.clone(),
                    timestamp: now,
                }),
            );
            self.publish_to(
                engine_index,
                Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: now,
                }),
            );
            self.note(format!("combo REJECTED: {note}"));
            self.pump_client();
            // NAMES the rejected order, like `lower_submit`'s capability reject: it was minted,
            // journaled and registered (terminal), and `apply_strategy_intent` attributes only what
            // it is returned.
            return vec![order_client_id];
        }

        // Snapshot each leg's own risk facts BEFORE reaching for the gate: `check_combo` takes
        // `leg_context` by `Fn`, and a closure reading the engine's `account` live would borrow the
        // engine immutably while `gate` is borrowed mutably off that same struct. A `Vec` (not a
        // map) because a combo is 2..~6 legs — the linear probe is cheaper than hashing, and this
        // is a per-order path anyway.
        let mut leg_contexts: Vec<(String, vike_execution::RiskContext)> =
            Vec::with_capacity(spec.legs.len());
        {
            let engine = self.engine(engine_index);
            // Equity is an ACCOUNT-level fact — identical for every leg — folded ONCE, and only
            // when the margin knob is armed for at least one leg (`resolved_equity` is
            // O(positions); per-order path, still not free). One-price law: resolver-priced,
            // the SAME `config.price_cfg` source the snapshot/sampler/watchdog read.
            let armed_any = spec
                .legs
                .iter()
                .any(|l| engine.gate.limits.initial_margin_for(&l.symbol).is_some());
            // The ACCOUNT-aggregate ceiling is a per-ENGINE fact, so whether it is armed is
            // loop-invariant — read once here, beside the equity gate above and for the same
            // reason: the fold it guards is O(positions + live orders), and a deployment that wrote
            // no `max_account_exposure` must pay nothing for a lane it did not turn on. (The FOLD
            // itself is not loop-invariant; it excludes the leg's own symbol, so it runs per leg.)
            let account_ceiling_armed = engine.gate.limits.max_account_exposure.is_some();
            // ⚠ `sizing_equity`, not `resolved_equity` — the combo twin of `gate_and_register`'s
            // own line. This is the buying-power basis a combo is ADMITTED against, so
            // `RiskLimits::max_sizing_equity` applies and a lower figure can only refuse sooner.
            // Bit-identical when no ceiling is armed; the margin-CALL sweep keeps the uncapped
            // resolver, and `vike_execution::Engine::sizing_equity`'s doc carries the asymmetry.
            let equity = if armed_any {
                engine.sizing_equity(engine.equity_seed, &self.config.price_config)
            } else {
                0.0
            };
            for leg in &spec.legs {
                // THE gate's margin (`Engine::gate_margin_used`, the single order's too), the
                // open positions and the live un-filled orders, priced PER LEG: a combo has no single
                // order symbol, so a no-override position or order falls back to THE LEG BEING PRICED's
                // rate, which is why this runs per leg rather than once: two legs with different rates
                // price the same no-override position differently, exactly as two naked orders on
                // those symbols would. An unarmed leg (`initial_margin_for` None) gets the single path's unarmed
                // tuple: the buying-power check is off for it and never reads these fields.
                // `check_combo` threads each admitted leg's own margin on top of this baseline itself.
                let (leg_equity, margin_used) = if let Some(leg_initial_margin) =
                    engine.gate.limits.initial_margin_for(&leg.symbol)
                {
                    (
                        equity,
                        engine.gate_margin_used(
                            &self.config.price_config,
                            leg_initial_margin,
                            &order_client_id,
                        ),
                    )
                } else {
                    (0.0, 0.0)
                };
                // The leg's own position size as a plain order on it is judged (a leg carries no
                // reduce-only flag, so `cover_position_size` would answer this same figure), reused for
                // the resolver's side-aware quote choice below exactly as the single-order gate does.
                let pos = engine.gate_position_size(&leg.symbol);
                let (quote_rate, quote_stale) = engine.account.quote_leg(&leg.symbol, now);
                leg_contexts.push((
                    leg.symbol.clone(),
                    vike_execution::RiskContext {
                        position_size: pos,
                        // The leg's notional/exposure REFERENCE now shares the resolver with the
                        // per-leg equity/margin folded above — the one-price law extended to this
                        // gate's LAST split-basis site (the combo twin of #550's `gate_and_register`
                        // convergence). Priced through the position's OWN side chain under
                        // `config.price_cfg`, the SAME basis the snapshot/sampler/watchdog read,
                        // rather than off the raw untagged `Account.marks` scalar `mark_of` returned
                        // while the same call's equity/margin were board-priced. `check_combo` still
                        // DENIES a leg whose reference is missing, non-finite or <= 0
                        // (`leg <sym>: no-mark`) precisely because a zero reference makes every
                        // price-based limit (notional cap, exposure, buying power) vacuous at once —
                        // a `Missing` resolution maps to 0.0, exactly the empty raw-scalar read it
                        // replaces, so that refusal is preserved. A fresh venue mark prices both
                        // stores identically, so an ordinary marked leg is byte-identical to the
                        // pre-convergence `mark_of` read; only a STALE mark tightens the verdict.
                        mark_price: engine
                            .resolved_position_price(
                                &engine.venue,
                                &leg.symbol,
                                pos,
                                &self.config.price_config,
                            )
                            .unwrap_or(0.0),
                        trading_state: engine.trading_state,
                        now_ms: now,
                        equity: leg_equity,
                        margin_used,
                        // `check_combo` re-derives each leg's own reduce_only from the PROJECTED
                        // book, so the reversing-leg margin credit `gate_and_register` computes has
                        // no single-symbol meaning here. Left 0.0 so the crossing errs strictly
                        // HIGH — the gate's own stance ("a combo must never pass a gate its naked
                        // legs would fail").
                        closing_credit: 0.0,
                        contract_value: engine.account.contract_value_of(&leg.symbol),
                        // The leg's quote currency, as the single-order gate reads it: the rate the
                        // value carries and whether it has gone stale at this combo's `now`.
                        quote_rate,
                        quote_stale,
                        // The ACCOUNT-aggregate ceiling's other half, per leg — this account's
                        // gross exposure with THIS leg's symbol's POSITION left out, so
                        // `check_combo` can add each leg's own projection back on, plus every live
                        // resting order the account already has out. Folded only when the ceiling
                        // is armed, the same discipline `gate_and_register`'s single-order twin
                        // follows and for the same reason (it is O(positions + orders), and a
                        // deployment that wrote no `max_account_exposure` must pay nothing for it).
                        // ⚠ Per LEG rather than once for the combo: the excluded symbol differs.
                        // `check_combo` threads each admitted leg's own committed notional on top
                        // of this baseline itself, exactly as it does for margin.
                        // ⚠ `order_client_id` is the JUDGED order — this combo's own, minted above and not yet
                        // registered, so it excludes nothing today. It is passed rather than `""`
                        // because the producer's contract is "leave out the order being judged",
                        // and a combo that ever became re-judgeable (an amend) would need exactly
                        // this and would have no other place to learn it.
                        // ⚠ Plus the lane-twin partner's book (`Engine::sibling_exposure`, 0.0
                        // for an engine with none), the same term `Engine::risk_context` adds.
                        account_exposure_excl_order: if account_ceiling_armed {
                            engine.resolved_account_exposure_excluding(
                                &leg.symbol,
                                &order_client_id,
                                &self.config.price_config,
                            ) + engine.sibling_exposure
                        } else {
                            0.0
                        },
                    },
                ));
            }
        }

        // Account-level ctx. `check_combo` takes `trading_state` + `now_ms` from HERE and lets them
        // OVERRIDE whatever `leg_context` returns, so a `Reducing`/`Halted` account can never be
        // laundered into `Active` by the per-leg snapshot.
        let ctx = vike_execution::RiskContext {
            trading_state: self.engine(engine_index).trading_state,
            now_ms: now,
            ..Default::default()
        };
        // The spot rule on every leg the combo sells (`Engine::combo_cash_denial`), asked ahead of
        // the gate as `gate_and_register` asks the plain rule, so a refused combo takes no throttle slot.
        let verdict = match self.engine(engine_index).combo_cash_denial(&req) {
            Some(reason) => vike_execution::RiskVerdict { ok: false, request: None, reason },
            None => self.engine_mut(engine_index).gate.check_combo(&req, &ctx, |symbol_name| {
                // Total by contract. A symbol with no snapshot falls back to a default ctx whose
                // mark is 0.0, which `check_combo` DENIES as `no-mark` — the honest answer for a
                // symbol the runtime knows nothing about.
                leg_contexts
                    .iter()
                    .find(|(s, _)| s == symbol_name)
                    .map(|(_, c)| *c)
                    .unwrap_or_default()
            }),
        };

        let Some(admitted) = verdict.request.filter(|_| verdict.ok) else {
            // Per-ORDER (not per-tick): a RiskGate veto is fault-adjacent, off the measured hop —
            // the same budget the single-order veto log takes in `gate_and_register`.
            tracing::warn!(
                target: "vike_core::risk",
                order_client_id = %order_client_id,
                reason = %verdict.reason,
                "combo denied by RiskGate"
            );
            self.note(format!("combo DENIED: {}", verdict.reason));
            // Mirror `gate_and_register`'s veto capture for `Strategy::on_order_event`: a denied
            // combo never enters the registry, so the FSM-apply capture site can't see it — without
            // this a strategy would never learn its combo was vetoed. Routed by (venue, FIRST leg
            // symbol), CONSISTENTLY: the combo's own `req.symbol` is EMPTY by design (`build_combo`
            // leaves it for the adapter to resolve at submit), and the first leg is the spec's
            // defining leg — the mount trading it is the one that armed the combo. Gated on the
            // same mount flag as `applied_fills`.
            {
                let engine = self.engine_mut(engine_index);
                if engine.collect_applied_fills {
                    engine.order_events.push(vike_execution::execution_engine::OrderEventOut {
                        venue: req.venue.clone(),
                        symbol: req
                            .combo_legs
                            .first()
                            .map(|l| l.symbol.clone())
                            .unwrap_or_default(),
                        event: vike_model::OrderEvent {
                            order_client_id: order_client_id.clone(),
                            // Stamped at DELIVERY (`dispatch_order_events`) if this order client id holds a
                            // tag entry, never here — one spelling of the rule. A combo leg is
                            // not a tagged quote, so in practice this stays `None`.
                            tag: None,
                            kind: vike_model::OrderEventKind::Denied {
                                reason: verdict.reason.clone(),
                            },
                        },
                    });
                }
            }
            // A denied combo never enters the registry — mirroring `gate_and_register`, where a
            // vetoed order is published as denied and dropped without registration.
            self.publish_to(
                engine_index,
                Event::OrderDenied(vike_model::events::OrderDenied {
                    order_client_id: order_client_id.clone(),
                    reason: verdict.reason.into(),
                    timestamp: now,
                }),
            );
            self.pump_client();
            // NAMES the denied order, like `lower_submit`'s RiskGate refusal: it exists (minted,
            // journaled, an `OrderDenied` published) and `apply_strategy_intent` can only attribute
            // what it is returned, so an empty list left the owner to the first-mount fallback.
            return vec![order_client_id];
        };

        // Mirrors `Engine::submit_order`'s tail (register + `client.submit`), open-coded
        // because the crossing above is `check_combo` — calling `submit_order` here would re-run
        // the SINGLE-order `check`, which prices the request off its SIGNED net (negative for a
        // credit structure, so every price-based limit inverts) and would burn a SECOND throttle
        // slot for one order.
        let engine = self.engine_mut(engine_index);
        // ADMIT every leg symbol into this engine's venue-event scope BEFORE the order is on the
        // wire: `Engine::on_event` gates a bare `Event::Fill` on `accepts_symbol` (the
        // account-wide-WS scoping filter), and a combo's leg fills arrive carrying LEG symbols —
        // the combo has no symbol of its own. Without this the `OrderFilled` wraps advance the
        // FSM (they route by order client id) while the `Account` never folds the fills and
        // `Strategy::on_fill` never fires — the strategy is blind to its own filled combo.
        // `extra_symbols` (the Phase D multi-symbol mechanism) stays the PRIMARY admission
        // because it also lets Funding/PositionLiquidated events on the leg symbols fold — leg
        // positions are REAL positions with real funding, and a fill-only route cannot carry
        // those. Since combo gate 4, `Engine::owns_fill_symbol` ALSO routes a bare fill
        // whose order client id names a registered order and whose symbol is that order's own symbol or one
        // of its `combo_legs` — the safety net for fills, and the ONLY route that classifies the
        // venue's combo-instrument net-price print (deliberately NOT folded — a phantom position;
        // see that fn's doc + the deribit fill probe) — so the two mechanisms are complementary,
        // not alternatives. Replay-safe: the journaled `Command` re-runs this lowering on replay, and
        // `extra_symbols` rides the `EngineSnapshot`.
        for leg in &admitted.combo_legs {
            if !engine.accepts_symbol(&leg.symbol) {
                engine.extra_symbols.push(leg.symbol.clone());
            }
        }
        let mut mo = vike_model::OrderRecord::new(admitted.clone());
        mo.created_ms = Some(now);
        engine.registry.insert(order_client_id.clone(), mo);
        engine.fence_send();
        engine.client.submit(&admitted);
        self.pump_client();
        // ...and the HOST hears the legs (`CoreConfig::on_combo_legs`): it tells the venue's reconcile
        // client, which no `AdmitSymbol` told. Every leg of every accepted combo (the host de-duplicates),
        // never a refused one: those pushed nothing above.
        if let Some(tell) = self.config.on_combo_legs.as_ref() {
            let engine = self.engine(engine_index);
            let legs: Vec<String> = admitted
                .combo_legs
                .iter()
                .filter(|leg| leg.symbol != engine.symbol)
                .map(|leg| leg.symbol.clone())
                .collect();
            if !legs.is_empty() {
                tell(&engine.route_key, &order_client_id, &legs);
            }
        }
        vec![order_client_id]
    }
}
