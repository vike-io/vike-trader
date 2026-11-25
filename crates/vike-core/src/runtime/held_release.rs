//! The HELD-FOR-RELEASE table: an order that carries a `vike_model::OrderRelease` is kept OFF the venue by
//! the core, in `CoreThread::held_releases`, until the release holds, and then sent. The owner's
//! `TimeTriggered` and `Conditional` kinds (a time release, and a time release AND a price condition)
//! are this one table: the venue sees a plain order at the moment it may trade.
//!
//! * **Accept** (`lower_submit`). The order is judged as any other: its account, the ambiguity gate,
//!   the mint and its `MintedSubmit`, a stopped account, and the CAPABILITY PREFLIGHT (decision 0219's
//!   shape: a kind, a lifetime, a flag or a display size the venue cannot take is refused NOW, at
//!   accept, never at the release). Then it is HELD instead of sent: nothing reaches an engine registry,
//!   so the pre-trade gate judges it at the release, against the account as it is then, and so does its
//!   owning mount's LEVERAGE cap (`CoreThread::leverage_refusal_on`, at `CoreThread::release_due`,
//!   against the mount's book then, as the simulator's `release_held` judges it; the drain does not judge
//!   such an order at accept). A refused release is dispatched as a journaled CANCEL of the held order
//!   instead (it ends `canceled before its release`, the mount hears it, the ring line names the cap), so
//!   the replay core, which runs no release timer, re-derives the same end.
//!   `arm_core_held_stop` is not asked for a held order: a reduce-only stop the row refuses is refused
//!   at accept, not armed early. A PAPER engine's order is held here too: the paper book persists
//!   nothing, and this table rides the `Snapshot`.
//! * **A venue that holds a time release itself** (decision 0267's 2.6: a row listing the attribute
//!   word `release`, IB's `goodAfterTime`, on an engine told `native_time_release` by its account's
//!   backend): a release with NO price condition is not held here. `lower_submit` sends it at accept
//!   with its release ([`venue_holds_release`]: the request, the row and the engine's fact, which rides
//!   the `Snapshot`, so the replay core agrees), the pre-trade gate judges it then, and it is an ordinary
//!   resting order from there; vike runs no second check at its release moment. Every release with a
//!   price condition, and every release on a row without the word, is held here as below.
//! * **Release** (`TimerKind::Release`, at the drain-loop boundary, never per message). One timer per
//!   held order at its `at`. When it fires, every order whose release holds goes out in the order of
//!   its release time: its time has come and, for a conditional one, a FRESH price of its symbol on its
//!   own engine's board meets the condition (`CoreThread::release_price`, read at the timer: the side
//!   that has to reach the level, every rung capped at `PriceOnlyConfig::max_price_age`). ⚠ **A stale or
//!   missing price never releases** (owner rule: no trading without a price): the order waits and its
//!   HELD row says why (`OrderHeldView::waiting`, set when the words change, with one ring line). A
//!   condition that NAMES a venue (`OrderReleasePrice::venue`, decision 0267's 2.13: buy on IBKR once
//!   BTCUSDT on binance is at 100000) reads the same rule on the boards of that exchange's running
//!   engines (`CoreThread::release_price_on`: the first fresh one, any account of it), never on the
//!   order's own engine, and is never a venue's own release; with no account of that venue running it
//!   waits and says so. The venue is a price source only, never traded. A
//!   conditional order whose price does not meet it, waits for ONE poll timer every
//!   [`RELEASE_PRICE_POLL_MS`]: the fold reads no price for it, so a condition met between two polls is
//!   seen at the next. A release is
//!   dispatched as one `Command::OrderOn` of the held request (its `release` taken off, its order client id the
//!   one accepted) on its own engine, through `handle`: journaled write-ahead like any command, so the
//!   replay core re-applies it through the same `lower_submit` and needs no timer, no clock and no
//!   price. `lower_submit` reads an order client id this table holds, on a request with no release, as the release:
//!   it drops the entry and sends the order, or refuses it as expired when its `gtd` lifetime ended
//!   while it was held (the expiry wins). A `day` order is registered at its release, so its day counts
//!   from there (`created_ms`, the day sweep's anchor).
//! * **The replay core** runs no release timer (`CoreConfig::release_timer` is false there): a timer
//!   would read a clock stamp per boundary and shift every stamp of the tail it refolds. Its table moves
//!   only through the journaled records, so its exit `Snapshot` is the restore's table.
//! * **Cancel, modify, mass-cancel.** A cancel of a held order client id ends it with no venue call: registered
//!   as the order the node accepted and held, then a terminal `OrderCanceled` its owning mount hears (a
//!   replay re-derives both from the journaled cancel). A mass-cancel or market exit ends the held orders
//!   in its scope the same way. A modify edits the held request in place, its prices through
//!   `vike_model::modified_prices`. A submit that REUSES a held order client id is refused in words (the duplicate
//!   order client id refusal, `refuse_held_release_order_client_id`); only the journaled `OrderOn` of a release takes the
//!   entry out.
//! * **Visible.** Each held order is published in the HELD list (`CoreSnapshot::held_exits`, after the
//!   held bracket exits) with its account and its `release`, which the doors show as HELD and when; never
//!   as a row of `orders`, so it is listed once. One `is_empty` per publish while nothing is held.
//! * **Restart.** The table rides every `Snapshot` (`Snapshot.held_releases`, additive). At assembly each entry
//!   gets its engine row and its timer back; a release that came while the node was down fires at the
//!   first boundary, all of them in release-time order, and an expiry that also passed wins. The board
//!   is not snapped, so a conditional order waits for a price the restarted node reads. Its
//!   ownership line does not make it a restored order for the reconcile pass to judge: the venue holds
//!   no such order yet.
//!
//! Nothing here is on the per-message fold: the submit hop gains one `Option` test and one `is_empty`
//! (and, for an order that carries a release, one capability-row lookup).

use super::*;

/// How often a conditional order whose time has come re-reads its price, in ms: the delay a price
/// condition met between two reads can add. One poll timer for the whole table, never per message.
pub(crate) const RELEASE_PRICE_POLL_MS: i64 = 1_000;

impl<C: ExecutionClient> CoreThread<C> {
    /// Hold `req` (minted, journaled, preflighted on engine `engine_index`) until its release; the caller has
    /// checked `req.release` is `Some`.
    pub(super) fn hold_for_release(&mut self, engine_index: usize, req: OrderRequest) {
        let at = req.release.map_or(i64::MIN, |r| r.at);
        let order_client_id = req.order_client_id.clone();
        self.note(format!("order {order_client_id} HELD: {}", release_words(&req)));
        let route_key = self.engine(engine_index).route_key.to_string();
        self.held_releases
            .insert(order_client_id, vike_journal::OrderHeldRelease { route_key, request: req });
        self.arm_release_timer(at);
    }

    /// Whether `req` (preflighted on the route `routed`) goes to its venue NOW with its release instead of
    /// being held here: [`venue_holds_release`] on the routed engine's capability row AND that engine's
    /// own per-account fact (`Engine::native_time_release`: one ibkr row covers the socket,
    /// which holds a release, and the Client Portal, which does not). Asked by `lower_submit` for an
    /// order that carries a release; one row lookup.
    pub(crate) fn release_held_by_venue(&self, routed: Option<usize>, req: &OrderRequest) -> bool {
        let native = self.engine(routed.unwrap_or(0)).native_time_release();
        let capabilities =
            vike_venues::capabilities_for(self.capabilities_venue(routed, &req.venue));
        venue_holds_release(&capabilities, native, req)
    }

    /// Arm a [`TimerKind::Release`] at `at` (a past one fires at the next boundary), unless this is
    /// the replay core.
    fn arm_release_timer(&mut self, at: i64) {
        if self.config.release_timer {
            self.timers.insert(at, TimerKind::Release);
        }
    }

    /// The release half, asked by `apply_routed_order` alone (the one door a release comes through,
    /// journaled as a `Command::OrderOn`): whether `req` IS the release of an order this table holds (its
    /// order client id, no release left on it), dropping the entry when it is. A plain submit reusing a held order client id is
    /// refused by `lower_submit` instead, so it never releases the held order early.
    pub(crate) fn take_held_release(&mut self, req: &OrderRequest) -> bool {
        !self.held_releases.is_empty()
            && req.release.is_none()
            && !req.order_client_id.is_empty()
            && self.held_releases.shift_remove(&req.order_client_id).is_some()
    }

    /// Refuse a submit that REUSES the order client id of an order held for its release, in words: a WARN and a ring
    /// line and nothing else, as `refuse_held_order_client_id` refuses an answered order client id (an `OrderRejected` under the
    /// order client id would be the held order's). Deterministic: a replay of the journaled submit refuses it too.
    #[cold]
    #[inline(never)]
    pub(crate) fn refuse_held_release_order_client_id(&mut self, order_client_id: &str) {
        let message = format!(
            "order {order_client_id} REFUSED, nothing sent: order client id {order_client_id} is already used by an order held for its \
             release (still live): pick a new order client id, or change that order by this id with \
             modify or cancel"
        );
        tracing::warn!(target: "vike_core::core", order_client_id = %order_client_id, "{message}");
        self.note(message);
    }

    /// The engine a held entry is released onto, by its route key (`None`: no engine carries it).
    fn held_release_engine_index(&self, h: &vike_journal::OrderHeldRelease) -> Option<usize> {
        self.engine_index_for_route_key(RouteKey::declared(&h.route_key))
    }

    /// The FRESH price a held order's condition `c` reads on engine `engine_index`'s board at `now`, or why there
    /// is none, in words. The side that has to reach the level (the bid for above, the ask for below; a
    /// venue mark ranks first and a last trade or a bar close follow, as the resolver ranks them), every
    /// rung capped at the risk clock's age bound (`PriceOnlyConfig::max_price_age`), or at [`MAX_PRICE_AGE`]
    /// itself on a core that configures no `price_only` (the paper rehearsal cores): the one constant the
    /// live root sets that bound from, so every core judges a release alike. A stale or missing price never
    /// releases. The one reader of a release's price, for the order's own venue and for a venue a condition
    /// names. Cold: per waiting order, per poll.
    pub(crate) fn release_price(
        &self,
        engine_index: usize,
        c: vike_model::OrderReleasePrice,
        now: i64,
    ) -> Result<f64, String> {
        let engine = self.engine(engine_index);
        let max_age = self.config.price_only.as_ref().map_or(MAX_PRICE_AGE, |po| po.max_price_age);
        let config = super::watchdog::risk::bounded_config(self.config.price_config, max_age);
        let (venue, symbol) = (engine.venue.as_str(), c.symbol.as_str());
        let above = c.side == vike_model::OrderReleasePriceSide::Above;
        match engine.price_board.classify(venue, symbol, above, now, &config) {
            vike_execution::price_board::MarkStatus::Fresh { price: px, .. } => Ok(px),
            vike_execution::price_board::MarkStatus::Stale { age_ms, .. } => Err(format!(
                "{venue}'s last {symbol} price is {} s old, and a price older than {} s never releases",
                age_ms / 1000,
                max_age.as_secs()
            )),
            vike_execution::price_board::MarkStatus::Missing => {
                Err(format!("no price of {symbol} on {venue} on this node yet"))
            }
        }
    }

    /// A condition that names its PRICE VENUE `venue` (decision 0267's 2.13): the first running engine of
    /// that exchange (any account of it: a venue price belongs to every account, `mirror_venue_price`) whose
    /// board reads a FRESH price by [`Self::release_price`], else why none does: the first engine's words, or
    /// that no account of the venue runs here. The venue is a price source only, never traded. Cold: per
    /// waiting order, per poll.
    fn release_price_on(
        &self,
        venue: &str,
        c: vike_model::OrderReleasePrice,
        now: i64,
    ) -> Result<f64, String> {
        let mut first = None;
        for i in (0..=self.extra_engines.len()).filter(|&i| self.engine(i).venue == venue) {
            if self.is_retired(i) {
                continue;
            }
            match self.release_price(i, c, now) {
                Ok(px) => return Ok(px),
                Err(why) => {
                    first.get_or_insert(why);
                }
            }
        }
        Err(first.unwrap_or_else(|| {
            format!(
                "no {venue} account runs on this node now, so its {} price is not read",
                c.symbol
            )
        }))
    }

    /// [`TimerKind::Release`]: send every held order whose release holds at `now`, in release-time
    /// order (a tie keeps the held order), each through `handle` as its own journaled `OrderOn`. A
    /// conditional one whose FRESH price does not meet its condition yet, or that has no fresh price
    /// ([`Self::release_price`]), keeps one poll timer armed; the second case is said on its HELD row.
    pub(crate) fn release_due(&mut self, now: i64) {
        let mut due: Vec<(i64, String)> = Vec::new();
        let mut waiting = false;
        // The waiting words that change at this poll (`None`: nothing to say any more).
        let mut said: Vec<(String, Option<String>)> = Vec::new();
        for (order_client_id, h) in &self.held_releases {
            let Some(r) = h.request.release else { continue };
            if r.at > now {
                continue; // its own timer is armed
            }
            let read = r.price.map(|c| match (c.venue, self.held_release_engine_index(h)) {
                (Some(v), _) => self.release_price_on(v.as_str(), c, now),
                (None, Some(i)) => self.release_price(i, c, now),
                (None, None) => Err(format!(
                    "its account {} runs no engine on this node now, so its {} price is not read",
                    h.route_key, c.symbol
                )),
            });
            let px = read.as_ref().and_then(|p| p.as_ref().ok().copied());
            if r.holds(now, |_| px) {
                due.push((r.at, order_client_id.clone()));
                continue;
            }
            waiting = true;
            let why = read.and_then(Result::err);
            if self.release_waiting.get(order_client_id) != why.as_ref() {
                said.push((order_client_id.clone(), why));
            }
        }
        for (order_client_id, why) in said {
            self.dirty = true;
            match why {
                Some(w) => {
                    self.note(format!("order {order_client_id} still HELD: {w}"));
                    self.release_waiting.insert(order_client_id, w);
                }
                None => {
                    self.release_waiting.remove(&order_client_id);
                }
            }
        }
        due.sort_by_key(|(at, _)| *at);
        for (_, order_client_id) in due {
            self.release_waiting.remove(&order_client_id);
            let Some(h) = self.held_releases.get(&order_client_id) else { continue };
            let route_key = h.route_key.clone();
            let mut req = h.request.clone();
            req.release = None;
            // A mount's held order meets its LEVERAGE cap now, ALONE, against the mount's book as it stands
            // (the simulator's `release_held` judges it there too). A refusal ENDS it through a journaled
            // cancel, which a replay re-applies, and the ring says why.
            let owner = self.order_client_id_mount.get(&order_client_id).copied();
            let refusal = owner.zip(self.held_release_engine_index(h)).and_then(|(m, e)| {
                let terms = (req.price, req.trigger_price);
                let discretion = req.attributes.and_then(|a| a.discretionary_amount);
                let leg = vike_execution::risk::MountLeverageLeg {
                    side: req.side,
                    quantity: req.qty,
                    price: vike_execution::risk::mount_leverage_price(req.side, terms, discretion),
                    ..Default::default()
                };
                let legs = [leg];
                let order = [(e, req.venue.as_str(), req.symbol.as_str(), &legs[..])];
                self.leverage_refusal_on(m, &order)
            });
            let intent = match refusal {
                Some(why) => {
                    self.note(format!("order {order_client_id} NOT RELEASED: {why}"));
                    OrderIntent::Cancel(order_client_id.clone())
                }
                None => OrderIntent::Submit(Box::new(req)),
            };
            let order = vike_execution::RoutedOrder { route_key: route_key.clone(), intent };
            let _ = self.handle(Ingest::Command(Command::OrderOn(Box::new(order))));
            if self.held_releases.contains_key(&order_client_id) {
                // `apply_routed_order` found no engine for the key, and the replay core finds none
                // either: the order stays held (asked again at the next release), never dropped unsaid.
                self.note(format!(
                    "order {order_client_id} NOT RELEASED: its account {route_key} runs no engine; it stays held \
                     until that account runs or it is cancelled"
                ));
            }
        }
        if waiting && self.release_poll_due.is_none_or(|t| t <= now) {
            let at = now.saturating_add(RELEASE_PRICE_POLL_MS);
            self.release_poll_due = Some(at);
            self.arm_release_timer(at);
        }
    }

    /// Cancel a HELD order, with no venue call. Returns `true` iff `order_client_id` was held, so the cancel caller
    /// skips the venue path. The order ENDS where its owner hears it ([`Self::end_held_release`]).
    pub(crate) fn cancel_held_release(&mut self, order_client_id: &str) -> bool {
        !self.held_releases.is_empty()
            && self.end_held_release(order_client_id, vike_model::RELEASE_CANCELED_REASON)
    }

    /// Drop the held order `order_client_id` and END it: registered on its engine as the order the node accepted
    /// and held, then a terminal `OrderCanceled` with `reason`, through the lane a venue's cancel takes
    /// (the owning mount's lifecycle, the order list), and one ring line. A replay re-derives both from the
    /// journaled cancel it re-applies. An order whose account runs no engine is only said. A held
    /// bracket exit keeps its own shape (`cancel_held`: no event). `false` iff `order_client_id` was not held.
    fn end_held_release(&mut self, order_client_id: &str, reason: &str) -> bool {
        let Some(h) = self.held_releases.shift_remove(order_client_id) else { return false };
        self.release_waiting.remove(order_client_id);
        let Some(engine_index) = self.held_release_engine_index(&h) else {
            self.note(format!(
                "held order {order_client_id} {reason} (its account {} runs no engine)",
                h.route_key
            ));
            return true;
        };
        let now = self.engine(engine_index).now_ms;
        let mut mo = vike_model::OrderRecord::new(h.request);
        mo.status = vike_model::OrderStatus::Accepted; // accepted by the node, which held it
        mo.created_ms = Some(now);
        self.engine_mut(engine_index).registry.insert(order_client_id.to_string(), mo);
        self.note(format!("held order {order_client_id} {reason}"));
        self.publish_synthesized(
            engine_index,
            Event::OrderCanceled(vike_model::events::OrderCanceled {
                order_client_id: order_client_id.to_string(),
                reason: reason.into(),
                timestamp: now,
                post_only_would_cross: false,
                refusal_id: None,
            }),
            None,
        );
        true
    }

    /// Modify a HELD order in place (it is not at a venue): the quantity, and the prices where
    /// `vike_model::modified_prices` lands them on its kind. The pre-trade gate judges the new terms at
    /// the release. Returns `true` iff `order_client_id` was held.
    pub(crate) fn modify_held_release(
        &mut self,
        order_client_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
    ) -> bool {
        // An adjustable stop's price is never changed (`ADJUSTABLE_AMEND_REASON`, the words every book
        // refuses it in); its size moves, here as in place.
        if (new_price.is_some() || new_trigger_price.is_some())
            && self.held_releases.get(order_client_id).is_some_and(|h| h.request.adjust.is_some())
        {
            let why = vike_execution::execution_engine::ADJUSTABLE_AMEND_REASON;
            self.note(format!("held order {order_client_id} NOT modified: {why}"));
            return true;
        }
        let Some(h) = self.held_releases.get_mut(order_client_id) else { return false };
        let req = &mut h.request;
        if let Some(q) = new_qty {
            req.qty = q;
        }
        let (price, trigger) =
            vike_model::modified_prices(&req.order_type, new_price, new_trigger_price);
        if let Some(p) = price {
            req.price = Some(p);
        }
        if let Some(t) = trigger {
            req.trigger_price = Some(t);
        }
        self.note(format!("held order {order_client_id} modified in place"));
        true
    }

    /// Drop every held order in a `(venue, symbol)` scope (`None` = any), narrowed to the orders of
    /// `engines` when the payload named an account — the mass-cancel twin of
    /// [`Self::cancel_held_release`], scoped as `clear_held_scope` scopes the held bracket exits.
    pub(crate) fn clear_held_releases(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        if self.held_releases.is_empty() {
            return;
        }
        let doomed: Vec<String> = self
            .held_releases
            .iter()
            .filter(|(_, h)| {
                venue.is_none_or(|v| h.request.venue == v)
                    && symbol.is_none_or(|s| h.request.symbol == s)
                    && engines.is_none_or(|set| {
                        self.held_release_engine_index(h).is_some_and(|i| set.contains(&i))
                    })
            })
            .map(|(c, _)| c.clone())
            .collect();
        for c in &doomed {
            self.end_held_release(c, "canceled before its release (mass-cancel)");
        }
    }

    /// The held orders as rows of the published HELD list (`CoreSnapshot::held_exits`, beside the held
    /// bracket exits): each with its account and its release, which the doors show as HELD and when.
    /// Called from `publish` only while something is held.
    pub(crate) fn held_release_views(&self) -> Vec<vike_execution::OrderHeldView> {
        self.held_releases
            .values()
            .map(|h| {
                let r = &h.request;
                vike_execution::OrderHeldView {
                    order_client_id: r.order_client_id.clone(),
                    venue: r.venue.clone(),
                    symbol: r.symbol.clone(),
                    side: r.side,
                    qty: r.qty,
                    order_type: r.order_type.clone(),
                    price: r.price,
                    trigger_price: r.trigger_price,
                    parent_order_id: None,
                    account: vike_model::accounts::account_keys::label_of_route_key(
                        &r.venue,
                        &h.route_key,
                    )
                    .filter(|l| !l.is_default()),
                    release: r.release,
                    waiting: self.release_waiting.get(&r.order_client_id).cloned(),
                    // Its terms: `Copy`s of an `Option` each, no allocation.
                    attributes: r.attributes,
                    trail_percent: r.trail_percent,
                    adjust: r.adjust,
                    display_quantity: r.display_quantity,
                }
            })
            .collect()
    }

    /// Assembly: give each restored held order its engine row and its timer back. A release that came
    /// while the node was down fires at the first boundary (in release-time order, `release_due`).
    /// Cold: once per boot.
    pub(super) fn rehold_restored_releases(&mut self) {
        if self.held_releases.is_empty() {
            return;
        }
        // Published at the first idle moment, so the held rows are listed before any message arrives.
        self.dirty = true;
        let entries: Vec<(String, Option<usize>, i64)> = self
            .held_releases
            .iter()
            .map(|(c, h)| {
                (
                    c.clone(),
                    self.held_release_engine_index(h),
                    h.request.release.map_or(0, |r| r.at),
                )
            })
            .collect();
        for (order_client_id, engine_index, at) in &entries {
            if let Some(i) = *engine_index
                && self.order_client_id_row_needed(i)
            {
                self.order_client_id_venue.insert(order_client_id.clone(), i);
            }
            self.arm_release_timer(*at);
        }
        self.note(format!(
            "RESTORE HELD: {} order(s) held for their release are held again ({})",
            entries.len(),
            entries.iter().map(|(c, ..)| c.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
}

/// **Whether the VENUE holds `req`'s release itself** (decision 0267's 2.6, the owner's answer A, "it has
/// to be under IBKR control"): the row `capabilities` lists the attribute word `release` (IB's `goodAfterTime` on
/// the socket backend), the engine's account backend says it holds one (`native`, the engine's
/// `native_time_release` fact: the row alone would send a Client Portal account's release, which shares
/// ibkr's row, to a bridge that refuses it), and the release states NO price condition. Then the core
/// does not hold the order:
/// `lower_submit` sends it at once with its release, the pre-trade gate judges it at accept, and it rests
/// at the venue as any order does (a cancel, a mass-cancel and a HALT reach it there; a restart never
/// cancels it). A price condition is always held here, on its own venue or another one. It reads the
/// request, the capability table and the engine's fact (which `EngineSnapshot::native_time_release`
/// carries), so the replay core, which re-applies the submit through `lower_submit`, decides the same.
pub(crate) fn venue_holds_release(
    capabilities: &vike_venues::VenueCapabilities,
    native: bool,
    req: &OrderRequest,
) -> bool {
    native
        && req.release.is_some_and(|r| r.price.is_none())
        && capabilities.order_attributes.contains(&"release")
}

/// When a held request releases, in words: `at <ms>`, and its price condition when it states one (with
/// the venue it is read on when that is not the order's own).
fn release_words(req: &OrderRequest) -> String {
    let Some(r) = req.release else { return "released".to_string() };
    match r.price {
        None => format!("releases at {} (epoch ms)", r.at),
        Some(c) => format!(
            "releases at {} (epoch ms) once {}{} is {} {}",
            r.at,
            c.symbol,
            c.venue.map_or_else(String::new, |v| format!(" on {v}")),
            match c.side {
                vike_model::OrderReleasePriceSide::Above => "at or above",
                vike_model::OrderReleasePriceSide::Below => "at or below",
            },
            c.level
        ),
    }
}
