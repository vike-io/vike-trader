//! The Binance [`VenueFeed`] — the STATIC-family case, and the proof that "families are general"
//! (spec §8.2) is not a Polymarket-shaped claim.
//!
//! Polymarket resolves a family by asking Gamma what is live right now, because its token ids rotate
//! every 5 minutes. Binance resolves the same key as a **filter over an instrument list that does not
//! change from tick to tick** — a glob (`*USDT`, `BTC*`, `*USDT.P`) matched against
//! [`BinanceCatalog::list_instruments`]. One trait, rotation as the general case, static as the
//! degenerate one.
//!
//! ## ⚠ What this can actually record: TRADES and conflating DEPTH. Not quotes, not book.
//!
//! Binance's declared capabilities are `LiveDataCaps { bars, trades, depth }` — **`quotes: false`,
//! `book: false`** (`vike_model::venue_caps`), and its `DataClient` refuses both verbs accordingly.
//! What it does serve is `subscribe_depth`, the DOM's **conflating** L2 lane, which emits through
//! [`vike_data::LiveDataSink::l2_snapshot`] — and `RecorderSink` PERSISTS that verb, as its own
//! `kind=depth` series.
//!
//! ⚠ **That last clause used to read "…and `RecorderSink` does not implement that verb, so the
//! trait's default no-op swallows it", and it stopped being true in #995** — the commit that added
//! the impl (`crates/vike-data/src/live_rec.rs`'s `RecorderSink`, gated by
//! `crates/vike-data/tests/hist_datafusion.rs`) edited this very paragraph without editing that
//! sentence. It is called out rather than quietly replaced because the stale sentence went on to be
//! used as EVIDENCE: a later change reasoned "so a depth socket is never opened by this daemon" from
//! it, and left `crates/vike-bridge-core/src/depth.rs`'s `connect_depth` dialing unbounded inside the
//! recorder's own teardown budget. A wrong doc is load-bearing right up until somebody trusts it.
//!
//! So a Binance recording is the **trade tape plus a conflated depth series**, and that is worth
//! stating plainly because it interacts with [`vike_backfill::caps`]: no venue-direct backfill serves
//! `trade` or `book` either. Combined:
//!
//! | binance | backfill | record live |
//! |---|---|---|
//! | `bar` | ✓ (klines) | — (derived; the recorder does not store bars) |
//! | `trade` | ✗ | **✓ — this module is the only path** |
//! | `quote` | ✗ | ✗ (venue serves none) |
//! | `depth` | ✗ | **✓ — the conflating lane, under its own kind** |
//! | `book` | ✗ (archive rights-blocked) | ✗ (venue serves no lossless lane) |
//!
//! **A LOSSLESS Binance L2 book is still not obtainable in this workspace by any means**, and
//! `kind=depth` is deliberately not a rename of one: a full snapshot every 100 ms with every
//! intermediate state discarded is not a lossless book, and writing it as `kind=book` would let a
//! maker-fill backtest run on it and report fills it could never have got. THE PATH IS THE
//! DISCLOSURE.
//!
//! No special-casing was needed for any of this. [`crate::session::SubscriptionSet`] asks for each
//! stream once, learns `Unsupported` as a permanent capability answer, and never asks again — so a
//! Binance feed subscribes trades and depth, is told twice that quotes and book do not exist, and
//! records the two lanes it got.

use std::collections::BTreeSet;
use std::sync::Arc;

use vike_binance::catalog::BinanceCatalog;
use vike_binance::market_feed::Feeds;
use vike_catalog::CatalogProvider;
use vike_data::live::{DataClient, LiveDataSink};

use crate::runtime::VenueFeed;

/// The venue key, as it appears in the store's `venue=` partition.
pub const VENUE: &str = "binance";

/// Match a symbol against a family glob supporting a leading and/or trailing `*`.
///
/// Deliberately NOT a regex: a family key is something a customer types into a TOML file and should
/// be able to predict the meaning of. `*USDT` / `BTC*` / `*USDT.P` / `*` cover the real cases
/// (quote-asset, base-asset, perp-suffix, everything) and a literal with no `*` matches exactly one
/// symbol, which is how a one-instrument family degenerates to naming it.
pub fn glob_matches(pattern: &str, symbol: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        // `*x*` — contains. Bare `*` and `**` degenerate to "contains empty" = everything.
        (Some(rest), Some(_)) => symbol.contains(rest.strip_suffix('*').unwrap_or(rest)),
        (Some(suffix), None) => symbol.ends_with(suffix),
        (None, Some(prefix)) => symbol.starts_with(prefix),
        (None, None) => symbol == pattern,
    }
}

/// The instrument-list source, injected so the filter is testable without a network call — the same
/// seam `discovery`'s `GammaSource` plays for Polymarket.
pub trait SymbolSource {
    fn symbols(&self) -> Result<Vec<String>, String>;
}

/// The production source: Binance's own spot + perp `exchangeInfo`.
pub struct CatalogSymbols;

impl SymbolSource for CatalogSymbols {
    fn symbols(&self) -> Result<Vec<String>, String> {
        BinanceCatalog
            .list_instruments()
            .map_err(|e| format!("binance instrument list: {e}"))
            // `raw_symbol` is the venue's own wire symbol — what `subscribe_trades` takes and what
            // the store partitions on. The catalog's other fields are display/classification.
            .map(|v| v.into_iter().map(|i| i.raw_symbol).collect())
    }
}

enum Target {
    /// A glob over the venue's instrument list. Resolved ONCE and cached: Binance's listing changes
    /// on the order of weeks, so re-fetching `exchangeInfo` on every tick would be a REST call a
    /// minute for an answer that does not move. A restart re-resolves, which is the right cadence.
    Family { pattern: String, src: Box<dyn SymbolSource>, resolved: Option<BTreeSet<String>> },
    /// An explicit symbol list — the same set forever.
    Fixed(BTreeSet<String>),
}

/// One profile subscription, wired to a live Binance [`Feeds`].
pub struct BinanceFeed {
    family: Option<String>,
    target: Target,
    feeds: Feeds,
}

impl BinanceFeed {
    /// A static family (`*USDT.P`) over the venue's real instrument list.
    pub fn family(family: &str, sink: Arc<dyn LiveDataSink>) -> Self {
        Self::family_with_source(family, sink, Box::new(CatalogSymbols))
    }

    /// [`family`](Self::family) with an injected symbol source — the offline test seam.
    pub fn family_with_source(
        family: &str,
        sink: Arc<dyn LiveDataSink>,
        src: Box<dyn SymbolSource>,
    ) -> Self {
        Self {
            family: Some(family.to_string()),
            target: Target::Family { pattern: family.to_string(), src, resolved: None },
            feeds: Feeds::new(sink, || {}),
        }
    }

    /// An explicit symbol list. No family ⇒ no group ⇒ per-symbol series.
    pub fn symbols(symbols: &[String], sink: Arc<dyn LiveDataSink>) -> Self {
        Self {
            family: None,
            target: Target::Fixed(symbols.iter().cloned().collect()),
            feeds: Feeds::new(sink, || {}),
        }
    }
}

impl VenueFeed for BinanceFeed {
    fn venue(&self) -> &str {
        VENUE
    }

    fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }

    fn desired(&mut self, _now_ms: i64) -> Result<BTreeSet<String>, String> {
        match &mut self.target {
            Target::Fixed(set) => Ok(set.clone()),
            Target::Family { pattern, src, resolved } => {
                if let Some(cached) = resolved {
                    return Ok(cached.clone());
                }
                // A fetch failure is `Err`, NOT an empty set — the runtime reads `Err` as UNKNOWN and
                // leaves subscriptions untouched, where an empty set would unsubscribe every live
                // stream because `exchangeInfo` happened to time out.
                let all = src.symbols()?;
                let set: BTreeSet<String> =
                    all.into_iter().filter(|s| glob_matches(pattern, s)).collect();
                *resolved = Some(set.clone());
                Ok(set)
            }
        }
    }

    fn client(&mut self) -> &mut dyn DataClient {
        &mut self.feeds
    }

    // `narrow` is deliberately the DEFAULT identity. Binance does not derive quotes from a book the
    // way Polymarket does — it serves neither verb — so there is nothing to de-duplicate here, and
    // `SubscriptionSet` handles the refusals on its own by learning `Unsupported` once.
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::NoopSink;

    struct Fixture(Vec<&'static str>);

    impl SymbolSource for Fixture {
        fn symbols(&self) -> Result<Vec<String>, String> {
            Ok(self.0.iter().map(|s| s.to_string()).collect())
        }
    }

    struct Failing;

    impl SymbolSource for Failing {
        fn symbols(&self) -> Result<Vec<String>, String> {
            Err("binance instrument list: connection reset".into())
        }
    }

    fn feed(pattern: &str, syms: Vec<&'static str>) -> BinanceFeed {
        BinanceFeed::family_with_source(pattern, Arc::new(NoopSink), Box::new(Fixture(syms)))
    }

    #[test]
    fn a_quote_asset_glob_selects_by_suffix() {
        let mut f = feed("*USDT", vec!["BTCUSDT", "ETHUSDT", "BTCUSDC", "BTCUSDT.P"]);
        assert_eq!(
            f.desired(0).unwrap(),
            ["BTCUSDT", "ETHUSDT"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn a_base_asset_glob_selects_by_prefix() {
        let mut f = feed("BTC*", vec!["BTCUSDT", "ETHUSDT", "BTCUSDC"]);
        assert_eq!(
            f.desired(0).unwrap(),
            ["BTCUSDT", "BTCUSDC"].iter().map(|s| s.to_string()).collect()
        );
    }

    /// The perp suffix is part of the symbol, so `*USDT` must NOT sweep perps in with spot — those
    /// are different instruments with different tapes, and silently recording both under one family
    /// would mix them in a grouped series.
    #[test]
    fn spot_and_perp_are_different_families() {
        let syms = vec!["BTCUSDT", "BTCUSDT.P", "ETHUSDT", "ETHUSDT.P"];
        let mut spot = feed("*USDT", syms.clone());
        let mut perp = feed("*USDT.P", syms);
        assert_eq!(spot.desired(0).unwrap().len(), 2);
        assert_eq!(perp.desired(0).unwrap().len(), 2);
        assert!(spot.desired(0).unwrap().iter().all(|s| !s.ends_with(".P")));
        assert!(perp.desired(0).unwrap().iter().all(|s| s.ends_with(".P")));
    }

    #[test]
    fn a_literal_pattern_matches_exactly_one_symbol() {
        let mut f = feed("BTCUSDT", vec!["BTCUSDT", "BTCUSDT.P", "ETHUSDT"]);
        assert_eq!(f.desired(0).unwrap(), ["BTCUSDT"].iter().map(|s| s.to_string()).collect());
    }

    /// A STATIC family: the clock changes nothing, which is the whole contrast with Polymarket's
    /// 5-minute rotation. Same trait, degenerate case.
    #[test]
    fn the_desired_set_does_not_move_with_the_clock() {
        let mut f = feed("*USDT", vec!["BTCUSDT", "ETHUSDT"]);
        let a = f.desired(0).unwrap();
        let b = f.desired(1_800_000_000_000).unwrap();
        assert_eq!(a, b);
    }

    /// The instrument list is fetched ONCE. Binance's listing moves on the order of weeks; re-asking
    /// `exchangeInfo` every tick would be a REST call a minute for an answer that does not change.
    #[test]
    fn the_instrument_list_is_fetched_once_not_per_tick() {
        struct Counting(std::cell::Cell<usize>);
        impl SymbolSource for Counting {
            fn symbols(&self) -> Result<Vec<String>, String> {
                self.0.set(self.0.get() + 1);
                Ok(vec!["BTCUSDT".into()])
            }
        }
        // Peek at the count through a raw pointer-free trick: resolve twice and assert the set is
        // stable, then assert the cache field is populated.
        let mut f = BinanceFeed::family_with_source(
            "*USDT",
            Arc::new(NoopSink),
            Box::new(Counting(std::cell::Cell::new(0))),
        );
        assert_eq!(f.desired(0).unwrap().len(), 1);
        assert_eq!(f.desired(1).unwrap().len(), 1);
        match &f.target {
            Target::Family { resolved, .. } => {
                assert!(resolved.is_some(), "cached after first tick")
            }
            _ => panic!("expected a family target"),
        }
    }

    /// A fetch failure is `Err`, never an empty set — the runtime reads `Err` as UNKNOWN and leaves
    /// subscriptions alone, where an empty set would unsubscribe every live stream because
    /// `exchangeInfo` timed out.
    #[test]
    fn an_instrument_fetch_failure_is_an_error_not_an_empty_set() {
        let mut f = BinanceFeed::family_with_source("*USDT", Arc::new(NoopSink), Box::new(Failing));
        assert!(f.desired(0).is_err());
    }

    #[test]
    fn an_explicit_symbol_list_has_no_family() {
        let mut f = BinanceFeed::symbols(&["BTCUSDT".into()], Arc::new(NoopSink));
        assert_eq!(f.family(), None);
        assert_eq!(f.desired(0).unwrap().len(), 1);
    }

    #[test]
    fn glob_edge_cases() {
        assert!(glob_matches("*", "ANYTHING"));
        assert!(glob_matches("*USD*", "BTCUSDT"));
        assert!(!glob_matches("*USD*", "BTCEUR"));
        assert!(!glob_matches("BTC*", "ETHBTC"), "prefix, not contains");
        assert!(!glob_matches("*USDT", "USDTBTC"), "suffix, not contains");
    }
}
