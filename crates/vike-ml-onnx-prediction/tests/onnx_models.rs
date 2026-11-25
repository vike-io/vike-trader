//! The committed ONNX fixtures, an MLP (`tests/fixtures/mlp.onnx`) and an LSTM
//! (`tests/fixtures/lstm.onnx`), answer what their WEIGHTS say.
//!
//! * **Where the bytes come from.** Each fixture is the encoding of a graph this file builds,
//!   [`mlp_graph`] and [`lstm_graph`], from the documented weight rule [`weights`]: no other tool
//!   made them. `capture_the_onnx_fixtures` (ignored; a reviewed step, run on a lane and the printed
//!   base64 committed) writes them, and `the_fixtures_are_the_encoding_of_the_graphs_built_here`
//!   holds the files and the builders together.
//! * **What the answers are checked against.** The reference forward passes read the weights back
//!   OUT of the committed bytes (prost) and compute in `f64` with the workspace's one logistic
//!   (`vike_math::special_functions::logistic`) and `libm`'s tanh, the ONNX
//!   operator definitions written out by hand (Gemm, Relu, LSTM with its i, o, f, c gate order), so
//!   they share no code with tract.
//!
//! The tolerance is 1e-5 absolute plus 1e-5 relative, D2's figure for the same comparison
//! (`vike-ml-neural-training`'s export test): tract computes in `f32` with its own kernels and its
//! own sigmoid and tanh approximations, so equality to the bit is not a property it promises (the
//! crate doc), and a wrong graph or a wrong gate order misses by orders of magnitude more.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use prost::Message as _;
use tract_onnx::pb;
use vike_math::special_functions::logistic;
use vike_ml_onnx_prediction::OnnxModel;
use vike_ml_prediction::{PredictionKind, Predictor};

/// ONNX's element type FLOAT (`TensorProto.DataType`).
const FLOAT: i32 = 1;
/// ONNX's attribute type INT (`AttributeProto.AttributeType`).
const INT: i32 = 2;

/// The MLP: 3 inputs, a hidden layer of 8 (ReLU), 2 outputs; the batch axis is symbolic.
const MLP: (usize, usize, usize) = (3, 8, 2);
/// The LSTM: 5 steps of 2 features, hidden size 3, batch 1; the answer is the last hidden state.
const LSTM: (usize, usize, usize) = (5, 2, 3);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).unwrap_or_else(|e| panic!("the committed fixture {name}: {e}"))
}

/// The weight rule: `count` values in [-1, 1] on a 0.001 grid, a different run per `seed`.
fn weights(seed: u64, count: usize) -> Vec<f32> {
    (0..count as u64)
        .map(|k| ((seed * 104_729 + k * 7_919) % 2_001) as f32 / 1000.0 - 1.0)
        .collect()
}

fn tensor(name: &str, dims: &[usize], data: Vec<f32>) -> pb::TensorProto {
    pb::TensorProto {
        name: name.to_string(),
        dims: dims.iter().map(|&d| d as i64).collect(),
        data_type: FLOAT,
        float_data: data,
        ..Default::default()
    }
}

/// A float value; `dims` is `None` for an output whose shape tract derives, and a `Err` dimension is
/// a symbolic one.
fn value(name: &str, dims: Option<&[Result<usize, &str>]>) -> pb::ValueInfoProto {
    use pb::tensor_shape_proto::{Dimension, dimension::Value};
    let shape = dims.map(|dims| pb::TensorShapeProto {
        dim: dims
            .iter()
            .map(|d| Dimension {
                value: Some(match d {
                    Ok(n) => Value::DimValue(*n as i64),
                    Err(symbol) => Value::DimParam(symbol.to_string()),
                }),
                ..Default::default()
            })
            .collect(),
    });
    pb::ValueInfoProto {
        name: name.to_string(),
        r#type: Some(pb::TypeProto {
            value: Some(pb::type_proto::Value::TensorType(pb::type_proto::Tensor {
                elem_type: FLOAT,
                shape,
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn node(op: &str, inputs: &[&str], outputs: &[&str], ints: &[(&str, i64)]) -> pb::NodeProto {
    pb::NodeProto {
        op_type: op.to_string(),
        input: inputs.iter().map(|s| s.to_string()).collect(),
        output: outputs.iter().map(|s| s.to_string()).collect(),
        attribute: ints
            .iter()
            .map(|(name, i)| pb::AttributeProto {
                name: name.to_string(),
                r#type: INT,
                i: *i,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn model(graph: pb::GraphProto) -> pb::ModelProto {
    pb::ModelProto {
        ir_version: 8,
        opset_import: vec![pb::OperatorSetIdProto { domain: String::new(), version: 17 }],
        producer_name: "vike-ml-onnx-prediction tests/onnx_models.rs".to_string(),
        graph: Some(graph),
        ..Default::default()
    }
}

/// `y = W2 · relu(W1 · x + b1) + b2`, two `Gemm`s with `transB = 1` (PyTorch's `Linear` layout).
fn mlp_graph() -> pb::ModelProto {
    let (inputs, hidden, outputs) = MLP;
    model(pb::GraphProto {
        name: "mlp".to_string(),
        node: vec![
            node("Gemm", &["x", "w1", "b1"], &["z1"], &[("transB", 1)]),
            node("Relu", &["z1"], &["a1"], &[]),
            node("Gemm", &["a1", "w2", "b2"], &["y"], &[("transB", 1)]),
        ],
        initializer: vec![
            tensor("w1", &[hidden, inputs], weights(1, hidden * inputs)),
            tensor("b1", &[hidden], weights(2, hidden)),
            tensor("w2", &[outputs, hidden], weights(3, outputs * hidden)),
            tensor("b2", &[outputs], weights(4, outputs)),
        ],
        input: vec![value("x", Some(&[Err("batch"), Ok(inputs)]))],
        output: vec![value("y", None)],
        ..Default::default()
    })
}

/// One forward `LSTM` (default activations), its last hidden state `Y_h` the only graph output.
fn lstm_graph() -> pb::ModelProto {
    let (steps, features, hidden) = LSTM;
    model(pb::GraphProto {
        name: "lstm".to_string(),
        node: vec![node(
            "LSTM",
            &["x", "w", "r", "b"],
            &["", "h"],
            &[("hidden_size", hidden as i64)],
        )],
        initializer: vec![
            tensor("w", &[1, 4 * hidden, features], weights(5, 4 * hidden * features)),
            tensor("r", &[1, 4 * hidden, hidden], weights(6, 4 * hidden * hidden)),
            tensor("b", &[1, 8 * hidden], weights(7, 8 * hidden)),
        ],
        input: vec![value("x", Some(&[Ok(steps), Ok(1), Ok(features)]))],
        output: vec![value("h", None)],
        ..Default::default()
    })
}

/// The initializer `name` of a committed fixture, as `f64`.
fn weights_in(bytes: &[u8], name: &str) -> Vec<f64> {
    let model = pb::ModelProto::decode(bytes).expect("the fixture is a ModelProto");
    let graph = model.graph.expect("the fixture has a graph");
    let tensor = graph.initializer.iter().find(|t| t.name == name).expect("the initializer");
    tensor.float_data.iter().map(|&w| f64::from(w)).collect()
}

/// The MLP's outputs for one row, from the fixture's weights.
fn mlp_reference(bytes: &[u8], x: &[f32]) -> Vec<f64> {
    let (inputs, hidden, outputs) = MLP;
    let (w1, b1) = (weights_in(bytes, "w1"), weights_in(bytes, "b1"));
    let (w2, b2) = (weights_in(bytes, "w2"), weights_in(bytes, "b2"));
    let a1: Vec<f64> = (0..hidden)
        .map(|j| {
            let z = (0..inputs).map(|k| w1[j * inputs + k] * f64::from(x[k])).sum::<f64>() + b1[j];
            z.max(0.0)
        })
        .collect();
    (0..outputs)
        .map(|o| (0..hidden).map(|j| w2[o * hidden + j] * a1[j]).sum::<f64>() + b2[o])
        .collect()
}

/// The LSTM's last hidden state for one sequence (`steps × features`, step-major), from the fixture's
/// weights: ONNX's equations, gates in W, R and B in the order i, o, f, c, both bias halves added.
fn lstm_reference(bytes: &[u8], x: &[f32]) -> Vec<f64> {
    let (steps, features, hidden) = LSTM;
    let (w, r, b) = (weights_in(bytes, "w"), weights_in(bytes, "r"), weights_in(bytes, "b"));
    let (mut h, mut c) = (vec![0.0; hidden], vec![0.0; hidden]);
    for t in 0..steps {
        let pre: Vec<f64> = (0..4 * hidden)
            .map(|row| {
                let input: f64 = (0..features)
                    .map(|k| w[row * features + k] * f64::from(x[t * features + k]))
                    .sum();
                let recurrent: f64 = (0..hidden).map(|j| r[row * hidden + j] * h[j]).sum();
                input + recurrent + b[row] + b[4 * hidden + row]
            })
            .collect();
        h = (0..hidden)
            .map(|u| {
                let input_gate = logistic(pre[u]);
                let output_gate = logistic(pre[hidden + u]);
                let forget_gate = logistic(pre[2 * hidden + u]);
                let cell = libm::tanh(pre[3 * hidden + u]);
                c[u] = forget_gate * c[u] + input_gate * cell;
                output_gate * libm::tanh(c[u])
            })
            .collect();
    }
    h
}

fn assert_close(what: &str, theirs: &[f32], ours: &[f64]) {
    assert_eq!(theirs.len(), ours.len(), "{what}: tract and the reference answer different counts");
    for (index, (&a, &b)) in theirs.iter().zip(ours).enumerate() {
        let a = f64::from(a);
        assert!(
            (a - b).abs() <= 1e-5 + 1e-5 * b.abs(),
            "{what}, output {index}: tract {a}, reference {b}"
        );
    }
}

/// Four MLP rows.
const MLP_ROWS: [[f32; 3]; 4] =
    [[0.5, -1.25, 2.0], [0.0, 0.0, 0.0], [-3.0, 0.75, 1.5], [1.0, 1.0, -1.0]];

#[test]
#[ignore = "a reviewed step: writes the committed fixtures and prints them as base64"]
fn capture_the_onnx_fixtures() {
    for (name, graph) in [("mlp.onnx", mlp_graph()), ("lstm.onnx", lstm_graph())] {
        let bytes = graph.encode_to_vec();
        std::fs::create_dir_all(fixture("")).expect("the fixture folder");
        std::fs::write(fixture(name), &bytes).expect("write the fixture");
        println!("FIXTURE {name} {}", STANDARD.encode(&bytes));
    }
}

#[test]
fn the_fixtures_are_the_encoding_of_the_graphs_built_here() {
    assert_eq!(read_fixture("mlp.onnx"), mlp_graph().encode_to_vec(), "mlp.onnx: recapture it");
    assert_eq!(read_fixture("lstm.onnx"), lstm_graph().encode_to_vec(), "lstm.onnx: recapture it");
}

#[test]
fn the_mlp_answers_what_its_weights_say_row_by_row_and_as_one_batch() {
    let bytes = read_fixture("mlp.onnx");
    let one = OnnxModel::from_bytes(&bytes, &[1, 3]).expect("the MLP loads for one row");
    for row in MLP_ROWS {
        assert_close("one row", &one.predict(&row).expect("one row"), &mlp_reference(&bytes, &row));
    }
    // The symbolic batch axis takes the caller's 4.
    let batch = OnnxModel::from_bytes(&bytes, &[4, 3]).expect("the MLP loads for four rows");
    let flat: Vec<f32> = MLP_ROWS.iter().flatten().copied().collect();
    let reference: Vec<f64> = MLP_ROWS.iter().flat_map(|row| mlp_reference(&bytes, row)).collect();
    assert_close("the batch", &batch.predict(&flat).expect("four rows"), &reference);
}

#[test]
fn the_lstm_answers_what_its_weights_say() {
    let bytes = read_fixture("lstm.onnx");
    let (steps, features, _) = LSTM;
    let lstm = OnnxModel::from_bytes(&bytes, &[steps, 1, features]).expect("the LSTM loads");
    for seed in 11..14 {
        let sequence: Vec<f32> = weights(seed, steps * features).iter().map(|w| 2.0 * w).collect();
        let answer = lstm.predict(&sequence).expect("the LSTM runs");
        assert_close("the last hidden state", &answer, &lstm_reference(&bytes, &sequence));
    }
}

/// What a strategy file is handed (the base64 of a run's `model.onnx`) loads the same network: the
/// same bits on this machine.
#[test]
fn the_model_text_a_strategy_file_is_handed_is_the_same_network() {
    let bytes = read_fixture("mlp.onnx");
    let text = format!("{}\n", STANDARD.encode(&bytes));
    let from_text = OnnxModel::from_model_text(&text, &[1, 3]).expect("the text loads");
    let from_bytes = OnnxModel::from_bytes(&bytes, &[1, 3]).expect("the bytes load");
    for row in MLP_ROWS {
        let (a, b) = (from_text.predict(&row).unwrap(), from_bytes.predict(&row).unwrap());
        assert_eq!(
            a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
    assert_eq!(from_text.input_shape(), &[1, 3]);
}

#[test]
fn a_shape_the_model_contradicts_a_wrong_input_and_bytes_that_are_no_model_are_refused() {
    let mlp = read_fixture("mlp.onnx");
    let refusal =
        OnnxModel::from_bytes(&mlp, &[1, 4]).expect_err("4 features against a declared 3");
    assert!(refusal.contains("contradicts"), "{refusal}");
    let lstm = read_fixture("lstm.onnx");
    let refusal =
        OnnxModel::from_bytes(&lstm, &[4, 1, 2]).expect_err("4 steps against a declared 5");
    assert!(refusal.contains("contradicts"), "{refusal}");

    let one = OnnxModel::from_bytes(&mlp, &[1, 3]).expect("the MLP loads");
    let refusal = one.predict(&[1.0, 2.0]).expect_err("two values for a [1, 3] input");
    assert!(refusal.contains("does not fit shape [1, 3]"), "{refusal}");

    let refusal = OnnxModel::from_bytes(b"not an onnx model", &[1, 3]).expect_err("no model");
    assert!(refusal.contains("does not load"), "{refusal}");
    let refusal =
        OnnxModel::from_model_text("tree\nversion=v4\n", &[1, 3]).expect_err("a GBDT text");
    assert!(refusal.contains("not the base64 of an ONNX model"), "{refusal}");
}

#[test]
fn a_model_is_send_and_sync_so_a_strategy_may_hold_it() {
    fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<OnnxModel>();
}

/// The one prediction trait (B1's `Predictor`): the caller declares what the first output means, the
/// count is checked once, and the answer is `predict`'s, widened to `f64`. An output that is none of
/// the kinds (two raw scores as one value, the LSTM's three hidden numbers) is refused at the
/// declaration, never cut down to fit.
#[test]
fn a_network_answers_the_one_prediction_trait_as_its_caller_declares_it() {
    let bytes = read_fixture("mlp.onnx");
    let model = OnnxModel::from_bytes(&bytes, &[1, 3]).expect("the MLP loads");
    let refusal = model.clone().predictor(PredictionKind::Value).expect_err("two numbers");
    assert!(refusal.contains("holds 2 numbers"), "{refusal}");
    let two = PredictionKind::ClassProbabilities { class_count: 2 };
    let predictor: Box<dyn Predictor> =
        Box::new(model.clone().predictor(two).expect("two numbers, as declared"));
    assert_eq!(predictor.kind(), two);
    for row in MLP_ROWS {
        let mut out = [0.0; 2];
        predictor.predict_into(&row.map(f64::from), &mut out).expect("one row");
        let raw = model.predict(&row).expect("one row");
        assert_eq!(out, [f64::from(raw[0]), f64::from(raw[1])]);
    }
    assert!(predictor.predict_one(&[0.0; 3]).is_err(), "two class probabilities are no one number");
    assert!(predictor.predict_into(&[0.0; 3], &mut [0.0; 1]).is_err(), "one slot for two numbers");
    assert!(predictor.predict_into(&[0.0; 2], &mut [0.0; 2]).is_err(), "a row of the wrong length");

    let (steps, features, _) = LSTM;
    let lstm = OnnxModel::from_bytes(&read_fixture("lstm.onnx"), &[steps, 1, features]).unwrap();
    assert!(
        lstm.predictor(PredictionKind::Probability).is_err(),
        "three numbers are no probability"
    );
}
