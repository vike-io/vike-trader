//! The account-mode rule at the ORDER DOOR (offline): `PerSymbolRest::with_mode_check` asks
//! `vike_bybit::modes::Modes::ensure` before the first order of a symbol and signs nothing on a refusal.
//! One scripted wire plays the venue for both the mode probe and the order client, so the order of the
//! requests is the proof that the switch comes BEFORE the order that would have been answered
//! `10001 "position idx not match position mode"`. The rule's own branches are `src/modes_tests.rs`.

use std::assert_matches;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::modes::Modes;
use vike_bybit::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use vike_bybit::perp::{BybitPerpRest, DEMO_REST, MAINNET_REST};
use vike_bybit::transport::BybitTransport;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties};
use vike_secrets::credentials::Credentials;

type Call = (String, Vec<(String, Value)>);

/// What the scripted venue holds for EVERY symbol asked about.
#[derive(Clone, Copy)]
struct World {
    hedge: bool,
    /// `size` of the first leg row.
    size: &'static str,
}

#[derive(Clone)]
struct Wire {
    log: Arc<Mutex<Vec<Call>>>,
    world: World,
}

impl Wire {
    fn paths(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().map(|c| c.0.clone()).collect()
    }

    fn calls(&self) -> Vec<Call> {
        self.log.lock().unwrap().clone()
    }
}

fn param(call: &Call, key: &str) -> Option<String> {
    call.1.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str().map(str::to_string))
}

impl BybitTransport for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        let owned: Vec<(String, Value)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let symbol =
            owned.iter().find(|(k, _)| k == "symbol").and_then(|(_, v)| v.as_str()).unwrap_or("");
        let leg = |idx: i64, size: &str| json!({"symbol": symbol, "positionIdx": idx, "size": size, "tradeMode": 0});
        let result = match path {
            "/v5/position/list" if self.world.hedge => {
                json!({"list": [leg(1, self.world.size), leg(2, "0")]})
            }
            "/v5/position/list" => json!({"list": [leg(0, self.world.size)]}),
            "/v5/account/info" => json!({"marginMode": "REGULAR_MARGIN", "unifiedMarginStatus": 3}),
            "/v5/order/realtime" => json!({"list": []}),
            _ => json!({"orderId": "oid-1", "list": [{"orderId": "oid-1"}]}),
        };
        self.log.lock().unwrap().push((path.to_string(), owned));
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": result}))
    }
}

fn creds() -> Credentials {
    Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    }
}

fn client_on(symbol: &str, wire: &Wire, base_url: &str) -> BybitPerpRest<Wire> {
    BybitPerpRest {
        category: vike_bybit::data::Category::Linear,
        signer: BybitV5Signer::new(&creds(), || 0),
        transport: wire.clone(),
        base_url: base_url.to_string(),
        symbol: symbol.to_string(),
        properties: SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() },
        leverage: 1.0,
    }
}

/// A router mounted on BTCUSDT whose factory builds an ETHUSDT-style client for any other symbol (or
/// fails with `no grid for <symbol>`), with the mode check wired the way `exec::run` wires it.
fn router(
    world: World,
    grid_fetch_ok: bool,
    fetches: &Arc<AtomicUsize>,
) -> (PerSymbolRest<BybitPerpRest<Wire>>, Wire) {
    router_on(world, grid_fetch_ok, fetches, DEMO_REST, false)
}

/// [`router`] on a chosen host and mainnet flag (the rule and every client are built on the same pair, as
/// `exec::run` builds them from one tier answer).
fn router_on(
    world: World,
    grid_fetch_ok: bool,
    fetches: &Arc<AtomicUsize>,
    base_url: &'static str,
    mainnet: bool,
) -> (PerSymbolRest<BybitPerpRest<Wire>>, Wire) {
    let wire = Wire { log: Arc::default(), world };
    let (factory_wire, fetches) = (wire.clone(), fetches.clone());
    let make: ClientFactory<BybitPerpRest<Wire>> = Box::new(move |symbol: &str| {
        fetches.fetch_add(1, Ordering::SeqCst);
        if grid_fetch_ok {
            Ok(client_on(symbol, &factory_wire, base_url))
        } else {
            Err(format!("no grid for {symbol}"))
        }
    });
    let modes =
        Arc::new(Modes::new(BybitV5Signer::new(&creds(), || 0), wire.clone(), base_url, mainnet));
    let router = PerSymbolRest::new(
        "BTCUSDT",
        client_on("BTCUSDT", &wire, base_url),
        make,
        Arc::new(CoidSymbols::default()),
    )
    .with_mode_check(Box::new(move |symbol| modes.ensure(symbol)));
    (router, wire)
}

fn limit(coid: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "bybit".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    }
}

const HEDGE_FLAT: World = World { hedge: true, size: "0" };
const HEDGE_HELD: World = World { hedge: true, size: "0.5" };
const ONE_WAY: World = World { hedge: false, size: "0" };

#[test]
fn a_hedge_symbol_is_switched_to_one_way_before_the_first_order_is_signed() {
    let (router, wire) = router(HEDGE_FLAT, true, &Arc::default());
    let events = router.submit_order(&limit("c1", "BTCUSDT"));
    assert_matches!(events.last(), Some(Event::OrderAccepted(a)) if a.client_order_id == "c1");
    assert_eq!(
        wire.paths(),
        [
            "/v5/position/list",
            "/v5/order/realtime",
            "/v5/position/switch-mode",
            "/v5/account/info",
            "/v5/order/create"
        ]
    );
    let calls = wire.calls();
    assert_eq!(param(&calls[2], "symbol").as_deref(), Some("BTCUSDT"));
    assert_eq!(
        calls[2].1.iter().find(|(k, _)| k == "mode").map(|(_, v)| v.clone()),
        Some(json!(0))
    );
}

#[test]
fn a_second_order_on_the_same_symbol_is_not_probed_again() {
    let (router, wire) = router(HEDGE_FLAT, true, &Arc::default());
    router.submit_order(&limit("c1", "BTCUSDT"));
    let before = wire.paths().len();
    router.submit_order(&limit("c2", "btcusdt"));
    router.submit_order(&limit("c3", ""));
    assert_eq!(wire.paths().len(), before + 2, "two orders, two creates, no probe");
    assert!(wire.paths()[before..].iter().all(|p| p == "/v5/order/create"));
}

#[test]
fn a_one_way_symbol_costs_two_reads_and_no_write() {
    let (router, wire) = router(ONE_WAY, true, &Arc::default());
    router.submit_order(&limit("c1", "BTCUSDT"));
    assert_eq!(wire.paths(), ["/v5/position/list", "/v5/account/info", "/v5/order/create"]);
}

#[test]
fn a_hedge_symbol_that_cannot_be_switched_is_a_terminal_rejection_in_words_and_nothing_is_signed() {
    let (router, wire) = router(HEDGE_HELD, true, &Arc::default());
    let events = router.submit_order(&limit("c1", "BTCUSDT"));
    assert_eq!(events.len(), 2, "{events:?}");
    assert_matches!(&events[0], Event::OrderSubmitted(s) if s.client_order_id == "c1");
    match &events[1] {
        Event::OrderRejected(rej) => {
            assert_eq!(rej.client_order_id, "c1");
            for word in ["BTCUSDT", "HEDGE", "holds an open position"] {
                assert!(rej.reason.contains(word), "{word}: {}", rej.reason);
            }
        }
        other => panic!("expected a terminal OrderRejected, got {other:?}"),
    }
    assert!(!wire.paths().iter().any(|p| p.contains("order/create") || p.contains("switch-mode")));
    // A cancel never asks the mode rule.
    let before = wire.paths().len();
    router.cancel_order("c1").expect("cancel");
    assert_eq!(wire.paths()[before..], ["/v5/order/cancel"]);
}

#[test]
fn a_foreign_symbol_is_checked_after_its_grid_and_on_its_own_symbol() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let (router, wire) = router(HEDGE_FLAT, true, &fetches);
    let events = router.submit_order(&limit("c1", "ETHUSDT"));
    assert_matches!(events.last(), Some(Event::OrderAccepted(_)));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    for call in wire.calls() {
        assert_eq!(
            param(&call, "symbol").as_deref().or(Some("ETHUSDT")),
            Some("ETHUSDT"),
            "{}",
            call.0
        );
    }
    assert!(wire.paths().contains(&"/v5/position/switch-mode".to_string()));
}

#[test]
fn an_unknown_symbol_is_refused_for_its_grid_before_any_mode_probe() {
    let (router, wire) = router(HEDGE_FLAT, false, &Arc::default());
    let events = router.submit_order(&limit("c1", "NOPEUSDT"));
    match events.last() {
        Some(Event::OrderRejected(rej)) => assert!(rej.reason.contains("no grid for NOPEUSDT")),
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(wire.paths().is_empty(), "{:?}", wire.paths());
}

#[test]
fn a_mounted_batch_the_mode_rule_refuses_is_refused_order_by_order() {
    let (router, wire) = router(HEDGE_HELD, true, &Arc::default());
    let events = router.submit_batch(&[limit("c1", "BTCUSDT"), limit("c2", "")]);
    let rejected: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.client_order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(rejected, ["c1", "c2"], "{events:?}");
    assert!(!wire.paths().iter().any(|p| p.contains("create")), "{:?}", wire.paths());

    // The same batch on a one-way symbol takes the native batch.
    let (native, wire) = self::router(ONE_WAY, true, &Arc::default());
    native.submit_batch(&[limit("c1", "BTCUSDT"), limit("c2", "")]);
    assert_eq!(wire.paths().last().map(String::as_str), Some("/v5/order/create-batch"));
}

/// OWNER RULING 2026-10-26: no exchange write on a live account. A rule built for the mainnet host, or flagged
/// mainnet whatever host it names, or built on a host nobody named, asks the venue NOTHING: the order goes
/// straight to the venue, as it did before the rule existed, even on a symbol the venue holds in hedge mode.
#[test]
fn a_live_account_is_never_probed_or_written_and_its_order_goes_straight_to_the_venue() {
    for (base_url, mainnet) in [
        (MAINNET_REST, true),
        (MAINNET_REST, false),
        (DEMO_REST, true),
        ("https://unused.invalid", false),
    ] {
        let (router, wire) = router_on(HEDGE_FLAT, true, &Arc::default(), base_url, mainnet);
        let events = router.submit_order(&limit("c1", "BTCUSDT"));
        assert_matches!(events.last(), Some(Event::OrderAccepted(_)), "{base_url} {mainnet}");
        assert_eq!(wire.paths(), ["/v5/order/create"], "{base_url} {mainnet}");

        // An admitted symbol too: its grid is fetched, then its order is signed, and no mode is probed.
        let (router, wire) = router_on(HEDGE_FLAT, true, &Arc::default(), base_url, mainnet);
        router.submit_order(&limit("c2", "ETHUSDT"));
        assert_eq!(wire.paths(), ["/v5/order/create"], "{base_url} {mainnet}");
    }
}

/// A transport that answers every signed request `10001 "position idx not match position mode"`, the
/// venue's refusal of a one-way order on a hedge symbol (measured on the demo host).
struct HedgeRefuses;

impl BybitTransport for HedgeRefuses {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        Ok(
            json!({"retCode": 10001, "retMsg": "position idx not match position mode", "result": {}}),
        )
    }
}

fn refusing_client(base_url: &str) -> BybitPerpRest<HedgeRefuses> {
    BybitPerpRest {
        category: vike_bybit::data::Category::Linear,
        signer: BybitV5Signer::new(&creds(), || 0),
        transport: HedgeRefuses,
        base_url: base_url.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() },
        leverage: 1.0,
    }
}

fn reasons(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .collect()
}

/// A hedge symbol's order on a MAINNET host is refused in words that name the mismatch and the hand fix, on
/// the single and the batch path; on the demo host the venue's own text is untouched (the rule handles it).
#[test]
fn a_live_hedge_reject_names_the_mismatch_and_the_hand_fix() {
    let venue_text = "position idx not match position mode";
    let live = refusing_client(MAINNET_REST);
    let mut all = live.submit_order(&limit("c1", "BTCUSDT"));
    all.extend(live.submit_batch(&[limit("c2", "BTCUSDT"), limit("c3", "BTCUSDT")]));
    let why = reasons(&all);
    assert_eq!(why.len(), 3, "{all:?}");
    for w in &why {
        for word in
            ["BTCUSDT", "HEDGE", "ONE-WAY", "10001", venue_text, "changes no LIVE account's mode"]
        {
            assert!(w.contains(word), "{word}: {w}");
        }
        assert!(w.contains("One-Way position mode"), "the hand fix: {w}");
    }

    let demo = refusing_client(DEMO_REST);
    let mut all = demo.submit_order(&limit("c1", "BTCUSDT"));
    all.extend(demo.submit_batch(&[limit("c2", "BTCUSDT")]));
    assert_eq!(reasons(&all), [venue_text, venue_text], "demo keeps the venue's own words");
}
