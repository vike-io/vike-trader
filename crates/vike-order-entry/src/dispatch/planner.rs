//! The dispatcher's accumulator ([`plan_dispatch`](super::plan_dispatch)'s): each intent of the frame
//! becomes a command through the planner's two order writes, admit and admit_bracket, which both
//! validate, or a reject that says why. Private on purpose: nothing outside the dispatcher writes a
//! command.

use super::order::{self, OrderEntryDispatchOrder};
use super::{
    ORDER_CLIENT_ID_COCKPIT, ORDER_CLIENT_ID_DOM, ORDER_CLIENT_ID_OPTIONS, ORDER_CLIENT_ID_TRADE,
    OrderEntryDispatchReject, OrderEntryDispatchRejectReason, OrderEntryDispatchSource, block_at,
    bracket_lane_holds_stop, bracket_on_mounted_symbol, contract_value_at, order_client_id_intent,
    order_client_id_plan, orders_name_their_account, untradable, window_orders, wire_account,
};
use crate::cockpit::CockpitCommand;
use crate::option_chain::ticket::OptionChainTicket;
use crate::trade_window::address::TradeWindowAddress;
use crate::trade_window::{TradeWindowAction, TradeWindowOrigin, TradeWindowTicketKind};
use vike_execution::{Command, CoreSnapshot, OrderIntent};
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::orders::client_id::{
    OrderClientIdGenerator, OrderClientIdLabels, OrderClientIdSource, OrderClientIdStrategyCode,
};
use vike_tradehub_client::order_checks::{self, OrderLimits};

/// The accumulator. Private on purpose: [`OrderEntryDispatchPlanner::admit`] / [`OrderEntryDispatchPlanner::admit_bracket`] are the
/// ONLY ways an order write reaches `commands`, and both validate.
pub(super) struct OrderEntryDispatchPlanner<'a> {
    pub(super) limits: &'a OrderLimits,
    pub(super) snapshot: &'a CoreSnapshot,
    pub(super) shot_n: u32,
    pub(super) next_window_number: u32,
    /// The process's mint; `None` = the deterministic spelling (see [`plan_dispatch`](super::plan_dispatch)).
    pub(super) order_client_ids: Option<&'a mut OrderClientIdGenerator>,
    /// [`OrderEntryDispatchInputs::bracket_stem`](super::OrderEntryDispatchInputs::bracket_stem).
    pub(super) bracket_stem: bool,
    pub(super) commands: Vec<Command>,
    pub(super) rejects: Vec<OrderEntryDispatchReject>,
}

impl OrderEntryDispatchPlanner<'_> {
    /// Stamp `req`'s order client id (its venue, account, symbol, side and reduce-only are read),
    /// advancing `next_window_number` (see [`plan_dispatch`](super::plan_dispatch) for the three spellings). A v2 session mints `G…`
    /// through the plan of the block `req` reaches. A v1 session keeps the origin prefix first, as an
    /// operator reads it off the working-orders table, and the generator's own `<8-hex-session><seq>`
    /// follows it with no separator, a fixed-width session keeping that unambiguous.
    fn order_client_id(&mut self, prefix: &str, req: &mut OrderRequest) {
        let n = self.next_window_number;
        self.next_window_number += 1;
        req.order_client_id = match self.order_client_ids.as_deref_mut() {
            Some(mint) if mint.is_v2() => {
                let block = block_at(self.snapshot, &req.venue, req.account.as_ref());
                let intent = order_client_id_intent(
                    block,
                    &req.venue,
                    &req.symbol,
                    req.side,
                    req.reduce_only,
                );
                order_client_id_plan(block).mint(
                    mint,
                    OrderClientIdLabels::new(
                        OrderClientIdSource::Gui,
                        OrderClientIdStrategyCode::NONE,
                        intent,
                    ),
                )
            }
            Some(mint) => format!("{prefix}{}", mint.generate()),
            None => format!("{prefix}-{n}-{}", self.shot_n),
        };
    }

    /// **THE CHOKEPOINT.** Every single-order write goes through here, and here is where the local
    /// preview runs. Notional is measured WITH the instrument's contract value, read in the book of the
    /// ACCOUNT the order names ([`contract_value_at`]), so the UI cap measures the same quantity
    /// `RiskGate` and `OrderBrokerSimulator` do; a symbol absent from the grid resolves to the default (`Linear`,
    /// 1.0), i.e. the plain `validate` verdict.
    fn admit(&mut self, source: OrderEntryDispatchSource, req: OrderRequest) {
        let value = contract_value_at(self.snapshot, &req.venue, req.account.as_ref(), &req.symbol);
        // Bound to a `let` so the `&req` borrow is over before the Err arm moves out of `req`.
        let verdict = order_checks::validate_with_value(&req, self.limits, value);
        match verdict {
            Ok(()) => self.commands.push(Command::Order(OrderIntent::Submit(Box::new(req)))),
            Err(reject) => self.rejects.push(OrderEntryDispatchReject {
                source,
                venue: req.venue,
                symbol: req.symbol,
                account: req.account,
                reason: OrderEntryDispatchRejectReason::Preview(reject),
            }),
        }
    }

    /// The bracket chokepoint. A bracket is not one order — the runtime lowers it into THREE
    /// (`vike_model::build_bracket`: OTO entry + OCO stop-loss + OCO take-profit, all at `qty`), so
    /// all three legs are validated and the whole bracket is refused if ANY leg fails. Validating
    /// the legs the runtime will actually build (rather than an approximation of the entry) is what
    /// makes this honest: it catches a fat-fingered TP/SL price and a non-finite exit level, and it
    /// gives a MARKET-entry bracket a notional cap through its priced take-profit leg that a plain
    /// market order does not get.
    ///
    /// Refusal is ATOMIC — nothing is emitted, so no leg can be stranded by a partial reject.
    ///
    /// The legs' order client ids here are placeholders: [`vike_tradehub_client::order_checks::validate_with_value`]
    /// reads only `qty`/`price`/`trigger_price`/`order_type`, never the id. An ADMITTED bracket names
    /// its group STEM (decision 0270's 4.1: `G`, no strategy, the block's account code, one sequence
    /// number) when the mint is a v2 session and the node takes it ([`OrderEntryDispatchInputs::bracket_stem`](super::OrderEntryDispatchInputs::bracket_stem));
    /// the node derives `…EN`, `…SL`, `…TP1` from it and refuses the bracket whole when one is in use,
    /// so a resent bracket cannot double. Otherwise the runtime mints the three ids at `apply_intent`.
    /// Either way the bracket path consumes NO `next_window_number`, exactly as the pre-extraction site did.
    ///
    /// `account` is the window address's, carried by a refusal exactly as `OrderEntryDispatchPlanner::reject` carries it
    /// (M-4 of the A4 review); the spec's own `account` is the WIRE spelling of it ([`wire_account`]).
    fn admit_bracket(
        &mut self,
        source: OrderEntryDispatchSource,
        mut spec: vike_model::OrderBracket,
        account: Option<AccountLabel>,
    ) {
        let value =
            contract_value_at(self.snapshot, &spec.venue, spec.account.as_ref(), &spec.symbol);
        let legs = vike_model::build_bracket(&spec, "entry", "sl", "tp");
        for leg in legs.iter() {
            if let Err(reject) = order_checks::validate_with_value(leg, self.limits, value) {
                self.rejects.push(OrderEntryDispatchReject {
                    source,
                    venue: spec.venue.clone(),
                    symbol: spec.symbol.clone(),
                    account,
                    reason: OrderEntryDispatchRejectReason::Preview(reject),
                });
                return;
            }
        }
        if self.bracket_stem
            && let Some(mint) = self.order_client_ids.as_deref_mut().filter(|m| m.is_v2())
        {
            let block = block_at(self.snapshot, &spec.venue, spec.account.as_ref());
            let stem = order_client_id_plan(block).generate_stem(
                mint,
                OrderClientIdSource::Gui,
                OrderClientIdStrategyCode::NONE,
            );
            spec.order_client_id = stem.as_str().map(str::to_string);
        }
        self.commands.push(Command::Order(OrderIntent::Bracket(Box::new(spec))));
    }

    /// A command that is NOT an order write (cancels, a reprice) — no quantity/price to cap. Kept as
    /// a named method so the module's one push-to-`commands` rule reads as three explicit doors,
    /// not an ad-hoc `commands.push` anywhere.
    pub(super) fn pass_through(&mut self, command: Command) {
        self.commands.push(command);
    }

    /// Refuse an order the snapshot cannot route (empty venue or symbol).
    fn reject_unroutable(
        &mut self,
        source: OrderEntryDispatchSource,
        address: &TradeWindowAddress,
    ) {
        self.reject(source, address, OrderEntryDispatchRejectReason::NoRoutableMarket);
    }

    /// Refuse a Trade window intent at its address, for `reason`.
    fn reject(
        &mut self,
        source: OrderEntryDispatchSource,
        address: &TradeWindowAddress,
        reason: OrderEntryDispatchRejectReason,
    ) {
        self.rejects.push(OrderEntryDispatchReject {
            source,
            venue: address.venue.clone(),
            symbol: address.symbol.clone(),
            account: address.account.clone(),
            reason,
        });
    }

    /// One `Cancel` per order id, never a `CancelBatch`: the desktop's lift has no wire form for a
    /// batch (Ruling R8), and a single cancel works against every node.
    fn cancel_each(&mut self, order_client_ids: Vec<String>) {
        for order_client_id in order_client_ids {
            self.pass_through(Command::Order(OrderIntent::Cancel(order_client_id)));
        }
    }

    /// Trade window intents → commands, each through the one chokepoint for its kind: an order
    /// write through [`Self::admit`] / [`Self::admit_bracket`], a cancel or a reprice through
    /// [`Self::pass_through`].
    pub(super) fn plan_trade_window(
        &mut self,
        actions: Vec<(TradeWindowAddress, TradeWindowAction)>,
    ) {
        // Copy the `&'a CoreSnapshot` out of `self` up front: the lookups below must NOT hold a
        // borrow of `self` across the `admit`/`pass_through` calls that need `&mut self`.
        let snapshot = self.snapshot;
        for (address, act) in actions {
            match act {
                TradeWindowAction::Place {
                    side,
                    order_type,
                    price,
                    quantity,
                    reduce_only,
                    post_only,
                    exits,
                    origin,
                } => {
                    let source = match (exits.is_some(), origin) {
                        (true, _) => OrderEntryDispatchSource::TradeBracket,
                        (false, TradeWindowOrigin::Ticket) => OrderEntryDispatchSource::Trade,
                        (false, TradeWindowOrigin::Ladder) => OrderEntryDispatchSource::Dom,
                    };
                    if address.venue.is_empty() || address.symbol.is_empty() {
                        self.reject_unroutable(source, &address);
                        continue;
                    }
                    if let Some(why) = untradable(snapshot, &address) {
                        self.reject(source, &address, why);
                        continue;
                    }
                    // ⚠ A Limit or Stop with no price is REFUSED, never sent at market (see
                    // `OrderEntryDispatchRejectReason::MissingPrice`).
                    let price = match (order_type, price) {
                        (TradeWindowTicketKind::Market, _) => None,
                        (_, Some(p)) => Some(p),
                        (_, None) => {
                            self.reject(
                                source,
                                &address,
                                OrderEntryDispatchRejectReason::MissingPrice,
                            );
                            continue;
                        }
                    };
                    // ⚠ The bracket path builds its entry from a `OrderBracket`, which names no lifetime:
                    // a post-only order with exits would lose the flag and take liquidity. Refused.
                    if post_only && exits.is_some() {
                        self.reject(
                            source,
                            &address,
                            OrderEntryDispatchRejectReason::PostOnlyHasNoBracket,
                        );
                        continue;
                    }
                    if let Some(e) = exits {
                        let refused = if !bracket_on_mounted_symbol(snapshot, &address) {
                            Some(OrderEntryDispatchRejectReason::BracketNeedsMountedSymbol)
                        } else if !bracket_lane_holds_stop(snapshot, &address) {
                            Some(OrderEntryDispatchRejectReason::BracketNeedsPerpEngine)
                        } else if order_type == TradeWindowTicketKind::Stop {
                            Some(OrderEntryDispatchRejectReason::BracketNeedsMarketOrLimit)
                        } else if reduce_only {
                            Some(OrderEntryDispatchRejectReason::BracketNeedsOpeningEntry)
                        } else {
                            None
                        };
                        if let Some(r) = refused {
                            self.reject(source, &address, r);
                            continue;
                        }
                        // TP + SL on an entry → an OTO/OCO bracket: the runtime mints the 3 order client ids
                        // and wires the linkage. A Market entry carries no price. It names its
                        // account exactly as the plain order below does (`wire_account`): a labelled
                        // account itself, the default account as `DEFAULT` only where its venue runs
                        // another engine, so a single-engine node still gets the account-less frame.
                        let account = wire_account(snapshot, &address);
                        self.admit_bracket(
                            source,
                            vike_model::OrderBracket {
                                venue: address.venue,
                                symbol: address.symbol,
                                side,
                                qty: quantity,
                                entry_price: price,
                                stop_loss: e.stop_loss,
                                take_profit: e.take_profit,
                                account,
                                order_client_id: None,
                            },
                            address.account,
                        );
                        continue;
                    }
                    let account = wire_account(snapshot, &address);
                    // Every pair is named. A Limit or Stop with no price was refused above, and it
                    // must stay refused HERE too rather than reach a MARKET ticket: the old `_ =>`
                    // arm sent any unmatched pair, a priceless Limit included, at market.
                    let ticket = match (order_type, price) {
                        (TradeWindowTicketKind::Limit, Some(p)) => OrderEntryDispatchOrder::limit(
                            address.venue,
                            address.symbol,
                            side,
                            quantity,
                            p,
                        ),
                        (TradeWindowTicketKind::Stop, Some(p)) => OrderEntryDispatchOrder::stop(
                            address.venue,
                            address.symbol,
                            side,
                            quantity,
                            p,
                        ),
                        (TradeWindowTicketKind::Market, _) => OrderEntryDispatchOrder::market(
                            address.venue,
                            address.symbol,
                            side,
                            quantity,
                        ),
                        (TradeWindowTicketKind::Limit | TradeWindowTicketKind::Stop, None) => {
                            self.reject(
                                source,
                                &address,
                                OrderEntryDispatchRejectReason::MissingPrice,
                            );
                            continue;
                        }
                    };
                    let mut req = order::build_order_request(
                        &OrderEntryDispatchOrder { reduce_only, ..ticket },
                        String::new(),
                    );
                    req.account = account;
                    let prefix = match origin {
                        TradeWindowOrigin::Ladder => ORDER_CLIENT_ID_DOM,
                        TradeWindowOrigin::Ticket => ORDER_CLIENT_ID_TRADE,
                    };
                    self.order_client_id(prefix, &mut req);
                    // Maker-only: the ticket's (or a ladder click's) Post only becomes the order's
                    // lifetime, `vike_model::TimeInForce::PostOnly`. It travels the same wire as
                    // `vike-cli trade order submit --post-only` (`wire_from_command`); the node's
                    // door and the core's preflight refuse it by name where it cannot be kept (a
                    // venue with no post-only order, an order that is not a limit).
                    if post_only {
                        req.time_in_force = vike_model::TimeInForce::PostOnly;
                    }
                    self.admit(source, req);
                }
                TradeWindowAction::Modify { order_client_id, new_price } => {
                    // audit br6: consult the venue's DECLARED caps before sending. The ladder
                    // greys the drag for a venue whose adapter has no native modify; this is the
                    // matching guard on the command lane, so a stale or never-offered action can't
                    // reach a venue that would only reject it.
                    //
                    // NOT notional-capped — see the module doc's declared residual.
                    if vike_venues::capabilities_for(&address.venue).allows_modify() {
                        self.pass_through(Command::Order(OrderIntent::Modify {
                            order_client_id,
                            new_qty: None,
                            new_price: Some(new_price),
                            new_trigger_price: None,
                        }));
                    } else {
                        tracing::debug!(
                            venue = %address.venue,
                            "Trade window modify suppressed: venue adapter declares no native modify"
                        );
                    }
                }
                TradeWindowAction::Cancel(order_client_ids) => self.cancel_each(order_client_ids),
                TradeWindowAction::CancelSide(_) | TradeWindowAction::CancelAll => {
                    if !orders_name_their_account(snapshot, &address.venue) {
                        self.reject(
                            OrderEntryDispatchSource::Dom,
                            &address,
                            OrderEntryDispatchRejectReason::OrdersUnattributed,
                        );
                        continue;
                    }
                    let side = match act {
                        TradeWindowAction::CancelSide(side) => Some(side),
                        _ => None,
                    };
                    // scope the pull to THIS address's LIVE resting orders (a MassCancel would hit
                    // the engine's other symbols too) — the same rule the window's markers read
                    let order_client_ids: Vec<String> = window_orders(snapshot, &address)
                        .filter(|o| side.is_none_or(|s| o.side == s))
                        .map(|o| o.order_client_id.clone())
                        .collect();
                    self.cancel_each(order_client_ids);
                }
                TradeWindowAction::ClosePosition | TradeWindowAction::Reverse => {
                    let reverse = matches!(act, TradeWindowAction::Reverse);
                    if address.venue.is_empty() || address.symbol.is_empty() {
                        self.reject_unroutable(OrderEntryDispatchSource::DomExit, &address);
                        continue;
                    }
                    if let Some(why) = untradable(snapshot, &address) {
                        self.reject(OrderEntryDispatchSource::DomExit, &address, why);
                        continue;
                    }
                    // The position is the WINDOW ACCOUNT's: read off the block its route key
                    // names, never the first block of the venue.
                    let key = address.route_key();
                    let position =
                        snapshot.portfolio.venues.iter().find(|v| v.route_key == key).and_then(
                            |vb| vb.positions.iter().find(|p| p.symbol == address.symbol),
                        );
                    let Some(p) = position else { continue };
                    let signed = p.position_side.signed_size(p.size);
                    if signed.abs() <= 0.0 {
                        continue;
                    }
                    // exit is the OPPOSITE side of the held position: a long (signed>0) closes by
                    // SELL(-1), a short (signed<0) closes by BUY(+1). Reverse doubles the quantity
                    // (close + mirror).
                    let quantity = if reverse { signed.abs() * 2.0 } else { signed.abs() };
                    let account = wire_account(snapshot, &address);
                    let ticket = OrderEntryDispatchOrder {
                        // a close is reduce-only; a reverse is not
                        reduce_only: !reverse,
                        ..OrderEntryDispatchOrder::market(
                            address.venue,
                            address.symbol,
                            vike_model::closing_side(signed),
                            quantity,
                        )
                    };
                    let mut req = order::build_order_request(&ticket, String::new());
                    req.account = account;
                    self.order_client_id(ORDER_CLIENT_ID_DOM, &mut req);
                    self.admit(OrderEntryDispatchSource::DomExit, req);
                }
                // Not orders: the window's shell applies these before dispatch and never forwards
                // them; matched so a new intent has to be classified here.
                TradeWindowAction::PickSymbol { .. }
                | TradeWindowAction::PickAccount { .. }
                | TradeWindowAction::OpenConnections
                | TradeWindowAction::Note { .. } => {}
            }
        }
    }

    /// Polymarket cockpit: mirrors the Trade window ladder's place/cancel path — a unique `polymarket-` order client id, request
    /// via the one [`order`](super::order) constructor, gated through the same local preview, routed by
    /// `OrderRequest.venue` (`"polymarket"`).
    pub(super) fn plan_cockpit(&mut self, commands: Vec<CockpitCommand>) {
        for command in commands {
            match command {
                CockpitCommand::Cancel(order_client_id) => {
                    self.pass_through(Command::Order(OrderIntent::Cancel(order_client_id)));
                }
                CockpitCommand::Submit { token, side, price, quantity } => {
                    let ticket = match price {
                        Some(p) => {
                            OrderEntryDispatchOrder::limit("polymarket", token, side, quantity, p)
                        }
                        None => {
                            OrderEntryDispatchOrder::market("polymarket", token, side, quantity)
                        }
                    };
                    let mut req = order::build_order_request(&ticket, String::new());
                    self.order_client_id(ORDER_CLIENT_ID_COCKPIT, &mut req);
                    self.admit(OrderEntryDispatchSource::Cockpit, req);
                }
            }
        }
    }

    /// Options chain: each CONFIRMED ticket → the deribit execution engine. SAFETY: these only reach
    /// here AFTER the user clicked Confirm in the ticket modal — a raw chain bid/ask click merely
    /// opens the (editable) ticket and submits nothing.
    pub(super) fn plan_options(&mut self, tickets: Vec<OptionChainTicket>) {
        for t in tickets {
            let ticket = OrderEntryDispatchOrder::limit(
                "deribit",
                t.instrument,
                t.side,
                t.quantity,
                t.price,
            );
            let mut req = order::build_order_request(&ticket, String::new());
            self.order_client_id(ORDER_CLIENT_ID_OPTIONS, &mut req);
            self.admit(OrderEntryDispatchSource::Options, req);
        }
    }
}
