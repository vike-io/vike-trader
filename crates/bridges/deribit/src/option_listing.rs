//! **The options the venue LISTS, read from its own instrument rows**: which underlyings the options
//! surface serves, which book each one's chain is read from, and the `markprice.options` index of that
//! book. Nothing here is a table. Every term comes from one `public/get_instruments` read
//! (`currency=any&kind=option`), so a new underlying shows up, and a delisted one goes, at the next
//! read: [`crate::feed::options::chain::DeribitOptionsProvider::list_underlyings`] holds the list for
//! [`LISTING_TTL_MS`] and reads it again after that.
//!
//! # The rule, per underlying (a row's `base_currency`)
//!
//! - Its BOOK is its own coin when the venue lists coin-settled options on it
//!   (`settlement_currency == base_currency`: BTC, ETH), and otherwise the currency its options settle
//!   in (`USDC` for SOL and every other altcoin). An underlying listed in both (BTC also has linear
//!   `BTC_USDC-…` options) is served from its coin book, as the surface always served it; its linear
//!   options are not part of this surface. Two non-coin books (none today) take the alphabetically
//!   first.
//! - Its INDEX is the `price_index` the rows of that book publish (`btc_usd`, `sol_usdc`), the name the
//!   venue's `markprice.options.{index}` channel takes. Rows of one book naming two indexes (none
//!   today) take the alphabetically first, with one warn.
//! - A row that is inactive, not an option, or missing one of the three fields is skipped (counted, one
//!   warn per read), never guessed.
//!
//! The order is the listing's own: the underlying with the most listed options first, a tie by name
//! (`vike_options::underlyings_by_listed_options`; no symbol is ranked by its spelling), the order a
//! client's selector shows.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// How long a listing is served before it is read again: one hour. A new underlying or a delisted one
/// is a rare event; a user who will not wait asks for a refresh
/// ([`crate::feed::options::chain::DeribitOptionsProvider::refresh_underlyings`]).
pub const LISTING_TTL_MS: i64 = 60 * 60 * 1_000;

/// The shortest gap between two listing reads, whatever asks: a refresh asked sooner, or a retry after
/// a failed read, answers the list already held. The read is the venue's whole option universe (some
/// 4.6 MB), so no client may make this box ask for it faster than this.
pub const LISTING_MIN_GAP_MS: i64 = 60 * 1_000;

/// One underlying the venue lists options on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionUnderlying {
    /// The `base_currency`, upper-cased: `BTC`, `SOL`.
    pub name: String,
    /// The book-summary `currency` its options are read from: `BTC`, or `USDC` for SOL.
    pub book: String,
    /// The `price_index` of that book's options: `btc_usd`, `sol_usdc`.
    pub index: String,
}

impl OptionUnderlying {
    /// Whether its book quotes premiums in the settlement currency (a linear USDC book) rather than in
    /// coin units (a coin book, scaled to USD by the underlying price).
    pub fn usd_quoted(&self) -> bool {
        self.book != self.name
    }
}

/// The underlyings `rows` (one `public/get_instruments` answer's `result`) list options on, by the rule
/// of the module doc, in the listing's order.
pub fn option_underlyings(rows: &[Value]) -> Vec<OptionUnderlying> {
    // base -> book (settlement currency) -> the indexes that book's rows name
    let mut books: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    // One base per listed option: what the order counts.
    let mut bases: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    for row in rows {
        if row.get("is_active").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        if row.get("kind").and_then(Value::as_str) != Some("option") {
            continue;
        }
        let field = |key: &str| row.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());
        let (Some(base), Some(settle), Some(index)) =
            (field("base_currency"), field("settlement_currency"), field("price_index"))
        else {
            skipped += 1;
            continue;
        };
        let base = base.to_ascii_uppercase();
        books
            .entry(base.clone())
            .or_default()
            .entry(settle.to_ascii_uppercase())
            .or_default()
            .insert(index.to_string());
        bases.push(base);
    }
    if skipped > 0 {
        tracing::warn!(
            target: "vike_deribit::option_listing",
            skipped,
            "deribit option rows lacking a base currency, a settlement currency or a price index \
             were left out of the listing"
        );
    }
    vike_options::underlyings_by_listed_options(bases.iter().map(String::as_str))
        .into_iter()
        .filter_map(|name| {
            let by_book = books.get(&name)?;
            let (book, indexes) = match by_book.get_key_value(&name) {
                Some(own) => own,
                None => by_book.iter().next()?,
            };
            if indexes.len() > 1 {
                tracing::warn!(
                    target: "vike_deribit::option_listing",
                    underlying = %name,
                    book = %book,
                    ?indexes,
                    "deribit lists one book's options under two price indexes; the first is used"
                );
            }
            let index = indexes.iter().next()?.clone();
            Some(OptionUnderlying { book: book.clone(), index, name })
        })
        .collect()
}

#[cfg(test)]
#[path = "option_listing_tests.rs"]
mod option_listing_tests;
