//! **A fill of a PLAIN order carries the order's client id** (spot and perp), offline against a
//! scripted venue: no network, nothing `#[ignore]`d.
//!
//! The case it comes from, MEASURED 2026-10-24 on the Binance demo perpetual: four operator LIMIT
//! orders (`b7binla6`..`b7binld6`) filled while the node was down, and the reconcile pass held four
//! `MissingFill` rows with `coid=None` plus a `PositionDrift`. `userTrades` (and spot `myTrades`) name
//! the order by id and carry no client order id, so the fill could not be joined to our order and the
//! operator could tell the rows apart only by price (Bybit's twin case came back WITH the coid).
//!
//! The reconcile client now reads each coid-less fill's order by id (`GET /fapi/v1/order?orderId=` on
//! the perpetual, `GET /api/v3/order?orderId=` on spot: both answer the `clientOrderId` the venue
//! stored) and names the fill with it, when the stored id is OURS (`x-<link id>-<coid>` or a bare
//! coid). The pins, each guarding a different way this can go wrong:
//!   * the measured case: four plain fills come back with their bare coids, for both stored forms;
//!   * a fill that already has a coid (named from the Algo Service history) is never changed or asked
//!     about;
//!   * an order the venue no longer finds leaves its fill coid-less, once, and the rest are named;
//!   * the per-pass budget (`ALGO_CHILD_LOOKUPS_PER_PASS`): the pass still succeeds and the rest wait;
//!   * every answer is remembered: a second pass makes no lookup;
//!   * an id that is not ours (the web UI's, another broker's, a link id this node does not know) names
//!     nothing;
//!   * a failing lookup is strict for three passes, then unnamed with ONE warning an hour;
//!   * an admitted symbol's fill is looked up under the symbol it traded.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_binance::algo::{ALGO_CHILD_LOOKUPS_PER_PASS, PATH_ALGO_ORDER, PATH_ALL_ALGO_ORDERS};
use vike_binance::recon_client::{BINANCE_RECON, BinanceReconClient};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_exec::recon::ReconClient;
use vike_model::FillReport;

use crate::tif_gate::NullSigner;

const SYMBOL: &str = "BTCUSDT";
const LINK: &str = "LINK";
const PERP_ORDER: &str = vike_binance::perp::PATH_ORDER;
const SPOT_ORDER: &str = vike_binance::spot::PATH_ORDER;

type Call = (String, Vec<(String, String)>);
type Calls = Arc<Mutex<Vec<Call>>>;

/// The venue: fills that name their order by id and no coid, and orders that know their stored id.
struct Venue {
    spot: bool,
    /// `(symbol, order id, clientOrderId as the venue stores it)`; an order not listed answers `-2013`.
    orders: Vec<(&'static str, u64, String)>,
    /// One coid-less fill per entry: `(symbol, the order id it traded)`.
    fills: Vec<(&'static str, u64)>,
    /// Triggered stops the Algo Service history names: `(the order the trigger placed, bare coid)`.
    stops: Vec<(u64, &'static str)>,
    /// Make `GET order` fail with a code that is not "unknown order".
    order_read_down: Arc<AtomicBool>,
    /// Fills that appear only once `later_on` is set (a trade that happens between two passes).
    later_fills: Vec<(&'static str, u64)>,
    later_on: Arc<AtomicBool>,
    calls: Calls,
}

impl Venue {
    fn new(spot: bool) -> (Self, Calls) {
        let calls: Calls = Arc::default();
        let venue = Venue {
            spot,
            orders: Vec::new(),
            fills: Vec::new(),
            stops: Vec::new(),
            order_read_down: Arc::default(),
            later_fills: Vec::new(),
            later_on: Arc::default(),
            calls: Arc::clone(&calls),
        };
        (venue, calls)
    }

    /// `(symbol, order id, stored id)` for each of `coids`, one fill per order (ids from `first`).
    fn plain(mut self, first: u64, link: Option<&str>, coids: &[&str]) -> Self {
        for (n, coid) in coids.iter().enumerate() {
            let id = first + n as u64;
            self.orders.push((SYMBOL, id, stored(link, coid)));
            self.fills.push((SYMBOL, id));
        }
        self
    }
}

/// The id the venue stores for a bare coid placed under `link`.
fn stored(link: Option<&str>, bare: &str) -> String {
    link.map_or_else(|| bare.to_string(), |l| format!("x-{l}-{bare}"))
}

/// A `userTrades` / `myTrades` row: the order it traded by id, no client order id.
fn fill_row(symbol: &str, order: u64) -> Value {
    json!({
        "symbol": symbol, "id": 5_000 + order, "orderId": order, "side": "SELL", "price": "80000",
        "qty": "0.001", "commission": "0.01", "commissionAsset": "USDT", "maker": true,
        "isBuyer": false, "isMaker": true, "time": 1,
    })
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
        self.calls.lock().unwrap().push((path.to_string(), params.clone()));
        let get = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        let unknown = || VenueApiError { code: -2013, msg: "Order does not exist.".to_string() };
        let paths = BINANCE_RECON.paths;
        let (trades, order) = if self.spot {
            (paths.spot_my_trades, SPOT_ORDER)
        } else {
            (paths.perp_user_trades, PERP_ORDER)
        };
        if path == trades {
            let symbol = get("symbol").unwrap_or_default();
            let later: &[(&str, u64)] =
                if self.later_on.load(Ordering::Relaxed) { &self.later_fills } else { &[] };
            let rows = self.fills.iter().chain(later);
            let rows = rows.filter(|(s, _)| *s == symbol).map(|(s, o)| fill_row(s, *o));
            return Ok(Value::Array(rows.collect()));
        }
        if path == order {
            if self.order_read_down.load(Ordering::Relaxed) {
                return Err(VenueApiError { code: -1000, msg: "order lookup refused".to_string() });
            }
            let (symbol, id) = (get("symbol"), get("orderId"));
            return self
                .orders
                .iter()
                .find(|(s, o, _)| Some(*s) == symbol && Some(o.to_string().as_str()) == id)
                .map(|(s, o, c)| json!({ "symbol": s, "orderId": o, "clientOrderId": c }))
                .ok_or_else(unknown);
        }
        // the Algo Service exists on the perpetual only: spot reads none of this
        if !self.spot && path == PATH_ALL_ALGO_ORDERS {
            let rows = self.stops.iter().map(|(child, coid)| {
                json!({ "algoId": 55, "clientAlgoId": coid, "symbol": SYMBOL,
                        "actualOrderId": child.to_string(), "algoStatus": "FINISHED" })
            });
            return Ok(Value::Array(rows.collect()));
        }
        // no stop rests under these ids: a plain order has no algo record
        if !self.spot && path == PATH_ALGO_ORDER {
            return Err(unknown());
        }
        panic!(
            "a fill-naming pass on the {} lane reads no {path}",
            if self.spot { "spot" } else { "perp" }
        );
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
    let link = link.map(str::to_string);
    if venue.spot {
        BinanceReconClient::spot(NullSigner, venue, "https://x", SYMBOL).with_link_id(link)
    } else {
        BinanceReconClient::perp(NullSigner, venue, "https://x", SYMBOL).with_link_id(link)
    }
}

/// `(order id, coid)` of each fill, in the order the venue listed them.
fn named(fills: &[FillReport]) -> Vec<(String, Option<String>)> {
    fills.iter().map(|f| (f.venue_order_id.to_string(), f.client_order_id.clone())).collect()
}

fn some(order: u64, coid: &str) -> (String, Option<String>) {
    (order.to_string(), Some(coid.to_string()))
}

fn none(order: u64) -> (String, Option<String>) {
    (order.to_string(), None)
}

/// How many requests went to `path`.
fn reads(calls: &Calls, path: &str) -> usize {
    calls.lock().unwrap().iter().filter(|(p, _)| p == path).count()
}

fn order_path(spot: bool) -> &'static str {
    if spot { SPOT_ORDER } else { PERP_ORDER }
}

fn lane(spot: bool) -> &'static str {
    if spot { "spot" } else { "perp" }
}

/// **The measured case.** Four operator LIMIT orders filled while the node was down: each fill comes
/// back with its order's bare coid, whether the venue stored it bare or under the broker link, and the
/// venue order id is untouched. The order is read by symbol and id, nothing else.
#[test]
fn four_plain_orders_filled_while_the_node_was_down_come_back_with_their_coids() {
    let coids = ["b7binla6", "b7binlb6", "b7binlc6", "b7binld6"];
    for spot in [false, true] {
        for link in [None, Some(LINK)] {
            let why = format!("{} lane, link {link:?}", lane(spot));
            let (venue, calls) = Venue::new(spot);
            let fills =
                client(venue.plain(1_001, link, &coids), link).fetch_fill_reports(0).unwrap();
            assert_eq!(
                named(&fills),
                [
                    some(1_001, "b7binla6"),
                    some(1_002, "b7binlb6"),
                    some(1_003, "b7binlc6"),
                    some(1_004, "b7binld6")
                ],
                "{why}"
            );
            let asked: Vec<_> = calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p == order_path(spot))
                .map(|(_, params)| params.clone())
                .collect();
            let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
            let expected: Vec<_> = (1_001..=1_004)
                .map(|id| vec![pair("symbol", SYMBOL), pair("orderId", &id.to_string())])
                .collect();
            assert_eq!(asked, expected, "{why}: each order, once, by symbol and id");
        }
    }
}

/// **A fill that already has a coid is never changed, nor asked about.** Order 9 is a triggered stop's
/// child, which the Algo Service history names; the venue would answer a plain id for it, and the
/// history's answer must stand without a lookup. Order 10 is a plain order and is named by id.
#[test]
fn a_fill_that_already_has_a_coid_is_never_changed_or_asked_about() {
    let (mut venue, calls) = Venue::new(false);
    venue.stops = vec![(9, "stopcoid")];
    venue.orders = vec![
        (SYMBOL, 9, stored(Some(LINK), "plain9")),
        (SYMBOL, 10, stored(Some(LINK), "plain10")),
    ];
    venue.fills = vec![(SYMBOL, 9), (SYMBOL, 10)];
    let fills = client(venue, Some(LINK)).fetch_fill_reports(0).unwrap();
    assert_eq!(named(&fills), [some(9, "stopcoid"), some(10, "plain10")]);
    let orders: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p == PERP_ORDER)
        .flat_map(|(_, params)| {
            params.iter().filter(|(k, _)| k == "orderId").map(|(_, v)| v.clone())
        })
        .collect();
    assert_eq!(orders, ["10"], "only the fill still without a coid is looked up");
}

/// **An order the venue no longer finds stays unnamed**, once: the other fills of the pass are still
/// named, the pass succeeds, and the "no" is remembered (a second pass asks nothing).
#[test]
fn an_order_the_venue_no_longer_finds_leaves_its_fill_coid_less_and_is_asked_once() {
    for spot in [false, true] {
        let why = lane(spot);
        let (mut venue, calls) = Venue::new(spot);
        venue = venue.plain(2_001, None, &["found1"]);
        venue.fills.push((SYMBOL, 2_002)); // a fill of an order the venue answers -2013 for
        let client = client(venue, None);

        let fills = client.fetch_fill_reports(0).unwrap();
        assert_eq!(named(&fills), [some(2_001, "found1"), none(2_002)], "{why}");
        let asked = reads(&calls, order_path(spot));
        assert_eq!(asked, 2, "{why}: both orders asked");
        let again = client.fetch_fill_reports(0).unwrap();
        assert_eq!(named(&again), [some(2_001, "found1"), none(2_002)], "{why}: from memory");
        assert_eq!(reads(&calls, order_path(spot)), asked, "{why}: the 'no' is remembered too");
    }
}

/// **The per-pass budget.** A pass that meets more fill orders than `ALGO_CHILD_LOOKUPS_PER_PASS` names
/// that many, leaves the 26th unnamed and SUCCEEDS; the next pass asks only the rest.
#[test]
fn fill_orders_beyond_the_per_pass_budget_stay_unnamed_this_pass_and_the_pass_succeeds() {
    let total = ALGO_CHILD_LOOKUPS_PER_PASS + 5;
    let coids: Vec<String> = (0..total).map(|n| format!("plain{n:03}")).collect();
    let coids: Vec<&str> = coids.iter().map(String::as_str).collect();
    for spot in [false, true] {
        let why = lane(spot);
        let (venue, calls) = Venue::new(spot);
        let client = client(venue.plain(3_000, None, &coids), None);

        let first = client.fetch_fill_reports(0).expect("a capped pass is not a failed one");
        let flags: Vec<bool> = first.iter().map(|f| f.client_order_id.is_some()).collect();
        assert_eq!(
            flags.iter().filter(|named| **named).count(),
            ALGO_CHILD_LOOKUPS_PER_PASS,
            "{why}"
        );
        assert!(flags[ALGO_CHILD_LOOKUPS_PER_PASS - 1], "{why}: the 25th is named");
        assert!(!flags[ALGO_CHILD_LOOKUPS_PER_PASS], "{why}: the 26th is not");
        assert_eq!(
            reads(&calls, order_path(spot)),
            ALGO_CHILD_LOOKUPS_PER_PASS,
            "{why}: the budget"
        );

        let second = client.fetch_fill_reports(0).unwrap();
        assert!(second.iter().all(|f| f.client_order_id.is_some()), "{why}: the rest, next pass");
        assert_eq!(
            reads(&calls, order_path(spot)),
            total,
            "{why}: the 25 before are not asked again"
        );
    }
}

/// **Never ask twice.** The first pass pays for each fill order (the order read, and on the perpetual
/// the Algo Service read that says it is no stop's child); the second pass makes no lookup at all.
#[test]
fn the_answers_are_remembered_so_the_second_pass_makes_no_lookup() {
    for spot in [false, true] {
        let why = lane(spot);
        let (venue, calls) = Venue::new(spot);
        let client = client(venue.plain(4_001, Some(LINK), &["rem1", "rem2", "rem3"]), Some(LINK));
        let lookups =
            |calls: &Calls| reads(calls, order_path(spot)) + reads(calls, PATH_ALGO_ORDER);

        let first = client.fetch_fill_reports(0).unwrap();
        let per_order = if spot { 1 } else { 2 };
        assert_eq!(lookups(&calls), 3 * per_order, "{why}: three orders, once each");
        let second = client.fetch_fill_reports(0).unwrap();
        assert_eq!(lookups(&calls), 3 * per_order, "{why}: nothing asked on the second pass");
        assert_eq!(named(&second), named(&first), "{why}: and the same names");
        assert!(second.iter().all(|f| f.client_order_id.is_some()), "{why}");
    }
}

/// **Only an id of OURS names a fill** (the stored forms): the bare coid and `x-<our link>-<coid>`
/// do; the web UI's `web_…`, another broker's `x-OTHER-…` and a link id the node was not told do not,
/// because a coid this node never minted is worse than an unnamed fill.
#[test]
fn only_an_id_of_ours_names_a_fill() {
    for spot in [false, true] {
        let why = lane(spot);
        let stored_ids = [
            "x-LINK-ours1",
            "bareours2",
            "x-OTHER-theirs3",
            "web_abc123",
            "android_9Zx",
            "x-LINK-", // our prefix and no coid
        ];
        let (mut venue, _calls) = Venue::new(spot);
        for (n, id) in stored_ids.iter().enumerate() {
            venue.orders.push((SYMBOL, 5_001 + n as u64, id.to_string()));
            venue.fills.push((SYMBOL, 5_001 + n as u64));
        }
        let (unlinked_venue, _) = Venue::new(spot);
        let linked = client(venue, Some(LINK)).fetch_fill_reports(0).unwrap();
        assert_eq!(
            named(&linked),
            [
                some(5_001, "ours1"),
                some(5_002, "bareours2"),
                none(5_003),
                none(5_004),
                none(5_005),
                none(5_006)
            ],
            "{why}: with the account's link id"
        );

        // without the account's link id no `x-` id is ours; a bare coid still is
        let mut venue = unlinked_venue;
        venue.orders = vec![
            (SYMBOL, 6_001, "x-LINK-ours1".to_string()),
            (SYMBOL, 6_002, "bareours2".to_string()),
        ];
        venue.fills = vec![(SYMBOL, 6_001), (SYMBOL, 6_002)];
        let unlinked = client(venue, None).fetch_fill_reports(0).unwrap();
        assert_eq!(
            named(&unlinked),
            [none(6_001), some(6_002, "bareours2")],
            "{why}: without a link id"
        );
    }
}

/// **A failing lookup is strict, then unnamed with one warning an hour.** The first three failing
/// passes fail the fill fetch (a fill list silently lacking names would let the restore planner read a
/// filled order as gone); from the fourth on the pass goes ahead with the fills unnamed and ONE plain
/// warning an hour; one good pass makes it strict again.
#[test]
fn a_failing_order_read_is_strict_for_three_passes_then_unnamed_with_one_warning_an_hour() {
    for spot in [false, true] {
        let why = lane(spot);
        let (mut venue, _calls) = Venue::new(spot);
        let (down, later) = (Arc::clone(&venue.order_read_down), Arc::clone(&venue.later_on));
        // a second order that trades only after the good pass (the first one's answer is remembered)
        venue.later_fills = vec![(SYMBOL, 7_002)];
        venue.orders.push((SYMBOL, 7_002, "down2".to_string()));
        let client = client(venue.plain(7_001, None, &["down1"]), None);

        down.store(true, Ordering::Relaxed);
        for pass in 1..=3 {
            assert!(client.fetch_fill_reports(0).is_err(), "{why}: pass {pass}: strict");
        }
        let (fills, events) = vike_log::capture::captured(|| {
            let mut last = None;
            for _ in 0..3 {
                last = Some(client.fetch_fill_reports(0).expect("the fourth failure goes ahead"));
            }
            last.unwrap()
        });
        assert_eq!(named(&fills), [none(7_001)], "{why}: unnamed while the read is down");
        let warned = events
            .iter()
            .filter(|e| {
                e.level == tracing::Level::WARN
                    && e.message.contains("could not be named after repeated passes")
            })
            .count();
        assert_eq!(warned, 1, "{why}: three passes ahead inside the hour, one warning: {events:?}");

        down.store(false, Ordering::Relaxed);
        let fills = client.fetch_fill_reports(0).unwrap();
        assert_eq!(named(&fills), [some(7_001, "down1")], "{why}: named again");
        later.store(true, Ordering::Relaxed);
        down.store(true, Ordering::Relaxed);
        assert!(client.fetch_fill_reports(0).is_err(), "{why}: a good pass made it strict again");
    }
}

/// **An admitted symbol's plain fill is looked up under the symbol it traded** (not the mounted one),
/// and still carries the symbol-qualified trade id the stream spells.
#[test]
fn an_admitted_symbols_plain_fill_is_named_under_its_own_symbol() {
    for spot in [false, true] {
        let why = lane(spot);
        let (mut venue, calls) = Venue::new(spot);
        venue = venue.plain(8_001, None, &["mounted1"]);
        venue.orders.push(("ETHUSDT", 8_001, "admitted1".to_string())); // the same id on ANOTHER symbol
        venue.fills.push(("ETHUSDT", 8_001));
        let client = client(venue, None);
        client.admit_symbol(if spot { "ETHUSDT" } else { "ETHUSDT.P" });

        let fills = client.fetch_fill_reports(0).unwrap();
        let by_symbol: Vec<_> =
            fills.iter().map(|f| (f.symbol.clone(), f.client_order_id.clone())).collect();
        let (mounted, admitted) =
            if spot { ("BTCUSDT", "ETHUSDT") } else { ("BTCUSDT.P", "ETHUSDT.P") };
        assert_eq!(
            by_symbol,
            [
                (mounted.to_string(), Some("mounted1".to_string())),
                (admitted.to_string(), Some("admitted1".to_string()))
            ],
            "{why}"
        );
        let symbols: Vec<_> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == order_path(spot))
            .map(|(_, params)| params.iter().find(|(k, _)| k == "symbol").unwrap().1.clone())
            .collect();
        assert_eq!(symbols, ["BTCUSDT", "ETHUSDT"], "{why}: each order read under its own symbol");
    }
}
