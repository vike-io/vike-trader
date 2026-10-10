//! Spot and perpetual instruments, grouped by what they are an instrument OF — the model behind the
//! symbol pickers' "one line per underlying, a chip per venue" (design:
//! `docs/superpowers/specs/2026-10-04-symbol-search-design.md`).
//!
//! # What a line is
//!
//! A line is `(base, kind, quote)` where the quote folds the dollar quotes (`USD`, `USDT`, `USDC`,
//! `BUSD`, `FDUSD`, `TUSD`) into one: Deribit's `BTC-PERPETUAL` (BTC/USD) and Binance's
//! `BTCUSDT.P` (BTC/USDT) are the same bet on the same coin, and a trader moving a window from one
//! venue to the other wants exactly that pairing. Any other quote stays apart, so `ETH/BTC` is not
//! an `ETH` line. The catalog has no cross-venue identity of its own, and this is the cheapest
//! honest one `Instrument`'s own fields (`base`, `quote`, `asset_class`) can carry.
//!
//! # Scope
//!
//! Spot, perpetual, forex, CFD and stocks ([`kind_of`]). Options are traded in the Options window, and dated
//! futures, forex, stocks, indices and prediction markets are later tabs; [`kind_of`] is an
//! exhaustive `match`, so a new asset class forces its author to decide whether a picker offers it.
//!
//! # Ranking, and why "the best listing" is the first one
//!
//! [`group_by_underlying`] takes an already RANKED slice (`Catalog::search`'s order) and keeps
//! first-appearance order for lines and for venues, so the listing a venue's chip opens is the one
//! that ranked best for the query: typing `BTCUSDC.P` opens that instrument, typing `btc` opens the
//! USDT one. A venue with several listings in one line keeps one chip and counts the rest in
//! [`Listing::others`], which the chip's hover names — nothing is hidden silently.
//!
//! Cost: [`picker_results`] is one ranked pass over the whole catalog. Callers memoise it on
//! `(text, kind, venues, catalog)`; nothing here caches.

use std::collections::HashMap;

use vike_model::AssetClass;

use crate::{Catalog, Instrument, SearchFilter};

/// The kinds the pickers offer: spot and perpetual (crypto), then forex, CFD and stocks (the owner
/// widened the scope on 2026-10-04, "extend to forex and cfd and stocks"). Options and dated futures
/// stay in their own windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Spot,
    Perp,
    Forex,
    Cfd,
    Stock,
}

impl Kind {
    /// The word a line shows for its kind.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Kind::Spot => "spot",
            Kind::Perp => "perpetual",
            Kind::Forex => "forex",
            Kind::Cfd => "cfd",
            Kind::Stock => "stock",
        }
    }

    /// The product's name as a venue's row says it beside the venue (`Binance Perp`, `Deribit
    /// Spot`): the Trade window's account menu and its header button spell an account's product so.
    #[must_use]
    pub fn product(self) -> &'static str {
        match self {
            Kind::Spot => "Spot",
            Kind::Perp => "Perp",
            Kind::Forex => "Forex",
            Kind::Cfd => "CFD",
            Kind::Stock => "Stock",
        }
    }

    /// Whether the kind is a crypto one, whose dollar quotes fold into one ([`quote_group`]). A
    /// forex pair's quote is part of WHAT it is (EUR/USD is not EUR/USDT), and a stock's quote is its
    /// currency.
    #[must_use]
    fn folds_dollar_quotes(self) -> bool {
        matches!(self, Kind::Spot | Kind::Perp)
    }
}

/// The kind a picker offers for `class`, `None` for everything it does not. Exhaustive on purpose.
#[must_use]
pub fn kind_of(class: AssetClass) -> Option<Kind> {
    match class {
        AssetClass::CryptoSpot => Some(Kind::Spot),
        AssetClass::CryptoPerp => Some(Kind::Perp),
        AssetClass::Fx => Some(Kind::Forex),
        AssetClass::Cfd => Some(Kind::Cfd),
        AssetClass::Equity | AssetClass::Etf => Some(Kind::Stock),
        AssetClass::CryptoFuture
        | AssetClass::Option
        | AssetClass::Future
        | AssetClass::Index
        | AssetClass::PredictionMarket => None,
    }
}

/// Quotes that are one dollar for the purpose of "the same instrument on another venue".
const DOLLAR_QUOTES: [&str; 6] = ["USD", "USDT", "USDC", "BUSD", "FDUSD", "TUSD"];

/// The quote a line is keyed on: for a crypto kind, `"USD"` for every dollar quote; for any other
/// kind the quote as listed. Upper-cased.
fn quote_group(kind: Kind, quote: &str) -> String {
    let q = quote.to_ascii_uppercase();
    if kind.folds_dollar_quotes() && DOLLAR_QUOTES.contains(&q.as_str()) {
        "USD".to_string()
    } else {
        q
    }
}

/// One venue's listing in a line.
#[derive(Clone, Debug)]
pub struct Listing<'a> {
    pub instrument: &'a Instrument,
    /// How many MORE listings this venue has in the same line (its chip's hover names them).
    pub others: usize,
}

/// One line: an underlying and the venues that list it.
#[derive(Clone, Debug)]
pub struct Underlying<'a> {
    pub base: &'a str,
    /// The folded quote: `"USD"` for every dollar quote, else the quote as listed, upper-cased.
    pub quote: String,
    pub kind: Kind,
    /// One per venue, in the order the venues first appear in the ranked input.
    pub listings: Vec<Listing<'a>>,
}

impl Underlying<'_> {
    /// `BTC` for a crypto line on a dollar quote, `AAPL` for a stock, and `ETH/BTC` or `EUR/USD`
    /// where the quote is part of what the line is.
    #[must_use]
    pub fn heading(&self) -> String {
        let bare = match self.kind {
            Kind::Spot | Kind::Perp => self.quote == "USD",
            Kind::Stock => true,
            Kind::Forex | Kind::Cfd => false,
        };
        if bare { self.base.to_string() } else { format!("{}/{}", self.base, self.quote) }
    }
}

/// Group an already RANKED slice into lines. Lines and venues keep first-appearance order, so the
/// listing kept per venue is the best-ranked one. Instruments outside [`kind_of`] are dropped.
#[must_use]
pub fn group_by_underlying<'a>(ranked: &[&'a Instrument]) -> Vec<Underlying<'a>> {
    let mut groups: Vec<Underlying<'a>> = Vec::new();
    let mut at: HashMap<(String, String, Kind), usize> = HashMap::new();
    for &i in ranked {
        let Some(kind) = kind_of(i.asset_class) else { continue };
        let quote = quote_group(kind, &i.quote);
        let key = (i.base.to_ascii_uppercase(), quote.clone(), kind);
        let n = *at.entry(key).or_insert_with(|| {
            groups.push(Underlying { base: &i.base, quote, kind, listings: Vec::new() });
            groups.len() - 1
        });
        let group = &mut groups[n];
        match group.listings.iter_mut().find(|l| l.instrument.venue.eq_ignore_ascii_case(&i.venue))
        {
            Some(l) => l.others += 1,
            None => group.listings.push(Listing { instrument: i, others: 0 }),
        }
    }
    groups
}

/// What a picker asks the catalog.
#[derive(Clone, Copy, Debug)]
pub struct PickerQuery<'q> {
    pub text: &'q str,
    /// `None` is "All": spot and perpetual both.
    pub kind: Option<Kind>,
    /// Venue keys to keep; empty keeps every venue.
    pub venues: &'q [String],
}

/// Search, keep spot and perpetual, apply the kind switch and the venue filter, group, cap at
/// `max_lines`. An empty query has nothing to rank by, so the line listed on the most venues comes
/// first (stable: ties keep catalog order).
#[must_use]
pub fn picker_results<'a>(
    catalog: &'a Catalog,
    q: &PickerQuery<'_>,
    max_lines: usize,
) -> Vec<Underlying<'a>> {
    let hits = catalog.search(q.text, &SearchFilter::default(), usize::MAX);
    let mut kept: Vec<&Instrument> = hits
        .into_iter()
        .filter(|i| kind_of(i.asset_class).is_some_and(|k| q.kind.is_none_or(|want| want == k)))
        .filter(|i| {
            q.venues.is_empty() || q.venues.iter().any(|v| v.eq_ignore_ascii_case(&i.venue))
        })
        .collect();
    if q.text.trim().is_empty() {
        // Nothing ranked these (the catalog's order is the venue's own), and the listing a venue's
        // chip opens is the first it holds in a line: a venue with a USDC and an inverse perpetual
        // beside its USDT one opened the USDC or the inverse one (measured live, 2026-10-04: Bybit's
        // `BTCPERP.P`, OKX's `ETH-USD-SWAP`). The dollar quote a trader means by default goes first
        // — the same preference `Catalog::search` breaks ties with. Stable, so ties keep order.
        kept.sort_by_key(|i| crate::catalog::quote_rank(&i.quote));
    }
    let mut groups = group_by_underlying(&kept);
    if q.text.trim().is_empty() {
        groups.sort_by_key(|g| std::cmp::Reverse(g.listings.len()));
    }
    groups.truncate(max_lines);
    groups
}

/// Spot and perpetual listings per venue, in first-seen order: what the picker's source chips
/// show as loaded.
#[must_use]
pub fn venue_counts(catalog: &Catalog) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for i in catalog.search("", &SearchFilter::default(), usize::MAX) {
        if kind_of(i.asset_class).is_none() {
            continue;
        }
        match out.iter_mut().find(|(v, _)| v.eq_ignore_ascii_case(&i.venue)) {
            Some((_, n)) => *n += 1,
            None => out.push((i.venue.clone(), 1)),
        }
    }
    out
}

/// The Trade picker's FLAT list (the owner's v3 design): every spot and perpetual instrument that
/// matches `text`, one row per INSTRUMENT, capped at `max`. A line of venue chips overflows its row
/// once a symbol is on five venues, and a flat list never does.
///
/// With a query the order is `Catalog::search`'s own — exact, prefix, base, substring, the dollar
/// quote on a tie — and every matching instrument is a row, so two instruments of one venue are two
/// rows. With no query nothing ranks them, so the underlying listed on the most venues comes first
/// with its venues together, the dollar quote first within a venue, as [`picker_results`] orders it.
#[must_use]
pub fn picker_flat<'a>(catalog: &'a Catalog, text: &str, max: usize) -> Vec<&'a Instrument> {
    let hits = catalog.search(text, &SearchFilter::default(), usize::MAX);
    let mut kept: Vec<&Instrument> =
        hits.into_iter().filter(|i| kind_of(i.asset_class).is_some()).collect();
    if text.trim().is_empty() {
        kept.sort_by_key(|i| crate::catalog::quote_rank(&i.quote));
        let mut groups = group_by_underlying(&kept);
        groups.sort_by_key(|g| std::cmp::Reverse(g.listings.len()));
        return groups
            .into_iter()
            .flat_map(|g| g.listings.into_iter().map(|l| l.instrument))
            .take(max)
            .collect();
    }
    kept.truncate(max);
    kept
}

#[path = "underlying_tests.rs"]
#[cfg(test)]
mod underlying_tests;
