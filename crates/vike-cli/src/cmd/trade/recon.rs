//! `vike-cli trade recon` - the RECONCILE group: the operator's door to the divergences a node HOLDS.
//!
//! Under the default `quarantine` policy the reconcile engine fetches the venue's positions, cash
//! and orders, diffs them against the book and HOLDS what disagrees (decision 0043). Before this
//! group nothing could resolve a held row: a phantom position the venue had closed, or a cash move
//! the daemon could not explain, stayed in the book until a restart, and the account's exposure
//! ceiling kept counting it.
//!
//! - `ls --node <observe addr> [--json]` - every held divergence the node's OBSERVE snapshot carries
//!   (`CoreSnapshotWire::recon`), with the id `confirm` takes and one line saying what a confirm would DO
//!   to that kind. A read; needs the observe key.
//! - `confirm <id> --node <control addr> [--yes] [--json] [--reason <text>]` - send
//!   `CommandWire::ReconcileConfirm`; the node folds what that divergence proposed into the book of the
//!   account the row names (`Command::ConfirmReconcile`). A WRITE: it previews first, asks `confirm?
//!   [y/N]` unless `--yes`, and is audited with the control actor and the `--reason`.
//!
//! - `cancel <id> --node <control addr> [--yes] [--json] [--reason <text>]` - send
//!   `CommandWire::ReconcileCancel` (decision 0196, rule (b)): the node sends ONE cancel to the venue for
//!   the order a held `UnknownOrder` row names (`Command::CancelReconcileOrder`), on a DEMO account only.
//!   A WRITE with the same preview, `confirm? [y/N]`, `--yes` and `--reason` as `confirm`. It folds
//!   nothing and removes no row: the row clears at the next reconcile pass if the venue cancelled it.
//!
//! The reads and the writes take DIFFERENT addresses, exactly as `order ls` and `order submit` do: the
//! observe port for the read, the control port for the writes.
//!
//! There is no `--adopt`/`--drop` choice: a kind has one resolution (`vike_client::health::effect_of` is
//! the whole menu), and a divergence an operator disagrees with is simply not confirmed. An id is a
//! digest of the divergence's identity (account, kind, instrument), so it names the same divergence
//! after a restart; one the node no longer holds (confirmed, or cleared by a later pass) is refused.
//!
//! The preview line carries only the id (`RECON-CONFIRM #n`); the kind and the detail are printed by
//! `ls`, which is what the operator reads before confirming.

use std::process::ExitCode;
use std::time::Duration;

use vike_tradehub_client::wire::ReconcileAlertViewWire;

use crate::cmd::args;
use crate::cmd::trade::oneshot::{self, WrapperArgs};
use vike_client::health::effect_of;
use vike_client::hubkeys::HubKeyring;
use vike_client::verbs::Verb;

const COMMAND: &str = "trade recon";

/// The program name a refusal of this group's own rules prints under.
const BIN: &str = "vike-cli trade recon";

// The `recon` group's verbs. A group needs a verb: a bare `trade recon` is refused by `clap`, and
// the roster in that refusal and in `--help` is `clap`'s own, derived from this enum.
#[derive(clap::Subcommand, Debug)]
pub(super) enum ReconcileVerb {
    #[command(about = "list the reconcile divergences the node is HOLDING for an operator")]
    Ls(LsCommand),
    #[command(
        about = "resolve one held divergence by its id (a decision about the venue's real account)",
        long_about = CONFIRM_PROSE
    )]
    Confirm(ConfirmCommand),
    #[command(
        about = "cancel, at the venue, the ONE order a held UnknownOrder row names (DEMO accounts only)",
        long_about = CANCEL_PROSE
    )]
    Cancel(CancelCommand),
}

/// `ls`'s command line: the observe address and the output shape.
#[derive(clap::Args, Debug)]
pub(super) struct LsCommand {
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address (required)"
    )]
    node: Option<String>,
    #[arg(long, overrides_with = "json", help = "print the rows as one JSON array")]
    json: bool,
}

/// `confirm`'s command line: the id, then the shared one-shot write flags.
#[derive(clap::Args, Debug)]
pub(super) struct ConfirmCommand {
    #[arg(value_name = "ID", help = "the id `trade recon ls` printed")]
    id: u64,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// `cancel`'s command line: the same shape as `confirm`'s, the id then the shared one-shot write flags.
#[derive(clap::Args, Debug)]
pub(super) struct CancelCommand {
    #[arg(value_name = "ID", help = "the id `trade recon ls` printed for an UnknownOrder row")]
    id: u64,
    #[command(flatten)]
    wrap: WrapperArgs,
}

const CANCEL_PROSE: &str = "\
ID is the number `trade recon ls` printed for an UnknownOrder row: an order the venue reports that \
this node's order registry does not hold (placed before a restart, or outside this node). This sends \
ONE cancel to the venue for the client id that row carries, through the account's own client, and \
nothing else: it folds nothing and removes no row. The row clears at the next reconcile pass if the \
venue really cancelled the order; if it is still listed after that pass, the venue did not.

Read `trade recon ls` first: the row can be another deployment's order, or the only protective stop of \
a position (cancelling it leaves the position without one).

DEMO accounts only. The node refuses, with a note in its recent events and a WARN in its log (the \
command reports `sent`, because the verdict comes after the node's answer), an account at any other \
tier or one it runs no engine for, a row of another kind, a row the account's latest pass no longer \
raises, a row that carries no client id (IG, FXCM and Polymarket report none, and neither does an \
order placed in Hyperliquid's own UI), an id its order registry holds (`trade order cancel` is that \
one's verb) and an id it does not hold.

A Hyperliquid row prints the venue's own hashed cloid (`0x` and 32 hex digits), and that is an id the \
venue can cancel by: the node sends it as printed, and the bridge cancels the one open order of the \
account carrying it. The bridge sends nothing, answering OrderCancelRejected in the node's recent \
events, when the open-order list cannot be read, no order or more than one carries that cloid, or \
its coin is not in the loaded symbology.

Whether a venue's adapter can act on an id this process did not place differs by \
venue and is not proven on all of them (decision 0196): look at the venue afterwards, and cancel \
there what is still resting. There is no REPL word and no MCP tool for it.

--node is the node's CONTROL address (a write), not the observe address `ls` takes.

--reason must be LAST - everything after it, to end of line, is taken verbatim as the recorded \
rationale.";

const CONFIRM_PROSE: &str = "\
ID is the number `trade recon ls` printed. It is a digest of the divergence's identity (account, \
kind, instrument), so a retried id after a restart reaches the same divergence or nothing. A row a \
later reconcile pass no longer raises is removed from the list and its id is refused.

The node folds what that held divergence proposed into the book of the account the row names. \
`trade recon ls` says, per kind, what that is. This is a decision about the venue's REAL account \
and the book follows it: read the venue first. In particular an OrphanLocalPosition says the venue \
never reported a position the book holds; confirming it DROPS the local position at its own \
average price (zero PnL, no fee), which is right when the venue closed it and wrong when the \
fetch simply did not ask about that symbol.

An id the node does not hold (already confirmed, cleared by a later pass, or not yet re-raised \
after a restart) folds nothing and is a note in the node's recent events plus a WARN in its log, \
not an error reply: the command reports `sent`, so run `trade recon ls` again to see what \
is left. The audit record (`recon_confirm`) names the id, the account, the kind and the \
description the node's snapshot held for it.

--node is the node's CONTROL address (a write), not the observe address `ls` takes.

--reason must be LAST - everything after it, to end of line, is taken verbatim as the recorded \
rationale.";

/// Entry point [`crate::cmd::trade::run`] routes `trade recon ...` to, with the verb `clap` read.
pub(super) fn run(
    verb: ReconcileVerb,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    match verb {
        ReconcileVerb::Ls(c) => run_ls(c, keys),
        ReconcileVerb::Confirm(c) => oneshot::run_verb(
            COMMAND,
            "confirm",
            Ok(Verb::ReconcileConfirm { id: c.id }),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        ReconcileVerb::Cancel(c) => oneshot::run_verb(
            COMMAND,
            "cancel",
            Ok(Verb::ReconcileCancel { id: c.id }),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
    }
}

fn run_ls(c: LsCommand, keys: &HubKeyring) -> ExitCode {
    let Some(node) = c.node else {
        return args::refuse(BIN, "--node <host:port> is required");
    };
    let snap = match crate::cmd::trade::observe_read(COMMAND, &node, keys) {
        Ok(snap) => snap,
        Err(exit) => return exit,
    };
    if c.json {
        println!("{}", reconcile_json(&snap.reconcile));
    } else {
        println!("{}", reconcile_table(&snap.reconcile));
    }
    ExitCode::SUCCESS
}

/// The human table: one header, then per row the id/account/kind line, what a confirm does, and the
/// detail. The detail is long and free text, so it gets its own line rather than a column.
fn reconcile_table(rows: &[ReconcileAlertViewWire]) -> String {
    let width = |header: &str, cell: fn(&ReconcileAlertViewWire) -> &str| {
        rows.iter().map(|r| cell(r).len()).chain(std::iter::once(header.len())).max().unwrap_or(0)
    };
    let w_account = width("ACCOUNT", |r| r.account.as_str());
    let w_kind = width("KIND", |r| r.kind.as_str());
    let mut out = format!("{:>6} {:<w_account$} {:<w_kind$} CONFIRM DOES", "ID", "ACCOUNT", "KIND");
    if rows.is_empty() {
        out.push_str(
            "\n  (none held; a node that predates the `recon-verbs` capability publishes none \
             either)",
        );
        return out;
    }
    for r in rows {
        out.push_str(&format!(
            "\n{:>6} {:<w_account$} {:<w_kind$} {}\n       {}",
            r.id,
            r.account,
            r.kind,
            effect_of(&r.kind, r.proposed_event_count),
            r.detail,
        ));
    }
    out
}

/// The flat JSON form: an array of self-describing rows, `effect` included so a script reads the
/// same words an operator does.
fn reconcile_json(rows: &[ReconcileAlertViewWire]) -> String {
    let rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "account": r.account,
                "kind": r.kind,
                "effect": effect_of(&r.kind, r.proposed_event_count),
                "detail": r.detail,
                "proposed_event_count": r.proposed_event_count,
            })
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, kind: &str) -> ReconcileAlertViewWire {
        ReconcileAlertViewWire {
            id,
            account: "bybit".to_string(),
            kind: kind.to_string(),
            detail: format!("detail of {id}"),
            proposed_event_count: 2,
        }
    }

    /// `confirm 9` reads as the verb `Verb::ReconcileConfirm { id: 9 }` that the wire lowers, through
    /// the plane's one parser.
    #[test]
    fn confirm_reads_its_id_through_the_plane_parser() {
        let argv = ["recon", "confirm", "9", "--node", "127.0.0.1:1", "--yes"]
            .into_iter()
            .map(String::from);
        match super::super::parse(argv).expect("parses").sub {
            Some(super::super::Sub::Recon { verb: ReconcileVerb::Confirm(c) }) => {
                assert_eq!(c.id, 9);
                assert!(c.wrap.yes);
            }
            other => panic!("`trade recon confirm 9` did not read as a recon confirm: {other:?}"),
        }
    }

    /// `cancel 9` reads as the verb `Verb::ReconcileCancel { id: 9 }`, through the plane's one parser, with
    /// the shared write flags.
    #[test]
    fn cancel_reads_its_id_through_the_plane_parser() {
        let argv = ["recon", "cancel", "9", "--node", "127.0.0.1:1", "--yes"]
            .into_iter()
            .map(String::from);
        match super::super::parse(argv).expect("parses").sub {
            Some(super::super::Sub::Recon { verb: ReconcileVerb::Cancel(c) }) => {
                assert_eq!(c.id, 9);
                assert!(c.wrap.yes);
            }
            other => panic!("`trade recon cancel 9` did not read as a recon cancel: {other:?}"),
        }
    }

    /// A missing or non-numeric id is the parser's refusal, before any socket opens.
    #[test]
    fn a_cancel_without_a_numeric_id_is_refused_by_the_parser() {
        for argv in [
            vec!["recon", "cancel", "--node", "127.0.0.1:1"],
            vec!["recon", "cancel", "nine", "--node", "127.0.0.1:1"],
        ] {
            assert!(super::super::parse(argv.into_iter().map(String::from)).is_err());
        }
    }

    /// The write the verb builds is `ReconcileCancel`, a write (so it previews and asks `y/N` like
    /// `confirm`), and its help says it is demo-only and has no other door.
    #[test]
    fn the_cancel_verb_lowers_to_the_wire_command_and_says_it_is_demo_only() {
        use vike_tradehub_client::wire::CommandWire;
        assert_eq!(
            Verb::ReconcileCancel { id: 4 }.to_wire_command(),
            Some(CommandWire::ReconcileCancel { id: 4 })
        );
        assert!(Verb::ReconcileCancel { id: 4 }.is_write());
        // Whitespace-flattened: clap wraps the long help at the terminal width.
        let help = super::super::help_of(&["recon", "cancel"])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(help.contains("DEMO accounts only"), "{help}");
        assert!(help.contains("no REPL word and no MCP tool"), "{help}");
    }

    /// A non-numeric id is the parser's refusal, before any socket opens.
    #[test]
    fn a_non_numeric_id_is_refused_by_the_parser() {
        let argv =
            ["recon", "confirm", "nine", "--node", "127.0.0.1:1"].into_iter().map(String::from);
        assert!(super::super::parse(argv).is_err());
    }

    /// The group has to be a verb away: a bare `trade recon` is refused.
    #[test]
    fn a_bare_group_is_refused() {
        let argv = ["recon"].into_iter().map(String::from);
        assert!(super::super::parse(argv).is_err());
    }

    /// The write the verb builds is `ReconcileConfirm`, on the control scope's vocabulary.
    #[test]
    fn the_verb_lowers_to_the_wire_command() {
        use vike_tradehub_client::wire::CommandWire;
        assert_eq!(
            Verb::ReconcileConfirm { id: 4 }.to_wire_command(),
            Some(CommandWire::ReconcileConfirm { id: 4 })
        );
        assert!(Verb::ReconcileConfirm { id: 4 }.is_write());
    }

    /// An empty list says so, and says an old node reads the same (the capability, never the
    /// emptiness, is what tells them apart).
    #[test]
    fn an_empty_list_is_marked_none_held() {
        let t = reconcile_table(&[]);
        assert!(t.contains("none held"), "{t}");
        assert!(t.starts_with("    ID ACCOUNT KIND"), "{t}");
    }

    /// Every row prints its id, kind, effect and detail; the orphan kind says it DROPS the local
    /// position, which is the sentence an operator has to read before confirming it.
    #[test]
    fn a_row_prints_its_id_effect_and_detail() {
        let t = reconcile_table(&[row(9, "OrphanLocalPosition"), row(10, "BalanceDrift")]);
        assert!(t.contains("     9 bybit"), "{t}");
        assert!(t.contains("OrphanLocalPosition"), "{t}");
        assert!(t.contains("drop the LOCAL position"), "{t}");
        assert!(t.contains("detail of 9"), "{t}");
        assert!(t.contains("adopt the VENUE's cash balance"), "{t}");
    }

    /// JSON carries the same words and a machine-readable id.
    #[test]
    fn json_rows_carry_id_kind_effect_and_detail() {
        let v: serde_json::Value =
            serde_json::from_str(&reconcile_json(&[row(9, "OrphanLocalPosition")])).expect("json");
        assert_eq!(v[0]["id"], 9);
        assert_eq!(v[0]["kind"], "OrphanLocalPosition");
        assert!(v[0]["effect"].as_str().is_some_and(|e| e.contains("drop the LOCAL position")));
        assert_eq!(v[0]["detail"], "detail of 9");
    }

    /// A drift with legs resizes to the venue's position; one with NONE (the flat, unpriced venue
    /// row) says it closes the local remainder at the local average price, never a venue price.
    #[test]
    fn a_position_drift_words_the_flat_unpriced_close_apart_from_the_resize() {
        assert!(effect_of("PositionDrift", 1).contains("resize the book to the VENUE's position"));
        let flat = effect_of("PositionDrift", 0);
        assert!(flat.contains("LOCAL remainder at its own average price"), "{flat}");
        assert!(flat.contains("never at a venue price"), "{flat}");
    }

    /// An external position with no legs books nothing on a confirm, so it may not read as an adoption.
    #[test]
    fn a_position_only_external_with_no_legs_does_not_read_as_an_adoption() {
        assert!(effect_of("PositionOnlyExternal", 1).contains("adopt the VENUE's position"));
        let none = effect_of("PositionOnlyExternal", 0);
        assert!(none.contains("books nothing"), "{none}");
        assert!(!none.contains("adopt"), "{none}");
    }

    /// An unlisted kind still says how many events a confirm folds, rather than going blank.
    #[test]
    fn an_unlisted_kind_names_its_event_count() {
        assert_eq!(effect_of("SomethingNew", 3), "fold 3 proposed event(s); the detail says which");
    }
}
