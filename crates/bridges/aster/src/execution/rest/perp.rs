//! VenueRestAsterPerp — signed USDⓈ-M perp REST submit/cancel + set-leverage. Aster's perp API is a
//! Binance-fapi fork, so the endpoint shapes, order params, and `exchangeInfo` filter shapes are
//! byte-identical to Binance's `/fapi/*`, and the request bodies are the family's
//! (`vike_binance::family::execution::signed_rest`, the owner's ruling of 2026-11-13). The
//! key divergence from the template: Binance splits order-management under `/fapi/v1/*` and
//! position/balance reads under `/fapi/v2/*`; Aster puts EVERY fapi path — order, batch, leverage,
//! positions, balance, openOrders, userTrades, exchangeInfo — under a single `/fapi/v3/*`. The
//! other two differences are (1) auth — generic over `S: Signer` here, with the concrete EIP-712
//! `SignerAster` injected by `execution/mod.rs` instead of Binance's HMAC signer, and (2) the base URL,
//! which arrives via the `base_url` field (set by `execution::mk_perp_rest` from the account's exec rows,
//! `transport::urls::AsterUrls::exec`) rather than a hardcoded const.
//!
//! fapi deltas vs spot: `/fapi/` paths, `positionSide='BOTH'` (one-way; a hedge-mode account is
//! READ and said at the start, never switched: [`VenueRestAsterPerp::check_position_mode`]), `reduceOnly` as
//! the STRING `'true'/'false'`, qty in BASE asset (no ctVal), set-leverage
//! (idempotent HTTP-200 — no benign swallow). Positions and balance are read by the reconcile
//! client (`crate::reconcile`'s `ReconcileClientAster`, `/fapi/v3/positionRisk` and `/fapi/v3/balance`;
//! `positionAmt` is ALREADY SIGNED: long > 0, short < 0); the legacy snapshot reader that once sat
//! here had no caller and is gone. The fill stream is the listenKey user-data WS
//! (`execution/user_data/perp.rs`).

use indexmap::IndexMap;
use serde_json::Value;

use vike_bridge_core::confirm::ConfirmByOrderClientId;
use vike_bridge_core::rest::VenueRest;
use vike_model::events::{Event, OrderSubmitted};
use vike_model::{Contract, OrderRequest, PositionMode, SymbolProperties};

use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// The submit, batch run, cancel, amend, leverage write, confirm and resync reads are the family's
// bodies (`vike_binance::family::execution::signed_rest`), over this client's parts and facts
// (`FamilySignedRest` below).
use vike_binance::family::execution::signed_rest::{
    FamilyRestPaths, FamilySignedRest, batch_cancel_refusal,
};
use vike_binance::transport::error_codes::cancel_says_order_unknown;

pub const PATH_ORDER: &str = "/fapi/v3/order";
/// Aster's facts for the family's perp amend: its `PATH_ORDER`, and `updateTime`, the field its order
/// rows carry (binance's; the family's history reads it on both venues).
pub const PERP_MODIFY: vike_binance::family::execution::rest_reads::PerpModifySpec =
    vike_binance::family::execution::rest_reads::PerpModifySpec {
        path: PATH_ORDER,
        time_field: "updateTime",
    };
pub const PATH_BATCH: &str = "/fapi/v3/batchOrders";
/// fapi caps batchOrders at 5 orders per request.
pub const BATCH_MAX: usize = 5;
pub const PATH_POSITIONS: &str = "/fapi/v3/positionRisk";
/// fapi live open orders (signed) — the perp twin of spot `PATH_OPEN_ORDERS`.
pub const PATH_PERP_OPEN_ORDERS: &str = "/fapi/v3/openOrders";
pub const PATH_LEVERAGE: &str = "/fapi/v3/leverage";
pub const PATH_BALANCE: &str = "/fapi/v3/balance";
/// The account's position mode (`dualSidePosition`), READ by [`VenueRestAsterPerp::check_position_mode`] and
/// never written. Under this module's `/fapi/v3/*` umbrella; UNMEASURED on aster (no answer captured: binance's
/// twin, `/fapi/v1/positionSide/dual`, answers `{"dualSidePosition": <bool>}`).
pub const PATH_POSITION_MODE: &str = "/fapi/v3/positionSide/dual";

/// What a HEDGE-mode account is told at the perp start ([`VenueRestAsterPerp::check_position_mode`]).
pub const HEDGE_MODE: &str = "aster: this account is in HEDGE position mode and this client orders ONE-WAY \
    (positionSide BOTH), so the venue will refuse its orders. The bridge changes no account's mode: choose \
    One-way Mode in Aster's futures preferences (the venue allows it only while the account holds no \
    position and no resting order), then set the account's `active` off and on";
/// Audit A3 resync: recent fills (signed) — the perp twin of spot [`crate::execution::rest::spot::PATH_SPOT_USER_TRADES`]
/// (which Aster spells `userTrades` too, unlike Binance's spot `myTrades`).
pub const PATH_PERP_USER_TRADES: &str = "/fapi/v3/userTrades";
/// Aster "Aster Code" — the one-time standing grant a builder address needs before Aster accepts a
/// nonzero `feeRate` on an order carrying that builder's code (see [`VenueRestAsterPerp::approve_builder`]
/// and the unified attribution registry's `AttributionMechanic::SignedBuilder { needs_onchain_approval:
/// true }` row for `"aster"` in `vike_venues::attribution`). Lives under the same `/fapi/v3/*`
/// umbrella as every other Aster perp/account endpoint (see this module's doc).
pub const PATH_APPROVE_BUILDER: &str = "/fapi/v3/approveBuilder";
/// PUBLIC (keyless) server clock on the FUTURES host — the one the exec client binds
/// ([`VenueRestAsterPerp`] is built over the account's exec `fapi_rest` row). Read by the startup
/// preflight through `VenueAdapterAster` in `crates/bridges/aster/src/adapter.rs`, and for a perp client's
/// `vike_bridge_core::signer::VenueClock`; aster is binance-API-shaped,
/// so the body is `{"serverTime": <epoch ms>}` exactly like [`crate::execution::rest::spot::PATH_TIME`].
///
/// ⚠ **`v1`, not `v3`** — the one Aster fapi path in this module that is not `/fapi/v3/*`.
/// Measured from the CI box on 2026-08-08 against mainnet `https://fapi.asterdex.com`:
/// `{"serverTime":1786218772315}` (three reps, ~+12 ms skew, 261-263 ms rtt). The SPOT twin
/// (`sapi` + `/api/v3/time`) answers with the same shape and the same skew, but the perp exec path
/// never talks to that host.
pub const PATH_TIME: &str = "/fapi/v1/time";

/// RiskLimits-shaped properties + base_asset + the typed [`Contract`] — the row
/// `parse_aster_perp_instruments` produces per symbol.
/// Perp deltas: market-order qty cap from MARKET_LOT_SIZE.maxQty; min notional from
/// MIN_NOTIONAL.notional (spot uses NOTIONAL/minNotional).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerpInstrument {
    pub properties: SymbolProperties,
    pub base_asset: String,
    /// What the entry itself states the instrument IS ([`perp_contract`]); `None` when it does not
    /// say (an empty `contractType`), and then `properties.asset_class` is `None` too — the class
    /// is this contract's projection, never a second reading.
    pub contract: Option<Contract>,
}

/// Aster's `deliveryDate` on an entry with NO scheduled settlement (2100-12-25 00:00 UTC), the
/// Binance-fapi convention its fork kept. Measured 2026-10-11 on mainnet `GET
/// https://fapi.asterdex.com/fapi/v3/exchangeInfo` (621 symbols): every `TRADING` and
/// `PENDING_TRADING` entry carries exactly this value, and every one of the 22 `SETTLING`
/// entries — all `contractType` `PERPETUAL` — a real date instead.
pub const NO_DELIVERY_SENTINEL_MS: i64 = 4_133_404_800_000;

/// The typed [`Contract`] ONE fapi `exchangeInfo` entry states. Aster's entry is field-for-field
/// Binance's, so the reading is the shared [`vike_binance::family::catalog::fapi_contract`] (the
/// picker's [`crate::catalog::parse_perp`] reads the same function, so the picker and the order
/// path cannot classify one row two ways): `contractType` for the kind, `marginAsset` against the
/// entry's own `quoteAsset`/`baseAsset` for the margining, `deliveryDate` for a dated contract.
/// The 2026-10-11 measurement above: 616 `PERPETUAL` entries, all with `marginAsset ==
/// quoteAsset` (597 `USDT`, 17 `USD1`, 2 `U`), so every live Aster perp is a `Linear`
/// [`Contract::Perp`]; the 5 `PENDING_TRADING` entries publish an EMPTY `contractType` and stay
/// `None`.
///
/// ⚠ **One Aster-own delta.** Binance marks a delisting perp with its own word
/// (`PERPETUAL_DELIVERING`) and the shared reading takes `settling_ms` from that word only. Aster
/// keeps `PERPETUAL` and publishes the settlement in `deliveryDate` instead (its 22 `SETTLING`
/// entries), so a `PERPETUAL` perp whose `deliveryDate` is not [`NO_DELIVERY_SENTINEL_MS`] gets it
/// as `settling_ms` here. A perp still has no expiry; this is the delisting instant.
pub fn perp_contract(entry: &Value) -> Option<Contract> {
    let mut contract = vike_binance::family::catalog::fapi_contract(entry)?;
    if let Contract::Perp(spec) = &mut contract
        && spec.settling_ms.is_none()
    {
        spec.settling_ms = entry
            .get("deliveryDate")
            .and_then(Value::as_i64)
            .filter(|&ms| ms != NO_DELIVERY_SENTINEL_MS);
    }
    Some(contract)
}

/// Every fapi `exchangeInfo` entry keyed by its upper-cased exchange symbol: the grid off the shared
/// [`vike_binance::family::catalog::perp_grid`], the class as the projection of [`perp_contract`]
/// (never the symbol text, never a blanket `CryptoPerp`: an entry that does not say stays
/// UNCLASSIFIED — see `vike_model::SymbolProperties::asset_class` for why a guess is worse than an
/// absence). Everything `/exchangeInfo` does not report stays ABSENT: Aster perps are 1:1 with the
/// base asset (no contract multiplier), the grid is flat, and Aster declares no venue taker hold.
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
        let contract = perp_contract(entry);
        out.insert(
            symbol,
            PerpInstrument {
                properties: SymbolProperties {
                    asset_class: contract.as_ref().map(Contract::class),
                    ..vike_binance::family::catalog::perp_grid(entry)
                },
                base_asset: entry
                    .get("baseAsset")
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string(),
                contract,
            },
        );
    }
    out
}

/// The REST half of the live perp client (ACK-only); fills come from the listenKey WS.
pub struct VenueRestAsterPerp<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub leverage: f64,
    /// Unified cross-venue attribution (task 8): the Aster Code `(builder address, feeRate)` pair
    /// stamped onto every order when configured. `None` (the default at every start lacking
    /// `ASTER_BUILDER_CODE`) means `build_order_params` emits neither `builder` nor `feeRate` —
    /// byte-identical to every order shape before this field existed.
    pub builder: Option<(String, String)>,
}

impl<S: Signer, T: RestTransport> VenueRestAsterPerp<S, T> {
    /// **The account's position mode, read at the perp start and NEVER written** (master plan P3-G: the
    /// bridge used neither to check nor to switch it). Every order this client sends is ONE-WAY
    /// (`positionSide BOTH`, the family's order builder), which a HEDGE-mode account refuses (binance's
    /// `-4061`; aster's code is unmeasured), so a hedge account is said at `error` with the hand fix
    /// ([`HEDGE_MODE`]), an unreadable answer at `warn`, and the start goes on either way. No write, unlike
    /// binance's demo-only switch: aster's only order-capable key pair is a live (mainnet) one, and the
    /// owner's ruling of 2026-10-26 allows no exchange write on a live account.
    pub fn check_position_mode(&self) -> PositionMode {
        let read = self.transport.signed_requery(
            &self.base_url,
            PATH_POSITION_MODE,
            "GET",
            &[],
            &self.signer,
        );
        let mode = read
            .as_ref()
            .map_or(PositionMode::Unknown, vike_binance::account::modes::position_mode);
        let host = self.base_url.as_str();
        match (mode, &read) {
            (PositionMode::Hedge, _) => {
                tracing::error!(target: "vike_aster::execution::rest::perp", host, "{HEDGE_MODE}")
            }
            (PositionMode::OneWay, _) => {
                tracing::debug!(target: "vike_aster::execution::rest::perp", host, "position mode: one-way, the mode this client orders under")
            }
            (PositionMode::Unknown, Ok(body)) => tracing::warn!(
                target: "vike_aster::execution::rest::perp", host,
                "position mode not read ({PATH_POSITION_MODE} answered {body}); the start goes on as before"
            ),
            (PositionMode::Unknown, Err(e)) => tracing::warn!(
                target: "vike_aster::execution::rest::perp", host, code = e.code, msg = %e.msg,
                "position mode not read (GET {PATH_POSITION_MODE} failed); the start goes on as before"
            ),
        }
        mode
    }

    /// POST /fapi/v3/leverage {symbol, leverage:int}. Aster change-leverage is
    /// idempotent (HTTP-200 even when already at target) — NO benign-error swallow.
    pub fn set_leverage(&self) -> Result<(), VenueApiError> {
        vike_binance::family::execution::signed_rest::set_leverage(self, self.leverage)
    }

    /// Pure, golden-gated. Delegates to the shared, byte-identical
    /// [`vike_binance::family::execution::order_map::build_perp_order_params`] (mappers rung), passing this
    /// client's `symbol`/`properties`: positionSide BOTH (one-way); reduceOnly as STRING;
    /// `order_type:"stop"` maps to a native STOP_MARKET conditional (additive over the golden
    /// limit/market shapes).
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        let mut params = vike_binance::family::execution::order_map::build_perp_order_params(
            request,
            &self.symbol,
            &self.properties,
            crate::transport::urls::VENUE, // aster's tif row is still Ignored{GTC} — bytes unchanged
            // Aster does not use the Binance broker-prefix order client id mechanic (task 6) — see the spot
            // builder's identical note. Aster's OWN attribution mechanic (Aster Code) is the
            // `self.builder` push below, not this shared family builder's `link_id`.
            None,
        );
        // Unified cross-venue attribution (task 8): Aster Code is a builder address + feeRate pair
        // on the order params (the HL-style builder model on this Binance-fork REST API), NOT a
        // order client id prefix. `None` (unconfigured) pushes neither key — byte-identical.
        if let Some((addr, rate)) = &self.builder {
            params.push(("builder", addr.clone()));
            params.push(("feeRate", rate.clone()));
        }
        params
    }

    /// `POST /fapi/v3/approveBuilder {builder, feeRate}` — the one-time standing grant Aster Code
    /// needs before a NONZERO `feeRate` is accepted on an order carrying `builder`'s code (see
    /// [`PATH_APPROVE_BUILDER`]'s doc). Run once per `(account, builder, rate)`, not on every
    /// submit; an attribution-only start (feeRate `"0"`, this repo's default) may not need this at
    /// all. Unlike Hyperliquid's separate user-signed EIP-712 action, Aster has ONE signing scheme
    /// for every request (`SignerAster`, `transport/sign.rs`'s module doc) — this reuses the SAME
    /// signed-REST seam every order/leverage call goes through, no bespoke master-wallet plumbing.
    ///
    /// ⚠ UNVERIFIED live: the param names (`builder`/`feeRate`) and path mirror this crate's
    /// existing `/fapi/v3/*` umbrella convention and the task-8 brief; Aster publishes no public
    /// spec doc this port could pin against at write time. A real round-trip is owed before this is
    /// trusted with a live grant — and it can be a TESTNET one: `crates/bridges/aster/src/transport/urls.rs`'s
    /// `AsterUrls::exec` routes `AccountTier::Demo` at a working fapi host, so the only thing between this
    /// doc and a proof that risks nothing is a `demo`-tier aster account holding the testnet
    /// `USER` + `PRIVATE_KEY` pair `crates/bridges/aster/src/transport/sign.rs`'s `load_aster_credentials`
    /// reads. ⚠ This note used to
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

    /// Native batch-submit (POST /fapi/v3/batchOrders — `batchOrders` is a JSON-array string param,
    /// ≤5/req). Emits OrderSubmitted per order, then per-order Accepted|Rejected from the response
    /// array, matched by index ([`vike_binance::family::execution::signed_rest::send_batch_run`], chunked at
    /// BATCH_MAX). RUST-NATIVE, no Python twin.
    pub fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        let mut events: Vec<Event> = requests
            .iter()
            .map(|r| {
                Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: r.order_client_id.clone(),
                    timestamp: r.timestamp,
                })
            })
            .collect();
        // Trigger-source gate, per order (the same floor as `submit_order`'s): a denied order is
        // partitioned out of the wire chunk entirely; the supported rest still batch as before.
        let mut sendable: Vec<&OrderRequest> = Vec::with_capacity(requests.len());
        for r in requests {
            match self.floor(r) {
                Some(reject) => events.push(reject),
                None => sendable.push(r),
            }
        }
        vike_binance::family::execution::signed_rest::send_batch_run(
            self,
            &mut events,
            &sendable,
            BATCH_MAX,
        );
        events
    }

    /// Native batch-cancel (DELETE /fapi/v3/batchOrders, origClientOrderIdList = JSON array, ≤5).
    /// Per-order "already gone" (`-2011` "Unknown order sent.") rides the response array (non-fatal); any
    /// other element's code, and a whole-request/network failure, returns the first error, so a refused
    /// element never vanishes. Chunks at BATCH_MAX. RUST-NATIVE.
    pub fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        for chunk in order_client_ids.chunks(BATCH_MAX) {
            let params: Vec<(&str, String)> = vec![
                ("symbol", self.symbol.clone()),
                ("origClientOrderIdList", serde_json::to_string(&chunk).expect("array")),
            ];
            match self.transport.signed(&self.base_url, PATH_BATCH, "DELETE", &params, &self.signer)
            {
                Ok(response) => {
                    let list = response.as_array().map(Vec::as_slice).unwrap_or_default();
                    let refused = list
                        .iter()
                        .filter_map(batch_cancel_refusal)
                        .find(|e| !cancel_says_order_unknown(e));
                    if let Some(e) = refused {
                        first_err.get_or_insert(e);
                    }
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Native modify (PUT /fapi/v3/order): Aster's fapi modify (like Binance's) requires `side` PLUS
    /// both `quantity` and `price` and is LIMIT-only. The family's one perp amend,
    /// [`vike_binance::family::execution::signed_rest::modify_perp`], with aster's facts ([`PERP_MODIFY`]) and the
    /// bare `origClientOrderId` (aster stamps no prefix).
    pub fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        vike_binance::family::execution::signed_rest::modify_perp(
            self,
            &PERP_MODIFY,
            &self.properties,
            order,
            new_qty,
            new_price,
        )
    }
}

/// Aster perp's parts and facts for the family's signed REST bodies: every path under `/fapi/v3`,
/// the bare order client id (no broker stamp), ONE bridge floor — a trigger source aster's fapi
/// cannot express (Index: no such `workingType` on aster's fork either, `vike_bridge_core::trigger`'s
/// module doc) is a LOUD terminal reject with the wire never touched, never a silent
/// `CONTRACT_PRICE` substitution (`tests/offline/trigger_gate.rs`) — no refusal cause, and its own
/// log target. Binance's TIF, GTD and kind floors are not aster's.
impl<S: Signer, T: RestTransport> FamilySignedRest for VenueRestAsterPerp<S, T> {
    const VENUE: &'static str = crate::transport::urls::VENUE;
    const PATHS: FamilyRestPaths = FamilyRestPaths {
        order: PATH_ORDER,
        all_orders: "/fapi/v3/allOrders",
        trades: PATH_PERP_USER_TRADES,
        leverage: PATH_LEVERAGE,
        batch: PATH_BATCH,
    };

    fn transport(&self) -> &dyn RestTransport {
        &self.transport
    }
    fn signer(&self) -> &dyn Signer {
        &self.signer
    }
    fn base_url(&self) -> &str {
        &self.base_url
    }
    fn symbol(&self) -> &str {
        &self.symbol
    }
    fn order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        self.build_order_params(request)
    }
    fn floor(&self, request: &OrderRequest) -> Option<Event> {
        vike_bridge_core::trigger::deny_unsupported_trigger_by(
            crate::transport::urls::VENUE,
            request,
        )
    }
    fn log_refused(&self, order_client_id: &str, exc: &VenueApiError) {
        tracing::warn!(target: "vike_aster::execution::rest::perp", order_client_id, code = exc.code, msg = %exc.msg, "submit rejected");
    }
}

impl<S: Signer + Send, T: RestTransport + Send> ConfirmByOrderClientId
    for VenueRestAsterPerp<S, T>
{
    const LOOKUP_WINDOW: Option<std::time::Duration> = None; // none stated by the venue

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        vike_binance::family::execution::signed_rest::confirm_when(
            self,
            order_client_id,
            ts,
            absent_rejects,
        )
    }
}

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for VenueRestAsterPerp<S, T> {
    /// The family's submit ([`vike_binance::family::execution::signed_rest::submit_order`]): Submitted → the
    /// trigger floor → REST → Accepted|Rejected. The returned events go into the core ingest — never
    /// applied locally.
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        vike_binance::family::execution::signed_rest::submit_order(self, request)
    }

    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        vike_binance::family::execution::signed_rest::cancel_by_order_client_id(
            self,
            order_client_id,
        )
    }

    /// Native Aster-futures batch-submit — overrides the fan-out default with /fapi/v3/batchOrders.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        VenueRestAsterPerp::submit_batch(self, requests)
    }

    /// Native Aster-futures batch-cancel — overrides the fan-out default.
    fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        VenueRestAsterPerp::cancel_batch(self, order_client_ids)
    }

    /// ACTIVELY confirm one order by its client id (the by-order-client-id status GET): found -> `OrderAccepted`
    /// (+ the terminal when the venue already closed it), absent -> the "venue confirms absent"
    /// reject, a FAILED query -> NOTHING (the watchdog ladder and reconcile decide). This trait door
    /// assumes the submit was fully dispatched; the exec thread's confirm closure goes through
    /// [`ConfirmByOrderClientId::confirm_when`] with the real answer.
    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        self.confirm_when(order_client_id, ts, true)
    }

    /// Native Aster-futures modify — overrides the no-op default with PUT /fapi/v3/order.
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        VenueRestAsterPerp::modify_order(self, order, new_qty, new_price)
    }
}

/// The param-building pins, and the pieces `crate::execution::rest::spot`'s twin module shares with them.
#[cfg(test)]
pub(crate) mod order_param_tests {
    use super::*;
    use vike_model::OrderRequest;

    /// A concrete signer over fake keys, just to own a client for pure param-building (no network).
    pub(crate) fn signer() -> vike_binance::family::signer::BinanceHmacSigner {
        let creds = vike_secrets::credentials::Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        vike_binance::family::signer::BinanceHmacSigner::new(&creds, || 0)
    }

    /// A limit order `c-1` of 0.01 BTCUSDT at 50000 on `side` (`+1` buy, `-1` sell).
    pub(crate) fn limit(side: i32) -> OrderRequest {
        OrderRequest {
            order_client_id: "c-1".to_string(),
            venue: "aster".to_string(),
            symbol: "BTCUSDT".to_string(),
            side,
            qty: 0.01,
            order_type: vike_model::OrderType::Limit,
            price: Some(50000.0),
            ..Default::default()
        }
    }

    /// Aster Code stamps `builder`+`feeRate` on the params `with` a builder pair configured, and
    /// NEITHER key `without` one (the byte-identical-when-absent property).
    pub(crate) fn assert_builder_stamped(with: &[(&str, String)], without: &[(&str, String)]) {
        assert!(with.iter().any(|(k, v)| *k == "builder" && v == "0xbuilder"));
        assert!(with.iter().any(|(k, v)| *k == "feeRate" && v == "0"));
        assert!(without.iter().all(|(k, _)| *k != "builder" && *k != "feeRate"));
    }

    fn rest() -> VenueRestAsterPerp<
        vike_binance::family::signer::BinanceHmacSigner,
        vike_bridge_core::transport::UreqTransport,
    > {
        VenueRestAsterPerp {
            signer: signer(),
            transport: vike_bridge_core::transport::UreqTransport::new("aster"),
            base_url: "https://fapi.asterdex-testnet.com".into(),
            symbol: "BTCUSDT".into(),
            properties: vike_model::SymbolProperties::default(),
            leverage: 1.0,
            builder: None,
        }
    }

    #[test]
    fn perp_limit_params_have_position_side_and_reduce_only() {
        let p = rest().build_order_params(&limit(-1));
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
        let mut with_builder = rest();
        with_builder.builder = Some(("0xbuilder".to_string(), "0".to_string()));
        let with = with_builder.build_order_params(&limit(1));
        assert_builder_stamped(&with, &rest().build_order_params(&limit(1)));
    }
}
