//! **Which parameter-search METHODS own each method-specific knob** — the one pure table
//! ([`METHOD_KNOBS`]) every surface that selects a search reads.
//!
//! The rule it states ("may this flag be written at all under this method") is consulted by the
//! engine's `crates/vike-backtest/src/search/select.rs` (`resolve`, over the wire's `Option`
//! presence), by `crates/vike-compute/src/backtest_cli/search_flags.rs`'s `parse_search_flags` (over
//! argv presence, before any value is parsed) and by the CLI's published surface
//! (`crates/vike-cli/src/surface.rs`'s `method_knobs` roster row). It is a table of FLAG NAMES and
//! METHOD NAMES only: what a method does with its knob, the refusal sentence
//! (`refuse_unowned_knob`) and the construction arms stay in `vike-backtest`.

/// Which METHODS own each method-specific knob — moved from `backtest_cli`'s `METHOD_FLAGS`,
/// including the argument for why the second element is a SET.
///
/// ⚠ **The second element is a SET, and `--seed` is why.** A seed is genuinely the same knob under
/// tpe and genetic — same parse, same meaning, same `u64` reaching a searcher's constructor. Two
/// rows would scatter a flag's owners with nothing binding them, and would refuse
/// `--optimizer genetic --seed 7` BY THE TPE ROW; dropping the row would let grid and euler discard
/// the flag in silence. An EMPTY slice refuses the flag under every method, which is the safe
/// direction for a typo.
pub const METHOD_KNOBS: &[(&str, &[&str])] = &[
    ("--euler-depth", &["euler"]),
    ("--trials", &["tpe"]),
    // The reproducibility seed is a knob BOTH stochastic searchers take, and the only asymmetry is
    // what their ABSENCE means — tpe defaults to 0, genetic refuses. That belongs in the
    // construction arms, where a method's own semantics live, not here: this table answers "may
    // this flag be written at all", and the answer is yes for both.
    ("--seed", &["tpe", "genetic"]),
];

#[cfg(test)]
#[path = "method_knobs_tests.rs"]
mod tests;
