//! `templates.json`: the three strategy rosters, in order, every record complete.

use vike_docs::templates_value;
use vike_strategy::{PORTABLE_STRATEGIES, SCRIPT_ONLY, SIMULATOR_ONLY};

use super::{ids, object_keys};

/// The key sets of `templates.json` and its records — exact, sorted, as `venues::RECORD_KEYS`.
const TEMPLATES_DOC_KEYS: &[&str] = &["portable", "script_only", "simulator_only"];
const TEMPLATE_KEYS: &[&str] = &["id", "live_capable", "params"];
const REASONED_ROW_KEYS: &[&str] = &["id", "reason"];

/// `templates.json` carries exactly the three rosters, one record per `PORTABLE_STRATEGIES` /
/// `SIMULATOR_ONLY` / `SCRIPT_ONLY` entry, each in its table's order.
#[test]
fn templates_json_covers_all_three_strategy_rosters_in_order() {
    let doc = templates_value();
    assert_eq!(object_keys(&doc, "templates.json"), TEMPLATES_DOC_KEYS, "the roster set");
    assert_eq!(
        ids(&doc["portable"], "id"),
        PORTABLE_STRATEGIES.to_vec(),
        "portable, in roster order"
    );
    let simulator_only: Vec<&str> = SIMULATOR_ONLY.iter().map(|&(id, _)| id).collect();
    assert_eq!(ids(&doc["simulator_only"], "id"), simulator_only, "simulator_only, in table order");
    let script_only: Vec<&str> = SCRIPT_ONLY.iter().map(|&(id, _)| id).collect();
    assert_eq!(ids(&doc["script_only"], "id"), script_only, "script_only, in table order");
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
