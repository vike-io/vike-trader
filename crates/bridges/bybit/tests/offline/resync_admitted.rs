//! **The reconnect-resync and the gap-sentinel replay the history of the symbols the engine trades, not only
//! the mounted one** (decision 0174's reopener; offline: a recording `BybitTransport`, canned envelopes, no
//! network, nothing `#[ignore]`d).
//!
//! A fill or a terminal that landed on an ADMITTED symbol while the private socket was down used to be found
//! by the next reconcile pass or by nothing: the replay read `order/history` and `execution/list` for the
//! mounted symbol alone. `bybit_history_events_of` reads the mounted symbol FIRST with the requests it has
//! always made and then each foreign symbol with the SAME two requests on its own name.
//!
//! Pins: nothing foreign = the mounted symbol's two requests and nothing else; a foreign symbol is read after
//! it with `category`/`symbol`/`limit` in the same shape; a repeat, an empty label and the mounted symbol are
//! never read twice; the first answer that says "slow down" ends the walk (the mounted symbol's events still
//! come back); any other failed read costs its own rows only; the router records the foreign symbols a
//! command named and a drain starts the list again.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::exec::{bybit_history_events, bybit_history_events_of};
use vike_bybit::per_symbol::{CoidSymbols, PerSymbolRest};
use vike_bybit::perp::{PATH_EXECUTION_LIST, PATH_ORDER_HISTORY};
use vike_bybit::transport::BybitTransport;
use vike_bybit::user_data::CancelNotes;
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::r6_bybit_parity::{bybit_perp, fixture};

type Asked = Arc<Mutex<Vec<(String, String)>>>;

/// Records `(path, symbol)` of every history read; each answers one `Trade` execution on the symbol asked
/// about (`x-<symbol>`), with the order-history list empty. `fail_on` makes one symbol's reads fail with
/// `code`.
struct Venue {
    asked: Asked,
    fail_on: Option<(&'static str, i64)>,
}

impl BybitTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        let symbol = params
            .iter()
            .find(|(k, _)| *k == "symbol")
            .and_then(|(_, v)| v.as_str().map(str::to_string))
            .unwrap_or_default();
        self.asked.lock().unwrap().push((path.to_string(), symbol.clone()));
        if let Some((bad, code)) = self.fail_on
            && bad == symbol
        {
            return Err(VenueApiError { code, msg: format!("refused: {symbol}") });
        }
        let list = match path {
            PATH_EXECUTION_LIST => vec![json!({
                "symbol": symbol, "execType": "Trade", "orderId": format!("o-{symbol}"),
                "orderLinkId": format!("c-{symbol}"), "execId": format!("x-{symbol}"),
                "execPrice": "10", "execQty": "1", "side": "Buy", "execFee": "0.1",
                "feeCurrency": "USDT", "isMaker": false, "execTime": "5",
            })],
            _ => Vec::new(),
        };
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"list": list}}))
    }
}

fn rest(fail_on: Option<(&'static str, i64)>) -> (vike_bybit::perp::BybitPerpRest<Venue>, Asked) {
    let asked: Asked = Arc::default();
    (bybit_perp(&fixture(), Venue { asked: Arc::clone(&asked), fail_on }), asked)
}

fn asked(a: &Asked) -> Vec<(String, String)> {
    a.lock().unwrap().clone()
}

fn pair(path: &str, symbol: &str) -> (String, String) {
    (path.to_string(), symbol.to_string())
}

fn trade_ids(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.to_string()),
            _ => None,
        })
        .collect()
}

fn labels(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.symbol.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn nothing_foreign_is_the_mounted_symbols_two_requests_and_nothing_else() {
    let (client, seen) = rest(None);
    let notes = CancelNotes::default();
    let events = bybit_history_events_of(&client, "BTCUSDT", &[], &notes);
    assert_eq!(
        asked(&seen),
        [pair(PATH_ORDER_HISTORY, "BTCUSDT"), pair(PATH_EXECUTION_LIST, "BTCUSDT")]
    );
    let (again, _) = rest(None);
    let plain = bybit_history_events(&again, "BTCUSDT", &notes);
    assert_eq!(
        trade_ids(&events),
        trade_ids(&plain),
        "the old entry point is the same walk with nothing foreign"
    );
    assert_eq!(trade_ids(&events), ["x-BTCUSDT"]);
}

#[test]
fn a_foreign_symbol_is_read_after_the_mounted_one_with_the_same_two_requests() {
    let (client, seen) = rest(None);
    let foreign: Vec<String> =
        ["ETHUSDT", "ethusdt", "BTCUSDT", "", "SOLUSDT"].map(String::from).to_vec();
    let events = bybit_history_events_of(&client, "BTCUSDT", &foreign, &CancelNotes::default());
    assert_eq!(
        asked(&seen),
        [
            pair(PATH_ORDER_HISTORY, "BTCUSDT"),
            pair(PATH_EXECUTION_LIST, "BTCUSDT"),
            pair(PATH_ORDER_HISTORY, "ETHUSDT"),
            pair(PATH_EXECUTION_LIST, "ETHUSDT"),
            pair(PATH_ORDER_HISTORY, "SOLUSDT"),
            pair(PATH_EXECUTION_LIST, "SOLUSDT"),
        ],
        "mounted first; a repeat (any case), the mounted symbol and an empty label are never read twice"
    );
    assert_eq!(trade_ids(&events), ["x-BTCUSDT", "x-ETHUSDT", "x-SOLUSDT"]);
    assert_eq!(
        labels(&events),
        ["BTCUSDT", "ETHUSDT", "SOLUSDT"],
        "each fill carries the symbol it happened on, which the engine routes by"
    );
}

#[test]
fn the_first_answer_that_says_slow_down_ends_the_walk_and_the_mounted_events_still_come_back() {
    let (client, seen) = rest(Some(("ETHUSDT", 10006)));
    let foreign: Vec<String> = ["ETHUSDT", "SOLUSDT"].map(String::from).to_vec();
    let events = bybit_history_events_of(&client, "BTCUSDT", &foreign, &CancelNotes::default());
    let reads = asked(&seen);
    assert!(
        reads.iter().all(|(_, s)| s != "SOLUSDT"),
        "nothing is asked after a rate limit: {reads:?}"
    );
    assert_eq!(trade_ids(&events), ["x-BTCUSDT"], "the mounted symbol's replay is not lost");

    // Cloudflare answers a rate-limited IP with a 403 on this venue.
    let (client, seen) = rest(Some(("ETHUSDT", 403)));
    bybit_history_events_of(&client, "BTCUSDT", &foreign, &CancelNotes::default());
    assert!(asked(&seen).iter().all(|(_, s)| s != "SOLUSDT"));
}

#[test]
fn any_other_failed_read_costs_its_own_rows_only() {
    let (client, seen) = rest(Some(("ETHUSDT", 10001)));
    let foreign: Vec<String> = ["ETHUSDT", "SOLUSDT"].map(String::from).to_vec();
    let events = bybit_history_events_of(&client, "BTCUSDT", &foreign, &CancelNotes::default());
    assert_eq!(trade_ids(&events), ["x-BTCUSDT", "x-SOLUSDT"], "ETH failed, SOL was still read");
    assert!(asked(&seen).contains(&pair(PATH_EXECUTION_LIST, "SOLUSDT")));
}

fn order(coid: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        ..OrderRequest::default()
    }
}

/// The router names the foreign symbols a command touched, once each, upper-cased; the mounted symbol and an
/// order of an unknown coid (the mounted symbol again) name nothing; draining starts the list again.
#[test]
fn the_router_records_the_foreign_symbols_a_command_named() {
    let coids = Arc::new(CoidSymbols::default());
    let router = PerSymbolRest::new(
        "BTCUSDT",
        rest(None).0,
        Box::new(|_| Ok(rest(None).0)),
        Arc::clone(&coids),
    );
    router.submit_order(&order("c1", "BTCUSDT"));
    router.submit_order(&order("c2", "ethusdt"));
    router.submit_order(&order("c3", "ETHUSDT"));
    let _ = router.cancel_order("c2");
    let _ = router.cancel_order("never-seen");
    assert_eq!(coids.take_touched(), ["ETHUSDT"]);
    assert!(coids.take_touched().is_empty(), "a drain starts the list again");
    let _ = router.cancel_order("c2");
    assert_eq!(coids.take_touched(), ["ETHUSDT"], "a cancel of a foreign order names its symbol");
}

/// The list is bounded by the engine's own cap, so a stream of distinct symbols cannot grow it.
#[test]
fn the_touched_list_is_bounded_by_the_engines_cap() {
    let coids = CoidSymbols::default();
    for i in 0..(vike_exec::MAX_EXTRA_SYMBOLS + 5) {
        coids.touch(&format!("S{i}USDT"));
    }
    coids.touch("");
    assert_eq!(coids.take_touched().len(), vike_exec::MAX_EXTRA_SYMBOLS);
}
