//! The train↔infer equality gate.
//!
//! `vike-ml` relies on two implementations of one function agreeing: LightGBM's own predictor, and
//! this crate's pure-Rust tree walker. The whole point of the second is that a live binary can
//! predict without the first — and that is only defensible if they agree on the same model file.
//!
//! So: train a real model with the pinned binary, run LightGBM's OWN `task=predict` over it, parse
//! the same model text with the pure-Rust half, and compare row by row. This is not a smoke test
//! and it is not optional — **it is the proof that a LightGBM bump did not change the numbers**,
//! and the `version=` check in `crate::pins` is only a tripwire in front of it.
//!
//! Double-gated like the other binary-dependent tests: `#[ignore]`d and self-skipping.
//!
//! ```text
//! cargo test -p vike-ml --test train_infer_equality -- --ignored --nocapture
//! ```
//!
//! The eval rows deliberately include the cases a hand-written walker gets wrong: a NaN in a
//! numerical column, a NaN in the CATEGORICAL column, a negative category and a category the model
//! never saw. Those are the paths that survive to production precisely because ordinary rows never
//! exercise them.

use std::path::PathBuf;

use vike_model::scratch::ScratchDir;

use vike_ml::model::is_categorical;
use vike_ml::train::{Task, TrainConfig, TrainData};
use vike_ml::{parse_model_text, GbdtParams, LightGbmCli};

const N_FEATURES: usize = 4;
/// Column 3 is the categorical one. Ids 0..3 — never negative, per `CategoryMap`'s rule.
const CAT_COL: usize = 3;
const N_CATEGORIES: usize = 4;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/lightgbm/lightgbm")
}

fn cli() -> Option<LightGbmCli> {
    let path = binary_path();
    if !path.exists() {
        eprintln!("SKIP: no LightGBM binary at {} — run `just lightgbm-build`", path.display());
        return None;
    }
    Some(LightGbmCli::new(&path).expect("a present binary must be the pinned version"))
}

/// A scratch directory this test OWNS — removed when the returned guard drops. See
/// `crates/vike-ml/tests/lightgbm_cli_smoke.rs`'s `scratch` for the measurement that replaced the
/// pid-keyed name this used to build, and `vike_model::scratch::ScratchDir` for the guard that
/// replaced it — std-only, in this crate's ONE dependency, so it costs no new dependency.
fn scratch(tag: &str) -> ScratchDir {
    ScratchDir::create_in(&std::env::temp_dir(), &format!("vike_ml_equality_{tag}"))
        .expect("scratch dir")
}

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
        // The categorical column shifts the boundary, so a model that ignored it would be worse —
        // which is what makes LightGBM spend a split on it.
        let bump = if cat as i32 == 2 { 0.8 } else { -0.3 };
        y.push(if a + 0.5 * b + bump > 0.0 { 1.0 } else { 0.0 });
    }
    (x, y)
}

/// Rows to compare on: ordinary ones, then one per divergence-prone path.
fn eval_rows() -> Vec<f64> {
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
        num_iterations: 40,
        num_leaves: 15,
        learning_rate: 0.1,
        min_data_in_leaf: 5,
        categorical_feature: Some(GbdtParams::categorical_feature_csv(&[CAT_COL])),
        // ⚠ NOT the default -1. `verbosity=-1` suppresses LightGBM's UNKNOWN-PARAMETER warnings,
        // and this gate is precisely where a renamed or deprecated parameter after a version bump
        // has to be visible rather than swallowed. A grid run wants the quiet default; the gate
        // wants the noise, in the log of the run that is supposed to prove the bump safe.
        verbosity: 1,
        ..GbdtParams::default()
    }
}

/// Train a model with the real binary and return `(model_path, model_text, scratch_guard)`.
///
/// ⚠ The caller must BIND the third element for as long as it uses the first two — they live
/// inside the directory that guard removes on drop.
fn trained(cli: &LightGbmCli, tag: &str) -> (PathBuf, String, ScratchDir) {
    let dir = scratch(tag);
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
/// ⚠ `Log::Info`/`Warning`/`Debug` all `printf` to STDOUT — only `Log::Fatal` uses stderr — so
/// `run_config`'s return value is the only place an unknown-parameter warning exists. Combined
/// with `verbosity=1` above, echoing it here is what makes "a renamed training parameter is
/// visible in the gate's log" true rather than aspirational. Run with `--nocapture` to see it.
fn train_echoing_the_log(cli: &LightGbmCli, conf: &TrainConfig<'_>, dir: &std::path::Path) {
    let conf_path = dir.join("echo.conf");
    std::fs::write(&conf_path, conf.render().unwrap()).unwrap();
    let log = cli.run_config(&conf_path).expect("train");
    eprintln!("--- LightGBM said (verbosity=1) ---\n{log}\n--- end ---");
    assert!(
        !log.to_lowercase().contains("unknown parameter"),
        "LightGBM reported an UNKNOWN PARAMETER. After a version bump this is the first and often \
         the ONLY signal that a training parameter was renamed or deprecated — do not ignore it, \
         and do not lower verbosity to hide it. See crates/vike-ml/CLAUDE.md.\n{log}"
    );
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn the_pure_rust_walker_reproduces_the_scorer_it_replaces() {
    let Some(cli) = cli() else { return };
    let (model, text, dir) = trained(&cli, "main");
    let parsed = parse_model_text(&text).expect("parse");

    // Echo LightGBM's own log and fail on an unknown-parameter warning — the bump tripwire that
    // only exists because run_config returns stdout and this gate trains at verbosity=1.
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

    // A gate that proved nothing would be worse than none: assert the model actually contains the
    // structures the traps are about, BEFORE comparing anything.
    assert!(
        parsed.trees.iter().any(|t| t.decision_type.iter().copied().any(is_categorical)),
        "no categorical split in the trained model — this gate would then prove nothing about the \
         trap most likely to reach production, and nothing about the requirement that disqualified \
         every Rust binding. Strengthen the signal in the categorical column."
    );
    assert!(parsed.trees.len() > 5, "expected real boosting, got {} trees", parsed.trees.len());

    let eval = eval_rows();
    let theirs = cli.predict(&model, &eval, N_FEATURES, &dir).expect("task=predict");
    let ours = parsed.predict_proba_batch(&eval, N_FEATURES).expect("predict_proba_batch");

    assert_eq!(theirs.len(), eval.len() / N_FEATURES);
    assert_eq!(ours.len(), theirs.len());

    for i in 0..theirs.len() {
        let row = &eval[i * N_FEATURES..(i + 1) * N_FEATURES];
        // The walker sums leaf values and applies exp; LightGBM does the same and prints ~17
        // significant digits which this side re-parses exactly. The only difference available is
        // one `exp` through two libms — sub-ulp on a modern glibc, ~1e-16 near 0.5. 1e-12 is four
        // orders of headroom for that and still orders tighter than any routing bug, which moves
        // a probability by 1e-3 at the very least.
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
    // The EXACT half of the tolerance table, and the justification for `GbdtModel::raw_score`
    // using a naive left fold rather than the workspace's compensated `py_sum`. Both sides sum
    // the same 17-significant-digit leaf values in the same tree order with a plain `+`, and no
    // transcendental is involved on either side — so no rounding difference is AVAILABLE, and any
    // difference at all means a row reached a different leaf.
    //
    // ⚠ If this ever fails, do not convert it to a tolerance. Print the row, walk it by hand
    // against the model text, and find the node that disagreed.
    let Some(cli) = cli() else { return };
    let (model, text, dir) = trained(&cli, "raw");
    let parsed = parse_model_text(&text).expect("parse");
    let eval = eval_rows();

    let theirs = cli.predict_raw(&model, &eval, N_FEATURES, &dir).expect("predict_raw_score=true");
    let ours = parsed.raw_scores_batch(&eval, N_FEATURES).expect("raw_scores_batch");
    assert_eq!(ours.len(), theirs.len());
    for i in 0..theirs.len() {
        let row = &eval[i * N_FEATURES..(i + 1) * N_FEATURES];
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
    // The categorical path is the interesting one, but a purely numerical model must not regress
    // as a side effect of getting it right.
    let Some(cli) = cli() else { return };
    let dir = scratch("numeric");
    let (x, y) = training_set(600);
    let data = dir.join("train.csv");
    let mut f = std::fs::File::create(&data).unwrap();
    TrainData::new(&x, &y, N_FEATURES).unwrap().write_csv(&mut f).unwrap();
    drop(f);

    let model = dir.join("model.txt");
    let p = GbdtParams { num_iterations: 30, verbosity: 1, ..GbdtParams::default() };
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
    let eval = eval_rows();
    let theirs = cli.predict(&model, &eval, N_FEATURES, &dir).unwrap();
    let ours = parsed.predict_proba_batch(&eval, N_FEATURES).unwrap();
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        assert!((a - b).abs() <= 1e-12, "row {i}: {a} vs {b}");
    }
}

/// Write the committed golden fixture. Run EXPLICITLY, on the CI box, when the fixture is deliberately
/// being regenerated — after a LightGBM bump, per `crates/vike-ml/CLAUDE.md`:
///
/// ```text
/// cargo test -p vike-ml --test train_infer_equality -- --ignored capture
/// ```
///
/// A regeneration that CHANGES existing values must be justified in the commit message. The
/// fixture is the record of what the real library answered, and a walker change must never quietly
/// rewrite the thing that judges it.
#[test]
#[ignore = "writes tests/fixtures/; run explicitly on the CI box when regenerating"]
fn capture_the_golden_fixture() {
    use std::fmt::Write as _;

    let Some(cli) = cli() else { panic!("regeneration needs the pinned binary") };
    let (model, text, dir) = trained(&cli, "capture");
    let eval = eval_rows();
    let proba = cli.predict(&model, &eval, N_FEATURES, &dir).expect("task=predict");

    // PROVENANCE. Without it the fixture is a number nobody can re-derive: which LightGBM
    // produced it, from what source, on what box, with which parameters. The repo's drift-gate
    // family (vendor/jforex-bridge.jar, the ctrader bindings) exists because a generated artifact
    // whose generator is unpinned drifts silently.
    let provenance = std::fs::read_to_string(binary_path().with_file_name("PROVENANCE"))
        .expect("the binary's PROVENANCE file, written by scripts/build_lightgbm.sh");
    let p = params();

    let mut tsv = String::new();
    tsv.push_str("# vike-ml golden fixture — LightGBM's own task=predict output.\n");
    tsv.push_str("# Regenerate with train_infer_equality.rs's capture_the_golden_fixture.\n");
    tsv.push_str("# Fields: f0 f1 f2 cat proba — each an IEEE-754 f64 as 16 hex digits.\n");
    writeln!(
        tsv,
        "# captured_at={}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    )
    .unwrap();
    writeln!(tsv, "# pinned_tag={}", vike_ml::PINNED_LIGHTGBM_TAG).unwrap();
    for l in provenance.lines() {
        writeln!(tsv, "# lightgbm_{l}").unwrap();
    }
    writeln!(tsv, "# params_num_iterations={}", p.num_iterations).unwrap();
    writeln!(tsv, "# params_num_leaves={}", p.num_leaves).unwrap();
    writeln!(tsv, "# params_learning_rate={}", p.learning_rate).unwrap();
    writeln!(tsv, "# params_seed={}", p.seed).unwrap();
    writeln!(tsv, "# params_categorical_feature={}", p.categorical_feature.clone().unwrap())
        .unwrap();
    for (i, row) in eval.chunks(N_FEATURES).enumerate() {
        for v in row {
            write!(tsv, "{:016x}\t", v.to_bits()).unwrap();
        }
        writeln!(tsv, "{:016x}", proba[i].to_bits()).unwrap();
    }

    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("real_binary_categorical_v4.txt"), text).unwrap();
    std::fs::write(out.join("real_binary_categorical_expected.tsv"), tsv).unwrap();
}
