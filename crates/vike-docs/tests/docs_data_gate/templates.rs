//! `templates.json`: every shipped strategy file on exactly one roster, in path order, every record
//! complete and read from the file's header.

use vike_docs::templates_value;
use vike_strategy::file::StrategyFileRunsOn;

use super::{ids, object_keys, shipped};

/// The key sets of `templates.json` and its records — exact, sorted, as `venues::RECORD_KEYS`.
const TEMPLATES_DOC_KEYS: &[&str] = &["portable", "script_only", "simulator_only"];
const TEMPLATE_KEYS: &[&str] = &["id", "live_capable", "params"];
const PARAM_KEY_KEYS: &[&str] = &["default", "kind", "name"];
const REASONED_ROW_KEYS: &[&str] = &["id", "reason"];

/// `templates.json` carries the three rosters: `simulator_only` is every file whose header says
/// `runs_on = "backtest"`, `portable` every other file, each in the scan's path order, and
/// `script_only` is empty (no file is the by-name inline-script arm it listed).
#[test]
fn templates_json_puts_every_shipped_file_on_one_roster_in_path_order() {
    let shipped = shipped();
    let doc = templates_value(&shipped);
    assert_eq!(object_keys(&doc, "templates.json"), TEMPLATES_DOC_KEYS, "the roster set");
    let on = |backtest: bool| {
        shipped
            .iter()
            .filter(|(_, h)| (h.strategy.runs_on == StrategyFileRunsOn::Backtest) == backtest)
            .map(|(path, _)| path.as_str())
            .collect::<Vec<&str>>()
    };
    let (portable, simulator_only) = (on(false), on(true));
    // Non-vacuity: a roster both of whose halves are empty would pass every equality below.
    assert!(!portable.is_empty() && !simulator_only.is_empty(), "both rosters hold a file");
    assert_eq!(ids(&doc["portable"], "id"), portable, "portable, in path order");
    assert_eq!(ids(&doc["simulator_only"], "id"), simulator_only, "simulator_only, in path order");
    assert!(doc["script_only"].as_array().expect("array").is_empty(), "script_only is empty");
}

/// Every portable record carries its full key set, a `declared` params object listing the header's
/// keys in its order (name, kind, default), and a `live_capable` whose verdict is the header's
/// `runs_on = "live"` and whose `reason` is present exactly when the verdict is `false` (the
/// header's `why` when it states one); every simulator-only record carries a non-empty reason.
#[test]
fn every_template_record_has_all_fields() {
    let shipped = shipped();
    let doc = templates_value(&shipped);
    let header_of = |id: &str| &shipped.iter().find(|(path, _)| path == id).expect("shipped").1;
    for record in doc["portable"].as_array().expect("array") {
        let id = record["id"].as_str().expect("id");
        let header = header_of(id);
        assert_eq!(object_keys(record, id), TEMPLATE_KEYS, "{id}: template keys");
        let params = &record["params"];
        assert_eq!(params["kind"].as_str(), Some("declared"), "{id}: a header declares its keys");
        let keys = params["keys"].as_array().expect("keys is an array");
        let names: Vec<&str> = keys.iter().map(|k| k["name"].as_str().expect("name")).collect();
        let declared: Vec<&str> = header.params.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, declared, "{id}: the header's keys, in its order");
        for key in keys {
            assert_eq!(object_keys(key, id), PARAM_KEY_KEYS, "{id}: a declared key's fields");
            assert!(key["kind"].is_string(), "{id}: a declared key needs a kind: {key}");
        }
        let live = &record["live_capable"];
        let verdict = live["verdict"]
            .as_bool()
            .unwrap_or_else(|| panic!("{id}: live_capable.verdict is a bool"));
        assert_eq!(verdict, header.strategy.runs_on == StrategyFileRunsOn::Live, "{id}: verdict");
        assert_eq!(
            verdict,
            live["reason"].is_null(),
            "{id}: a false verdict carries a reason and a true one carries none"
        );
        if let (false, Some(why)) = (verdict, &header.strategy.why) {
            assert_eq!(live["reason"].as_str(), Some(why.as_str()), "{id}: the header's why");
        }
    }
    for record in doc["simulator_only"].as_array().expect("array") {
        let id = record["id"].as_str().expect("id");
        assert_eq!(object_keys(record, id), REASONED_ROW_KEYS, "simulator_only/{id}: row keys");
        assert!(
            record["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "simulator_only/{id}: reason is non-empty"
        );
    }
}
