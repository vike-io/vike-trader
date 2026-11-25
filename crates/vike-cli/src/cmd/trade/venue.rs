//! `vike-cli trade venue` - the VENUE group: `balance|open-orders|positions <account> --node <observe addr>
//! [--json]` asks a running vike-tradehub what an account's VENUE says right now: the wallet balance (with the
//! allowance each exchange contract holds, where the venue has one), the venue's OWN list of the account's open
//! orders or the positions the venue says the account holds. `resume <venue> [--force]` releases the link
//! dead-man's halt of ONE venue ([`ResumeCommand`]).
//!
//! The verb comes first, as in every other group (the owner's stage-5 ruling 1): the pre-stage-5 order
//! `trade venue <account> balance`, and the group-less `trade resume-venue`, are refused naming the new
//! spelling (`crate::cmd::trade::plane`'s `renamed`), never read as it.
//!
//! # A read through the node, and nothing else
//!
//! The node holds no venue connection, so it forwards the question over the account's open trade lease to the
//! gatewayhub, whose bridge asks the venue with the account's own credentials (`Request::VenueRead`, feature
//! `venue-read`; `crates/vike-tradehub/src/server/venue_read.rs`'s module doc is the contract). It places,
//! moves and cancels nothing, folds nothing into any book and is no reconcile pass, so a `live` account is as
//! readable as a `demo` one and `flags.poly_reconcile` is not needed. That is the point of the verb: a
//! Polymarket wallet that was funded and traded before the node ran, whose reconcile is off, can be looked at.
//!
//! # Read-only, so a read role suffices
//!
//! The wire verb answers under either scope and [`vike_tradehub_client::venue_read`] always connects under
//! `Scope::Read`, exactly as `trade reconcile ls` and `trade status` do, so a read needs this box's
//! device key at a role that admits `Read`, resolved by the dispatcher.
//!
//! # What it does not say
//!
//! * `balance` is the account's COLLATERAL (Polymarket's pUSD, Hyperliquid's account value, Deribit's cash in
//!   each currency that holds any, Bybit's unified-wallet `walletBalance` of each coin that holds any, and on a
//!   UTA 1.0 account its `CONTRACT` wallet's, each named `<COIN> (CONTRACT wallet)`). It is no position read: a
//!   held outcome-token position appears in neither `balance` nor `open-orders`.
//! * `positions` is the VENUE's list of what the wallet holds (Polymarket's public data-api, by the funder
//!   address; Deribit's own list, a short with a negative figure), not the node's own: `trade position ls` prints
//!   what the node's ENGINE holds, and a holding bought before the node ran, or outside it, is in the first and
//!   not in the second. It folds nothing into the engine and is not a reconcile pass. Each row says whether the
//!   market has RESOLVED (`REDEEMABLE`), which a losing outcome does too; `NOW` and `VALUE` say what a share
//!   pays. Only Polymarket and Deribit answer.
//! * `open-orders` is account-wide at the venues that answer it (Polymarket, Hyperliquid, Deribit, IG, Binance, Bybit,
//!   OKX, cTrader, OANDA, Alpaca, Dukascopy, IBKR on the socket backend and FXCM; IG lists every epic's working orders
//!   on the account's running order session, which opens no login of its own; Binance every book's, spot, USDⓈ-M with
//!   its Algo Service stops, and COIN-M, so `binance` and `binance/PERP` list the same orders; Bybit every category's;
//!   OKX every instrument's with its resting stops and trailing orders; Dukascopy, IBKR and FXCM on the account's
//!   RUNNING sidecar or reconcile session). A size is the venue's own unit (contracts on an OKX swap, dollars on an
//!   inverse contract). Each row names its KIND and a trigger kind's TRIGGER level where the venue's list states them
//!   (`-` where it does not: Polymarket states neither, FXCM no PRICE or TRIGGER); a STOP's level is under TRIGGER, not
//!   PRICE, which is a resting limit's. Every other
//!   venue's bridge says `not offered by <venue>` and that is an ERROR here (exit `1`), never an empty list: an
//!   empty list is an answer ("nothing rests"), so a read that could not be made must not look like one.
//! * Two reads of one account and kind within a couple of seconds, or two reads of one venue at once, are
//!   refused by the gatewayhub in words (it protects the venue's request budget); retry as it says.
//! * The account must be RUNNING on that node (`trade health` says why one is not): the read rides the lease
//!   its start opened, and the node opens none for a read.
//!
//! # Exit codes
//!
//! `0` an answer; `2` a refused command line (nothing was sent); `1` the node ANSWERED and said no (not
//! offered, not running, asked too soon, a node too old for the verb, a key that does not verify);
//! `3` the node could not be reached at all (`status::failure_exit` is the one ladder both reads use).

use std::process::ExitCode;
use std::time::Duration;

use vike_hub_protocol::call::HubCallError;
use vike_model::accounts::account_keys::RESERVED_DEFAULT_LABEL;
use vike_model::{
    VenueRead, VenueReadAllowance, VenueReadKind, VenueReadOpenOrder, VenueReadPosition,
};

use super::describe::{opt_number, side_word};
use super::selector::{self, Book};
use super::status::failure_exit;
use crate::cmd::args;
use crate::exit::CliError;
use vike_client::hubkeys::HubKeyring;
use vike_client::verbs;

/// The command name every message prints.
pub(crate) const COMMAND: &str = "trade venue";

/// The program name a refusal of this verb's own rule prints under.
const BIN: &str = "vike-cli trade venue";

/// The three reads' verbs: the old order's second word, too.
const READS: &[&str] = &["balance", "open-orders", "positions"];

// The `venue` group's verbs. A group needs a verb: a bare `trade venue` is refused by `clap`, and the
// roster in that refusal and in `--help` is `clap`'s own, derived from this enum.
#[derive(clap::Subcommand, Debug)]
pub(super) enum VenueVerb {
    #[command(
        about = "the account's balance at its venue, with each exchange contract's allowance",
        long_about = LONG_ABOUT
    )]
    Balance(ReadCommand),
    #[command(
        name = "open-orders",
        about = "the venue's OWN list of the account's open orders",
        long_about = LONG_ABOUT
    )]
    OpenOrders(ReadCommand),
    #[command(
        about = "the positions the VENUE says the account holds (not the node's own)",
        long_about = LONG_ABOUT
    )]
    Positions(ReadCommand),
    #[command(about = "release the link dead-man's halt of ONE venue, early", long_about = RESUME_PROSE)]
    Resume(ResumeCommand),
    // ⚠ Any other word is answered BY NAME: the pre-stage-5 order `<account> <read>` with its new
    // spelling ([`other_refusal`]), anything else with the group's roster.
    #[command(external_subcommand)]
    Other(Vec<String>),
}

/// `--help`'s prose (the `usage:` line and the options table are `clap`'s).
pub(super) const LONG_ABOUT: &str = "\
Ask a running vike-tradehub what an account's VENUE says right now, through the account's own gatewayhub lease:

  balance      the wallet's balance, with the allowance each exchange contract holds where the venue states one
  open-orders  the venue's OWN list of the account's open (resting) orders
  positions    the positions the VENUE says the wallet holds (not `trade position ls`, which is the node's own)

ACCOUNT is the account, as in every trade verb: <venue> | <venue>/<LABEL> | <venue>/DEFAULT.

Read-only: it places, moves and cancels nothing, folds nothing into the node's books and is no reconcile pass, so \
it needs neither `flags.poly_reconcile` nor a `demo` tier, and it authenticates under the OBSERVE scope (the \
observe key alone). The account must be RUNNING on that node (`trade health` says why one is not).

Polymarket, Hyperliquid, Deribit and Bybit answer `balance` and `open-orders`, and Binance, OKX, IG, cTrader, OANDA, \
Alpaca, Dukascopy, IBKR (socket backend) and FXCM `open-orders` alone (every book's, every instrument's or every \
epic's, stops included; FXCM's rows carry no PRICE or TRIGGER), and Polymarket and Deribit \
answer `positions`; every other venue says `not offered by <venue>` and the command exits 1: an empty list is an answer (\"nothing \
rests\", \"the wallet holds nothing\"), and a read that could not be made is never shown as one. An `open-orders` \
row names its KIND and a stop's or take-profit's TRIGGER level where the venue states them (`-` where not); PRICE \
is a resting limit's. `balance` is the \
collateral (Polymarket pUSD; Deribit's cash in each currency that holds any). `positions` lists each holding with \
its market in words, the outcome, the shares, the average price paid, the venue's price and value now, and \
REDEEMABLE: the market has resolved, which a LOSING outcome is too (it pays nothing, so read NOW and VALUE). A \
Deribit row names the contract under TOKEN, long or short under OUTCOME, and the size in the venue's own unit under \
SHARES (dollars on `BTC-PERPETUAL`), negative for a short. The gatewayhub holds reads of one account to one every \
couple of seconds and one at a time per venue, and says so in words.

--node is the node's OBSERVE address. --json prints one document: the venue, the account, the kind and the \
node's answer verbatim under `read`.";

// A read's command line, as `clap` reads it. The node (`--node`, else `config.node_addr`) is judged after
// the parse ([`ReadCommand::into_args`]), so `--help` never has to supply it.
#[derive(clap::Args, Debug)]
pub(super) struct ReadCommand {
    #[arg(
        value_name = "ACCOUNT",
        help = "the account: <venue> | <venue>/<LABEL> | <venue>/DEFAULT"
    )]
    account: String,
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address [default: config.node_addr]"
    )]
    node: Option<String>,
    #[arg(long, overrides_with = "json", help = "print the answer as one JSON document")]
    json: bool,
}

/// The resolved command line. PURE: the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    node: String,
    book: Book,
    kind: VenueReadKind,
    json: bool,
}

impl ReadCommand {
    /// The rules after the parse: the account through the plane's one selector, and the node (`--node`, else
    /// `configured`, this box's `config.node_addr`).
    fn into_args(self, kind: VenueReadKind, configured: Option<&str>) -> Result<Args, CliError> {
        let book = selector::parse(&self.account)?;
        let node = super::node_or_refusal(self.node, configured).map_err(CliError::usage)?;
        Ok(Args { node, book, kind, json: self.json })
    }
}

/// What a word that is none of the group's verbs earns: the pre-stage-5 `<account> <read>` order is
/// refused naming the verb-first spelling, anything else with the roster. PURE.
fn other_refusal(words: &[String]) -> String {
    match words {
        [account, read, ..] if READS.contains(&read.to_ascii_lowercase().as_str()) => {
            super::plane::renamed(
                format!("trade venue {account} {read}"),
                format!("trade venue {} {account}", read.to_ascii_lowercase()),
            )
        }
        _ => format!(
            "unknown `trade venue` verb '{}' (balance | open-orders | positions | resume)",
            words.first().map_or("", String::as_str)
        ),
    }
}

/// The account as the wire spells it: `DEFAULT` for the unlabelled one, else the label.
fn wire_account(book: &Book) -> &str {
    book.label.text().unwrap_or(RESERVED_DEFAULT_LABEL)
}

/// Entry point [`crate::cmd::trade::run`] routes `trade venue …` to, with the verb `clap` read; `keys` is the
/// [`HubKeyring`] the dispatcher resolved. A read uses its observe half; `resume` is a WRITE, threaded the
/// guardrail's `policy_max_notional` and the `ack_wait` every write waits for the node's verdict with.
pub(super) fn run(
    verb: VenueVerb,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    let (cmd, kind) = match verb {
        VenueVerb::Balance(cmd) => (cmd, VenueReadKind::Balance),
        VenueVerb::OpenOrders(cmd) => (cmd, VenueReadKind::OpenOrders),
        VenueVerb::Positions(cmd) => (cmd, VenueReadKind::Positions),
        VenueVerb::Resume(cmd) => return run_resume(cmd, keys, policy_max_notional, ack_wait),
        VenueVerb::Other(words) => return args::refuse(BIN, other_refusal(&words)),
    };
    let a = match cmd.into_args(kind, keys.node_addr()) {
        Ok(a) => a,
        Err(e) => return args::refuse(BIN, e.message),
    };
    // The OBSERVE key specifically: a control key cannot authenticate a read.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND, &a.node));
        return ExitCode::FAILURE;
    };
    let account = wire_account(&a.book);
    match vike_tradehub_client::venue_read(a.node.as_str(), key, &a.book.venue, account, a.kind) {
        Ok(read) => {
            if a.json {
                println!("{}", json_body(&a.book.venue, account, &read));
            } else {
                let now = vike_model::now_ms();
                for line in human_lines(&selector::render(&a.book), &read, now) {
                    println!("{line}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            for line in failure_lines(&a.node, &e) {
                eprintln!("{line}");
            }
            failure_exit(&e).into()
        }
    }
}

/// `--help`'s prose for `resume`.
const RESUME_PROSE: &str = "\
Release the LINK DEAD-MAN's halt of ONE venue, early: the engines of VENUE go back to the trading \
state they held before the halt, and nothing else changes. That is what `trade resume` cannot do: it \
is node-wide, so it also clears every OTHER venue's halt and un-latches the drawdown latch.

VENUE is an exchange id (`bybit`), not an account: the halt is the whole exchange's, every account of it.

The node REFUSES while one of the venue's links is still down (a WARN in its log and a note in its \
recent events; the command reports `sent`, because the verdict is the core's), unless \
--force. A forced resume starts the venue's windows over, so a link that stays down trips again \
after a fresh grace. After a restart the node no longer remembers what the venue held before the \
halt (the overlay is not journaled), and takes the state from its OTHER venues instead; a node whose \
other venues are halted too is a node-wide halt, and `trade resume` lifts it.

It needs a device whose role admits control and a node with `tradehub_control` on, and a node \
that advertises `venue-resume`. `vike-cli trade status` is the READ.

--reason must be LAST - everything after it, to end of line, is taken verbatim as the recorded \
rationale. ALWAYS pass --yes when nothing is at the keyboard (see `trade halt --help`).";

/// `trade venue resume`'s command line: the venue, `--force`, then the shared one-shot write flags
/// (`--node` is the CONTROL address).
#[derive(clap::Args, Debug)]
pub(super) struct ResumeCommand {
    #[arg(value_name = "VENUE", help = "the exchange whose link-dead-man halt to release")]
    venue: String,
    #[arg(
        long,
        overrides_with = "force",
        help = "resume even though a link of the venue is still down (the node refuses otherwise)"
    )]
    force: bool,
    #[command(flatten)]
    wrap: super::oneshot::WrapperArgs,
}

/// `trade venue resume VENUE [--force]` through the plane's one write tail: preview, `confirm?
/// [y/N]` (`--yes` skips), send, and the exit rung the node's verdict earns.
///
/// ⚠ The venue is judged here, before a socket opens: an account (`bybit/ALT`, `bybit#ALT`) is
/// refused by name, because the halt is the exchange's and naming an account would read as a
/// narrower request than the node honours.
fn run_resume(
    cmd: ResumeCommand,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    let built = resume_verb(&cmd.venue, cmd.force);
    super::oneshot::run_verb(
        COMMAND,
        "resume",
        built,
        &cmd.wrap,
        keys,
        policy_max_notional,
        ack_wait,
    )
}

/// The verb `trade venue resume VENUE [--force]` resolves, or the usage refusal. PURE.
fn resume_verb(venue: &str, force: bool) -> Result<verbs::Verb, CliError> {
    let name = venue.trim();
    if name.is_empty() || name.contains(['/', '#']) {
        return Err(CliError::usage(format!(
            "`{venue}` is not a venue: the link dead-man's halt is the whole EXCHANGE's, so name \
             the exchange (`bybit`), not an account"
        )));
    }
    Ok(verbs::Verb::ResumeVenue { venue: name.to_string(), force })
}

/// The words for a failed ask, by the one kind of fact each variant carries
/// (`crate::cmd::trade::status::failure_exit` is the rung for the same error).
fn failure_lines(node: &str, err: &HubCallError) -> Vec<String> {
    match err {
        // The node ANSWERED: its sentence is the operator's (not offered, not running here, asked too
        // soon, the venue's own, not yet, another protocol), so it is printed as it came.
        HubCallError::Refused { .. } | HubCallError::NotNow { .. } | HubCallError::Protocol(_) => {
            vec![format!("vike-cli {COMMAND}: {err}")]
        }
        _ => vike_client::hubkeys::node_read_failure_lines(
            &format!("vike-cli {COMMAND}"),
            node,
            "the venue read",
            err,
        ),
    }
}

/// How long ago a venue stamp was, in the largest whole unit (`12s`, `3m`, `5h`, `2d`); `-` when the venue
/// stated none (`0`), and `now` for a stamp in the future (clock skew). PURE.
fn age_word(ts_ms: i64, now_ms: i64) -> String {
    if ts_ms <= 0 {
        return "-".to_string();
    }
    let secs = (now_ms - ts_ms).max(0) / 1000;
    match secs {
        0 => "now".to_string(),
        1..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn allowance_line(a: &VenueReadAllowance) -> String {
    let who = if a.name.is_empty() {
        a.spender.clone()
    } else if a.spender.is_empty() {
        a.name.clone()
    } else {
        format!("{} ({})", a.name, a.spender)
    };
    let amount = if a.unlimited { "unlimited".to_string() } else { a.amount.to_string() };
    format!("  allowance to {who}: {amount}")
}

fn order_row(o: &VenueReadOpenOrder, now_ms: i64) -> [String; 11] {
    [
        o.venue_order_id.clone(),
        side_word(o.side).to_string(),
        if o.order_type.is_empty() { "-".to_string() } else { o.order_type.clone() },
        opt_number(&o.price),
        opt_number(&o.trigger_price),
        vike_utility::number_text::format_number(o.qty),
        vike_utility::number_text::format_number(o.filled_qty),
        o.status.clone(),
        o.symbol.clone(),
        o.order_client_id.clone().unwrap_or_else(|| "-".to_string()),
        age_word(o.timestamp_ms, now_ms),
    ]
}

fn position_row(p: &VenueReadPosition) -> [String; 8] {
    let word = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
    [
        word(&p.title),
        word(&p.outcome),
        p.qty.to_string(),
        opt_number(&p.average_price),
        opt_number(&p.price),
        opt_number(&p.value),
        (if p.redeemable { "yes" } else { "no" }).to_string(),
        p.symbol.clone(),
    ]
}

/// `head` over `rows`, every column as wide as its widest cell, two spaces in from the margin. PURE.
fn aligned<const N: usize>(head: [&str; N], rows: &[[String; N]]) -> Vec<String> {
    let head = head.map(String::from);
    let width: Vec<usize> = (0..N)
        .map(|i| {
            rows.iter()
                .map(|r| r[i].chars().count())
                .chain(std::iter::once(head[i].chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let render = |cells: &[String; N]| {
        let mut line = String::from(" ");
        for (cell, w) in cells.iter().zip(&width) {
            line.push_str(&format!(" {cell:<w$}"));
        }
        line.trim_end().to_string()
    };
    std::iter::once(render(&head)).chain(rows.iter().map(render)).collect()
}

/// The human answer: one header naming the account and the read, then the figures or the table. PURE.
/// `now_ms` only dates a resting order's age.
fn human_lines(book: &str, read: &VenueRead, now_ms: i64) -> Vec<String> {
    match read {
        VenueRead::Balance(b) => {
            let asset = if b.asset.is_empty() { "(the venue names no asset)" } else { &b.asset };
            let mut lines = vec![format!("{book} balance: {} {asset}", b.balance)];
            lines.extend(b.other_assets.iter().map(|o| format!("  and {} {}", o.balance, o.asset)));
            if b.allowances.is_empty() {
                lines.push("  (the venue states no per-exchange allowance)".to_string());
            }
            lines.extend(b.allowances.iter().map(allowance_line));
            lines
        }
        VenueRead::OpenOrders { orders, truncated } => {
            let mut lines = vec![format!("{book} open orders at the venue: {}", orders.len())];
            if orders.is_empty() {
                lines.push("  (none rests at the venue)".to_string());
            } else {
                let head = [
                    "ORDER ID", "SIDE", "KIND", "PRICE", "TRIGGER", "QTY", "FILLED", "STATUS",
                    "SYMBOL", "OUR ID", "AGE",
                ];
                let rows: Vec<[String; 11]> = orders.iter().map(|o| order_row(o, now_ms)).collect();
                lines.extend(aligned(head, &rows));
            }
            if *truncated {
                lines.push(
                    "  (the venue has more open orders than this list holds: it is cut, not complete)"
                        .to_string(),
                );
            }
            lines
        }
        VenueRead::Positions { positions, truncated } => {
            let mut lines = vec![format!("{book} positions at the venue: {}", positions.len())];
            if positions.is_empty() {
                lines.push("  (the wallet holds nothing)".to_string());
            } else {
                let rows: Vec<[String; 8]> = positions.iter().map(position_row).collect();
                let head = [
                    "MARKET",
                    "OUTCOME",
                    "SHARES",
                    "AVG PAID",
                    "NOW",
                    "VALUE",
                    "REDEEMABLE",
                    "TOKEN",
                ];
                lines.extend(aligned(head, &rows));
                if positions.iter().any(|p| p.redeemable) {
                    lines.push(
                        "  REDEEMABLE yes = the market has resolved. A losing outcome says yes too and pays \
                         nothing: NOW and VALUE say what a share pays."
                            .to_string(),
                    );
                }
            }
            if *truncated {
                lines.push(
                    "  (the venue has more positions than this list holds: it is cut, not complete)"
                        .to_string(),
                );
            }
            lines
        }
    }
}

/// The JSON answer: one document naming what was asked, the node's payload verbatim under `read`. PURE.
fn json_body(venue: &str, account: &str, read: &VenueRead) -> String {
    serde_json::json!({
        "venue": venue,
        "account": account,
        "kind": read.kind().word(),
        "read": read,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::{VenueReadAssetBalance, VenueReadBalance};
    use vike_tradehub_client::test_support::canned_balance;

    /// `trade venue <argv>` through the plane's one parser: its verb, as `clap` read it.
    fn verb(argv: &[&str]) -> VenueVerb {
        let argv = std::iter::once("venue").chain(argv.iter().copied()).map(String::from);
        match super::super::parse(argv).expect("clap reads it").sub {
            Some(super::super::Sub::Venue { verb }) => verb,
            other => panic!("did not read as `trade venue`: {other:?}"),
        }
    }

    /// A read's line, through the parser and its rules, on a box with no `config.node_addr` row.
    fn parsed(argv: &[&str]) -> Result<Args, CliError> {
        parsed_with(argv, None)
    }

    /// [`parsed`] on a box whose `config.node_addr` row is `configured`.
    fn parsed_with(argv: &[&str], configured: Option<&str>) -> Result<Args, CliError> {
        match verb(argv) {
            VenueVerb::Balance(cmd) => cmd.into_args(VenueReadKind::Balance, configured),
            VenueVerb::OpenOrders(cmd) => cmd.into_args(VenueReadKind::OpenOrders, configured),
            VenueVerb::Positions(cmd) => cmd.into_args(VenueReadKind::Positions, configured),
            other => panic!("did not read as a venue read: {other:?}"),
        }
    }

    /// **Variant A** (P4 #13): `--node`, else this box's `config.node_addr` row, else the one refusal naming
    /// `vike-cli backend connect`, on the usage rung.
    #[test]
    fn the_node_is_the_flag_then_the_row_then_a_refusal_naming_backend_connect() {
        let row = Some("<host>:7979");
        let node = |argv: &[&str]| parsed_with(argv, row).expect("parses").node;
        assert_eq!(node(&["balance", "polymarket"]), "<host>:7979");
        assert_eq!(node(&["balance", "polymarket", "--node", "n:1"]), "n:1", "the flag wins");
        let refused = parsed(&["balance", "polymarket"]).expect_err("no flag, no row");
        assert_eq!(refused.exit, crate::exit::Exit::Usage);
        assert_eq!(refused.message, vike_client::refusals::NodeAddressRefusal::Unset.rendered());
    }

    /// `trade venue resume <argv>` through the plane's one parser.
    fn resume(argv: &[&str]) -> ResumeCommand {
        match verb(&std::iter::once("resume").chain(argv.iter().copied()).collect::<Vec<_>>()) {
            VenueVerb::Resume(c) => c,
            other => panic!("did not read as `trade venue resume`: {other:?}"),
        }
    }

    fn order() -> VenueReadOpenOrder {
        VenueReadOpenOrder {
            venue_order_id: "0xabc".into(),
            order_client_id: None,
            symbol: "6710259892".into(),
            side: 1,
            price: Some(0.2),
            trigger_price: None,
            order_type: String::new(),
            qty: 5.0,
            filled_qty: 0.0,
            status: "ACCEPTED".into(),
            timestamp_ms: 1_000_000,
        }
    }

    /// The verb, the account and `--node` are read; the default account travels as the reserved word and a
    /// labelled one as its label.
    #[test]
    fn it_reads_the_verb_the_account_and_the_node() {
        let a = parsed(&["balance", "polymarket", "--node", "127.0.0.1:17879"]).expect("parses");
        assert_eq!((a.book.venue.as_str(), wire_account(&a.book)), ("polymarket", "DEFAULT"));
        assert_eq!(
            (a.kind, a.node.as_str(), a.json),
            (VenueReadKind::Balance, "127.0.0.1:17879", false)
        );
        let b = parsed(&["open-orders", "--node", "n:1", "hyperliquid/TESTNET", "--json"])
            .expect("parses");
        assert_eq!((b.book.venue.as_str(), wire_account(&b.book)), ("hyperliquid", "TESTNET"));
        assert_eq!((b.kind, b.json), (VenueReadKind::OpenOrders, true));
        assert_eq!(
            parsed(&["positions", "polymarket", "--node", "n:1"]).unwrap().kind,
            VenueReadKind::Positions
        );
    }

    /// Every malformed line is the parser's or the rule's refusal, before a socket opens: a route key, a
    /// missing node, a missing account, a word that is no verb.
    #[test]
    fn a_malformed_line_is_refused_before_anything_is_sent() {
        let usage = |argv: &[&str]| parsed(argv).expect_err("refused").message;
        assert!(usage(&["balance", "polymarket#ALT", "--node", "n:1"]).contains("ROUTE KEY"));
        assert!(usage(&["balance", "polymarket"]).contains("--node"));
        let missing = ["venue", "balance", "--node", "n:1"].into_iter().map(String::from);
        assert!(super::super::parse(missing).is_err(), "the account is required");
        let VenueVerb::Other(words) = verb(&["fills", "polymarket", "--node", "n:1"]) else {
            panic!("an unknown verb is answered by name");
        };
        assert_eq!(
            other_refusal(&words),
            "unknown `trade venue` verb 'fills' (balance | open-orders | positions | resume)"
        );
    }

    /// The pre-stage-5 order, the account before the read, is refused by its id, naming the verb-first
    /// spelling; it is never read as it.
    #[test]
    fn the_account_first_order_is_refused_naming_the_verb_first_spelling() {
        for (read, now) in [
            ("balance", "balance"),
            ("open-orders", "open-orders"),
            ("positions", "positions"),
            ("BALANCE", "balance"),
        ] {
            let VenueVerb::Other(words) = verb(&["bybit/ALT", read, "--node", "n:1"]) else {
                panic!("`trade venue bybit/ALT {read}` read as a verb");
            };
            assert_eq!(
                other_refusal(&words),
                super::super::plane::renamed(
                    format!("trade venue bybit/ALT {read}"),
                    format!("trade venue {now} bybit/ALT")
                )
            );
            assert!(other_refusal(&words).starts_with("error [cli.trade.renamed]: "));
        }
    }

    /// The venue, `--force` in either position, and the shared write flags are read; the override
    /// is off unless spelled.
    #[test]
    fn resume_reads_the_venue_the_override_and_the_shared_write_flags() {
        let plain = resume(&["bybit", "--node", "127.0.0.1:1", "--yes"]);
        assert_eq!((plain.venue.as_str(), plain.force, plain.wrap.yes), ("bybit", false, true));
        let forced =
            resume(&["--force", "bybit", "--node", "127.0.0.1:1", "--reason", "link back"]);
        assert!(forced.force && forced.venue == "bybit");
        assert_eq!(forced.wrap.reason().as_deref(), Some("link back"));
        let bare = ["venue", "resume"].into_iter().map(String::from);
        assert!(super::super::parse(bare).is_err(), "the venue is required");
    }

    /// The verb lowers to the VENUE-scoped wire command and never to `SetTradingState` (the whole
    /// reason it exists), is a write, and previews with the override shouted.
    #[test]
    fn resume_lowers_to_the_venue_scoped_verb_and_previews_the_override() {
        use super::super::describe::describe_command;
        use vike_tradehub_client::wire::CommandWire;
        let verb = verbs::Verb::ResumeVenue { venue: "bybit".into(), force: true };
        assert!(verb.is_write());
        let wire = verb.to_wire_command().expect("a write verb lowers");
        assert_eq!(wire, CommandWire::ResumeVenue { venue: "bybit".into(), force: true });
        let forced = describe_command(&wire);
        assert!(forced.contains("bybit") && forced.contains("FORCE"), "{forced}");
        assert!(forced.contains("ONLY"), "the preview says it is one venue: {forced}");
        let plain =
            describe_command(&CommandWire::ResumeVenue { venue: "bybit".into(), force: false });
        assert!(!plain.contains("FORCE"), "{plain}");
    }

    /// `--help` says what the verb is for and what it does not do, and the REPL has no word for it.
    #[test]
    fn resume_help_states_the_scope_and_the_refusal() {
        let help = super::super::help_of(&["venue", "resume"]);
        assert!(help.contains("ONE venue") && help.contains("--force"), "{help}");
        assert!(help.contains("drawdown latch"), "it says what `resume` also does: {help}");
        assert!(help.contains("--node"), "the flags `clap` reads: {help}");
        assert!(
            super::super::parse_line("resume-venue bybit").is_err(),
            "no REPL word: it is a one-shot verb, like `reconcile confirm`"
        );
    }

    /// An account is not a venue: the halt is the whole exchange's, so the one-shot refuses `bybit/ALT`
    /// and `bybit#ALT` (and nothing) by name, as a usage error, before a socket opens.
    #[test]
    fn resume_refuses_an_account_as_a_venue() {
        for book in ["bybit/ALT", "bybit#ALT", "  "] {
            let err = resume_verb(book, false).expect_err(book);
            assert_eq!(err.exit, crate::exit::Exit::Usage, "{book:?}");
            assert!(err.message.contains("not a venue"), "{}", err.message);
        }
        assert_eq!(
            resume_verb(" bybit ", true).expect("a venue"),
            verbs::Verb::ResumeVenue { venue: "bybit".into(), force: true }
        );
    }

    /// The balance prints the figure and the asset, and each allowance by its contract's name and address.
    #[test]
    fn a_balance_prints_its_figure_and_its_allowances() {
        let lines = human_lines("polymarket", &canned_balance("0xE111"), 0);
        assert_eq!(lines[0], "polymarket balance: 20 pUSD");
        assert_eq!(lines[1], "  allowance to CTF exchange (0xE111): unlimited");
        let bare = VenueRead::Balance(VenueReadBalance {
            asset: String::new(),
            balance: 1.5,
            allowances: Vec::new(),
            other_assets: Vec::new(),
        });
        let lines = human_lines("hyperliquid/ALT", &bare, 0);
        assert!(lines[0].contains("(the venue names no asset)"), "{lines:?}");
        assert!(lines[1].contains("no per-exchange allowance"), "{lines:?}");
    }

    /// An account with a wallet per currency shows every one of them: a line for one wallet alone would
    /// read as the whole account.
    #[test]
    fn every_wallet_of_the_account_gets_a_line() {
        let wallets = VenueRead::Balance(VenueReadBalance {
            asset: "BTC".into(),
            balance: 1.5,
            allowances: Vec::new(),
            other_assets: vec![VenueReadAssetBalance { asset: "USDC".into(), balance: 250.0 }],
        });
        let lines = human_lines("deribit", &wallets, 0);
        assert_eq!(lines[..2], ["deribit balance: 1.5 BTC", "  and 250 USDC"]);
        assert!(json_body("deribit", "DEFAULT", &wallets).contains(r#""other_assets":[{"#));
    }

    /// An empty list says nothing rests (an ANSWER); a cut list says it is cut, in its own line.
    #[test]
    fn open_orders_print_a_table_say_none_rest_and_say_when_the_list_is_cut() {
        let none = VenueRead::OpenOrders { orders: Vec::new(), truncated: false };
        let lines = human_lines("polymarket", &none, 0);
        assert_eq!(
            lines,
            ["polymarket open orders at the venue: 0", "  (none rests at the venue)"]
        );

        let some = VenueRead::OpenOrders { orders: vec![order()], truncated: true };
        let lines = human_lines("polymarket", &some, 1_000_000 + 125_000);
        assert_eq!(lines[0], "polymarket open orders at the venue: 1");
        assert!(
            lines[1].contains("ORDER ID")
                && lines[1].contains("OUR ID")
                && lines[1].contains("AGE")
        );
        let row = &lines[2];
        for cell in ["0xabc", "buy", "0.2", "5", "ACCEPTED", "6710259892", "2m"] {
            assert!(row.contains(cell), "`{cell}` in {row}");
        }
        assert!(lines[3].contains("cut, not complete"), "{lines:?}");
    }

    /// A stop lists its kind and its trigger in their own columns, and a row that states neither shows a
    /// dash under both (never a zero, never a blank that shifts the columns).
    #[test]
    fn a_stop_shows_its_kind_and_its_trigger_and_a_row_without_them_dashes() {
        let stop = VenueReadOpenOrder {
            venue_order_id: "DIWO2".into(),
            symbol: "CS.D.GBPUSD.MINI.IP".into(),
            side: -1,
            price: None,
            trigger_price: Some(1.2412),
            order_type: "stop".into(),
            qty: 0.04,
            ..order()
        };
        let book = VenueRead::OpenOrders { orders: vec![stop, order()], truncated: false };
        let lines = human_lines("ig", &book, 0);
        let col = |word: &str| lines[1].find(word).unwrap_or_else(|| panic!("{word}: {lines:?}"));
        let (kind, price, trigger, qty) = (col("KIND"), col("PRICE"), col("TRIGGER"), col("QTY"));
        let cell =
            |line: &str, at: usize| line[at..].split_whitespace().next().unwrap().to_string();
        assert_eq!(
            [cell(&lines[2], kind), cell(&lines[2], price), cell(&lines[2], trigger)],
            ["stop", "-", "1.2412"]
        );
        assert_eq!(
            [cell(&lines[3], kind), cell(&lines[3], price), cell(&lines[3], trigger)],
            ["-", "0.2", "-"]
        );
        assert_eq!([cell(&lines[2], qty), cell(&lines[3], qty)], ["0.04", "5"]);
    }

    fn position(title: &str, qty: f64, price: Option<f64>, redeemable: bool) -> VenueReadPosition {
        VenueReadPosition {
            symbol: "1484359444963781501765067677755406951259550728919736807361564486189514136251"
                .into(),
            title: title.into(),
            outcome: "Down".into(),
            qty,
            average_price: Some(0.2099),
            price,
            value: price.map(|p| p * qty),
            redeemable,
        }
    }

    /// The positions table is in words: the market, the side, the shares, what was paid, what the venue says
    /// a share is worth and the holding, REDEEMABLE and the token; a note explains that a losing outcome is
    /// redeemable too, and only when a row says yes. An empty wallet is an ANSWER; a cut list says it is cut.
    #[test]
    fn positions_print_a_table_in_words_say_none_held_and_say_when_cut() {
        let none = VenueRead::Positions { positions: Vec::new(), truncated: false };
        assert_eq!(
            human_lines("polymarket", &none, 0),
            ["polymarket positions at the venue: 0", "  (the wallet holds nothing)"]
        );

        let held = VenueRead::Positions {
            positions: vec![
                position("Solana Up or Down - June 9, 3:40AM-3:45AM ET", 11.11, Some(0.0), true),
                position("BNB Up or Down - June 12, 4:30AM-4:45AM ET", 5.0, Some(1.0), true),
            ],
            truncated: true,
        };
        let lines = human_lines("polymarket", &held, 0);
        assert_eq!(lines[0], "polymarket positions at the venue: 2");
        for word in
            ["MARKET", "OUTCOME", "SHARES", "AVG PAID", "NOW", "VALUE", "REDEEMABLE", "TOKEN"]
        {
            assert!(lines[1].contains(word), "`{word}` in {}", lines[1]);
        }
        let losing = &lines[2];
        for cell in ["Solana Up or Down", "Down", "11.11", "0.2099", "yes", "1484359444963781501"] {
            assert!(losing.contains(cell), "`{cell}` in {losing}");
        }
        let winning = &lines[3];
        assert!(winning.contains("BNB Up or Down") && winning.contains(" 5 "), "{winning}");
        assert!(lines[4].contains("A losing outcome says yes too and pays nothing"), "{lines:?}");
        assert!(lines[5].contains("cut, not complete"), "{lines:?}");

        // The columns line up: every row starts its REDEEMABLE cell where the header does.
        let col = lines[1].find("REDEEMABLE").unwrap();
        assert_eq!(&lines[2][col..col + 3], "yes");
        assert_eq!(&lines[3][col..col + 3], "yes");
    }

    /// A figure the venue did not state is a dash, an open market says `no`, and with no redeemable row the
    /// explanatory note is not printed.
    #[test]
    fn a_missing_figure_is_a_dash_and_an_unresolved_market_says_no() {
        let open = VenueRead::Positions {
            positions: vec![VenueReadPosition {
                symbol: "7".into(),
                title: String::new(),
                outcome: String::new(),
                qty: 2.5,
                average_price: None,
                price: None,
                value: None,
                redeemable: false,
            }],
            truncated: false,
        };
        let lines = human_lines("polymarket", &open, 0);
        assert_eq!(lines.len(), 3, "no note without a redeemable row: {lines:?}");
        let row: Vec<&str> = lines[2].split_whitespace().collect();
        assert_eq!(row, ["-", "-", "2.5", "-", "-", "-", "no", "7"]);
    }

    /// A stamp is an age in the largest whole unit; none stated is a dash.
    #[test]
    fn an_age_is_the_largest_whole_unit() {
        assert_eq!(age_word(0, 5_000), "-");
        assert_eq!(age_word(5_000, 5_000), "now");
        assert_eq!(age_word(1_000, 13_000), "12s");
        assert_eq!(age_word(1_000, 1_000 + 180_000), "3m");
        assert_eq!(age_word(1_000, 1_000 + 5 * 3_600_000), "5h");
        assert_eq!(age_word(1_000, 1_000 + 2 * 86_400_000), "2d");
        assert_eq!(age_word(9_000, 1_000), "now", "a stamp from the future is not negative");
    }

    /// The JSON names what was asked and carries the node's payload verbatim under `read`.
    #[test]
    fn json_names_the_ask_and_carries_the_payload_verbatim() {
        let v: serde_json::Value =
            serde_json::from_str(&json_body("polymarket", "DEFAULT", &canned_balance("0xE111")))
                .expect("json");
        assert_eq!(
            (v["venue"].as_str(), v["account"].as_str(), v["kind"].as_str()),
            (Some("polymarket"), Some("DEFAULT"), Some("balance"))
        );
        assert_eq!(v["read"], serde_json::to_value(canned_balance("0xE111")).unwrap());
    }

    /// The words follow the variant: a node too old for the verb is told to upgrade; the node's own sentence
    /// is printed as it came; and the rung is the plane's one ladder (the node answered => 1, unreachable => 3).
    #[test]
    fn the_failure_words_and_rungs_follow_the_variant() {
        let old = HubCallError::NotOffered {
            feature: "venue-read".into(),
            text: "no venue-read capability".into(),
        };
        let lines = failure_lines("n:1", &old);
        // Whole, and naming THIS verb: the words are `vike_client::hubkeys::node_read_failure_lines`,
        // shared with `trade status`, which says `the strategy verbs` instead, and with `vike-mcp`.
        assert_eq!(
            lines,
            [
                "vike-cli trade venue: no venue-read capability",
                "this is a fact about the node, not the request: upgrade the node at n:1 to a \
                 vike-tradehub build that serves the venue read; asking this node again cannot succeed",
            ]
        );
        let denied = HubCallError::AccessDenied("auth denied: bad mac".into());
        assert!(failure_lines("n:1", &denied)[0].starts_with("vike-cli trade venue"));
        let refused = HubCallError::Refused {
            id: None,
            text: "tradehub venue read error: not offered by aster".into(),
        };
        let lines = failure_lines("n:1", &refused);
        assert_eq!(
            lines,
            ["vike-cli trade venue: tradehub venue read error: not offered by aster"]
        );
        let down = HubCallError::NotSent(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert_eq!(
            failure_lines("n:1", &down),
            ["vike-cli trade venue: cannot query the node at n:1: refused"]
        );
        for answered in [&old, &refused] {
            assert_eq!(failure_exit(answered), crate::exit::Exit::Failed);
        }
        assert_eq!(failure_exit(&down), crate::exit::Exit::Connect);
    }
}
