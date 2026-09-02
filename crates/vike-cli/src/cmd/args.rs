//! Shared hand-rolled argument-parsing glue for the `vike-cli` subcommands (no `clap` — this crate
//! deliberately adds no dependency). One tiny flag iterator replaces the `split_once('=')` /
//! `take_value` loop that was hand-copied across `backtest`/`sweep`/`walkforward` and re-rolled as
//! closures in `mcp`/`trade`, already drifting in shape. Each command KEEPS its own `Args` struct
//! and its own match over flag names — this module owns only the mechanics every parser shares:
//! the `--flag value` AND `--flag=value` forms, value resolution with a clean dangling-flag error,
//! bare-BOOLEAN rejection of inline values, the `-h`/`--help` short-circuit, and the ONE place a
//! parser's `Err` becomes an exit code + a stream ([`exit_for_parse_error`]).

use std::process::ExitCode;

/// The flag iterator every subcommand parser drains: wraps the raw argv iterator and yields
/// `(flag, inline)` pairs via [`Flags::next_flag`], resolving value-taking flags via
/// [`Flags::value`].
pub(crate) struct Flags<I: Iterator<Item = String>> {
    it: I,
}

impl<I: Iterator<Item = String>> Flags<I> {
    pub(crate) fn new(it: I) -> Self {
        Self { it }
    }

    /// The next argument, split into `(flag, inline)`: `--flag=value` is `(flag, Some(value))`
    /// (first `=` only, so the value may itself contain `=`); a bare `--flag` is `(flag, None)`.
    pub(crate) fn next_flag(&mut self) -> Option<(String, Option<String>)> {
        let arg = self.it.next()?;
        Some(match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg, None),
        })
    }

    /// Resolve a value-taking flag's value from either its inline `=value` or the NEXT argument
    /// (consumed raw — never re-split on `=`), erroring if neither is present (a trailing
    /// `--profile` with nothing after it) — or if what it found is [`is_flag_token`].
    pub(crate) fn value(&mut self, flag: &str, inline: Option<String>) -> Result<String, String> {
        let found = match inline {
            Some(v) => v,
            None => self.it.next().ok_or_else(|| format!("{flag} requires a value"))?,
        };
        if is_flag_token(&found) {
            return Err(format!(
                "{flag} requires a value, but the next argument is another flag ({found})"
            ));
        }
        Ok(found)
    }
}

/// **THE RULE, spelled once for this crate: a token beginning with `--` is a FLAG, never a value.**
///
/// Without it a valued flag ate whatever followed, including another flag —
/// `vike-cli backtest --script --json` took the literal `--json` as the SCRIPT PATH and then ran
/// with JSON output off, and `--profile --addr 1.2.3.4:9` put `--addr` in the profile path and
/// died on the address as an unknown argument, naming a token the operator had written correctly.
/// The same class of defect the nine binary parsers carried; this is the one place it is spelled
/// for every `vike-cli` subcommand at once.
///
/// It is `--`, not a bare `-`, and that is the load-bearing half: a NEGATIVE NUMBER is a real value
/// in this workspace's parsers, so a `starts_with('-')` test would turn every signed field into a
/// usage error. The residual is stated rather than implied — a single-dash token (`-h`, `-5`) is
/// still accepted as a value.
///
/// It is applied to the INLINE `=value` form as well, deliberately: one rule, one sentence, no
/// per-spelling exception to remember. The cost is that a value which genuinely begins with `--`
/// has no flag-attached spelling; a path is still reachable as `./--weird`.
fn is_flag_token(token: &str) -> bool {
    token.starts_with("--")
}

/// Reject an inline `=value` on a bare BOOLEAN flag (`--json=1` is a usage error, never a silent
/// true) — call before setting the flag.
pub(crate) fn no_value(flag: &str, inline: Option<String>) -> Result<(), String> {
    match inline {
        Some(_) => Err(format!("{flag} takes no value")),
        None => Ok(()),
    }
}

/// The token [`help_requested`] carries. It is CONTROL FLOW — these parsers return `Result`, so
/// "the user asked for help" has to travel back through the `Err` channel — and it must never
/// reach a terminal: it is not a diagnostic, and it tells a user nothing.
///
/// Spelled once, here, so [`exit_for_parse_error`] and the parsers cannot disagree about it.
pub(crate) const HELP_SENTINEL: &str = "help requested";

/// The shared `-h`/`--help` short-circuit: the `Err` the non-interactive commands return so their
/// caller can print the usage and exit. Pair it with [`exit_for_parse_error`], which is what turns
/// it back into a SUCCESS. The `trade` REPL is the one deliberate outlier — it records
/// `help = true` and prints its verb reference instead.
pub(crate) fn help_requested<T>() -> Result<T, String> {
    Err(HELP_SENTINEL.to_string())
}

/// Turn a parser's `Err` into this subcommand's exit code, printing the right thing to the right
/// stream. THE one place that decision is made.
///
/// * `-h`/`--help` — a **success**: the usage goes to **stdout**, because help is normal output a
///   user pipes into a pager, not a diagnostic. Nothing else is printed, and [`HELP_SENTINEL`]
///   never appears anywhere.
/// * anything else — a usage **error**: `vike-cli <command>: <msg>` and the usage, on stderr,
///   exit 1.
///
/// Every non-interactive command used to inline the error arm and route the help short-circuit
/// straight into it, so `--help` exited **1** and printed `vike-cli backtest: help requested`. A
/// non-zero `--help` breaks `set -e`, packaging smoke tests and any wrapper that checks a status,
/// and the leaked token reads as an internal error escaping to a user. Sharing the decision is
/// what stops the spellings drifting apart again: `config` had already been fixed at its verb
/// level and NOT at its `show --help` level, in the same file.
pub(crate) fn exit_for_parse_error(command: &str, usage: &str, msg: &str) -> ExitCode {
    if msg == HELP_SENTINEL {
        println!("{usage}");
        return ExitCode::SUCCESS;
    }
    eprintln!("vike-cli {command}: {msg}\n{usage}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(args: &[&str]) -> Flags<std::vec::IntoIter<String>> {
        Flags::new(args.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter())
    }

    #[test]
    fn space_and_inline_forms_both_resolve() {
        let mut f = flags(&["--profile", "run.toml", "--addr=1.2.3.4:9"]);
        let (flag, inline) = f.next_flag().unwrap();
        assert_eq!(flag, "--profile");
        assert_eq!(inline, None);
        assert_eq!(f.value(&flag, inline).unwrap(), "run.toml");
        let (flag, inline) = f.next_flag().unwrap();
        assert_eq!(flag, "--addr");
        assert_eq!(f.value(&flag, inline).unwrap(), "1.2.3.4:9");
        assert!(f.next_flag().is_none());
    }

    #[test]
    fn only_the_first_equals_splits_and_next_arg_values_are_never_resplit() {
        // `--script=a=b.rhai`: the inline value keeps its own `=`.
        let mut f = flags(&["--script=a=b.rhai", "--profile", "k=v.toml"]);
        let (flag, inline) = f.next_flag().unwrap();
        assert_eq!((flag.as_str(), inline.as_deref()), ("--script", Some("a=b.rhai")));
        // A next-argument value is consumed raw — never split on `=`.
        let (flag, inline) = f.next_flag().unwrap();
        assert_eq!(flag, "--profile");
        assert_eq!(f.value(&flag, inline).unwrap(), "k=v.toml");
    }

    #[test]
    fn a_dangling_value_flag_is_a_clean_error() {
        let mut f = flags(&["--profile"]);
        let (flag, inline) = f.next_flag().unwrap();
        assert_eq!(f.value(&flag, inline).unwrap_err(), "--profile requires a value");
    }

    /// **A valued flag may not eat a FLAG**, in either spelling. `--script --json` used to take the
    /// literal `--json` as the script PATH and leave JSON output off; `--profile --addr 1.2.3.4:9`
    /// put `--addr` in the profile path and then died on the address, naming a token the operator
    /// had written correctly. Both errors now name the unfed flag AND the token it would have
    /// eaten.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        for line in [&["--script", "--json"][..], &["--script=--json"][..]] {
            let mut f = flags(line);
            let (flag, inline) = f.next_flag().unwrap();
            let e = f.value(&flag, inline).expect_err("a flag is not a value");
            assert!(e.contains("--script") && e.contains("--json"), "{line:?}: {e}");
        }
        // The rule is `--`, not `-`: a single-dash token is still a value, which is what keeps
        // every negative number in this workspace's parsers reachable.
        let mut f = flags(&["--qty", "-5"]);
        let (flag, inline) = f.next_flag().unwrap();
        assert_eq!(f.value(&flag, inline).unwrap(), "-5");
        // …and an ordinary value is untouched, including one that merely CONTAINS dashes.
        let mut f = flags(&["--profile", "run--x.toml"]);
        let (flag, inline) = f.next_flag().unwrap();
        assert_eq!(f.value(&flag, inline).unwrap(), "run--x.toml");
    }

    #[test]
    fn a_bare_boolean_rejects_an_inline_value() {
        assert_eq!(no_value("--json", Some("1".to_string())).unwrap_err(), "--json takes no value");
        assert!(no_value("--json", None).is_ok());
    }

    #[test]
    fn help_short_circuit_is_the_pinned_message() {
        assert_eq!(help_requested::<()>().unwrap_err(), "help requested");
        assert_eq!(HELP_SENTINEL, "help requested");
    }

    /// The two outcomes [`exit_for_parse_error`] separates, as EXIT CODES. The stream each writes
    /// to is not observable from inside the process, so it is asserted where a user sees it — the
    /// spawn tests in `tests/help_cli.rs`.
    #[test]
    fn the_help_short_circuit_is_a_success_and_a_real_error_is_not() {
        assert_eq!(
            format!("{:?}", exit_for_parse_error("backtest", "usage: …", HELP_SENTINEL)),
            format!("{:?}", ExitCode::SUCCESS),
            "--help is a success — a non-zero help breaks every `set -e` caller"
        );
        assert_eq!(
            format!("{:?}", exit_for_parse_error("backtest", "usage: …", "unknown argument: --x")),
            format!("{:?}", ExitCode::FAILURE),
            "…and a genuine usage error still fails"
        );
    }
}
