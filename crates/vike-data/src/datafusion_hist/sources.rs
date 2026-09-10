//! The STORE's own source-precedence policy: which writer wins when two of them covered the same
//! rows, persisted next to the data so maintenance can resolve duplicates on its own.
//!
//! ## Why this exists
//!
//! Commit-key idempotency stops the SAME writer re-ingesting a window it already ingested. It does
//! nothing about two DIFFERENT writers covering one window — `live-…` from the recorder and
//! `databento:…` from a backfill are different keys, both accepted, and the store never dedups by
//! row value (deliberately: two genuine trades can share a price, size and millisecond). So a
//! `scan_trades` over that window returns BOTH copies, and a backtest sees double the volume with
//! nothing anywhere reporting a problem.
//!
//! [`SourceRankPolicy`] already resolves that — but it was a parameter passed at call time, so it
//! lived nowhere, no caller in the workspace constructed one outside tests, and the store could not
//! answer "do I have a rule for this?". Persisting it here makes the rule a property of the STORE
//! (which is where it belongs: it describes which writers wrote *here*), so
//! [`DataFusionHist::run_maintenance`] can apply it without every caller re-supplying it.
//!
//! ## Making a row-DROPPING pass safe to run on a timer
//!
//! Supersession deletes rows. Default compaction is byte-identical; this is not, which is exactly
//! why it was kept out of the automatic pass. Four properties make automation defensible:
//!
//! 1. **Absent file ⇒ no policy ⇒ byte-identical to today.** An existing store is untouched until
//!    someone deliberately writes one. There is no default ordering, because any default would be a
//!    guess about which of a customer's writers is authoritative.
//! 2. **An empty prefix list is a safe no-op** — [`SourceRankPolicy::rank_of`] ranks every part `0`,
//!    so nothing is ever superseded.
//! 3. **STRICT BY DEFAULT: a series containing a commit key that matches NO listed prefix is
//!    SKIPPED**, with the unknown key named. This is the load-bearing guard.
//!    [`SourceRankPolicy::rank_of`] gives an unmatched key `prefixes.len()` — the LOWEST precedence
//!    — so running anyway would drop rows from a writer the operator never ranked, silently and
//!    irreversibly, on a timer. Refusing to touch that series is the only safe reading of "I do not
//!    know what this source is". Set `strict: false` to accept the documented risk.
//! 4. **The drop is reported.** `CompactionReport::rows_superseded` already carries the count, so a
//!    pass that deleted rows can never look like one that did not.
//!
//! ## What it is NOT
//!
//! Not a dedup of one writer against itself (commit keys already do that), and not a correctness
//! claim about which source is *better* — only which one this operator has decided wins. The rule is
//! entirely theirs; the store just remembers it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hist::DataError;
use crate::hist_maint::SourceRankPolicy;

use super::io;

/// Store-root file holding the policy. Underscore-prefixed like `_manifest.json` so it never looks
/// like a `kind=` partition.
const SOURCES: &str = "_sources.json";

/// Bumped only on a breaking shape change; an unknown version is a hard error rather than a
/// best-effort parse, because guessing here would mean dropping rows under a misread rule.
const SOURCES_FORMAT: u32 = 1;

/// The persisted precedence rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreSourcePolicy {
    pub version: u32,
    /// Commit-key prefixes in DESCENDING precedence — index 0 wins. Same semantics as
    /// [`SourceRankPolicy::prefixes`], which this builds.
    pub prefixes: Vec<String>,
    /// Skip any series holding a commit key that matches no prefix, rather than superseding it away
    /// as lowest-precedence. `true` unless deliberately disabled — see the module doc's point 3.
    #[serde(default = "default_strict")]
    pub strict: bool,
}

fn default_strict() -> bool {
    true
}

impl StoreSourcePolicy {
    /// A policy from prefixes in DESCENDING precedence, strict.
    pub fn new<I, S>(prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            version: SOURCES_FORMAT,
            prefixes: prefixes.into_iter().map(Into::into).collect(),
            strict: true,
        }
    }

    /// Accept the documented risk of superseding rows from an unranked writer.
    pub fn permissive(mut self) -> Self {
        self.strict = false;
        self
    }

    /// The call-time policy this persists.
    pub fn rank_policy(&self) -> SourceRankPolicy {
        SourceRankPolicy::new(self.prefixes.clone())
    }

    /// `true` when this policy would supersede nothing anyway — an empty prefix list ranks every
    /// part `0`. Callers skip the whole superseding path rather than paying a rewrite for a no-op.
    pub fn is_inert(&self) -> bool {
        self.prefixes.is_empty()
    }

    /// Commit keys matching NO prefix — the ones this policy cannot rank.
    ///
    /// Under [`strict`](Self::strict) a non-empty result means the series is SKIPPED: those rows
    /// would otherwise be superseded away as lowest-precedence by a rule that never mentioned them.
    pub fn unranked<'a>(&self, keys: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        keys.into_iter()
            .filter(|k| !self.prefixes.iter().any(|p| crate::store_kind::key_matches_prefix(k, p)))
            .map(str::to_string)
            .collect()
    }
}

fn path(root: &Path) -> PathBuf {
    root.join(SOURCES)
}

/// Read the store's policy. `Ok(None)` when absent — the byte-identical default.
///
/// An unreadable or unknown-version file is an ERROR, never a silent `None`: falling back to "no
/// policy" would be safe, but falling back QUIETLY would hide that the operator's configured rule
/// is not being applied, and they would believe duplicates were being resolved when they were not.
pub fn load_policy(root: &Path) -> Result<Option<StoreSourcePolicy>, DataError> {
    let p = path(root);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let policy: StoreSourcePolicy = serde_json::from_slice(&bytes)
        .map_err(|e| DataError::Query(format!("{}: {e}", p.display())))?;
    if policy.version != SOURCES_FORMAT {
        return Err(DataError::Query(format!(
            "{}: unknown source-policy version {} (this build understands {SOURCES_FORMAT}) — \
             refusing to supersede rows under a rule it may be misreading",
            p.display(),
            policy.version
        )));
    }
    Ok(Some(policy))
}

/// Write the store's policy, replacing any existing one.
pub fn save_policy(root: &Path, policy: &StoreSourcePolicy) -> Result<(), DataError> {
    let bytes = serde_json::to_vec_pretty(policy).map_err(|e| DataError::Query(e.to_string()))?;
    std::fs::create_dir_all(root).map_err(io)?;
    std::fs::write(path(root), bytes).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// The default that keeps every existing store byte-identical: no file, no policy, no rewrite.
    #[test]
    fn an_absent_file_is_no_policy() {
        let d = tmp();
        assert_eq!(load_policy(d.path()).unwrap(), None);
    }

    #[test]
    fn a_policy_round_trips() {
        let d = tmp();
        let p = StoreSourcePolicy::new(["live-", "databento:"]);
        save_policy(d.path(), &p).unwrap();
        assert_eq!(load_policy(d.path()).unwrap(), Some(p));
    }

    /// Strict is the DEFAULT even for a hand-written file that omits the key — an operator who did
    /// not think about unranked sources gets the safe behaviour, not the permissive one.
    #[test]
    fn strict_defaults_to_true_when_the_key_is_absent() {
        let d = tmp();
        std::fs::write(d.path().join(SOURCES), br#"{"version":1,"prefixes":["live-"]}"#).unwrap();
        assert!(load_policy(d.path()).unwrap().unwrap().strict);
    }

    /// An unknown version ERRORS rather than degrading to "no policy". Silently ignoring the
    /// operator's rule would leave them believing duplicates were being resolved.
    #[test]
    fn an_unknown_version_is_an_error_not_a_silent_none() {
        let d = tmp();
        std::fs::write(d.path().join(SOURCES), br#"{"version":99,"prefixes":[]}"#).unwrap();
        let err = load_policy(d.path()).unwrap_err().to_string();
        assert!(err.contains("unknown source-policy version"), "{err}");
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_silent_none() {
        let d = tmp();
        std::fs::write(d.path().join(SOURCES), b"{ not json").unwrap();
        assert!(load_policy(d.path()).is_err());
    }

    /// An empty prefix list ranks every part 0, so it can supersede nothing — callers skip the
    /// rewrite entirely rather than paying for a guaranteed no-op.
    #[test]
    fn an_empty_prefix_list_is_inert() {
        assert!(StoreSourcePolicy::new(Vec::<String>::new()).is_inert());
        assert!(!StoreSourcePolicy::new(["live-"]).is_inert());
    }

    /// **The safety guard.** `rank_of` gives an unmatched key the LOWEST precedence, so a series
    /// holding a writer the policy never mentions would have those rows superseded away — silently,
    /// irreversibly, on a timer. `unranked` is what lets the caller refuse.
    #[test]
    fn unranked_names_the_keys_the_policy_cannot_rank() {
        let p = StoreSourcePolicy::new(["live-", "pmxt:"]);
        assert!(p.unranked(["live-binance-x", "pmxt:2026"]).is_empty());
        assert_eq!(
            p.unranked(["live-a", "mystery:42", "pmxt:b", "other-7"]),
            vec!["mystery:42".to_string(), "other-7".to_string()]
        );
    }

    /// The prefix match is `starts_with`, so a longer key sharing a listed prefix IS ranked — that
    /// is the same rule `SourceRankPolicy::rank_of` uses, and the two must not disagree about which
    /// keys are known.
    #[test]
    fn unranked_agrees_with_rank_of_about_what_is_known() {
        let p = StoreSourcePolicy::new(["live-", "pmxt:"]);
        let rank = p.rank_policy();
        for key in ["live-x", "pmxt:y", "mystery:z"] {
            let known = p.unranked([key]).is_empty();
            let ranked = rank.rank_of(&[key.to_string()]) < p.prefixes.len();
            assert_eq!(known, ranked, "{key}");
        }
    }

    #[test]
    fn permissive_opts_out_of_the_guard() {
        assert!(!StoreSourcePolicy::new(["live-"]).permissive().strict);
    }
}
