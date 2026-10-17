//! BinanceSpotRest — signed REST submit/cancel/connect. Exact port of
//! `exec/binance/client.py` + the shared `exec/crypto_client.py` flow.
//!
//! The WS executionReport is the SOLE source of fills; the REST POST/DELETE response is
//! ACK-ONLY. `submit_order` returns [OrderSubmitted, OrderAccepted|OrderRejected] for the
//! session to pump into the core ingest (Python published the same pair to its bus).
//! qty/price are formatted to the symbol's step/tick as decimal strings to dodge
//! -1111 BAD_PRECISION.

use crate::family::confirm::ConfirmByCoid;
use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_exec::ReconcileSnapshot;
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderModified, OrderModifyRejected, OrderRejected,
    OrderSubmitted,
};
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, OrderType, SymbolProperties};

use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// The pure, byte-identical-between-binance/aster mappers live once in `crate::family::order_map`
// (mappers rung): `json_id` (used internally below), and `parse_symbol_properties` re-exported so
// `vike_binance::spot::parse_symbol_properties` stays the same public path.
use crate::family::order_map::REPLACE_CANCEL_ID_PREFIX;
use crate::family::order_map::json_id;
pub use crate::family::order_map::parse_symbol_properties;

// Homed at the crate root since the exec/feeds seam (this module is `exec`-gated, the venue id is
// not: `crate::data`'s keyless kline REST names it too); imported here like any other consumer —
// no `crate::spot::VENUE` re-export, per the no-pub-use-shims-on-a-move convention (root
// CLAUDE.md). The value is unchanged, so every signed order this module builds is byte-identical.
use crate::VENUE;
pub const PATH_ORDER: &str = "/api/v3/order";
/// Binance spot's atomic cancel-then-place endpoint, the only price-modify spot has (see
/// [`BinanceSpotRest::modify_order`]).
pub const PATH_CANCEL_REPLACE: &str = "/api/v3/order/cancelReplace";
/// `-2021` ORDER_CANCEL_REPLACE_PARTIALLY_FAILED: under `STOP_ON_FAILURE` the cancel LANDED and the
/// replacement was refused (the venue evaluates filters after the cancel).
const CANCEL_REPLACE_PARTIAL: i64 = -2021;
/// `-2022` ORDER_CANCEL_REPLACE_FAILED: the cancel failed, so nothing was placed.
const CANCEL_REPLACE_FAILED: i64 = -2022;
pub const PATH_OPEN_ORDERS: &str = "/api/v3/openOrders";
pub const PATH_ACCOUNT: &str = "/api/v3/account";
pub const PATH_TICKER: &str = "/api/v3/ticker/price";
pub const PATH_TIME: &str = "/api/v3/time";
pub const PATH_EXCHANGE_INFO: &str = "/api/v3/exchangeInfo";

pub const DEMO_REST: &str = "https://demo-api.binance.com";
pub const MAINNET_REST: &str = "https://api.binance.com";

/// The account's live spot maker/taker commission rates, as **fractions** (e.g. `0.001` = 10 bps).
/// Binance returns these under `commissionRates{maker,taker}` in the SAME `/api/v3/account` body
/// [`BinanceSpotRest::connect`] already fetches — they were previously dropped (only `balances`
/// was read). The fee model (steal-list #5) surfaces them so a live start can prefer the account's
/// actual rates over the static [`vike_venues::fee_schedule_for`] default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BinanceCommissionRates {
    pub maker: f64,
    pub taker: f64,
}

/// Parse `commissionRates{maker,taker}` (fractions) out of a `/api/v3/account` response body.
/// `None` when the object is absent (some account/permission shapes omit it) or a rate is
/// unparseable — the caller then keeps the static default (fail-soft). Pure + fixture-tested.
pub fn parse_commission_rates(account: &serde_json::Value) -> Option<BinanceCommissionRates> {
    let cr = account.get("commissionRates")?;
    Some(BinanceCommissionRates {
        maker: cr.get("maker").and_then(json_num)?,
        taker: cr.get("taker").and_then(json_num)?,
    })
}

/// **WHICH ACCOUNT this key belongs to** — `uid` out of a `/api/v3/account` response body.
///
/// ⚠ **MEASURED, not read off the venue's documentation.** On 2026-09-20 a read-only probe
/// (`crates/bridges/binance/tests/binance_identity_probe.rs`) asked the live demo endpoint and the
/// sanitizer's audit trail came back naming `/uid`. That matters because this tree has an incident
/// on file from writing a venue parser out of public docs — `vike_secrets::key_permissions`'
/// module doc carries it as *the `canWithdraw` trap* — and `vike_tradehub_core::book_identity`'s binance row
/// had said only that *an authenticated call* could answer, naming no field, because nothing here
/// had ever looked.
///
/// # Why this costs NOTHING to call
///
/// It parses a body the live path already holds. `vike_tradehub_core::arming`'s `resolve_fee_schedule` makes
/// one blocking `/api/v3/account` read at every live start (through `fetch_fee_rates`, whose spot
/// arm is parameterless), and `vike_tradehub_core::startup`'s `authed_read_probes` reaches the same endpoint
/// at every start through `fetch_balance` — which keeps one USDT figure and then discards even that
/// (`.map(|_| ())`). So recording the account this key trades needs no extra request, no extra
/// weight and no new permission: it needs the body to stop being thrown away.
///
/// # What it answers with
///
/// The uid as a STRING, whatever JSON type the venue used. It is an identifier to compare and to
/// store, never a number to do arithmetic on, and `vike_secrets::Account::venue_account_id` is a
/// `TEXT` column — so parsing it into an integer here would be a conversion with a lossy edge and no
/// consumer.
///
/// `None` when the field is absent or is neither a string nor a number. Fail-soft on purpose: a
/// missing uid must degrade to *not yet known* — the state every migrated row already carries —
/// rather than fail a start over a bookkeeping field.
#[must_use]
pub fn parse_account_identity(account: &serde_json::Value) -> Option<String> {
    match account.get("uid")? {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The REST half of the live spot client (ACK-only); fills come from the user-data WS
/// (slice 2). Signer/transport are seams: offline tests stub the transport with canned
/// JSON; the demo smoke uses `UreqTransport`.
pub struct BinanceSpotRest<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub base_asset: String,
    /// Binance Broker/Link attribution id (unified cross-venue attribution, task 6), resolved ONCE
    /// at start from `BINANCE_BROKER_CODE`/`BINANCE_BUILDER_CODE` via `attribution_code_from` (see
    /// `vike_secrets::credentials`). Applied at the venue edge to every client-order-id string
    /// this client sends Binance (`newClientOrderId` on submit, `origClientOrderId` on cancel/query)
    /// via [`crate::family::order_map::binance_broker_coid`] — `None` (unset, or a history/resync-
    /// only client that never identifies an order by coid) reproduces the pre-task-6 wire body
    /// byte-for-byte.
    pub link_id: Option<String>,
}

impl<S: Signer, T: RestTransport> BinanceSpotRest<S, T> {
    /// `build_order_params` hook — pure, golden-gated. Delegates to the shared, byte-identical
    /// [`crate::family::order_map::build_spot_order_params`] (mappers rung), passing this client's
    /// `symbol`/`properties`; `newOrderRespType=ACK` and the response's fills[] is deliberately
    /// ignored (WS is the sole fill source).
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        crate::family::order_map::build_spot_order_params(
            request,
            &self.symbol,
            &self.properties,
            VENUE,
            self.link_id.as_deref(),
        )
    }

    /// Shared submit flow (crypto_client.py): Submitted → REST → Accepted|Rejected.
    /// The returned events go into the core ingest — never applied locally.
    pub fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // tif step-2 gate: a TIF binance cannot express (GTD/Day) is a LOUD terminal reject —
        // the wire is never touched, never a silent GTC (pinned in `tif_gate_tests` below).
        // Same discipline for a trigger source spot cannot express (Mark/Index — spot's only
        // price series is last trades): `deny_unsupported_trigger_by`.
        if let Some(reject) = vike_venues::venue_tif::deny_unsupported_tif(VENUE, request)
            .or_else(|| vike_bridge_core::trigger::deny_unsupported_trigger_by(VENUE, request))
        {
            events.push(reject);
            return events;
        }
        let params = self.build_order_params(request);
        match self.transport.signed(&self.base_url, PATH_ORDER, "POST", &params, &self.signer) {
            Ok(resp) => events.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: Some(resp.get("orderId").map(json_id).unwrap_or_default().into()),
                ts: request.ts,
            })),
            // audit T1: an ambiguous timeout may have been accepted at the venue — re-query instead
            // of emitting a false terminal reject (which would leave a phantom position).
            Err(exc) if crate::error_codes::submit_outcome_unknown(exc.code) => {
                events.extend(self.resolve_ambiguous_submit(request));
            }
            Err(exc) => {
                tracing::warn!(target: "vike_binance::spot", code = exc.code, msg = %exc.msg, "submit rejected");
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: exc.msg.into(),
                    ts: request.ts,
                }))
            }
        }
        events
    }

    /// Fetch the account's live maker/taker commission rates from `/api/v3/account` (the SAME
    /// signed call [`Self::connect`] makes; the `commissionRates` fields were previously dropped).
    /// `Ok(None)` = the object is absent/unparseable (fail-soft → static default).
    pub fn fetch_commission_rates(&self) -> Result<Option<BinanceCommissionRates>, VenueApiError> {
        let account =
            self.transport.signed(&self.base_url, PATH_ACCOUNT, "GET", &[], &self.signer)?;
        Ok(parse_commission_rates(&account))
    }

    /// Audit A3 resync: recent order states (`GET /api/v3/allOrders`) for the post-reconnect
    /// history replay. On the short-timeout requery transport so the resync can't stall. The body is
    /// [`crate::family::rest_reads::get_all_orders`], shared with aster.
    pub fn get_all_orders(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        crate::family::rest_reads::get_all_orders(
            &self.transport,
            &self.signer,
            &self.base_url,
            "/api/v3/allOrders",
            &self.symbol,
            limit,
        )
    }

    /// Audit A3 resync: recent fills (`GET /api/v3/myTrades`) for the post-reconnect history replay.
    pub fn get_my_trades(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            "/api/v3/myTrades",
            "GET",
            &params,
            &self.signer,
        )
    }

    /// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1): live → managed
    /// OrderAccepted; venue-confirmed-absent → true terminal OrderRejected; query inconclusive → NO event (never a
    /// false terminal, nor a phantom accept; the watchdog confirm decides). The re-query is
    /// [`crate::family::rest_reads::query_order_orderid`], shared with aster; `origClientOrderId`
    /// must match the venue's OWN stored `clientOrderId` exactly, so the bare local id is
    /// re-prefixed here the same way submit stamped it (task 6) — never sent bare when a link id is
    /// configured, or the venue would report the order unknown.
    fn resolve_ambiguous_submit(&self, request: &OrderRequest) -> Vec<Event> {
        crate::family::confirm::resolve_submit(
            &request.client_order_id,
            request.ts,
            self.lookup_by_coid(&request.client_order_id),
        )
    }

    /// The by-coid status query behind BOTH the ambiguous-submit arm and the active `confirm_order`
    /// (the `ExecActor::with_confirm` closure in `crate::exec`): ONE body, so they cannot disagree.
    fn lookup_by_coid(
        &self,
        coid: &str,
    ) -> Result<Option<crate::family::rest_reads::FoundOrder>, VenueApiError> {
        let orig = crate::family::order_map::binance_broker_coid(self.link_id.as_deref(), coid);
        crate::family::rest_reads::query_order_orderid(
            &self.transport,
            &self.signer,
            &self.base_url,
            PATH_ORDER,
            &self.symbol,
            &orig,
        )
    }

    /// Cancel by client-order-id. "Unknown order" (-2011) is swallowed (already gone ≠
    /// failure — the R6 gate's "unknown ≠ rejection" rule); other codes propagate.
    /// `origClientOrderId` re-applies the same broker prefix submit stamped (task 6) — see
    /// [`crate::family::rest_reads::query_order_orderid`]'s doc for why this can't be sent bare.
    pub fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let orig =
            crate::family::order_map::binance_broker_coid(self.link_id.as_deref(), client_order_id);
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("origClientOrderId", orig)];
        match self.transport.signed(&self.base_url, PATH_ORDER, "DELETE", &params, &self.signer) {
            Ok(_) => Ok(()),
            Err(exc) if exc.code == -2011 => {
                // order not found — idempotent
                tracing::debug!(target: "vike_binance::spot", client_order_id, "cancel: order already gone");
                Ok(())
            }
            Err(exc) => Err(exc),
        }
    }

    /// Spot modify = ATOMIC cancel-replace (`POST /api/v3/order/cancelReplace`), the only way Binance
    /// spot changes a resting order's PRICE (`order/amend/keepPriority` can only reduce quantity).
    /// There is no in-place amend here, so every guard below exists to keep the engine's view of
    /// the order true; none of it is optional.
    ///
    /// * **Same coid.** The replacement is placed under the SAME client order id as the order it
    ///   replaces (the registry, cancel path and reconcile key on it); the venue order id changes
    ///   and rides the `OrderModified`. The cancel half carries a throwaway id with
    ///   [`REPLACE_CANCEL_ID_PREFIX`] so the stream mapper drops its CANCELED report, and
    ///   `map_history` ignores an older CANCELED row of a coid that has a newer incarnation.
    /// * **`cancelRestrictions=ONLY_NEW`.** The replacement is a FRESH order of the full quantity,
    ///   so replacing an order that already executed would re-expose the executed lots. The venue
    ///   refuses the cancel of a partially filled order atomically (no fill race), which surfaces
    ///   as `OrderModifyRejected`; a partially filled spot order is cancelled and re-submitted by
    ///   the caller instead. This is also why `vike_venues::amend_semantics("binance")`'s perp
    ///   `InPlaceTotal` row stays sound for spot: a spot amend never sees `filled_qty > 0`.
    /// * **`STOP_ON_FAILURE`.** A failed cancel never places the replacement. Venue filters are
    ///   evaluated AFTER the cancel, so a refused replacement (`-2021`) may leave the order
    ///   cancelled: [`Self::settle_replace`] asks the venue what became of it rather than guess.
    /// * LIMIT orders only (the spot builder emits no stop price); anything else is an explicit
    ///   `OrderModifyRejected`, never a silent no-op.
    ///
    /// Always answers with at least one event.
    pub fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let coid = order.client_order_id.clone();
        let reject = |reason: String| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: coid.clone(),
                reason: reason.into(),
                ts: 0,
            })]
        };
        if order.order_type != OrderType::Limit {
            return reject(
                "binance spot modify supports LIMIT orders only (cancel and re-submit instead)"
                    .to_string(),
            );
        }
        let mut projected = order.clone();
        if let Some(q) = new_qty {
            projected.qty = q;
        }
        if let Some(p) = new_price {
            projected.price = Some(p);
        }
        let price_ok = projected.price.is_some_and(|p| p.is_finite() && p > 0.0);
        if !(projected.qty.is_finite() && projected.qty > 0.0) || !price_ok {
            return reject("binance spot modify needs a positive finite quantity and price".into());
        }
        let orig = crate::family::order_map::binance_broker_coid(self.link_id.as_deref(), &coid);
        static REPLACE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let cancel_id = format!(
            "{REPLACE_CANCEL_ID_PREFIX}{}-{}",
            vike_model::now_ms(),
            REPLACE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut params = self.build_order_params(&projected);
        params.push(("cancelReplaceMode", "STOP_ON_FAILURE".to_string()));
        params.push(("cancelRestrictions", "ONLY_NEW".to_string()));
        params.push(("cancelOrigClientOrderId", orig.clone()));
        params.push(("cancelNewClientOrderId", cancel_id));
        match self.transport.signed(
            &self.base_url,
            PATH_CANCEL_REPLACE,
            "POST",
            &params,
            &self.signer,
        ) {
            Ok(resp) => {
                let void = resp
                    .get("newOrderResponse")
                    .and_then(|n| n.get("orderId"))
                    .or_else(|| resp.get("orderId"))
                    .map(json_id)
                    .unwrap_or_default();
                vec![Event::OrderModified(OrderModified {
                    client_order_id: coid.clone(),
                    venue_order_id: Some(void.into()),
                    new_qty,
                    new_price,
                    ts: 0,
                })]
            }
            Err(exc) => {
                tracing::warn!(
                    target: "vike_binance::spot",
                    code = exc.code,
                    msg = %exc.msg,
                    "cancel-replace modify failed"
                );
                if exc.code == CANCEL_REPLACE_PARTIAL
                    || exc.code == CANCEL_REPLACE_FAILED
                    || crate::error_codes::submit_outcome_unknown(exc.code)
                {
                    self.settle_replace(&coid, &orig, &projected, new_qty, new_price, &exc)
                } else {
                    // Refused before (or instead of) the cancel: the order is untouched.
                    reject(exc.msg)
                }
            }
        }
    }

    /// After a cancel-replace whose outcome is not a clean success (`-2021` partial failure,
    /// `-2022` failure, or a timeout / status-unknown code that may have gone through), ask the venue what the coid is
    /// NOW and report that: the error code alone cannot say whether the cancel half landed (the
    /// transport keeps only `{code, msg}`, not the response's `data`).
    ///
    /// * live at the requested terms: `OrderModified` (the replace did go through);
    /// * live at other terms: `OrderModifyRejected` (untouched);
    /// * cancelled / expired / rejected / unknown: `OrderCanceled` (the cancel landed, the
    ///   replacement did not; the order is GONE and the engine must say so, because the stream's
    ///   report of that cancel is dropped);
    /// * filled or any other status: `OrderModifyRejected` (the fill events own the terminal);
    /// * query failed: `OrderModifyRejected` naming the doubt; the gap-sentinel resync that
    ///   `run_loop` arms after every modify replays the true state.
    fn settle_replace(
        &self,
        coid: &str,
        orig: &str,
        projected: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        cause: &VenueApiError,
    ) -> Vec<Event> {
        let reject = |reason: String| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: coid.to_string(),
                reason: reason.into(),
                ts: 0,
            })]
        };
        let gone = |why: &str| {
            vec![Event::OrderCanceled(OrderCanceled {
                client_order_id: coid.to_string(),
                reason: format!("cancel-replace: {why} ({})", cause.msg).into(),
                ts: 0,
            })]
        };
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("origClientOrderId", orig.to_string())];
        let body = match self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER,
            "GET",
            &params,
            &self.signer,
        ) {
            Ok(b) => b,
            // -2013 / -2011: the venue has no such order any more.
            Err(e) if e.code == -2013 || e.code == -2011 => {
                return gone("the order is gone at the venue");
            }
            Err(e) => {
                return reject(format!(
                    "{} (order state unknown, status query failed: {}; resync pending)",
                    cause.msg, e.msg
                ));
            }
        };
        match body.get("status").and_then(|v| v.as_str()).unwrap_or("") {
            "NEW" | "PARTIALLY_FILLED" => {
                let tick = self.properties.tick_size.max(f64::MIN_POSITIVE);
                let step = self.properties.step_size.max(f64::MIN_POSITIVE);
                let price = body.get("price").and_then(json_num);
                let qty = body.get("origQty").and_then(json_num);
                let replaced =
                    price.zip(projected.price).is_some_and(|(a, b)| (a - b).abs() < tick)
                        && qty.is_some_and(|q| (q - projected.qty).abs() < step);
                if replaced {
                    vec![Event::OrderModified(OrderModified {
                        client_order_id: coid.to_string(),
                        venue_order_id: body.get("orderId").map(|v| json_id(v).into()),
                        new_qty,
                        new_price,
                        ts: 0,
                    })]
                } else {
                    reject(cause.msg.clone())
                }
            }
            "CANCELED" | "EXPIRED" | "EXPIRED_IN_MATCH" | "REJECTED" => {
                gone("the cancel landed, the replacement did not")
            }
            _ => reject(cause.msg.clone()),
        }
    }

    /// Spot reconcile (crypto_client.py `connect`): free balance + open orders (locked
    /// sell qty added back — Binance "free" excludes it) + ticker mark seeded as avg_px
    /// so an immediate close is ~0 PnL instead of garbage.
    pub fn connect(&self) -> Result<ReconcileSnapshot, VenueApiError> {
        let account =
            self.transport.signed(&self.base_url, PATH_ACCOUNT, "GET", &[], &self.signer)?;
        let mut free = 0.0;
        for b in account.get("balances").and_then(|b| b.as_array()).unwrap_or(&vec![]) {
            if b.get("asset").and_then(|a| a.as_str()) == Some(self.base_asset.as_str()) {
                free = b.get("free").and_then(json_num).unwrap_or(0.0);
                break;
            }
        }

        let raw = self.transport.signed(
            &self.base_url,
            PATH_OPEN_ORDERS,
            "GET",
            &[("symbol", self.symbol.clone())],
            &self.signer,
        )?;
        let mut locked_sell_qty = 0.0;
        let mut orders: Vec<ManagedOrder> = Vec::new();
        for o in raw.as_array().unwrap_or(&vec![]) {
            let side = if o.get("side").and_then(|s| s.as_str()) == Some("BUY") { 1 } else { -1 };
            let orig_qty = o.get("origQty").and_then(json_num).unwrap_or(0.0);
            // a non-finite wire qty ("NaN", "1e999") would serialize as JSON null and fail the typed
            // decode below
            let orig_qty = if orig_qty.is_finite() { orig_qty } else { 0.0 };
            let executed_qty = o.get("executedQty").and_then(json_num).unwrap_or(0.0);
            if side < 0 {
                locked_sell_qty += (orig_qty - executed_qty).max(0.0);
            }
            // Python: price None for "", "0", "0.00000000" (unpriced/market rows)
            let price = o.get("price").and_then(|p| p.as_str()).and_then(|p| {
                if matches!(p, "" | "0" | "0.00000000") { None } else { p.parse::<f64>().ok() }
            });
            // Broker-prefix stripped (unified cross-venue attribution, task 6 fix-round-1): this
            // snapshot-adopted order's coid must match the registry's bare local coid, exactly like
            // the WS mapper's decode (see `family::order_map::strip_broker_coid_prefix`'s doc).
            let coid = o
                .get("clientOrderId")
                .and_then(|c| c.as_str())
                .map(crate::family::order_map::strip_broker_coid_prefix)
                .unwrap_or("");
            let request: OrderRequest = serde_json::from_value(serde_json::json!({
                "client_order_id": coid,
                "venue": VENUE,
                "symbol": self.symbol,
                "side": side,
                "qty": orig_qty,
                "order_type": o.get("type").and_then(|t| t.as_str()).unwrap_or("").to_lowercase(),
                "price": price,
            }))
            .expect("static shape");
            let mut mo = ManagedOrder::new(request);
            mo.status = OrderStatus::Accepted;
            mo.venue_order_id = Some(o.get("orderId").map(json_id).unwrap_or_default());
            orders.push(mo);
        }

        let seeded_size = free + locked_sell_qty; // Binance "free" is the free portion only
        let ticker = self.transport.public(
            &self.base_url,
            PATH_TICKER,
            &[("symbol", self.symbol.clone())],
        )?;
        let mark_px = ticker.get("price").and_then(json_num).unwrap_or(0.0);
        Ok(ReconcileSnapshot {
            positions: vec![(self.symbol.clone(), seeded_size)],
            open_orders: orders,
            position_avg_px: vec![(self.symbol.clone(), mark_px)],
            position_mark_px: Vec::new(),
            position_sides: Vec::new(),
            balance: 0.0,
            // Spot carries no margin mode on the wire — empty = carry priors (default Cross).
            position_margin: Vec::new(),
        })
    }

    /// GET /api/v3/time → server-minus-local offset for `Signer::set_offset_ms`. The body is
    /// [`crate::family::rest_reads::server_time_offset`], shared with aster.
    pub fn server_time_offset(&self, local_now_ms: i64) -> Result<i64, VenueApiError> {
        crate::family::rest_reads::server_time_offset(
            &self.transport,
            &self.base_url,
            PATH_TIME,
            local_now_ms,
        )
    }
}

impl<S: Signer + Send, T: RestTransport + Send> ConfirmByCoid for BinanceSpotRest<S, T> {
    fn confirm_when(&self, coid: &str, ts: i64, dispatched: bool) -> Vec<Event> {
        crate::family::confirm::resolve_confirm(coid, ts, self.lookup_by_coid(coid), dispatched)
    }
}

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for BinanceSpotRest<S, T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        BinanceSpotRest::submit_order(self, request)
    }
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        BinanceSpotRest::cancel_order(self, client_order_id)
    }
    /// ACTIVELY confirm one order by its client id (the by-coid status GET): found -> `OrderAccepted`
    /// (+ the terminal when the venue already closed it), absent -> the "venue confirms absent"
    /// reject, a FAILED query -> NOTHING (the watchdog ladder and reconcile decide). This trait door
    /// assumes the submit was fully dispatched; the exec thread's confirm closure goes through
    /// [`ConfirmByCoid::confirm_when`] with the real answer.
    fn confirm_order(&self, client_order_id: &str, ts: i64) -> Vec<Event> {
        self.confirm_when(client_order_id, ts, true)
    }
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        BinanceSpotRest::modify_order(self, order, new_qty, new_price)
    }
}

/// Back-compat name for the slice-2 smoke wiring.
pub type LiveBinanceSpotClient<S, T> = LiveRestClient<BinanceSpotRest<S, T>>;

#[cfg(test)]
mod identity_tests {
    use super::*;

    /// **THE MEASURED SHAPE.** The live demo endpoint answered `uid` as a JSON NUMBER on
    /// 2026-09-20, alongside the `commissionRates` object this same body is already read for.
    #[test]
    fn a_numeric_uid_is_read_as_the_account_identity() {
        let body = serde_json::json!({
            "accountType": "SPOT",
            "uid": 1_234_567_890_u64,
            "commissionRates": {"maker": "0.00100000", "taker": "0.00100000"},
        });
        assert_eq!(parse_account_identity(&body).as_deref(), Some("1234567890"));
        // …and the field this body has always been read for is untouched by the new one.
        assert!(parse_commission_rates(&body).is_some(), "the fee read still works");
    }

    /// ⚠ **A STRING uid is read too, and this is not defensive coding.** The probe saw a number on
    /// the SPOT demo endpoint; okx answers its own uid as a string, and a venue is free to widen a
    /// numeric id the day it outgrows 2^53. Accepting both costs one match arm and removes a class
    /// of silent `None` that would read as *this account has no uid*.
    #[test]
    fn a_string_uid_is_read_too() {
        let body = serde_json::json!({ "uid": "  592123  " });
        assert_eq!(
            parse_account_identity(&body).as_deref(),
            Some("592123"),
            "and it is trimmed — an operator-visible id with edge whitespace is one id"
        );
    }

    /// **AN ABSENT OR UNUSABLE uid DEGRADES, it does not fail.** *Not yet known* is the state every
    /// migrated `account` row already carries, and `venue_account_id`'s own doc forbids reading it
    /// as *this account has no book*. A start must not be lost over a bookkeeping field.
    #[test]
    fn an_absent_or_unusable_uid_is_not_yet_known() {
        for body in [
            serde_json::json!({ "accountType": "SPOT" }),
            serde_json::json!({ "uid": serde_json::Value::Null }),
            serde_json::json!({ "uid": "" }),
            serde_json::json!({ "uid": "   " }),
            serde_json::json!({ "uid": {"nested": 1} }),
            serde_json::json!({ "uid": [1, 2] }),
            serde_json::json!({ "uid": true }),
        ] {
            assert_eq!(parse_account_identity(&body), None, "{body}");
        }
    }
}
