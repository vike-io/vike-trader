//! `rosters.json`: each roster complete in authority order, unique slugs, COMPUTED verdicts.

use vike_data::store::store_kind::STORE_KINDS;
use vike_docs::{EVENTS, rosters_value};
use vike_exec::recon::{DivergenceKind, POLICY_NAMES, ReconPolicy, mode_applies};
use vike_model::AssetClass;
use vike_model::venues::venue_caps::{ORDER_KINDS, TRIGGER_KINDS};

use super::{ids, object_keys};

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

/// `rosters.json` carries exactly the five rosters, each complete against its authority and in its
/// authority's order.
#[test]
fn rosters_json_covers_every_roster_in_authority_order() {
    let doc = rosters_value();
    assert_eq!(object_keys(&doc, "rosters.json"), ROSTERS_DOC_KEYS, "the roster set");

    let events: Vec<&str> = EVENTS.iter().map(|&(v, _, _)| v).collect();
    assert_eq!(
        ids(&doc["events"], "variant"),
        events,
        "one record per Event variant, in enum order"
    );

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

    let store_kinds: Vec<&str> = STORE_KINDS.iter().map(|k| k.kind).collect();
    assert_eq!(
        ids(&doc["store_kinds"], "kind"),
        store_kinds,
        "one record per STORE_KINDS row, in order"
    );

    assert_eq!(
        doc["asset_classes"].as_array().expect("an array").len(),
        AssetClass::ALL.len(),
        "every AssetClass variant"
    );
    assert_eq!(
        doc["divergence_kinds"].as_array().expect("an array").len(),
        DivergenceKind::ALL.len(),
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
        let mut ids = ids(&doc[key], field);
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
            let kind = DivergenceKind::ALL
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
        .map(|p| DivergenceKind::ALL.iter().map(|&k| mode_applies(p, k)).collect())
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
