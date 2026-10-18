//! A node's refusal of an order, carried back to the Trade window that sent it.
//!
//! The desktop sends fire-and-forget (blocking a repaint on a network reply is the one thing that
//! path must never do), so the node's `Response::Error` used to land in exactly one place: the
//! handle's LATCHED `last_error`, which the global status bar clips to a short tail and which names
//! no order. A refusal such as "this node's `bybit` engine trades `BTCUSDT` only, not `ETHUSDT`"
//! leaves NO order row either, so the window that sent it showed nothing at all.
//!
//! [`SentOrders`] keeps each sent order's ticket beside the address it was sent to. Each frame
//! [`SentOrders::poll`] asks the handle, without waiting, what the node did with the ones still open;
//! a refusal comes back with the node's own words and the address, and
//! `crate::ui::tool_views::route_node_refusals` puts it on the strip of the windows at that address.
//! Generic over the ticket so the whole decision runs in a test with a stub for the handle.
//!
//! ⚠ A ticket belongs to ONE control handle (its sequence restarts at 0 on a new connection), so the
//! list lives in `crate::backend::backend_conn::BackendConn`, beside the handle, and goes with it
//! on a backend switch.

use vike_exec::{Command, OrderIntent};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_client::CommandOutcome;

/// The address a sent order went to: what a Trade window matches its own address against.
#[derive(Clone, Debug, PartialEq)]
pub struct SentAt {
    pub venue: String,
    pub symbol: String,
    /// `None` for a bracket, which names no account.
    pub account: Option<AccountLabel>,
}

/// Orders sent to a node whose answer has not been read yet.
#[derive(Debug)]
pub struct SentOrders<T> {
    open: Vec<(T, SentAt)>,
}

impl<T> Default for SentOrders<T> {
    fn default() -> Self {
        SentOrders { open: Vec::new() }
    }
}

impl<T: Copy> SentOrders<T> {
    /// Remember `ticket`, the write of `cmd`, when `cmd` places an order (a submit or a bracket); a
    /// cancel or any other command has no window to answer to.
    pub fn track(&mut self, ticket: T, cmd: &Command) {
        let at = match cmd {
            Command::Order(OrderIntent::Submit(req)) => SentAt {
                venue: req.venue.clone(),
                symbol: req.symbol.clone(),
                account: req.account.clone(),
            },
            Command::Order(OrderIntent::Bracket(spec)) => {
                SentAt { venue: spec.venue.clone(), symbol: spec.symbol.clone(), account: None }
            }
            _ => return,
        };
        self.open.push((ticket, at));
    }

    /// Read what the node did with every open order (`outcome_of` must not wait): an answer closes
    /// the order, and a REFUSAL is returned with the node's text, verbatim. Accepted ones, and ones
    /// whose link died, are dropped (the control segment of the status bar says the link died); an
    /// order the node has not answered stays open.
    pub fn poll(
        &mut self,
        outcome_of: impl Fn(T) -> Option<CommandOutcome>,
    ) -> Vec<(SentAt, String)> {
        let mut refused = Vec::new();
        self.open.retain(|(ticket, at)| match outcome_of(*ticket) {
            None => true,
            Some(CommandOutcome::Refused(text)) => {
                refused.push((at.clone(), text));
                false
            }
            Some(_) => false,
        });
        refused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vike_model::OrderRequest;

    fn submit(venue: &str, symbol: &str, account: Option<&str>) -> Command {
        Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "w-1-0".into(),
            venue: venue.into(),
            symbol: symbol.into(),
            account: account.map(|a| AccountLabel::parse(a).expect("a test label")),
            ..Default::default()
        })))
    }

    /// The node refuses the order on a symbol its engine does not trade: the refusal comes back
    /// with the node's words and the ADDRESS the order was sent to, and only once.
    #[test]
    fn a_refusal_comes_back_with_the_nodes_words_and_the_address_once() {
        let node = "submit: this node's `bybit` engine trades `BTCUSDT` only, not `ETHUSDT`";
        let mut sent = SentOrders::<u64>::default();
        sent.track(7, &submit("bybit", "ETHUSDT", Some("SUB")));
        sent.track(8, &submit("bybit", "BTCUSDT", None));
        let board = HashMap::from([
            (7, CommandOutcome::Refused(node.to_string())),
            (8, CommandOutcome::Accepted { coid: "w-1-0".into() }),
        ]);
        let got = sent.poll(|t| board.get(&t).cloned());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, node, "the node's text, unmodified");
        assert_eq!((got[0].0.venue.as_str(), got[0].0.symbol.as_str()), ("bybit", "ETHUSDT"));
        assert_eq!(got[0].0.account, AccountLabel::parse("SUB").ok());
        assert!(sent.poll(|t| board.get(&t).cloned()).is_empty(), "answered orders are closed");
    }

    /// An order the node has not answered stays open and says nothing; a bracket is tracked too;
    /// a cancel is not.
    #[test]
    fn an_unanswered_order_stays_open_and_a_cancel_is_not_tracked() {
        let mut sent = SentOrders::<u64>::default();
        sent.track(1, &submit("binance", "ETHUSDT", None));
        sent.track(2, &Command::Order(OrderIntent::Cancel("w-1-0".into())));
        assert!(sent.poll(|_| None).is_empty());
        let got = sent.poll(|t| (t == 1).then(|| CommandOutcome::Refused("no".into())));
        assert_eq!(got.len(), 1, "still open, now answered");
        assert!(sent.poll(|_| Some(CommandOutcome::Refused("late".into()))).is_empty());
    }

    /// A dead link is not a refusal: it is dropped without a line on the window.
    #[test]
    fn a_dead_link_is_dropped_not_reported_as_a_refusal() {
        let mut sent = SentOrders::<u64>::default();
        sent.track(1, &submit("binance", "ETHUSDT", None));
        assert!(sent.poll(|_| Some(CommandOutcome::Disconnected)).is_empty());
        assert!(sent.poll(|_| Some(CommandOutcome::Refused("x".into()))).is_empty());
    }
}
