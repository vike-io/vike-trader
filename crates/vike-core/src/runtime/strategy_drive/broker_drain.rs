//! The `LiveBroker` drain: an intent's symbol and venue, the strategy-tag registry, and the apply.

use super::*;
use vike_execution::risk::MountLeverageLeg;
// The reason is the `Denied` event's text when a tagged submit is refused while a restored order still
// holds its tag (`CoreThread::restored_tag_holds`); vike-venues spells it, beside the window.
use vike_venues::venue_restore::{RESTORED_TAG_DENY_REASON, RESTORED_TAG_DENY_WINDOW_MS};

/// How the LEVERAGE judge keys a row of a mount's ledger: `None` its own row, else a leg's venue, account
/// and symbol (`vike_execution::MountAttributionRow`'s key).
type LeverageRowKey =
    Option<(String, Option<vike_model::accounts::account_keys::AccountLabel>, String)>;

/// One leg of `side` · `qty` at its own `price` (`None`: a market order): a single order as the leverage
/// judge reads it.
fn one_leg(side: i32, qty: f64, price: Option<f64>) -> [MountLeverageLeg; 1] {
    [MountLeverageLeg { side, quantity: qty, price, ..Default::default() }]
}

/// The words a strategy's order list naming its own leg ids is refused in ([`order_list_refusal`]).
const STRATEGY_NAMED_LEG_IDS_REFUSAL: &str = "a strategy's order list may not name its own leg ids: \
     the core mints every strategy order's id so its strategy travels on it";

/// Why a strategy's order list is refused WHOLE at the drain, or `None`: the first leg whose lifetime
/// or terms the submit lane's two rules refuse (`vike_model::lifetime_refusal`, judged at `now`, then
/// `vike_model::order_terms_refusal`), named by its index; else links `vike_model::build_order_list`
/// cannot resolve, in its words. A leg carries no expiry, release or offset, so a `gtd` leg is refused
/// here; a trailing leg is judged with its trail (`vike_model::OrderListLeg::trail`, its activation the
/// trigger). Order cadence, never the per-message fold.
///
/// A list whose legs NAME their own order client ids is refused first, before anything is minted: the core mints every
/// strategy order's id so the strategy's code travels on it, and an id the strategy chose would carry none, so a late
/// fill on it could not name its strategy (an operator's list keeps its retry-safe named ids; it never reaches here).
fn order_list_refusal(spec: &vike_model::OrderList, now: i64) -> Option<String> {
    if spec.legs.iter().any(|l| l.order_client_id.is_some()) {
        return Some(STRATEGY_NAMED_LEG_IDS_REFUSAL.to_string());
    }
    let leg = spec.legs.iter().enumerate().find_map(|(i, l)| {
        vike_model::lifetime_refusal(&l.order_type, l.time_in_force, None, None, now)
            .or_else(|| {
                vike_model::order_terms_refusal(
                    &l.order_type,
                    l.price,
                    l.trigger_price,
                    l.trail,
                    None,
                    None,
                    None,
                )
            })
            .map(|why| format!("leg {i}: {why}"))
    });
    // the order client ids are the runtime's to mint; placeholders suffice to judge the links
    leg.or_else(|| vike_model::build_order_list(spec, &vec![String::new(); spec.legs.len()]).err())
}

/// Whether a strategy's intent names an ACCOUNT (a leg named by its account, decision 0293): a submit, a
/// bracket, an order list or a core-held arm carrying one. One match per intent, order cadence.
fn intent_names_an_account(intent: &OrderIntent) -> bool {
    match intent {
        OrderIntent::Submit(r) => r.account.is_some(),
        OrderIntent::Bracket(s) => s.account.is_some(),
        OrderIntent::OrderList(s) => s.account.is_some(),
        OrderIntent::ArmConditional(c) => c.account.is_some(),
        _ => false,
    }
}

/// Why an order on `requested` is refused on a mount that trades `own` alone: it would otherwise
/// have traded `own` (decision 0277's item 6: another symbol needs a mount that declares it).
fn other_symbol_reason(own: &str, requested: &str) -> String {
    format!(
        "this mount trades {own} only: an order on {requested} is refused, never sent on {own}; a \
         strategy that trades {requested} too declares it (its `symbols`), which mounts it as a leg"
    )
}

/// Why a strategy's release cannot be held, or `None`: its price condition NAMES a venue (decision
/// 0267's 2.13). The simulator names an instrument by its symbol alone, so a backtest could not read it;
/// a strategy reads another venue through its own reference leg. Three `Option` tests per buffered order.
fn strategy_release_venue_refusal(release: Option<vike_model::OrderRelease>) -> Option<String> {
    let c = release?.price?;
    let venue = c.venue?;
    Some(format!(
        "a strategy's price condition reads its own venue only: this one names {venue} for {} \
         (a strategy reads another venue through its own reference leg)",
        c.symbol
    ))
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Resolve ONE buffered intent's target symbol against the mount's declaration.
    ///
    /// - This mount declared no extra symbols (every mount when `any_mount_multi` is false, and every
    ///   mount whose strategy declares no `symbols`) ⇒ the mount's own symbol for an intent that
    ///   names none (`""`) or names it;
    ///   one naming ANOTHER symbol ⇒ `None`, REFUSED (a mount that declares nothing trades its own
    ///   and nothing else; live multi-symbol mounts lift this by declaring).
    /// - This mount DID declare, and the intent named its own symbol or a declared one ⇒ that
    ///   symbol. This is the whole point of the lane.
    /// - This mount DID declare, and the intent named nothing (`""`) ⇒ the MOUNT's own symbol
    ///   (decision 0277's rule 1), whichever series dispatched: on a leg's bar or fill the drain series
    ///   is the leg's, and `""` never meant the leg.
    /// - This mount declared, and the intent named something ELSE ⇒ `None`, the order is REFUSED.
    ///
    /// The refusal is deliberate. Silently rewriting a symbol to the mount's own is exactly the bug
    /// this program exists to remove (`multi_symbol_routing.rs`): it turns "trade X" into "trade Y"
    /// with no signal, which the order language's `Tactics::on` made one call away. Shipped
    /// strategies name `""` or the mount's own symbol (a live bar carries its series' symbol, and
    /// `crates/vike-strategy/src/file/params_check.rs`'s `misrouted_params` refuses a params symbol
    /// that is not the mount's), so they route as before; an `Option` test and, for an intent that
    /// names one, one string compare.
    ///
    /// `drain_symbol` is the DRAIN series' symbol, the mount's own on a mount that declared nothing
    /// (`tag_key`'s argument); on a declared mount the own symbol comes from its slot (the drain's for
    /// a transiently-taken slot, which no call site reaches).
    fn resolve_intent_symbol(
        &self,
        mount_index: usize,
        requested: Option<&str>,
        drain_symbol: &str,
    ) -> Option<String> {
        if !self.any_mount_multi || self.mount_symbols[mount_index].is_empty() {
            return match requested {
                Some(r) if r != drain_symbol => {
                    tracing::warn!(
                        mount_index,
                        requested = r,
                        mount_symbol = drain_symbol,
                        "refusing an order for another symbol than the mount's own"
                    );
                    None
                }
                _ => Some(drain_symbol.to_string()),
            };
        }
        let declared = &self.mount_symbols[mount_index];
        let own = self.mounts[mount_index].as_ref().map_or(drain_symbol, |m| m.symbol.as_str());
        match requested {
            None => Some(own.to_string()),
            Some(r) if r == own || declared.iter().any(|l| l.name == r) => Some(r.to_string()),
            Some(r) => {
                tracing::warn!(
                    mount_index,
                    requested = r,
                    mount_symbol = own,
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
    /// `engine_index_for_route_key(&req.venue)` and every adapter forwards the request verbatim. The
    /// only reason a foreign venue could not be reached was that `drain_broker` stamped the
    /// mount's venue onto every intent — which is what this resolves.
    ///
    /// The mount's OWN symbol, unless a leg declares it on another venue, trades on the mount's own
    /// venue whichever series dispatched: `""` is the mount's own (rule 1), so its venue is too. Every
    /// mount whose drain series is its own (each undeclared one) reads the same string as before.
    fn resolve_intent_venue(&self, mount_index: usize, symbol: &str, drain_venue: &str) -> String {
        if !self.any_mount_multi {
            return drain_venue.to_string();
        }
        let own_venue = self.mounts[mount_index]
            .as_ref()
            .filter(|m| m.symbol == symbol)
            .map_or(drain_venue, |m| m.venue.as_str());
        self.leg_venue(mount_index, symbol, own_venue)
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
    /// So this mirrors `Engine::gate_and_register`'s veto path VERBATIM, both halves:
    ///
    /// 1. an [`vike_execution::execution_engine::OrderEventOut`] carrying
    ///    `OrderEventKind::Denied { reason }`, pushed onto the routing engine's buffer under the
    ///    same `collect_applied_fills` gate the gate's own veto capture uses (a strategy is
    ///    mounted ⇔ someone can hear it), delivered by [`Self::dispatch_order_events`];
    /// 2. an [`vike_model::events::Event::OrderDenied`] published to that engine, which the bus's
    ///    delivery log turns into a recent-events line at `drain_delivered` — the same way an
    ///    operator sees a `RiskGate` veto. Like a vetoed order, the refusal was never registered,
    ///    so the fold counts it into `events_dropped_order_unknown` exactly as a denied order is.
    ///
    /// The id comes from [`Self::mint_refusal_id`], NOT the order client id generator — see that method.
    /// `order_client_id_mount` records it against the refusing mount purely so `dispatch_order_events` routes
    /// the denial back to the mount that asked, precisely, rather than falling back to the first
    /// mount on a `(venue, symbol)` pair (the requested symbol matches no mount by construction —
    /// it is the undeclared one). No fill can ever carry a refusal id: nothing was submitted.
    ///
    /// Cold path — a refusal happens at ORDER cadence at most, inside `drain_broker`, which is
    /// never on the per-message fold the `p99 < 10µs` gate measures.
    ///
    /// `tag` is the refused submit's own (`None` for a verb that carries none), so a strategy that
    /// files its orders by tag (the order language) hears which order ended.
    fn deny_undeclared_symbol(
        &mut self,
        mount_index: usize,
        venue: &str,
        (requested, tag): (Option<&str>, Option<String>),
        mount_symbol: &str,
        now: i64,
    ) {
        let requested = requested.unwrap_or_default().to_string();
        let reason = if self.any_mount_multi && !self.mount_symbols[mount_index].is_empty() {
            let declared = self.mount_symbols[mount_index]
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(",");
            // the mount's own symbol, not the drain's: on a leg's dispatch the drain is the leg
            let own = self.mounts[mount_index].as_ref().map_or(mount_symbol, |m| m.symbol.as_str());
            format!(
                "undeclared symbol `{requested}`: mount {mount_index} on {venue}/{own} declared \
                 [{declared}]"
            )
        } else {
            other_symbol_reason(mount_symbol, &requested)
        };
        // No `tracing` call here on purpose: `resolve_intent_symbol` already warns on the refusal it
        // decided, and a second warn per refusal would only duplicate it. The channels this method
        // adds are the ones a WARN cannot reach — the strategy's own callback and the operator ring
        // — and the ring line carries the richer text (requested + mount + full declared set).
        self.deny_intent(mount_index, venue, requested, tag, reason, now);
    }

    /// Refuse ONE buffered intent in words, before anything is minted or journaled: the lane
    /// [`Self::deny_undeclared_symbol`] documents (a `Denied` the refusing mount hears under a
    /// [`Self::mint_refusal_id`] id, and an `OrderDenied` on the operator's ring), shared by every drain
    /// refusal that has a reason to say. `tag` is the refused submit's own, carried ahead of delivery as
    /// [`Self::deny_restored_tag`] does, so a maker keying its side on the tag hears which quote never
    /// happened. Order cadence, never the per-message fold.
    #[cold]
    fn deny_intent(
        &mut self,
        mount_index: usize,
        venue: &str,
        symbol: String,
        tag: Option<String>,
        reason: String,
        now: i64,
    ) {
        self.deny_intent_as(mount_index, venue, symbol, tag, reason, None, now);
    }

    /// [`Self::deny_intent`] for a refusal whose catalog id is known (`vike_model::events::refusal_id_of`): the
    /// mount's leverage cap, the gate's own lane.
    #[cold]
    fn deny_intent_as(
        &mut self,
        mount_index: usize,
        venue: &str,
        symbol: String,
        tag: Option<String>,
        reason: String,
        catalog_id: Option<compact_str::CompactString>,
        now: i64,
    ) {
        let refused_order_client_id = self.mint_refusal_id();
        // The REFUSING mount's own engine: this denial is that mount's order-event, so it must land
        // in the book that mount reads. The same `EngineRoute::Mount` rule the intent itself would
        // have taken had it not been refused.
        let engine_index = self.route_of(EngineRoute::Mount(mount_index), venue).unwrap_or(0);
        self.order_client_id_mount.insert(refused_order_client_id.clone(), mount_index);
        let denied = vike_model::events::OrderDenied {
            order_client_id: refused_order_client_id.clone(),
            reason: reason.as_str().into(),
            timestamp: now,
            refusal_id: catalog_id,
        };
        self.note_denied(engine_index, &denied, Some((venue, &symbol)));
        {
            let engine = self.engine_mut(engine_index);
            if engine.collect_applied_fills {
                engine.order_events.push(vike_execution::execution_engine::OrderEventOut {
                    venue: venue.to_string(),
                    symbol,
                    event: vike_model::OrderEvent {
                        order_client_id: refused_order_client_id,
                        // A tag is stamped at DELIVERY (`dispatch_order_events`) from the registry,
                        // which cannot name a refusal id, and a tag already on the event is kept.
                        tag,
                        kind: vike_model::OrderEventKind::Denied { reason },
                    },
                });
            }
        }
        self.publish_to(engine_index, Event::OrderDenied(denied));
        self.dirty = true;
    }

    /// Whether a tagged submit under `tag` must wait because the entry it would overwrite names an
    /// order a PREVIOUS session left resting that no reconcile pass has judged yet.
    ///
    /// After a restart [`Self::restore_owned_order`] files the old order's tag back at boot, but the
    /// order only enters the registry when the first pass adopts it. A submit in that window
    /// overwrites the entry and orphans the order, which no tag can address any more
    /// ([`Self::note_tag_overwrite`] cannot even see it, the registry does not hold it yet). The
    /// refusal ends by itself at the pass: ADOPT, GONE or FILLED spends the `restored_orders` row
    /// (GONE also retires the tag), and the maker re-quotes. A pass that finds a PLAIN order
    /// unlisted (UNPROVEN, `restore_unproven`: the row stays for a later pass, no mount is told)
    /// ends it too, as the old GONE did; a stop, or an entry that records no kind, may rest where the
    /// client does not look, so it stays refused until the window below ends.
    ///
    /// Answers `false` (the submit proceeds, as before this check) when:
    /// - nothing is restored (one `is_empty()`, the whole cost of a tagged submit on a normal boot);
    /// - the tag does not name a restored order;
    /// - no pass can judge the entry: it carries no venue and symbol facts, or the venue gate
    ///   (`CoreConfig::restore_venue_gate`, by default the declared `restores_orders` table: open
    ///   for the venues whose row a demo smoke has enabled, closed for the rest; closed for every
    ///   venue on a box whose operator refused reconcile, where no pass will ever come) is closed
    ///   for the entry's venue, in which case the entry waits for ever and a refusal would too;
    /// - [`RESTORED_TAG_DENY_WINDOW_MS`] has passed since the first refusal: a pass that never
    ///   comes (a fetch that keeps failing) must not silence the mount, so the check fails open
    ///   to the old behaviour. A refusal clock is global, not per entry: a runtime mount that takes
    ///   over a pending entry later than the window gets no protection.
    ///
    /// ⚠ **The fail-open is also a deadline the first pass has to beat**: a pass that arrives
    /// after it finds the refusal lifted and the maker's second quote already resting beside the
    /// restored one. The first pass runs after `config.reconcile_startup_delay_ms`, which the
    /// settings loader therefore bounds at half the window
    /// (`vike_venues::venue_restore::MAX_RECONCILE_STARTUP_DELAY_MS`, refused above it at
    /// the write and at the load); the other half is the room the pass itself needs (its venues
    /// are fetched one after another). One case the bound does not cover: a first pass that
    /// SKIPS or FAILS the entry's own venue (a failed fetch, or a Degraded feed suppressing that
    /// leg) when the next pass is further away than what is left of the window. A box whose
    /// operator refused reconcile is not a case: its root closes the venue gate, so this check
    /// answers `false` there and nothing is muted for a pass that cannot come.
    ///
    /// The maker's own restored-quote clock (`strategies/market_making/spread_maker.rs`'s
    /// `restored_quote_expired`) is not part of this arithmetic. If it runs out BEFORE the first
    /// pass, its cancel by tag finds the restored order in `strategy_tags` but not in the engine
    /// registry, and `Engine::cancel_order_with_intent` sends a cancel only for an order
    /// the registry holds live, so nothing reaches the venue; the maker's re-quote is then refused
    /// here, and the pass's synthetic tagged `Accepted` hands the side back
    /// (`crates/vike-core/src/runtime/tests/restore.rs`'s
    /// `a_cancel_by_tag_of_a_restored_order_waits_for_the_pass_to_register_it` pins the cancel).
    ///
    /// Order cadence (a tagged submit), never the per-message fold.
    fn restored_tag_holds(
        &mut self,
        mount_index: usize,
        venue: &str,
        symbol: &str,
        tag: &str,
    ) -> bool {
        if self.restored_orders.is_empty() {
            return false;
        }
        let key = self.tag_key(mount_index, venue, symbol, tag);
        let Some(order_client_id) = self.strategy_tags.get(&key) else {
            return false;
        };
        let Some(held) = self.restored_orders.get(order_client_id) else {
            return false;
        };
        // The pass already judged a plain order the venue does not list (unproven: the row stays).
        if self.restore_unproven.contains_key(order_client_id)
            && held.order_type.as_ref().is_some_and(|t| !t.is_trigger())
        {
            return false;
        }
        let judged = held.symbol.is_some()
            && held.venue.as_deref().is_some_and(|v| (self.config.restore_venue_gate)(v));
        if !judged {
            return false;
        }
        let now = self.engine.now_ms;
        match self.restored_tag_deny_until {
            Some(until) => now < until,
            None => {
                self.restored_tag_deny_until =
                    Some(now.saturating_add(RESTORED_TAG_DENY_WINDOW_MS));
                self.note(format!(
                    "tag `{tag}` of mount `{}` is held by a restored order: submits under it are \
                     refused until the first reconcile pass judges it (at most {} s)",
                    self.mount_ids[mount_index],
                    RESTORED_TAG_DENY_WINDOW_MS / 1_000
                ));
                true
            }
        }
    }

    /// Refuse ONE tagged submit under a tag a restored order still holds
    /// ([`Self::restored_tag_holds`]), as a `Denied` lifecycle event the owning mount hears; and each
    /// leg of a tagged order list one of whose tags is held ([`Self::list_tag_held`]), in the same
    /// words, since the whole list waits for the same pass.
    ///
    /// The lane is [`Self::deny_undeclared_symbol`]'s: a synthetic refusal id ([`Self::mint_refusal_id`],
    /// recorded in `order_client_id_mount` so the event routes back to the asking mount) and an
    /// `OrderEventOut` pushed on the mount's engine buffer, delivered by `dispatch_order_events`.
    /// Two differences. The event carries the TAG: that lane re-stamps `tag` at delivery from the
    /// tag registry, which cannot name a refusal id, so the tag rides ahead of delivery and the
    /// stamp keeps it. A maker keys its side on that tag, and a tagless `Denied` would leave it
    /// holding a side whose submit never happened. And no `Event::OrderDenied` is published: a
    /// maker re-quotes every tick until the pass, and one bus line plus one recent-events ring line
    /// per tick would flood the 64-line ring; the one note [`Self::restored_tag_holds`] leaves
    /// stands for all of them. The tag registry is untouched, so the restored order stays
    /// addressable.
    fn deny_restored_tag(&mut self, mount_index: usize, venue: &str, symbol: &str, tag: &str) {
        let refusal_id = self.mint_refusal_id();
        let engine_index = self.route_of(EngineRoute::Mount(mount_index), venue).unwrap_or(0);
        self.order_client_id_mount.insert(refusal_id.clone(), mount_index);
        let engine = self.engine_mut(engine_index);
        if engine.collect_applied_fills {
            engine.order_events.push(vike_execution::execution_engine::OrderEventOut {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                event: vike_model::OrderEvent {
                    order_client_id: refusal_id,
                    tag: Some(tag.to_string()),
                    kind: vike_model::OrderEventKind::Denied {
                        reason: RESTORED_TAG_DENY_REASON.to_string(),
                    },
                },
            });
        }
        self.dirty = true;
    }

    /// Whether a tagged order list must wait: [`Self::restored_tag_holds`] answers true for one of
    /// its legs' `tags`. Every tag is asked, so the window opens and notes as for a single submit;
    /// an untagged list asks nothing.
    fn list_tag_held(
        &mut self,
        mount_index: usize,
        venue: &str,
        symbol: &str,
        tags: &[String],
    ) -> bool {
        let mut held = false;
        for tag in tags {
            held |= self.restored_tag_holds(mount_index, venue, symbol, tag);
        }
        held
    }

    /// Whether `tag` is BUSY (P1, the owner's ruling of 2026-11-10; the simulator's twin is
    /// `OrderBrokerSimulator`'s `tag_busy`): it names an order still working ([`Self::tag_order_works`]) that the
    /// mount sent no cancel for. A tagged submit under it is refused by name
    /// (`vike_model::tag_busy_reason`, through [`Self::deny_intent`]) before anything is minted: the
    /// older order keeps its tag, where an overwrite would leave it resting with no tag naming it.
    ///
    /// ⚠ The cancel is read off the drain's own record (`CoreThread::tag_cancels_sent`), never off
    /// `OrderStatus::PendingCancel`: `Engine::cancel_order_with_intent` sends the cancel and
    /// marks nothing, and the venue's acknowledgement is the terminal `OrderCanceled`, so an order
    /// is `PendingCancel` only when a reconcile report seeds the venue's own word. Read off the
    /// status alone, a maker that cancels in one hook and places in the next would be refused until
    /// the cancel is answered (`a_cancel_marks_no_order_pending_cancel_and_frees_its_tag`).
    ///
    /// One `is_empty()` while the core holds no tag; order cadence (a tagged submit) otherwise.
    fn tag_busy(&self, mount_index: usize, venue: &str, symbol: &str, tag: &str) -> bool {
        if self.strategy_tags.is_empty() {
            return false;
        }
        let key = self.tag_key(mount_index, venue, symbol, tag);
        self.strategy_tags.get(&key).is_some_and(|order_client_id| {
            !self.tag_cancels_sent.contains(order_client_id)
                && self.tag_order_works(order_client_id)
        })
    }

    /// Whether the order `order_client_id` still works: its engine's registry holds it live and not
    /// `PendingCancel`, or it is an emulated parent still worked (`emulated.rs`: in no registry).
    fn tag_order_works(&self, order_client_id: &str) -> bool {
        let engine_index = self.engine_of_order_client_id(order_client_id).unwrap_or(0);
        let live = self.engine(engine_index).registry.get(order_client_id).is_some_and(|mo| {
            mo.status.is_live() && mo.status != vike_model::OrderStatus::PendingCancel
        });
        live || self.emulated.working.contains_key(order_client_id)
    }

    /// The key of the row of mount `mount_index` an order on `symbol` (as sent, on engine `engine_index`,
    /// `venue`) books into, `dispatch_applied_fills`' choice: `None` the mount's own row.
    fn leverage_row_key(
        &self,
        mount_index: usize,
        engine_index: usize,
        (venue, symbol): (&str, &str),
    ) -> LeverageRowKey {
        let account = self.mount_row_account(mount_index, venue, engine_index);
        if account.is_none() && self.fill_row_is_own(mount_index, venue, symbol) {
            None
        } else {
            Some((venue.to_string(), account, symbol.to_string()))
        }
    }

    /// What mount `mount_index` holds on its row `key` ([`Self::leverage_row_key`]); nothing on a row it never
    /// traded.
    fn leverage_row_held(&self, mount_index: usize, key: &LeverageRowKey) -> f64 {
        let attr = &self.mount_attribution[mount_index];
        match key {
            None => attr.size,
            Some((venue, account, symbol)) => attr
                .legs
                .iter()
                .find(|l| &l.venue == venue && &l.account == account && &l.symbol == symbol)
                .map_or(0.0, |l| l.attr.size),
        }
    }

    /// [`Self::leverage_refusal_on`] for one verb on `venue`, `order` its legs on each leg the strategy names
    /// (`(name, legs)`; several names are ONE combo): on the engine and under the symbol each is sent with
    /// (`leg_engine`, `leg_order_target`).
    fn leverage_hold(
        &self,
        mount_index: usize,
        venue: &str,
        order: &[(&str, &[MountLeverageLeg])],
    ) -> Option<String> {
        let sent: Vec<(usize, String)> = order
            .iter()
            .map(|(name, _)| {
                let (symbol, _) = self.leg_order_target(mount_index, name);
                (self.leg_engine(mount_index, name, venue), symbol)
            })
            .collect();
        let order: Vec<_> = sent
            .iter()
            .zip(order)
            .map(|((engine_index, symbol), (_, legs))| {
                (*engine_index, venue, symbol.as_str(), *legs)
            })
            .collect();
        self.leverage_refusal_on(mount_index, &order)
    }

    /// Why mount `mount_index`'s LEVERAGE cap (`vike_execution::MountBudget::max_leverage`) refuses ONE verb, or
    /// `None`: `order` its legs on each instrument it trades (`(engine, venue, symbol as sent, legs)`; several
    /// are ONE combo), through the ONE judge (`vike_execution::risk::judge_mount_leverage`) the backtest asks
    /// too: judged ALONE against the mount's OWN ledger rows (`MountAttribution`: its own row and every leg,
    /// each at its engine's resolver mark and contract value), never the account's positions nor the callback's
    /// other verbs; an order's row is the one its fill would book into (`dispatch_applied_fills`' choice), and
    /// an instrument the ledger holds no row of yet gets one at its resolver mark. Against the mount's own
    /// engine's SIZING equity (decision 0048). Asked by the drain ([`Self::leverage_hold`],
    /// [`Self::amend_leverage_hold`]) and, for an order the core holds for its release, at the release
    /// (`release_due`). Order cadence, only for a mount whose budget states a cap; never the per-message fold.
    #[inline(never)]
    pub(crate) fn leverage_refusal_on(
        &self,
        mount_index: usize,
        order: &[(usize, &str, &str, &[MountLeverageLeg])],
    ) -> Option<String> {
        use vike_execution::risk::MountLeverageRow;
        let cap = self.mount_budget[mount_index].and_then(|b| b.max_leverage)?;
        let config = &self.config.price_config;
        let row = |engine_index: usize,
                   (venue, symbol): (&str, &str),
                   key: LeverageRowKey,
                   (size, average_price): (f64, f64)| {
            let e = self.engine(engine_index);
            let price = e.resolved_position_price(venue, symbol, size, config);
            let value = e.account.contract_value_of(symbol);
            MountLeverageRow { key, size, average_price, price, value }
        };
        let attr = &self.mount_attribution[mount_index];
        let (own_venue, own_symbol) = &self.mount_vs[mount_index];
        let own_engine = self.mount_engine(mount_index);
        let mut rows = Vec::with_capacity(attr.legs.len() + 2);
        let own = (own_venue.as_str(), own_symbol.as_str());
        rows.push(row(own_engine, own, None, (attr.size, attr.average_price)));
        for l in &attr.legs {
            let key = Some((l.venue.clone(), l.account.clone(), l.symbol.clone()));
            let leg_engine = self.mount_row_engine(mount_index, &l.venue, l.account.as_ref());
            let held = (l.attr.size, l.attr.average_price);
            rows.push(row(leg_engine, (l.venue.as_str(), l.symbol.as_str()), key, held));
        }
        let mut keyed = Vec::with_capacity(order.len());
        for &(engine_index, venue, symbol, legs) in order {
            let key = self.leverage_row_key(mount_index, engine_index, (venue, symbol));
            if !rows.iter().any(|r| r.key == key) {
                // a declared leg never traded: its row at its mark, never at the order's price
                rows.push(row(engine_index, (venue, symbol), key.clone(), (0.0, 0.0)));
            }
            keyed.push((key, self.engine(engine_index).account.contract_value_of(symbol), legs));
        }
        let order: Vec<_> = keyed.iter().map(|(key, value, legs)| (key, *value, *legs)).collect();
        let equity = self.engine(own_engine).sizing_equity(self.seed_of(own_engine), config);
        vike_execution::risk::judge_mount_leverage(Some(cap), equity, &rows, &order)
    }

    /// Why mount `mount_index`'s LEVERAGE cap refuses an amend of its order `order_client_id` to `new_qty` at
    /// `new_price` / `new_trigger`, or `None`, judged alone by the simulator's rule
    /// (`vike_execution::risk::mount_leverage_amend`): an amend that grows an order on its growth over what is
    /// still to fill (`new_qty − (qty − filled)`), one that names a price at its new price on what it will
    /// rest, a size-only decrease passing; a HELD OTO child (`held_orders`: never at the venue, all of it
    /// unfilled) on its whole new size after the unfilled ancestors that arm it ([`Self::unfilled_ancestors`]),
    /// as its list was; a repriced reduce-only exit its row or its own list covers passes. An order neither held
    /// nor live passes.
    fn amend_leverage_hold(
        &self,
        mount_index: usize,
        order_client_id: &str,
        terms: (Option<f64>, Option<f64>, Option<f64>),
    ) -> Option<String> {
        let engine_index = self.engine_of_order_client_id(order_client_id).unwrap_or(0);
        let (r, left, held) = match self.held_orders.get(order_client_id) {
            Some(held) => (held, held.qty, true),
            None => {
                let registry = &self.engine(engine_index).registry;
                let order = registry.get(order_client_id).filter(|mo| mo.status.is_live())?;
                (&order.request, order.request.qty - order.filled_qty, false)
            }
        };
        let order = vike_execution::risk::MountLeverageAmended {
            side: r.side,
            left,
            kind: Some(r.order_type.clone()),
            limit: r.price,
            trigger: r.trigger_price,
            discretion: r.attributes.and_then(|a| a.discretionary_amount),
            reduce_only: r.reduce_only,
        };
        let at = (r.venue.as_str(), r.symbol.as_str());
        let row = self.leverage_row_key(mount_index, engine_index, at);
        let held_row = self.leverage_row_held(mount_index, &row);
        let ancestors = if held { self.unfilled_ancestors(r) } else { Vec::new() };
        let legs = vike_execution::risk::mount_leverage_amend(&order, terms, held_row, ancestors)?;
        self.leverage_refusal_on(mount_index, &[(engine_index, at.0, at.1, legs.as_slice())])
    }

    /// The unfilled ancestors of the order `r`, root first, each armed by the one before (its `parent`): a
    /// held one all of it, a live one what is still to fill; none for an order no list arms.
    fn unfilled_ancestors(&self, r: &vike_model::OrderRequest) -> Vec<MountLeverageLeg> {
        let mut chain = Vec::new();
        let mut parent = r.parent_order_id.as_deref();
        while let Some(id) = parent {
            let (request, left) = match self.held_orders.get(id) {
                Some(held) => (held, held.qty),
                None => {
                    let engine_index = self.engine_of_order_client_id(id).unwrap_or(0);
                    let registry = &self.engine(engine_index).registry;
                    let Some(mo) = registry.get(id).filter(|mo| mo.status.is_live()) else { break };
                    (&mo.request, mo.request.qty - mo.filled_qty)
                }
            };
            let discretion = request.attributes.and_then(|a| a.discretionary_amount);
            let terms = (request.price, request.trigger_price);
            let price = vike_execution::risk::mount_leverage_price(request.side, terms, discretion);
            chain.push(MountLeverageLeg {
                side: request.side,
                quantity: left,
                price,
                ..Default::default()
            });
            parent = request.parent_order_id.as_deref();
        }
        chain.reverse();
        for (k, leg) in chain.iter_mut().enumerate() {
            leg.parent = k.checked_sub(1);
        }
        chain
    }

    /// Whether the core HOLDS a strategy order carrying `release` on the leg `name` (on `venue`) until the
    /// release (`held_release.rs`), where the cap judges it instead of here: every release but a time-only
    /// one the order's engine's venue holds itself (`release_held_by_venue`), which is judged here.
    fn held_until_release(
        &self,
        mount_index: usize,
        venue: &str,
        name: &str,
        release: Option<vike_model::OrderRelease>,
    ) -> bool {
        release.is_some_and(|release| {
            let request = OrderRequest {
                venue: venue.to_string(),
                release: Some(release),
                ..Default::default()
            };
            let engine_index = self.leg_engine(mount_index, name, venue);
            !self.release_held_by_venue(Some(engine_index), &request)
        })
    }

    /// Mint → RiskGate → client for each buffered submission, then resolve buffered modifications
    /// (tag → order client id) and route them. Shared by the bar path ([`drive_strategy`]) and the tick path
    /// ([`drive_strategy_tick`]) so all order construction flows through the ONE hot-path literal
    /// (piece 1) and the ONE live path. Submits are processed before modifications so a submit-then-modify
    /// within the SAME handler call resolves against the tag registered this call.
    ///
    /// `venue`/`symbol` are the DRAIN SERIES — whichever series this dispatch was about. They are
    /// the default target of a symbol-less intent of a mount that declares no legs (a mount with legs
    /// sends it to its own symbol, [`Self::resolve_intent_symbol`]); they are NOT the tag-registry
    /// key, which is the MOUNT's own series ([`Self::tag_key`]).
    ///
    /// `mount_index` is the buffering mount's index into `self.mounts` / `self.mount_states` — every
    /// call site already has it on hand (it is how the mount was found/taken in the first place).
    /// Portfolio-observer PR-4 T5: while that mount is [`MountState::Pending`] (only reachable when
    /// [`CoreConfig::readiness_gate`] is on — every mount is `Ready` immediately otherwise), the
    /// ENTIRE buffered `ctx` — submissions, brackets, order lists, modifications, cancels,
    /// conditionals, mass_cancel — is DISCARDED right here, before any of it reaches `apply_intent`. The strategy
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
        mount_index: usize,
    ) {
        // The step's FORECASTS (book W35) are journaled, never sent, and BEFORE the gate below: a forecast is
        // not an order, so a latched or not-yet-priced mount's are recorded too. One `is_empty()` for a step
        // that published none (every strategy that does not publish). Only a core told to
        // (`CoreConfig::journal_forecasts`, the run profile's `sinks.forecasts`, OFF by default) journals them:
        // otherwise they drop here and the journal's format never moves (REVIEW-LIVE-MONEY M1).
        // At most `MAX_FORECASTS_PER_STEP` of them (`journal_forecasts` holds the cap, REVIEW-LIVE-MONEY M3).
        if !ctx.forecasts.is_empty() && self.config.journal_forecasts {
            self.journal_forecasts(mount_index, venue, symbol, ctx.now, &ctx.forecasts);
        }
        // Discard this mount's buffered intents when it is either not yet priced (readiness gate,
        // portfolio-observer PR-4 T5) OR budget-latched liquidate-only (steal/core-per-mount-budget)
        // — in both cases the strategy hook already ran at the call site; only its ORDER OUTPUT is
        // dropped. `mount_latched` is all-false when no budget is active, so this stays byte-identical
        // to the readiness-only check for a budget-free runtime.
        if self.mount_states[mount_index] == MountState::Pending || self.mount_latched[mount_index]
        {
            return; // ctx (and every buffered intent in it) is dropped here — nothing reaches the engine
        }
        // A STALE INPUT (decision 0271, `CoreThread::input_hold`) holds this mount's OPENING orders, each
        // refused by name; a covered reduce, a cancel, an amend and a mass cancel pass (0243's posture for a
        // stale rate). One `Option` read for a mount that reads no input.
        let hold =
            ctx.inputs.as_ref().map(InputsAt::at).and_then(|at| self.input_hold(mount_index, at));
        // A mount's LEVERAGE cap (`MountBudget::max_leverage`, decision 0249's leverage arm) refuses its OPENING
        // orders past it by name, each judged ALONE on the mount's OWN book (`Self::leverage_refusal_on`), LAST in
        // each arm, after every other refusal of the drain, as the simulator judges it before its engine gates.
        // One read for a mount whose budget states none.
        let capped = self.mount_budget[mount_index].is_some_and(|b| b.max_leverage.is_some());
        for s in ctx.submissions {
            let Some(symbol_name) =
                self.resolve_intent_symbol(mount_index, s.symbol.as_deref(), symbol)
            else {
                // refused: the intent named a symbol this mount does not trade (one it did not
                // declare, or any other than its own on a mount that declared none). No order, and
                // VISIBLE: the strategy hears a Denied lifecycle event under the submit's tag and
                // the operator sees an `OrderDenied` line. See `deny_undeclared_symbol`.
                let refused = (s.symbol.as_deref(), s.tag);
                self.deny_undeclared_symbol(mount_index, venue, refused, symbol, now);
                continue;
            };
            if let Some(tag) = s.tag.as_deref()
                && self.restored_tag_holds(mount_index, venue, symbol, tag)
            {
                self.deny_restored_tag(mount_index, venue, symbol, tag);
                continue;
            }
            let ven = self.resolve_intent_venue(mount_index, &symbol_name, venue);
            if let Some(why) = s.refused {
                // the strategy's own refusal (`OrderBroker::deny`), heard as any denial here is
                self.deny_intent(mount_index, &ven, symbol_name, s.tag, why, now);
                continue;
            }
            // P1: a place under a BUSY tag is refused by name, and the older order keeps its tag
            if let Some(tag) = s.tag.as_deref()
                && self.tag_busy(mount_index, venue, symbol, tag)
            {
                let why = vike_model::tag_busy_reason(tag);
                self.deny_intent(mount_index, &ven, symbol_name, s.tag, why, now);
                continue;
            }
            if let Some(stale) = hold
                && self.opens_on_mount(mount_index, &ven, &symbol_name, s.side, s.qty)
            {
                self.deny_intent(
                    mount_index,
                    &ven,
                    symbol_name,
                    s.tag,
                    self.input_stale_reason(mount_index, stale),
                    now,
                );
                continue;
            }
            // The two rules the node's door and the CLI apply to an operator's order, applied here to the
            // strategy's: a lifetime the order cannot carry (`lifetime_refusal`: a lifetime other than
            // `gtc` off a limit, a `gtd` with no expiry or one not after this dispatch's `now`, an expiry
            // on any other lifetime) and prices it cannot be sent with (`order_terms_refusal`: a trigger
            // kind with no trigger above zero, a stop-limit with no limit above zero, a price that is no
            // finite number). Refused in their words before anything is minted or journaled, so the
            // replay core (which runs no drain) never sees them. Per buffered order (a few compares),
            // never per market message. The release rides the same rule: one not after `now`, or a
            // `gtd` that would expire before it, is refused here too, and so is a price condition that
            // names a venue (decision 0267's 2.13: the operator's doors alone send one, so the simulator
            // and the live core never disagree; a strategy reads another venue through its own leg). The
            // attributes wave's terms ride their two rules (`attributes_refusal`, `stop_adjust_refusal`) and the
            // percent trail the terms rule.
            let refused = vike_model::lifetime_refusal(
                &s.order_type,
                s.time_in_force,
                s.gtd_expiry,
                s.release,
                now,
            )
            .or_else(|| strategy_release_venue_refusal(s.release))
            .or_else(|| {
                vike_model::order_terms_refusal(
                    &s.order_type,
                    s.price,
                    s.trigger,
                    s.trail,
                    s.limit_offset,
                    s.peg.as_ref(),
                    s.trail_percent,
                )
            })
            .or_else(|| vike_model::display_refusal(&s.order_type, s.qty, s.display_quantity))
            .or_else(|| {
                vike_model::attributes_refusal(
                    &s.order_type,
                    s.time_in_force,
                    s.attributes.as_ref(),
                )
            })
            .or_else(|| {
                vike_model::stop_adjust_refusal(&s.order_type, s.side, s.trigger, s.adjust.as_ref())
            });
            if let Some(why) = refused {
                self.deny_intent(mount_index, &ven, symbol_name, s.tag, why, now);
                continue;
            }
            // The cap, last. An order the core holds for its release is judged AT the release
            // (`release_due`), against the mount's book then, as the simulator's `release_held` judges it.
            let discretion = s.attributes.and_then(|a| a.discretionary_amount);
            let price = vike_execution::risk::mount_leverage_price(
                s.side,
                (s.price, s.trigger),
                discretion,
            );
            let legs = one_leg(s.side, s.qty, price);
            let order = [(symbol_name.as_str(), &legs[..])];
            if capped
                && !self.held_until_release(mount_index, &ven, &symbol_name, s.release)
                && let Some(why) = self.leverage_hold(mount_index, &ven, &order)
            {
                self.deny_intent_as(
                    mount_index,
                    &ven,
                    symbol_name,
                    s.tag,
                    why,
                    vike_model::events::refusal_id_of(
                        vike_model::OrderDenial::OverMountLeverage.id().word(),
                    ),
                    now,
                );
                continue;
            }
            // A leg the strategy names by its account (`SYMBOL@venue/LABEL`, decision 0293) is sent by its
            // symbol and carries its account, which routes it; every other name is sent as named.
            let (order_symbol, account) = self.leg_order_target(mount_index, &symbol_name);
            let req = OrderRequest {
                order_client_id: String::new(),
                venue: ven.clone(),
                symbol: order_symbol,
                account,
                side: s.side,
                qty: s.qty,
                order_type: s.order_type,
                price: s.price,
                trigger_price: s.trigger,
                // a trailing kind's distance and a trailing-limit's offset; `None` for every other verb
                trail: s.trail,
                limit_offset: s.limit_offset,
                reduce_only: s.reduce_only,
                // the lifetime the strategy named (order cadence, not message cadence): `Gtc` for every
                // verb that names none, i.e. the request as it always was
                time_in_force: s.time_in_force,
                gtd_expiry: s.gtd_expiry,
                // held by the core until it holds (`held_release.rs`); `None` for every other verb
                release: s.release,
                // the attributes wave's terms (`LiveBroker::submit_limit_attributed`,
                // `submit_trailing_take_profit`, `submit_trailing_percent`, `submit_stop_adjustable`)
                attributes: s.attributes,
                trail_percent: s.trail_percent,
                adjust: s.adjust,
                // a pegged order's peg and a limit's visible size (`LiveBroker::submit_pegged_tagged`,
                // `submit_limit_display_tagged`), emulated where the venue holds neither (decision 0275)
                peg: s.peg,
                display_quantity: s.display_quantity,
                timestamp: now,
                ..Default::default()
            };
            // The tag rides into the ownership record (`own_order_client_id`), so a restart can restore it.
            let order_client_ids = self.apply_strategy_intent_with_cancel_intent(
                OrderIntent::Submit(Box::new(req)),
                now,
                mount_index,
                OrderCancelIntent::Unspecified,
                s.tag.as_slice(),
            );
            if let (Some(tag), Some(order_client_id)) = (s.tag, order_client_ids.first()) {
                // Keyed by the BUFFERING mount's index first — see `CoreThread::strategy_tags` for
                // why a mount-blind key let one mount's `cancel("bid")` hit another mount's
                // resting quote — and on the MOUNT's own series, never this drain's: see
                // `Self::tag_key`. A tagged submission names a symbol only through
                // `OrderBroker::place` (the inherent tagged verbs carry none:
                // `broker.rs`'s producer-side `a_tagged_order_carries_only_the_symbol_its_verb_named`),
                // resolved above like any submission's; the KEY is the mount's whatever the
                // ORDER's instrument, which is exactly what makes one tag name one order of the
                // mount across every lane.
                let key = self.tag_key(mount_index, venue, symbol, tag.as_str());
                self.note_tag_overwrite(mount_index, &key, tag.as_str(), order_client_id);
                if let Some(old) = self.strategy_tags.insert(key, order_client_id.clone())
                    && !self.tag_cancels_sent.is_empty()
                {
                    self.tag_cancels_sent.remove(&old);
                }
            }
        }
        for b in ctx.brackets {
            let Some(symbol_name) =
                self.resolve_intent_symbol(mount_index, b.symbol.as_deref(), symbol)
            else {
                self.deny_undeclared_symbol(
                    mount_index,
                    venue,
                    (b.symbol.as_deref(), None),
                    symbol,
                    now,
                );
                continue;
            };
            let ven = self.resolve_intent_venue(mount_index, &symbol_name, venue);
            if let Some(stale) = hold
                && self.opens_on_mount(mount_index, &ven, &symbol_name, b.side, b.qty)
            {
                self.deny_intent(
                    mount_index,
                    &ven,
                    symbol_name,
                    None,
                    self.input_stale_reason(mount_index, stale),
                    now,
                );
                continue;
            }
            // A bracket is judged as the list it lowers to: its entry, and its two reduce-only exits OCO
            // under it (`vike_model::build_bracket`), covered by the entry and the row.
            let exit = |price: f64, other: usize| MountLeverageLeg {
                side: -b.side,
                quantity: b.qty,
                price: Some(price),
                parent: Some(0),
                linked: vec![other],
            };
            let [entry] = one_leg(b.side, b.qty, b.entry_price);
            let legs = [entry, exit(b.stop_loss, 2), exit(b.take_profit, 1)];
            let order = [(symbol_name.as_str(), &legs[..])];
            if capped && let Some(why) = self.leverage_hold(mount_index, &ven, &order) {
                self.deny_intent_as(
                    mount_index,
                    &ven,
                    symbol_name,
                    None,
                    why,
                    vike_model::events::refusal_id_of(
                        vike_model::OrderDenial::OverMountLeverage.id().word(),
                    ),
                    now,
                );
                continue;
            }
            let (symbol, account) = self.leg_order_target(mount_index, &symbol_name);
            let spec = OrderBracket {
                venue: ven,
                symbol,
                side: b.side,
                qty: b.qty,
                entry_price: b.entry_price,
                stop_loss: b.stop_loss,
                take_profit: b.take_profit,
                account,
                order_client_id: None,
            };
            self.apply_strategy_intent(OrderIntent::Bracket(Box::new(spec)), now, mount_index);
        }
        // An order list lowers as ONE intent on the symbol it names, resolved by the submissions' one rule
        // (a list naming none, every list but `place_group`'s: the drain series on a mount with no legs,
        // as before, the mount's own on one with legs); refused WHOLE in words (`order_list_refusal`)
        // before anything is minted or journaled. An undeclared symbol is refused as a submission's is,
        // each leg of a tagged list hearing its tag.
        for l in ctx.order_lists {
            let Some(symbol_name) =
                self.resolve_intent_symbol(mount_index, l.symbol.as_deref(), symbol)
            else {
                let tags: Vec<Option<String>> = if l.tags.is_empty() {
                    vec![None]
                } else {
                    l.tags.into_iter().map(Some).collect()
                };
                for tag in tags {
                    let refused = (l.symbol.as_deref(), tag);
                    self.deny_undeclared_symbol(mount_index, venue, refused, symbol, now);
                }
                continue;
            };
            let ven = self.resolve_intent_venue(mount_index, &symbol_name, venue);
            // A list opens when any leg does (a ladder's entry); an exit pair of covered reduces passes.
            if let Some(stale) = hold
                && l.legs.as_ref().is_ok_and(|legs| {
                    legs.iter().any(|g| {
                        self.opens_on_mount(mount_index, &ven, &symbol_name, g.side, g.qty)
                    })
                })
            {
                self.deny_intent(
                    mount_index,
                    &ven,
                    symbol_name,
                    None,
                    self.input_stale_reason(mount_index, stale),
                    now,
                );
                continue;
            }
            let (list_symbol, account) = self.leg_order_target(mount_index, &symbol_name);
            let spec = l.legs.and_then(|legs| {
                let spec = vike_model::OrderList {
                    venue: ven.clone(),
                    symbol: list_symbol,
                    legs,
                    account,
                };
                order_list_refusal(&spec, now).map_or(Ok(spec), Err)
            });
            match spec {
                // A list naming a tag a restored order still holds is refused whole (decision 0219's
                // shape), every leg hearing the restart's refusal, so none waits for an order never sent.
                Ok(_) if self.list_tag_held(mount_index, venue, symbol, &l.tags) => {
                    for tag in &l.tags {
                        self.deny_restored_tag(mount_index, venue, symbol, tag);
                    }
                }
                Ok(spec) => {
                    // The cap, last, judges the list WHOLE, here and now, as the simulator's `judge_group`
                    // does: every leg at its own price, linked (OCO/OUO) legs as alternatives, a held OTO child
                    // (`OrderListLeg::parent`) counting where its parent filled, since the core releases it
                    // later (`drive_contingency_on_fill`) with no judge. Refused, each leg hears it under its tag.
                    let legs: Vec<MountLeverageLeg> = spec
                        .legs
                        .iter()
                        .map(|g| MountLeverageLeg {
                            side: g.side,
                            quantity: g.qty,
                            price: g.price.or(g.trigger_price),
                            parent: g.parent,
                            linked: g.linked.clone(),
                        })
                        .collect();
                    let order = [(symbol_name.as_str(), legs.as_slice())];
                    if capped && let Some(why) = self.leverage_hold(mount_index, &ven, &order) {
                        let tags: Vec<Option<String>> = if l.tags.is_empty() {
                            vec![None]
                        } else {
                            l.tags.into_iter().map(Some).collect()
                        };
                        for tag in tags {
                            let at = symbol_name.clone();
                            self.deny_intent_as(
                                mount_index,
                                &ven,
                                at,
                                tag,
                                why.clone(),
                                vike_model::events::refusal_id_of(
                                    vike_model::OrderDenial::OverMountLeverage.id().word(),
                                ),
                                now,
                            );
                        }
                        continue;
                    }
                    // Each leg's tag rides into its ownership record (`own_order_client_id`), in leg order.
                    let order_client_ids = self.apply_strategy_intent_with_cancel_intent(
                        OrderIntent::OrderList(Box::new(spec)),
                        now,
                        mount_index,
                        OrderCancelIntent::Unspecified,
                        &l.tags,
                    );
                    // Each leg of a tagged list (`vike_model::OrderBroker::place_group`) is filed under
                    // its tag against the order client id minted for it, in leg order, as a tagged submit's is
                    // (the same key and overwrite note); an untagged list carries none.
                    for (tag, order_client_id) in l.tags.iter().zip(&order_client_ids) {
                        let key = self.tag_key(mount_index, venue, symbol, tag);
                        self.note_tag_overwrite(mount_index, &key, tag, order_client_id);
                        self.strategy_tags.insert(key, order_client_id.clone());
                    }
                }
                Err(why) => self.deny_intent(mount_index, &ven, symbol_name, None, why, now),
            }
        }
        // A combo lowers as ONE intent on the mount's venue, routed by the mount (its one account: no account on
        // the spec, the bracket's way); refused WHOLE in the model's words before anything is minted or journaled.
        for c in ctx.combos {
            let ven = self.resolve_intent_venue(mount_index, symbol, venue);
            // A combo leg trades `ratio` units of the combo, its side the combo's times the ratio's sign.
            if let Some(stale) = hold
                && c.legs.iter().any(|g| {
                    let side = c.side * g.ratio.signum();
                    let qty = c.qty * f64::from(g.ratio.unsigned_abs());
                    self.opens_on_mount(mount_index, &ven, &g.symbol, side, qty)
                })
            {
                self.deny_intent(
                    mount_index,
                    &ven,
                    symbol.to_string(),
                    None,
                    self.input_stale_reason(mount_index, stale),
                    now,
                );
                continue;
            }
            let spec = vike_model::OrderCombo {
                venue: ven.clone(),
                side: c.side,
                qty: c.qty,
                legs: c.legs,
                net_limit: c.net_limit,
                time_in_force: vike_model::TimeInForce::default(),
                account: None,
                // the runtime mints a strategy's combo id, as every strategy-minted order's
                order_client_id: None,
            };
            match spec.validate() {
                Ok(()) => {
                    // The cap, last: the combo as ONE unit, all its legs or none, each on its own row,
                    // as the simulator's `judge_combo` judges it.
                    let legs: Vec<[MountLeverageLeg; 1]> = spec
                        .legs
                        .iter()
                        .map(|g| {
                            let side = spec.side * g.ratio.signum();
                            one_leg(side, spec.qty * f64::from(g.ratio.unsigned_abs()), g.limit)
                        })
                        .collect();
                    let order: Vec<(&str, &[MountLeverageLeg])> = spec
                        .legs
                        .iter()
                        .zip(&legs)
                        .map(|(g, l)| (g.symbol.as_str(), &l[..]))
                        .collect();
                    if capped && let Some(why) = self.leverage_hold(mount_index, &ven, &order) {
                        self.deny_intent_as(
                            mount_index,
                            &ven,
                            symbol.to_string(),
                            None,
                            why,
                            vike_model::events::refusal_id_of(
                                vike_model::OrderDenial::OverMountLeverage.id().word(),
                            ),
                            now,
                        );
                        continue;
                    }
                    self.apply_strategy_intent(
                        OrderIntent::Combo(Box::new(spec)),
                        now,
                        mount_index,
                    );
                }
                Err(why) => {
                    self.deny_intent(
                        mount_index,
                        &ven,
                        symbol.to_string(),
                        None,
                        why.to_string(),
                        now,
                    );
                }
            }
        }
        for a in ctx.modifications {
            let key = self.tag_key(mount_index, venue, symbol, &a.tag);
            if let Some(order_client_id) = self.strategy_tags.get(&key).cloned() {
                // The cap judges the amend by the one amend rule (`Self::amend_leverage_hold`): a grow on its
                // growth, a named price at its new price on what it will rest, a held child's grow on its whole
                // size with its ancestors; refused, the order keeps its terms and the ring says why (the
                // simulator's seam amend records it in `dropped`).
                let terms = (a.new_qty, a.new_price, a.new_trigger_price);
                if capped
                    && let Some(why) =
                        self.amend_leverage_hold(mount_index, &order_client_id, terms)
                {
                    self.note(format!(
                        "strategy amend of order {order_client_id} REFUSED, its terms kept: {why}"
                    ));
                    continue;
                }
                self.apply_strategy_intent(
                    OrderIntent::Modify {
                        order_client_id,
                        new_qty: a.new_qty,
                        new_price: a.new_price,
                        new_trigger_price: a.new_trigger_price,
                    },
                    now,
                    mount_index,
                );
            }
        }
        for tag in ctx.cancels {
            let key = self.tag_key(mount_index, venue, symbol, &tag);
            if let Some(order_client_id) = self.strategy_tags.get(&key).cloned() {
                // ROUTINE, and this is the ONE site in the runtime where that is genuinely known:
                // `OrderBroker::cancel` pulls ONE of a strategy's own tagged quotes, which is
                // ladder churn by construction — a maker suppressing a side, or the cancel half of
                // a requote it is about to re-place. A venue metering its cancels may hold this
                // back under pressure (it comes back as a NON-terminal cancel-reject and the
                // strategy re-offers on its next tick), which is exactly the churn a flatten
                // reserve is protected FROM.
                //
                // ⚠ `LiveBroker::mass_cancel()` is NOT classified with it, further down: "pull ALL my
                // quotes" is the adverse-selection hammer, it is not retried on a shed, and it
                // stays at the flatten-safe default.
                // The tag is free from here (P1, `Self::tag_busy`): the engine marks nothing on its own
                // cancel, so the drain keeps the record of one it sends (an order it holds working).
                if self.tag_order_works(&order_client_id) {
                    self.tag_cancels_sent.insert(order_client_id.clone());
                }
                self.apply_strategy_intent_with_cancel_intent(
                    OrderIntent::Cancel(order_client_id),
                    now,
                    mount_index,
                    OrderCancelIntent::Routine,
                    &[],
                );
            }
        }
        for c in ctx.order_emulation_arms {
            let Some(cond_symbol) =
                self.resolve_intent_symbol(mount_index, c.symbol.as_deref(), symbol)
            else {
                self.deny_undeclared_symbol(
                    mount_index,
                    venue,
                    (c.symbol.as_deref(), None),
                    symbol,
                    now,
                );
                continue;
            };
            let cond_ven = self.resolve_intent_venue(mount_index, &cond_symbol, venue);
            // A leg named by its account arms on that account and fires there (decision 0293).
            let (arm_symbol, account) = self.leg_order_target(mount_index, &cond_symbol);
            if let Some(stale) = hold
                && self.opens_on_mount(mount_index, &cond_ven, &cond_symbol, c.side, c.qty)
            {
                self.deny_intent(
                    mount_index,
                    &cond_ven,
                    cond_symbol,
                    None,
                    self.input_stale_reason(mount_index, stale),
                    now,
                );
                continue;
            }
            // A trailing arm (`submit_trailing`, `submit_trailing_limit`) is judged by the doors' terms rule: a
            // trail not above zero (or no finite number) or a negative offset is a `Denied` the mount hears,
            // in the rule's words, before anything is armed or journaled.
            if c.trail.is_some() {
                let kind = if c.limit_offset.is_some() {
                    vike_model::OrderType::TrailingLimit
                } else {
                    vike_model::OrderType::Trailing
                };
                if let Some(why) = vike_model::order_terms_refusal(
                    &kind,
                    None,
                    None,
                    c.trail,
                    c.limit_offset,
                    None,
                    None,
                ) {
                    self.deny_intent(mount_index, &cond_ven, cond_symbol, None, why, now);
                    continue;
                }
            }
            // The cap, last.
            let legs = one_leg(c.side, c.qty, c.price);
            let order = [(cond_symbol.as_str(), &legs[..])];
            if capped && let Some(why) = self.leverage_hold(mount_index, &cond_ven, &order) {
                self.deny_intent_as(
                    mount_index,
                    &cond_ven,
                    cond_symbol,
                    None,
                    why,
                    vike_model::events::refusal_id_of(
                        vike_model::OrderDenial::OverMountLeverage.id().word(),
                    ),
                    now,
                );
                continue;
            }
            // `LiveBroker::submit_trailing_limit`'s offset makes the arm a trailing-limit, stated in
            // its `kind_terms` (the legacy fields `None`); every other verb arms today's stop or
            // trailing stop, byte for byte.
            let kind_terms = c.limit_offset.map(|offset| {
                Box::new(vike_execution::ConditionalKindTerms {
                    kind: vike_model::OrderType::TrailingLimit,
                    price: None,
                    trail: c.trail,
                    extreme: None,
                    limit: None,
                    limit_offset: Some(offset),
                    activation: None,
                    trail_percent: None,
                    adjust: None,
                })
            });
            let legacy = kind_terms.is_none();
            self.apply_strategy_intent(
                OrderIntent::ArmConditional(ConditionalIntent {
                    venue: cond_ven,
                    symbol: arm_symbol,
                    side: c.side,
                    qty: c.qty,
                    price: c.price.filter(|_| legacy),
                    trail: c.trail.filter(|_| legacy),
                    // strategies arm on the default (Last) lane today — the strategy's order
                    // seam exposes no trigger-source verb; None keeps this path byte-identical
                    trigger_by: None,
                    kind_terms,
                    account,
                }),
                now,
                mount_index,
            );
        }
        if ctx.mass_cancel {
            // ⚠ The MOUNT's series, NEVER the drain's — the same rule `tag_key` states at length,
            // and the last verb that was still keyed the other way.
            //
            // `LiveBroker::mass_cancel` is documented as "cancel ALL of THIS ENGINE's live orders", and
            // `apply/exit.rs`'s `(Some(v), symbol)` arm makes the VENUE load-bearing: it resolves that
            // venue's engine and calls `Engine::mass_cancel_with_intent` on the whole
            // thing, using the symbol only to scope held exits and conditional books (on this
            // `EngineRoute::Mount` lane; an OPERATOR's named symbol also narrows the registry
            // cancel, `lower_mass_cancel`). So the venue stamped here decides which book gets pulled.
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
            let (mc_venue, mc_symbol) = self.mounts[mount_index]
                .as_ref()
                .map_or((venue, symbol), |m| (m.venue.as_str(), m.symbol.as_str()));
            self.apply_strategy_intent(
                OrderIntent::MassCancel {
                    venue: Some(mc_venue.to_string()),
                    symbol: Some(mc_symbol.to_string()),
                    account: None,
                },
                now,
                mount_index,
            );
        }
        self.dirty = true;
    }

    /// ONE tag's key into `strategy_tags` — `{mount_index}|{mount_venue}|{mount_symbol}|{tag}`.
    ///
    /// ## ⚠ The MOUNT's series, NEVER the drain's — the insert and BOTH lookups, together
    ///
    /// [`Self::drain_broker`] runs on whichever series dispatched, and that is NOT always the
    /// mount's own: the bar and tick lanes dispatch a DECLARED LEG's series, and the fill and
    /// order-event lanes dispatch the FILL's / the ORDER's `(venue, symbol)`. The key used to be
    /// built from those drain ARGUMENTS, so a tagged quote placed from one lane was filed under a
    /// key a `cancel` from another lane never looks up. The failure mode is the one this
    /// program's own plan called worse than the netting bug: the lookup finds nothing, `drain_broker`
    /// simply does not act, and a REAL quote is left resting at the venue with no error, no event
    /// and no ring line. The cross-exchange maker's lane notes (its `XemmMaker` impl in
    /// `strategies/market_making/cross_exchange_maker.rs`) carry the per-lane table this was found
    /// through; its hedge-fill arm still emits no tagged SUBMIT, but for the EMISSION reason that
    /// survives here (a symbol-less submit still resolves to the drain's series) rather than for
    /// the registry reason, which this method removes.
    ///
    /// Keying on the mount removes the second dimension the strategy could not see anyway:
    /// `OrderBroker::amend`/`cancel` name NO symbol, so from the strategy's side a tag has always
    /// been a MOUNT-scoped name. The drain-keyed registry gave it a
    /// hidden per-series dimension that no verb could address.
    ///
    /// The insert and both lookups go through this ONE method deliberately. Moving only the insert —
    /// the obvious half-fix — reproduces the silent no-op above exactly, which is why
    /// `crates/vike-core/tests/wiring/multi_symbol_reads.rs` drives the tag in BOTH directions (place on
    /// the mount's series and cancel from the leg's, and the reverse): each direction fails if only
    /// one side moved.
    ///
    /// ⚠ **One tag names ONE order per mount.** Re-using a tag whose order still works, before
    /// cancelling it, is refused (`Self::tag_busy`, P1); one placed after its cancel was sent re-points
    /// the entry. Keying on the mount makes that hold across two LEGS too: a mount quoting both legs
    /// must use per-leg tags — which is also the only shape the symbol-less `amend`/`cancel` can
    /// express.
    ///
    /// Byte-identical for every single-symbol mount: on every lane its drain series IS its own
    /// (`drive_strategy`/`drive_strategy_tick` match `m.symbol == symbol`; `drive_strategy_feed_status`/
    /// `_flow`/`_params` require the pair; `_mark`/`_reference_quote`/`drive_schedule` drain the
    /// mount's own by construction; and an undeclared mount's orders — hence its fills and order
    /// events — can only ever carry its own symbol, since `resolve_intent_symbol` admits no other
    /// for it). Cold path: called only when a tagged verb was actually
    /// buffered, so the per-market-message dispatch that buffers nothing pays nothing and the
    /// `p99 < 10µs` fold is untouched. `drain_venue`/`drain_symbol` are the fallback for a
    /// transiently-taken mount slot, which no call site can reach (each restores the mount before
    /// draining).
    pub(super) fn tag_key(
        &self,
        mount_index: usize,
        drain_venue: &str,
        drain_symbol: &str,
        tag: &str,
    ) -> String {
        let (venue, symbol) = self.mounts[mount_index]
            .as_ref()
            .map_or((drain_venue, drain_symbol), |m| (m.venue.as_str(), m.symbol.as_str()));
        format!("{mount_index}|{venue}|{symbol}|{tag}")
    }

    /// The STRATEGY's own tag for `order_client_id` under `mount_index`, or `None` if that mount has no live tag
    /// entry naming this order — the reverse of [`Self::tag_key`], and the stamp that gives
    /// [`vike_model::OrderEvent::tag`] its value.
    ///
    /// ## Why the lookup is VALUE-matched, and why that is the whole safety argument
    ///
    /// `strategy_tags` maps `{mount_index}|{venue}|{symbol}|{tag}` → order client id, and a re-submit under the
    /// same tag OVERWRITES the entry (`drain_broker`'s insert). So the question "which tag names
    /// this order" is only answerable by matching the order client id on the VALUE side. Doing it that way is
    /// not merely convenient — it is what makes a LATE terminal harmless: a maker that submits under
    /// `"bid"` again before the previous order's cancel-ack arrives has an entry pointing at the NEW
    /// order client id, the old order client id matches nothing, this returns `None`, and the strategy is told about an
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
    pub(crate) fn tag_for_order_client_id(
        &self,
        mount_index: usize,
        order_client_id: &str,
    ) -> Option<String> {
        let m = self.mounts[mount_index].as_ref()?;
        let prefix = format!("{mount_index}|{}|{}|", m.venue, m.symbol);
        self.strategy_tags
            .iter()
            .find(|(k, v)| v.as_str() == order_client_id && k.starts_with(&prefix))
            .and_then(|(k, _)| k.strip_prefix(&prefix).map(str::to_string))
    }

    /// OBSERVE (change nothing) a tagged submit that is about to re-point `key` at `new_order_client_id` while
    /// the entry already names a DIFFERENT order that is still live in its engine's registry: that
    /// older order keeps resting and no tag can address it any more (the overwrite
    /// [`Self::tag_key`] warns about). Counts it in
    /// `RestoreCounters::tags_repointed_order_still_live` and leaves one recent-events note per occurrence.
    /// No refusal here, the insert follows: a submit under a tag whose order works is refused before
    /// it is minted ([`Self::tag_busy`], P1), so what reaches this is a place after the old order's
    /// cancel was sent and before its answer, which leaves it resting untagged if the venue refuses
    /// that cancel, and a tagged order list's leg, which no busy check judges.
    ///
    /// Per ORDER (called once per tagged submit, never per market message). A dead old order (the
    /// registry holds it terminal, or no longer holds it) is the ordinary requote and counts
    /// nothing.
    fn note_tag_overwrite(
        &mut self,
        mount_index: usize,
        key: &str,
        tag: &str,
        new_order_client_id: &str,
    ) {
        let Some(old) = self.strategy_tags.get(key) else {
            return;
        };
        if old == new_order_client_id {
            return;
        }
        let engine_index = self.engine_of_order_client_id(old.as_str()).unwrap_or(0);
        let live = self
            .engine(engine_index)
            .registry
            .get(old.as_str())
            .is_some_and(|mo| mo.status.is_live());
        if !live {
            return;
        }
        let old = old.clone();
        self.restore_counters.tags_repointed_order_still_live += 1;
        self.note(format!(
            "tag `{tag}` of mount `{}` re-pointed from live order {old} to {new_order_client_id}: the older \
             order keeps resting with no tag naming it",
            self.mount_ids[mount_index]
        ));
    }

    /// Drop `mount_index`'s tag entry for `order_client_id` — called ONLY once that order is terminal, so the
    /// registry holds live orders and nothing else.
    ///
    /// Unlike [`Self::note_terminal_order_client_id`], this erases IMMEDIATELY and needs no linger window: the
    /// linger exists so a late fill can still be ATTRIBUTED to a mount, and attribution reads
    /// `order_client_id_mount`, never this map. What this map answers is "which order does the tag `"bid"` name
    /// right now", and after a terminal the honest answer is "none" — keeping the dead entry is what
    /// let `amend("bid", ..)` resolve a terminal order client id and be swallowed by
    /// `Engine::modify_order`'s not-modifiable early return, with no error, no event and no
    /// ring line. The removal is value-guarded through [`Self::tag_for_order_client_id`], so a late terminal
    /// can never retire an entry that has since been re-pointed at a live order.
    pub(crate) fn retire_tag_for_order_client_id(
        &mut self,
        mount_index: usize,
        order_client_id: &str,
    ) -> Option<String> {
        let tag = self.tag_for_order_client_id(mount_index, order_client_id)?;
        let key = self.tag_key(mount_index, "", "", &tag);
        self.strategy_tags.shift_remove(&key);
        Some(tag)
    }

    /// Journal a mounted strategy's ALREADY-RESOLVED order intent write-ahead of `apply_intent`
    /// (portfolio-observer PR-5 T2) — the `drain_broker` boundary's twin of `dispatch()`'s `Command`
    /// gate (~:954-958): same idiom, journal BEFORE the fold that mints order client ids. `self.journal.is_some()`
    /// gates the whole hook so journaling off stays byte-identical to today (every existing
    /// drain_broker/readiness/journal_wiring test is unaffected). `intent` is journaled AFTER the
    /// tag→order client id resolution `drain_broker`'s modify/cancel call sites already did, so replay never
    /// needs the `strategy_tags` registry — it re-applies the same resolved `OrderIntent` a
    /// `Command::Order` `Command` would carry. `journaled_since_snap` is bumped here too — the SAME
    /// field `dispatch()`'s tail cadence check (~:1165) reads — so a strategy-heavy session still
    /// feeds the `snapshot_every` cadence; the actual `write_snap` call fires at the next journaled
    /// (Event/Command/Watchdog) dispatch, since `drain_broker` itself runs from within a
    /// BarClose/Quote/Trade/Book fold that is not in that `journaled` match arm — the counter is
    /// not lost, just possibly observed one dispatch later. Cold path (`drain_broker` is off the
    /// per-message hot fold, so this never touches the `p99 < 10µs` gate): the mount_id is cloned
    /// rather than borrowed to sidestep holding `self.journal.as_mut()` (mutable) and
    /// `self.mount_ids[mount_index]` (immutable) at once.
    ///
    /// ⚠ Visible to the runtime's white-box tests only: lowering through here SKIPS `drain_broker`'s
    /// readiness, latch and undeclared-symbol gates, so no production caller outside the drain
    /// may use it.
    pub(in crate::runtime) fn apply_strategy_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        mount_index: usize,
    ) -> Vec<String> {
        self.apply_strategy_intent_with_cancel_intent(
            intent,
            now,
            mount_index,
            OrderCancelIntent::Unspecified,
            &[],
        )
    }

    /// [`Self::apply_strategy_intent`], classifying every cancel it lowers (see [`OrderCancelIntent`]).
    /// The classification reaches the venue and is journaled NOWHERE — see
    /// [`Self::apply_intent_with_cancel_intent`] for why it rides beside the [`OrderIntent`] rather
    /// than inside it, and why that keeps the write-ahead record's shape unchanged.
    pub(crate) fn apply_strategy_intent_with_cancel_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        mount_index: usize,
        cancel_intent: OrderCancelIntent,
        tags: &[String],
    ) -> Vec<String> {
        // A `Modify` the core will act on in no way is lowered and NOT journaled (`Self::modify_is_inert`, decision
        // 0127 amended 2026-11-19): a replay re-applying it would change nothing. Every other intent is journaled.
        let journaled = self.journal.is_some() && !self.journal_skips(&intent);
        // The engine this mount lowers onto, named on the record so a replay can lower it onto the same
        // one (`Command::OrderOn`); `None` on a core the payload's venue can tell apart (and no journal).
        let rk = if journaled {
            let engine_index = match self.strategy_route(mount_index, &intent) {
                EngineRoute::Engine(i) => i,
                _ => self.mount_engine(mount_index),
            };
            self.journal_replay_route_key(engine_index)
        } else {
            None
        };
        if journaled {
            // A submit of a key the gate denied before is HELD until its verdict (`denial_streaks.rs`).
            self.begin_denial_candidate(mount_index, &intent);
        }
        if journaled && let Some(journal) = self.journal.as_mut() {
            let mid = self.mount_ids[mount_index].clone();
            // a HELD record counts toward the cadence when it is framed (`frame_held_submits`), never when dropped
            let held = journal.deferred_submits().is_some();
            journal
                .append_strategy_submit(now, &mid, &intent, rk.as_deref())
                .expect("journal append");
            self.journaled_since_snap += u64::from(!held);
        }
        let order_client_ids =
            self.lower_strategy_intent(intent, now, mount_index, cancel_intent, tags);
        if journaled {
            self.end_denial_candidate();
        }
        order_client_ids
    }

    /// Whether [`Self::apply_strategy_intent_with_cancel_intent`] leaves `intent` out of the journal: an inert
    /// `Modify` (`Self::modify_is_inert`). A white-box test's `journal_every_modify` keeps every one, which is the
    /// journal before the 2026-11-19 amendment, for the equivalence proof.
    fn journal_skips(&self, intent: &OrderIntent) -> bool {
        #[cfg(test)]
        if self.journal_every_modify {
            return false;
        }
        self.modify_is_inert(intent)
    }

    /// [`Self::apply_strategy_intent_with_cancel_intent`] WITHOUT its write-ahead `StrategySubmit`: the
    /// lowering and the attribution alone. For an order a replay places off records it already re-applies
    /// (an amend's replacement: the Modify, or the replacement's own `MintedSubmit`; `apply/amend.rs`), where
    /// a re-applied record of its own would place it twice; the mint's `MintedSubmit` journals it.
    pub(crate) fn lower_strategy_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        mount_index: usize,
        cancel_intent: OrderCancelIntent,
        tags: &[String],
    ) -> Vec<String> {
        // ⚠ ROUTED BY THE MOUNT, not by the intent's venue string. This is the one choke point every
        // strategy-minted intent lowers through, so it is the one place the mount's declared
        // ACCOUNT can be turned into an engine — and turning it into one HERE is what makes a
        // misroute unwritable rather than merely untested: there is no venue string anywhere
        // downstream that could name the wrong account, because the account never becomes a string.
        // `EngineRoute::Mount` still defers to the payload for a leg declared on a FOREIGN venue;
        // see that enum. A leg on ANOTHER account of the mount's own venue lowers onto that account's
        // engine ([`Self::strategy_route`], decision 0293).
        let route = self.strategy_route(mount_index, &intent);
        // …and an intent naming an account, whatever engine route it ends on (that one, or the payload's
        // account resolved below), is still the MOUNT's: its id carries the mount's code and a core-held arm
        // it becomes is the mount's. Every other intent lowers exactly as before.
        let owner = self.order_client_id_owner;
        if intent_names_an_account(&intent) {
            self.order_client_id_owner = Some(mount_index);
        }
        let order_client_ids = self.apply_intent_routed(intent, now, cancel_intent, route);
        self.order_client_id_owner = owner;
        // steal/core-per-mount-budget ATTRIBUTION: tag every order client id this mount's intent minted back to
        // the mount, so its fills fold into the mount's ledger and its resting orders can be canceled
        // scoped-to-this-mount on a budget breach. `apply_intent` returns the order client ids of orders it
        // SUBMITTED (empty for modify/cancel/arm), so nothing is over-tagged. Cold path (order
        // cadence, off the p99 fold); the map is written even with no budget (attribution is
        // always-on + read-only — it changes no fold decision, so behavior stays byte-identical).
        // `own_order_client_id` also records the ownership for the next boot when the ownership file is on.
        // `tags` name the order client ids in mint order, as `drain_broker` files them in `strategy_tags`: one
        // for a tagged submit, one per leg for a tagged list, none for anything else.
        for (n, c) in order_client_ids.iter().enumerate() {
            self.own_order_client_id(c.clone(), mount_index, tags.get(n).map(String::as_str));
        }
        // A `fill = "close"` mount's bar step on a paper engine files what it placed (`closing_ids`).
        if let Some((closing, ids)) = self.closing_ids.as_mut()
            && *closing == mount_index
        {
            ids.extend(order_client_ids.iter().cloned());
        }
        order_client_ids
    }

    /// The mount that MINTED `order_client_id` — the EXACT strategy-callback routing key for a fill or an
    /// order event (multi-mount correctness, bug B).
    ///
    /// `order_client_id_mount` is written at [`Self::apply_strategy_intent`], i.e. the one choke point where a
    /// mount's buffered intent lowers into `apply_intent`, so it already records precisely which
    /// mount owns each server-minted order client id. Routing by it replaces the previous FIRST-MATCH scan over
    /// `(venue, symbol)`, which was not a routing key at all once two mounts shared a symbol: only
    /// the first ever received `on_fill`/`on_order_event`, and the second silently never heard about
    /// its own orders. (The bar lane has always matched the full `(venue, symbol, interval)` triple;
    /// the fill/order-event lanes did not.)
    ///
    /// `None` means "no mount minted this order client id" — a manual operator ticket, a margin-call
    /// liquidation, a reconcile-synthesised or adopted order, a settlement — or that the minting mount
    /// is gone (unmounted: its `order_client_id_mount` entry outlives the slot) or its slot is transiently taken
    /// (mid another hook dispatch, which no call site reaches). Callers then deliver to NO mount
    /// (decision 0116: an event no mount caused is not a strategy's to answer); there is no
    /// `(venue, symbol)` fallback and no sole-mount exception.
    /// A pure map lookup at order/fill cadence; the per-message fold never calls it.
    pub(crate) fn mount_for_order_client_id(&self, order_client_id: &str) -> Option<usize> {
        let index = *self.order_client_id_mount.get(order_client_id)?;
        self.mounts.get(index).and_then(|slot| slot.as_ref()).map(|_| index)
    }

    /// **The mount that minted a FILL's order**, a tombstone's included: its `order_client_id_mount` entry, else, for an
    /// order whose entry was retired (`ORDER_CLIENT_ID_PRUNE_LINGER_MS` after its terminal) or that ended before this
    /// process started, the one LIVE mount whose `Mount::order_client_id_code` its v2 strategy id names, when the id is
    /// OURS: the rule the boot restore books such an order's late tail fill by (`crates/vike-core/src/replay/boot.rs`'s
    /// `restore_from`, through `crates/vike-core/src/replay/refold.rs`'s `mount_by_code`). The fill lane's one lookup
    /// ([`Self::dispatch_applied_fills`]), so its ledger and its delivery read one answer. A late fill is the one late
    /// word that matters: the FSM drops a lifecycle event of an order that has ended, an `Account` folds a fill whatever.
    pub(crate) fn mount_of_fill(&self, order_client_id: &str) -> Option<usize> {
        match self.order_client_id_mount.get(order_client_id) {
            Some(&index) => Some(index),
            None => self.mount_by_strategy_code(order_client_id),
        }
    }

    /// [`Self::mount_of_fill`]'s answer for an order without an entry.
    #[cold]
    #[inline(never)]
    fn mount_by_strategy_code(&self, order_client_id: &str) -> Option<usize> {
        use vike_model::orders::client_id::{
            OrderClientIdReading, OrderClientIdSource, parse_order_client_id,
        };
        let Some(OrderClientIdReading::V2(parts)) = parse_order_client_id(order_client_id) else {
            return None;
        };
        if parts.src != OrderClientIdSource::Strategy {
            return None;
        }
        let mut slots = (0..self.mounts.len()).filter(|&i| {
            self.mounts[i].as_ref().is_some_and(|m| m.order_client_id_code == Some(parts.strat))
        });
        let slot = slots.next().filter(|_| slots.next().is_none())?;
        (self.execution_ownership(order_client_id) == vike_model::orders::ExecutionOwnership::Ours)
            .then_some(slot)
    }
}
