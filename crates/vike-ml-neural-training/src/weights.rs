//! A trained network's WEIGHTS as a file, so the next walk-forward fold can start from them
//! (`warm_start` in the trainer's spec, `crate::trainer`'s module doc; decision 0292, amended
//! 2026-11-18): every float parameter in the network's own visiting order, its shape and its `f32`
//! values, little-endian. [`weights_of`] writes them; [`with_weights`] puts them into a freshly built
//! network, refusing by name a file whose parameter count or any parameter's shape differs from that
//! network's (a fold that changed the network's shape starts from nothing it could misread), and a
//! non-finite number ([`finite_weights`]): a diverged fold's weights carry down no chain.
//!
//! | bytes | what |
//! |---|---|
//! | 8 | `vikewgts` |
//! | 4 | the version, a `u32` (1) |
//! | 8 | the parameter count, a `u64` |
//! | each parameter | its rank (`u32`), its dimensions (`u64` each), its values (`f32` each) |

use burn::module::{Module, ModuleMapper, ModuleVisitor, Param};
use burn::tensor::{Tensor, TensorData};

use crate::InputError;

const MAGIC: [u8; 8] = *b"vikewgts";
const VERSION: u32 = 1;

/// The weights file of `network` (module doc).
pub fn weights_of<M: Module>(network: &M) -> Vec<u8> {
    /// Appends each float parameter to the body and counts them.
    struct Writer(Vec<u8>, u64);
    impl ModuleVisitor for Writer {
        fn visit_float<const D: usize>(&mut self, parameter: &Param<Tensor<D>>) {
            let tensor = parameter.val();
            self.0.extend((D as u32).to_le_bytes());
            for dimension in tensor.dims() {
                self.0.extend((dimension as u64).to_le_bytes());
            }
            let data = tensor.into_data_as::<f32>();
            let values = data.as_slice::<f32>().expect("a parameter read as f32 is f32");
            for value in values {
                self.0.extend(value.to_le_bytes());
            }
            self.1 += 1;
        }
    }
    let mut body = Writer(Vec::new(), 0);
    network.visit(&mut body);
    let mut file = Vec::with_capacity(20 + body.0.len());
    file.extend(MAGIC);
    file.extend(VERSION.to_le_bytes());
    file.extend(body.1.to_le_bytes());
    file.extend(body.0);
    file
}

/// `network` with every float parameter replaced by the one at the same place in `file` (module
/// doc), each keeping its identity and its gradient setting.
pub fn with_weights<M: Module>(network: M, file: &[u8]) -> Result<M, InputError> {
    /// Hands out each stored parameter in order; the first disagreement is the refusal.
    struct Loader {
        stored: std::vec::IntoIter<Stored>,
        index: usize,
        refusal: Option<String>,
    }
    impl ModuleMapper for Loader {
        fn map_float<const D: usize>(&mut self, parameter: Param<Tensor<D>>) -> Param<Tensor<D>> {
            self.index += 1;
            let shape = parameter.val().dims();
            match self.stored.next() {
                Some(Stored { dimensions, values }) if dimensions == shape => {
                    parameter.map(|old| {
                        let wanted = old.is_require_grad();
                        Tensor::from_data(TensorData::new(values, shape), &old.device())
                            .set_require_grad(wanted)
                    })
                }
                stored => {
                    let found =
                        stored.map_or("none".to_string(), |s| format!("{:?}", s.dimensions));
                    self.refusal.get_or_insert(format!(
                        "parameter {} is {shape:?} in this network and {found} in the weights file",
                        self.index
                    ));
                    parameter
                }
            }
        }
    }
    let mut loader = Loader { stored: parse(file)?.into_iter(), index: 0, refusal: None };
    let network = network.map(&mut loader);
    let left = loader.stored.len();
    match loader.refusal {
        Some(refusal) => Err(InputError(format!("the previous fold's network differs: {refusal}"))),
        None if left > 0 => Err(InputError(format!(
            "the previous fold's network differs: its weights file holds {left} more parameter(s) \
             than this network's {}",
            loader.index
        ))),
        None => Ok(network),
    }
}

/// Refuses a weights file that does not read or that holds a non-finite number: a diverged network
/// is no fold's warm start.
pub fn finite_weights(file: &[u8]) -> Result<(), InputError> {
    parse(file).map(|_| ())
}

/// One parameter as a weights file holds it.
struct Stored {
    dimensions: Vec<usize>,
    values: Vec<f32>,
}

/// The parameters of a weights file, in order.
fn parse(file: &[u8]) -> Result<Vec<Stored>, InputError> {
    let refused = |why: &str| InputError(format!("the weights file {why}"));
    let mut at = 0usize;
    let mut take = |count: usize| {
        let bytes = file.get(at..at.checked_add(count)?)?;
        at += count;
        Some(bytes)
    };
    let short = || refused("ends early");
    if take(8).ok_or_else(short)? != MAGIC {
        return Err(refused("is not a network's weights: it does not start `vikewgts`"));
    }
    let word = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let count = |b: &[u8]| {
        usize::try_from(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            .unwrap_or(usize::MAX)
    };
    let version = word(take(4).ok_or_else(short)?);
    if version != VERSION {
        return Err(refused(&format!("is version {version}; this trainer reads {VERSION}")));
    }
    let parameters = count(take(8).ok_or_else(short)?);
    let mut stored = Vec::new();
    for _ in 0..parameters {
        let rank = word(take(4).ok_or_else(short)?) as usize;
        let mut shape = Vec::with_capacity(rank.min(8));
        for _ in 0..rank {
            shape.push(count(take(8).ok_or_else(short)?));
        }
        let numbers = shape.iter().try_fold(1usize, |n, &d| n.checked_mul(d)).ok_or_else(short)?;
        let bytes = take(numbers.checked_mul(4).ok_or_else(short)?).ok_or_else(short)?;
        let values: Vec<f32> =
            bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        if values.iter().any(|v| !v.is_finite()) {
            let place = stored.len() + 1;
            return Err(refused(&format!("holds a non-finite value in parameter {place}")));
        }
        stored.push(Stored { dimensions: shape, values });
    }
    if take(1).is_some() {
        return Err(refused("holds bytes after its last parameter"));
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networks::{LstmNetworkConfig, MlpConfig};
    use crate::training::{Loss, TrainingSettings, predict, train};

    const LSTM: LstmNetworkConfig = LstmNetworkConfig { inputs: 3, hidden: 4, outputs: 3 };
    const SHAPE: [usize; 3] = [24, 5, 3];

    fn rows() -> (Vec<f32>, Vec<f32>) {
        let x = (0..24 * 5 * 3).map(|i| ((i * 37 % 101) as f32 / 50.0) - 1.0).collect();
        (x, (0..24).map(|i| (i % 3) as f32).collect())
    }

    #[test]
    fn a_network_given_anothers_weights_is_that_network_bit_for_bit() {
        let (x, y) = rows();
        let settings = TrainingSettings { seed: 1, epochs: 2, batch_size: 8, learning_rate: 0.01 };
        let trained =
            train(LSTM.init(1), &x, SHAPE, &y, Loss::CrossEntropy { classes: 3 }, &settings)
                .expect("trains")
                .network;
        let file = weights_of(&trained);
        assert_ne!(weights_of(&LSTM.init(99)), file, "another seed's network is another network");
        let loaded = with_weights(LSTM.init(99), &file).expect("the same shape");
        assert_eq!(weights_of(&loaded), file, "every parameter, bit for bit");
        let bits = |scores: Vec<f32>| scores.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        assert_eq!(
            bits(predict(&loaded, &x, SHAPE).expect("answers")),
            bits(predict(&trained, &x, SHAPE).expect("answers"))
        );
    }

    #[test]
    fn weights_of_another_shape_or_a_malformed_file_are_refused() {
        fn refusal<M>(network: Result<M, InputError>) -> String {
            network.err().expect("refused").0
        }
        let file = weights_of(&LSTM.init(1));
        let wider = LstmNetworkConfig { hidden: 5, ..LSTM }.init(1);
        assert!(refusal(with_weights(wider, &file)).contains("differs: parameter 1 is"));
        let more_inputs = LstmNetworkConfig { inputs: 4, ..LSTM }.init(1);
        assert!(refusal(with_weights(more_inputs, &file)).contains("differs"));
        let mlp = MlpConfig { inputs: 3, hidden: vec![], outputs: 3 }.init(1);
        assert!(refusal(with_weights(mlp, &file)).contains("differs"));
        assert!(
            refusal(with_weights(LSTM.init(1), &file[..file.len() - 1])).contains("ends early")
        );
        assert!(refusal(with_weights(LSTM.init(1), b"a weights file")).contains("does not start"));
        let mut long = file.clone();
        long.push(0);
        assert!(refusal(with_weights(LSTM.init(1), &long)).contains("after its last"));
    }

    #[test]
    fn a_weights_file_holding_a_non_finite_number_is_refused() {
        let mut file = MAGIC.to_vec();
        file.extend(VERSION.to_le_bytes());
        file.extend(1u64.to_le_bytes());
        file.extend(1u32.to_le_bytes());
        file.extend(2u64.to_le_bytes());
        file.extend(1.0f32.to_le_bytes());
        let finite = {
            let mut whole = file.clone();
            whole.extend(2.0f32.to_le_bytes());
            whole
        };
        assert!(finite_weights(&finite).is_ok());
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut diverged = file.clone();
            diverged.extend(bad.to_le_bytes());
            let error = finite_weights(&diverged).expect_err("refused").0;
            assert!(error.contains("non-finite value in parameter 1"), "{error}");
        }
        let mut nan = file.clone();
        nan.extend(f32::NAN.to_le_bytes());
        let mlp = MlpConfig { inputs: 1, hidden: vec![], outputs: 2 }.init(1);
        assert!(with_weights(mlp, &nan).expect_err("refused").0.contains("non-finite"));
    }
}
