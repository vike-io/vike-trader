//! `Diagnostic` — ONE finding about ONE configured key, in the shape a validator reports when it
//! is allowed to say more than one thing.
//!
//! # Why the type exists rather than a `Vec<String>`
//!
//! A validator that returns its FIRST error costs the person who typed the file one
//! edit-and-rerun per mistake, and a config with eighty-odd keys in it genuinely carries several
//! at once when it is new. The first accumulating door onto that is
//! `crates/vike-backtest/src/harness/profile.rs`'s `BacktestProfile::validate_all`, and on that
//! crate's remote route every one of those reruns is a dial to a compute daemon which then
//! re-reads the profile it was handed — so the round trips are not free, and they are paid in
//! series.
//!
//! What a caller then does with the list is what fixes the shape. It has to ANCHOR each finding on
//! the key the operator actually wrote (so a CLI can sort by it and a UI can put it beside that
//! line), and it has to know whether the thing being configured will start at all. So a finding is
//! a key path, a sentence and a severity, and nothing else: everything else a renderer might want
//! is already somewhere it can look, and a field here would be a second copy of it.
//!
//! # Why it lives HERE and not in the crate that raises the first one
//!
//! vike-model is already in every consumer's graph — the crate that validates, the CLI that
//! prints, the daemon that answers over a wire — so the type costs no new package edge in any
//! direction. That is the cheap half of the argument. The load-bearing half is that a type born in
//! an ENGINE crate cannot later descend to a wire DTO without a second MOVE, and the
//! no-`pub use`-shims-on-a-move rule in root `CLAUDE.md`'s "Conventions that will bite you if
//! ignored" means a move rewrites every call site rather than leaving a compatibility spelling
//! behind. Born at the bottom, it never has to move at all.
//!
//! # What it deliberately is NOT
//!
//! - **Not an error type.** It implements neither `std::error::Error` nor `Display`, and nothing
//!   returns one as `Err`. A validator keeps its OWN error type for the first-error door its
//!   existing callers already read (`HarnessError` in the backtest harness); a `Diagnostic` is the
//!   other door's currency. Giving this type a `Display` would invite exactly the drift the two
//!   doors exist to avoid — a second rendering of a sentence whose only authority is the validator
//!   that wrote it.
//! - **Not a span.** No file, no line, no byte offset. The anchor is a KEY PATH because the thing
//!   validated may never have been a file: the same backtest profile reaches the same validator as
//!   TOML on an operator's disk and as a payload a remote runner was handed, and a line number
//!   would be a lie on one of those two routes.
//! - **Not `deny_unknown_fields`.** This is a REPORT a consumer reads, not a configuration a human
//!   types, so the two sides face opposite risks: refusing a field a newer producer added would
//!   turn an additive change into a hard failure at the door, where an unknown key in a config
//!   file is a typo that must never silently no-op.

use serde::{Deserialize, Serialize};

/// How much a finding costs the thing it describes — the only question a consumer must answer
/// before it can decide whether to proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The configuration is REFUSED: whatever it configures does not start.
    Error,
    /// The configuration is accepted and something in it is still worth saying — a key that
    /// parsed but will be read by nothing, a value whose effect is not what its name suggests.
    ///
    /// ⚠ **Nothing in this workspace emits one yet**, and that is stated here rather than left to
    /// be discovered: every diagnostic `BacktestProfile::validate_all` produces today is
    /// [`Severity::Error`], because a load-time refusal is all that validator has to say. The
    /// variant exists because the alternative is worse than an unused one — a consumer that cannot
    /// tell a refusal from a remark has to treat every remark as fatal, which is what makes a
    /// warning channel unusable on the day a producer has one to send.
    Warning,
}

/// One finding: the key path it is about, what is wrong, and how much it costs.
///
/// The `key` is where a reader should LOOK — the path an operator can find in their own file — and
/// it is deliberately not a claim that the message names only that key. A rule that compares two
/// keys (`engine.leverage` against `[risk]`) has one place a reader should start and names both in
/// its sentence; a rule delegated to a sub-validator is anchored on the TABLE that sub-validator
/// owns. The authoritative per-message key SET is derived from the message text against the parsed
/// schema by `crates/vike-backtest/src/profile_surface.rs`'s `refusals_json`, which is a different
/// job done in a different place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Dotted key path of the thing this is about, in the spelling the operator typed
    /// (`engine.cash`, `walkforward.n_splits`, `data`). Never a Rust field path.
    pub key: String,
    /// The finding itself, as one sentence, exactly as the validator that raised it wrote it.
    /// Never re-worded on the way through: for the backtest profile these sentences are also a
    /// PUBLISHED surface, harvested from the source that raises them.
    pub message: String,
    pub severity: Severity,
}

impl Diagnostic {
    /// A finding that REFUSES: the configuration does not start.
    pub fn error(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self { key: key.into(), message: message.into(), severity: Severity::Error }
    }

    /// A finding that does not refuse — see [`Severity::Warning`] for why the constructor exists
    /// before a producer does.
    pub fn warning(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self { key: key.into(), message: message.into(), severity: Severity::Warning }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WIRE SPELLING of a severity is the contract a consumer parses, so it is pinned here
    /// rather than left to `rename_all`'s reputation: a consumer reading `"error"` must keep
    /// working if this enum ever grows a variant or a `Debug` impl.
    #[test]
    fn severity_serialises_lowercase() {
        assert_eq!(serde_json::to_string(&Severity::Error).unwrap(), "\"error\"");
        assert_eq!(serde_json::to_string(&Severity::Warning).unwrap(), "\"warning\"");
    }

    #[test]
    fn a_diagnostic_roundtrips_its_key_message_and_severity() {
        let d = Diagnostic::error("engine.cash", "engine.cash must be > 0, got 0");
        let j = serde_json::to_string(&d).unwrap();
        let back: Diagnostic = serde_json::from_str(&j).unwrap();
        assert_eq!(d, back);
        assert_eq!(back.key, "engine.cash");
        assert_eq!(back.severity, Severity::Error);
        assert_eq!(Diagnostic::warning("engine.fee_rate", "x").severity, Severity::Warning);
    }
}
