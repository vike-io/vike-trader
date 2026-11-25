//! The BERT encoder both pinned models are: the uncased WordPiece tokenizer built from `vocab.txt`,
//! and `candle-transformers`' `BertModel` over the safetensors weights, on the CPU.
//!
//! The tokenizer is spelled here rather than read from a `tokenizer.json`, because only one of the two
//! repositories ships one and both were trained with the same BERT uncased pipeline: the BERT
//! normalizer (clean the text, split CJK characters, lowercase, strip accents), the BERT
//! pre-tokenizer (whitespace and punctuation), WordPiece with `##` continuations and `[UNK]`, then
//! `[CLS] … [SEP]`, cut at the end to the pin's `max_tokens`. That is what `tokenizer_config.json`
//! says of both (`do_lower_case`, `strip_accents: null`, `tokenize_chinese_chars`).

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::models::wordpiece::WordPiece;
use tokenizers::normalizers::BertNormalizer;
use tokenizers::pre_tokenizers::bert::BertPreTokenizer;
use tokenizers::processors::bert::BertProcessing;
use tokenizers::{Tokenizer, TruncationParams};

use crate::error::{NlpError, model_error};
use crate::fetch::read_verified;
use crate::pins::{CONFIG_FILE, ModelTask, PinnedModel, VOCABULARY_FILE, WEIGHTS_FILE};

/// The tokenizer and the encoder of one model.
pub(crate) struct Encoder {
    tokenizer: Tokenizer,
    model: BertModel,
}

/// What a head needs besides the encoder: the weights (for its own layers) and `config.json`.
pub(crate) struct Opened {
    pub(crate) encoder: Encoder,
    pub(crate) weights: VarBuilder<'static>,
    pub(crate) config: serde_json::Value,
    pub(crate) hidden_size: usize,
}

/// Open `model` from its VERIFIED folder under `models_dir`, refusing a pin of another task.
pub(crate) fn open(
    model: &PinnedModel,
    models_dir: &std::path::Path,
    task: ModelTask,
) -> Result<Opened, NlpError> {
    if model.task != task {
        return Err(NlpError::Model(format!(
            "{} is a {:?} model, not {task:?}",
            model.name, model.task
        )));
    }
    let config = read_verified(model, models_dir, CONFIG_FILE)?;
    let vocabulary = read_verified(model, models_dir, VOCABULARY_FILE)?;
    let weights = read_verified(model, models_dir, WEIGHTS_FILE)?;
    let weights = VarBuilder::from_buffered_safetensors(weights, DType::F32, &Device::Cpu)?;
    open_parts(&config, &vocabulary, weights, model.max_tokens)
}

/// [`open`] from bytes and a weight source: the seam the tests build a tiny random model through.
pub(crate) fn open_parts(
    config: &[u8],
    vocabulary: &[u8],
    weights: VarBuilder<'static>,
    max_tokens: usize,
) -> Result<Opened, NlpError> {
    let config_json: serde_json::Value =
        serde_json::from_slice(config).map_err(|e| model_error("read config.json", e))?;
    let bert: Config = serde_json::from_value(config_json.clone())
        .map_err(|e| model_error("read config.json", e))?;
    if max_tokens > bert.max_position_embeddings {
        return Err(NlpError::Model(format!(
            "max_tokens {max_tokens} exceeds the model's {} positions",
            bert.max_position_embeddings
        )));
    }
    let tokenizer = tokenizer(vocabulary, max_tokens)?;
    let model = BertModel::load(weights.clone(), &bert)?;
    Ok(Opened {
        encoder: Encoder { tokenizer, model },
        weights,
        config: config_json,
        hidden_size: bert.hidden_size,
    })
}

/// The BERT uncased tokenizer over `vocabulary` (module doc).
pub(crate) fn tokenizer(vocabulary: &[u8], max_tokens: usize) -> Result<Tokenizer, NlpError> {
    let vocab = WordPiece::read_bytes(vocabulary).map_err(|e| model_error("read vocab.txt", e))?;
    let id = |token: &str| {
        vocab
            .get(token)
            .copied()
            .ok_or_else(|| NlpError::Model(format!("vocab.txt has no {token}")))
    };
    let (cls, sep) = (id("[CLS]")?, id("[SEP]")?);
    id("[UNK]")?;
    let wordpiece = WordPiece::builder()
        .vocab(vocab)
        .unk_token("[UNK]".to_string())
        .build()
        .map_err(|e| model_error("build the tokenizer", e))?;
    let mut tokenizer = Tokenizer::new(wordpiece);
    tokenizer
        .with_normalizer(Some(BertNormalizer::default()))
        .map_err(|e| model_error("build the tokenizer", e))?;
    tokenizer.with_pre_tokenizer(Some(BertPreTokenizer));
    tokenizer.with_post_processor(Some(BertProcessing::new(
        ("[SEP]".to_string(), sep),
        ("[CLS]".to_string(), cls),
    )));
    tokenizer
        .with_truncation(Some(TruncationParams { max_length: max_tokens, ..Default::default() }))
        .map_err(|e| model_error("build the tokenizer", e))?;
    Ok(tokenizer)
}

impl Encoder {
    /// The last hidden states of ONE text, `(tokens, hidden)`, `[CLS]` first. One text, so no
    /// padding and no attention mask: every position is a real token (crate doc).
    pub(crate) fn hidden_states(&self, text: &str) -> Result<Tensor, NlpError> {
        let encoding = self.tokenizer.encode(text, true).map_err(|e| model_error("tokenize", e))?;
        let ids = Tensor::new(encoding.get_ids(), &Device::Cpu)?.unsqueeze(0)?;
        let type_ids = Tensor::new(encoding.get_type_ids(), &Device::Cpu)?.unsqueeze(0)?;
        Ok(self.model.forward(&ids, &type_ids, None)?.squeeze(0)?)
    }
}
