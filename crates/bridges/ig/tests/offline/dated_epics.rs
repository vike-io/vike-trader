//! A DATED contract's own `expiry` rides every order body: IG deals and closes `IX.D.DAX.FBMU1.IP` (Germany 40 (E1),
//! `DEC-26`) with that word, where every body this bridge ever sent said `-`. MEASURED on the demo gateway on
//! 2026-10-30 (a market BUY and its close, and a far LIMIT, on four dated epics: the deal, the working order and the close
//! all carried the epic's `expiry`); what this suite proves is the bridge's side of it, through the real exec client over
//! `fake_ig` (a hand-written loopback gateway, never IG): the open writes the table's expiry, a close of an epic this
//! process opened writes the same one without reading the table again, a close of an epic it never opened reads the
//! table ONCE, and a table that cannot be read never refuses a close (it goes out with `-`, as every close did).

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_exec::{ExecutionClient, event_channel};
use vike_ig::IgExecutionClient;
use vike_model::{OrderRequest, OrderType};

use crate::fake_ig::{self, FakeIg, Route};

const DAX_DEC: &str = "IX.D.DAX.FBMU1.IP";
const GBPUSD: &str = "CS.D.GBPUSD.MINI.IP";

/// `GET /markets/{epic}` of a dated index quoted in euros (the captured `IX.D.DAX.FBMU1.IP` states exactly these).
fn dated_table() -> Route {
    Route::ok(
        "GET /markets/IX.D.DAX.FBMU1",
        json!({ "instrument": { "epic": DAX_DEC, "type": "INDICES", "expiry": "DEC-26",
                                "currencies": [{ "code": "EUR", "isDefault": false }] } }),
    )
}

/// The gateway: a login, the deal endpoint answering one reference, and a confirm for it.
fn gateway(tables: Vec<Route>) -> FakeIg {
    let mut routes = vec![
        fake_ig::login_route(),
        Route::ok("POST /positions/otc", json!({ "dealReference": "DREF1" })),
        Route::ok(
            "GET /confirms/",
            json!({ "dealStatus": "ACCEPTED", "dealReference": "DREF1", "dealId": "DIAAA1",
                    "epic": DAX_DEC, "direction": "BUY", "size": 0.5, "level": 25820.1 }),
        ),
    ];
    routes.extend(tables);
    fake_ig::serve(routes)
}

fn order(coid: &str, epic: &str, side: i32, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ig".into(),
        symbol: epic.into(),
        side,
        qty: 0.5,
        order_type: OrderType::Market,
        reduce_only,
        ts: 42,
        ..Default::default()
    }
}

/// Send `orders` one after the other through the real exec client, each waiting until the gateway has been POSTed
/// `n` times (the body is what these tests read, not the events).
fn send(ig: &FakeIg, orders: &[OrderRequest]) {
    let (events, _rx) = event_channel(64);
    let mut client = IgExecutionClient::spawn(ig.config(), events);
    for (n, req) in orders.iter().enumerate() {
        client.submit(req);
        let deadline = Instant::now() + Duration::from_secs(10);
        while ig.count("POST /positions/otc") < n + 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            ig.count("POST /positions/otc") > n,
            "order {n} never reached the deal endpoint: {:?}",
            ig.served()
        );
    }
    client.detach();
}

fn expiries(ig: &FakeIg) -> Vec<Value> {
    ig.bodies_of("POST /positions/otc").iter().map(|b| b["expiry"].clone()).collect()
}

/// An open of a dated epic writes the table's `expiry`, and the close of the epic this process opened writes the same
/// word with no second read of the table. RED on the base: both said `-`.
#[test]
fn a_dated_epic_is_opened_and_closed_with_its_own_expiry_off_one_table_read() {
    let ig = gateway(vec![dated_table()]);
    send(&ig, &[order("c-open", DAX_DEC, 1, false), order("c-close", DAX_DEC, -1, true)]);
    assert_eq!(
        expiries(&ig),
        [json!("DEC-26"), json!("DEC-26")],
        "{:?}",
        ig.bodies_of("POST /positions/otc")
    );
    let open = &ig.bodies_of("POST /positions/otc")[0];
    assert_eq!(open["currencyCode"], "EUR");
    assert_eq!(open["forceOpen"], true);
    assert_eq!(
        ig.count("GET /markets/IX.D.DAX.FBMU1"),
        1,
        "one table read for the open AND the close"
    );
}

/// A close of an epic this process never opened (a restart, a deal made elsewhere) reads the table once for the
/// expiry and keeps it. RED on the base: the close said `-` and IG answers a dated epic closed with `-` an error.
#[test]
fn a_close_of_a_dated_epic_this_process_never_opened_reads_the_table_once() {
    let ig = gateway(vec![dated_table()]);
    send(&ig, &[order("c-close-1", DAX_DEC, -1, true), order("c-close-2", DAX_DEC, -1, true)]);
    assert_eq!(expiries(&ig), [json!("DEC-26"), json!("DEC-26")]);
    assert_eq!(ig.count("GET /markets/IX.D.DAX.FBMU1"), 1, "read once, kept");
}

/// An undated epic keeps the dash on both, and a close is NEVER refused for a table that cannot be read (here every
/// market read is a 404): it goes out with `-`, as every close did before dated epics were wired.
#[test]
fn an_undated_epic_and_an_unreadable_table_keep_the_dash_and_never_refuse_a_close() {
    let ig = gateway(vec![Route::ok(
        "GET /markets/CS.D.GBPUSD",
        json!({ "instrument": { "epic": GBPUSD, "type": "CURRENCIES", "expiry": "-",
                                "currencies": [{ "code": "USD", "isDefault": true }] } }),
    )]);
    send(&ig, &[order("c-open", GBPUSD, 1, false), order("c-close", GBPUSD, -1, true)]);
    assert_eq!(expiries(&ig), [json!("-"), json!("-")]);

    let unreadable = gateway(vec![]);
    send(&unreadable, &[order("c-close", DAX_DEC, -1, true)]);
    assert_eq!(expiries(&unreadable), [json!("-")], "the close still goes");
}
