//! The Trade window's catalog answers (I13): the listing, the spellings and the picker's rows.

use std::sync::{Arc, Weak};

use vike_catalog::{Catalog, CatalogSearchFilter, kind_of, picker_flat, same_instrument_per_venue};
use vike_model::{AssetClass, Grid};
use vike_order_entry::trade_window::grid_value;

/// How many symbol-picker lines are kept for one query. The picker searches EVERY venue the catalog
/// lists (it used to search only the venues the server runs, which is why it showed one), spot and
/// perpetual both, ranked; the widget applies its switch and draws the first lines of what is left,
/// so this holds enough of each kind that the switch never empties a list the query could fill.
pub(super) const ROWS_CACHED: usize = 60;

/// The quote-side unit word of an instrument the catalog does not list: no quote currency is read
/// off its spelling (the owner's ruling of 2026-11-13, no per-symbol facts in code), so the size
/// fields say `quote` rather than guess one.
pub(crate) const UNLISTED_QUOTE: &str = "quote";

/// The size-field units `(base, quote)` of a symbol the catalog does not list: the whole symbol is
/// the base and the quote is [`UNLISTED_QUOTE`]. Such a symbol has no lot size (I18), so these
/// words label a size field that refuses every size.
pub(crate) fn unlisted_base_quote(symbol: &str) -> (&str, &str) {
    (symbol, UNLISTED_QUOTE)
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
pub(super) struct TradeWindowCatalogListing {
    pub(super) grid: Grid,
    base: String,
    quote: String,
    pub(super) class: AssetClass,
}

/// One symbol-picker row as the catalog lists it: one instrument on one venue.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TradeWindowCatalogRow {
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
pub struct TradeWindowCatalog {
    from: Weak<Catalog>,
    /// Whether that catalog held any instrument at all: an empty one has not loaded yet.
    pub(super) loaded: bool,
    /// `(venue, symbol)` the listing below was looked up for.
    listing_for: Option<(String, String)>,
    pub(super) listing: Option<TradeWindowCatalogListing>,
    /// `(base, quote, class)` the spellings below were looked up for. The CLASS is part of the key
    /// because the spellings prefer it: a window moved from a venue's swap to its spot pair keeps
    /// its base and quote, and must stop opening another venue's perp.
    spellings_for: Option<(String, String, Option<AssetClass>)>,
    /// `(venue, venue-native symbol)`: the instrument's spelling on every venue that lists its
    /// base, one per venue, the pickers' line for it preferred (`vike_catalog::same_instrument_per_venue`).
    spellings: Vec<(String, String, AssetClass)>,
    /// The query the rows below were searched for.
    rows_for: Option<String>,
    /// The symbol picker's rows for that query, on every venue the catalog lists, best first.
    pub(super) rows: Vec<TradeWindowCatalogRow>,
}

impl TradeWindowCatalog {
    /// Bring the answers in line with this frame's catalog, instrument and picker query, scanning
    /// the catalog only for what changed.
    pub(crate) fn sync(&mut self, catalog: &Arc<Catalog>, venue: &str, symbol: &str, query: &str) {
        if !std::ptr::eq(self.from.as_ptr(), Arc::as_ptr(catalog)) {
            *self = TradeWindowCatalog {
                from: Arc::downgrade(catalog),
                loaded: !catalog.is_empty(),
                ..TradeWindowCatalog::default()
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
            None => unlisted_base_quote(symbol),
        };
        let class = self.listing.as_ref().map(|l| l.class);
        if self
            .spellings_for
            .as_ref()
            .is_none_or(|(b, q, c)| b != base || q != quote || *c != class)
        {
            // The pickers' own rule for "the same instrument on another venue".
            self.spellings = same_instrument_per_venue(catalog, base, quote, class)
                .into_iter()
                .map(|i| (i.venue.clone(), i.raw_symbol.clone(), i.asset_class()))
                .collect();
            self.spellings_for = Some((base.to_string(), quote.to_string(), class));
        }
        let query = query.trim();
        if self.rows_for.as_deref() != Some(query) {
            self.rows = picker_rows(catalog, query);
            self.rows_for = Some(query.to_string());
        }
    }

    /// The window instrument's base and quote: the catalog's, else [`unlisted_base_quote`].
    pub(super) fn base_quote<'s>(&'s self, symbol: &'s str) -> (&'s str, &'s str) {
        match &self.listing {
            Some(l) => (l.base.as_str(), l.quote.as_str()),
            None => unlisted_base_quote(symbol),
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
            .map_or("", vike_catalog::SymbolPickerKind::product)
    }
}

/// What the catalog lists under `symbol` on `venue`: an EXACT lookup over the venue's whole list.
///
/// ⚠ Not the ranked `Catalog::search` with the symbol as the query: its tiers compare the raw symbol
/// with the UPPER-CASED query, so a mixed-case raw symbol (hyperliquid's `kPEPE`) would never be
/// found, and the window would say the catalog does not list it and refuse every size (the other
/// venues' spellings read the whole list for the same reason). The empty query returns the venue's
/// list in catalog order. The answer is cached ([`TradeWindowCatalog`]).
fn listing(catalog: &Catalog, venue: &str, symbol: &str) -> Option<TradeWindowCatalogListing> {
    let filter = CatalogSearchFilter { tab: None, venue: Some(venue.to_string()) };
    catalog.search("", &filter, usize::MAX).into_iter().find(|i| i.raw_symbol == symbol).map(|i| {
        TradeWindowCatalogListing {
            grid: Grid {
                tick_size: grid_value(i.properties.tick_size),
                lot_size: grid_value(i.properties.step_size),
                min_notional: grid_value(i.properties.min_notional),
                min_qty: grid_value(i.properties.min_qty),
            },
            base: i.base.clone(),
            quote: i.quote.clone(),
            class: i.asset_class(),
        }
    })
}

/// The symbol picker's rows for `query`: spot and perpetual on every venue the catalog lists, ranked
/// (`vike_catalog::picker_flat`), one row per instrument. The picker used to search only the venues
/// the server runs an engine on, so a venue the trader could not trade yet — but could look at, and
/// move to — never appeared in it.
fn picker_rows(catalog: &Catalog, query: &str) -> Vec<TradeWindowCatalogRow> {
    picker_flat(catalog, query, ROWS_CACHED)
        .into_iter()
        .filter_map(|i| {
            let kind = kind_of(i.asset_class())?;
            Some(TradeWindowCatalogRow {
                venue: i.venue.clone(),
                symbol: i.raw_symbol.clone(),
                pair: format!("{}/{} {}", i.base, i.quote, kind.label()),
            })
        })
        .collect()
}
