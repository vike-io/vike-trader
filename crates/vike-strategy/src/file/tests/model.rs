//! A `type = "model"` key: the run id in the table, the run's model text in the file.

use vike_model::params::{ParamValue, Params};

use crate::file::StrategyFileHeader;

const HEADER: &str =
    "[strategy]\n\n[params.model]\ntype = \"model\"\n[params.every]\ntype = \"integer\"\n";

fn header() -> StrategyFileHeader {
    StrategyFileHeader::from_toml(HEADER).expect("a model key parses")
}

fn params(text: &str) -> Params {
    toml::from_str(text).expect("params")
}

#[test]
fn the_run_id_is_replaced_by_the_text_and_nothing_else_moves() {
    let table = params("model = \"1700000000-abc-0\"\nevery = 3\n");
    let mut asked = Vec::new();
    let resolved = header()
        .with_model_texts(&table, |run| {
            asked.push(run.to_string());
            Ok("tree\nversion=v4\n".to_string())
        })
        .expect("resolved");
    assert_eq!(asked, ["1700000000-abc-0"]);
    assert_eq!(resolved.get("model").and_then(ParamValue::as_str), Some("tree\nversion=v4\n"));
    assert_eq!(resolved.get("every"), table.get("every"));
    assert_eq!(resolved.len(), table.len());
}

#[test]
fn a_header_with_no_model_key_hands_the_table_back_unchanged() {
    let plain = StrategyFileHeader::from_toml("[strategy]\n\n[params.model]\ntype = \"text\"\n")
        .expect("parses");
    let table = params("model = \"1700000000-abc-0\"\n");
    let resolved =
        plain.with_model_texts(&table, |_| panic!("no model key: nothing is read")).expect("ok");
    assert_eq!(resolved, table);
    assert_eq!(plain.model_keys().count(), 0);
}

#[test]
fn an_absent_model_key_is_not_read_and_a_refusal_names_the_key_and_the_run() {
    let absent = header().with_model_texts(&params("every = 3\n"), |_| panic!("absent: not read"));
    assert!(absent.expect("ok").get("model").is_none());
    let refused = header()
        .with_model_texts(&params("model = \"9-x-0\"\n"), |_| Err("no model.txt".to_string()))
        .expect_err("refused");
    assert_eq!(refused, "`model` names model run \"9-x-0\": no model.txt");
}

#[test]
fn a_model_key_takes_a_string_and_nothing_else() {
    let h = header();
    assert!(h.mistyped_params(&params("model = \"1700000000-abc-0\"\n")).is_empty());
    let bad = h.mistyped_params(&params("model = 7\n"));
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].to_string(), "`model` wants a run id (a string), got integer");
    let ranged = "[strategy]\n\n[params.model]\ntype = \"model\"\nrange = [0, 1]\n";
    assert!(StrategyFileHeader::from_toml(ranged).is_err(), "a range is for a number");
}
