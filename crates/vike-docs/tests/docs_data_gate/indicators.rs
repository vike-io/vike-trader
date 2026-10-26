//! `indicators.json`: the whole category set and every registry row exactly once.

use vike_docs::indicators_value;
use vike_indicators::{pair_registry, registry as indicator_registry};

use super::object_keys;

/// The key sets of `indicators.json`'s records — exact, sorted, for the same reason as
/// `venues::RECORD_KEYS`.
const CATEGORY_KEYS: &[&str] = &["indicators", "name"];
const INDICATOR_KEYS: &[&str] = &["batch_only", "display", "id", "params"];
const PAIR_KEYS: &[&str] = &["display", "id", "params"];
const PARAM_SPEC_KEYS: &[&str] = &["default", "max", "min", "name", "step"];
/// The PUBLISHED name of every `vike_indicators::Category` variant, kebab-case, sorted — the exact
/// category set `indicators.json` carries, EMPTY categories included: the rendered set must equal
/// this list. Deliberately NOT derived from `Category::ALL`, which the renderer walks: this is the
/// independent pin of the published slugs, so a variant added to `Category::ALL` or a renamed slug
/// fails here until the published name is written down.
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

    // `super::` because this test's own `ids` above shadows the helper.
    let pair_ids = super::ids(&doc["pairs"], "id");
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
