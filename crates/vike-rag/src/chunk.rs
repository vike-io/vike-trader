//! **The chunking rule**: how a stored text is cut into the pieces that are embedded, searched and cited.
//!
//! A text is its TITLE, then its SUMMARY, then its BODY (`vike_data::TextRow`'s three wording columns,
//! each possibly empty). The structure kept:
//!
//! - **The title is a heading.** A news headline, or an SEC section's title (`Item 1A. Risk Factors`:
//!   E3 stores one row per standard section, so the section split is already the row split). Every
//!   chunk of a cut text opens with it on its own line, so a chunk from the middle of a section still
//!   says which section it is.
//! - **A paragraph is the unit.** Lines separated by a blank line; a paragraph is never split unless it
//!   alone is longer than [`CHUNK_MAX_WORDS`], and then into runs of that many words.
//! - **A text that fits is ONE chunk, verbatim**: [`whole_text`] when its summary and body hold at most
//!   [`CHUNK_MAX_WORDS`] words, so a news item is its title, then its summary on the next line, exactly
//!   what `vike-nlp` scored before chunks existed.
//! - **Otherwise paragraphs are packed greedily, in order**, into chunks of at most [`CHUNK_MAX_WORDS`]
//!   words (the title not counted), joined by a blank line.
//!
//! ⚠ **The chunk's INDEX in [`chunk_texts`] is a stored join key.** `vike-nlp score` stores one vector
//! per chunk under it (`vike_data::TextScoreRow::chunk`) and a study re-cuts the same text with this
//! same function to find the chunk a vector belongs to. Changing this rule or the constant silently
//! pairs stored vectors with the wrong chunks: after any change, re-score every stored text. The pinned
//! cases in `chunk_tests.rs` are the tripwire.

/// The most words a chunk holds besides its title. all-MiniLM-L6-v2 reads 256 word pieces
/// (`vike_nlp::pins::EMBEDDING_MODEL`) and FinBERT 512; English prose runs about 1.3 pieces a word, so
/// 128 words and a headline stay inside the shorter window with room for numbers and tickers, which
/// split into more pieces. Changing it is a re-score (the module doc).
pub const CHUNK_MAX_WORDS: usize = 128;

/// The whole text as one string: title, summary and body, each non-empty one on its own line(s), in
/// that order. What a text that fits in one chunk is.
pub fn whole_text(title: &str, summary: &str, body: &str) -> String {
    let parts: Vec<&str> = [title, summary, body].into_iter().filter(|p| !p.is_empty()).collect();
    parts.join("\n")
}

/// The chunks of a text, in order (the module doc's rule); none for a text with no word at all.
pub fn chunk_texts(title: &str, summary: &str, body: &str) -> Vec<String> {
    let paragraphs: Vec<String> = paragraphs(summary).chain(paragraphs(body)).collect();
    let words: usize = paragraphs.iter().map(|p| p.split_whitespace().count()).sum();
    if words <= CHUNK_MAX_WORDS {
        let whole = whole_text(title, summary, body);
        return if whole.trim().is_empty() { Vec::new() } else { vec![whole] };
    }
    let mut chunks = Vec::new();
    let mut packed: Vec<String> = Vec::new();
    let mut packed_words = 0;
    for paragraph in paragraphs {
        let count = paragraph.split_whitespace().count();
        if packed_words + count > CHUNK_MAX_WORDS && !packed.is_empty() {
            chunks.push(headed(title, &packed.join("\n\n")));
            packed.clear();
            packed_words = 0;
        }
        if count > CHUNK_MAX_WORDS {
            let words: Vec<&str> = paragraph.split_whitespace().collect();
            chunks.extend(words.chunks(CHUNK_MAX_WORDS).map(|run| headed(title, &run.join(" "))));
            continue;
        }
        packed.push(paragraph);
        packed_words += count;
    }
    if !packed.is_empty() {
        chunks.push(headed(title, &packed.join("\n\n")));
    }
    chunks
}

/// The paragraphs of `text`: runs of non-blank lines, each line's trailing whitespace dropped.
fn paragraphs(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .split(|line| line.trim().is_empty())
        .filter(|run| !run.is_empty())
        .map(|run| run.join("\n"))
        .collect::<Vec<_>>()
        .into_iter()
}

/// `text` under its title's line (the text alone when there is no title).
fn headed(title: &str, text: &str) -> String {
    if title.is_empty() { text.to_string() } else { format!("{title}\n{text}") }
}

#[cfg(test)]
#[path = "chunk_tests.rs"]
mod tests;
