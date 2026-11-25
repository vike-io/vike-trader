//! Making a trained network cheaper to ask inside a trading loop (W25): a SMALLER network trained to
//! answer what the big one answers ([`distill`]), and an exported graph rewritten to compute its
//! matrix products in 8-bit integers ([`quantize_int8`]).
//!
//! Neither is free: each answers a little differently from the network it stands in for.
//! `tests/compression.rs` states that difference (the positions and the out-of-sample Sharpe ratio)
//! and the per-prediction latency in `tract`, and `crates/vike-backend/tests/strategy_file_onnx.rs`'s
//! `the_compressed_networks_answer_in_the_jail_and_how_fast` measures the same graphs inside a jailed
//! strategy file. Which variant earns its keep is a MEASUREMENT, not a property of this module.
//!
//! ⚠ MEASURED 2026-11-17, two runs (the latency box, release, tract 0.23.10, the book's small networks: an MLP
//! 16-64-64-1, an LSTM of 32 over 16 steps, a TCN of 3 x 32 channels): the int8 graph answered within
//! 0.005 of the float graph's positions and was 5 to 14 times SLOWER per prediction (the run-time
//! quantisation of every activation costs more than these products save), so it is a size tool at
//! best, never a speed one at these sizes. A distilled student was 1.1 to 2.8 times faster, its
//! out-of-sample Sharpe ratio at most 2% below its teacher's (two of the three were above), while
//! holding positions 0.16 to 0.24 away from its teacher's on average: a cheaper strategy, not a
//! faithful copy.
//!
//! # [`quantize_int8`]: dynamic quantisation, the form `tract` runs
//!
//! Every `MatMul` and `Conv` whose weight is a constant of the graph (an initializer) is rewritten:
//!
//! * the weight becomes `int8`, symmetric, with ONE scale per output (a `MatMul` column, a `Conv`
//!   output channel): `scale = max |w| / 127` and `q = round_ties_even(w / scale)` (an output whose
//!   weights are all zero keeps the scale 1);
//! * the activation is quantised at RUN time, per tensor, to `uint8` by `DynamicQuantizeLinear`;
//! * the product runs as `MatMulInteger` / `ConvInteger` into `int32`, is cast back to float and
//!   multiplied by `activation scale * weight scale`; a `Conv`'s bias is added afterwards, in float.
//!
//! Every other node, initializer, input and output is copied as it was, and a float weight no node
//! reads any more is dropped. The operators are opset 10 and 11, inside the exporter's opset 18. A
//! non-finite weight is refused: a network holding one is broken, and no scale represents it.

use std::collections::{HashMap, HashSet};

use prost::Message as _;
use tract_onnx::pb;

use crate::InputError;
use crate::networks::Network;
use crate::training::{Loss, Trained, TrainingSettings, predict, train};

/// ONNX's element type FLOAT (`TensorProto.DataType`).
const FLOAT: i32 = 1;
/// ONNX's element type INT8.
const INT8: i32 = 3;
/// ONNX's attribute type INT (`AttributeProto.AttributeType`).
const INT_ATTRIBUTE: i32 = 2;

/// Trains `student` to answer what `teacher` answers on `inputs` (row-major, shaped `input_shape`):
/// the teacher's RAW scores, by mean squared error (knowledge distillation on the scores). Export the
/// student with the TEACHER's loss, so its head (sigmoid, tanh or none) is the teacher's.
pub fn distill<T: Network<D>, S: Network<D>, const D: usize>(
    teacher: &T,
    student: S,
    inputs: &[f32],
    input_shape: [usize; D],
    settings: &TrainingSettings,
) -> Result<Trained<S>, InputError> {
    let scores = predict(teacher, inputs, input_shape)?;
    train(student, inputs, input_shape, &scores, Loss::MeanSquaredError, settings)
}

/// The ONNX model in `onnx` with its constant-weight `MatMul` and `Conv` nodes computed in 8-bit
/// integers (module doc).
pub fn quantize_int8(onnx: &[u8]) -> Result<Vec<u8>, InputError> {
    let mut model = pb::ModelProto::decode(onnx)
        .map_err(|e| InputError(format!("the bytes are no ONNX model: {e}")))?;
    let graph =
        model.graph.as_mut().ok_or_else(|| InputError("the ONNX model has no graph".into()))?;
    let weights: HashMap<String, pb::TensorProto> = graph
        .initializer
        .iter()
        .filter(|tensor| tensor.data_type == FLOAT)
        .map(|tensor| (tensor.name.clone(), tensor.clone()))
        .collect();
    let mut added: Vec<pb::TensorProto> = Vec::new();
    let mut quantised: HashSet<String> = HashSet::new();
    let mut nodes = Vec::with_capacity(graph.node.len());
    for node in std::mem::take(&mut graph.node) {
        let weight = node.input.get(1).and_then(|name| weights.get(name));
        match (node.op_type.as_str(), weight) {
            ("MatMul", Some(weight)) if weight.dims.len() == 2 && node.input.len() == 2 => {
                let columns = weight.dims[1] as usize;
                let twin = int8_weight(
                    weight,
                    columns,
                    |i| i % columns,
                    &[columns],
                    &mut quantised,
                    &mut added,
                )?;
                nodes.extend(integer_product(&node, "MatMulInteger", &twin, None));
            }
            ("Conv", Some(weight)) if weight.dims.len() >= 3 => {
                let channels = weight.dims[0] as usize;
                let per_channel = weight.dims[1..].iter().product::<i64>() as usize;
                // One value per output channel, broadcast over the batch and every spatial axis.
                let mut broadcast = vec![1; weight.dims.len() - 1];
                broadcast[0] = channels;
                let twin = int8_weight(
                    weight,
                    channels,
                    |i| i / per_channel,
                    &broadcast,
                    &mut quantised,
                    &mut added,
                )?;
                let bias = match node.input.get(2).filter(|bias| !bias.is_empty()) {
                    Some(bias) => {
                        let values = floats(weights.get(bias).ok_or_else(|| {
                            InputError(format!("the bias `{bias}` is no float constant"))
                        })?)?;
                        let column = format!("{bias}.column");
                        if quantised.insert(column.clone()) {
                            added.push(float_tensor(&column, &broadcast, values));
                        }
                        Some(column)
                    }
                    None => None,
                };
                nodes.extend(integer_product(&node, "ConvInteger", &twin, bias));
            }
            _ => nodes.push(node),
        }
    }
    graph.node = nodes;
    let read: HashSet<&str> =
        graph.node.iter().flat_map(|node| node.input.iter().map(String::as_str)).collect();
    graph.initializer.retain(|tensor| {
        read.contains(tensor.name.as_str()) || !weights.contains_key(&tensor.name)
    });
    graph.initializer.extend(added);
    Ok(model.encode_to_vec())
}

/// The float values of an initializer (raw little-endian bytes or the `float_data` list), refusing a
/// non-finite one.
fn floats(tensor: &pb::TensorProto) -> Result<Vec<f32>, InputError> {
    let values: Vec<f32> = if tensor.raw_data.is_empty() {
        tensor.float_data.clone()
    } else {
        tensor.raw_data.as_chunks::<4>().0.iter().map(|bytes| f32::from_le_bytes(*bytes)).collect()
    };
    if values.iter().any(|value| !value.is_finite()) {
        return Err(InputError(format!("the weight `{}` holds a non-finite value", tensor.name)));
    }
    Ok(values)
}

fn float_tensor(name: &str, dims: &[usize], values: Vec<f32>) -> pb::TensorProto {
    pb::TensorProto {
        name: name.to_string(),
        dims: dims.iter().map(|&dim| dim as i64).collect(),
        data_type: FLOAT,
        float_data: values,
        ..Default::default()
    }
}

/// Adds `weight`'s int8 twin and its per-output scales (shaped `scale_dims`) once (an LSTM reads one
/// weight at every step), and names both. `output_of(i)` is the output element `i` of the weight
/// feeds.
fn int8_weight(
    weight: &pb::TensorProto,
    outputs: usize,
    output_of: impl Fn(usize) -> usize,
    scale_dims: &[usize],
    quantised: &mut HashSet<String>,
    added: &mut Vec<pb::TensorProto>,
) -> Result<(String, String), InputError> {
    let name = format!("{}.int8", weight.name);
    let scale = format!("{}.scale", weight.name);
    if !quantised.insert(name.clone()) {
        return Ok((name, scale));
    }
    let values = floats(weight)?;
    let mut largest = vec![0.0_f32; outputs];
    for (index, value) in values.iter().enumerate() {
        let output = output_of(index);
        largest[output] = largest[output].max(value.abs());
    }
    let scales: Vec<f32> =
        largest.iter().map(|&max| if max > 0.0 { max / 127.0 } else { 1.0 }).collect();
    let raw: Vec<u8> = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let level = (value / scales[output_of(index)]).round_ties_even().clamp(-127.0, 127.0);
            // Two's complement: the byte an ONNX INT8 tensor stores.
            level as i8 as u8
        })
        .collect();
    added.push(pb::TensorProto {
        name: name.clone(),
        dims: weight.dims.clone(),
        data_type: INT8,
        raw_data: raw,
        ..Default::default()
    });
    added.push(float_tensor(&scale, scale_dims, scales));
    Ok((name, scale))
}

/// The nodes that replace `node` (a `MatMul` or a `Conv` over a constant weight): quantise the
/// activation, the integer product `op` against the int8 twin `weight.0` with `node`'s attributes,
/// back to float, times the activation's scale and the weight's (`weight.1`), plus `bias` (a float
/// constant) when there is one. Each new node is named by its first output, which is unique.
fn integer_product(
    node: &pb::NodeProto,
    op: &str,
    weight: &(String, String),
    bias: Option<String>,
) -> Vec<pb::NodeProto> {
    let output = node.output[0].clone();
    let part = |suffix: &str| format!("{output}.{suffix}");
    let step = |op_type: &str, input: Vec<String>, outputs: Vec<String>| pb::NodeProto {
        name: outputs[0].clone(),
        op_type: op_type.to_string(),
        input,
        output: outputs,
        ..Default::default()
    };
    let scaled = if bias.is_some() { part("unbiased") } else { output.clone() };
    let mut product =
        step(op, vec![part("x_q"), weight.0.clone(), part("x_zero")], vec![part("int32")]);
    product.attribute = node.attribute.clone();
    let mut cast = step("Cast", vec![part("int32")], vec![part("float")]);
    cast.attribute = vec![pb::AttributeProto {
        name: "to".to_string(),
        r#type: INT_ATTRIBUTE,
        i: i64::from(FLOAT),
        ..Default::default()
    }];
    let mut nodes = vec![
        step(
            "DynamicQuantizeLinear",
            vec![node.input[0].clone()],
            vec![part("x_q"), part("x_scale"), part("x_zero")],
        ),
        product,
        cast,
        step("Mul", vec![part("x_scale"), weight.1.clone()], vec![part("scale")]),
        step("Mul", vec![part("float"), part("scale")], vec![scaled.clone()]),
    ];
    if let Some(bias) = bias {
        nodes.push(step("Add", vec![scaled, bias], vec![output]));
    }
    nodes
}
