//! Engine routing: which engine an event, a venue/symbol, a route key, an arm or a mount resolves to.

use super::*;

/// The SYMBOL a venue-tagged payload names, for the two-accounts-of-one-exchange disambiguation in
/// [`CoreThread::route_event`]. `None` for every other event, and — importantly — for
/// [`Event::AccountState`].
///
/// ⚠ **`AccountState` has no symbol, and it does not need one.** It is an account-wide balance
/// snapshot, so it never had a symbol to be disambiguated by — the residual this used to declare.
/// It is now closed by [`event_route_key`] instead: the payload carries the route key outright,
/// stamped by the MOUNT (`vike_mount::account_event_sender`), which is the layer that knows which
/// account a venue lane belongs to. Its POSITIONS and FILLS were never affected — those are
/// symbol-tagged and route exactly.
pub(crate) fn event_symbol(ev: &Event) -> Option<&str> {
    match ev {
        Event::Fill(f) => Some(&f.symbol),
        Event::Funding(f) => Some(&f.symbol),
        Event::PositionLiquidated(p) => Some(&p.symbol),
        Event::OrderSubmitted(_)
        | Event::OrderAccepted(_)
        | Event::OrderRejected(_)
        | Event::OrderDenied(_)
        | Event::OrderTriggered(_)
        | Event::OrderPartiallyFilled(_)
        | Event::OrderFilled(_)
        | Event::OrderCanceled(_)
        | Event::OrderExpired(_)
        | Event::OrderLiquidated(_)
        | Event::OrderModified(_)
        | Event::PositionOpened(_)
        | Event::PositionChanged(_)
        | Event::PositionClosed(_)
        | Event::AccountState(_)
        | Event::OrderCancelRejected(_)
        | Event::OrderModifyRejected(_) => None,
    }
}

/// The route key a venue-tagged payload carries OUTRIGHT — the exact answer to "which account of
/// this exchange", consulted by [`CoreThread::route_event`] before the symbol disambiguator.
///
/// **EVERY venue-tagged payload with no CLIENT-ORDER-ID answers here** — [`Event::AccountState`],
/// [`Event::Funding`] and [`Event::PositionLiquidated`] — and the membership rule is exactly that:
/// a coid or a stamped key, never a symbol.
///
/// ⚠ **The last two are a CORRECTION, and the reasoning that omitted them is kept because it was
/// nearly right.** The rule used to be "only `AccountState`, because every other venue-tagged
/// payload names a symbol and a symbol resolves EXACTLY — the mount's symbol-collision rule makes
/// it unique per account within a venue". When that rule was deleted (two accounts on one
/// instrument is an ordinary spread — `vike_config::venue_accounts`), the justification was
/// rewritten to "what carries the other payloads instead is their client-order-id", and THAT is
/// where it went wrong: it is true of [`Event::Fill`] and of nothing else.
/// [`Event::Funding`] and [`Event::PositionLiquidated`] carry a symbol and no coid, so on the very
/// configuration a labelled mount creates — both engines claiming one symbol, so
/// [`CoreThread::engine_idx_for_venue_symbol`] answers `None` — they fell through to the venue
/// lookup and folded into the venue's DEFAULT engine. A labelled account's funding debit landed on
/// the default account's `balance`; a labelled account's liquidation CLOSED the default account's
/// position at the venue's liq price while the liquidated account went on reporting it open.
/// `vike_model::events::FundingEvent::route_key` carries the same argument from the wire's side.
///
/// [`Event::Fill`] stays OUT deliberately: [`event_coid`] resolves it through the submit-time
/// `coid_venue` map, exactly, for every order this process placed and with nothing on the wire — so
/// stamping it would put a redundant field on the one shape the frozen parity fixtures pin, to
/// answer a question already answered.
///
/// `None` on a default-account box — nothing stamps a key equal to its own venue
/// (`vike_exec::EventSender::routed`), so this returns `None` for every payload such a box has
/// ever seen and the caller takes the branch it always took.
pub(crate) fn event_route_key(ev: &Event) -> Option<&str> {
    match ev {
        Event::AccountState(a) => a.route_key.as_ref().map(|k| k.as_str()),
        Event::Funding(f) => f.route_key.as_ref().map(|k| k.as_str()),
        Event::PositionLiquidated(p) => p.route_key.as_ref().map(|k| k.as_str()),
        Event::Fill(_)
        | Event::OrderSubmitted(_)
        | Event::OrderAccepted(_)
        | Event::OrderRejected(_)
        | Event::OrderDenied(_)
        | Event::OrderTriggered(_)
        | Event::OrderPartiallyFilled(_)
        | Event::OrderFilled(_)
        | Event::OrderCanceled(_)
        | Event::OrderExpired(_)
        | Event::OrderLiquidated(_)
        | Event::OrderModified(_)
        | Event::PositionOpened(_)
        | Event::PositionChanged(_)
        | Event::PositionClosed(_)
        | Event::OrderCancelRejected(_)
        | Event::OrderModifyRejected(_) => None,
    }
}

/// Order-lifecycle reply routing key: the coid every lifecycle event carries.
///
/// ⚠ **[`Event::Fill`] is in this list and is the one VENUE-TAGGED payload that is**, which reads
/// as an inconsistency until you look at where it is consulted. A bare execution report names the
/// order it belongs to, and [`CoreThread::route_event`] uses that as its second-most-exact answer —
/// above the symbol, below the payload's own stamped route key — because a coid resolves through
/// the submit-time `coid_venue` map and is therefore EXACT for every order this process placed,
/// while a symbol is exact only while one engine of the venue claims it. It changes nothing for a
/// payload with no venue: `route_event`'s venue arm returns before the coid fallback below, so the
/// non-venue-tagged lifecycle events reach that fallback exactly as they always did.
pub(crate) fn event_coid(ev: &Event) -> Option<&str> {
    match ev {
        Event::Fill(e) => Some(&e.client_order_id),
        Event::OrderSubmitted(e) => Some(&e.client_order_id),
        Event::OrderAccepted(e) => Some(&e.client_order_id),
        Event::OrderRejected(e) => Some(&e.client_order_id),
        Event::OrderDenied(e) => Some(&e.client_order_id),
        Event::OrderTriggered(e) => Some(&e.client_order_id),
        Event::OrderPartiallyFilled(e) => Some(&e.client_order_id),
        Event::OrderFilled(e) => Some(&e.client_order_id),
        Event::OrderCanceled(e) => Some(&e.client_order_id),
        Event::OrderExpired(e) => Some(&e.client_order_id),
        Event::OrderLiquidated(e) => Some(&e.client_order_id),
        Event::OrderModified(e) => Some(&e.client_order_id),
        Event::OrderCancelRejected(e) => Some(&e.client_order_id),
        Event::OrderModifyRejected(e) => Some(&e.client_order_id),
        Event::PositionOpened(_)
        | Event::PositionChanged(_)
        | Event::PositionClosed(_)
        | Event::AccountState(_)
        | Event::Funding(_)
        | Event::PositionLiquidated(_) => None,
    }
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Drain every conflated slot (per-symbol freshest ticks + freshest forming bars)
    /// and clear the marker so the next publish re-arms it. One marker can carry many
    /// symbols' updates.
    /// Routing index for a ROUTE KEY: 0 = primary, i+1 = extra i, None = unknown.
    ///
    /// Matches on `ExecutionEngine::route_key` — which engine — and NEVER on
    /// `ExecutionEngine::venue`, which answers the different question of what the exchange
    /// supports. `ExecutionEngine::new` seeds the two equal, so while every engine in this process
    /// is its venue's only account this resolves exactly what the old `venue` comparison did, for
    /// every input, bit-identically.
    ///
    /// ⚠ **Almost every caller passes a CANONICAL VENUE STRING** — `FillEvent::venue`,
    /// `OrderRequest::venue`, `MountSpec::venue`, an operator's `--venue` argument. That is correct
    /// precisely while `route_key == venue` for every engine, and it is the seam a second account
    /// has to come through: the payload has to start carrying the route key, because a
    /// `FillEvent` labelled `"binance"` cannot say WHICH binance account it belongs to. Nothing
    /// resolves that here — this function only stops the ROUTING question and the CAPABILITY
    /// question from sharing one field, which is what made the two-account shape unexpressible.
    ///
    /// That "almost every" is why the parameter is a [`RouteKey`] and not a `&str`: it cannot be
    /// reached from a canonical venue without spelling [`RouteKey::sole_account_of`], so
    /// `git grep -c 'RouteKey::sole_account_of' -- crates/` IS the roster of sites a second account
    /// must revisit — a derived list rather than the prose one this doc used to carry. The
    /// exceptions, which pass [`RouteKey::declared`] because they carry a REAL route key, are
    /// [`Self::reconcile_reports`] (via `ReconcileReports::route`) and [`Self::confirm_recon`] (via
    /// the held alert's stored key).
    ///
    /// Zero-cost: `RouteKey<'_>` is a `&str`, so the per-event constructions in [`Self::route_event`]
    /// and `drain_market` allocate nothing and branch nowhere — the p99 fold is untouched.
    /// The engine that owns `(venue, symbol)` — the LAST-RESORT disambiguator [`Self::route_event`]
    /// uses when two engines share one exchange, and consulted from nowhere else.
    ///
    /// It matches on `ExecutionEngine::venue` (the exchange, shared by both accounts) plus the
    /// engine's own mounted symbol or one of its `extra_symbols`.
    ///
    /// ⚠ **It answers only when EXACTLY ONE engine claims the symbol, and that qualifier is a
    /// correction.** This used to return the FIRST match, and it was described as exact — because
    /// two active accounts of one venue could not be armed on one symbol. That refusal is gone
    /// (`vike_config::venue_accounts`' module doc: two accounts on one instrument is an ordinary
    /// spread), so a first-match answer would silently fold a second account's venue-tagged payload
    /// into the first account's book. Ambiguity is now `None`, which sends the caller to the venue
    /// lookup — the same place a symbol no engine claims has always gone.
    ///
    /// **What that leaves, declared rather than implied**: an ambiguous payload with no other
    /// handle lands on the venue's DEFAULT engine. In practice the handle is there —
    /// [`Self::route_event`] consults the payload's stamped route key (every coid-less payload:
    /// `AccountState`, `Funding`, `PositionLiquidated`) and then its client-order-id (resolved
    /// through the submit-time `coid_venue` map) BEFORE reaching here, so every payload belonging
    /// to an order this process placed, and every payload the mount stamped, routes exactly.
    ///
    /// ⚠ This residual was once stated as covering the funding and liquidation lanes too, on the
    /// strength of "every other payload carries a coid". It does not: those two carry a symbol and
    /// nothing else, so the residual silently included a labelled account's funding debits and its
    /// liquidations — see [`event_route_key`]. They are stamped now, and what is genuinely left is
    /// the one case nothing can name: a FOREIGN fill (an order this process did not place, so no
    /// `coid_venue` entry) reaching an UNSTAMPED lane, on a symbol two of its accounts both trade.
    /// That is reconcile's territory, not this lane's.
    pub(crate) fn engine_idx_for_venue_symbol(&self, venue: &str, symbol: &str) -> Option<usize> {
        let claims = |e: &ExecutionEngine<C>| {
            e.venue == venue && (e.symbol == symbol || e.extra_symbols.iter().any(|s| s == symbol))
        };
        let mut found = None;
        if claims(&self.engine) {
            found = Some(0);
        }
        for (i, (_, e)) in self.extra_engines.iter().enumerate() {
            if claims(e) {
                if found.is_some() {
                    // TWO accounts of one exchange on one symbol — legal, and not something this
                    // lookup can decide. Saying nothing is the whole point.
                    return None;
                }
                found = Some(i + 1);
            }
        }
        found
    }

    /// **Every engine of one EXCHANGE**, default account first — the routing answer for work that
    /// belongs to the venue rather than to one account of it.
    ///
    /// Exactly one element on every single-account core (and empty for a venue this core runs no
    /// engine for, which is the `None` the venue lookup returns there), so a caller that iterates
    /// it does on such a core precisely what the venue lookup made it do.
    pub(crate) fn engines_of_venue(&self, venue: &str) -> Vec<usize> {
        let primary = self.engine_idx_for_route_key(RouteKey::sole_account_of(venue));
        let mut out: Vec<usize> = primary.into_iter().collect();
        if self.multi_account {
            for i in 0..=self.extra_engines.len() {
                if Some(i) != primary && self.eng(i).venue == venue {
                    out.push(i);
                }
            }
        }
        out
    }

    pub(crate) fn engine_idx_for_route_key(&self, route_key: RouteKey<'_>) -> Option<usize> {
        let route_key = route_key.as_str();
        if route_key == self.engine.route_key {
            return Some(0);
        }
        self.extra_engines.iter().position(|(_, e)| e.route_key == route_key).map(|i| i + 1)
    }

    /// **§4.2's `N`** — how many engines of one EXCHANGE this process runs, counted without
    /// allocating.
    ///
    /// Equal to `self.engines_of_venue(venue).len()` for every input (pinned by
    /// `mount_account_tests`' `the_account_count_is_the_engine_set_it_claims_to_be`); it exists
    /// separately because [`Self::route_event`]'s last rung asks the question ON THE FOLD, where a
    /// `Vec` allocation would be a per-event cost for an answer that is one integer.
    ///
    /// `1` on every single-account process for every venue it mounts, `0` for a venue it does not.
    pub(crate) fn accounts_of_venue(&self, venue: &str) -> usize {
        let primary = self.engine_idx_for_route_key(RouteKey::sole_account_of(venue));
        let mut n = usize::from(primary.is_some());
        if self.multi_account {
            n += (0..=self.extra_engines.len())
                .filter(|&i| Some(i) != primary && self.eng(i).venue == venue)
                .count();
        }
        n
    }

    /// **THE engine one MOUNT trades on** — [`Self::mount_engine`] at that index, i.e. the account
    /// the mount declared, resolved once at assemble.
    ///
    /// Every strategy lane reads its `LiveBroker` context through this rather than re-deriving an
    /// index from the mount's venue STRING, and that is the structural half of the account seam: a
    /// venue-keyed lookup (`engine_idx_for_route_key(sole_account_of(venue))`) can only ever answer
    /// with a venue's DEFAULT account, so while the lanes asked that question a second account was
    /// unaddressable no matter what a mount declared — and the mount's READS would have kept
    /// pointing at the default account's book even once its WRITES moved, which is a worse defect
    /// and one no order-routing test would catch.
    pub(crate) fn mount_eng(&self, mount_idx: usize) -> usize {
        self.mount_engine[mount_idx]
    }

    /// **The engine an [`EngineRoute`] NAMES, or `None` when it names none for this payload** —
    /// [`Self::route_of`] without its venue-default fallback, which is the half a VENUE-SCOPED
    /// command needs: "did the caller address one ACCOUNT" and "which account does this venue
    /// default to" are different questions, and collapsing them is how a command that names an
    /// EXCHANGE came to act on one account of it (see [`Self::exit_scope_engines`]).
    pub(crate) fn routed_engine(&self, route: EngineRoute, payload_venue: &str) -> Option<usize> {
        let declared = match route {
            EngineRoute::Payload => None,
            EngineRoute::Mount(i) => Some(self.mount_eng(i)),
            EngineRoute::Engine(i) => Some(i),
        };
        // The SAME foreign-venue deference `EngineRoute::Mount` needs, applied to
        // `EngineRoute::Engine` too: an engine index names an ACCOUNT of one exchange, so a
        // payload on a different exchange is not that account's business.
        //
        // ⚠ It is INERT for every `Engine` producer in this tree, and since
        // [`Self::route_for_payload_account`] joined them that is true for TWO different reasons
        // rather than one. The core-internal producers (the per-engine margin-call sweep, the
        // conditional FIRE, the panic button's flatten legs) read the venue OUT OF the engine they
        // name, so the two sides are the same string by construction. The payload-account
        // resolution goes the other way — it composes a route key FROM the payload's venue and
        // looks an engine up by it — and is inert for a second reason: `route_key_of` renders
        // either the bare venue or `venue#LABEL`, and `vike_mount::make_engine_accounts` stamps
        // that key on an engine whose `venue` is the canonical id it was built from, so a key that
        // matches determines the engine's venue.
        //
        // That second property is a fact about the MOUNT rather than about this function, which is
        // exactly why the filter stays spelled once, here, rather than being assumed at each
        // producer: an engine whose `route_key` and `venue` were ever set apart would make it live,
        // and the right answer there is still to defer to the payload rather than to trade a
        // foreign book. A backstop that costs one string compare on a cold path is worth keeping
        // even while nothing can trip it.
        declared.filter(|&e| self.eng(e).venue == payload_venue)
    }

    /// **The route a PAYLOAD'S OWN `account` field names** — `Some(route)` when the destination is
    /// determined, `None` when the payload names an account this process runs no engine for.
    ///
    /// This is the seam `vike_model::OrderRequest`'s `account` field exists for, and it is asked
    /// BEFORE [`Self::ambiguous_accounts`] rather than inside it. The ordering is the whole fix:
    /// [`Self::routed_engine`] answers `None` for [`EngineRoute::Payload`] unconditionally — an
    /// `EngineRoute` carries no account, because until the field existed a payload had nowhere to
    /// name one — so every labelled order reaching a two-account venue was refused as *ambiguous*
    /// before routing was consulted at all, whatever it said. Resolving here turns the payload's
    /// statement into an [`EngineRoute::Engine`], after which the gate, the journal's route key,
    /// `caps_venue` and [`Self::route_of`] are each handed a destination that is already
    /// determined, and none of them needs widening.
    ///
    /// # The three answers
    ///
    /// * **A route that ALREADY names a book on this payload's exchange wins, unchanged.** That is
    ///   what keeps the strategy lane untouched by construction rather than by care: a mount's
    ///   account is resolved at assemble through `CoreThread::mount_engine`, it is a stronger
    ///   statement than a string on a payload, and a strategy-minted `OrderRequest` carries no
    ///   account to consult in the first place. The case where the route names a book on a
    ///   DIFFERENT exchange is §9 item 2's foreign-venue fallthrough — `routed_engine` filters that
    ///   deference away, correctly — and there the payload's own account is the only statement
    ///   anyone has made about THIS venue, so it is consulted.
    /// * **An account this core runs an engine for** resolves to that engine and only that one.
    /// * **An account it does not** is `None`, and the caller REFUSES. It must not fall through to
    ///   [`Self::route_of`]'s `sole_account_of`, and the single-account node is the dangerous case
    ///   rather than the safe one: there `multi_account` is false, the ambiguity gate is silent by
    ///   construction, and the fallback would answer `Some(0)` — trading the one book the node has
    ///   on a client's typo, with nothing anywhere saying so.
    ///
    /// ⚠ **This widens NOTHING for an account-LESS payload.** `account: None` never reaches here
    /// (the caller's `match` short-circuits on it), so every existing caller — every strategy
    /// order, every pre-field wire ticket, every journal replay — takes a path this function is not
    /// on. The `N = 0` affordance [`Self::ambiguous_accounts`] protects by name (an unknown venue
    /// keeps `unwrap_or(0)`) is likewise untouched for them; a payload that NAMES an account of a
    /// venue this core runs nothing for is a different question with a different answer, because
    /// the sender named a specific book rather than declining to.
    ///
    /// # Cost
    ///
    /// One `String` per labelled submit, from `vike_model::accounts::account_keys::route_key_of`, and NONE
    /// on the account-less path. The allocation is deliberate: `route_key_of` is the ONE composer
    /// of this spelling — the mount fan-out stamps it, `vike_ops::live_lock` names a sentinel file
    /// after it, the journal records it, [`mount_engine_resolution`] resolves through it — and
    /// re-deriving the bare-venue case inline to save it would be a second copy of the default
    /// account's spelling, free to disagree with that one. It is paid on an ORDER boundary, never
    /// on the per-message fold, and only by a payload that came from the wire naming an account.
    pub(crate) fn route_for_payload_account(
        &self,
        route: EngineRoute,
        payload_venue: &str,
        account: &vike_model::accounts::account_keys::AccountLabel,
    ) -> Option<EngineRoute> {
        if self.routed_engine(route, payload_venue).is_some() {
            return Some(route);
        }
        // `RouteKey::declared`, never `sole_account_of`: the key was COMPOSED from a venue and a
        // label, so it IS a route key, and the caller is asserting nothing about how many accounts
        // of this venue exist. Spelling it the other way would also enrol this site in the derived
        // `sole_account_of` roster (`vike_exec::route_key`'s module doc), which is the list of
        // places a second account has yet to reach — and this is the one place that has.
        let key = vike_model::accounts::account_keys::route_key_of(payload_venue, account);
        self.engine_idx_for_route_key(RouteKey::declared(&key)).map(EngineRoute::Engine)
    }

    /// **The route a risk-REDUCING verb acts under, given the account its payload names** —
    /// [`Self::route_for_payload_account`] for `MassCancel`, `Flatten` and `MarketExit`, with the
    /// two refusals those verbs need said here once rather than at three arms.
    ///
    /// * **No account** — `Some(route)`, UNCHANGED. This is the whole of the no-regression claim:
    ///   §4.5's fan-out (a reducing verb naming no account reaches every account of its venue) and
    ///   the unscoped panic button (`venue: None` too — every engine) both stay exactly as they
    ///   were, because nothing here is consulted for them. Every internal producer — the dead-man,
    ///   the shutdown sweep, a strategy's `mass_cancel`, the panic button's own flatten legs —
    ///   passes `None` and takes this arm.
    /// * **An account of a venue this core HOLDS** — the route that names that account's engine,
    ///   which [`Self::exit_scope_engines`] then answers with as ONE engine: the verb reaches that
    ///   book and no other. A route that already names a book on this venue (a labelled mount's
    ///   own intent) wins, exactly as it does for a submit.
    /// * **An account it does NOT hold** — refused by name ([`Self::refuse_unheld_account`]), and
    ///   `None`. It must not widen back to the venue: a narrowed verb that fell through to the
    ///   fan-out would cancel and flatten every book of the exchange on a client's typo, and on a
    ///   single-account core the fan-out IS the one book the node has.
    /// * **An account with NO venue** — refused by name ([`Self::refuse_venueless_account`]), and
    ///   `None`. An account label names one book OF a venue, and the arm a venue-less reducing verb
    ///   reaches is the GLOBAL one; reading the account as ignorable would turn the narrowest
    ///   request into the widest action this core can take.
    ///
    /// The caller returns on `None` without touching an engine, a book or a held exit — the refusal
    /// has already been said on both of the core's surfaces.
    ///
    /// Command cadence, never the per-message fold: one `String` (the composed route key) per
    /// LABELLED reducing verb, and nothing at all on the account-less path.
    pub(crate) fn reduce_route_for_account(
        &mut self,
        verb: &str,
        route: EngineRoute,
        venue: Option<&str>,
        account: Option<&vike_model::accounts::account_keys::AccountLabel>,
    ) -> Option<EngineRoute> {
        let Some(account) = account else {
            return Some(route);
        };
        let Some(venue) = venue else {
            self.refuse_venueless_account(verb, account);
            return None;
        };
        let resolved = self.route_for_payload_account(route, venue, account);
        if resolved.is_none() {
            self.refuse_unheld_account(verb, venue, account);
        }
        resolved
    }

    /// **The engine a HELD bracket exit will be released onto** — the one answer
    /// [`Self::submit_resolved`] acts on, factored out so the account-scoped clear of the held set
    /// (`clear_held_scope`) attributes an exit to exactly the book its release would reach rather
    /// than to a second derivation of it.
    ///
    /// `coid_venue` first (recorded at the bracket's mint for every non-primary engine), then the
    /// payload venue's sole-account engine, then engine 0 — the fallback chain `submit_resolved`'s
    /// own comment argues, and whose stage-4 ruling (leave it) is recorded there.
    pub(crate) fn held_release_engine(&self, coid: &str, venue: &str) -> usize {
        self.coid_venue
            .get(coid)
            .copied()
            .or_else(|| self.engine_idx_for_route_key(RouteKey::sole_account_of(venue)))
            .unwrap_or(0)
    }

    /// **The engine an ARMED conditional will fire onto, or `None` when its fire reaches NO
    /// engine.** It serves the same purpose as [`Self::held_release_engine`]: the account-scoped
    /// clear attributes an arm to the book its fire would actually reach, never to a guess at it.
    ///
    /// `cond_engine`'s recorded index answers first. An arm with no row (one seeded from a restored
    /// `Snap`, `CoreConfig::conditionals`' declared residual) fires through `submit_fired`'s MISS
    /// arm: `EngineRoute::Payload`, with no account on the request. So this asks what the Submit arm
    /// asks of that request, in the same order:
    ///
    /// * **[`Self::ambiguous_accounts`] says the venue has several accounts** → `None`. The Submit
    ///   arm refuses that fire as ambiguous, so it lands in no book, and the arm belongs to no
    ///   account a labelled clear could name. The clear then leaves it under every label. Only
    ///   the account-less, venue-wide clear takes it.
    ///
    ///   ⚠ This answered with the venue's DEFAULT engine until 2026-09-26, through
    ///   `route_of(Payload, ..)`, which is the engine the Submit arm would have used before the
    ///   ambiguity gate existed. So `mass-cancel binance DEFAULT` disarmed a restored arm that may
    ///   have been `ALT`'s, and `mass-cancel binance ALT` kept it for the same wrong reason.
    ///   `mount_account_tests`'
    ///   `a_labelled_mass_cancel_leaves_a_restored_arm_it_cannot_attribute_to_the_named_account`
    ///   pins both the refused fire and the clear.
    /// * **Otherwise** → the Submit arm's own `route_of(..).unwrap_or(0)`. That is the venue's one
    ///   engine, or engine 0 for a venue this core runs nothing for. This is exactly the engine the
    ///   fire is submitted to.
    ///
    /// Command cadence, called only by the narrowed clear. The ambiguity question allocates one
    /// `Vec` per restored arm there, and nothing on the fold.
    pub(crate) fn armed_engine(&self, arm_id: &str, venue: &str) -> Option<usize> {
        if let Some(armed) = self.cond_engine.get(arm_id) {
            return Some(armed.engine);
        }
        if self.ambiguous_accounts(EngineRoute::Payload, venue).is_some() {
            return None;
        }
        Some(self.route_of(EngineRoute::Payload, venue).unwrap_or(0))
    }

    /// **Lower an [`EngineRoute`] onto a concrete engine index**, given the payload's own venue.
    ///
    /// [`EngineRoute::Mount`] answers with the mount's engine when the payload is on that engine's
    /// own exchange, and defers to the payload otherwise — see that enum for why a cross-venue
    /// declared leg must not inherit the mount's account. `None` has the same meaning it has in
    /// [`Self::engine_idx_for_route_key`]: no engine claims this payload, and the caller applies
    /// its own historical fallback.
    ///
    /// ⚠ **This answers with ONE engine, always, and that is right for a payload that names an
    /// ORDER and wrong for one that names an EXCHANGE.** A single order belongs to a single book;
    /// a venue-scoped mass-cancel or market exit belongs to every account of that exchange. The
    /// second shape resolves through [`Self::exit_scope_engines`] instead.
    pub(crate) fn route_of(&self, route: EngineRoute, payload_venue: &str) -> Option<usize> {
        if let Some(e) = self.routed_engine(route, payload_venue) {
            return Some(e);
        }
        self.engine_idx_for_route_key(RouteKey::sole_account_of(payload_venue))
    }

    /// **Every engine ONE venue-scoped command acts on** — the engine set behind
    /// [`vike_exec::OrderIntent::MarketExit`] and the `MassCancel { venue: Some(_) }` arm it lowers
    /// through, and the set its flatten legs are READ from.
    ///
    /// Three cases, and the middle one is the fix:
    ///
    /// * **`venue: None`** — EVERY engine, whatever the route says. Naming the whole set IS naming
    ///   the target: the panic button must never require an argument, and the unscoped `MassCancel`
    ///   arm has always fanned over every engine.
    /// * **`venue: Some(v)`, route names no engine** (every EXTERNAL command path that names no
    ///   account — a DOM click, a tradehub ticket, a CLI verb) — EVERY engine of that EXCHANGE,
    ///   through [`Self::engines_of_venue`]. This used to be [`Self::route_of`], i.e. the venue's
    ///   DEFAULT account: on a two-account node the operator's "get me out of binance" cancelled
    ///   one book, flattened the other account's position ONTO that same book — the default
    ///   flattened twice, possibly reversed — and left the second account holding both its position
    ///   and its resting orders.
    /// * **`venue: Some(v)`, route names an engine** (a labelled mount's own intent, a
    ///   core-internal producer that already resolved one — [`super::EngineRoute::Engine`] — or an
    ///   operator's reducing verb whose PAYLOAD named its account, which
    ///   [`Self::reduce_route_for_account`] turns into that engine's route before this is asked) —
    ///   that ONE engine. A strategy's exit must not reach another account's book,
    ///   `sweep_link_deadman` addresses each account explicitly for exactly this reason, and a
    ///   `market-exit binance ALT` is a way out of ALT's book and no other.
    ///
    /// The empty-`engines_of_venue` case (a venue this core runs no engine for) keeps
    /// `route_of(..).unwrap_or(0)`'s historical answer verbatim — engine 0 — rather than becoming a
    /// silent no-op: that fallback is load-bearing for the `MassCancel` arm's own tests, and the
    /// flatten walk discards it anyway through its `eng_venue != v` filter.
    ///
    /// Command cadence, never the per-message fold: one `Vec` per operator action, sized by the
    /// number of accounts this process runs on one exchange (one, on every core whose account
    /// table arms no labelled account).
    pub(crate) fn exit_scope_engines(&self, route: EngineRoute, venue: Option<&str>) -> Vec<usize> {
        let Some(v) = venue else {
            return (0..=self.extra_engines.len()).collect();
        };
        if let Some(e) = self.routed_engine(route, v) {
            return vec![e];
        }
        let all = self.engines_of_venue(v);
        // `engines_of_venue` is empty EXACTLY when no engine of this core claims `v`, which is
        // precisely when `route_of` answered `None` and the caller took its `unwrap_or(0)`.
        if all.is_empty() { vec![0] } else { all }
    }

    /// The CANONICAL venue whose declared capability row applies to a payload that has ALREADY been
    /// routed — the ROUTED engine's own `ExecutionEngine::venue`, which is the only thing
    /// `vike_model::caps_for` / `preflight_order_at` / `amend_semantics` are facts about.
    ///
    /// **Why this exists.** An order payload carries ONE venue string and it does two jobs: it is
    /// what [`Self::engine_idx_for_route_key`] routes on, and it is what the caps row is selected
    /// by. `ExecutionEngine` split those two jobs into two fields; a payload cannot, so the moment
    /// one has to carry a per-ACCOUNT routing key the caps lookup reads a string
    /// `vike_model::VENUES` does not contain — and `preflight_order`'s unknown-venue affordance
    /// answers `Ok(())`, i.e. every capability check SKIPPED (order kind, TIF, margin mode), for
    /// one account of one exchange, with no log line. Asking the engine instead of the payload
    /// removes that: the engine knows its own exchange whatever it is routed by.
    ///
    /// `routed` is [`Self::engine_idx_for_route_key`]'s VERBATIM answer, `None` included — do not
    /// pass `unwrap_or(0)`. A payload that resolves NO engine keeps its own string
    /// (`payload_venue`), because that is what the unknown-venue affordance is for (paper/sim
    /// engines behind non-roster ids) and because attributing it to the primary engine's caps row
    /// would start refusing traffic that flows today.
    ///
    /// Inert while every engine's `route_key` IS its `venue`: a routed payload's own venue string
    /// then EQUALS `self.eng(i).venue` by construction, so the answer is the same string it always
    /// was.
    pub(crate) fn caps_venue<'a>(
        &'a self,
        routed: Option<usize>,
        payload_venue: &'a str,
    ) -> &'a str {
        match routed {
            Some(i) => self.eng(i).venue.as_str(),
            None => payload_venue,
        }
    }

    /// **A venue PRICE belongs to every account of that exchange** — mirror one onto the venue's
    /// other engines, after the routed one has taken it.
    ///
    /// A mark, a bar close and a quote are facts about the EXCHANGE, not about an account: two
    /// accounts of one venue see the same book. The mark lanes route by venue, so with a second
    /// account mounted every price landed in the FIRST account's engine and the second account's
    /// `Account` priced its positions off nothing — its `resolved_equity` and
    /// `resolved_position_price` fell through to their fallbacks while the operator watched a
    /// correctly-routed order fill into a book that could not value it.
    ///
    /// ⚠ **Guarded on [`Self::multi_account`], which is `false` for every single-account process** —
    /// so this is one already-computed bool read on the measured p99 mark drain and the fold is
    /// byte-identical there. It writes through the SAME `Account::set_mark_from` the routed write
    /// used, so the mark-source precedence law is applied once per engine rather than bypassed.
    ///
    /// `board` mirrors the ROUTED write's own board behaviour and must not be inferred from
    /// `source`: the bar-close and venue-mark lanes file a board slot (`Some(ts)`), while the
    /// TRADE-TICK lane writes the account mark only and leaves the board's trade/quote slots to
    /// their own producers — filing a sub-bar print as a venue mark there would corrupt the price
    /// resolver's source tagging on the mirrored engine while the routed one stayed correct.
    pub(crate) fn mirror_venue_price(
        &mut self,
        venue: &str,
        symbol: &str,
        px: f64,
        source: MarkSource,
        now: i64,
        board: Option<i64>,
    ) {
        if !self.multi_account {
            return;
        }
        let primary = self.engine_idx_for_route_key(RouteKey::sole_account_of(venue)).unwrap_or(0);
        for i in 0..=self.extra_engines.len() {
            if i == primary || self.eng(i).venue != venue {
                continue;
            }
            let eng = if i == 0 { &mut self.engine } else { &mut self.extra_engines[i - 1].1 };
            eng.account.set_mark_from(venue, symbol, px, source, now);
            if let Some(ts) = board {
                match source {
                    MarkSource::BarClose => eng.price_board.set_bar_close(venue, symbol, px, ts),
                    MarkSource::VenueMark | MarkSource::ReconcileMark | MarkSource::TradeTick => {
                        eng.price_board.set_mark(venue, symbol, px, ts)
                    }
                }
            }
        }
    }

    /// Read access to an engine by routing index (0 = primary).
    pub(crate) fn eng(&self, idx: usize) -> &ExecutionEngine<C> {
        if idx == 0 { &self.engine } else { &self.extra_engines[idx - 1].1 }
    }

    /// Mutable twin of [`Self::eng`].
    pub(crate) fn eng_mut(&mut self, idx: usize) -> &mut ExecutionEngine<C> {
        if idx == 0 { &mut self.engine } else { &mut self.extra_engines[idx - 1].1 }
    }

    /// The equity seed an engine folds against (primary = config.seed_cash).
    pub(crate) fn seed_of(&self, idx: usize) -> f64 {
        if idx == 0 { self.config.seed_cash } else { self.extra_engines[idx - 1].0 }
    }
}
