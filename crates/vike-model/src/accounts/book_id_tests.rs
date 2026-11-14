use super::BookId;

/// The normalisation is the old `normalize_book`'s, byte for byte: `str::trim`, then ASCII lowercase.
#[test]
fn a_book_is_trimmed_and_lowercased_once() {
    let id = BookId::new("ibkr", "  DUQ186573\t").expect("a book");
    assert_eq!(id.book(), "duq186573");
    assert_eq!(id.venue(), "ibkr");
    assert_eq!(BookId::new("ibkr", "duq186573"), Some(id), "case and blanks are not identity");
    // Non-ASCII letters keep their case: the old rule lowercased ASCII only.
    assert_eq!(BookId::new("ig", "ÄBC").expect("a book").book(), "Äbc");
}

/// A blank book is no book: it would compare equal to every other unknown.
#[test]
fn a_blank_book_is_none() {
    for raw in ["", "   ", "\t\n"] {
        assert_eq!(BookId::new("hyperliquid", raw), None, "`{raw}`");
    }
}

/// The venue is part of the identity: one text at two venues is two books.
#[test]
fn the_same_text_at_two_venues_is_two_books() {
    assert_ne!(BookId::new("binance", "123"), BookId::new("bybit", "123"));
}
