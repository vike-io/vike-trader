//! **A protective stop that rested for more than seven days is still named when it fires** (decision
//! 0192 item 5), proven offline against a scripted venue instead of by waiting a week for a real one.
//!
//! The venue's rules, MEASURED on the demo host on 2026-10-24 (the answers these fixtures are cut
//! from; every id here is synthetic):
//!   * `GET /fapi/v1/allAlgoOrders` filters on a stop's `updateTime`, never its `createTime`: a window
//!     that holds only a stop's update returns it, a window that holds only its creation returns
//!     nothing. Sent with no window it answers the latest seven days, newest `algoId` first, `limit`
//!     at most 1000. A window longer than seven days is refused: `{"code":-4165,"msg":"Maximum time
//!     interval is 7 days."}`.
//!   * A triggered stop's `updateTime` is its `triggerTime` plus a few milliseconds, so a stop that
//!     fires is inside the default window for the seven days after it FIRED, however long it rested
//!     before.
//!   * `GET /fapi/v1/order?orderId=` of the child order answers `clientOrderId` equal to the stop's
//!     `clientAlgoId` (the REST spelling, not only the user stream's), and `GET
//!     /fapi/v1/algoOrder?clientAlgoId=` answers the stop with `actualOrderId` equal to that order.
//!     Neither read takes a window.
//!   * `GET /fapi/v1/userTrades` names the child order by id and carries no coid.
//!
//! What this file pins: the node never SENDS a window (so the `-4165` above cannot hide a fill); a
//! stop created 8-30 days ago that fired while the node was down is named, by the history while its
//! `updateTime` is inside the seven days and by the by-id reads when it is not; the restore planner
//! then reads it as filled, not gone; and a history the venue refuses leaves no fill silently
//! unnamed. What it cannot pin: how the real venue answers a by-id read of a record that is itself
//! eight days old (the lane twin's `b7binstoph` stop is the optional end-to-end read of that).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_binance::algo::{
    ALGO_CHILD_LOOKUPS_PER_PASS, PATH_ALGO_ORDER, PATH_ALL_ALGO_ORDERS, PATH_OPEN_ALGO_ORDERS,
};
use vike_binance::perp::PATH_ORDER;
use vike_binance::recon_client::{BINANCE_RECON, BinanceReconClient};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_exec::recon::{
    RESTORE_QUIET_PASSES, ReconClient, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore,
};
use vike_model::OrderType;

use crate::tif_gate::NullSigner;

/// The clock of the measuring reads (`now=` of the GET), epoch milliseconds.
const NOW: i64 = 1_792_869_342_192;
const DAY: i64 = 86_400_000;
/// The longest window `allAlgoOrders` accepts, and the span it answers with no window.
const WINDOW: i64 = 7 * DAY;
const SYMBOL: &str = "BTCUSDT";

type Call = (String, Vec<(String, String)>);

fn ago(days: f64) -> i64 {
    NOW - (days * DAY as f64) as i64
}

fn param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// A `STOP_MARKET` that TRIGGERED, in the shapes the demo host answered.
#[derive(Clone)]
struct Stop {
    /// `clientAlgoId` as the venue stores it (`x-<link id>-<coid>` when the account has a broker link).
    stored: String,
    algo_id: u64,
    /// The order the trigger placed: the stop's `actualOrderId` and the child's own order id.
    child: u64,
    created: i64,
    triggered: i64,
}

impl Stop {
    /// Created `created_days` ago, triggered `fired_days` ago.
    fn fired(stored: &str, n: u64, created_days: f64, fired_days: f64) -> Self {
        Stop {
            stored: stored.to_string(),
            algo_id: 1_000_000_000_000 + n,
            child: 2_000_000_000_000 + n,
            created: ago(created_days),
            triggered: ago(fired_days),
        }
    }

    /// Measured: `updateTime` was `triggerTime` plus 17 ms.
    fn updated(&self) -> i64 {
        self.triggered + 17
    }

    /// The `allAlgoOrders` / `algoOrder` row of a finished stop.
    fn row(&self) -> Value {
        json!({
            "algoId": self.algo_id, "clientAlgoId": self.stored, "algoType": "CONDITIONAL",
            "orderType": "STOP_MARKET", "symbol": SYMBOL, "side": "SELL", "positionSide": "BOTH",
            "timeInForce": "GTC", "quantity": "0.0010", "algoStatus": "FINISHED",
            "actualOrderId": self.child.to_string(), "actualPrice": "80639.200000",
            "actualQty": "0.0010", "actualType": "MARKET", "triggerPrice": "80664.60",
            "price": "0.00", "icebergQuantity": null, "tpOrderType": "",
            "selfTradePreventionMode": "EXPIRE_MAKER", "workingType": "CONTRACT_PRICE",
            "priceMatch": "NONE", "closePosition": false, "priceProtect": false,
            "reduceOnly": true, "createTime": self.created, "updateTime": self.updated(),
            "triggerTime": self.triggered, "goodTillDate": 0, "isActivated": false,
        })
    }

    /// `GET /fapi/v1/order` of the child: the stop's `clientAlgoId` is its `clientOrderId`.
    fn child_row(&self) -> Value {
        json!({
            "orderId": self.child, "symbol": SYMBOL, "status": "FILLED",
            "clientOrderId": self.stored, "price": "0.00", "avgPrice": "80639.200000",
            "origQty": "0.0010", "executedQty": "0.0010", "cumQuote": "80.639200",
            "timeInForce": "GTC", "type": "MARKET", "reduceOnly": true, "closePosition": false,
            "side": "SELL", "positionSide": "BOTH", "stopPrice": "0.00",
            "workingType": "CONTRACT_PRICE", "priceProtect": false, "origType": "MARKET",
            "priceMatch": "NONE", "selfTradePreventionMode": "EXPIRE_MAKER", "goodTillDate": 0,
            "time": self.triggered + 1, "updateTime": self.triggered + 1,
        })
    }

    /// `GET /fapi/v1/userTrades`: the child's fill, which names its order by id and no coid.
    fn trade_row(&self) -> Value {
        json!({
            "symbol": SYMBOL, "id": 3_000_000_000_u64 + self.algo_id % 1_000_000, "orderId": self.child,
            "pair": SYMBOL, "side": "SELL", "price": "80639.20", "qty": "0.0010",
            "realizedPnl": "0", "quoteQty": "80.639200", "commission": "0.03225568",
            "commissionAsset": "USDT", "time": self.triggered + 1, "buyer": false, "maker": false,
            "positionSide": "BOTH", "baseQty": "0", "marginAsset": "USDT",
        })
    }
}

/// What the scripted venue was asked and what its history answered, kept outside the client that owns
/// the venue.
#[derive(Clone)]
struct Seen {
    calls: Arc<Mutex<Vec<Call>>>,
    /// The `clientAlgoId`s each `allAlgoOrders` read returned.
    history: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Seen {
    fn params_of(&self, path: &str) -> Vec<Vec<(String, String)>> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|(p, _)| p == path).map(|(_, params)| params.clone()).collect()
    }

    fn answers(&self) -> Vec<Vec<String>> {
        self.history.lock().unwrap().clone()
    }
}

/// The venue: stops that fired, answered by the rules in the module doc.
struct Venue {
    stops: Vec<Stop>,
    /// Refuse every history read with the venue's own `-4165` (the node built a window it must not).
    refuse_history: bool,
    /// Know no order by id (`-2013`): the by-id reads cannot name anything.
    forget_orders: bool,
    seen: Seen,
}

impl Venue {
    fn new(stops: Vec<Stop>) -> (Self, Seen) {
        let seen = Seen { calls: Arc::default(), history: Arc::default() };
        (Venue { stops, refuse_history: false, forget_orders: false, seen: seen.clone() }, seen)
    }

    fn all_algo_orders(&self, params: &[(String, String)]) -> Result<Value, VenueApiError> {
        let refused =
            || VenueApiError { code: -4165, msg: "Maximum time interval is 7 days.".to_string() };
        if self.refuse_history {
            return Err(refused());
        }
        assert_eq!(param(params, "symbol"), Some(SYMBOL), "the symbol is mandatory");
        let num = |key: &str| param(params, key).map(|v| v.parse::<i64>().expect("a number"));
        // Strict side: an unsent bound is filled from the other (and from now), so any request that
        // reaches further back than seven days is refused. Only the default and an explicit window
        // were measured.
        let end = num("endTime").unwrap_or(NOW);
        let start = num("startTime").unwrap_or(end - WINDOW);
        if end - start > WINDOW {
            return Err(refused());
        }
        let mut rows: Vec<&Stop> =
            self.stops.iter().filter(|s| (start..=end).contains(&s.updated())).collect();
        rows.sort_by_key(|s| std::cmp::Reverse(s.algo_id)); // newest first
        rows.truncate(num("limit").unwrap_or(500).min(1000) as usize);
        self.seen.history.lock().unwrap().push(rows.iter().map(|s| s.stored.clone()).collect());
        Ok(Value::Array(rows.iter().map(|s| s.row()).collect()))
    }
}

impl RestTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        assert_eq!(method, "GET", "a reconcile pass only reads");
        let params: Vec<(String, String)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.seen.calls.lock().unwrap().push((path.to_string(), params.clone()));
        let unknown = || VenueApiError { code: -2013, msg: "Order does not exist.".to_string() };
        let p = BINANCE_RECON.paths;
        match path {
            PATH_ALL_ALGO_ORDERS => self.all_algo_orders(&params),
            PATH_ORDER => {
                let id = param(&params, "orderId");
                let known = self.stops.iter().find(|s| id == Some(s.child.to_string().as_str()));
                known.filter(|_| !self.forget_orders).map(Stop::child_row).ok_or_else(unknown)
            }
            PATH_ALGO_ORDER => {
                let cid = param(&params, "clientAlgoId");
                let known = self.stops.iter().find(|s| cid == Some(s.stored.as_str()));
                known.map(Stop::row).ok_or_else(unknown)
            }
            // nothing rests: the stop fired
            PATH_OPEN_ALGO_ORDERS => Ok(json!([])),
            _ if path == p.perp_open_orders => Ok(json!([])),
            _ if path == p.perp_user_trades => {
                let from = param(&params, "startTime")
                    .map(|v| v.parse::<i64>().expect("a number"))
                    .unwrap_or(NOW - WINDOW);
                let rows = self.stops.iter().filter(|s| s.triggered + 1 >= from);
                Ok(Value::Array(rows.map(Stop::trade_row).collect()))
            }
            other => panic!("a stop-naming pass reads no {other}"),
        }
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("a reconcile read is signed, never public ({path})");
    }
}

fn client(venue: Venue, link: Option<&str>) -> BinanceReconClient<NullSigner, Venue> {
    BinanceReconClient::perp(NullSigner, venue, "https://x", SYMBOL)
        .with_link_id(link.map(str::to_string))
}

/// The id the venue stores for a bare coid.
fn stored(link: Option<&str>, bare: &str) -> String {
    link.map_or_else(|| bare.to_string(), |l| format!("x-{l}-{bare}"))
}

/// Every history read asked for the symbol and a limit and nothing else: no `startTime`, no
/// `endTime`, no `algoId`, so the node cannot send the window the venue refuses.
fn assert_no_window(seen: &Seen) {
    let reads = seen.params_of(PATH_ALL_ALGO_ORDERS);
    assert!(!reads.is_empty(), "the pass read the history at all");
    for params in reads {
        let keys: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["symbol", "limit"], "a window parameter was sent: {params:?}");
    }
}

/// One planner pass over what the client reports, for a restored stop `bare` that has been silent for
/// the quiet passes the planner counts.
fn plan_for(client: &dyn ReconClient, bare: &str, since: i64) -> RestorePlan {
    let scope = RestoreScope {
        venue: "binance".to_string(),
        account: None,
        symbols: vec!["BTCUSDT.P".to_string()],
        // the REAL client's claim, as the pass carries it
        lists_trigger_orders: client.lists_trigger_orders(),
    };
    let restored = [RestoredOrderRef {
        coid: bare.to_string(),
        venue: Some("binance".to_string()),
        symbol: Some("BTCUSDT.P".to_string()),
        order_type: Some(OrderType::Stop),
        quiet_passes: RESTORE_QUIET_PASSES,
        ..Default::default()
    }];
    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let fills = client.fetch_fill_reports(since).expect("fetch_fill_reports");
    plan_restore(&scope, &restored, &orders, &fills)
}

/// The model is only worth what it matches: it answers the way the demo host did.
#[test]
fn the_scripted_venue_answers_the_way_the_demo_host_did() {
    let recent = Stop::fired("recentstop", 1, 30.0, 0.02); // created 30 days ago, updated at the fire
    let older = Stop::fired("olderstop", 2, 9.0, 8.0); // updated eight days ago
    let (venue, _seen) = Venue::new(vec![recent.clone(), older.clone()]);
    let ask = |params: &[(&str, String)]| {
        let mut all = vec![("symbol", SYMBOL.to_string())];
        all.extend_from_slice(params);
        venue.signed("https://x", PATH_ALL_ALGO_ORDERS, "GET", &all, &NullSigner)
    };
    let coids = |body: Value| -> Vec<String> {
        body.as_array()
            .expect("an array")
            .iter()
            .map(|r| r["clientAlgoId"].as_str().expect("a coid").to_string())
            .collect()
    };

    // no window: the latest seven days BY UPDATE TIME, so the old stop that fired is in, the one
    // updated eight days ago is out
    assert_eq!(coids(ask(&[]).unwrap()), ["recentstop"]);
    // a window over seven days is refused, with the words the venue used
    let refused =
        ask(&[("startTime", ago(8.0).to_string()), ("endTime", NOW.to_string())]).unwrap_err();
    assert_eq!((refused.code, refused.msg.as_str()), (-4165, "Maximum time interval is 7 days."));
    // a window that holds only the UPDATE returns the stop; one that holds only its CREATION does not
    let window = |from: i64, to: i64| {
        coids(ask(&[("startTime", from.to_string()), ("endTime", to.to_string())]).unwrap())
    };
    assert_eq!(window(older.updated() - 1_000, older.updated() + 1_000), ["olderstop"]);
    assert!(window(older.created - 1_000, older.created + 1_000).is_empty());
}

/// **8-30 days old, fired inside the last hour: named.** The stop's `updateTime` is the fire, so the
/// default window holds it however old its `createTime` is, and the history names the fill. Both spellings
/// of the stored id (bare, and under a broker link) come out as the bare coid.
#[test]
fn a_stop_that_rested_for_a_week_or_more_and_fired_while_the_node_was_down_is_named() {
    for link in [None, Some("LINK")] {
        for created_days in [8.0, 9.5, 15.0, 30.0] {
            let why = format!("link {link:?}, created {created_days} days ago");
            let stop = Stop::fired(&stored(link, "oldstop1"), 1, created_days, 0.02);
            let child = stop.child;
            let (venue, seen) = Venue::new(vec![stop]);
            let fills = client(venue, link).fetch_fill_reports(ago(1.0 / 24.0)).unwrap();
            assert_eq!(fills.len(), 1, "{why}");
            assert_eq!(fills[0].client_order_id.as_deref(), Some("oldstop1"), "{why}");
            assert_eq!(&*fills[0].venue_order_id, child.to_string().as_str(), "{why}");
            assert_no_window(&seen);
            assert_eq!(
                seen.answers(),
                [[stored(link, "oldstop1")]],
                "{why}: the venue's window holds it by updateTime"
            );
        }
    }
}

/// **Its own record older than the history's window: named by id, never by a longer window.** A stop
/// that fired 7.5-20 days ago has an `updateTime` outside the seven days the unwindowed history
/// answers, so the history cannot name it (the venue's answer is empty) and the by-id reads must: the
/// child order for its stored id, then the stop for that id, neither with a window. The node reaches
/// the fill with a lookback that long (the operator's `reconcile_lookback_ms` allows about 49 days).
/// The restore planner then reads the stop as filled, and a stop whose child the venue cannot show is
/// the control that makes it gone, so the assertion can fail.
#[test]
fn a_stop_whose_own_record_is_older_than_the_history_window_is_named_by_id_alone() {
    for (created_days, fired_days) in [(8.0, 7.5), (9.0, 8.0), (15.0, 10.0), (30.0, 20.0)] {
        let why = format!("created {created_days} days ago, fired {fired_days} days ago");
        let since = ago(31.0);

        let stop = Stop::fired("oldstop2", 2, created_days, fired_days);
        let (child, algo_stored) = (stop.child, stop.stored.clone());
        let (venue, seen) = Venue::new(vec![stop.clone()]);
        let client_ok = client(venue, None);
        let fills = client_ok.fetch_fill_reports(since).unwrap();
        assert_eq!(fills.len(), 1, "{why}");
        assert_eq!(fills[0].client_order_id.as_deref(), Some("oldstop2"), "{why}");
        assert_no_window(&seen);
        assert_eq!(seen.answers(), [Vec::<String>::new()], "{why}: the history holds nothing");
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            seen.params_of(PATH_ORDER),
            [vec![pair("symbol", SYMBOL), pair("orderId", &child.to_string())]],
            "{why}: the child order, by id"
        );
        assert_eq!(
            seen.params_of(PATH_ALGO_ORDER),
            [vec![pair("clientAlgoId", &algo_stored)]],
            "{why}: the stop that child names, by its stored id"
        );

        let plan = plan_for(&client_ok, "oldstop2", since);
        assert_eq!(plan.filled_while_down, ["oldstop2"], "{why}: {plan:?}");
        assert!(plan.gone.is_empty() && plan.unproven.is_empty() && plan.adopt.is_empty(), "{why}");

        // control: a venue that shows no child order leaves the fill unnamed, and the same planner
        // pass then declares the silent stop gone
        let (mut venue, _seen) = Venue::new(vec![stop]);
        venue.forget_orders = true;
        let plan = plan_for(&client(venue, None), "oldstop2", since);
        assert!(plan.filled_while_down.is_empty(), "{why}: {plan:?}");
        assert_eq!(plan.gone.iter().map(|r| r.coid.as_str()).collect::<Vec<_>>(), ["oldstop2"]);
    }
}

/// **A history the venue refuses never leaves a fill silently unnamed.** Were the node ever to send a
/// window over seven days, every history read would be answered `-4165`. The by-id reads still name
/// the fills (twenty-five fill orders a pass); a pass that is left with fill orders the history would
/// have had to name FAILS, so the restore planner reads nothing as gone from it, and the next pass
/// names the rest. Once every fill order is resolved by id the refused history is forgiven.
#[test]
fn a_history_refused_with_4165_is_covered_by_id_and_a_pass_that_cannot_be_is_loud() {
    let count = ALGO_CHILD_LOOKUPS_PER_PASS as u64 + 5;
    let stops =
        (1..=count).map(|n| Stop::fired(&format!("refused{n}"), n, 12.0, 0.01)).collect::<Vec<_>>();
    let (mut venue, _seen) = Venue::new(stops);
    venue.refuse_history = true;
    let client = client(venue, None);
    let since = ago(1.0);

    // pass 1: the budget names 25 of 30 and five are left that only the (refused) history could name
    let failed = client.fetch_fill_reports(since).unwrap_err();
    assert!(failed.contains("-4165"), "the venue's refusal is the failure: {failed}");
    // pass 2: the five left are asked, all 30 are named, the refusal is forgiven
    let fills = client.fetch_fill_reports(since).unwrap();
    assert_eq!(fills.len(), count as usize);
    assert!(fills.iter().all(|f| f.client_order_id.is_some()), "every fill named: {fills:?}");
}
