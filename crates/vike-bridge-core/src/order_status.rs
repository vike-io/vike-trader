//! A venue's order-status WORDS → the FSM [`OrderStatus`] a reconcile report (or an open list) carries,
//! and the ONE reading of a word the venue's table does not name.
//!
//! The words are the venue's and stay in its bridge, as an [`OrderStatusTable`] it reads through
//! [`OrderStatusTable::status_of`]. What a word OUTSIDE the table means is this module's:
//! [`unknown_status_word`] warns and reads it `Accepted`, live, the conservative direction — `diff`
//! raises no `MissingTerminal` on it, adoption synthesizes no cumulative fill, and the restore planner
//! keeps the order rather than declaring it gone.
//!
//! What is NOT a table here, and why: the binance family's `OrderWord` (an enum its five readers match
//! with no `_` arm; its unknown arm calls [`unknown_status_word`]), hyperliquid's suffix rule
//! (`*Canceled`, `*Rejected`: a venue fact beside its table), vike-ibkr's `OrderStatusKind` (shared with
//! its exec path, and an unknown IBKR word is `Rejected` by the owner's ruling), cTrader's protobuf
//! enum, and a row that names NO word, whose status is its fill progress
//! ([`OrderStatus::from_fill_progress`]).

use vike_model::OrderStatus;

/// One venue's status words, each with the FSM status it stands for. A word is matched exactly (the
/// venue's own case); the first row naming it wins.
#[derive(Debug, Clone, Copy)]
pub struct OrderStatusTable {
    pub words: &'static [(&'static str, OrderStatus)],
}

impl OrderStatusTable {
    /// The status `word` stands for, or `None` for a word the table does not name (nothing logged).
    #[must_use]
    pub fn get(&self, word: &str) -> Option<OrderStatus> {
        self.words.iter().find(|(w, _)| *w == word).map(|(_, status)| *status)
    }

    /// The status `word` stands for; a word the table does not name is [`unknown_status_word`].
    pub fn status_of(&self, venue: &str, word: &str) -> OrderStatus {
        self.get(word).unwrap_or_else(|| unknown_status_word(venue, word))
    }
}

/// A status word no table of `venue` names: WARNED, and read `Accepted` (the module doc says why).
pub fn unknown_status_word(venue: &str, word: &str) -> OrderStatus {
    tracing::warn!(
        venue,
        status = word,
        "unknown order status word — reporting the order as live (Accepted)"
    );
    OrderStatus::Accepted
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: OrderStatusTable = OrderStatusTable {
        words: &[
            ("live", OrderStatus::Accepted),
            ("filled", OrderStatus::Filled),
            ("canceled", OrderStatus::Canceled),
        ],
    };

    #[test]
    fn a_named_word_is_its_status_and_logs_nothing() {
        let (status, logged) = vike_log::capture::captured(|| TABLE.status_of("planted", "filled"));
        assert_eq!(status, OrderStatus::Filled);
        assert!(logged.is_empty(), "{logged:?}");
        assert_eq!(TABLE.get("canceled"), Some(OrderStatus::Canceled));
    }

    /// The match is exact: a word in another case is not the table's word.
    #[test]
    fn an_unknown_word_is_live_and_warned_with_the_venue_and_the_word() {
        for word in ["mystery", "FILLED", ""] {
            assert_eq!(TABLE.get(word), None, "{word:?}");
            let (status, logged) = vike_log::capture::captured(|| TABLE.status_of("planted", word));
            assert_eq!(status, OrderStatus::Accepted, "{word:?}");
            let [line] = logged.as_slice() else { panic!("one line for {word:?}: {logged:?}") };
            assert_eq!(line.level, tracing::Level::WARN);
            assert_eq!(line.field("venue"), Some("planted"));
            assert_eq!(line.field("status"), Some(word));
            assert_eq!(
                line.message,
                "unknown order status word — reporting the order as live (Accepted)"
            );
        }
    }
}
