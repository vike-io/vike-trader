//! The rollover interest of a closed trade through the drain's own wiring (`fold_with_rollover`: the settle, the routing
//! and decode, then the funding beside the closing fill) and the core's account fold, with scripted shim envelopes in the
//! grammar `fcshim.cpp`'s `pushFill` writes. The pure rule is `rollover_tests.rs`'s; this holds that the exec thread
//! actually applies it, once, and what the BOOKS read afterwards.
//!
//! Take the `rollover.events(..)` call out of `fold_with_rollover` and every test here that closes a trade reads a
//! `funding_paid` of 0.

use super::{PlacedOrder, RouteTable, fold_with_rollover};
use crate::close::CloseTrack;
use crate::commission::OpenCommissions;
use crate::exit_group::ExitGroup;
use crate::rollover::RolloverBooked;
use serde_json::{Value, json};
use std::collections::HashMap;
use vike_exec::{Account, BalanceMode};
use vike_model::events::Event;

struct Desk {
    routes: RouteTable,
    placed: HashMap<String, PlacedOrder>,
    closes: HashMap<String, CloseTrack>,
    exits: HashMap<String, ExitGroup>,
    opens: OpenCommissions,
    rollover: RolloverBooked,
    account: Account,
    /// How many times the session was asked for the account's currency.
    asked: usize,
    /// What the session states for it.
    session_ccy: Option<&'static str>,
}

impl Desk {
    fn new() -> Self {
        let mut routes = RouteTable::default();
        routes.insert("o-open".into(), "c-open".into());
        routes.insert("o-close".into(), "c-close".into());
        Self {
            routes,
            placed: HashMap::new(),
            closes: HashMap::new(),
            exits: HashMap::new(),
            opens: OpenCommissions::default(),
            rollover: RolloverBooked::default(),
            account: Account::new(1.0, "fxcm", None, BalanceMode::Delta),
            asked: 0,
            session_ccy: Some("EUR"),
        }
    }

    /// Drain ONE envelope as `drain_events` does; fold every `Fill` and `Funding` it yields, as the core does, and
    /// return the funding events.
    fn drain(&mut self, v: Value) -> Vec<vike_model::events::FundingEvent> {
        let ccy = self.session_ccy;
        let asked = &mut self.asked;
        let out = fold_with_rollover(
            v,
            &mut self.routes,
            &mut self.placed,
            &mut self.closes,
            &mut self.exits,
            &mut self.opens,
            &mut self.rollover,
            || {
                *asked += 1;
                ccy.map(str::to_string)
            },
        );
        let mut funding = Vec::new();
        for ev in out {
            match ev {
                Event::Fill(f) => {
                    let _ = self.account.apply_fill(&f);
                }
                Event::Funding(f) => {
                    self.account.apply_funding(&f);
                    funding.push(f);
                }
                _ => {}
            }
        }
        funding
    }
}

fn fill(order_id: &str, trade_id: &str, side: &str, rollover: f64) -> Value {
    json!({"kind": "fill", "order_id": order_id, "trade_id": trade_id, "instrument": "EUR/USD",
           "side": side, "amount": 10_000, "rate": 1.1, "commission": 0.0, "rollover": rollover, "ts": 0})
}

/// A trade held overnight closes with a rollover the venue DEBITED: the closing fill books it as funding, negative,
/// under the canonical symbol, in the account's currency, and the account's balance and `funding_paid` follow.
#[test]
fn a_closed_trade_books_its_rollover_beside_the_closing_fill() {
    let mut desk = Desk::new();
    assert!(
        desk.drain(fill("o-open", "101", "B", 0.0)).is_empty(),
        "an opening fill has held nothing overnight"
    );
    let funding = desk.drain(fill("o-close", "101-close-o-close", "S", -0.14));
    assert_eq!(funding.len(), 1, "{funding:?}");
    assert_eq!((funding[0].symbol.as_str(), funding[0].amount), ("EURUSD", -0.14));
    assert_eq!(funding[0].ccy.as_ref().map(|c| c.as_str()), Some("EUR"));
    assert_eq!(desk.account.net_qty("EURUSD"), 0.0, "the trade is closed");
    assert!(
        (desk.account.funding_paid + 0.14).abs() < 1e-12,
        "funding_paid {}",
        desk.account.funding_paid
    );
    assert!((desk.account.balance + 0.14).abs() < 1e-12, "balance {}", desk.account.balance);
}

/// Interest EARNED is positive.
#[test]
fn a_collected_rollover_is_positive() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 0.0));
    let funding = desk.drain(fill("o-close", "101-close-o-close", "S", 0.31));
    assert_eq!(funding[0].amount, 0.31);
    assert!((desk.account.funding_paid - 0.31).abs() < 1e-12);
}

/// A ForexConnect reconnect re-delivers the closed row: the core drops the fill by trade id; the funding must not be
/// booked twice either (it has no id of its own).
#[test]
fn a_redelivered_closing_row_books_its_rollover_once() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 0.0));
    assert_eq!(desk.drain(fill("o-close", "101-close-o-close", "S", -0.14)).len(), 1);
    assert!(desk.drain(fill("o-close", "101-close-o-close", "S", -0.14)).is_empty());
    assert!((desk.account.funding_paid + 0.14).abs() < 1e-12, "{}", desk.account.funding_paid);
}

/// No rollover, no event; and the session is not asked for a currency it has no use for.
#[test]
fn a_close_with_no_rollover_books_nothing_and_asks_nothing() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 0.0));
    assert!(desk.drain(fill("o-close", "101-close-o-close", "S", 0.0)).is_empty());
    assert_eq!(desk.asked, 0);
    assert_eq!(desk.account.funding_paid, 0.0);
}

/// A close of an order this process never placed emits no fill (the declared restart hole), so it books no rollover
/// either: the position and its interest move together or not at all.
#[test]
fn a_close_this_process_did_not_place_books_no_rollover() {
    let mut desk = Desk::new();
    let funding = desk.drain(fill("o-unknown", "202-close-o-unknown", "S", -0.14));
    assert!(funding.is_empty());
    assert_eq!(desk.account.funding_paid, 0.0);
}

/// The currency is read once, from the session, and an unstated one leaves the event unstated.
#[test]
fn the_accounts_currency_is_read_once_and_an_unstated_one_stays_unstated() {
    let mut desk = Desk::new();
    desk.drain(fill("o-close", "301-close-o-close", "S", -0.1));
    assert_eq!(desk.asked, 1);
    desk.routes.insert("o-close2".into(), "c-close2".into());
    desk.drain(fill("o-close2", "302-close-o-close2", "S", -0.1));
    assert_eq!(desk.asked, 1, "answered once: never asked again");

    let mut unstated = Desk::new();
    unstated.session_ccy = None;
    let funding = unstated.drain(fill("o-close", "401-close-o-close", "S", -0.1));
    assert!(funding[0].ccy.is_none(), "the session stated none");
    assert_eq!(unstated.asked, 1);
    unstated.routes.insert("o-close2".into(), "c-close2".into());
    unstated.drain(fill("o-close2", "402-close-o-close2", "S", -0.1));
    assert_eq!(unstated.asked, 2, "asked again until it answers");
}
