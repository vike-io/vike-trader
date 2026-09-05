//! The driver against the REAL LightGBM binary.
//!
//! Double-gated, the same shape as this repo's venue live smokes
//! (`crates/bridges/*/tests/*_smoke.rs`): `#[ignore]`d so no ordinary run touches them, AND
//! self-skipping when the binary is absent, so an explicit `--ignored` run on a machine without
//! one prints a reason instead of failing. Build the binary with `just lightgbm-build`.
//!
//! ```text
//! cargo test -p vike-ml --test lightgbm_cli_smoke -- --ignored --nocapture
//! ```
//!
//! ⚠ No environment variable decides where the binary is — this crate reads none, and
//! `crates/vike-ops/tests/settings_registry.rs` scans `tests/` too. The location is a CONVENTION
//! (`bin/lightgbm/lightgbm`, relative to the repo root) resolved from `CARGO_MANIFEST_DIR`,
//! which is a compile-time macro and not a process-environment read.

use std::path::PathBuf;

use vike_model::scratch::ScratchDir;

use vike_ml::cli::LightGbmCli;
use vike_ml::train::{Task, TrainConfig, TrainData};
use vike_ml::{GbdtParams, parse_model_text};

/// `<repo>/bin/lightgbm/lightgbm` — the path `just lightgbm-build` writes by default.
fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/lightgbm/lightgbm")
}

/// `Some(cli)` when the pinned binary is present and IS the pinned version; `None` (with a printed
/// reason) otherwise.
fn cli() -> Option<LightGbmCli> {
    let path = binary_path();
    if !path.exists() {
        eprintln!("SKIP: no LightGBM binary at {} — run `just lightgbm-build`", path.display());
        return None;
    }
    Some(LightGbmCli::new(&path).expect("a present binary must be the pinned version"))
}

/// A scratch directory this test OWNS — removed when the returned guard drops.
///
/// ⚠ This used to answer with a bare `PathBuf` under `<temp>/vike_ml_smoke_<tag>_<pid>`, which
/// nothing ever deleted: measured on the CI box on 2026-08-25, this family and its
/// `vike_ml_equality_*` twin had leaked 2,102 `/tmp` directories. A PID is also REUSED, and that
/// box runs these tests as TWO users — when a pid collides with a directory the other user made,
/// `create_dir_all` succeeds and the write fails `PermissionDenied`.
/// `vike_model::scratch::ScratchDir` closes both halves — its `Drop` removes the directory, and it
/// is std-only in this crate's ONE dependency, so it costs nothing against the zero-dependency
/// constraint that rules out `tempfile` here.
fn scratch(tag: &str) -> ScratchDir {
    ScratchDir::create_in(&std::env::temp_dir(), &format!("vike_ml_smoke_{tag}"))
        .expect("scratch dir")
}

/// A separable problem with one genuinely categorical column, deterministic.
fn toy(n: usize) -> (Vec<f64>, Vec<f32>) {
    let mut x = Vec::with_capacity(n * 3);
    let mut y = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64;
        let a = (t * 0.37).sin();
        let b = (t * 0.11).cos();
        let cat = (i % 4) as f64;
        x.extend_from_slice(&[a, b, cat]);
        let bump = if cat as i32 == 2 { 0.8 } else { -0.3 };
        y.push(if a + 0.5 * b + bump > 0.0 { 1.0 } else { 0.0 });
    }
    (x, y)
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn a_fit_produces_model_text_this_crates_own_parser_accepts() {
    let Some(cli) = cli() else { return };
    let dir = scratch("fit");
    let (x, y) = toy(400);
    let data = dir.join("train.csv");
    let mut f = std::fs::File::create(&data).unwrap();
    TrainData::new(&x, &y, 3).unwrap().write_csv(&mut f).unwrap();
    drop(f);

    let model = dir.join("model.txt");
    let params = GbdtParams {
        num_iterations: 30,
        categorical_feature: Some(GbdtParams::categorical_feature_csv(&[2])),
        ..GbdtParams::default()
    };
    let text = cli
        .train(
            &TrainConfig {
                task: Task::Train,
                params: &params,
                data: &data,
                output_model: Some(&model),
                input_model: None,
                output_result: None,
                predict_raw_score: false,
            },
            &dir,
        )
        .expect("train");

    assert!(text.starts_with("tree"), "{}", &text[..text.len().min(40)]);
    let parsed = parse_model_text(&text).expect("the walker must accept what train emits");
    assert!(parsed.trees.len() > 5, "{} trees", parsed.trees.len());

    // The categorical column must actually be USED — this is the requirement every Rust binding
    // failed, and the one that must never silently regress on this route either.
    assert!(
        parsed.trees.iter().any(|t| !t.cat_threshold.is_empty()),
        "no cat_threshold anywhere: the declared categorical column was binned as NUMERIC, which \
         is exactly the silent failure that disqualified the FFI bindings"
    );
    assert!(text.contains("categorical_feature: 2"), "and the model must record it: {text:.0}");
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn two_fits_with_the_same_config_produce_byte_identical_models() {
    // deterministic=true was MEASURED to give byte-identical models in the backend bakeoff (two
    // runs, `cmp` clean, 189,535 bytes each) at a cost of roughly 25% against the fastest
    // non-deterministic variant. A research grid whose winner gets re-fit and audited buys that.
    // ⚠ LightGBM's own docs caveat that different VERSIONS, compilers or systems may still differ,
    // which is why this is proven HERE, on the box that trains, and never assumed.
    let Some(cli) = cli() else { return };
    let dir = scratch("determinism");
    let (x, y) = toy(400);
    let data = dir.join("train.csv");
    let mut f = std::fs::File::create(&data).unwrap();
    TrainData::new(&x, &y, 3).unwrap().write_csv(&mut f).unwrap();
    drop(f);

    let params = GbdtParams { num_iterations: 30, ..GbdtParams::default() };
    let one = dir.join("a.txt");
    let two = dir.join("b.txt");
    let mut text = Vec::new();
    for out in [&one, &two] {
        text.push(
            cli.train(
                &TrainConfig {
                    task: Task::Train,
                    params: &params,
                    data: &data,
                    output_model: Some(out),
                    input_model: None,
                    output_result: None,
                    predict_raw_score: false,
                },
                &dir,
            )
            .unwrap(),
        );
    }
    assert_eq!(text[0], text[1], "deterministic=true did not produce identical models");
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn the_missing_token_this_crate_writes_is_the_one_lightgbm_reads_as_missing() {
    // `train::MISSING_TOKEN` is the one assumption in the data writer that cannot be checked
    // without the real parser. Prove it: a row whose categorical value is missing must land where
    // the walker says a missing category lands (right, unconditionally), and that agreement is
    // exactly what the equality gate measures. A mismatch here means the token is wrong — fix the
    // TOKEN, never the walker.
    let Some(cli) = cli() else { return };
    let dir = scratch("missing");
    let (x, y) = toy(400);
    let data = dir.join("train.csv");
    let mut f = std::fs::File::create(&data).unwrap();
    TrainData::new(&x, &y, 3).unwrap().write_csv(&mut f).unwrap();
    drop(f);

    let model = dir.join("model.txt");
    let params = GbdtParams {
        num_iterations: 30,
        categorical_feature: Some(GbdtParams::categorical_feature_csv(&[2])),
        ..GbdtParams::default()
    };
    let text = cli
        .train(
            &TrainConfig {
                task: Task::Train,
                params: &params,
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

    let rows = [0.5, 0.5, f64::NAN, 0.5, 0.5, -1.0, 0.5, 0.5, 99.0];
    let theirs = cli.predict(&model, &rows, 3, &dir).expect("predict");
    let ours = parsed.predict_proba_batch(&rows, 3).unwrap();
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        assert!((a - b).abs() <= 1e-12, "row {i}: ours {a} vs LightGBM {b}");
    }
}

#[test]
#[ignore = "needs the pinned LightGBM binary; run explicitly on the CI box"]
fn a_binned_dataset_fits_and_carries_its_construction_time_decisions() {
    use vike_ml::grid::bin_dataset;

    let Some(cli) = cli() else { return };
    let dir = scratch("binning");
    let (x, y) = toy(400);
    let data = TrainData::new(&x, &y, 3).unwrap();
    let params = GbdtParams {
        num_iterations: 30,
        categorical_feature: Some(GbdtParams::categorical_feature_csv(&[2])),
        ..GbdtParams::default()
    };

    let csv_len = {
        let mut buf = Vec::new();
        data.write_csv(&mut buf).unwrap();
        buf.len() as u64
    };
    let binned = bin_dataset(
        &cli,
        &data,
        &params,
        vike_ml::train::BinPreFilter::MinDataAgnostic,
        &dir.join("fold.bin"),
        &dir,
    )
    .expect("bin");
    let bin_len = std::fs::metadata(&binned).unwrap().len();
    assert!(bin_len > 0);
    assert!(
        bin_len < csv_len,
        "the binary ({bin_len} B) should be materially smaller than the CSV ({csv_len} B) — that \
         size difference IS the ~260 GB of text I/O the grid does not do"
    );

    // Fit from the .bin, varying min_data_in_leaf — the axis that makes feature_pre_filter
    // load-bearing. If binning had pre-filtered features against the binning-time
    // min_data_in_leaf, this fit would silently see a reduced feature set and nothing would say
    // so: DatasetLoader::CheckDataset validates min_data_in_bin, max_bin, use_missing and
    // zero_as_missing, and min_data_in_leaf is not in that list.
    //
    // ⚠ The fit is driven HERE rather than through a helper, because the helper is gone: the
    // `fit_point`/`Workspace` pair this test used to call had no caller outside it, and a test
    // that is the only user of the thing it tests proves the thing exists, not that it is needed.
    // What the test is ABOUT — that the .bin carries its construction-time decisions — is
    // unchanged, and it now runs the same `TrainConfig` a real consumer builds.
    let fit_params = GbdtParams { min_data_in_leaf: 3, ..params.clone() };
    let model = dir.join("from-bin.model.txt");
    let text = cli
        .train(
            &TrainConfig {
                task: Task::Train,
                params: &fit_params,
                data: &binned,
                output_model: Some(&model),
                input_model: None,
                output_result: None,
                predict_raw_score: false,
            },
            &dir,
        )
        .expect("fit from the .bin");
    let parsed = parse_model_text(&text).expect("the walker must accept a fit from a .bin");
    assert!(parsed.trees.len() > 5, "{} trees", parsed.trees.len());

    // The categorical decision survived binning — which is the ONLY place it could have been made.
    assert!(
        parsed.trees.iter().any(|t| !t.cat_threshold.is_empty()),
        "a fit from the .bin has no categorical split: the save_binary config lost \
         categorical_feature, and every fit on this fold would inherit an all-numeric dataset"
    );
}
