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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::AssetClass::{CryptoFuture, CryptoPerp, CryptoSpot, Equity, Option as Opt};

    fn inst(venue: &str, sym: &str, base: &str, quote: &str, class: AssetClass) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: sym.into(),
            asset_class: class,
            base: base.into(),
            quote: quote.into(),
            description: String::new(),
            properties: Default::default(),
            contract_type: None,
            settle_asset: None,
        }
    }

    #[test]
    fn the_offered_kinds_are_spot_perp_forex_cfd_and_stocks_and_nothing_else() {
        assert_eq!(kind_of(CryptoSpot), Some(Kind::Spot));
        assert_eq!(kind_of(CryptoPerp), Some(Kind::Perp));
        assert_eq!(kind_of(AssetClass::Fx), Some(Kind::Forex));
        assert_eq!(kind_of(AssetClass::Cfd), Some(Kind::Cfd));
        assert_eq!(kind_of(Equity), Some(Kind::Stock));
        assert_eq!(kind_of(AssetClass::Etf), Some(Kind::Stock));
        for c in
            [CryptoFuture, Opt, AssetClass::Future, AssetClass::Index, AssetClass::PredictionMarket]
        {
            assert_eq!(
                kind_of(c),
                None,
                "{c:?} must not be offered (options and futures have their own windows, polymarket its cockpit)"
            );
        }
    }

    /// A forex pair keeps its quote (EUR/USD is not EUR/USDT), a stock is its ticker alone, and the
    /// dollar fold stays a crypto thing.
    #[test]
    fn forex_and_stocks_group_on_their_own_terms() {
        let all = [
            inst("dukascopy", "EURUSD", "EUR", "USD", AssetClass::Fx),
            inst("oanda", "EUR_USD", "EUR", "USD", AssetClass::Fx),
            inst("oanda", "EUR_GBP", "EUR", "GBP", AssetClass::Fx),
            inst("alpaca", "AAPL", "AAPL", "USD", Equity),
            inst("ibkr", "AAPL", "AAPL", "USD", Equity),
            inst("ig", "US500", "US500", "USD", AssetClass::Cfd),
        ];
        let refs: Vec<&Instrument> = all.iter().collect();
        let g = group_by_underlying(&refs);
        let heads: Vec<(String, Kind, usize)> =
            g.iter().map(|u| (u.heading(), u.kind, u.listings.len())).collect();
        assert_eq!(
            heads,
            [
                ("EUR/USD".to_string(), Kind::Forex, 2),
                ("EUR/GBP".to_string(), Kind::Forex, 1),
                ("AAPL".to_string(), Kind::Stock, 2),
                ("US500/USD".to_string(), Kind::Cfd, 1),
            ]
        );
    }

    #[test]
    fn dollar_quotes_fold_into_one_line_and_other_quotes_do_not() {
        let all = [
            inst("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
            inst("deribit", "BTC-PERPETUAL", "BTC", "USD", CryptoPerp),
            inst("okx", "BTC-USDT-SWAP", "BTC", "USDT", CryptoPerp),
            inst("binance", "ETHBTC", "ETH", "BTC", CryptoSpot),
            inst("binance", "ETHUSDT", "ETH", "USDT", CryptoSpot),
        ];
        let refs: Vec<&Instrument> = all.iter().collect();
        let g = group_by_underlying(&refs);
        assert_eq!(g.len(), 3);
        assert_eq!((g[0].heading(), g[0].kind, g[0].listings.len()), ("BTC".into(), Kind::Perp, 3));
        assert_eq!(g[1].heading(), "ETH/BTC");
        assert_eq!(g[2].heading(), "ETH");
    }

    #[test]
    fn a_venue_with_two_listings_keeps_the_best_ranked_and_counts_the_rest() {
        let cat = Catalog::from_instruments(vec![
            inst("binance", "BTCUSDC.P", "BTC", "USDC", CryptoPerp),
            inst("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        ]);
        let q = PickerQuery { text: "btc", kind: None, venues: &[] };
        let g = picker_results(&cat, &q, 10);
        assert_eq!(g.len(), 1);
        // USDT outranks USDC on a tie, whatever the catalog order.
        assert_eq!(g[0].listings[0].instrument.raw_symbol, "BTCUSDT.P");
        assert_eq!(g[0].listings[0].others, 1);
        // The exact symbol wins outright, and nothing else matches it.
        let q = PickerQuery { text: "btcusdc.p", kind: None, venues: &[] };
        let g = picker_results(&cat, &q, 10);
        assert_eq!(g[0].listings[0].instrument.raw_symbol, "BTCUSDC.P");
        assert_eq!(g[0].listings[0].others, 0);
    }

    #[test]
    fn options_dated_futures_and_equities_are_never_offered() {
        let cat = Catalog::from_instruments(vec![
            inst("deribit", "BTC-27NOV26-58000-C", "BTC", "BTC", Opt),
            inst("deribit", "BTC-27NOV26", "BTC", "USD", CryptoFuture),
            inst("ibkr", "ESZ6", "ES", "USD", AssetClass::Future),
            inst("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        ]);
        let q = PickerQuery { text: "", kind: None, venues: &[] };
        let g = picker_results(&cat, &q, 10);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].listings[0].instrument.venue, "bybit");
    }

    #[test]
    fn the_kind_switch_and_the_venue_filter_narrow_the_results() {
        let cat = Catalog::from_instruments(vec![
            inst("binance", "BTCUSDT", "BTC", "USDT", CryptoSpot),
            inst("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
            inst("okx", "BTC-USDT", "BTC", "USDT", CryptoSpot),
        ]);
        let only_perp = PickerQuery { text: "btc", kind: Some(Kind::Perp), venues: &[] };
        assert_eq!(picker_results(&cat, &only_perp, 10).len(), 1);
        let okx = ["okx".to_string()];
        let only_okx = PickerQuery { text: "btc", kind: None, venues: &okx };
        let g = picker_results(&cat, &only_okx, 10);
        assert!(!g.is_empty());
        assert!(g.iter().all(|u| u.listings.iter().all(|l| l.instrument.venue == "okx")));
    }

    #[test]
    fn an_empty_query_lists_the_widest_underlying_first() {
        let cat = Catalog::from_instruments(vec![
            inst("binance", "XRPUSDT.P", "XRP", "USDT", CryptoPerp),
            inst("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
            inst("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        ]);
        let g = picker_results(&cat, &PickerQuery { text: "", kind: None, venues: &[] }, 10);
        assert_eq!(g[0].heading(), "BTC");
    }

    /// The flat list is one row per matching INSTRUMENT, ranked as a search ranks, spot and
    /// perpetual only — two instruments of one venue are two rows.
    #[test]
    fn the_flat_list_has_a_row_per_instrument_and_only_spot_and_perpetual() {
        let cat = Catalog::from_instruments(vec![
            inst("binance", "BTCUSDT", "BTC", "USDT", CryptoSpot),
            inst("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
            inst("binance", "BTCUSDC.P", "BTC", "USDC", CryptoPerp),
            inst("deribit", "BTC-27NOV26", "BTC", "USD", CryptoFuture),
            inst("deribit", "BTC-27NOV26-58000-C", "BTC", "BTC", Opt),
            inst("okx", "BTC-USDT-SWAP", "BTC", "USDT", CryptoPerp),
        ]);
        let rows = picker_flat(&cat, "btc", 10);
        let syms: Vec<&str> = rows.iter().map(|i| i.raw_symbol.as_str()).collect();
        assert_eq!(syms.len(), 4, "{syms:?}");
        assert!(syms.contains(&"BTCUSDT.P") && syms.contains(&"BTCUSDC.P"), "{syms:?}");
        assert!(!syms.iter().any(|s| s.contains("27NOV26")), "futures and options never");
        assert_eq!(picker_flat(&cat, "btc", 2).len(), 2, "the cap applies");
    }

    /// With no query the underlying on the most venues leads, its venues together.
    #[test]
    fn an_empty_flat_query_leads_with_the_widest_underlying() {
        let cat = Catalog::from_instruments(vec![
            inst("binance", "XRPUSDT.P", "XRP", "USDT", CryptoPerp),
            inst("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
            inst("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        ]);
        let rows = picker_flat(&cat, "", 10);
        let venues: Vec<(&str, &str)> =
            rows.iter().map(|i| (i.base.as_str(), i.venue.as_str())).collect();
        assert_eq!(venues, [("BTC", "binance"), ("BTC", "bybit"), ("XRP", "binance")]);
    }
    /// With no query to rank by, a venue's chip opens its USDT listing, not whichever the venue
    /// lists first: Bybit lists a USDC perpetual and an inverse one beside the USDT one.
    #[test]
    fn an_empty_query_opens_each_venues_usdt_listing_first() {
        let cat = Catalog::from_instruments(vec![
            inst("bybit", "BTCPERP.P", "BTC", "USDC", CryptoPerp),
            inst("bybit", "BTCUSD.P", "BTC", "USD", CryptoPerp),
            inst("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        ]);
        let g = picker_results(&cat, &PickerQuery { text: "", kind: None, venues: &[] }, 10);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].listings[0].instrument.raw_symbol, "BTCUSDT.P");
        assert_eq!(g[0].listings[0].others, 2);
    }

    #[test]
    fn the_line_cap_applies_after_grouping() {
        let cat = Catalog::from_instruments(vec![
            inst("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
            inst("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
            inst("binance", "ETHUSDT.P", "ETH", "USDT", CryptoPerp),
        ]);
        let g = picker_results(&cat, &PickerQuery { text: "", kind: None, venues: &[] }, 1);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].listings.len(), 2, "a cap on LINES must not drop a line's venues");
    }

    #[test]
    fn venue_counts_counts_spot_and_perpetual_only_in_first_seen_order() {
        let cat = Catalog::from_instruments(vec![
            inst("okx", "BTC-USDT", "BTC", "USDT", CryptoSpot),
            inst("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
            inst("okx", "BTC-27NOV26", "BTC", "USD", CryptoFuture),
            inst("okx", "ETH-USDT", "ETH", "USDT", CryptoSpot),
        ]);
        assert_eq!(venue_counts(&cat), vec![("okx".to_string(), 2), ("binance".to_string(), 1)]);
    }
}
