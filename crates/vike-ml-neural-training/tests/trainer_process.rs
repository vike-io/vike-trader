//! The learner seam over the trainer PROCESS, end to end: `vike_ml_training::NeuralLearner` drives the
//! `vike-ml-neural-training` binary this crate builds, as a study host does outside the jail. Each network
//! learns, the held model answers what the exported ONNX answers (the same bytes, read by the one ONNX
//! runner), and a bad spec is the fit's `Err`, never a hang.

use std::path::PathBuf;

use vike_ml_onnx_prediction::OnnxModel;
use vike_ml_prediction::{PredictionKind, Predictor};
use vike_ml_training::{
    Capture, DEFAULT_POINT, GridPoint, Learner, NeuralLearner, NeuralModel, TrainData,
};

use crate::common::{momentum_returns, sharpe_of, uniform, windows_and_next};

const ROWS: usize = 96;
const STEPS: usize = 6;

/// A scratch root of this test's own under cargo's per-target temporary directory.
fn scratch(tag: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("trainer-process-{tag}"))
}

fn learner(tag: &str, spec: &str) -> NeuralLearner {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_vike-ml-neural-training"));
    NeuralLearner::new(&binary, &scratch(tag), spec).expect("the trainer is built beside this test")
}

/// `rows` windows of `STEPS` values, step-major, and per window 1 when its sum is positive.
fn sign_of_sum(rows: usize) -> (Vec<f64>, Vec<f32>) {
    let windows: Vec<f64> = uniform(11, rows * STEPS).into_iter().map(f64::from).collect();
    let labels = windows
        .chunks(STEPS)
        .map(|w| if w.iter().sum::<f64>() > 0.0 { 1.0 } else { 0.0 })
        .collect();
    (windows, labels)
}

const POINT: GridPoint = GridPoint { learning_rate: 0.02, ..DEFAULT_POINT };

/// A network fitted through the seam: its held answers are right on most training rows, and are the
/// bits the exported ONNX gives for the same rows.
fn learns_and_matches_its_onnx(tag: &str, spec: &str, input_shape: &[usize]) {
    let (x, y) = sign_of_sum(ROWS);
    let data = TrainData::new(&x, &y, STEPS).expect("data");
    let want = Capture { importance: false, text: true };
    let (model, importance, text) =
        learner(tag, spec).fit_captured(&data, &POINT, 7, want).expect("the trainer fits");
    assert!(importance.is_none(), "a network reports no importance");
    assert_eq!(model.kind(), PredictionKind::Probability);
    let onnx = OnnxModel::from_model_text(&text.expect("the model text"), input_shape)
        .expect("the captured text is the ONNX the trainer wrote");
    let mut right = 0;
    for (row, &label) in x.chunks(STEPS).zip(&y) {
        let held = model.predict_one(row).expect("the held model answers");
        let floats: Vec<f32> = row.iter().map(|&v| v as f32).collect();
        let exported = onnx.predict(&floats).expect("the ONNX answers")[0];
        assert_eq!(held, f64::from(exported), "the held model and its ONNX disagree");
        right += usize::from((held > 0.5) == (label == 1.0));
    }
    assert!(right * 10 >= ROWS * 8, "{tag}: right on {right} of {ROWS}");
}

#[test]
fn an_mlp_fitted_through_the_seam_learns_and_answers_its_onnx() {
    let spec =
        "network = \"mlp\"\nhidden = [16]\nloss = \"binary\"\nepochs = 40\nbatch_size = 16\n";
    learns_and_matches_its_onnx("mlp", spec, &[1, STEPS]);
}

#[test]
fn an_lstm_fitted_through_the_seam_learns_and_answers_its_onnx() {
    let spec = "network = \"lstm\"\nhidden = [8]\nsteps = 6\nloss = \"binary\"\nepochs = 40\nbatch_size = 16\n";
    learns_and_matches_its_onnx("lstm", spec, &[1, STEPS, 1]);
}

#[test]
fn a_tcn_fitted_through_the_seam_learns_and_answers_its_onnx() {
    // One feature per step, so the channel-major row is the step-major one.
    let spec = "network = \"tcn\"\nchannels = [8, 8]\nkernel_size = 3\nsteps = 6\nloss = \"binary\"\n\
                epochs = 40\nbatch_size = 16\n";
    learns_and_matches_its_onnx("tcn", spec, &[1, 1, STEPS]);
}

/// The Sharpe objective through the seam (W24): the label is each row's next return, the held model
/// is a VALUE (its provenance's `loss=sharpe`, never assumed a probability), its answers are positions
/// in [-1, 1], they are the bits the exported ONNX gives, and on later rows they earn a positive
/// Sharpe ratio.
#[test]
fn an_lstm_trained_on_the_sharpe_objective_answers_positions() {
    let steps = 8;
    let (windows, next) = windows_and_next(&momentum_returns(21, 600 + steps), steps);
    let (fit_rows, later) = (400, 200);
    let x: Vec<f64> = windows[..fit_rows * steps].iter().map(|&v| f64::from(v)).collect();
    let data = TrainData::new(&x, &next[..fit_rows], steps).expect("data");
    let spec = "network = \"lstm\"\nhidden = [8]\nsteps = 8\nloss = \"sharpe\"\nturnover_cost = 0.0\n\
                epochs = 30\nbatch_size = 50\n";
    let point = GridPoint { learning_rate: 0.01, ..DEFAULT_POINT };
    let want = Capture { importance: false, text: true };
    let (model, _, text) =
        learner("sharpe", spec).fit_captured(&data, &point, 3, want).expect("the trainer fits");
    assert_eq!(model.kind(), PredictionKind::Value, "a position is a value, not a probability");
    let onnx = OnnxModel::from_model_text(&text.expect("the model text"), &[1, steps, 1])
        .expect("the captured text is the ONNX the trainer wrote");
    let mut positions = Vec::new();
    for row in windows[fit_rows * steps..].chunks(steps) {
        let wide: Vec<f64> = row.iter().map(|&v| f64::from(v)).collect();
        let held = model.predict_one(&wide).expect("the held model answers");
        assert_eq!(held, f64::from(onnx.predict(row).expect("the ONNX answers")[0]));
        assert!(held.abs() <= 1.0, "a position is in [-1, 1]: {held}");
        positions.push(held as f32);
    }
    assert_eq!(positions.len(), later);
    let sharpe = sharpe_of(&positions, &next[fit_rows..]);
    assert!(sharpe > 0.2, "out of sample the positions earn a Sharpe ratio of {sharpe} per period");
}

#[test]
fn a_deeplob_fitted_through_the_seam_answers_class_probabilities_its_onnx_answers() {
    // Rows of 6 steps of one book level (4 numbers a step); the class is the sign of the window's sum.
    let (steps, width) = (6, 4);
    let x: Vec<f64> = uniform(13, 48 * steps * width).into_iter().map(f64::from).collect();
    let y: Vec<f32> = x
        .chunks(steps * width)
        .map(|w| match w.iter().sum::<f64>() {
            sum if sum > 1.0 => 2.0,
            sum if sum < -1.0 => 0.0,
            _ => 1.0,
        })
        .collect();
    let data = TrainData::new(&x, &y, steps * width).expect("data");
    let spec = "network = \"deeplob\"\nchannels = [2, 2]\nhidden = [4]\nsteps = 6\nloss = \"multiclass\"\n\
                classes = 3\nepochs = 2\nbatch_size = 16\n";
    let want = Capture { importance: false, text: true };
    let (model, _, text) =
        learner("deeplob", spec).fit_captured(&data, &POINT, 5, want).expect("the trainer fits");
    assert_eq!(model.kind(), PredictionKind::ClassProbabilities { class_count: 3 });
    let onnx = OnnxModel::from_model_text(&text.expect("the model text"), &[1, 1, steps, width])
        .expect("the captured text is the ONNX the trainer wrote");
    for row in x.chunks(steps * width).take(8) {
        let mut held = [0.0; 3];
        model.predict_into(row, &mut held).expect("the held model answers");
        let floats: Vec<f32> = row.iter().map(|&v| v as f32).collect();
        let exported = onnx.predict(&floats).expect("the ONNX answers");
        assert_eq!(held.to_vec(), exported.iter().map(|&v| f64::from(v)).collect::<Vec<_>>());
        assert!(
            (held.iter().sum::<f64>() - 1.0).abs() < 1e-5,
            "probabilities sum to one: {held:?}"
        );
    }
    assert!(model.predict_one(&x[..steps * width]).is_err(), "three numbers are not one");
}

#[test]
fn a_bad_spec_is_the_fits_error_with_the_trainers_reason() {
    let (x, y) = sign_of_sum(8);
    let data = TrainData::new(&x, &y, STEPS).expect("data");
    let spec = "network = \"mlp\"\nhiden = [16]\nloss = \"binary\"\nepochs = 1\nbatch_size = 4\n";
    let error = learner("bad-spec", spec).fit(&data, &POINT, 0).expect_err("refused");
    assert!(error.contains("does not parse"), "{error}");
    let tree = GridPoint { max_depth: 9, ..POINT };
    let error = learner("tree-axis", spec).fit(&data, &tree, 0).expect_err("refused");
    assert!(error.contains("no leaves"), "{error}");
    let costly = "network = \"mlp\"\nhidden = [4]\nloss = \"binary\"\nturnover_cost = 0.1\nepochs = 1\n\
                  batch_size = 4\n";
    let error = learner("cost-on-binary", costly).fit(&data, &POINT, 0).expect_err("refused");
    assert!(error.contains("only to `loss = \"sharpe\"`"), "{error}");
    let base = "network = \"mlp\"\nhidden = [4]\nloss = \"binary\"\nepochs = 1\nbatch_size = 4\n";
    let error = learner("no-warm-epochs", &format!("{base}warm_start = true\n"))
        .fit(&data, &POINT, 0)
        .expect_err("refused");
    assert!(error.contains("needs `warm_epochs`"), "{error}");
    let error = learner("cold-warm-epochs", &format!("{base}warm_epochs = 1\n"))
        .fit(&data, &POINT, 0)
        .expect_err("refused");
    assert!(error.contains("only to `warm_start = true`"), "{error}");
}

/// The spec of the chain tests: two passes from the seed, one pass for a fold that continues the one
/// before it.
const WARM: &str = "network = \"mlp\"\nhidden = [8]\nloss = \"binary\"\nepochs = 2\nbatch_size = 8\n\
                    warm_start = true\nwarm_epochs = 1\n";

/// A warm chain's next fold starts from EXACTLY its previous fold's network: trained at a step far
/// too small to move an `f32` weight, it answers that fold's numbers bit for bit, where a cold fit at
/// the same step answers the seed's own network. Chains are strictly forward and kept apart by name,
/// and a fold whose network changed shape is refused in words.
#[test]
fn a_warm_chains_fold_starts_from_exactly_the_previous_folds_network() {
    let (x, y) = sign_of_sum(ROWS);
    let data = TrainData::new(&x, &y, STEPS).expect("data");
    let learner = learner("warm-chain", WARM);
    let still = GridPoint { learning_rate: 1e-30, ..POINT };
    let fold = |point: &GridPoint, chain: &str, step: i64| {
        learner.fit_chained(&data, point, 3, Capture::default(), chain, step).map(|fit| fit.0)
    };
    let answers = |model: &NeuralModel| -> Vec<u64> {
        let scores = model.predict_batch(&x, STEPS).expect("answers");
        scores.iter().map(|v| v.to_bits()).collect()
    };
    let first = fold(&POINT, "long", 1).expect("fold 1, from the seed");
    let second = fold(&still, "long", 2).expect("fold 2, from fold 1");
    assert_eq!(answers(&second), answers(&first), "fold 2 starts where fold 1 ended");
    let cold = learner.fit(&data, &still, 3).expect("a cold fit");
    assert_ne!(answers(&cold), answers(&first), "a cold fit starts from the seed");
    let other = fold(&still, "short", 1).expect("another chain starts from the seed");
    assert_eq!(answers(&other), answers(&cold), "chains are kept apart");

    let again = fold(&POINT, "long", 2).map(|_| ()).expect_err("not after");
    assert!(again.contains("step 2 is not after its last fold's 2"), "{again}");
    let wide: Vec<f64> = x.chunks(STEPS).flat_map(|row| row.iter().chain(row)).copied().collect();
    let wide = TrainData::new(&wide, &y, 2 * STEPS).expect("wider rows");
    let changed = learner
        .fit_chained(&wide, &POINT, 3, Capture::default(), "long", 3)
        .map(|_| ())
        .expect_err("refused");
    assert!(changed.contains("the previous fold's network differs"), "{changed}");
    fold(&still, "long", 4).expect("the chain stays at fold 2's network after a refused fold");
}

/// A fold whose training diverges (Adam at a step of 1e35) is refused in words that name the fold, and
/// its non-finite weights never reach the chain: the next fold starts from the fold before it, bit
/// for bit (trained at a step too small to move an `f32`, it answers that fold's numbers).
#[test]
fn a_diverged_fold_is_refused_and_the_chain_stays_at_the_fold_before() {
    let (x, y) = sign_of_sum(ROWS);
    let data = TrainData::new(&x, &y, STEPS).expect("data");
    let learner = learner("diverged-chain", WARM);
    let fold = |rate: f64, step: i64| {
        let point = GridPoint { learning_rate: rate, ..POINT };
        learner.fit_chained(&data, &point, 3, Capture::default(), "long", step).map(|fit| fit.0)
    };
    let answers = |model: &NeuralModel| -> Vec<u64> {
        let scores = model.predict_batch(&x, STEPS).expect("answers");
        scores.iter().map(|v| v.to_bits()).collect()
    };
    let first = fold(POINT.learning_rate, 1).expect("fold 1");
    let diverged = fold(1e35, 2).map(|_| ()).expect_err("a diverged fold is refused");
    assert!(diverged.contains("chain `long`, step 2:"), "{diverged}");
    assert!(diverged.contains("diverged"), "{diverged}");
    let next = fold(1e-30, 3).expect("fold 3, from fold 1");
    assert_eq!(answers(&next), answers(&first), "fold 3 starts where fold 1 ended");
}

/// Without `warm_start` a chained fit is the plain fit it always was: the same seed gives the same
/// ONNX bytes fold after fold, and no fold starts from another.
#[test]
fn without_warm_start_a_chained_fit_is_the_plain_fit_bit_for_bit() {
    let (x, y) = sign_of_sum(ROWS);
    let data = TrainData::new(&x, &y, STEPS).expect("data");
    let spec = "network = \"mlp\"\nhidden = [8]\nloss = \"binary\"\nepochs = 2\nbatch_size = 8\n";
    let learner = learner("cold-chain", spec);
    let want = Capture { importance: false, text: true };
    let plain = learner.fit_captured(&data, &POINT, 5, want).expect("a plain fit").2;
    assert!(plain.is_some(), "the model's text");
    for step in 1..=2 {
        let chained = learner.fit_chained(&data, &POINT, 5, want, "long", step).expect("chained");
        assert_eq!(chained.2, plain, "fold {step} is the plain fit, bit for bit");
    }
}

/// The binary itself refuses a malformed data file (neither the matrix `NeuralLearner` writes nor an
/// older host's CSV, a cut matrix, a ragged CSV), exits 1 and names the reason; it trains on the
/// matrix and, for a rollback of the host alone, on the CSV an older host writes.
#[test]
fn the_trainer_refuses_a_malformed_data_file_and_says_why() {
    let dir = scratch("malformed");
    std::fs::create_dir_all(&dir).expect("dir");
    let spec = dir.join("spec.toml");
    std::fs::write(
        &spec,
        "network = \"mlp\"\nhidden = [4]\nloss = \"binary\"\nepochs = 1\nbatch_size = 4\n",
    )
    .expect("spec");
    let (x, y) = sign_of_sum(8);
    let mut matrix = Vec::new();
    TrainData::new(&x, &y, STEPS).expect("data").write_matrix(&mut matrix).expect("written");
    let train = |name: &str, bytes: &[u8]| {
        let data = dir.join(name);
        std::fs::write(&data, bytes).expect("data file");
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_vike-ml-neural-training"))
            .args(["train", "--seed", "0", "--learning-rate", "0.01"])
            .arg("--data")
            .arg(&data)
            .arg("--spec")
            .arg(&spec)
            .arg("--out")
            .arg(&dir)
            .output()
            .expect("the trainer runs");
        (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let (code, why) = train("garbage", &[0xff, 0xfe, 0x00, 0x01]);
    assert_eq!(code, Some(1), "{why}");
    assert!(why.contains("neither a training matrix"), "{why}");
    let mut csv = Vec::new();
    TrainData::new(&x, &y, STEPS).expect("data").write_csv(&mut csv).expect("csv");
    let (code, why) = train("older-host.csv", &csv);
    assert_eq!(code, Some(0), "an older host's CSV still trains (a rollback of the host): {why}");
    let (code, why) = train("ragged.csv", b"1,0.5,0.25\n0,1\n");
    assert_eq!(code, Some(1), "{why}");
    assert!(why.contains("row 2: 1 features"), "{why}");
    let (code, why) = train("cut.matrix", &matrix[..matrix.len() - 4]);
    assert_eq!(code, Some(1), "{why}");
    assert!(why.contains("not the 8 rows x 6 features"), "{why}");
    let (code, why) = train("whole.matrix", &matrix);
    assert_eq!(code, Some(0), "{why}");
}

#[test]
fn a_held_model_refuses_a_row_of_the_wrong_width() {
    let (x, y) = sign_of_sum(16);
    let data = TrainData::new(&x, &y, STEPS).expect("data");
    let spec =
        "network = \"mlp\"\nhidden = [4]\nloss = \"regression\"\nepochs = 1\nbatch_size = 8\n";
    let model = learner("width", spec).fit(&data, &POINT, 0).expect("the trainer fits");
    assert_eq!(model.kind(), PredictionKind::Value);
    let error = model.predict_one(&[0.0; 3]).expect_err("refused");
    assert!(error.contains("takes 6 features"), "{error}");
    assert_eq!(
        model.predict_batch(&[0.0; 6], 3),
        Err(error),
        "a batch refuses it in the same words"
    );
}

/// A held network's batch streams every row through the child in one pass and answers the bits row
/// by row answers, in row order, over more than a 64 kB pipe holds either way (some 700 kB of rows,
/// 65 kB of answers), so the stream cannot stall on a full pipe.
#[test]
fn a_held_networks_batch_answers_the_bits_each_row_answers() {
    let (x, y) = sign_of_sum(16);
    let data = TrainData::new(&x, &y, STEPS).expect("data");
    let spec = "network = \"mlp\"\nhidden = [4]\nloss = \"binary\"\nepochs = 1\nbatch_size = 8\n";
    let model = learner("batch", spec).fit(&data, &POINT, 0).expect("the trainer fits");
    let rows: Vec<f64> = uniform(17, 6_000 * STEPS).into_iter().map(f64::from).collect();
    let per_row: Vec<u64> = rows
        .chunks(STEPS)
        .map(|row| model.predict_one(row).expect("the held model answers").to_bits())
        .collect();
    let batch = model.predict_batch(&rows, STEPS).expect("the batch");
    assert_eq!(batch.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), per_row);
    assert_eq!(model.predict_batch(&[], STEPS), Ok(vec![]));
}

/// **The predict pipe answers a frame with the bits it answers text lines** (2026-11-19): the trainer's
/// `ready` line offers predict frames, and a test block of rows sent as ONE frame is answered with the
/// numbers the same rows sent as comma-separated lines (what an older host sends, and still answered) get,
/// bit for bit, in a fraction of the bytes. Prints each form's bytes and time.
#[test]
fn the_predict_pipe_answers_a_frame_with_the_bits_it_answers_text_lines() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use vike_ml_training::train::matrix::{PREDICT_WORD, predict_frame, read_frame};

    let dir = scratch("predict-forms");
    std::fs::create_dir_all(&dir).expect("dir");
    let (x, y) = sign_of_sum(16);
    let mut matrix = Vec::new();
    TrainData::new(&x, &y, STEPS).expect("data").write_matrix(&mut matrix).expect("written");
    std::fs::write(dir.join("data.matrix"), &matrix).expect("data file");
    let spec = "network = \"mlp\"\nhidden = [4]\nloss = \"binary\"\nepochs = 1\nbatch_size = 8\n";
    std::fs::write(dir.join("spec.toml"), spec).expect("spec");
    let binary = env!("CARGO_BIN_EXE_vike-ml-neural-training");
    let trained = Command::new(binary)
        .args(["train", "--seed", "0", "--learning-rate", "0.02", "--data"])
        .arg(dir.join("data.matrix"))
        .arg("--spec")
        .arg(dir.join("spec.toml"))
        .arg("--out")
        .arg(&dir)
        .output()
        .expect("the trainer runs");
    assert!(trained.status.success(), "{}", String::from_utf8_lossy(&trained.stderr));
    let mut child = Command::new(binary)
        .arg("predict")
        .arg("--model")
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the predicting child");
    let mut input = child.stdin.take().expect("stdin");
    let mut output = BufReader::new(child.stdout.take().expect("stdout"));
    let mut ready = String::new();
    output.read_line(&mut ready).expect("ready");
    assert!(ready.split_whitespace().any(|word| word == PREDICT_WORD), "{ready}");

    let rows: Vec<f64> = uniform(23, 20_000 * STEPS).into_iter().map(f64::from).collect();
    let text: Vec<u8> = rows
        .chunks(STEPS)
        .flat_map(|row| {
            let line: Vec<String> = row.iter().map(f64::to_string).collect();
            format!("{}\n", line.join(",")).into_bytes()
        })
        .collect();
    let frame = predict_frame(&rows, STEPS);
    // Each request written on a thread of its own while this one reads the answers: neither pipe fills.
    type Answers<'a> =
        &'a mut dyn FnMut(&mut BufReader<std::process::ChildStdout>) -> (Vec<u32>, usize);
    let mut exchange = |request: &[u8], read: Answers<'_>| {
        let started = Instant::now();
        let answered = std::thread::scope(|scope| {
            let input = &mut input;
            scope.spawn(move || {
                input.write_all(request).and_then(|()| input.flush()).expect("rows")
            });
            read(&mut output)
        });
        (answered, started.elapsed())
    };
    let ((by_line, line_bytes), line_time) = exchange(&text, &mut |output| {
        let (mut answers, mut bytes, mut line) = (Vec::new(), 0, String::new());
        for _ in 0..rows.len() / STEPS {
            line.clear();
            bytes += output.read_line(&mut line).expect("an answer line");
            answers.push(line.trim().parse::<f32>().expect("a number").to_bits());
        }
        (answers, bytes)
    });
    let ((by_frame, frame_bytes), frame_time) = exchange(&frame, &mut |output| {
        let answer = read_frame(output).expect("an answer frame");
        (answer.values.iter().map(|v| v.to_bits()).collect(), 32 + 4 * answer.values.len())
    });
    assert_eq!(by_frame, by_line, "a frame answers the bits text lines answer");
    println!(
        "predict pipe, {} rows of {STEPS}: text lines {} B down, {line_bytes} B up, {line_time:?}; one frame \
         {} B down, {frame_bytes} B up, {frame_time:?}",
        rows.len() / STEPS,
        text.len(),
        frame.len(),
    );
    drop(input);
    assert!(child.wait().expect("reaped").success());
}
