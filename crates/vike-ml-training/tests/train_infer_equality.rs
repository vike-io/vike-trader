//! The train↔infer equality gate.
//!
//! A live binary predicts with this crate's pure-Rust walker instead of LightGBM's own predictor,
//! which is only defensible if the two agree on the same model file. So: train a real model with
//! the pinned binary, run LightGBM's OWN `task=predict` over it, parse the same text with the walker,
//! and compare row by row. Not a smoke test and not optional — **it is the proof that a LightGBM
//! bump did not change the numbers**; the `version=` check in `crate::pins` is only a tripwire.
//!
//! Double-gated like the other binary-dependent tests: `#[ignore]`d and self-skipping.
//!
//! ```text
//! cargo test -p vike-ml-training --test integration -- --ignored --skip capture --nocapture train_infer_equality::
//! ```
//!
//! The eval rows deliberately include the paths ordinary rows never exercise: a NaN in a numerical
//! column, a NaN in the CATEGORICAL column, a negative category and a category never seen.

use std::path::PathBuf;

use vike_paths::scratch::ScratchDir;

use vike_ml_prediction::model::is_categorical;
use vike_ml_prediction::{GbdtModel, Objective, parse_model_text};
use vike_ml_training::train::{Task, TrainConfig};
use vike_ml_training::{GbdtParams, TrainData, train::cli::LightGbmCli};

use crate::lightgbm::{binary_path, cli, scratch};

const N_FEATURES: usize = 4;
/// Column 3 is the categorical one. Ids 0..3 — never negative, per `CategoryMap`'s rule.
const CAT_COL: usize = 3;
const N_CATEGORIES: usize = 4;

/// A deterministic training set with three numerical columns and one categorical column that
/// genuinely carries signal, so LightGBM actually splits on it.
fn training_set(n: usize) -> (Vec<f64>, Vec<f32>) {
    let mut x = Vec::with_capacity(n * N_FEATURES);
    let mut y = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64;
        let a = (t * 0.37).sin();
        let b = (t * 0.11).cos() * 2.0;
        let c = ((i % 7) as f64) - 3.0;
        let cat = (i % N_CATEGORIES) as f64;
        x.extend_from_slice(&[a, b, c, cat]);
        // The categorical column shifts the boundary, so LightGBM spends a split on it.
        let bump = if cat as i32 == 2 { 0.8 } else { -0.3 };
        y.push(if a + 0.5 * b + bump > 0.0 { 1.0 } else { 0.0 });
    }
    (x, y)
}

/// Rows to compare on: ordinary ones, then one per divergence-prone path.
fn evaluation_rows() -> Vec<f64> {
    let mut x = Vec::new();
    for i in 0..40usize {
        let t = i as f64;
        x.extend_from_slice(&[
            (t * 0.29).sin(),
            (t * 0.13).cos() * 2.0,
            ((i % 5) as f64) - 2.0,
            (i % N_CATEGORIES) as f64,
        ]);
    }
    x.extend_from_slice(&[f64::NAN, 0.5, 1.0, 1.0]); // NaN in a numerical column
    x.extend_from_slice(&[0.5, f64::NAN, 1.0, 2.0]); // NaN in another
    x.extend_from_slice(&[0.5, 0.5, 1.0, f64::NAN]); // NaN CATEGORY -> right, unconditionally
    x.extend_from_slice(&[0.5, 0.5, 1.0, -1.0]); // negative category -> right, unconditionally
    x.extend_from_slice(&[0.5, 0.5, 1.0, 9.0]); // a category never seen in training
    x.extend_from_slice(&[0.5, 0.5, 1.0, 2.9]); // truncates to 2, does not round to 3
    x
}

fn params() -> GbdtParams {
    GbdtParams {
        iteration_count: 40,
        num_leaves: 15,
        learning_rate: 0.1,
        min_data_in_leaf: 5,
        categorical_feature: Some(GbdtParams::categorical_feature_csv(&[CAT_COL])),
        // ⚠ NOT the default -1, which suppresses LightGBM's UNKNOWN-PARAMETER warnings: this gate
        // is where a renamed or deprecated parameter after a bump has to be visible.
        verbosity: 1,
        ..GbdtParams::default()
    }
}

/// Train a model with the real binary and return `(model_path, model_text, scratch_guard)`.
///
/// ⚠ The caller must BIND the third element for as long as it uses the first two — they live
/// inside the directory that guard removes on drop.
fn trained(cli: &LightGbmCli, tag: &str) -> (PathBuf, String, ScratchDir) {
    let dir = scratch("vike_ml_equality", tag);
    let (x, y) = training_set(800);
    let data = dir.join("train.csv");
    let mut f = std::fs::File::create(&data).unwrap();
    TrainData::new(&x, &y, N_FEATURES).unwrap().write_csv(&mut f).unwrap();
    drop(f);

    let model = dir.join("model.txt");
    let p = params();
    let text = cli
        .train(
            &TrainConfig {
                task: Task::Train,
                params: &p,
                data: &data,
                output_model: Some(&model),
                input_model: None,
                output_result: None,
                predict_raw_score: false,
            },
            &dir,
        )
        .expect("train");
    (model, text, dir)
}

/// Train once and ECHO LightGBM's own log, which is where a bump's damage shows up first.
///
/// ⚠ `run_config`'s return value (captured stdout) is the only place an unknown-parameter warning
/// exists; with `verbosity=1` above, echoing it makes a renamed parameter visible in the gate's
/// log. Run with `--nocapture` to see it.
fn train_echoing_the_log(cli: &LightGbmCli, conf: &TrainConfig<'_>, dir: &std::path::Path) {
    let conf_path = dir.join("echo.conf");
    std::fs::write(&conf_path, conf.render().unwrap()).unwrap();
    let log = cli.run_config(&conf_path).expect("train");
    eprintln!("--- LightGBM said (verbosity=1) ---\n{log}\n--- end ---");
    assert!(
        !log.to_lowercase().contains("unknown parameter"),
        "LightGBM reported an UNKNOWN PARAMETER. After a version bump this is the first and often \
         the ONLY signal that a training parameter was renamed or deprecated — do not ignore it, \
         and do not lower verbosity to hide it. See crates/vike-ml-training/CLAUDE.md.\n{log}"
    );
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn the_pure_rust_walker_reproduces_the_scorer_it_replaces() {
    let Some(cli) = cli() else { return };
    let (model, text, dir) = trained(&cli, "main");
    let parsed = parse_model_text(&text).expect("parse");

    // Echo LightGBM's own log and fail on an unknown-parameter warning — the bump tripwire.
    let p = params();
    let data = dir.join("train.csv");
    train_echoing_the_log(
        &cli,
        &TrainConfig {
            task: Task::Train,
            params: &p,
            data: &data,
            output_model: Some(&dir.join("echo.model.txt")),
            input_model: None,
            output_result: None,
            predict_raw_score: false,
        },
        &dir,
    );

    // Non-vacuity: the model must contain the structures the traps are about.
    assert!(
        parsed.trees.iter().any(|t| t.decision_type.iter().copied().any(is_categorical)),
        "no categorical split in the trained model — this gate would then prove nothing about the \
         trap most likely to reach production, and nothing about the requirement that disqualified \
         every Rust binding. Strengthen the signal in the categorical column."
    );
    assert!(parsed.trees.len() > 5, "expected real boosting, got {} trees", parsed.trees.len());

    let evaluation = evaluation_rows();
    let theirs = cli.predict(&model, &evaluation, N_FEATURES, &dir).expect("task=predict");
    let ours = parsed.predict_proba_batch(&evaluation, N_FEATURES).expect("predict_proba_batch");

    assert_eq!(theirs.len(), evaluation.len() / N_FEATURES);
    assert_eq!(ours.len(), theirs.len());

    for i in 0..theirs.len() {
        let row = &evaluation[i * N_FEATURES..(i + 1) * N_FEATURES];
        // The only difference available is one `exp` through two libms (~1e-16 near 0.5). 1e-12 is
        // four orders of headroom for that and still orders tighter than any routing bug, which
        // moves a probability by 1e-3 at the very least.
        let d = (ours[i] - theirs[i]).abs();
        assert!(
            d <= 1e-12,
            "row {i} {row:?}: walker {} vs LightGBM {} (differ by {d}) — at this magnitude the \
             walk reached a DIFFERENT LEAF; do not widen this, find the node",
            ours[i],
            theirs[i]
        );
    }
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn the_raw_scores_match_bit_for_bit() {
    // The EXACT half, and why `GbdtModel::raw_score` is a naive left fold rather than `compensated_sum`:
    // both sides sum the same leaf values in the same order with a plain `+`, no transcendental,
    // so any difference means a row reached a different leaf.
    //
    // ⚠ If this ever fails, do not convert it to a tolerance. Walk the row by hand against the
    // model text and find the node that disagreed.
    let Some(cli) = cli() else { return };
    let (model, text, dir) = trained(&cli, "raw");
    let parsed = parse_model_text(&text).expect("parse");
    let evaluation = evaluation_rows();

    let theirs =
        cli.predict_raw(&model, &evaluation, N_FEATURES, &dir).expect("predict_raw_score=true");
    let ours = parsed.raw_scores_batch(&evaluation, N_FEATURES).expect("raw_scores_batch");
    assert_eq!(ours.len(), theirs.len());
    for i in 0..theirs.len() {
        let row = &evaluation[i * N_FEATURES..(i + 1) * N_FEATURES];
        assert_eq!(
            ours[i].to_bits(),
            theirs[i].to_bits(),
            "row {i} {row:?}: walker {} vs LightGBM {} — no rounding difference is available \
             here, so the walk reached a DIFFERENT LEAF. Do not widen this; find the node.",
            ours[i],
            theirs[i]
        );
    }
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn the_walker_agrees_on_a_model_with_no_categorical_feature_either() {
    // A purely numerical model must not regress as a side effect of the categorical path.
    let Some(cli) = cli() else { return };
    let dir = scratch("vike_ml_equality", "numeric");
    let (x, y) = training_set(600);
    let data = dir.join("train.csv");
    let mut f = std::fs::File::create(&data).unwrap();
    TrainData::new(&x, &y, N_FEATURES).unwrap().write_csv(&mut f).unwrap();
    drop(f);

    let model = dir.join("model.txt");
    let p = GbdtParams { iteration_count: 30, verbosity: 1, ..GbdtParams::default() };
    let text = cli
        .train(
            &TrainConfig {
                task: Task::Train,
                params: &p,
                data: &data,
                output_model: Some(&model),
                input_model: None,
                output_result: None,
                predict_raw_score: false,
            },
            &dir,
        )
        .unwrap();
    let parsed = parse_model_text(&text).unwrap();
    let evaluation = evaluation_rows();
    let theirs = cli.predict(&model, &evaluation, N_FEATURES, &dir).unwrap();
    let ours = parsed.predict_proba_batch(&evaluation, N_FEATURES).unwrap();
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        assert!((a - b).abs() <= 1e-12, "row {i}: {a} vs {b}");
    }
}

/// The class count the multiclass gate trains with.
const CLASS_COUNT: usize = 3;

/// `training_set`'s rows with a continuous target (`regression`) or a three-class one
/// (`multiclass`): the same signal as the binary label, so the categorical column still splits.
fn labels_for(x: &[f64], objective: &str) -> Vec<f32> {
    x.as_chunks::<N_FEATURES>()
        .0
        .iter()
        .map(|r| {
            let bump = if r[CAT_COL] as i32 == 2 { 0.8 } else { -0.3 };
            let score = r[0] + 0.5 * r[1] + bump;
            match objective {
                "regression" => score as f32,
                _ if score < -0.5 => 0.0,
                _ if score < 0.5 => 1.0,
                _ => 2.0,
            }
        })
        .collect()
}

/// [`params`] for `objective`. Fewer iterations than the binary model: a multiclass model is
/// `CLASS_COUNT` trees per iteration, and its committed fixture grows with every one.
fn params_for(objective: &str) -> GbdtParams {
    GbdtParams { objective: objective.to_string(), iteration_count: 20, ..params() }
}

/// Train `objective` (`regression` or `multiclass`) with the real binary, echoing LightGBM's log
/// and failing on an unknown-parameter warning; returns what [`trained`] returns.
///
/// `num_class` has no `GbdtParams` field (every learner in the workspace fits binary), so the
/// multiclass config gets it as one extra line here.
fn trained_objective(cli: &LightGbmCli, objective: &str) -> (PathBuf, String, ScratchDir) {
    let dir = scratch("vike_ml_equality", objective);
    let (x, _) = training_set(800);
    let y = labels_for(&x, objective);
    let data = dir.join("train.csv");
    let mut f = std::fs::File::create(&data).unwrap();
    TrainData::new(&x, &y, N_FEATURES).unwrap().write_csv(&mut f).unwrap();
    drop(f);

    let model = dir.join("model.txt");
    let p = params_for(objective);
    let conf = TrainConfig {
        task: Task::Train,
        params: &p,
        data: &data,
        output_model: Some(&model),
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    };
    let mut text = conf.render().unwrap();
    if objective == "multiclass" {
        text.push_str(&format!("num_class={CLASS_COUNT}\n"));
    }
    let conf_path = dir.join("train.conf");
    std::fs::write(&conf_path, text).unwrap();
    let log = cli.run_config(&conf_path).expect("train");
    eprintln!("--- LightGBM said ({objective}, verbosity=1) ---\n{log}\n--- end ---");
    assert!(
        !log.to_lowercase().contains("unknown parameter"),
        "LightGBM reported an UNKNOWN PARAMETER — see crates/vike-ml-training/CLAUDE.md.\n{log}"
    );
    let model_text = std::fs::read_to_string(&model).unwrap();
    (model, model_text, dir)
}

/// The walker against LightGBM's own `task=predict` on one trained model: raw scores BIT FOR BIT
/// (K per row for multiclass), then the transformed output within `tolerance`.
fn assert_the_walker_reproduces(
    cli: &LightGbmCli,
    model: &std::path::Path,
    parsed: &GbdtModel,
    dir: &std::path::Path,
    tolerance: f64,
) {
    assert!(
        parsed.trees.iter().any(|t| t.decision_type.iter().copied().any(is_categorical)),
        "no categorical split in the trained model — the gate would prove nothing about that path"
    );
    assert!(parsed.trees.len() > 5, "expected real boosting, got {} trees", parsed.trees.len());
    let evaluation = evaluation_rows();
    let width = parsed.objective.output_count();
    let rows = evaluation.len() / N_FEATURES;

    let theirs = cli.predict_raw(model, &evaluation, N_FEATURES, dir).expect("raw task=predict");
    let ours = parsed.raw_scores_batch(&evaluation, N_FEATURES).expect("raw_scores_batch");
    assert_eq!((ours.len(), theirs.len()), (rows * width, rows * width));
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "row {} output {}: walker {a} vs LightGBM {b} — a DIFFERENT LEAF or the wrong class's \
             tree; do not widen this, find the node",
            i / width,
            i % width
        );
    }

    let theirs = cli.predict(model, &evaluation, N_FEATURES, dir).expect("task=predict");
    let ours = parsed.predict_batch(&evaluation, N_FEATURES).expect("predict_batch");
    assert_eq!((ours.len(), theirs.len()), (rows * width, rows * width));
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        let d = (a - b).abs();
        assert!(
            d <= tolerance,
            "row {} output {}: walker {a} vs LightGBM {b}",
            i / width,
            i % width
        );
    }
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn the_walker_reproduces_a_regression_model_bit_for_bit() {
    let Some(cli) = cli() else { return };
    let (model, text, dir) = trained_objective(&cli, "regression");
    let parsed = parse_model_text(&text).expect("parse");
    assert_eq!(parsed.objective, Objective::Regression);
    // No transcendental at all: the prediction IS the raw sum, so it agrees to the bit too.
    assert_the_walker_reproduces(&cli, &model, &parsed, &dir, 0.0);
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn the_walker_reproduces_a_multiclass_model() {
    let Some(cli) = cli() else { return };
    let (model, text, dir) = trained_objective(&cli, "multiclass");
    let parsed = parse_model_text(&text).expect("parse");
    assert_eq!(parsed.objective, Objective::Multiclass { class_count: CLASS_COUNT });
    // The softmax's `exp`s through two libms, the binary gate's budget for the same reason.
    assert_the_walker_reproduces(&cli, &model, &parsed, &dir, 1e-12);
}

/// LightGBM's own `predict_contrib=true` over `model`: per row, one block per output of
/// `N_FEATURES + 1` numbers (the contributions, then the expected value), row-major.
///
/// `TrainConfig` has no contrib field (nothing but this gate asks LightGBM for one), so the predict
/// config gets it as one extra line, the way `trained_objective` adds `num_class`.
fn lightgbm_contributions(
    cli: &LightGbmCli,
    model: &std::path::Path,
    rows: &[f64],
    dir: &std::path::Path,
) -> Vec<f64> {
    let data = dir.join("contrib.csv");
    let mut buffer = Vec::new();
    vike_ml_training::train::write_predict_csv(rows, N_FEATURES, &mut buffer).unwrap();
    std::fs::write(&data, buffer).unwrap();
    let result = dir.join("contrib.out");
    let params = GbdtParams::default();
    let conf = TrainConfig {
        task: Task::Predict,
        params: &params,
        data: &data,
        output_model: None,
        input_model: Some(model),
        output_result: Some(&result),
        predict_raw_score: false,
    };
    let conf_path = dir.join("contrib.conf");
    std::fs::write(&conf_path, conf.render().unwrap() + "predict_contrib=true\n").unwrap();
    cli.run_config(&conf_path).expect("task=predict predict_contrib=true");
    std::fs::read_to_string(&result)
        .unwrap()
        .split_whitespace()
        .map(|field| field.parse::<f64>().expect("a number"))
        .collect()
}

/// `vike-ml-prediction`'s TreeSHAP against LightGBM's own `predict_contrib`, on a binary, a
/// regression and a multiclass model, every evaluation row (the NaN, negative and unseen-category
/// rows included): BIT FOR BIT. The explainer mirrors `Tree::TreeSHAP` and its three path routines
/// operation for operation, with no transcendental anywhere, so a difference is a different branch
/// or a different order of the same arithmetic — find it, never widen this.
#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn the_shap_values_match_lightgbms_own_contributions() {
    let Some(cli) = cli() else { return };
    let evaluation = evaluation_rows();
    let rows = evaluation.len() / N_FEATURES;
    for objective in ["binary", "regression", "multiclass"] {
        let (model, text, dir) = if objective == "binary" {
            trained(&cli, "contrib")
        } else {
            trained_objective(&cli, objective)
        };
        let parsed = parse_model_text(&text).expect("parse");
        let width = parsed.objective.output_count() * (N_FEATURES + 1);
        let theirs = lightgbm_contributions(&cli, &model, &evaluation, &dir);
        let ours = parsed.shap_values_batch(&evaluation, N_FEATURES).expect("shap_values_batch");
        assert_eq!((ours.len(), theirs.len()), (rows * width, rows * width), "{objective}");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "{objective} row {} entry {}: TreeSHAP {a} vs LightGBM {b}",
                i / width,
                i % width
            );
        }
    }
}

/// Write the committed golden fixture. Run EXPLICITLY, on the CI box, when the fixture is deliberately
/// being regenerated — after a LightGBM bump, per `crates/vike-ml-training/CLAUDE.md`:
///
/// ```text
/// cargo test -p vike-ml-training --test integration -- --ignored train_infer_equality::capture
/// ```
///
/// A regeneration that CHANGES existing values must be justified in the commit message: a walker
/// change must never quietly rewrite the thing that judges it.
#[test]
#[ignore = "writes vike-ml-prediction's tests/fixtures/; run explicitly on the CI box when regenerating"]
fn capture_the_golden_fixture() {
    let Some(cli) = cli() else { panic!("regeneration needs the pinned binary") };
    let (model, text, dir) = trained(&cli, "capture");
    let evaluation = evaluation_rows();
    let proba = cli.predict(&model, &evaluation, N_FEATURES, &dir).expect("task=predict");
    let tsv = golden_tsv("capture_the_golden_fixture", "proba", &params(), &evaluation, &proba);
    write_golden("real_binary_categorical", &text, &tsv);
}

/// [`capture_the_golden_fixture`]'s twin for the regression model (`real_regression_categorical_*`).
#[test]
#[ignore = "writes vike-ml-prediction's tests/fixtures/; run explicitly on the CI box when regenerating"]
fn capture_the_regression_golden_fixture() {
    capture_objective("regression", "prediction", "capture_the_regression_golden_fixture");
}

/// [`capture_the_golden_fixture`]'s twin for the multiclass model (`real_multiclass_categorical_*`).
#[test]
#[ignore = "writes vike-ml-prediction's tests/fixtures/; run explicitly on the CI box when regenerating"]
fn capture_the_multiclass_golden_fixture() {
    let fields = (0..CLASS_COUNT).map(|k| format!("proba_{k}")).collect::<Vec<_>>().join(" ");
    capture_objective("multiclass", &fields, "capture_the_multiclass_golden_fixture");
}

fn capture_objective(objective: &str, fields: &str, capture: &str) {
    let Some(cli) = cli() else { panic!("regeneration needs the pinned binary") };
    let (model, text, dir) = trained_objective(&cli, objective);
    let evaluation = evaluation_rows();
    let predicted = cli.predict(&model, &evaluation, N_FEATURES, &dir).expect("task=predict");
    let tsv = golden_tsv(capture, fields, &params_for(objective), &evaluation, &predicted);
    write_golden(&format!("real_{objective}_categorical"), &text, &tsv);
}

/// Write `<stem>_v4.txt` (the model) and `<stem>_expected.tsv` into `vike-ml-prediction`'s
/// `tests/fixtures/`, where `real_model_golden.rs` judges the walker against them.
fn write_golden(stem: &str, model_text: &str, tsv: &str) {
    let out =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vike-ml-prediction/tests/fixtures");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join(format!("{stem}_v4.txt")), model_text).unwrap();
    std::fs::write(out.join(format!("{stem}_expected.tsv")), tsv).unwrap();
}

/// The golden TSV: the provenance header, then one line per evaluation row — its features, then
/// `predicted`'s values for that row (one, or K for multiclass), each as 16 hex digits.
fn golden_tsv(
    capture: &str,
    fields: &str,
    p: &GbdtParams,
    evaluation: &[f64],
    predicted: &[f64],
) -> String {
    use std::fmt::Write as _;

    // PROVENANCE: without it the fixture is a number nobody can re-derive (which LightGBM, from
    // what source, with which parameters).
    let provenance = std::fs::read_to_string(binary_path().with_file_name("PROVENANCE"))
        .expect("the binary's PROVENANCE file, written by scripts/build_lightgbm.sh");
    let width = predicted.len() / (evaluation.len() / N_FEATURES);

    let mut tsv = String::new();
    tsv.push_str("# vike-ml golden fixture — LightGBM's own task=predict output.\n");
    writeln!(tsv, "# Regenerate with train_infer_equality.rs's {capture}.").unwrap();
    writeln!(tsv, "# Fields: f0 f1 f2 cat {fields} — each an IEEE-754 f64 as 16 hex digits.")
        .unwrap();
    writeln!(
        tsv,
        "# captured_at={}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    )
    .unwrap();
    writeln!(tsv, "# pinned_tag={}", vike_ml_training::PINNED_LIGHTGBM_TAG).unwrap();
    for l in provenance.lines() {
        writeln!(tsv, "# lightgbm_{l}").unwrap();
    }
    writeln!(tsv, "# params_num_iterations={}", p.iteration_count).unwrap();
    writeln!(tsv, "# params_num_leaves={}", p.num_leaves).unwrap();
    writeln!(tsv, "# params_learning_rate={}", p.learning_rate).unwrap();
    writeln!(tsv, "# params_seed={}", p.seed).unwrap();
    writeln!(tsv, "# params_categorical_feature={}", p.categorical_feature.clone().unwrap())
        .unwrap();
    for (row, values) in evaluation.chunks(N_FEATURES).zip(predicted.chunks(width)) {
        for v in row {
            write!(tsv, "{:016x}\t", v.to_bits()).unwrap();
        }
        let values: Vec<String> = values.iter().map(|v| format!("{:016x}", v.to_bits())).collect();
        writeln!(tsv, "{}", values.join("\t")).unwrap();
    }
    tsv
}
