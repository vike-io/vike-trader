use super::*;
use std::path::Path;

fn conf_for(params: &GbdtParams) -> String {
    TrainConfig {
        task: Task::Train,
        params,
        data: Path::new("/tmp/fold.bin"),
        output_model: Some(Path::new("/tmp/model.txt")),
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    }
    .render()
    .unwrap()
}

fn line<'a>(conf: &'a str, key: &str) -> Option<&'a str> {
    conf.lines().find_map(|l| l.trim().strip_prefix(&format!("{key}=")))
}

#[test]
fn train_data_accepts_a_well_shaped_matrix() {
    let x = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let y = [0.0f32, 1.0];
    let d = TrainData::new(&x, &y, 3).unwrap();
    assert_eq!(d.n_rows, 2);
}

#[test]
fn train_data_refuses_a_ragged_matrix() {
    let x = [1.0, 2.0, 3.0, 4.0, 5.0];
    let y = [0.0f32, 1.0];
    assert!(matches!(TrainData::new(&x, &y, 3), Err(MlError::Shape(_))));
}

#[test]
fn train_data_refuses_a_label_count_that_does_not_match_the_rows() {
    let x = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let y = [0.0f32];
    assert!(matches!(TrainData::new(&x, &y, 3), Err(MlError::Shape(_))));
}

#[test]
fn train_data_refuses_zero_features() {
    assert!(matches!(TrainData::new(&[], &[], 0), Err(MlError::Shape(_))));
}

/// The seam's fixture, moved with the accessor it exercises: 6 rows x 3 cols, row-major.
fn seam_data() -> (Vec<f64>, Vec<f32>) {
    ((0..18).map(|v| v as f64).collect(), vec![0.0, 1.0, 0.0, 1.0, 1.0, 0.0])
}

#[test]
fn a_row_is_a_contiguous_slice_of_the_row_major_buffer() {
    let (x, y) = seam_data();
    let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[2] };
    assert_eq!(d.row(0), &[0.0, 1.0, 2.0]);
    assert_eq!(d.row(5), &[15.0, 16.0, 17.0]);
}

#[test]
fn splitting_at_a_row_splits_both_the_features_and_the_labels() {
    let (x, y) = seam_data();
    let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[2] };
    let (a, b) = d.split_at_row(4);
    assert_eq!((a.n_rows, b.n_rows), (4, 2));
    assert_eq!(a.y, &[0.0, 1.0, 0.0, 1.0]);
    assert_eq!(b.y, &[1.0, 0.0]);
    assert_eq!(b.row(0), &[12.0, 13.0, 14.0]);
    assert_eq!(a.categorical, b.categorical, "the schema does not change across a split");
}

/// The assertion the test above CANNOT make, and the one a real trainer depends on.
///
/// A `split_at_row` that handed the first half the parent's whole `x` buffer passes every
/// assertion above: `a.row(0..4)` reads the same bytes either way, and `a.y` is already
/// correct. The difference is only visible in `a.x.len()` — and [`TrainData::write_csv`]
/// derives its own row count from `x.len() / n_cols`, so that half would train 6 rows of
/// features against 4 labels. This is the assertion that says the flat buffer, not just the
/// row accessor, was cut.
#[test]
fn a_split_half_carries_only_its_own_rows_in_the_flat_buffer() {
    let (x, y) = seam_data();
    let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[2] };
    let (a, b) = d.split_at_row(4);
    assert_eq!(a.x.len(), a.n_rows * a.n_cols);
    assert_eq!(b.x.len(), b.n_rows * b.n_cols);
    assert_eq!(a.y.len(), a.n_rows);
    assert_eq!(b.y.len(), b.n_rows);
    assert_eq!(b.x, &[12.0, 13.0, 14.0, 15.0, 16.0, 17.0]);
    // ...and both halves are still well-shaped, which is what makes a half directly fittable.
    a.validate().unwrap();
    b.validate().unwrap();
}

#[test]
fn splitting_at_either_end_yields_an_empty_half_rather_than_panicking() {
    let (x, y) = seam_data();
    let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[] };
    let (a, b) = d.split_at_row(0);
    assert_eq!((a.n_rows, a.x.len(), a.y.len()), (0, 0, 0));
    assert_eq!(b.n_rows, 6);
    // A cut past the end is CLAMPED — a caller that computed it from a shorter axis gets an
    // empty second half, not a slice panic.
    let (a, b) = d.split_at_row(99);
    assert_eq!(a.n_rows, 6);
    assert_eq!((b.n_rows, b.x.len(), b.y.len()), (0, 0, 0));
}

/// The half of the merge that a struct literal can get wrong and [`TrainData::new`] cannot:
/// an `n_rows` the buffer does not have. The CSV writer never reads the field, so this is the
/// only place the disagreement is visible before a child process trains the wrong row count.
#[test]
fn validate_refuses_a_row_count_the_flat_buffer_does_not_have() {
    let (x, y) = seam_data();
    let honest = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[] };
    honest.validate().unwrap();
    let lying = TrainData { n_rows: 4, ..honest };
    let err = lying.validate().unwrap_err().to_string();
    assert!(err.contains("expected 12"), "{err}");
    // ...the labels are checked against the same declared count...
    let err = TrainData { y: &y[..5], ..honest }.validate().unwrap_err().to_string();
    assert!(err.contains("6 rows but 5 labels"), "{err}");
    // ...and a zero-width matrix is refused rather than divided by.
    let err = TrainData { x: &[], y: &[], n_rows: 0, n_cols: 0, categorical: &[] }
        .validate()
        .unwrap_err()
        .to_string();
    assert!(err.contains("n_cols is 0"), "{err}");
}

#[test]
fn a_categorical_index_outside_the_row_is_refused_rather_than_ignored() {
    // LightGBM ignores an out-of-range index in silence, bins the column as a NUMBER, and the
    // saved model's parameter echo still claims the column was categorical.
    let (x, y) = seam_data();
    let d = TrainData { x: &x, y: &y, n_rows: 6, n_cols: 3, categorical: &[3] };
    let err = d.validate().unwrap_err().to_string();
    assert!(err.contains("categorical column 3"), "{err}");
    assert!(TrainData::new(&x, &y, 3).unwrap().with_categorical(&[3]).is_err());
    assert!(TrainData::new(&x, &y, 3).unwrap().with_categorical(&[2]).is_ok());
}

/// `new` derives the row count rather than taking one, so the disagreement above is
/// unrepresentable on that path — and it declares NO categorical column, which is not the
/// same as declaring an empty one.
#[test]
fn new_derives_the_row_count_and_declares_no_categorical_column() {
    let (x, y) = seam_data();
    let d = TrainData::new(&x, &y, 3).unwrap();
    assert_eq!(d.n_rows, 6);
    assert_eq!(d.n_cols, 3);
    assert!(d.categorical.is_empty());
}

#[test]
fn the_data_file_puts_the_label_first_and_writes_no_header() {
    let x = [1.0, 2.0, 3.0, 4.0];
    let y = [0.0f32, 1.0];
    let mut out = Vec::new();
    TrainData::new(&x, &y, 2).unwrap().write_csv(&mut out).unwrap();
    // `label_column=0` is LightGBM's own default, and `header=false` means row 1 is data.
    assert_eq!(String::from_utf8(out).unwrap(), "0,1,2\n1,3,4\n");
}

#[test]
fn the_writer_emits_the_shortest_decimal_that_round_trips_the_bits() {
    // ⚠ This tests OUR HALF ONLY, and says so, because the obvious version of this test is a
    // tautology: formatting with Rust's `{}` and parsing back with Rust's parser proves a
    // property of Rust and nothing about LightGBM. LightGBM's side is NOT correctly rounded by
    // default — `precise_float_parser` defaults to false and the default `Common::Atof` clamps
    // `expon` at 308 — which is why `render` writes `precise_float_parser=true` (asserted
    // below) and why the END-TO-END lossless claim is proven by the equality gate against the
    // real binary in Task 15, not here.
    let hard = [0.1, 1.0 / 3.0, f64::MIN_POSITIVE, 1e308, -0.0, 5e-324];
    let y = vec![0.0f32; hard.len()];
    let mut out = Vec::new();
    TrainData::new(&hard, &y, 1).unwrap().write_csv(&mut out).unwrap();
    let back: Vec<f64> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| l.split(',').nth(1).unwrap().parse::<f64>().unwrap())
        .collect();
    for (a, b) in hard.iter().zip(&back) {
        assert_eq!(a.to_bits(), b.to_bits(), "{a} did not survive our own formatter");
    }
}

#[test]
fn a_prediction_file_carries_a_dummy_label_column_in_the_training_files_shape() {
    // ⚠ THE trap that would have killed the whole gate. `Predictor::Predict` sets
    // `label_idx = header ? -1 : boosting_->LabelIdx()` — 0 by default — and
    // `CSVParser::NumFeatures()` returns `total_columns_ - (label_idx_ >= 0)`. A label-less
    // prediction file therefore makes an N-feature model see N-1 features and Log::Fatal.
    // Upstream's own examples/binary_classification/binary.test carries a label column.
    let mut out = Vec::new();
    write_predict_csv(&[1.0, 2.0, 3.0, 4.0], 2, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "0,1,2\n0,3,4\n");
}

#[test]
fn a_prediction_file_and_a_training_file_have_the_same_column_count() {
    // The property that actually matters, stated as a property rather than as two literals:
    // whatever the trainer saw, the predictor must see.
    let x = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let y = [0.0f32, 1.0];
    let mut train = Vec::new();
    TrainData::new(&x, &y, 3).unwrap().write_csv(&mut train).unwrap();
    let mut pred = Vec::new();
    write_predict_csv(&x, 3, &mut pred).unwrap();
    let cols = |b: &[u8]| String::from_utf8_lossy(b).lines().next().unwrap().split(',').count();
    assert_eq!(cols(&train), cols(&pred));
}

#[test]
fn write_predict_csv_refuses_a_ragged_matrix() {
    assert!(matches!(
        write_predict_csv(&[1.0, 2.0, 3.0], 2, &mut Vec::new()),
        Err(MlError::Shape(_))
    ));
    assert!(matches!(write_predict_csv(&[], 0, &mut Vec::new()), Err(MlError::Shape(_))));
}

#[test]
fn every_text_reading_task_asks_for_the_precise_float_parser() {
    // `precise_float_parser` defaults to FALSE, and only `true` routes through AtofPrecise.
    // The default Common::Atof is hand-rolled, clamps expon at 308 and is not correctly
    // rounded — so a value near a split threshold could route differently in LightGBM than in
    // the walker, and the equality gate would report a routing bug that does not exist.
    let p = GbdtParams::default();
    for task in [
        Task::Train,
        Task::Predict,
        Task::SaveBinary(BinPreFilter::MinDataAgnostic),
        Task::SaveBinary(BinPreFilter::MatchFit),
    ] {
        let conf = TrainConfig {
            task,
            params: &p,
            data: Path::new("/tmp/d.csv"),
            output_model: Some(Path::new("/tmp/m.txt")),
            input_model: Some(Path::new("/tmp/m.txt")),
            output_result: Some(Path::new("/tmp/r.txt")),
            predict_raw_score: false,
        }
        .render()
        .unwrap();
        assert_eq!(line(&conf, "precise_float_parser"), Some("true"), "{task:?}");
    }
}

#[test]
fn binning_disables_feature_pre_filter_because_the_whole_grid_inherits_the_bins() {
    // ⚠ The SECOND binning decision every fit from the binary inherits, and the silent one.
    // LightGBM's docs, verbatim: "as dataset object is initialized only once and cannot be
    // changed after that, you may need to set this to false when searching parameters with
    // min_data_in_leaf, otherwise features are filtered by min_data_in_leaf firstly if you
    // don't reconstruct dataset object". `min_data_in_leaf` IS a grid axis here (Tasks 9/10),
    // and `DatasetLoader::CheckDataset` validates min_data_in_bin / max_bin / use_missing /
    // zero_as_missing — NOT min_data_in_leaf — so a mismatch never says anything.
    //
    // It is a CONSTRUCTION-time parameter: putting it in the fit config does nothing at all.
    let conf = TrainConfig {
        task: Task::SaveBinary(BinPreFilter::MinDataAgnostic),
        params: &GbdtParams::default(),
        data: Path::new("/tmp/d.csv"),
        output_model: None,
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    }
    .render()
    .unwrap();
    assert_eq!(line(&conf, "feature_pre_filter"), Some("false"));
    assert_eq!(
        line(&conf, "min_data_in_leaf"),
        None,
        "an agnostic bin must not bake the value it exists to stay agnostic of"
    );
}

#[test]
fn a_match_fit_binning_bakes_the_fits_min_data_in_leaf_and_keeps_the_default_pre_filter() {
    // The other arm: construction byte-identical to what a plain CSV fit performs, which means
    // the fit's own `min_data_in_leaf` under LightGBM's DEFAULT pre-filter. Writing
    // `feature_pre_filter=false` here — or omitting `min_data_in_leaf` — would each produce a
    // binary whose trees differ from the CSV fit's whenever the pre-filter bites, measured on
    // the real binary (see [`BinPreFilter`]'s doc). The parity claim itself is gated against
    // the real binary by `crates/vike-ml/tests/gbdt_learner_smoke.rs`'s matrix smoke.
    let params = GbdtParams { min_data_in_leaf: 50, ..GbdtParams::default() };
    let conf = TrainConfig {
        task: Task::SaveBinary(BinPreFilter::MatchFit),
        params: &params,
        data: Path::new("/tmp/d.csv"),
        output_model: None,
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    }
    .render()
    .unwrap();
    assert_eq!(line(&conf, "min_data_in_leaf"), Some("50"));
    assert_eq!(
        line(&conf, "feature_pre_filter"),
        None,
        "the default pre-filter IS the CSV fit's construction — writing false here is the \
             measured tree-changing divergence, and writing true would be a redundant restatement \
             that could drift from upstream's default"
    );
}

#[test]
fn a_missing_value_is_written_as_the_token_lightgbm_reads_as_missing() {
    let x = [f64::NAN, 1.0];
    let y = [0.0f32];
    let mut out = Vec::new();
    TrainData::new(&x, &y, 2).unwrap().write_csv(&mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), format!("0,{MISSING_TOKEN},1\n"));
}

#[test]
fn the_config_pins_num_threads_to_one() {
    // ⚠ THE headline measurement of the backend bakeoff: on the 600x943 shape this study
    // trains, LightGBM's default (`num_threads` = all cores) measured 143,293 ms against
    // 2,105 ms at one thread — a 68x CLIFF, in the wrong direction, silently. The binary is
    // built `-DUSE_OPENMP=OFF` so the cliff cannot exist (Task 12), and the config says it
    // anyway so that a rebuild WITH OpenMP does not resurrect it. Two independent defences,
    // because this one costs weeks of wall clock when it goes wrong.
    assert_eq!(line(&conf_for(&GbdtParams::default()), "num_threads"), Some("1"));
}

#[test]
fn the_config_carries_the_determinism_knobs() {
    let conf = conf_for(&GbdtParams::default());
    assert_eq!(line(&conf, "deterministic"), Some("true"));
    assert_eq!(line(&conf, "force_row_wise"), Some("true"));
    assert!(line(&conf, "seed").is_some());
}

#[test]
fn a_categorical_feature_reaches_the_config_with_no_brackets() {
    let params = GbdtParams {
        categorical_feature: Some(GbdtParams::categorical_feature_csv(&[0, 3])),
        ..GbdtParams::default()
    };
    assert_eq!(line(&conf_for(&params), "categorical_feature"), Some("0,3"));
    assert!(!conf_for(&params).contains('['), "KV2Map strips quotes, not brackets");
}

#[test]
fn no_categorical_feature_means_the_key_is_absent_rather_than_empty() {
    // `categorical_feature=` with an empty value is not the same as not saying it, and an
    // empty list is a value LightGBM has to interpret. Say nothing instead.
    assert_eq!(line(&conf_for(&GbdtParams::default()), "categorical_feature"), None);
}

#[test]
fn each_task_names_the_files_that_task_actually_uses() {
    let p = GbdtParams::default();
    let train = conf_for(&p);
    assert_eq!(line(&train, "task"), Some("train"));
    assert_eq!(line(&train, "output_model"), Some("/tmp/model.txt"));
    assert_eq!(line(&train, "input_model"), None);

    let predict_conf = |raw: bool| {
        TrainConfig {
            task: Task::Predict,
            params: &p,
            data: Path::new("/tmp/eval.csv"),
            output_model: None,
            input_model: Some(Path::new("/tmp/model.txt")),
            output_result: Some(Path::new("/tmp/pred.txt")),
            predict_raw_score: raw,
        }
        .render()
        .unwrap()
    };

    let predict = predict_conf(false);
    assert_eq!(line(&predict, "task"), Some("predict"));
    assert_eq!(line(&predict, "input_model"), Some("/tmp/model.txt"));
    assert_eq!(line(&predict, "output_result"), Some("/tmp/pred.txt"));
    assert_eq!(
        line(&predict, "predict_raw_score"),
        Some("false"),
        "`false` is what makes this predict_proba rather than the margin"
    );
    assert_eq!(
        line(&predict_conf(true), "predict_raw_score"),
        Some("true"),
        "and `true` is the pre-transform margin the exact-equality gate compares"
    );
}

#[test]
fn a_train_task_with_no_output_model_is_refused_rather_than_writing_somewhere_default() {
    // LightGBM's own default is `output_model=LightGBM_model.txt` IN THE WORKING DIRECTORY.
    // With 32 workers in one scratch root that is a collision, not a default.
    let err = TrainConfig {
        task: Task::Train,
        params: &GbdtParams::default(),
        data: Path::new("/tmp/fold.bin"),
        output_model: None,
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    }
    .render()
    .unwrap_err();
    assert!(err.to_string().contains("output_model"), "{err}");
}

#[test]
fn a_path_that_would_break_the_line_format_is_refused() {
    let err = TrainConfig {
        task: Task::Train,
        params: &GbdtParams::default(),
        data: Path::new("/tmp/two\nlines"),
        output_model: Some(Path::new("/tmp/model.txt")),
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    }
    .render()
    .unwrap_err();
    assert!(err.to_string().contains("newline"), "{err}");
}
