//! One bybit account, many symbols (offline): [`PerSymbolRest`] signs every order, cancel, amend and
//! status query on the ORDER's own symbol with THAT symbol's price/qty grid. A fake transport records
//! the wire params; the mounted symbol (BTCUSDT, tick 0.1 / step 0.001) is the default for an order on
//! it or with no symbol, and a symbol whose grid cannot be fetched is a terminal `OrderRejected` with
//! nothing signed.

use std::assert_matches;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use vike_bybit::perp::BybitPerpRest;
use vike_bybit::transport::BybitTransport;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties};
use vike_secrets::credentials::Credentials;

type Call = (String, Vec<(String, Value)>);

/// Every signed request, from every client of one router, in order.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Call>>>);

impl Log {
    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
}

struct Wire(Log);
impl BybitTransport for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.0.0.lock().unwrap().push((path.to_string(), owned));
        Ok(json!({
            "retCode": 0, "retMsg": "OK",
            "result": { "orderId": "oid-1", "list": [{ "orderId": "oid-1" }] },
        }))
    }
}

fn param(call: &Call, key: &str) -> Option<String> {
    call.1.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str().map(str::to_string))
}

fn client(symbol: &str, tick: f64, step: f64, log: &Log) -> BybitPerpRest<Wire> {
    BybitPerpRest {
        signer: BybitV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: None,
            },
            || 0,
        ),
        transport: Wire(log.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: symbol.to_string(),
        properties: SymbolProperties { tick_size: tick, step_size: step, ..Default::default() },
        leverage: 1.0,
    }
}

/// A router mounted on BTCUSDT (tick 0.1, step 0.001) whose factory builds an ETHUSDT-style client
/// (tick 0.01, step 0.01) for any other symbol, or fails with `no grid for <symbol>`.
fn router(
    log: &Log,
    fetches: &Arc<AtomicUsize>,
    grid_fetch_ok: bool,
) -> PerSymbolRest<BybitPerpRest<Wire>> {
    let (factory_log, fetches) = (log.clone(), fetches.clone());
    let make: ClientFactory<BybitPerpRest<Wire>> = Box::new(move |symbol: &str| {
        fetches.fetch_add(1, Ordering::SeqCst);
        if grid_fetch_ok {
            Ok(client(symbol, 0.01, 0.01, &factory_log))
        } else {
            Err(format!("no grid for {symbol}"))
        }
    });
    PerSymbolRest::new(
        "BTCUSDT",
        client("BTCUSDT", 0.1, 0.001, log),
        make,
        Arc::new(CoidSymbols::default()),
    )
}

fn limit(coid: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "bybit".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.239,
        order_type: OrderType::Limit,
        price: Some(2500.126),
        ..Default::default()
    }
}

#[test]
fn a_foreign_symbol_order_is_signed_on_its_own_symbol_and_grid() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), true);
    let events = r.submit_order(&limit("c1", "ETHUSDT"));
    assert_matches!(events.last(), Some(Event::OrderAccepted(a)) if a.client_order_id == "c1");
    let calls = log.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, "/v5/order/create");
    assert_eq!(param(&calls[0], "symbol").as_deref(), Some("ETHUSDT"));
    assert_eq!(param(&calls[0], "qty").as_deref(), Some("1.23"), "ETH step 0.01, not BTC's 0.001");
    assert_eq!(param(&calls[0], "price").as_deref(), Some("2500.12"), "ETH tick 0.01, not 0.1");
}

#[test]
fn the_mounted_symbol_and_an_empty_symbol_stay_on_the_mounted_client() {
    let log = Log::default();
    let fetches = Arc::new(AtomicUsize::new(0));
    let r = router(&log, &fetches, true);
    r.submit_order(&limit("c1", "BTCUSDT"));
    r.submit_order(&limit("c2", ""));
    r.submit_order(&limit("c3", "btcusdt"));
    let calls = log.calls();
    assert_eq!(calls.len(), 3);
    for c in &calls {
        assert_eq!(param(c, "symbol").as_deref(), Some("BTCUSDT"));
        assert_eq!(param(c, "qty").as_deref(), Some("1.239"));
        assert_eq!(param(c, "price").as_deref(), Some("2500.1"));
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 0, "the mounted path never asks for a grid");
}

#[test]
fn a_symbols_grid_is_fetched_once() {
    let log = Log::default();
    let fetches = Arc::new(AtomicUsize::new(0));
    let r = router(&log, &fetches, true);
    r.submit_order(&limit("c1", "ETHUSDT"));
    r.submit_order(&limit("c2", "ETHUSDT"));
    r.submit_order(&limit("c3", "ethusdt"));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    assert_eq!(log.calls().len(), 3);
}

#[test]
fn cancel_modify_and_status_of_a_foreign_order_use_its_symbol() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), true);
    let order = limit("c1", "ETHUSDT");
    r.submit_order(&order);

    r.cancel_order("c1").expect("cancel");
    let events = r.modify_order(&order, Some(1.239), Some(2500.126));
    assert_matches!(events.as_slice(), [Event::OrderModified(_)]);
    r.confirm_order("c1", 7);

    let calls = log.calls();
    let paths: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        paths,
        ["/v5/order/create", "/v5/order/cancel", "/v5/order/amend", "/v5/order/realtime"]
    );
    for c in &calls {
        assert_eq!(param(c, "symbol").as_deref(), Some("ETHUSDT"), "{}", c.0);
    }
    let amend = &calls[2];
    assert_eq!(param(amend, "qty").as_deref(), Some("1.23"));
    assert_eq!(param(amend, "price").as_deref(), Some("2500.12"));
}

/// After a restart a foreign order's CANCEL (and status) must not need `instruments-info`: with the
/// grid fetch DOWN the cancel still reaches the venue on the order's own symbol and the factory is
/// never asked for it, so HALT's cancel over several uncached symbols costs no fetch each.
#[test]
fn a_foreign_cancel_and_status_succeed_with_the_grid_fetch_down() {
    let log = Log::default();
    let fetches = Arc::new(AtomicUsize::new(0));
    let light_log = log.clone();
    let light: ClientFactory<BybitPerpRest<Wire>> =
        Box::new(move |symbol: &str| Ok(client(&symbol.to_uppercase(), 0.01, 0.01, &light_log)));
    let r = router(&log, &fetches, false).with_light_factory(light);
    // The coid map knows the symbol (as `adopt_restored` teaches it after a restart); the submit is
    // refused because its grid fetch fails, which is what makes the fetch "down".
    let events = r.submit_order(&limit("c1", "ETHUSDT"));
    assert_matches!(events.last(), Some(Event::OrderRejected(_)));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    r.cancel_order("c1").expect("the cancel needs no grid");
    r.confirm_order("c1", 7);
    for c in ["c2", "c3"] {
        r.submit_order(&limit(c, "ETHUSDT"));
    }
    r.cancel_batch(&["c1".into(), "c2".into(), "c3".into()]).expect("batch cancel");

    let calls = log.calls();
    let paths: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/v5/order/cancel",
            "/v5/order/realtime",
            "/v5/order/cancel",
            "/v5/order/cancel",
            "/v5/order/cancel"
        ],
        "{calls:?}"
    );
    for c in &calls {
        assert_eq!(param(c, "symbol").as_deref(), Some("ETHUSDT"), "{}", c.0);
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "the grid factory was never asked again");
}

#[test]
fn a_coid_the_router_never_saw_is_cancelled_on_the_mounted_symbol() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), true);
    r.cancel_order("adopted").expect("cancel");
    r.confirm_order("adopted", 7);
    for c in log.calls() {
        assert_eq!(param(&c, "symbol").as_deref(), Some("BTCUSDT"), "{}", c.0);
    }
}

#[test]
fn a_failed_grid_fetch_is_a_terminal_rejection_and_nothing_is_signed() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), false);
    let events = r.submit_order(&limit("c1", "ETHUSDT"));
    assert_eq!(events.len(), 2, "{events:?}");
    assert_matches!(&events[0], Event::OrderSubmitted(s) if s.client_order_id == "c1");
    match &events[1] {
        Event::OrderRejected(rej) => {
            assert_eq!(rej.client_order_id, "c1");
            assert!(rej.reason.contains("no grid for ETHUSDT"), "{}", rej.reason);
        }
        other => panic!("expected a terminal OrderRejected, got {other:?}"),
    }
    assert!(log.calls().is_empty(), "a mis-signed order is worse than a refused one");

    let err = r.cancel_order("c1").expect_err("no client for the symbol, so no cancel");
    assert!(err.msg.contains("no grid for ETHUSDT"), "{}", err.msg);
    let modified = r.modify_order(&limit("c1", "ETHUSDT"), Some(1.0), None);
    assert_matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]);
    assert!(log.calls().is_empty());
}
