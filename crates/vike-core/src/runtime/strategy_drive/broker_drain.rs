//! The `LiveBroker` drain: an intent's symbol and venue, the strategy-tag registry, and the apply.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Resolve ONE buffered intent's target symbol against the mount's declaration.
    ///
    /// - No mount declared extra symbols (`any_mount_multi == false`, every runtime today) ⇒ the
    ///   mount's own symbol, via a single bool read. Byte-identical.
    /// - This mount declared nothing ⇒ likewise the mount's own symbol, so a single-symbol mount
    ///   is unaffected even when a SIBLING mount is multi-symbol.
    /// - This mount DID declare, and the intent named its own symbol or a declared one ⇒ that
    ///   symbol. This is the whole point of the lane.
    /// - This mount declared, and the intent named something ELSE ⇒ `None`, the order is REFUSED.
    ///
    /// The refusal is deliberate. Silently rewriting an undeclared symbol to the mount's own is
    /// exactly the bug this program exists to remove (`multi_symbol_routing.rs`): it turns "trade
    /// X" into "trade Y" with no signal. Once a mount opts in, symbol correctness is ENFORCED for
    /// it; a mount that never opted in keeps the old permissive contract, which shipped strategies
    /// depend on (they pass `""`, and live bars carry no symbol at all).
    fn resolve_intent_symbol(
        &self,
        mount_idx: usize,
        requested: Option<&str>,
        mount_symbol: &str,
    ) -> Option<String> {
        if !self.any_mount_multi {
            return Some(mount_symbol.to_string());
        }
        let declared = &self.mount_symbols[mount_idx];
        if declared.is_empty() {
            return Some(mount_symbol.to_string());
        }
        match requested {
            None => Some(mount_symbol.to_string()),
            Some(r) if r == mount_symbol => Some(mount_symbol.to_string()),
            Some(r) if declared.iter().any(|l| l.symbol == r) => Some(r.to_string()),
            Some(r) => {
                tracing::warn!(
                    mount_idx,
                    requested = r,
                    mount_symbol,
                    "refusing an order for a symbol this mount did not declare"
                );
                None
            }
        }
    }

    /// The VENUE a resolved symbol trades on for this mount.
    ///
    /// A declared leg may name a DIFFERENT venue (`MountLeg::at`) — that is what makes a
    /// cross-exchange strategy possible: an xEMM maker rests on one venue and hedges on another,
    /// so its two legs cannot share a venue by construction. Anything not declared, or declared
    /// without a venue, uses the mount's own — so every single-venue mount is byte-identical and
    /// the whole path is one bool read when no mount declared anything.
    ///
    /// Downstream is ALREADY venue-aware: `apply_intent` routes to an engine by
    /// `engine_idx_for_route_key(&req.venue)` and every adapter forwards the request verbatim. The
    /// only reason a foreign venue could not be reached was that `drain_broker` stamped the
    /// mount's venue onto every intent — which is what this resolves.
    fn resolve_intent_venue(&self, mount_idx: usize, symbol: &str, mount_venue: &str) -> String {
        if !self.any_mount_multi {
            return mount_venue.to_string();
        }
        self.leg_venue(mount_idx, symbol, mount_venue)
    }

    /// Make ONE refused intent OBSERVABLE — the denial [`Self::resolve_intent_symbol`]'s `None`
    /// stands for, emitted through the EXISTING `RiskGate`-veto mechanism rather than a second one
    /// of its own.
    ///
    /// The refusal itself is older than this method and unchanged: an opted-in mount that names a
    /// symbol it never declared gets no order. What was missing is that `continue` told NOBODY. The
    /// strategy received no callback, no event reached the bus, and nothing appeared in the
    /// operator's recent-events ring — a strategy asked for something and the platform quietly did
    /// nothing, which is the same silent-drop class the two-leg routing bug was.
    ///
    /// So this mirrors `ExecutionEngine::gate_and_register`'s veto path VERBATIM, both halves:
    ///
    /// 1. an [`vike_exec::execution_engine::OrderEventOut`] carrying
    ///    `OrderEventKind::Denied { reason }`, pushed onto the routing engine's buffer under the
    ///    same `collect_applied_fills` gate the gate's own veto capture uses (a strategy is
    ///    mounted ⇔ someone can hear it), delivered by [`Self::dispatch_order_events`];
    /// 2. an [`vike_model::events::Event::OrderDenied`] published to that engine, which the bus's
    ///    delivery log turns into a recent-events line at `drain_delivered` — the same way an
    ///    operator sees a `RiskGate` veto. Like a vetoed order, the refusal was never registered,
    ///    so the fold counts it into `dropped_unknown_coid` exactly as a denied order is.
    ///
    /// The id comes from [`Self::mint_refusal_id`], NOT the coid generator — see that method.
    /// `coid_mount` records it against the refusing mount purely so `dispatch_order_events` routes
    /// the denial back to the mount that asked, precisely, rather than falling back to the first
    /// mount on a `(venue, symbol)` pair (the requested symbol matches no mount by construction —
    /// it is the undeclared one). No fill can ever carry a refusal id: nothing was submitted.
    ///
    /// Cold path — a refusal happens at ORDER cadence at most, inside `drain_broker`, which is
    /// never on the per-message fold the `p99 < 10µs` gate measures.
    fn deny_undeclared_symbol(
        &mut self,
        mount_idx: usize,
        venue: &str,
        requested: Option<&str>,
        mount_symbol: &str,
        now: i64,
    ) {
        let requested = requested.unwrap_or_default().to_string();
        let refusal_id = self.mint_refusal_id();
        let declared = self.mount_symbols[mount_idx]
            .iter()
            .map(|l| l.symbol.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let reason = format!(
            "undeclared symbol `{requested}`: mount {mount_idx} on {venue}/{mount_symbol} declared \
             [{declared}]"
        );
        // No `tracing` call here on purpose: `resolve_intent_symbol` already warns on the refusal it
        // decided, and a second warn per refusal would only duplicate it. The channels this method
        // adds are the ones a WARN cannot reach — the strategy's own callback and the operator ring
        // — and the ring line carries the richer text (requested + mount + full declared set).
        // The REFUSING mount's own engine: this denial is that mount's order-event, so it must land
        // in the book that mount reads. The same `EngineRoute::Mount` rule the intent itself would
        // have taken had it not been refused.
        let eidx = self.route_of(EngineRoute::Mount(mount_idx), venue).unwrap_or(0);
        self.coid_mount.insert(refusal_id.clone(), mount_idx);
        {
            let eng = self.eng_mut(eidx);
            if eng.collect_applied_fills {
                eng.order_events.push(vike_exec::execution_engine::OrderEventOut {
                    venue: venue.to_string(),
                    symbol: requested,
                    event: vike_model::OrderLifecycle {
                        client_order_id: refusal_id.clone(),
                        // Stamped at DELIVERY (`dispatch_order_events`), never here — one spelling
                        // of the rule. This synthetic refusal id never reaches the tag registry
                        // (the refused intent minted no order), so it stays `None` in practice.
                        tag: None,
                        kind: vike_model::OrderEventKind::Denied { reason: reason.clone() },
                    },
                });
            }
        }
        self.publish_to(
            eidx,
            Event::OrderDenied(vike_model::events::OrderDenied {
                client_order_id: refusal_id,
                reason: reason.into(),
                ts: now,
            }),
        );
        self.dirty = true;
    }

    /// Mint → RiskGate → client for each buffered submission, then resolve buffered modifications
    /// (tag → coid) and route them. Shared by the bar path ([`drive_strategy`]) and the tick path
    /// ([`drive_strategy_tick`]) so all order construction flows through the ONE hot-path literal
    /// (piece 1) and the ONE live path. Submits are processed before modifications so a submit-then-modify
    /// within the SAME handler call resolves against the tag registered this call.
    ///
    /// `venue`/`symbol` are the DRAIN SERIES — whichever series this dispatch was about. They are
    /// the default target of a symbol-less intent ([`Self::resolve_intent_symbol`]); they are NOT
    /// the tag-registry key, which is the MOUNT's own series ([`Self::tag_key`]).
    ///
    /// `mount_idx` is the buffering mount's index into `self.mounts` / `self.mount_states` — every
    /// call site already has it on hand (it is how the mount was found/taken in the first place).
    /// Portfolio-observer PR-4 T5: while that mount is [`MountState::Pending`] (only reachable when
    /// [`CoreConfig::readiness_gate`] is on — every mount is `Ready` immediately otherwise), the
    /// ENTIRE buffered `ctx` — submissions, brackets, modifications, cancels, conditionals,
    /// mass_cancel — is DISCARDED right here, before any of it reaches `apply_intent`. The strategy
    /// hook that filled `ctx` already ran at the call site (warmup/observation is unaffected); only
    /// the order OUTPUT of this one dispatch is dropped. This is a single choke point rather than a
    /// check duplicated at each of the six call sites, and it leaves NO residue by construction: `ctx`
    /// is a fresh, owned `LiveBroker` built new by every call site (never a persisted field), so an
    /// early return here simply drops its `Vec`s — there is no outbox to "clear" and nothing can
    /// accumulate across ticks regardless of how many consecutive dispatches stay Pending.
    pub(crate) fn drain_broker(
        &mut self,
        ctx: LiveBroker,
        venue: &str,
        symbol: &str,
        now: i64,
        mount_idx: usize,
    ) {
        // Discard this mount's buffered intents when it is either not yet priced (readiness gate,
        // portfolio-observer PR-4 T5) OR budget-latched liquidate-only (steal/core-per-mount-budget)
        // — in both cases the strategy hook already ran at the call site; only its ORDER OUTPUT is
        // dropped. `mount_latched` is all-false when no budget is active, so this stays byte-identical
        // to the readiness-only check for a budget-free runtime.
        if self.mount_states[mount_idx] == MountState::Pending || self.mount_latched[mount_idx] {
            return; // ctx (and every buffered intent in it) is dropped here — nothing reaches the engine
        }
        for s in ctx.submissions {
            let Some(sym) = self.resolve_intent_symbol(mount_idx, s.symbol.as_deref(), symbol)
            else {
                // refused: a declared-multi mount named an UNDECLARED symbol. The refusal is
                // unchanged (no order) — it is now VISIBLE: the strategy hears a Denied lifecycle
                // event and the operator sees an `OrderDenied` line. See `deny_undeclared_symbol`.
                self.deny_undeclared_symbol(mount_idx, venue, s.symbol.as_deref(), symbol, now);
                continue;
            };
            let ven = self.resolve_intent_venue(mount_idx, &sym, venue);
            let req = OrderRequest {
                client_order_id: String::new(),
                venue: ven.clone(),
                symbol: sym.clone(),
                side: s.side,
                qty: s.qty,
                order_type: s.order_type,
                price: s.price,
                reduce_only: s.reduce_only,
                ts: now,
                ..Default::default()
            };
            let coids =
                self.apply_strategy_intent(OrderIntent::Submit(Box::new(req)), now, mount_idx);
            if let (Some(tag), Some(coid)) = (s.tag, coids.first()) {
                // Keyed by the BUFFERING mount's index first — see `CoreThread::strategy_tags` for
                // why a mount-blind key let one mount's `cancel_tagged("bid")` hit another mount's
                // resting quote — and on the MOUNT's own series, never this drain's: see
                // `Self::tag_key`. A tagged submission carries no symbol (`HftBroker` contract, and
                // `broker.rs`'s producer-side `tagged_orders_never_carry_a_symbol` pins the closed
                // producer set), so it always resolves to the drain's `symbol`; the ORDER is
                // therefore on the drain's instrument while the KEY is the mount's, which is
                // exactly what makes one tag name one quote across every lane.
                let key = self.tag_key(mount_idx, venue, symbol, tag.as_str());
                self.strategy_tags.insert(key, coid.clone());
            }
        }
        for b in ctx.brackets {
            let Some(sym) = self.resolve_intent_symbol(mount_idx, b.symbol.as_deref(), symbol)
            else {
                self.deny_undeclared_symbol(mount_idx, venue, b.symbol.as_deref(), symbol, now);
                continue;
            };
            let ven = self.resolve_intent_venue(mount_idx, &sym, venue);
            let spec = BracketSpec {
                venue: ven,
                symbol: sym,
                side: b.side,
                qty: b.qty,
                entry_price: b.entry_price,
                stop_loss: b.stop_loss,
                take_profit: b.take_profit,
            };
            self.apply_strategy_intent(OrderIntent::Bracket(Box::new(spec)), now, mount_idx);
        }
        for a in ctx.modifications {
            let key = self.tag_key(mount_idx, venue, symbol, &a.tag);
            if let Some(coid) = self.strategy_tags.get(&key).cloned() {
                self.apply_strategy_intent(
                    OrderIntent::Modify {
                        client_order_id: coid,
                        new_qty: a.new_qty,
                        new_price: a.new_price,
                    },
                    now,
                    mount_idx,
                );
            }
        }
        for tag in ctx.cancels {
            let key = self.tag_key(mount_idx, venue, symbol, &tag);
            if let Some(coid) = self.strategy_tags.get(&key).cloned() {
                // ROUTINE, and this is the ONE site in the runtime where that is genuinely known:
                // `HftBroker::cancel_tagged` pulls ONE of a strategy's own tagged quotes, which is
                // ladder churn by construction — a maker suppressing a side, or the cancel half of
                // a requote it is about to re-place. A venue metering its cancels may hold this
                // back under pressure (it comes back as a NON-terminal cancel-reject and the
                // strategy re-offers on its next tick), which is exactly the churn a flatten
                // reserve is protected FROM.
                //
                // ⚠ `Broker::mass_cancel()` is NOT classified with it, further down: "pull ALL my
                // quotes" is the adverse-selection hammer, it is not retried on a shed, and it
                // stays at the flatten-safe default.
                self.apply_strategy_intent_with_cancel_intent(
                    OrderIntent::Cancel(coid),
                    now,
                    mount_idx,
                    CancelIntent::Routine,
                );
            }
        }
        for c in ctx.conditionals {
            let Some(cond_sym) = self.resolve_intent_symbol(mount_idx, c.symbol.as_deref(), symbol)
            else {
                self.deny_undeclared_symbol(mount_idx, venue, c.symbol.as_deref(), symbol, now);
                continue;
            };
            let cond_ven = self.resolve_intent_venue(mount_idx, &cond_sym, venue);
            self.apply_strategy_intent(
                OrderIntent::ArmConditional(ConditionalIntent {
                    venue: cond_ven,
                    symbol: cond_sym,
                    side: c.side,
                    qty: c.qty,
                    price: c.price,
                    trail: c.trail,
                    // strategies arm on the default (Last) lane today — the portable Broker
                    // surface exposes no trigger-source verb; None keeps this path byte-identical
                    trigger_by: None,
                }),
                now,
                mount_idx,
            );
        }
        if ctx.mass_cancel {
            // ⚠ The MOUNT's series, NEVER the drain's — the same rule `tag_key` states at length,
            // and the last verb that was still keyed the other way.
            //
            // `Broker::mass_cancel` is documented as "cancel ALL of THIS ENGINE's live orders", and
            // `apply/exit.rs`'s `(Some(v), sym)` arm makes the VENUE load-bearing: it resolves that
            // venue's engine and calls `ExecutionEngine::mass_cancel` on the whole thing, using the
            // symbol only to scope held exits and conditional books. So the venue stamped here
            // decides which book gets pulled.
            //
            // `drain_broker`'s `venue`/`symbol` are the DRAIN SERIES, which its own doc says are not
            // the mount's: the fill and order-event lanes dispatch the FILL's `(venue, symbol)`. A
            // maker that calls `mass_cancel()` from inside `on_fill` — the canonical use, pulling
            // quotes on adverse selection — therefore pulled quotes on whichever venue the fill
            // arrived from. On a cross-venue mount that is routinely the HEDGE venue: its own quotes
            // stayed live at the maker venue while an unrelated book was cleared.
            //
            // Byte-identical for every single-symbol mount, whose drain series IS its own on every
            // lane (`tag_key`'s doc enumerates why). The `map_or` fallback covers a transiently-taken
            // mount slot, which no call site can reach.
            let (mc_venue, mc_symbol) = self.mounts[mount_idx]
                .as_ref()
                .map_or((venue, symbol), |m| (m.venue.as_str(), m.symbol.as_str()));
            self.apply_strategy_intent(
                OrderIntent::MassCancel {
                    venue: Some(mc_venue.to_string()),
                    symbol: Some(mc_symbol.to_string()),
                    account: None,
                },
                now,
                mount_idx,
            );
        }
        self.dirty = true;
    }

    /// ONE tag's key into `strategy_tags` — `{mount_idx}|{mount_venue}|{mount_symbol}|{tag}`.
    ///
    /// ## ⚠ The MOUNT's series, NEVER the drain's — the insert and BOTH lookups, together
    ///
    /// [`Self::drain_broker`] runs on whichever series dispatched, and that is NOT always the
    /// mount's own: the bar and tick lanes dispatch a DECLARED LEG's series, and the fill and
    /// order-event lanes dispatch the FILL's / the ORDER's `(venue, symbol)`. The key used to be
    /// built from those drain ARGUMENTS, so a tagged quote placed from one lane was filed under a
    /// key a `cancel_tagged` from another lane never looks up. The failure mode is the one this
    /// program's own plan called worse than the netting bug: the lookup finds nothing, `drain_broker`
    /// simply does not act, and a REAL quote is left resting at the venue with no error, no event
    /// and no ring line. `vike_mm::xemm`'s `strategy_impl` module doc carries the per-lane table
    /// this was found through; its hedge-fill arm still emits no tagged SUBMIT, but for the
    /// EMISSION reason that survives here (a symbol-less submit still resolves to the drain's
    /// series) rather than for the registry reason, which this method removes.
    ///
    /// Keying on the mount removes the second dimension the strategy could not see anyway:
    /// `HftBroker::submit_limit_tagged`/`modify_tagged`/`cancel_tagged` name NO symbol, so from the
    /// strategy's side a tag has always been a MOUNT-scoped name. The drain-keyed registry gave it a
    /// hidden per-series dimension that no verb could address.
    ///
    /// The insert and both lookups go through this ONE method deliberately. Moving only the insert —
    /// the obvious half-fix — reproduces the silent no-op above exactly, which is why
    /// `crates/vike-core/tests/wiring/multi_symbol_reads.rs` drives the tag in BOTH directions (place on
    /// the mount's series and cancel from the leg's, and the reverse): each direction fails if only
    /// one side moved.
    ///
    /// ⚠ **One tag names ONE order per mount.** Re-using a tag before cancelling overwrites the
    /// entry and orphans the previous order (it keeps resting, and no tag can address it any more).
    /// That was already true for a repeat on the SAME series; keying on the mount widens it to a
    /// repeat across two LEGS. A mount quoting both legs must use per-leg tags — which is also the
    /// only shape the symbol-less `HftBroker` verbs can express.
    ///
    /// Byte-identical for every single-symbol mount: on every lane its drain series IS its own
    /// (`drive_strategy`/`drive_strategy_tick` match `m.symbol == symbol`; `drive_strategy_feed_status`/
    /// `_flow`/`_params` require the pair; `_mark`/`_reference_quote`/`drive_schedule` drain the
    /// mount's own by construction; and an undeclared mount's orders — hence its fills and order
    /// events — can only ever carry its own symbol, since `resolve_intent_symbol` returns
    /// `mount_symbol` for it unconditionally). Cold path: called only when a tagged verb was actually
    /// buffered, so the per-market-message dispatch that buffers nothing pays nothing and the
    /// `p99 < 10µs` fold is untouched. `drain_venue`/`drain_symbol` are the fallback for a
    /// transiently-taken mount slot, which no call site can reach (each restores the mount before
    /// draining).
    fn tag_key(
        &self,
        mount_idx: usize,
        drain_venue: &str,
        drain_symbol: &str,
        tag: &str,
    ) -> String {
        let (venue, symbol) = self.mounts[mount_idx]
            .as_ref()
            .map_or((drain_venue, drain_symbol), |m| (m.venue.as_str(), m.symbol.as_str()));
        format!("{mount_idx}|{venue}|{symbol}|{tag}")
    }

    /// The STRATEGY's own tag for `coid` under `mount_idx`, or `None` if that mount has no live tag
    /// entry naming this order — the reverse of [`Self::tag_key`], and the stamp that gives
    /// [`vike_model::OrderLifecycle::tag`] its value.
    ///
    /// ## Why the lookup is VALUE-matched, and why that is the whole safety argument
    ///
    /// `strategy_tags` maps `{mount_idx}|{venue}|{symbol}|{tag}` → coid, and a re-submit under the
    /// same tag OVERWRITES the entry (`drain_broker`'s insert). So the question "which tag names
    /// this order" is only answerable by matching the coid on the VALUE side. Doing it that way is
    /// not merely convenient — it is what makes a LATE terminal harmless: a maker that submits under
    /// `"bid"` again before the previous order's cancel-ack arrives has an entry pointing at the NEW
    /// coid, the old coid matches nothing, this returns `None`, and the strategy is told about an
    /// order it no longer tracks under a tag that now belongs to a LIVE quote. A key-side lookup
    /// (build the key, compare nothing) would instead hand the strategy its own live tag and make it
    /// cancel a quote it had just placed.
    ///
    /// The mount's own `(venue, symbol)` builds the prefix — the same rule [`Self::tag_key`] states
    /// at length — and the tag is the REMAINDER after it, taken with `strip_prefix` rather than by
    /// splitting on `|`, so a tag containing a `|` round-trips.
    ///
    /// Cost: a scan of `strategy_tags`, which holds one entry per LIVE tag per mount (two for a
    /// single-quote maker, `2 × levels` for a laddered one). Called only from the two OCCASIONAL
    /// order-outcome lanes at ORDER cadence — never from the per-market-message fold — so the
    /// `p99 < 10µs` hop is untouched.
    pub(crate) fn tag_for_coid(&self, mount_idx: usize, coid: &str) -> Option<String> {
        let m = self.mounts[mount_idx].as_ref()?;
        let prefix = format!("{mount_idx}|{}|{}|", m.venue, m.symbol);
        self.strategy_tags
            .iter()
            .find(|(k, v)| v.as_str() == coid && k.starts_with(&prefix))
            .and_then(|(k, _)| k.strip_prefix(&prefix).map(str::to_string))
    }

    /// Drop `mount_idx`'s tag entry for `coid` — called ONLY once that order is terminal, so the
    /// registry holds live orders and nothing else.
    ///
    /// Unlike [`Self::note_terminal_coid`], this erases IMMEDIATELY and needs no linger window: the
    /// linger exists so a late fill can still be ATTRIBUTED to a mount, and attribution reads
    /// `coid_mount`, never this map. What this map answers is "which order does the tag `"bid"` name
    /// right now", and after a terminal the honest answer is "none" — keeping the dead entry is what
    /// let `modify_tagged("bid")` resolve a terminal coid and be swallowed by
    /// `ExecutionEngine::modify_order`'s not-modifiable early return, with no error, no event and no
    /// ring line. The removal is value-guarded through [`Self::tag_for_coid`], so a late terminal
    /// can never retire an entry that has since been re-pointed at a live order.
    pub(crate) fn retire_tag_for_coid(&mut self, mount_idx: usize, coid: &str) -> Option<String> {
        let tag = self.tag_for_coid(mount_idx, coid)?;
        let key = self.tag_key(mount_idx, "", "", &tag);
        self.strategy_tags.shift_remove(&key);
        Some(tag)
    }

    /// Journal a mounted strategy's ALREADY-RESOLVED order intent write-ahead of `apply_intent`
    /// (portfolio-observer PR-5 T2) — the `drain_broker` boundary's twin of `dispatch()`'s `Cmd`
    /// gate (~:954-958): same idiom, journal BEFORE the fold that mints coids. `self.journal.is_some()`
    /// gates the whole hook so journaling off stays byte-identical to today (every existing
    /// drain_broker/readiness/journal_wiring test is unaffected). `intent` is journaled AFTER the
    /// tag→coid resolution `drain_broker`'s modify/cancel call sites already did, so replay never
    /// needs the `strategy_tags` registry — it re-applies the same resolved `OrderIntent` a
    /// `Command::Order` `Cmd` would carry. `journaled_since_snap` is bumped here too — the SAME
    /// field `dispatch()`'s tail cadence check (~:1165) reads — so a strategy-heavy session still
    /// feeds the `snapshot_every` cadence; the actual `write_snap` call fires at the next journaled
    /// (Event/Command/Watchdog) dispatch, since `drain_broker` itself runs from within a
    /// BarClose/Quote/Trade/Book fold that is not in that `journaled` match arm — the counter is
    /// not lost, just possibly observed one dispatch later. Cold path (`drain_broker` is off the
    /// per-message hot fold, so this never touches the `p99 < 10µs` gate): the mount_id is cloned
    /// rather than borrowed to sidestep holding `self.journal.as_mut()` (mutable) and
    /// `self.mount_ids[mount_idx]` (immutable) at once.
    ///
    /// ⚠ Visible to the runtime's white-box tests only: lowering through here SKIPS `drain_broker`'s
    /// readiness, latch and undeclared-symbol gates, so no production caller outside the drain
    /// may use it.
    pub(in crate::runtime) fn apply_strategy_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        mount_idx: usize,
    ) -> Vec<String> {
        self.apply_strategy_intent_with_cancel_intent(
            intent,
            now,
            mount_idx,
            CancelIntent::Unspecified,
        )
    }

    /// [`Self::apply_strategy_intent`], classifying every cancel it lowers (see [`CancelIntent`]).
    /// The classification reaches the venue and is journaled NOWHERE — see
    /// [`Self::apply_intent_with_cancel_intent`] for why it rides beside the [`OrderIntent`] rather
    /// than inside it, and why that keeps the write-ahead record's shape unchanged.
    fn apply_strategy_intent_with_cancel_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        mount_idx: usize,
        cancel_intent: CancelIntent,
    ) -> Vec<String> {
        if let Some(journal) = self.journal.as_mut() {
            let mid = self.mount_ids[mount_idx].clone();
            journal.append_strategy_submit(now, &mid, &intent).expect("journal append");
            self.journaled_since_snap += 1;
        }
        // ⚠ ROUTED BY THE MOUNT, not by the intent's venue string. This is the one choke point every
        // strategy-minted intent lowers through, so it is the one place the mount's declared
        // ACCOUNT can be turned into an engine — and turning it into one HERE is what makes a
        // misroute unwritable rather than merely untested: there is no venue string anywhere
        // downstream that could name the wrong account, because the account never becomes a string.
        // `EngineRoute::Mount` still defers to the payload for a leg declared on a FOREIGN venue;
        // see that enum.
        let coids =
            self.apply_intent_routed(intent, now, cancel_intent, EngineRoute::Mount(mount_idx));
        // steal/core-per-mount-budget ATTRIBUTION: tag every coid this mount's intent minted back to
        // the mount, so its fills fold into the mount's ledger and its resting orders can be canceled
        // scoped-to-this-mount on a budget breach. `apply_intent` returns the coids of orders it
        // SUBMITTED (empty for modify/cancel/arm), so nothing is over-tagged. Cold path (order
        // cadence, off the p99 fold); the map is written even with no budget (attribution is
        // always-on + read-only — it changes no fold decision, so behavior stays byte-identical).
        // `own_coid` also records the ownership for the next boot when the ownership file is on.
        for c in &coids {
            self.own_coid(c.clone(), mount_idx);
        }
        coids
    }

    /// The mount that MINTED `coid` — the EXACT strategy-callback routing key for a fill or an
    /// order event (multi-mount correctness, bug B).
    ///
    /// `coid_mount` is written at [`Self::apply_strategy_intent`], i.e. the one choke point where a
    /// mount's buffered intent lowers into `apply_intent`, so it already records precisely which
    /// mount owns each server-minted coid. Routing by it replaces the previous FIRST-MATCH scan over
    /// `(venue, symbol)`, which was not a routing key at all once two mounts shared a symbol: only
    /// the first ever received `on_fill`/`on_order_event`, and the second silently never heard about
    /// its own orders. (The bar lane has always matched the full `(venue, symbol, interval)` triple;
    /// the fill/order-event lanes did not.)
    ///
    /// `None` means "no mount minted this coid" — a manual operator ticket, a margin-call
    /// liquidation, a reconcile-synthesised or adopted order, a settlement — or that the minting mount
    /// is gone (unmounted: its `coid_mount` entry outlives the slot) or its slot is transiently taken
    /// (mid another hook dispatch, which no call site reaches). Callers then deliver to NO mount
    /// (decision 0116: an event no mount caused is not a strategy's to answer); there is no
    /// `(venue, symbol)` fallback and no sole-mount exception.
    /// A pure map lookup at order/fill cadence; the per-message fold never calls it.
    pub(crate) fn mount_for_coid(&self, coid: &str) -> Option<usize> {
        let idx = *self.coid_mount.get(coid)?;
        self.mounts.get(idx).and_then(|slot| slot.as_ref()).map(|_| idx)
    }
}
