//! `vike-cli strategy` — what a strategy FILE does across the numbered SDKs it can be built against
//! (`docs/decisions/0296-a-rust-strategy-file-builds-against-a-numbered-frozen-sdk-it-may-pin.md`).
//!
//! A PLANE, like `backtest` and `research`: a sub-verb is required, and a missing one prints the plane's
//! help on the usage rung. Its sub-verbs:
//!
//! - `sdk-diff <FILE> --from N --to M --profile P` runs the same backtest with the file built against
//!   SDK N and against SDK M and compares the two results (`crate::cmd::strategy::sdk_diff`'s module doc).
//! - `migrate <FILE> [--to N]` first rewrites a Rust file's JSON `save_state`/`load_state` onto
//!   `StrategyState` (ABI 15), pinned or not, and a bar's or a tick's `String` symbol onto
//!   `vike_marketdata::Symbol` unless its pinned SDK already lends `Symbol` rows, then moves a file
//!   that pins `[strategy] sdk = N` to a newer SDK, one SDK at a time, each step built before it is
//!   kept, or carries a Rhai script across
//!   the names this vike renamed (`migrate`'s module doc).
//!
//! # Usage
//!
//! ```text
//! vike-cli strategy sdk-diff trend/sma_cross.rs --from 3 --to 4 --profile lab.toml [--addr …] [--json]
//! vike-cli strategy sdk-diff --all trend --from 3 --to 4 --profile lab.toml
//! vike-cli strategy migrate FILE [--to N] [--dry-run] [--addr host:port]
//! ```

pub(crate) mod sdk_diff;

use std::path::Path;
use std::process::ExitCode;

use vike_hub_protocol::auth::HubKeys;

use crate::cmd::args::{self, Stop};
use sdk_diff::SdkDiffCommand;

mod migrate;
mod rename_table;

/// The program name this plane is read under; a refusal prints as `vike-cli strategy: …`.
const BIN: &str = "vike-cli strategy";

/// The `strategy` plane's command line: the sub-verb is required, so one action has one spelling.
#[derive(clap::Parser, Debug)]
#[command(
    about = "What a strategy file does across the SDKs it can be built against",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    sub: Sub,
}

/// The plane's sub-verbs. Adding one is a variant here and an arm in [`run`].
#[derive(clap::Subcommand, Debug)]
pub(crate) enum Sub {
    // (An attribute, not a doc comment: a doc comment would reset the struct's own help.)
    #[command(
        name = "sdk-diff",
        about = "Run one backtest with the file built against SDK N and against SDK M, and compare \
                 the results: exit 0 identical, 6 they differ, 7 neither traded (nothing compared), \
                 1 a run or a build failed"
    )]
    SdkDiff(SdkDiffCommand),
    #[command(
        about = "Move a Rust strategy file onto the current strategy interface: first its JSON \
                 `save_state`/`load_state` are rewritten onto `StrategyState` (ABI 15), pinned or \
                 not, and a bar's or a tick's `String` symbol onto `vike_marketdata::Symbol` unless \
                 its pinned SDK already lends `Symbol` rows (a hook or read of another shape is left \
                 as it is, with a hint naming its line); then a \
                 file that pins `[strategy] sdk = N` moves to a newer SDK, one SDK at a time (each \
                 step's renames applied, its pin bumped, kept only when the file BUILDS on it). On a \
                 Rhai script it applies the renamed names. Keeps a numbered backup (`FILE.bak.K`, \
                 never overwritten); `--dry-run` prints the diff and writes nothing"
    )]
    Migrate(migrate::MigrateCommand),
}

/// This plane as a `clap` command, for the published reference (`crate::surface`).
pub(crate) fn command() -> clap::Command {
    args::command::<Cli>(BIN)
}

/// Read a `strategy` argv tail — everything after the verb word.
pub(crate) fn parse(argv: impl IntoIterator<Item = String>) -> Result<Sub, Stop> {
    args::parse::<Cli, _, _>(BIN, argv).map(|cli| cli.sub)
}

/// What the plane needs from the composition root (a `src/cmd/` file resolves no project and
/// reads no setting of its own).
pub(crate) struct Context<'a> {
    /// The project root, for `vendor/vike-strategy-source/` (the installed SDKs and their rename
    /// tables).
    pub(crate) project_root: Option<&'a Path>,
    /// `config.backtest_addr`, folded by `vike_client::resolved`'s `backtesthub_dial_address`.
    pub(crate) configured_address: Option<&'a str>,
    /// This box's device key, which a Build or a run signs with (both are Control verbs).
    pub(crate) keys: Option<&'a HubKeys>,
}

/// Entry point the dispatcher routes to. `argv` is everything AFTER the `strategy` verb.
pub(crate) fn run(argv: impl Iterator<Item = String>, ctx: &Context<'_>) -> ExitCode {
    match parse(argv) {
        Ok(Sub::SdkDiff(cmd)) => sdk_diff::run(&cmd, ctx.configured_address, ctx.keys),
        Ok(Sub::Migrate(cmd)) => match migrate::run(&cmd, ctx) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{BIN} migrate: {}", e.message);
                e.exit.into()
            }
        },
        Err(stop) => args::finish(&stop),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_sub(argv: &[&str]) -> Result<Sub, Stop> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn migrate_is_reachable_with_its_flags_and_a_bare_plane_names_both_verbs() {
        let parsed = parse_sub(&["migrate", "s.rs", "--to", "4", "--dry-run"])
            .unwrap_or_else(|e| panic!("migrate: {e}"));
        let Sub::Migrate(cmd) = parsed else { panic!("not migrate: {parsed:?}") };
        assert_eq!((cmd.to, cmd.dry_run), (Some(4), true));
        let err = parse_sub(&[]).unwrap_err();
        assert!(!err.is_help(), "a bare `strategy` is a usage error: {err}");
        assert!(err.contains("migrate") && err.contains("sdk-diff"), "must name both: {err}");
    }
}
