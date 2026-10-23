//! The OANDA `ReconClient` reads every symbol the engine admitted on demand (decision 0173), beside the
//! mounted one - over a SCRIPTED LOOPBACK HTTP double, never the venue: the real `OandaReconClient` dials
//! `127.0.0.1` and the double answers by the request line.
//!
//! What this pins: a pass with nothing admitted makes the request it always made; an admitted symbol is
//! read with the same request shape (one order read per instrument), its fills and position come out of
//! the account-wide bodies, two spellings of one instrument are read once, the client claims only what it
//! reads, and ONE failed read fails the whole fetch (the restore planner reads an order a successful
//! report lacks as gone).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_exec::recon::ReconClient;
use vike_oanda::{OandaConfig, OandaReconClient};

/// `(a text the request line contains, status, body)`; the first route that matches answers, and a route
/// that has a LATER twin (the same text) is used up by that answer, so one request line can be scripted
/// to answer differently the second time.
type Route = (&'static str, u16, String);

/// A loopback HTTP double answering `expect` requests (or until ten seconds pass); its join value is
/// every request line it served, in order.
fn serve(mut routes: Vec<Route>, expect: usize) -> (SocketAddr, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let addr = listener.local_addr().expect("resolve assigned port");
    let handle = thread::spawn(move || {
        let mut served = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while served.len() < expect && Instant::now() < deadline {
            let Ok((mut sock, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            sock.set_nonblocking(false).expect("blocking reads");
            sock.set_read_timeout(Some(Duration::from_secs(2))).expect("read timeout");
            let mut buf = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let line = String::from_utf8_lossy(&buf).lines().next().unwrap_or_default().to_string();
            let at = routes.iter().position(|(key, _, _)| line.contains(key));
            let (status, body) = at.map_or((404, "{}".to_string()), |i| {
                let (key, status, body) = routes[i].clone();
                if routes[i + 1..].iter().any(|(later, _, _)| *later == key) {
                    routes.remove(i);
                }
                (status, body)
            });
            served.push(line);
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes());
            let _ = sock.flush();
        }
        served
    });
    (addr, handle)
}

fn config(addr: SocketAddr) -> OandaConfig {
    OandaConfig {
        api_token: "tok".to_string(),
        account_id: "acct-1".to_string(),
        rest_base: format!("http://{addr}"),
        stream_base: format!("http://{addr}"),
    }
}

fn summary() -> Route {
    (
        "/summary",
        200,
        json!({ "account": { "currency": "USD", "lastTransactionID": "100" } }).to_string(),
    )
}

fn order(id: &str, instrument: &str, coid: &str) -> serde_json::Value {
    json!({
        "id": id, "createTime": "1.0", "type": "LIMIT", "instrument": instrument,
        "units": "100", "state": "PENDING", "clientExtensions": { "id": coid }
    })
}

/// With nothing admitted a pass makes exactly the request it always made, and the client claims an EMPTY
/// list (a claim: "told nothing").
#[test]
fn nothing_admitted_reads_the_mounted_instrument_with_the_old_request() {
    let orders = json!({ "orders": [order("11", "EUR_USD", "c-eur")] }).to_string();
    let (addr, served) = serve(vec![("/orders?", 200, orders), summary()], 2);
    let client = OandaReconClient::connect(&config(addr), "EURUSD").expect("connect");
    assert_eq!(client.admitted_symbols(), Some(vec![]));
    let reports = client.fetch_order_status_reports(0).expect("orders");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].symbol, "EURUSD");
    let lines = served.join().expect("double");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[1]
            .contains("GET /v3/accounts/acct-1/orders?instrument=EUR_USD&state=PENDING&count=500"),
        "{}",
        lines[1]
    );
}

/// An admitted symbol's resting orders are read with the SAME request shape, once per instrument; a
/// second spelling of one instrument adds no read and the mounted symbol told again adds none either.
#[test]
fn an_admitted_symbol_has_its_orders_read_once_per_instrument() {
    let eur = json!({ "orders": [order("11", "EUR_USD", "c-eur")] }).to_string();
    let gbp = json!({ "orders": [order("12", "GBP_USD", "c-gbp")] }).to_string();
    let (addr, served) = serve(
        vec![("instrument=EUR_USD", 200, eur), ("instrument=GBP_USD", 200, gbp), summary()],
        3,
    );
    let client = OandaReconClient::connect(&config(addr), "EURUSD").expect("connect");
    for told in ["gbpusd", "GBP_USD", "EURUSD", "eurusd"] {
        client.admit_symbol(told);
    }
    assert_eq!(
        client.admitted_symbols(),
        Some(vec!["GBPUSD".to_string(), "GBP_USD".to_string()]),
        "the list is what it was told (the mounted symbol excluded), upper-cased"
    );
    let reports = client.fetch_order_status_reports(0).expect("orders");
    let seen: Vec<(&str, &str)> =
        reports.iter().map(|r| (r.symbol.as_str(), r.venue_order_id.as_str())).collect();
    assert_eq!(
        seen,
        [("EURUSD", "11"), ("GBPUSD", "12")],
        "mounted first, under the told spelling"
    );
    let lines = served.join().expect("double");
    assert_eq!(lines.len(), 3, "connect plus ONE read per instrument: {lines:?}");
}

/// Positions and fills are account-wide on the wire: one body each, parsed per instrument. An admitted
/// instrument the account never traded is a FLAT row (so a stale local position is still detectable), and
/// a fill of an instrument nobody admitted is not reported.
#[test]
fn positions_and_fills_come_out_of_one_account_wide_body_each() {
    let positions = json!({ "positions": [
        { "instrument": "GBP_USD", "long": { "units": "1000", "averagePrice": "1.25000" },
          "short": { "units": "0" } },
        { "instrument": "USD_JPY", "long": { "units": "5", "averagePrice": "150.000" },
          "short": { "units": "0" } },
    ] })
    .to_string();
    let fills = json!({
        "transactions": [
            { "id": "101", "type": "ORDER_FILL", "instrument": "GBP_USD", "units": "1000",
              "price": "1.25000", "orderID": "9", "time": "2.0", "clientExtensions": { "id": "c-gbp" } },
            { "id": "102", "type": "ORDER_FILL", "instrument": "EUR_USD", "units": "-50",
              "price": "1.10000", "orderID": "10", "time": "3.0" },
            { "id": "103", "type": "ORDER_FILL", "instrument": "USD_JPY", "units": "5",
              "price": "150.000", "orderID": "11", "time": "4.0" },
        ],
        "lastTransactionID": "103"
    })
    .to_string();
    let (addr, served) = serve(
        vec![("/positions", 200, positions), ("/transactions/sinceid", 200, fills), summary()],
        3,
    );
    let client = OandaReconClient::connect(&config(addr), "EURUSD").expect("connect");
    client.admit_symbol("GBPUSD");

    let held = client.fetch_position_status_reports().expect("positions");
    let held: Vec<(&str, f64)> = held.iter().map(|p| (p.symbol.as_str(), p.qty)).collect();
    assert_eq!(
        held,
        [("EURUSD", 0.0), ("GBPUSD", 1000.0)],
        "mounted flat row, then the admitted one"
    );

    let filled = client.fetch_fill_reports(0).expect("fills");
    let filled: Vec<(&str, &str, f64)> =
        filled.iter().map(|f| (f.symbol.as_str(), f.trade_id.as_str(), f.last_qty)).collect();
    assert_eq!(
        filled,
        [("EURUSD", "102", 50.0), ("GBPUSD", "101", 1000.0)],
        "an unadmitted instrument's fill (USD_JPY) is not reported"
    );
    assert_eq!(
        served.join().expect("double").len(),
        3,
        "connect, ONE positions read, ONE fills read"
    );
}

/// An `ORDER_FILL` transaction body for `sinceid` (`last` is the account's `lastTransactionID`).
fn fills_body(rows: &[(&str, &str, &str)], last: &str) -> String {
    let transactions: Vec<_> = rows
        .iter()
        .map(|(id, instrument, units)| {
            json!({ "id": id, "type": "ORDER_FILL", "instrument": instrument, "units": units,
                    "price": "1.25000", "orderID": "9", "time": "2.0" })
        })
        .collect();
    json!({ "transactions": transactions, "lastTransactionID": last }).to_string()
}

fn trade_ids(reports: &[vike_model::FillReport]) -> Vec<(&str, &str)> {
    reports.iter().map(|f| (f.symbol.as_str(), f.trade_id.as_str())).collect()
}

/// **A pass that ran before the admission reached the client does not lose that symbol's fills** (the
/// floor is account-wide: pass 1 advanced it over a GBP_USD fill nobody read). The admitted instrument is
/// read ONCE from the floor the client started with, the others are not re-reported, and the pass after
/// that is the plain one-read pass again.
#[test]
fn a_symbol_admitted_after_the_floor_moved_gets_the_fills_the_floor_passed() {
    let first = fills_body(&[("101", "GBP_USD", "1000"), ("102", "EUR_USD", "-50")], "103");
    let second = fills_body(&[("104", "EUR_USD", "10")], "104");
    let (addr, served) = serve(
        vec![
            ("sinceid?id=100&", 200, first),
            ("sinceid?id=103&", 200, second),
            ("sinceid?id=104&", 200, fills_body(&[], "104")),
            summary(),
        ],
        5,
    );
    let client = OandaReconClient::connect(&config(addr), "EURUSD").expect("connect");

    let one = client.fetch_fill_reports(0).expect("pass 1: the mounted instrument only");
    assert_eq!(trade_ids(&one), [("EURUSD", "102")], "GBP_USD is not read yet");

    client.admit_symbol("GBPUSD");
    let two = client.fetch_fill_reports(0).expect("pass 2");
    assert_eq!(
        trade_ids(&two),
        [("EURUSD", "104"), ("GBPUSD", "101")],
        "the new fill, and the admitted one's older fill; 102 is not reported twice"
    );

    let three = client.fetch_fill_reports(0).expect("pass 3");
    assert!(three.is_empty(), "{three:?}: the backfill happens once");
    let lines = served.join().expect("double");
    assert_eq!(
        lines.len(),
        5,
        "connect, pass 1, pass 2 (plus ONE backfill read), pass 3: {lines:?}"
    );
    assert!(lines[3].contains("sinceid?id=100&type=ORDER_FILL"), "{}", lines[3]);
    assert!(lines[4].contains("sinceid?id=104&"), "{}", lines[4]);
}

/// A failed backfill fails the fetch BEFORE the floor moves: the next pass asks again from the same floor
/// and still reports the admitted symbol's fill (and the mounted one's, which the failed pass did not lose).
#[test]
fn a_failed_backfill_leaves_the_floor_where_it_was() {
    let old = fills_body(&[("101", "GBP_USD", "1000")], "103");
    let new = fills_body(&[("105", "EUR_USD", "10")], "105");
    let (addr, served) = serve(
        vec![
            ("sinceid?id=100&", 200, fills_body(&[], "103")),
            ("sinceid?id=100&", 500, json!({ "errorMessage": "boom" }).to_string()),
            ("sinceid?id=100&", 200, old),
            ("sinceid?id=103&", 200, new),
            summary(),
        ],
        6,
    );
    let client = OandaReconClient::connect(&config(addr), "EURUSD").expect("connect");
    assert!(client.fetch_fill_reports(0).expect("pass 1").is_empty());
    client.admit_symbol("GBPUSD");

    let why = client.fetch_fill_reports(0).expect_err("the backfill read failed");
    assert!(why.contains("500") || why.contains("boom"), "{why}");
    let again = client.fetch_fill_reports(0).expect("pass 3: same floor, backfill answers");
    assert_eq!(trade_ids(&again), [("EURUSD", "105"), ("GBPUSD", "101")]);
    let lines = served.join().expect("double");
    let from_103 = lines.iter().filter(|l| l.contains("sinceid?id=103&")).count();
    assert_eq!(from_103, 2, "the failed pass did not advance the floor: {lines:?}");
}

/// One failed read fails the WHOLE fetch: a report that lacks an admitted symbol's orders because its
/// read failed would let the restore planner retire them as gone.
#[test]
fn one_failed_instrument_read_fails_the_whole_order_fetch() {
    let eur = json!({ "orders": [order("11", "EUR_USD", "c-eur")] }).to_string();
    let (addr, served) = serve(
        vec![
            ("instrument=EUR_USD", 200, eur),
            ("instrument=GBP_USD", 500, json!({ "errorMessage": "boom" }).to_string()),
            summary(),
        ],
        3,
    );
    let client = OandaReconClient::connect(&config(addr), "EURUSD").expect("connect");
    client.admit_symbol("GBPUSD");
    let why = client.fetch_order_status_reports(0).expect_err("the second read failed");
    assert!(why.contains("500") || why.contains("boom"), "{why}");
    assert_eq!(served.join().expect("double").len(), 3);
}
