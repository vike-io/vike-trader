//! A strategy file's text as a door read it, and what a run takes from it: its sha256 and its
//! header's backtest word. `ProfileStrategy::read_file` reads the file; a door holding the text itself
//! (a Studio editor buffer) builds one with [`StrategySource::of`].

use vike_strategy::file::StrategyFileHeader;

/// A strategy file as read: its text, the sha256 of that text (the hash a result records,
/// `strategy_sha256`) and what its header declares for the backtest.
#[derive(Debug, Clone, PartialEq)]
pub struct StrategySource {
    pub text: String,
    /// The lowercase hex sha256 of [`Self::text`]'s bytes (`vike_hub_protocol::hash::sha256_hex`, the
    /// hash the folder scan lists the file under).
    pub sha256: String,
    /// The header's `[strategy] optimistic_quotes`: the law a tagged plain limit fills by when the
    /// profile states no `[engine.quotes]`. `false` for a text with no header.
    pub optimistic_quotes: bool,
}

impl StrategySource {
    /// The source of a file's `text` and its parsed header, or the header's own reason it is
    /// listed broken (`StrategyFileHeader::parse`): what a door reading a profile's file holds it to.
    pub fn read(text: String) -> Result<(Self, StrategyFileHeader), String> {
        let header = StrategyFileHeader::parse(&text)?;
        Ok((Self::hashed(text, header.strategy.optimistic_quotes), header))
    }

    /// The source of a text a door holds rather than reads (a Studio editor buffer): hashed, and
    /// its header read where it has one (a text without one declares nothing).
    pub fn of(text: String) -> Self {
        let optimistic_quotes =
            StrategyFileHeader::parse(&text).is_ok_and(|h| h.strategy.optimistic_quotes);
        Self::hashed(text, optimistic_quotes)
    }

    fn hashed(text: String, optimistic_quotes: bool) -> Self {
        let sha256 = vike_hub_protocol::hash::sha256_hex(text.as_bytes());
        StrategySource { text, sha256, optimistic_quotes }
    }
}
