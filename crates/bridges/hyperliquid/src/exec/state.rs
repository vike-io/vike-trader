//! The exec-thread state: the command lane, the live exchange, and submit/adopt/cancel/modify.

use std::collections::HashMap;

use serde_json::Value;

use vike_bridge_core::exec_actor::{CancelOutcome, cancel_event};
use vike_bridge_core::transport::VenueApiError;
use vike_exec::EventSender;
use vike_model::events::{
    Event, OrderModified, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_model::{OrderRequest, OrderStatusReport, OrderType};

use crate::base::config::Product;
use crate::base::consts::VENUE;
use crate::base::px;
use crate::base::symbology::Symbology;
use crate::base::transport::HyperliquidTransport;
use crate::exec::event_mapper::{
    SubmittedOrder, accept_implied_by_fill, cloid_from_client_order_id, map_order_response,
};
use crate::exec::recon_client::parse_orders;
use crate::exec::user_data::remap_symbol;
use crate::signing::action::{
    Action, CancelByCloidAction, CancelCloidWire, HlBuilderFee, LimitParams, ModifyWire,
    OrderAction, OrderKind, OrderWire,
};
use crate::signing::{NonceManager, Signer};

use super::order_wire::{
    HlExchange, ModifyReply, OrderStatusAnswer, build_trigger_kind, hl_tif, is_wire_cloid,
    parse_modify_reply, parse_order_status,
};
use super::registry::{CloidRegistry, RETIRED_TTL};

#[cfg(doc)]
use super::market_slippage_for;

/// Per-order facts exec keeps so a later cancel/modify can be built from just the coid the core
/// passes: the order `asset` id (cancel-by-cloid needs it) and the venue `oid` once the order rests
/// (modify = cancel-replace by oid). Exec-thread-local; never shared.
#[derive(Clone, Copy)]
pub(super) struct OrderMeta {
    asset: u32,
    pub(super) oid: Option<u64>,
    /// Set when a modify's outcome is UNKNOWN (a post-send timeout, or a reply that named no new
    /// oid): `oid` may be dead (the replace applied) or still live. Within [`RETIRED_TTL`] of it a
    /// further modify is refused; a stale `canceled` for the old oid is suppressed for the same window.
    pub(super) unknown_since: Option<std::time::Instant>,
    /// The one `orderStatus` requery after an unknown modify has been spent (reset on each mark).
    requeried: bool,
}

/// The command lane the newtype pushes onto the exec thread. Carries the batch verbs the shared
/// `ExecActor`'s `ExecCommand` lacks (HL is batch-first).
#[derive(Debug)]
pub(super) enum HlCommand {
    Submit(Box<OrderRequest>),
    SubmitBatch(Vec<OrderRequest>),
    Cancel(String),
    CancelBatch(Vec<String>),
    Modify {
        order: Box<OrderRequest>,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    },
    /// An order a previous session left resting (`ExecutionClient::adopt_restored`): teach the exec
    /// thread its asset so a later `Cancel` can be built. FIFO with `Cancel` on this one lane.
    Adopt {
        coid: String,
        symbol: String,
    },
    Shutdown,
}

/// The live [`HlExchange`]: a [`HyperliquidTransport`] + the [`Signer`] + the monotonic nonce.
pub(super) struct LiveExchange {
    pub(super) transport: HyperliquidTransport,
    pub(super) signer: Signer,
    pub(super) nonce: NonceManager,
    /// The MASTER address `orderStatus` is scoped to (an agent key's own address sees nothing).
    pub(super) master: String,
}

impl HlExchange for LiveExchange {
    fn resting_oid(&self, cloid: &str) -> Result<OrderStatusAnswer, ()> {
        // `orderStatus` takes the cloid (0x hex) in its `oid` field.
        let body = serde_json::json!({ "type": "orderStatus", "user": self.master, "oid": cloid });
        Ok(parse_order_status(&self.transport.info(&body).map_err(|_| ())?))
    }

    fn resting_orders(&self) -> Result<Vec<OrderStatusReport>, ()> {
        // The very read (same address, same parser) that raised the held reconcile row.
        let body = serde_json::json!({ "type": "frontendOpenOrders", "user": self.master });
        let listed = self.transport.info(&body).map_err(|e| e.msg);
        listed.and_then(|v| parse_orders(&v.to_string())).map_err(|error| {
            tracing::warn!(venue = VENUE, %error, "frontendOpenOrders read failed");
        })
    }

    fn place(&self, action: &Action) -> Result<Value, VenueApiError> {
        // v1: no vault, no expiresAfter. `exchange` posts EXACTLY once (never auto-retries) so a
        // post-send timeout can't double-submit — the caller re-queries via the WS/recon instead.
        let nonce = self.nonce.next();
        self.transport.exchange(action, &self.signer, nonce, None)
    }

    fn mid(&self, coin: &str) -> Option<f64> {
        let mids = self.transport.info(&serde_json::json!({ "type": "allMids" })).ok()?;
        mids.get(coin).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok())
    }
}

/// The exec-thread state: the exchange seam, the shared cloid registry (written here, read by the
/// pump), the event lane, the per-order asset/oid map, and the configured Builder Code (task 7:
/// `vike_venues::attribution::AttributionMechanic::SignedBuilder`, resolved once at start from
/// `.env` — `None` when unconfigured, the default, and byte-identical to before this field existed).
/// Generic over [`HlExchange`] so the command handling is exercised with a fake in tests.
pub(super) struct ExecState<X: HlExchange> {
    pub(super) exchange: X,
    pub(super) registry: CloidRegistry,
    pub(super) events: EventSender,
    pub(super) orders: HashMap<String, OrderMeta>,
    pub(super) builder: Option<HlBuilderFee>,
    /// The mount-resolved emulated-market slippage band ([`market_slippage_for`]) — the ONLY two
    /// consumers are the emulated-market price in [`Self::build_order_wire`] and the protective
    /// stop-MARKET bound in [`build_trigger_kind`]. Resolved once at the start and carried here, so
    /// no thread re-reads a config and the two sites can never use different bands.
    pub(super) slippage: f64,
}

impl<X: HlExchange> ExecState<X> {
    /// Build one order's signed wire form + the positional [`SubmittedOrder`] the response mapper
    /// needs. `Err(reason)` for an un-placeable order (unknown symbol, side 0, sub-step size, missing
    /// price, or no mid for an emulated market) — the caller turns it into a terminal [`OrderRejected`].
    fn build_order_wire(
        &self,
        symbology: &Symbology,
        req: &OrderRequest,
    ) -> Result<(OrderWire, SubmittedOrder), String> {
        if req.side == 0 {
            return Err("order side is 0 (neither buy nor sell)".to_string());
        }
        let inst = symbology
            .by_symbol(&req.symbol)
            .ok_or_else(|| format!("unknown symbol {}", req.symbol))?;
        let is_spot = matches!(inst.product, Product::Spot);
        let is_buy = req.side > 0;

        let sz = px::round_size(req.qty, inst.sz_decimals);
        if sz == "0" {
            return Err(format!("size {} rounds below the {}-dp step", req.qty, inst.sz_decimals));
        }

        // Request shapes fold onto HL's two `OrderKind`s. A "stop"/"take_profit" (or any request
        // carrying a `trigger_price`) becomes an `OrderKind::Trigger` (its `tpsl` set from the
        // order_type); "market"/"limit" stay the existing `OrderKind::Limit` paths, byte-for-byte.
        // An unrecognised kind is refused here (it used to rest as a plain limit, or trigger when
        // it carried a `trigger_price`); the core preflight refuses one before it gets this far.
        if let OrderType::Unrecognized(word) = &req.order_type {
            return Err(format!("unsupported order_type `{word}` on {VENUE}; order refused"));
        }
        let is_trigger = matches!(req.order_type, OrderType::Stop | OrderType::TakeProfit)
            || req.trigger_price.is_some();
        let (limit_px, order_kind) = if is_trigger {
            // Trigger-source law: HL carries NO trigger-by field — triggers evaluate against
            // MARK by venue law (the `hyperliquid` row of
            // `vike_bridge_core::trigger::venue_trigger_by`). A requested source that matches
            // (Mark) proceeds with nothing to emit; a conflicting one (Last/Index) is a LOUD
            // local reject through this Err path — never silently triggered off a different
            // price series. `None` = the venue's law, byte-identical as ever.
            if let Some(source) = req.trigger_by {
                use vike_bridge_core::trigger::{TriggerByOutcome, venue_trigger_by};
                if venue_trigger_by(VENUE, source) != TriggerByOutcome::VenueLaw {
                    return Err(format!(
                        "trigger_by {source:?} is not supported on {VENUE} — triggers evaluate \
                         against MARK by venue law; order refused, never silently coerced"
                    ));
                }
            }
            build_trigger_kind(req, inst.sz_decimals, is_spot, is_buy, self.slippage)?
        } else if req.order_type == OrderType::Market {
            // HL has no native market order — emulate as an `Ioc` limit at an aggressive price.
            let mid = self.exchange.mid(&inst.coin).ok_or_else(|| {
                format!("no mid available to emulate a market order for {}", req.symbol)
            })?;
            let slip = if is_buy { 1.0 + self.slippage } else { 1.0 - self.slippage };
            let px = px::clamp_price(mid * slip, inst.sz_decimals, is_spot);
            (px, OrderKind::Limit(LimitParams { tif: "Ioc".to_string() }))
        } else {
            // plain limit at the requested price (a `limit`, or a `stop_limit` that carries no
            // `trigger_price`, exactly as before).
            let price =
                req.price.ok_or_else(|| format!("{} order without a price", req.order_type))?;
            let px = px::clamp_price(price, inst.sz_decimals, is_spot);
            (px, OrderKind::Limit(LimitParams { tif: hl_tif(req.time_in_force).to_string() }))
        };
        if limit_px == "0" {
            return Err("price rounds to zero".to_string());
        }

        let cloid = self.registry.register(&req.client_order_id);
        let req_sz = sz.parse::<f64>().unwrap_or(req.qty);
        let wire = OrderWire {
            asset: inst.asset_id,
            is_buy,
            limit_px,
            sz,
            reduce_only: req.reduce_only,
            order_type: order_kind,
            cloid: Some(cloid),
        };
        let sub = SubmittedOrder {
            client_order_id: req.client_order_id.clone(),
            coin: inst.coin.clone(),
            side: if is_buy { 1 } else { -1 },
            req_sz,
            ts: req.ts,
        };
        Ok((wire, sub))
    }

    /// Submit N orders as ONE native `order` action (HL is batch-first). Emits [`OrderSubmitted`]
    /// synchronously per order first, rejects un-buildable ones locally, sends the rest, then maps
    /// the positional response and captures each resting `oid`.
    pub(super) fn submit(&mut self, symbology: &Symbology, reqs: &[OrderRequest]) {
        let mut wires = Vec::with_capacity(reqs.len());
        let mut subs = Vec::with_capacity(reqs.len());
        for req in reqs {
            let _ = self.events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: req.client_order_id.clone(),
                ts: req.ts,
            }));
            match self.build_order_wire(symbology, req) {
                Ok((wire, sub)) => {
                    self.orders.insert(
                        req.client_order_id.clone(),
                        OrderMeta {
                            asset: wire.asset,
                            oid: None,
                            unknown_since: None,
                            requeried: false,
                        },
                    );
                    wires.push(wire);
                    subs.push(sub);
                }
                Err(reason) => {
                    let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                        client_order_id: req.client_order_id.clone(),
                        reason: reason.into(),
                        ts: req.ts,
                    }));
                }
            }
        }
        if wires.is_empty() {
            return;
        }
        let action = Action::Order(OrderAction {
            orders: wires,
            grouping: "na".to_string(),
            builder: self.builder.clone(),
        });
        match self.exchange.place(&action) {
            Ok(resp) => {
                for mut ev in map_order_response(&resp, VENUE, &subs) {
                    // A `filled` answer is the order's only word from the venue: say the accept it
                    // implies first (`accept_implied_by_fill`). Its oid is NOT captured below, as before.
                    if let Some(accept) = accept_implied_by_fill(&ev) {
                        let _ = self.events.blocking_send(accept);
                    }
                    // Capture the resting oid so a later modify can cancel-replace by it.
                    if let Event::OrderAccepted(a) = &ev
                        && let (Some(oid), Some(meta)) =
                            (a.venue_order_id.as_ref(), self.orders.get_mut(&a.client_order_id))
                    {
                        meta.oid = oid.parse().ok();
                    }
                    remap_symbol(&mut ev, symbology);
                    let _ = self.events.blocking_send(ev);
                }
            }
            Err(e) if e.kind().must_requery() => {
                // Ambiguous post-send timeout: the batch MAY have landed. NEVER synthesize a reject
                // (that would strand a phantom position) — the WS pump / recon resolves the truth.
                tracing::warn!(venue = VENUE, error = %e, "submit timed out (ambiguous); awaiting WS/recon");
            }
            Err(e) => {
                // Definite failure (nothing landed): reject every order so none silently vanishes.
                for sub in &subs {
                    let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                        client_order_id: sub.client_order_id.clone(),
                        reason: e.msg.clone().into(),
                        ts: sub.ts,
                    }));
                }
            }
        }
    }

    /// Learn a restored order's asset from its UNIFIED symbol (`Symbology::by_symbol`), so a cancel
    /// by coid can be built after a restart. Never overwrites an entry (a live order's `oid` stays);
    /// a symbol the symbology does not know inserts nothing, so the cancel still rejects loudly
    /// rather than guessing an asset id.
    pub(super) fn adopt(&mut self, symbology: &Symbology, coid: &str, symbol: &str) {
        match symbology.by_symbol(symbol) {
            Some(inst) => {
                self.orders.entry(coid.to_string()).or_insert(OrderMeta {
                    asset: inst.asset_id,
                    oid: None,
                    unknown_since: None,
                    requeried: false,
                });
            }
            None => tracing::warn!(
                venue = VENUE,
                coid,
                symbol,
                "restored order names a symbol this symbology does not know; it cannot be cancelled"
            ),
        }
    }

    /// Cancel N orders as ONE native `cancelByCloid` action. Unknown coids (no local asset mapping)
    /// surface a non-terminal [`Event::OrderCancelRejected`] — never swallowed (audit A2).
    pub(super) fn cancel(&mut self, coids: &[String]) {
        let mut cancels = Vec::new();
        let mut known = Vec::new();
        for coid in coids {
            match self.orders.get(coid) {
                Some(meta) => {
                    cancels.push(CancelCloidWire {
                        asset: meta.asset,
                        cloid: cloid_from_client_order_id(coid),
                    });
                    known.push(coid.clone());
                }
                None => {
                    let _ = self.events.blocking_send(cancel_event(
                        coid,
                        CancelOutcome::Rejected("no local order mapping to cancel".to_string()),
                    ));
                }
            }
        }
        if cancels.is_empty() {
            return;
        }
        self.send_cancels(cancels, &known);
    }

    /// One command-lane cancel: the coid decides the path. A venue-printed `cloid` ([`is_wire_cloid`])
    /// that this thread holds no entry for is an order SOME OTHER session or hand placed, named by the
    /// only id the venue gave back for it: [`Self::cancel_wire_cloid`]. Every other id, and a hex id
    /// this thread DOES hold, takes [`Self::cancel`] unchanged.
    pub(super) fn cancel_any(&mut self, symbology: &Symbology, id: &str) {
        if is_wire_cloid(id) && !self.orders.contains_key(id) {
            self.cancel_wire_cloid(symbology, id);
        } else {
            self.cancel(&[id.to_string()]);
        }
    }

    /// Cancel the resting order whose venue `cloid` is `cloid`, an order this thread never placed (the
    /// `recon cancel` of a held `UnknownOrder` row, whose id is the hash of a coid nothing stores and
    /// nothing can invert). `cancelByCloid` takes exactly that hex, so it is sent VERBATIM (hashing it
    /// again would name another order), on the asset of the coin the account's own open-order list
    /// shows for it: the list is read now, so an order that is gone, a failed read, a coin the
    /// symbology lacks and a `cloid` listed more than once are each an [`Event::OrderCancelRejected`]
    /// naming `cloid`, with nothing sent.
    fn cancel_wire_cloid(&mut self, symbology: &Symbology, cloid: &str) {
        let Ok(listed) = self.exchange.resting_orders() else {
            let why =
                "the venue's open-order list could not be read, so the order was not cancelled";
            return self.refuse_wire_cancel(cloid, why.to_string());
        };
        let mut rows = listed.iter().filter(|r| r.client_order_id.as_deref() == Some(cloid));
        let Some(row) = rows.next() else {
            let why = "no resting order of this account carries that cloid (already gone or \
                       filled?); nothing was sent";
            return self.refuse_wire_cancel(cloid, why.to_string());
        };
        // The venue is not known to keep a cloid unique across coins: cancelling the first of two
        // would name a guess.
        if rows.next().is_some() {
            let why = "that cloid is listed more than once, so which order it names is unknown; \
                       nothing was sent, cancel it at the venue";
            return self.refuse_wire_cancel(cloid, why.to_string());
        }
        let Some(inst) = symbology.by_coin(&row.symbol) else {
            let why = format!(
                "the order rests on coin {}, which the loaded symbology does not know, so its \
                 asset is unknown; cancel it at the venue",
                row.symbol
            );
            return self.refuse_wire_cancel(cloid, why);
        };
        let cancels = vec![CancelCloidWire { asset: inst.asset_id, cloid: cloid.to_string() }];
        self.send_cancels(cancels, &[cloid.to_string()]);
    }

    /// The loud refusal of [`Self::cancel_wire_cloid`]: a WARN and one non-terminal rejection.
    fn refuse_wire_cancel(&self, cloid: &str, why: String) {
        tracing::warn!(venue = VENUE, cloid, %why, "cancel of a venue cloid refused; nothing sent");
        let _ = self.events.blocking_send(cancel_event(cloid, CancelOutcome::Rejected(why)));
    }

    /// Send ONE `cancelByCloid` action and answer for each of `known` (the coid each `cancels` entry
    /// stands for, in order): the response mapped per entry, an ambiguous timeout a non-terminal
    /// advisory, a definite failure a rejection.
    fn send_cancels(&mut self, cancels: Vec<CancelCloidWire>, known: &[String]) {
        let action = Action::CancelByCloid(CancelByCloidAction { cancels });
        match self.exchange.place(&action) {
            Ok(resp) => self.map_cancel_response(&resp, known),
            Err(e) if e.kind().must_requery() => {
                tracing::warn!(venue = VENUE, error = %e, "cancel timed out (ambiguous); awaiting WS/recon");
                // NON-terminal advisory (the order may still be live, or already gone): a cancel the
                // operator asked for must not vanish without an event, exactly as modify's arm.
                for coid in known {
                    let _ = self.events.blocking_send(cancel_event(
                        coid,
                        CancelOutcome::Rejected(format!(
                            "cancel outcome unknown (timed out after send; the venue may have applied it): {}",
                            e.msg
                        )),
                    ));
                }
            }
            Err(e) => {
                for coid in known {
                    let _ = self
                        .events
                        .blocking_send(cancel_event(coid, CancelOutcome::Rejected(e.msg.clone())));
                }
            }
        }
    }

    /// Map the positional cancel response: top-level `err` → reject all; each `"success"` →
    /// [`Event::OrderCanceled`], each `{error}` → [`Event::OrderCancelRejected`] (already gone/filled).
    fn map_cancel_response(&self, resp: &Value, coids: &[String]) {
        if resp.get("status").and_then(|s| s.as_str()) == Some("err") {
            let reason =
                resp.get("response").and_then(|r| r.as_str()).unwrap_or("cancel request failed");
            for coid in coids {
                let _ = self
                    .events
                    .blocking_send(cancel_event(coid, CancelOutcome::Rejected(reason.to_string())));
            }
            return;
        }
        let statuses = resp
            .get("response")
            .and_then(|r| r.get("data"))
            .and_then(|d| d.get("statuses"))
            .and_then(|s| s.as_array());
        let Some(statuses) = statuses else {
            return; // unrecognized ok-envelope — the WS/recon owns that residual
        };
        for (st, coid) in statuses.iter().zip(coids) {
            let outcome = if st.as_str() == Some("success") {
                CancelOutcome::Canceled
            } else if let Some(err) = st.get("error").and_then(|e| e.as_str()) {
                CancelOutcome::Rejected(err.to_string())
            } else {
                CancelOutcome::Rejected("cancel not confirmed".to_string())
            };
            let _ = self.events.blocking_send(cancel_event(coid, outcome));
        }
    }

    /// The modify of `coid` may have replaced `oid` (new oid unknown): keep `oid`, remember WHEN, and
    /// retire it so HL's stale `canceled` for it (same cloid) cannot end the live re-placed order.
    fn mark_modify_unknown(&mut self, coid: &str, oid: u64) {
        if let Some(meta) = self.orders.get_mut(coid) {
            meta.unknown_since = Some(std::time::Instant::now());
            meta.requeried = false;
        }
        self.registry.retire_oid(oid);
    }

    /// After an unknown modify, learn the oid the order rests under now. A WS row naming a DIFFERENT
    /// oid proves the replace applied and is adopted (free); a WS row naming the held oid proves
    /// nothing (it may predate the modify, or the modify may not be processed yet), so it never
    /// lifts the old oid's retirement or the unknown mark. Only ONE `orderStatus` by cloid answers
    /// that authoritatively: open under a new oid adopts it; open under the held oid changes NOTHING
    /// until [`RETIRED_TTL`] has passed since the mark (the first modify may still be in flight at the
    /// venue, or `/info` may lag it), after which it withdraws the retirement (the order never moved).
    /// The one-shot is spent only by an answer that settles something: open under a new oid, or the
    /// venue's definitive `unknownOid`. A failed read, a `canceled`/`filled`/`replaced` status (it can
    /// flip under a replace) and an open-under-the-held-oid inside the window leave it unspent. The
    /// OLD oid stays retired otherwise (its stale `canceled` is suppressed, once, within
    /// [`RETIRED_TTL`]).
    fn learn_oid_after_unknown_modify(&mut self, coid: &str) {
        let Some(meta) = self.orders.get(coid).copied() else { return };
        let Some(since) = meta.unknown_since else { return };
        let Some(old) = meta.oid else { return };
        let cloid = cloid_from_client_order_id(coid);
        let new = match self.registry.take_live_oid(&cloid) {
            Some(ws) if ws != old => ws,
            _ => {
                if meta.requeried {
                    return;
                }
                let Ok(answer) = self.exchange.resting_oid(&cloid) else { return };
                let spend = |orders: &mut HashMap<String, OrderMeta>| {
                    if let Some(m) = orders.get_mut(coid) {
                        m.requeried = true;
                    }
                };
                match answer {
                    OrderStatusAnswer::Open(open) if open == old => {
                        if since.elapsed() < RETIRED_TTL {
                            return;
                        }
                        spend(&mut self.orders);
                        self.registry.take_retired(old);
                        open
                    }
                    OrderStatusAnswer::Open(open) => {
                        spend(&mut self.orders);
                        open
                    }
                    OrderStatusAnswer::UnknownOid => {
                        spend(&mut self.orders);
                        return;
                    }
                    OrderStatusAnswer::NotOpen => return,
                }
            }
        };
        if let Some(m) = self.orders.get_mut(coid) {
            m.oid = Some(new);
            m.unknown_since = None;
        }
    }

    /// Modify a resting order via native cancel-replace (`Action::Modify`) keyed by the captured
    /// `oid`. No resting oid (not yet accepted / unknown here) or an un-buildable replacement →
    /// a non-terminal [`Event::OrderModifyRejected`] (the order keeps its terms — never swallowed).
    pub(super) fn modify(
        &mut self,
        symbology: &Symbology,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) {
        let coid = order.client_order_id.clone();
        let Some(oid) = self.orders.get(&coid).and_then(|m| m.oid) else {
            let _ = self.events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: coid,
                reason: "no resting venue order to modify".into(),
                ts: order.ts,
            }));
            return;
        };
        self.learn_oid_after_unknown_modify(&coid);
        let oid = self.orders.get(&coid).and_then(|m| m.oid).unwrap_or(oid);
        // A previous modify of this order left its outcome unknown: the held oid may be dead. Refuse
        // loudly inside the window instead of aiming a replace at it; a cancel is never blocked.
        if let Some(t) = self.orders.get(&coid).and_then(|m| m.unknown_since)
            && t.elapsed() < RETIRED_TTL
        {
            let _ = self.events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: coid,
                reason: "modify refused: outcome of the previous modify unknown, requery pending"
                    .into(),
                ts: order.ts,
            }));
            return;
        }
        // The passed `order` carries the resting terms; apply the requested changes over them.
        let mut mreq = order.clone();
        if let Some(q) = new_qty {
            mreq.qty = q;
        }
        if let Some(p) = new_price {
            mreq.price = Some(p);
        }
        let wire = match self.build_order_wire(symbology, &mreq) {
            Ok((wire, _)) => wire,
            Err(reason) => {
                let _ =
                    self.events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                        client_order_id: coid,
                        reason: reason.into(),
                        ts: order.ts,
                    }));
                return;
            }
        };
        let action = Action::Modify(ModifyWire { oid, order: wire });
        // Only rows streamed after this send may be read as its outcome.
        self.registry.forget_live_oid(&cloid_from_client_order_id(&coid));
        match self.exchange.place(&action) {
            Ok(resp) => match parse_modify_reply(&resp) {
                // `/exchange` answers HTTP 200 for a refusal too (`{status:"err"}` or
                // `statuses[0].error`): the order keeps its terms and the venue's reason surfaces.
                ModifyReply::Refused(reason) => {
                    let _ = self.events.blocking_send(Event::OrderModifyRejected(
                        OrderModifyRejected {
                            client_order_id: coid,
                            reason: reason.into(),
                            ts: order.ts,
                        },
                    ));
                }
                ModifyReply::Applied { new_oid } => {
                    if let Some(o) = new_oid {
                        if let Some(meta) = self.orders.get_mut(&coid) {
                            meta.oid = Some(o); // the cancel-replace produced a NEW oid under the same cloid
                            meta.unknown_since = None;
                        }
                        // Retire the OLD oid: HL may stream a stale `canceled` for it (same cloid); the
                        // pump drops that update so it can't cancel the live re-placed order.
                        if o != oid {
                            self.registry.retire_oid(oid);
                        }
                    } else {
                        // Applied, but the reply named no resting/filled oid: the held oid may be dead.
                        self.mark_modify_unknown(&coid, oid);
                    }
                    let _ = self.events.blocking_send(Event::OrderModified(OrderModified {
                        client_order_id: coid,
                        venue_order_id: new_oid.map(|o| o.to_string().into()),
                        new_qty,
                        new_price,
                        ts: order.ts,
                    }));
                }
            },
            Err(e) if e.kind().must_requery() => {
                // Ambiguous post-send timeout: the replace MAY have landed (and then the resting
                // oid changed). Never claim success or a terminal failure; say so explicitly as a
                // NON-terminal advisory — the WS `orderUpdates`/recon own the truth.
                tracing::warn!(venue = VENUE, error = %e, "modify timed out (ambiguous); awaiting WS/recon");
                self.mark_modify_unknown(&coid, oid);
                let _ = self.events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                    client_order_id: coid,
                    reason: format!(
                        "modify outcome unknown (timed out after send; the venue may have applied it): {}",
                        e.msg
                    )
                    .into(),
                    ts: order.ts,
                }));
            }
            Err(e) => {
                let _ =
                    self.events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                        client_order_id: coid,
                        reason: e.msg.clone().into(),
                        ts: order.ts,
                    }));
            }
        }
    }
}
