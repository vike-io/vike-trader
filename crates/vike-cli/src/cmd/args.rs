//! The glue between `vike-cli` and the workspace's one command-line reader, `vike_util::cli`
//! (decision 0155). There is no flag loop in this crate any more: every verb declares its
//! arguments as a `#[derive(clap::Parser)]` struct (a PLANE — `backtest`, `config`, `data`, … —
//! as one struct with a `#[command(subcommand)]` enum), and reads its argv tail with [`parse`].
//!
//! What this module owns is the three things every verb used to re-spell around its parser:
//!
//! * [`parse`] — one verb's argv tail (everything after the verb word) read as a `clap` struct
//!   under the program name `vike-cli <verb>`, so a usage error prints `vike-cli <verb>: <message>`
//!   and the usage — the shape `vike-cli`'s own refusals always had. A verb that handles
//!   credential-shaped tokens uses [`parse_hiding_values`] (`secrets`), which keeps every value
//!   out of the error text.
//! * [`Stop`] — how a command line ended WITHOUT producing the verb's arguments: `-h`/`--help`
//!   and `-V`/`--version` (stdout, exit 0), a `clap` refusal, or one of the verb's own post-parse
//!   refusals built with [`Stop::message`]. It derefs to its text so a test reads
//!   `parse(..).unwrap_err().contains("--json")`.
//! * [`finish`] — the ONE place a [`Stop`] becomes a stream and an `ExitCode`: help is a SUCCESS on
//!   stdout (a non-zero `--help` breaks `set -e`, packaging smoke tests and every wrapper that
//!   checks a status), anything else is [`Exit::Usage`] on stderr. `vike_util::cli::USAGE_EXIT` and
//!   [`Exit::Usage`] are both 2 and `the_usage_rung_is_the_cli_layers_usage_exit` holds them equal,
//!   so no RUN rung (0-7) moved.
//!
//! The verb ROUTER is not here: [`crate::run`] reads the first token as the verb word and hands the
//! tail to that verb's module, which is the one place argv is split by hand (decision 0155 keeps
//! "first token = verb"). The root `clap` command that backs `vike-cli --help` and the
//! unknown-verb suggestion lives with it.
//!
//! ⚠ A verb's post-parse rules — a companion flag without its partner, a value only its own roster
//! can judge, a retired spelling answered by name — stay in the verb, after the parse, as
//! `Result<_, String>` and are lifted into a [`Stop`] by [`Stop::message`]; `clap` never learns
//! them, so their wording is the verb's own and pinned where it always was.
//!
//! ⚠ **History, kept so a citation by symbol still lands.** This module used to hold `Flags` (the
//! hand flag iterator), `no_value`, `help_requested`, `HELP_SENTINEL` (the help short-circuit
//! token), `exit_for_parse_error` and `is_flag_token`; decision 0155 deleted them in favour of
//! `clap`. The published command-surface table (`crate::surface`) and older records still name
//! them in the past tense, because they describe the code as it was when each row was read.

// History, kept so a citation by symbol still lands: `Flags::next_flag` was the hand-rolled flag
// loop's step; the `next_flag` word is gone with the loop (decision 0155).

use std::io::Write as _;
use std::ops::Deref;
use std::process::ExitCode;
use std::sync::OnceLock;

use vike_util::cli::{CliExit, Program, USAGE_EXIT};

use crate::exit::Exit;

/// The program name every usage line and every `<bin>: <message>` begins with. `vike-cli` is also
/// the multicall target (`vike-backend vike-cli config check`, `…/bin/vike-cli`): [`crate::run`]
/// takes the argv WITHOUT the program name in every case, so the name is this constant and the
/// usage reads `vike-cli …` whichever way the process was reached.
pub(crate) const PROGRAM: &str = "vike-cli";

/// The line `-V`/`--version` prints: `<name> <version> (<build identity>)` — the shape every
/// `--version` on the box has (`crates/vike-buildinfo/tests/identity_adoption.rs` holds each
/// printer to it). Built once, because [`Program`] borrows it.
fn version_line() -> &'static str {
    static LINE: OnceLock<String> = OnceLock::new();
    LINE.get_or_init(|| {
        vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
    })
}

/// The [`Program`] a verb is read under: `bin` is its full name (`vike-cli backtest`).
pub(crate) fn program(bin: &str) -> Program<'_> {
    Program::new(bin, version_line())
}

/// Read `args` — the argv tail AFTER the verb word — as the verb's `clap` struct `T`, under the
/// program name `bin` (`vike-cli backtest`).
///
/// # Errors
/// A [`Stop`]: help or version (exit 0, stdout) or a refusal (exit 2, stderr).
pub(crate) fn parse<T, I, A>(bin: &str, args: I) -> Result<T, Stop>
where
    T: clap::Parser,
    I: IntoIterator<Item = A>,
    A: Into<std::ffi::OsString> + Clone,
{
    program(bin).try_parse_from::<T, _, _>(args).map_err(Stop)
}

/// [`parse`] for a verb that is handed credential-shaped tokens: no VALUE appears in a refusal,
/// only flag names (`vike_util::cli::Program::hide_values`).
pub(crate) fn parse_hiding_values<T, I, A>(bin: &str, args: I) -> Result<T, Stop>
where
    T: clap::Parser,
    I: IntoIterator<Item = A>,
    A: Into<std::ffi::OsString> + Clone,
{
    program(bin).hide_values().try_parse_from::<T, _, _>(args).map_err(Stop)
}

/// `T`'s command under the program name `bin` — what `--help`, the parser, the docs gate and the
/// published reference all describe.
pub(crate) fn command<T: clap::CommandFactory>(bin: &str) -> clap::Command {
    program(bin).command::<T>()
}

/// What a RETIRED verb or sub-verb reads: everything after its word, ignored. The old dispatcher
/// never looked past a retired word either, and `vike-cli sweep --help` must still say where
/// `sweep` went — so the carrying command disables its own help and version flags and takes the
/// rest as one hidden trailing argument.
#[derive(clap::Args, Debug)]
pub(crate) struct Ignored {
    // Never read: the word alone decides the answer.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0.., hide = true)]
    #[allow(dead_code, clippy::allow_attributes)]
    rest: Vec<String>,
}

/// How a command line ended without producing the verb's arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stop(CliExit);

impl Stop {
    /// A refusal the VERB made after the parse: `<bin>: <msg>` on stderr, exit [`Exit::Usage`].
    pub(crate) fn message(bin: &str, msg: impl AsRef<str>) -> Self {
        Stop(CliExit {
            code: USAGE_EXIT,
            to_stdout: false,
            text: format!("{bin}: {}\n", msg.as_ref().trim_end()),
        })
    }

    /// `-h`/`--help` or `-V`/`--version`: a SUCCESS whose text is stdout.
    pub(crate) fn is_help(&self) -> bool {
        self.0.code == 0
    }

    /// Everything that would be printed.
    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.0.text
    }
}

impl Deref for Stop {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0.text
    }
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.text)
    }
}

impl PartialEq<str> for Stop {
    fn eq(&self, other: &str) -> bool {
        self.0.text == other
    }
}

impl PartialEq<&str> for Stop {
    fn eq(&self, other: &&str) -> bool {
        self.0.text == *other
    }
}

/// Print a [`Stop`] to its stream and turn it into this process's exit code. THE one place that
/// decision is made — see the module doc.
pub(crate) fn finish(stop: &Stop) -> ExitCode {
    // A closed pipe (`vike-cli backtest --help | head -1`) is not worth a different exit code.
    if stop.0.to_stdout {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(stop.0.text.as_bytes());
        let _ = out.flush();
    } else {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(stop.0.text.as_bytes());
        let _ = err.flush();
    }
    if stop.0.code == 0 { ExitCode::SUCCESS } else { Exit::Usage.into() }
}

/// A BARE `vike-cli`: the help page on STDOUT (it is the answer to "what can I run?"), and the
/// usage rung for the exit code (nothing was asked, and re-running unchanged cannot succeed). The
/// one place a [`Stop`] that is a success is printed and still a refusal.
pub(crate) fn finish_help_as_usage(stop: &Stop) -> ExitCode {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(stop.0.text.as_bytes());
    let _ = out.flush();
    Exit::Usage.into()
}

/// `T`'s long help page, as `--help` prints it — for a test asserting the help names what the
/// parser reads, instead of a hand-kept usage text that could say otherwise.
#[cfg(test)]
pub(crate) fn long_help<T: clap::CommandFactory>(bin: &str) -> String {
    command::<T>(bin).render_long_help().to_string()
}

/// The long help page of `cmd` AND of every subcommand under it, concatenated — for a test holding a
/// piece of prose (the MCP instructions, a skill page) to the flags and sub-verbs the command
/// actually accepts, instead of to a copy of them.
#[cfg(test)]
pub(crate) fn help_tree(cmd: &clap::Command) -> String {
    fn walk(cmd: &clap::Command, out: &mut String) {
        out.push_str(&cmd.clone().render_long_help().to_string());
        out.push('\n');
        for sub in cmd.get_subcommands() {
            walk(sub, out);
        }
    }
    let mut built = cmd.clone();
    built.build();
    let mut out = String::new();
    walk(&built, &mut out);
    out
}

/// A post-parse refusal printed and turned into the usage rung in one step — for the verbs whose
/// `run` finds out after the parse that the command line cannot be run.
pub(crate) fn refuse(bin: &str, msg: impl AsRef<str>) -> ExitCode {
    finish(&Stop::message(bin, msg))
}

/// `s` without ONE wrapping pair of matching `"` or `'` quotes, else unchanged. Nothing else is
/// decoded: no escape is honoured, an unbalanced or mismatched pair (`"a'`, a lone `"`) is left as
/// it stands, and only the outermost pair goes (`"'a'"` answers `'a'`). Byte indexing is safe: both
/// quote characters are single-byte, so slicing just inside them lands on char boundaries.
///
/// The one home for the trade grammar's `--reason` tail (`crate::cmd::trade`'s `split_reason` and
/// the one-shot `--reason`) and `crate::cmd::config::retired_env`'s environment-assignment value.
pub(crate) fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    let quoted = b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'');
    if quoted { &s[1..s.len() - 1] } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`finish`] answers a refusal on the rung `vike_util::cli` calls its usage exit, so the one
    /// number an argv error exits on is the same on both sides of this glue and no RUN rung
    /// (0-7, `crate::exit::Exit`) moved with the parser.
    #[test]
    fn the_usage_rung_is_the_cli_layers_usage_exit() {
        assert_eq!(USAGE_EXIT, 2);
        assert_eq!(
            format!("{:?}", ExitCode::from(Exit::Usage)),
            format!("{:?}", ExitCode::from(USAGE_EXIT as u8)),
            "`Exit::Usage` and `vike_util::cli::USAGE_EXIT` are one number"
        );
        assert_eq!(
            format!("{:?}", finish(&Stop::message("vike-cli x", "no"))),
            format!("{:?}", ExitCode::from(Exit::Usage)),
            "a post-parse refusal exits on the usage rung"
        );
    }

    /// `--help` and `--version` are SUCCESSES, and a refusal is not — read off the `Stop`, since
    /// the stream each writes to is observable only on the shipped binary (`tests/help_cli.rs`).
    #[test]
    fn help_is_a_success_and_a_refusal_is_not() {
        #[derive(clap::Parser, Debug)]
        struct Probe {
            #[arg(long, help = "a switch")]
            json: bool,
        }
        let help = parse::<Probe, _, _>("vike-cli probe", ["--help"]).unwrap_err();
        assert!(help.is_help(), "--help is a success: {help}");
        assert!(help.contains("Usage: vike-cli probe"), "{help}");
        assert_eq!(
            format!("{:?}", finish(&help)),
            format!("{:?}", ExitCode::SUCCESS),
            "--help exits 0 — a non-zero help breaks every `set -e` caller"
        );
        let version = parse::<Probe, _, _>("vike-cli probe", ["-V"]).unwrap_err();
        assert!(version.is_help(), "--version is a success: {version}");
        assert!(version.starts_with("vike-cli "), "name and version first: {version}");
        let refused = parse::<Probe, _, _>("vike-cli probe", ["--nope"]).unwrap_err();
        assert!(!refused.is_help());
        assert!(refused.starts_with("vike-cli probe: "), "{refused}");
        assert!(refused.contains("--nope"), "names the flag: {refused}");
        assert_eq!(
            format!("{:?}", finish(&refused)),
            format!("{:?}", ExitCode::from(Exit::Usage)),
            "…and a genuine usage error exits on the USAGE rung, not the run-failure one"
        );
        let valued = parse::<Probe, _, _>("vike-cli probe", ["--json=1"]).unwrap_err();
        assert!(valued.contains("--json"), "a switch with a value is refused: {valued}");
    }

    /// [`unquote`] replaced two copies of different SHAPE: the trade grammar's byte test (kept as
    /// the home) and `crate::cmd::config::retired_env`'s `strip_prefix`/`strip_suffix` search. The
    /// table pins the exact answers, and every row is also run through the retired copy's
    /// formulation, spelled here as the oracle, so equal behaviour is proven over quotes, escapes,
    /// empty and unbalanced input rather than asserted.
    #[test]
    fn unquote_strips_one_matching_pair_exactly_as_both_copies_did() {
        let retired_env_shape = |value: &str| -> String {
            ['"', '\'']
                .into_iter()
                .find_map(|q| value.strip_prefix(q).and_then(|rest| rest.strip_suffix(q)))
                .unwrap_or(value)
                .to_string()
        };
        let table: &[(&str, &str)] = &[
            ("", ""),
            ("plain", "plain"),
            ("\"a b\"", "a b"),
            ("'a b'", "a b"),
            ("\"\"", ""),
            ("''", ""),
            ("\"", "\""),
            ("'", "'"),
            ("\"\"\"", "\""),
            ("\"a'", "\"a'"),
            ("'a\"", "'a\""),
            ("\"a", "\"a"),
            ("a\"", "a\""),
            ("\"'a'\"", "'a'"),
            ("'\"a\"'", "\"a\""),
            ("\"a\\\"", "a\\"),
            ("\"a\\\"b\"", "a\\\"b"),
            (" \"a\" ", " \"a\" "),
            ("\"é ü\"", "é ü"),
            ("é", "é"),
        ];
        for (input, want) in table {
            assert_eq!(unquote(input), *want, "unquote({input:?})");
            assert_eq!(retired_env_shape(input), *want, "the retired_env shape on {input:?}");
        }
    }
}
