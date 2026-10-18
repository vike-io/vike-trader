//! `vike-cli trade recon` - the RECONCILE group: the operator's door to the divergences a node HOLDS.
//!
//! Under the default `quarantine` policy the reconcile engine fetches the venue's positions, cash
//! and orders, diffs them against the book and HOLDS what disagrees (decision 0043). Before this
//! group nothing could resolve a held row: a phantom position the venue had closed, or a cash move
//! the daemon could not explain, stayed in the book until a restart, and the account's exposure
//! ceiling kept counting it.
//!
//! - `ls --node <observe addr> [--json]` - every held divergence the node's OBSERVE snapshot carries
//!   (`WireSnapshot::recon`), with the id `confirm` takes and one line saying what a confirm would DO
//!   to that kind. A read; needs the observe key.
//! - `confirm <id> --node <control addr> [--yes] [--json] [--reason <text>]` - send
//!   `WireCommand::ReconConfirm`; the node folds what that divergence proposed into the book of the
//!   account the row names (`Command::ConfirmRecon`). A WRITE: it previews first, asks `confirm?
//!   [y/N]` unless `--yes`, and is audited with the control actor and the `--reason`.
//!
//! The two take DIFFERENT addresses, exactly as `order ls` and `order submit` do: the observe port
//! for the read, the control port for the write.
//!
//! There is no `--adopt`/`--drop` choice: a kind has one resolution (the `effect_of` table below is
//! the whole menu), and a divergence an operator disagrees with is simply not confirmed. The ids are
//! monotonic within one daemon run and restart with it, so read `ls` again after a restart.
//!
//! The preview line carries only the id (`RECON-CONFIRM #n`); the kind and the detail are printed by
//! `ls`, which is what the operator reads before confirming.

use std::process::ExitCode;
use std::time::Duration;

use vike_tradehub_client::wire::WireReconAlert;

use crate::cmd::args;
use crate::cmd::hubkeys::{self, HubKeyring};
use crate::cmd::trade::oneshot::{self, WrapperArgs};
use crate::cmd::verbs::Verb;
use crate::exit::Exit;

const COMMAND: &str = "trade recon";

/// The program name a refusal of this group's own rules prints under.
const BIN: &str = "vike-cli trade recon";

// The `recon` group's verbs. A group needs a verb: a bare `trade recon` is refused by `clap`, and
// the roster in that refusal and in `--help` is `clap`'s own, derived from this enum.
#[derive(clap::Subcommand, Debug)]
pub(super) enum ReconVerb {
    #[command(about = "list the reconcile divergences the node is HOLDING for an operator")]
    Ls(LsCmd),
    #[command(
        about = "resolve one held divergence by its id (a decision about the venue's real account)",
        long_about = CONFIRM_PROSE
    )]
    Confirm(ConfirmCmd),
}

/// `ls`'s command line: the observe address and the output shape.
#[derive(clap::Args, Debug)]
pub(super) struct LsCmd {
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
pub(super) struct ConfirmCmd {
    #[arg(value_name = "ID", help = "the id `trade recon ls` printed")]
    id: u64,
    #[command(flatten)]
    wrap: WrapperArgs,
}

const CONFIRM_PROSE: &str = "\
ID is the number `trade recon ls` printed. It is monotonic within one daemon run and restarts \
with the daemon, so read the list again after a restart.

The node folds what that held divergence proposed into the book of the account the row names. \
`trade recon ls` says, per kind, what that is. This is a decision about the venue's REAL account \
and the book follows it: read the venue first. In particular an OrphanLocalPosition says the venue \
never reported a position the book holds; confirming it DROPS the local position at its own \
average price (zero PnL, no fee), which is right when the venue closed it and wrong when the \
fetch simply did not ask about that symbol.

An id the node does not hold (already confirmed, or from before a restart) is a note in the \
node's recent events, not a refusal: the command still reports accepted, so run `trade recon ls` \
again to see what is left.

--node is the node's CONTROL address (a write), not the observe address `ls` takes.

--reason must be LAST - everything after it, to end of line, is taken verbatim as the recorded \
rationale.";

/// Entry point [`crate::cmd::trade::run`] routes `trade recon ...` to, with the verb `clap` read.
pub(super) fn run(
    verb: ReconVerb,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    match verb {
        ReconVerb::Ls(c) => run_ls(c, keys),
        ReconVerb::Confirm(c) => oneshot::run_verb(
            COMMAND,
            "confirm",
            Ok(Verb::ReconConfirm { id: c.id }),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
    }
}

fn run_ls(c: LsCmd, keys: &HubKeyring) -> ExitCode {
    let Some(node) = c.node else {
        return args::refuse(BIN, "--node <host:port> is required");
    };
    // The OBSERVE key specifically: this is a read, and a control key cannot authenticate one.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND));
        return ExitCode::FAILURE;
    };
    let snap = match crate::cmd::trade::connect_observe(
        &node,
        key.as_bytes(),
        keys.patience().observe_stall,
    ) {
        Ok((_handle, snap)) => snap,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            for line in hubkeys::observe_permission_denied_lines(COMMAND, &node, &e) {
                eprintln!("{line}");
            }
            return Exit::Failed.into();
        }
        Err(e) => {
            eprintln!("vike-cli {COMMAND}: cannot open observe connection to {node}: {e}");
            return Exit::Connect.into();
        }
    };
    // A frame with nothing built is a placeholder, not "nothing is held" - said on STDERR, so a
    // `--json` stdout stays one bare document.
    if let Some(line) = crate::cmd::trade::pre_fold_line(&snap) {
        eprintln!("{line}");
    }
    if c.json {
        println!("{}", recon_json(&snap.recon));
    } else {
        println!("{}", recon_table(&snap.recon));
    }
    ExitCode::SUCCESS
}

/// What a confirm DOES to a row of this kind, in one line. The whole menu: the node's fold
/// (`vike_core`'s `confirm_recon`) is the authority, this only words it.
fn effect_of(kind: &str, proposed_events: usize) -> String {
    match kind {
        "OrphanLocalPosition" => "drop the LOCAL position at its own average price (zero PnL, no \
                                  fee); right only if the venue really holds nothing"
            .to_string(),
        "PositionOnlyExternal" => {
            "adopt the VENUE's position into the book (cost basis = the venue's average price)"
                .to_string()
        }
        "PositionDrift" => {
            "resize the book to the VENUE's position, booking any close at the venue's average \
             price"
                .to_string()
        }
        "MissingFill" => "book the fill the book missed".to_string(),
        "BalanceDrift" => "adopt the VENUE's cash balance as the book's new anchor".to_string(),
        "OrphanLocalOrder" => "acknowledge only: folds and cancels nothing".to_string(),
        "JournalDivergence" => {
            "re-register the lost order(s) the detail names; acknowledge only if it names none"
                .to_string()
        }
        _ => format!("fold {proposed_events} proposed event(s); the detail says which"),
    }
}

/// The human table: one header, then per row the id/account/kind line, what a confirm does, and the
/// detail. The detail is long and free text, so it gets its own line rather than a column.
fn recon_table(rows: &[WireReconAlert]) -> String {
    let width = |header: &str, cell: fn(&WireReconAlert) -> &str| {
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
fn recon_json(rows: &[WireReconAlert]) -> String {
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

    fn row(id: u64, kind: &str) -> WireReconAlert {
        WireReconAlert {
            id,
            account: "bybit".to_string(),
            kind: kind.to_string(),
            detail: format!("detail of {id}"),
            proposed_event_count: 2,
        }
    }

    /// `confirm 9` reads as the verb `Verb::ReconConfirm { id: 9 }` that the wire lowers, through
    /// the plane's one parser.
    #[test]
    fn confirm_reads_its_id_through_the_plane_parser() {
        let argv = ["recon", "confirm", "9", "--node", "127.0.0.1:1", "--yes"]
            .into_iter()
            .map(String::from);
        match super::super::parse(argv).expect("parses").sub {
            Some(super::super::Sub::Recon { verb: ReconVerb::Confirm(c) }) => {
                assert_eq!(c.id, 9);
                assert!(c.wrap.yes);
            }
            other => panic!("`trade recon confirm 9` did not read as a recon confirm: {other:?}"),
        }
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

    /// The write the verb builds is `ReconConfirm`, on the control scope's vocabulary.
    #[test]
    fn the_verb_lowers_to_the_wire_command() {
        use vike_tradehub_client::wire::WireCommand;
        assert_eq!(
            Verb::ReconConfirm { id: 4 }.to_wire_command(),
            Some(WireCommand::ReconConfirm { id: 4 })
        );
        assert!(Verb::ReconConfirm { id: 4 }.is_write());
    }

    /// An empty list says so, and says an old node reads the same (the capability, never the
    /// emptiness, is what tells them apart).
    #[test]
    fn an_empty_list_is_marked_none_held() {
        let t = recon_table(&[]);
        assert!(t.contains("none held"), "{t}");
        assert!(t.starts_with("    ID ACCOUNT KIND"), "{t}");
    }

    /// Every row prints its id, kind, effect and detail; the orphan kind says it DROPS the local
    /// position, which is the sentence an operator has to read before confirming it.
    #[test]
    fn a_row_prints_its_id_effect_and_detail() {
        let t = recon_table(&[row(9, "OrphanLocalPosition"), row(10, "BalanceDrift")]);
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
            serde_json::from_str(&recon_json(&[row(9, "OrphanLocalPosition")])).expect("json");
        assert_eq!(v[0]["id"], 9);
        assert_eq!(v[0]["kind"], "OrphanLocalPosition");
        assert!(v[0]["effect"].as_str().is_some_and(|e| e.contains("drop the LOCAL position")));
        assert_eq!(v[0]["detail"], "detail of 9");
    }

    /// An unlisted kind still says how many events a confirm folds, rather than going blank.
    #[test]
    fn an_unlisted_kind_names_its_event_count() {
        assert_eq!(effect_of("SomethingNew", 3), "fold 3 proposed event(s); the detail says which");
    }
}
