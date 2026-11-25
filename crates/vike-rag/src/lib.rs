//! `vike-rag` — RETRIEVAL for retrieval-augmented generation (the book's W17): the R and the A.
//! The G, a language model writing an answer from the assembled context, is not here: it is the
//! reader's own model through MCP, which is deferred.
//!
//! # The contract
//!
//! - **Pure.** No network, no process, no file, no clock: a study runs this crate inside the jail
//!   (decision 0281) over what its `StudyContext` read, and every answer is a function of its inputs.
//! - **One chunking rule for both sides** ([`chunk`]). `vike-nlp score` embeds each stored text's
//!   [`chunk::chunk_texts`] host-side and stores one vector per chunk (`vike_data::TextScoreRow::chunk`);
//!   a study re-cuts the same text with the same call and joins each vector to its chunk by
//!   `(text id, received time, chunk index)` ([`search::attach_vectors`]). No chunk text is stored twice.
//! - **Point in time.** Every ranking takes the decision time and considers only the chunks RECEIVED at
//!   or before it, and BM25's document statistics (the count, the mean length, each term's document
//!   frequency) are those of that set alone, so a later text never shapes an earlier answer.
//! - **Brute force.** A ranking scores every eligible chunk: tens of thousands of chunks of 384 numbers
//!   are milliseconds, so no vector database and no approximate index (the plan's W17 row).
//! - **Deterministic.** A ranking is ordered by score, then by chunk position; every sum runs in a fixed
//!   order; BM25's logarithm is `libm`'s (decision 0032).
//!
//! The modules: [`chunk`] (the cut), [`search`] (the chunk, the keyword, vector and hybrid rankings,
//! reciprocal rank fusion) and [`context`] (the top chunks with their citations).

pub mod chunk;
pub mod context;
pub mod search;
