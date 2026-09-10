//! `order_dispatch` — the ONE place a drained UI order intent becomes a [`vike_exec::Command`],
//! and therefore the ONE place the local [`crate::order_entry`] preview cap is applied.
//!
//! ## Why this module exists
//! The per-frame "fold the drained tool-window intents onto the command lane" block lived inline in
//! `vike-app`'s `main.rs`. That file is compiled by NOTHING: the `justfile`'s `ci_crates` omits
//! `vike-app` (so `just windows-check` skips it too) and `xtask/src/ci/tables.rs`'s
//! `EXCLUDE_FROM_CI` lists it. The consequence was a real hole — of the five submit paths, only
//! three (the DOM ladder `Place`, the Polymarket cockpit `Submit`, the Deribit options confirm
//! ticket) called [`crate::order_entry::validate_with_multiplier`]. **The Trade window — the manual
//! order-entry panel a human types into — called neither `validate` nor `validate_with_multiplier`,
//! and neither did its TP+SL bracket sub-path or the DOM's Close/Reverse exit.** Those orders went
//! straight to the command lane with no local notional cap at all, bounded only by the venue-side
//! `vike_exec::RiskGate`.
//!
//! Patching `main.rs` in place would have been exactly as unverified as the bug. This is the same
//! lesson `order_entry`'s own module doc records (its notional-MULTIPLIER bug — the UI cap
//! under-measuring options and inverse-perp notional, so it PASSED orders it should have blocked —
//! shipped for the same reason). So the DECISION moved here, into a crate CI actually compiles and
//! tests, and `main.rs` keeps only the I/O: drain the intents in, hand the plan's commands to
//! `Dispatch::send`, log the rejects.
//!
//! ## The structural gate
//! Validation is not a step a future path can forget to call, because [`Planner::admit`] is the
//! ONLY function in this module that may push an order-WRITE command, and it always validates.
//! Three CI tests hold that shape:
//!
//!  * `every_submit_source_is_capped` iterates [`SubmitSource::ALL`] and drives each path with an
//!    over-cap order — a NEW variant fails to compile in [`SubmitSource::label`]'s no-wildcard
//!    `match` and in the test's own no-wildcard input builder until it is handled.
//!  * `order_intent_capping_is_classified` matches EXHAUSTIVELY over `vike_exec::OrderIntent`, so a
//!    new order verb cannot appear without a deliberate Capped / NotAnOrderWrite / exempt-with-
//!    reason classification (the allowlist-with-a-reason discipline `settings_registry.rs` and
//!    `duplicate_shape_gate.rs` use).
//!  * every test in this module asserts the OUTPUT invariant via [`assert_plan_respects_limits`]:
//!    every emitted `Submit`/`Bracket` command satisfies the cap. A future path that emits an
//!    unvalidated over-cap order fails the moment any test drives it.
//!
//! ## What is deliberately NOT capped, and why
//!  * **Cancels** (`Cancel`/`CancelBatch`) and `SetMargin` are not order writes — a cancel only
//!    reduces exposure, and a margin change carries no qty/price to measure.
//!  * **`OrderIntent::Modify`** (the DOM drag-to-reprice) is a REAL residual hole, declared rather
//!    than silently closed: repricing a resting order changes its notional, but the intent carries
//!    only `{coid, new_price}` — capping it means resolving the resting order's qty out of the
//!    snapshot and deciding what happens to an order that no longer fits, which is a behavior
//!    change, not an extraction. `order_intent_capping_is_classified` names it explicitly so the
//!    exemption is visible in CI rather than being an omission nobody wrote down.
//!  * **Market orders carry no price**, so `validate` measures no notional for them — the venue
//!    `RiskGate` owns that, exactly as [`crate::order_entry::validate_with_multiplier`] documents.
//!    A market entry inside a BRACKET is still notional-capped through the bracket's take-profit
//!    leg (see [`Planner::admit_bracket`]).
//!
//! ## The Trade window's market is the SNAPSHOT's, not a literal
//! The extracted Trade-window path routes to `snap.venue` / `snap.symbol` where `main.rs` used to
//! hardcode `("binance", "BTCUSDT")`. In the local FAT build this is byte-identical — `vike_run::
//! build_node` mounts `BINANCE_MARKET = ("binance", "BTCUSDT")` as the PRIMARY engine and
//! `CoreSnapshot::build` publishes that primary's `(venue, symbol)` verbatim. It stops being
//! identical in exactly the case where the literal was WRONG: a `vike-app --observe <ADDR>` thin
//! client with control enabled renders a REMOTE `vike-tradehub` daemon's snapshot
//! ([`crate::observe_bridge::wire_to_core`] carries the daemon's `venue`/`symbol` through), and
//! that daemon's profile venue defaults to `"polymarket"` (or `"hyperliquid"`). The Trade card
//! renders `snap.symbol` and sizes against `snap.symbol` — so the panel showed one market and the
//! BUY button submitted another. The pre-first-frame observer placeholder has EMPTY strings, which
//! is not a routable market either; that is now an explicit
//! [`DispatchRejectReason::NoRoutableMarket`] rather than a silent misroute.

use crate::order_entry::{self, OrderLimits, OrderReject, OrderTicket};
use crate::tool_views::CockpitCmd;
use crate::tools::{OptOrderTicket, TradeSubmit};
use vike_core::CoreSnapshot;
use vike_exec::{Command, OrderIntent};
use vike_model::OrderRequest;
use vike_panels::dom::DomAction;

/// The client-order-id prefix each submit path stamps. Kept as consts so the historical coid
/// spelling (`"{prefix}-{next_win_n}-{shot_n}"`) is stated once — a VALID order's coid is
/// byte-identical to the pre-extraction `format!` sites in `main.rs`.
const COID_TRADE: &str = "ui";
const COID_DOM: &str = "dom";
const COID_COCKPIT: &str = "poly";
const COID_OPTIONS: &str = "opt";

/// The distinct UI origins that can produce an order WRITE. One variant per submit path, so a
/// reject can name where it came from and the CI roster can drive each path individually.
///
/// Adding a path means adding a variant, which fails to compile in [`SubmitSource::label`]'s
/// no-wildcard `match` and in the roster test's input builder until it is BOTH handled and driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitSource {
    /// Trade window ticket → a plain Market/Limit/Stop order.
    Trade,
    /// Trade window ticket with BOTH a take-profit and a stop-loss on a non-reduce-only entry →
    /// an `OrderIntent::Bracket` (OTO entry + OCO exits).
    TradeBracket,
    /// DOM ladder `DomAction::Place`.
    Dom,
    /// DOM ladder `DomAction::ClosePosition` / `DomAction::Reverse` — a market exit leg.
    DomExit,
    /// Polymarket scalp-cockpit `CockpitCmd::Submit`.
    Cockpit,
    /// Deribit options chain confirm-ticket.
    Options,
}

impl SubmitSource {
    /// EVERY variant. The roster the CI gate iterates — a path missing from here is a path the
    /// gate never drives, so keep it exhaustive (the `label` match below is the compile-time
    /// reminder that a variant exists).
    pub const ALL: &[SubmitSource] = &[
        SubmitSource::Trade,
        SubmitSource::TradeBracket,
        SubmitSource::Dom,
        SubmitSource::DomExit,
        SubmitSource::Cockpit,
        SubmitSource::Options,
    ];

    /// Short human label for the warn log. NO WILDCARD ARM — a new variant is a compile error here.
    pub fn label(self) -> &'static str {
        match self {
            SubmitSource::Trade => "trade ticket",
            SubmitSource::TradeBracket => "trade bracket",
            SubmitSource::Dom => "DOM ladder",
            SubmitSource::DomExit => "DOM exit",
            SubmitSource::Cockpit => "Polymarket cockpit",
            SubmitSource::Options => "options ticket",
        }
    }
}

/// Why the dispatch refused to emit an order write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DispatchRejectReason {
    /// The local [`crate::order_entry`] preview rejected it (malformed shape, or over a cap).
    Preview(OrderReject),
    /// The snapshot named no routable `(venue, symbol)` — an empty venue or symbol. Reachable only
    /// on the snapshot-derived Trade path, and only before an observer's first `SnapshotFrame`
    /// lands (`WireSnapshot::empty()` carries empty strings).
    NoRoutableMarket,
}

impl std::fmt::Display for DispatchRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchRejectReason::Preview(r) => write!(f, "{r}"),
            DispatchRejectReason::NoRoutableMarket => {
                write!(f, "snapshot names no routable (venue, symbol) yet")
            }
        }
    }
}

/// One refused order write, carrying enough to log the same warn `main.rs` used to log inline.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchReject {
    pub source: SubmitSource,
    pub venue: String,
    pub symbol: String,
    pub reason: DispatchRejectReason,
}

/// The drained per-frame UI intents, in the exact order `main.rs` folded them. Owned because the
/// caller moves its drain buffers in (`DomAction`/`CockpitCmd` carry owned coid strings).
#[derive(Debug, Default)]
pub struct DispatchInputs {
    /// Trade-window tickets (BUY/SELL/Flatten). Routed to the snapshot's `(venue, symbol)`.
    pub trade_orders: Vec<TradeSubmit>,
    /// Trade-window working-order ✕ clicks (client-order-ids).
    pub trade_cancels: Vec<String>,
    /// Trade-window leverage pill → `(venue, symbol, im_requirement)`.
    pub trade_margins: Vec<(String, String, f64)>,
    /// DOM ladder actions, each tagged with the ladder's own `(venue, instrument)`.
    pub dom_actions: Vec<(String, String, DomAction)>,
    /// Polymarket cockpit intents (venue is always `"polymarket"`).
    pub cockpit_cmds: Vec<CockpitCmd>,
    /// Deribit options confirm-tickets.
    pub opt_orders: Vec<OptOrderTicket>,
    /// Deribit options chain working-order-marker cancels.
    pub opt_cancels: Vec<String>,
}

/// The planned frame: commands to fire in order, refusals to log, and the advanced coid counter.
#[derive(Debug)]
pub struct DispatchPlan {
    /// Fire these in order — `main.rs` hands each to `Dispatch::send`.
    pub commands: Vec<Command>,
    /// Order writes the local preview refused; `main.rs` warns one line per entry.
    pub rejects: Vec<DispatchReject>,
    /// `next_win_n` after this frame's coid minting. A REJECTED order still consumed its coid,
    /// exactly as the pre-extraction sites did (they minted before validating), so this counter is
    /// byte-identical to the old `self.next_win_n += 1` sequence.
    pub next_win_n: u32,
}

/// The frame's plan. PURE: no I/O, no env, no clock — inputs + limits + snapshot in, commands out.
///
/// `next_win_n`/`shot_n` are the GUI's coid counters; the returned [`DispatchPlan::next_win_n`] is
/// what the caller writes back.
pub fn plan_dispatch(
    inputs: DispatchInputs,
    limits: &OrderLimits,
    snap: &CoreSnapshot,
    next_win_n: u32,
    shot_n: u32,
) -> DispatchPlan {
    let mut p =
        Planner { limits, snap, shot_n, next_win_n, commands: Vec::new(), rejects: Vec::new() };

    // ORDER IS LOAD-BEARING: this is the exact sequence `main.rs` folded these in, so the command
    // lane sees the same interleaving it always has.
    p.plan_trade(inputs.trade_orders);
    for coid in inputs.trade_cancels {
        p.pass_through(Command::Order(OrderIntent::Cancel(coid)));
    }
    for (venue, symbol, im_requirement) in inputs.trade_margins {
        p.pass_through(Command::SetMargin(Box::new(vike_exec::MarginUpdate {
            venue,
            symbol,
            im_requirement,
        })));
    }
    p.plan_dom(inputs.dom_actions);
    p.plan_cockpit(inputs.cockpit_cmds);
    p.plan_options(inputs.opt_orders);
    for coid in inputs.opt_cancels {
        p.pass_through(Command::Order(OrderIntent::Cancel(coid)));
    }

    DispatchPlan { commands: p.commands, rejects: p.rejects, next_win_n: p.next_win_n }
}

/// The accumulator. Private on purpose: [`Planner::admit`] / [`Planner::admit_bracket`] are the
/// ONLY ways an order write reaches `commands`, and both validate.
struct Planner<'a> {
    limits: &'a OrderLimits,
    snap: &'a CoreSnapshot,
    shot_n: u32,
    next_win_n: u32,
    commands: Vec<Command>,
    rejects: Vec<DispatchReject>,
}

impl Planner<'_> {
    /// `"{prefix}-{next_win_n}-{shot_n}"`, advancing the counter — the historical coid spelling.
    /// (Not [`crate::order_entry::next_client_order_id`], which is the two-part `"{prefix}-{seq}"`
    /// form; this three-part one is what the live GUI has always minted and what an operator reads
    /// off the working-orders table.)
    fn coid(&mut self, prefix: &str) -> String {
        let coid = format!("{prefix}-{}-{}", self.next_win_n, self.shot_n);
        self.next_win_n += 1;
        coid
    }

    /// **THE CHOKEPOINT.** Every single-order write goes through here, and here is where the local
    /// preview runs. Notional is measured WITH the instrument's contract multiplier
    /// (`CoreSnapshot::multiplier_of`, the published `vike_exec::Account::multiplier_of` grid) so
    /// the UI cap measures the same quantity `RiskGate` and `SimBroker` do; a venue/symbol absent
    /// from the grid resolves to the 1.0 default, i.e. the plain `validate` verdict.
    fn admit(&mut self, source: SubmitSource, req: OrderRequest) {
        let mult = self.snap.multiplier_of(&req.venue, &req.symbol);
        // Bound to a `let` so the `&req` borrow is over before the Err arm moves out of `req`.
        let verdict = order_entry::validate_with_multiplier(&req, self.limits, mult);
        match verdict {
            Ok(()) => self.commands.push(Command::Order(OrderIntent::Submit(Box::new(req)))),
            Err(reject) => self.rejects.push(DispatchReject {
                source,
                venue: req.venue,
                symbol: req.symbol,
                reason: DispatchRejectReason::Preview(reject),
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
    /// The coids are placeholders: the runtime mints the three real ones at `apply_intent`, and
    /// [`crate::order_entry::validate_with_multiplier`] reads only `qty`/`price`/`trigger_price`/
    /// `order_type`, never the id. The bracket path therefore consumes NO `next_win_n`, exactly as
    /// the pre-extraction site did.
    fn admit_bracket(&mut self, source: SubmitSource, spec: vike_model::BracketSpec) {
        let mult = self.snap.multiplier_of(&spec.venue, &spec.symbol);
        let legs = vike_model::build_bracket(&spec, "entry", "sl", "tp");
        for leg in legs.iter() {
            if let Err(reject) = order_entry::validate_with_multiplier(leg, self.limits, mult) {
                self.rejects.push(DispatchReject {
                    source,
                    venue: spec.venue.clone(),
                    symbol: spec.symbol.clone(),
                    reason: DispatchRejectReason::Preview(reject),
                });
                return;
            }
        }
        self.commands.push(Command::Order(OrderIntent::Bracket(Box::new(spec))));
    }

    /// A command that is NOT an order write (cancels, `SetMargin`) — no qty/price to cap. Kept as
    /// a named method so the module's one push-to-`commands` rule reads as three explicit doors,
    /// not an ad-hoc `commands.push` anywhere.
    fn pass_through(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }

    /// Refuse an order the snapshot cannot route (empty venue or symbol).
    fn reject_unroutable(&mut self, source: SubmitSource, venue: &str, symbol: &str) {
        self.rejects.push(DispatchReject {
            source,
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            reason: DispatchRejectReason::NoRoutableMarket,
        });
    }

    /// Trade window: each ticket becomes a bracket (TP + SL on a non-reduce-only entry) or a plain
    /// Market/Limit/Stop order, routed to the snapshot's primary `(venue, symbol)`.
    fn plan_trade(&mut self, orders: Vec<TradeSubmit>) {
        let venue = self.snap.venue.clone();
        let symbol = self.snap.symbol.clone();
        for sub in orders {
            // TP + SL on an entry → an OTO/OCO bracket: the runtime mints the 3 coids and wires the
            // linkage (paper engine arms the exits on the entry fill, cancels the loser).
            if let (Some(tp), Some(sl), false) = (sub.tp, sub.sl, sub.reduce_only) {
                if venue.is_empty() || symbol.is_empty() {
                    self.reject_unroutable(SubmitSource::TradeBracket, &venue, &symbol);
                    continue;
                }
                self.admit_bracket(
                    SubmitSource::TradeBracket,
                    vike_model::BracketSpec {
                        venue: venue.clone(),
                        symbol: symbol.clone(),
                        side: sub.side,
                        qty: sub.qty,
                        entry_price: (sub.kind == order_entry::OrderKind::Limit)
                            .then_some(sub.price)
                            .flatten(),
                        stop_loss: sl,
                        take_profit: tp,
                    },
                );
                continue;
            }
            if venue.is_empty() || symbol.is_empty() {
                self.reject_unroutable(SubmitSource::Trade, &venue, &symbol);
                continue;
            }
            let coid = self.coid(COID_TRADE);
            // Build the OrderRequest via the shared order-entry constructor (the same path the DOM
            // ladder uses), so Market/Limit/Stop all flow through one tested seam.
            let ticket = match sub.kind {
                order_entry::OrderKind::Market => {
                    OrderTicket::market(venue.clone(), symbol.clone(), sub.side, sub.qty)
                }
                order_entry::OrderKind::Limit => OrderTicket::limit(
                    venue.clone(),
                    symbol.clone(),
                    sub.side,
                    sub.qty,
                    sub.price.unwrap_or(0.0),
                ),
                order_entry::OrderKind::Stop => OrderTicket::stop(
                    venue.clone(),
                    symbol.clone(),
                    sub.side,
                    sub.qty,
                    sub.trigger_price.unwrap_or(0.0),
                ),
            };
            let mut req = order_entry::build_order_request(&ticket, coid);
            req.reduce_only = sub.reduce_only;
            self.admit(SubmitSource::Trade, req);
        }
    }

    /// DOM ladder: map each click-trade intent (tagged with its VENUE + instrument) onto the
    /// command lane. `Submit` routes by `OrderRequest.venue` to that venue's engine
    /// (`engine_idx_for_route_key`); rejections surface in the snapshot's `rejected_commands`.
    fn plan_dom(&mut self, actions: Vec<(String, String, DomAction)>) {
        // Copy the `&'a CoreSnapshot` out of `self` up front: the position/order lookups below
        // must NOT hold a borrow of `self` across the `admit`/`pass_through` calls that need
        // `&mut self`.
        let snap = self.snap;
        for (venue, inst, act) in actions {
            let is_reverse = matches!(act, DomAction::Reverse); // before `act` is consumed
            match act {
                DomAction::Place { side, price, qty, stop, reduce_only } => {
                    let coid = self.coid(COID_DOM);
                    let ticket = if stop {
                        OrderTicket::stop(venue.clone(), inst.clone(), side, qty, price)
                    } else {
                        OrderTicket::limit(venue.clone(), inst.clone(), side, qty, price)
                    };
                    let ticket = OrderTicket { reduce_only, ..ticket };
                    self.admit(SubmitSource::Dom, order_entry::build_order_request(&ticket, coid));
                }
                DomAction::Modify { coid, new_price } => {
                    // audit br6: consult the venue's DECLARED caps before sending. The DOM UI
                    // already greys the drag control for a venue whose adapter has no native
                    // modify; this is the matching guard on the command lane, so a stale or
                    // never-offered action can't reach a venue that would only reject it.
                    //
                    // NOT notional-capped — see the module doc's declared residual.
                    if vike_model::caps_for(&venue).allows_modify() {
                        self.pass_through(Command::Order(OrderIntent::Modify {
                            client_order_id: coid,
                            new_qty: None,
                            new_price: Some(new_price),
                        }));
                    } else {
                        tracing::debug!(
                            venue = %venue,
                            "DOM modify suppressed: venue adapter declares no native modify"
                        );
                    }
                }
                DomAction::Cancel(coid) => {
                    self.pass_through(Command::Order(OrderIntent::Cancel(coid)));
                }
                DomAction::CancelSide(side) => {
                    let coids: Vec<String> = snap
                        .orders
                        .iter()
                        .filter(|o| o.venue == venue && o.symbol == inst && o.side == side)
                        .map(|o| o.client_order_id.clone())
                        .collect();
                    if !coids.is_empty() {
                        self.pass_through(Command::Order(OrderIntent::CancelBatch(coids)));
                    }
                }
                DomAction::CancelAll => {
                    // scope the pull to THIS venue+symbol's resting orders (a MassCancel would hit
                    // the engine's other symbols too)
                    let coids: Vec<String> = snap
                        .orders
                        .iter()
                        .filter(|o| o.venue == venue && o.symbol == inst)
                        .map(|o| o.client_order_id.clone())
                        .collect();
                    if !coids.is_empty() {
                        self.pass_through(Command::Order(OrderIntent::CancelBatch(coids)));
                    }
                }
                DomAction::ClosePosition | DomAction::Reverse => {
                    let pos = snap
                        .portfolio
                        .venues
                        .iter()
                        .find(|v| v.venue == venue)
                        .and_then(|vb| vb.positions.iter().find(|p| p.symbol == inst));
                    let Some(p) = pos else { continue };
                    let signed = crate::dom_math::signed_position_size(p.size, &p.position_side);
                    let mag = signed.abs();
                    if mag <= 0.0 {
                        continue;
                    }
                    // exit is the OPPOSITE side of the held position: a long (signed>0) closes by
                    // SELL(-1), a short (signed<0) closes by BUY(+1). Reverse doubles the qty
                    // (close + mirror).
                    let exit_side = vike_model::closing_side(signed);
                    let qty = if is_reverse { mag * 2.0 } else { mag };
                    let coid = self.coid(COID_DOM);
                    let ticket = OrderTicket {
                        // a close is reduce-only; a reverse is not
                        reduce_only: !is_reverse,
                        ..OrderTicket::market(venue.clone(), inst.clone(), exit_side, qty)
                    };
                    self.admit(
                        SubmitSource::DomExit,
                        order_entry::build_order_request(&ticket, coid),
                    );
                }
            }
        }
    }

    /// Polymarket cockpit: mirrors the DOM `Place`/`Cancel` path — a unique `poly-` coid, request
    /// via the one `order_entry` constructor, gated through the same local preview, routed by
    /// `OrderRequest.venue` (`"polymarket"`).
    fn plan_cockpit(&mut self, cmds: Vec<CockpitCmd>) {
        for cmd in cmds {
            match cmd {
                CockpitCmd::Cancel(coid) => {
                    self.pass_through(Command::Order(OrderIntent::Cancel(coid)));
                }
                CockpitCmd::Submit { token, side, price, qty } => {
                    let coid = self.coid(COID_COCKPIT);
                    let ticket = match price {
                        Some(p) => OrderTicket::limit("polymarket", token, side, qty, p),
                        None => OrderTicket::market("polymarket", token, side, qty),
                    };
                    self.admit(
                        SubmitSource::Cockpit,
                        order_entry::build_order_request(&ticket, coid),
                    );
                }
            }
        }
    }

    /// Options chain: each CONFIRMED ticket → the deribit exec engine. SAFETY: these only reach
    /// here AFTER the user clicked Confirm in the ticket modal — a raw chain bid/ask click merely
    /// opens the (editable) ticket and submits nothing.
    fn plan_options(&mut self, tickets: Vec<OptOrderTicket>) {
        for t in tickets {
            let coid = self.coid(COID_OPTIONS);
            let ticket = OrderTicket::limit("deribit", t.instrument, t.side, t.qty, t.price);
            self.admit(SubmitSource::Options, order_entry::build_order_request(&ticket, coid));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order_entry::OrderKind;

    /// A snapshot whose PRIMARY market is `(venue, symbol)` and whose multiplier grid is empty
    /// (every symbol resolves to `multiplier_default`).
    fn snap(venue: &str, symbol: &str) -> CoreSnapshot {
        CoreSnapshot::empty(venue, symbol)
    }

    /// The binance/BTCUSDT primary the local FAT build always publishes.
    fn binance_snap() -> CoreSnapshot {
        snap("binance", "BTCUSDT")
    }

    fn capped(max_notional: f64) -> OrderLimits {
        OrderLimits { max_notional, ..OrderLimits::default() }
    }

    fn trade(kind: OrderKind, qty: f64, price: Option<f64>) -> TradeSubmit {
        TradeSubmit {
            side: 1,
            qty,
            reduce_only: false,
            kind,
            price,
            trigger_price: None,
            tp: None,
            sl: None,
        }
    }

    /// THE OUTPUT INVARIANT, asserted by every test below: no `Submit`/`Bracket` command in a plan
    /// may breach `limits`. This is what makes a FUTURE unvalidated path fail rather than pass —
    /// it inspects the plan's commands, not the code that produced them.
    fn assert_plan_respects_limits(plan: &DispatchPlan, limits: &OrderLimits, s: &CoreSnapshot) {
        for cmd in &plan.commands {
            match cmd {
                Command::Order(OrderIntent::Submit(req)) => {
                    let m = s.multiplier_of(&req.venue, &req.symbol);
                    assert_eq!(
                        order_entry::validate_with_multiplier(req, limits, m),
                        Ok(()),
                        "an emitted Submit breaches the local cap: {req:?}"
                    );
                }
                Command::Order(OrderIntent::SubmitBatch(reqs)) => {
                    for req in reqs {
                        let m = s.multiplier_of(&req.venue, &req.symbol);
                        assert_eq!(
                            order_entry::validate_with_multiplier(req, limits, m),
                            Ok(()),
                            "an emitted SubmitBatch leg breaches the local cap: {req:?}"
                        );
                    }
                }
                Command::Order(OrderIntent::Bracket(spec)) => {
                    let m = s.multiplier_of(&spec.venue, &spec.symbol);
                    for leg in vike_model::build_bracket(spec, "e", "s", "t").iter() {
                        assert_eq!(
                            order_entry::validate_with_multiplier(leg, limits, m),
                            Ok(()),
                            "an emitted Bracket leg breaches the local cap: {leg:?}"
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn submit_count(plan: &DispatchPlan) -> usize {
        plan.commands
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    Command::Order(
                        OrderIntent::Submit(_)
                            | OrderIntent::SubmitBatch(_)
                            | OrderIntent::Bracket(_)
                    )
                )
            })
            .count()
    }

    /// Build a one-intent [`DispatchInputs`] that drives EXACTLY `source`, with an order whose
    /// notional is 300 (`qty 2 × price 150`) — over the 100 cap the roster test uses, under the
    /// permissive default.
    ///
    /// NO WILDCARD ARM: a new [`SubmitSource`] variant fails to compile right here until it is
    /// given an input shape, which is what stops a future submit path from being added without
    /// being driven by the gate.
    fn one_intent_for(source: SubmitSource) -> DispatchInputs {
        let mut i = DispatchInputs::default();
        match source {
            SubmitSource::Trade => {
                i.trade_orders.push(trade(OrderKind::Limit, 2.0, Some(150.0)));
            }
            SubmitSource::TradeBracket => {
                i.trade_orders.push(TradeSubmit {
                    tp: Some(150.0),
                    sl: Some(140.0),
                    ..trade(OrderKind::Limit, 2.0, Some(150.0))
                });
            }
            SubmitSource::Dom => {
                i.dom_actions.push((
                    "binance".into(),
                    "BTCUSDT".into(),
                    DomAction::Place {
                        side: 1,
                        price: 150.0,
                        qty: 2.0,
                        stop: false,
                        reduce_only: false,
                    },
                ));
            }
            SubmitSource::DomExit => {
                // A market exit has no price, so notional is not the lever here — `max_qty` is.
                i.dom_actions.push(("binance".into(), "BTCUSDT".into(), DomAction::ClosePosition));
            }
            SubmitSource::Cockpit => {
                i.cockpit_cmds.push(CockpitCmd::Submit {
                    token: "TOK".into(),
                    side: 1,
                    price: Some(150.0),
                    qty: 2.0,
                });
            }
            SubmitSource::Options => {
                i.opt_orders.push(OptOrderTicket {
                    instrument: "BTC-28MAR25-100000-C".into(),
                    side: 1,
                    price: 150.0,
                    qty: 2.0,
                    is_call: true,
                    strike: 100_000.0,
                });
            }
        }
        i
    }

    /// A snapshot carrying a `(venue, symbol)` position of `size`, so the DOM exit path has
    /// something to close.
    fn snap_with_position(venue: &str, symbol: &str, size: f64) -> CoreSnapshot {
        let mut s = snap(venue, symbol);
        s.portfolio.venues.push(vike_core::snapshot::VenueBlock {
            venue: venue.to_string(),
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: vike_exec::BalanceMode::Delta,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: vike_exec::TradingState::Active,
            multipliers: std::sync::Arc::new(indexmap::IndexMap::new()),
            multiplier_default: 1.0,
            positions: vec![vike_core::snapshot::PositionView {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                position_side: "BOTH".to_string(),
                size,
                avg_px: 150.0,
                unrealized: 0.0,
                mark_source: None,
                leverage: 1.0,
                liq_price: 0.0,
                margin_mode: vike_model::MarginMode::Cross,
                isolated_margin: None,
            }],
        });
        s
    }

    /// **THE GATE.** Every submit source — Trade window and its bracket sub-path included — is
    /// subject to the limits. Driven one source at a time so a failure names the path.
    ///
    /// The lever differs by shape and that is deliberate: a PRICED order (Trade limit / bracket /
    /// DOM place / cockpit / options) trips `max_notional`; a market EXIT carries no price, so the
    /// same over-size intent trips `max_qty` instead. Both are limits from the same `OrderLimits`,
    /// which is the property under test — "this path consults `OrderLimits`", not "this path
    /// happens to have a price".
    #[test]
    fn every_submit_source_is_capped() {
        let limits = OrderLimits { max_notional: 100.0, max_qty: 1.0, ..OrderLimits::default() };
        for &source in SubmitSource::ALL {
            let s = snap_with_position("binance", "BTCUSDT", 2.0);
            let plan = plan_dispatch(one_intent_for(source), &limits, &s, 0, 7);
            assert_eq!(
                submit_count(&plan),
                0,
                "{source:?} ({}) emitted an order write past the cap",
                source.label()
            );
            assert_eq!(plan.rejects.len(), 1, "{source:?} must report exactly one reject");
            assert_eq!(plan.rejects[0].source, source);
            assert_plan_respects_limits(&plan, &limits, &s);
        }
    }

    /// The same roster under the PERMISSIVE default: every source still produces its order, so the
    /// gate above is proving the cap and not merely that the paths are broken.
    #[test]
    fn every_submit_source_still_trades_under_permissive_limits() {
        let limits = OrderLimits::default();
        for &source in SubmitSource::ALL {
            let s = snap_with_position("binance", "BTCUSDT", 2.0);
            let plan = plan_dispatch(one_intent_for(source), &limits, &s, 0, 7);
            assert_eq!(
                submit_count(&plan),
                1,
                "{source:?} ({}) produced no order under permissive limits",
                source.label()
            );
            assert!(plan.rejects.is_empty(), "{source:?} rejected under permissive limits");
            assert_plan_respects_limits(&plan, &limits, &s);
        }
    }

    /// All sources at once, all over-cap: NOTHING order-shaped escapes, and the non-order commands
    /// (cancels / margin) still flow — a cap must not silently swallow a risk-REDUCING verb.
    #[test]
    fn a_whole_frame_of_over_cap_intents_emits_no_order_write() {
        let limits = OrderLimits { max_notional: 100.0, max_qty: 1.0, ..OrderLimits::default() };
        let s = snap_with_position("binance", "BTCUSDT", 2.0);
        let mut inputs = DispatchInputs {
            trade_cancels: vec!["c-1".into()],
            trade_margins: vec![("binance".into(), "BTCUSDT".into(), 0.2)],
            opt_cancels: vec!["c-2".into()],
            ..DispatchInputs::default()
        };
        for &source in SubmitSource::ALL {
            let one = one_intent_for(source);
            inputs.trade_orders.extend(one.trade_orders);
            inputs.dom_actions.extend(one.dom_actions);
            inputs.cockpit_cmds.extend(one.cockpit_cmds);
            inputs.opt_orders.extend(one.opt_orders);
        }
        let plan = plan_dispatch(inputs, &limits, &s, 0, 0);

        assert_eq!(submit_count(&plan), 0, "an order write escaped the cap");
        assert_eq!(plan.rejects.len(), SubmitSource::ALL.len());
        assert_plan_respects_limits(&plan, &limits, &s);

        // the risk-reducing / non-order verbs are untouched
        let cancels = plan
            .commands
            .iter()
            .filter(|c| matches!(c, Command::Order(OrderIntent::Cancel(_))))
            .count();
        assert_eq!(cancels, 2, "cancels must never be capped away");
        assert!(plan.commands.iter().any(|c| matches!(c, Command::SetMargin(_))));
    }

    /// How this module treats one `vike_exec::OrderIntent` kind.
    #[derive(Debug, PartialEq)]
    enum Class {
        /// An order write this module emits — it goes through `admit`/`admit_bracket` and is
        /// therefore subject to [`OrderLimits`].
        Capped,
        /// Not an order write: no qty/price to measure (cancels, queries, disarms).
        NotAnOrderWrite,
        /// An order write this module does NOT cap, with the reason it cannot yet.
        ExemptWithReason(&'static str),
        /// This module never emits it (a runtime/strategy-lowered verb).
        NotEmittedHere,
    }

    /// THE CLASSIFICATION, with **NO WILDCARD ARM** — this is the compile-time pin. A new
    /// `vike_exec::OrderIntent` variant fails to compile right here until someone decides, in
    /// writing, whether the UI dispatch caps it, must not, or never emits it. That is what stops a
    /// new order verb from silently inheriting "uncapped".
    fn classify(intent: &OrderIntent) -> Class {
        match intent {
            OrderIntent::Submit(_) | OrderIntent::Bracket(_) => Class::Capped,
            OrderIntent::Cancel(_)
            | OrderIntent::CancelBatch(_)
            | OrderIntent::Confirm(_)
            | OrderIntent::MassCancel { .. }
            | OrderIntent::DisarmConditional { .. } => Class::NotAnOrderWrite,
            OrderIntent::SubmitBatch(_)
            | OrderIntent::Flatten { .. }
            | OrderIntent::MarketExit { .. }
            | OrderIntent::ArmConditional(_)
            | OrderIntent::Combo(_) => Class::NotEmittedHere,
            OrderIntent::Modify { .. } => Class::ExemptWithReason(
                "the DOM drag-to-reprice. Repricing DOES change notional, but the intent carries \
                 only {coid, new_price}: capping it means resolving the resting order's qty out \
                 of the snapshot and deciding what happens to an order that no longer fits — a \
                 behavior change, not an extraction. Declared here so the hole is visible in CI \
                 instead of being an omission nobody wrote down.",
            ),
        }
    }

    /// The classification is exercised (not merely written): the two verbs this module emits as
    /// order writes classify `Capped`, the exempt one states a real reason, and a cancel is not an
    /// order write.
    #[test]
    fn order_intent_capping_is_classified() {
        assert_eq!(classify(&OrderIntent::Submit(Box::<OrderRequest>::default())), Class::Capped);
        assert_eq!(
            classify(&OrderIntent::Bracket(Box::new(vike_model::BracketSpec {
                venue: String::new(),
                symbol: String::new(),
                side: 1,
                qty: 0.0,
                entry_price: None,
                stop_loss: 0.0,
                take_profit: 0.0,
            }))),
            Class::Capped
        );
        assert_eq!(
            classify(&OrderIntent::Cancel(String::new())),
            Class::NotAnOrderWrite,
            "a cancel only reduces exposure — capping it would be a footgun, not a guard"
        );
        match classify(&OrderIntent::Modify {
            client_order_id: String::new(),
            new_qty: None,
            new_price: None,
        }) {
            Class::ExemptWithReason(why) => {
                assert!(why.len() > 80, "an exemption must state a real reason, not a shrug")
            }
            other => panic!("Modify's declared residual changed shape: {other:?}"),
        }

        // EVERY intent this module actually emits is `Capped` or `NotAnOrderWrite` — never an
        // unclassified escape. Driven off a real, fully-populated frame.
        let s = snap_with_position("binance", "BTCUSDT", 2.0);
        let mut inputs = DispatchInputs {
            trade_cancels: vec!["c-1".into()],
            opt_cancels: vec!["c-2".into()],
            ..DispatchInputs::default()
        };
        for &source in SubmitSource::ALL {
            let one = one_intent_for(source);
            inputs.trade_orders.extend(one.trade_orders);
            inputs.dom_actions.extend(one.dom_actions);
            inputs.cockpit_cmds.extend(one.cockpit_cmds);
            inputs.opt_orders.extend(one.opt_orders);
        }
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
        for cmd in &plan.commands {
            if let Command::Order(intent) = cmd {
                assert!(
                    matches!(classify(intent), Class::Capped | Class::NotAnOrderWrite),
                    "the dispatch emitted an intent it does not claim to cap: {intent:?}"
                );
            }
        }
    }

    /// The Trade window routes to the SNAPSHOT's primary market, not a hardcoded literal. On the
    /// local FAT build that primary IS `("binance", "BTCUSDT")` (`vike_run::build_node` mounts
    /// `BINANCE_MARKET` as the primary engine), so this is byte-identical there…
    #[test]
    fn trade_window_routes_to_the_binance_primary_byte_identically() {
        let s = binance_snap();
        let inputs = DispatchInputs {
            trade_orders: vec![trade(OrderKind::Market, 0.5, None)],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 3, 9);
        match &plan.commands[..] {
            [Command::Order(OrderIntent::Submit(req))] => {
                assert_eq!(req.venue, "binance");
                assert_eq!(req.symbol, "BTCUSDT");
                assert_eq!(req.client_order_id, "ui-3-9", "the historical coid spelling");
                assert_eq!(req.order_type, "market");
                assert_eq!(req.qty, 0.5);
            }
            other => panic!("expected exactly one Submit, got {other:?}"),
        }
        assert_eq!(plan.next_win_n, 4);
    }

    /// …and it FOLLOWS a non-binance primary, which is the misroute the literal caused: a
    /// control-enabled `--observe` client renders a remote `vike-tradehub` daemon whose profile
    /// venue defaults to `"polymarket"`, so the Trade card showed that market while the BUY button
    /// submitted `("binance","BTCUSDT")`.
    #[test]
    fn trade_window_follows_a_non_binance_primary() {
        let s = snap("polymarket", "7132104567925221259462638553270691275033272857194253228963");
        let inputs = DispatchInputs {
            trade_orders: vec![trade(OrderKind::Limit, 10.0, Some(0.42))],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
        match &plan.commands[..] {
            [Command::Order(OrderIntent::Submit(req))] => {
                assert_eq!(req.venue, "polymarket");
                assert_eq!(req.symbol, s.symbol);
                assert_eq!(req.price, Some(0.42));
            }
            other => panic!("expected exactly one Submit, got {other:?}"),
        }
    }

    /// An observer's pre-first-frame placeholder (`WireSnapshot::empty()` → empty venue/symbol) is
    /// not a routable market: the ticket is refused, not misrouted.
    #[test]
    fn trade_window_refuses_an_unroutable_snapshot() {
        let s = snap("", "");
        let inputs = DispatchInputs {
            trade_orders: vec![
                trade(OrderKind::Market, 1.0, None),
                TradeSubmit {
                    tp: Some(2.0),
                    sl: Some(1.0),
                    ..trade(OrderKind::Limit, 1.0, Some(1.5))
                },
            ],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
        assert_eq!(submit_count(&plan), 0);
        assert_eq!(plan.rejects.len(), 2);
        for r in &plan.rejects {
            assert_eq!(r.reason, DispatchRejectReason::NoRoutableMarket);
        }
        // an unroutable ticket consumes NO coid
        assert_eq!(plan.next_win_n, 0);
    }

    /// THE BUG THIS MODULE EXISTS FOR, stated as the pre-fix behaviour it inverts: a Trade-window
    /// ticket whose notional is 10x the configured per-order ceiling (`max_notional_per_order` in
    /// `policy.toml` — it was `VIKE_MAX_ORDER_NOTIONAL` before settings-unification Phase 5) used
    /// to reach the command lane unexamined. It is now refused, with the reason named.
    #[test]
    fn trade_window_over_notional_is_refused() {
        let limits = capped(1_000.0);
        let s = binance_snap();
        let inputs = DispatchInputs {
            // 0.2 × 50_000 = 10_000 > 1_000
            trade_orders: vec![trade(OrderKind::Limit, 0.2, Some(50_000.0))],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &limits, &s, 0, 0);
        assert_eq!(submit_count(&plan), 0);
        assert_eq!(plan.rejects.len(), 1);
        assert_eq!(plan.rejects[0].source, SubmitSource::Trade);
        assert!(matches!(
            plan.rejects[0].reason,
            DispatchRejectReason::Preview(OrderReject::NotionalAboveMax { .. })
        ));
        assert_plan_respects_limits(&plan, &limits, &s);
    }

    /// The CONTRACT MULTIPLIER is honoured on the Trade path too — the precise mistake the earlier
    /// `order_entry` bug made. A deribit-primary snapshot with a 100x grid entry makes a ticket
    /// that measures 300 bare measure 30,000 for real, so a 1,000 cap must block it.
    #[test]
    fn trade_window_measures_notional_with_the_contract_multiplier() {
        let limits = capped(1_000.0);
        let mut s = snap("deribit", "BTC-28MAR25-100000-C");
        let mut grid = indexmap::IndexMap::new();
        grid.insert("BTC-28MAR25-100000-C".to_string(), 100.0);
        s.portfolio.venues.push(vike_core::snapshot::VenueBlock {
            venue: "deribit".into(),
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: vike_exec::BalanceMode::Delta,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: vike_exec::TradingState::Active,
            multipliers: std::sync::Arc::new(grid),
            multiplier_default: 1.0,
            positions: Vec::new(),
        });
        assert_eq!(s.multiplier_of("deribit", "BTC-28MAR25-100000-C"), 100.0);

        let inputs = DispatchInputs {
            trade_orders: vec![trade(OrderKind::Limit, 2.0, Some(150.0))],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &limits, &s, 0, 0);
        assert_eq!(submit_count(&plan), 0, "2 × 150 × 100 = 30_000 must not pass a 1_000 cap");
        assert!(matches!(
            plan.rejects[0].reason,
            DispatchRejectReason::Preview(OrderReject::NotionalAboveMax { notional, .. })
                if notional == 30_000.0
        ));

        // …and the bare (multiplier-blind) measure would have PASSED it — the pinned pre-fix hole.
        assert_eq!(
            order_entry::validate(
                &order_entry::build_order_request(
                    &OrderTicket::limit("deribit", "BTC-28MAR25-100000-C", 1, 2.0, 150.0),
                    "c".into()
                ),
                &limits
            ),
            Ok(())
        );
    }

    /// A bracket is refused when ANY of its three lowered legs breaches — here the TAKE-PROFIT
    /// leg, whose price is the fat-fingered one while the entry fits. Refusal is atomic: no leg
    /// is emitted, so nothing is stranded.
    #[test]
    fn bracket_is_refused_when_an_exit_leg_breaches() {
        let limits = capped(1_000.0);
        let s = binance_snap();
        let inputs = DispatchInputs {
            trade_orders: vec![TradeSubmit {
                tp: Some(50_000.0), // 0.01 × 50_000 = 500 … fine
                sl: Some(90.0),
                ..trade(OrderKind::Limit, 0.01, Some(100.0)) // entry: 1.0
            }],
            ..DispatchInputs::default()
        };
        // that one passes …
        let plan = plan_dispatch(inputs, &limits, &s, 0, 0);
        assert_eq!(submit_count(&plan), 1);
        assert_plan_respects_limits(&plan, &limits, &s);

        // … and an extra zero on the take-profit (5_000 > 1_000) refuses the WHOLE bracket.
        let inputs = DispatchInputs {
            trade_orders: vec![TradeSubmit {
                tp: Some(500_000.0),
                sl: Some(90.0),
                ..trade(OrderKind::Limit, 0.01, Some(100.0))
            }],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &limits, &s, 0, 0);
        assert_eq!(submit_count(&plan), 0, "a breaching exit leg must refuse the whole bracket");
        assert_eq!(plan.rejects.len(), 1);
        assert_eq!(plan.rejects[0].source, SubmitSource::TradeBracket);
        assert_plan_respects_limits(&plan, &limits, &s);
    }

    /// A non-finite stop-loss is caught by the bracket's leg validation — the exact shape a plain
    /// `validate` on the entry alone would have let through (the entry has no NaN in it).
    #[test]
    fn bracket_with_a_non_finite_exit_is_refused() {
        let s = binance_snap();
        let inputs = DispatchInputs {
            trade_orders: vec![TradeSubmit {
                tp: Some(120.0),
                sl: Some(f64::NAN),
                ..trade(OrderKind::Limit, 1.0, Some(100.0))
            }],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
        assert_eq!(submit_count(&plan), 0);
        assert_eq!(
            plan.rejects[0].reason,
            DispatchRejectReason::Preview(OrderReject::NonFiniteQtyOrPrice)
        );
    }

    /// A bracket consumes NO `next_win_n` (the runtime mints its three coids), matching the
    /// pre-extraction site — while a plain ticket consumes exactly one, whether it is ADMITTED or
    /// REFUSED (the old sites minted before validating).
    #[test]
    fn coid_counter_advances_exactly_as_before() {
        let s = binance_snap();
        let inputs = DispatchInputs {
            trade_orders: vec![
                TradeSubmit {
                    tp: Some(120.0),
                    sl: Some(90.0),
                    ..trade(OrderKind::Limit, 1.0, Some(100.0))
                },
                trade(OrderKind::Market, 1.0, None),
            ],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 10, 2);
        assert_eq!(plan.next_win_n, 11, "bracket mints nothing; the plain ticket mints one");

        // a REFUSED ticket still burns its coid
        let inputs = DispatchInputs {
            trade_orders: vec![trade(OrderKind::Limit, 1.0, Some(100.0))],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &capped(1.0), &s, 10, 2);
        assert_eq!(submit_count(&plan), 0);
        assert_eq!(plan.next_win_n, 11);
    }

    /// The DOM's non-order actions keep working through the extraction: `CancelSide`/`CancelAll`
    /// scope to the ladder's own (venue, symbol) and `Modify` is gated on the venue's declared
    /// caps, exactly as the inline block did.
    #[test]
    fn dom_cancel_scoping_and_modify_gating_survive_the_extraction() {
        let mut s = binance_snap();
        for (coid, venue, symbol, side) in [
            ("a", "binance", "BTCUSDT", 1),
            ("b", "binance", "BTCUSDT", -1),
            ("c", "binance", "ETHUSDT", 1),
            ("d", "bybit", "BTCUSDT", 1),
        ] {
            s.orders.push(vike_core::snapshot::OrderView {
                client_order_id: coid.into(),
                venue: venue.into(),
                symbol: symbol.into(),
                side,
                qty: 1.0,
                order_type: "limit".into(),
                price: Some(100.0),
                trigger_price: None,
                status: vike_exec::OrderStatus::Accepted,
                venue_order_id: None,
                filled_qty: 0.0,
                avg_fill_px: 0.0,
            });
        }
        let inputs = DispatchInputs {
            dom_actions: vec![
                ("binance".into(), "BTCUSDT".into(), DomAction::CancelSide(1)),
                ("binance".into(), "BTCUSDT".into(), DomAction::CancelAll),
            ],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
        let batches: Vec<&Vec<String>> = plan
            .commands
            .iter()
            .filter_map(|c| match c {
                Command::Order(OrderIntent::CancelBatch(v)) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], &["a".to_string()], "CancelSide(+1) on this venue+symbol only");
        assert_eq!(
            batches[1],
            &["a".to_string(), "b".to_string()],
            "CancelAll stays scoped to this venue+symbol"
        );

        // Modify is gated on the venue's declared caps — a venue that declares it emits the
        // command, one that does not emits nothing. Driven off the canonical roster so this stays
        // true as `venue_caps.rs` changes.
        let modify_case = |venue: &str| {
            let inputs = DispatchInputs {
                dom_actions: vec![(
                    venue.to_string(),
                    "X".into(),
                    DomAction::Modify { coid: "a".into(), new_price: 1.0 },
                )],
                ..DispatchInputs::default()
            };
            plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0).commands.len()
        };
        let allowed =
            vike_model::VENUES.iter().copied().find(|&v| vike_model::caps_for(v).allows_modify());
        let denied =
            vike_model::VENUES.iter().copied().find(|&v| !vike_model::caps_for(v).allows_modify());
        if let Some(v) = allowed {
            assert_eq!(modify_case(v), 1, "{v} declares modify");
        }
        if let Some(v) = denied {
            assert_eq!(modify_case(v), 0, "{v} declares no native modify");
        }
    }

    /// The DOM exit legs keep their historical shape: a CLOSE is reduce-only at |position|, a
    /// REVERSE is not reduce-only and doubles the qty, both on the OPPOSITE side.
    #[test]
    fn dom_close_and_reverse_keep_their_shape() {
        let s = snap_with_position("binance", "BTCUSDT", 2.0); // long 2
        let inputs = DispatchInputs {
            dom_actions: vec![
                ("binance".into(), "BTCUSDT".into(), DomAction::ClosePosition),
                ("binance".into(), "BTCUSDT".into(), DomAction::Reverse),
            ],
            ..DispatchInputs::default()
        };
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
        match &plan.commands[..] {
            [
                Command::Order(OrderIntent::Submit(close)),
                Command::Order(OrderIntent::Submit(rev)),
            ] => {
                assert_eq!(close.side, -1, "closing a long SELLs");
                assert_eq!(close.qty, 2.0);
                assert!(close.reduce_only);
                assert_eq!(close.order_type, "market");
                assert_eq!(rev.side, -1);
                assert_eq!(rev.qty, 4.0, "reverse doubles");
                assert!(!rev.reduce_only);
            }
            other => panic!("expected two market exits, got {other:?}"),
        }
        // a flat book emits nothing
        let flat = snap_with_position("binance", "BTCUSDT", 0.0);
        let inputs = DispatchInputs {
            dom_actions: vec![("binance".into(), "BTCUSDT".into(), DomAction::ClosePosition)],
            ..DispatchInputs::default()
        };
        assert!(plan_dispatch(inputs, &OrderLimits::default(), &flat, 0, 0).commands.is_empty());
    }

    /// The emission ORDER is the one the command lane has always seen: trade → trade cancels →
    /// margins → DOM → cockpit → options → option cancels.
    #[test]
    fn emission_order_is_preserved() {
        let s = binance_snap();
        let inputs = DispatchInputs {
            trade_orders: vec![trade(OrderKind::Market, 1.0, None)],
            trade_cancels: vec!["tc".into()],
            trade_margins: vec![("binance".into(), "BTCUSDT".into(), 0.1)],
            dom_actions: vec![(
                "binance".into(),
                "BTCUSDT".into(),
                DomAction::Place { side: 1, price: 1.0, qty: 1.0, stop: false, reduce_only: false },
            )],
            cockpit_cmds: vec![CockpitCmd::Submit {
                token: "TOK".into(),
                side: 1,
                price: Some(0.5),
                qty: 1.0,
            }],
            opt_orders: vec![OptOrderTicket {
                instrument: "OPT".into(),
                side: 1,
                price: 1.0,
                qty: 1.0,
                is_call: true,
                strike: 1.0,
            }],
            opt_cancels: vec!["oc".into()],
        };
        let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
        let coids: Vec<String> = plan
            .commands
            .iter()
            .map(|c| match c {
                Command::Order(OrderIntent::Submit(r)) => r.client_order_id.clone(),
                Command::Order(OrderIntent::Cancel(c)) => format!("cancel:{c}"),
                Command::SetMargin(m) => format!("margin:{}", m.symbol),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            coids,
            vec![
                "ui-0-0".to_string(),
                "cancel:tc".into(),
                "margin:BTCUSDT".into(),
                "dom-1-0".into(),
                "poly-2-0".into(),
                "opt-3-0".into(),
                "cancel:oc".into(),
            ]
        );
    }
}
