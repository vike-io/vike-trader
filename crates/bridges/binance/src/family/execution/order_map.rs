//! Shared Binance-wire-grammar pure order mappers reused by vike-aster (mappers rung, 4c).
//!
//! The pure, signing-independent, endpoint-path-independent slice of the two venues' signed REST
//! clients (`execution/rest/spot.rs`/`execution/rest/perp.rs`). Aster's spot API is a Binance-`/api/v3` fork and its perp API a
//! Binance-`/fapi` fork, so the numeric-id coercion, the `/exchangeInfo`→[`SymbolProperties`] parse
//! and the order-param *payload shaping* are byte-for-byte identical between them. Those live here
//! ONCE; each venue's `spot`/`perp` module keeps its struct + method surface and delegates the pure
//! body to these functions, so the golden fixtures (`r6_*`) and Aster's own tests still prove the
//! shared code twice — once per venue.
//!
//! **What is a caller-supplied parameter here (the family discipline).** A venue literal
//! (`crate::VENUE` = `"binance"` / `"aster"`) is a `venue` parameter here, and each venue passes its
//! own (the same shape [`crate::family::instrument`] uses). The order-param builders take `symbol` /
//! `properties` as parameters (they were `self.symbol` / `self.properties` fields), so nothing here
//! reaches into a struct. The param ORDER is load-bearing (r6 fixtures pin the exact byte output),
//! identical between the two venues; [`format_to_step_f`] stays the ONE pinned Decimal wire site.
//!
//! **Deliberately NOT here (out of this rung's scope).** The signed METHODS themselves (`submit_*`,
//! `cancel_*`, `set_leverage`, batch/modify, the audit-A3 reads)
//! name diverging endpoint paths (Binance's `/fapi/v1|v2` split vs Aster's single `/fapi/v3`) and
//! carry per-venue `tracing` targets (a `const` baked into a `static Metadata`, so not a runtime
//! parameter) — the same call the rung-4a/4b [`crate::family`] doc already makes. Also left
//! per-venue on purpose: the crate-local `PerpInstrument` type + `parse_*_perp_instruments` (r6 tests
//! construct it by name), the submit-event extraction, and `signing` / `urls` / `ratelimit` /
//! `recon_client`.

use std::sync::atomic::{AtomicBool, Ordering};

use indexmap::IndexMap;
use serde_json::Value;

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::trigger;

use vike_model::{OrderRequest, OrderType, SymbolProperties};
use vike_venues::venue_order_client_id_budget::BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET;

/// Binance/Aster ids arrive as JSON numbers or strings — Python-style `str(response.get("orderId",""))`.
/// One copy of what was four identical private `json_id` fns (spot + perp × two venues).
pub fn json_id(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Parse `/exchangeInfo` into per-symbol [`SymbolProperties`] (the venue-neutral type in
/// vike-model) — twin of `data/instrument_db.py::parse_symbol_properties`. The per-row grid is
/// [`crate::family::catalog::spot_properties`], the SAME parse the picker's catalog row carries, so
/// the grid an order is rounded to and the one the picker shows cannot disagree. This parser is
/// reached only from a `/api/v3/exchangeInfo` fetch (binance spot, or aster's fork of it), which
/// lists spot pairs and nothing else, so every row is `CryptoSpot` — unlike `/fapi`, whose
/// `/exchangeInfo` mixes perpetuals with dated futures and therefore has to read `contractType`
/// per entry (`crates/bridges/binance/src/execution/rest/perp.rs`'s `perp_asset_class`).
pub fn parse_symbol_properties(payload: &Value) -> IndexMap<String, SymbolProperties> {
    let mut out = IndexMap::new();
    let Some(symbols) = payload.get("symbols").and_then(|s| s.as_array()) else {
        return out;
    };
    for entry in symbols {
        let symbol = entry.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        out.insert(symbol, crate::family::catalog::spot_properties(entry));
    }
    out
}

/// Set by the first id sent bare for want of room, so the warning is once per process.
static BARE_WARNED: AtomicBool = AtomicBool::new(false);

/// Apply the Binance Broker/Link prefix to an order client id at the venue edge (`newClientOrderId`
/// on both lanes and the Algo Service's `clientAlgoId`): `x-<link_id>-<order client id>` when the whole of it
/// fits [`BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET`] (35), otherwise the order client id BARE, with one warning per process. Never
/// truncated: a cut tail can make two ids one, and a bare id loses only that order's attribution.
/// The answer depends on the two arguments alone, so a cancel or lookup re-derives the id the venue
/// stored. No link id → the order client id unchanged. Done here, not in the central `OrderClientIdGenerator`,
/// because the `-` in the prefix is rejected by `is_valid_crypto_order_client_id` (alphanumeric-only, ≤32
/// chars), so a bare local order client id can never itself start with `x-...-`. Its inverse, given the same
/// link id, is [`strip_broker_order_client_id_prefix`].
pub fn binance_broker_order_client_id(link_id: Option<&str>, order_client_id: &str) -> String {
    match link_id {
        Some(id)
            if "x-".len() + id.len() + "-".len() + order_client_id.len()
                <= BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET =>
        {
            format!("x-{id}-{order_client_id}")
        }
        Some(id) => {
            if !BARE_WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    order_client_id,
                    link_id_len = id.len(),
                    budget = BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET,
                    "an order client id too long for the broker prefix goes out BARE (never cut): \
                     this order and any later one like it carry no broker attribution"
                );
            }
            order_client_id.to_string()
        }
        None => order_client_id.to_string(),
    }
}

/// The prefix of the `cancelNewClientOrderId` the SPOT cancel-replace modify
/// (`crate::execution::rest::spot::VenueRestBinanceSpot::modify_order`) stamps on the CANCEL half of the pair.
///
/// A cancel-replace keeps the order's client id: the replacement is placed under the SAME
/// `newClientOrderId` as the order it replaces, because the engine's registry, the cancel path and
/// reconciliation all key on that one id. The consequence is that the venue then reports a
/// CANCELED order carrying that id, which would terminate the live order locally. The cancel
/// request's own id is the only thing that tells the two cancels apart (a cancel-family report puts
/// it in `c`, see `crate::family::execution::event_mapper::spot`'s `resting_order_client_id`), so it carries this prefix and
/// [`is_replace_cancel_id`] lets the stream mapper drop exactly that report. Not an `x-` shape, so
/// [`strip_broker_order_client_id_prefix`] leaves it alone.
pub const REPLACE_CANCEL_ID_PREFIX: &str = "vkrp";

/// Whether `c` (the cancel request's own id on a CANCELED report) is the throwaway id of the cancel
/// half of a cancel-replace modify — see [`REPLACE_CANCEL_ID_PREFIX`].
pub fn is_replace_cancel_id(c: &str) -> bool {
    c.starts_with(REPLACE_CANCEL_ID_PREFIX)
}

/// Inverse of [`binance_broker_order_client_id`]: the bare id behind an INBOUND venue-echoed order client id (WS
/// `executionReport`/`ORDER_TRADE_UPDATE` `c`/`C` and `ALGO_UPDATE` `caid`, the audit-A3 REST history's
/// `clientOrderId`/`clientAlgoId`, a reconcile snapshot's), the id the local registry keys orders by.
/// `link_id` is the broker link id the account's orders are placed under (`None` = none configured).
///
/// **An id is OURS only under OUR link id** (owner ruling V8, 2026-11-12): `x-<link_id>-<order client id>` with
/// exactly this `link_id` is stripped to `<order client id>`. Every other id comes back EXACTLY as received:
/// a bare id (ours with no link id configured or a prefix that would not fit, and aster's, which
/// never prefixes), and a FOREIGN one: another broker's `x-OTHER-...`, an id placed under a link id
/// this node was not told, and any `x-...` id when none is configured. A foreign id then matches no
/// local order, so the core shows it (an unknown order client id on the stream, an `UnknownOrder` to reconcile)
/// and never touches it. A hyphenated link id (`AB-12`, legal: `validate_code` checks the length
/// only) needs no separator search: the prefix is matched whole.
///
/// The ONE rule for every reader, the stream mappers, the history replay, reconcile and the algo
/// fill naming (`crate::execution::rest::algo`'s `own_order_client_id`), so they cannot read one id two ways.
pub fn strip_broker_order_client_id_prefix<'a>(stored: &'a str, link_id: Option<&str>) -> &'a str {
    link_id
        .and_then(|id| stored.strip_prefix("x-")?.strip_prefix(id)?.strip_prefix('-'))
        .unwrap_or(stored)
}

/// **The order client id a spot FILL carries**: `order_client_id` (broker prefix already stripped) when this workspace
/// could have minted it (`vike_model::is_valid_crypto_order_client_id`, the shape every id of ours has), else NONE (`""`).
/// An order placed by hand in the venue's app (`web_…`, `android_…`) or by another tool is not ours, and a
/// spot fill with no client id is the user's hand trade, which the fold keeps out of the strategy's position
/// (the spot rule, owner 2026-11-11; `vike_execution`'s fold). The order's lifecycle events keep the id as sent.
pub fn spot_fill_order_client_id(order_client_id: &str) -> String {
    if vike_model::is_valid_crypto_order_client_id(order_client_id) {
        order_client_id.to_string()
    } else {
        String::new()
    }
}

/// Spot order params (`newOrderRespType=ACK`) — pure, golden-gated. The response's `fills[]` is
/// deliberately ignored (the WS user-data stream is the sole fill source). qty/price format to the
/// symbol's step/tick as decimal strings via the pinned [`format_to_step_f`] site to dodge
/// BAD_PRECISION-class rejects. The param ORDER is load-bearing (fixtures pin it) and identical
/// between the two venues; whether the signed request then sorts params stays at each venue's call
/// site — this only shapes the vec. `symbol`/`properties` were `self.symbol`/`self.properties`.
/// `venue` selects the TIF row (family discipline: a caller-supplied literal, `"binance"` /
/// `"aster"`) — see [`limit_time_in_force_wire`]. `link_id` (unified cross-venue attribution) is the
/// resolved Binance Broker/Link id, applied to `newClientOrderId` via [`binance_broker_order_client_id`];
/// `None` (aster's callers always pass this, and binance's when unconfigured) is byte-identical to
/// before the field existed.
pub fn build_spot_order_params(
    request: &OrderRequest,
    symbol: &str,
    properties: &SymbolProperties,
    venue: &str,
    link_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut params: Vec<(&'static str, String)> = vec![
        ("symbol", symbol.to_string()),
        ("side", if request.side > 0 { "BUY" } else { "SELL" }.to_string()),
        ("type", spot_type_wire(&request.order_type)),
        // properties carry FLOATS (parse_symbol_properties) — the float twin reproduces
        // Python's str(float) step path exactly
        ("quantity", format_to_step_f(request.qty, properties.step_size)),
        ("newClientOrderId", binance_broker_order_client_id(link_id, &request.order_client_id)),
        ("newOrderRespType", "ACK".to_string()),
    ];
    if request.order_type == OrderType::Limit {
        if let Some(wire) = limit_time_in_force_wire(venue, request) {
            params.push(("timeInForce", wire.to_string()));
        }
        params
            .push(("price", format_to_step_f(request.price.unwrap_or(0.0), properties.tick_size)));
    }
    params
}

/// The spot `type` param: the request kind's word upper-cased, byte for byte what
/// `order_type.to_uppercase()` sent while the kind was a string (`STOP` and `TAKE_PROFIT` are not
/// spot types; the venue refuses them server-side, as it always did).
fn spot_type_wire(order_type: &OrderType) -> String {
    match order_type {
        OrderType::Market => "MARKET".to_string(),
        OrderType::Limit => "LIMIT".to_string(),
        OrderType::Stop => "STOP".to_string(),
        OrderType::StopLimit => "STOP_LIMIT".to_string(),
        OrderType::TakeProfit => "TAKE_PROFIT".to_string(),
        // the bytes `Unrecognized("market_to_limit")` sent before the kind had a variant
        OrderType::MarketToLimit => "MARKET_TO_LIMIT".to_string(),
        // the same, for `trailing`, `trailing_limit`, `pegged` and `trailing_take_profit`
        OrderType::Trailing => "TRAILING".to_string(),
        OrderType::TrailingLimit => "TRAILING_LIMIT".to_string(),
        OrderType::Pegged => "PEGGED".to_string(),
        OrderType::TrailingTakeProfit => "TRAILING_TAKE_PROFIT".to_string(),
        OrderType::Unrecognized(text) => text.to_uppercase(),
    }
}

/// The family's ONE limit-path TIF site — consumes the venue's `vike_venues::venue_time_in_force::venue_time_in_force`
/// row (tif step-2). Binance is LANE-SPLIT: the spot builder passes `"binance"` (GTC/IOC/FOK map
/// 1:1 — a default/GTC request emits the same `timeInForce=GTC` bytes as before the flip) and the
/// perp builder passes `"binance-perp"` (`vike_binance::execution::rest::perp::TIME_IN_FORCE_LANE` — same trio plus native
/// fapi GTD); aster's row is still `Ignored{GTC}` (no demo account to prove a flip), so its bytes
/// are unchanged. `None` (an `Unsupported` TIF that reached the builder despite the
/// `deny_unsupported_time_in_force` submit gate) emits NO `timeInForce` — Binance then rejects the LIMIT
/// loudly server-side (mandatory-param error), never a silent GTC.
fn limit_time_in_force_wire(venue: &str, request: &OrderRequest) -> Option<&'static str> {
    use vike_venues::venue_time_in_force::{TimeInForceOutcome, venue_time_in_force};
    match venue_time_in_force(venue, request.time_in_force) {
        TimeInForceOutcome::Mapped(w) | TimeInForceOutcome::Coerced { wire: w, .. } => Some(w),
        TimeInForceOutcome::Ignored { wire } => Some(wire),
        TimeInForceOutcome::NotEmitted | TimeInForceOutcome::Unsupported => None,
    }
}

/// Perp (fapi) order params — pure, golden-gated. positionSide BOTH (one-way; hedge LONG/SHORT is
/// a future gap); reduceOnly as the STRING `'true'/'false'`. `order_type:"stop"` (a bracket
/// stop-loss leg) maps to a native STOP_MARKET conditional (stopPrice = trigger) so the VENUE holds
/// the trigger — additive over the golden limit/market shapes (fixtures never send "stop", so their
/// byte output is unchanged). Param order is load-bearing and identical between the two venues.
/// `symbol`/`properties` were `self.symbol`/`self.properties`; `venue` selects the TIF row
/// (family discipline: a caller-supplied literal — binance's perp lane passes `"binance-perp"`,
/// whose `Mapped("GTD")` row makes the LIMIT arm emit the `goodTillDate` companion from
/// `gtd_expiry`) — see [`limit_time_in_force_wire`]. `link_id` — see [`build_spot_order_params`]'s doc.
pub fn build_perp_order_params(
    request: &OrderRequest,
    symbol: &str,
    properties: &SymbolProperties,
    venue: &str,
    link_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let type_str = match request.order_type {
        OrderType::Limit => "LIMIT",
        OrderType::Stop => "STOP_MARKET",
        OrderType::Market
        | OrderType::StopLimit
        | OrderType::TakeProfit
        | OrderType::MarketToLimit
        | OrderType::Trailing
        | OrderType::TrailingLimit
        | OrderType::Pegged
        | OrderType::TrailingTakeProfit
        | OrderType::Unrecognized(_) => "MARKET",
    };
    let mut params: Vec<(&'static str, String)> = vec![
        ("symbol", symbol.to_string()),
        ("side", if request.side > 0 { "BUY" } else { "SELL" }.to_string()),
        ("type", type_str.to_string()),
        ("quantity", format_to_step_f(request.qty, properties.step_size)),
        ("newClientOrderId", binance_broker_order_client_id(link_id, &request.order_client_id)),
        ("newOrderRespType", "ACK".to_string()),
        ("positionSide", "BOTH".to_string()), // one-way; hedge LONG/SHORT is a future gap
        ("reduceOnly", if request.reduce_only { "true" } else { "false" }.to_string()),
    ];
    match request.order_type {
        OrderType::Limit => {
            // tif step-2: same venue-row consumption as the spot builder — see `limit_time_in_force_wire`.
            if let Some(wire) = limit_time_in_force_wire(venue, request) {
                params.push(("timeInForce", wire.to_string()));
                // Native fapi GTD (the binance-perp lane's Mapped("GTD") row — no other row
                // emits this wire string here): `goodTillDate` is the venue-MANDATORY companion
                // of `timeInForce=GTD`, taken VERBATIM from the request's `gtd_expiry` (epoch
                // ms; the venue keeps second-level precision and ignores the ms part). A date
                // is NEVER invented: a dateless GTD that slipped past the submit gate
                // (`vike_binance::execution::rest::perp::deny_invalid_gtd`) emits no `goodTillDate`, so the
                // venue rejects the mandatory-param violation loudly — mirroring the
                // no-`timeInForce` philosophy of `limit_time_in_force_wire`. Additive param: every
                // non-GTD path (all goldens, all aster rows) is byte-identical.
                if let ("GTD", Some(exp)) = (wire, request.gtd_expiry) {
                    params.push(("goodTillDate", exp.to_string()));
                }
            }
            params.push((
                "price",
                format_to_step_f(request.price.unwrap_or(0.0), properties.tick_size),
            ));
        }
        OrderType::Stop => {
            params.push((
                "stopPrice",
                format_to_step_f(request.trigger_price.unwrap_or(0.0), properties.tick_size),
            ));
            // Trigger-source law: a requested `trigger_by` consumes the venue's
            // `vike_bridge_core::trigger::venue_trigger_by` row — binance's perp lane AND
            // aster (its fapi fork, same law, evidenced by aster's own API docs — see the
            // trigger module doc) both map Last/Mark onto fapi `workingType` (Index is denied
            // at submit, `deny_unsupported_trigger_by`). A `None` request emits no
            // `workingType` at all — the venue default CONTRACT_PRICE (last) rules,
            // byte-identical to before the field existed.
            if let Some(wire) =
                request.trigger_by.and_then(|s| trigger::venue_trigger_by(venue, s).wire())
            {
                params.push(("workingType", wire.to_string()));
            }
        }
        OrderType::Market
        | OrderType::StopLimit
        | OrderType::TakeProfit
        | OrderType::MarketToLimit
        | OrderType::Trailing
        | OrderType::TrailingLimit
        | OrderType::Pegged
        | OrderType::TrailingTakeProfit
        | OrderType::Unrecognized(_) => {}
    }
    params
}

#[path = "trigger_table_tests.rs"]
#[cfg(test)]
mod trigger_table_tests;

#[path = "time_in_force_table_tests.rs"]
#[cfg(test)]
mod time_in_force_table_tests;

#[path = "broker_order_client_id_tests.rs"]
#[cfg(test)]
mod broker_order_client_id_tests;
