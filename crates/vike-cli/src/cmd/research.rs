//! `vike-cli research` — investigate a signal and FIT a model. The plane BEFORE a strategy exists.
//!
//! ⚠ **This is a PLANE, not a verb, and its one sub-verb was a top-level verb until ruling 1 of
//! the 2026-09-13 owner rulings on
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`.** The top level names
//! planes — `backtest` (compute a strategy over history), `research` (investigate a signal, fit a
//! model), `data` (market history), `trade` (live) — and a study belongs to none of the other
//! three: it drives a LightGBM child process per fold and produces a fitted MODEL, not a PnL
//! curve. The tree already drew this line before the CLI did: `crates/vike-user-research`'s
//! `build.rs` scans `<project>/user_data/research/studies/rust/<name>/` into a compiled registry,
//! so `research` is the AREA and `study` is one unit of work inside it.
//!
//! ⚠ **A plane obliges.** Today this plane's only sub-verb REFUSES on every box, because no daemon
//! serves the study wire verb — `crate::cmd::study`'s module doc argues that scope decision where
//! the code is, and `vike_datahub_client::FEATURE_STUDY` is the negotiation anchor the server arm
//! will attach to. Ruling 1 put that request in scope for stage 7 rather than leaving it open, for
//! exactly this reason: one refusing client-half VERB was tolerable, a whole refusing PLANE is not.
//!
//! ⚠ **The backend's names are NOT this module's to rename.** `vike-backend study` and the
//! `vike-study` binary are a different program and a separate decision (ruling 1 says so
//! outright); `crate::cmd::study`'s `backend_fallback` names the one invocation that actually runs
//! a study, in every failure this plane can produce.
//!
//! # Usage
//!
//! ```text
//! vike-cli research study --study NAME --recipe FILE --from WHEN --to WHEN [--addr host:port]
//! ```

use std::process::ExitCode;

use vike_node_proto::auth::NodeKeys;

use crate::cmd::args;

/// The plane's usage roster. `pub(crate)` for the same reason `crate::cmd::backtest`'s `USAGE` is:
/// one text, read by the refusal path and by the tests that hold it honest.
pub(crate) const USAGE: &str = "usage: vike-cli research <subcommand> [options]\n\
\n\
  study    ask the backend to run a compiled study over the hist store IT holds. ⚠ No backend\n\
           serves this yet — it refuses and names `vike-backend study`, which does\n\
\n\
       vike-cli research study --study NAME --recipe FILE --from WHEN --to WHEN [--addr host:port]";

/// Which sub-verb ran. Adding one is an arm here, an arm in [`claim_subcommand`], an arm in
/// [`run`]'s dispatch, and a row in [`USAGE`] — and `every_subcommand_is_named_in_usage` plus
/// `every_subcommand_is_reachable_by_the_name_it_advertises` hold the last two honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sub {
    /// `study` — ask the backend to run a COMPILED study over the hist store it holds. A
    /// top-level verb until ruling 1. Its whole grammar, address ladder, negotiation and messages
    /// stay in `crate::cmd::study`; this arm only routes to them.
    Study,
}

/// Every sub-verb, in the order [`USAGE`] lists them.
///
/// ⚠ It exists so the "a subcommand is required (…)" refusal is DERIVED rather than typed, and a
/// roster of ONE is exactly where that matters: a hand-written refusal reads perfectly today and is
/// short the day a second row lands. `crate::cmd::data`'s hand-written copy named its roster short
/// on the verb that DELETES, which is what put this shape in the crate.
const SUBCOMMANDS: &[Sub] = &[Sub::Study];

impl Sub {
    /// The name the operator typed, which is also what every refusal message names it by.
    fn as_str(self) -> &'static str {
        match self {
            Sub::Study => "study",
        }
    }
}

/// The roster as one message fragment, so no refusal writes the list down.
fn subcommand_roster() -> String {
    SUBCOMMANDS.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")
}

/// Claim the FIRST argv token as the sub-verb, leaving the rest for that sub-verb's own parser.
///
/// ⚠ **There is no bare `vike-cli research …`** — the same rule `crate::cmd::backtest` follows, so
/// one action has one spelling. A missing sub-verb prints usage and exits 2.
///
/// This plane carries no RETIRED sub-verb spelling of its own (the retired name is the TOP-LEVEL
/// `study`, answered one level up by `crate::RETIRED_COMMANDS`), which is why it does not share
/// `crate::cmd::backtest`'s router: that one answers `--list-params` by name and this one has
/// nothing to answer.
///
/// ⚠ The help arm is [`args::help_requested`] and nothing else, for the reason
/// `crate::cmd::backtest`'s router spells out: it is generic, `T` is fixed by this function's own
/// `Result<Sub, String>`, and hand-typing the sentinel's text prints the internal token
/// `help requested` to a user's terminal on exit 1 instead of usage on exit 0.
/// `crates/vike-cli/tests/help_cli.rs`'s module doc carries the incident that cost four commands
/// at once.
fn claim_subcommand(it: &mut impl Iterator<Item = String>) -> Result<Sub, String> {
    let Some(first) = it.next() else {
        return Err(format!("a subcommand is required ({})", subcommand_roster()));
    };
    match first.as_str() {
        "study" => Ok(Sub::Study),
        "-h" | "--help" | "help" => args::help_requested(),
        other if other.starts_with("--") => Err(format!(
            "{other} is a flag, and a subcommand is required first ({}) — try `vike-cli research \
             study {other} …`",
            subcommand_roster()
        )),
        other => Err(format!("unknown `research` subcommand '{other}' ({})", subcommand_roster())),
    }
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `research` verb;
/// `configured_addr` is `config.backtest_addr`, resolved once by [`crate::run`] — the middle rung
/// of the address ladder, which only the composition root may read.
///
/// ⚠ The ladder itself is `crate::cmd::backtest`'s `resolve_addr` and is NOT re-derived here. Both
/// planes dial the same compute daemon and read the same setting; a ladder forks when a SETTING
/// forks, not when a plane does, and a second copy of that fold is the exact defect it was written
/// to remove.
pub fn run(
    args: impl Iterator<Item = String>,
    configured_addr: Option<&str>,
    keys: Option<&NodeKeys>,
) -> ExitCode {
    let mut it = args;
    let sub = match claim_subcommand(&mut it) {
        Ok(s) => s,
        Err(msg) => return args::exit_for_parse_error("research", USAGE, &msg),
    };
    match sub {
        // ⚠ DELEGATED whole, rather than re-parsed here. `study`'s grammar is disjoint from every
        // other verb in this binary (a recipe rather than a profile, a registry name rather than a
        // strategy) and it carries three bespoke refusals of its own — `--store`/`--lightgbm` name
        // paths on the BACKEND's box, `--json` has nothing to format yet — each with a sentence a
        // generic "unknown option" could not produce. `crate::cmd::trade`'s one-shot verbs are the
        // in-crate precedent for a sub-verb owning its own parser.
        Sub::Study => crate::cmd::study::run(it, configured_addr, keys),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_sub(args: &[&str]) -> Result<Sub, String> {
        claim_subcommand(&mut args.iter().map(|s| (*s).to_string()))
    }

    /// Every sub-verb is reachable by the name it advertises, and [`claim_subcommand`]'s match and
    /// [`SUBCOMMANDS`] agree in BOTH directions.
    #[test]
    fn every_subcommand_is_reachable_by_the_name_it_advertises() {
        for sub in SUBCOMMANDS {
            let argv: Vec<&str> = match sub {
                // ⚠ `claim_subcommand` returns before any flag is looked at, so these tokens are
                // inert here. They are written out so the row reads the way a sibling row will,
                // and so the test survives a change that makes the router look further.
                Sub::Study => {
                    vec!["study", "--study", "c", "--recipe", "r.toml", "--from", "a", "--to", "b"]
                }
            };
            let parsed = parse_sub(&argv).unwrap_or_else(|e| panic!("{}: {e}", sub.as_str()));
            assert_eq!(parsed, *sub, "{} parsed as a different subcommand", sub.as_str());
        }
    }

    /// ⚠ A missing sub-verb names EVERY sub-verb that exists, DERIVED from [`SUBCOMMANDS`]. The
    /// roster is ONE row today, which is exactly when a hand-written refusal looks fine and then
    /// rots — `crate::cmd::data`'s hand-written copy omitted a subcommand that had shipped months
    /// earlier, on the verb that DELETES.
    #[test]
    fn a_missing_subcommand_names_every_subcommand_that_exists() {
        let err = parse_sub(&[]).unwrap_err();
        for sub in SUBCOMMANDS {
            assert!(err.contains(sub.as_str()), "must name {}: {err}", sub.as_str());
        }
    }

    /// Every sub-verb is NAMED in the usage text — the third leg of the same contract, and the one
    /// no other test covers.
    #[test]
    fn every_subcommand_is_named_in_usage() {
        for sub in SUBCOMMANDS {
            let spelled = format!("vike-cli research {}", sub.as_str());
            assert!(USAGE.contains(&spelled), "USAGE must offer `{spelled}`:\n{USAGE}");
        }
    }

    /// ⚠ A flag where a sub-verb belongs must not read as an unknown SUBCOMMAND called `--study`.
    /// The message says it is a flag and names the roster, the same shape
    /// `crate::cmd::backtest`'s router uses one plane over.
    #[test]
    fn a_flag_where_a_subcommand_belongs_says_so() {
        let err = parse_sub(&["--study", "cohort"]).unwrap_err();
        assert!(err.contains("--study"), "names what was typed: {err}");
        assert!(err.contains("subcommand"), "…and says what was expected: {err}");
        assert!(err.contains("study"), "…and names the roster: {err}");
    }
}
