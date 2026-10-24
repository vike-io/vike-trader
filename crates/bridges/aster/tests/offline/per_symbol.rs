//! One aster lane, many symbols (offline): `vike_binance::per_symbol::PerSymbolRest` over aster's
//! clients signs every order, cancel, modify and status query on the ORDER's own symbol with THAT
//! symbol's grid. `crates/bridges/binance/tests/offline/per_symbol.rs` is the full pin of the router
//! (mounted default, fetch once, unknown coid, failure); this twin proves aster's own wire paths
//! (`/fapi/v3`, `/api/v3`) and that a failed grid fetch is a terminal rejection.

use std::assert_matches;
use std::sync::{Arc, Mutex};

use vike_aster::perp::AsterPerpRest;
use vike_aster::spot::AsterSpotRest;
use vike_binance::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties};

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

type Call = (String, String, Vec<(String, String)>);

/// Every signed request `(method, path, params)`, from every client of one router, in order.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Call>>>);

impl Log {
    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
}

struct Wire(Log);
impl RestTransport for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.0.0.lock().unwrap().push((method.to_string(), path.to_string(), owned));
        Ok(serde_json::json!({ "orderId": 777, "status": "NEW" }))
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("no public call expected ({path})")
    }
}

fn param(call: &Call, key: &str) -> Option<String> {
    call.2.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

fn grid(tick: f64, step: f64) -> SymbolProperties {
    SymbolProperties { tick_size: tick, step_size: step, ..Default::default() }
}

fn perp(symbol: &str, tick: f64, step: f64, log: &Log) -> AsterPerpRest<NullSigner, Wire> {
    AsterPerpRest {
        signer: NullSigner,
        transport: Wire(log.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: symbol.to_string(),
        properties: grid(tick, step),
        leverage: 1.0,
        builder: None,
    }
}

fn spot(symbol: &str, tick: f64, step: f64, log: &Log) -> AsterSpotRest<NullSigner, Wire> {
    AsterSpotRest {
        signer: NullSigner,
        transport: Wire(log.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: symbol.to_string(),
        properties: grid(tick, step),
        base_asset: String::new(),
        builder: None,
    }
}

/// Mounted on BTCUSDT (tick 0.1, step 0.001); any other symbol gets an ETHUSDT-style client (tick
/// 0.01, step 0.01), or `no grid for <symbol>` when `grid_fetch_ok` is false.
fn router<R: VenueRest + 'static>(
    log: &Log,
    grid_fetch_ok: bool,
    build: fn(&str, f64, f64, &Log) -> R,
) -> PerSymbolRest<R> {
    let factory_log = log.clone();
    let make: ClientFactory<R> = Box::new(move |symbol: &str| {
        if grid_fetch_ok {
            Ok(build(symbol, 0.01, 0.01, &factory_log))
        } else {
            Err(format!("no grid for {symbol}"))
        }
    });
    PerSymbolRest::new(
        "BTCUSDT",
        build("BTCUSDT", 0.1, 0.001, log),
        make,
        Arc::new(CoidSymbols::default()),
    )
}

fn limit(coid: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "aster".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.239,
        order_type: OrderType::Limit,
        price: Some(2500.126),
        ..Default::default()
    }
}

fn wire(calls: &[Call]) -> Vec<(&str, &str)> {
    calls.iter().map(|c| (c.0.as_str(), c.1.as_str())).collect()
}

#[test]
fn a_foreign_perp_order_lifecycle_uses_its_own_symbol_and_grid() {
    let log = Log::default();
    let r = router(&log, true, perp);
    let order = limit("c1", "ETHUSDT");
    let events = r.submit_order(&order);
    assert_matches!(events.last(), Some(Event::OrderAccepted(_)));
    r.cancel_order("c1").expect("cancel");
    assert_matches!(
        r.modify_order(&order, Some(1.239), Some(2500.126)).as_slice(),
        [Event::OrderModified(_)]
    );
    r.confirm_order("c1", 7);

    let calls = log.calls();
    assert_eq!(
        wire(&calls),
        [
            ("POST", "/fapi/v3/order"),
            ("DELETE", "/fapi/v3/order"),
            ("PUT", "/fapi/v3/order"),
            ("GET", "/fapi/v3/order")
        ]
    );
    for c in &calls {
        assert_eq!(param(c, "symbol").as_deref(), Some("ETHUSDT"), "{} {}", c.0, c.1);
    }
    for c in [&calls[0], &calls[2]] {
        assert_eq!(param(c, "quantity").as_deref(), Some("1.23"));
        assert_eq!(param(c, "price").as_deref(), Some("2500.12"));
    }
}

#[test]
fn a_foreign_spot_order_lifecycle_uses_its_own_symbol_and_grid() {
    let log = Log::default();
    let r = router(&log, true, spot);
    r.submit_order(&limit("c1", "ETHUSDT"));
    r.cancel_order("c1").expect("cancel");
    r.confirm_order("c1", 7);
    let calls = log.calls();
    assert_eq!(
        wire(&calls),
        [("POST", "/api/v3/order"), ("DELETE", "/api/v3/order"), ("GET", "/api/v3/order")]
    );
    for c in &calls {
        assert_eq!(param(c, "symbol").as_deref(), Some("ETHUSDT"), "{}", c.1);
    }
    assert_eq!(param(&calls[0], "quantity").as_deref(), Some("1.23"));
    assert_eq!(param(&calls[0], "price").as_deref(), Some("2500.12"));
}

#[test]
fn the_mounted_symbol_is_unchanged_and_a_failed_grid_fetch_signs_nothing() {
    let log = Log::default();
    let r = router(&log, false, perp);
    r.submit_order(&limit("m1", "BTCUSDT"));
    let calls = log.calls();
    assert_eq!(param(&calls[0], "symbol").as_deref(), Some("BTCUSDT"));
    assert_eq!(param(&calls[0], "quantity").as_deref(), Some("1.239"));
    assert_eq!(param(&calls[0], "price").as_deref(), Some("2500.1"));

    let events = r.submit_order(&limit("c1", "ETHUSDT"));
    assert_eq!(events.len(), 2, "{events:?}");
    match &events[1] {
        Event::OrderRejected(rej) => {
            assert_eq!(rej.client_order_id, "c1");
            assert!(rej.reason.contains("no grid for ETHUSDT"), "{}", rej.reason);
        }
        other => panic!("expected a terminal OrderRejected, got {other:?}"),
    }
    assert_eq!(log.calls().len(), 1, "nothing signed for the refused order");
}
