//! **The reconnect-resync and the gap-sentinel replay the history of the symbols the engine trades, not only
//! the mounted one** (decision 0174's reopener; offline: a recording `RestTransport`, canned rows, nothing
//! `#[ignore]`d), aster's face. `crates/bridges/binance/tests/offline/resync_admitted.rs` is the full pin of
//! the family walk and mappers (`vike_binance::family::history`); this twin proves aster's own wire paths
//! (`/fapi/v3`, `/api/v3`), its venue word, and that a foreign fill carries the scoped trade id the stream
//! spells (`SYMBOL:t`, `t` being a per-symbol sequence) while the mounted symbol's stays bare.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_aster::exec::{perp_history_events_of, spot_history_events_of};
use vike_aster::perp::AsterPerpRest;
use vike_aster::spot::AsterSpotRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::SymbolProperties;
use vike_model::events::Event;

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

type Asked = Arc<Mutex<Vec<(String, String)>>>;

/// Records `(path, symbol)` of every read; every symbol holds one FILLED order and one trade with `id` 7.
/// `fail_on` makes every read of one symbol answer HTTP 429.
struct Wire {
    asked: Asked,
    fail_on: Option<&'static str>,
}

impl RestTransport for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        let symbol =
            params.iter().find(|(k, _)| *k == "symbol").map(|(_, v)| v.clone()).unwrap_or_default();
        self.asked.lock().unwrap().push((path.to_string(), symbol.clone()));
        if self.fail_on == Some(symbol.as_str()) {
            return Err(VenueApiError { code: 429, msg: "too many requests".to_string() });
        }
        Ok(match path {
            "/fapi/v3/allOrders" | "/api/v3/allOrders" => json!([{
                "orderId": 9, "clientOrderId": format!("c-{symbol}"), "status": "FILLED",
                "updateTime": 5,
            }]),
            "/fapi/v3/userTrades" => json!([{
                "id": 7, "orderId": 9, "price": "10", "qty": "1", "commission": "0.1",
                "commissionAsset": "USDT", "time": 5, "side": "BUY", "maker": false,
                "positionSide": "BOTH",
            }]),
            "/api/v3/userTrades" => json!([{
                "id": 7, "orderId": 9, "price": "10", "qty": "1", "commission": "0.1",
                "commissionAsset": "USDT", "time": 5, "side": "BUY", "maker": false,
            }]),
            other => panic!("not a history read: {other}"),
        })
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("no public call expected ({path})")
    }
}

fn perp(fail_on: Option<&'static str>) -> (AsterPerpRest<NullSigner, Wire>, Asked) {
    let asked: Asked = Arc::default();
    let rest = AsterPerpRest {
        signer: NullSigner,
        transport: Wire { asked: Arc::clone(&asked), fail_on },
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
        builder: None,
    };
    (rest, asked)
}

fn spot() -> (AsterSpotRest<NullSigner, Wire>, Asked) {
    let asked: Asked = Arc::default();
    let rest = AsterSpotRest {
        signer: NullSigner,
        transport: Wire { asked: Arc::clone(&asked), fail_on: None },
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        base_asset: String::new(),
        builder: None,
    };
    (rest, asked)
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn fills(events: &[Event]) -> Vec<(String, String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => {
                Some((f.venue.to_string(), f.symbol.to_string(), f.trade_id.to_string()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_foreign_perp_symbol_is_read_on_the_v3_paths_and_its_fill_id_is_scoped() {
    let (client, seen) = perp(None);
    let events = perp_history_events_of(
        &client,
        "BTCUSDT.P",
        &strings(&["ETHUSDT.P", "BTCUSDT.P", "SOLUSDT"]),
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        [
            ("/fapi/v3/allOrders".to_string(), "BTCUSDT".to_string()),
            ("/fapi/v3/userTrades".to_string(), "BTCUSDT".to_string()),
            ("/fapi/v3/allOrders".to_string(), "ETHUSDT".to_string()),
            ("/fapi/v3/userTrades".to_string(), "ETHUSDT".to_string()),
        ],
        "the mounted symbol first; the mounted symbol again and the spot symbol (the other lane) are not read"
    );
    assert_eq!(
        fills(&events),
        [
            ("aster".to_string(), "BTCUSDT.P".to_string(), "7".to_string()),
            ("aster".to_string(), "ETHUSDT.P".to_string(), "ETHUSDT.P:7".to_string()),
        ]
    );
}

#[test]
fn a_foreign_spot_symbol_is_read_on_the_spot_paths_and_its_fill_id_is_scoped() {
    let (client, seen) = spot();
    let events = spot_history_events_of(&client, "BTCUSDT", &strings(&["ETHUSDT"]));
    assert_eq!(
        seen.lock().unwrap().clone(),
        [
            ("/api/v3/allOrders".to_string(), "BTCUSDT".to_string()),
            ("/api/v3/userTrades".to_string(), "BTCUSDT".to_string()),
            ("/api/v3/allOrders".to_string(), "ETHUSDT".to_string()),
            ("/api/v3/userTrades".to_string(), "ETHUSDT".to_string()),
        ]
    );
    assert_eq!(
        fills(&events),
        [
            ("aster".to_string(), "BTCUSDT".to_string(), "7".to_string()),
            ("aster".to_string(), "ETHUSDT".to_string(), "ETHUSDT:7".to_string()),
        ]
    );
}

#[test]
fn a_rate_limit_ends_the_walk_and_the_mounted_events_still_come_back() {
    let (client, seen) = perp(Some("ETHUSDT"));
    let events =
        perp_history_events_of(&client, "BTCUSDT.P", &strings(&["ETHUSDT.P", "XRPUSDT.P"]));
    assert!(seen.lock().unwrap().iter().all(|(_, s)| s != "XRPUSDT"));
    assert_eq!(fills(&events).len(), 1);
}

#[test]
fn nothing_foreign_is_the_mounted_symbols_two_requests_and_nothing_else() {
    let (client, seen) = perp(None);
    perp_history_events_of(&client, "BTCUSDT.P", &[]);
    assert_eq!(seen.lock().unwrap().len(), 2);
}
