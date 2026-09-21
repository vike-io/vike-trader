//! BinanceSpotRest — signed REST submit/cancel/connect. Exact port of
//! `exec/binance/client.py` + the shared `exec/crypto_client.py` flow.
//!
//! The WS executionReport is the SOLE source of fills; the REST POST/DELETE response is
//! ACK-ONLY. `submit_order` returns [OrderSubmitted, OrderAccepted|OrderRejected] for the
//! session to pump into the core ingest (Python published the same pair to its bus).
//! qty/price are formatted to the symbol's step/tick as decimal strings to dodge
//! -1111 BAD_PRECISION.

use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_exec::{ManagedOrder, OrderStatus, ReconcileSnapshot};
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};
use vike_model::{OrderRequest, SymbolProperties};

use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// The pure, byte-identical-between-binance/aster mappers live once in `crate::family::order_map`
// (mappers rung): `json_id` (used internally below), and `parse_symbol_properties` re-exported so
// `vike_binance::spot::parse_symbol_properties` stays the same public path.
use crate::family::order_map::json_id;
pub use crate::family::order_map::parse_symbol_properties;

// Homed at the crate root since the exec/feeds seam (this module is `exec`-gated, the venue id is
// not: `crate::data`'s keyless kline REST names it too); imported here like any other consumer —
// no `crate::spot::VENUE` re-export, per the no-pub-use-shims-on-a-move convention (root
// CLAUDE.md). The value is unchanged, so every signed order this module builds is byte-identical.
use crate::VENUE;
pub const PATH_ORDER: &str = "/api/v3/order";
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
/// was read). The fee model (steal-list #5) surfaces them so a live mount can prefer the account's
/// actual rates over the static [`vike_model::fee_schedule_for`] default.
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
/// on file from writing a venue parser out of public docs — `vike_bridge_core::key_permissions`'
/// module doc carries it as *the `canWithdraw` trap* — and `vike_mount::book_identity`'s binance row
/// had said only that *an authenticated call* could answer, naming no field, because nothing here
/// had ever looked.
///
/// # Why this costs NOTHING to call
///
/// It parses a body the live path already holds. `vike_mount::arming`'s `resolve_fee_schedule` makes
/// one blocking `/api/v3/account` read at every live mount (through `fetch_fee_rates`, whose spot
/// arm is parameterless), and `vike_mount::startup`'s `authed_read_probes` reaches the same endpoint
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
/// rather than fail a mount over a bookkeeping field.
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
    /// at mount from `BINANCE_BROKER_CODE`/`BINANCE_BUILDER_CODE` via `attribution_code_from` (see
    /// `vike_bridge_core::credentials`). Applied at the venue edge to every client-order-id string
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
        if let Some(reject) = vike_bridge_core::tif::deny_unsupported_tif(VENUE, request)
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
            Err(exc) if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
                events.push(self.resolve_ambiguous_submit(request));
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
    /// history replay. On the short-timeout requery transport so the resync can't stall.
    pub fn get_all_orders(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            "/api/v3/allOrders",
            "GET",
            &params,
            &self.signer,
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

    /// Audit T1: re-query the venue for the order by our idempotent client_order_id to resolve an
    /// ambiguous (timed-out) submit. `Ok(Some(id))` = live, `Ok(None)` = venue confirms absent
    /// (-2013/-2011), `Err` = query failed. Runs on `signed_requery` (the short-timeout transport)
    /// so a double-timeout can't stall the core toward ~60s. `origClientOrderId` must match the
    /// venue's OWN stored `clientOrderId` exactly, so `coid` (always the bare local id) is
    /// re-prefixed here the same way submit stamped it (task 6) — never sent bare when a link id is
    /// configured, or the venue would report the order unknown.
    fn query_order_orderid(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
        let orig = crate::family::order_map::binance_broker_coid(self.link_id.as_deref(), coid);
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("origClientOrderId", orig)];
        match self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER,
            "GET",
            &params,
            &self.signer,
        ) {
            Ok(resp) => Ok(resp.get("orderId").map(json_id)),
            Err(exc) if exc.code == -2013 || exc.code == -2011 => Ok(None),
            Err(exc) => Err(exc),
        }
    }

    /// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1): live → managed
    /// OrderAccepted; venue-confirmed-absent → true terminal OrderRejected; query inconclusive →
    /// optimistic OrderAccepted (never a false terminal).
    fn resolve_ambiguous_submit(&self, request: &OrderRequest) -> Event {
        vike_bridge_core::resolve_ambiguous_submit(
            &request.client_order_id,
            request.ts,
            self.query_order_orderid(&request.client_order_id),
        )
    }

    /// Cancel by client-order-id. "Unknown order" (-2011) is swallowed (already gone ≠
    /// failure — the R6 gate's "unknown ≠ rejection" rule); other codes propagate.
    /// `origClientOrderId` re-applies the same broker prefix submit stamped (task 6) — see
    /// [`Self::query_order_orderid`]'s doc for why this can't be sent bare.
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

    /// GET /api/v3/time → server-minus-local offset for `Signer::set_offset_ms`.
    pub fn server_time_offset(&self, local_now_ms: i64) -> Result<i64, VenueApiError> {
        let resp = self.transport.public(&self.base_url, PATH_TIME, &[])?;
        let server = resp.get("serverTime").and_then(|t| t.as_i64()).unwrap_or(local_now_ms);
        Ok(server - local_now_ms)
    }
}

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for BinanceSpotRest<S, T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        BinanceSpotRest::submit_order(self, request)
    }
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        BinanceSpotRest::cancel_order(self, client_order_id)
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
    /// as *this account has no book*. A mount must not be lost over a bookkeeping field.
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
