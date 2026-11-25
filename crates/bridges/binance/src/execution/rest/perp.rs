//! VenueRestBinancePerp — signed fapi (USDS-M futures) submit/cancel + set-leverage.
//! Exact port of `exec/binance/perp_client.py` + `perp_instruments.py`.
//!
//! fapi deltas vs spot: /fapi/ paths, positionSide='BOTH' (one-way; hedge is 5f),
//! reduceOnly as the STRING 'true'/'false', qty in BASE asset (no ctVal), set-leverage
//! (idempotent HTTP-200 — no benign swallow). The fill stream is the
//! listenKey user-data WS (`execution/user_data/perp.rs`).
//!
//! Positions and balance are read through `crate::reconcile`'s `ReconcileClientBinance`; this client
//! holds no reconcile read.

use indexmap::IndexMap;
use serde_json::Value;

use vike_bridge_core::confirm::ConfirmByOrderClientId;
use vike_bridge_core::rest::VenueRest;
use vike_model::events::{Event, OrderRejected, OrderSubmitted, RejectCause};
use vike_model::{
    AssetClass, OrderRefusalId, OrderRequest, OrderType, SymbolProperties, TimeInForce,
};
use vike_venues::venue_delivery_guard::DeliveryGuardVerdict;

use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// The submit, batch run, amend, leverage write, confirm and resync reads are the family's bodies
// (`crate::family::execution::signed_rest`), over this client's parts and facts (`FamilySignedRest` below).
use crate::execution::rest::algo::OrderVenueIdRegistryAlgo;
use crate::family::execution::rest_reads::FoundOrder;
use crate::family::execution::signed_rest::{
    FamilyRestPaths, FamilySignedRest, modify_perp, send_batch_run,
};
use vike_bridge_core::execution_client::resync::ReplayRows;
use vike_bridge_core::websocket_session::user::RESYNC_HISTORY_LIMIT;

pub const PATH_ORDER: &str = "/fapi/v1/order";
/// Binance's facts for the family's perp amend: its `PATH_ORDER`, and `updateTime`, the reply's
/// stamp of the amend.
pub const PERP_MODIFY: crate::family::execution::rest_reads::PerpModifySpec =
    crate::family::execution::rest_reads::PerpModifySpec {
        path: PATH_ORDER,
        time_field: "updateTime",
    };
pub const PATH_BATCH_ORDERS: &str = "/fapi/v1/batchOrders";
/// fapi caps batchOrders at 5 orders per request.
pub const BATCH_MAX: usize = 5;
pub const PATH_POSITIONS: &str = "/fapi/v2/positionRisk";
/// fapi live open orders (signed) — the perp twin of spot `PATH_OPEN_ORDERS`.
pub const PATH_OPEN_ORDERS: &str = "/fapi/v1/openOrders";
pub const PATH_SET_LEVERAGE: &str = "/fapi/v1/leverage";
pub const PATH_BALANCE: &str = "/fapi/v2/balance";
pub const PATH_EXCHANGE_INFO: &str = "/fapi/v1/exchangeInfo";
/// PUBLIC server clock on the futures host, `{"serverTime": <epoch ms>}`: what a perp client's
/// `vike_bridge_core::signer::VenueClock` is read from. Read on the demo host by
/// `tests/binance_smokes/binance_perp_fine_tick_price_smoke.rs`.
pub const PATH_TIME: &str = "/fapi/v1/time";
/// The listenKey user-data socket's path under a futures `venue_url` row (`wss://fstream.binance.com` +
/// `/ws`), the same on both books and both networks; the key is appended after it.
pub const PATH_USER_WEBSOCKET: &str = "/ws";

/// This lane's `vike_venues::venue_time_in_force::venue_time_in_force` row key — the `"binance-perp"` LANE sub-key,
/// NOT the canonical venue id ([`crate::VENUE`] stays `"binance"` everywhere an
/// `OrderRequest`/event names the venue). The lane split exists because fapi has native GTD
/// while spot `/api/v3` has none, and the TIF table keys rows by string: the perp submit gates
/// and the family builder's perp face consume THIS key; spot keeps consuming `"binance"`.
pub const TIME_IN_FORCE_LANE: &str = "binance-perp";

/// fapi native-GTD wire constraints (POST /fapi/v1/order, venue error `-5040
/// FUTURE_GOOD_TILL_DATE`): `goodTillDate` is MANDATORY with `timeInForce=GTD`, "must be greater
/// than the current time plus 600 seconds and smaller than 253402300799000" (UTC 9999-12-31
/// 23:59:59). The venue keeps second-level precision only (the ms part is ignored server-side),
/// so there is no local granularity constraint to enforce.
pub const GTD_MIN_LEAD_MS: i64 = 600_000;
/// See [`GTD_MIN_LEAD_MS`] — the exclusive upper bound fapi accepts for `goodTillDate`.
pub const GTD_MAX_MS: i64 = 253_402_300_799_000;

/// The perp lane's GTD VALIDITY gate — the dynamic companion of the static
/// `venue_time_in_force(TIME_IN_FORCE_LANE, Gtd) = Mapped("GTD")` row (a table row cannot carry a date). Returns the
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
/// paths pass `vike_model::now_ms()`.
#[must_use]
pub fn deny_invalid_gtd(request: &OrderRequest, now_ms: i64) -> Option<Event> {
    if request.order_type != OrderType::Limit || request.time_in_force != TimeInForce::Gtd {
        return None;
    }
    let reason = match request.gtd_expiry {
        None => format!(
            "GTD on {TIME_IN_FORCE_LANE} requires gtd_expiry (fapi goodTillDate is mandatory) — a \
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
        cause: None,
        order_client_id: request.order_client_id.clone(),
        reason: reason.into(),
        timestamp: request.timestamp,
        // A terms check before the wire.
        refusal_id: vike_model::events::refusal_id_of(OrderRefusalId::NotSent.word()),
    }))
}

/// The perp lane's DELIVERY gate (okx's rule for every venue, the one verdict
/// `vike_venues::venue_delivery_guard::delivery_verdict` over the delivery day the order's name carries): an OPENING
/// order on a dated contract inside its last 24 h, and any order once it is delivered, is a terminal `OrderRejected`
/// with the wire untouched; a reduce-only order is taken until the delivery, so no position is trapped. The engine
/// books no delivery settlement, which is why. A perpetual names no day and passes. `now_ms` is a parameter so the
/// gate stays pure under test; the submit paths pass `vike_model::now_ms()`.
#[must_use]
pub fn deny_delivery(request: &OrderRequest, now_ms: i64) -> Option<Event> {
    let reason = match vike_venues::venue_delivery_guard::delivery_verdict(
        crate::VENUE,
        &request.symbol,
        now_ms,
        request.reduce_only,
    ) {
        DeliveryGuardVerdict::Taken => return None,
        DeliveryGuardVerdict::OpeningInsideWindow { left_ms } => format!(
            "binance: {} is delivered in {} h and takes no OPENING order inside its last 24 h: the venue \
             settles a position at the delivery and the engine books no settlement, so it would hold a position \
             the venue closed. A reduce-only order is still taken; open the next expiry instead; order refused",
            request.symbol,
            left_ms / vike_model::MS_PER_HOUR
        ),
        DeliveryGuardVerdict::Delivered => format!(
            "binance: {} was delivered at its expiry and no longer trades; order refused",
            request.symbol
        ),
    };
    Some(Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: request.order_client_id.clone(),
        reason: reason.into(),
        timestamp: request.timestamp,
        // The delivery gate, before the wire.
        refusal_id: vike_model::events::refusal_id_of(OrderRefusalId::NotSent.word()),
    }))
}

/// The ORDER-KIND gate of BOTH lanes (spot's `submit_order` calls it with `spot` set): the terminal
/// `OrderRejected` for a request this bridge does not build, pushed after the synchronous
/// `OrderSubmitted` with the wire untouched, or `None`. It refuses, by name:
///
/// - a kind neither lane builds (`market_to_limit`, an unrecognised word): the shared perp builder
///   sends a kind it does not know as `MARKET` (`crate::family::execution::order_map::build_perp_order_params`,
///   aster's to keep), so the refusal is here, never a fall-through to a market order;
/// - a stop, stop-limit or take-profit whose prices [`vike_model::order_terms_refusal`] refuses (no
///   trigger above zero, a stop-limit with no limit above zero, a stated limit not above zero, a
///   number that is not finite);
/// - a stop-limit, or a take-profit that states a limit, whose lifetime is not GTC: every builder
///   sends `GTC` (the perp algo `STOP`/`TAKE_PROFIT`, [`crate::execution::rest::algo::build_perp_algo_params`], and
///   spot's `STOP_LOSS_LIMIT`/`TAKE_PROFIT_LIMIT`, `crate::execution::rest::spot::VenueRestBinanceSpot::build_order_params`),
///   and another lifetime is unmeasured on either lane;
/// - a `trailing` order trailed by a PRICE, or whose percent spot cannot state (`trailing_refusal`);
///   a `trailing_limit`, a `pegged` order and a `trailing_take_profit` whatever their terms;
/// - a `display_qty` (an iceberg) the lane cannot show (`iceberg_refusal`).
#[must_use]
pub fn deny_unbuilt_kind(request: &OrderRequest, spot: bool) -> Option<Event> {
    let kind = &request.order_type;
    // the kinds whose LIMIT rests at the venue once triggered
    let resting_limit = match kind {
        OrderType::StopLimit => Some("stop_limit"),
        OrderType::TakeProfit if request.price.is_some() => Some("take_profit with a limit"),
        _ => None,
    };
    // Nothing is sent either way: an order's own terms are a check before the wire, the rest what binance cannot hold.
    let (not_sent, capability) = (OrderRefusalId::NotSent, OrderRefusalId::VenueCapability);
    let (reason, refusal) = match kind {
        OrderType::Market | OrderType::Limit => None,
        OrderType::Stop | OrderType::StopLimit | OrderType::TakeProfit => {
            let tif = request.time_in_force;
            vike_model::order_terms_refusal(
                kind,
                request.price,
                request.trigger_price,
                request.trail,
                request.limit_offset,
                request.peg.as_ref(),
                request.trail_percent,
            )
            .map(|why| (why, not_sent))
            .or_else(|| {
                let what = resting_limit.filter(|_| tif != TimeInForce::Gtc)?;
                let why =
                    format!("binance rests a {what} GTC only (`{}` was asked)", tif.as_word());
                Some((why, capability))
            })
        }
        // binance trails by a percent (perp) or basis points (spot), never by a price: a `trailing` is
        // built with a `trail_percent` only ([`trailing_refusal`]). No `trailing_limit` (spot's
        // `STOP_LOSS_LIMIT` takes a fixed limit, not an offset), no pegged order, and no trailing
        // take-profit (its trailing `TAKE_PROFIT` is a trailing STOP armed on the profit side: the
        // `trailing` with an activation that spot builds).
        // shortcut: a trailing order's own malformed terms read as a capability too; split `trailing_refusal` if it
        // ever matters.
        OrderType::Trailing => trailing_refusal(request, spot).map(|why| (why, capability)),
        OrderType::MarketToLimit
        | OrderType::TrailingLimit
        | OrderType::Pegged
        | OrderType::TrailingTakeProfit
        | OrderType::Unrecognized(_) => {
            Some((format!("binance builds no `{kind}` order"), capability))
        }
    }
    .or_else(|| {
        let shown = request.display_quantity?;
        iceberg_refusal(request, shown, resting_limit.is_some(), spot).map(|why| (why, capability))
    })?;
    Some(Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: request.order_client_id.clone(),
        reason: format!("{reason}; order refused").into(),
        timestamp: request.timestamp,
        refusal_id: vike_model::events::refusal_id_of(refusal.word()),
    }))
}

/// Why a `trailing` order cannot be sent on this lane, or `None`: its terms
/// ([`vike_model::order_terms_refusal`]: exactly one distance, an activation above zero, no price), a
/// PRICE trail (the perpetual's `callbackRate` and spot's `trailingDelta` are both a share of the
/// extreme), on the perpetual a percent outside the page's `callbackRate` range
/// ([`crate::execution::rest::algo::CALLBACK_RATE_RANGE`]; its step is unstated, so the venue's), and on spot a percent
/// that is no whole number of basis points ([`crate::execution::rest::spot::trailing_delta_bips`]: a rounded delta would
/// move the stop). Spot's `TRAILING_DELTA` filter is NOT cached by this bridge (the symbol grid,
/// `vike_model::SymbolProperties`, has no slot for it), so whether the delta fits it is the venue's
/// answer, a terminal `OrderRejected` in its words.
fn trailing_refusal(request: &OrderRequest, spot: bool) -> Option<String> {
    vike_model::order_terms_refusal(
        &request.order_type,
        request.price,
        request.trigger_price,
        request.trail,
        request.limit_offset,
        request.peg.as_ref(),
        request.trail_percent,
    )
    .or_else(|| {
        request.trail.map(|t| {
            format!("binance trails by a percent, never by a price (a trail of {t} was asked)")
        })
    })
    .or_else(|| {
        let p = request.trail_percent?;
        if spot {
            crate::execution::rest::spot::trailing_delta_bips(p).is_none().then(|| {
                format!(
                    "binance spot trails in whole basis points (0.01%): a trail of {p}% is not one"
                )
            })
        } else {
            let range = crate::execution::rest::algo::CALLBACK_RATE_RANGE;
            (!range.contains(&p)).then(|| {
                format!(
                    "binance's perpetual trails by {}% to {}% (`callbackRate`): a trail of {p}% is \
                     outside it",
                    range.start(),
                    range.end()
                )
            })
        }
    })
}

/// Why `request`, which asks the venue to show only `shown` of it, cannot be sent, or `None`.
/// `resting_limit`: a stop-limit or a take-profit with a limit, already held to GTC.
///
/// - The PERPETUAL has no visible-size field (its only slicing is the TWAP/VP algos): refused on that
///   lane whatever the row says, since the row is one for both lanes.
/// - SPOT's `icebergQty` rides only a `LIMIT`, `STOP_LOSS_LIMIT` or `TAKE_PROFIT_LIMIT` with
///   `timeInForce=GTC` (the spot trading-endpoints page), so a market, a stop, a market take-profit, an
///   IOC/FOK limit and a post-only `LIMIT_MAKER` (which carries no `timeInForce`) are refused; a visible
///   size that is not a number above zero too. Whether it fits the symbol's `ICEBERG_PARTS` filter is
///   the venue's answer.
fn iceberg_refusal(
    request: &OrderRequest,
    shown: f64,
    resting_limit: bool,
    spot: bool,
) -> Option<String> {
    if !spot {
        return Some(
            "binance's perpetual has no iceberg: it shows every order whole (`display_qty` was asked)"
                .to_string(),
        );
    }
    if !(shown.is_finite() && shown > 0.0) {
        return Some(format!("the display quantity {shown} is not a number above zero"));
    }
    let gtc_limit = resting_limit
        || (request.order_type == OrderType::Limit && request.time_in_force == TimeInForce::Gtc);
    (!gtc_limit).then(|| {
        format!(
            "binance spot shows part of a LIMIT, STOP_LOSS_LIMIT or TAKE_PROFIT_LIMIT resting GTC \
             only (an iceberg `{}` `{}` was asked)",
            request.order_type,
            request.time_in_force.as_word()
        )
    })
}

/// RiskLimits-shaped properties + base_asset — twin of `parse_binance_perp_instruments`.
/// Perp deltas: market-order qty cap from MARKET_LOT_SIZE.maxQty; min notional from
/// MIN_NOTIONAL.notional (spot uses NOTIONAL/minNotional).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerpInstrument {
    pub properties: SymbolProperties,
    pub base_asset: String,
}

/// The asset class of ONE `/fapi` `/exchangeInfo` `symbols[]` entry: the class of the typed
/// [`vike_model::Contract`] the picker's catalog builds from the same entry
/// (`crates/bridges/binance/src/family/catalog.rs`'s `fapi_contract`), so the order path and the
/// picker cannot classify one row two ways. That function reads the venue's own `contractType`
/// (never the symbol text) and argues the mapping, MEASURED 2026-10-11: `PERPETUAL`,
/// `TRADIFI_PERPETUAL` and `PERPETUAL_DELIVERING` (a perpetual scheduled for delivery, which keeps
/// its `.P` route and carries the date as `settling_ms`) are perps; `CURRENT_QUARTER`/
/// `NEXT_QUARTER` are dated futures.
///
/// ⚠ `contractType` is NOT a binary perpetual-or-dated word: `TRADIFI_PERPETUAL` (217 equity and
/// commodity perpetuals) proved the list open, and the old "everything else is dated" rule
/// classed every one of them `CryptoFuture`. An unmeasured or ABSENT word is now `None`: this
/// venue's answer is missing, and [`vike_model::SymbolProperties::asset_class`]'s doc argues at
/// length why a missing answer must not be replaced by a guess.
fn perp_asset_class(entry: &Value) -> Option<AssetClass> {
    crate::family::catalog::fapi_contract(entry).map(|c| c.class())
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
        out.insert(
            symbol,
            PerpInstrument {
                // The grid is parsed ONCE, by the same function the picker's catalog row reads
                // (`crates/bridges/binance/src/family/catalog.rs`'s `perp_grid`: tick, step,
                // min qty, the `MARKET_LOT_SIZE` market-order cap, `MIN_NOTIONAL.notional`), and
                // the class is the VENUE'S OWN answer read off the same entry ([`perp_asset_class`]).
                // `/fapi` `/exchangeInfo` is NOT a perp-only endpoint: it lists delivery futures
                // (`CURRENT_QUARTER`/`NEXT_QUARTER`, symbols like `BTCUSDT_261225`) beside the
                // perpetuals, so "this parser is the perp parser" would be a wrong answer for part
                // of its own output.
                properties: SymbolProperties {
                    asset_class: perp_asset_class(entry),
                    ..crate::family::catalog::perp_grid(entry)
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

/// The REST half of the live perp client (ACK-only); fills come from the listenKey WS.
pub struct VenueRestBinancePerp<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
    /// The NETWORK `base_url` was looked up at (the `venue_url` type, `demo` or `live`): what the demo-only mode
    /// writes decide by (`crate::account::modes`'s `writes_modes`), never the host string (the venue_url work's PLAN
    /// trap 1). Set where the host is looked up (`crate::execution`'s `BinanceUrls`), so the two cannot disagree.
    pub ty: vike_venues::venue_urls::UrlType,
    /// The BOOK `base_url` serves, by its margining (`crate::execution::rest::coin_m`'s `margining_of`, from the name the host
    /// was looked up by): `Linear` USDⓈ-M (`fapi`), `Inverse` COIN-M (`dapi`). What the COIN-M path rewrite
    /// (`crate::execution::rest::coin_m`'s `RestTransportDapiPaths`) and refusals (`crate::execution::rest::coin_m`'s `deny_unbuilt`) decide by, never the
    /// host string; no binance book is `Quanto`, and a client stating it is refused in words.
    pub margining: vike_model::Margining,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub leverage: f64,
    /// Binance Broker/Link attribution id — see [`crate::execution::rest::spot::VenueRestBinanceSpot::link_id`]'s doc
    /// (unified cross-venue attribution, task 6); the perp twin, applied identically to
    /// `newClientOrderId`/`origClientOrderId`/`origClientOrderIdList`.
    pub link_id: Option<String>,
    /// The order client ids this client sent to the Algo Service (`crate::execution::rest::algo`): a cancel or a
    /// status query of a member goes to `/fapi/v1/algoOrder` first. A hint — see [`OrderVenueIdRegistryAlgo`].
    pub algo_order_client_ids: OrderVenueIdRegistryAlgo,
}

impl<S: Signer, T: RestTransport> VenueRestBinancePerp<S, T> {
    /// POST /fapi/v1/leverage {symbol, leverage:int}. Binance change-leverage is
    /// idempotent (HTTP-200 even when already at target) — NO benign-error swallow.
    pub fn set_leverage(&self) -> Result<(), VenueApiError> {
        crate::family::execution::signed_rest::set_leverage(self, self.leverage)
    }

    /// Pure, golden-gated. Delegates to the shared, byte-identical
    /// [`crate::family::execution::order_map::build_perp_order_params`] (mappers rung), passing this client's
    /// `symbol`/`properties`: positionSide BOTH (one-way); reduceOnly as STRING.
    /// The TIF row key is [`TIME_IN_FORCE_LANE`] (`"binance-perp"`, native fapi GTD) — every non-GTD
    /// request's bytes are identical to the `"binance"` row it consumed before the lane split.
    ///
    /// ⚠ **NOT for a stop, a stop-limit, a take-profit or a trailing stop.** That builder's `Stop` arm (`STOP_MARKET` +
    /// `stopPrice`) is aster's, and it sends the others as `MARKET`; the venue has refused conditional
    /// types on `/fapi/v1/order` since 2025-12-09 (`-4120`). All four are sent by [`Self::build_algo_params`] to
    /// `/fapi/v1/algoOrder`, and `submit_order`/`submit_batch` route them ([`deny_unbuilt_kind`] refuses
    /// every kind neither path builds).
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        crate::family::execution::order_map::build_perp_order_params(
            request,
            &self.symbol,
            &self.properties,
            TIME_IN_FORCE_LANE,
            self.link_id.as_deref(),
        )
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
                    order_client_id: r.order_client_id.clone(),
                    timestamp: r.timestamp,
                })
            })
            .collect();
        // tif step-2 gate, per order: a TIF this lane cannot express (Day) or a GTD whose date
        // fails the checkable-here fapi bounds ([`deny_invalid_gtd`]) is a LOUD terminal reject
        // and its order never enters a wire chunk; the supported rest batch as before. The
        // trigger-source gate (`deny_unsupported_trigger_by` — Index has no fapi workingType)
        // and the delivery gate ([`deny_delivery`]) ride the same per-order chain.
        let now = vike_model::now_ms();
        let mut sendable: Vec<&OrderRequest> = Vec::with_capacity(requests.len());
        for r in requests {
            match vike_venues::venue_time_in_force::deny_unsupported_time_in_force(
                TIME_IN_FORCE_LANE,
                r,
            )
            .or_else(|| deny_invalid_gtd(r, now))
            .or_else(|| deny_delivery(r, now))
            .or_else(|| {
                vike_bridge_core::trigger::deny_unsupported_trigger_by(TIME_IN_FORCE_LANE, r)
            })
            .or_else(|| deny_unbuilt_kind(r, false))
            .or_else(|| crate::execution::rest::coin_m::deny_unbuilt(self.margining, r))
            {
                Some(reject) => events.push(reject),
                None => sendable.push(r),
            }
        }
        // A stop, stop-limit or take-profit is a CONDITIONAL order: the venue refuses it on `batchOrders` (`-4120`)
        // and no algo batch endpoint exists, so it leaves the chunks and is sent on its own. Each run of regular
        // orders is the family's ([`send_batch_run`]: `POST /fapi/v1/batchOrders`, chunked at [`BATCH_MAX`],
        // each element read through this client's refusal words). The
        // sendable orders are walked in request order and every consecutive run of regular ones
        // batches as before, so the events keep the order the requests came in (and with no stop
        // there is exactly one run, chunked as ever).
        let mut run: Vec<&OrderRequest> = Vec::new();
        for r in sendable {
            if crate::execution::rest::algo::is_algo_kind(&r.order_type) {
                send_batch_run(self, &mut events, &run, BATCH_MAX);
                run.clear();
                events.extend(self.submit_algo(r));
            } else {
                run.push(r);
            }
        }
        send_batch_run(self, &mut events, &run, BATCH_MAX);
        events
    }

    /// Native batch-cancel (DELETE /fapi/v1/batchOrders, origClientOrderIdList = JSON array, ≤5).
    /// Per-order "already gone" rides the response array (non-fatal); a whole-request/network
    /// failure returns the first error. Chunks at BATCH_MAX. RUST-NATIVE. Each id is re-prefixed
    /// with the same broker link id submit stamped (task 6) — see
    /// [`crate::family::execution::rest_reads::query_order_orderid`]'s doc for why a bare id can't identify
    /// the order.
    ///
    /// **Conditional orders:** an order client id in [`OrderVenueIdRegistryAlgo`] leaves the batch and is cancelled singly, as
    /// [`Self::cancel_member`] does (no algo batch cancel exists); an order client id the regular batch answers
    /// "unknown" for (a per-element `-2011` with those words in the response array) is retried ONCE on
    /// the algo endpoint, as [`VenueRest::cancel_order`]'s non-member path does; any other element code (a
    /// `-2011` in other words, `-4117` a stop mid-trigger, `-1003`) is the batch's error, never a silent `Ok`.
    pub fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        let (stops, regular): (Vec<&String>, Vec<&String>) =
            order_client_ids.iter().partition(|c| self.algo_order_client_ids.contains(c));
        for order_client_id in stops {
            if let Err(e) = self.cancel_member(order_client_id) {
                first_err.get_or_insert(e);
            }
        }
        for chunk in regular.chunks(BATCH_MAX) {
            let orig_ids: Vec<String> = chunk
                .iter()
                .map(|c| {
                    crate::family::execution::order_map::binance_broker_order_client_id(
                        self.link_id.as_deref(),
                        c,
                    )
                })
                .collect();
            let params: Vec<(&str, String)> = vec![
                ("symbol", self.symbol.clone()),
                ("origClientOrderIdList", serde_json::to_string(&orig_ids).expect("array")),
            ];
            match self.transport.signed(
                &self.base_url,
                PATH_BATCH_ORDERS,
                "DELETE",
                &params,
                &self.signer,
            ) {
                Ok(response) => {
                    // the response array is index-matched to the chunk
                    let list = response.as_array().map(Vec::as_slice).unwrap_or_default();
                    for (order_client_id, element) in chunk.iter().zip(list) {
                        let Some(refusal) =
                            crate::family::execution::signed_rest::batch_cancel_refusal(element)
                        else {
                            continue;
                        };
                        let retried =
                            if crate::transport::error_codes::cancel_says_order_unknown(&refusal) {
                                self.cancel_algo(order_client_id)
                            } else {
                                Err(refusal)
                            };
                        if let Err(e) = retried {
                            first_err.get_or_insert(e);
                        }
                    }
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Native modify (PUT /fapi/v1/order): the family's one perp amend, [`modify_perp`], with
    /// binance's facts ([`PERP_MODIFY`]) and the broker-stamped `origClientOrderId`.
    ///
    /// **A stop, stop-limit or take-profit is not modifiable** (Algo Service: "modification of untriggered
    /// conditional orders is not supported"): it answers the non-terminal `modify_failed` advisory and
    /// sends NOTHING.
    pub fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        if self.is_algo_order(order) {
            return self.refuse_stop_modify(order);
        }
        // origClientOrderId must match the venue's OWN stored id — re-prefixed (task 6) by the family's
        // amend, same as cancel.
        modify_perp(self, &PERP_MODIFY, &self.properties, order, new_qty, new_price)
    }
}

impl<S: Signer, T: RestTransport> VenueRestBinancePerp<S, T> {
    /// `DELETE /fapi/v1/order` by `origClientOrderId` (re-prefixed as submit stamped it). The raw
    /// verb: the caller judges the venue's answer.
    fn delete_order(&self, order_client_id: &str) -> Result<(), VenueApiError> {
        let orig = crate::family::execution::order_map::binance_broker_order_client_id(
            self.link_id.as_deref(),
            order_client_id,
        );
        crate::family::execution::rest_reads::delete_by_order_client_id(
            &self.transport,
            &self.signer,
            &self.base_url,
            PATH_ORDER,
            &self.symbol,
            &orig,
        )
    }

    /// Cancel an order client id this client sent to the Algo Service: the algo endpoint first, then — when it
    /// answers "unknown" (already gone) — the regular endpoint ONCE under the same id. A TRIGGERED
    /// stop-limit is no longer the algo endpoint's: it answers `-2011 Unknown order sent.` while the
    /// stop's child LIMIT rests on `/fapi/v1/order` under the stop's own `clientAlgoId`, and
    /// `DELETE /fapi/v1/order` by that id cancels it (MEASURED on the demo host 2026-11-08,
    /// `tests/binance_smokes/perp_algo_smoke.rs`'s `perp_stop_limit_on_the_demo_algo_service`), so the
    /// algo answer alone read a resting child as "already gone". The regular endpoint's "unknown" too
    /// is "already gone"; any other failure there is an error, never "gone". An algo cancel or an algo
    /// error stands as it always did (one request). What it costs a triggered-and-filled stop: one
    /// more call that answers `-2011`. It never reads the kind, so a take-profit (`TAKE_PROFIT`'s
    /// resting child LIMIT; `TAKE_PROFIT_MARKET`'s child fills at once) takes the same two steps: that
    /// its child keeps the `clientAlgoId` is assumed from the stop-limit's measurement, not measured.
    pub(crate) fn cancel_member(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        use vike_bridge_core::rest::OrderCancelAck;
        match self.cancel_algo(order_client_id) {
            Ok(OrderCancelAck::AlreadyGone) => OrderCancelAck::from_venue_answer(
                crate::VENUE,
                order_client_id,
                self.delete_order(order_client_id),
                crate::transport::error_codes::cancel_says_order_unknown,
            ),
            answered => answered,
        }
    }
}

/// Binance perp's parts and facts for the family's signed REST bodies: `/fapi/v1` (a COIN-M client's
/// transport carries each request to its `dapi` twin, `crate::execution::rest::coin_m`'s `RestTransportDapiPaths`), the broker link
/// id on every order client id (task 6), the lane's bridge floors, the Algo Service for a conditional
/// order (decision 0186: [`FamilySignedRest::route`], the two-endpoint lookup below and the
/// `allAlgoOrders` replay read, all in `crate::execution::rest::algo`), the post-only cause and `crate::account::modes`'s
/// reject words of a refusal, and its own log target.
impl<S: Signer, T: RestTransport> FamilySignedRest for VenueRestBinancePerp<S, T> {
    const VENUE: &'static str = crate::VENUE;
    const PATHS: FamilyRestPaths = FamilyRestPaths {
        order: PATH_ORDER,
        all_orders: "/fapi/v1/allOrders",
        trades: "/fapi/v1/userTrades",
        leverage: PATH_SET_LEVERAGE,
        batch: PATH_BATCH_ORDERS,
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
    fn link_id(&self) -> Option<&str> {
        self.link_id.as_deref()
    }
    /// ⚠ The family builder: NOT for a conditional order, which [`FamilySignedRest::route`] sends first.
    fn order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        self.build_order_params(request)
    }
    /// tif step-2 gate: a TIF this lane cannot express (Day) — or a GTD whose date fails the
    /// checkable-here fapi bounds ([`deny_invalid_gtd`]) — is a LOUD terminal reject; the wire is never
    /// touched, never a silent GTC (`tests/offline/time_in_force_gate.rs` pins both gates). Same discipline for a
    /// trigger source fapi cannot express (Index — no such workingType): `deny_unsupported_trigger_by`
    /// (`tests/offline/trigger_gate.rs`). A kind this bridge does not build ([`deny_unbuilt_kind`]) is
    /// refused by name, never sent as the family builder's `MARKET`; so is what a COIN-M client does not
    /// build (`crate::execution::rest::coin_m`). An opening order on a dated contract inside its last day, and any
    /// order on a delivered one, is refused here too ([`deny_delivery`]), before the algo route, so a stop is judged
    /// as a plain order is.
    fn floor(&self, request: &OrderRequest) -> Option<Event> {
        let now = vike_model::now_ms();
        vike_venues::venue_time_in_force::deny_unsupported_time_in_force(
            TIME_IN_FORCE_LANE,
            request,
        )
        .or_else(|| deny_invalid_gtd(request, now))
        .or_else(|| deny_delivery(request, now))
        .or_else(|| {
            vike_bridge_core::trigger::deny_unsupported_trigger_by(TIME_IN_FORCE_LANE, request)
        })
        .or_else(|| deny_unbuilt_kind(request, false))
        .or_else(|| crate::execution::rest::coin_m::deny_unbuilt(self.margining, request))
    }
    /// A stop, stop-limit, take-profit or percent trailing stop is a CONDITIONAL order and goes to the Algo
    /// Service (`crate::execution::rest::algo`); the venue refuses it on `/fapi/v1/order` (`-4120`). The floors are the same
    /// either way.
    fn route(&self, request: &OrderRequest) -> Option<Vec<Event>> {
        crate::execution::rest::algo::is_algo_kind(&request.order_type)
            .then(|| self.submit_algo(request))
    }
    fn refusal(
        &self,
        request: &OrderRequest,
        code: i64,
        msg: &str,
    ) -> (Option<RejectCause>, String) {
        (
            crate::transport::error_codes::post_only_cause(request, code, msg),
            crate::account::modes::reject_reason(self, code, msg),
        )
    }
    fn log_refused(&self, order_client_id: &str, exc: &VenueApiError) {
        tracing::warn!(target: "vike_binance::execution::rest::perp", order_client_id, code = exc.code, msg = %exc.msg, "submit rejected");
    }
    fn conditional_history_rows(&self, symbol: &str, rows: ReplayRows<'_>) -> Option<Value> {
        Some(rows(self.get_all_algo_orders_for(symbol, RESYNC_HISTORY_LIMIT)))
    }
    fn conditional_history_events(&self, rows: &Value) -> Vec<Event> {
        crate::execution::rest::algo::map_algo_history(rows, self.link_id.as_deref())
    }

    /// The by-order-client-id status query behind BOTH the ambiguous-submit arm and the active `confirm_order`
    /// (the `ExecutionClientActor::with_confirm` closure in `crate::execution`): ONE body, so they cannot disagree.
    ///
    /// **Two endpoints, one verdict.** An order is on `GET /fapi/v1/order` or — a conditional order
    /// the Algo Service holds — on `GET /fapi/v1/algoOrder`; "absent" is only `Ok(None)` from BOTH.
    /// An order client id this client sent to the algo endpoint asks it first (one call, not two); any other asks
    /// the regular endpoint first, which is also the ordinary order's one-call path. A FAILED query on
    /// either is `Err`: it says nothing about the order (never "absent", which would reject a live
    /// stop and orphan it at the venue).
    ///
    /// ⚠ **The same rule reaches ORDINARY orders:** their "venue confirms absent" verdict now also needs
    /// the algo endpoint to answer (`says_order_unknown` = absent); while it is in trouble a never-placed
    /// limit/market order whose submit timed out waits for the watchdog's backstop instead of being
    /// rejected at once. Deliberate: the alternative rejects a live stop.
    ///
    /// **A TRIGGERED stop-limit's state is its CHILD's.** The algo endpoint keeps reading `TRIGGERED`
    /// after the trigger, while the child LIMIT rests, fills or is cancelled on `/fapi/v1/order` under
    /// the stop's own client id (MEASURED on the demo host 2026-11-08, as for
    /// [`Self::cancel_member`]). So a member the algo endpoint answers `TRIGGERED` or `FINISHED` for,
    /// whose `orderType` is a stop-limit (`STOP`) or a take-profit (`TAKE_PROFIT`, `TAKE_PROFIT_MARKET`:
    /// one kind once read back, and the market one's child ends at once, so reading it costs one call
    /// and can only say more), is read ONCE more on the regular endpoint, and the child's status is the
    /// answer, under the stop's `algoId` (the id its accept carried). No child there yet, or a failed
    /// child read, keeps the algo's own answer, as before. A plain stop (`STOP_MARKET`) is answered by
    /// the algo endpoint alone, exactly as before.
    fn lookup_by_order_client_id(
        &self,
        order_client_id: &str,
    ) -> Result<Option<FoundOrder>, VenueApiError> {
        let regular = || {
            let orig = crate::family::execution::order_map::binance_broker_order_client_id(
                self.link_id.as_deref(),
                order_client_id,
            );
            crate::family::execution::rest_reads::query_order_orderid(
                &self.transport,
                &self.signer,
                &self.base_url,
                PATH_ORDER,
                &self.symbol,
                &orig,
            )
        };
        let algo = || self.lookup_algo(order_client_id);
        if self.algo_order_client_ids.contains(order_client_id) {
            match algo()? {
                Some((stop, OrderType::StopLimit | OrderType::TakeProfit))
                    if matches!(stop.status.as_str(), "TRIGGERED" | "FINISHED") =>
                {
                    Ok(Some(match regular() {
                        Ok(Some(child)) => {
                            FoundOrder { order_id: stop.order_id, status: child.status }
                        }
                        _ => stop,
                    }))
                }
                Some((stop, _)) => Ok(Some(stop)),
                None => regular(),
            }
        } else {
            regular().and_then(|found| {
                if found.is_some() { Ok(found) } else { algo().map(|a| a.map(|(stop, _)| stop)) }
            })
        }
    }
}

impl<S: Signer + Send, T: RestTransport + Send> ConfirmByOrderClientId
    for VenueRestBinancePerp<S, T>
{
    const LOOKUP_WINDOW: Option<std::time::Duration> = None; // none stated by the venue

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        crate::family::execution::signed_rest::confirm_when(
            self,
            order_client_id,
            ts,
            absent_rejects,
        )
    }
}

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for VenueRestBinancePerp<S, T> {
    /// The family's submit ([`crate::family::execution::signed_rest::submit_order`]): Submitted → this lane's
    /// floors ([`FamilySignedRest::floor`]) → the Algo Service for a conditional order
    /// ([`FamilySignedRest::route`]) → REST → Accepted|Rejected (crypto_client.py's flow).
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        crate::family::execution::signed_rest::submit_order(self, request)
    }

    /// `origClientOrderId` re-applies the same broker prefix submit stamped (task 6) — see
    /// [`crate::family::execution::rest_reads::query_order_orderid`]'s doc for why this can't be sent bare.
    ///
    /// **A conditional order is cancelled on the Algo Service** (`DELETE /fapi/v1/algoOrder`, by
    /// `clientAlgoId`): straight there for an order client id this client sent to it, and — for any other order client id the
    /// regular endpoint answers "unknown order" for (`-2011` with those words,
    /// [`crate::transport::error_codes::cancel_says_order_unknown`]), e.g. a stop placed before a restart —
    /// there ONCE before "already gone" is concluded. Never "gone" while the stop may still rest: the
    /// retry's own failure is an error, and so is a `-2011` in any other words (no retry: the regular
    /// endpoint refused a real order, and the algo endpoint's "unknown" would have read as gone).
    ///
    /// **What it costs ordinary orders:** a cancel of a REGULAR order the venue already closed (the
    /// common cancel-races-a-fill case, `-2011`) is now TWO sequential calls on the exec thread, and an
    /// algo-endpoint answer that does not say unknown (a 429, a 5xx) turns the old idempotent `Ok` into
    /// an `Err` — a non-terminal `OrderCancelRejected` the fill's own events then settle. Accepted: it is
    /// the price of never calling a resting stop cancelled.
    ///
    /// **A TRIGGERED stop-limit is cancelled on the REGULAR endpoint** ([`Self::cancel_member`]).
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        if self.algo_order_client_ids.contains(order_client_id) {
            return self.cancel_member(order_client_id);
        }
        match self.delete_order(order_client_id) {
            Ok(()) => Ok(vike_bridge_core::rest::OrderCancelAck::Canceled),
            Err(exc) if crate::transport::error_codes::cancel_says_order_unknown(&exc) => {
                // not on the regular endpoint: maybe a resting stop this client never saw — one retry
                // on the algo endpoint, whose own "unknown" is the idempotent "already gone"
                tracing::debug!(target: "vike_binance::execution::rest::perp", order_client_id, "cancel: unknown on /order, trying the algo endpoint");
                self.cancel_algo(order_client_id)
            }
            Err(exc) => Err(exc),
        }
    }

    /// ACTIVELY confirm one order by its client id (the by-order-client-id status GET): found -> `OrderAccepted`
    /// (+ the terminal when the venue already closed it), absent -> the "venue confirms absent"
    /// reject, a FAILED query -> NOTHING (the watchdog ladder and reconcile decide). This trait door
    /// assumes the submit was fully dispatched; the exec thread's confirm closure goes through
    /// [`ConfirmByOrderClientId::confirm_when`] with the real answer.
    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        self.confirm_when(order_client_id, ts, true)
    }

    /// Native Binance-futures batch-submit — overrides the fan-out default with /fapi/v1/batchOrders.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        VenueRestBinancePerp::submit_batch(self, requests)
    }

    /// Native Binance-futures batch-cancel — overrides the fan-out default.
    fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        VenueRestBinancePerp::cancel_batch(self, order_client_ids)
    }

    /// Native Binance-futures modify — overrides the no-op default with PUT /fapi/v1/order.
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        VenueRestBinancePerp::modify_order(self, order, new_qty, new_price)
    }
}

#[cfg(test)]
mod cancel_member_tests {
    //! [`VenueRestBinancePerp::cancel_member`] over a scripted venue: the answers the demo host gave for a
    //! TRIGGERED stop-limit (2026-11-08: the algo `DELETE` says `-2011 Unknown order sent.`, the
    //! regular `DELETE` by the same client id cancels the resting child), and the answers that must
    //! keep standing.
    use serde_json::json;
    use vike_bridge_core::rest::{OrderCancelAck, VenueRest};

    use super::PATH_ORDER;
    use crate::account::modes::modes_tests::{Answer, DEMO_USDM, Venue, perp_rest, refusal};
    use crate::execution::rest::algo::PATH_ALGO_ORDER;

    /// Cancel order client id `vtslD1`, a member of the algo set, against `algo` and `order` answers; the result
    /// and the requests sent.
    fn cancel(algo: Answer, order: Option<Answer>) -> (Result<OrderCancelAck, i64>, Vec<String>) {
        let mut venue = Venue::new().on("DELETE", PATH_ALGO_ORDER, algo);
        if let Some(order) = order {
            venue = venue.on("DELETE", PATH_ORDER, order);
        }
        let rest = perp_rest(venue, DEMO_USDM, "ETHUSDT", 2.0);
        rest.algo_order_client_ids.insert("vtslD1");
        let got = rest.cancel_order("vtslD1").map_err(|e| e.code);
        (got, rest.transport.sent())
    }

    #[test]
    fn a_triggered_stop_limits_resting_child_is_cancelled_on_the_regular_endpoint() {
        let unknown = || refusal(-2011, "Unknown order sent.");
        let child = Ok(
            json!({"orderId": 16855423581_i64, "clientOrderId": "vtslD1", "status": "CANCELED"}),
        );
        let both = ["DELETE /fapi/v1/algoOrder", "DELETE /fapi/v1/order"];
        assert_eq!(
            cancel(unknown(), Some(child)),
            (Ok(OrderCancelAck::Canceled), both.map(String::from).to_vec())
        );
        // unknown on both endpoints: already gone, as before
        assert_eq!(cancel(unknown(), Some(unknown())).0, Ok(OrderCancelAck::AlreadyGone));
        // the regular endpoint failing otherwise is an error, never "gone"
        assert_eq!(cancel(unknown(), Some(refusal(-1003, "Too many requests."))).0, Err(-1003));
        // an algo endpoint that refuses (not "unknown") is the answer, with no second request
        let busy = cancel(refusal(-1003, "Too many requests."), None);
        assert_eq!(busy, (Err(-1003), vec!["DELETE /fapi/v1/algoOrder".to_string()]));
        // an untriggered stop cancelled on the algo endpoint costs one request, as before
        let ok =
            Ok(json!({"algoId": 1, "clientAlgoId": "vtslD1", "code": "200", "msg": "success"}));
        assert_eq!(
            cancel(ok, None),
            (Ok(OrderCancelAck::Canceled), vec!["DELETE /fapi/v1/algoOrder".to_string()])
        );
    }

    /// A take-profit is a member once submitted (`submit_algo` remembers it), so its cancel takes the
    /// stop-limit's two steps: a TRIGGERED one's resting child is cancelled on the regular endpoint.
    #[test]
    fn a_submitted_take_profit_is_cancelled_like_a_stop_limit() {
        use vike_model::events::Event;
        use vike_model::{OrderRequest, OrderType, SymbolProperties};
        let venue = Venue::new()
            .on("POST", PATH_ALGO_ORDER, Ok(json!({"algoId": 7})))
            .on("DELETE", PATH_ALGO_ORDER, refusal(-2011, "Unknown order sent."))
            .on("DELETE", PATH_ORDER, Ok(json!({"orderId": 8, "status": "CANCELED"})));
        let mut rest = perp_rest(venue, DEMO_USDM, "ETHUSDT", 2.0);
        rest.properties =
            SymbolProperties { tick_size: 0.01, step_size: 0.001, ..Default::default() };
        let take_profit = OrderRequest {
            order_client_id: "vttpL1".into(),
            venue: "binance".into(),
            symbol: "ETHUSDT.P".into(),
            side: -1,
            qty: 0.01,
            order_type: OrderType::TakeProfit,
            trigger_price: Some(2_400.0),
            price: Some(2_390.0),
            reduce_only: true,
            ..Default::default()
        };
        let placed = rest.submit_order(&take_profit);
        assert!(
            matches!(placed.as_slice(), [Event::OrderSubmitted(_), Event::OrderAccepted(_)]),
            "{placed:?}"
        );
        assert_eq!(rest.cancel_order("vttpL1").map_err(|e| e.code), Ok(OrderCancelAck::Canceled));
        assert_eq!(
            rest.transport.sent(),
            ["POST /fapi/v1/algoOrder", "DELETE /fapi/v1/algoOrder", "DELETE /fapi/v1/order"]
        );
    }
}

#[cfg(test)]
mod confirm_member_tests {
    //! The by-order-client-id confirm of an order client id this client sent to the Algo Service
    //! (`VenueRestBinancePerp::lookup_by_order_client_id`), over a scripted venue answering in the shapes the demo
    //! host gave on 2026-11-08: a TRIGGERED stop-limit (`orderType` `STOP`) is answered by its CHILD on
    //! `/fapi/v1/order`, under the stop's `algoId`; a plain stop (`STOP_MARKET`) by the algo endpoint
    //! alone, exactly as before.
    use serde_json::json;
    use vike_bridge_core::rest::VenueRest;
    use vike_model::events::Event;

    use super::PATH_ORDER;
    use crate::account::modes::modes_tests::{Answer, DEMO_USDM, Venue, perp_rest, refusal};
    use crate::execution::rest::algo::PATH_ALGO_ORDER;

    const ALGO_ID: i64 = 1_000_000_278_610_930;
    const CHILD_ID: i64 = 16_855_501_187;

    /// `GET /fapi/v1/algoOrder` as measured for the stop `vtslD1` of `order_type` in `status`.
    fn algo_row(order_type: &str, status: &str) -> Answer {
        Ok(json!({
            "algoId": ALGO_ID, "clientAlgoId": "vtslD1", "algoType": "CONDITIONAL",
            "orderType": order_type, "symbol": "ETHUSDT", "side": "SELL", "positionSide": "BOTH",
            "timeInForce": "GTC", "quantity": "0.014", "algoStatus": status,
            "actualOrderId": CHILD_ID.to_string(), "triggerPrice": "2234.67", "price": "2306.94",
            "reduceOnly": false, "workingType": "CONTRACT_PRICE"
        }))
    }

    /// `GET /fapi/v1/order` of the child LIMIT the trigger placed, under the stop's client id.
    fn child_row(status: &str) -> Answer {
        Ok(json!({
            "orderId": CHILD_ID, "symbol": "ETHUSDT", "status": status, "clientOrderId": "vtslD1",
            "price": "2306.94", "origQty": "0.014", "executedQty": "0", "type": "LIMIT",
            "timeInForce": "GTC", "side": "SELL", "positionSide": "BOTH", "reduceOnly": false
        }))
    }

    /// Confirm the member `vtslD1` against the two answers (`None` = that endpoint must not be asked:
    /// the scripted venue panics on it); the events' kinds, the accept's venue id, the requests sent.
    fn confirm(
        algo: Answer,
        child: Option<Answer>,
    ) -> (Vec<&'static str>, Option<String>, Vec<String>) {
        let mut venue = Venue::new().on("GET", PATH_ALGO_ORDER, algo);
        if let Some(child) = child {
            venue = venue.on("GET", PATH_ORDER, child);
        }
        let rest = perp_rest(venue, DEMO_USDM, "ETHUSDT", 2.0);
        rest.algo_order_client_ids.insert("vtslD1");
        let events = rest.confirm_order("vtslD1", 5);
        let kinds = events
            .iter()
            .map(|e| match e {
                Event::OrderAccepted(_) => "accepted",
                Event::OrderCanceled(_) => "canceled",
                Event::OrderExpired(_) => "expired",
                Event::OrderRejected(_) => "rejected",
                _ => "other",
            })
            .collect();
        let venue_id = events.iter().find_map(|e| match e {
            Event::OrderAccepted(a) => a.venue_order_id.as_deref().map(str::to_string),
            _ => None,
        });
        (kinds, venue_id, rest.transport.sent())
    }

    #[test]
    fn a_triggered_stop_limit_confirms_as_its_child_stands() {
        let both = vec!["GET /fapi/v1/algoOrder".to_string(), "GET /fapi/v1/order".to_string()];
        let algo_id = Some(ALGO_ID.to_string());
        for (algo_status, child_status, want) in [
            ("TRIGGERED", "NEW", &["accepted"][..]),
            ("TRIGGERED", "PARTIALLY_FILLED", &["accepted"][..]),
            ("TRIGGERED", "FILLED", &["accepted"][..]),
            ("TRIGGERED", "CANCELED", &["accepted", "canceled"][..]),
            ("FINISHED", "CANCELED", &["accepted", "canceled"][..]),
            ("TRIGGERED", "EXPIRED", &["accepted", "expired"][..]),
        ] {
            let got = confirm(algo_row("STOP", algo_status), Some(child_row(child_status)));
            assert_eq!(got, (want.to_vec(), algo_id.clone(), both.clone()), "{child_status}");
        }
        // no child on the regular endpoint yet, or a failed child read: the algo's answer, as before
        for child in [refusal(-2013, "Order does not exist."), refusal(-1003, "Too many requests.")]
        {
            let got = confirm(algo_row("STOP", "TRIGGERED"), Some(child));
            assert_eq!(got, (vec!["accepted"], algo_id.clone(), both.clone()));
        }
    }

    #[test]
    fn a_plain_stop_and_an_untriggered_stop_limit_are_answered_by_the_algo_endpoint_alone() {
        let one = vec!["GET /fapi/v1/algoOrder".to_string()];
        let algo_id = Some(ALGO_ID.to_string());
        for status in ["TRIGGERED", "FINISHED", "NEW"] {
            let got = confirm(algo_row("STOP_MARKET", status), None);
            assert_eq!(
                got,
                (vec!["accepted"], algo_id.clone(), one.clone()),
                "STOP_MARKET {status}"
            );
        }
        for status in ["NEW", "TRIGGERING"] {
            let got = confirm(algo_row("STOP", status), None);
            assert_eq!(got, (vec!["accepted"], algo_id.clone(), one.clone()), "STOP {status}");
        }
        let canceled = confirm(algo_row("STOP", "CANCELED"), None);
        assert_eq!(canceled.0, ["accepted", "canceled"]);
    }

    /// A TRIGGERED take-profit, either word, is answered by its child as the stop-limit is; an
    /// untriggered one by the algo endpoint alone. ⚠ The shapes are the stop-limit's measured ones with
    /// the take-profit words, not captures.
    #[test]
    fn a_triggered_take_profit_confirms_as_its_child_stands() {
        let both = vec!["GET /fapi/v1/algoOrder".to_string(), "GET /fapi/v1/order".to_string()];
        let one = vec!["GET /fapi/v1/algoOrder".to_string()];
        let algo_id = Some(ALGO_ID.to_string());
        for order_type in ["TAKE_PROFIT", "TAKE_PROFIT_MARKET"] {
            for (child_status, want) in [
                ("NEW", &["accepted"][..]),
                ("FILLED", &["accepted"][..]),
                ("CANCELED", &["accepted", "canceled"][..]),
                ("EXPIRED", &["accepted", "expired"][..]),
            ] {
                let got = confirm(algo_row(order_type, "TRIGGERED"), Some(child_row(child_status)));
                let pinned = (want.to_vec(), algo_id.clone(), both.clone());
                assert_eq!(got, pinned, "{order_type} {child_status}");
            }
            let got = confirm(algo_row(order_type, "NEW"), None);
            assert_eq!(got, (vec!["accepted"], algo_id.clone(), one.clone()), "{order_type} NEW");
        }
    }
}
