//! One okx account, many instruments (offline): [`PerSymbolRest`] signs every order, cancel, amend
//! and status query on the ORDER's own `instId`, with THAT instrument's tick/lot grid AND its `ctVal`
//! (okx sizes in CONTRACTS, so the contract size is part of the wire). A fake transport records the
//! wire params. The mounted instrument (BTC-USDT-SWAP: ctVal 0.01, tick 0.1, lot 0.01) is the
//! default for an order on it or with no symbol, byte for byte; a foreign instrument (ETH-USDT-SWAP:
//! ctVal 0.1, tick 0.01, lot 0.01) is built by the factory; an instrument whose row cannot be read
//! is a terminal `OrderRejected` with nothing signed. The second half pins the FILL side: a fill of a
//! foreign instrument is converted to base with THAT instrument's `ctVal`, never the mounted one's.

use std::assert_matches;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties};
use vike_okx::event_mapper::map_okx_perp_by_inst;
use vike_okx::per_symbol::{ClientFactory, CoidSymbols, ContractSizes, PerSymbolRest};
use vike_okx::perp::OkxPerpRest;
use vike_okx::transport::OkxTransport;
use vike_secrets::credentials::Credentials;

type Call = (String, String, Vec<(String, Value)>);

/// Every signed request, from every client of one router, in order: (path, method, params).
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Call>>>);

impl Log {
    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
}

struct Wire(Log);
impl OkxTransport for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.0.0.lock().unwrap().push((path.to_string(), method.to_string(), owned));
        Ok(json!({
            "code": "0", "msg": "",
            "data": [{ "ordId": "oid-1", "algoId": "algo-1", "sCode": "0" }],
        }))
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn param(call: &Call, key: &str) -> Option<String> {
    call.2.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str().map(str::to_string))
}

fn client(inst: &str, ct_val: f64, tick: f64, lot: f64, log: &Log) -> OkxPerpRest<Wire> {
    OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            || 0,
        ),
        transport: Wire(log.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: inst.to_string(),
        properties: SymbolProperties { tick_size: tick, step_size: lot, ..Default::default() },
        ct_val,
        leverage: 2.0,
        broker_code: None,
    }
}

const BTC: &str = "BTC-USDT-SWAP";
const ETH: &str = "ETH-USDT-SWAP";

/// A router mounted on BTC-USDT-SWAP whose factory builds an ETH-USDT-SWAP client (ctVal 0.1, tick
/// 0.01, lot 0.01) for any other instrument, or fails with `no instrument row for <inst>`.
fn router(
    log: &Log,
    fetches: &Arc<AtomicUsize>,
    row_fetch_ok: bool,
) -> PerSymbolRest<OkxPerpRest<Wire>> {
    let (factory_log, fetches) = (log.clone(), fetches.clone());
    let make: ClientFactory<OkxPerpRest<Wire>> = Box::new(move |inst: &str| {
        fetches.fetch_add(1, Ordering::SeqCst);
        if row_fetch_ok {
            Ok(client(&inst.to_uppercase(), 0.1, 0.01, 0.01, &factory_log))
        } else {
            Err(format!("no instrument row for {inst}"))
        }
    });
    PerSymbolRest::new(
        BTC,
        client(BTC, 0.01, 0.1, 0.01, log),
        make,
        Arc::new(CoidSymbols::default()),
    )
}

fn limit(coid: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "okx".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.239,
        order_type: OrderType::Limit,
        price: Some(2500.126),
        ..Default::default()
    }
}

#[test]
fn a_foreign_instrument_is_signed_on_its_own_instid_grid_and_contract_size() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), true);
    let events = r.submit_order(&limit("c1", ETH));
    assert_matches!(events.last(), Some(Event::OrderAccepted(a)) if a.client_order_id == "c1");
    let calls = log.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, "/api/v5/trade/order");
    assert_eq!(param(&calls[0], "instId").as_deref(), Some(ETH));
    // 1.239 base / ctVal 0.1 = 12.39 contracts (BTC's 0.01 would have sent 123.90)
    assert_eq!(param(&calls[0], "sz").as_deref(), Some("12.39"), "ETH ctVal 0.1, not BTC's 0.01");
    assert_eq!(param(&calls[0], "px").as_deref(), Some("2500.12"), "ETH tick 0.01, not 0.1");
}

#[test]
fn the_mounted_instrument_and_an_empty_symbol_stay_on_the_mounted_client() {
    let log = Log::default();
    let fetches = Arc::new(AtomicUsize::new(0));
    let r = router(&log, &fetches, true);
    r.submit_order(&limit("c1", BTC));
    r.submit_order(&limit("c2", ""));
    r.submit_order(&limit("c3", "btc-usdt-swap"));
    let calls = log.calls();
    assert_eq!(calls.len(), 3);
    for c in &calls {
        assert_eq!(param(c, "instId").as_deref(), Some(BTC));
        assert_eq!(param(c, "sz").as_deref(), Some("123.90"));
        assert_eq!(param(c, "px").as_deref(), Some("2500.1"));
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 0, "the mounted path never asks for a row");
}

#[test]
fn an_instruments_row_is_fetched_once() {
    let log = Log::default();
    let fetches = Arc::new(AtomicUsize::new(0));
    let r = router(&log, &fetches, true);
    r.submit_order(&limit("c1", ETH));
    r.submit_order(&limit("c2", ETH));
    r.submit_order(&limit("c3", "eth-usdt-swap"));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    assert_eq!(log.calls().len(), 3);
}

#[test]
fn a_foreign_stop_is_an_algo_order_on_its_own_instrument() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), true);
    let stop = OrderRequest {
        order_type: OrderType::Stop,
        trigger_price: Some(2400.126),
        reduce_only: true,
        ..limit("s1", ETH)
    };
    r.submit_order(&stop);
    let calls = log.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, "/api/v5/trade/order-algo");
    assert_eq!(param(&calls[0], "instId").as_deref(), Some(ETH));
    assert_eq!(param(&calls[0], "sz").as_deref(), Some("12.39"));
    assert_eq!(param(&calls[0], "slTriggerPx").as_deref(), Some("2400.12"));
}

#[test]
fn cancel_modify_and_status_of_a_foreign_order_use_its_instrument() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), true);
    let order = limit("c1", ETH);
    r.submit_order(&order);

    r.cancel_order("c1").expect("cancel");
    let events = r.modify_order(&order, Some(1.239), Some(2500.126));
    assert_matches!(events.as_slice(), [Event::OrderModified(_)]);
    let status = r.confirm_order("c1", 7);
    assert_matches!(status.as_slice(), [Event::OrderAccepted(_)]);

    let calls = log.calls();
    let paths: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/api/v5/trade/order",
            "/api/v5/trade/cancel-order",
            "/api/v5/trade/amend-order",
            "/api/v5/trade/order"
        ]
    );
    assert_eq!(calls[3].1, "GET", "the status query is the by-clOrdId GET");
    for c in &calls {
        assert_eq!(param(c, "instId").as_deref(), Some(ETH), "{}", c.0);
    }
    let amend = &calls[2];
    assert_eq!(param(amend, "newSz").as_deref(), Some("12.39"));
    assert_eq!(param(amend, "newPx").as_deref(), Some("2500.12"));
}

/// A router like [`router`] with a LIGHT factory (no grid fetch) for cancel and status.
fn router_with_light(
    log: &Log,
    fetches: &Arc<AtomicUsize>,
    row_fetch_ok: bool,
) -> PerSymbolRest<OkxPerpRest<Wire>> {
    let light_log = log.clone();
    let light: ClientFactory<OkxPerpRest<Wire>> =
        Box::new(move |inst: &str| Ok(client(&inst.to_uppercase(), 0.1, 0.01, 0.01, &light_log)));
    router(log, fetches, row_fetch_ok).with_light_factory(light)
}

/// After a restart a foreign order's CANCEL (and status) must not need `public/instruments`: with the
/// grid fetch DOWN the cancel still reaches the venue on the order's own instrument, the factory is
/// never asked for it, and HALT's cancel over several uncached instruments costs no fetch each. A
/// submit still needs the grid and is still refused.
#[test]
fn a_foreign_cancel_and_status_succeed_with_the_grid_fetch_down() {
    let log = Log::default();
    let fetches = Arc::new(AtomicUsize::new(0));
    let r = router_with_light(&log, &fetches, false);
    // The coid map knows the instrument (as `adopt_restored` teaches it after a restart); the
    // submit itself is refused (its grid fetch fails), which is what makes the fetch "down".
    let events = r.submit_order(&limit("c1", ETH));
    assert_matches!(events.last(), Some(Event::OrderRejected(_)));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    r.cancel_order("c1").expect("the cancel needs no grid");
    assert_matches!(r.confirm_order("c1", 7).as_slice(), [Event::OrderAccepted(_)]);
    let calls = log.calls();
    let paths: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(paths, ["/api/v5/trade/cancel-order", "/api/v5/trade/order"], "{calls:?}");
    for c in &calls {
        assert_eq!(param(c, "instId").as_deref(), Some(ETH), "{}", c.0);
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "the factory was not asked again");

    // The batch HALT sends: every foreign id cancels, none waits for a fetch.
    for c in ["c2", "c3"] {
        r.submit_order(&limit(c, ETH));
    }
    let before = fetches.load(Ordering::SeqCst);
    r.cancel_batch(&["c1".into(), "c2".into(), "c3".into()]).expect("batch cancel");
    assert_eq!(fetches.load(Ordering::SeqCst), before);
}

#[test]
fn a_coid_the_router_never_saw_is_cancelled_on_the_mounted_instrument() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), true);
    r.cancel_order("adopted").expect("cancel");
    r.confirm_order("adopted", 7);
    for c in log.calls() {
        assert_eq!(param(&c, "instId").as_deref(), Some(BTC), "{}", c.0);
    }
}

#[test]
fn a_failed_row_fetch_is_a_terminal_rejection_and_nothing_is_signed() {
    let log = Log::default();
    let r = router(&log, &Arc::default(), false);
    let events = r.submit_order(&limit("c1", ETH));
    assert_eq!(events.len(), 2, "{events:?}");
    assert_matches!(&events[0], Event::OrderSubmitted(s) if s.client_order_id == "c1");
    match &events[1] {
        Event::OrderRejected(rej) => {
            assert_eq!(rej.client_order_id, "c1");
            assert!(rej.reason.contains("no instrument row for ETH-USDT-SWAP"), "{}", rej.reason);
        }
        other => panic!("expected a terminal OrderRejected, got {other:?}"),
    }
    assert!(log.calls().is_empty(), "a mis-sized order is worse than a refused one");

    let err = r.cancel_order("c1").expect_err("no client for the instrument, so no cancel");
    assert!(err.msg.contains("no instrument row for ETH-USDT-SWAP"), "{}", err.msg);
    let modified = r.modify_order(&limit("c1", ETH), Some(1.0), None);
    assert_matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]);
    assert!(log.calls().is_empty());
}

// ---------------------------------------------------------------------------------------------
// The fill side: contracts -> base uses the FILLED instrument's ctVal.

/// One `orders`-channel frame carrying a partial fill of `fill_sz` CONTRACTS on `inst`.
fn fill_frame(inst: &str, fill_sz: &str) -> Value {
    json!({
        "arg": { "channel": "orders", "instType": "SWAP" },
        "data": [{
            "instId": inst, "clOrdId": "c1", "ordId": "o1", "tradeId": "t1",
            "fillSz": fill_sz, "fillPx": "2500", "fillFee": "-0.1", "fillFeeCcy": "USDT",
            "side": "buy", "state": "partially_filled", "sz": "50", "accFillSz": fill_sz,
            "execType": "T", "posSide": "net", "fillTime": "1700000000000",
        }],
    })
}

fn fill_qty(events: &[Event]) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.last_qty),
            _ => None,
        })
        .collect()
}

/// BTC mounted (ctVal 0.01), ETH registered (ctVal 0.1).
fn sizes() -> ContractSizes {
    let s = ContractSizes::new(BTC, 0.01);
    s.register(ETH, 0.1);
    s
}

#[test]
fn a_foreign_fill_is_converted_with_its_own_contract_size() {
    let s = sizes();
    let load = || -> Option<f64> { panic!("a registered instrument needs no fetch") };
    let eth = map_okx_perp_by_inst(&fill_frame(ETH, "12"), "okx", BTC, |i| s.get_or_load(i, load));
    let btc = map_okx_perp_by_inst(&fill_frame(BTC, "12"), "okx", BTC, |i| s.get_or_load(i, load));
    assert_eq!(fill_qty(&eth).len(), 1);
    assert!((fill_qty(&eth)[0] - 1.2).abs() < 1e-12, "12 ETH contracts * 0.1 = 1.2 base");
    assert!((fill_qty(&btc)[0] - 0.12).abs() < 1e-12, "12 BTC contracts * 0.01 = 0.12 base");
    match &eth[0] {
        Event::Fill(f) => assert_eq!(f.symbol.as_str(), ETH, "the fill names its instrument"),
        other => panic!("expected the bare Fill first, got {other:?}"),
    }
}

#[test]
fn an_unregistered_instrument_is_loaded_once_and_remembered() {
    let s = ContractSizes::new(BTC, 0.01);
    let loads = AtomicUsize::new(0);
    let load = || {
        loads.fetch_add(1, Ordering::SeqCst);
        Some(1000.0)
    };
    assert_eq!(s.get_or_load("DOGE-USDT-SWAP", load), Some(1000.0));
    assert_eq!(s.get_or_load("doge-usdt-swap", load), Some(1000.0));
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    // the mounted instrument never loads
    assert_eq!(s.get_or_load(BTC, || panic!("mounted")), Some(0.01));
}

#[test]
fn a_fill_whose_contract_size_cannot_be_read_is_dropped_never_mis_sized() {
    let s = ContractSizes::new(BTC, 0.01);
    let events =
        map_okx_perp_by_inst(&fill_frame(ETH, "12"), "okx", BTC, |i| s.get_or_load(i, || None));
    assert!(
        events.is_empty(),
        "a fill scaled by another instrument's ctVal is wrong money: {events:?}"
    );
    // a lifecycle row (no fill) of the same unknown instrument still passes: it carries no size
    let mut live = fill_frame(ETH, "0");
    live["data"][0]["state"] = json!("live");
    live["data"][0]["tradeId"] = json!("");
    let events = map_okx_perp_by_inst(&live, "okx", BTC, |i| s.get_or_load(i, || None));
    assert_matches!(events.as_slice(), [Event::OrderAccepted(_)]);
}

#[test]
fn the_mounted_wrapper_is_unchanged() {
    // `map_okx_perp` (the pinned parity entry) still scales every row by the ONE ctVal it is given.
    let events = vike_okx::event_mapper::map_okx_perp(&fill_frame(BTC, "12"), "okx", BTC, 0.01);
    assert!((fill_qty(&events)[0] - 0.12).abs() < 1e-12);
}

// ---------------------------------------------------------------------------------------------
// An instrument whose row cannot be read costs ONE blocking fetch, not one per order or per fill.

#[test]
fn a_failed_row_fetch_is_remembered_so_a_burst_costs_one_fetch() {
    let log = Log::default();
    let fetches = Arc::new(AtomicUsize::new(0));
    let r = router(&log, &fetches, false);
    for n in 0..5 {
        let events = r.submit_order(&limit(&format!("c{n}"), ETH));
        assert_matches!(events.last(), Some(Event::OrderRejected(rej))
            if rej.reason.contains("no instrument row for ETH-USDT-SWAP"));
    }
    r.cancel_order("c0").expect_err("still no client");
    r.confirm_order("c0", 1);
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "the failure is remembered for a while");
    // the mounted instrument is untouched by a bad neighbour
    assert_matches!(r.submit_order(&limit("m", BTC)).last(), Some(Event::OrderAccepted(_)));
}

#[test]
fn an_unreadable_contract_size_is_not_asked_again_at_once() {
    let s = ContractSizes::new(BTC, 0.01);
    let loads = AtomicUsize::new(0);
    let load = || {
        loads.fetch_add(1, Ordering::SeqCst);
        None
    };
    assert_eq!(s.get_or_load("DOGE-USDT-SWAP", load), None);
    assert_eq!(s.get_or_load("doge-usdt-swap", load), None);
    assert_eq!(s.get_or_load("DOGE-USDT-SWAP", load), None);
    assert_eq!(loads.load(Ordering::SeqCst), 1, "a stalled pump must not re-ask on every fill");
    // another instrument is its own question
    assert_eq!(s.get_or_load("SOL-USDT-SWAP", || Some(1.0)), Some(1.0));
}

// ---------------------------------------------------------------------------------------------
// The fill dedup key: OKX `tradeId` is not promised unique across instruments.

fn trade_ids(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.to_string()),
            Event::OrderFilled(w) => Some(w.fill.trade_id.to_string()),
            Event::OrderPartiallyFilled(w) => Some(w.fill.trade_id.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_fill_of_another_instrument_carries_an_instrument_qualified_trade_id() {
    let s = sizes();
    let ids = |inst: &str| {
        trade_ids(&map_okx_perp_by_inst(&fill_frame(inst, "12"), "okx", BTC, |i| {
            s.get_or_load(i, || None)
        }))
    };
    let eth = ids(ETH);
    assert!(!eth.is_empty());
    assert!(eth.iter().all(|t| t == "ETH-USDT-SWAP:t1"), "{eth:?}");
    // the mounted instrument keeps the bare id (resync, reconcile and a restored ledger spell it so)
    let btc = ids(BTC);
    assert!(!btc.is_empty());
    assert!(btc.iter().all(|t| t == "t1"), "{btc:?}");
    assert!(ids("btc-usdt-swap").iter().all(|t| t == "t1"), "the mounted id in any case is bare");
}

#[test]
fn a_remembered_foreign_order_is_never_forgotten_to_make_room() {
    // Clearing the map at a cap sends an old foreign order's cancel to the MOUNTED instrument, where
    // okx answers 51401 and the cancel reads "already gone" over a resting order.
    let coids = CoidSymbols::default();
    for n in 0..10_000 {
        coids.record(&format!("c{n}"), ETH);
    }
    assert_eq!(coids.symbol_of("c0").as_deref(), Some(ETH));
    assert_eq!(coids.symbol_of("c9999").as_deref(), Some(ETH));
}
