//! `order_dispatch` — the ONE place a drained UI order intent becomes a [`vike_exec::Command`],
//! and therefore the ONE place the local [`crate::orders::order_entry`] preview cap is applied.
//!
//! ## Why this module exists
//! The per-frame "fold the drained tool-window intents onto the command lane" block lived inline in
//! `vike-app`'s `main.rs`. That file was compiled by NOTHING then: the `justfile`'s `ci_crates`
//! omitted `vike-app` (so `just windows-check` skipped it too) and `xtask/src/ci/tables.rs`'s
//! `EXCLUDE_FROM_CI` listed it. (The shell, `vike-desktop` now, is still outside that roster; the
//! `app-check` job has checked and clippy-gated it since, and executes none of this logic.) The
//! consequence was a real hole — of the five submit paths, only
//! three (the DOM ladder `Place`, the Polymarket cockpit `Submit`, the Deribit options confirm
//! ticket) called [`crate::orders::order_entry::validate_with_multiplier`]. **The Trade window — the manual
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
//!  * every test in this module asserts the OUTPUT invariant via `order_dispatch_tests::assert_plan_respects_limits`:
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
//!    `RiskGate` owns that, exactly as [`crate::orders::order_entry::validate_with_multiplier`] documents.
//!    A market entry inside a BRACKET is still notional-capped through the bracket's take-profit
//!    leg (see [`Planner::admit_bracket`]).
//!
//! ## The Trade window's market is the SNAPSHOT's, not a literal
//! The extracted Trade-window path routes to `snap.venue` / `snap.symbol` where `main.rs` used to
//! hardcode `("binance", "BTCUSDT")`. In the local FAT build (deleted 2026-09-09) this was
//! byte-identical — `vike_mount::build_node` mounts the `BINANCE_MARKET` row (binance, `BTCUSDT`) as
//! the PRIMARY engine and `CoreSnapshot::build` publishes that primary's `(venue, symbol)` verbatim.
//! It stops being identical in exactly the case where the literal was WRONG — and that case is
//! every launch now: the desktop observer (a `vike-app --observe <ADDR>` thin client when this was
//! written) with control enabled renders a REMOTE `vike-tradehub` daemon's snapshot
//! ([`crate::backend::observe_bridge::wire_to_core`] carries the daemon's `venue`/`symbol` through), and
//! that daemon's profile venue defaults to `"polymarket"` (or `"hyperliquid"`). The Trade card
//! renders `snap.symbol` and sizes against `snap.symbol` — so the panel showed one market and the
//! BUY button submitted another. The pre-first-frame observer placeholder has EMPTY strings, which
//! is not a routable market either; that is now an explicit
//! [`DispatchRejectReason::NoRoutableMarket`] rather than a silent misroute.

use crate::orders::order_entry::{self, OrderLimits, OrderReject, OrderTicket};
use crate::tools::{OptOrderTicket, TradeSubmit};
use crate::ui::tool_views::CockpitCmd;
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
    /// The local [`crate::orders::order_entry`] preview rejected it (malformed shape, or over a cap).
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
    /// (Not [`crate::orders::order_entry::next_client_order_id`], which is the two-part `"{prefix}-{seq}"`
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
    /// [`crate::orders::order_entry::validate_with_multiplier`] reads only `qty`/`price`/`trigger_price`/
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
    ///
    /// ⚠ **THAT PAIR IS THE PRIMARY ENGINE'S, AND ON AN OBSERVED DAEMON IT NAMES NO MOUNT** — a
    /// declared residual, not a claim that the module doc above is wrong. That doc's point stands:
    /// routing to `snap.venue`/`snap.symbol` is SELF-CONSISTENT with what the Trade card renders,
    /// where the old `("binance", "BTCUSDT")` literal was not. What neither states is what the pair
    /// MEANS. `CoreSnapshot::venue`/`symbol` mirror the PRIMARY ENGINE, and the primary engine is
    /// the first row of `crates/vike-tradehub/src/wired_markets.rs`'s `WIRED_MARKETS`
    /// (`BINANCE_MARKET`), which `build_node` mounts first and holds as its primary engine
    /// unconditionally — so on a daemon whose only STRATEGY MOUNT is bybit, both the card and this
    /// route read `"binance"`: a human charting bybit cannot place a Trade-window order on it at
    /// all, and the order instead reaches whatever binance engine that node built — PAPER when
    /// binance is unarmed, but a LIVE one on a box where the policy ceiling and credentials arm it.
    ///
    /// Same root constant as the empty-bar-lane defect fixed in
    /// `crates/vike-tradehub/src/publish.rs`'s `project_bar_series` — but this one MOVES ORDERS, so
    /// it is deliberately NOT repaired as a side effect of that chart fix. [`TradeSubmit`] carries
    /// no addressing fields at all (unlike [`Self::plan_dom`]'s intents, which are tagged with
    /// their own venue + instrument), so a repair means adding a routing surface and deciding which
    /// mount an untagged ticket belongs to: its own review, not a one-line change to a live order
    /// path.
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
                    let signed =
                        crate::orders::dom_math::signed_position_size(p.size, &p.position_side);
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

#[path = "order_dispatch_tests.rs"]
#[cfg(test)]
mod order_dispatch_tests;
