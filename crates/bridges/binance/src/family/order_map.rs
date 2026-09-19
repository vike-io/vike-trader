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
use vike_model::{OrderRequest, SymbolProperties};

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
/// (`vike_exec::client_order_id::ClientOrderIdGenerator`) is `^[A-Za-z0-9]{1,32}$`
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

/// The family's ONE limit-path TIF site — consumes the venue's `vike_bridge_core::tif::venue_tif`
/// row (tif step-2). Binance is LANE-SPLIT: the spot builder passes `"binance"` (GTC/IOC/FOK map
/// 1:1 — a default/GTC request emits the same `timeInForce=GTC` bytes as before the flip) and the
/// perp builder passes `"binance-perp"` (`vike_binance::perp::TIF_LANE` — same trio plus native
/// fapi GTD); aster's row is still `Ignored{GTC}` (no demo account to prove a flip), so its bytes
/// are unchanged. `None` (an `Unsupported` TIF that reached the builder despite the
/// `deny_unsupported_tif` submit gate) emits NO `timeInForce` — Binance then rejects the LIMIT
/// loudly server-side (mandatory-param error), never a silent GTC.
fn limit_tif_wire(venue: &str, request: &OrderRequest) -> Option<&'static str> {
    use vike_bridge_core::tif::{TifOutcome, venue_tif};
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

#[cfg(test)]
mod trigger_table_tests {
    //! Pins the binance-perp AND aster (same `Mapped` CONTRACT_PRICE/MARK_PRICE, Index denied at
    //! submit — aster's fapi fork documents the same `workingType` param, see the trigger module
    //! doc) rows of the ONE cross-venue trigger-source authority
    //! (`vike_bridge_core::trigger::venue_trigger_by`) against THIS shared family builder, which
    //! CONSUMES the rows on its perp STOP arm. Byte-identity pin: a `None` request emits no
    //! `workingType` at all — the venue default CONTRACT_PRICE rules, the exact pre-`trigger_by`
    //! bytes.
    use vike_bridge_core::trigger::{TriggerByOutcome, venue_trigger_by};
    use vike_model::TriggerBy::{Index, Last, Mark};
    use vike_model::{OrderRequest, SymbolProperties, TriggerBy};

    use super::build_perp_order_params;
    use crate::perp::TIF_LANE;

    fn stop_req(tb: Option<TriggerBy>) -> OrderRequest {
        OrderRequest {
            client_order_id: "c-trig".to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: -1,
            qty: 0.01,
            order_type: "stop".to_string(),
            trigger_price: Some(58000.0),
            reduce_only: true,
            trigger_by: tb,
            ..Default::default()
        }
    }

    fn working_type(params: &[(&'static str, String)]) -> Option<String> {
        params.iter().find(|(k, _)| *k == "workingType").map(|(_, v)| v.clone())
    }

    /// Byte-identity: a stop with NO requested source emits exactly the pre-field params —
    /// `stopPrice` is the tail, no `workingType` anywhere.
    #[test]
    fn perp_stop_without_trigger_by_is_byte_identical() {
        let props = SymbolProperties::default();
        let params = build_perp_order_params(&stop_req(None), "BTCUSDT", &props, TIF_LANE, None);
        assert_eq!(working_type(&params), None, "None = venue default CONTRACT_PRICE, no param");
        assert_eq!(params.last().unwrap().0, "stopPrice", "stopPrice stays the stop tail");
    }

    #[test]
    fn perp_stop_maps_last_and_mark_onto_working_type() {
        let props = SymbolProperties::default();
        for (tb, wire) in [(Last, "CONTRACT_PRICE"), (Mark, "MARK_PRICE")] {
            assert_eq!(venue_trigger_by(TIF_LANE, tb), TriggerByOutcome::Mapped(wire), "{tb:?}");
            let params =
                build_perp_order_params(&stop_req(Some(tb)), "BTCUSDT", &props, TIF_LANE, None);
            assert_eq!(working_type(&params).as_deref(), Some(wire), "{tb:?}");
            // workingType directly follows stopPrice (additive tail — nothing reordered)
            assert_eq!(params[params.len() - 2].0, "stopPrice");
        }
    }

    /// Index is denied at submit (`deny_unsupported_trigger_by`, pinned in
    /// `tests/offline/trigger_gate.rs`); if it reaches the builder anyway, NO workingType is emitted —
    /// never a silently substituted series.
    #[test]
    fn perp_stop_index_emits_no_working_type() {
        assert_eq!(venue_trigger_by(TIF_LANE, Index), TriggerByOutcome::Unsupported);
        let props = SymbolProperties::default();
        let params =
            build_perp_order_params(&stop_req(Some(Index)), "BTCUSDT", &props, TIF_LANE, None);
        assert_eq!(working_type(&params), None);
    }

    /// Aster shares binance-perp's law exactly (its fapi fork documents the same `workingType`
    /// param — see the trigger module doc, not assumed from binance alone): Last/Mark map onto
    /// `workingType`, directly following `stopPrice` (additive tail — nothing reordered).
    #[test]
    fn aster_stop_maps_last_and_mark_onto_working_type() {
        let props = SymbolProperties::default();
        for (tb, wire) in [(Last, "CONTRACT_PRICE"), (Mark, "MARK_PRICE")] {
            assert_eq!(venue_trigger_by("aster", tb), TriggerByOutcome::Mapped(wire), "{tb:?}");
            let params =
                build_perp_order_params(&stop_req(Some(tb)), "BTCUSDT", &props, "aster", None);
            assert_eq!(working_type(&params).as_deref(), Some(wire), "{tb:?}");
            assert_eq!(params[params.len() - 2].0, "stopPrice");
        }
    }

    /// Index has no fapi index `workingType` on aster either (same fork, same gap): denied at
    /// submit (`deny_unsupported_trigger_by`, pinned in `aster`'s `tests/trigger_gate.rs`); if it
    /// reaches the builder anyway, NO workingType is emitted — never a silently substituted series.
    #[test]
    fn aster_stop_index_emits_no_working_type() {
        assert_eq!(venue_trigger_by("aster", Index), TriggerByOutcome::Unsupported);
        let props = SymbolProperties::default();
        let params =
            build_perp_order_params(&stop_req(Some(Index)), "BTCUSDT", &props, "aster", None);
        assert_eq!(working_type(&params), None);
        assert_eq!(params.last().unwrap().0, "stopPrice", "unchanged tail");
    }

    /// The field is stop-arm-only: limit/market params never grow a workingType, whatever the
    /// request carries.
    #[test]
    fn non_stop_orders_carry_no_working_type() {
        let props = SymbolProperties::default();
        for ot in ["limit", "market"] {
            let mut req = stop_req(Some(Mark));
            req.order_type = ot.to_string();
            req.price = Some(50000.0);
            req.trigger_price = None;
            let params = build_perp_order_params(&req, "BTCUSDT", &props, TIF_LANE, None);
            assert_eq!(working_type(&params), None, "{ot}");
        }
    }
}

#[cfg(test)]
mod tif_table_tests {
    //! Pins the binance spot (`"binance"`: Mapped GTC/IOC/FOK, Unsupported GTD/Day), binance
    //! perp (`"binance-perp"` lane sub-key: same trio plus native Mapped GTD, Unsupported Day)
    //! and aster (still `Ignored{GTC}`) rows of the ONE cross-venue TIF authority
    //! (`vike_bridge_core::tif::venue_tif`) against THIS shared family builder, which
    //! CONSUMES the rows on its LIMIT path. Byte-identity pins: a default/GTC binance request
    //! and EVERY aster request still emit `timeInForce=GTC` in the same slot; market orders
    //! carry no TIF param at all; an Unsupported TIF (gated at submit) emits no TIF param;
    //! `goodTillDate` exists ONLY on the perp lane's GTD row and is never invented.
    use vike_bridge_core::tif::{TifOutcome, venue_tif};
    use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
    use vike_model::{OrderRequest, SymbolProperties, TimeInForce};

    use super::{build_perp_order_params, build_spot_order_params};
    use crate::perp::TIF_LANE;

    fn limit_req(tif: TimeInForce) -> OrderRequest {
        OrderRequest {
            client_order_id: "c-tif".to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 0.01,
            order_type: "limit".to_string(),
            price: Some(50000.0),
            time_in_force: tif,
            ..Default::default()
        }
    }

    fn tif_param(params: &[(&'static str, String)]) -> Option<String> {
        params.iter().find(|(k, _)| *k == "timeInForce").map(|(_, v)| v.clone())
    }

    fn gtd_param(params: &[(&'static str, String)]) -> Option<String> {
        params.iter().find(|(k, _)| *k == "goodTillDate").map(|(_, v)| v.clone())
    }

    #[test]
    fn binance_limit_orders_honor_request_tif() {
        let props = SymbolProperties::default();
        for (tif, wire) in [(Gtc, "GTC"), (Ioc, "IOC"), (Fok, "FOK")] {
            assert_eq!(venue_tif("binance", tif), TifOutcome::Mapped(wire), "{tif:?}");
            assert_eq!(venue_tif(TIF_LANE, tif), TifOutcome::Mapped(wire), "{tif:?} (perp lane)");
            let req = limit_req(tif);
            for params in [
                build_spot_order_params(&req, "BTCUSDT", &props, "binance", None),
                build_perp_order_params(&req, "BTCUSDT", &props, TIF_LANE, None),
            ] {
                assert_eq!(tif_param(&params).as_deref(), Some(wire), "{tif:?}");
                assert_eq!(gtd_param(&params), None, "{tif:?}: goodTillDate is GTD-only");
            }
        }
    }

    /// Byte-identity: the default request (TIF unset = Gtc) emits the exact params the
    /// pre-flip hardcode emitted — `timeInForce=GTC` in the same position, same bytes.
    #[test]
    fn binance_default_tif_bytes_are_unchanged() {
        let req = limit_req(TimeInForce::default());
        let props = SymbolProperties::default();
        let spot = build_spot_order_params(&req, "BTCUSDT", &props, "binance", None);
        assert_eq!(spot[6], ("timeInForce", "GTC".to_string()), "same slot, same bytes");
        let perp = build_perp_order_params(&req, "BTCUSDT", &props, TIF_LANE, None);
        assert_eq!(perp[8], ("timeInForce", "GTC".to_string()), "same slot, same bytes");
        assert_eq!(perp[9].0, "price", "no param slots in between");
    }

    /// Unsupported rows emit no param — the submit gate rejects before the builder runs
    /// (pinned in `spot.rs`/`perp.rs`); if one reaches the builder anyway, NO timeInForce param
    /// is emitted (Binance rejects a TIF-less LIMIT loudly server-side, never a silent GTC).
    /// Spot lane: GTD and Day; perp lane: Day (its GTD is Mapped — see the wire test below).
    #[test]
    fn binance_unsupported_tif_emits_no_param() {
        let props = SymbolProperties::default();
        for tif in [Gtd, Day] {
            assert_eq!(venue_tif("binance", tif), TifOutcome::Unsupported, "{tif:?}");
            let req = limit_req(tif);
            let params = build_spot_order_params(&req, "BTCUSDT", &props, "binance", None);
            assert_eq!(tif_param(&params), None, "{tif:?} (spot)");
            assert_eq!(gtd_param(&params), None, "{tif:?} (spot): never a goodTillDate");
        }
        assert_eq!(venue_tif(TIF_LANE, Day), TifOutcome::Unsupported);
        let params = build_perp_order_params(&limit_req(Day), "BTCUSDT", &props, TIF_LANE, None);
        assert_eq!(tif_param(&params), None, "Day (perp)");
        assert_eq!(gtd_param(&params), None, "Day (perp)");
    }

    /// The perp lane's native GTD wire shape: `timeInForce=GTD` with the venue-mandatory
    /// `goodTillDate` companion (verbatim `gtd_expiry` epoch ms) in the NEXT slot, then price —
    /// the exact positions the golden order established for the limit tail.
    #[test]
    fn binance_perp_gtd_wires_time_in_force_and_good_till_date() {
        assert_eq!(venue_tif(TIF_LANE, Gtd), TifOutcome::Mapped("GTD"));
        let props = SymbolProperties::default();
        let mut req = limit_req(Gtd);
        req.gtd_expiry = Some(1_770_736_694_000);
        let params = build_perp_order_params(&req, "BTCUSDT", &props, TIF_LANE, None);
        assert_eq!(params[8], ("timeInForce", "GTD".to_string()), "exact slot");
        assert_eq!(params[9], ("goodTillDate", "1770736694000".to_string()), "exact slot");
        assert_eq!(params[10].0, "price", "price stays the limit tail");
        // the SPOT builder never emits GTD even with an expiry set — its row is Unsupported
        let spot = build_spot_order_params(&req, "BTCUSDT", &props, "binance", None);
        assert_eq!(tif_param(&spot), None, "spot has no GTD");
        assert_eq!(gtd_param(&spot), None, "spot has no goodTillDate");
    }

    /// A date is NEVER invented: a dateless GTD that slipped past the submit gate
    /// (`crate::perp::deny_invalid_gtd`) emits `timeInForce=GTD` with NO `goodTillDate` — the
    /// venue then rejects the mandatory-param violation loudly, never a made-up expiry.
    #[test]
    fn binance_perp_dateless_gtd_emits_no_good_till_date() {
        let props = SymbolProperties::default();
        let req = limit_req(Gtd); // gtd_expiry stays None
        let params = build_perp_order_params(&req, "BTCUSDT", &props, TIF_LANE, None);
        assert_eq!(tif_param(&params).as_deref(), Some("GTD"));
        assert_eq!(gtd_param(&params), None, "no invented goodTillDate");
        assert_eq!(params[9].0, "price", "price directly follows timeInForce");
    }

    /// Aster is UNFLIPPED: every request TIF still rests GTC (the `Ignored{GTC}` row), so its
    /// wire bytes are byte-identical to before the family builder grew the venue parameter —
    /// and the perp-lane GTD companion NEVER leaks onto aster's wire, even with an expiry set.
    #[test]
    fn aster_limit_orders_still_ignore_request_tif() {
        let props = SymbolProperties::default();
        for tif in [Gtc, Ioc, Fok, Gtd, Day] {
            assert_eq!(venue_tif("aster", tif), TifOutcome::Ignored { wire: "GTC" }, "{tif:?}");
            let mut req = limit_req(tif);
            req.gtd_expiry = Some(1_770_736_694_000); // must never reach aster's wire
            for params in [
                build_spot_order_params(&req, "BTCUSDT", &props, "aster", None),
                build_perp_order_params(&req, "BTCUSDT", &props, "aster", None),
            ] {
                assert_eq!(
                    tif_param(&params).as_deref(),
                    Some("GTC"),
                    "aster LIMIT rests GTC regardless of requested {tif:?}"
                );
                assert_eq!(gtd_param(&params), None, "no goodTillDate on aster ({tif:?})");
            }
        }
    }

    #[test]
    fn family_market_orders_carry_no_tif_param() {
        let mut req = limit_req(Ioc);
        req.order_type = "market".to_string();
        req.price = None;
        req.gtd_expiry = Some(1_770_736_694_000); // never emitted off the limit path either
        let props = SymbolProperties::default();
        for venue in ["binance", TIF_LANE, "aster"] {
            for params in [
                build_spot_order_params(&req, "BTCUSDT", &props, venue, None),
                build_perp_order_params(&req, "BTCUSDT", &props, venue, None),
            ] {
                assert!(params.iter().all(|(k, _)| *k != "timeInForce"), "{venue}");
                assert!(params.iter().all(|(k, _)| *k != "goodTillDate"), "{venue}");
            }
        }
    }
}

#[cfg(test)]
mod broker_coid_tests {
    //! Unified cross-venue attribution (task 6): [`binance_broker_coid`]/[`strip_broker_coid_prefix`]
    //! are the encode/decode pair for Binance's Broker/Link `newClientOrderId` prefix. No link id ⇒
    //! byte-identical (bare coid); a configured link id prefixes `newClientOrderId` on BOTH builders,
    //! bounded to the venue's 36-char ceiling.
    use super::{
        binance_broker_coid, build_perp_order_params, build_spot_order_params,
        strip_broker_coid_prefix,
    };
    use vike_model::{OrderRequest, SymbolProperties};

    #[test]
    fn broker_coid_prefixes_and_bounds_36() {
        // no link id → unchanged
        assert_eq!(binance_broker_coid(None, "abc123"), "abc123");
        // with link id → x-<id>-<coid>, and the whole thing stays <= 36
        let c = binance_broker_coid(Some("ABC123"), "deadbeef01");
        assert!(c.starts_with("x-ABC123-"));
        assert!(c.len() <= 36, "newClientOrderId must fit 36, got {}", c.len());
        // an over-long coid is truncated to keep the prefix and the 36 bound
        let long = binance_broker_coid(Some("ABC123"), &"z".repeat(40));
        assert!(long.starts_with("x-ABC123-"));
        assert_eq!(long.len(), 36);
    }

    /// The inverse recovers the bare coid exactly, round-tripping through the encode side.
    #[test]
    fn strip_is_the_inverse_of_prefix_for_untruncated_ids() {
        let prefixed = binance_broker_coid(Some("ABC123"), "deadbeef01");
        assert_eq!(strip_broker_coid_prefix(&prefixed), "deadbeef01");
        // no link id: encode is a no-op, decode is a no-op too
        let bare = binance_broker_coid(None, "deadbeef01");
        assert_eq!(strip_broker_coid_prefix(&bare), "deadbeef01");
    }

    /// A hyphenated link id (legal: `validate_code`'s `CoidPrefix` arm checks length only, not
    /// charset) must still round-trip — the separator is the LAST `-`, not the first, since the
    /// coid half is always hyphen-free.
    #[test]
    fn strip_uses_last_hyphen_so_a_hyphenated_link_id_round_trips() {
        let prefixed = binance_broker_coid(Some("AB-12"), "deadbeef01");
        assert_eq!(prefixed, "x-AB-12-deadbeef01");
        assert_eq!(strip_broker_coid_prefix(&prefixed), "deadbeef01");
    }

    /// A string that never carried the prefix (no link id configured, or Aster's always-bare coids)
    /// passes through the decode side unchanged — no false-positive strip.
    #[test]
    fn strip_is_a_no_op_on_bare_ids() {
        assert_eq!(strip_broker_coid_prefix("deadbeef01"), "deadbeef01");
        assert_eq!(strip_broker_coid_prefix(""), "");
        // starts with "x-" but no second '-': not our shape, left alone
        assert_eq!(strip_broker_coid_prefix("x-onlyone"), "x-onlyone");
    }

    /// Both builders stamp the prefixed id into `newClientOrderId` when a link id is configured;
    /// absent, the wire byte is exactly the bare coid (byte-identical to before this field existed).
    #[test]
    fn build_order_params_apply_link_id_to_new_client_order_id() {
        let props = SymbolProperties::default();
        let req = OrderRequest {
            client_order_id: "deadbeef01".to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 0.01,
            order_type: "market".to_string(),
            ..Default::default()
        };
        let ncoid = |params: &[(&'static str, String)]| {
            params
                .iter()
                .find(|(k, _)| *k == "newClientOrderId")
                .map(|(_, v)| v.clone())
                .expect("newClientOrderId present")
        };

        let spot_none = build_spot_order_params(&req, "BTCUSDT", &props, "binance", None);
        assert_eq!(ncoid(&spot_none), "deadbeef01", "no link id: bare coid, byte-identical");
        let spot_some = build_spot_order_params(&req, "BTCUSDT", &props, "binance", Some("ABC123"));
        assert_eq!(ncoid(&spot_some), "x-ABC123-deadbeef01");

        let perp_none = build_perp_order_params(&req, "BTCUSDT", &props, "binance-perp", None);
        assert_eq!(ncoid(&perp_none), "deadbeef01", "no link id: bare coid, byte-identical");
        let perp_some =
            build_perp_order_params(&req, "BTCUSDT", &props, "binance-perp", Some("ABC123"));
        assert_eq!(ncoid(&perp_some), "x-ABC123-deadbeef01");
    }
}
