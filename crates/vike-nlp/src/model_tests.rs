//! The tokenizer and both heads over a TINY random BERT (8 wide, one layer, a 12-token vocabulary),
//! so the whole path runs on any box with no download. Random weights prove shapes, invariants and
//! determinism, never a prediction: `tests/pinned_models.rs` runs the real pinned weights.

use candle_core::{DType, Device};
use candle_nn::{VarBuilder, VarMap};

use crate::embedding::EmbeddingModel;
use crate::encoder::tokenizer;
use crate::sentiment::SentimentModel;

const VOCABULARY: &[u8] =
    b"[PAD]\n[UNK]\n[CLS]\n[SEP]\n[MASK]\nthe\nstocks\nrall\n##ied\nfell\na\nb\n";

fn config(id2label: &str) -> Vec<u8> {
    format!(
        r#"{{"vocab_size": 12, "hidden_size": 8, "num_hidden_layers": 1, "num_attention_heads": 2,
            "intermediate_size": 16, "hidden_act": "gelu", "hidden_dropout_prob": 0.1,
            "max_position_embeddings": 16, "type_vocab_size": 2, "initializer_range": 0.02,
            "layer_norm_eps": 1e-12, "pad_token_id": 0, "model_type": "bert", "id2label": {id2label}}}"#
    )
    .into_bytes()
}

const THREE_CLASSES: &str = r#"{"0": "positive", "1": "negative", "2": "neutral"}"#;

/// Fresh random weights, created on first use by the loaders.
fn random_weights() -> VarBuilder<'static> {
    VarBuilder::from_varmap(&VarMap::new(), DType::F32, &Device::Cpu)
}

const LONG_TEXT: &str = "the stocks rallied and the stocks fell and a b the stocks rallied again";

#[test]
fn the_tokenizer_is_bert_uncased_and_cuts_at_max_tokens() {
    let bert = tokenizer(VOCABULARY, 8).unwrap();
    let encoding = bert.encode("Stöcks RALLIED zzz", true).unwrap();
    // WordPiece is greedy longest-prefix: `rallied` is `rall` + `##ied`; `zzz` has no piece.
    assert_eq!(encoding.get_tokens(), ["[CLS]", "stocks", "rall", "##ied", "[UNK]", "[SEP]"]);
    let long = bert.encode("the the the the the the the the the", true).unwrap();
    assert_eq!(long.get_tokens().len(), 8, "special tokens count toward the cut");
    assert_eq!(long.get_tokens().last().map(String::as_str), Some("[SEP]"));
    assert!(tokenizer(b"[CLS]\n[SEP]\n", 8).is_err(), "a vocabulary without [UNK] is refused");
}

#[test]
fn sentiment_is_three_probabilities_and_the_same_text_scores_the_same() {
    let model =
        SentimentModel::from_parts(&config(THREE_CLASSES), VOCABULARY, random_weights(), 16)
            .unwrap();
    for text in ["the stocks rallied", "", LONG_TEXT] {
        let a = model.score(text).unwrap();
        let sum = a.positive + a.negative + a.neutral;
        assert!((sum - 1.0).abs() < 1e-12, "{text:?}: {a:?} sums to {sum}");
        for p in [a.positive, a.negative, a.neutral] {
            assert!((0.0..=1.0).contains(&p), "{text:?}: {a:?}");
        }
        assert_eq!(a.score(), a.positive - a.negative);
        let b = model.score(text).unwrap();
        assert_eq!(a, b, "{text:?}: one text, one answer");
    }
}

#[test]
fn a_classifier_must_name_the_three_sentiment_classes() {
    let other = r#"{"0": "up", "1": "down", "2": "flat"}"#;
    let refused = SentimentModel::from_parts(&config(other), VOCABULARY, random_weights(), 16);
    assert!(refused.is_err());
    let two = r#"{"0": "positive", "1": "negative"}"#;
    assert!(SentimentModel::from_parts(&config(two), VOCABULARY, random_weights(), 16).is_err());
    let twice = r#"{"0": "positive", "1": "positive", "2": "neutral"}"#;
    assert!(SentimentModel::from_parts(&config(twice), VOCABULARY, random_weights(), 16).is_err());
}

#[test]
fn an_embedding_is_unit_length_and_the_same_text_embeds_the_same() {
    let model =
        EmbeddingModel::from_parts(&config(THREE_CLASSES), VOCABULARY, random_weights(), 16)
            .unwrap();
    assert_eq!(model.dimension(), 8);
    for text in ["the stocks rallied", LONG_TEXT] {
        let a = model.embed(text).unwrap();
        assert_eq!(a.len(), 8);
        let norm = a.iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "{text:?}: norm {norm}");
        assert_eq!(a, model.embed(text).unwrap(), "{text:?}: one text, one answer");
    }
}

#[test]
fn more_tokens_than_the_model_has_positions_is_refused() {
    let refused =
        EmbeddingModel::from_parts(&config(THREE_CLASSES), VOCABULARY, random_weights(), 17);
    assert!(refused.is_err(), "17 tokens over 16 positions");
}
