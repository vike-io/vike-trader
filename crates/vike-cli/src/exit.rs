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
//! # `Exit::Venue` — LIVE since task 7 of the trade-CLI-plane
//!
//! [`Exit::Venue`] (`5`) is produced by `crates/vike-cli/src/cmd/trade/oneshot.rs`'s
//! `execute_write` — the one-shot WRITE engine behind `trade order submit`/`cancel`/`modify`/
//! `mass-cancel` and `trade position flatten`/`close-all` — when the ticket a command was sent under
//! resolves to `vike_tradehub_client::CommandOutcome::Refused`: the node or the venue accepted the
//! connection, read the frame, and said no. That is the property that separates it from every other
//! rung on this ladder: [`Exit::Refused`] is refused LOCALLY, before a byte left this machine;
//! [`Exit::Connect`] means the far side was never reached at all; `Exit::Venue` means the far side
//! spoke, and the answer was a refusal. A wrapper that retries a `3` and fixes a `2` should treat a
//! `5` as a DECISION about the command it sent, not a transport hiccup — resending it verbatim
//! cannot change the venue's or the node's mind.
//!
//! ⚠ **This section used to say the rung was RESERVED**, because "this crate's two order-write
//! surfaces (`crates/vike-cli/src/cmd/trade.rs`'s REPL, `crates/vike-cli/src/cmd/mcp.rs`'s JSON-RPC
//! server) each answer in-band rather than exiting the process on a per-order decision" — true the
//! day it was written, and false the moment a THIRD, non-interactive surface existed to be the
//! producer. `crates/vike-cli/tests/exit_codes.rs`'s case for this rung runs over a REAL paper node
//! genuinely refusing a command (`crates/vike-tradehub/src/server.rs`'s `venue_refusal`, over an
//! order addressed to a venue the node runs no engine for) — not a fabricated `Response::Error` a
//! mock would have to invent, for the same reason `an_unaddressable_book_is_four` insists on a real
//! refusal rather than asserting a rung nothing produces.
//!
//! # `Exit::Refused` (`4`) — LIVE since the READ side, and ONE write producer again
//!
//! ⚠ This heading said "and now TWO WRITE producers too" until stage 5 of
//! `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md` deleted one of
//! them on 2026-09-26; the correction below records which.
//!
//! Its first producer was not the order-write guardrail an early draft of this section anticipated
//! (`crates/vike-cli/src/cmd/verbs.rs`'s `guardrail_check`, still ADVISORY-only and not wired to a
//! rung) — it was a READ-side refusal: `crates/vike-cli/src/cmd/trade/selector.rs`'s
//! `refuse_an_unaddressable_book`, which `trade order ls` calls to refuse a book selector naming an
//! ACCOUNT the pushed snapshot's `WireOrderView` rows carry no field to check against, rather than
//! silently widening to every account of a venue.
//! `crates/vike-cli/tests/exit_codes.rs`'s `an_unaddressable_book_is_four` is its case.
//!
//! ⚠ **This section has said three things about the WRITE side.** It first said the write side
//! calls no refusal of its own, because `WireOrderRequest`/the account-scoped `WireCommand` variants
//! carry `account` and every running `vike-tradehub` advertises `FEATURE_ACCOUNT_ROUTING` — both
//! true, and one layer short of the node, whose `lower_command` then built a `vike_model::OrderRequest`
//! with no `account` field and routed a labelled book by venue alone, silently. It then named a
//! second write producer: a selector function, `refuse_an_unroutable_account`, called from the four
//! order/position write parsers on any labelled book. **That producer is DELETED** (stage 5, above):
//! the node now reads the account back, routes a labelled `submit` to the engine it names, and
//! refuses an account it does not hold at its own edge — which is [`Exit::Venue`], the far side
//! speaking, not this rung.
//!
//! What remains on this rung for writes is the producer this section named first:
//! `execute_write` folds the client's own capability refusal onto it
//! (`vike_tradehub_client::ControlRejected::UnsupportedByNode` — a command the connected node does
//! not advertise support for, refused CLIENT-side before anything is sent). Three things reach it:
//! a `strategy mount` naming an account against a node too old to decode that field; — since the
//! deletion, against EVERY node today — a `mass-cancel`/`flatten`/`close-all` naming a book, which
//! `crates/vike-tradehub-client/src/remote_control.rs`'s `required_feature` gates on
//! `FEATURE_ACCOUNT_SCOPED_REDUCE` until a node can confine such a verb to one account; and a
//! `submit` against a node that does not advertise `FEATURE_ACCOUNT_SCOPED_SUBMIT`, the string that
//! gate demands of a labelled submit because six released nodes advertise the older
//! `FEATURE_ACCOUNT_ROUTING` while discarding the account. (⚠ This said "Two things" and named
//! the node's edge as the whole of the submit case — "the node now reads the account back" above —
//! until a review measured those releases.) `crate::cmd::trade::oneshot`'s module doc carries both
//! order-plane cases, including why a BARE venue meets the same gate.
//! ⚠ A no-control-key / control-scope-refused / declined-confirm one-shot write is deliberately
//! **NOT** this rung — `execute_write`'s own doc argues why those stay [`Exit::Failed`], the same
//! classification `crate::cmd::trade::run_set_mode_once` already gives the identical pre-send
//! failures for the one-shot `halt`/`resume` verbs.
//!
//! # Two more rungs, and these ones are LIVE
//!
//! [`Exit::Breach`] and [`Exit::Empty`] belong to the JUDGING verbs — the ones whose product is
//! this number rather than their output
//! (`docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §7.1). They are not
//! reserved: `crates/vike-cli/src/cmd/runs/gate.rs` produces both and
//! `crates/vike-cli/tests/exit_codes.rs` asserts both over the shipped binary, which is the rule
//! this module states for every rung — a documented rung nothing emits reads as covered, which is
//! worse than an undocumented one.
//!
//! ⚠ **There are TWO such verbs now, one per plane, and this paragraph said "the backtest plane's"
//! until `data hist gate` landed.** `crates/vike-cli/src/cmd/data/gate.rs` judges whether a STORE
//! holds what a run needs, on exactly these two rungs and by exactly the same rule — a breach is a
//! command that WORKED, and "the gate checked nothing" may never share a number with "the gate
//! passed". `crates/vike-cli/tests/data_cli.rs` asserts its three outcomes over the shipped binary.
//! A third judging verb joins them the same way: produce the rung, assert it over the binary, and
//! name itself here.
//!
//! They subdivide what a caller used to get as `0` and `1` in the worst possible direction: a gate
//! that BREACHED and a gate that CHECKED NOTHING were both indistinguishable from a gate that
//! passed, because the verb did not exist and a script's only alternative was `jq` over a report.
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
//! `vike-cli backtest run --local` (or `data`) spawns it,
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
    /// Refused LOCALLY — by a ceiling, a client-side guardrail, a book selector naming an account a
    /// READ has no row field to check against
    /// (`crates/vike-cli/src/cmd/trade/selector.rs`'s `refuse_an_unaddressable_book`, its first live
    /// producer), or a capability the connected node does not advertise — which is how a
    /// risk-reducing verb naming a book (a mass-cancel, flatten or close-all) is refused today —
    /// before anything was sent in every case. Distinct from [`Exit::Venue`] because nothing left this machine. See this
    /// module's doc for two corrections: it used to say RESERVED, naming only the order-write
    /// guardrail as the site waiting to produce it, and it used to list a write-side account refusal
    /// that stage 5 of the order-payload design deleted.
    Refused = 4,
    /// The venue or the node accepted the request and REJECTED it: the far side spoke, and it said
    /// no. Produced by `crate::cmd::trade::oneshot::execute_write` when a sent command's ticket
    /// resolves to `vike_tradehub_client::CommandOutcome::Refused` — see this module's doc for why
    /// that is a different claim from [`Exit::Refused`] (nothing left this machine) and from
    /// [`Exit::Connect`] (the far side was never reached). This doc used to say RESERVED; see the
    /// module doc's correction.
    Venue = 5,
    /// A DECLARED THRESHOLD WAS BREACHED — the product of this crate's JUDGING verbs, one per
    /// plane (`vike-cli backtest gate` and `vike-cli data hist gate`), and the one rung on this
    /// ladder that is a RESULT rather than a failure. The verdict document goes to stdout in full;
    /// this number is its summary, so a CI step can branch without parsing it.
    ///
    /// ⚠ This opening line named `backtest gate` ALONE while there was one producer, and the ⚠
    /// paragraph below was swept when the second landed while this sentence was not — so the line
    /// a reader hits FIRST, in the module that declares itself the authority for the ladder, said
    /// one of two. A wrapper scoped from it (`case $? in 6)` reaching only the compute plane) would
    /// miss a `6` the data plane returns. The SET is the claim; it is named here and counted
    /// nowhere.
    ///
    /// ⚠ **It has no [`CliError`] constructor, deliberately.** Routing a breach through `CliError`
    /// would print one stderr line and discard the per-criterion document that is the whole point
    /// (§7.1 of `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`: "The verdict is
    /// a document naming every criterion that passed and failed, never a bare number"). Two
    /// producers are the `rung` functions — `crates/vike-cli/src/cmd/runs/gate.rs`'s for the
    /// compute plane and `crates/vike-cli/src/cmd/data/gate.rs`'s for the data plane — each of
    /// which collapses a rendered verdict onto this rung the way
    /// `crates/vike-cli/src/cmd/config_check.rs`'s `Report::failed` collapses findings. A third is
    /// shaped differently: **`config retired-env`** (a RETIRED variable is set — decision 0095)
    /// constructs this rung directly, with no `rung` function and no per-criterion document to
    /// collapse — the refusal `vike_config::refuse_removed_env` built is already printed in full,
    /// on stdout, and IS the verdict. (This said "its ONE producer" while there was one, then "the
    /// two `rung` functions" while there were two; the SET is the claim, and it is stated here
    /// rather than counted.)
    ///
    /// Distinct from [`Exit::Failed`] because a breach means the command WORKED: it evaluated every
    /// criterion it was given and the answer was no. A wrapper re-runs a `1` and escalates this.
    Breach = 6,
    /// NOTHING WAS EVALUATED — an empty slice, no coverage, zero trials, a run directory with no
    /// report in it, or a set of criteria that named no key the document carries.
    ///
    /// ⚠ It exists so that "the gate passed" and "the gate checked nothing" stop sharing a number.
    /// A green that means nothing ran is the failure mode every gate in this repository is built
    /// against, and a CI step cannot tell the two apart from `0`. Reached two ways, both correct:
    /// through a judging verb's `rung` when every criterion was unevaluated
    /// (`crates/vike-cli/src/cmd/runs/gate.rs`, `crates/vike-cli/src/cmd/data/gate.rs`), and
    /// through [`CliError::empty`] when there was nothing to judge at all — a run with no report,
    /// or a series spec the store matches nowhere.
    Empty = 7,
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

    /// Refused locally by a ceiling or a guardrail — see [`Exit::Refused`]. Called from
    /// `crates/vike-cli/src/cmd/trade/selector.rs`'s `refuse_an_unaddressable_book` (`trade order
    /// ls`'s consumer of it) and, since task 7, from `crate::cmd::trade::oneshot::execute_write`'s
    /// control-scope and node-capability refusal arms; this doc used to say CALLED FROM NOWHERE
    /// while it was reserved.
    pub fn refused(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Refused, msg: msg.into() }
    }

    /// The far side said no — see [`Exit::Venue`]. Called from
    /// `crate::cmd::trade::oneshot::execute_write`'s `CommandOutcome::Refused` arm, since task 7 of
    /// the trade-CLI-plane wired the one-shot order-write verbs; this doc used to say CALLED FROM
    /// NOWHERE while that surface did not exist yet.
    pub fn venue(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Venue, msg: msg.into() }
    }

    /// The ordinary run failure — see [`Exit::Failed`]. Spelled out where a reader would otherwise
    /// wonder whether a site was classified or merely forgotten.
    pub fn failed(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Failed, msg: msg.into() }
    }

    /// Nothing was evaluated — see [`Exit::Empty`]. Classify at the site that KNOWS what was
    /// missing, so the message can name it: which run had no report, or which criteria named keys
    /// the document does not carry.
    pub fn empty(msg: impl Into<String>) -> Self {
        Self { exit: Exit::Empty, msg: msg.into() }
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
        assert_eq!(Exit::Breach as u8, 6);
        assert_eq!(Exit::Empty as u8, 7);
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
        assert_eq!(CliError::empty("x").exit, Exit::Empty);
        assert_eq!(CliError::connect("cannot connect to datahub at 1.2.3.4:9").msg, {
            "cannot connect to datahub at 1.2.3.4:9"
        });
    }
}
