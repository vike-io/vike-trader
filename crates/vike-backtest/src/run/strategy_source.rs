//! A strategy file's text as a door read it, and what a run takes from it: its sha256 and its
//! header's backtest words. `ProfileStrategy::read_file` reads the file; a door holding the text itself
//! (a Studio editor buffer) builds one with [`StrategySource::of`].

use vike_strategy::file::scan::StrategyFileLanguage;
use vike_strategy::file::{StrategyFileFill, StrategyFileHeader, StrategyFileRunsOn};

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
    /// The header's `[strategy] fill = "close"`: a market order placed on a bar fills at that bar's
    /// close (`vike_simulator::InBarRules::fill_at_close`). `false` for `next_open` and for a text
    /// with no header.
    pub fill_at_close: bool,
    /// Whether the header says `runs_on = "backtest"`: the one file a run serves its inputs' whole
    /// series (`vike_simulator::StrategyEngineConfig::input_whole`, decision 0271's amendment of
    /// 2026-11-14), since it never mounts on paper or live. `false` for a text with no header.
    pub backtest_only: bool,
    /// The numbered SDK the file's artifact was built against (decision 0296), set by the door that
    /// loaded the artifact (the sandboxed child, from the artifact's path): what a result records as
    /// `strategy_sdk`. `None` for a `.rhai` file, a text no artifact was loaded for, and a build
    /// against a development tree.
    pub sdk: Option<u32>,
}

impl StrategySource {
    /// The source of a file's `text`, written in `language`, and its parsed header, or the header's own
    /// reason it is listed broken (`StrategyFileHeader::parse_file`: a `.rhai` file may not pin an SDK):
    /// what a door reading a profile's file holds it to.
    pub fn read(
        text: String,
        language: StrategyFileLanguage,
    ) -> Result<(Self, StrategyFileHeader), String> {
        let header = StrategyFileHeader::parse_file(&text, language)?;
        Ok((Self::hashed(text, Some(&header)), header))
    }

    /// The source of a text a door holds rather than reads (a Studio editor buffer): hashed, and
    /// its header read where it has one (a text without one declares nothing).
    pub fn of(text: String) -> Self {
        let header = StrategyFileHeader::parse(&text).ok();
        Self::hashed(text, header.as_ref())
    }

    fn hashed(text: String, header: Option<&StrategyFileHeader>) -> Self {
        let sha256 = vike_hub_protocol::hash::sha256_hex(text.as_bytes());
        let optimistic_quotes = header.is_some_and(|h| h.strategy.optimistic_quotes);
        let fill_at_close = header.is_some_and(|h| h.strategy.fill == StrategyFileFill::Close);
        let backtest_only =
            header.is_some_and(|h| h.strategy.runs_on == StrategyFileRunsOn::Backtest);
        StrategySource { text, sha256, optimistic_quotes, fill_at_close, backtest_only, sdk: None }
    }
}
