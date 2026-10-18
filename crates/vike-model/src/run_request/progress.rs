//! **What `--progress` selected** — [`ProgressMode`] and its ONE roster ([`ProgressMode::NAMES`]).
//!
//! Pure: the three modes, their spellings and the case-insensitive parse. What a mode DOES —
//! the sink a mode implies (`StderrProgress::for_mode`), the terminal test behind `Auto`, the
//! events and the trade floor — is `crates/vike-backtest/src/harness/optimize/progress.rs`'s.

/// What `--progress` selected.
///
/// ⚠ `Auto` is not "on": it is "on IF a human is watching". A non-terminal stderr means a log file,
/// a CI transcript or a captured pipe, and appending one line per backtest to a captured log that
/// nobody will read is how a log directory reached 341 GB elsewhere in this workspace.
/// `StderrProgress::for_mode` (`crates/vike-backtest/src/harness/optimize/progress.rs`) performs
/// the decision once, at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgressMode {
    /// Human-readable, throttled, ONLY when stderr is a terminal. The default.
    #[default]
    Auto,
    /// Silent. Nothing is constructed and nothing is counted. ⚠ Spelled `none` on the command line
    /// and `Off` here, deliberately: a variant literally named `None` sits beside `Option::None` in
    /// every match in this file, and a reader cannot tell which one an arm means at a glance.
    Off,
    /// One newline-delimited JSON event per point on stderr, terminal or not, UNTHROTTLED — a
    /// machine consuming a stream wants every event, and dropping some to save a line is a
    /// disservice to the only consumer that can use them all.
    Json,
}

impl ProgressMode {
    /// The spellings, in the order a usage line should print them. The ONE roster — a refusal
    /// RENDERS it rather than restating it, so a fourth mode cannot be accepted by the parser and
    /// missing from the message.
    pub const NAMES: [&'static str; 3] = ["auto", "none", "json"];

    /// Case-insensitive parse, `None` for anything else — the same shape
    /// `RankMetric::from_str_ci` has (`crates/vike-backtest/src/harness/sweep.rs`), so the caller
    /// owns the refusal SENTENCE and this owns only the grammar.
    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(ProgressMode::Auto),
            "none" => Some(ProgressMode::Off),
            "json" => Some(ProgressMode::Json),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
