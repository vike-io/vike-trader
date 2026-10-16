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
//! ⚠ **A plane obliges, and this one's obligation was MET in stage 7.** This module used to say the
//! plane's only sub-verb refused on every box because no daemon served the study wire verb. One
//! does now: `vike-backend backtesthub --addr` serves `Request::RunStudy` (it used to need the multicall
//! to hand it a study runner; `crates/vike-backtesthub/src/backtesthub_server/dispatch.rs`'s `handle_request`
//! calls `vike_studio_core::run_study_local` directly now), and
//! `vike_datahub_client::FEATURE_STUDY` is the negotiation anchor it advertises. Ruling
//! 1 put that request in scope for stage 7 for exactly this reason: one refusing client-half VERB
//! was tolerable, a whole refusing PLANE is not. What still refuses is a peer that advertises no
//! `study` (the data server, a bare `backtesthub` bin build) — `crate::cmd::study`'s module doc
//! carries both refusals.
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

use vike_hub_proto::auth::HubKeys;

use crate::cmd::args::{self, Stop};
use crate::cmd::study::StudyCmd;

/// The program name this plane is read under; a refusal prints as `vike-cli research: …`.
const BIN: &str = "vike-cli research";

/// The `research` plane's command line. There is no bare `vike-cli research …`: the sub-verb is
/// required, so one action has one spelling (the rule `crate::cmd::backtest` follows). This plane
/// carries no RETIRED sub-verb spelling of its own — the retired name is the TOP-LEVEL `study`,
/// answered one level up by `crate::RETIRED_COMMANDS`.
#[derive(clap::Parser, Debug)]
#[command(
    about = "Investigate a signal and fit a model — the plane BEFORE a strategy exists",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    sub: Sub,
}

/// The plane's sub-verbs. Adding one is a variant here and an arm in [`run`]; the roster in every
/// refusal and in `--help` is `clap`'s own, so it cannot name the set short.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum Sub {
    // (An attribute, not a doc comment: a doc comment here would reset the `long_about` that
    // `StudyCmd` carries, because `clap` applies a variant's doc after its struct's arguments.)
    #[command(
        about = "Ask the backend to run a compiled study over the hist store IT holds. ⚠ A peer \
                 that advertises no `study` capability is refused and pointed at `vike-backend \
                 study`"
    )]
    Study(StudyCmd),
}

/// Read a `research` argv tail — everything after the verb word.
pub(crate) fn parse(argv: impl IntoIterator<Item = String>) -> Result<Sub, Stop> {
    args::parse::<Cli, _, _>(BIN, argv).map(|cli| cli.sub)
}

/// Entry point the dispatcher routes to. `argv` is everything AFTER the `research` verb;
/// `configured_addr` is `config.backtest_addr`, resolved once by [`crate::run`] — the middle rung
/// of the address ladder, which only the composition root may read.
///
/// ⚠ The ladder itself is `crate::cmd::backtest`'s `resolve_addr` and is NOT re-derived here. Both
/// planes dial the same backtesthub and read the same setting; a ladder forks when a SETTING
/// forks, not when a plane does, and a second copy of that fold is the exact defect it was written
/// to remove.
pub fn run(
    argv: impl Iterator<Item = String>,
    configured_addr: Option<&str>,
    keys: Option<&HubKeys>,
) -> ExitCode {
    match parse(argv) {
        // ⚠ DELEGATED whole, rather than re-parsed here. `study`'s grammar is disjoint from every
        // other verb in this binary (a recipe rather than a profile, a registry name rather than a
        // strategy) and it carries two bespoke refusals of its own — `--store`/`--lightgbm` name
        // paths on the BACKEND's box — each with a sentence a generic "unknown option" could not
        // produce. `crate::cmd::trade`'s one-shot verbs are the in-crate precedent for a sub-verb
        // owning its own arguments.
        Ok(Sub::Study(cmd)) => crate::cmd::study::run(cmd, configured_addr, keys),
        Err(stop) => args::finish(&stop),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_sub(args: &[&str]) -> Result<Sub, Stop> {
        parse(args.iter().map(|s| (*s).to_string()))
    }

    /// Every sub-verb is reachable by the name it advertises.
    #[test]
    fn every_subcommand_is_reachable_by_the_name_it_advertises() {
        let parsed =
            parse_sub(&["study", "--study", "c", "--recipe", "r.toml", "--from", "a", "--to", "b"])
                .unwrap_or_else(|e| panic!("study: {e}"));
        assert!(matches!(parsed, Sub::Study(_)));
    }

    /// ⚠ A missing sub-verb names EVERY sub-verb that exists — `clap` derives the roster from the
    /// enum, so no hand-written refusal can name it short (the defect `crate::cmd::data`'s copy had
    /// on the verb that DELETES).
    #[test]
    fn a_missing_subcommand_names_every_subcommand_that_exists() {
        let err = parse_sub(&[]).unwrap_err();
        assert!(!err.is_help(), "a bare `research` is a usage error: {err}");
        assert!(err.contains("study"), "must name study: {err}");
    }

    /// Every sub-verb is NAMED in the help — the discovery surface for the plane.
    #[test]
    fn every_subcommand_is_named_in_the_help() {
        let help = args::long_help::<Cli>(BIN);
        assert!(help.contains("vike-cli research"), "{help}");
        assert!(help.contains("study"), "the plane's help must offer `study`:\n{help}");
    }

    /// ⚠ A flag where a sub-verb belongs is refused, naming the flag.
    #[test]
    fn a_flag_where_a_subcommand_belongs_is_refused() {
        let err = parse_sub(&["--study", "cohort"]).unwrap_err();
        assert!(!err.is_help());
        assert!(err.contains("--study"), "names what was typed: {err}");
    }

    /// `--help` at the plane and at its sub-verb are successes.
    #[test]
    fn help_is_a_success_at_every_level() {
        for argv in [&["--help"][..], &["-h"][..], &["help"][..], &["study", "--help"][..]] {
            let stop = parse_sub(argv).unwrap_err();
            assert!(stop.is_help(), "{argv:?} must be a success: {stop}");
        }
    }
}
