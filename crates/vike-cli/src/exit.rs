//! The process exit ladder — the numbers `vike-cli` returns to whatever ran it.
//!
//! Scripts branch on these, so they are a PUBLIC INTERFACE: a rung's meaning may be added to, never
//! repurposed. Two rungs predate this module — `0` success and `1` "the command ran and failed" —
//! and keep their meanings exactly, so every script written against the old two-value behaviour
//! still reads correctly. Everything this module adds is a SUBDIVISION of what used to be `1`.
//!
//! # The distinction that earns the rest
//!
//! A caller FIXES a `2`, WAITS on a `3`, and treats a `1` as the ordinary "it ran and failed". One
//! number could not carry that: an unreachable datahub and a misspelled flag were the same answer,
//! so a wrapper either retried a typo forever or gave up on a tunnel that was one `ssh -L` away.
//! The agent case is the sharper one — an MCP client or a CI step has no operator to read the
//! stderr line that actually distinguishes them.
//!
//! # ⚠ Two rungs are RESERVED and NOTHING IN THIS CRATE PRODUCES THEM YET
//!
//! [`Exit::Refused`] (`4`) and [`Exit::Venue`] (`5`) are declared here, and no code path can reach
//! either today — their constructors are called from nowhere outside this module's own tests, and
//! `crates/vike-cli/tests/exit_codes.rs` therefore asserts `0`, `1`, `2` and `3` and no more.
//!
//! They are declared anyway, and said to be RESERVED rather than quietly documented as live,
//! because those are the two honest options and the third — describing them as behaviour — is the
//! defect this repository deleted `Policy::max_total_exposure` for: a script author reading
//! `case $? in 4) escalate;; esac` would get positive confirmation of a branch that cannot fire.
//! Reserving the numbers costs nothing and keeps them from being re-used for something else, which
//! is the one thing a public exit code may never survive.
//!
//! What each is WAITING FOR, so the wiring is a decision rather than an archaeology exercise: both
//! are ORDER-WRITE outcomes, and this crate's two order-write surfaces are
//! `crates/vike-cli/src/cmd/trade.rs` (an interactive REPL) and `crates/vike-cli/src/cmd/mcp.rs`
//! (a JSON-RPC server). Neither exits the process on a per-order decision — a REPL prints and
//! prompts again, a protocol answers in-band — so a PROCESS rung needs a NON-INTERACTIVE submit
//! verb first, which does not exist here. The day one lands, `4` is
//! `crates/vike-cli/src/cmd/verbs.rs`'s `guardrail_check` refusing before anything is sent (it is
//! ADVISORY today and says so), and `5` is a node or venue rejection coming back over the wire.
//!
//! # Why this is an enum and not four `ExitCode::from(n)` literals
//!
//! Because the classification travels: a failure is classified where it is KNOWN (at the socket, at
//! the parser, at the guardrail) and collapsed to a number at the `run` boundary of the verb, which
//! is several frames away. [`CliError`] is what carries it, and [`CmdResult`] is the shape every
//! converted `execute` returns.
//!
//! ⚠ The sibling `crates/vike-backtest/src/backtest_cli.rs` runs a two-rung version of this and
//! its numbers DO NOT mean the same things. Its `ExitCode::from(2)` covers a bad command line AND
//! a failed venue fetch, an unopenable store and a failed demo seed — so when
//! `vike-cli backtest --local` (or `data`, or `sweep --local`) spawns it,
//! `crates/vike-cli/src/cmd/engine.rs`'s `fold_status` deliberately does NOT re-publish that `2`
//! as this binary's `2`. It is stated here because "the two ladders agree" is the natural
//! assumption and acting on it inverts rung 2's whole promise; `fold_status`'s own doc carries the
//! argument and the condition that would change it.

use std::process::ExitCode;

/// One rung of the ladder. The discriminants ARE the exit codes — `#[repr(u8)]` plus the
/// [`From<Exit> for ExitCode`] impl below is the whole conversion, so there is no second table
/// mapping variants to numbers that could disagree with these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// The command did what was asked.
    Ok = 0,
    /// The command ran and failed — the pre-existing catch-all, and still where every failure that
    /// has not been deliberately classified lands (see [`From<String> for CliError`]).
    Failed = 1,
    /// The command line was wrong: an unknown verb, an unknown flag, a missing required flag, a bad
    /// value. Nothing was attempted, and re-running unchanged cannot succeed.
    Usage = 2,
    /// A service could not be reached: no datahub, no node, a refused or timed-out connection.
    /// The command line was fine and the same invocation may well work later — this is the one rung
    /// a wrapper should back off and retry on.
    Connect = 3,
    /// ⚠ **RESERVED — nothing produces this yet.** Refused LOCALLY by a ceiling — a policy limit
    /// or a client-side guardrail — before anything was sent. Distinct from [`Exit::Venue`]
    /// because nothing left this machine. See this module's doc for what has to exist first (a
    /// non-interactive submit verb) and which site becomes its producer.
    Refused = 4,
    /// ⚠ **RESERVED — nothing produces this yet.** The venue or the node accepted the request and
    /// rejected the order: the far side spoke, and it said no. Same module doc, same reason.
    Venue = 5,
}

impl From<Exit> for ExitCode {
    fn from(e: Exit) -> Self {
        ExitCode::from(e as u8)
    }
}

/// A failure that knows which rung it exits on: the message a verb prints to stderr, plus the
/// classification the process exits with.
///
/// The message is kept as a `String` rather than becoming a variant per failure: this crate has no
/// `anyhow` and no `thiserror` (its whole identity is adding no dependency), and the ONE thing a
/// caller does with a failure here is print it and exit. What was missing was never structure in
/// the text — it was the number beside it.
#[derive(Debug)]
pub struct CliError {
    /// The rung the process exits on.
    pub exit: Exit,
    /// What went wrong, in the same words the verb printed before this type existed.
    pub msg: String,
}

impl CliError {
    /// The command line was wrong — see [`Exit::Usage`].
    pub fn usage(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Usage, msg: msg.into() }
    }

    /// A service could not be reached — see [`Exit::Connect`]. Classify at the SOCKET, where the
    /// address is still in scope, so the message can name what was unreachable.
    pub fn connect(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Connect, msg: msg.into() }
    }

    /// Refused locally by a ceiling or a guardrail — see [`Exit::Refused`]. ⚠ CALLED FROM NOWHERE:
    /// the rung is reserved, not live (this module's doc says what would call it).
    pub fn refused(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Refused, msg: msg.into() }
    }

    /// The far side said no — see [`Exit::Venue`]. ⚠ CALLED FROM NOWHERE, for the same reason
    /// [`CliError::refused`] is.
    pub fn venue(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Venue, msg: msg.into() }
    }

    /// The ordinary run failure — see [`Exit::Failed`]. Spelled out where a reader would otherwise
    /// wonder whether a site was classified or merely forgotten.
    pub fn failed(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Failed, msg: msg.into() }
    }
}

/// ⚠ **This impl is what makes the ladder migratable one verb at a time.** Every failure inside
/// this crate is a `String` today; an unconverted `?` therefore still compiles, still prints the
/// same sentence, and still exits `1` — byte-identical behaviour. A verb is converted by
/// classifying the sites that deserve a rung and leaving the rest, rather than by a ten-file atomic
/// rewrite that has to be right everywhere at once.
///
/// It is also why `Exit::Failed` is not a hole: an unclassified site is not silently promoted to
/// something more specific, it stays exactly where it was.
impl From<String> for CliError {
    fn from(msg: String) -> Self {
        Self { exit: Exit::Failed, msg }
    }
}

/// The result shape a converted `execute` returns. Its `run` caller prints `e.msg` on stderr and
/// exits `e.exit`.
pub type CmdResult<T> = Result<T, CliError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The discriminants ARE the wire, so they are pinned as literals: a reordering of the enum
    /// would silently renumber a public interface, and nothing else in this crate would notice.
    #[test]
    fn the_rungs_are_the_numbers_scripts_branch_on() {
        assert_eq!(Exit::Ok as u8, 0);
        assert_eq!(Exit::Failed as u8, 1);
        assert_eq!(Exit::Usage as u8, 2);
        assert_eq!(Exit::Connect as u8, 3);
        assert_eq!(Exit::Refused as u8, 4);
        assert_eq!(Exit::Venue as u8, 5);
    }

    /// An UNCLASSIFIED `String` failure keeps the pre-existing rung — the property the incremental
    /// migration rests on. If this ever became anything but `Failed`, every not-yet-converted site
    /// in the crate would change behaviour at once.
    #[test]
    fn an_unclassified_string_error_still_exits_one() {
        let e: CliError = "cannot read profile run.toml: no such file".to_string().into();
        assert_eq!(e.exit, Exit::Failed);
        assert!(e.msg.starts_with("cannot read profile"), "and the message is carried verbatim");
    }

    /// Each constructor lands on its own rung, and the message is untouched — the constructors are
    /// classification, never rewording.
    #[test]
    fn each_constructor_carries_its_own_rung() {
        assert_eq!(CliError::usage("x").exit, Exit::Usage);
        assert_eq!(CliError::connect("x").exit, Exit::Connect);
        assert_eq!(CliError::refused("x").exit, Exit::Refused);
        assert_eq!(CliError::venue("x").exit, Exit::Venue);
        assert_eq!(CliError::failed("x").exit, Exit::Failed);
        assert_eq!(CliError::connect("cannot connect to datahub at 1.2.3.4:9").msg, {
            "cannot connect to datahub at 1.2.3.4:9"
        });
    }
}
