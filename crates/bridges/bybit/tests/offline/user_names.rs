//! **The SpreadMaker mount comes back on the LINEAR book under its USER name** (decision 0290, SYM-NAMES step 2,
//! offline). the CI box's `tradehub` profile mounts bybit `CryptoPerp` as mount 0; the deploy renames its symbol from the
//! old exec spelling `BTCUSDT` to the user name `BTCUSDT.P`. Over the bridge's held kept list
//! (`vike_bybit::history::kept_list_for_tests`, rows keyed and typed as `crates/bridges/bybit/src/catalog.rs` keeps the
//! venue's listing) every seam the mount touches speaks that name and signs the kept row's venue name:
//!
//! * an order on `BTCUSDT.P` signs `symbol=BTCUSDT&category=linear`; the bare `BTCUSDT` is the SPOT pair
//!   (`category=spot`), and `BTCUSD.P` the inverse book (`symbol=BTCUSD&category=inverse`);
//! * its ticks, bars, marks, trades and depth route to the LINEAR book (`vike_bybit::history::live_route`, the one
//!   route every feed lane and the tick pump read in a process that keeps the list);
//! * a fill of the linear `BTCUSDT` comes back as `BTCUSDT.P`, a fill of the spot `BTCUSDT` as the pair `BTCUSDT`.
//!
//! The order client is built as the exec thread builds every client of a name (`execution/mod.rs`'s
//! `order_rest_gated`): the venue's name and the book `vike_bybit::instrument::class::venue_symbol` reads off the kept
//! row, over a transport that records the signed request instead of sending it.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::event_mapper::map_bybit_perp;
use vike_bybit::execution::rest::{PATH_ORDER_CREATE, VenueRestBybit};
use vike_bybit::history::Category;
use vike_bybit::instrument::class::venue_symbol;
use vike_bybit::signer::BybitV5Signer;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties};
use vike_secrets::credentials::Credentials;

/// Every signed request: its path and its parameters.
type Signed = Arc<Mutex<Vec<(String, Vec<(String, Value)>)>>>;

struct Recorder(Signed);

impl RestTransportJson for Recorder {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        self.0.lock().unwrap().push((path.to_string(), owned));
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"orderId": "oid-1"}}))
    }
}

/// The client the exec thread builds for the USER name `user`, and what it signed.
fn client_of(user: &str) -> (VenueRestBybit<Recorder>, Signed) {
    vike_bybit::history::kept_list_for_tests();
    let (wire, category) = venue_symbol(user).unwrap_or_else(|why| panic!("{user}: {why}"));
    let signed = Signed::default();
    let client = VenueRestBybit {
        signer: BybitV5Signer::new(&Credentials::for_test("k", "s", None), || 0),
        transport: Recorder(Arc::clone(&signed)),
        base_url: "https://unused.invalid".to_string(),
        mainnet: false,
        symbol: wire,
        properties: SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() },
        leverage: 2.0,
        category,
    };
    (client, signed)
}

/// A limit order on `user`, as the engine sends it (the order names the USER name).
fn limit(user: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: "sm1".into(),
        symbol: user.into(),
        side: 1,
        qty: 0.002,
        order_type: OrderType::Limit,
        price: Some(50_000.0),
        ..Default::default()
    }
}

/// The `symbol` and `category` of the one create `user`'s order signed.
fn signed_create(user: &str) -> (Value, Value) {
    let (client, signed) = client_of(user);
    let events = client.submit_order(&limit(user));
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{user}: {events:?}");
    let signed = signed.lock().unwrap();
    let [(path, params)] = signed.as_slice() else { panic!("{user}: one create: {signed:?}") };
    assert_eq!(path, PATH_ORDER_CREATE);
    let param = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap();
    (param("symbol"), param("category"))
}

/// **An order on the mount `BTCUSDT.P` signs the LINEAR `BTCUSDT`**; the bare `BTCUSDT` is the spot pair, and
/// `BTCUSD.P` the inverse book.
#[test]
fn an_order_on_the_user_name_signs_its_kept_rows_venue_name_and_book() {
    assert_eq!(signed_create("BTCUSDT.P"), (json!("BTCUSDT"), json!("linear")));
    assert_eq!(signed_create("BTCUSDT"), (json!("BTCUSDT"), json!("spot")));
    assert_eq!(signed_create("BTCUSD.P"), (json!("BTCUSD"), json!("inverse")));
    assert_eq!(signed_create("BTCUSDC.P"), (json!("BTCPERP"), json!("linear")));
}

/// **The mount's ticks and bars are the LINEAR book's** (BYB-2, BYB-6): every feed lane and the tick pump route a
/// user name by its kept row, with no listing read (the listings host here is a closed port).
#[test]
fn the_mounts_market_data_routes_to_the_linear_book() {
    vike_bybit::history::kept_list_for_tests();
    const NO_LISTING: &str = "http://127.0.0.1:1";
    let route = |user: &str| vike_bybit::history::live_route(NO_LISTING, user);
    assert_eq!(route("BTCUSDT.P"), Ok(("BTCUSDT".to_string(), Category::Linear)));
    assert_eq!(Category::Linear.wire(), "linear", "the socket and the REST seed's category");
    assert_eq!(route("BTCUSDT"), Ok(("BTCUSDT".to_string(), Category::Spot)), "the pair");
    assert_eq!(route("BTCUSD.P"), Ok(("BTCUSD".to_string(), Category::Inverse)));
    assert!(route("BTCUSDT.S").is_err(), "the old spot spelling is no user name");
}

/// **A fill of the mount comes back as `BTCUSDT.P`**: the private stream's linear `BTCUSDT` row, the venue's own
/// captured frame, is the mount's user name; the same venue name in the spot book is the pair's.
#[test]
fn a_fill_comes_back_under_the_user_name() {
    vike_bybit::history::kept_list_for_tests();
    let captured: Value = serde_json::from_str(include_str!("../fixtures/captured/ws_fill.json"))
        .expect("the captured fill frames");
    let frame = &captured["frames"][0];
    assert_eq!(frame["data"][0]["category"], "linear");
    assert_eq!(frame["data"][0]["symbol"], "BTCUSDT");
    let fills = |frame: &Value| -> Vec<String> {
        map_bybit_perp(frame, "bybit", "BTCUSDT.P")
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f.symbol.to_string()),
                _ => None,
            })
            .collect()
    };
    let linear = fills(frame);
    assert!(!linear.is_empty() && linear.iter().all(|s| s == "BTCUSDT.P"), "{linear:?}");
    let mut spot = frame.clone();
    for row in spot["data"].as_array_mut().expect("rows") {
        row["category"] = json!("spot");
    }
    let spot = fills(&spot);
    assert!(!spot.is_empty() && spot.iter().all(|s| s == "BTCUSDT"), "{spot:?}");
}
