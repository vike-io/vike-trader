//! **Context assembly**: the chunks a ranking chose, as ONE text a reader or a language model reads,
//! each under a numbered citation of the text it came from.
//!
//! ```text
//! [1] CoinDesk, published 2026-11-12T10:00:00Z, news/CoinDesk/https://a.test/1 chunk 0
//! Bitcoin ETF decision delayed
//! The regulator postponed its ruling to January.
//!
//! [2] …
//! ```
//!
//! The citation names the source, the published time (UTC, to the second) and the text's id with
//! each control character (the id's field separator) shown as `/`, then the chunk's index; the chunk's
//! words follow, a blank line after each. The order is the hits' order; how many is the caller's.

use crate::search::{Corpus, Hit};

/// The chunks of `hits`, in their order, each under its citation (the module doc).
pub fn assemble_context(corpus: &Corpus, hits: &[Hit]) -> String {
    let mut out = String::new();
    for (number, hit) in (1..).zip(hits) {
        let chunk = &corpus.chunks()[hit.chunk];
        let id: String =
            chunk.of.text_id.chars().map(|c| if c.is_control() { '/' } else { c }).collect();
        out.push_str(&format!(
            "[{number}] {}, published {}, {id} chunk {}\n{}\n\n",
            chunk.of.source,
            vike_model::time::epoch_ms_to_utc_timestamp(chunk.of.published_ms),
            chunk.index,
            chunk.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Chunk, TextSource};

    #[test]
    fn each_chunk_comes_under_its_numbered_citation_in_the_hits_order() {
        let of = |id: &str, published_ms: i64| TextSource {
            text_id: format!("news\u{1f}CoinDesk\u{1f}{id}"),
            source: "CoinDesk".to_string(),
            published_ms,
            received_ms: published_ms + 5,
            url: id.to_string(),
        };
        let corpus = Corpus::new(vec![
            Chunk { of: of("https://a.test/1", 0), index: 0, text: "First".into(), vector: vec![] },
            Chunk {
                of: of("https://a.test/2", 1_791_763_200_000),
                index: 3,
                text: "Second\nline".into(),
                vector: vec![],
            },
        ]);
        let hits = [Hit { chunk: 1, score: 0.5 }, Hit { chunk: 0, score: 0.25 }];
        assert_eq!(
            assemble_context(&corpus, &hits),
            "[1] CoinDesk, published 2026-10-12T00:00:00Z, news/CoinDesk/https://a.test/2 chunk 3\n\
             Second\nline\n\n\
             [2] CoinDesk, published 1970-01-01T00:00:00Z, news/CoinDesk/https://a.test/1 chunk 0\n\
             First\n\n"
        );
        assert_eq!(assemble_context(&corpus, &[]), "");
    }
}
