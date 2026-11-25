//! **A strategy file runs a neural network, through the production route** (W6). A real hub reads
//! `<project>/user_data/runs/<id>/model.onnx` for the request's `type = "model"` key and hands its bytes, as
//! base64 text, to its `vike-backend strategy-sandbox backtest` child in the start message; the jailed child
//! builds the file with that text where the request wrote the id. The probe file's `refuse` loads the network
//! with `vike_ml_onnx_prediction` INSIDE the jail and asks it through the one prediction trait
//! (`vike_ml_prediction::Predictor`, as a gradient-boosted model is asked), for the rows its `rows` key names;
//! the answers must equal, bit for bit, what the same predictor answers here on the same committed fixture
//! (same machine, same kernels: the crate doc's promise). The same probe runs DeepLOB, the order-book
//! network, from `vike-ml-neural-training`'s committed fixture (its convolutions, max pool and softmax).
//!
//! **The ONNX runtime is opt-in per file.** The production builder also builds a probe that does NOT name the
//! crate: the two land in different build directories, and only the network probe's compiled the runtime
//! (`vike_strategy_builder::render`'s `Kind::of_strategy`). Both build into a fresh builds directory, so
//! each runs cargo in its work directory right before the check (a cached artifact is served without one).
//! Both artifacts' sizes and build times are printed: on a lane where this template version was never
//! built, those are its first builds.
//!
//! ```text
//! LANE=the build runner MSYS_NO_PATHCONV=1 just the latency box <branch> cargo test -p vike-backend --test strategy_file_onnx -- --nocapture
//! ```
//!
//! **W25's measurement lives here too, ignored.** `the_compressed_networks_answer_in_the_jail_and_how_fast`
//! takes the graphs `vike-ml-neural-training`'s `measure_the_compressed_networks` wrote (float, int8 and a
//! distilled student, for an MLP, an LSTM and a TCN), plants each as a run, and has a jailed strategy file
//! time its own predictions; it prints each graph's median and 99th percentile per prediction, jailed and in
//! this process. Run both in release, the first one first, on one lane (they share cargo's per-target
//! temporary directory):
//!
//! ```text
//! cargo test --release -p vike-ml-neural-training --test integration -- --ignored --nocapture compression::
//! cargo test --release -p vike-backend --test strategy_file_onnx -- --ignored --nocapture
//! ```
#![cfg(all(feature = "vike-strategy-sandbox", target_os = "linux"))]

// The shared suite support: this binary uses the build, the stage, the store and the exchange, not the
// shared Pulse file, so its other items are unused here.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/strategy_file.rs"]
mod strategy_file;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use vike_backtesthub::test_support::spawn_hub_over;
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::wire_studio::StrategySpecWire;
use vike_ml_onnx_prediction::OnnxModel;
use vike_ml_prediction::{PredictionKind, Predictor};
use vike_rust_strategy::sdk::TemplateKind;
use vike_strategy_builder::render::{Profile, work_directory_of};

use strategy_file::{
    ask, build_text, build_text_into, sandbox_children_once_reaped, seeded_store, slice, stage,
    use_the_multicall_as_the_sandbox,
};

/// The probe: a `type = "model"` key, a `rows` key, and a `refuse` that answers the network's predictions.
const PROBE: &str = r#"//! Network probe: refuses with what its `model` key's network predicts (the W6 suite's strategy file).
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//!
//! [params.model]
//! type = "model"
//! [params.rows]
//! type = "text"
//! [params.shape]
//! type = "text"
//! [params.classes]
//! type = "text"
//! ```

use vike_ml_prediction::{PredictionKind, Predictor};
use vike_model::OrderBroker;

pub struct Probe;

impl<B: OrderBroker> vike_model::Strategy<B> for Probe {}

pub fn build<B: vike_model::OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(Probe)
}

/// For `rows` (`;`-separated rows of `,`-separated f64 bit patterns in hex) and the network's input `shape`
/// (`,`-separated), its `classes` outputs per row, through the one prediction trait, as bit patterns.
pub fn refuse(params: &vike_model::params::Params) -> Option<String> {
    let word = |key: &str| params.get(key).and_then(|v| v.as_str()).unwrap_or_default();
    let (text, rows) = (word("model"), word("rows"));
    let shape: Vec<usize> = word("shape").split(',').map(|n| n.parse().unwrap_or(0)).collect();
    let classes: usize = word("classes").parse().unwrap_or(0);
    let kind = PredictionKind::ClassProbabilities { class_count: classes };
    let model: Box<dyn Predictor> = match vike_ml_onnx_prediction::OnnxModel::from_model_text(text, &shape)
        .and_then(|network| network.predictor(kind))
    {
        Ok(model) => Box::new(model),
        Err(e) => return Some(format!("network probe: the model does not load: {e}")),
    };
    let mut answers = Vec::new();
    for row in rows.split(';') {
        let x: Vec<f64> =
            row.split(',').map(|h| f64::from_bits(u64::from_str_radix(h, 16).unwrap_or(0))).collect();
        let mut y = vec![0.0; classes];
        match model.predict_into(&x, &mut y) {
            Ok(()) => answers.push(y.iter().map(|v| format!("{:016x}", v.to_bits())).collect::<Vec<_>>().join(",")),
            Err(e) => return Some(format!("network probe: the model does not run: {e}")),
        }
    }
    Some(format!("network probe: predictions {}", answers.join(";")))
}
"#;

/// The probe's path under `strategies/`.
const PROBE_FILE: &str = "other/network_probe.rs";

/// A file that names no network: the base template, as before W6.
const PLAIN: &str = r#"//! Plain probe: names nothing but the entry contract (the W6 suite's base-template file).

use vike_model::OrderBroker;

pub struct Plain;

impl<B: OrderBroker> vike_model::Strategy<B> for Plain {}

pub fn build<B: vike_model::OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(Plain)
}
"#;

/// The run holding the committed fixture network.
const RUN: &str = "1763251200-network-0";

/// The rows the probe predicts.
const ROWS: [[f64; 3]; 3] = [[0.5, -1.25, 2.0], [-3.0, 0.75, 1.5], [1.0, 1.0, -1.0]];

/// `vike-ml-onnx-prediction`'s committed MLP (3 inputs, 2 outputs, a symbolic batch axis).
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../vike-ml-onnx-prediction/tests/fixtures/mlp.onnx")
}

/// A finished study run `id` under `runs` holding `network` as its `model.onnx`: the artifact, then the
/// manifest.
fn plant_a_run(runs: &Path, id: &str, network: &[u8]) {
    let dir = runs.join(id);
    std::fs::create_dir_all(&dir).expect("run dir");
    std::fs::write(dir.join(vike_runs::MODEL_ONNX_ARTIFACT), network).expect("model");
    let manifest = format!(
        "{{\"run_id\":\"{id}\",\"kind\":\"study\",\"produced_by\":\"study\",\"started_at\":\
         \"2026-11-16T00:00:00Z\",\"finished_at\":\"2026-11-16T00:00:01Z\",\"git_sha\":null,\
         \"config\":{{}},\"detail\":{{}}}}"
    );
    std::fs::write(dir.join(vike_runs::MANIFEST_FILE), manifest).expect("manifest");
}

fn refusal(response: Response) -> String {
    match response {
        Response::Error(why) => why,
        other => panic!("the probe refuses every run: {other:?}"),
    }
}

/// Build the `what` through the production builder (`build`), printing its artifact's size and build time.
fn build_measured(what: &str, build: impl FnOnce() -> Result<PathBuf, String>) -> PathBuf {
    let started = Instant::now();
    let artifact =
        build().unwrap_or_else(|e| panic!("the production builder must build the {what}:\n{e}"));
    let size = std::fs::metadata(&artifact).expect("the artifact").len();
    println!(
        "MEASURED {what} artifact ({}): {size} bytes ({:.1} MiB), built in {:.1} s",
        if cfg!(debug_assertions) { "debug" } else { "release" },
        size as f64 / (1024.0 * 1024.0),
        started.elapsed().as_secs_f64()
    );
    artifact
}

/// Whether a scratch build directory's `target/` compiled the ONNX runtime: a `libtract_*` output in `deps/`
/// (cargo before 1.100) or a `tract-*` package directory in `build/` (cargo 1.100 gives each package one).
fn compiled_tract(build_dir: &Path) -> bool {
    let any_named = |dir: &Path, prefix: &str| {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|f| f.file_name().to_string_lossy().starts_with(prefix))
        })
    };
    ["debug", "release"].iter().any(|profile| {
        let profile = build_dir.join("target").join(profile);
        any_named(&profile.join("deps"), "libtract_") || any_named(&profile.join("build"), "tract-")
    })
}

/// The opt-in, on disk: this checkout's base work directory holds no tract, its ONNX one does (decision 0296:
/// one work directory per SDK, folder and template kind; `render::work_directory_of`), right after a build
/// into `builds` ran cargo in each.
fn assert_only_the_onnx_directory_compiled_tract(builds: &Path) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let work = |kind| {
        work_directory_of(builds, &root, kind, Profile::of_this_binary())
            .expect("the checkout is a source tree")
    };
    let (base, onnx) = (work(TemplateKind::Strategy), work(TemplateKind::StrategyOnnx));
    assert!(compiled_tract(&onnx), "{}: the ONNX work directory holds no tract", onnx.display());
    assert!(!compiled_tract(&base), "{}: the base work directory compiled tract", base.display());
    println!("tract only in the ONNX work directory ({})", onnx.display());
}

/// Both tests enter a project (the working directory is process-global): one at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

#[test]
fn a_strategy_file_runs_a_network_in_the_jail_and_predicts_what_it_predicts_here() {
    let _one = vike_utility::mutex::lock(&ONE_AT_A_TIME);
    let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
    // A FRESH builds directory, so both builds run cargo: a cached artifact is served without it, and the
    // shared work directory it was built in may be gone since (any builder's size pass evicts the least
    // recently used one: `crates/vike-strategy-builder/src/render.rs`'s `prune_stale_scratch`).
    let builds = tempfile::tempdir().expect("builds dir");
    build_measured("plain probe", || build_text_into(builds.path(), "other/plain_probe.rs", PLAIN));
    let artifact =
        build_measured("network probe", || build_text_into(builds.path(), PROBE_FILE, PROBE));
    assert_only_the_onnx_directory_compiled_tract(builds.path());
    let network = std::fs::read(fixture()).expect("the committed fixture network");
    let rows: Vec<Vec<f64>> = ROWS.iter().map(|row| row.to_vec()).collect();
    the_jail_answers_what_it_answers_here(&artifact, &network, &[1, 3], 2, &rows);
}

/// DeepLOB, the order-book network (book chapter 14): its convolutions, max pool, inception concat, LSTM
/// and softmax run in the jailed file as they run here. `vike-ml-neural-training`'s committed fixture is
/// an untrained DeepLOB over 12 steps of 2 book levels, three classes.
#[test]
fn a_strategy_file_runs_deeplob_in_the_jail_and_predicts_what_it_predicts_here() {
    let _one = vike_utility::mutex::lock(&ONE_AT_A_TIME);
    let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
    let artifact = build_measured("network probe", || build_text(PROBE_FILE, PROBE));
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vike-ml-neural-training/tests/fixtures/deeplob.onnx");
    let network = std::fs::read(fixture).expect("the committed DeepLOB fixture");
    let rows: Vec<Vec<f64>> = (0..2_usize)
        .map(|row| (0..96).map(|i| ((i * 37 + row * 11) % 19) as f64 / 4.0 - 2.25).collect())
        .collect();
    the_jail_answers_what_it_answers_here(&artifact, &network, &[1, 1, 12, 8], 3, &rows);
}

/// `network` planted as a run of a new project; the probe `artifact`, jailed, answers `rows` with it through
/// `Predictor` as `classes` class probabilities of an input `shape`, and the answers must equal, bit for bit,
/// what the same predictor answers here.
fn the_jail_answers_what_it_answers_here(
    artifact: &Path,
    network: &[u8],
    shape: &[usize],
    classes: usize,
    rows: &[Vec<f64>],
) {
    let project = tempfile::tempdir().expect("project root");
    std::fs::create_dir_all(project.path().join("settings")).expect("plant the project marker");
    let strategies = project.path().join("strategies");
    let file = strategies.join(PROBE_FILE);
    std::fs::create_dir_all(file.parent().expect("a folder")).expect("plant strategies/other");
    std::fs::write(&file, PROBE).expect("write the probe");
    plant_a_run(&project.path().join("user_data").join("runs"), RUN, network);
    let restore = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(project.path()).expect("enter the project");
    stage(artifact, &vike_backtesthub::studio_run::strategy_files_dir());
    use_the_multicall_as_the_sandbox();
    let address = spawn_hub_over(seeded_store(50), strategies);

    let kind = PredictionKind::ClassProbabilities { class_count: classes };
    let model: Box<dyn Predictor> = Box::new(
        OnnxModel::from_bytes(network, shape)
            .and_then(|network| network.predictor(kind))
            .expect("the fixture loads here"),
    );
    let bits = |values: &[f64]| {
        values.iter().map(|v| format!("{:016x}", v.to_bits())).collect::<Vec<_>>().join(",")
    };
    let here: Vec<String> = rows
        .iter()
        .map(|row| {
            let mut y = vec![0.0; classes];
            model.predict_into(row, &mut y).expect("the fixture runs here");
            bits(&y)
        })
        .collect();
    let rows_text: Vec<String> = rows.iter().map(|row| bits(row)).collect();
    let shape_text: Vec<String> = shape.iter().map(usize::to_string).collect();
    let params = format!(
        "model = \"{RUN}\"\nrows = \"{}\"\nshape = \"{}\"\nclasses = \"{classes}\"\n",
        rows_text.join(";"),
        shape_text.join(",")
    );
    let said = refusal(ask(
        address,
        &Request::RunSlice {
            spec: StrategySpecWire::File {
                file: PROBE_FILE.to_string(),
                params_toml: params,
                sdk: None,
            },
            slice: slice(),
            params: None,
        },
    ));
    let expected = format!("network probe: predictions {}", here.join(";"));
    assert!(
        said.contains(&expected),
        "the file's predictions in the jail must equal the predictor's here, row for row:\n  said: \
         {said}\n  want: {expected}"
    );
    println!("{} rows predicted by the network in the jail, equal bit for bit", here.len());
    assert!(sandbox_children_once_reaped().is_empty(), "no child outlives the request it served");
    std::env::set_current_dir(restore).expect("restore the working directory");
}

// ---- W25: how long one prediction takes in the jail ---------------------------------------------

/// The latency probe: a `type = "model"` key, its input `shape` and a count of `asks`; `refuse` answers
/// the first prediction's bits and how long each ask took, after as many warm-up asks.
const LATENCY_PROBE: &str = r#"//! Network latency probe: refuses with how long its `model` key's network takes per prediction (W25).
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//!
//! [params.model]
//! type = "model"
//! [params.shape]
//! type = "text"
//! [params.asks]
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

/// The first answer's bits and each ask's nanoseconds, after as many warm-up asks, on one fixed row.
pub fn refuse(params: &vike_model::params::Params) -> Option<String> {
    let text = |key: &str| params.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let shape: Vec<usize> = text("shape").split(',').filter_map(|n| n.parse().ok()).collect();
    let asks: usize = text("asks").parse().unwrap_or(0);
    let model = match vike_ml_onnx_prediction::OnnxModel::from_model_text(&text("model"), &shape) {
        Ok(model) => model,
        Err(e) => return Some(format!("latency probe: the model does not load: {e}")),
    };
    let width: usize = shape.iter().product();
    let row: Vec<f32> = (0..width).map(|k| ((k * 7919) % 2001) as f32 / 1000.0 - 1.0).collect();
    let first = match model.predict(&row) {
        Ok(answer) => answer[0],
        Err(e) => return Some(format!("latency probe: the model does not run: {e}")),
    };
    for _ in 0..asks {
        let _ = std::hint::black_box(model.predict(std::hint::black_box(&row)));
    }
    let mut nanos = Vec::with_capacity(asks);
    for _ in 0..asks {
        let started = std::time::Instant::now();
        let _ = std::hint::black_box(model.predict(std::hint::black_box(&row)));
        nanos.push(started.elapsed().as_nanos().to_string());
    }
    Some(format!("latency probe: first {:08x} nanos {}", first.to_bits(), nanos.join(",")))
}
"#;

/// The latency probe's path under `strategies/`.
const LATENCY_PROBE_FILE: &str = "other/network_latency.rs";

/// Asks per figure, after as many warm-up asks.
const ASKS: usize = 1_000;

/// The probe's fixed row of `width` values.
fn probe_row(width: usize) -> Vec<f32> {
    (0..width).map(|k| ((k * 7919) % 2001) as f32 / 1000.0 - 1.0).collect()
}

/// The median and the 99th percentile of `nanos`, in microseconds.
fn p50_p99(mut nanos: Vec<f64>) -> (f64, f64) {
    nanos.sort_by(f64::total_cmp);
    let micros: Vec<f64> = nanos.iter().map(|n| n / 1_000.0).collect();
    (
        vike_math::statistics::percentile(&micros, 0.5),
        vike_math::statistics::percentile(&micros, 0.99),
    )
}

#[test]
#[ignore = "a measurement (W25): after vike-ml-neural-training's `measure_the_compressed_networks`, in release"]
fn the_compressed_networks_answer_in_the_jail_and_how_fast() {
    let _one = vike_utility::mutex::lock(&ONE_AT_A_TIME);
    let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
    let graphs_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("compressed-networks");
    let manifest = std::fs::read_to_string(graphs_dir.join("manifest.tsv")).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; run `cargo test --release -p vike-ml-neural-training --test integration -- \
             --ignored compression::` first, on this lane",
            graphs_dir.display()
        )
    });
    let graphs: Vec<(String, Vec<usize>)> = manifest
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(name, shape)| {
            (name.to_string(), shape.split(',').map(|n| n.parse().expect("a dimension")).collect())
        })
        .collect();
    let artifact =
        build_measured("latency probe", || build_text(LATENCY_PROBE_FILE, LATENCY_PROBE));

    let project = tempfile::tempdir().expect("project root");
    std::fs::create_dir_all(project.path().join("settings")).expect("plant the project marker");
    let strategies = project.path().join("strategies");
    let file = strategies.join(LATENCY_PROBE_FILE);
    std::fs::create_dir_all(file.parent().expect("a folder")).expect("plant strategies/other");
    std::fs::write(&file, LATENCY_PROBE).expect("write the probe");
    let runs = project.path().join("user_data").join("runs");
    for (index, (name, _)) in graphs.iter().enumerate() {
        let bytes = std::fs::read(graphs_dir.join(format!("{name}.onnx"))).expect("the graph");
        plant_a_run(&runs, &format!("1763251200-latency-{index}"), &bytes);
    }
    let restore = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(project.path()).expect("enter the project");
    stage(&artifact, &vike_backtesthub::studio_run::strategy_files_dir());
    use_the_multicall_as_the_sandbox();
    let address = spawn_hub_over(seeded_store(50), strategies);

    println!("MEASURED cores={:?}", std::thread::available_parallelism());
    for (index, (name, shape)) in graphs.iter().enumerate() {
        let bytes = std::fs::read(graphs_dir.join(format!("{name}.onnx"))).expect("the graph");
        let model = OnnxModel::from_bytes(&bytes, shape).expect("the graph loads here");
        let row = probe_row(shape.iter().product());
        let first = model.predict(&row).expect("the graph runs here")[0];
        for _ in 0..ASKS {
            std::hint::black_box(model.predict(std::hint::black_box(&row)).ok());
        }
        let here: Vec<f64> = (0..ASKS)
            .map(|_| {
                let started = Instant::now();
                std::hint::black_box(model.predict(std::hint::black_box(&row)).ok());
                started.elapsed().as_nanos() as f64
            })
            .collect();
        let shape_text: Vec<String> = shape.iter().map(usize::to_string).collect();
        let params = format!(
            "model = \"1763251200-latency-{index}\"\nshape = \"{}\"\nasks = \"{ASKS}\"\n",
            shape_text.join(",")
        );
        let said = refusal(ask(
            address,
            &Request::RunSlice {
                spec: StrategySpecWire::File {
                    file: LATENCY_PROBE_FILE.to_string(),
                    params_toml: params,
                    sdk: None,
                },
                slice: slice(),
                params: None,
            },
        ));
        let (_, answer) = said.split_once("latency probe: first ").unwrap_or_else(|| {
            panic!("{name}: the probe must answer its timings, it said: {said}")
        });
        let (bits, nanos) = answer.split_once(" nanos ").expect("the probe's spelling");
        assert_eq!(bits, format!("{:08x}", first.to_bits()), "{name}: the jail ran another graph");
        let jailed: Vec<f64> =
            nanos.split(',').map(|n| n.trim().parse().expect("nanoseconds")).collect();
        let ((jail_p50, jail_p99), (here_p50, here_p99)) = (p50_p99(jailed), p50_p99(here));
        println!(
            "MEASURED\t{name}\tjailed p50 {jail_p50:.2} us p99 {jail_p99:.2} us\t\
             in-process p50 {here_p50:.2} us p99 {here_p99:.2} us"
        );
    }
    assert!(sandbox_children_once_reaped().is_empty(), "no child outlives the request it served");
    std::env::set_current_dir(restore).expect("restore the working directory");
}
