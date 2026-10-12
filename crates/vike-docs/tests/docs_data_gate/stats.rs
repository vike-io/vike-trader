//! `stats.json`, the `generated_from` stamp it carries, and the rendered set as a whole.

use serde_json::Value;
use vike_data::store::store_kind::STORE_KINDS;
use vike_docs::{
    DEFAULT_GENERATED_FROM, EVENTS, INDICATORS_JSON, P99_BUDGET_NS, ROSTERS_JSON, SCHEMA_VERSION,
    STATS_JSON, TEMPLATES_JSON, VENUES_JSON, generated_from, rendered_files, stats_value,
};
use vike_exec::recon::DivergenceKind;
use vike_indicators::{pair_registry, registry as indicator_registry};
use vike_model::venues::venue_caps::ORDER_KINDS;
use vike_model::{AssetClass, VENUES};
use vike_strategy::{PORTABLE_STRATEGIES, SCRIPT_ONLY, SIMULATOR_ONLY};

/// `stats.json`: the venue count is DERIVED from the roster, the latency figure is the gate's
/// budget constant, and the generator identity fields are non-empty.
#[test]
fn stats_derive_from_the_roster_and_the_budget() {
    let stats = stats_value(DEFAULT_GENERATED_FROM);
    assert_eq!(stats["venue_count"].as_u64(), Some(VENUES.len() as u64));
    assert_eq!(stats["indicator_count"].as_u64(), Some(indicator_registry().len() as u64));
    assert_eq!(stats["pair_indicator_count"].as_u64(), Some(pair_registry().len() as u64));
    assert_eq!(stats["portable_strategy_count"].as_u64(), Some(PORTABLE_STRATEGIES.len() as u64));
    assert_eq!(stats["simulator_only_strategy_count"].as_u64(), Some(SIMULATOR_ONLY.len() as u64));
    assert_eq!(stats["script_only_strategy_count"].as_u64(), Some(SCRIPT_ONLY.len() as u64));
    assert_eq!(stats["event_count"].as_u64(), Some(EVENTS.len() as u64));
    assert_eq!(stats["store_kind_count"].as_u64(), Some(STORE_KINDS.len() as u64));
    assert_eq!(stats["order_kind_count"].as_u64(), Some(ORDER_KINDS.len() as u64));
    assert_eq!(stats["asset_class_count"].as_u64(), Some(AssetClass::ALL.len() as u64));
    assert_eq!(stats["divergence_kind_count"].as_u64(), Some(DivergenceKind::ALL.len() as u64));
    assert_eq!(stats["latency_p99_budget_ns"].as_u64(), Some(P99_BUDGET_NS));
    assert_eq!(stats["schema_version"].as_u64(), Some(u64::from(SCHEMA_VERSION)));
    for key in ["generator_version", "generated_from"] {
        let v = stats[key].as_str().unwrap_or_else(|| panic!("{key} is a string"));
        assert!(!v.is_empty(), "{key} is non-empty");
    }
}

/// The `GENERATED_FROM` argument reaches `stats.json` VERBATIM, and its absence reaches it as
/// `DEFAULT_GENERATED_FROM` — the two halves of the resolution the `docs_data` bin performs over
/// its argv.
///
/// Why a compile-time stamp could name the wrong commit: `vike_docs::DEFAULT_GENERATED_FROM`'s doc.
/// A test that only asserted non-emptiness, as the one above does, passes under BOTH spellings
/// (compile-time and caller-supplied); this one fails unless the caller's value is the one
/// published.
#[test]
fn the_generated_from_argument_flows_through_to_stats_json() {
    assert_eq!(
        generated_from(None),
        Ok(DEFAULT_GENERATED_FROM),
        "no argument stamps the local-build default"
    );
    assert_eq!(
        stats_value(generated_from(None).expect("absent is never an error"))["generated_from"]
            .as_str(),
        Some(DEFAULT_GENERATED_FROM),
        "the default reaches stats.json"
    );

    // A 40-hex commit, the shape `git rev-parse HEAD` produces and the release workflow passes.
    let sha = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(generated_from(Some(sha)), Ok(sha), "a supplied stamp is passed through unchanged");
    assert_eq!(
        stats_value(sha)["generated_from"].as_str(),
        Some(sha),
        "the supplied stamp reaches stats.json verbatim — not a compile-time value"
    );

    // ...and through the rendering the bin actually writes, not only through `stats_value`.
    let rendered = rendered_files(sha);
    let (name, contents) = &rendered[1];
    assert_eq!(*name, STATS_JSON);
    let parsed: Value = serde_json::from_str(contents).expect("stats.json is valid JSON");
    assert_eq!(parsed["generated_from"].as_str(), Some(sha));

    // Every other file is stamp-INDEPENDENT: only stats.json carries the commit, so two renders
    // with different stamps differ in exactly one file.
    let unstamped = rendered_files(DEFAULT_GENERATED_FROM);
    for i in [0, 2, 3, 4] {
        assert_eq!(rendered[i], unstamped[i], "{} is unstamped", rendered[i].0);
    }
}

/// A present-but-BLANK `GENERATED_FROM` is refused, never silently defaulted. The release workflow
/// passes `$(git rev-parse HEAD)`; a command substitution that fails yields the empty string and
/// the surrounding command still runs, so defaulting would publish a release-rendered `stats.json`
/// claiming a dev build.
#[test]
fn a_blank_generated_from_is_refused_rather_than_defaulted() {
    for blank in ["", " ", "\t", "\n"] {
        assert!(
            generated_from(Some(blank)).is_err(),
            "a blank GENERATED_FROM ({blank:?}) must be a usage error, not a silent fallback"
        );
    }
}

/// What the bin writes is exactly these five renderings: valid JSON, newline-terminated, under the
/// canonical asset names, and re-rendering is byte-identical (the determinism the module doc
/// promises).
#[test]
fn rendered_files_are_the_five_assets_and_deterministic() {
    let files = rendered_files(DEFAULT_GENERATED_FROM);
    assert_eq!(files[0].0, VENUES_JSON);
    assert_eq!(files[1].0, STATS_JSON);
    assert_eq!(files[2].0, INDICATORS_JSON);
    assert_eq!(files[3].0, TEMPLATES_JSON);
    assert_eq!(files[4].0, ROSTERS_JSON);
    for (name, contents) in &files {
        assert!(contents.ends_with('\n'), "{name} is newline-terminated");
        let parsed: Value = serde_json::from_str(contents)
            .unwrap_or_else(|e| panic!("{name} is not valid JSON: {e}"));
        match *name {
            n if n == VENUES_JSON => assert!(parsed.is_array(), "{name} is a top-level array"),
            _ => assert!(parsed.is_object(), "{name} is a top-level object"),
        }
    }
    let again = rendered_files(DEFAULT_GENERATED_FROM);
    assert_eq!(files, again, "re-rendering the same tree is byte-identical");
}
