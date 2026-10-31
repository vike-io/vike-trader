//! **The reconnect-resync and the gap-sentinel replay the history of the symbols the engine trades, not only
//! the mounted one** (decision 0174's reopener; offline: a recording `RestTransport`, canned rows, no network,
//! nothing `#[ignore]`d). `crates/bridges/aster/tests/offline/resync_admitted.rs` is the aster twin: the
//! walk and the mappers are the family's (`family::history`).
//!
//! A fill or a terminal that landed on an ADMITTED symbol while the private socket was down used to be found
//! by the next reconcile pass or by nothing: the replay read `allOrders`, `userTrades` (or `myTrades`) and, on
//! the perp lane, `allAlgoOrders` for the mounted symbol alone. `perp_history_events_of` and
//! `spot_history_events_of` read the mounted symbol FIRST with the requests it has always made and then each
//! foreign symbol with the SAME requests on its own exchange symbol.
//!
//! `t` is a per-symbol sequence on this grammar, so a foreign fill carries the symbol-qualified id the
//! user-data stream spells for it (`family::fill_trade_id`): a replayed fill the stream already folded is then
//! a KNOWN id, and the same `t` on two symbols is two ids. Pins: nothing foreign = the mounted symbol's
//! requests and nothing else; the foreign order of reads with a repeat, the mounted symbol, an empty label and
//! a symbol of the other lane skipped; a liquidation carries the scoped id too; the first rate-limit answer
//! ends the walk (the mounted symbol's events still come back).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_binance::exec::{perp_history_events, perp_history_events_of, spot_history_events_of};
use vike_binance::perp::BinancePerpRest;
use vike_binance::spot::BinanceSpotRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::SymbolProperties;
use vike_model::events::Event;

use crate::tif_gate::NullSigner;

type Asked = Arc<Mutex<Vec<(String, String)>>>;

/// Records `(path, symbol)` of every read. Every symbol holds one FILLED order and one trade with `id` 7
/// (per-symbol numbering) on it, and no algo order.
struct Wire {
    asked: Asked,
    /// `(symbol, http code)`: every read of that symbol answers this error.
    fail_on: Option<(&'static str, i64)>,
    /// The order's client id is a venue liquidation (`autoclose-…`).
    liquidation: bool,
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
        if let Some((bad, code)) = self.fail_on
            && bad == symbol
        {
            return Err(VenueApiError { code, msg: format!("refused: {symbol}") });
        }
        let coid =
            if self.liquidation { format!("autoclose-{symbol}") } else { format!("c-{symbol}") };
        Ok(match path {
            "/fapi/v1/allOrders" | "/api/v3/allOrders" => json!([{
                "orderId": 9, "clientOrderId": coid, "status": "FILLED", "updateTime": 5,
            }]),
            "/fapi/v1/userTrades" => json!([{
                "id": 7, "orderId": 9, "price": "10", "qty": "1", "commission": "0.1",
                "commissionAsset": "USDT", "time": 5, "side": "BUY", "maker": false,
                "positionSide": "BOTH",
            }]),
            "/api/v3/myTrades" => json!([{
                "id": 7, "orderId": 9, "price": "10", "qty": "1", "commission": "0.1",
                "commissionAsset": "USDT", "time": 5, "isBuyer": true, "isMaker": false,
            }]),
            _ => json!([]),
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

fn wire(fail_on: Option<(&'static str, i64)>, liquidation: bool) -> (Wire, Asked) {
    let asked: Asked = Arc::default();
    (Wire { asked: Arc::clone(&asked), fail_on, liquidation }, asked)
}

fn perp(transport: Wire) -> BinancePerpRest<NullSigner, Wire> {
    BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport,
        base_url: "https://unused.invalid".to_string(),
        algo_coids: Default::default(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    }
}

fn spot(transport: Wire) -> BinanceSpotRest<NullSigner, Wire> {
    BinanceSpotRest {
        link_id: None,
        signer: NullSigner,
        transport,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        base_asset: String::new(),
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn pair(path: &str, symbol: &str) -> (String, String) {
    (path.to_string(), symbol.to_string())
}

/// `(label, trade id)` of every bare fill.
fn fills(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some((f.symbol.to_string(), f.trade_id.to_string())),
            _ => None,
        })
        .collect()
}

#[test]
fn nothing_foreign_is_the_mounted_symbols_three_requests_and_nothing_else() {
    let (w, seen) = wire(None, false);
    let client = perp(w);
    let events = perp_history_events_of(&client, "BTCUSDT.P", &[]);
    assert_eq!(
        seen.lock().unwrap().clone(),
        [
            pair("/fapi/v1/allOrders", "BTCUSDT"),
            pair("/fapi/v1/userTrades", "BTCUSDT"),
            pair("/fapi/v1/allAlgoOrders", "BTCUSDT"),
        ]
    );
    assert_eq!(fills(&events), [("BTCUSDT.P".to_string(), "7".to_string())]);
    assert_eq!(
        fills(&events),
        fills(&perp_history_events(&client, "BTCUSDT.P")),
        "the old entry point is the same walk with nothing foreign"
    );
}

#[test]
fn a_foreign_perp_symbol_is_read_after_the_mounted_one_and_its_fill_id_is_scoped() {
    let (w, seen) = wire(None, false);
    let client = perp(w);
    let foreign = strings(&["ETHUSDT.P", "ethusdt.p", "BTCUSDT.P", "", "SOLUSDT", "XRPUSDT.P"]);
    let events = perp_history_events_of(&client, "BTCUSDT.P", &foreign);
    let reads: Vec<(String, String)> = seen.lock().unwrap().clone();
    let symbols: Vec<&str> = reads.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(
        symbols,
        [
            "BTCUSDT", "BTCUSDT", "BTCUSDT", "ETHUSDT", "ETHUSDT", "ETHUSDT", "XRPUSDT", "XRPUSDT",
            "XRPUSDT"
        ],
        "mounted first; a repeat (any case), the mounted symbol, an empty label and the SPOT symbol (the \
         other lane) are never read"
    );
    assert_eq!(
        reads[3],
        pair("/fapi/v1/allOrders", "ETHUSDT"),
        "the same three reads, on its own name"
    );
    assert_eq!(
        fills(&events),
        [
            ("BTCUSDT.P".to_string(), "7".to_string()),
            ("ETHUSDT.P".to_string(), "ETHUSDT.P:7".to_string()),
            ("XRPUSDT.P".to_string(), "XRPUSDT.P:7".to_string()),
        ],
        "the same `t` on three symbols is three ids: the mounted one bare, the rest scoped as the stream spells them"
    );
}

#[test]
fn a_foreign_liquidation_carries_the_scoped_id_too() {
    let (w, _seen) = wire(None, true);
    let client = perp(w);
    let events = perp_history_events_of(&client, "BTCUSDT.P", &strings(&["ETHUSDT.P"]));
    let liquidated: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match e {
            Event::PositionLiquidated(l) => Some((l.symbol.to_string(), l.trade_id.to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(
        liquidated,
        [
            ("BTCUSDT.P".to_string(), "7".to_string()),
            ("ETHUSDT.P".to_string(), "ETHUSDT.P:7".to_string()),
        ]
    );
    assert!(fills(&events).is_empty(), "a liquidation replays as a liquidation, never as a fill");
}

#[test]
fn the_first_answer_that_says_slow_down_ends_the_walk_and_the_mounted_events_still_come_back() {
    for code in [429, 418, -1003] {
        let (w, seen) = wire(Some(("ETHUSDT", code)), false);
        let client = perp(w);
        let events =
            perp_history_events_of(&client, "BTCUSDT.P", &strings(&["ETHUSDT.P", "XRPUSDT.P"]));
        assert!(
            seen.lock().unwrap().iter().all(|(_, s)| s != "XRPUSDT"),
            "{code}: nothing is asked after a rate limit"
        );
        assert_eq!(fills(&events).len(), 1, "{code}: the mounted symbol's replay is not lost");
    }
    // Any other failure costs its own rows only.
    let (w, seen) = wire(Some(("ETHUSDT", -1121)), false);
    let client = perp(w);
    let events =
        perp_history_events_of(&client, "BTCUSDT.P", &strings(&["ETHUSDT.P", "XRPUSDT.P"]));
    assert_eq!(fills(&events).len(), 2, "ETH failed, XRP was still read");
    assert!(seen.lock().unwrap().iter().any(|(_, s)| s == "XRPUSDT"));
}

#[test]
fn a_foreign_spot_symbol_is_read_with_the_two_spot_reads_and_a_perp_label_is_not_this_lane() {
    let (w, seen) = wire(None, false);
    let client = spot(w);
    let events =
        spot_history_events_of(&client, "BTCUSDT", &strings(&["ETHUSDT", "SOLUSDT.P", "BTCUSDT"]));
    assert_eq!(
        seen.lock().unwrap().clone(),
        [
            pair("/api/v3/allOrders", "BTCUSDT"),
            pair("/api/v3/myTrades", "BTCUSDT"),
            pair("/api/v3/allOrders", "ETHUSDT"),
            pair("/api/v3/myTrades", "ETHUSDT"),
        ]
    );
    assert_eq!(
        fills(&events),
        [
            ("BTCUSDT".to_string(), "7".to_string()),
            ("ETHUSDT".to_string(), "ETHUSDT:7".to_string()),
        ]
    );
}
