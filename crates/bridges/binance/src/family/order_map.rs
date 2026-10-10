//! Shared Binance-wire-grammar pure order mappers reused by vike-aster (mappers rung, 4c).
//!
//! The pure, signing-independent, endpoint-path-independent slice of the two venues' signed REST
//! clients (`spot.rs`/`perp.rs`). Aster's spot API is a Binance-`/api/v3` fork and its perp API a
//! Binance-`/fapi` fork, so the numeric-id coercion, the `/exchangeInfo`→[`SymbolProperties`] parse,
//! the order-param *payload shaping*, and the resting-perp-order row map are byte-for-byte identical
//! between them. Those live here ONCE; each venue's `spot`/`perp` module keeps its struct + method
//! surface and delegates the pure body to these functions, so the golden fixtures (`r6_*`) and
//! Aster's own tests still prove the shared code twice — once per venue.
//!
//! **What is a caller-supplied parameter here (the family discipline).** [`map_perp_open_order`]
//! carried a HARDCODED venue literal in each venue's body (`crate::VENUE` = `"binance"` /
//! `"aster"`); `venue` is a parameter here, and each venue's thin wrapper passes its own literal (the
//! same shape [`crate::family::filters_rec`] uses). The order-param builders take `symbol` /
//! `properties` as parameters (they were `self.symbol` / `self.properties` fields), so nothing here
//! reaches into a struct. The param ORDER is load-bearing (r6 fixtures pin the exact byte output),
//! identical between the two venues; [`format_to_step_f`] stays the ONE pinned Decimal wire site.
//!
//! **Deliberately NOT here (out of this rung's scope).** The signed METHODS themselves (`submit_*`,
//! `cancel_*`, `connect`, `reconcile_positions`, `set_leverage`, batch/modify, the audit-A3 reads)
//! name diverging endpoint paths (Binance's `/fapi/v1|v2` split vs Aster's single `/fapi/v3`) and
//! carry per-venue `tracing` targets (a `const` baked into a `static Metadata`, so not a runtime
//! parameter) — the same call the rung-4a/4b [`crate::family`] doc already makes. Also left
//! per-venue on purpose: the crate-local `PerpInstrument` type + `parse_*_perp_instruments` (r6 tests
//! construct it by name), spot `connect`'s inline open-order mapping and its reconcile snapshot, the
//! submit-event extraction, and `signing` / `urls` / `ratelimit` / `recon_client`.

use indexmap::IndexMap;
use serde_json::Value;

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_num;
use vike_bridge_core::trigger;
use vike_exec::{ManagedOrder, OrderStatus};
use vike_model::{AssetClass, OrderRequest, SymbolProperties};

/// Binance/Aster ids arrive as JSON numbers or strings — Python-style `str(resp.get("orderId",""))`.
/// One copy of what was four identical private `json_id` fns (spot + perp × two venues).
pub fn json_id(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Parse `/exchangeInfo` into per-symbol [`SymbolProperties`] (the venue-neutral type in
/// vike-model) — twin of `data/instrument_db.py::parse_symbol_properties`. Filter fields arrive as
/// decimal STRINGS (`"0.00100000"`); `json_num` `float()`s them like Python.
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
        let filters: IndexMap<&str, &Value> = entry
            .get("filters")
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|f| f.get("filterType").and_then(|t| t.as_str()).map(|t| (t, f)))
                    .collect()
            })
            .unwrap_or_default();
        // fields arrive as decimal STRINGS ("0.00100000"); float() them like Python
        let f = |ftype: &str, field: &str| -> f64 {
            filters.get(ftype).and_then(|v| v.get(field)).and_then(json_num).unwrap_or(0.0)
        };
        let notional = filters
            .get("NOTIONAL")
            .or_else(|| filters.get("MIN_NOTIONAL"))
            .and_then(|v| v.get("minNotional"))
            .and_then(json_num)
            .unwrap_or(0.0);
        out.insert(
            symbol,
            SymbolProperties {
                tick_size: f("PRICE_FILTER", "tickSize"),
                step_size: f("LOT_SIZE", "stepSize"),
                min_qty: f("LOT_SIZE", "minQty"),
                max_qty: f("LOT_SIZE", "maxQty"),
                min_notional: notional,
                // The ENDPOINT is the venue's answer here, and it is an unambiguous one: this
                // parser is reached only from a `/api/v3/exchangeInfo` fetch (binance spot, or
                // aster's fork of it), which lists spot pairs and nothing else — unlike `/fapi`,
                // whose `/exchangeInfo` mixes perpetuals with dated futures and therefore has to
                // read `contractType` per entry (`crates/bridges/binance/src/perp.rs`'s
                // `perp_asset_class`). Recording it here is the cheapest honest moment: the fact
                // is free at the fetch and would otherwise be re-derived from the symbol text,
                // which `docs/decisions/0061-an-instrument-names-its-kind.md` exists to stop.
                asset_class: Some(AssetClass::CryptoSpot),
                // Everything else absent: spot is 1:1 with the base asset (multiplier 1.0), the
                // grid is flat (ONE `tickSize`, no tiers), and no venue taker hold. FRU rather
                // than an exhaustive literal so a new `SymbolProperties` field costs this parser
                // nothing.
                ..Default::default()
            },
        );
    }
    out
}

/// Apply the Binance Broker/Link prefix to a client order id at the venue edge. `x-<link_id>-<coid>`,
/// truncated to Binance's 36-char `newClientOrderId` ceiling (prefix preserved, coid tail dropped).
/// No link id → the coid unchanged. Done here, not in the central `ClientOrderIdGenerator`, because
/// the `-` in the prefix is rejected by `is_valid_crypto_coid` (alphanumeric-only, ≤32 chars) — the
/// same property that makes [`strip_broker_coid_prefix`] an unconditionally safe inverse: a bare
/// local coid can never itself start with `x-...-`.
pub fn binance_broker_coid(link_id: Option<&str>, coid: &str) -> String {
    match link_id {
        Some(id) => {
            let prefix = format!("x-{id}-");
            let budget = 36usize.saturating_sub(prefix.len());
            let tail: String = coid.chars().take(budget).collect();
            format!("{prefix}{tail}")
        }
        None => coid.to_string(),
    }
}

/// Inverse of [`binance_broker_coid`] — strips a leading `x-<link_id>-` broker prefix from an
/// INBOUND venue-echoed client_order_id (WS `executionReport`/`ORDER_TRADE_UPDATE` `c`/`C`, the
/// audit-A3 REST history's `clientOrderId`, and a reconcile snapshot's `clientOrderId`), recovering
/// the bare id the local registry keys orders by. Safe to call UNCONDITIONALLY, not just when a
/// link id is configured: every coid this codebase mints
/// (`vike_model::orders::client_order_id::ClientOrderIdGenerator`) is `^[A-Za-z0-9]{1,32}$`
/// (`is_valid_crypto_coid`) — it can never itself start with `x-` followed by another `-`, so a
/// string matching that shape can only be OUR broker prefix, never a coincidental bare id. A venue
/// (or Aster, which never prefixes — its callers pass `link_id: None`) that echoes a bare id back
/// unchanged passes through untouched.
///
/// Uses the LAST `-` (`rfind`, not `find`) as the prefix/coid separator: `validate_code`'s
/// `CoidPrefix` arm checks only the code's LENGTH, not its charset, so a hyphenated `link_id` (e.g.
/// `AB-12`) is a legal configured code and `binance_broker_coid` would stamp `x-AB-12-<coid>` —
/// splitting on the FIRST `-` would then cut inside the link id instead of at the true coid
/// boundary. The coid half is always hyphen-free (`is_valid_crypto_coid`, even after truncation),
/// so the LAST `-` is always the correct separator regardless of what the link id contains.
pub fn strip_broker_coid_prefix(coid: &str) -> &str {
    let Some(rest) = coid.strip_prefix("x-") else { return coid };
    match rest.rfind('-') {
        Some(idx) => &rest[idx + 1..],
        None => coid,
    }
}

/// Spot order params (`newOrderRespType=ACK`) — pure, golden-gated. The response's `fills[]` is
/// deliberately ignored (the WS user-data stream is the sole fill source). qty/price format to the
/// symbol's step/tick as decimal strings via the pinned [`format_to_step_f`] site to dodge
/// BAD_PRECISION-class rejects. The param ORDER is load-bearing (fixtures pin it) and identical
/// between the two venues; whether the signed request then sorts params stays at each venue's call
/// site — this only shapes the vec. `symbol`/`properties` were `self.symbol`/`self.properties`.
/// `venue` selects the TIF row (family discipline: a caller-supplied literal, `"binance"` /
/// `"aster"`) — see [`limit_tif_wire`]. `link_id` (unified cross-venue attribution) is the
/// resolved Binance Broker/Link id, applied to `newClientOrderId` via [`binance_broker_coid`];
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
        ("type", request.order_type.to_uppercase()),
        // properties carry FLOATS (parse_symbol_properties) — the float twin reproduces
        // Python's str(float) step path exactly
        ("quantity", format_to_step_f(request.qty, properties.step_size)),
        ("newClientOrderId", binance_broker_coid(link_id, &request.client_order_id)),
        ("newOrderRespType", "ACK".to_string()),
    ];
    if request.order_type.eq_ignore_ascii_case("limit") {
        if let Some(wire) = limit_tif_wire(venue, request) {
            params.push(("timeInForce", wire.to_string()));
        }
        params
            .push(("price", format_to_step_f(request.price.unwrap_or(0.0), properties.tick_size)));
    }
    params
}

/// The family's ONE limit-path TIF site — consumes the venue's `vike_model::venues::venue_tif::venue_tif`
/// row (tif step-2). Binance is LANE-SPLIT: the spot builder passes `"binance"` (GTC/IOC/FOK map
/// 1:1 — a default/GTC request emits the same `timeInForce=GTC` bytes as before the flip) and the
/// perp builder passes `"binance-perp"` (`vike_binance::perp::TIF_LANE` — same trio plus native
/// fapi GTD); aster's row is still `Ignored{GTC}` (no demo account to prove a flip), so its bytes
/// are unchanged. `None` (an `Unsupported` TIF that reached the builder despite the
/// `deny_unsupported_tif` submit gate) emits NO `timeInForce` — Binance then rejects the LIMIT
/// loudly server-side (mandatory-param error), never a silent GTC.
fn limit_tif_wire(venue: &str, request: &OrderRequest) -> Option<&'static str> {
    use vike_model::venues::venue_tif::{TifOutcome, venue_tif};
    match venue_tif(venue, request.time_in_force) {
        TifOutcome::Mapped(w) | TifOutcome::Coerced { wire: w, .. } => Some(w),
        TifOutcome::Ignored { wire } => Some(wire),
        TifOutcome::NotEmitted | TifOutcome::Unsupported => None,
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
/// `gtd_expiry`) — see [`limit_tif_wire`]. `link_id` — see [`build_spot_order_params`]'s doc.
pub fn build_perp_order_params(
    request: &OrderRequest,
    symbol: &str,
    properties: &SymbolProperties,
    venue: &str,
    link_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let ot = request.order_type.to_ascii_lowercase();
    let type_str = match ot.as_str() {
        "limit" => "LIMIT",
        "stop" => "STOP_MARKET",
        _ => "MARKET",
    };
    let mut params: Vec<(&'static str, String)> = vec![
        ("symbol", symbol.to_string()),
        ("side", if request.side > 0 { "BUY" } else { "SELL" }.to_string()),
        ("type", type_str.to_string()),
        ("quantity", format_to_step_f(request.qty, properties.step_size)),
        ("newClientOrderId", binance_broker_coid(link_id, &request.client_order_id)),
        ("newOrderRespType", "ACK".to_string()),
        ("positionSide", "BOTH".to_string()), // one-way; hedge LONG/SHORT is a future gap
        ("reduceOnly", if request.reduce_only { "true" } else { "false" }.to_string()),
    ];
    match ot.as_str() {
        "limit" => {
            // tif step-2: same venue-row consumption as the spot builder — see `limit_tif_wire`.
            if let Some(wire) = limit_tif_wire(venue, request) {
                params.push(("timeInForce", wire.to_string()));
                // Native fapi GTD (the binance-perp lane's Mapped("GTD") row — no other row
                // emits this wire string here): `goodTillDate` is the venue-MANDATORY companion
                // of `timeInForce=GTD`, taken VERBATIM from the request's `gtd_expiry` (epoch
                // ms; the venue keeps second-level precision and ignores the ms part). A date
                // is NEVER invented: a dateless GTD that slipped past the submit gate
                // (`vike_binance::perp::deny_invalid_gtd`) emits no `goodTillDate`, so the
                // venue rejects the mandatory-param violation loudly — mirroring the
                // no-`timeInForce` philosophy of `limit_tif_wire`. Additive param: every
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
        "stop" => {
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
        _ => {}
    }
    params
}

/// Map ONE fapi `/openOrders` JSON row → a reconcile-seeded [`ManagedOrder`] (status `ACCEPTED` —
/// the venue's resting truth). Pure + fixture-gated, the perp twin of spot `connect`'s open-order
/// mapping. Side BUY⇒+1 / else −1; `order_type` is the venue `type` lowercased; unpriced/market rows
/// carry `price: None` (the venue sends `""`, `"0"`, or `"0.00000000"`). Keyed by the ROW's own
/// `symbol` so a reaped drift check compares like-for-like; `venue` is a parameter (each venue's thin
/// wrapper passes `crate::VENUE`). `clientOrderId` is broker-prefix stripped (unified
/// cross-venue attribution, task 6 fix-round-1) — this snapshot-adopted order's coid must match the
/// registry's bare local coid, exactly like the WS mappers' decode.
pub fn map_perp_open_order(o: &Value, venue: &str) -> ManagedOrder {
    let side = if o.get("side").and_then(|s| s.as_str()) == Some("BUY") { 1 } else { -1 };
    let orig_qty = o.get("origQty").and_then(json_num).unwrap_or(0.0);
    // a non-finite wire qty ("NaN", "1e999") would serialize as JSON null and fail the typed decode below
    let orig_qty = if orig_qty.is_finite() { orig_qty } else { 0.0 };
    // price None for "", "0", "0.00000000" (unpriced/market rows)
    let price = o.get("price").and_then(|p| p.as_str()).and_then(|p| {
        if matches!(p, "" | "0" | "0.00000000") { None } else { p.parse::<f64>().ok() }
    });
    let coid =
        o.get("clientOrderId").and_then(|c| c.as_str()).map(strip_broker_coid_prefix).unwrap_or("");
    let request: OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid,
        "venue": venue,
        "symbol": o.get("symbol").and_then(|s| s.as_str()).unwrap_or(""),
        "side": side,
        "qty": orig_qty,
        "order_type": o.get("type").and_then(|t| t.as_str()).unwrap_or("").to_lowercase(),
        "price": price,
    }))
    .expect("static shape");
    let mut mo = ManagedOrder::new(request);
    mo.status = OrderStatus::Accepted;
    mo.venue_order_id = Some(o.get("orderId").map(json_id).unwrap_or_default());
    mo
}

#[path = "trigger_table_tests.rs"]
#[cfg(test)]
mod trigger_table_tests;

#[path = "tif_table_tests.rs"]
#[cfg(test)]
mod tif_table_tests;

#[path = "broker_coid_tests.rs"]
#[cfg(test)]
mod broker_coid_tests;
