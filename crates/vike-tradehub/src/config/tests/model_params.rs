//! A `type = "model"` key on a mount: the file is opened with the run's model TEXT where the profile wrote
//! the run id (`crate::config::sandboxed::params_with_models`).

use vike_model::params::{ParamValue, Params};
use vike_strategy::file::StrategyFileHeader;

use crate::config::sandboxed::params_with_models;

const HEADER: &str = "[strategy]\nruns_on = \"live\"\nwhy = \"a test\"\n\n[params.model]\ntype = \
                      \"model\"\n[params.size]\ntype = \"number\"\n";

fn finished_run(runs: &std::path::Path, id: &str, model: &str) {
    let dir = runs.join(id);
    std::fs::create_dir_all(&dir).expect("run dir");
    std::fs::write(dir.join(vike_runs::MODEL_ARTIFACT), model).expect("model");
    let manifest = format!(
        "{{\"run_id\":\"{id}\",\"kind\":\"study\",\"produced_by\":\"study\",\"started_at\":\
         \"2026-11-16T00:00:00Z\",\"finished_at\":\"2026-11-16T00:00:01Z\",\"git_sha\":null,\
         \"config\":{{}},\"detail\":{{}}}}"
    );
    std::fs::write(dir.join(vike_runs::MANIFEST_FILE), manifest).expect("manifest");
}

#[test]
fn the_file_is_opened_with_the_runs_model_text_and_the_mount_keeps_the_id() {
    let runs = tempfile::tempdir().expect("runs root");
    finished_run(runs.path(), "1763251200-study-0", "tree\nversion=v4\nend of trees\n");
    let header = StrategyFileHeader::from_toml(HEADER).expect("header");
    let params: Params =
        toml::from_str("model = \"1763251200-study-0\"\nsize = 2.0\n").expect("params");
    let opened = params_with_models(&header, &params, runs.path()).expect("resolved");
    assert_eq!(
        opened.get("model").and_then(ParamValue::as_str),
        Some("tree\nversion=v4\nend of trees\n")
    );
    assert_eq!(opened.get("size"), params.get("size"));
    assert_eq!(params.get("model").and_then(ParamValue::as_str), Some("1763251200-study-0"));
}

#[test]
fn a_model_run_that_cannot_be_read_refuses_the_mount_naming_it() {
    let runs = tempfile::tempdir().expect("runs root");
    let header = StrategyFileHeader::from_toml(HEADER).expect("header");
    let params: Params = toml::from_str("model = \"1763251200-gone-0\"\n").expect("params");
    let why = params_with_models(&header, &params, runs.path()).expect_err("refused");
    assert!(why.starts_with("`model` names model run \"1763251200-gone-0\": no "), "{why}");
}
