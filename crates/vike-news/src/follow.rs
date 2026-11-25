//! **Follow chart**: the words a headline names the focused chart's asset by, read through the
//! instrument catalog ONCE per chart and kept in the window's view ([`NewsFollowCache`]), and the
//! test a headline passes ([`NewsFollow::is_named_in`]).
//!
//! ⚠ **What the wire carries.** The datahub's news lane is sixteen RSS 2.0 feeds
//! (`crates/vike-datahub/src/tool_feeds/news.rs`'s `FEEDS`): no provider's own ticker field
//! (Finnhub's `related`, CryptoPanic's `currencies`) reaches a [`NewsItem`], only up to six
//! `<category>` values, its `tags`, which the test reads first. ⚠ **No catalog row states an
//! asset's name** (`vike_model::Instrument::description` is each venue's own free text: a contract
//! label, a company or pair name, the symbol), so [`NewsFollow::names`] is empty when read from the
//! catalog and a headline that says only "Bitcoin" is not matched to `BTC` yet.

use std::sync::{Arc, Weak};

use vike_catalog::{Catalog, CatalogSearchFilter};
use vike_datahub_client::tool_feeds::NewsItem;

/// The words a headline names the followed asset by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewsFollow {
    /// The ticker, upper-cased (`BTC`).
    pub(crate) ticker: String,
    /// The asset's names (`Bitcoin`); empty from the catalog (the module doc).
    pub(crate) names: Vec<String>,
}

impl NewsFollow {
    /// The words for a chart on `(venue, symbol)`: the catalog row's base (`BTC` for binance
    /// `BTCUSDT`), else the symbol itself (an unlisted one, or a list not loaded yet); `None` for a
    /// chart that shows no symbol.
    pub(crate) fn of(catalog: &Catalog, venue: &str, symbol: &str) -> Option<Self> {
        if symbol.is_empty() {
            return None;
        }
        let venue_only = CatalogSearchFilter { tab: None, venue: Some(venue.to_string()) };
        let base = catalog
            .search(symbol, &venue_only, 1)
            .first()
            .and_then(|row| row.raw_symbol.eq_ignore_ascii_case(symbol).then(|| row.base.clone()));
        let ticker = base.unwrap_or_else(|| symbol.to_string()).to_uppercase();
        Some(NewsFollow { ticker, names: Vec::new() })
    }

    /// Does `n` name the asset? Its feed tags first: a whole tag equal to the ticker or a name, in
    /// any case. Else its title or summary: the ticker as a whole word in its upper-case form only
    /// (`ONE` never matches "one-day"), or a name as a whole word in any case.
    pub(crate) fn is_named_in(&self, n: &NewsItem) -> bool {
        let tagged = n.tags.iter().any(|t| {
            t.trim().eq_ignore_ascii_case(&self.ticker)
                || self.names.iter().any(|name| t.trim().eq_ignore_ascii_case(name))
        });
        tagged
            || [&n.title, &n.summary].into_iter().any(|text| {
                whole_word(text, &self.ticker)
                    || self
                        .names
                        .iter()
                        .any(|name| whole_word(&text.to_lowercase(), &name.to_lowercase()))
            })
    }
}

/// Does `hay` hold `word` with no letter or digit right before or right after it?
fn whole_word(hay: &str, word: &str) -> bool {
    !word.is_empty()
        && hay.match_indices(word).any(|(at, _)| {
            !hay[..at].chars().next_back().is_some_and(char::is_alphanumeric)
                && !hay[at + word.len()..].chars().next().is_some_and(char::is_alphanumeric)
        })
}

/// What one News window follows, kept in its view ([`crate::NewsView::follow_words`]) so the
/// catalog is read when the focused chart or the catalog changes, never every frame. The catalog
/// is held as a `Weak`, its identity (the shell replaces the `Arc` on every refresh), as the Trade
/// window's catalog cache holds it; it does not keep the instruments alive.
#[derive(Clone, Default)]
pub struct NewsFollowCache {
    from: Weak<Catalog>,
    chart: Option<(String, String)>,
    words: Option<NewsFollow>,
}

impl NewsFollowCache {
    /// The words for `chart` read through `catalog`: read again only when either changed.
    pub(crate) fn words(
        &mut self,
        catalog: &Arc<Catalog>,
        chart: Option<&(String, String)>,
    ) -> Option<&NewsFollow> {
        if self.chart.as_ref() != chart || self.from.as_ptr() != Arc::as_ptr(catalog) {
            self.from = Arc::downgrade(catalog);
            self.chart = chart.cloned();
            self.words = chart.and_then(|(venue, symbol)| NewsFollow::of(catalog, venue, symbol));
        }
        self.words.as_ref()
    }
}

#[path = "follow_tests.rs"]
#[cfg(test)]
mod follow_tests;
