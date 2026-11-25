//! A node's refusal of an order, carried back to the Trade window that sent it.
//!
//! The desktop sends fire-and-forget (blocking a repaint on a network reply is the one thing that
//! path must never do), so the node's `Response::Error` used to land in exactly one place: the
//! handle's LATCHED `last_error`, which the global status bar clips to a short tail and which names
//! no order. A refusal such as "this node's `bybit` engine trades `BTCUSDT` only, not `ETHUSDT`"
//! leaves NO order row either, so the window that sent it showed nothing at all.
//!
//! `SentOrders` keeps each sent order beside its ticket. Each frame `SentOrders::poll` asks the
//! handle, without waiting, what the node did with the ones still open; a refusal comes back with the
//! node's own words and the address, and `crate::ui::windows::route_node_refusals` puts it on the
//! strip of the windows at that address. The same poll returns the node's NOTES on an order it
//! accepted and sent (the closed-market note, [`SentOrderWord::Noted`]), which go on the same strip:
//! the desktop adds no widget for them. Generic over the ticket so the whole decision runs in a test
//! with a stub for the handle.
//!
//! ⚠ **An order with no answer is said too, and its retry goes under its OWN order client id.** A link
//! that dies after the order was written is OUTCOME UNKNOWN, one already dead is NOT SENT
//! ([`SentOrderWord::Unanswered`], worded by `vike_tradehub_client::UnansweredCommand`'s `Display`, the
//! wording `vike-cli` and `vike-mcp` show). Each click mints a NEW id, so a re-click after an unknown
//! outcome used to place the order twice whenever the first had landed. [`UnansweredOrders`] keeps
//! each unknown order and gives a re-click of the same order its id back, and the node's door refuses
//! an id an earlier order carries: the id is the retry's idempotency key.
//!
//! ⚠ A ticket belongs to ONE control handle (its sequence restarts at 0 on a new connection), so the
//! list lives in `crate::backend::connection::BackendConnection`, beside the handle, and goes with it
//! on a backend switch. [`UnansweredOrders`] names no ticket and must outlive that switch (a lost link
//! is only reopened by one), so the shell keeps it beside its id mint.

use vike_execution::{Command, CoreSnapshot, OrderIntent};
use vike_model::{OrderBracket, OrderRequest};
use vike_order_entry::trade_window::address::TradeWindowAddress;
use vike_tradehub_client::{CommandOutcome, CommandRefusal, UnansweredCommand};
use vike_utility::errors::FailureOutcome;

/// What the node said about a sent order, for the strip of its window.
#[derive(Clone, Debug, PartialEq)]
pub enum SentOrderWord {
    /// The node answered NOT NOW (it is starting or its core is busy): nothing was placed, and the same order is
    /// taken shortly. Never a refusal: the strip says it in the one NOT NOW wording.
    NotNow(CommandRefusal),
    /// The node REFUSED the order, its final no, as it came: nothing was placed.
    Refused(CommandRefusal),
    /// The node ACCEPTED and sent the order and has this to say about it, in its own words (today: it was
    /// placed into a closed market). One entry per note, never a reason to think the order was held back.
    /// `order_client_id` is the order's order client id as the node echoed it (empty for a bracket: the runtime mints
    /// its ids after the Ack).
    Noted { order_client_id: String, text: String },
    /// No answer came: OUTCOME UNKNOWN or NOT SENT (`unanswered.failure_outcome()`), said on the strip
    /// in `unanswered`'s words. `order` is the order as it was sent, its order client id included.
    Unanswered { unanswered: UnansweredCommand, order: SentOrder },
}

/// An order a window sent, as it went: its order client id (a bracket's group stem) is what a retry of
/// it must carry. Boxed as `Command` boxes them.
#[derive(Clone, Debug, PartialEq)]
pub enum SentOrder {
    Submit(Box<OrderRequest>),
    Bracket(Box<OrderBracket>),
}

impl SentOrder {
    /// The order `command` places: a submit or a bracket; a cancel or any other command places none.
    fn of(command: &Command) -> Option<SentOrder> {
        match command {
            Command::Order(OrderIntent::Submit(req)) => Some(SentOrder::Submit(req.clone())),
            Command::Order(OrderIntent::Bracket(spec)) => Some(SentOrder::Bracket(spec.clone())),
            _ => None,
        }
    }

    /// The window address the order was sent from.
    fn address(&self) -> TradeWindowAddress {
        let (venue, symbol, account) = match self {
            SentOrder::Submit(req) => (&req.venue, &req.symbol, &req.account),
            SentOrder::Bracket(spec) => (&spec.venue, &spec.symbol, &spec.account),
        };
        TradeWindowAddress {
            venue: venue.clone(),
            symbol: symbol.clone(),
            account: account.clone(),
        }
    }

    /// Whether the node's order `order_client_id` is this order (or, for a bracket's stem, one of its legs).
    fn is(&self, order_client_id: &str) -> bool {
        match self {
            SentOrder::Submit(req) => req.order_client_id == order_client_id,
            SentOrder::Bracket(spec) => spec
                .order_client_id
                .as_deref()
                .is_some_and(|stem| order_client_id.starts_with(stem)),
        }
    }

    /// The same order as `other` but for its order client id.
    fn repeats(&self, other: &SentOrder) -> bool {
        match (self, other) {
            (SentOrder::Submit(a), SentOrder::Submit(b)) => {
                let unnamed =
                    |r: &OrderRequest| OrderRequest { order_client_id: String::new(), ..r.clone() };
                unnamed(a) == unnamed(b)
            }
            (SentOrder::Bracket(a), SentOrder::Bracket(b)) => {
                let unnamed =
                    |s: &OrderBracket| OrderBracket { order_client_id: None, ..s.clone() };
                unnamed(a) == unnamed(b)
            }
            _ => false,
        }
    }
}

/// Orders sent to a node whose answer has not been read yet.
#[derive(Debug)]
pub(crate) struct SentOrders<T> {
    open: Vec<(T, SentOrder)>,
}

impl<T> Default for SentOrders<T> {
    fn default() -> Self {
        SentOrders { open: Vec::new() }
    }
}

impl<T: Copy> SentOrders<T> {
    /// Remember `ticket`, the write of `command`, when `command` places an order (a submit or a bracket); a
    /// cancel or any other command has no window to answer to.
    pub(crate) fn track(&mut self, ticket: T, command: &Command) {
        if let Some(order) = SentOrder::of(command) {
            self.open.push((ticket, order));
        }
    }

    /// Read what the node did with every open order (`outcome_of` must not wait): an answer closes
    /// the order, and a NOT NOW or a REFUSAL is returned as the node gave it, as is each NOTE of an
    /// accepted one ([`SentOrderWord::Noted`]) and an order whose link died
    /// ([`SentOrderWord::Unanswered`]). Accepted ones with nothing to say are dropped; an order the node
    /// has not answered stays open.
    pub(crate) fn poll(
        &mut self,
        outcome_of: impl Fn(T) -> Option<CommandOutcome>,
    ) -> Vec<(TradeWindowAddress, SentOrderWord)> {
        let mut words = Vec::new();
        self.open.retain(|(ticket, order)| match outcome_of(*ticket) {
            None => true,
            Some(CommandOutcome::NotNow(refusal)) => {
                words.push((order.address(), SentOrderWord::NotNow(refusal)));
                false
            }
            Some(CommandOutcome::Refused(refusal)) => {
                words.push((order.address(), SentOrderWord::Refused(refusal)));
                false
            }
            Some(CommandOutcome::Accepted { order_client_id, notes }) => {
                words.extend(notes.into_iter().map(|text| {
                    (
                        order.address(),
                        SentOrderWord::Noted { order_client_id: order_client_id.clone(), text },
                    )
                }));
                false
            }
            // Named, never a wildcard: an outcome added later must not fall silently into this arm.
            Some(outcome @ (CommandOutcome::Disconnected | CommandOutcome::NeverSent)) => {
                if let Some(unanswered) = outcome.unanswered() {
                    let word = SentOrderWord::Unanswered { unanswered, order: order.clone() };
                    words.push((order.address(), word));
                }
                false
            }
        });
        words
    }
}

/// The orders whose outcome is UNKNOWN, kept until the node is seen to hold one or a re-click sends it
/// again ([`Self::reuse_id`]). The shell holds it beside its id mint, across backend switches.
#[derive(Debug, Default)]
pub struct UnansweredOrders {
    orders: Vec<SentOrder>,
}

impl UnansweredOrders {
    /// Keep each OUTCOME UNKNOWN order of `words` that carries an id (a bracket the node mints the legs of
    /// carries none). A NOT SENT one is not kept: nothing of it reached the node, so a new id is as safe.
    pub fn note(&mut self, words: &[(TradeWindowAddress, SentOrderWord)]) {
        for (_, word) in words {
            if let SentOrderWord::Unanswered { unanswered, order } = word
                && unanswered.failure_outcome() == FailureOutcome::OutcomeUnknown
                && !matches!(order, SentOrder::Bracket(spec) if spec.order_client_id.is_none())
            {
                self.orders.push(order.clone());
            }
        }
    }

    /// Give `command` the order client id of the unknown order it repeats (the same order, only the id
    /// differs), and forget that order: the retry is tracked as any sent order is. An unknown order
    /// `snapshot` holds reached the node, so it is forgotten first and a re-click of it is a new order.
    pub fn reuse_id(&mut self, command: &mut Command, snapshot: &CoreSnapshot) {
        self.orders.retain(|kept| !snapshot.orders.iter().any(|o| kept.is(&o.order_client_id)));
        let Some(sent) = SentOrder::of(command) else { return };
        let Some(at) = self.orders.iter().position(|kept| kept.repeats(&sent)) else { return };
        match (command, self.orders.remove(at)) {
            (Command::Order(OrderIntent::Submit(req)), SentOrder::Submit(kept)) => {
                req.order_client_id = kept.order_client_id;
            }
            (Command::Order(OrderIntent::Bracket(spec)), SentOrder::Bracket(kept)) => {
                spec.order_client_id = kept.order_client_id;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vike_model::accounts::account_keys::AccountLabel;

    fn submit(venue: &str, symbol: &str, account: Option<&str>) -> Command {
        Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            order_client_id: "w-1-0".into(),
            venue: venue.into(),
            symbol: symbol.into(),
            account: account.map(|a| AccountLabel::parse(a).expect("a test label")),
            ..Default::default()
        })))
    }

    /// A market buy of `qty` under `order_client_id`, the order a Trade window click sends.
    fn buy(order_client_id: &str, qty: f64) -> Command {
        Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty,
            ..Default::default()
        })))
    }

    fn order_client_id_of(command: &Command) -> &str {
        match command {
            Command::Order(OrderIntent::Submit(req)) => &req.order_client_id,
            other => panic!("not a submit: {other:?}"),
        }
    }

    /// What the desktop learns of the order sent under ticket 1 when its link dies after the write.
    fn unknown_after_a_drop(sent: &Command) -> Vec<(TradeWindowAddress, SentOrderWord)> {
        let mut orders = SentOrders::<u64>::default();
        orders.track(1, sent);
        orders.poll(|_| Some(CommandOutcome::Disconnected))
    }

    /// The node's book holding one order, `order_client_id`.
    fn holding(order_client_id: &str) -> CoreSnapshot {
        let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
        snapshot.orders.push(std::sync::Arc::new(vike_execution::OrderView {
            order_client_id: order_client_id.into(),
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            price: None,
            trigger_price: None,
            status: vike_model::OrderStatus::Filled,
            venue_order_id: None,
            filled_qty: 1.0,
            average_fill_price: 100.0,
            refusal_id: None,
            reason: None,
            pending_price: None,
            trail: None,
            limit_offset: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            release: None,
            display_quantity: None,
            data_delay: None,
        }));
        snapshot
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
            (7, CommandOutcome::Refused(CommandRefusal::Text(node.to_string()))),
            (8, CommandOutcome::Accepted { order_client_id: "w-1-0".into(), notes: Vec::new() }),
        ]);
        let got = sent.poll(|t| board.get(&t).cloned());
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].1,
            SentOrderWord::Refused(CommandRefusal::Text(node.to_string())),
            "the node's refusal, unmodified"
        );
        assert_eq!((got[0].0.venue.as_str(), got[0].0.symbol.as_str()), ("bybit", "ETHUSDT"));
        assert_eq!(got[0].0.account, AccountLabel::parse("SUB").ok());
        assert!(sent.poll(|t| board.get(&t).cloned()).is_empty(), "answered orders are closed");
    }

    /// An order the node ACCEPTED with a note (it was placed into a closed market) comes back with the
    /// note in the node's words and the address, once; an accepted order with nothing to say comes back
    /// with nothing.
    #[test]
    fn a_note_on_an_accepted_order_comes_back_with_the_address_once() {
        let note = "EURUSD: the FX market is closed until Sunday 22:00 UTC: the venue may hold, \
                    refuse or queue this order";
        let mut sent = SentOrders::<u64>::default();
        sent.track(1, &submit("oanda", "EURUSD", None));
        sent.track(2, &submit("oanda", "GBPUSD", None));
        let board = HashMap::from([
            (
                1,
                CommandOutcome::Accepted {
                    order_client_id: "w-1-0".into(),
                    notes: vec![note.to_string()],
                },
            ),
            (2, CommandOutcome::Accepted { order_client_id: "w-1-1".into(), notes: Vec::new() }),
        ]);
        let got = sent.poll(|t| board.get(&t).cloned());
        assert_eq!(got.len(), 1, "only the order with a note says anything: {got:?}");
        assert_eq!(
            got[0].1,
            SentOrderWord::Noted { order_client_id: "w-1-0".into(), text: note.to_string() }
        );
        assert_eq!((got[0].0.venue.as_str(), got[0].0.symbol.as_str()), ("oanda", "EURUSD"));
        assert!(sent.poll(|t| board.get(&t).cloned()).is_empty(), "answered orders are closed");
    }

    /// **An order the node answered NOT NOW comes back as NOT NOW, never as a refusal**: a busy core's frame, at the
    /// address it was sent from, once.
    #[test]
    fn a_not_now_order_comes_back_as_not_now_never_as_a_refusal() {
        use vike_tradehub_client::refusals::TradehubRefusal;
        let busy = CommandRefusal::Frame(TradehubRefusal::Busy.into());
        let mut sent = SentOrders::<u64>::default();
        sent.track(1, &submit("bybit", "BTCUSDT", None));
        let board = HashMap::from([(1, CommandOutcome::NotNow(busy.clone()))]);
        let got = sent.poll(|t| board.get(&t).cloned());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, SentOrderWord::NotNow(busy));
        assert_eq!((got[0].0.venue.as_str(), got[0].0.symbol.as_str()), ("bybit", "BTCUSDT"));
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
        let refused = |text: &str| CommandOutcome::Refused(CommandRefusal::Text(text.into()));
        let got = sent.poll(|t| (t == 1).then(|| refused("no")));
        assert_eq!(got.len(), 1, "still open, now answered");
        assert!(sent.poll(|_| Some(refused("late"))).is_empty());
    }

    /// A dead link is not a refusal and not silence: an order written before it died is OUTCOME
    /// UNKNOWN, one it never reached is NOT SENT, each said once at its address with the order as sent.
    #[test]
    fn a_dead_link_says_outcome_unknown_or_not_sent_once() {
        let mut sent = SentOrders::<u64>::default();
        sent.track(1, &submit("binance", "ETHUSDT", None));
        sent.track(2, &submit("binance", "BTCUSDT", None));
        let board =
            HashMap::from([(1, CommandOutcome::Disconnected), (2, CommandOutcome::NeverSent)]);
        let got = sent.poll(|t| board.get(&t).cloned());
        let said: Vec<_> = got
            .iter()
            .map(|(at, word)| match word {
                SentOrderWord::Unanswered { unanswered, order } => {
                    assert_eq!(order.address(), *at, "said at the address it was sent from");
                    (at.symbol.as_str(), unanswered.failure_outcome())
                }
                other => panic!("an unanswered order said {other:?}"),
            })
            .collect();
        assert_eq!(
            said,
            [("ETHUSDT", FailureOutcome::OutcomeUnknown), ("BTCUSDT", FailureOutcome::NotSent)]
        );
        assert!(sent.poll(|t| board.get(&t).cloned()).is_empty(), "said once");
    }

    /// THE double order: a re-click of an order whose outcome is unknown mints a new id, and the
    /// retry goes out under the FIRST one's id instead, once; a different order keeps its own id.
    #[test]
    fn a_retry_of_an_unknown_order_goes_under_its_own_order_client_id_once() {
        let mut unanswered = UnansweredOrders::default();
        unanswered.note(&unknown_after_a_drop(&buy("G-first", 1.0)));
        let nothing = CoreSnapshot::empty("binance", "BTCUSDT");

        let mut other = buy("G-other", 2.0);
        unanswered.reuse_id(&mut other, &nothing);
        assert_eq!(order_client_id_of(&other), "G-other", "another size is another order");

        let mut retry = buy("G-retry", 1.0);
        unanswered.reuse_id(&mut retry, &nothing);
        assert_eq!(order_client_id_of(&retry), "G-first", "the retry carries the first id");

        let mut again = buy("G-again", 1.0);
        unanswered.reuse_id(&mut again, &nothing);
        assert_eq!(order_client_id_of(&again), "G-again", "the retry used the kept id up");
    }

    /// An unknown order the node is seen to hold reached it: a re-click of it is a new order under a
    /// new id. A NOT SENT order is not kept: nothing of it went.
    #[test]
    fn an_unknown_order_the_node_holds_is_settled_and_a_not_sent_one_is_not_kept() {
        let mut unanswered = UnansweredOrders::default();
        unanswered.note(&unknown_after_a_drop(&buy("G-first", 1.0)));
        let mut next = buy("G-next", 1.0);
        unanswered.reuse_id(&mut next, &holding("G-first"));
        assert_eq!(order_client_id_of(&next), "G-next", "the first one landed: this is a second");

        let mut sent = SentOrders::<u64>::default();
        sent.track(1, &buy("G-unsent", 1.0));
        unanswered.note(&sent.poll(|_| Some(CommandOutcome::NeverSent)));
        let mut fresh = buy("G-fresh", 1.0);
        unanswered.reuse_id(&mut fresh, &CoreSnapshot::empty("binance", "BTCUSDT"));
        assert_eq!(order_client_id_of(&fresh), "G-fresh", "nothing of a NOT SENT order went");
    }
}
