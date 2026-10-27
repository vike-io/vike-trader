//! **The reconcile client reads every ADMITTED instrument of the account, not only the mounted one**
//! (decisions 0173 and 0174; offline: a recording `OkxTransport`, canned envelopes, nothing `#[ignore]`d).
//!
//! The restore planner reads an order the venue's report does not carry as GONE, so a client that read
//! the mounted instrument alone made every resting order on an instrument admitted later invisible after
//! a restart (the node HELD them instead, and `vike_gatewayhub_client::trade::RECON_READS_ADMITTED` did
//! not list okx). The client is told which instruments the engine admitted (`ReconClient::admit_symbol`)
//! and reads each one with the SAME request shape it uses for the mounted one.
//!
//! OKX sizes in CONTRACTS, so the instrument is part of every quantity: each admitted instrument is
//! scaled by ITS OWN `ctVal`, read once from `public/instruments`. Pins: no admitted instrument = the
//! mounted instrument's requests byte for byte and no public call at all; an admitted instrument's
//! orders, stops, fills and positions appear in base units of its own contract; positions are ONE
//! account-wide read; one failed read fails the whole fetch; a delisted instrument is read as holding
//! nothing; an inverse or multiplied one fails the pass; the fired-stop naming policy counts passes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_exec::recon::ReconClient;
use vike_okx::OkxReconClient;
use vike_okx::transport::OkxTransport;
use vike_secrets::credentials::Credentials;

const BTC: &str = "BTC-USDT-SWAP";
const ETH: &str = "ETH-USDT-SWAP";
const XRP: &str = "XRP-USDT-SWAP";
const DOGE: &str = "DOGE-USDT-SWAP";
const BTC_CT: f64 = 0.01;

type Call = (String, Value);
type Calls = Arc<Mutex<Vec<Call>>>;

fn envelope(rows: Vec<Value>) -> Value {
    json!({"code": "0", "msg": "", "data": rows})
}

fn swap_row(inst: &str, ct_val: &str, ct_mult: &str, ct_type: &str) -> Value {
    json!({
        "instType": "SWAP", "instId": inst, "uly": "X-USDT", "settleCcy": "USDT",
        "ctVal": ct_val, "ctMult": ct_mult, "ctValCcy": "X", "ctType": ct_type, "lever": "100",
        "tickSz": "0.01", "lotSz": "1", "minSz": "1", "maxMktSz": "1000", "state": "live"
    })
}

/// A scripted OKX: every instrument holds one resting order, one resting stop, one fill and (in the
/// account-wide positions read) one position, all sized in contracts. `fail` makes ONE read answer an
/// error; `history_down` makes every `orders-algo-history` read fail.
struct Venue {
    calls: Calls,
    fail: Mutex<Option<(&'static str, Option<&'static str>)>>,
    history_down: Mutex<bool>,
    instruments: HashMap<&'static str, Value>,
}

fn short(inst: &str) -> String {
    inst.split('-').next().unwrap_or(inst).to_lowercase()
}

impl Venue {
    fn new() -> (Self, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let instruments = HashMap::from([
            (ETH, envelope(vec![swap_row(ETH, "0.1", "1", "linear")])),
            (XRP, envelope(vec![swap_row(XRP, "1", "1", "linear")])),
            (DOGE, envelope(vec![swap_row(DOGE, "10", "1", "linear")])),
            (
                "USD-INVERSE-SWAP",
                envelope(vec![swap_row("USD-INVERSE-SWAP", "100", "1", "inverse")]),
            ),
            ("MULT-USDT-SWAP", envelope(vec![swap_row("MULT-USDT-SWAP", "1", "10", "linear")])),
            (
                "GONE-USDT-SWAP",
                json!({"code": "51001", "msg": "Instrument ID doesn't exist", "data": []}),
            ),
            ("BUSY-USDT-SWAP", json!({"code": "50013", "msg": "System is busy", "data": []})),
        ]);
        (
            Venue {
                calls: Arc::clone(&calls),
                fail: Mutex::new(None),
                history_down: Mutex::new(false),
                instruments,
            },
            calls,
        )
    }

    fn failing(self, path: &'static str, inst: Option<&'static str>) -> Self {
        *self.fail.lock().unwrap() = Some((path, inst));
        self
    }

    fn with_history_down(self) -> Self {
        *self.history_down.lock().unwrap() = true;
        self
    }
}

impl OkxTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        let tail = path.rsplit('/').next().unwrap_or(path).to_string();
        let map: serde_json::Map<String, Value> =
            params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        let inst = map.get("instId").and_then(Value::as_str).map(str::to_string);
        self.calls.lock().unwrap().push((tail.clone(), Value::Object(map)));
        if let Some((fail_tail, fail_inst)) = *self.fail.lock().unwrap()
            && fail_tail == tail
            && fail_inst.is_none_or(|i| Some(i) == inst.as_deref())
        {
            return Ok(json!({"code": "50011", "msg": "Requests too frequent", "data": []}));
        }
        if tail == "orders-algo-history" && *self.history_down.lock().unwrap() {
            return Ok(json!({"code": "50013", "msg": "System is busy", "data": []}));
        }
        let inst = inst.unwrap_or_default();
        Ok(envelope(match tail.as_str() {
            "orders-pending" => vec![json!({
                "instId": inst, "ordId": format!("o-{}", short(&inst)), "clOrdId": format!("c-{}", short(&inst)),
                "side": "buy", "ordType": "limit", "sz": "3", "accFillSz": "1", "avgPx": "0",
                "state": "partially_filled", "uTime": "5"
            })],
            "orders-algo-pending" => vec![json!({
                "instId": inst, "algoId": format!("a-{}", short(&inst)),
                "algoClOrdId": format!("s-{}", short(&inst)), "ordType": "conditional",
                "side": "sell", "sz": "2", "state": "live", "uTime": "6"
            })],
            "fills" => vec![json!({
                "instId": inst, "tradeId": format!("t-{}", short(&inst)),
                "ordId": format!("child-{}", short(&inst)), "clOrdId": format!("venue-{}", short(&inst)),
                "side": "sell", "fillSz": "4", "fillPx": "10", "fee": "-0.1", "feeCcy": "USDT",
                "execType": "T", "ts": "7"
            })],
            "orders-algo-history" => vec![json!({
                "instId": inst, "algoClOrdId": format!("s-{}", short(&inst)),
                "ordIdList": [format!("child-{}", short(&inst))], "ordId": ""
            })],
            "positions" => {
                // account-wide when no instId is asked: BTC long 2, ETH short 3, and an instrument nobody
                // admitted; instrument-filtered: that instrument's row alone.
                let all = [(BTC, "2"), (ETH, "-3"), (DOGE, "9")];
                all.iter()
                    .filter(|(i, _)| inst.is_empty() || *i == inst)
                    .map(|(i, pos)| {
                        json!({"instId": i, "pos": pos, "posSide": "net", "avgPx": "10",
                               "mgnMode": "cross", "uTime": "8"})
                    })
                    .collect()
            }
            other => {
                panic!("a reconcile pass reads orders, stops, fills and positions only: {other}")
            }
        }))
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        let inst = params.iter().find(|(k, _)| *k == "instId").map(|(_, v)| v.clone()).unwrap();
        self.calls.lock().unwrap().push((
            path.rsplit('/').next().unwrap().to_string(),
            json!({"instType": "SWAP", "instId": inst}),
        ));
        Ok(self
            .instruments
            .get(inst.as_str())
            .unwrap_or_else(|| panic!("an instrument nobody scripted was asked for: {inst}"))
            .clone())
    }
}

fn client(venue: Venue) -> OkxReconClient<Venue> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    OkxReconClient::new(
        OkxV5Signer::new(&creds, || 0),
        venue,
        "https://unused.invalid",
        BTC,
        BTC_CT,
    )
}

fn paths(calls: &Calls) -> Vec<String> {
    calls.lock().unwrap().iter().map(|(p, _)| p.clone()).collect()
}

fn asked(calls: &Calls, path: &str) -> Vec<Value> {
    calls.lock().unwrap().iter().filter(|(p, _)| p == path).map(|(_, v)| v.clone()).collect()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// With nothing admitted a pass is the mounted instrument's requests, byte for byte, in the order it
/// always made them, and `public/instruments` is never called.
#[test]
fn with_nothing_admitted_a_pass_asks_exactly_what_it_always_did() {
    let (venue, calls) = Venue::new();
    let c = client(venue);
    c.fetch_order_status_reports(0).unwrap();
    c.fetch_fill_reports(0).unwrap();
    c.fetch_position_status_reports().unwrap();
    assert_eq!(
        paths(&calls),
        ["orders-pending", "orders-algo-pending", "fills", "orders-algo-history", "positions"]
    );
    assert_eq!(asked(&calls, "orders-pending"), [json!({"instId": BTC})]);
    assert_eq!(
        asked(&calls, "orders-algo-pending"),
        [json!({"ordType": "conditional", "instId": BTC, "limit": "100"})]
    );
    assert_eq!(asked(&calls, "fills"), [json!({"instId": BTC})]);
    assert_eq!(asked(&calls, "positions"), [json!({"instType": "SWAP", "instId": BTC})]);
}

/// An admitted instrument's resting order and stop are read, each scaled by THAT instrument's `ctVal`
/// (3 contracts of ETH are 0.3 ETH; 3 contracts of BTC are 0.03 BTC), and its contract size is asked
/// for ONCE however many passes follow.
#[test]
fn an_admitted_instruments_orders_and_stops_use_its_own_contract_size() {
    let (venue, calls) = Venue::new();
    let c = client(venue);
    c.admit_symbol(ETH);
    let first = c.fetch_order_status_reports(0).unwrap();
    c.fetch_order_status_reports(0).unwrap();
    let qty =
        |coid: &str| first.iter().find(|r| r.client_order_id.as_deref() == Some(coid)).unwrap().qty;
    assert!(near(qty("c-btc"), 0.03), "BTC order: 3 contracts x 0.01");
    assert!(near(qty("c-eth"), 0.3), "ETH order: 3 contracts x 0.1");
    assert!(near(qty("s-eth"), 0.2), "ETH stop: 2 contracts x 0.1");
    assert_eq!(first.len(), 4, "the mounted instrument's two reports and the admitted one's two");
    assert_eq!(asked(&calls, "instruments").len(), 1, "a contract size is read once");
    assert_eq!(
        asked(&calls, "orders-pending"),
        [
            json!({"instId": BTC}),
            json!({"instId": ETH}),
            json!({"instId": BTC}),
            json!({"instId": ETH})
        ],
        "two passes, the mounted instrument first each time"
    );
}

/// An admitted instrument's FILLS are read and scaled by its own `ctVal`, and a fill of a fired stop on
/// it is filed under the stop's own client id (the join is read per instrument that has fills).
#[test]
fn an_admitted_instruments_fills_are_scaled_and_its_fired_stop_is_named() {
    let (venue, calls) = Venue::new();
    let c = client(venue);
    c.admit_symbol(ETH);
    let fills = c.fetch_fill_reports(0).unwrap();
    let of = |trade: &str| fills.iter().find(|f| f.trade_id.as_str() == trade).unwrap();
    assert!(near(of("t-btc").last_qty, 0.04), "4 contracts x 0.01");
    assert!(near(of("t-eth").last_qty, 0.4), "4 contracts x 0.1");
    assert_eq!(of("t-eth").client_order_id.as_deref(), Some("s-eth"), "named by the stop's id");
    assert_eq!(of("t-btc").client_order_id.as_deref(), Some("s-btc"));
    let history = asked(&calls, "orders-algo-history");
    assert_eq!(history.len(), 2, "one history read per instrument that has fills");
    assert!(history.iter().any(|p| p["instId"] == ETH));
}

/// Positions with admitted instruments are ONE account-wide `instType=SWAP` read (the endpoint meters
/// 10 per 2 s), split per instrument: the mounted and the admitted rows are scaled by their own
/// `ctVal`, an instrument with no row gets the positive flat row, and a row of an instrument nobody
/// admitted is dropped.
#[test]
fn positions_are_one_account_wide_read_split_per_instrument() {
    let (venue, calls) = Venue::new();
    let c = client(venue);
    c.admit_symbol(ETH);
    c.admit_symbol(XRP);
    let positions = c.fetch_position_status_reports().unwrap();
    assert_eq!(asked(&calls, "positions"), [json!({"instType": "SWAP"})], "one read, no instId");
    let of = |inst: &str| positions.iter().filter(|p| p.symbol == inst).collect::<Vec<_>>();
    assert!(near(of(BTC)[0].qty, 0.02), "long 2 x 0.01");
    assert!(near(of(ETH)[0].qty, -0.3), "short 3 x 0.1");
    assert!(near(of(XRP)[0].qty, 0.0), "no row is the positive flat row, not silence");
    assert!(of(DOGE).is_empty(), "nobody admitted DOGE: its row is not the account's to report");
    assert_eq!(positions.len(), 3);
}

/// The claim is exactly the instruments told, and the mounted instrument, a repeat and a seventeenth are
/// never read.
#[test]
fn the_claim_names_what_is_read_and_a_repeat_costs_nothing() {
    let (venue, calls) = Venue::new();
    let c = client(venue);
    c.admit_symbol(ETH);
    c.admit_symbol("eth-usdt-swap");
    c.admit_symbol(BTC);
    assert_eq!(c.admitted_symbols(), Some(vec![ETH.to_string()]));
    c.fetch_order_status_reports(0).unwrap();
    assert_eq!(asked(&calls, "instruments").len(), 1);
    assert_eq!(asked(&calls, "orders-pending").len(), 2);
}

/// One failed read fails the WHOLE fetch: the planner reads an order a successful report lacks as gone.
#[test]
fn one_failed_read_fails_the_whole_fetch() {
    for (path, inst) in [("orders-pending", ETH), ("orders-algo-pending", ETH)] {
        let (venue, _) = Venue::new();
        let c = client(venue.failing(path, Some(inst)));
        c.admit_symbol(ETH);
        let err = c.fetch_order_status_reports(0).expect_err(path);
        assert!(err.contains("Requests too frequent"), "{path}: {err}");
    }
    let (venue, _) = Venue::new();
    let c = client(venue.failing("fills", Some(ETH)));
    c.admit_symbol(ETH);
    assert!(c.fetch_fill_reports(0).is_err());
    let (venue, _) = Venue::new();
    let c = client(venue.failing("positions", None));
    c.admit_symbol(ETH);
    assert!(c.fetch_position_status_reports().is_err());
    // a contract-size read that fails for any reason but "not listed" fails it too
    let (venue, _) = Venue::new();
    let c = client(venue);
    c.admit_symbol("BUSY-USDT-SWAP");
    let err = c.fetch_order_status_reports(0).expect_err("busy");
    assert!(err.contains("BUSY-USDT-SWAP") && err.contains("System is busy"), "{err}");
}

/// An admitted instrument the venue no longer lists holds nothing: it is read as such (a warning), the
/// account's other reads still go through, the claim still names it, and its position is the positive
/// flat row so a position vike still holds there is seen as drift.
#[test]
fn a_delisted_admitted_instrument_is_read_as_holding_nothing() {
    let (venue, calls) = Venue::new();
    let c = client(venue);
    c.admit_symbol("GONE-USDT-SWAP");
    c.admit_symbol(ETH);
    let orders = c.fetch_order_status_reports(0).unwrap();
    assert_eq!(orders.len(), 4, "BTC and ETH only");
    assert!(orders.iter().all(|r| r.symbol != "GONE-USDT-SWAP"));
    assert!(asked(&calls, "orders-pending").iter().all(|p| p["instId"] != "GONE-USDT-SWAP"));
    assert_eq!(c.admitted_symbols().map(|s| s.len()), Some(2));
    let positions = c.fetch_position_status_reports().unwrap();
    let gone: Vec<_> = positions.iter().filter(|p| p.symbol == "GONE-USDT-SWAP").collect();
    assert_eq!(gone.len(), 1);
    assert!(near(gone[0].qty, 0.0));
}

/// A contract this adapter does not size (an inverse swap, a multiplier) fails the pass loudly rather
/// than reporting a size in the wrong unit.
#[test]
fn an_inverse_or_multiplied_admitted_instrument_fails_the_pass() {
    for (inst, word) in [("USD-INVERSE-SWAP", "inverse"), ("MULT-USDT-SWAP", "ctMult")] {
        let (venue, _) = Venue::new();
        let c = client(venue);
        c.admit_symbol(inst);
        let err = c.fetch_order_status_reports(0).expect_err(inst);
        assert!(err.contains(word), "{inst}: {err}");
    }
}

/// The fired-stop naming policy counts PASSES: with two instruments whose history reads both fail, the
/// first three passes fail the fill read and the fourth goes ahead unnamed. (Counted per instrument, the
/// second pass would already have been the degraded one.)
#[test]
fn the_naming_policy_counts_passes_not_instruments() {
    let (venue, _) = Venue::new();
    let c = client(venue.with_history_down());
    c.admit_symbol(ETH);
    for pass in 1..=3 {
        let err = c.fetch_fill_reports(0).expect_err("strict");
        assert!(err.contains("orders-algo-history"), "pass {pass}: {err}");
    }
    let fills = c.fetch_fill_reports(0).expect("the fourth failing pass goes ahead");
    assert_eq!(fills.len(), 2);
    assert!(
        fills.iter().all(|f| f.client_order_id.as_deref().is_some_and(|c| c.starts_with("venue-")))
    );
}
