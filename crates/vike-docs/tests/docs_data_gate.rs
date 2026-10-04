//! The docs-data export's gate: the rendered assets are roster-complete, schema-complete, and the
//! one constant this export duplicates cannot drift from its authority.
//!
//! Runs the EXACT rendering the `docs_data` bin writes (`vike_docs::rendered_files`),
//! in-process — no subprocess, no filesystem — and asserts over the parsed output, so what is
//! gated here is byte-for-byte what `.github/workflows/release.yml` attaches to a Release.
//!
//! ⚠ It also READS FOUR FILES outside this crate — the release workflow's asset list, the public
//! mirror script's, the latency gate's budget and the `Event` enum's source — and three of them
//! cannot select this crate through the reverse-dependency closure: two belong to no crate at all,
//! and the third is a TEST file of `vike-core`, which this crate does not depend on. So
//! `xtask/src/ci/tables.rs`'s `DOCS_DATA_GATE_INPUTS` force-adds this crate on a change to any of
//! them. Every path read here goes through `repo_file` or `read_repo` as ONE repo-root-relative
//! literal, which is the spelling `crates/vike-ops/tests/ci_plan_gate.rs` holds that table equal to.
//!
//! ⚠ It carried a `#![cfg(feature = "docs-data")]` until 2026-09-26, when this crate left
//! `vike-ops` and the feature went with it: nothing here is optional any more.

use std::path::Path;

use serde_json::Value;
use vike_data::store::store_kind::STORE_KINDS;
use vike_docs::{
    DEFAULT_GENERATED_FROM, EVENTS, INDICATORS_JSON, P99_BUDGET_NS, ROSTERS_JSON, SCHEMA_VERSION,
    STATS_JSON, TEMPLATES_JSON, VENUES_JSON, WIRING, generated_from, indicators_value,
    rendered_files, rosters_value, stats_value, templates_value, venues_value, wiring_for,
};
use vike_exec::recon::{DivergenceKind, POLICY_NAMES, ReconPolicy, mode_applies};
use vike_indicators::{pair_registry, registry as indicator_registry};
use vike_model::VENUES;
use vike_model::venues::venue_caps::{ORDER_KINDS, TRIGGER_KINDS};
use vike_strategy::{PORTABLE_STRATEGIES, SCRIPT_ONLY, SIMULATOR_ONLY};

/// Every `DivergenceKind`, in declaration order — the test's own copy of the renderer's roster, so
/// a variant added to one and not the other is a compile error here rather than a silent gap.
const ALL_DIVERGENCE_KINDS: [DivergenceKind; 9] = [
    DivergenceKind::MissingFill,
    DivergenceKind::MissingTerminal,
    DivergenceKind::OrphanLocalOrder,
    DivergenceKind::UnknownOrder,
    DivergenceKind::PositionDrift,
    DivergenceKind::PositionOnlyExternal,
    DivergenceKind::OrphanLocalPosition,
    DivergenceKind::BalanceDrift,
    DivergenceKind::JournalDivergence,
];

/// The per-record top-level keys, the `caps` keys, and the `tif` keys — the SCHEMA the module doc
/// promises. Asserted as exact SETS (not subsets) so a silently dropped OR silently added field is
/// loud here; a deliberate schema change edits this list beside `SCHEMA_VERSION`. Spelled sorted,
/// and [`object_keys`] sorts what it collects: key ORDER is deliberately outside the contract —
/// serde_json's map flavor flips with `preserve_order` (which the workspace's DataFusion crates
/// enable through feature unification whenever they share a build with vike-docs), so an order
/// assertion would pass standalone and fail in the roster lane, or vice versa.
const RECORD_KEYS: &[&str] = &[
    "amend_semantics",
    "amend_semantics_note",
    "attribution",
    "caps",
    "fees",
    "id",
    "margin",
    "tif",
    "wiring",
];
const CAPS_KEYS: &[&str] = &[
    "accepted_tifs",
    "backfill_bars",
    "backfill_ticks",
    "default_margin_mode",
    "live_data",
    "margin_modes",
    "max_batch",
    "supported_order_kinds",
    "supported_tifs",
    "supports_combo",
    "supports_modify",
    "supports_native_batch",
    "supports_post_only",
    "supports_reduce_only",
    "trigger_types",
];
const MARGIN_KEYS: &[&str] = &["isolated_wallet_adjustable", "offered_modes", "switch_mechanism"];
const TIF_KEYS: &[&str] = &["day", "fok", "gtc", "gtd", "ioc"];

fn object_keys(value: &Value, what: &str) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("{what} is not a JSON object: {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort_unstable();
    keys
}

/// The wiring map is roster-complete and roster-exact: one row per `vike_model::VENUES` entry, no
/// duplicate, no non-roster stray. This is the test the playbook demands of a per-venue table —
/// adding a venue to the roster reddens it until the row exists.
#[test]
fn wiring_map_is_roster_complete() {
    assert_eq!(WIRING.len(), VENUES.len(), "one WIRING row per roster venue, no strays");
    for &venue in VENUES {
        assert!(
            wiring_for(venue).is_some(),
            "roster venue {venue} has no WIRING row — add one in crates/vike-docs/src/lib.rs"
        );
    }
    let mut ids: Vec<&str> = WIRING.iter().map(|&(v, _)| v).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), WIRING.len(), "duplicate venue id in WIRING");
}

/// Every roster venue appears in `venues.json` exactly once, in roster order.
#[test]
fn every_roster_venue_appears_in_venues_json() {
    let venues = venues_value();
    let records = venues.as_array().expect("venues.json is a top-level array");
    let ids: Vec<&str> =
        records.iter().map(|r| r["id"].as_str().expect("every record has a string id")).collect();
    assert_eq!(ids, VENUES.to_vec(), "one record per roster venue, in roster order");
}

/// Every record carries the full schema: the top-level keys, the caps/margin sub-objects, all five
/// TIF entries (each a tagged object), and non-empty tagged `fees`/`attribution` objects.
#[test]
fn every_record_has_all_fields() {
    let venues = venues_value();
    for record in venues.as_array().expect("array") {
        let id = record["id"].as_str().expect("id");
        assert_eq!(object_keys(record, id), RECORD_KEYS, "{id}: record keys");
        assert_eq!(object_keys(&record["caps"], id), CAPS_KEYS, "{id}: caps keys");
        assert_eq!(object_keys(&record["margin"], id), MARGIN_KEYS, "{id}: margin keys");
        assert_eq!(object_keys(&record["tif"], id), TIF_KEYS, "{id}: tif keys");
        for tif in TIF_KEYS {
            let entry = &record["tif"][tif];
            assert!(entry["outcome"].is_string(), "{id}: tif.{tif} has no outcome tag: {entry}");
        }
        for tagged in ["fees", "attribution"] {
            assert!(
                record[tagged]["kind"].is_string(),
                "{id}: {tagged} has no kind tag: {}",
                record[tagged]
            );
        }
        assert!(record["wiring"].is_string(), "{id}: wiring is a string");
        assert!(record["amend_semantics"].is_string(), "{id}: amend_semantics is a string");
    }
}

/// ⚠ **THE TWO AMEND FIELDS CANNOT CONTRADICT EACH OTHER UNEXPLAINED.** `caps.supports_modify`
/// (does an amend exist at all) and `amend_semantics` (what its quantity MEANS) are rendered from
/// two different tables, and this record is where they are flattened into one view — so this export
/// is where an unexplained pair becomes a reader's problem. It already has: a generated per-venue
/// page read `amend_semantics` FIRST and told the reader amending was supported on a venue whose
/// own exported caps row says it is not.
///
/// `vike_model::venues::venue_amend`'s `amend_semantics_agrees_with_venue_caps_supports_modify` gates the
/// same pairing over the TABLES. This is deliberately not that test moved: it gates the rendered
/// JSON, which is the artifact the pages read, and it gates the half the model test cannot see —
/// that the note is present exactly when it is needed. A model-side exception list with no rendered
/// counterpart is how the pair reached a page as a bare contradiction in the first place.
///
/// Both directions, so neither an unexplained contradiction nor a stale explanation can ship:
///
/// * COHERENT — `supports_modify: false` with `"unsupported"`, or `supports_modify: true` with
///   anything but `"unsupported"` — the note MUST be `null`. A note left behind after the tables
///   were reconciled ships an explanation for a contradiction that is no longer there.
/// * INCOHERENT — any other pair — the note MUST be a non-empty string. This is the assertion that
///   would have caught the finding: the pair is allowed to exist (ibkr's backend split is real),
///   but never bare.
#[test]
fn an_incoherent_amend_pair_is_explained_and_a_coherent_one_is_not() {
    let venues = venues_value();
    let records = venues.as_array().expect("array");
    assert_eq!(records.len(), VENUES.len(), "one record per roster venue");
    let mut explained = 0_usize;
    for record in records {
        let id = record["id"].as_str().expect("id");
        let modifiable = record["caps"]["supports_modify"]
            .as_bool()
            .unwrap_or_else(|| panic!("{id}: caps.supports_modify is a bool"));
        let sem = record["amend_semantics"].as_str().expect("amend_semantics");
        let note = &record["amend_semantics_note"];
        // "unsupported" is the ONLY amend_semantics value that means "no amend leaves the process",
        // so it is exactly the value that may sit beside supports_modify: false.
        let coherent = modifiable != (sem == "unsupported");
        if coherent {
            assert!(
                note.is_null(),
                "{id}: caps.supports_modify={modifiable} and amend_semantics={sem:?} agree, so \
                 amend_semantics_note must be null — delete the venue_amend CAPS_DISAGREEMENTS row \
                 it is rendered from; got {note}"
            );
        } else {
            let why = note.as_str().unwrap_or_else(|| {
                panic!(
                    "{id}: caps.supports_modify={modifiable} contradicts amend_semantics={sem:?} \
                     with NO amend_semantics_note. A consumer flattening these two fields cannot \
                     see the backend axis that makes the pair legitimate, and reads it as a bug. \
                     Either fix whichever value is wrong, or declare the disagreement in \
                     vike_model::venues::venue_amend's CAPS_DISAGREEMENTS with its reason."
                )
            });
            assert!(!why.trim().is_empty(), "{id}: amend_semantics_note is blank");
            explained += 1;
        }
    }
    // The exception list is a RATCHET in spirit: every explained pair is a venue whose two tables
    // disagree, and that set should shrink as evidence lands, never quietly grow. Spelled as an
    // upper bound naming today's one so a second one is a deliberate edit here.
    assert!(
        explained <= 1,
        "{explained} venues now render a contradictory amend pair (was 1: ibkr's socket/cpapi \
         backend split). Each new one needs its reason read, not just a CAPS_DISAGREEMENTS row."
    );
}

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
    assert_eq!(stats["asset_class_count"].as_u64(), Some(11));
    assert_eq!(stats["divergence_kind_count"].as_u64(), Some(ALL_DIVERGENCE_KINDS.len() as u64));
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
/// This is the gate on the fix for a stamp that could name the wrong commit. The previous spelling
/// baked `option_env!("GITHUB_SHA")` at COMPILE time, which the release workflow breaks twice over
/// (a dispatched re-release checks out the TAG while `GITHUB_SHA` names main's head; sccache's
/// cache key does not hash `GITHUB_SHA`, so a cached rustc result can carry a previous run's
/// value) — see `vike_docs::DEFAULT_GENERATED_FROM`. A test that only asserted
/// non-emptiness, as the one above does, passes under BOTH spellings; this one fails unless the
/// caller's value is the one published.
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

/// The exported latency budget equals `crates/vike-core/tests/runtime_latency.rs`'s
/// `P99_BUDGET_NS` — parsed out of the source, because a test-file const cannot be imported. This
/// is the pin that lets `vike_docs` carry a copy at all: the copy cannot drift silently.
#[test]
fn latency_budget_matches_the_runtime_latency_gate() {
    let src = read_repo("crates/vike-core/tests/runtime_latency.rs");
    let needle = "const P99_BUDGET_NS: u64 =";
    let line = src
        .lines()
        .find(|l| l.trim_start().starts_with(needle))
        .unwrap_or_else(|| panic!("{needle:?} not found in the latency gate's source"));
    let digits: String = line[line.find('=').expect("declaration has an `=`") + 1..]
        .chars()
        .take_while(|c| *c != ';')
        .filter(char::is_ascii_digit)
        .collect();
    let authority: u64 = digits.parse().unwrap_or_else(|e| {
        panic!("could not parse the budget out of {line:?} in the latency gate's source: {e}")
    });
    assert_eq!(
        P99_BUDGET_NS, authority,
        "vike_docs::P99_BUDGET_NS drifted from the latency gate's constant — \
         update the copy in crates/vike-docs/src/lib.rs"
    );
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

// ── indicators.json / templates.json ─────────────────────────────────────────────────────────────

/// The record key sets of the two registry exports — exact, sorted, for the same reason as
/// [`RECORD_KEYS`].
const CATEGORY_KEYS: &[&str] = &["indicators", "name"];
const INDICATOR_KEYS: &[&str] = &["batch_only", "display", "id", "params"];
const PAIR_KEYS: &[&str] = &["display", "id", "params"];
const PARAM_SPEC_KEYS: &[&str] = &["default", "max", "min", "name", "step"];
const TEMPLATES_DOC_KEYS: &[&str] = &["portable", "script_only", "simulator_only"];
const TEMPLATE_KEYS: &[&str] = &["id", "live_capable", "params"];
const REASONED_ROW_KEYS: &[&str] = &["id", "reason"];
/// Every `vike_indicators::Category` variant, kebab-case, sorted — the exact category set
/// `indicators.json` carries, EMPTY categories included. A new variant reddens this until it is
/// listed, which is the point: a category the renderer forgot would otherwise vanish silently.
const CATEGORY_NAMES: &[&str] = &[
    "momentum",
    "overlap",
    "pattern",
    "price",
    "statistics",
    "structure",
    "user",
    "volatility",
    "volume",
];

/// `indicators.json` carries the whole category set, every built-in registry row exactly once
/// (across the categories), and every pair-registry row in registry order.
#[test]
fn indicators_json_covers_the_category_set_and_every_registry_row_once() {
    let doc = indicators_value();
    let categories = doc["categories"].as_array().expect("categories is an array");
    let mut names: Vec<&str> =
        categories.iter().map(|c| c["name"].as_str().expect("category name")).collect();
    names.sort_unstable();
    assert_eq!(names, CATEGORY_NAMES, "the category set is the Category enum, empty ones included");

    let mut ids: Vec<&str> = categories
        .iter()
        .flat_map(|c| c["indicators"].as_array().expect("indicators is an array").iter())
        .map(|i| i["id"].as_str().expect("indicator id"))
        .collect();
    let mut expected: Vec<&str> = indicator_registry().iter().map(|m| m.name).collect();
    ids.sort_unstable();
    expected.sort_unstable();
    assert_eq!(ids, expected, "every registry row appears exactly once across the categories");

    let pair_ids: Vec<&str> = doc["pairs"]
        .as_array()
        .expect("pairs is an array")
        .iter()
        .map(|p| p["id"].as_str().expect("pair id"))
        .collect();
    let expected_pairs: Vec<&str> = pair_registry().iter().map(|m| m.name).collect();
    assert_eq!(
        pair_ids, expected_pairs,
        "one pair record per pair-registry row, in registry order"
    );
}

/// Every category, indicator, pair and parameter record carries its full key set.
#[test]
fn every_indicator_record_has_all_fields() {
    let doc = indicators_value();
    for category in doc["categories"].as_array().expect("array") {
        let name = category["name"].as_str().expect("name");
        assert_eq!(object_keys(category, name), CATEGORY_KEYS, "{name}: category keys");
        for record in category["indicators"].as_array().expect("array") {
            let id = record["id"].as_str().expect("id");
            assert_eq!(object_keys(record, id), INDICATOR_KEYS, "{id}: indicator keys");
            assert!(
                record["display"].as_str().is_some_and(|d| !d.is_empty()),
                "{id}: display is non-empty"
            );
            assert!(record["batch_only"].is_boolean(), "{id}: batch_only is a bool");
            for param in record["params"].as_array().expect("params is an array") {
                assert_eq!(object_keys(param, id), PARAM_SPEC_KEYS, "{id}: param keys");
                assert!(param["default"].is_number(), "{id}: a param default is a number");
            }
        }
    }
    for record in doc["pairs"].as_array().expect("array") {
        let id = record["id"].as_str().expect("id");
        assert_eq!(object_keys(record, id), PAIR_KEYS, "{id}: pair keys");
        for param in record["params"].as_array().expect("params is an array") {
            assert_eq!(object_keys(param, id), PARAM_SPEC_KEYS, "{id}: pair param keys");
        }
    }
}

/// `templates.json` carries exactly the three rosters, one record per `PORTABLE_STRATEGIES` /
/// `SIMULATOR_ONLY` / `SCRIPT_ONLY` entry, each in its table's order.
#[test]
fn templates_json_covers_all_three_strategy_rosters_in_order() {
    let doc = templates_value();
    assert_eq!(object_keys(&doc, "templates.json"), TEMPLATES_DOC_KEYS, "the roster set");
    let ids = |key: &str| -> Vec<&str> {
        doc[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} is an array"))
            .iter()
            .map(|r| r["id"].as_str().expect("id"))
            .collect()
    };
    assert_eq!(ids("portable"), PORTABLE_STRATEGIES.to_vec(), "portable, in roster order");
    let simulator_only: Vec<&str> = SIMULATOR_ONLY.iter().map(|&(id, _)| id).collect();
    assert_eq!(ids("simulator_only"), simulator_only, "simulator_only, in table order");
    let script_only: Vec<&str> = SCRIPT_ONLY.iter().map(|&(id, _)| id).collect();
    assert_eq!(ids("script_only"), script_only, "script_only, in table order");
}

/// Every portable record carries its full key set, a tagged `params` object whose payload matches
/// its tag, and a `live_capable` whose `reason` is present exactly when the verdict is `false`;
/// every simulator-only and script-only record carries a non-empty reason.
#[test]
fn every_template_record_has_all_fields() {
    let doc = templates_value();
    for record in doc["portable"].as_array().expect("array") {
        let id = record["id"].as_str().expect("id");
        assert_eq!(object_keys(record, id), TEMPLATE_KEYS, "{id}: template keys");
        let params = &record["params"];
        match params["kind"].as_str() {
            Some("declared") => {
                for key in params["keys"].as_array().expect("keys is an array") {
                    assert!(
                        key["name"].is_string() && key["kind"].is_string(),
                        "{id}: a declared key needs a name and a kind: {key}"
                    );
                }
            }
            Some("not-enumerated") => assert!(
                params["reason"].as_str().is_some_and(|r| !r.is_empty()),
                "{id}: not-enumerated carries a reason"
            ),
            other => panic!("{id}: params has an unknown kind tag {other:?}"),
        }
        let live = &record["live_capable"];
        let verdict = live["verdict"]
            .as_bool()
            .unwrap_or_else(|| panic!("{id}: live_capable.verdict is a bool"));
        assert_eq!(
            verdict,
            live["reason"].is_null(),
            "{id}: a false verdict carries a reason and a true one carries none"
        );
    }
    for roster in ["simulator_only", "script_only"] {
        for record in doc[roster].as_array().expect("array") {
            let id = record["id"].as_str().expect("id");
            assert_eq!(object_keys(record, id), REASONED_ROW_KEYS, "{roster}/{id}: row keys");
            assert!(
                record["reason"].as_str().is_some_and(|r| !r.is_empty()),
                "{roster}/{id}: reason is non-empty"
            );
        }
    }
}

// ── rosters.json ─────────────────────────────────────────────────────────────────────────────────

const ROSTERS_DOC_KEYS: &[&str] =
    &["asset_classes", "divergence_kinds", "events", "order_kinds", "store_kinds"];
const EVENT_KEYS: &[&str] = &["payload", "variant", "wire_tag"];
const ASSET_CLASS_KEYS: &[&str] = &["id", "picker_tab", "variant"];
const STORE_KIND_KEYS: &[&str] = &[
    "codec",
    "columns",
    "commit_keys",
    "grouped",
    "identity",
    "kind",
    "notes",
    "partition",
    "read_verb",
    "row",
    "schema_meta",
    "tick_lane",
    "write_verb",
];
const DIVERGENCE_KEYS: &[&str] = &["id", "origin", "policies", "resolves_to_events", "variant"];

/// The declared [`EVENTS`] table IS `vike_model::Event` — every variant, its payload type, and its
/// wire tag — parsed straight out of the enum's source.
///
/// This is the pin that lets `docs_data` declare the table at all. A wire tag is a
/// `#[serde(rename = "…")]` ATTRIBUTE: no runtime renderer can read one, and the enum has no value
/// to enumerate, so the table is written by hand and held equal to its authority here — the same
/// device as `latency_budget_matches_the_runtime_latency_gate`. A new variant, a renamed payload
/// or a changed wire tag reddens CI until the table is updated.
#[test]
fn event_table_matches_the_event_enum() {
    let src = read_repo("crates/vike-model/src/events.rs");
    let body = src
        .split_once("pub enum Event {")
        .unwrap_or_else(|| panic!("no `pub enum Event {{` in vike-model's events source"))
        .1
        .split_once("\n}")
        .expect("the enum body is closed by a line-start `}`")
        .0;

    // (variant, payload, wire tag) per variant: a `Variant(Payload),` line, with the wire tag
    // taken from the most recent `#[serde(rename = "…")]` when one sits directly above it.
    let mut parsed: Vec<(String, String, String)> = Vec::new();
    let mut pending_rename: Option<String> = None;
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("#[serde(rename = \"") {
            pending_rename = rest.split('"').next().map(str::to_string);
            continue;
        }
        if t.starts_with("//") || t.is_empty() {
            continue;
        }
        let Some((variant, rest)) = t.split_once('(') else {
            pending_rename = None;
            continue;
        };
        let Some(payload) = rest.split_once(')').map(|(p, _)| p) else {
            pending_rename = None;
            continue;
        };
        if !variant.chars().next().is_some_and(char::is_uppercase) {
            pending_rename = None;
            continue;
        }
        let wire = pending_rename.take().unwrap_or_else(|| variant.to_string());
        parsed.push((variant.to_string(), payload.to_string(), wire));
    }

    let declared: Vec<(String, String, String)> =
        EVENTS.iter().map(|&(v, p, w)| (v.to_string(), p.to_string(), w.to_string())).collect();
    assert_eq!(
        declared, parsed,
        "vike_docs::EVENTS drifted from `vike_model::Event` — update the table in \
         crates/vike-docs/src/lib.rs to match crates/vike-model/src/events.rs"
    );
}

/// `rosters.json` carries exactly the five rosters, each complete against its authority and in its
/// authority's order.
#[test]
fn rosters_json_covers_every_roster_in_authority_order() {
    let doc = rosters_value();
    assert_eq!(object_keys(&doc, "rosters.json"), ROSTERS_DOC_KEYS, "the roster set");

    let ids = |key: &str, field: &str| -> Vec<String> {
        doc[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} is an array"))
            .iter()
            .map(|r| r[field].as_str().expect("a string id").to_string())
            .collect()
    };

    let events: Vec<String> = EVENTS.iter().map(|&(v, _, _)| v.to_string()).collect();
    assert_eq!(ids("events", "variant"), events, "one record per Event variant, in enum order");

    assert_eq!(
        doc["order_kinds"]["all"].as_array().expect("an array").len(),
        ORDER_KINDS.len(),
        "every canonical order kind"
    );
    assert_eq!(
        doc["order_kinds"]["trigger"].as_array().expect("an array").len(),
        TRIGGER_KINDS.len(),
        "every trigger kind"
    );
    for kind in TRIGGER_KINDS {
        assert!(ORDER_KINDS.contains(kind), "{kind} is a trigger kind but not an order kind");
    }

    let store_kinds: Vec<String> = STORE_KINDS.iter().map(|k| k.kind.to_string()).collect();
    assert_eq!(ids("store_kinds", "kind"), store_kinds, "one record per STORE_KINDS row, in order");

    assert_eq!(
        doc["asset_classes"].as_array().expect("an array").len(),
        11,
        "every AssetClass variant"
    );
    assert_eq!(
        doc["divergence_kinds"].as_array().expect("an array").len(),
        9,
        "every DivergenceKind variant"
    );
}

/// Every record of every roster carries its full key set, and no id collides — including the one
/// slug exception that exists because a page keyed on `index` would overwrite its tree's landing
/// page.
#[test]
fn every_roster_record_has_all_fields_and_a_unique_slug() {
    let doc = rosters_value();
    for (key, expected) in [
        ("events", EVENT_KEYS),
        ("asset_classes", ASSET_CLASS_KEYS),
        ("store_kinds", STORE_KIND_KEYS),
        ("divergence_kinds", DIVERGENCE_KEYS),
    ] {
        let rows = doc[key].as_array().unwrap_or_else(|| panic!("{key} is an array"));
        for row in rows {
            assert_eq!(object_keys(row, key), expected, "{key}: record keys");
        }
    }

    for (key, field) in
        [("asset_classes", "id"), ("divergence_kinds", "id"), ("store_kinds", "kind")]
    {
        let mut ids: Vec<&str> = doc[key]
            .as_array()
            .expect("an array")
            .iter()
            .map(|r| r[field].as_str().expect("a string id"))
            .collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "{key}: duplicate {field}");
        assert!(
            !ids.contains(&"index"),
            "{key}: a member slugged `index` would shadow the tree's own index page"
        );
    }

    // The asset-class exception, asserted rather than implied: `AssetClass::Index` renders
    // `index-instrument`.
    let index_class = doc["asset_classes"]
        .as_array()
        .expect("an array")
        .iter()
        .find(|c| c["variant"] == "Index")
        .expect("AssetClass::Index is exported");
    assert_eq!(index_class["id"].as_str(), Some("index-instrument"));

    // Every store-kind column and commit key is a complete pair.
    for kind in doc["store_kinds"].as_array().expect("an array") {
        let id = kind["kind"].as_str().expect("kind");
        for col in kind["columns"].as_array().expect("columns is an array") {
            assert!(
                col["name"].is_string() && col["flavor"].is_string(),
                "{id}: a column needs a name and a flavor: {col}"
            );
        }
        for ck in kind["commit_keys"].as_array().expect("commit_keys is an array") {
            assert!(
                ck["producer"].is_string() && ck["template"].is_string(),
                "{id}: a commit key needs a producer and a template: {ck}"
            );
        }
        assert!(kind["grouped"].is_boolean() && kind["tick_lane"].is_boolean(), "{id}: flags");
    }
}

/// Every divergence kind carries a verdict for every policy, and each verdict is the one
/// `vike_exec::recon::mode_applies` gives — the export COMPUTES the fold-vs-hold answer rather
/// than restating a list.
///
/// This is the roster that exists because the opposite claim about `hybrid` was written down in
/// six places and was false in all six. The test re-asks the authority for every (kind, policy)
/// pair, so a rendered verdict cannot drift from the policy the runtime actually applies.
#[test]
fn every_divergence_verdict_is_computed_from_mode_applies() {
    let doc = rosters_value();
    let rows = doc["divergence_kinds"].as_array().expect("an array");
    for row in rows {
        let variant = row["variant"].as_str().expect("variant");
        let policies = row["policies"].as_object().expect("policies is an object");
        assert_eq!(policies.len(), POLICY_NAMES.len(), "{variant}: one verdict per policy");
        for name in POLICY_NAMES {
            let policy = ReconPolicy::from_policy_name(name).unwrap_or_else(|| {
                panic!("POLICY_NAMES lists {name}, from_policy_name refuses it")
            });
            let kind = ALL_DIVERGENCE_KINDS
                .iter()
                .copied()
                .find(|k| format!("{k:?}") == variant)
                .unwrap_or_else(|| panic!("{variant} is not a DivergenceKind"));
            let expected = if mode_applies(&policy, kind) { "fold" } else { "hold" };
            assert_eq!(
                policies[*name].as_str(),
                Some(expected),
                "{variant} under {name}: the export disagrees with mode_applies"
            );
        }
    }

    // The two properties the workspace's own guidance says keep being written down wrongly, held
    // here so a rendered page cannot repeat them: under `hybrid` exactly two kinds BOTH fold and
    // resolve to events, and `quarantine` folds nothing at all.
    let folds_and_resolves: Vec<&str> = rows
        .iter()
        .filter(|r| r["policies"]["hybrid"] == "fold" && r["resolves_to_events"] == true)
        .map(|r| r["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        folds_and_resolves,
        vec!["missing-fill", "position-drift"],
        "under hybrid exactly MissingFill and PositionDrift both auto-apply AND resolve to events"
    );
    assert!(
        rows.iter().all(|r| r["policies"]["quarantine"] == "hold"),
        "quarantine holds every kind"
    );
}

/// `ReconPolicy::from_policy_name` and `POLICY_NAMES` are exhaustive against each other, and each
/// name resolves to a DISTINCT policy — the pin that keeps the four presets one construction.
#[test]
fn every_policy_name_resolves_and_they_differ() {
    let policies: Vec<ReconPolicy> = POLICY_NAMES
        .iter()
        .map(|&n| {
            ReconPolicy::from_policy_name(n).unwrap_or_else(|| panic!("{n} does not resolve"))
        })
        .collect();
    assert!(
        ReconPolicy::from_policy_name("nonsense").is_none(),
        "an unknown name resolves to None"
    );
    assert!(
        ReconPolicy::from_policy_name("Hybrid").is_none(),
        "the match is exact — lower-casing is the caller's"
    );
    // Distinctness is asserted through the fold-vs-hold vector each policy produces over the
    // roster, which is what a consumer of this data actually sees.
    let verdicts: Vec<Vec<bool>> = policies
        .iter()
        .map(|p| ALL_DIVERGENCE_KINDS.iter().map(|&k| mode_applies(p, k)).collect())
        .collect();
    for i in 0..verdicts.len() {
        for j in (i + 1)..verdicts.len() {
            assert_ne!(
                verdicts[i], verdicts[j],
                "{} and {} produce the same verdicts over every kind",
                POLICY_NAMES[i], POLICY_NAMES[j]
            );
        }
    }
}

// ── rendered ⇒ attached ⇒ mirrored ───────────────────────────────────────────────────────────────
//
// Three files name the docs-data set and none of them can see the others: this module RENDERS it,
// `.github/workflows/release.yml` ATTACHES it to a private Release, and `scripts/publish_mirror.sh`
// re-publishes it onto the PUBLIC mirror, which is where the documentation site fetches from. The
// two tests below hold the second and third equal to the first.
//
// They exist because the chain silently broke. #1620 added `rosters.json` to `rendered_files` and
// to the site's page generators, and neither consumer list gained it: the asset was rendered at
// every release and attached to none, so the 58 generated `concepts/` pages it feeds stayed pinned
// to a committed snapshot while the other five moved. Nothing failed. The site's fetch is
// all-or-nothing precisely so its files cannot describe two different commits, and an asset that is
// never published is never the half that fails to fetch — the invariant was reached around, not
// broken.
//
// ⚠ `bins.json` is xtask's (`cargo run -p xtask -- docs-bins`), not this module's, so it is a
// literal below. A rename on that side is uncovered: the two crates cannot see each other, and a
// shared constant crate for one string would cost more than the hole it closes.

/// The docs-data set as every consumer must spell it: what this module renders, then xtask's.
fn expected_docs_data_assets() -> Vec<&'static str> {
    let mut names: Vec<&'static str> =
        rendered_files(DEFAULT_GENERATED_FROM).iter().map(|(name, _)| *name).collect();
    names.push("bins.json");
    // ⚠ A BARE LITERAL, like `bins.json` above: the renderer is
    // `crates/vike-cli/src/surface.rs`'s `rendered_files`, and this crate does not link it. A
    // NORMAL edge would point up — `crates/vike-cli/Cargo.toml` declares layer 30 against this
    // crate's 25 (this said it "declares the top layer" until 2026-09-28) — and
    // `crates/vike-ops/tests/layer_gate.rs` refuses that. This file needs only a DEV edge, though,
    // and that gate EXEMPTS dev edges, so rank decides nothing here: what keeps the edge out is
    // COST — it would compile vike-cli's whole library into this crate's test build to read one
    // string, as would a shared constant crate built for it. (This said layer_gate "fails the edge"
    // for this test until 2026-09-28, which is true of a normal edge only.) Declared here rather
    // than assumed away.
    names.push("cli.json");
    // ⚠ A BARE LITERAL, and since 2026-09-26 for the SAME reason as the one above. The renderer is
    // `crates/vike-backtest/src/profile_surface.rs`'s `rendered_files`, and that crate declares
    // layer 30 against this crate's 25 (this said 35 against 25 until 2026-09-28, and 45 against 35
    // until 2026-09-27; `vike-backtest` moved twice and this crate's own number was never 35) —
    // which refuses a NORMAL edge, while this test's dev edge would pass
    // `crates/vike-ops/tests/layer_gate.rs` and pull the simulator and harness into this crate's
    // test build to name one string.
    // ⚠ This said the edge was LEGAL while the gate lived in `vike-ops` (ranked above the engine),
    // and argued against it on cost alone. For this test's edge, cost is STILL the whole argument —
    // it said "the rank is simply the one that is enforced" until 2026-09-28, and the rank is not
    // enforced on a dev edge. The release copies the committed fixture that module's own gate holds
    // equal to its render, so the bytes are still derived from the type; only the spelling of the
    // name is duplicated here.
    names.push("profile.json");
    names
}

/// Read a repo-root-relative file that EVERY checkout carries, the public mirror included — so an
/// absence is a broken tree and panics, where [`repo_file`] below skips a withheld one.
///
/// ⚠ Spelled as one literal at every call site, never assembled from `join` segments: that literal
/// is what `crates/vike-ops/tests/ci_plan_gate.rs` reads to hold `DOCS_DATA_GATE_INPUTS` equal to
/// the files this gate actually reads.
fn read_repo(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Read a repo-root-relative file, or `None` where it is absent.
///
/// `.github/` and `scripts/` are both withheld from the public source mirror, so a checkout of the
/// mirror has neither file to read. These gates are for the private repository; skipping loudly
/// there beats failing a tree that was never meant to contain the thing being gated.
fn repo_file(rel: &str) -> Option<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join(rel);
    match std::fs::read_to_string(&p) {
        Ok(text) => Some(text),
        Err(_) => {
            eprintln!(
                "SKIPPED: {} is absent — this gate runs in the private repository only",
                p.display()
            );
            None
        }
    }
}

/// The one line in `file` whose trimmed form starts with `prefix` and mentions a `.json`, with that
/// prefix and the closing paren stripped — the shape both lists happen to share.
fn sole_asset_list<'a>(text: &'a str, prefix: &str, what: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(prefix) && l.contains(".json"))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "{what} carries exactly one `{prefix}…` list naming .json assets; found {}",
        lines.len()
    );
    lines[0].trim_start_matches(prefix).trim_end_matches(')').split_whitespace().collect()
}

/// `.github/workflows/release.yml` attaches exactly what is rendered, plus xtask's `bins.json`.
///
/// Rendering an asset and shipping it are two different lists, and until this gate they were free
/// to disagree.
#[test]
fn the_release_attaches_every_rendered_asset() {
    let Some(text) = repo_file(".github/workflows/release.yml") else { return };
    let attached = sole_asset_list(&text, "ASSETS+=(", "release.yml");
    assert_eq!(
        attached,
        expected_docs_data_assets(),
        "the docs-data assets release.yml attaches must be exactly what `rendered_files` renders, \
         plus xtask's bins.json, in that order. Add the missing name to the `ASSETS+=(` line that \
         carries the .json set — an asset rendered but not attached reaches no reader."
    );
}

/// `scripts/publish_mirror.sh` re-publishes exactly the same set onto the public mirror.
///
/// The private Release is not what the documentation site reads: it reads the MIRROR's latest
/// release. An asset that stops at the private release is as invisible to a reader as one that was
/// never rendered, and just as silent — the site falls back to its committed snapshot without
/// erroring.
///
/// ⚠ Since 2026-09-05 that script ALSO copies every binary the private release's `SHA256SUMS`
/// names — DERIVED from the manifest, never listed — so `RELEASE_ASSETS` is no longer "what the
/// mirror release carries". It is the subset the documentation site depends on BY NAME, whose
/// absence must be a refusal rather than whatever the manifest happened to hold that day: a
/// manifest-derived set cannot notice that an asset is missing from it. That is why the list
/// survives the derivation, and why this gate still holds it equal to the renderer's.
#[test]
fn the_mirror_publishes_every_released_asset() {
    let Some(text) = repo_file("scripts/publish_mirror.sh") else { return };
    let published = sole_asset_list(&text, "readonly RELEASE_ASSETS=(", "publish_mirror.sh");
    assert_eq!(
        published,
        expected_docs_data_assets(),
        "publish_mirror.sh's RELEASE_ASSETS must name exactly what release.yml attaches. An asset \
         missing here never reaches the public mirror, so the documentation site keeps rendering \
         from its committed snapshot — with no error anywhere, indefinitely."
    );
}

/// No rendered asset may cite a path the public source mirror withholds.
///
/// These files are not internal notes: every string in them is rendered onto a page at
/// `vike.io/docs/trader/`, for a reader whose only view of this workspace is the mirror. The mirror
/// publishes `crates/`, `xtask/`, `fixtures/`, `assets/`, `deploy/docker/` and the root manifests —
/// so citing those by path and symbol is CORRECT and is what makes a claim checkable. It publishes
/// no `CLAUDE.md`, no `docs/`, no `scripts/`, no `.github/` and no `justfile`, so a citation to one
/// of those is a dead link on a public page.
///
/// One had already shipped: `SCRIPT_ONLY`'s `rhai` gloss ended "per
/// docs/decisions/0024-rhai-strategies-live.md", which reached `templates.json`, and from there the
/// generated `strategies/simulator-only` page. A doc COMMENT may cite a decision record — that is
/// how this workspace argues with itself, and the comment stays. A string that is EXPORTED may not.
///
/// ⚠ This gate reads only what this module renders. It cannot see prose the site's own generators
/// add around these values, and it is not a substitute for the citation discipline in the docs
/// repository — it closes the one path that leads from a Rust string onto a public page unread.
#[test]
fn no_rendered_asset_cites_a_path_the_mirror_withholds() {
    const WITHHELD: &[&str] = &["CLAUDE.md", "justfile", ".github/", "scripts/", "docs/"];

    let mut offenders: Vec<String> = Vec::new();
    for (name, contents) in rendered_files(DEFAULT_GENERATED_FROM).iter() {
        for needle in WITHHELD {
            let mut from = 0;
            while let Some(at) = contents[from..].find(needle) {
                let start = from + at;
                let end = (start + 80).min(contents.len());
                let excerpt = contents[start..end].replace('\n', " ");
                offenders.push(format!("{name}: …{excerpt}…"));
                from = start + needle.len();
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these exported strings cite paths the public source mirror does not publish, so they \
         render as dead links on vike.io/docs/. Rewrite the exported string — the fact usually \
         stands without the citation — and keep the reasoning in a doc comment, which stays \
         internal:\n{}",
        offenders.join("\n")
    );
}
