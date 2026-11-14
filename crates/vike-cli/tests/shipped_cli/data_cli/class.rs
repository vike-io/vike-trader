//! `data hist ls --class`: the recorded asset class, and its refusal on the sibling read verbs.

use super::support::spawn_class_probe_datahub;
use super::*;

/// **The READ `vike_model::SymbolProperties::asset_class` did not have**, end to end: a venue
/// producer's recorded class travels store → `properties_as_of` → wire → the operator's table.
///
/// ⚠ **What this case is FOR.** Until `--class`, `git grep` found no production code in the
/// workspace that read that field back — `scan_symbol_properties` was called from the bridges'
/// `filters_rec.rs` TEST modules and nowhere else. A stored field nothing reads is a field that
/// goes wrong in silence: a venue writing the wrong class, or writing none, is invisible. So the
/// assertion that matters most here is not that `CryptoPerp` arrives — it is that the two ways of
/// having NO class arrive as two different words.
#[test]
fn the_recorded_asset_class_reaches_the_operator_and_its_absences_are_two_words() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_class_probe_datahub().to_string();

    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--class", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["class_requested"], true);
    let series = doc["series"].as_array().expect("an array of rows");
    assert_eq!(series.len(), 3, "the fixture's three instruments: {series:?}");

    // Indexed by venue rather than by position: the enumeration's sort order is the store's
    // business, and a case that encoded it would fail on a store that sorted differently.
    let by_venue = |venue: &str| -> &serde_json::Value {
        series.iter().find(|s| s["venue"] == venue).unwrap_or_else(|| panic!("{venue} is a row"))
    };

    let okx = by_venue("okx");
    assert_eq!(okx["asset_class_status"], "classified");
    assert_eq!(
        okx["asset_class"], "CryptoPerp",
        "the LATER of the two recorded rows — the probe is as-of now, not as-of the first row"
    );

    let bybit = by_venue("bybit");
    assert_eq!(bybit["asset_class_status"], "unclassified", "a grid was recorded, naming no class");
    assert!(bybit["asset_class"].is_null(), "…so there is no word to carry: {bybit}");

    let binance = by_venue("binance");
    assert_eq!(binance["asset_class_status"], "unrecorded", "no properties row at all");
    assert!(binance["asset_class"].is_null());

    // Nothing failed, so no row carries a reason.
    for row in series {
        assert!(row["asset_class_error"].is_null(), "{row}");
    }

    // ...and the human table, where the operator actually reads it.
    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr, "--class"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("CLASS"), "the column has a header: {text}");
    assert!(text.contains("CryptoPerp"), "{text}");
    assert!(text.contains("unclassified"), "the producer-not-wired signal: {text}");
    assert!(text.contains("no-properties"), "…and the never-recorded one, distinctly: {text}");

    // WITHOUT the flag the column is not there — the probe is opt-in, and a listing that paid for
    // it unasked would cost one round trip per instrument on every `data list`.
    let out = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("CLASS"), "{}", stdout(&out));
    assert!(!stdout(&out).contains("CryptoPerp"), "{}", stdout(&out));
}

/// `--class` is refused BY NAME on the three read siblings, each with its own reason — never
/// silently ignored, which is this verb's standing rule for a flag that does not apply.
///
/// ⚠ No datahub is spawned and none is needed: the refusal is a PARSE result, so it lands before a
/// socket is opened. That is the property worth having — a flag that could not be honoured must not
/// cost a connection to discover.
#[test]
fn the_class_flag_is_refused_on_the_sibling_read_verbs() {
    let scratch = tempfile::tempdir().expect("tempdir");

    for sub in ["coverage", "health", "universe"] {
        let out = run(scratch.path(), &["data", "hist", sub, "--class"]);
        assert_eq!(out.status.code(), Some(2), "a bad command line is the usage rung: {sub}");
        let err = stderr(&out);
        assert!(err.contains("--class"), "{sub} must name the flag it refused: {err}");
        assert!(err.contains("data hist ls --class"), "…and where the class is: {err}");
    }
}
