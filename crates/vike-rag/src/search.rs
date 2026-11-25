//! **Search over chunks**: a keyword ranking (BM25), a vector ranking (cosine, brute force) and the
//! hybrid of the two (reciprocal rank fusion), each POINT IN TIME.
//!
//! - **The corpus** ([`Corpus`]) is a set of [`Chunk`]s, each carrying its text's citation
//!   ([`TextSource`]), its position in [`crate::chunk::chunk_texts`], its words and, when the text was
//!   embedded, its vector ([`attach_vectors`]). The keyword index is built once, over every chunk.
//! - **Point in time.** Each ranking takes `as_of_ms` and ranks only the chunks RECEIVED at or before it
//!   ([`TextSource::received_ms`]). BM25's statistics are those of that set alone: the number of chunks,
//!   their mean length and each term's document frequency, so a text received later changes no earlier
//!   score.
//! - **BM25** (Robertson and Spärck Jones; Robertson and Zaragoza, *The Probabilistic Relevance
//!   Framework: BM25 and Beyond*, 2009) with [`BM25_K1`] and [`BM25_B`] and the non-negative inverse
//!   document frequency `ln(1 + (N − n + 0.5) / (n + 0.5))`. A term is a maximal run of alphanumeric
//!   characters, lower-cased; a repeated query term counts once. Only chunks sharing a term are ranked.
//! - **Cosine** over the stored embeddings, in `f64`. A chunk with no vector, a vector of another length
//!   (another model) or a zero vector is not ranked.
//! - **Reciprocal rank fusion** (Cormack, Clarke and Büttcher, SIGIR 2009): a chunk scores
//!   `Σ 1 / (k + rank)` over the rankings it appears in, rank from 1, `k` = [`RRF_K`].
//! - **Order.** Every ranking is by score, highest first, then by chunk position: the same inputs give
//!   the same ranking on every box.

use std::collections::{BTreeMap, HashMap};

/// BM25's term-frequency saturation, the textbook value.
pub const BM25_K1: f64 = 1.2;
/// BM25's length normalisation, the textbook value.
pub const BM25_B: f64 = 0.75;
/// Reciprocal rank fusion's constant, the value its authors found robust.
pub const RRF_K: f64 = 60.0;

/// What a chunk cites: the text it was cut from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSource {
    /// The text's stable id (`vike_data::TextRow::text_id`).
    pub text_id: String,
    /// Who published it (`vike_data::TextRow::source`: `CoinDesk`, `SEC`).
    pub source: String,
    /// When its source says it was published, epoch milliseconds UTC.
    pub published_ms: i64,
    /// When it was RECEIVED, epoch milliseconds UTC: the clock every ranking selects on.
    pub received_ms: i64,
    /// Where it can be read (empty when it has no address).
    pub url: String,
}

/// One chunk of one text.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// The text it was cut from.
    pub of: TextSource,
    /// Its position in [`crate::chunk::chunk_texts`] of that text: the stored vector's join key.
    pub index: u32,
    /// Its words (the title line included).
    pub text: String,
    /// Its embedding; empty when the text has none stored.
    pub vector: Vec<f64>,
}

/// The chunks of one text, in order, none embedded yet.
pub fn chunks_of(of: &TextSource, title: &str, summary: &str, body: &str) -> Vec<Chunk> {
    crate::chunk::chunk_texts(title, summary, body)
        .into_iter()
        .zip(0u32..)
        .map(|(text, index)| Chunk { of: of.clone(), index, text, vector: Vec::new() })
        .collect()
}

/// Give each chunk its stored vector, matched on `(text id, received time, chunk index)`; returns how
/// many chunks got one. A vector naming no chunk is ignored. Every vector should come from ONE model
/// revision: vectors of two models are not comparable, and the caller picks the revision.
pub fn attach_vectors(
    chunks: &mut [Chunk],
    vectors: impl IntoIterator<Item = (String, i64, u32, Vec<f64>)>,
) -> usize {
    let mut position: HashMap<(&str, i64, u32), usize> = HashMap::new();
    for (at, chunk) in chunks.iter().enumerate() {
        position.insert((chunk.of.text_id.as_str(), chunk.of.received_ms, chunk.index), at);
    }
    let mut found: Vec<(usize, Vec<f64>)> = Vec::new();
    for (text_id, received_ms, index, vector) in vectors {
        if let Some(&at) = position.get(&(text_id.as_str(), received_ms, index)) {
            found.push((at, vector));
        }
    }
    let mut attached = 0;
    for (at, vector) in found {
        if chunks[at].vector.is_empty() {
            attached += 1;
        }
        chunks[at].vector = vector;
    }
    attached
}

/// One ranked chunk: its position in the corpus and its score in that ranking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// The chunk's position in [`Corpus::chunks`].
    pub chunk: usize,
    /// Its score: BM25, cosine or fused, by the ranking that made it.
    pub score: f64,
}

/// The cosine similarity of two vectors; `None` when their lengths differ, they are empty, or either
/// is zero.
pub fn cosine(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        aa += x * x;
        bb += y * y;
    }
    (aa > 0.0 && bb > 0.0).then(|| dot / (aa.sqrt() * bb.sqrt()))
}

/// Fuse rankings by reciprocal rank fusion (the module doc), best first.
pub fn reciprocal_rank_fusion(rankings: &[&[Hit]]) -> Vec<Hit> {
    let mut fused: BTreeMap<usize, f64> = BTreeMap::new();
    for ranking in rankings {
        for (rank, hit) in (1u32..).zip(ranking.iter()) {
            *fused.entry(hit.chunk).or_insert(0.0) += 1.0 / (RRF_K + f64::from(rank));
        }
    }
    ordered(fused.into_iter().map(|(chunk, score)| Hit { chunk, score }).collect())
}

/// The terms of a text: maximal runs of alphanumeric characters, lower-cased, in order.
pub fn terms(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_lowercase)
}

/// A searchable set of chunks (the module doc).
#[derive(Debug, Clone)]
pub struct Corpus {
    chunks: Vec<Chunk>,
    /// Each chunk's number of terms.
    lengths: Vec<usize>,
    /// Term → `(chunk, how often it occurs there)`, chunks ascending.
    postings: HashMap<String, Vec<(usize, u32)>>,
}

impl Corpus {
    /// Index `chunks` (their keyword postings; vectors are read as they are).
    pub fn new(chunks: Vec<Chunk>) -> Self {
        let mut lengths = Vec::with_capacity(chunks.len());
        let mut postings: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
        for (at, chunk) in chunks.iter().enumerate() {
            let mut counts: BTreeMap<String, u32> = BTreeMap::new();
            let mut length = 0;
            for term in terms(&chunk.text) {
                *counts.entry(term).or_insert(0) += 1;
                length += 1;
            }
            lengths.push(length);
            for (term, count) in counts {
                postings.entry(term).or_default().push((at, count));
            }
        }
        Self { chunks, lengths, postings }
    }

    /// The chunks, in the order they were given.
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    fn known(&self, at: usize, as_of_ms: i64) -> bool {
        self.chunks[at].of.received_ms <= as_of_ms
    }

    /// The chunks received by `as_of_ms` that share a term with `query`, by BM25 (the module doc).
    pub fn keyword_ranking(&self, query: &str, as_of_ms: i64) -> Vec<Hit> {
        let known: Vec<usize> =
            (0..self.chunks.len()).filter(|&at| self.known(at, as_of_ms)).collect();
        let total_length: usize = known.iter().map(|&at| self.lengths[at]).sum();
        if known.is_empty() || total_length == 0 {
            return Vec::new();
        }
        let n = known.len() as f64;
        let mean_length = total_length as f64 / n;
        let mut unique: Vec<String> = Vec::new();
        for term in terms(query) {
            if !unique.contains(&term) {
                unique.push(term);
            }
        }
        let mut scores: BTreeMap<usize, f64> = BTreeMap::new();
        for term in &unique {
            let Some(list) = self.postings.get(term) else { continue };
            let hits: Vec<(usize, u32)> =
                list.iter().copied().filter(|&(at, _)| self.known(at, as_of_ms)).collect();
            if hits.is_empty() {
                continue;
            }
            let df = hits.len() as f64;
            let idf = libm::log(1.0 + (n - df + 0.5) / (df + 0.5));
            for (at, count) in hits {
                let tf = f64::from(count);
                let norm =
                    BM25_K1 * (1.0 - BM25_B + BM25_B * self.lengths[at] as f64 / mean_length);
                *scores.entry(at).or_insert(0.0) += idf * tf * (BM25_K1 + 1.0) / (tf + norm);
            }
        }
        ordered(scores.into_iter().map(|(chunk, score)| Hit { chunk, score }).collect())
    }

    /// The embedded chunks received by `as_of_ms`, by cosine similarity to `query` (the module doc).
    pub fn vector_ranking(&self, query: &[f64], as_of_ms: i64) -> Vec<Hit> {
        let hits = (0..self.chunks.len())
            .filter(|&at| self.known(at, as_of_ms))
            .filter_map(|at| {
                cosine(query, &self.chunks[at].vector).map(|score| Hit { chunk: at, score })
            })
            .collect();
        ordered(hits)
    }

    /// The keyword and the vector rankings fused by [`reciprocal_rank_fusion`]: hybrid retrieval.
    pub fn hybrid_ranking(
        &self,
        query_text: &str,
        query_vector: &[f64],
        as_of_ms: i64,
    ) -> Vec<Hit> {
        let keyword = self.keyword_ranking(query_text, as_of_ms);
        let vector = self.vector_ranking(query_vector, as_of_ms);
        reciprocal_rank_fusion(&[&keyword, &vector])
    }
}

/// Best first: score descending, then chunk position ascending.
fn ordered(mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk.cmp(&b.chunk)));
    hits
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
