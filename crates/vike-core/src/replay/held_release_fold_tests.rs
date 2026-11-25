//! White-box tests of `boot.rs`'s `live_held_releases`: which of the latest `Snapshot`'s held orders a
//! SNAP-ONLY restore holds again, read off the crash tail alone. A venue-scoped mass-cancel or market exit
//! reaches the held orders of the ONE account it was lowered onto (an order names one account, never fans
//! out); a release is the journaled `OrderOn` of the order client id, and a plain submit reusing it is not one.

use super::boot::live_held_releases;
use super::*;
use vike_execution::{Command, OrderIntent, RoutedOrder};
use vike_journal::OrderHeldRelease;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{OrderRelease, OrderType};

fn held(order_client_id: &str, route_key: &str) -> OrderHeldRelease {
    OrderHeldRelease {
        route_key: route_key.into(),
        request: OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: OrderType::Limit,
            price: Some(100.0),
            release: Some(OrderRelease { at: 5_000, price: None }),
            ..Default::default()
        },
    }
}

fn cmd(intent: OrderIntent) -> JournalRecord {
    JournalRecord::Command {
        sequence: 1,
        now_ms: 1,
        message: Ingest::Command(Command::Order(intent)),
    }
}

fn mass_cancel(account: Option<&str>) -> OrderIntent {
    OrderIntent::MassCancel {
        venue: Some("binance".into()),
        symbol: None,
        account: account.map(|a| AccountLabel::parse(a).expect("a label")),
    }
}

fn kept(tail: &[JournalRecord]) -> Vec<String> {
    let book = [held("d", "binance"), held("a", "binance#ALT")];
    live_held_releases(&book, tail).into_iter().map(|h| h.request.order_client_id).collect()
}

/// A mass-cancel that NAMES an account ends that account's held orders only; one that names none is the
/// venue's default account's (a strategy's carries its own engine's route key); a market exit the same.
#[test]
fn a_tail_mass_cancel_ends_the_held_orders_of_its_own_account_only() {
    assert_eq!(kept(&[cmd(mass_cancel(Some("ALT")))]), vec!["d"]);
    assert_eq!(kept(&[cmd(mass_cancel(None))]), vec!["a"], "account-less: the default account's");
    let strategy = JournalRecord::StrategySubmit {
        sequence: 1,
        now_ms: 1,
        mount_id: "m".into(),
        intent: mass_cancel(None),
        route_key: Some("binance#ALT".into()),
    };
    assert_eq!(kept(&[strategy]), vec!["d"], "a mount's: its own engine's");
    let exit = OrderIntent::MarketExit { venue: Some("binance".into()), account: None };
    assert_eq!(kept(&[cmd(exit)]), vec!["a"]);
    let all = OrderIntent::MassCancel { venue: None, symbol: None, account: None };
    assert!(kept(&[cmd(all)]).is_empty(), "a cancel naming no venue reaches every account");
}

/// The journaled `OrderOn` of an order client id is its release; a plain `Submit` reusing the order client id was refused live,
/// so the order is still held; a cancel by order client id ends it.
#[test]
fn only_the_journaled_release_or_a_cancel_ends_a_held_order() {
    let mut released = held("d", "binance").request;
    released.release = None;
    let release = JournalRecord::Command {
        sequence: 1,
        now_ms: 1,
        message: Ingest::Command(Command::OrderOn(Box::new(RoutedOrder {
            route_key: "binance".into(),
            intent: OrderIntent::Submit(Box::new(released.clone())),
        }))),
    };
    assert_eq!(kept(&[release]), vec!["a"]);
    assert_eq!(kept(&[cmd(OrderIntent::Submit(Box::new(released)))]), vec!["d", "a"]);
    assert_eq!(kept(&[cmd(OrderIntent::Cancel("a".into()))]), vec!["d"]);
}
