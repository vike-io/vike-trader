use super::*;
use vike_data::{LiveDataError, SubscriptionId, live::FixedSymbols};

/// A client that accepts every lane but bars and emits nothing — `AssembledFeed`'s own logic never
/// calls it, so these tests need only something that type-checks as a `DataClient + Send`.
struct NullClient;

impl DataClient for NullClient {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("test"))
    }
    fn subscribe_quotes(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(0))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(0))
    }
    fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(0))
    }
    fn unsubscribe(&mut self, _: SubscriptionId) {}
    fn shutdown(&mut self) {}
}

fn feed(book_carries_quotes: bool) -> AssembledFeed {
    AssembledFeed::new(
        "v",
        book_carries_quotes,
        Box::new(FixedSymbols::new(["A"])),
        Box::new(NullClient),
    )
}

/// An explicit symbol list names no group, so it records per-symbol series — whichever venue's
/// constructor built the feed (this was the binance and polymarket modules' own test until both
/// became constructors of this one type).
#[test]
fn an_explicit_symbol_list_has_no_family() {
    let mut f = feed(false);
    assert_eq!(f.family(), None);
    assert_eq!(f.desired(0).unwrap().len(), 1);
}

/// `Depth` passes through either way: a venue that serves none is told so by its own client, and
/// `SubscriptionSet` learns `Unsupported` once rather than `narrow` having to know about it. Only
/// the book-implies-quotes overlap is the feed's business, and only where the venue declared it.
#[test]
fn book_supersedes_quotes_only_where_the_book_carries_them() {
    assert_eq!(feed(true).narrow(&Stream::ALL), vec![Stream::Trades, Stream::Book, Stream::Depth]);
    assert_eq!(feed(false).narrow(&Stream::ALL), Stream::ALL.to_vec());
}

/// Without `Book`, the cheaper quote-only pump is exactly what was asked for.
#[test]
fn quotes_alone_survive_without_a_book() {
    assert_eq!(
        feed(true).narrow(&[Stream::Quotes, Stream::Trades]),
        vec![Stream::Quotes, Stream::Trades]
    );
}
