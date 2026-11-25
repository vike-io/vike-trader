//! **A strategy file predicts with a study's model, through the production route** (W32). A real hub reads
//! `<project>/user_data/runs/<id>/model.txt` for the request's `type = "model"` key, hands the text to its
//! `vike-backend strategy-sandbox backtest` child in the start message, and the jailed child builds the file
//! with the text where the request wrote the id. The probe file's `refuse` answers what it was handed:
//!
//! - the text's length and FNV-1a hash, so the run's refusal says whether the bytes crossed whole;
//! - for the rows its `rows` key names, the probabilities `vike_ml_prediction` predicts INSIDE the jail, which
//!   must equal, bit for bit, what [`GbdtModel`] predicts here on the same committed fixture model.
//!
//! A run id with no run behind it is refused by the hub before any child starts.
//!
//! One `#[test]` for the phases: the hub resolves its folders from the process's working directory.
//!
//! ```text
//! LANE=the build runner MSYS_NO_PATHCONV=1 just the CI box <branch> cargo test -p vike-backend --test strategy_file_model
//! ```
#![cfg(all(feature = "vike-strategy-sandbox", target_os = "linux"))]

// The shared suite support: this binary uses the build, the stage, the store and the exchange, not the
// shared Pulse file, so its other items are unused here.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/strategy_file.rs"]
mod strategy_file;

use std::path::{Path, PathBuf};

use vike_backtesthub::test_support::spawn_hub_over;
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::wire_studio::StrategySpecWire;
use vike_ml_prediction::{GbdtModel, parse_model_text};

use strategy_file::{
    ask, build_text, sandbox_children_once_reaped, seeded_store, slice, stage,
    use_the_multicall_as_the_sandbox,
};

/// The probe: a `type = "model"` key, a `rows` key, and a `refuse` that answers what it was handed.
const PROBE: &str = r#"//! Model probe: refuses with what its `model` key handed it (the W32 suite's strategy file).
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//!
//! [params.model]
//! type = "model"
//! [params.rows]
//! type = "text"
//! ```

use vike_model::OrderBroker;

pub struct Probe;

impl<B: OrderBroker> vike_model::Strategy<B> for Probe {}

pub fn build<B: vike_model::OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(Probe)
}

/// The model text's length and FNV-1a hash, and, for `rows` (`;`-separated rows of `,`-separated f64 bit
/// patterns in hex), the probability of each row as a bit pattern.
pub fn refuse(params: &vike_model::params::Params) -> Option<String> {
    let text = params.get("model").and_then(|v| v.as_str()).unwrap_or_default();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    let mut said = format!("model probe: {} bytes, fnv1a {hash:016x}", text.len());
    if let Some(rows) = params.get("rows").and_then(|v| v.as_str()) {
        match vike_ml_prediction::parse_model_text(text) {
            Ok(model) => {
                let probabilities: Vec<String> = rows
                    .split(';')
                    .map(|row| {
                        let x: Vec<f64> = row
                            .split(',')
                            .map(|h| f64::from_bits(u64::from_str_radix(h, 16).unwrap_or(0)))
                            .collect();
                        format!("{:016x}", model.predict_proba(&x).to_bits())
                    })
                    .collect();
                said.push_str(&format!(", probabilities {}", probabilities.join(",")));
            }
            Err(e) => said.push_str(&format!(", the model does not parse: {e}")),
        }
    }
    Some(said)
}
"#;

/// The probe's path under `strategies/`.
const PROBE_FILE: &str = "other/model_probe.rs";

/// The model text the first run holds: CRLF, a tab and non-ASCII, so a crossing that rewrote anything shows.
const MODEL: &str = "tree\r\nversion=v4\tµ\nend of trees\n";

/// The run holding [`MODEL`].
const RUN: &str = "1763251200-study-0";

/// The run holding `vike-ml-prediction`'s committed real model.
const GOLDEN_RUN: &str = "1763251200-golden-0";

/// `vike-ml-prediction`'s committed fixture folder: a real LightGBM model with a categorical split, and the
/// rows its golden test predicts.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../vike-ml-prediction/tests/fixtures")
}

/// FNV-1a over `text`, the probe's own fold.
fn fnv1a(text: &str) -> u64 {
    text.bytes()
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// A finished study run `id` under `runs` holding `model`: the artifact, then the manifest.
fn plant_a_run(runs: &Path, id: &str, model: &str) {
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

/// The fixture's feature rows (every column but the last, LightGBM's own probability), as f64 bit patterns.
fn fixture_rows() -> Vec<Vec<u64>> {
    let text = std::fs::read_to_string(fixtures().join("real_binary_categorical_expected.tsv"))
        .expect("the golden rows");
    let rows: Vec<Vec<u64>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let mut fields: Vec<u64> = l
                .split('\t')
                .filter(|t| !t.is_empty())
                .map(|t| u64::from_str_radix(t, 16).expect("a hex bit pattern"))
                .collect();
            fields.pop();
            fields
        })
        .collect();
    assert!(rows.len() > 10, "the fixture holds its rows: {}", rows.len());
    rows
}

fn run_with(params_toml: String) -> Request {
    Request::RunSlice {
        spec: StrategySpecWire::File { file: PROBE_FILE.to_string(), params_toml, sdk: None },
        slice: slice(),
        params: None,
    }
}

fn refusal(response: Response) -> String {
    match response {
        Response::Error(why) => why,
        other => panic!("the probe refuses every run: {other:?}"),
    }
}

#[test]
fn a_strategy_file_predicts_in_the_jail_what_the_model_predicts_here() {
    let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
    let artifact = build_text(PROBE_FILE, PROBE)
        .unwrap_or_else(|e| panic!("the production builder must build the probe:\n{e}"));
    let golden = std::fs::read_to_string(fixtures().join("real_binary_categorical_v4.txt"))
        .expect("the committed real model");
    let project = tempfile::tempdir().expect("project root");
    std::fs::create_dir_all(project.path().join("settings")).expect("plant the project marker");
    let strategies = project.path().join("strategies");
    let file = strategies.join(PROBE_FILE);
    std::fs::create_dir_all(file.parent().expect("a folder")).expect("plant strategies/other");
    std::fs::write(&file, PROBE).expect("write the probe");
    let runs = project.path().join("user_data").join("runs");
    plant_a_run(&runs, RUN, MODEL);
    plant_a_run(&runs, GOLDEN_RUN, &golden);
    let restore = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(project.path()).expect("enter the project");
    stage(&artifact, &vike_backtesthub::studio_run::strategy_files_dir());
    use_the_multicall_as_the_sandbox();
    let address = spawn_hub_over(seeded_store(50), strategies);

    // The bytes cross whole.
    let said = refusal(ask(address, &run_with(format!("model = \"{RUN}\"\n"))));
    let crossed = format!("model probe: {} bytes, fnv1a {:016x}", MODEL.len(), fnv1a(MODEL));
    assert!(said.contains(&crossed), "the file must be handed the run's text whole: {said}");

    // The jailed file predicts what the model predicts in this process, bit for bit.
    let rows = fixture_rows();
    let model: GbdtModel = parse_model_text(&golden).expect("the fixture parses here");
    let here: Vec<String> = rows
        .iter()
        .map(|row| {
            let x: Vec<f64> = row.iter().map(|b| f64::from_bits(*b)).collect();
            format!("{:016x}", model.predict_proba(&x).to_bits())
        })
        .collect();
    let rows_text: Vec<String> = rows
        .iter()
        .map(|row| row.iter().map(|b| format!("{b:016x}")).collect::<Vec<_>>().join(","))
        .collect();
    let params = format!("model = \"{GOLDEN_RUN}\"\nrows = \"{}\"\n", rows_text.join(";"));
    let said = refusal(ask(address, &run_with(params)));
    let expected = format!(
        "model probe: {} bytes, fnv1a {:016x}, probabilities {}",
        golden.len(),
        fnv1a(&golden),
        here.join(",")
    );
    assert!(
        said.contains(&expected),
        "the file's probabilities in the jail must equal GbdtModel's here, row for row:\n  said: \
         {said}\n  want: {expected}"
    );
    println!("{} rows predicted in the jail, equal bit for bit", here.len());

    // A run id with no run behind it is the hub's refusal, before any child.
    let gone = refusal(ask(address, &run_with("model = \"1763251200-gone-0\"\n".to_string())));
    assert!(
        gone.contains("`model` names model run \"1763251200-gone-0\"") && !gone.contains("probe"),
        "a run id with no run is the hub's refusal, before any child: {gone}"
    );
    assert!(sandbox_children_once_reaped().is_empty(), "no child outlives the request it served");
    std::env::set_current_dir(restore).expect("restore the working directory");
}
