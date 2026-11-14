//! The venue-event fold: the engine's `EventHandler::on_event` and its lifecycle helpers.

use vike_model::FiniteNumbers;
use vike_model::events::{Event, FillEvent, OrderLiquidated, PositionLiquidated, PositionSide};

use crate::bus::{EventBusFold, EventBusOutbox, EventHandler};
use vike_model::OrderStatus;

use super::{AppliedFill, AppliedFx, Engine, ExecutionClient, OrderEventOut};

/// The most fills one engine parks as unproven ([`Engine::take_unproven_fills`]) between two takes of
/// the core; past it the oldest goes (the core takes them at every reconcile pass and stuck-order sweep).
const MAX_UNPROVEN_FILLS: usize = 64;

/// Why [`Engine::fill_drop_reason`] says the fold would drop a fill: one variant per guard of
/// the `Fill` arm of [`EventHandler::on_event`], in the order the fold meets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillDrop {
    /// Neither the engine's mounted symbol nor an admitted one, and no order of its registry names it.
    UntradedSymbol,
    /// A fill with no order client id on a spot pair (a `Cash` symbol): a trade placed by hand in the venue's
    /// app, the user's and not the strategy's (the spot rule).
    HandTrade,
    /// An inverse engine with no contract row for the symbol: folding it as linear books PnL wrong by the price.
    UnknownContractKind,
    /// A ledger holding foreign-quoted symbols, and this one's quote currency is not known to it.
    UnknownQuoteCurrency,
    /// Declared quoted in another currency, no conversion rate yet, and the fill is not of an order it owns.
    UnratedQuote,
    /// A number of the fill is not finite.
    NonFinite,
    /// The fill names another venue than the account's.
    ForeignVenue,
    /// The trade id is already folded into the account (booked once, or reused by another fill).
    AlreadyFolded,
    /// The account's ledger keeps a window of days (`crate::account::fill_window`), and the fill is stamped
    /// before it or not stamped at all, so it cannot be told from a re-delivery of one the window left out.
    BeforeDedupWindow,
}

impl FillDrop {
    /// The reason in an operator's words.
    pub fn words(self) -> &'static str {
        match self {
            FillDrop::UntradedSymbol => {
                "it is not a symbol this account's engine trades (neither its mounted symbol nor an admitted one)"
            }
            FillDrop::HandTrade => {
                "it names no order of ours on a spot pair: a trade placed by hand in the venue's app is the \
                 user's, not the strategy's"
            }
            FillDrop::UnknownContractKind => {
                "this inverse engine does not know the symbol's contract kind, and folding it as linear would book \
                 its PnL wrong by the price"
            }
            FillDrop::UnknownQuoteCurrency => {
                "this engine holds symbols quoted in another currency and does not know this symbol's, so folding it \
                 at par would be wrong by the whole rate"
            }
            FillDrop::UnratedQuote => {
                "the symbol is declared quoted in another currency and the engine holds no conversion rate for it yet"
            }
            FillDrop::NonFinite => "a number of the fill is not finite",
            FillDrop::ForeignVenue => "the fill names another venue than this account's",
            FillDrop::AlreadyFolded => {
                "its trade id is already folded into this account (booked once already, or reused by another fill)"
            }
            FillDrop::BeforeDedupWindow => {
                "it carries no timestamp, or one before the days of fills this account's ledger remembers, so it \
                 cannot be told from a fill booked before them"
            }
        }
    }
}

impl<C: ExecutionClient> Engine<C> {
    /// Most-recent live order on this symbol/side — the leg a liquidation force-closed.
    /// One-way (BOTH) keys by symbol only; hedge (LONG/SHORT) also requires the order's leg
    /// (from request.side: +1 → LONG, -1 → SHORT) to match. Reversed insertion-order scan,
    /// first non-terminal wins. None if nothing matches (Account still flattens by key).
    fn order_client_id_for_position(&self, event: &PositionLiquidated) -> Option<String> {
        let want_side = match event.position_side {
            PositionSide::Long | PositionSide::Short => Some(event.position_side),
            PositionSide::Both => None,
        };
        for (order_client_id, mo) in self.registry.iter().rev() {
            // ⚠ POSITIVE selection on the FSM's OWN allowed-from set, not a negation of a few
            // statuses: newest-first, an order `order.rs`'s `transition` refuses for
            // `OrderLiquidated` (e.g. still SUBMITTED) would shadow the ACCEPTED leg actually
            // force-closed. `can_receive_liquidation` makes the picker and the applier one
            // predicate, so they cannot drift.
            if mo.request.symbol != event.symbol || !mo.status.can_receive_liquidation() {
                continue;
            }
            if let Some(want) = want_side {
                let order_leg =
                    if mo.request.side > 0 { PositionSide::Long } else { PositionSide::Short };
                if order_leg != want {
                    continue;
                }
            }
            return Some(order_client_id.clone());
        }
        None
    }

    /// The tail of the fold for an `OrderAccepted` that just landed on an order still holding what it kept
    /// while it waited (W19g): replay it (`OrderRecord::replay_deferred`: a fill, cancel or expiry that
    /// came BEFORE the answer), mark the replayed fills in the FSM-side dedup set (so the venue's
    /// redelivery of one is dropped, not counted twice), and tell the strategy the answer and then each
    /// replayed outcome, in that order. Takes the place of the ordinary tail, which it must not repeat for
    /// the answer.
    ///
    /// Two callers, both COLD and both in `on_event`'s order arm: the bare `OrderAccepted` that
    /// `OrderRecord::apply_over_lost_prefix` just landed on an `Initialized` order (the `Err` branch), and
    /// an ORDINARY `Submitted -> Accepted` walk that finds the buffer non-empty (the `else` of that branch:
    /// the fill came first, then `OrderSubmitted`, then the accept, so the order was never `Initialized` at
    /// its accept). Deterministic: a pure function of the order's own state, which the `Snapshot` carries, so a
    /// refold lands what the live run did.
    #[cold]
    #[inline(never)]
    fn land_lost_prefix_answer(&mut self, order_client_id: &str, answer: &Event) -> EventBusFold {
        let Some(mo) = self.registry.get_mut(order_client_id) else {
            return EventBusFold::Dropped; // unreachable (the caller just applied to it) — but never panic in the fold
        };
        let seen = &self.seen_fsm_trade_ids;
        // each kept fill is averaged by its OWN symbol's value, as the ordinary arm does (a combo leg's)
        let account = &self.account;
        let value_of = |s: &str| account.contract_value_of(s);
        let replayed = mo.replay_deferred(value_of, |tid| seen.contains_key(tid));
        let day = crate::account::fill_window::day_of(self.now_ms);
        let (venue, symbol) = (mo.request.venue.clone(), mo.request.symbol.clone());
        for event in &replayed {
            let tid = match event {
                Event::OrderPartiallyFilled(w) => Some(&w.fill.trade_id),
                Event::OrderFilled(w) => Some(&w.fill.trade_id),
                // every other event: carries no fill
                _ => None,
            };
            if let Some(tid) = tid {
                self.seen_fsm_trade_ids.insert(tid.as_str(), day);
            }
        }
        if self.collect_applied_fills {
            for event in std::iter::once(answer).chain(replayed.iter()) {
                if let Some(lc) = vike_model::OrderEvent::from_event(event) {
                    self.order_events.push(OrderEventOut {
                        venue: venue.clone(),
                        symbol: symbol.clone(),
                        event: lc,
                    });
                }
            }
        }
        EventBusFold::Applied
    }

    fn lifecycle_order_client_id(event: &Event) -> Option<&str> {
        // OrderLiquidated is NOT lifecycle-dispatched (the PositionLiquidated branch applies it).
        match event {
            Event::OrderSubmitted(e) => Some(&e.order_client_id),
            Event::OrderAccepted(e) => Some(&e.order_client_id),
            Event::OrderTriggered(e) => Some(&e.order_client_id),
            Event::OrderPartiallyFilled(e) => Some(&e.order_client_id),
            Event::OrderFilled(e) => Some(&e.order_client_id),
            Event::OrderCanceled(e) => Some(&e.order_client_id),
            Event::OrderRejected(e) => Some(&e.order_client_id),
            Event::OrderExpired(e) => Some(&e.order_client_id),
            Event::OrderDenied(e) => Some(&e.order_client_id),
            // RUST-NATIVE modify — folds via `mo.apply` (self-transition) + persist_order.
            Event::OrderModified(e) => Some(&e.order_client_id),
            // RUST-NATIVE cancel/modify-reject advisories — a non-terminal self-transition, so the
            // failed intent is journaled.
            Event::OrderCancelRejected(e) => Some(&e.order_client_id),
            Event::OrderModifyRejected(e) => Some(&e.order_client_id),
            Event::Fill(_)
            | Event::OrderLiquidated(_)
            | Event::PositionOpened(_)
            | Event::PositionChanged(_)
            | Event::PositionClosed(_)
            | Event::AccountState(_)
            | Event::Funding(_)
            | Event::PositionLiquidated(_)
            // a fee correction of a folded fill: the account's ledger, not the order's FSM
            | Event::FeeAdjustment(_) => None,
        }
    }

    /// order-client-id-ownership routing for a bare [`Event::Fill`] whose symbol is NOT mounted (the
    /// fill-drop hazard). True iff the fill names an order THIS engine manages (registry hit on
    /// its `order_client_id`) AND carries position truth for that order — the order's own
    /// `request.symbol` (a single-leg order on a never-mounted instrument, e.g. a deribit option
    /// ticket) or one of its `combo_legs` symbols (the real per-leg position deltas). Without it
    /// such a fill is symbol-filtered: order Filled via the wrap, `Account` flat.
    ///
    /// The combo-INSTRUMENT net print matches NEITHER arm (a combo request's `symbol` is EMPTY by
    /// `build_combo`'s contract, and the venue-minted combo id is not a leg) and deliberately
    /// stays OUT of the fold: Deribit reports one combo execution as N leg rows PLUS one
    /// aggregate net-price row under the combo instrument, and books NO position under the combo
    /// id (`vike-deribit`'s `deribit_combo_fill_probe`). Folding the net print would mint a
    /// phantom position at the net price and double-count PnL.
    ///
    /// Hot-fold budget: mounted-symbol fills short-circuit at [`Self::accepts_symbol`];
    /// label-less fills return on the empty check without hashing; only LABELED foreign-symbol
    /// fills pay the one registry probe. No logging on any path (the p99 gate).
    fn owns_fill_symbol(&self, fill: &FillEvent) -> bool {
        if fill.order_client_id.is_empty() {
            return false;
        }
        let Some(mo) = self.registry.get(&fill.order_client_id) else {
            return false;
        };
        let symbol = fill.symbol.as_str();
        symbol == mo.request.symbol || mo.request.combo_legs.iter().any(|l| l.symbol == symbol)
    }

    /// **Why the fold would DROP `fill` now, or `None` when it would book it**: the `Fill` arm's guards of
    /// [`EventHandler::on_event`] in the SAME order, read without moving anything (the dedup half is
    /// `Account::apply_fill`'s). For a caller that must not announce a fold that does not happen: the reconcile
    /// confirm asks it of every fill a held row proposes (`vike_core`'s `confirm_reconcile`). Cold, never on the fold;
    /// `crates/vike-execution/tests/engine/fill_drop_reason.rs` pins each answer against what `on_event` does.
    #[cold]
    pub fn fill_drop_reason(&self, fill: &FillEvent) -> Option<FillDrop> {
        let accepted = self.accepts_symbol(&fill.symbol);
        let owned = self.owns_fill_symbol(fill);
        if !accepted && !owned {
            return Some(FillDrop::UntradedSymbol);
        }
        if self.is_hand_trade(fill) {
            return Some(FillDrop::HandTrade);
        }
        if self.account.kind_is_unknown_for(&fill.symbol) {
            return Some(FillDrop::UnknownContractKind);
        }
        if !accepted && self.account.has_foreign_quote() {
            return Some(FillDrop::UnknownQuoteCurrency);
        }
        if accepted
            && self.account.has_foreign_quote()
            && self.account.quote_unrated(&fill.symbol)
            && !owned
        {
            return Some(FillDrop::UnratedQuote);
        }
        if !fill.numbers_finite() {
            return Some(FillDrop::NonFinite);
        }
        if fill.venue != self.account.venue {
            return Some(FillDrop::ForeignVenue);
        }
        if self.account.has_folded_fill(fill.trade_id.as_str()) {
            return Some(FillDrop::AlreadyFolded);
        }
        if self.account.before_fill_floor(fill.timestamp)
            || self.account.unprovable_fill(fill.timestamp)
        {
            return Some(FillDrop::BeforeDedupWindow);
        }
        None
    }

    /// **A trade placed by hand on a spot pair** (the spot rule, owner 2026-11-11: a hand trade in the venue's app is
    /// NOT the strategy's): a fill that names no order client id, on a symbol booked `Cash`. Every order this
    /// workspace places carries an id, and so does every fill it makes itself (a reconcile leg `EXT-…`, a settlement
    /// `resolution:…`); each spot bridge gives a fill of an order placed by hand none (bybit, okx: the venue sends
    /// none; binance and aster: an id no generator of ours mints; hyperliquid: no cloid). The empty test comes
    /// first, so a fill of ours never reaches the mode lookup, and an engine with no `Cash` row answers with two
    /// `is_empty` tests (`Account::default_margin_mode_of`).
    #[inline]
    fn is_hand_trade(&self, fill: &FillEvent) -> bool {
        fill.order_client_id.is_empty()
            && self.account.default_margin_mode_of(&fill.symbol) == vike_model::MarginMode::Cash
    }

    /// KEEP one hand trade on a spot pair OUT of the book ([`Self::is_hand_trade`]): nothing moves and its `trade_id`
    /// is not burned; the wallet moved and this ledger did not (`Account::note_unfolded_cash`, so the next cash pass
    /// adopts the quote coin the trade moved). Said at `info!`, once per such fill. `#[cold]` + `#[inline(never)]`:
    /// a hand trade is rare, never a per-message cost.
    #[cold]
    #[inline(never)]
    fn keep_hand_trade_out(&mut self, fill: &FillEvent) -> EventBusFold {
        tracing::info!(
            target: "vike_execution::account",
            venue = %fill.venue,
            symbol = %fill.symbol,
            trade_id = %fill.trade_id,
            "a fill with no order client id on a spot pair (a trade placed by hand in the venue's app) is the \
             user's, not the strategy's: it is not folded into the strategy's position"
        );
        self.account.note_unfolded_cash();
        EventBusFold::Dropped
    }

    /// PARK one fill the account refused as [`crate::account::FillFold::Unproven`] (no stamp, an id the ledger
    /// does not hold, a floor standing) for the core, which holds it as a `MissingFill` row for the operator
    /// (`vike_core`'s `hold_unproven_fills`): it may be a fill the venue sends again after the `Snapshot` window
    /// left its id out, so it is never booked, and an operator's confirm of the row is refused in words
    /// ([`Self::fill_drop_reason`]). Said once at `error!` per fill (a malformed venue frame, never routine);
    /// the same trade id parked twice is one row; past [`MAX_UNPROVEN_FILLS`] the oldest goes. `#[cold]`.
    #[cold]
    #[inline(never)]
    fn park_unproven_fill(&mut self, fill: &FillEvent) -> EventBusFold {
        if self.unproven_fills.iter().any(|f| f.trade_id == fill.trade_id) {
            return EventBusFold::Dropped;
        }
        tracing::error!(
            target: "vike_execution::account",
            venue = %fill.venue,
            symbol = %fill.symbol,
            trade_id = %fill.trade_id,
            order_client_id = %fill.order_client_id,
            "HELD a fill that carries NO timestamp and whose trade id this account's ledger does not hold, while \
             the ledger keeps only a window of days (restored from a bounded journal Snapshot): it cannot be told from \
             a fill booked before the window, so it is NOT booked. It is held for the operator as a reconcile row"
        );
        if self.unproven_fills.len() >= MAX_UNPROVEN_FILLS {
            self.unproven_fills.remove(0);
        }
        self.unproven_fills.push(fill.clone());
        EventBusFold::Dropped
    }

    /// HOLD one externally-originated fill on a symbol the engine declared quoted in another currency and
    /// holds no rate for yet (`Account::quote_unrated`): say so at `error!` and answer [`EventBusFold::Dropped`].
    /// Nothing moves and the `trade_id` is not burned (the guard sits before the dedup insert), so the
    /// venue's own record of the fill is what reconcile raises (`MissingFill`, held for an operator under
    /// the default `quarantine`) and a redelivery once the rate stands folds it at that rate.
    /// `#[cold]` + `#[inline(never)]`: a fault transition, never a per-message cost.
    #[cold]
    #[inline(never)]
    fn hold_unrated_fill(&self, fill: &FillEvent) -> EventBusFold {
        tracing::error!(
            target: "vike_execution::account",
            venue = %fill.venue,
            symbol = %fill.symbol,
            trade_id = %fill.trade_id,
            order_client_id = %fill.order_client_id,
            "HELD a fill that came from outside this engine on a symbol it holds no conversion rate \
             for yet (declared quoted in another currency, not rated): folding it now would book it at \
             the venue's own unit, wrong by the whole rate. Nothing was folded; reconcile raises it \
             and it folds at the rate once the node has rated the symbol"
        );
        EventBusFold::Dropped
    }

    /// Refuse one money-lane event whose folded f64s were not all finite: count it, say so loudly,
    /// and answer [`EventBusFold::Dropped`]. The ONE policy site for [`vike_model::FiniteNumbers`] (the
    /// predicate is pure; this decides what a violation DOES).
    ///
    /// **Drop, not halt**: halting on one malformed frame would hand any single venue a kill
    /// switch over every OTHER venue in the process. Dropping costs this one event, RECOVERABLY —
    /// reconcile raises the gap as `MissingFill` / `PositionDrift` — while a folded NaN propagates
    /// through `compute_fill` into `average_price` and makes every later comparison false, so reconcile
    /// cannot even SEE the divergence.
    ///
    /// **ERROR, not warn**: no venue legitimately sends a non-finite number, so one occurrence is a
    /// venue bug or a compromised socket, and both want an operator.
    ///
    /// `#[cold]` + `#[inline(never)]` keep this formatting off the per-message fold the
    /// `p99 < 10µs` gate measures.
    #[cold]
    #[inline(never)]
    fn refuse_nonfinite(
        &mut self,
        lane: &str,
        symbol: &str,
        order_client_id: &str,
    ) -> EventBusFold {
        self.dropped_nonfinite += 1;
        tracing::error!(
            target: "vike_execution::oms",
            lane,
            venue = %self.venue,
            symbol,
            order_client_id,
            "REFUSED a venue event carrying a NON-FINITE number (NaN/inf) — no venue sends this; \
             the event was dropped so it cannot poison the ledger. Treat a nonzero \
             dropped_nonfinite as a venue fault or a compromised feed, and reconcile the venue."
        );
        EventBusFold::Dropped
    }
}

/// A terminal lifecycle event: one that (in a legal transition) closes the order. Used by the
/// `dropped_terminal_on_live` check to tell a genuinely-lost terminal from a benign replay.
fn is_terminal_event(event: &Event) -> bool {
    event.ended_order().is_some()
}

/// A venue LIVENESS or EXECUTION event: the venue asserting the order is working (`OrderAccepted`)
/// or has executed (`OrderPartiallyFilled`/`OrderFilled`). Distinct from [`is_terminal_event`]:
/// `OrderAccepted`/`OrderPartiallyFilled` are non-terminal, but all three prove the venue considered
/// the order LIVE — so one arriving (and failing to apply) on an already-terminalized order is the
/// stranded-position signal `stranded_terminal_drops` counts.
fn is_liveness_or_fill_event(event: &Event) -> bool {
    matches!(
        event,
        Event::OrderAccepted(_) | Event::OrderPartiallyFilled(_) | Event::OrderFilled(_)
    )
}

/// A KILL terminal: an order closed WITHOUT completing (venue/gate rejected, canceled, expired, or
/// denied it). Excludes `Filled` (legitimate completion — a duplicate fill is benign, not a strand)
/// and `Liquidated` (a live perp force-close, not `is_terminal`). A venue liveness/fill event landing
/// on one of these is a contradiction: the order the venue is executing is one we believe we killed.
fn is_kill_terminal(status: OrderStatus) -> bool {
    matches!(
        status,
        OrderStatus::Rejected | OrderStatus::Canceled | OrderStatus::Expired | OrderStatus::Denied
    )
}

impl<C: ExecutionClient> EventHandler for Engine<C> {
    fn on_event(&mut self, event: &Event, _outbox: &mut EventBusOutbox) -> EventBusFold {
        if let Event::Fill(fill) = event {
            let accepted = self.accepts_symbol(&fill.symbol);
            if !accepted && !self.owns_fill_symbol(fill) {
                // account-wide WS stream: not this engine's order — OR our own combo's aggregate
                // net-price print, which must never fold (see `owns_fill_symbol`). Either way the
                // wallet moved and this ledger did not.
                self.account.note_unfolded_cash();
                return EventBusFold::Dropped;
            }
            // A HAND TRADE on a spot pair is the user's (the spot rule): not folded, `trade_id` not burned.
            if self.is_hand_trade(fill) {
                return self.keep_hand_trade_out(fill);
            }
            // An INVERSE engine cannot value a fill on an instrument it did not read the row of
            // (decision 0170): folding it as `Linear` 1.0 books PnL wrong by the price. Dropped, and
            // said at `error!` (a real fill the ledger will not carry; reconcile re-converges the
            // mounted symbol only).
            if self.account.kind_is_unknown_for(&fill.symbol) {
                tracing::error!(
                    target: "vike_execution::account",
                    venue = %fill.venue,
                    symbol = %fill.symbol,
                    trade_id = %fill.trade_id,
                    "DROPPED a fill on an instrument whose contract kind this inverse engine does not \
                     know — folding it as a linear contract would book its PnL wrong by the price"
                );
                return EventBusFold::Dropped;
            }
            // ...and the same for the CURRENCY. Once this ledger holds a symbol quoted in a currency
            // other than the account's, a fill on a symbol it never admitted (it reached here only
            // by the order client id of an order it owns: a combo leg, an option ticket) has a currency the
            // engine does not know, and folding it at par would book it wrong by the whole rate (150x
            // for a JPY pair). Dropped and said at `error!`; reconcile re-converges the mounted
            // symbol. The test is one `Option` check on the account, so an account with no
            // foreign-quoted symbol never reaches the second half.
            if !accepted && self.account.has_foreign_quote() {
                tracing::error!(
                    target: "vike_execution::account",
                    venue = %fill.venue,
                    symbol = %fill.symbol,
                    trade_id = %fill.trade_id,
                    "DROPPED a fill on a symbol whose quote currency this engine does not know — \
                     folding it as if it were quoted in the account's currency would book its PnL \
                     wrong by the whole conversion rate"
                );
                self.account.note_unfolded_cash();
                return EventBusFold::Dropped;
            }
            // ...and the RATE. A fill on an accepted symbol that is DECLARED quoted in another currency
            // and has no rate row yet (the mounted symbol of a ledger not in its money, in the first
            // seconds after a start) would be booked at the venue's own unit, i.e. at par, wrong by the
            // whole rate. The node's own orders cannot open there (`quote-rate-stale`) and a fill of an
            // order this engine owns (a covered reduce) goes through; one that came from outside it (a
            // venue-UI trade, an order the registry never held) is HELD: not folded, its `trade_id`
            // not burned, so reconcile raises it and a redelivery after the rate lands folds at that
            // rate. One `Option` test for an account with no foreign-quoted symbol, as the guard above.
            if accepted
                && self.account.has_foreign_quote()
                && self.account.quote_unrated(&fill.symbol)
                && !self.owns_fill_symbol(fill)
            {
                return self.hold_unrated_fill(fill);
            }
            // HOSTILE-VENUE GUARD, BEFORE the dedup insert so a rejected fill does not burn its
            // `trade_id` (a well-formed retransmission with the same id must still fold). See
            // `vike_model::FiniteNumbers`.
            if !fill.numbers_finite() {
                return self.refuse_nonfinite("Fill", &fill.symbol, &fill.order_client_id);
            }
            // Reconnect-replay dedup. ⚠ The check is inside `Account::apply_fill`, on the aggregate
            // that owns the money; this site only OBSERVES the verdict, because several paths can
            // deliver an already-folded fill (`exec_actor::run_loop`'s history resync,
            // `run_user_resync`'s replay, hyperliquid's per-reconnect `userFills` snapshot)
            // and a guard in front of the mutation protects only the callers that remember it. The
            // guard is TOTAL: `fill.trade_id` is a `TradeId`, which cannot be empty (#1341).
            //
            // Anything but `Applied` is a refusal and the fill goes no further: a `Duplicate` is a
            // reconnect replay, a `Collision` is a DIFFERENT fill that reused a trade id. Either way the
            // account moved nothing, so no mark, no `AppliedFill` (a strategy's `on_fill` and the
            // per-mount ledger would book what the account does not hold) and no `EventBusFold::Applied`.
            let day = crate::account::fill_window::day_of(self.now_ms);
            match self.account.apply_fill_on_day(fill, day) {
                crate::account::FillFold::Applied => {}
                // No stamp under a floor: nothing proves it new. Parked for the operator, never booked.
                crate::account::FillFold::Unproven => return self.park_unproven_fill(fill),
                _ => return EventBusFold::Dropped, // the account refused it, nothing moved
            }
            if let Some(mp) = fill.mark_price {
                // `mark_price` is deliberately NOT part of `numbers_finite` (a decorative field must
                // not discard a money event); `Account::set_mark_from`, THE single writer of the
                // mark slot, guards it. ⚠ `mp > 0.0` alone does NOT: `+inf > 0.0` is TRUE.
                if mp > 0.0 {
                    // `fill.mark_price` is the venue's mark at fill time — a genuine venue mark.
                    // Aged on the CORE clock (`now_ms`), not `fill.ts`: see `set_mark_from`.
                    self.account.set_mark_from(
                        &fill.venue,
                        &fill.symbol,
                        mp,
                        crate::MarkSource::VenueMark,
                        self.now_ms,
                    );
                    self.price_board.set_mark(&fill.venue, &fill.symbol, mp, fill.timestamp);
                }
            }
            if self.collect_applied_fills {
                // Snapshot the state HERE, per fill — a multi-fill batch must deliver each
                // `on_fill` with the position/equity after THAT fill, exactly matching the
                // backtest engine's synchronous firing point (fold → fire, one at a time).
                // Keyed on the FILL's symbol, not the engine's primary: `position_size` reads
                // `self.symbol`, another instrument on an engine with `extra_symbols`. The `Both`
                // bucket stays on purpose (hedge mode is a separate concern).
                let (position_after, average_after) =
                    self.position_and_average_of(&fill.symbol, PositionSide::Both);
                self.applied_fills.push(AppliedFill {
                    position_after,
                    average_after,
                    // ⚠ CAPPED: a strategy-facing equity (the `ctx.equity` a `Strategy::on_fill`
                    // handler sizes from), computed from `Account::equity_all` rather than
                    // `resolved_equity`, so a grep for the obvious symbol misses it; uncapped it
                    // would be the one hole in the ceiling. `Self::cap_sizing_equity` is the
                    // comparison shared with `Self::sizing_equity`.
                    equity_after: self.cap_sizing_equity(self.account.equity_all(self.equity_seed)),
                    fill: fill.clone(),
                    // What the account folded the fill WITH, for the per-mount ledger; only an
                    // account holding a foreign-quoted symbol pays for it.
                    fx: self.account.has_foreign_quote().then(|| self.account.applied_fx(fill)),
                });
            }
            return EventBusFold::Applied;
        }
        if let Some(order_client_id) = Self::lifecycle_order_client_id(event) {
            let order_client_id = order_client_id.to_string();
            if !self.registry.contains_key(&order_client_id) {
                // A lifecycle event for an unknown order (pre-restart order absent from reconcile,
                // an external-account order, or a bug): dropped, but counted + logged at debug (not
                // warn: a shared exchange account legitimately streams other sources' orders).
                self.dropped_unknown_order_client_id += 1;
                tracing::debug!(target: "vike_execution::execution_engine", order_client_id = %order_client_id, "lifecycle event for unknown order dropped");
                return EventBusFold::Dropped;
            }
            // FSM-side fill dedup: a reconnect-replayed fill re-emits the wrap, which would re-run
            // accumulate_fill and double-count filled_qty/average_fill_price. A SEPARATE set: the
            // Account's ledger was already consumed by the preceding bare FillEvent.
            // `Option<&TradeId>`: `None` means only "this lifecycle event carries no fill", and
            // `Some` is always a real id (an empty-string sentinel once conflated the two).
            let tid: Option<&vike_model::events::TradeId> = match event {
                Event::OrderPartiallyFilled(w) => Some(&w.fill.trade_id),
                Event::OrderFilled(w) => Some(&w.fill.trade_id),
                // every other event: not this tap's business (audited 2026-10, I-21)
                _ => None,
            };
            if let Some(tid) = tid
                && self.seen_fsm_trade_ids.contains_key(tid.as_str())
            {
                return EventBusFold::Dropped; // reconnect replay — the FSM already advanced for this fill
            }
            // HOSTILE-VENUE GUARD, the FSM side: a fill WRAP's embedded `FillEvent` is a second,
            // independent path to a poisoned number (`accumulate_fill` folds it into
            // `filled_qty`/`average_fill_price`), and a NaN `average_fill_price` makes every later
            // `remaining_qty` comparison false, so the order never completes.
            let wrap_fill = match event {
                Event::OrderPartiallyFilled(w) => Some(&w.fill),
                Event::OrderFilled(w) => Some(&w.fill),
                // every other event: not this tap's business (audited 2026-10, I-21)
                _ => None,
            };
            if let Some(f) = wrap_fill
                && !f.numbers_finite()
            {
                return self.refuse_nonfinite("fill wrap", &f.symbol, &order_client_id);
            }
            // The fill's contract averages it into `average_fill_price` (harmonic on an inverse contract):
            // one `Copy` grid lookup, on a fill wrap only.
            let value = wrap_fill.map_or_else(vike_model::ContractValue::default, |f| {
                self.account.contract_value_of(&f.symbol)
            });
            let Some(mo) = self.registry.get_mut(&order_client_id) else {
                return EventBusFold::Dropped; // unreachable (checked above) — but never panic in the fold
            };
            // --- Cancel-vs-fill race guard (LEAN `CancelPendingOrders` semantics) ---
            // A local cancel never mutates status (`cancel_order` is fire-and-forget), but an order
            // can sit at PENDING_CANCEL via venue-status seeding (`reregister_orders` / the
            // journal-view path), and the r5 fixture-pinned FSM table rejects fills from that state
            // and leaves a cancel-reject inert — dropping venue truth and stranding the order until
            // a reconcile reap mislabels a FILLED order CANCELED. So restore the pre-cancel live
            // status HERE, before the one `apply` site (`OrderRecord::resolve_pending_cancel`),
            // and the event applies through the UNCHANGED table. Only a fill wrap or the venue's
            // cancel-reject resolve it; `OrderCanceled` does NOT (PENDING_CANCEL → CANCELED is the
            // legal ack edge). Fold-deterministic: no new state, replay and state-hash unchanged.
            if matches!(
                event,
                Event::OrderPartiallyFilled(_)
                    | Event::OrderFilled(_)
                    | Event::OrderCancelRejected(_)
            ) && mo.resolve_pending_cancel()
            {
                // Per-ORDER fault-adjacent boundary (the race is rare) — never per-message.
                tracing::warn!(
                    target: "vike_execution::oms",
                    order_client_id = %order_client_id,
                    restored = mo.status.as_str(),
                    "cancel-vs-fill race: PENDING_CANCEL order restored to its pre-cancel status so the venue event applies"
                );
            }
            let apply_result = mo.apply_valued(event, value);
            if apply_result.is_err() && mo.status == OrderStatus::Initialized {
                // COLD (an `Err` on an order that never left `Initialized`): the venue's ANSWER may be all
                // that survives of an order whose `OrderSubmitted` was lost (the resolution of an UNKNOWN
                // submit, W19-D1). `apply_over_lost_prefix` walks the missing hop for a bare
                // `OrderAccepted` that names the venue's order; a fill, cancel or expiry that came BEFORE
                // that answer is kept on the order (W19g) and `land_lost_prefix_answer` replays it. No
                // work, no branch beyond the `Err` test above, on the ordinary path (but for the one
                // compare of the `else if` below).
                if matches!(event, Event::OrderAccepted(_)) {
                    if mo.apply_over_lost_prefix(event).is_ok() {
                        return self.land_lost_prefix_answer(&order_client_id, event);
                    }
                } else {
                    mo.defer_refused(event);
                }
            } else if matches!(event, Event::OrderAccepted(_))
                && apply_result.is_ok()
                && !mo.deferred_events.is_empty()
            {
                // COLD (an ordinary accept that finds the order holding a kept event): a fill reached the
                // fold before `OrderSubmitted`, was refused from `Initialized` and kept, and the order then
                // walked `Submitted -> Accepted` the ordinary way, so the branch above never ran for its
                // accept and nothing would replay the kept event (it rested `Accepted` with the venue
                // holding it filled). The same replay, the same rule. The test order matters: the event
                // kind first (false for every other lifecycle event: one compare), the buffer only for an
                // accept, so the ordinary road reads nothing it did not read before.
                return self.land_lost_prefix_answer(&order_client_id, event);
            }
            let status_after = mo.status; // Copy — releases the &mut mo borrow for the counter below
            if apply_result.is_err() {
                // Normally a benign idempotent/out-of-order WS replay — but a TERMINAL event
                // failing on a still-LIVE order is a genuinely-lost terminal (e.g. OrderCanceled
                // before OrderAccepted): the order can stay live forever. Count + warn on THAT case
                // only (recovery needs reconcile).
                if is_terminal_event(event) && !status_after.is_terminal() {
                    self.dropped_terminal_on_live += 1;
                    tracing::warn!(
                        target: "vike_execution::execution_engine",
                        order_client_id = %order_client_id,
                        status = status_after.as_str(),
                        "terminal event dropped on a live order (out-of-order); order may be stranded"
                    );
                } else if is_liveness_or_fill_event(event) && is_kill_terminal(status_after) {
                    // A venue liveness/execution event dropped onto an order we ALREADY killed (the
                    // watchdog stage-2 phantom-reject signature): a live position may be stranded.
                    // The branch above cannot see it (`is_terminal()` is TRUE here). Still dropped;
                    // counted + warned.
                    self.stranded_terminal_drops += 1;
                    tracing::warn!(
                        target: "vike_execution::execution_engine",
                        order_client_id = %order_client_id,
                        status = status_after.as_str(),
                        "venue liveness/fill event dropped onto an already-terminalized order; a live position may be STRANDED (premature terminal, e.g. watchdog phantom-reject)"
                    );
                }
                return EventBusFold::Dropped; // idempotent/out-of-order WS replay — skip (Fix 2)
            }
            // WHY a refused order was refused (`OrderView::reason`): cold, only on a rejection.
            match event {
                Event::OrderRejected(r) => {
                    self.note_rejection(&order_client_id, &r.reason);
                    // The typed "nothing to reduce" (cold, rejections only): the engine parks the
                    // claim, the core's cold sweep holds it for the operator. Nothing is folded here.
                    if r.cause == Some(vike_model::events::RejectCause::NothingToReduce) {
                        self.note_nothing_to_reduce(&order_client_id);
                    }
                }
                Event::OrderDenied(d) => self.note_rejection(&order_client_id, &d.reason),
                // A refused MODIFY leaves the order live with its terms: its reason is what
                // `order ls` shows beside it, until a modify lands (`OrderModified` forgets it).
                Event::OrderModifyRejected(m) => {
                    self.note_rejection(&order_client_id, &m.reason);
                    self.forget_pending_modify(&order_client_id);
                    self.note_amend(&order_client_id, self.now_ms);
                }
                Event::OrderModified(_) => {
                    self.forget_rejection(&order_client_id);
                    self.forget_pending_modify(&order_client_id);
                    self.note_amend(&order_client_id, self.now_ms);
                }
                _ => {}
            }
            if let Some(tid) = tid {
                // mark seen only after a successful apply (`Some` ⇒ a real id), at the day it folded on.
                let day = crate::account::fill_window::day_of(self.now_ms);
                self.seen_fsm_trade_ids.insert(tid.as_str(), day);
            }
            // Capture the NON-FILL lifecycle transition for `Strategy::on_order_event`
            // (`from_event` is None for fills, which take the `on_fill` lane). (venue, symbol)
            // come from the advanced order. A DENIED order is captured at the RiskGate veto site
            // instead (it never enters the registry).
            if self.collect_applied_fills
                && let Some(lc) = vike_model::OrderEvent::from_event(event)
                && let Some((venue, symbol)) = self
                    .registry
                    .get(&order_client_id)
                    .map(|mo| (mo.request.venue.clone(), mo.request.symbol.clone()))
            {
                self.order_events.push(OrderEventOut { venue, symbol, event: lc });
            }
            // The order's death by a fill (`OrderEventKind::Filled`) rides its money: the core's
            // applied-fill delivery tells it when the registry is terminal by then, which holds when
            // the bare fill and this wrap fold in ONE message. A venue whose bare fill came in an
            // EARLIER message (binance's early trade frame, then its order frame) leaves nothing to
            // carry it, and a strategy that frees its child on the death (an executor's slot) would
            // wait for ever. So the wrap that ends the order says it, exactly when its money was
            // already folded and is not still waiting for delivery. Cold: fill wraps only, on an
            // engine a strategy is mounted on.
            if self.collect_applied_fills
                && tid.is_some_and(|t| self.account.has_folded_fill(t.as_str()))
                && !self.applied_fills.iter().any(|af| af.fill.order_client_id == order_client_id)
                && let Some(mo) = self.registry.get(&order_client_id)
                && mo.status == vike_model::OrderStatus::Filled
            {
                self.order_events.push(OrderEventOut {
                    venue: mo.request.venue.clone(),
                    symbol: mo.request.symbol.clone(),
                    event: vike_model::OrderEvent {
                        order_client_id,
                        tag: None,
                        kind: vike_model::OrderEventKind::Filled,
                    },
                });
            }
            return EventBusFold::Applied;
        }
        if let Event::Funding(event_value) = event {
            if !self.accepts_symbol(&event_value.symbol) {
                return EventBusFold::Dropped;
            }
            // …and the ROUTING backstop, the twin of the `AccountState` arm's: a stamped key naming
            // another account of this exchange must not fold here. Load-bearing: `accepts_symbol`
            // is TRUE on both engines when two accounts trade one instrument. An unstamped payload
            // (`None`, every single-account box) skips it.
            if let Some(key) = event_value.route_key
                && key.as_str() != self.route_key
            {
                return EventBusFold::Dropped;
            }
            // HOSTILE-VENUE GUARD: `amount` lands straight on `balance`/`funding_paid`.
            if !event_value.numbers_finite() {
                return self.refuse_nonfinite("Funding", &event_value.symbol, "");
            }
            self.account.apply_funding(event_value);
            // A paper engine's funding clock counts the settlement as booked (`funding.rs`): one
            // `is_empty` on every other engine.
            if !self.funding_clocks.is_empty() {
                self.settle_funding_clock(&event_value.symbol, event_value.timestamp);
            }
            return EventBusFold::Applied;
        }
        if let Event::AccountState(event_value) = event {
            // LABEL vs LABEL, so it reads `venue`, NOT `route_key`: `vike_core`'s `route_event` has
            // ALREADY chosen this engine, and this filters an account-wide stream ("is this
            // payload from my exchange"). `AccountState::venue` is always a canonical roster id,
            // so comparing it against a decorated route_key would silently drop every
            // AccountState for that engine.
            if event_value.venue != self.venue {
                return EventBusFold::Dropped;
            }
            // …and the SECOND filter, about routing, firing only on a stamped `route_key`
            // (`vike_tradehub_core::account_event_sender`). `vike_core::CoreThread::route_event` already
            // sends a stamped payload to its engine; this is the backstop for a key naming NO
            // mounted engine, which falls through to the venue lookup and would otherwise
            // overwrite the DEFAULT account's balance with a labelled account's money. Unstamped
            // (`None`: every single-account box, and every correction `vike_execution::recon::resolve`
            // synthesizes) skips it.
            if let Some(key) = event_value.route_key
                && key.as_str() != self.route_key
            {
                return EventBusFold::Dropped;
            }
            // HOSTILE-VENUE GUARD: an AUTHORITATIVE balance assignment — the most damaging of the
            // four, since it OVERWRITES `balance` outright (and flips `balance_mode`) rather than
            // accumulating into it.
            if !event_value.numbers_finite() {
                return self.refuse_nonfinite("AccountState", "", "");
            }
            let qa = self.quote_asset.clone();
            self.account.apply_account_state(event_value, &qa);
            return EventBusFold::Applied;
        }
        if let Event::PositionLiquidated(event_value) = event {
            if !self.accepts_symbol(&event_value.symbol) {
                self.account.note_unfolded_cash();
                return EventBusFold::Dropped;
            }
            // The routing backstop (as in the funding arm), BEFORE the dedup: `seen_liquidation_ids` is
            // per-engine, so it cannot stand in for routing. The most damaging arm:
            // `apply_liquidation` CLOSES the position and books the realized PnL.
            if let Some(key) = event_value.route_key
                && key.as_str() != self.route_key
            {
                return EventBusFold::Dropped;
            }
            // HOSTILE-VENUE GUARD: `liquidation_price` reaches `compute_fill` and `fee` reaches `balance`.
            if !event_value.numbers_finite() {
                return self.refuse_nonfinite("PositionLiquidated", &event_value.symbol, "");
            }
            // Liquidation dedup: a WS reconnect can replay a partial liq frame. Mirror the
            // FillEvent guard — an empty trade_id skips dedup and always applies (the legacy
            // whole-flatten path); a distinct id closes its own clamped qty exactly once.
            if !event_value.trade_id.is_empty() {
                if self.seen_liquidation_ids.contains(event_value.trade_id.as_str()) {
                    return EventBusFold::Dropped; // reconnect replay — drop
                }
                self.seen_liquidation_ids.insert(event_value.trade_id.to_string());
            }
            self.account.apply_liquidation(event_value);
            if let Some(order_client_id) = self.order_client_id_for_position(event_value)
                && let Some(mo) = self.registry.get_mut(&order_client_id)
            {
                let liquidation = Event::OrderLiquidated(OrderLiquidated {
                    order_client_id: mo.order_client_id().to_string(),
                    liquidation_price: event_value.liquidation_price,
                    timestamp: event_value.timestamp,
                });
                // ⚠ `order_client_id_for_position` selects on the FSM's own allowed-from set, so this cannot
                // be refused; log rather than discard — if it fires, the picker and the applier
                // have drifted apart.
                if let Err(e) = mo.apply(&liquidation) {
                    tracing::warn!(
                        target: "vike_execution::execution_engine",
                        order_client_id = %order_client_id,
                        status = ?mo.status,
                        error = %e,
                        "liquidation leg refused the OrderLiquidated transition — the picker \
                         and the FSM disagree about which orders can receive it"
                    );
                }
            }
            return EventBusFold::Applied;
        }
        // LAST on purpose: an adjustment is rare, and every hotter event (fills, the order lifecycle,
        // funding, account state) has returned above, so this arm costs them nothing.
        if let Event::FeeAdjustment(event_value) = event {
            return self.fold_fee_adjustment(event_value);
        }
        // PositionOpened/Changed/Closed: derived read-model events — no engine state to fold.
        EventBusFold::Dropped
    }
}

impl<C: ExecutionClient> Engine<C> {
    /// Fold one LATER correction of a folded fill's fee (decision 0238): the account books the
    /// difference to what it already booked under the trade id ([`Account::apply_fee_adjustment`]), and
    /// the per-mount ledger is handed the same change as a fee-only [`AppliedFill`] (zero quantity,
    /// [`AppliedFill::is_fee_only`]) so a mount's `fees_paid` follows its account's. No symbol or
    /// route-key filter: the fill ledger the account checks first is the ownership test (a trade id this
    /// account never folded books nothing), and `vike_core` routed the event by its order client id like a fill.
    /// `#[cold]`, out of line: this is not the per-message fold.
    #[cold]
    #[inline(never)]
    fn fold_fee_adjustment(
        &mut self,
        event: &vike_model::events::FeeAdjustmentEvent,
    ) -> EventBusFold {
        // HOSTILE-VENUE GUARD, BEFORE the ledger moves: `extra_commission` lands on `balance`.
        if !event.numbers_finite() {
            return self.refuse_nonfinite("FeeAdjustment", &event.symbol, &event.order_client_id);
        }
        let crate::account::FeeFold::Applied { charge } = self.account.apply_fee_adjustment(event)
        else {
            return EventBusFold::Dropped; // unchanged, unknown fill or foreign venue: nothing moved
        };
        if self.collect_applied_fills {
            let (position_after, average_after) =
                self.position_and_average_of(&event.symbol, PositionSide::Both);
            self.applied_fills.push(AppliedFill {
                fill: event.as_fee_only_fill(),
                position_after,
                average_after,
                equity_after: self.cap_sizing_equity(self.account.equity_all(self.equity_seed)),
                fx: Some(AppliedFx {
                    value: self.account.contract_value_of(&event.symbol),
                    charge,
                }),
            });
        }
        EventBusFold::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One event of a wire tag: each payload reads what it needs of this one body and ignores the rest.
    fn event_of(tag: &str) -> Event {
        serde_json::from_value(serde_json::json!({
            "type": tag, "order_client_id": "c1", "trade_id": "t1", "venue": "v", "symbol": "s",
            "position_side": "BOTH", "qty": 1.0, "average_price": 1.0, "funding_rate": 0.0, "amount": 0.0,
            "liquidation_price": 1.0, "side": 1, "last_qty": 1.0, "last_price": 1.0, "extra_commission": 0.0,
            "fill": { "trade_id": "t1", "order_client_id": "c1", "venue": "v", "symbol": "s",
                      "side": 1, "last_qty": 1.0, "last_price": 1.0 },
        }))
        .unwrap_or_else(|e| panic!("{tag}: {e}"))
    }

    /// `is_terminal_event`'s answers over the whole `Event::TABLE`, so a move cannot change them.
    #[test]
    fn five_variants_of_the_table_are_terminal_and_no_other() {
        let yes: Vec<&str> = Event::TABLE
            .iter()
            .map(|row| row.wire_tag)
            .filter(|tag| is_terminal_event(&event_of(tag)))
            .collect();
        assert_eq!(
            yes,
            ["OrderRejected", "OrderDenied", "OrderFilled", "OrderCanceled", "OrderExpired"]
        );
    }
}
