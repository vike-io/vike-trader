//! Sentence EMBEDDINGS: a BERT sentence encoder (all-MiniLM-L6-v2, [`crate::pins::EMBEDDING_MODEL`]).
//!
//! What its `modules.json` declares, in order: the encoder, MEAN pooling over the tokens (one text,
//! so every position is a real token and the mean is plain), then L2 normalization, so the dot
//! product of two embeddings is their cosine similarity. The norm is taken in `f64` (`sqrt` is
//! correctly rounded on every platform).

use crate::encoder::{Encoder, open};
use crate::error::NlpError;
use crate::pins::{ModelTask, PinnedModel};

/// A loaded sentence encoder.
pub struct EmbeddingModel {
    encoder: Encoder,
    dimension: usize,
}

impl EmbeddingModel {
    /// Load `model` (a [`ModelTask::Embedding`] pin) from its verified folder under `models_dir`.
    pub fn load(model: &PinnedModel, models_dir: &std::path::Path) -> Result<Self, NlpError> {
        let opened = open(model, models_dir, ModelTask::Embedding)?;
        Ok(Self { encoder: opened.encoder, dimension: opened.hidden_size })
    }

    /// [`EmbeddingModel::load`] from parts (the tests' tiny random model).
    #[cfg(test)]
    pub(crate) fn from_parts(
        config: &[u8],
        vocabulary: &[u8],
        weights: candle_nn::VarBuilder<'static>,
        max_tokens: usize,
    ) -> Result<Self, NlpError> {
        let opened = crate::encoder::open_parts(config, vocabulary, weights, max_tokens)?;
        Ok(Self { encoder: opened.encoder, dimension: opened.hidden_size })
    }

    /// How many numbers [`EmbeddingModel::embed`] returns (the encoder's hidden size).
    #[must_use]
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// The unit-length embedding of ONE text.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, NlpError> {
        let mean: Vec<f32> = self.encoder.hidden_states(text)?.mean(0)?.to_vec1()?;
        // The 1e-12 floor is sentence-transformers' `F.normalize` eps: a zero vector stays zero.
        let norm = mean.iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>().sqrt().max(1e-12);
        Ok(mean.iter().map(|&x| (f64::from(x) / norm) as f32).collect())
    }
}
