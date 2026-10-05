use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use vike_catalog::{CatalogError, CatalogMode};
use vike_model::AssetClass;

/// The offline instrument list. `symbols: None` is a failing fetch; `calls` counts fetches.
struct FakeCatalog {
    symbols: Option<Vec<&'static str>>,
    calls: Arc<AtomicUsize>,
}

impl FakeCatalog {
    fn of(symbols: Vec<&'static str>) -> Box<dyn CatalogProvider> {
        Box::new(FakeCatalog { symbols: Some(symbols), calls: Arc::default() })
    }
}

impl CatalogProvider for FakeCatalog {
    fn venue(&self) -> &str {
        "binance"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::CryptoSpot, AssetClass::CryptoPerp]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let Some(syms) = &self.symbols else {
            return Err(CatalogError("connection reset".into()));
        };
        // The class is derived from the `.P` marker here because that is exactly what binance's
        // own catalog does with it (`crates/bridges/binance/src/catalog.rs` mints `BTCUSDT.P` as
        // `CryptoPerp`), so the double answers what the real source would.
        Ok(syms
            .iter()
            .map(|s| Instrument {
                venue: "binance".into(),
                raw_symbol: (*s).to_string(),
                asset_class: if s.ends_with(vike_catalog::PERP_SUFFIX) {
                    AssetClass::CryptoPerp
                } else {
                    AssetClass::CryptoSpot
                },
                base: String::new(),
                quote: String::new(),
                description: String::new(),
                properties: Default::default(),
                contract_type: None,
                settle_asset: None,
            })
            .collect())
    }
}

fn glob(pattern: &str, syms: Vec<&'static str>) -> CatalogGlob {
    CatalogGlob::new(pattern, FakeCatalog::of(syms))
}

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
            vike_model::paths::store_path::refuse_a_path_hostile_symbol(&name).is_ok(),
            "pattern {pattern:?} rendered {name:?}, which the store refuses"
        );
        assert!(!name.is_empty(), "pattern {pattern:?} rendered an EMPTY group name");
    }
}

/// ⚠ **The other half, and the one a reader should check first:** rendering the NAME must not
/// have changed what the feed SUBSCRIBES to. The glob stays on [`CatalogGlob`]'s `pattern`,
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

#[test]
fn a_quote_asset_glob_selects_by_suffix() {
    let mut g = glob("*USDT", vec!["BTCUSDT", "ETHUSDT", "BTCUSDC", "BTCUSDT.P"]);
    assert_eq!(
        g.desired(0).unwrap(),
        ["BTCUSDT", "ETHUSDT"].iter().map(|s| s.to_string()).collect()
    );
}

#[test]
fn a_base_asset_glob_selects_by_prefix() {
    let mut g = glob("BTC*", vec!["BTCUSDT", "ETHUSDT", "BTCUSDC"]);
    assert_eq!(
        g.desired(0).unwrap(),
        ["BTCUSDT", "BTCUSDC"].iter().map(|s| s.to_string()).collect()
    );
}

/// The perp suffix is part of the symbol, so `*USDT` must NOT sweep perps in with spot — those
/// are different instruments with different tapes, and silently recording both under one family
/// would mix them in a grouped series.
#[test]
fn spot_and_perp_are_different_families() {
    let syms = vec!["BTCUSDT", "BTCUSDT.P", "ETHUSDT", "ETHUSDT.P"];
    let mut spot = glob("*USDT", syms.clone());
    let mut perp = glob("*USDT.P", syms);
    assert_eq!(spot.desired(0).unwrap().len(), 2);
    assert_eq!(perp.desired(0).unwrap().len(), 2);
    assert!(spot.desired(0).unwrap().iter().all(|s| !s.ends_with(".P")));
    assert!(perp.desired(0).unwrap().iter().all(|s| s.ends_with(".P")));
}

#[test]
fn a_literal_pattern_matches_exactly_one_symbol() {
    let mut g = glob("BTCUSDT", vec!["BTCUSDT", "BTCUSDT.P", "ETHUSDT"]);
    assert_eq!(g.desired(0).unwrap(), ["BTCUSDT"].iter().map(|s| s.to_string()).collect());
}

/// A STATIC family: the clock changes nothing, which is the whole contrast with Polymarket's
/// 5-minute rotation. Same trait, degenerate case.
#[test]
fn the_desired_set_does_not_move_with_the_clock() {
    let mut g = glob("*USDT", vec!["BTCUSDT", "ETHUSDT"]);
    let a = g.desired(0).unwrap();
    let b = g.desired(1_800_000_000_000).unwrap();
    assert_eq!(a, b);
}

/// The instrument list is fetched ONCE. A listing moves on the order of weeks; re-asking the venue
/// every tick would be a REST call a minute for an answer that does not change. The shared counter
/// observes the FETCH itself, which the old binance-module test could only infer from the cache.
#[test]
fn the_instrument_list_is_fetched_once_not_per_tick() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = FakeCatalog { symbols: Some(vec!["BTCUSDT"]), calls: Arc::clone(&calls) };
    let mut g = CatalogGlob::new("*USDT", Box::new(provider));
    assert_eq!(g.desired(0).unwrap().len(), 1);
    assert_eq!(g.desired(1).unwrap().len(), 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1, "fetched once, then served from the cache");
    assert!(g.resolved.is_some(), "cached after first tick");
}

/// **`docs/decisions/0061` Phase 2's second drop seam, as far as it goes.** The class the
/// venue's own listing already decided reaches [`CatalogGlob`]'s cache instead of dying inside a
/// `.map`; `desired` still returns bare symbols, because widening `VenueFeed::desired` is a change
/// to a SHARED HOME and therefore a STOP-AND-REPORT rather than a local patch.
///
/// This test is what makes the difference observable: without it the two states — "carried to
/// the boundary" and "dropped at the source" — look identical from outside `desired`.
#[test]
fn the_venues_own_classification_survives_as_far_as_the_feeds_cache() {
    let mut g = glob("*USDT*", vec!["BTCUSDT", "BTCUSDT.P", "ETHUSDT"]);
    // Resolve once, then read the cache the resolve populated.
    let _ = g.desired(0).unwrap();
    let held = g.resolved.as_ref().expect("resolved on the first tick");
    assert_eq!(held.len(), 3, "every listing is cached, not just the matching ones");
    assert_eq!(
        held.iter()
            .find(|i| i.raw_symbol == "BTCUSDT.P")
            .map(|i| i.asset_class)
            .expect("the perp listing"),
        AssetClass::CryptoPerp,
        "the perp's class survived the source"
    );
    assert_eq!(
        held.iter()
            .find(|i| i.raw_symbol == "BTCUSDT")
            .map(|i| i.asset_class)
            .expect("the spot listing"),
        AssetClass::CryptoSpot
    );
    // ...and `desired` still hands the runtime the same bare symbol set it always did.
    assert_eq!(
        g.desired(0).unwrap(),
        ["BTCUSDT", "BTCUSDT.P", "ETHUSDT"].iter().map(|s| s.to_string()).collect()
    );
}

/// A fetch failure is `Err`, never an empty set — the runtime reads `Err` as UNKNOWN and leaves
/// subscriptions alone, where an empty set would unsubscribe every live stream because the
/// instrument list timed out.
#[test]
fn an_instrument_fetch_failure_is_an_error_not_an_empty_set() {
    let provider = FakeCatalog { symbols: None, calls: Arc::default() };
    let mut g = CatalogGlob::new("*USDT", Box::new(provider));
    let err = g.desired(0).unwrap_err();
    assert!(err.starts_with("binance instrument list: "), "{err}");
}

#[test]
fn glob_edge_cases() {
    assert!(glob_matches("*", "ANYTHING"));
    assert!(glob_matches("*USD*", "BTCUSDT"));
    assert!(!glob_matches("*USD*", "BTCEUR"));
    assert!(!glob_matches("BTC*", "ETHBTC"), "prefix, not contains");
    assert!(!glob_matches("*USDT", "USDTBTC"), "suffix, not contains");
}
