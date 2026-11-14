//! R6 slice-3 golden parity: the Binance perp adapter against the FROZEN `fixtures/r6/perp.json`
//! bytes — ORDER_TRADE_UPDATE/ACCOUNT_UPDATE mapper events, fapi instruments, order params
//! (positionSide/reduceOnly deltas), and the reconcile matrix incl. the pinned SHORT-position and
//! hedge-legs gates, read through the `ReconcileClient` production runs.
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use vike_binance::execution::event_mapper::perp::map_binance_perp;
use vike_binance::execution::rest::perp::{VenueRestBinancePerp, parse_binance_perp_instruments};
use vike_bridge_core::execution_drive;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport, VenueApiError};
use vike_model::f64_to_hex_bits;

use crate::r6_binance_parity::pairs;
use crate::time_in_force_gate::NullSigner;

fn fixture() -> serde_json::Value {
    execution_drive::r6_fixture(env!("CARGO_MANIFEST_DIR"), "perp.json")
}

/// ⚠ **RELAXED in ONE field, under `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`'s
/// fence — and the relaxation is a stated function, not a tolerance.**
///
/// The mechanism that makes the frozen bytes unachievable: the exported oracle labelled every
/// execution with the frame's BARE `o.s` (`BTCUSDT`), and this port labels it with the perp SERIES
/// label, `o.s` + `vike_venues::PERP_SUFFIX` (`BTCUSDT.P`). It has to: production starts a perp
/// engine on the catalog spelling `BTCUSDT.P`, an engine folds a fill only when the fill names that
/// exact string, and the bare label dropped every live aster and binance perp execution from the
/// position (fixed 2026-10-03 — `crates/vike-bridge-core/tests/perp_fill_label.rs` is the money
/// proof). The oracle predates the suffix convention; nothing else about these events moved.
///
/// What keeps this a fence rather than a widened assertion:
/// 1. **The comparison stays EXACT** — whole events, value for value, against
///    [`as_this_port_labels_it`] of the oracle's own event, which changes exactly one string field
///    per execution and nothing else.
/// 2. **The oracle's raw value is still gated**: every relabelled field must equal the frame's own
///    `o.s` before the suffix is appended, so the function can only ever have suffixed the venue's
///    name for the instrument.
/// 3. **A control fails if the relaxation is a no-op** on this fixture: it must have relabelled
///    exactly the six execution fields the fixture carries (two fills, each bare + wrapped, and two
///    liquidations).
/// 4. **The committed fixture bytes are untouched.** The FUNDING events still compare raw, because
///    their label is the caller's symbol (a wallet row names no instrument) and this test still
///    passes the bare one it always did.
#[test]
fn perp_mapper_parity() {
    let fx = fixture();
    let mut relabelled = 0;
    for (i, case) in fx["mapper"].as_array().unwrap().iter().enumerate() {
        let got: Vec<serde_json::Value> =
            map_binance_perp(&case["frame"], "binance", "BTCUSDT", None)
                .iter()
                .map(|e| serde_json::to_value(e).unwrap())
                .collect();
        let want = case["events"].as_array().unwrap();
        assert_eq!(got.len(), want.len(), "mapper[{i}] event count: {got:?}");
        for (g, w) in got.iter().zip(want) {
            let (expected, n) = as_this_port_labels_it(w, &case["frame"], i);
            relabelled += n;
            assert_eq!(g, &expected, "mapper[{i}]");
        }
    }
    assert_eq!(
        relabelled, 6,
        "the label relaxation must have applied to every execution field the fixture carries \
         (2 fills x bare+wrap, 2 liquidations) — fewer means it silently stopped proving anything"
    );
}

/// The oracle's `want` event with each EXECUTION label (`FillEvent.symbol`,
/// `PositionLiquidated.symbol`, a fill wrap's `fill.symbol`) suffixed with
/// [`vike_venues::PERP_SUFFIX`] — after asserting the oracle's raw label IS the frame's `o.s`. Every
/// other field, and every other event type, is returned byte-for-byte. Returns the event and how
/// many labels it relabelled. See [`perp_mapper_parity`] for why.
fn as_this_port_labels_it(
    want: &serde_json::Value,
    frame: &serde_json::Value,
    case: usize,
) -> (serde_json::Value, usize) {
    let mut out = want.clone();
    let slot = match want["type"].as_str() {
        Some("FillEvent" | "PositionLiquidated") => out.get_mut("symbol"),
        Some("OrderFilled" | "OrderPartiallyFilled") => {
            out.get_mut("fill").and_then(|f| f.get_mut("symbol"))
        }
        _ => None,
    };
    let Some(slot) = slot else { return (out, 0) };
    let raw = slot.as_str().expect("an execution label is a string").to_string();
    assert_eq!(
        raw,
        frame["o"]["s"].as_str().expect("an execution frame names its instrument"),
        "mapper[{case}]: the oracle's raw label must be the frame's own `o.s` — the relaxation only \
         ever suffixes the venue's name for the instrument"
    );
    *slot = serde_json::Value::String(format!("{raw}{}", vike_venues::PERP_SUFFIX));
    (out, 1)
}

#[test]
fn perp_order_trade_update_surfaces_the_commission_asset() {
    // USDS-M ORDER_TRADE_UPDATE carries `N` = commission asset inside `o`.
    let frame = serde_json::json!({
        "e": "ORDER_TRADE_UPDATE",
        "o": {
            "x": "TRADE", "X": "FILLED", "c": "c1", "S": "BUY", "s": "BTCUSDT",
            "l": "1.0", "L": "100.0", "n": "0.5", "N": "USDT", "t": 7, "m": false, "ps": "BOTH"
        }
    });
    let fill = map_binance_perp(&frame, "binance", "BTCUSDT", None)
        .into_iter()
        .find_map(|e| match e {
            vike_model::events::Event::OrderFilled(f) => Some(f.fill),
            vike_model::events::Event::OrderPartiallyFilled(f) => Some(f.fill),
            _ => None,
        })
        .expect("a fill event");
    assert_eq!(fill.commission, 0.5);
    assert_eq!(fill.commission_asset, "USDT");
}

#[test]
fn perp_instruments_parity() {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    let want = fx["instruments"].as_object().unwrap();
    assert_eq!(instruments.len(), want.len());
    for (symbol, instrument) in &instruments {
        let w = &want[symbol];
        for (name, val) in [
            ("tick_size", instrument.properties.tick_size),
            ("step_size", instrument.properties.step_size),
            ("min_qty", instrument.properties.min_qty),
            ("max_qty", instrument.properties.max_qty),
            ("min_notional", instrument.properties.min_notional),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[name].as_str().unwrap(), "{symbol}.{name}");
        }
        assert_eq!(instrument.base_asset, w["base_asset"].as_str().unwrap(), "{symbol}.base_asset");
    }
}

// `perp_open_order_mapping` retired (unit A2): its mapper, the legacy snapshot's open-order row map,
// was deleted with the dead `Command::ApplySnapshot` path; production's open-order read is
// `crates/bridges/binance/src/family/recon.rs`'s `parse_perp_open_orders`, pinned by
// `tests/offline/reconcile_client_parse.rs`.

struct Stub {
    position_rows: serde_json::Value,
    balance_response: serde_json::Value,
    balance_fails: bool,
}

impl RestTransport for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/positionRisk") {
            Ok(self.position_rows.clone())
        } else if path.ends_with("/balance") {
            if self.balance_fails {
                Err(VenueApiError { code: 0, msg: "balance down".into() })
            } else {
                Ok(self.balance_response.clone())
            }
        } else {
            panic!("unexpected path {path}")
        }
    }

    vike_bridge_core::no_public_call!();
}

/// The frozen `order_params` block. Its reconcile matrix, once read here through the legacy snapshot
/// reader, was retired with that reader (unit A2, the dead `Command::ApplySnapshot` path): the same
/// frozen rows stay pinned through production's reader below.
#[test]
fn perp_params_parity() {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    let make = |rows: serde_json::Value, fails: bool| VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: Stub {
            position_rows: rows,
            balance_response: fx["balance_resp"].clone(),
            balance_fails: fails,
        },
        base_url: "https://stub".into(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };

    // build_order_params — ordered pairs vs the frozen `order_params` block (limit sell +
    // reduce-only market)
    let client = make(serde_json::json!([]), false);
    let limit_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "vtr6perpL", "venue": "binance", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.0034999999, "order_type": "limit", "price": 62584.567
    }))
    .unwrap();
    let market_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "vtr6perpM", "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.002, "order_type": "market", "reduce_only": true
    }))
    .unwrap();
    let got: Vec<(String, String)> = client
        .build_order_params(&limit_request)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, pairs(&fx["order_params"]["limit"]), "limit params");
    let got: Vec<(String, String)> = client
        .build_order_params(&market_request)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, pairs(&fx["order_params"]["market"]), "market params");
}

/// The frozen reconcile matrix (net long / NET SHORT / hedge / flat / balance-fail) through the reader
/// production runs, the perp `ReconcileClientBinance` (`crates/bridges/binance/src/family/recon.rs`'s
/// `parse_perp_position_risk` and `parse_perp_balance`). The fixture's `snapshot` blocks were
/// exported for the legacy snapshot reader, deleted with the dead `Command::ApplySnapshot` path
/// (unit A2); they stay frozen, and production's answer relates to them as follows:
///
/// - **flat rows:** production keeps EVERY row, a flat (`positionAmt` 0) leg included, because the
///   reconcile diff needs the row present; the frozen `snapshot` drops it (and holds one zero row
///   when nothing is left). The live legs agree bit for bit: quantity, entry price, side;
/// - **failed balance:** production's read is an `Err`; the frozen `snapshot` holds 0.0;
/// - **mark price:** production carries none (a position report has no mark); the frozen `snapshot`
///   does;
/// - **symbol:** production names a row by the row's own `symbol` as the `.P` series label (these
///   frozen rows carry none, so it is empty); the frozen `snapshot` names the client's bare symbol.
#[test]
fn production_reads_the_frozen_perp_rows_through_the_reconcile_client() {
    use vike_execution::recon::ReconcileClient;
    use vike_model::events::PositionSide;
    let fx = fixture();
    for case in fx["reconcile"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let fails = case["balance_fails"].as_bool().unwrap();
        let rows = case["position_rows"].as_array().unwrap();
        let recon = vike_binance::ReconcileClientBinance::perp(
            NullSigner,
            Stub {
                position_rows: case["position_rows"].clone(),
                balance_response: fx["balance_resp"].clone(),
                balance_fails: fails,
            },
            "https://stub",
            "BTCUSDT",
        );
        let want = &case["snapshot"];

        let balance = recon.fetch_balance();
        if fails {
            assert!(balance.is_err(), "{name}: production fails the read: {balance:?}");
            assert_eq!(want["balance"], "0000000000000000", "{name}: the snapshot booked 0.0");
        } else {
            let got = balance.unwrap().map(f64_to_hex_bits);
            assert_eq!(got.as_deref(), want["balance"].as_str(), "{name}: balance");
        }

        let reports = recon.fetch_position_status_reports().unwrap();
        assert_eq!(reports.len(), rows.len(), "{name}: production keeps every row");
        assert!(reports.iter().all(|r| r.symbol.is_empty()), "{name}: the row's own symbol");
        let live: Vec<_> = reports.iter().filter(|r| r.qty != 0.0).collect();
        let legs: Vec<(&str, &str)> = want["positions"]
            .as_array()
            .unwrap()
            .iter()
            .zip(want["position_average_price"].as_array().unwrap())
            .map(|(q, a)| (q[1].as_str().unwrap(), a[1].as_str().unwrap()))
            .filter(|(q, _)| *q != "0000000000000000")
            .collect();
        assert_eq!(live.len(), legs.len(), "{name}: the live legs");
        let sides = want["position_sides"].as_array().unwrap();
        for (i, (r, (qty, avg))) in live.iter().zip(&legs).enumerate() {
            assert_eq!(f64_to_hex_bits(r.qty), *qty, "{name}: qty");
            assert_eq!(f64_to_hex_bits(r.average_price), *avg, "{name}: avg_px");
            let side: PositionSide = sides
                .get(i)
                .map_or(PositionSide::Both, |s| serde_json::from_value(s[1].clone()).unwrap());
            assert_eq!(r.position_side, side, "{name}: side");
        }
    }
}

/// Stub for the native batch + modify endpoints: records the params and returns fapi-shaped
/// responses — batchOrders POST = an array (one order-ok, one error); PUT /order = the modified
/// order; DELETE batchOrders = an ok array.
struct OrderStub {
    last_params: std::cell::RefCell<Vec<(String, String)>>,
}

impl RestTransport for OrderStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        *self.last_params.borrow_mut() =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        if path.ends_with("/batchOrders") && method.eq_ignore_ascii_case("POST") {
            Ok(serde_json::json!([
                {"orderId": 111, "clientOrderId": "bat0", "status": "NEW"},
                {"code": -2019, "msg": "Margin is insufficient."}
            ]))
        } else if path.ends_with("/batchOrders") && method.eq_ignore_ascii_case("DELETE") {
            Ok(serde_json::json!([{"orderId": 111}, {"orderId": 112}]))
        } else if path.ends_with("/order") && method.eq_ignore_ascii_case("PUT") {
            Ok(serde_json::json!({
                "orderId": 999, "clientOrderId": "vtModify", "status": "NEW",
                "updateTime": 1_629_182_711_600_i64
            }))
        } else {
            panic!("unexpected {method} {path}")
        }
    }
    vike_bridge_core::no_public_call!();
}

fn perp_client() -> VenueRestBinancePerp<NullSigner, OrderStub> {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: OrderStub { last_params: std::cell::RefCell::new(Vec::new()) },
        base_url: "https://stub".into(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    }
}

fn limit(order_client_id: &str, side: i32, qty: f64, price: f64) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        side,
        ..execution_drive::limit("binance", order_client_id, "BTCUSDT", qty, price)
    }
}

#[test]
fn native_batch_submit_and_cancel() {
    let client = perp_client();
    let event =
        client.submit_batch(&[limit("bat0", 1, 0.002, 60000.0), limit("bat1", 1, 0.002, 60000.5)]);
    let kinds: Vec<String> = event
        .iter()
        .map(|e| serde_json::to_value(e).unwrap()["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, vec!["OrderSubmitted", "OrderSubmitted", "OrderAccepted", "OrderRejected"]);
    match (&event[2], &event[3]) {
        (
            vike_model::events::Event::OrderAccepted(a),
            vike_model::events::Event::OrderRejected(r),
        ) => {
            assert_eq!(a.order_client_id, "bat0");
            assert_eq!(a.venue_order_id.as_deref(), Some("111"));
            assert_eq!(r.order_client_id, "bat1");
            assert!(r.reason.contains("Margin"), "reason from the error element: {}", r.reason);
        }
        other => panic!("unexpected tail {other:?}"),
    }
    // the batchOrders param is a JSON array of 2 order objects
    let params = client.transport.last_params.borrow();
    let batch = params.iter().find(|(k, _)| k == "batchOrders").map(|(_, v)| v).unwrap();
    let arr: serde_json::Value = serde_json::from_str(batch).unwrap();
    assert_eq!(arr.as_array().map(|a| a.len()), Some(2), "batchOrders array");
    drop(params);

    let client = perp_client();
    assert!(client.cancel_batch(&["bat0".into(), "bat1".into()]).is_ok());
}

#[test]
fn native_modify_sends_side_and_both_qty_price() {
    let client = perp_client();
    // resting limit BUY qty 0.005 @ 60000; modify PRICE ONLY → qty must fall back to the resting
    // value (fapi modify requires BOTH), and `side` must be sent
    let order = limit("vtModify", 1, 0.005, 60000.0);
    let event = client.modify_order(&order, None, Some(61000.0));
    assert_eq!(event.len(), 1);
    match &event[0] {
        vike_model::events::Event::OrderModified(a) => {
            assert_eq!(a.order_client_id, "vtModify");
            assert_eq!(a.venue_order_id.as_deref(), Some("999"));
            assert_eq!(a.new_price, Some(61000.0));
            assert_eq!(
                a.timestamp, 1_629_182_711_600,
                "the venue's updateTime of the amend, never 0"
            );
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
    let params = client.transport.last_params.borrow();
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
    assert_eq!(get("side").as_deref(), Some("BUY"), "fapi modify requires side");
    assert_eq!(get("origClientOrderId").as_deref(), Some("vtModify"));
    assert!(get("quantity").is_some(), "quantity sent (fallback to resting qty)");
    assert!(get("price").is_some(), "price sent");
}

/// A bracket stop-loss leg is a CONDITIONAL order: since 2025-12-09 the venue refuses `STOP_MARKET` +
/// `stopPrice` on `/fapi/v1/order` (`-4120`), so binance builds it for `/fapi/v1/algoOrder` — the
/// renames the Algo Service made (`triggerPrice` for `stopPrice`, `clientAlgoId` for
/// `newClientOrderId`, plus `algoType`). The byte-for-byte golden is `src/execution/rest/algo_tests.rs`'s.
#[test]
fn stop_order_maps_to_a_conditional_algo_order() {
    let client = perp_client();
    let stop: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "sl1", "venue": "binance", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.005, "order_type": "stop", "trigger_price": 58000.0, "reduce_only": true
    }))
    .unwrap();
    let params = client.build_algo_params(&stop); // pure — no transport call
    let get = |k: &str| params.iter().find(|(pk, _)| *pk == k).map(|(_, v)| v.clone());
    assert_eq!(get("algoType").as_deref(), Some("CONDITIONAL"));
    assert_eq!(get("type").as_deref(), Some("STOP_MARKET"), "native conditional stop");
    assert!(get("triggerPrice").is_some(), "triggerPrice = trigger");
    assert_eq!(get("clientAlgoId").as_deref(), Some("sl1"));
    assert_eq!(get("reduceOnly").as_deref(), Some("true"));
    assert!(get("price").is_none(), "STOP_MARKET has no limit price");
    for refused in ["stopPrice", "newClientOrderId", "timeInForce", "closePosition", "priceProtect"]
    {
        assert!(get(refused).is_none(), "{refused} is not part of a binance stop");
    }
}

/// Stub whose PUT /order (modify) always fails — to exercise the modify-reject path (audit T2).
struct FailingModifyStub;
impl RestTransport for FailingModifyStub {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        Err(VenueApiError { code: -2013, msg: "Order does not exist.".into() })
    }
    vike_bridge_core::no_public_call!();
}

#[test]
fn native_modify_failure_emits_modify_rejected() {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    let client = VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: FailingModifyStub,
        base_url: "https://stub".into(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };
    let order = limit("vtModify", 1, 0.005, 60000.0);
    let event = client.modify_order(&order, None, Some(61000.0));
    assert_eq!(event.len(), 1, "a failed modify must not vanish");
    match &event[0] {
        vike_model::events::Event::OrderModifyRejected(r) => {
            assert_eq!(r.order_client_id, "vtModify");
            assert!(r.reason.contains("does not exist"), "carries venue reason: {}", r.reason);
        }
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
}

/// Audit T1: a submit whose POST returns the ambiguous-timeout sentinel must NOT emit a false
/// terminal — it re-queries order status (GET) and resolves live-vs-dead. `get` selects the GET
/// outcome: Some(Ok(has_order)) or an error code.
struct TimeoutThenStatusStub {
    get_has_order: Option<bool>, // Some(true)=venue has it, Some(false)=absent, None=GET also times out
    get_absent_code: i64,        // code used when get_has_order == Some(false)
}
impl RestTransport for TimeoutThenStatusStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/order") && method.eq_ignore_ascii_case("POST") {
            return Err(VenueApiError {
                code: E_TIMEOUT_AMBIGUOUS,
                msg: "network error: timeout".into(),
            });
        }
        // A conditional order is looked up on the Algo Service too once `/order` says absent; the
        // verdict is the same "absent", so the stub answers the same code for both endpoints.
        if (path.ends_with("/order") || path.ends_with("/algoOrder"))
            && method.eq_ignore_ascii_case("GET")
        {
            return match self.get_has_order {
                Some(true) => Ok(serde_json::json!({"orderId": 777, "status": "NEW"})),
                Some(false) => {
                    Err(VenueApiError { code: self.get_absent_code, msg: "absent".into() })
                }
                None => Err(VenueApiError {
                    code: E_TIMEOUT_AMBIGUOUS,
                    msg: "network error: timeout".into(),
                }),
            };
        }
        panic!("unexpected {method} {path}")
    }
    vike_bridge_core::no_public_call!();
}

fn perp_with(
    stub: TimeoutThenStatusStub,
) -> VenueRestBinancePerp<NullSigner, TimeoutThenStatusStub> {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: stub,
        base_url: "https://stub".into(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    }
}

#[test]
fn timeout_submit_with_live_order_resolves_to_accepted() {
    let client = perp_with(TimeoutThenStatusStub { get_has_order: Some(true), get_absent_code: 0 });
    let event = client.submit_order(&limit("t1", 1, 0.005, 60000.0));
    assert_eq!(
        execution_drive::last_kind(&event),
        "Accepted",
        "timed-out-but-live order must be managed, not a phantom"
    );
}

#[test]
fn timeout_submit_venue_absent_says_nothing() {
    // -2013 = not listed, read just after the POST: it may still land, so nothing is said and the
    // stuck-order ladder's confirm asks again.
    let client =
        perp_with(TimeoutThenStatusStub { get_has_order: Some(false), get_absent_code: -2013 });
    let event = client.submit_order(&limit("t2", 1, 0.005, 60000.0));
    assert_eq!(event.len(), 1, "only OrderSubmitted: {event:?}");
}

#[test]
fn timeout_submit_double_timeout_claims_nothing_never_a_false_terminal() {
    let client = perp_with(TimeoutThenStatusStub { get_has_order: None, get_absent_code: 0 });
    let event = client.submit_order(&limit("t3", 1, 0.005, 60000.0));
    assert!(
        !event.iter().any(|e| matches!(
            e,
            vike_model::events::Event::OrderAccepted(_)
                | vike_model::events::Event::OrderRejected(_)
        )),
        "double-timeout: neither a phantom accept nor a false terminal; the watchdog's confirm decides: {event:?}"
    );
}

/// Audit T1 regression: a DEFINITE failure (connect-refused code 0, or a venue business error)
/// must STILL be a terminal reject and must NOT trigger a status re-query — only the ambiguous
/// timeout sentinel diverts. The stub panics on any GET to prove no requery happens.
#[test]
fn definite_failure_still_terminal_rejects_without_requery() {
    struct RejectStub(i64);
    impl RestTransport for RejectStub {
        fn signed(
            &self,
            _b: &str,
            path: &str,
            method: &str,
            _p: &[(&str, String)],
            _s: &dyn Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            if path.ends_with("/order") && method.eq_ignore_ascii_case("POST") {
                return Err(VenueApiError { code: self.0, msg: "boom".into() });
            }
            panic!("definite reject must NOT re-query the venue (saw {method} {path})");
        }
        vike_bridge_core::no_public_call!();
    }
    for code in [0_i64, -2010] {
        let fx = fixture();
        let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
        let client = VenueRestBinancePerp {
            link_id: None,
            signer: NullSigner,
            transport: RejectStub(code),
            base_url: "https://stub".into(),
            ty: vike_venues::venue_urls::UrlType::Live,
            margining: vike_model::Margining::Linear,
            algo_order_client_ids: Default::default(),
            symbol: "BTCUSDT".into(),
            properties: instruments["BTCUSDT"].properties,
            leverage: 2.0,
        };
        let event = client.submit_order(&limit("d1", 1, 0.005, 60000.0));
        assert_eq!(
            execution_drive::last_kind(&event),
            "Rejected",
            "definite failure (code {code}) must stay terminal"
        );
    }
}
