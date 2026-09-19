//! BinancePerpRest — signed fapi (USDS-M futures) submit/cancel/reconcile + set-leverage.
//! Exact port of `exec/binance/perp_client.py` + `perp_instruments.py`.
//!
//! fapi deltas vs spot: /fapi/ paths, positionSide='BOTH' (one-way; hedge is 5f),
//! reduceOnly as the STRING 'true'/'false', qty in BASE asset (no ctVal), set-leverage
//! (idempotent HTTP-200 — no benign swallow), and a /fapi/v2/positionRisk signed-position
//! reconcile (positionAmt is ALREADY SIGNED: long > 0, short < 0). The fill stream is the
//! listenKey user-data WS (`perp_user_data.rs`).

use indexmap::IndexMap;
use serde_json::Value;

use vike_bridge_core::rest::VenueRest;
use vike_exec::{ManagedOrder, ReconcileSnapshot};
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted};
use vike_model::{OrderRequest, SymbolProperties, TimeInForce};

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// `json_id` (the numeric-id coercion) is shared, byte-identical between binance/aster — one copy
// lives in `crate::family::order_map` (mappers rung).
use crate::family::order_map::json_id;

pub const PATH_ORDER: &str = "/fapi/v1/order";
pub const PATH_BATCH_ORDERS: &str = "/fapi/v1/batchOrders";
/// fapi caps batchOrders at 5 orders per request.
pub const BATCH_MAX: usize = 5;
pub const PATH_POSITIONS: &str = "/fapi/v2/positionRisk";
/// fapi live open orders (signed) — the perp twin of spot `PATH_OPEN_ORDERS`.
pub const PATH_OPEN_ORDERS: &str = "/fapi/v1/openOrders";
pub const PATH_SET_LEVERAGE: &str = "/fapi/v1/leverage";
pub const PATH_BALANCE: &str = "/fapi/v2/balance";
pub const PATH_EXCHANGE_INFO: &str = "/fapi/v1/exchangeInfo";

pub const DEMO_FAPI_REST: &str = "https://demo-fapi.binance.com";
pub const MAINNET_FAPI_REST: &str = "https://fapi.binance.com";
pub const DEMO_FAPI_WS: &str = "wss://fstream.binancefuture.com/ws";
pub const MAINNET_FAPI_WS: &str = "wss://fstream.binance.com/ws";

/// This lane's `vike_bridge_core::tif::venue_tif` row key — the `"binance-perp"` LANE sub-key,
/// NOT the canonical venue id ([`crate::VENUE`] stays `"binance"` everywhere an
/// `OrderRequest`/event names the venue). The lane split exists because fapi has native GTD
/// while spot `/api/v3` has none, and the TIF table keys rows by string: the perp submit gates
/// and the family builder's perp face consume THIS key; spot keeps consuming `"binance"`.
pub const TIF_LANE: &str = "binance-perp";

/// fapi native-GTD wire constraints (POST /fapi/v1/order, venue error `-5040
/// FUTURE_GOOD_TILL_DATE`): `goodTillDate` is MANDATORY with `timeInForce=GTD`, "must be greater
/// than the current time plus 600 seconds and smaller than 253402300799000" (UTC 9999-12-31
/// 23:59:59). The venue keeps second-level precision only (the ms part is ignored server-side),
/// so there is no local granularity constraint to enforce.
pub const GTD_MIN_LEAD_MS: i64 = 600_000;
/// See [`GTD_MIN_LEAD_MS`] — the exclusive upper bound fapi accepts for `goodTillDate`.
pub const GTD_MAX_MS: i64 = 253_402_300_799_000;

/// The perp lane's GTD VALIDITY gate — the dynamic companion of the static
/// `venue_tif(TIF_LANE, Gtd) = Mapped("GTD")` row (a table row cannot carry a date). Returns the
/// terminal `OrderRejected` (emitter split: pushed after the synchronous `OrderSubmitted`, wire
/// untouched) when a limit-GTD request cannot satisfy fapi's checkable-here constraints:
///
/// - no `gtd_expiry` at all — a good-till date is never invented client-side;
/// - `gtd_expiry >= `[`GTD_MAX_MS`] (static venue bound);
/// - `gtd_expiry < now_ms + `[`GTD_MIN_LEAD_MS`] (the venue's "now + 600s" floor, checked
///   against the caller's wall clock — best-effort: a request that squeaks past locally but
///   crosses the floor in flight is rejected by the VENUE, which surfaces through the submit
///   path's error arm as a proper `OrderRejected`, never silence).
///
/// Non-limit paths and non-GTD TIFs pass (`None`) — they carry no `goodTillDate` axis. `now_ms`
/// is a parameter (not read here) so the gate stays pure/deterministic under test; the submit
/// paths pass `vike_model::clock::now_ms()`.
#[must_use]
pub fn deny_invalid_gtd(request: &OrderRequest, now_ms: i64) -> Option<Event> {
    if !request.order_type.eq_ignore_ascii_case("limit")
        || request.time_in_force != TimeInForce::Gtd
    {
        return None;
    }
    let reason = match request.gtd_expiry {
        None => format!(
            "GTD on {TIF_LANE} requires gtd_expiry (fapi goodTillDate is mandatory) — a \
             good-till date is never invented client-side; order refused"
        ),
        Some(exp) if exp >= GTD_MAX_MS => format!(
            "gtd_expiry {exp} is at/after the venue maximum {GTD_MAX_MS} (fapi -5040 bound) — \
             order refused"
        ),
        Some(exp) if exp < now_ms + GTD_MIN_LEAD_MS => format!(
            "gtd_expiry {exp} is less than 600s ahead of now ({now_ms}) — fapi requires \
             goodTillDate > now + 600s (-5040); order refused"
        ),
        Some(_) => return None,
    };
    Some(Event::OrderRejected(OrderRejected {
        client_order_id: request.client_order_id.clone(),
        reason: reason.into(),
        ts: request.ts,
    }))
}

/// RiskLimits-shaped properties + base_asset — twin of `parse_binance_perp_instruments`.
/// Perp deltas: market-order qty cap from MARKET_LOT_SIZE.maxQty; min notional from
/// MIN_NOTIONAL.notional (spot uses NOTIONAL/minNotional).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerpInstrument {
    pub properties: SymbolProperties,
    pub base_asset: String,
}

pub fn parse_binance_perp_instruments(payload: &Value) -> IndexMap<String, PerpInstrument> {
    let mut out = IndexMap::new();
    let Some(symbols) = payload.get("symbols").and_then(|s| s.as_array()) else {
        return out;
    };
    for entry in symbols {
        let symbol = entry.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        let by_type: IndexMap<&str, &Value> = entry
            .get("filters")
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|f| f.get("filterType").and_then(|t| t.as_str()).map(|t| (t, f)))
                    .collect()
            })
            .unwrap_or_default();
        let f = |ftype: &str, field: &str| -> f64 {
            by_type.get(ftype).and_then(|v| v.get(field)).and_then(json_num).unwrap_or(0.0)
        };
        out.insert(
            symbol,
            PerpInstrument {
                properties: SymbolProperties {
                    tick_size: f("PRICE_FILTER", "tickSize"),
                    step_size: f("LOT_SIZE", "stepSize"),
                    min_qty: f("LOT_SIZE", "minQty"),
                    max_qty: f("MARKET_LOT_SIZE", "maxQty"), // market-order cap
                    min_notional: f("MIN_NOTIONAL", "notional"),
                    // Everything else absent: Binance USDT-margined perps are 1:1 with the base
                    // asset (multiplier 1.0), `/fapi` `/exchangeInfo` reports ONE `tickSize` (flat
                    // grid, no tiers), and no venue taker hold. FRU rather than an exhaustive
                    // literal so a new `SymbolProperties` field costs this parser nothing.
                    ..Default::default()
                },
                base_asset: entry
                    .get("baseAsset")
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string(),
            },
        );
    }
    out
}

/// Venue face of [`crate::family::order_map::map_perp_open_order`] (mappers rung) bound to the
/// `"binance"` venue key (`crate::VENUE`). Keeps its 1-arg signature so the reconcile call
/// site ([`BinancePerpRest::fetch_open_orders`]) and the r6 parity test call it unchanged; the
/// shared body maps ONE fapi `/openOrders` row → a reconcile-seeded ACCEPTED [`ManagedOrder`].
pub fn map_perp_open_order(o: &Value) -> ManagedOrder {
    crate::family::order_map::map_perp_open_order(o, crate::VENUE)
}

/// The REST half of the live perp client (ACK-only); fills come from the listenKey WS.
pub struct BinancePerpRest<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub leverage: f64,
    /// Binance Broker/Link attribution id — see [`crate::spot::BinanceSpotRest::link_id`]'s doc
    /// (unified cross-venue attribution, task 6); the perp twin, applied identically to
    /// `newClientOrderId`/`origClientOrderId`/`origClientOrderIdList`.
    pub link_id: Option<String>,
}

impl<S: Signer, T: RestTransport> BinancePerpRest<S, T> {
    /// POST /fapi/v1/leverage {symbol, leverage:int}. Binance change-leverage is
    /// idempotent (HTTP-200 even when already at target) — NO benign-error swallow.
    pub fn set_leverage(&self) -> Result<(), VenueApiError> {
        let params: Vec<(&str, String)> = vec![
            ("symbol", self.symbol.clone()),
            ("leverage", format!("{}", self.leverage as i64)),
        ];
        self.transport
            .signed(&self.base_url, PATH_SET_LEVERAGE, "POST", &params, &self.signer)
            .map(|_| ())
    }

    /// Pure, golden-gated. Delegates to the shared, byte-identical
    /// [`crate::family::order_map::build_perp_order_params`] (mappers rung), passing this client's
    /// `symbol`/`properties`: positionSide BOTH (one-way); reduceOnly as STRING; `order_type:"stop"`
    /// maps to a native STOP_MARKET conditional (additive over the golden limit/market shapes).
    /// The TIF row key is [`TIF_LANE`] (`"binance-perp"`, native fapi GTD) — every non-GTD
    /// request's bytes are identical to the `"binance"` row it consumed before the lane split.
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        crate::family::order_map::build_perp_order_params(
            request,
            &self.symbol,
            &self.properties,
            TIF_LANE,
            self.link_id.as_deref(),
        )
    }

    /// fapi wallet balance → the USDT `balance` (total wallet cash, not counting
    /// unrealized — consistent with Bybit walletBalance). Default-safe: any failure
    /// returns 0.0 so reconcile is never broken.
    fn fetch_usdt_balance(&self) -> f64 {
        let Ok(rows) =
            self.transport.signed(&self.base_url, PATH_BALANCE, "GET", &[], &self.signer)
        else {
            return 0.0;
        };
        for entry in rows.as_array().unwrap_or(&vec![]) {
            if entry.get("asset").and_then(|a| a.as_str()) == Some("USDT") {
                return entry.get("balance").and_then(json_num).unwrap_or(0.0);
            }
        }
        0.0
    }

    /// Signed GET /fapi/v1/openOrders {symbol} → the venue's resting orders as reconcile-seeded
    /// [`ManagedOrder`]s (the perp twin of spot `connect`'s open-order fetch — perps previously
    /// returned an EMPTY `open_orders`, so `apply_snapshot`'s stale-order reap could never fire on
    /// a perp). Best-effort by contract: a REST hiccup returns `Vec::new()` (skip the order-fetch,
    /// KEEP the position snapshot) rather than failing the whole reconcile — matching the venue's
    /// "REST hiccup ⇒ skip tick" convention. This is the reconcile path only, already off the fold.
    fn fetch_open_orders(&self) -> Vec<ManagedOrder> {
        let Ok(raw) = self.transport.signed(
            &self.base_url,
            PATH_OPEN_ORDERS,
            "GET",
            &[("symbol", self.symbol.clone())],
            &self.signer,
        ) else {
            return Vec::new();
        };
        raw.as_array().unwrap_or(&vec![]).iter().map(map_perp_open_order).collect()
    }

    /// GET /fapi/v2/positionRisk {symbol}: positionAmt is ALREADY SIGNED base qty.
    /// One snapshot row per live leg — net = a single BOTH row (no position_sides
    /// entries, byte-equivalent); hedge = a LONG row AND a SHORT row with sides. Flat =
    /// one zero BOTH row. Balance rides along best-effort.
    pub fn reconcile_positions(&self) -> Result<ReconcileSnapshot, VenueApiError> {
        let rows = self.transport.signed(
            &self.base_url,
            PATH_POSITIONS,
            "GET",
            &[("symbol", self.symbol.clone())],
            &self.signer,
        )?;
        let bal = self.fetch_usdt_balance();
        // Best-effort resting-order fetch (empty on a REST hiccup) — feeds apply_snapshot's
        // stale-order reap. Fetched once; moved into whichever return path fires.
        let open_orders = self.fetch_open_orders();
        // (signed_qty, avg, mark, side, reported margin mode + isolated wallet)
        type Leg = (f64, f64, f64, String, (vike_model::MarginMode, Option<f64>));
        let mut legs: Vec<Leg> = Vec::new();
        for p in rows.as_array().unwrap_or(&vec![]) {
            let side = p.get("positionSide").and_then(|s| s.as_str()).unwrap_or("BOTH").to_string();
            let amt = p.get("positionAmt").and_then(json_num).unwrap_or(0.0); // already signed
            if amt == 0.0 {
                continue;
            }
            legs.push((
                amt,
                p.get("entryPrice").and_then(json_num).unwrap_or(0.0),
                p.get("markPrice").and_then(json_num).unwrap_or(0.0),
                side,
                // Step-2 (read-side only): `marginType`/`isolatedWallet` off the SAME row —
                // the shared Binance-grammar read the family recon rung fixture-tests.
                crate::family::recon::parse_margin_type(p),
            ));
        }
        if legs.is_empty() {
            // flat: one zero BOTH row (unchanged from pre-perp shape). No margin info reported
            // for a flat leg — empty `position_margin` = carry priors forward (the #487 law).
            return Ok(ReconcileSnapshot {
                positions: vec![(self.symbol.clone(), 0.0)],
                open_orders,
                position_avg_px: vec![(self.symbol.clone(), 0.0)],
                position_mark_px: vec![(self.symbol.clone(), 0.0)],
                position_sides: Vec::new(),
                balance: bal,
                position_margin: Vec::new(),
            });
        }
        let hedge = legs.iter().any(|(_, _, _, side, _)| side != "BOTH");
        Ok(ReconcileSnapshot {
            positions: legs.iter().map(|(q, ..)| (self.symbol.clone(), *q)).collect(),
            open_orders,
            position_avg_px: legs.iter().map(|(_, a, ..)| (self.symbol.clone(), *a)).collect(),
            position_mark_px: legs.iter().map(|(_, _, m, ..)| (self.symbol.clone(), *m)).collect(),
            position_sides: if hedge {
                legs.iter().map(|(_, _, _, sd, _)| (self.symbol.clone(), sd.clone())).collect()
            } else {
                Vec::new()
            },
            balance: bal,
            position_margin: legs
                .iter()
                .map(|(.., (mode, iso))| (self.symbol.clone(), *mode, *iso))
                .collect(),
        })
    }

    /// Native batch-submit (POST /fapi/v1/batchOrders — `batchOrders` is a JSON-array string param,
    /// ≤5/req). Emits OrderSubmitted per order, then per-order Accepted|Rejected from the response
    /// array (a success element has `orderId`; an error element has `code`/`msg`), matched by
    /// index. Chunks at BATCH_MAX. RUST-NATIVE, no Python twin.
    pub fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        let mut events: Vec<Event> = requests
            .iter()
            .map(|r| {
                Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: r.client_order_id.clone(),
                    ts: r.ts,
                })
            })
            .collect();
        // tif step-2 gate, per order: a TIF this lane cannot express (Day) or a GTD whose date
        // fails the checkable-here fapi bounds ([`deny_invalid_gtd`]) is a LOUD terminal reject
        // and its order never enters a wire chunk; the supported rest batch as before. The
        // trigger-source gate (`deny_unsupported_trigger_by` — Index has no fapi workingType)
        // rides the same per-order chain.
        let now = vike_model::clock::now_ms();
        let mut sendable: Vec<&OrderRequest> = Vec::with_capacity(requests.len());
        for r in requests {
            match vike_bridge_core::tif::deny_unsupported_tif(TIF_LANE, r)
                .or_else(|| deny_invalid_gtd(r, now))
                .or_else(|| vike_bridge_core::trigger::deny_unsupported_trigger_by(TIF_LANE, r))
            {
                Some(reject) => events.push(reject),
                None => sendable.push(r),
            }
        }
        for chunk in sendable.chunks(BATCH_MAX) {
            let orders: Vec<Value> = chunk
                .iter()
                .map(|r| {
                    Value::Object(
                        self.build_order_params(r)
                            .into_iter()
                            .map(|(k, v)| (k.to_string(), Value::String(v)))
                            .collect(),
                    )
                })
                .collect();
            let params: Vec<(&str, String)> =
                vec![("batchOrders", serde_json::to_string(&orders).expect("array"))];
            match self.transport.signed(
                &self.base_url,
                PATH_BATCH_ORDERS,
                "POST",
                &params,
                &self.signer,
            ) {
                Ok(resp) => {
                    let list = resp.as_array().cloned().unwrap_or_default();
                    for (i, r) in chunk.iter().enumerate() {
                        match list.get(i) {
                            Some(o) if o.get("orderId").is_some() => {
                                events.push(Event::OrderAccepted(OrderAccepted {
                                    client_order_id: r.client_order_id.clone(),
                                    venue_order_id: Some(
                                        o.get("orderId").map(json_id).unwrap_or_default().into(),
                                    ),
                                    ts: r.ts,
                                }));
                            }
                            Some(o) => events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: o
                                    .get("msg")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("batch order reject")
                                    .into(),
                                ts: r.ts,
                            })),
                            None => events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: "no batch ack".to_string().into(),
                                ts: r.ts,
                            })),
                        }
                    }
                }
                // audit T1: an ambiguous whole-batch timeout may have been accepted — re-query each
                // order's status instead of rejecting the whole chunk (which could strand phantoms).
                Err(exc) if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
                    for r in chunk {
                        events.push(self.resolve_ambiguous_submit(r));
                    }
                }
                Err(exc) => {
                    for r in chunk {
                        events.push(Event::OrderRejected(OrderRejected {
                            client_order_id: r.client_order_id.clone(),
                            reason: exc.msg.clone().into(),
                            ts: r.ts,
                        }));
                    }
                }
            }
        }
        events
    }

    /// Native batch-cancel (DELETE /fapi/v1/batchOrders, origClientOrderIdList = JSON array, ≤5).
    /// Per-order "already gone" rides the response array (non-fatal); a whole-request/network
    /// failure returns the first error. Chunks at BATCH_MAX. RUST-NATIVE. Each id is re-prefixed
    /// with the same broker link id submit stamped (task 6) — see
    /// [`BinancePerpRest::query_order_orderid`]'s doc for why a bare id can't identify the order.
    pub fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        for chunk in client_order_ids.chunks(BATCH_MAX) {
            let orig_ids: Vec<String> = chunk
                .iter()
                .map(|c| crate::family::order_map::binance_broker_coid(self.link_id.as_deref(), c))
                .collect();
            let params: Vec<(&str, String)> = vec![
                ("symbol", self.symbol.clone()),
                ("origClientOrderIdList", serde_json::to_string(&orig_ids).expect("array")),
            ];
            if let Err(e) = self.transport.signed(
                &self.base_url,
                PATH_BATCH_ORDERS,
                "DELETE",
                &params,
                &self.signer,
            ) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Native modify (PUT /fapi/v1/order): fapi modify requires `side` PLUS both `quantity` and
    /// `price`, so an unchanged field falls back to the resting `order`'s value (this is exactly why
    /// the modify seam passes the whole order, not just its id). Returns `[OrderModified]` on the
    /// venue's ack; `[]` on failure (the order keeps its terms). RUST-NATIVE, no Python twin.
    pub fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let qty = new_qty.unwrap_or(order.qty);
        let price = new_price.or(order.price).unwrap_or(0.0);
        // origClientOrderId must match the venue's OWN stored id — re-prefix (task 6), same as cancel.
        let orig = crate::family::order_map::binance_broker_coid(
            self.link_id.as_deref(),
            &order.client_order_id,
        );
        let params: Vec<(&str, String)> = vec![
            ("symbol", self.symbol.clone()),
            ("side", if order.side > 0 { "BUY" } else { "SELL" }.to_string()),
            ("origClientOrderId", orig),
            ("quantity", format_to_step_f(qty, self.properties.step_size)),
            ("price", format_to_step_f(price, self.properties.tick_size)),
        ];
        match self.transport.signed(&self.base_url, PATH_ORDER, "PUT", &params, &self.signer) {
            Ok(resp) => vec![Event::OrderModified(OrderModified {
                client_order_id: order.client_order_id.clone(),
                venue_order_id: Some(resp.get("orderId").map(json_id).unwrap_or_default().into()),
                new_qty,
                new_price,
                ts: 0,
            })],
            // A failed modify keeps the order's terms, but the failed INTENT must not vanish (audit
            // T2): surface it as a NON-terminal advisory instead of dropping an empty vec.
            Err(exc) => vec![Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: exc.msg.into(),
                ts: 0,
            })],
        }
    }
}

impl<S: Signer, T: RestTransport> BinancePerpRest<S, T> {
    /// Audit A3 resync: recent order states (`GET /fapi/v1/allOrders`) for the post-reconnect
    /// history replay, on the short-timeout requery transport. Returns the raw JSON array.
    pub fn get_all_orders(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            "/fapi/v1/allOrders",
            "GET",
            &params,
            &self.signer,
        )
    }

    /// Audit A3 resync: recent fills (`GET /fapi/v1/userTrades`) for the post-reconnect replay.
    pub fn get_user_trades(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            "/fapi/v1/userTrades",
            "GET",
            &params,
            &self.signer,
        )
    }

    /// Audit T1: re-query the venue for the order by our idempotent client_order_id to resolve an
    /// ambiguous (timed-out) submit. `Ok(Some(id))` = live at venue, `Ok(None)` = venue confirms it
    /// never landed (-2013/-2011), `Err` = the query itself failed. Runs on `signed_requery` (the
    /// short-timeout transport) so a double-timeout can't stall the core toward ~60s.
    /// `origClientOrderId` must match the venue's OWN stored `clientOrderId` exactly, so `coid`
    /// (always the bare local id) is re-prefixed here the same way submit stamped it (task 6) —
    /// never sent bare when a link id is configured, or the venue would report the order unknown.
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
    /// optimistic OrderAccepted (never a false terminal, which would strand a phantom position).
    fn resolve_ambiguous_submit(&self, request: &OrderRequest) -> Event {
        vike_bridge_core::resolve_ambiguous_submit(
            &request.client_order_id,
            request.ts,
            self.query_order_orderid(&request.client_order_id),
        )
    }
}

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for BinancePerpRest<S, T> {
    /// Shared flow (crypto_client.py): Submitted → REST → Accepted|Rejected.
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // tif step-2 gate: a TIF this lane cannot express (Day) — or a GTD whose date fails the
        // checkable-here fapi bounds ([`deny_invalid_gtd`]) — is a LOUD terminal reject; the
        // wire is never touched, never a silent GTC (`tests/offline/tif_gate.rs` pins both gates).
        // Same discipline for a trigger source fapi cannot express (Index — no such
        // workingType): `deny_unsupported_trigger_by` (`tests/offline/trigger_gate.rs`).
        if let Some(reject) = vike_bridge_core::tif::deny_unsupported_tif(TIF_LANE, request)
            .or_else(|| deny_invalid_gtd(request, vike_model::clock::now_ms()))
            .or_else(|| vike_bridge_core::trigger::deny_unsupported_trigger_by(TIF_LANE, request))
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
                tracing::warn!(target: "vike_binance::perp", code = exc.code, msg = %exc.msg, "submit rejected");
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: exc.msg.into(),
                    ts: request.ts,
                }))
            }
        }
        events
    }

    /// `origClientOrderId` re-applies the same broker prefix submit stamped (task 6) — see
    /// [`BinancePerpRest::query_order_orderid`]'s doc for why this can't be sent bare.
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let orig =
            crate::family::order_map::binance_broker_coid(self.link_id.as_deref(), client_order_id);
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("origClientOrderId", orig)];
        match self.transport.signed(&self.base_url, PATH_ORDER, "DELETE", &params, &self.signer) {
            Ok(_) => Ok(()),
            Err(exc) if exc.code == -2011 => {
                // order not found — idempotent
                tracing::debug!(target: "vike_binance::perp", client_order_id, "cancel: order already gone");
                Ok(())
            }
            Err(exc) => Err(exc),
        }
    }

    /// Native Binance-futures batch-submit — overrides the fan-out default with /fapi/v1/batchOrders.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        BinancePerpRest::submit_batch(self, requests)
    }

    /// Native Binance-futures batch-cancel — overrides the fan-out default.
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        BinancePerpRest::cancel_batch(self, client_order_ids)
    }

    /// Native Binance-futures modify — overrides the no-op default with PUT /fapi/v1/order.
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        BinancePerpRest::modify_order(self, order, new_qty, new_price)
    }
}
