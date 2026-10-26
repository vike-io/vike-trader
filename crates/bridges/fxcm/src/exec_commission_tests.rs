//! A trade's commission through the drain's own wiring (`fold_drained`: the settle, then the routing and the decode) and the
//! core's account fold, with scripted shim envelopes in the grammar `fcshim.cpp`'s `pushFill` writes. The pure rule is
//! `commission_tests.rs`'s; this holds that the exec thread actually applies it, and what the BOOKS read afterwards.
//!
//! Take the `opens.settle(..)` call out of `fold_drained` and the closing fill books the closed row's WHOLE figure on top of
//! the open fill's: every test here that closes a trade then reads double the fee (and one reads the open side twice).

use super::{PlacedOrder, RouteTable, fold_drained};
use crate::close::CloseTrack;
use crate::commission::OpenCommissions;
use serde_json::{Value, json};
use std::collections::HashMap;
use vike_exec::{Account, BalanceMode};
use vike_model::events::Event;

/// The exec thread's state for these tests, plus the account the node would fold the fills into.
struct Desk {
    routes: RouteTable,
    placed: HashMap<String, PlacedOrder>,
    closes: HashMap<String, CloseTrack>,
    opens: OpenCommissions,
    account: Account,
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
            opens: OpenCommissions::default(),
            account: Account::new(1.0, "fxcm", None, BalanceMode::Delta),
        }
    }

    /// Drain ONE envelope as `drain_events` does and fold every bare `Fill` it yields, as the core does.
    fn drain(&mut self, v: Value) {
        for ev in
            fold_drained(v, &mut self.routes, &mut self.placed, &mut self.closes, &mut self.opens)
        {
            if let Event::Fill(f) = ev {
                let _ = self.account.apply_fill(&f);
            }
        }
    }

    /// The fees the account has booked, as a sum of figures with six decimals: compared to the cent's millionth.
    fn assert_fees(&self, expected: f64, why: &str) {
        let booked = self.account.fees_paid;
        assert!(
            (booked - expected).abs() < 1e-9,
            "{why}: the account booked {booked}, expected {expected}"
        );
    }
}

fn fill(order_id: &str, trade_id: &str, side: &str, amount: i64, commission: f64) -> Value {
    json!({"kind": "fill", "order_id": order_id, "trade_id": trade_id, "instrument": "EUR/USD",
           "side": side, "amount": amount, "rate": 1.1, "commission": commission, "ts": 0})
}

/// The SDK's worked example: EUR/USD at $0.00008 a unit "any deal", 10K units. 0.40 at the open, 0.40 at the close, and the
/// closed row reads the position's total, 0.80. Both halves reach the books, each once.
#[test]
fn a_schedule_that_pays_half_at_each_deal_books_both_halves_once() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 10_000, 0.40));
    desk.assert_fees(0.40, "the open half");
    desk.drain(fill("o-close", "101-close-o-close", "S", 10_000, 0.80));
    desk.assert_fees(
        0.80,
        "the close owes the other half, not the closed row's whole figure on top of the first",
    );
    assert_eq!(desk.account.net_qty("EURUSD"), 0.0, "and the trade is closed");
}

/// A schedule that pays at the CLOSE leaves the open row at `0.0`: before this the whole fee was booked as nothing.
#[test]
fn a_schedule_that_pays_at_the_close_books_it_there() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 10_000, 0.0));
    desk.drain(fill("o-close", "101-close-o-close", "S", 10_000, 0.80));
    desk.assert_fees(0.80, "the whole schedule, at the close");
}

/// A schedule that pays at the OPEN: the closed row repeats it and the close books nothing more.
#[test]
fn a_schedule_that_pays_at_the_open_is_not_charged_again_at_the_close() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 10_000, 0.80));
    desk.drain(fill("o-close", "101-close-o-close", "S", 10_000, 0.80));
    desk.assert_fees(0.80, "charged at the open, repeated by the closed row, booked once");
}

/// The sign is unmeasured: an account that wrote its charges as NEGATIVE numbers pays the same fees, never a rebate. Without
/// `charge` the open fill books `-0.40` (the core nets a negative commission in as income) and the books read `-0.40`.
#[test]
fn a_venue_that_signs_a_charge_negative_still_books_it_as_a_cost() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 10_000, -0.40));
    desk.assert_fees(0.40, "the open half, a cost");
    desk.drain(fill("o-close", "101-close-o-close", "S", 10_000, -0.80));
    desk.assert_fees(0.80, "the other half at the close, a cost, once");
}

/// The demo account: nothing is charged anywhere (measured, 20 fills), and nothing is invented.
#[test]
fn a_commission_free_account_books_nothing() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 10_000, 0.0));
    desk.drain(fill("o-close", "101-close-o-close", "S", 10_000, 0.0));
    desk.assert_fees(0.0, "nothing is charged");
}

/// A reconnect re-surfaces the same rows: the core drops the repeated trade ids and the fee stays what it was.
#[test]
fn a_replay_after_a_reconnect_books_no_second_fee() {
    let mut desk = Desk::new();
    for _ in 0..2 {
        desk.drain(fill("o-open", "101", "B", 10_000, 0.40));
        desk.drain(fill("o-close", "101-close-o-close", "S", 10_000, 0.80));
    }
    desk.assert_fees(0.80, "the replay is the same two trade ids");
}

/// A part close: 2000 units opened (open half 0.08), 1000 closed (the closed row reads those 1000 units' total, 0.08): the
/// close books the other 0.04, so the two fills together book 0.12 of the 0.16 the 2000 units will owe when the rest closes.
#[test]
fn a_part_close_books_its_own_share_of_the_open_fill() {
    let mut desk = Desk::new();
    desk.drain(fill("o-open", "101", "B", 2_000, 0.08));
    desk.drain(fill("o-close", "101-close-o-close", "S", 1_000, 0.08));
    desk.assert_fees(0.12, "the open half of 2000 units plus the close half of 1000");
    assert_eq!(desk.account.net_qty("EURUSD"), 1_000.0);
}

/// A close this process cannot match to an opening fill books nothing rather than the whole figure: the open side may
/// already be in the books (a restart, a fill the reconcile pass booked), and a fee charged twice is the worse mistake.
#[test]
fn a_close_with_no_known_open_fill_books_nothing() {
    let mut desk = Desk::new();
    desk.drain(fill("o-close", "555-close-o-close", "S", 10_000, 0.80));
    desk.assert_fees(0.0, "no open fill was seen");
}
