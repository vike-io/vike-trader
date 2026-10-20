//! The Trade window's catalog answers (I13): the listing, the spellings and the picker's rows.

use std::sync::{Arc, Weak};

use vike_catalog::{Catalog, SearchFilter, kind_of, picker_flat};
use vike_model::AssetClass;
use vike_panels::trade::Grid;

/// How many symbol-picker lines are kept for one query. The picker searches EVERY venue the catalog
/// lists (it used to search only the venues the server runs, which is why it showed one), spot and
/// perpetual both, ranked; the widget applies its switch and draws the first lines of what is left,
/// so this holds enough of each kind that the switch never empties a list the query could fill.
pub(super) const ROWS_CACHED: usize = 60;

/// Split a venue symbol into `(base, quote)` for the size-field units, for a symbol the catalog
/// does not list — longest quote suffix first (`USDT`/`USDC` before `USD`, so `BTCUSDT` is not read
/// as `BTCUSD` + `T`). A symbol with no known quote suffix is all base and defaults to a `USDT`
/// quote; a symbol that is ONLY a quote suffix (`"USDT"`) keeps the whole symbol as base rather than
/// yielding an empty label. Such a symbol has no lot size (I18), so these words label a size field
/// that refuses every size.
pub(crate) fn split_base_quote(symbol: &str) -> (&str, &str) {
    let (base, quote) = if let Some(b) = symbol.strip_suffix("USDT") {
        (b, "USDT")
    } else if let Some(b) = symbol.strip_suffix("USDC") {
        (b, "USDC")
    } else if let Some(b) = symbol.strip_suffix("USD") {
        (b, "USD")
    } else {
        (symbol, "USDT")
    };
    (if base.is_empty() { symbol } else { base }, quote)
}

/// The words for an instrument the window has no lot size for (I18). Never a size refusal with a
/// `(0)` in it: the trader is told what is missing and where it comes from.
pub(super) fn no_lot_why(symbol: &str, listed: bool, loaded: bool) -> String {
    if !loaded {
        format!("No lot size for {symbol} yet: the instrument catalog has not loaded.")
    } else if !listed {
        format!(
            "No lot size for {symbol}: the instrument catalog does not list it. Refresh it in \
             Data Manager → Instruments."
        )
    } else {
        format!(
            "No lot size for {symbol}: the instrument catalog lists it without one. Refresh it in \
             Data Manager → Instruments."
        )
    }
}

/// What the catalog lists for the window's instrument.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Listing {
    pub(super) grid: Grid,
    base: String,
    quote: String,
    pub(super) class: AssetClass,
}

/// One symbol-picker row as the catalog lists it: one instrument on one venue.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TradeRow {
    pub(super) venue: String,
    pub(super) symbol: String,
    /// The pair and kind in words (`BTC/USDT perpetual`).
    pub(super) pair: String,
}

/// **One window's catalog answers (I13)**, each kept with the key it was computed for, and all of
/// them with the catalog they were read from.
///
/// ⚠ The catalog is identified by its `Arc` (the shell replaces the `Arc` on every refresh), held
/// as a `Weak`: a `Weak` keeps the allocation reserved, so a later catalog can never be allocated at
/// the same address and pass for the one these answers came from. It does not keep the catalog's
/// instruments alive.
#[derive(Clone, Default)]
pub struct TradeCatalog {
    from: Weak<Catalog>,
    /// Whether that catalog held any instrument at all: an empty one has not loaded yet.
    pub(super) loaded: bool,
    /// `(venue, symbol)` the listing below was looked up for.
    listing_for: Option<(String, String)>,
    pub(super) listing: Option<Listing>,
    /// `(base, quote, class)` the spellings below were looked up for. The CLASS is part of the key
    /// because the spellings prefer it: a window moved from a venue's swap to its spot pair keeps
    /// its base and quote, and must stop opening another venue's perp.
    spellings_for: Option<(String, String, Option<AssetClass>)>,
    /// `(venue, venue-native symbol)`: the instrument's spelling on every venue that lists its
    /// base, one per venue, the same quote and class preferred.
    spellings: Vec<(String, String, AssetClass)>,
    /// The query the rows below were searched for.
    rows_for: Option<String>,
    /// The symbol picker's rows for that query, on every venue the catalog lists, best first.
    pub(super) rows: Vec<TradeRow>,
}

impl TradeCatalog {
    /// Bring the answers in line with this frame's catalog, instrument and picker query, scanning
    /// the catalog only for what changed.
    pub fn sync(&mut self, catalog: &Arc<Catalog>, venue: &str, symbol: &str, query: &str) {
        if !std::ptr::eq(self.from.as_ptr(), Arc::as_ptr(catalog)) {
            *self = TradeCatalog {
                from: Arc::downgrade(catalog),
                loaded: !catalog.is_empty(),
                ..TradeCatalog::default()
            };
        }
        if self.listing_for.as_ref().is_none_or(|(v, s)| v != venue || s != symbol) {
            self.listing = listing(catalog, venue, symbol);
            self.listing_for = Some((venue.to_string(), symbol.to_string()));
        }
        // The listing's own fields, not `base_quote`: a method call would hold all of `self` while
        // the spellings below are written.
        let (base, quote) = match &self.listing {
            Some(l) => (l.base.as_str(), l.quote.as_str()),
            None => split_base_quote(symbol),
        };
        let class = self.listing.as_ref().map(|l| l.class);
        if self
            .spellings_for
            .as_ref()
            .is_none_or(|(b, q, c)| b != base || q != quote || *c != class)
        {
            self.spellings = spellings(catalog, base, quote, class);
            self.spellings_for = Some((base.to_string(), quote.to_string(), class));
        }
        let query = query.trim();
        if self.rows_for.as_deref() != Some(query) {
            self.rows = picker_rows(catalog, query);
            self.rows_for = Some(query.to_string());
        }
    }

    /// The window instrument's base and quote: the catalog's, else read off the symbol.
    pub(super) fn base_quote<'s>(&'s self, symbol: &'s str) -> (&'s str, &'s str) {
        match &self.listing {
            Some(l) => (l.base.as_str(), l.quote.as_str()),
            None => split_base_quote(symbol),
        }
    }

    /// The instrument's spelling on `venue`, or `None` when the catalog lists its base nowhere
    /// there.
    pub(super) fn spelling_on(&self, venue: &str) -> Option<&str> {
        self.spellings
            .iter()
            .find(|(v, ..)| v.eq_ignore_ascii_case(venue))
            .map(|(_, s, _)| s.as_str())
    }

    /// The product the instrument is on `venue` (`Perp`, `Spot`), as that venue's spelling of it
    /// is classed; empty where the catalog lists it nowhere there.
    pub(super) fn product_on(&self, venue: &str) -> &'static str {
        self.spellings
            .iter()
            .find(|(v, ..)| v.eq_ignore_ascii_case(venue))
            .and_then(|(_, _, class)| kind_of(*class))
            .map_or("", vike_catalog::Kind::product)
    }
}

/// What the catalog lists under `symbol` on `venue`: an EXACT lookup over the venue's whole list.
///
/// ⚠ Not the ranked `Catalog::search` with the symbol as the query: its tiers compare the raw symbol
/// with the UPPER-CASED query, so a mixed-case raw symbol (hyperliquid's `kPEPE`) would never be
/// found, and the window would say the catalog does not list it and refuse every size. The empty
/// query returns the venue's list in catalog order. The answer is cached ([`TradeCatalog`]).
fn listing(catalog: &Catalog, venue: &str, symbol: &str) -> Option<Listing> {
    let filter = SearchFilter { tab: None, venue: Some(venue.to_string()) };
    catalog.search("", &filter, usize::MAX).into_iter().find(|i| i.raw_symbol == symbol).map(|i| {
        Listing {
            grid: Grid {
                tick: i.properties.tick_size,
                lot: i.properties.step_size,
                min_qty: i.properties.min_qty,
            },
            base: i.base.clone(),
            quote: i.quote.clone(),
            class: i.asset_class(),
        }
    })
}

/// The instrument `(base, quote)` as every venue that lists `base` spells it: one symbol per venue,
/// preferring the same quote, then the same asset class, then the catalog's order. The whole list is
/// read (the empty query), for the reason [`listing`] gives: a ranked search would miss a
/// mixed-case base.
fn spellings(
    catalog: &Catalog,
    base: &str,
    quote: &str,
    class: Option<AssetClass>,
) -> Vec<(String, String, AssetClass)> {
    let mut best: Vec<(String, String, AssetClass, u8)> = Vec::new();
    for i in catalog.search("", &SearchFilter::default(), usize::MAX) {
        if !i.base.eq_ignore_ascii_case(base) {
            continue;
        }
        let score = 2 * u8::from(i.quote.eq_ignore_ascii_case(quote))
            + u8::from(class == Some(i.asset_class()));
        match best.iter().position(|(v, ..)| v.eq_ignore_ascii_case(&i.venue)) {
            None => best.push((i.venue.clone(), i.raw_symbol.clone(), i.asset_class(), score)),
            Some(k) if score > best[k].3 => {
                best[k] = (i.venue.clone(), i.raw_symbol.clone(), i.asset_class(), score);
            }
            Some(_) => {}
        }
    }
    best.into_iter().map(|(v, s, c, _)| (v, s, c)).collect()
}

/// The symbol picker's rows for `query`: spot and perpetual on every venue the catalog lists, ranked
/// (`vike_catalog::picker_flat`), one row per instrument. The picker used to search only the venues
/// the server runs an engine on, so a venue the trader could not trade yet — but could look at, and
/// move to — never appeared in it.
fn picker_rows(catalog: &Catalog, query: &str) -> Vec<TradeRow> {
    picker_flat(catalog, query, ROWS_CACHED)
        .into_iter()
        .filter_map(|i| {
            let kind = kind_of(i.asset_class())?;
            Some(TradeRow {
                venue: i.venue.clone(),
                symbol: i.raw_symbol.clone(),
                pair: format!("{}/{} {}", i.base, i.quote, kind.label()),
            })
        })
        .collect()
}
