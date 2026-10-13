//! The exec-thread state: the command lane, the live exchange, and submit/adopt/cancel/modify.

use std::collections::HashMap;

use serde_json::Value;

use vike_bridge_core::exec_actor::{CancelOutcome, cancel_event};
use vike_bridge_core::transport::VenueApiError;
use vike_exec::EventSender;
use vike_model::events::{
    Event, OrderModified, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_model::{OrderRequest, OrderType};

use crate::base::config::Product;
use crate::base::consts::VENUE;
use crate::base::px;
use crate::base::symbology::Symbology;
use crate::base::transport::HyperliquidTransport;
use crate::exec::event_mapper::{SubmittedOrder, cloid_from_client_order_id, map_order_response};
use crate::exec::user_data::remap_symbol;
use crate::signing::action::{
    Action, CancelByCloidAction, CancelCloidWire, HlBuilderFee, LimitParams, ModifyWire,
    OrderAction, OrderKind, OrderWire,
};
use crate::signing::{NonceManager, Signer};

use super::order_wire::{HlExchange, build_trigger_kind, hl_tif, parse_first_resting_oid};
use super::registry::CloidRegistry;

#[cfg(doc)]
use super::market_slippage_for;

/// Per-order facts exec keeps so a later cancel/modify can be built from just the coid the core
/// passes: the order `asset` id (cancel-by-cloid needs it) and the venue `oid` once the order rests
/// (modify = cancel-replace by oid). Exec-thread-local; never shared.
#[derive(Clone, Copy)]
pub(super) struct OrderMeta {
    asset: u32,
    pub(super) oid: Option<u64>,
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
}

impl HlExchange for LiveExchange {
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
/// `vike_model::venues::attribution::AttributionMechanic::SignedBuilder`, resolved once at mount from
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
    /// stop-MARKET bound in [`build_trigger_kind`]. Resolved once at the mount and carried here, so
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
                        OrderMeta { asset: wire.asset, oid: None },
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
                self.orders
                    .entry(coid.to_string())
                    .or_insert(OrderMeta { asset: inst.asset_id, oid: None });
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
        let action = Action::CancelByCloid(CancelByCloidAction { cancels });
        match self.exchange.place(&action) {
            Ok(resp) => self.map_cancel_response(&resp, &known),
            Err(e) if e.kind().must_requery() => {
                tracing::warn!(venue = VENUE, error = %e, "cancel timed out (ambiguous); awaiting WS/recon");
            }
            Err(e) => {
                for coid in &known {
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
        match self.exchange.place(&action) {
            Ok(resp) => {
                let new_oid = parse_first_resting_oid(&resp);
                if let Some(o) = new_oid {
                    if let Some(meta) = self.orders.get_mut(&coid) {
                        meta.oid = Some(o); // the cancel-replace produced a NEW oid under the same cloid
                    }
                    // Retire the OLD oid: HL may stream a stale `canceled` for it (same cloid); the
                    // pump drops that update so it can't cancel the live re-placed order.
                    if o != oid {
                        self.registry.retire_oid(oid);
                    }
                }
                let _ = self.events.blocking_send(Event::OrderModified(OrderModified {
                    client_order_id: coid,
                    venue_order_id: new_oid.map(|o| o.to_string().into()),
                    new_qty,
                    new_price,
                    ts: order.ts,
                }));
            }
            Err(e) if e.kind().must_requery() => {
                tracing::warn!(venue = VENUE, error = %e, "modify timed out (ambiguous); awaiting WS/recon");
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
