//! Hyperliquid execution — the [`ExecutionClient`] over a dedicated `ExecActor`-style command thread.
//!
//! Mirrors the Bybit/OKX exec anatomy (a newtype forwarding to an owned command thread with
//! deterministic Shutdown-and-join teardown), but with a HL-specific command enum: HL's `/exchange`
//! order action is **batch-first**, so `submit_batch`/`cancel_batch` are NATIVE (one signed action
//! with N items, [`vike_model::venue_caps::HYPERLIQUID::supports_native_batch`]) rather than the
//! trait-default fan-out. The shared [`vike_bridge_core::exec_actor::ExecActor`] carries no batch
//! command, which is why this crate rolls its own thin actor.
//!
//! Order flow (research §6):
//! - Rust emits [`OrderSubmitted`] synchronously at submit; the venue terminal events come from the
//!   `/exchange` response ([`map_order_response`]) AND the WS pump ([`crate::user_data`]).
//! - Limit orders map `time_in_force` → the case-sensitive wire TIF (`Gtc`/`Ioc`); **market orders
//!   are emulated** as an `Ioc` limit at ±the mount-resolved slippage band ([`market_slippage_for`],
//!   default [`DEFAULT_MARKET_SLIPPAGE`]) off the current mid (fetched via `allMids`); no quote ⇒ a
//!   locally-synthesized [`OrderRejected`].
//! - **Stop/trigger orders** (`order_type == "stop"`, or any request carrying a `trigger_price`) map
//!   to `OrderKind::Trigger { is_market, trigger_px, tpsl }` (research §6): a stop-MARKET (no
//!   `price`) fires aggressively when it trips (protective ± that SAME band off the trigger,
//!   NOT the far-away current mid); a stop-LIMIT (`price` present) rests at the requested limit.
//!   `tpsl` is `"sl"` for a stop-loss (`order_type == "stop"` / a bare `trigger_price`) or `"tp"` for
//!   a take-profit (`order_type == "take_profit"`) — see [`build_trigger_kind`].
//! - `cancel`/`cancel_batch` use `cancelByCloid` (the cloid is derived from the coid); `modify` is a
//!   native cancel-replace (`Action::Modify`) keyed by the resting venue `oid`.
//! - Every failure path is explicit: a build failure (unknown symbol / sub-step size / no mid) and a
//!   DEFINITE transport error synthesize a terminal [`OrderRejected`] — no order silently vanishes.
//!   The ambiguous post-send timeout ([`vike_bridge_core::transport::ErrorKind::must_requery`]) is the
//!   ONE exception: it is never rejected (that would strand a phantom position); the WS pump / recon
//!   resolves the truth.
//!
//! Cloid carry-through: HL's `cloid = keccak(coid)` is one-way, so exec REGISTERS every mapping into a
//! shared [`CloidRegistry`] at submit and the WS pump RESOLVES it when an `orderUpdates`/`userFills`
//! frame carries the cloid back (the pump is spawned here, sharing the registry + the symbology).
//!
//! LIVE GATE: absent credentials never reach here — the composition root only builds a [`Signer`] and
//! spawns this when the workspace `.env` yields a private key (absent-creds-is-the-live-gate).
//!
//! NOW WIRED (see the impls below): the reconnect→reconcile poke (the pump pokes the `recon_trigger`
//! threaded through [`HyperliquidExecutionClient::spawn`] on every WS reconnect, so a reconcile pass
//! re-syncs state missed while the socket was down), and **stale-`orderUpdates`-cancel suppression
//! during a native `modify`** — a native `Action::Modify` keeps the SAME cloid on a NEW oid, so a
//! `canceled` streamed for the retired old oid would (same cloid → same coid) prematurely terminate
//! the FSM entry the new oid now owns. `modify` records the old oid on [`CloidRegistry`] with a TTL
//! *after* the replace succeeds (so a REJECTED modify never mis-fires) and the pump drops a cancel
//! for a retired oid. Residual: a `canceled` the pump processes before exec finishes the retire
//! (WS-faster-than-REST) still slips through — the periodic reconcile is the backstop.
//!
//! NOT YET (deliberately out of this fan-out): pruning the per-order/cloid maps on terminal events
//! (they grow with a session's submitted orders).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::exec_actor::{CancelOutcome, cancel_event};
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{
    Event, OrderModified, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_model::{OrderRequest, TimeInForce, now_ms};

use crate::config::{Network, Product};
use crate::consts::VENUE;
use crate::event_mapper::{SubmittedOrder, cloid_from_client_order_id, map_order_response};
use crate::funding::{HlFundingPoller, fetch_funding};
use crate::instruments::HyperliquidInstruments;
use crate::px;
use crate::signing::action::{
    Action, CancelByCloidAction, CancelCloidWire, HlBuilderFee, LimitParams, ModifyWire,
    OrderAction, OrderKind, OrderWire, TriggerParams,
};
use crate::signing::{NonceManager, Signer};
use crate::symbology::Symbology;
use crate::transport::HyperliquidTransport;
use crate::user_data::{remap_symbol, spawn_hyperliquid_user_data};

/// FALLBACK emulated-market slippage band off the reference price — the literal this adapter priced
/// every market order with unconditionally before the operator's band was threaded in (research §6:
/// all HL adapters emulate a market order as an `Ioc` limit at ±5%). Used ONLY when the operator
/// expressed no band, so a mount that sets nothing is byte-identical to every mount before
/// [`market_slippage_for`] existed.
///
/// ⚠ Read what this number actually authorizes. It is not a slippage ESTIMATE and not a fee — it is
/// the worst price the order is ALLOWED to reach. On a deep BTC book it never binds (the `Ioc` fills
/// at the touch); on a thin alt book it binds completely, and the order fills 5% away **silently** —
/// no rejection, no alert, nothing in the fill distinguishing "the market moved" from "we authorized
/// this". A liquid major wants 0.1–0.5%; the safe direction is DOWN. Hence
/// [`vike_bridge_core::market_slippage`], whose ceiling is exactly this value, making the knob a
/// one-way ratchet.
const DEFAULT_MARKET_SLIPPAGE: f64 = 0.05;

/// The emulated-market slippage band this adapter prices with, resolved from the operator's policy.
///
/// Pure, and `pub` on purpose — the SAME shape [`crate::exec`]'s perp siblings use for leverage
/// (`vike_binance::exec::leverage_for` and friends): the composition root resolves this ONCE per
/// mount and threads the resolved `f64` into [`HyperliquidExecutionClient::spawn_with_market_slippage`].
/// Nothing inside the adapter reaches for a config, and no spawned thread re-reads one, so the band
/// on the wire and the band the operator configured can never disagree.
///
/// Unset (`None`) ⇒ [`DEFAULT_MARKET_SLIPPAGE`], byte-identical to before. An out-of-range value is
/// clamped and warned, never honored — see `vike_bridge_core::market_slippage` for the full rule.
pub fn market_slippage_for(requested: Option<f64>) -> f64 {
    vike_bridge_core::market_slippage::resolve_market_slippage(requested, DEFAULT_MARKET_SLIPPAGE)
}
/// Funding-payment poll cadence (`userFunding`; not on either WS channel this crate subscribes to
/// — see `crate::user_data`'s module doc). 60s keeps live equity current without over-querying.
const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Deterministic `cloid → framework client-order-id` table (see [`cloid_from_client_order_id`]).
///
/// HL's `cloid` is `keccak(coid)` — one-way — so the WS pump cannot invert it; exec REGISTERS each
/// mapping at submit and the pump RESOLVES it when an `orderUpdates`/`userFills` frame carries the
/// cloid back. Shared (`Arc<Mutex<_>>`) between the exec thread (writer) and the pump thread (reader).
#[derive(Clone, Default)]
pub struct CloidRegistry {
    inner: Arc<Mutex<HashMap<String, String>>>,
    /// oids retired by a native `modify` (cancel-replace), each stamped with the instant it was
    /// retired. A native modify keeps the SAME cloid on a NEW oid, so HL may stream a `canceled`
    /// `orderUpdates` for the OLD oid — which, sharing the cloid, would wrongly cancel the live
    /// re-placed order. The pump consults this to DROP that stale cancel. TTL-pruned so an oid whose
    /// stale cancel never arrives can't leak.
    retired: Arc<Mutex<HashMap<u64, std::time::Instant>>>,
}

/// How long a retired oid suppresses a stale `orderUpdates` cancel. A cancel-replace's stale cancel
/// arrives within a beat; 30 s is a generous ceiling that still bounds the map.
const RETIRED_TTL: std::time::Duration = std::time::Duration::from_secs(30);

impl CloidRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive `coid`'s cloid, record `cloid → coid`, and return the cloid to put on the wire.
    pub fn register(&self, coid: &str) -> String {
        let cloid = cloid_from_client_order_id(coid);
        self.inner.lock().unwrap().insert(cloid.clone(), coid.to_string());
        cloid
    }

    /// The framework coid a venue `cloid` maps to, if exec registered it this session (else `None`,
    /// so a foreign/unknown order's event keeps the venue id and the core drops it as not-ours).
    pub fn resolve(&self, cloid: &str) -> Option<String> {
        self.inner.lock().unwrap().get(cloid).cloned()
    }

    /// Mark `oid` retired by a native modify — the pump then suppresses a subsequent stale
    /// `orderUpdates` cancel of it. Opportunistically prunes expired entries.
    pub fn retire_oid(&self, oid: u64) {
        let mut m = self.retired.lock().unwrap();
        let now = std::time::Instant::now();
        m.retain(|_, t| now.duration_since(*t) < RETIRED_TTL);
        m.insert(oid, now);
    }

    /// If `oid` is a retired oid still inside the suppression window, CONSUME it (remove) and return
    /// `true` so the pump drops that one stale cancel; expired/unknown → `false`. Prunes as it goes.
    pub fn take_retired(&self, oid: u64) -> bool {
        let mut m = self.retired.lock().unwrap();
        let now = std::time::Instant::now();
        m.retain(|_, t| now.duration_since(*t) < RETIRED_TTL);
        m.remove(&oid).is_some()
    }
}

/// Per-order facts exec keeps so a later cancel/modify can be built from just the coid the core
/// passes: the order `asset` id (cancel-by-cloid needs it) and the venue `oid` once the order rests
/// (modify = cancel-replace by oid). Exec-thread-local; never shared.
#[derive(Clone, Copy)]
struct OrderMeta {
    asset: u32,
    oid: Option<u64>,
}

/// The command lane the newtype pushes onto the exec thread. Carries the batch verbs the shared
/// `ExecActor`'s `ExecCommand` lacks (HL is batch-first).
enum HlCommand {
    Submit(Box<OrderRequest>),
    SubmitBatch(Vec<OrderRequest>),
    Cancel(String),
    CancelBatch(Vec<String>),
    Modify { order: Box<OrderRequest>, new_qty: Option<f64>, new_price: Option<f64> },
    Shutdown,
}

/// Map a vike [`TimeInForce`] to HL's case-sensitive wire TIF — this venue's row of the ONE
/// cross-venue TIF authority ([`vike_bridge_core::tif::venue_tif`]), consumed. HL exposes only
/// `Gtc`/`Ioc`/`Alo` (the declared caps are `Gtc`+`Ioc`); `Alo` is post-only and is not expressible
/// via [`OrderRequest`] today, so `Fok` folds to the aggressive `Ioc` — the OPPOSITE direction of
/// polymarket's `Ioc`→`FOK` fold (same pair!); step 2 resolves that deliberately, behind demo
/// smokes — and `Gtc`/`Gtd`/`Day` fold to the resting `Gtc`.
fn hl_tif(tif: TimeInForce) -> &'static str {
    // `wire()` is Some for every hyperliquid row (Mapped/Coerced only) — the fallback is
    // unreachable, kept so the exec thread can never panic.
    vike_bridge_core::tif::venue_tif(crate::consts::VENUE, tif).wire().unwrap_or("Gtc")
}

/// Build a stop/trigger order's `(limit_px, OrderKind::Trigger)` (research §6: the wire is
/// `t: { trigger: { isMarket, triggerPx, tpsl } }`; the HL docs confirm `tpsl="sl"` = stop-loss).
///
/// - `trigger_px` = [`px::clamp_price`] of the request's `trigger_price` (the activation level).
/// - `is_market = req.price.is_none()`: a stop-MARKET fires aggressively on trip — its protective
///   `limit_px` is the trigger price shifted ±`slippage` (the SAME band the emulated market uses, so
///   a triggered stop actually fills; the current mid is far from the trigger at placement time, so
///   the trigger — not the mid — is the reference). A stop-LIMIT (`price` present) rests at
///   `clamp_price(req.price)` once triggered and never chases.
///
/// `slippage` is the mount-resolved band ([`market_slippage_for`]), passed in rather than read from
/// a global precisely because it is the number that decides how much worse than the trigger a
/// protective exit may fill.
/// - `tpsl` is `"tp"` (take-profit — fires on a FAVORABLE move) when `req.order_type ==
///   "take_profit"`, else `"sl"` (stop-loss — order_type `"stop"`, or any bare `trigger_price`).
///   The market/limit axis is orthogonal: a take-profit is stop-MARKET when `price` is absent and
///   stop-LIMIT when present, exactly like a stop-loss.
///
/// `Err` for a trigger without a `trigger_price`, or a trigger price that rounds below the grid.
fn build_trigger_kind(
    req: &OrderRequest,
    sz_decimals: u32,
    is_spot: bool,
    is_buy: bool,
    slippage: f64,
) -> Result<(String, OrderKind), String> {
    let trigger = req
        .trigger_price
        .ok_or_else(|| format!("stop order for {} without a trigger_price", req.symbol))?;
    let trigger_px = px::clamp_price(trigger, sz_decimals, is_spot);
    if trigger_px == "0" {
        return Err("trigger price rounds to zero".to_string());
    }
    let is_market = req.price.is_none();
    let limit_px = match req.price {
        // stop-LIMIT: rest at the requested limit once triggered.
        Some(price) => px::clamp_price(price, sz_decimals, is_spot),
        // stop-MARKET: no limit given — bound the market fill with an aggressive price off the
        // trigger (buy stop: trigger*(1+slip); sell stop: trigger*(1-slip)), mirroring the emulated
        // market order so the fill isn't stranded when the stop trips.
        None => {
            let slip = if is_buy { 1.0 + slippage } else { 1.0 - slippage };
            px::clamp_price(trigger * slip, sz_decimals, is_spot)
        }
    };
    // Take-profit vs stop-loss: an explicit `"take_profit"` order_type fires on a favorable move
    // (`"tp"`); everything else routed here (order_type `"stop"`, or a bare `trigger_price`) is a
    // stop-loss (`"sl"`). HL uses `tpsl` + side to decide the trigger comparison direction.
    let tpsl = if req.order_type == "take_profit" { "tp" } else { "sl" };
    let kind = OrderKind::Trigger(TriggerParams { is_market, trigger_px, tpsl: tpsl.to_string() });
    Ok((limit_px, kind))
}

/// Pull the first `statuses[i].resting.oid` out of an `/exchange` response — the NEW oid a
/// cancel-replace `modify` produces (research §6). `None` when the response has no resting oid
/// (filled/error/malformed).
fn parse_first_resting_oid(resp: &Value) -> Option<u64> {
    resp.get("response")?
        .get("data")?
        .get("statuses")?
        .as_array()?
        .first()?
        .get("resting")?
        .get("oid")?
        .as_u64()
}

/// Seam over the signed `/exchange` POST + the `allMids` read, so the command handling below is
/// unit-testable with a fake (no network, no real orders) — the twin of the Bybit/OKX `VenueRest`
/// mock seam. `place` owns nonce assignment + signing; `mid` backs the emulated-market price.
trait HlExchange {
    /// Sign + POST one `/exchange` action (assigning the next monotonic nonce). Returns the raw
    /// `{status,response}` body, or a transport error the caller classifies (`must_requery` vs dead).
    fn place(&self, action: &Action) -> Result<Value, VenueApiError>;
    /// Current mid for a venue `coin` (from `allMids`) — the reference for the emulated-market px.
    fn mid(&self, coin: &str) -> Option<f64>;
}

/// The live [`HlExchange`]: a [`HyperliquidTransport`] + the [`Signer`] + the monotonic nonce.
struct LiveExchange {
    transport: HyperliquidTransport,
    signer: Signer,
    nonce: NonceManager,
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
/// `vike_model::attribution::AttributionMechanic::SignedBuilder`, resolved once at mount from
/// `.env` — `None` when unconfigured, the default, and byte-identical to before this field existed).
/// Generic over [`HlExchange`] so the command handling is exercised with a fake in tests.
struct ExecState<X: HlExchange> {
    exchange: X,
    registry: CloidRegistry,
    events: EventSender,
    orders: HashMap<String, OrderMeta>,
    builder: Option<HlBuilderFee>,
    /// The mount-resolved emulated-market slippage band ([`market_slippage_for`]) — the ONLY two
    /// consumers are the emulated-market price in [`Self::build_order_wire`] and the protective
    /// stop-MARKET bound in [`build_trigger_kind`]. Resolved once at the mount and carried here, so
    /// no thread re-reads a config and the two sites can never use different bands.
    slippage: f64,
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
        let is_trigger = req.order_type == "stop"
            || req.order_type == "take_profit"
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
        } else if req.order_type == "market" {
            // HL has no native market order — emulate as an `Ioc` limit at an aggressive price.
            let mid = self.exchange.mid(&inst.coin).ok_or_else(|| {
                format!("no mid available to emulate a market order for {}", req.symbol)
            })?;
            let slip = if is_buy { 1.0 + self.slippage } else { 1.0 - self.slippage };
            let px = px::clamp_price(mid * slip, inst.sz_decimals, is_spot);
            (px, OrderKind::Limit(LimitParams { tif: "Ioc".to_string() }))
        } else {
            // plain limit at the requested price.
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
    fn submit(&mut self, symbology: &Symbology, reqs: &[OrderRequest]) {
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

    /// Cancel N orders as ONE native `cancelByCloid` action. Unknown coids (no local asset mapping)
    /// surface a non-terminal [`Event::OrderCancelRejected`] — never swallowed (audit A2).
    fn cancel(&mut self, coids: &[String]) {
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
    fn modify(
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

/// Spawn the funding-payment background poll thread: its OWN keyless [`HyperliquidTransport`]
/// (never shares the exec thread's signed transport — no signer is needed for `/info` reads),
/// polling [`FUNDING_POLL_INTERVAL`] until `stop` fires or the core goes away.
///
/// ⚠ The transport is its own; the per-IP REST **weight budget** is NOT, and `ip_gate` is how it
/// stops being. That budget is metered per IP rather than per client (see [`crate::ratelimit`]), so
/// the `/info` reads below and the exec thread's `/exchange` actions are spending the same minute
/// whether or not they are counting it together. The caller's handle is cloned in here; a
/// transport left on the fresh gate [`HyperliquidTransport::new`] mints would run a SECOND
/// full-budget window that neither side can see.
///
/// Mirrors the
/// bybit/okx REST-poll sourcing decision — see the module doc on why HL, despite exposing
/// `userFundings`-shaped ledger channels in principle, is polled here rather than added as a
/// third WS subscription: `crate::user_data`'s pump only carries `orderUpdates` + `userFills`
/// today, and reusing the already-ported [`fetch_funding`] keeps this change scoped to the same
/// REST-poll shape every venue in this wave uses, instead of growing the WS protocol surface.
///
/// The poller is floored at THIS spawn instant (`now_ms()`) — NOT a trailing lookback, which
/// would re-fold up to that whole lookback of already-balance-embedded payments on every restart
/// (the double-count the restart law forbids; see [`HlFundingPoller`]'s doc).
///
/// CROSS-MOUNT HAZARD (for a future multi-mount-per-venue-account world): `userFunding` is
/// ACCOUNT-WIDE (master-address-scoped, every coin — deliberate, so every mounted coin's payment
/// reaches the fold). If several HL perp mounts ever share one master account, EACH mount's
/// poller would emit EVERY coin's payments → N-fold duplication through the non-idempotent
/// `apply_funding`. Whoever adds multi-mount must dedupe this lane account-wide (one poller per
/// master address, not per mount).
fn spawn_funding_poll(
    network: Network,
    master_address: String,
    events: EventSender,
    ip_gate: RateGate,
) -> (Arc<AtomicBool>, JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let join = thread::Builder::new()
        .name("hyperliquid-funding-poll".to_string())
        .spawn(move || {
            let transport = HyperliquidTransport::new(network).with_rate_gate(ip_gate);
            // Floor = spawn instant: only payments landing WHILE mounted are folded.
            let mut poller = HlFundingPoller::new(
                |start, end| fetch_funding(&transport, &master_address, start, end),
                VENUE,
                now_ms(),
            );
            // Fetch failures retry each cadence, warned ONCE per consecutive-failure streak (a
            // 60s background thread, never the hot fold, so one warn per streak is bounded).
            let mut fail_streak = 0u32;
            while !stop_thread.load(Ordering::Relaxed) {
                match poller.poll(now_ms()) {
                    Err(e) => {
                        fail_streak += 1;
                        if fail_streak == 1 {
                            tracing::warn!(
                                target: "vike_hyperliquid",
                                error = %e,
                                "funding poll fetch failed; retrying each cadence (warns once per streak)"
                            );
                        }
                    }
                    Ok(evs) => {
                        fail_streak = 0;
                        for ev in evs {
                            if events.blocking_send(ev).is_err() {
                                return; // core gone — self-exit like every other pump thread
                            }
                        }
                    }
                }
                sleep_unless_stopped(&stop_thread, FUNDING_POLL_INTERVAL);
            }
        })
        .expect("spawn hyperliquid-funding-poll thread");
    (stop, join)
}

/// Live Hyperliquid exec client. `submit`/`cancel`/`modify` (and their batch twins) enqueue onto ONE
/// dedicated OS thread that drives the signed `/exchange` REST off the single-writer core; every venue
/// event returns through the core ingest (the `/exchange` response synchronously, fills/cancels via the
/// WS pump). Dropping it (or `detach`) sends `Shutdown` and JOINS the thread — which itself tears down
/// the owned user-data pump — for deterministic teardown. Also owns the funding-poll background
/// thread (its OWN keyless transport, joined the same way).
pub struct HyperliquidExecutionClient {
    tx: Sender<HlCommand>,
    events: EventSender,
    join: Option<JoinHandle<()>>,
    funding_stop: Arc<AtomicBool>,
    funding_join: Option<JoinHandle<()>>,
}

impl HyperliquidExecutionClient {
    /// Spawn the exec thread. `signer` is the (agent or master) L1 signer — its network selects the
    /// hosts + phantom-agent source; `master_address` is the MASTER account address the WS pump
    /// subscribes with (agent-wallet queries return empty — research §8); `instruments` is the loaded
    /// symbology + grid (shared read-only with the pump for coin⇄symbol remapping). `recon_trigger`
    /// (when a reconcile driver is mounted) is poked by the pump on every WS reconnect so a pass
    /// re-syncs any order/fill state that drifted while the socket was down; `None` ⇒ no poke. The
    /// thread spawns the private-WS pump, then drains commands until `Shutdown`. `builder` (task 7)
    /// is the configured Builder Code, resolved once by the caller (`vike-mount`'s
    /// `hyperliquid_live_client`, via `attribution_code_from(vars, "hyperliquid")` + the configured
    /// fee) — `None` (every non-configured mount) rides every submit byte-identically to before this
    /// field existed.
    ///
    /// Prices emulated market orders at [`DEFAULT_MARKET_SLIPPAGE`] — the historical literal. A
    /// caller with an operator band calls [`Self::spawn_with_market_slippage`] instead.
    ///
    /// ⚠ It also opens its OWN per-IP REST weight window ([`crate::ratelimit::ip_weight_gate`]),
    /// which is honest only for a process whose sole Hyperliquid REST caller is this client. A live
    /// mount has three (exec, funding poll, recon) and must NOT reach the venue through three
    /// budgets it publishes one of — so it resolves the gate itself and calls
    /// [`Self::spawn_with_market_slippage`] with the handle.
    pub fn spawn(
        signer: Signer,
        master_address: String,
        instruments: Arc<HyperliquidInstruments>,
        events: EventSender,
        recon_trigger: Option<std::sync::mpsc::Sender<()>>,
        builder: Option<HlBuilderFee>,
    ) -> Self {
        Self::spawn_with_market_slippage(
            signer,
            master_address,
            instruments,
            events,
            recon_trigger,
            builder,
            DEFAULT_MARKET_SLIPPAGE,
            crate::ratelimit::ip_weight_gate(),
        )
    }

    /// [`Self::spawn`] with the emulated-market slippage band supplied by the caller.
    ///
    /// `market_slippage` is the band the composition root resolved ONCE via [`market_slippage_for`]
    /// — the same "resolve at the mount, never re-read from a spawned thread" idiom the perp arms'
    /// leverage, the `{VENUE}_MAINNET` flag and the attribution codes already follow. It bounds BOTH
    /// aggressive-price sites: the emulated market order (off the mid) and the protective
    /// stop-MARKET limit (off the trigger).
    ///
    /// Pass [`DEFAULT_MARKET_SLIPPAGE`] — what [`Self::spawn`] does — for the historical behaviour.
    /// The value is used verbatim, so callers must resolve it through [`market_slippage_for`] rather
    /// than passing a raw number: that is where the bound lives.
    ///
    /// `ip_gate` is the venue's per-IP REST weight window, resolved ONCE by the composition root
    /// (`vike_mount::hyperliquid`'s `hyperliquid_live_client`) and cloned into BOTH REST paths this
    /// client owns: the exec thread's signed `/exchange` + its `allMids` reads, and the funding
    /// poller's `/info`. The root keeps a handle for the reconcile transport it builds beside them,
    /// so all three spend one budget. Hyperliquid meters that budget per IP — see
    /// [`crate::ratelimit`] — so a client that minted its own would not be conservative, it would
    /// be a second full window invisible to the first, on a row
    /// (`vike_model::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight`) that admits exactly what
    /// the venue publishes and holds nothing back to absorb it.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_market_slippage(
        signer: Signer,
        master_address: String,
        instruments: Arc<HyperliquidInstruments>,
        events: EventSender,
        recon_trigger: Option<std::sync::mpsc::Sender<()>>,
        builder: Option<HlBuilderFee>,
        market_slippage: f64,
        ip_gate: RateGate,
    ) -> Self {
        let (tx, rx) = channel();
        let run_events = events.clone();
        // Read the network BEFORE `signer` moves into the exec-thread closure below (`Network` is
        // `Copy`) — the funding poll needs it for its own keyless transport, but not the signer
        // itself (an `/info` read carries no signature).
        let network = signer.network();
        // Both REST paths below ride the caller's ONE per-IP weight window: the poller takes a
        // clone, the exec thread takes the handle itself.
        let (funding_stop, funding_join) =
            spawn_funding_poll(network, master_address.clone(), events.clone(), ip_gate.clone());
        let join = thread::Builder::new()
            .name("hyperliquid-exec".to_string())
            .spawn(move || {
                // Opt-in HFT pinning (VIKE_PIN_CORES=exec:N): keep the venue exec thread on a fixed
                // core. No-op unless the env names the `exec` role — the default desktop path.
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::Exec,
                    "hyperliquid-exec",
                );
                run(
                    signer,
                    master_address,
                    instruments,
                    run_events,
                    rx,
                    recon_trigger,
                    builder,
                    market_slippage,
                    ip_gate,
                );
            })
            .expect("spawn hyperliquid exec thread");
        Self { tx, events, join: Some(join), funding_stop, funding_join: Some(funding_join) }
    }

    /// Signal every thread this client owns to wind down, and JOIN NOTHING — the first half of
    /// [`Self::stop`], hoisted for the same reason `ExecActor::signal_stop` was
    /// (`crates/vike-bridge-core/src/exec_actor.rs`): it backs
    /// `ExecutionClient::begin_detach`, so a core holding several venues raises every flag before
    /// it joins the first. Idempotent — the send is `let _ =` and a flag store is a store.
    fn signal_stop(&mut self) {
        self.funding_stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(HlCommand::Shutdown);
    }

    fn stop(&mut self) {
        // Signal everything first, THEN join, so the command + background threads wind down
        // together (matches `ExecActor::stop`'s ordering).
        self.signal_stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if let Some(j) = self.funding_join.take() {
            let _ = j.join();
        }
    }

    /// True when the HALT sentinel file currently exists → new order placement must be refused.
    /// Consulted only at the submit/modify boundary (submits are infrequent), never on a hot path —
    /// the shared kill-switch an operator `touch`es over ssh even when the runtime is wedged.
    fn halt_engaged() -> bool {
        vike_bridge_core::halt::halt_path_from_env().exists()
    }

    /// Send a submit(-batch) command; if the exec thread is dead, synthesize the terminal
    /// [`OrderRejected`] the contract requires for every order (never let an intent vanish).
    fn submit_or_reject(&self, cmd: HlCommand, coids: &[&str], ts: i64) {
        if self.tx.send(cmd).is_err() {
            for coid in coids {
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                    client_order_id: (*coid).to_string(),
                    reason: "venue exec thread unavailable".into(),
                    ts,
                }));
            }
        }
    }

    /// Synthesize a HALT reject for every coid (the submit boundary is closed while HALT is engaged).
    fn reject_halted(&self, coids: &[&str], ts: i64) {
        for coid in coids {
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                client_order_id: (*coid).to_string(),
                reason: vike_bridge_core::halt::HALT_REJECT_REASON.into(),
                ts,
            }));
        }
    }
}

impl ExecutionClient for HyperliquidExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        // A REDUCING submit is admitted so a halt never traps the operator in a position — the rule
        // itself lives in `vike_bridge_core::halt::halt_admits_submit`, shared with the `ExecActor`
        // copy of this sentinel precisely so the two cannot disagree about what a halt lets out.
        if Self::halt_engaged() && !vike_bridge_core::halt::halt_admits_submit(request) {
            self.reject_halted(&[&request.client_order_id], request.ts);
            return;
        }
        self.submit_or_reject(
            HlCommand::Submit(Box::new(request.clone())),
            &[&request.client_order_id],
            request.ts,
        );
    }

    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        if requests.is_empty() {
            return;
        }
        let coids: Vec<&str> = requests.iter().map(|r| r.client_order_id.as_str()).collect();
        let ts = requests[0].ts;
        // ALL-OR-NOTHING, and stricter than the per-order arm above on purpose. This is ONE native
        // venue batch: it is submitted or refused as a unit, so a batch mixing reducing and opening
        // legs cannot be "partly admitted" without splitting it into a different request than the
        // caller built. Requiring EVERY leg to reduce keeps the guarantee that no opening order
        // reaches the venue under a halt, at the cost of refusing a mixed batch whole — which the
        // caller sees as a terminal rejection per coid, never as a silent partial send.
        if Self::halt_engaged() && !requests.iter().all(vike_bridge_core::halt::halt_admits_submit)
        {
            self.reject_halted(&coids, ts);
            return;
        }
        self.submit_or_reject(HlCommand::SubmitBatch(requests.to_vec()), &coids, ts);
    }

    fn cancel(&mut self, client_order_id: &str) {
        // Cancel is NEVER halt-gated — an operator must be able to reduce/exit under HALT.
        if self.tx.send(HlCommand::Cancel(client_order_id.to_string())).is_err() {
            let _ = self.events.blocking_send(cancel_event(
                client_order_id,
                CancelOutcome::Rejected("venue exec thread unavailable".to_string()),
            ));
        }
    }

    fn cancel_batch(&mut self, client_order_ids: &[String]) {
        if client_order_ids.is_empty() {
            return;
        }
        if self.tx.send(HlCommand::CancelBatch(client_order_ids.to_vec())).is_err() {
            for c in client_order_ids {
                let _ = self.events.blocking_send(cancel_event(
                    c,
                    CancelOutcome::Rejected("venue exec thread unavailable".to_string()),
                ));
            }
        }
    }

    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        // Conservative: HALT blocks modify (it can add size / chase price and reduce-only can't be
        // inferred here). Non-terminal like a dead channel — the resting order keeps its terms.
        if Self::halt_engaged() {
            return;
        }
        let _ =
            self.tx.send(HlCommand::Modify { order: Box::new(order.clone()), new_qty, new_price });
    }

    /// Phase-one teardown seam — see `crates/vike-exec/src/execution_engine/client.rs`'s
    /// `ExecutionClient::begin_detach`. This client owns its threads directly rather than through
    /// an `ExecActor`, so it wires the seam itself.
    fn begin_detach(&mut self) {
        self.signal_stop();
    }
    fn detach(&mut self) {
        self.stop();
    }
}

impl Drop for HyperliquidExecutionClient {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The exec thread body: build the live exchange + shared registry, start the private-WS pump, then
/// drain commands until `Shutdown`. ALL network I/O (signing, REST, WS) lives HERE, off the
/// single-writer core. On `Shutdown` the pump is stopped + joined before returning. `builder` (task
/// 7) is the configured Builder Code, carried into every submitted `order` action. `slippage` is the
/// mount-resolved emulated-market band ([`market_slippage_for`]), carried into [`ExecState`].
///
/// `ip_gate` is the caller's handle on the venue's ONE per-IP REST weight window, installed on this
/// thread's transport so the signed `/exchange` actions and the emulated-market `allMids` reads
/// charge the same budget as the funding poller and the mount's reconcile transport. See
/// [`HyperliquidExecutionClient::spawn_with_market_slippage`] for who resolves it.
#[allow(clippy::too_many_arguments)]
fn run(
    signer: Signer,
    master_address: String,
    instruments: Arc<HyperliquidInstruments>,
    events: EventSender,
    rx: Receiver<HlCommand>,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    builder: Option<HlBuilderFee>,
    slippage: f64,
    ip_gate: RateGate,
) {
    let network = signer.network();
    let ws_url = network.urls().2;
    let registry = CloidRegistry::new();
    // The private user-data pump: authoritative fills/cancels push straight into the core ingest,
    // remapping cloid→coid via the shared registry and coin→symbol via the shared instruments. On
    // reconnect it pokes `recon_trigger` (when reconcile is mounted) to re-sync missed state.
    let pump = spawn_hyperliquid_user_data(
        ws_url.to_string(),
        master_address,
        instruments.clone(),
        registry.clone(),
        events.clone(),
        recon_trigger,
    );
    let mut state = ExecState {
        exchange: LiveExchange {
            transport: HyperliquidTransport::new(network).with_rate_gate(ip_gate),
            signer,
            nonce: NonceManager::new(),
        },
        registry,
        events,
        orders: HashMap::new(),
        builder,
        slippage,
    };
    loop {
        match rx.recv() {
            Ok(HlCommand::Submit(req)) => {
                state.submit(instruments.symbology(), std::slice::from_ref(&*req));
            }
            Ok(HlCommand::SubmitBatch(reqs)) => state.submit(instruments.symbology(), &reqs),
            Ok(HlCommand::Cancel(coid)) => state.cancel(std::slice::from_ref(&coid)),
            Ok(HlCommand::CancelBatch(coids)) => state.cancel(&coids),
            Ok(HlCommand::Modify { order, new_qty, new_price }) => {
                state.modify(instruments.symbology(), &order, new_qty, new_price);
            }
            Ok(HlCommand::Shutdown) | Err(_) => break,
        }
    }
    let _ = pump.shutdown();
}

#[cfg(test)]
mod tests {
    //! Pure command→action→event wiring — a fake [`HlExchange`] (NO network, NO real orders) proves:
    //! `submit_batch` is ONE native `order` action of N wires; a market order is emulated as an `Ioc`
    //! at ±5% off the fake mid; `cancel`/`modify` build the native `cancelByCloid`/`modify` actions
    //! (modify keyed by the resting oid); and every failure path (build error, definite transport
    //! error) synthesizes the terminal reject while the ambiguous timeout does NOT.
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use vike_exec::event_channel;
    use vike_exec::lanes::Ingest;

    /// Pins the routed hyperliquid row of `vike_bridge_core::tif::venue_tif` byte-for-byte.
    /// NOTE the Fok→Ioc fold: the OPPOSITE direction of polymarket's Ioc→FOK (step-2 flip).
    #[test]
    fn hl_tif_row_is_pinned() {
        assert_eq!(hl_tif(TimeInForce::Gtc), "Gtc");
        assert_eq!(hl_tif(TimeInForce::Ioc), "Ioc");
        assert_eq!(hl_tif(TimeInForce::Fok), "Ioc");
        assert_eq!(hl_tif(TimeInForce::Gtd), "Gtc");
        assert_eq!(hl_tif(TimeInForce::Day), "Gtc");
    }

    fn symbology() -> Symbology {
        let meta = json!({"universe":[
            {"name":"BTC","szDecimals":5,"maxLeverage":40},
            {"name":"ETH","szDecimals":4,"maxLeverage":25}
        ]});
        let spot = json!({
            "tokens":[
                {"name":"USDC","szDecimals":8,"index":0},
                {"name":"PURR","szDecimals":0,"index":1},
                {"name":"HYPE","szDecimals":2,"index":150}
            ],
            "universe":[
                {"name":"PURR/USDC","tokens":[1,0],"index":0},
                {"name":"@107","tokens":[150,0],"index":107}
            ]
        });
        Symbology::from_meta(&meta, &spot)
    }

    #[derive(Default)]
    struct FakeExchange {
        actions: Mutex<Vec<Action>>,
        responses: Mutex<VecDeque<Result<Value, VenueApiError>>>,
        mid: Option<f64>,
    }
    impl HlExchange for FakeExchange {
        fn place(&self, action: &Action) -> Result<Value, VenueApiError> {
            self.actions.lock().unwrap().push(action.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(json!({"status":"ok","response":{"data":{"statuses":[]}}})))
        }
        fn mid(&self, _coin: &str) -> Option<f64> {
            self.mid
        }
    }

    /// An `ExecState` at the venue's historical band — so every pre-existing test below keeps
    /// asserting the exact prices it always did.
    fn state(exchange: FakeExchange, events: EventSender) -> ExecState<FakeExchange> {
        state_with_slippage(exchange, events, DEFAULT_MARKET_SLIPPAGE)
    }

    fn state_with_slippage(
        exchange: FakeExchange,
        events: EventSender,
        slippage: f64,
    ) -> ExecState<FakeExchange> {
        ExecState {
            exchange,
            registry: CloidRegistry::new(),
            events,
            orders: HashMap::new(),
            builder: None,
            slippage,
        }
    }

    fn limit_req(coid: &str, side: i32, price: f64, qty: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: VENUE.into(),
            symbol: "BTC".into(),
            side,
            qty,
            order_type: "limit".into(),
            price: Some(price),
            ..Default::default()
        }
    }

    /// A "stop" request: `trigger` is the activation price; `limit = None` ⇒ stop-MARKET, `Some(px)`
    /// ⇒ stop-LIMIT.
    fn stop_req(coid: &str, side: i32, trigger: f64, limit: Option<f64>, qty: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: VENUE.into(),
            symbol: "BTC".into(),
            side,
            qty,
            order_type: "stop".into(),
            price: limit,
            trigger_price: Some(trigger),
            ..Default::default()
        }
    }

    fn kind(e: &Event) -> &'static str {
        match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::OrderRejected(_) => "Rejected",
            Event::OrderCanceled(_) => "Canceled",
            Event::OrderCancelRejected(_) => "CancelRejected",
            Event::OrderModified(_) => "Modified",
            Event::OrderModifyRejected(_) => "ModifyRejected",
            Event::OrderFilled(_) => "Filled",
            Event::OrderPartiallyFilled(_) => "PartiallyFilled",
            Event::Fill(_) => "Fill",
            _ => "other",
        }
    }

    macro_rules! drain {
        ($rx:expr) => {{
            let mut out: Vec<Event> = Vec::new();
            while let Ok(ing) = $rx.try_recv() {
                if let Ingest::Event(e) = ing {
                    out.push(e);
                }
            }
            out
        }};
    }

    #[test]
    fn submit_batch_is_one_native_order_action() {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([Ok(json!({"status":"ok","response":{"data":{
                "statuses":[{"resting":{"oid":11}},{"resting":{"oid":12}}]
            }}}))])),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        let reqs = vec![limit_req("c1", 1, 50000.0, 0.01), limit_req("c2", -1, 51000.0, 0.02)];
        st.submit(&symbology(), &reqs);

        let actions = st.exchange.actions.lock().unwrap();
        assert_eq!(actions.len(), 1, "native batch: ONE action for N orders");
        match &actions[0] {
            Action::Order(a) => {
                assert_eq!(a.orders.len(), 2);
                assert_eq!(a.grouping, "na");
                assert_eq!(a.orders[0].asset, 0); // BTC = universe index 0
                assert!(a.orders[0].is_buy);
                assert!(!a.orders[1].is_buy);
                assert_eq!(a.orders[0].limit_px, "50000");
                assert!(matches!(&a.orders[0].order_type, OrderKind::Limit(l) if l.tif == "Gtc"));
                assert!(a.orders[0].cloid.is_some(), "cloid derived + set on the wire");
            }
            other => panic!("expected an Order action, got {other:?}"),
        }
        drop(actions);

        let evs = drain!(rx);
        assert_eq!(
            evs.iter().map(kind).collect::<Vec<_>>(),
            vec!["Submitted", "Submitted", "Accepted", "Accepted"]
        );
        assert_eq!(st.orders.get("c1").unwrap().oid, Some(11), "resting oid captured for modify");
    }

    /// The exec-level wiring twin of `signing::action::tests::
    /// order_wire_carries_builder_when_configured_and_omits_when_not` — proves `ExecState::submit`
    /// actually carries `self.builder` onto the sent `Action`, and that the default `None` (no
    /// `.env` config) omits it entirely, exactly like before this field existed.
    #[test]
    fn submit_carries_the_configured_builder_and_omits_it_when_unset() {
        let (tx, _rx) = event_channel(64);
        let mut st = state(FakeExchange::default(), tx);
        st.builder = Some(HlBuilderFee { address: "0x0c8d".to_string(), fee_tenths_bp: 0 });
        st.submit(&symbology(), &[limit_req("c1", 1, 50000.0, 0.01)]);
        let actions = st.exchange.actions.lock().unwrap();
        match &actions[0] {
            Action::Order(a) => assert_eq!(
                a.builder,
                Some(HlBuilderFee { address: "0x0c8d".to_string(), fee_tenths_bp: 0 })
            ),
            other => panic!("expected an Order action, got {other:?}"),
        }
        drop(actions);

        let (tx2, _rx2) = event_channel(64);
        let mut st2 = state(FakeExchange::default(), tx2); // builder: None (the default state)
        st2.submit(&symbology(), &[limit_req("c2", 1, 50000.0, 0.01)]);
        let actions2 = st2.exchange.actions.lock().unwrap();
        match &actions2[0] {
            Action::Order(a) => {
                assert!(a.builder.is_none(), "unconfigured ⇒ no builder on the wire")
            }
            other => panic!("expected an Order action, got {other:?}"),
        }
    }

    #[test]
    fn market_order_is_emulated_ioc_at_slippage() {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            mid: Some(50000.0),
            responses: Mutex::new(VecDeque::from([Ok(json!({"status":"ok","response":{"data":{
                "statuses":[{"filled":{"totalSz":"0.01","avgPx":"52000","oid":9}}]
            }}}))])),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        let req = OrderRequest {
            client_order_id: "m1".into(),
            venue: VENUE.into(),
            symbol: "BTC".into(),
            side: 1,
            qty: 0.01,
            order_type: "market".into(),
            ..Default::default()
        };
        st.submit(&symbology(), std::slice::from_ref(&req));

        let actions = st.exchange.actions.lock().unwrap();
        match &actions[0] {
            Action::Order(a) => {
                assert!(matches!(&a.orders[0].order_type, OrderKind::Limit(l) if l.tif == "Ioc"));
                // buy mid*1.05 = 52500, clamped to the BTC grid.
                assert_eq!(a.orders[0].limit_px, "52500");
            }
            other => panic!("{other:?}"),
        }
        drop(actions);
        assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Filled"]);
    }

    /// THE property that makes this safe to ship onto a live account: an operator who configures
    /// nothing gets the exact band this adapter has always priced with. NO network — `market_slippage_for`
    /// is the pure decision the composition root calls once, before anything is spawned.
    #[test]
    fn an_unset_band_is_the_historical_five_percent() {
        assert_eq!(market_slippage_for(None), DEFAULT_MARKET_SLIPPAGE);
        assert_eq!(market_slippage_for(None), 0.05, "the historical hardcoded MARKET_SLIPPAGE");
    }

    /// Drift alarm: the venue's own literal must itself be a legal band. If a future edit moves
    /// `DEFAULT_MARKET_SLIPPAGE` outside the workspace bounds, the unset path would be pricing at a
    /// value the config edge would refuse — the two must not be able to disagree.
    #[test]
    fn the_venue_default_is_within_the_workspace_bounds() {
        assert!(vike_model::market_slippage::is_usable_market_slippage(DEFAULT_MARKET_SLIPPAGE));
        // The ceiling IS this literal, which is what makes the knob a one-way ratchet.
        assert_eq!(DEFAULT_MARKET_SLIPPAGE, vike_model::market_slippage::MAX_MARKET_SLIPPAGE);
    }

    /// A configured band can only tighten: no value reaches the wire wider than the historical one.
    #[test]
    fn no_configured_band_can_price_more_aggressively_than_the_default() {
        for absurd in [0.5, 50.0, f64::INFINITY, f64::NAN, -1.0] {
            assert!(
                market_slippage_for(Some(absurd)) <= DEFAULT_MARKET_SLIPPAGE,
                "{absurd} widened the band"
            );
        }
        assert_eq!(market_slippage_for(Some(0.002)), 0.002, "a usable band rides through");
    }

    /// The tightened band on the real build path, BOTH sides — a buy prices UP, a sell prices DOWN.
    /// Mid 50000 at 0.2% ⇒ buy 50100 / sell 49900 (vs 52500 / 47500 at the historical 5%).
    #[test]
    fn a_tightened_band_prices_both_sides_off_the_mid() {
        for (side, expected) in [(1i32, "50100"), (-1, "49900")] {
            let (tx, _rx) = event_channel(64);
            let fake = FakeExchange { mid: Some(50000.0), ..Default::default() };
            let mut st = state_with_slippage(fake, tx, 0.002);
            let req = OrderRequest {
                client_order_id: "tight".into(),
                venue: VENUE.into(),
                symbol: "BTC".into(),
                side,
                qty: 0.01,
                order_type: "market".into(),
                ..Default::default()
            };
            st.submit(&symbology(), std::slice::from_ref(&req));
            let actions = st.exchange.actions.lock().unwrap();
            match &actions[0] {
                Action::Order(a) => {
                    assert_eq!(a.orders[0].is_buy, side > 0);
                    // Still an Ioc — only the band moved, never the order construction.
                    assert!(
                        matches!(&a.orders[0].order_type, OrderKind::Limit(l) if l.tif == "Ioc")
                    );
                    assert_eq!(a.orders[0].limit_px, expected, "side {side}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// The band bounds the PROTECTIVE stop too — the second, easily-forgotten site. A sell stop at
    /// trigger 48000 with a 0.2% band prices at 47904, not the historical 45600.
    #[test]
    fn a_tightened_band_also_bounds_the_protective_stop_market() {
        let (tx, _rx) = event_channel(64);
        let mut st = state_with_slippage(FakeExchange::default(), tx, 0.002);
        let req = stop_req("s-tight", -1, 48000.0, None, 0.01);
        st.submit(&symbology(), std::slice::from_ref(&req));
        let actions = st.exchange.actions.lock().unwrap();
        match &actions[0] {
            Action::Order(a) => {
                match &a.orders[0].order_type {
                    // The trigger LEVEL is untouched — only the protective bound moved.
                    OrderKind::Trigger(t) => {
                        assert!(t.is_market);
                        assert_eq!(t.trigger_px, "48000");
                        assert_eq!(t.tpsl, "sl");
                    }
                    other => panic!("expected a Trigger, got {other:?}"),
                }
                assert_eq!(a.orders[0].limit_px, "47904");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn market_order_without_a_mid_is_rejected_locally() {
        let (tx, mut rx) = event_channel(64);
        let mut st = state(FakeExchange { mid: None, ..Default::default() }, tx);
        let req = OrderRequest {
            client_order_id: "m2".into(),
            venue: VENUE.into(),
            symbol: "BTC".into(),
            side: 1,
            qty: 0.01,
            order_type: "market".into(),
            ..Default::default()
        };
        st.submit(&symbology(), std::slice::from_ref(&req));
        assert!(
            st.exchange.actions.lock().unwrap().is_empty(),
            "no action placed when the build fails"
        );
        assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Rejected"]);
    }

    #[test]
    fn stop_market_order_builds_trigger_kind_at_slippage() {
        let (tx, mut rx) = event_channel(64);
        // The venue parks a stop until it trips → "waitingForTrigger" (mapped to OrderAccepted).
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([Ok(json!({"status":"ok","response":{"data":{
                "statuses":["waitingForTrigger"]
            }}}))])),
            ..Default::default() // mid: None — a stop needs NO mid (unlike the emulated market)
        };
        let mut st = state(fake, tx);
        // A protective SELL stop for a long: trigger 48000, no limit ⇒ stop-MARKET.
        let req = stop_req("s1", -1, 48000.0, None, 0.01);
        st.submit(&symbology(), std::slice::from_ref(&req));

        let actions = st.exchange.actions.lock().unwrap();
        assert_eq!(actions.len(), 1, "one native order action, no mid fetch needed");
        match &actions[0] {
            Action::Order(a) => {
                assert_eq!(a.orders.len(), 1);
                assert!(!a.orders[0].is_buy);
                match &a.orders[0].order_type {
                    OrderKind::Trigger(t) => {
                        assert!(t.is_market, "no limit price ⇒ stop-MARKET (is_market=true)");
                        assert_eq!(t.trigger_px, "48000");
                        assert_eq!(t.tpsl, "sl", "a plain stop is a stop-loss");
                    }
                    other => panic!("expected an OrderKind::Trigger, got {other:?}"),
                }
                // stop-MARKET limit_px = trigger*(1-5% slippage) for a sell = 45600 (aggressive bound).
                assert_eq!(a.orders[0].limit_px, "45600");
            }
            other => panic!("expected an Order action, got {other:?}"),
        }
        drop(actions);
        assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Accepted"]);
    }

    #[test]
    fn stop_limit_order_builds_trigger_kind_with_resting_limit() {
        let (tx, _rx) = event_channel(64);
        let mut st = state(FakeExchange::default(), tx);
        // A SELL stop-LIMIT: trigger 48000, rest at limit 47900 once tripped.
        let req = stop_req("s2", -1, 48000.0, Some(47900.0), 0.01);
        st.submit(&symbology(), std::slice::from_ref(&req));

        let actions = st.exchange.actions.lock().unwrap();
        match &actions[0] {
            Action::Order(a) => match &a.orders[0].order_type {
                OrderKind::Trigger(t) => {
                    assert!(!t.is_market, "a limit price present ⇒ stop-LIMIT (is_market=false)");
                    assert_eq!(t.trigger_px, "48000");
                    assert_eq!(t.tpsl, "sl");
                    // rests at the requested limit, NOT a slipped bound.
                    assert_eq!(a.orders[0].limit_px, "47900");
                }
                other => panic!("expected an OrderKind::Trigger, got {other:?}"),
            },
            other => panic!("expected an Order action, got {other:?}"),
        }
    }

    /// **A REAL tie between the adapter and its `VenueCaps` row** — the deribit
    /// `modify_is_default_noop_for_deribit` pattern: drive the actual code path, then assert the
    /// declaration, in ONE test. (The crate's `caps_test` module cannot do this: every assertion
    /// there compares fields of the SAME constant the row defines, so it is a restatement.)
    ///
    /// The bug this pins: `HYPERLIQUID.supported_order_kinds` omitted `"stop_limit"`, so
    /// `vike_model::preflight_order` returned `TRIGGER_UNSUPPORTED` and `vike-core` terminally
    /// REJECTED the order — while `build_order_wire` builds it perfectly. The sibling test above
    /// proves the wire SHAPE using `order_type: "stop"`; this one uses the literal `"stop_limit"`
    /// spelling, because that string is what the preflight classifies. The routing keys off
    /// `trigger_price.is_some()`, not the spelling, which is exactly why the two never disagreed
    /// on the wire and the gap could hide in the declaration alone.
    #[test]
    fn stop_limit_kind_is_declared_and_built() {
        // 1. The DECLARATION: the row must list the kind, or the core edge refuses the order.
        let caps = vike_model::caps_for("hyperliquid");
        assert!(
            caps.supported_order_kinds.contains(&"stop_limit"),
            "the row must declare stop_limit — preflight_order refuses undeclared trigger kinds"
        );
        // ...and the core-edge preflight must therefore ADMIT it.
        let mut pf = stop_req("pf1", -1, 48_000.0, Some(47_900.0), 0.01);
        pf.order_type = "stop_limit".into();
        assert_eq!(vike_model::preflight_order(&pf), Ok(()), "core edge must admit stop_limit");

        // 2. The REALITY: the same request through the real `build_order_wire` path rests as a
        //    stop-LIMIT at the requested price (is_market=false), not a slipped market bound.
        let (tx, _rx) = event_channel(64);
        let mut st = state(FakeExchange::default(), tx);
        st.submit(&symbology(), std::slice::from_ref(&pf));
        let actions = st.exchange.actions.lock().unwrap();
        match &actions[0] {
            Action::Order(a) => match &a.orders[0].order_type {
                OrderKind::Trigger(t) => {
                    assert!(!t.is_market, "price present ⇒ resting stop-LIMIT");
                    assert_eq!(t.trigger_px, "48000");
                    assert_eq!(t.tpsl, "sl", "stop_limit is a stop-LOSS, not a take-profit");
                    assert_eq!(a.orders[0].limit_px, "47900", "rests at the requested limit");
                }
                other => panic!("expected an OrderKind::Trigger, got {other:?}"),
            },
            other => panic!("expected an Order action, got {other:?}"),
        }
    }

    #[test]
    fn take_profit_order_builds_trigger_kind_with_tp_tpsl() {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([Ok(json!({"status":"ok","response":{"data":{
                "statuses":["waitingForTrigger"]
            }}}))])),
            ..Default::default() // a take-profit needs NO mid either — it references its own trigger
        };
        let mut st = state(fake, tx);
        // A take-profit SELL (lock a long's gain): order_type "take_profit", trigger 70000, no limit
        // ⇒ tp-MARKET. Same trigger machinery as a stop, only `tpsl` flips to "tp".
        let mut req = stop_req("tp1", -1, 70000.0, None, 0.01);
        req.order_type = "take_profit".into();
        st.submit(&symbology(), std::slice::from_ref(&req));

        let actions = st.exchange.actions.lock().unwrap();
        assert_eq!(actions.len(), 1, "one native order action, no mid fetch needed");
        match &actions[0] {
            Action::Order(a) => match &a.orders[0].order_type {
                OrderKind::Trigger(t) => {
                    assert!(t.is_market, "no limit price ⇒ tp-MARKET (is_market=true)");
                    assert_eq!(t.trigger_px, "70000");
                    assert_eq!(t.tpsl, "tp", "order_type take_profit ⇒ tpsl tp");
                    // tp-MARKET sell limit_px = trigger*(1-5%) = 66500 (same aggressive bound).
                    assert_eq!(a.orders[0].limit_px, "66500");
                }
                other => panic!("expected an OrderKind::Trigger, got {other:?}"),
            },
            other => panic!("expected an Order action, got {other:?}"),
        }
        drop(actions);
        assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Accepted"]);
    }

    /// Trigger-source law (`vike_bridge_core::trigger`, hyperliquid row): HL has NO trigger-by
    /// field and evaluates triggers against MARK by venue law — a requested Last/Index is a LOUD
    /// local reject (the wire is never touched), a requested Mark proceeds identically to `None`,
    /// and `None` stays byte-identical as ever.
    #[test]
    fn trigger_by_mark_matches_venue_law_and_last_index_are_denied() {
        // Mark: accepted — same action as an unrequested stop.
        let (tx, mut rx) = event_channel(64);
        let mut st = state(FakeExchange::default(), tx);
        let mut req = stop_req("s-mark", -1, 48000.0, None, 0.01);
        req.trigger_by = Some(vike_model::TriggerBy::Mark);
        st.submit(&symbology(), std::slice::from_ref(&req));
        assert_eq!(st.exchange.actions.lock().unwrap().len(), 1, "Mark IS the venue law");
        assert!(!drain!(rx).iter().map(kind).any(|k| k == "Rejected"));

        // Last / Index: denied locally, terminal, no action placed — on stops AND take-profits.
        for (coid, tb) in
            [("s-last", vike_model::TriggerBy::Last), ("s-index", vike_model::TriggerBy::Index)]
        {
            let (tx, mut rx) = event_channel(64);
            let mut st = state(FakeExchange::default(), tx);
            let mut req = stop_req(coid, -1, 48000.0, None, 0.01);
            req.trigger_by = Some(tb);
            st.submit(&symbology(), std::slice::from_ref(&req));
            assert!(st.exchange.actions.lock().unwrap().is_empty(), "{tb:?}: wire never touched");
            let evs = drain!(rx);
            assert_eq!(evs.iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Rejected"]);
            match &evs[1] {
                Event::OrderRejected(r) => {
                    assert_eq!(r.client_order_id, coid);
                    assert!(r.reason.contains("MARK by venue law"), "{}", r.reason);
                }
                other => panic!("expected OrderRejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn cancel_uses_native_cancel_by_cloid() {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([
                Ok(
                    json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}}),
                ),
                Ok(json!({"status":"ok","response":{"data":{"statuses":["success"]}}})),
            ])),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        st.submit(&symbology(), std::slice::from_ref(&limit_req("c1", 1, 50000.0, 0.01)));
        st.cancel(std::slice::from_ref(&"c1".to_string()));

        let actions = st.exchange.actions.lock().unwrap();
        assert_eq!(actions.len(), 2);
        match &actions[1] {
            Action::CancelByCloid(a) => {
                assert_eq!(a.cancels.len(), 1);
                assert_eq!(a.cancels[0].asset, 0);
            }
            other => panic!("expected a CancelByCloid action, got {other:?}"),
        }
        drop(actions);
        let evs = drain!(rx);
        assert!(
            evs.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c1"))
        );
    }

    #[test]
    fn cancel_of_an_unknown_order_is_rejected_not_swallowed() {
        let (tx, mut rx) = event_channel(64);
        let mut st = state(FakeExchange::default(), tx);
        st.cancel(std::slice::from_ref(&"ghost".to_string()));
        assert!(
            st.exchange.actions.lock().unwrap().is_empty(),
            "nothing to place for an unknown coid"
        );
        match drain!(rx).as_slice() {
            [Event::OrderCancelRejected(e)] => assert_eq!(e.client_order_id, "ghost"),
            other => panic!("expected one OrderCancelRejected, got {other:?}"),
        }
    }

    #[test]
    fn modify_is_native_cancel_replace_by_oid() {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([
                Ok(
                    json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}}),
                ),
                Ok(
                    json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":22}}]}}}),
                ),
            ])),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        let req = limit_req("c1", 1, 50000.0, 0.01);
        st.submit(&symbology(), std::slice::from_ref(&req));
        st.modify(&symbology(), &req, None, Some(50500.0));

        let actions = st.exchange.actions.lock().unwrap();
        match &actions[1] {
            Action::Modify(m) => {
                assert_eq!(m.oid, 11, "cancel-replace keyed by the captured resting oid");
                assert_eq!(m.order.limit_px, "50500");
            }
            other => panic!("expected a Modify action, got {other:?}"),
        }
        drop(actions);
        let evs = drain!(rx);
        assert!(evs.iter().any(|e| matches!(
            e,
            Event::OrderModified(m) if m.client_order_id == "c1" && m.venue_order_id.as_deref() == Some("22")
        )));
        assert_eq!(st.orders.get("c1").unwrap().oid, Some(22), "meta oid rolled to the new one");
    }

    #[test]
    fn modify_without_a_resting_oid_is_rejected() {
        let (tx, mut rx) = event_channel(64);
        let mut st = state(FakeExchange::default(), tx);
        let req = limit_req("c1", 1, 50000.0, 0.01);
        st.modify(&symbology(), &req, None, Some(51000.0)); // never submitted → no oid
        assert!(st.exchange.actions.lock().unwrap().is_empty());
        match drain!(rx).as_slice() {
            [Event::OrderModifyRejected(e)] => assert_eq!(e.client_order_id, "c1"),
            other => panic!("expected one OrderModifyRejected, got {other:?}"),
        }
    }

    #[test]
    fn definite_transport_error_rejects_every_order() {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([Err(VenueApiError {
                code: 500,
                msg: "boom".into(),
            })])),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        st.submit(
            &symbology(),
            &[limit_req("c1", 1, 50000.0, 0.01), limit_req("c2", -1, 51000.0, 0.02)],
        );
        assert_eq!(
            drain!(rx).iter().map(kind).collect::<Vec<_>>(),
            vec!["Submitted", "Submitted", "Rejected", "Rejected"],
            "a definite failure rejects every order (none vanishes)"
        );
    }

    #[test]
    fn ambiguous_timeout_never_rejects() {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([Err(VenueApiError {
                code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
                msg: "timed out".into(),
            })])),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        st.submit(&symbology(), std::slice::from_ref(&limit_req("c1", 1, 50000.0, 0.01)));
        assert_eq!(
            drain!(rx).iter().map(kind).collect::<Vec<_>>(),
            vec!["Submitted"],
            "the ambiguous timeout must NOT synthesize a reject (would strand a phantom position)"
        );
    }
}
