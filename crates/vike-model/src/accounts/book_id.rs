//! [`BookId`] — **which venue BOOK an account trades**, in the one form two answers are compared in.
//!
//! "Book" here is the venue's own account (an EVM address, an IBKR `DU…` code, a numeric account
//! id): the `vike.db` column `account.venue_account_id`, a venue's handshake answer, an address
//! derived from a stored key. Every place that asks "is this the same book?" (two armed accounts on
//! one book, the recorded book against the one the venue confirmed) compares `BookId`s, so the
//! normalisation is written ONCE, in [`BookId::new`], and no literal can skip it (the fields are
//! private).
//!
//! ⚠ **A `BookId` is for COMPARING and LOGGING, never a value sent to a venue.** It lowercases: IG
//! sends the stored id raw to its session endpoint, Dukascopy addresses a broker row by it and IBKR
//! account codes are upper case. Every wire value keeps reading the raw stored text.
//!
//! No serde and no `Display`: nothing stores a `BookId`, and a log prints [`BookId::book`].

/// One venue book, normalised: the venue id and the book text, trimmed and ASCII-lowercased.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BookId {
    venue: String,
    book: String,
}

impl BookId {
    /// The book `raw` names at `venue`, trimmed and ASCII-lowercased (both shapes in use are
    /// case-insensitive: an EVM address, an IBKR `DUQ186573`). `None` when nothing is left: a blank
    /// book is the placeholder the shared-book rule forbids, because it would compare equal to every
    /// other unknown.
    #[must_use]
    pub fn new(venue: &str, raw: &str) -> Option<BookId> {
        let book = raw.trim();
        (!book.is_empty())
            .then(|| BookId { venue: venue.to_string(), book: book.to_ascii_lowercase() })
    }

    /// The venue id this book is at.
    #[must_use]
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The normalised book text.
    #[must_use]
    pub fn book(&self) -> &str {
        &self.book
    }
}

#[path = "book_id_tests.rs"]
#[cfg(test)]
mod book_id_tests;
