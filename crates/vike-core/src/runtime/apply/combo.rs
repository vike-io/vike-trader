//! `lower_combo` — one `OrderIntent::Combo` lowered onto the engine apply surface.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Lower one [`OrderIntent::Combo`] onto the engine apply surface — the PR-2 half of combo
    /// orders (PR-1 shipped the vocabulary; `vike_exec::RiskGate::check_combo` shipped the gate).
    ///
    /// `venue_supports_combo` is passed IN rather than read here so the static-registry lookup
    /// stays at the call site and this function is a pure function of the answer.
    /// [`vike_model::build_combo`] deliberately leaves `symbol` EMPTY for the venue adapter to
    /// resolve at submit, so a `true` caps row is a PROMISE the adapter really does that step —
    /// the registry keeps every other venue's combo on the terminal-reject path below, the
    /// protection that stops an empty-symbol order reaching a live venue. DERIBIT is the one
    /// `true` row (pinned by `vike_model`'s `combo_support_is_deribit_only`): its `submit_order`
    /// routes a non-empty `combo_legs` through `private/create_combo` + the orientation solve
    /// (`vike_deribit::combo`), proven live by the gated `deribit_combo_smoke` order lifecycle.
    ///
    /// Shape, in order:
    /// 1. **Validate, THEN mint ONE coid for the whole combo** — never one per leg. A combo is ONE
    ///    order everywhere downstream (one coid, one `ManagedOrder`, one event stream) because the
    ///    VENUE is the group manager; `build_combo` carries every leg on that single request. An
    ///    invalid (hand-built) spec refuses BEFORE the mint — no coid-sequence gap.
    /// 2. **Write-ahead journal** of the resolved request (`append_minted_submit`), exactly as the
    ///    `Submit`/`Bracket` arms do: the `Cmd` record for this intent carries the PRE-mint
    ///    `ComboSpec` with no coid at all, so without this a combo that terminalizes without ever
    ///    filling has no `exec_order` seed for the materializer. Journaled BEFORE the capability
    ///    and risk refusals on purpose — a refused combo is precisely the audit trail worth
    ///    keeping. (The invalid-spec refusal precedes the mint, so there is nothing to journal.)
    /// 3. **Venue capability.** A venue that cannot take a combo gets a SYNTHESIZED TERMINAL
    ///    lifecycle — `OrderSubmitted` then `OrderRejected` — never a silent drop (the emitter-split
    ///    contract: no order may vanish, and a dead venue path must synthesize the terminal
    ///    itself). Registered first so the FSM has an entry for those two events to advance.
    /// 4. **Risk.** [`vike_exec::RiskGate::check_combo`] crosses every leg as a synthetic
    ///    single-leg order and ACCUMULATES the admitted legs' commitments, so N legs cannot each
    ///    fit inside the same unchanged free buying power. A veto surfaces as `OrderDenied` — the
    ///    same event a single-order veto produces, captured for `Strategy::on_order_event` exactly
    ///    as `gate_and_register` captures it — and the combo never enters the registry.
    /// 5. **Submit.** Admit every leg symbol into the engine's `extra_symbols` scope (leg fills
    ///    carry LEG symbols; without this the bare-`Fill` symbol filter drops them and the Account
    ///    stays flat), then register + hand the request to the client, mirroring
    ///    `ExecutionEngine::submit_order`'s tail. The venue emits the rest of the lifecycle.
    ///
    /// Returns the coid when the combo reached the venue AND when it was REFUSED after the mint: the
    /// RiskGate denial (an `OrderDenied`) and the no-combo-venue reject (`venue_supports_combo` false:
    /// `OrderSubmitted` + `OrderRejected`) each name an order that was minted and journaled, exactly as
    /// `lower_submit`'s RiskGate and capability refusals do, so the mount that built it is attributed
    /// the event. The refusals BEFORE the mint (ambiguity, invalid spec) mint nothing and return empty.
    /// Validation runs BEFORE the mint (the `ArmConditional` mint-after-validation rule: a refused spec that
    /// burned a coid would leave a sequence gap, and a gap is only diagnostic while an id that
    /// exists but names no order stays impossible); the capability and risk refusals mint FIRST
    /// because their refusal events must NAME an order — the same trade-off the single-order path
    /// makes when it mints ahead of the gate.
    ///
    /// COLD PATH: this runs per ORDER, never per market message, so the per-leg context snapshot
    /// and its `Vec` are off the measured p99 core hop. No logging happens on the fold path.
    pub(crate) fn lower_combo(
        &mut self,
        spec: vike_model::ComboSpec,
        now: i64,
        venue_supports_combo: bool,
        route: EngineRoute,
    ) -> Vec<String> {
        // §4.2 row 1, the risk-INCREASING side of §4.5's law — refused FIRST, ahead of even the
        // spec validation, because a refusal that cannot name a book is not about the spec at all.
        // Empty return, matching every other refusal in this function.
        if let Some(candidates) = self.ambiguous_accounts(route, &spec.venue) {
            self.refuse_ambiguous("combo", &spec.venue, &candidates);
            self.pump_client();
            return Vec::new();
        }
        let eidx = self.route_of(route, &spec.venue).unwrap_or(0);
        // Validate FIRST, mint SECOND. `ComboSpec` cannot normally be invalid (`validate` runs in
        // both its constructor and its `Deserialize`), so a failure here is a hand-built spec.
        // Refuse loudly, mint nothing, emit nothing: there is no order to name yet — and minting
        // one would burn a coid into a sequence gap (see the doc above). `pump_client` still runs,
        // for uniformity with every other arm's tail.
        if let Err(e) = spec.validate() {
            self.note(format!("combo REFUSED: {e}"));
            self.pump_client();
            return Vec::new();
        }
        let coid = self.coid_gen.generate();
        let Ok(mut req) = vike_model::build_combo(&spec, &coid) else {
            // Unreachable: `validate` passed just above and `build_combo`'s only failure IS
            // `validate` on the same immutable spec. Refuse gracefully anyway — never panic in
            // the fold.
            self.note("combo REFUSED: spec failed re-validation".to_string());
            self.pump_client();
            return Vec::new();
        };
        req.ts = now;
        if eidx != 0 {
            self.coid_venue.insert(coid.clone(), eidx);
        }
        self.journal_minted_submit(eidx, &req, now);
        // Stamp the engine clock BEFORE the gate, mirroring the single-order path (the `Submit`
        // arm stamps ahead of `submit_order`): a denied combo must not leave `now_ms` stale.
        self.eng_mut(eidx).now_ms = now;

        if !venue_supports_combo {
            // No order may silently vanish. There is no venue client on the other side of this to
            // emit the lifecycle, so synthesize the whole terminal sequence locally. Registered
            // FIRST so the FSM has an entry for `OrderSubmitted` (Initialized -> Submitted) and
            // then `OrderRejected` (-> Rejected) to advance through. Per-ORDER log, off the fold.
            tracing::warn!(
                target: "vike_core::core",
                coid = %coid,
                venue = %spec.venue,
                "combo REJECTED: venue declares no combo support"
            );
            let eng = self.eng_mut(eidx);
            let mut mo = vike_exec::ManagedOrder::new(req.clone());
            mo.created_ms = Some(now);
            eng.registry.insert(coid.clone(), mo);
            self.publish_to(
                eidx,
                Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                    client_order_id: coid.clone(),
                    ts: now,
                }),
            );
            self.publish_to(
                eidx,
                Event::OrderRejected(OrderRejected {
                    client_order_id: coid.clone(),
                    reason: format!("venue {} does not support combo orders", spec.venue).into(),
                    ts: now,
                }),
            );
            self.note(format!("combo REJECTED: venue {} declares no combo support", spec.venue));
            self.pump_client();
            // NAMES the rejected order, like `lower_submit`'s capability reject: it was minted,
            // journaled and registered (terminal), and `apply_strategy_intent` attributes only what
            // it is returned.
            return vec![coid];
        }

        // Snapshot each leg's own risk facts BEFORE reaching for the gate: `check_combo` takes
        // `leg_ctx` by `Fn`, and a closure reading the engine's `account` live would borrow the
        // engine immutably while `gate` is borrowed mutably off that same struct. A `Vec` (not a
        // map) because a combo is 2..~6 legs — the linear probe is cheaper than hashing, and this
        // is a per-order path anyway.
        let mut leg_ctxs: Vec<(String, vike_exec::RiskContext)> =
            Vec::with_capacity(spec.legs.len());
        {
            let eng = self.eng(eidx);
            // Equity is an ACCOUNT-level fact — identical for every leg — folded ONCE, and only
            // when the margin knob is armed for at least one leg (`resolved_equity` is
            // O(positions); per-order path, still not free). One-price law: resolver-priced,
            // the SAME `config.price_cfg` source the snapshot/sampler/watchdog read.
            let armed_any = spec.legs.iter().any(|l| eng.gate.limits.im_for(&l.symbol).is_some());
            // The ACCOUNT-aggregate ceiling is a per-ENGINE fact, so whether it is armed is
            // loop-invariant — read once here, beside the equity gate above and for the same
            // reason: the fold it guards is O(positions + live orders), and a deployment that wrote
            // no `max_account_exposure` must pay nothing for a lane it did not turn on. (The FOLD
            // itself is not loop-invariant; it excludes the leg's own symbol, so it runs per leg.)
            let account_ceiling_armed = eng.gate.limits.max_account_exposure.is_some();
            // ⚠ `sizing_equity`, not `resolved_equity` — the combo twin of `gate_and_register`'s
            // own line. This is the buying-power basis a combo is ADMITTED against, so
            // `RiskLimits::max_sizing_equity` applies and a lower figure can only refuse sooner.
            // Bit-identical when no ceiling is armed; the margin-CALL sweep keeps the uncapped
            // resolver, and `vike_exec::ExecutionEngine::sizing_equity`'s doc carries the asymmetry.
            let equity = if armed_any {
                eng.sizing_equity(eng.equity_seed, &self.config.price_cfg)
            } else {
                0.0
            };
            for leg in &spec.legs {
                // Σ initial margin of the open book, priced PER LEG — the exact mirror of
                // `gate_and_register`'s Phase-B fold: an UNMARKED position contributes 0 (it
                // cannot be priced — LEAN's 0-rate skip), and a marked position with NO per-symbol
                // IM override falls back to the order symbol's own `im_req`. A combo has no single
                // order symbol, so the fallback here is the im_req of THE LEG BEING PRICED —
                // which is why this fold runs per leg rather than once: two legs with different
                // rates price the same no-override position differently, exactly as two naked
                // orders on those symbols would. (Silently SKIPPING a no-override position — this
                // block's original shape — understated `margin_used` whenever the global
                // `im_requirement` is unset and margin was armed per-symbol, which is exactly what
                // `Command::SetMargin` produces: it only writes `im_by_symbol`. That admitted
                // combos whose naked legs would be denied, violating the gate's own invariant.)
                // An unarmed leg (`im_for` None) gets the single path's unarmed tuple — the
                // buying-power check is off for it and never reads these fields. `check_combo`
                // threads each admitted leg's own margin on top of this baseline itself.
                let (leg_equity, margin_used) =
                    if let Some(leg_im) = eng.gate.limits.im_for(&leg.symbol) {
                        // THE shared margin-in-use fold, resolver-priced
                        // (`resolved_margin_in_use_by` under `config.price_cfg` — the same
                        // basis as the resolver `equity` above and the single-order gate's own
                        // fold), rated PER LEG: a no-override position falls back to THIS
                        // leg's `leg_im` (the combo has no single order symbol), exactly
                        // mirroring the single-order gate — including its POOL POLICY (the
                        // liquidation law's partition): only CROSS positions consume the
                        // shared equity the combo is admitted against; an Isolated position's
                        // wallet / a Cash position's full funding never backed it, so counting
                        // them would double-charge a mixed account. All-cross accounts: filter
                        // is a no-op.
                        let used = eng.resolved_margin_in_use_by(
                            &self.config.price_cfg,
                            |(_v, s, _side), p| {
                                p.margin_mode
                                    .is_cross()
                                    .then(|| eng.gate.limits.im_for(s).unwrap_or(leg_im))
                            },
                        );
                        (equity, used)
                    } else {
                        (0.0, 0.0)
                    };
                // The leg's own position size, reused for the resolver's side-aware quote choice
                // below exactly as the single-order gate feeds its `pos`.
                let pos = eng.position_size_of(&leg.symbol, "BOTH");
                leg_ctxs.push((
                    leg.symbol.clone(),
                    vike_exec::RiskContext {
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
                        mark_price: eng
                            .resolved_position_price(
                                &eng.venue,
                                &leg.symbol,
                                pos,
                                &self.config.price_cfg,
                            )
                            .unwrap_or(0.0),
                        trading_state: eng.trading_state,
                        now_ms: now,
                        equity: leg_equity,
                        margin_used,
                        // `check_combo` re-derives each leg's own reduce_only from the PROJECTED
                        // book, so the reversing-leg margin credit `gate_and_register` computes has
                        // no single-symbol meaning here. Left 0.0 so the crossing errs strictly
                        // HIGH — the gate's own stance ("a combo must never pass a gate its naked
                        // legs would fail").
                        closing_credit: 0.0,
                        multiplier: eng.account.multiplier_of(&leg.symbol),
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
                        // ⚠ `coid` is the JUDGED order — this combo's own, minted above and not yet
                        // registered, so it excludes nothing today. It is passed rather than `""`
                        // because the producer's contract is "leave out the order being judged",
                        // and a combo that ever became re-judgeable (an amend) would need exactly
                        // this and would have no other place to learn it.
                        account_exposure_excl_order: if account_ceiling_armed {
                            eng.resolved_account_exposure_excluding(
                                &leg.symbol,
                                &coid,
                                &self.config.price_cfg,
                            )
                        } else {
                            0.0
                        },
                    },
                ));
            }
        }

        // Account-level ctx. `check_combo` takes `trading_state` + `now_ms` from HERE and lets them
        // OVERRIDE whatever `leg_ctx` returns, so a `Reducing`/`Halted` account can never be
        // laundered into `Active` by the per-leg snapshot.
        let ctx = vike_exec::RiskContext {
            trading_state: self.eng(eidx).trading_state,
            now_ms: now,
            ..Default::default()
        };
        let verdict = self.eng_mut(eidx).gate.check_combo(&req, &ctx, |sym| {
            // Total by contract. A symbol with no snapshot falls back to a default ctx whose mark
            // is 0.0, which `check_combo` DENIES as `no-mark` — the honest answer for a symbol the
            // runtime knows nothing about.
            leg_ctxs.iter().find(|(s, _)| s == sym).map(|(_, c)| *c).unwrap_or_default()
        });

        let Some(admitted) = verdict.request.filter(|_| verdict.ok) else {
            // Per-ORDER (not per-tick): a RiskGate veto is fault-adjacent, off the measured hop —
            // the same budget the single-order veto log takes in `gate_and_register`.
            tracing::warn!(
                target: "vike_core::risk",
                coid = %coid,
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
                let eng = self.eng_mut(eidx);
                if eng.collect_applied_fills {
                    eng.order_events.push(vike_exec::execution_engine::OrderEventOut {
                        venue: req.venue.clone(),
                        symbol: req
                            .combo_legs
                            .first()
                            .map(|l| l.symbol.clone())
                            .unwrap_or_default(),
                        event: vike_model::OrderLifecycle {
                            client_order_id: coid.clone(),
                            // Stamped at DELIVERY (`dispatch_order_events`) if this coid holds a
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
                eidx,
                Event::OrderDenied(vike_model::events::OrderDenied {
                    client_order_id: coid.clone(),
                    reason: verdict.reason.into(),
                    ts: now,
                }),
            );
            self.pump_client();
            // NAMES the denied order, like `lower_submit`'s RiskGate refusal: it exists (minted,
            // journaled, an `OrderDenied` published) and `apply_strategy_intent` can only attribute
            // what it is returned, so an empty list left the owner to the first-mount fallback.
            return vec![coid];
        };

        // Mirrors `ExecutionEngine::submit_order`'s tail (register + `client.submit`), open-coded
        // because the crossing above is `check_combo` — calling `submit_order` here would re-run
        // the SINGLE-order `check`, which prices the request off its SIGNED net (negative for a
        // credit structure, so every price-based limit inverts) and would burn a SECOND throttle
        // slot for one order.
        let eng = self.eng_mut(eidx);
        // ADMIT every leg symbol into this engine's venue-event scope BEFORE the order is on the
        // wire: `ExecutionEngine::on_event` gates a bare `Event::Fill` on `accepts_symbol` (the
        // account-wide-WS scoping filter), and a combo's leg fills arrive carrying LEG symbols —
        // the combo has no symbol of its own. Without this the `OrderFilled` wraps advance the
        // FSM (they route by coid) while the `Account` never folds the fills and
        // `Strategy::on_fill` never fires — the strategy is blind to its own filled combo.
        // `extra_symbols` (the Phase D multi-symbol mechanism) stays the PRIMARY admission
        // because it also lets Funding/PositionLiquidated events on the leg symbols fold — leg
        // positions are REAL positions with real funding, and a fill-only route cannot carry
        // those. Since combo gate 4, `ExecutionEngine::owns_fill_symbol` ALSO routes a bare fill
        // whose coid names a registered order and whose symbol is that order's own symbol or one
        // of its `combo_legs` — the safety net for fills, and the ONLY route that classifies the
        // venue's combo-instrument net-price print (deliberately NOT folded — a phantom position;
        // see that fn's doc + the deribit fill probe) — so the two mechanisms are complementary,
        // not alternatives. Replay-safe: the journaled `Cmd` re-runs this lowering on replay, and
        // `extra_symbols` rides the `EngineSnapshot`.
        for leg in &admitted.combo_legs {
            if !eng.accepts_symbol(&leg.symbol) {
                eng.extra_symbols.push(leg.symbol.clone());
            }
        }
        let mut mo = vike_exec::ManagedOrder::new(admitted.clone());
        mo.created_ms = Some(now);
        eng.registry.insert(coid.clone(), mo);
        eng.client.submit(&admitted);
        self.pump_client();
        vec![coid]
    }
}
