//! `vike-nlp` — the TEXT models of the book's W8b: a FinBERT-class classifier for financial
//! SENTIMENT and a MiniLM-class encoder for sentence EMBEDDINGS, both BERT encoders run on the CPU
//! with `candle` and tokenized with `tokenizers`.
//!
//! # The contract
//!
//! - **Weights are fetched, never shipped.** [`pins::PINNED_MODELS`] names each model by its
//!   repository, a pinned COMMIT and the sha256 and size of every file the model needs;
//!   [`fetch::fetch`] downloads exactly those files into
//!   `<project>/models/<name>/<revision>/` (`vike_paths::state_path::PROJECT_MODELS_DIR`) and refuses a
//!   file whose bytes differ from the pin. Nothing here is in git or a release asset.
//!   ⚠ **Licences: each user fetches the weights for their own use; this project never ships or
//!   redistributes them.** FinBERT (`ProsusAI/finbert`) is licensed **cc-by-nc-sa-4.0** (attribution,
//!   NON-COMMERCIAL, share-alike); all-MiniLM-L6-v2 is apache-2.0. Each pin carries its licence
//!   ([`pins::PinnedModel::license`]) and `vike-nlp fetch` names it before it downloads.
//! - **A model loads only from a VERIFIED folder.** [`sentiment::SentimentModel::load`] and
//!   [`embedding::EmbeddingModel::load`] check every pinned file's size and sha256 first
//!   ([`fetch::read_verified`]), so a half-written or swapped file is a refusal, never a quietly
//!   different model.
//! - **One text per forward pass.** Batching pads texts to a common length, and the matrix kernels
//!   then sum in a shape-dependent order, so a text's last bits would depend on its batch mates.
//!   Scoring one at a time makes a text's numbers a function of the text and the pinned weights only.
//!   The scorer's "text" is one CHUNK of a stored text (`vike_rag::chunk::chunk_texts`, the book's W17),
//!   so a long filing section is embedded whole, piece by piece, never cut off at the model's window.
//! - **The model runs HOST-SIDE.** A study runs jailed with no file and no network (decision 0281), so
//!   it never loads a model: a host-side command scores the stored texts and a study reads the
//!   NUMBERS. That command is this crate's binary, shipped as its own release tool: it reads the
//!   texts over the datahub wire and writes day files ([`score_texts`]) that the datahub, which links
//!   no model, imports as `kind=text_score` (`vike-cli data history import text-scores <MODEL>`). The
//!   tensor engine's own kernels call the platform's `exp`/`tanh`/`erf`, so the same text can
//!   differ in the last bit between glibc and MSVC; a score is therefore computed once,
//!   where the store is, and read back as data. The one transcendental this crate asks for itself,
//!   the class softmax, is `vike_math::special_functions::softmax` (`libm`, decision 0032).
//!
//! No `env::var` read, no logging (the caller logs per batch), no `unsafe` (the weights are read into
//! memory, never mapped). The binary (`src/main.rs`) is the command line over these modules: its boot
//! (`vike_boot::boot`) finds the project and its settings, the library takes every path and time as a
//! parameter.

pub mod embedding;
mod encoder;
pub mod error;
pub mod fetch;
pub mod pins;
pub mod score_texts;
pub mod sentiment;

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;
