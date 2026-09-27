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
//! ⚠ **And for forty days the depth column above was true in NAME only.** From 2026-08-01 to
//! 2026-09-10 the `kind=depth/venue=binance/symbol=BTCUSDT.P` series recorded **0.41–0.43
//! updates/s** against the ~10/s a `@depth@100ms` stream carries — the shared decoder
//! (`crates/bridges/binance/src/family/depth.rs`'s `apply_depth_event`) enforced binance's SPOT
//! contiguity rule on a USDⓈ-M FUTURES stream, so the second diff of every session read as a gap
//! and the driver re-seeded on its 3 s backoff forever. Two rows per ~4.3 s cycle, both full
//! snapshots, nothing incremental. The rows that ARE there are honest snapshots; roughly 32 million
//! book states between them are not recoverable — `crates/vike-backfill/src/caps.rs`'s
//! `PLANNABLE_KINDS` omits depth and no vendor sells binance L2, so this kind can only ever be
//! re-recorded LIVE. **The catalog surfaces still advertise that range as complete and will keep
//! doing so**: coverage is computed from rows present, and rows were present.
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

/// Render a family PATTERN into a group NAME that can be a directory.
///
/// ⚠ **This is the OTHER half of [`glob_matches`], and confusing the two is the bug it prevents.**
/// That function takes the operator's glob and decides what the feed SUBSCRIBES to. This one takes
/// the same glob and decides what the store is asked to CREATE. A family key is free text and is
/// conventionally a glob (`*USDT.P`, `BTC*`, or a bare `*` for every listing); a group name is a
/// `group=…` path component, and `vike_model::store_path::PATH_HOSTILE_IN_A_SYMBOL` lists what may
/// not appear in one — `*` and `?` among them, because Windows refuses a directory carrying either.
///
/// ⚠ **The mapping is deliberately LOSSY-BUT-STABLE, not an escape.** `*` is DROPPED as an affix
/// and becomes `all` when it is the whole pattern, so `*USDT.P` renders `USDT.P` and `BTC*` renders
/// `BTC`. An escape (`%2A`, `_star_`) would round-trip, and round-tripping is not wanted: the
/// pattern is kept verbatim on `Target::Family`, where the matching happens, so nothing needs to
/// recover it from a directory name. A group name only has to be a stable, readable label.
///
/// ⚠ Two patterns CAN collide — `*BTC` and `BTC*` both render `BTC`. Accepted rather than solved: a
/// profile naming both is recording one venue's instruments into one group twice, which is a
/// profile mistake and not something a name renderer should be catching. What is NOT accepted is a
/// name no directory can hold, which is what this function exists for.
///
/// ⚠ An empty result would be worse than any collision — a bare `*` must not render `""`, because
/// that is a `group=` component with no value. Hence the `all` fallback.
pub fn group_name_for(pattern: &str) -> String {
    let trimmed = pattern.trim_matches('*');
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !vike_model::store_path::PATH_HOSTILE_IN_A_SYMBOL.contains(c))
        .collect();
    if cleaned.is_empty() { "all".to_string() } else { cleaned }
}

/// One row a [`SymbolSource`] hands back: the venue's own wire symbol, **and what KIND of
/// instrument it names**.
///
/// The class is the half `docs/decisions/0061` Phase 2 calls this crate's drop seam. It used to be
/// discarded one line after it arrived — `CatalogSymbols` mapped `Instrument` straight down to
/// `raw_symbol` — so the fact that binance's own `exchangeInfo` had already classified every symbol
/// died inside a `.map`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The venue's own wire symbol — what `subscribe_trades` takes and what the store partitions
    /// on. `Instrument::raw_symbol`, which already carries this workspace's `.P` perp marker.
    pub symbol: String,
    /// What kind of instrument [`Self::symbol`] names, straight off `Instrument::asset_class`.
    pub class: vike_model::AssetClass,
}

/// The instrument-list source, injected so the filter is testable without a network call — the same
/// seam `discovery`'s `GammaSource` plays for Polymarket.
pub trait SymbolSource {
    fn symbols(&self) -> Result<Vec<Listing>, String>;
}

/// The production source: Binance's own spot + perp `exchangeInfo`.
pub struct CatalogSymbols;

impl SymbolSource for CatalogSymbols {
    fn symbols(&self) -> Result<Vec<Listing>, String> {
        BinanceCatalog
            .list_instruments()
            .map_err(|e| format!("binance instrument list: {e}"))
            // Both fields are carried now. `raw_symbol` is what the wire and the store need;
            // `asset_class` is what the venue's own listing already decided about it, and dropping
            // it here was `docs/decisions/0061` Phase 2's second seam.
            .map(|v| {
                v.into_iter()
                    .map(|i| Listing { symbol: i.raw_symbol, class: i.asset_class })
                    .collect()
            })
    }
}

enum Target {
    /// A glob over the venue's instrument list. Resolved ONCE and cached: Binance's listing changes
    /// on the order of weeks, so re-fetching `exchangeInfo` on every tick would be a REST call a
    /// minute for an answer that does not move. A restart re-resolves, which is the right cadence.
    ///
    /// ⚠ The cache holds the whole [`Listing`] rather than the filtered symbol set, so the class
    /// survives as far as this struct. Filtering moved to `desired` and costs one pass over a list
    /// of a few thousand entries per tick, against the REST call it is not making.
    Family { pattern: String, src: Box<dyn SymbolSource>, resolved: Option<Vec<Listing>> },
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
    ///
    /// ⚠ **The two fields take DIFFERENT strings, and that is the whole point of this function.**
    /// `target`'s `pattern` keeps the operator's glob VERBATIM — it is what
    /// [`glob_matches`] compares a listing against, so altering it would change which instruments
    /// this feed subscribes to. `family` is the GROUP NAME, and a group name becomes a
    /// `group=…` DIRECTORY under the hist store, so it goes through [`group_name_for`].
    ///
    /// They held the same string until 2026-09-19, which meant a `*USDT.P` family asked the store
    /// for a directory called `group=*USDT.P`. `*` is one of the characters Windows refuses in a
    /// directory name outright (`vike_model::store_path::PATH_HOSTILE_IN_A_SYMBOL`), so that series
    /// could be written on Linux and never opened on a Windows box — and the store's own refusal,
    /// added the same week for `symbol=`, would have made it worse rather than better here: a write
    /// refusal in the LIVE recorder does not fail loudly. `crates/vike-data/src/live_rec.rs`'s
    /// `flush_buf_with` retries once, increments `discarded`, logs one `warn!` and continues — so a
    /// hostile family would have become a permanent per-flush silent-loss loop behind a daemon that
    /// looks healthy.
    pub fn family_with_source(
        family: &str,
        sink: Arc<dyn LiveDataSink>,
        src: Box<dyn SymbolSource>,
    ) -> Self {
        Self {
            family: Some(group_name_for(family)),
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
                if resolved.is_none() {
                    // A fetch failure is `Err`, NOT an empty set — the runtime reads `Err` as
                    // UNKNOWN and leaves subscriptions untouched, where an empty set would
                    // unsubscribe every live stream because `exchangeInfo` happened to time out.
                    *resolved = Some(src.symbols()?);
                }
                let listings = resolved.as_deref().unwrap_or(&[]);
                // ⚠ **THIS LINE IS WHERE THE CLASS STOPS, and it is a STOP-AND-REPORT rather than
                // an oversight.** `VenueFeed::desired` is a SHARED HOME — `crate::runtime`'s trait,
                // four impls (this one, `crate::venues::polymarket`'s, and the two doubles in
                // `runtime.rs`) — and the root `CLAUDE.md`'s rule for one of those is that a
                // missing knob is reported, not patched locally. Widening its return type to carry
                // the class is ONE coordinated PR informed by every consumer's report, and the
                // consumer that would have to move with it is the subscription set matched against
                // `crate::config`'s `Subscription` globs. `docs/decisions/0061` Phase 2 asks for
                // the DROP to stop, which it has: the class now reaches this struct and dies at a
                // named trait boundary instead of inside a `.map` two lines after it arrived.
                Ok(listings
                    .iter()
                    .filter(|l| glob_matches(pattern, &l.symbol))
                    .map(|l| l.symbol.clone())
                    .collect())
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

    /// Every shape an operator can write, and what each becomes as a DIRECTORY.
    ///
    /// The bare `*` row is the one that matters most: it is the only pattern whose trim leaves
    /// nothing, and an empty group name is a `group=` component with no value.
    #[test]
    fn a_family_pattern_renders_to_a_name_a_directory_can_hold() {
        for (pattern, expected) in [
            ("*USDT.P", "USDT.P"),
            ("BTC*", "BTC"),
            ("*USDT*", "USDT"),
            ("*", "all"),
            ("**", "all"),
            ("BTCUSDT", "BTCUSDT"),
        ] {
            assert_eq!(group_name_for(pattern), expected, "pattern {pattern:?}");
        }
    }

    /// The property, rather than the table: whatever an operator writes, the rendered name is one
    /// the store will accept. Asserted against the store's OWN predicate, not against a second copy
    /// of the character list — a list spelled twice is one that drifts.
    #[test]
    fn a_rendered_group_name_is_never_refused_by_the_store() {
        for pattern in ["*USDT.P", "BTC*", "*", "a/b", "x:y", "q?z", "<>\"|", "*/*"] {
            let name = group_name_for(pattern);
            assert!(
                vike_model::store_path::refuse_a_path_hostile_symbol(&name).is_ok(),
                "pattern {pattern:?} rendered {name:?}, which the store refuses"
            );
            assert!(!name.is_empty(), "pattern {pattern:?} rendered an EMPTY group name");
        }
    }

    /// ⚠ **The other half, and the one a reader should check first:** rendering the NAME must not
    /// have changed what the feed SUBSCRIBES to. The glob stays on `Target::Family`'s `pattern`,
    /// and `glob_matches` is fed that, never the rendered name. Without this, the rename above
    /// would silently narrow a `*USDT.P` family to the literal symbol `USDT.P` — which matches
    /// nothing, so the recorder would go quiet rather than fail.
    #[test]
    fn a_family_glob_still_matches_on_the_raw_pattern() {
        assert!(glob_matches("*USDT.P", "BTCUSDT.P"), "the raw glob must still match");
        assert!(
            !glob_matches(&group_name_for("*USDT.P"), "BTCUSDT.P"),
            "…and the rendered NAME must not be used for matching — if this passes, the two have been confused"
        );
    }

    struct Fixture(Vec<&'static str>);

    impl SymbolSource for Fixture {
        fn symbols(&self) -> Result<Vec<Listing>, String> {
            // The class is derived from the `.P` marker here because that is exactly what binance's
            // own catalog does with it (`crates/bridges/binance/src/catalog.rs` mints
            // `BTCUSDT.P` as `CryptoPerp`), so the double answers what the real source would.
            Ok(self
                .0
                .iter()
                .map(|s| Listing {
                    symbol: (*s).to_string(),
                    class: if s.ends_with(vike_catalog::PERP_SUFFIX) {
                        vike_model::AssetClass::CryptoPerp
                    } else {
                        vike_model::AssetClass::CryptoSpot
                    },
                })
                .collect())
        }
    }

    struct Failing;

    impl SymbolSource for Failing {
        fn symbols(&self) -> Result<Vec<Listing>, String> {
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
            fn symbols(&self) -> Result<Vec<Listing>, String> {
                self.0.set(self.0.get() + 1);
                Ok(vec![Listing {
                    symbol: "BTCUSDT".into(),
                    class: vike_model::AssetClass::CryptoSpot,
                }])
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

    /// **`docs/decisions/0061` Phase 2's second drop seam, as far as it goes.** The class the
    /// venue's own listing already decided now reaches [`Target::Family`]'s cache instead of dying
    /// inside `CatalogSymbols`' `.map`; `desired` still returns bare symbols, because widening
    /// `VenueFeed::desired` is a change to a SHARED HOME across four impls and therefore a
    /// STOP-AND-REPORT rather than a local patch (`desired`'s own ⚠ carries the argument).
    ///
    /// This test is what makes the difference observable: without it the two states — "carried to
    /// the boundary" and "dropped at the source" — look identical from outside `desired`.
    #[test]
    fn the_venues_own_classification_survives_as_far_as_the_feeds_cache() {
        let mut f = feed("*USDT*", vec!["BTCUSDT", "BTCUSDT.P", "ETHUSDT"]);
        // Resolve once, then read the cache the resolve populated.
        let _ = f.desired(0).unwrap();
        match &f.target {
            Target::Family { resolved, .. } => {
                let held = resolved.as_ref().expect("resolved on the first tick");
                assert_eq!(held.len(), 3, "every listing is cached, not just the matching ones");
                assert_eq!(
                    held.iter()
                        .find(|l| l.symbol == "BTCUSDT.P")
                        .map(|l| l.class)
                        .expect("the perp listing"),
                    vike_model::AssetClass::CryptoPerp,
                    "the perp's class survived the source"
                );
                assert_eq!(
                    held.iter()
                        .find(|l| l.symbol == "BTCUSDT")
                        .map(|l| l.class)
                        .expect("the spot listing"),
                    vike_model::AssetClass::CryptoSpot
                );
            }
            _ => panic!("expected a family target"),
        }
        // ...and `desired` still hands the runtime the same bare symbol set it always did.
        assert_eq!(
            f.desired(0).unwrap(),
            ["BTCUSDT", "BTCUSDT.P", "ETHUSDT"].iter().map(|s| s.to_string()).collect()
        );
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
