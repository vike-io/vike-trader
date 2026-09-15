//! AsterPerpRest — signed USDⓈ-M perp REST submit/cancel/reconcile + set-leverage. Ported from
//! `vike-binance`'s `perp.rs`: Aster's perp API is a Binance-fapi fork, so the endpoint shapes,
//! order params, and `exchangeInfo` filter shapes are byte-identical to Binance's `/fapi/*`. The
//! key divergence from the template: Binance splits order-management under `/fapi/v1/*` and
//! position/balance reads under `/fapi/v2/*`; Aster puts EVERY fapi path — order, batch, leverage,
//! positions, balance, openOrders, userTrades, exchangeInfo — under a single `/fapi/v3/*`. The
//! other two differences are (1) auth — generic over `S: Signer` here, with the concrete EIP-712
//! `AsterSigner` injected by `exec.rs` instead of Binance's HMAC signer, and (2) the base URL,
//! which arrives via the `base_url` field (set by `exec::mk_perp_rest` from `urls::urls_for`)
//! rather than a hardcoded const.
//!
//! fapi deltas vs spot: `/fapi/` paths, `positionSide='BOTH'` (one-way; hedge mode is a future
//! gap), `reduceOnly` as the STRING `'true'/'false'`, qty in BASE asset (no ctVal), set-leverage
//! (idempotent HTTP-200 — no benign swallow), and a `/fapi/v3/positionRisk` signed-position
//! reconcile (`positionAmt` is ALREADY SIGNED: long > 0, short < 0). The fill stream is the
//! listenKey user-data WS (a later task adds `perp_user_data.rs`).

use indexmap::IndexMap;
use serde_json::Value;

use vike_bridge_core::rest::VenueRest;
use vike_exec::{ManagedOrder, ReconcileSnapshot};
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted};
use vike_model::{OrderRequest, SymbolProperties};

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// `json_id` (the numeric-id coercion) is shared, byte-identical between binance/aster — one copy
// lives in the shared `vike_binance::family::order_map` (mappers rung).
use vike_binance::family::order_map::json_id;

pub const PATH_ORDER: &str = "/fapi/v3/order";
pub const PATH_BATCH: &str = "/fapi/v3/batchOrders";
/// fapi caps batchOrders at 5 orders per request.
pub const BATCH_MAX: usize = 5;
pub const PATH_POSITIONS: &str = "/fapi/v3/positionRisk";
/// fapi live open orders (signed) — the perp twin of spot `PATH_OPEN_ORDERS`.
pub const PATH_PERP_OPEN_ORDERS: &str = "/fapi/v3/openOrders";
pub const PATH_LEVERAGE: &str = "/fapi/v3/leverage";
pub const PATH_BALANCE: &str = "/fapi/v3/balance";
/// Audit A3 resync: recent fills (signed) — the perp twin of spot [`crate::spot::PATH_SPOT_USER_TRADES`]
/// (which Aster spells `userTrades` too, unlike Binance's spot `myTrades`).
pub const PATH_PERP_USER_TRADES: &str = "/fapi/v3/userTrades";
/// Aster "Aster Code" — the one-time standing grant a builder address needs before Aster accepts a
/// nonzero `feeRate` on an order carrying that builder's code (see [`AsterPerpRest::approve_builder`]
/// and the unified attribution registry's `AttributionMechanic::SignedBuilder { needs_onchain_approval:
/// true }` row for `"aster"` in `vike_model::attribution`). Lives under the same `/fapi/v3/*`
/// umbrella as every other Aster perp/account endpoint (see this module's doc).
pub const PATH_APPROVE_BUILDER: &str = "/fapi/v3/approveBuilder";
/// PUBLIC (keyless) server clock on the FUTURES host — the one the exec client binds
/// ([`AsterPerpRest`] is built over `urls_for(env).fapi_rest`). Read by the startup preflight
/// through `vike_mount::server_time`'s aster row; aster is binance-API-shaped, so the body is
/// `{"serverTime": <epoch ms>}` exactly like [`crate::spot::PATH_TIME`].
///
/// ⚠ **`v1`, not `v3`** — the one Aster fapi path in this module that is not `/fapi/v3/*`.
/// Measured from the CI box on 2026-08-08 against mainnet `https://fapi.asterdex.com`:
/// `{"serverTime":1786218772315}` (three reps, ~+12 ms skew, 261-263 ms rtt). The SPOT twin
/// (`sapi` + `/api/v3/time`) answers with the same shape and the same skew, but the perp exec path
/// never talks to that host.
pub const PATH_TIME: &str = "/fapi/v1/time";

/// RiskLimits-shaped properties + base_asset — twin of `parse_aster_perp_instruments`.
/// Perp deltas: market-order qty cap from MARKET_LOT_SIZE.maxQty; min notional from
/// MIN_NOTIONAL.notional (spot uses NOTIONAL/minNotional).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerpInstrument {
    pub properties: SymbolProperties,
    pub base_asset: String,
}

pub fn parse_aster_perp_instruments(payload: &Value) -> IndexMap<String, PerpInstrument> {
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
                    // Everything `/exchangeInfo` does not report stays ABSENT: Aster perps are
                    // USDT-margined and 1:1 with the base asset (no contract multiplier, = 1.0),
                    // the grid is flat (ONE `tickSize`, no tiers), and Aster declares no venue
                    // taker hold. Functional-update syntax deliberately — an exhaustive literal
                    // makes every new `SymbolProperties` field a compile error in 14 crates.
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

/// Venue face of [`vike_binance::family::order_map::map_perp_open_order`] (mappers rung) bound to
/// the `"aster"` venue key (`crate::urls::VENUE`). Keeps its 1-arg signature so the reconcile call
/// site ([`AsterPerpRest::fetch_open_orders`]) calls it unchanged; the shared body maps ONE fapi
/// `/openOrders` row → a reconcile-seeded ACCEPTED [`ManagedOrder`].
pub fn map_perp_open_order(o: &Value) -> ManagedOrder {
    vike_binance::family::order_map::map_perp_open_order(o, crate::urls::VENUE)
}

/// The REST half of the live perp client (ACK-only); fills come from the listenKey WS.
pub struct AsterPerpRest<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub leverage: f64,
    /// Unified cross-venue attribution (task 8): the Aster Code `(builder address, feeRate)` pair
    /// stamped onto every order when configured. `None` (the default at every mount lacking
    /// `ASTER_BUILDER_CODE`) means `build_order_params` emits neither `builder` nor `feeRate` —
    /// byte-identical to every order shape before this field existed.
    pub builder: Option<(String, String)>,
}

impl<S: Signer, T: RestTransport> AsterPerpRest<S, T> {
    /// POST /fapi/v3/leverage {symbol, leverage:int}. Aster change-leverage is
    /// idempotent (HTTP-200 even when already at target) — NO benign-error swallow.
    pub fn set_leverage(&self) -> Result<(), VenueApiError> {
        let params: Vec<(&str, String)> = vec![
            ("symbol", self.symbol.clone()),
            ("leverage", format!("{}", self.leverage as i64)),
        ];
        self.transport
            .signed(&self.base_url, PATH_LEVERAGE, "POST", &params, &self.signer)
            .map(|_| ())
    }

    /// Pure, golden-gated. Delegates to the shared, byte-identical
    /// [`vike_binance::family::order_map::build_perp_order_params`] (mappers rung), passing this
    /// client's `symbol`/`properties`: positionSide BOTH (one-way); reduceOnly as STRING;
    /// `order_type:"stop"` maps to a native STOP_MARKET conditional (additive over the golden
    /// limit/market shapes).
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        let mut params = vike_binance::family::order_map::build_perp_order_params(
            request,
            &self.symbol,
            &self.properties,
            crate::urls::VENUE, // aster's tif row is still Ignored{GTC} — bytes unchanged
            // Aster does not use the Binance broker-prefix coid mechanic (task 6) — see the spot
            // builder's identical note. Aster's OWN attribution mechanic (Aster Code) is the
            // `self.builder` push below, not this shared family builder's `link_id`.
            None,
        );
        // Unified cross-venue attribution (task 8): Aster Code is a builder address + feeRate pair
        // on the order params (the HL-style builder model on this Binance-fork REST API), NOT a
        // coid prefix. `None` (unconfigured) pushes neither key — byte-identical.
        if let Some((addr, rate)) = &self.builder {
            params.push(("builder", addr.clone()));
            params.push(("feeRate", rate.clone()));
        }
        params
    }

    /// `POST /fapi/v3/approveBuilder {builder, feeRate}` — the one-time standing grant Aster Code
    /// needs before a NONZERO `feeRate` is accepted on an order carrying `builder`'s code (see
    /// [`PATH_APPROVE_BUILDER`]'s doc). Run once per `(account, builder, rate)`, not on every
    /// submit; an attribution-only mount (feeRate `"0"`, this repo's default) may not need this at
    /// all. Unlike Hyperliquid's separate user-signed EIP-712 action, Aster has ONE signing scheme
    /// for every request (`AsterSigner`, `signing.rs`'s module doc) — this reuses the SAME
    /// signed-REST seam every order/leverage call goes through, no bespoke master-wallet plumbing.
    ///
    /// ⚠ UNVERIFIED live: the param names (`builder`/`feeRate`) and path mirror this crate's
    /// existing `/fapi/v3/*` umbrella convention and the task-8 brief; Aster publishes no public
    /// spec doc this port could pin against at write time. A real round-trip is owed before this is
    /// trusted with a live grant — and it can be a TESTNET one: `crates/bridges/aster/src/urls.rs`'s
    /// `urls_for` routes `Environment::Demo` at a working fapi host, so the only thing between this
    /// doc and a proof that risks nothing is the TESTNET credential pair
    /// `crates/bridges/aster/src/signing.rs`'s `load_aster_credentials` reads. ⚠ This note used to
    /// assert the opposite, citing a module doc that never said it, and so sent the next engineer to
    /// a REAL account to verify a standing grant —
    /// `crates/bridges/aster/tests/testnet_claim_gate.rs` is why it cannot come back.
    pub fn approve_builder(
        &self,
        builder: &str,
        max_fee_rate: &str,
    ) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("builder", builder.to_string()), ("feeRate", max_fee_rate.to_string())];
        self.transport.signed(&self.base_url, PATH_APPROVE_BUILDER, "POST", &params, &self.signer)
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

    /// Signed GET /fapi/v3/openOrders {symbol} → the venue's resting orders as reconcile-seeded
    /// [`ManagedOrder`]s (the perp twin of spot `connect`'s open-order fetch — feeds
    /// `apply_snapshot`'s stale-order reap). Best-effort by contract: a REST hiccup returns
    /// `Vec::new()` (skip the order-fetch, KEEP the position snapshot) rather than failing the
    /// whole reconcile — matching the venue's "REST hiccup ⇒ skip tick" convention. This is the
    /// reconcile path only, already off the fold.
    fn fetch_open_orders(&self) -> Vec<ManagedOrder> {
        let Ok(raw) = self.transport.signed(
            &self.base_url,
            PATH_PERP_OPEN_ORDERS,
            "GET",
            &[("symbol", self.symbol.clone())],
            &self.signer,
        ) else {
            return Vec::new();
        };
        raw.as_array().unwrap_or(&vec![]).iter().map(map_perp_open_order).collect()
    }

    /// GET /fapi/v3/positionRisk {symbol}: positionAmt is ALREADY SIGNED base qty.
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
        // (signed_qty, avg, mark, side)
        let mut legs: Vec<(f64, f64, f64, String)> = Vec::new();
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
            ));
        }
        if legs.is_empty() {
            // flat: one zero BOTH row (unchanged from pre-perp shape)
            return Ok(ReconcileSnapshot {
                positions: vec![(self.symbol.clone(), 0.0)],
                open_orders,
                position_avg_px: vec![(self.symbol.clone(), 0.0)],
                position_mark_px: vec![(self.symbol.clone(), 0.0)],
                position_sides: Vec::new(),
                balance: bal,
                // Margin-mode step-2 note: aster's SNAPSHOT builder does not parse `marginType`
                // yet (this legacy builder predates the family recon rung, which DOES parse it —
                // aster's `ReconClient` path inherits that). Empty = carry priors forward.
                position_margin: Vec::new(),
            });
        }
        let hedge = legs.iter().any(|(_, _, _, side)| side != "BOTH");
        Ok(ReconcileSnapshot {
            positions: legs.iter().map(|(q, ..)| (self.symbol.clone(), *q)).collect(),
            open_orders,
            position_avg_px: legs.iter().map(|(_, a, ..)| (self.symbol.clone(), *a)).collect(),
            position_mark_px: legs.iter().map(|(_, _, m, _)| (self.symbol.clone(), *m)).collect(),
            position_sides: if hedge {
                legs.iter().map(|(.., sd)| (self.symbol.clone(), sd.clone())).collect()
            } else {
                Vec::new()
            },
            balance: bal,
            position_margin: Vec::new(),
        })
    }

    /// Native batch-submit (POST /fapi/v3/batchOrders — `batchOrders` is a JSON-array string param,
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
        // Trigger-source gate, per order (same law as `submit_order`): a denied order is
        // partitioned out of the wire chunk entirely; the supported rest still batch as before.
        let mut sendable: Vec<&OrderRequest> = Vec::with_capacity(requests.len());
        for r in requests {
            match vike_bridge_core::trigger::deny_unsupported_trigger_by(crate::urls::VENUE, r) {
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
            match self.transport.signed(&self.base_url, PATH_BATCH, "POST", &params, &self.signer) {
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

    /// Native batch-cancel (DELETE /fapi/v3/batchOrders, origClientOrderIdList = JSON array, ≤5).
    /// Per-order "already gone" rides the response array (non-fatal); a whole-request/network
    /// failure returns the first error. Chunks at BATCH_MAX. RUST-NATIVE.
    pub fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        for chunk in client_order_ids.chunks(BATCH_MAX) {
            let params: Vec<(&str, String)> = vec![
                ("symbol", self.symbol.clone()),
                ("origClientOrderIdList", serde_json::to_string(&chunk).expect("array")),
            ];
            if let Err(e) =
                self.transport.signed(&self.base_url, PATH_BATCH, "DELETE", &params, &self.signer)
            {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Native modify (PUT /fapi/v3/order): Aster's fapi modify (like Binance's) requires `side`
    /// PLUS both `quantity` and `price` and is LIMIT-only, so an unchanged field falls back to the
    /// resting `order`'s value (this is exactly why the modify seam passes the whole order, not
    /// just its id). Returns `[OrderModified]` on the venue's ack; a modify-rejected advisory on
    /// failure (the order keeps its terms). RUST-NATIVE, no Python twin.
    pub fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let qty = new_qty.unwrap_or(order.qty);
        let price = new_price.or(order.price).unwrap_or(0.0);
        let params: Vec<(&str, String)> = vec![
            ("symbol", self.symbol.clone()),
            ("side", if order.side > 0 { "BUY" } else { "SELL" }.to_string()),
            ("origClientOrderId", order.client_order_id.clone()),
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

impl<S: Signer, T: RestTransport> AsterPerpRest<S, T> {
    /// Audit A3 resync: recent order states (`GET /fapi/v3/allOrders`) for the post-reconnect
    /// history replay, on the short-timeout requery transport. Returns the raw JSON array.
    pub fn get_all_orders(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            "/fapi/v3/allOrders",
            "GET",
            &params,
            &self.signer,
        )
    }

    /// Audit A3 resync: recent fills (`GET /fapi/v3/userTrades`) for the post-reconnect replay.
    pub fn get_user_trades(&self, limit: u32) -> Result<serde_json::Value, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("limit", limit.to_string())];
        self.transport.signed_requery(
            &self.base_url,
            PATH_PERP_USER_TRADES,
            "GET",
            &params,
            &self.signer,
        )
    }

    /// Audit T1: re-query the venue for the order by our idempotent client_order_id to resolve an
    /// ambiguous (timed-out) submit. `Ok(Some(id))` = live at venue, `Ok(None)` = venue confirms it
    /// never landed (-2013/-2011), `Err` = the query itself failed. Runs on `signed_requery` (the
    /// short-timeout transport) so a double-timeout can't stall the core toward ~60s.
    fn query_order_orderid(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("origClientOrderId", coid.to_string())];
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

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for AsterPerpRest<S, T> {
    /// Shared flow: Submitted → REST → Accepted|Rejected. The returned events go into the core
    /// ingest — never applied locally.
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // Trigger-source gate: Index has no fapi index `workingType` on aster's fork either (the
        // same law as binance-perp — see `vike_bridge_core::trigger`'s module doc for the API-docs
        // evidence) — a LOUD terminal reject, wire never touched, never a silent CONTRACT_PRICE
        // substitution (`tests/trigger_gate.rs`).
        if let Some(reject) =
            vike_bridge_core::trigger::deny_unsupported_trigger_by(crate::urls::VENUE, request)
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
                tracing::warn!(target: "vike_aster::perp", code = exc.code, msg = %exc.msg, "submit rejected");
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: exc.msg.into(),
                    ts: request.ts,
                }))
            }
        }
        events
    }

    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let params: Vec<(&str, String)> = vec![
            ("symbol", self.symbol.clone()),
            ("origClientOrderId", client_order_id.to_string()),
        ];
        match self.transport.signed(&self.base_url, PATH_ORDER, "DELETE", &params, &self.signer) {
            Ok(_) => Ok(()),
            Err(exc) if exc.code == -2011 => {
                // order not found — idempotent
                tracing::debug!(target: "vike_aster::perp", client_order_id, "cancel: order already gone");
                Ok(())
            }
            Err(exc) => Err(exc),
        }
    }

    /// Native Aster-futures batch-submit — overrides the fan-out default with /fapi/v3/batchOrders.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        AsterPerpRest::submit_batch(self, requests)
    }

    /// Native Aster-futures batch-cancel — overrides the fan-out default.
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        AsterPerpRest::cancel_batch(self, client_order_ids)
    }

    /// Native Aster-futures modify — overrides the no-op default with PUT /fapi/v3/order.
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        AsterPerpRest::modify_order(self, order, new_qty, new_price)
    }
}

#[cfg(test)]
mod order_param_tests {
    use super::*;
    use vike_model::OrderRequest;

    fn rest() -> AsterPerpRest<vike_bridge_core::BinanceHmacSigner, vike_bridge_core::UreqTransport>
    {
        // A concrete signer/transport just to own the struct for pure param-building (no network).
        let creds = vike_bridge_core::Credentials {
            api_key: "k".into(),
            api_secret: "s".into(),
            passphrase: None,
        };
        AsterPerpRest {
            signer: vike_bridge_core::BinanceHmacSigner::new(&creds, || 0),
            transport: vike_bridge_core::UreqTransport::new("aster"),
            base_url: "https://fapi.asterdex-testnet.com".into(),
            symbol: "BTCUSDT".into(),
            properties: vike_model::SymbolProperties::default(),
            leverage: 1.0,
            builder: None,
        }
    }

    #[test]
    fn perp_limit_params_have_position_side_and_reduce_only() {
        let req = OrderRequest {
            client_order_id: "c-1".to_string(),
            venue: "aster".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: -1, // -1 sell / +1 buy
            qty: 0.01,
            order_type: "limit".to_string(),
            price: Some(50000.0),
            ..Default::default()
        };
        let p = rest().build_order_params(&req);
        let m: std::collections::HashMap<_, _> = p.into_iter().collect();
        assert_eq!(m.get("side").map(String::as_str), Some("SELL"));
        assert_eq!(m.get("positionSide").map(String::as_str), Some("BOTH"));
        assert_eq!(m.get("reduceOnly").map(String::as_str), Some("false"));
        assert_eq!(m.get("type").map(String::as_str), Some("LIMIT"));
    }

    /// Unified cross-venue attribution (task 8): Aster Code stamps `builder`+`feeRate` on the order
    /// params when configured, and stamps NEITHER key when unset — the byte-identical-when-absent
    /// property every venue's attribution mechanic must hold.
    #[test]
    fn aster_order_carries_builder_and_feerate_when_configured() {
        let req = OrderRequest {
            client_order_id: "c-1".to_string(),
            venue: "aster".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 0.01,
            order_type: "limit".to_string(),
            price: Some(50000.0),
            ..Default::default()
        };

        let mut with_builder = rest();
        with_builder.builder = Some(("0xbuilder".to_string(), "0".to_string()));
        let with = with_builder.build_order_params(&req);
        assert!(with.iter().any(|(k, v)| *k == "builder" && v == "0xbuilder"));
        assert!(with.iter().any(|(k, v)| *k == "feeRate" && v == "0"));

        let without = rest().build_order_params(&req);
        assert!(without.iter().all(|(k, _)| *k != "builder" && *k != "feeRate"));
    }
}
