//! **A trade's stop-loss and take-profit, as the node hears them.** The venue holds both orders; what reaches the node is
//! the shim's envelope stream (`tests/fixtures/exit_events.json`, transcribed in the venue's own order from the demo probes
//! of 2026-10-26), folded here through the REAL pure layer (`vike_fxcm::event_mapper`) and the core's own account
//! (`vike_exec::Account`). It holds the three things the design rests on and no gate held before:
//!
//! * the exit that FIRES is a fill under ITS client order id, at the exit's rate, closing the position under a trade id
//!   the core has not folded (so it is not dropped as a replay of the opening fill);
//! * the sibling the VENUE deletes (the OCO partner, or either exit when the trade closes any other way) is cancelled
//!   EXACTLY ONCE under its own client order id: it never stays ACCEPTED in the book, and nothing else is said about it;
//! * an exit the node itself cancelled leaves its routing row at once (`exec.rs`'s `Cancel` arm), so the venue's own later
//!   `canceled` envelope for it publishes nothing: one cancel, not two.
//!
//! What it cannot prove is what FXCM sends: that is `docs/ops/fxcm-forexconnect.md`'s measurements and the live smoke.

use std::collections::HashMap;

use serde_json::Value;
use vike_exec::{Account, BalanceMode, FillFold};
use vike_fxcm::event_mapper::{closes_routing, ends_cancelability, map_drained_event};
use vike_model::events::Event;

fn fixture() -> Value {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exit_events.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the fixture {}: {e}", path.display()));
    serde_json::from_str(&text).expect("the fixture is JSON")
}

/// What one scenario came to: the events each client order id received, in order, and the account after every fill.
struct Run {
    by_coid: HashMap<String, Vec<Event>>,
    folds: Vec<FillFold>,
    account: Account,
}

fn run(name: &str) -> Run {
    let fixture = fixture();
    let scenario = &fixture["scenarios"][name];
    assert!(scenario.is_object(), "no scenario `{name}`");
    let routes: HashMap<String, String> = scenario["routes"]
        .as_object()
        .expect("routes")
        .iter()
        .map(|(oid, coid)| (oid.clone(), coid.as_str().expect("a coid").to_string()))
        .collect();
    let mut out = Run {
        by_coid: HashMap::new(),
        folds: Vec::new(),
        account: Account::new(1.0, "fxcm", None, BalanceMode::Delta),
    };
    for envelope in scenario["envelopes"].as_array().expect("envelopes") {
        for ev in map_drained_event(envelope, &routes) {
            if let Event::Fill(f) = &ev {
                out.folds.push(out.account.apply_fill(f));
            }
            let coid = match &ev {
                Event::OrderFilled(w) => w.client_order_id.clone(),
                Event::OrderCanceled(c) => c.client_order_id.clone(),
                Event::Fill(f) => f.client_order_id.clone(),
                other => panic!("an event this layer does not make: {other:?}"),
            };
            out.by_coid.entry(coid).or_default().push(ev);
        }
    }
    out
}

fn names(evs: &[Event]) -> Vec<&'static str> {
    evs.iter()
        .map(|e| match e {
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "OrderFilled",
            Event::OrderCanceled(_) => "OrderCanceled",
            _ => "other",
        })
        .collect()
}

/// The fill an order's events carry: `(side, last_qty, last_px, trade id)`.
fn fill_of(evs: &[Event]) -> (i32, f64, f64, String) {
    let Some(Event::OrderFilled(w)) = evs.iter().find(|e| matches!(e, Event::OrderFilled(_)))
    else {
        panic!("no OrderFilled in {evs:?}")
    };
    (w.fill.side, w.fill.last_qty, w.fill.last_px, w.fill.trade_id.as_str().to_string())
}

#[test]
fn a_stop_that_fires_closes_the_position_under_its_own_order_and_its_own_trade_id() {
    let run = run(
        "a_partial_close_then_the_stop_on_the_remainder_fires_and_the_venue_deletes_the_take_profit",
    );
    // 2000 bought, 1000 closed by a market close, the remaining 1000 closed by the STOP: flat, every fill applied once.
    assert_eq!(run.folds, [FillFold::Applied, FillFold::Applied, FillFold::Applied]);
    assert_eq!(
        run.account.net_qty("EURUSD"),
        0.0,
        "the stop closed the position: no hedge is left"
    );
    let stop = &run.by_coid["stop"];
    assert_eq!(names(stop), ["Fill", "OrderFilled"], "the stop is FILLED, not cancelled");
    assert_eq!(fill_of(stop), (-1, 1000.0, 1.11936, "186983241-close-416154904".to_string()));
    assert!(
        ends_cancelability(stop) && !closes_routing(stop),
        "a fill ends cancelability and keeps its route"
    );
}

#[test]
fn the_exit_the_venue_deletes_is_cancelled_exactly_once_and_never_filled() {
    for (name, deleted) in [
        (
            "a_partial_close_then_the_stop_on_the_remainder_fires_and_the_venue_deletes_the_take_profit",
            "take-profit",
        ),
        ("the_take_profit_fires_and_the_venue_deletes_the_stop", "stop"),
    ] {
        let result = run(name);
        let evs = &result.by_coid[deleted];
        assert_eq!(
            names(evs),
            ["OrderCanceled"],
            "{name}: the sibling {deleted} is cancelled once"
        );
        assert!(closes_routing(evs) && ends_cancelability(evs), "{name}: the cancel is terminal");
    }
}

#[test]
fn a_take_profit_that_fires_is_a_fill_at_its_rate() {
    let run = run("the_take_profit_fires_and_the_venue_deletes_the_stop");
    assert_eq!(run.folds, [FillFold::Applied, FillFold::Applied]);
    assert_eq!(run.account.net_qty("EURUSD"), 0.0);
    assert_eq!(
        fill_of(&run.by_coid["take-profit"]),
        (-1, 2000.0, 1.11922, "186983145-close-416154926".to_string())
    );
}

/// A trade closed by anything else takes BOTH its exits with it: each is cancelled once, the closing order is filled, and
/// the position is flat. The exits are not left ACCEPTED in the book (the cTrader hole a bridge-side orphan rule fills).
#[test]
fn a_trade_closed_another_way_cancels_both_exits_once_and_leaves_none_accepted() {
    let run = run("the_trade_closed_by_a_market_close_takes_both_exits_with_it");
    assert_eq!(run.account.net_qty("EURUSD"), 0.0);
    for exit in ["stop", "take-profit"] {
        assert_eq!(names(&run.by_coid[exit]), ["OrderCanceled"], "{exit}");
    }
    assert_eq!(names(&run.by_coid["close"]), ["Fill", "OrderFilled"]);
    assert_eq!(fill_of(&run.by_coid["close"]).3, "186983022-close-416154199");
}

/// An exit the node cancelled itself: `exec.rs`'s `Cancel` arm removes the routing row when the venue confirms, so the
/// `canceled` envelope the venue's own Orders Delete (status `C`) then produces for it is unroutable and says nothing.
/// One cancel event, never two.
#[test]
fn the_venues_echo_of_a_cancel_the_node_made_publishes_nothing() {
    let fixture = fixture();
    let scenario = &fixture["scenarios"]["the_take_profit_fires_and_the_venue_deletes_the_stop"];
    let mut routes: HashMap<String, String> = scenario["routes"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(o, c)| (o.clone(), c.as_str().unwrap().to_string()))
        .collect();
    let echo = &scenario["envelopes"][1];
    assert_eq!(echo["kind"], "canceled");
    assert_eq!(
        map_drained_event(echo, &routes).len(),
        1,
        "while the route stands the cancel is heard"
    );
    routes.remove("416154929");
    assert!(
        map_drained_event(echo, &routes).is_empty(),
        "after the node's own cancel removed the route the echo is dropped"
    );
}
