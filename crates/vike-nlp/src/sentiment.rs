//! Financial SENTIMENT: a BERT sequence classifier (FinBERT, [`crate::pins::SENTIMENT_MODEL`]).
//!
//! The head is Hugging Face's `BertForSequenceClassification`: the pooler (a dense layer and `tanh`
//! over the `[CLS]` hidden state, `bert.pooler.dense`), then a linear layer to one logit per class
//! (`classifier`). The classes and their ORDER come from the model's own `config.json` (`id2label`),
//! never from this file: the three words must be `positive`, `negative` and `neutral`, each once,
//! and a model that names other classes is refused at load. The probabilities are the softmax of the
//! logits in `f64` (`vike_math::special_functions::softmax`, `libm::exp`).

use candle_core::Module as _;
use candle_nn::{Linear, linear};

use crate::encoder::{Encoder, open};
use crate::error::NlpError;
use crate::pins::{ModelTask, PinnedModel};

/// The three class probabilities of one text; they sum to one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sentiment {
    pub positive: f64,
    pub negative: f64,
    pub neutral: f64,
}

impl Sentiment {
    /// `positive - negative`, in `[-1, 1]`: the one number a study usually wants.
    #[must_use]
    pub fn score(&self) -> f64 {
        self.positive - self.negative
    }
}

/// A loaded sentiment classifier.
pub struct SentimentModel {
    encoder: Encoder,
    pooler: Linear,
    classifier: Linear,
    /// Where `positive`, `negative` and `neutral` sit among the logits, in that order.
    class_index: [usize; 3],
}

impl SentimentModel {
    /// Load `model` (a [`ModelTask::Sentiment`] pin) from its verified folder under `models_dir`.
    pub fn load(model: &PinnedModel, models_dir: &std::path::Path) -> Result<Self, NlpError> {
        Self::from_opened(open(model, models_dir, ModelTask::Sentiment)?)
    }

    /// [`SentimentModel::load`] from parts (the tests' tiny random model).
    #[cfg(test)]
    pub(crate) fn from_parts(
        config: &[u8],
        vocabulary: &[u8],
        weights: candle_nn::VarBuilder<'static>,
        max_tokens: usize,
    ) -> Result<Self, NlpError> {
        Self::from_opened(crate::encoder::open_parts(config, vocabulary, weights, max_tokens)?)
    }

    fn from_opened(opened: crate::encoder::Opened) -> Result<Self, NlpError> {
        let labels = labels(&opened.config)?;
        if labels.len() != 3 {
            return Err(NlpError::Model(format!("expected three classes, id2label is {labels:?}")));
        }
        let index_of = |word: &str| {
            let mut found = labels.iter().enumerate().filter(|(_, l)| l.as_str() == word);
            match (found.next(), found.next()) {
                (Some((i, _)), None) => Ok(i),
                _ => Err(NlpError::Model(format!(
                    "id2label must name `{word}` exactly once; it is {labels:?}"
                ))),
            }
        };
        let class_index = [index_of("positive")?, index_of("negative")?, index_of("neutral")?];
        let hidden = opened.hidden_size;
        let bert = if opened.weights.contains_tensor("bert.pooler.dense.weight") {
            opened.weights.pp("bert")
        } else {
            opened.weights.clone()
        };
        let pooler = linear(hidden, hidden, bert.pp("pooler").pp("dense"))?;
        let classifier = linear(hidden, labels.len(), opened.weights.pp("classifier"))?;
        Ok(Self { encoder: opened.encoder, pooler, classifier, class_index })
    }

    /// The class probabilities of ONE text.
    pub fn score(&self, text: &str) -> Result<Sentiment, NlpError> {
        let hidden = self.encoder.hidden_states(text)?;
        let cls = hidden.narrow(0, 0, 1)?; // (1, hidden): the [CLS] position
        let pooled = self.pooler.forward(&cls)?.tanh()?;
        let logits: Vec<f32> = self.classifier.forward(&pooled)?.squeeze(0)?.to_vec1()?;
        let mut probabilities: Vec<f64> = logits.into_iter().map(f64::from).collect();
        vike_math::special_functions::softmax(&mut probabilities);
        let [p, n, u] = self.class_index;
        Ok(Sentiment {
            positive: probabilities[p],
            negative: probabilities[n],
            neutral: probabilities[u],
        })
    }
}

/// `id2label` as a list ordered by class id; refused unless the ids are exactly `0..len`.
fn labels(config: &serde_json::Value) -> Result<Vec<String>, NlpError> {
    let map = config
        .get("id2label")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| NlpError::Model("config.json has no id2label".to_string()))?;
    let mut labels = vec![String::new(); map.len()];
    for (id, label) in map {
        let slot = id.parse::<usize>().ok().and_then(|i| labels.get_mut(i));
        match (slot, label.as_str()) {
            (Some(slot), Some(label)) if slot.is_empty() => *slot = label.to_string(),
            _ => return Err(NlpError::Model(format!("id2label has a bad entry {id:?}: {label}"))),
        }
    }
    Ok(labels)
}
