//! "Send me your journal": deterministically replay a journal dir and verify its determinism fence.
//! The fence verifies EXEC-LANE determinism — that re-folding the journaled exec lane from the first
//! checkpoint reproduces the recorded final order/account state. Protocol/result stdout stays raw
//! (repo logging rule for tools).
//!
//!   cargo run -p vike-core --bin replay_journal -- <journal-dir>
//!
//! The journal directory is the one positional argument, read by `clap` through
//! `vike_util::cli::Program` (decision 0155): `-h`/`--help` and `-V`/`--version` on stdout with
//! exit 0, any usage error (no directory, a second one, an unknown flag) on stderr with exit 2.

use std::path::PathBuf;

use vike_util::cli::{CliExit, Program};

/// The command line.
#[derive(clap::Parser, Debug, PartialEq)]
#[command(
    name = "replay_journal",
    about = "Deterministically replay a journal directory and verify its determinism fence"
)]
struct Cli {
    /// The journal directory to replay.
    #[arg(value_name = "JOURNAL_DIR")]
    dir: PathBuf,
}

/// Read the command line (WITHOUT the program name).
///
/// # Errors
/// The [`CliExit`] a process would have printed: `--help`/`--version` (code 0) or a usage error
/// (code 2).
fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Cli, CliExit> {
    Program::new("replay_journal", concat!("replay_journal ", env!("CARGO_PKG_VERSION")))
        .try_parse_from(args)
}

fn main() {
    let Cli { dir } = match parse_args_from(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(exit) => exit.emit(),
    };
    match vike_core::replay::replay_offline(&dir) {
        Ok(out) => {
            // `records` is the TOTAL source journal record count (Cmds + Snaps), not the re-folded
            // tail length — label it honestly.
            println!("journal records  : {}", out.records);
            println!("snaps verified   : {}", out.snaps_compared);
            println!("final state hash : {:016x}", out.final_hash);
            for e in &out.engines {
                println!(
                    "engine {}/{}: {} open orders, {} positions, balance {}",
                    e.venue,
                    e.symbol,
                    e.registry.len(),
                    e.account.positions.len(),
                    e.account.balance
                );
            }
        }
        Err(e) => {
            eprintln!("REPLAY FAILED: {e:?}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &[&str]) -> Result<Cli, CliExit> {
        parse_args_from(line.iter().map(|s| (*s).to_string()))
    }

    /// The documented line: one positional directory, as `scripts/skills/record-and-replay-a-session.md`
    /// writes it (`replay_journal <journal-dir>`).
    #[test]
    fn the_journal_directory_is_the_one_positional() {
        assert_eq!(
            parse(&["/var/vike/journal"]).unwrap(),
            Cli { dir: PathBuf::from("/var/vike/journal") }
        );
        assert_eq!(parse(&["data/journal"]).unwrap().dir, PathBuf::from("data/journal"));
    }

    #[test]
    fn no_directory_a_second_one_and_an_unknown_flag_are_usage_errors() {
        for line in [&[][..], &["a", "b"], &["--nope"]] {
            let exit = parse(line).expect_err("this command line must be refused");
            assert_eq!((exit.code, exit.to_stdout), (2, false), "{line:?}: {exit:?}");
        }
    }

    #[test]
    fn help_and_version_are_a_success_on_stdout() {
        for flag in ["-h", "--help"] {
            let exit = parse(&[flag]).expect_err("help is an exit");
            assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}");
        }
        let exit = parse(&["-V"]).expect_err("version is an exit");
        assert_eq!(exit.text, concat!("replay_journal ", env!("CARGO_PKG_VERSION"), "\n"));
    }
}
