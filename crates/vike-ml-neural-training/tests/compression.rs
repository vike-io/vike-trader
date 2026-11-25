//! W25: a trained network made cheaper to ask, and what that costs in answers.
//!
//! * The int8 graph (`compress::quantize_int8`) is REWRITTEN (its products are `MatMulInteger` /
//!   `ConvInteger`, its float weights gone), still runs in tract, and answers within 0.05 of the float
//!   graph's positions.
//! * A distilled student (`compress::distill`) answers its teacher's scores at under a third of the
//!   same student's untrained error.
//! * `measure_the_compressed_networks` (ignored: a MEASUREMENT, run on a quiet lane in release) trains
//!   the three sequence-capable networks on the Sharpe objective (W24), compresses each both ways,
//!   prints the numbers the book and the unit's report quote (bytes, latency in tract in THIS process,
//!   mean |position change|, out-of-sample Sharpe ratio) and writes every graph and `manifest.tsv`
//!   under cargo's per-target temporary directory, where `crates/vike-backend/tests/strategy_file_onnx.rs`'s
//!   `the_compressed_networks_answer_in_the_jail_and_how_fast` reads them to measure the same graphs in
//!   a jailed strategy file:
//!
//! ```text
//! cargo test --release -p vike-ml-neural-training --test integration -- --ignored --nocapture compression::
//! cargo test --release -p vike-backend --test strategy_file_onnx -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use prost::Message as _;
use tract_onnx::pb;
use vike_math::statistics::percentile;
use vike_ml_neural_training::compress::{distill, quantize_int8};
use vike_ml_neural_training::networks::{LstmNetworkConfig, MlpConfig, Network, TcnConfig};
use vike_ml_neural_training::onnx::export_onnx;
use vike_ml_neural_training::training::{Loss, TrainingSettings, predict, train};
use vike_ml_onnx_prediction::OnnxModel;

use crate::common::{momentum_returns, sharpe_of, uniform, windows_and_next};

/// Returns per window: the MLP reads them as 16 features, the LSTM as 16 steps of one, the TCN as
/// one channel of 16.
const STEPS: usize = 16;
/// Rows fitted on, then later rows the answers are judged on.
const FIT: usize = 800;
const LATER: usize = 400;
/// The objective every network here is trained on, and the head every graph answers through.
const SHARPE: Loss = Loss::Sharpe { turnover_cost: 0.0 };

fn rows() -> (Vec<f32>, Vec<f32>) {
    windows_and_next(&momentum_returns(31, FIT + LATER + STEPS), STEPS)
}

fn settings(seed: u64, epochs: usize) -> TrainingSettings {
    TrainingSettings { seed, epochs, batch_size: 64, learning_rate: 0.01 }
}

/// The op types of an ONNX graph's nodes, and its initializers' element types.
fn op_types(onnx: &[u8]) -> (Vec<String>, Vec<i32>) {
    let graph = pb::ModelProto::decode(onnx).expect("a model").graph.expect("a graph");
    (
        graph.node.iter().map(|node| node.op_type.clone()).collect(),
        graph.initializer.iter().map(|tensor| tensor.data_type).collect(),
    )
}

/// The first output of `onnx` loaded for `shape` on each of `rows` flat rows.
fn answers(onnx: &[u8], shape: &[usize], rows: &[f32]) -> Vec<f32> {
    let model = OnnxModel::from_bytes(onnx, shape).expect("tract loads the graph");
    let width: usize = shape.iter().product();
    rows.chunks(width).map(|row| model.predict(row).expect("tract runs the graph")[0]).collect()
}

/// `float` and `int8` (the same network) answer within 0.05 of each other on seeded rows.
fn close(float: &[u8], int8: &[u8], shape: &[usize]) {
    let rows = uniform(41, 50 * shape.iter().product::<usize>());
    let (a, b) = (answers(float, shape, &rows), answers(int8, shape, &rows));
    let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0_f32, f32::max);
    assert!(worst <= 0.05, "the int8 graph strays {worst} from the float one");
}

#[test]
fn an_int8_graph_is_rewritten_and_answers_close_to_its_float_graph() {
    let mlp = MlpConfig { inputs: STEPS, hidden: vec![32, 32], outputs: 1 }.init(1);
    let float = export_onnx(&mlp, [1, STEPS], SHARPE).expect("exports");
    let int8 = quantize_int8(&float).expect("quantises");
    let (ops, types) = op_types(&int8);
    assert_eq!(ops.iter().filter(|op| *op == "MatMulInteger").count(), 3, "{ops:?}");
    assert!(!ops.iter().any(|op| op == "MatMul"), "no float product is left: {ops:?}");
    assert!(types.contains(&3), "the weights are int8: {types:?}");
    // The weights shrink fourfold; a graph this small also carries the new nodes' fixed overhead
    // (MEASURED 2026-11-17: 4261 bytes against 7505).
    assert!(int8.len() < float.len(), "{} bytes against {}", int8.len(), float.len());
    close(&float, &int8, &[1, STEPS]);

    let lstm = LstmNetworkConfig { inputs: 1, hidden: 16, outputs: 1 }.init(1);
    let float = export_onnx(&lstm, [1, STEPS, 1], SHARPE).expect("exports");
    let int8 = quantize_int8(&float).expect("quantises");
    assert!(op_types(&int8).0.iter().any(|op| op == "MatMulInteger"));
    close(&float, &int8, &[1, STEPS, 1]);

    let tcn = TcnConfig { inputs: 1, channels: vec![8, 8], kernel_size: 3, outputs: 1 }.init(1);
    let float = export_onnx(&tcn, [1, 1, STEPS], SHARPE).expect("exports");
    let int8 = quantize_int8(&float).expect("quantises");
    let (ops, _) = op_types(&int8);
    assert!(ops.iter().any(|op| op == "ConvInteger"), "{ops:?}");
    assert!(!ops.iter().any(|op| op == "Conv"), "no float convolution is left: {ops:?}");
    close(&float, &int8, &[1, 1, STEPS]);
}

#[test]
fn quantising_refuses_bytes_that_are_no_model() {
    let error = quantize_int8(&[0xff, 0xff, 0xff]).expect_err("refused");
    assert!(error.0.contains("no ONNX model"), "{error}");
}

/// The student learns its teacher's scores: its squared error to them is under a third of the same
/// student's untrained one (MEASURED 2026-11-17 at 40 epochs: 0.211 -> 0.063).
#[test]
fn a_distilled_student_answers_close_to_its_teacher() {
    let (windows, next) = rows();
    let shape = [FIT, STEPS];
    let inputs = &windows[..FIT * STEPS];
    let teacher = MlpConfig { inputs: STEPS, hidden: vec![32, 32], outputs: 1 }.init(2);
    let teacher =
        train(teacher, inputs, shape, &next[..FIT], SHARPE, &settings(2, 20)).expect("trains");
    let student = || MlpConfig { inputs: STEPS, hidden: vec![8], outputs: 1 }.init(3);
    let distilled =
        distill(&teacher.network, student(), inputs, shape, &settings(3, 40)).expect("distils");
    let wanted = predict(&teacher.network, inputs, shape).expect("runs");
    let error = |scores: Vec<f32>| {
        scores.iter().zip(&wanted).map(|(a, b)| (a - b) * (a - b)).sum::<f32>() / FIT as f32
    };
    let (before, after) = (
        error(predict(&student(), inputs, shape).expect("runs")),
        error(predict(&distilled.network, inputs, shape).expect("runs")),
    );
    assert!(after < before / 3.0, "the student's error to its teacher went {before} -> {after}");
}

// ---- the measurement ----------------------------------------------------------------------------

/// Asks per latency figure, after as many warm-up asks.
const ASKS: usize = 2_000;

/// The median and the 99th percentile of one ask, in microseconds, in THIS process.
fn latency(model: &OnnxModel, row: &[f32]) -> (f64, f64) {
    for _ in 0..ASKS {
        std::hint::black_box(model.predict(row).ok());
    }
    let mut micros: Vec<f64> = (0..ASKS)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(model.predict(std::hint::black_box(row)).ok());
            started.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    micros.sort_by(f64::total_cmp);
    (percentile(&micros, 0.5), percentile(&micros, 0.99))
}

/// Where the graphs go for the jailed measurement (`crates/vike-backend/tests/strategy_file_onnx.rs`).
fn out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("compressed-networks")
}

/// Trains `teacher` on the Sharpe objective, distils `student` from it, quantises the teacher, and
/// prints and writes each graph's line. `shape(rows)` is the network's input shape.
fn measure<T: Network<D>, S: Network<D>, const D: usize>(
    name: &str,
    teacher: T,
    student: S,
    shape: impl Fn(usize) -> [usize; D],
    dir: &Path,
    manifest: &mut String,
) {
    let (windows, next) = rows();
    let (inputs, later) = (&windows[..FIT * STEPS], &windows[FIT * STEPS..]);
    let started = Instant::now();
    let teacher =
        train(teacher, inputs, shape(FIT), &next[..FIT], SHARPE, &settings(7, 30)).expect("trains");
    let student =
        distill(&teacher.network, student, inputs, shape(FIT), &settings(8, 60)).expect("distils");
    let trained_in = started.elapsed().as_secs_f64();
    let float = export_onnx(&teacher.network, shape(1), SHARPE).expect("exports");
    let graphs = [
        ("float", float.clone()),
        ("int8", quantize_int8(&float).expect("quantises")),
        ("student", export_onnx(&student.network, shape(1), SHARPE).expect("exports")),
    ];
    let one_row = shape(1).to_vec();
    let reference = answers(&float, &one_row, later);
    let reference_sharpe = sharpe_of(&reference, &next[FIT..]);
    for (variant, onnx) in graphs {
        let positions = answers(&onnx, &one_row, later);
        let moved = positions.iter().zip(&reference).map(|(a, b)| (a - b).abs()).sum::<f32>()
            / positions.len() as f32;
        let sharpe = sharpe_of(&positions, &next[FIT..]);
        let model = OnnxModel::from_bytes(&onnx, &one_row).expect("loads");
        let (p50, p99) = latency(&model, &later[..STEPS]);
        println!(
            "MEASURED\t{name}\t{variant}\t{} B\tp50 {p50:.2} us\tp99 {p99:.2} us\tmean |dpos| {moved:.4}\t\
             oos sharpe {sharpe:.4} (float {reference_sharpe:.4})\ttrained in {trained_in:.1} s",
            onnx.len()
        );
        let file = format!("{name}-{variant}");
        std::fs::write(dir.join(format!("{file}.onnx")), &onnx).expect("write the graph");
        let shape_words: Vec<String> = one_row.iter().map(usize::to_string).collect();
        manifest.push_str(&format!("{file}\t{}\n", shape_words.join(",")));
    }
}

#[test]
#[ignore = "a measurement: run in release on a quiet lane (module doc)"]
fn measure_the_compressed_networks() {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).expect("the output directory");
    println!("MEASURED cores={:?}", std::thread::available_parallelism());
    let mut manifest = String::new();
    measure(
        "mlp",
        MlpConfig { inputs: STEPS, hidden: vec![64, 64], outputs: 1 }.init(7),
        MlpConfig { inputs: STEPS, hidden: vec![16], outputs: 1 }.init(8),
        |rows| [rows, STEPS],
        &dir,
        &mut manifest,
    );
    measure(
        "lstm",
        LstmNetworkConfig { inputs: 1, hidden: 32, outputs: 1 }.init(7),
        LstmNetworkConfig { inputs: 1, hidden: 8, outputs: 1 }.init(8),
        |rows| [rows, STEPS, 1],
        &dir,
        &mut manifest,
    );
    measure(
        "tcn",
        TcnConfig { inputs: 1, channels: vec![32, 32, 32], kernel_size: 3, outputs: 1 }.init(7),
        TcnConfig { inputs: 1, channels: vec![8, 8, 8], kernel_size: 3, outputs: 1 }.init(8),
        |rows| [rows, 1, STEPS],
        &dir,
        &mut manifest,
    );
    std::fs::write(dir.join("manifest.tsv"), manifest).expect("write the manifest");
    println!("MEASURED graphs written to {}", dir.display());
}
