//! `vike-cli trade venue <book> balance|open-orders|positions --node <observe addr> [--json]` - ask a running
//! vike-tradehub what an account's VENUE says right now: the wallet balance (with the allowance each exchange
//! contract holds, where the venue has one), the venue's OWN list of the account's open orders or the
//! positions the venue says the account holds.
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
//! # Read-only, so the OBSERVE key suffices
//!
//! The wire verb answers under either scope and [`vike_tradehub_client::venue_read`] always connects under
//! `Scope::Read`, exactly as `trade recon ls` and `trade status` do, so this command needs the observe key
//! alone, resolved by the dispatcher (process environment, then the hub-key store the daemon reads). A
//! control key cannot substitute: the node verifies each scope against its own key.
//!
//! # What it does not say
//!
//! * `balance` is the account's COLLATERAL (Polymarket's pUSD, Hyperliquid's account value). It is no
//!   position read: a held outcome-token position appears in neither `balance` nor `open-orders`.
//! * `positions` is the VENUE's list of what the wallet holds (Polymarket's public data-api, by the funder
//!   address), not the node's own: `trade positions` prints what the node's ENGINE holds, and a holding bought
//!   before the node ran, or outside it, is in the first and not in the second. It folds nothing into the
//!   engine and is not a reconcile pass. Each row says whether the market has RESOLVED (`REDEEMABLE`), which a
//!   losing outcome does too; `NOW` and `VALUE` say what a share pays. Only Polymarket answers.
//! * `open-orders` is account-wide at the venues that answer it (Polymarket and Hyperliquid). Every other
//!   venue's bridge says `not offered by <venue>` and that is an ERROR here (exit `1`), never an empty list: an
//!   empty list is an answer ("nothing rests"), so a read that could not be made must not look like one.
//! * Two reads of one account and kind within a couple of seconds, or two reads of one venue at once, are
//!   refused by the gatewayhub in words (it protects the venue's request budget); retry as it says.
//! * The account must be RUNNING on that node (`trade doctor` says why one is not): the read rides the lease
//!   its start opened, and the node opens none for a read.
//!
//! # Exit codes
//!
//! `0` an answer; `2` a refused command line (nothing was sent); `1` the node ANSWERED and said no (not
//! offered, not running, asked too soon, a node too old for the verb, a key that does not verify);
//! `3` the node could not be reached at all (`status::failure_exit` is the one ladder both reads use).

use std::io;
use std::process::ExitCode;

use vike_model::accounts::account_keys::RESERVED_DEFAULT_LABEL;
use vike_tradehub_client::wire::{
    WireVenueAllowance, WireVenueOpenOrder, WireVenuePosition, WireVenueRead, WireVenueReadKind,
};

use super::describe::{opt_num, side_word};
use super::selector::{self, Book};
use super::status::failure_exit;
use crate::cmd::args;
use crate::cmd::hubkeys::{self, HubKeyring};
use crate::cmd::verbs;
use crate::exit::CliError;

/// The command name every message prints.
pub(crate) const COMMAND: &str = "trade venue";

/// The program name a refusal of this verb's own rule prints under.
const BIN: &str = "vike-cli trade venue";

/// `--help`'s prose (the `usage:` line and the options table are `clap`'s).
pub(super) const LONG_ABOUT: &str = "\
Ask a running vike-tradehub what an account's VENUE says right now, through the account's own gatewayhub lease:

  balance      the wallet's balance, with the allowance each exchange contract holds where the venue states one
  open-orders  the venue's OWN list of the account's open (resting) orders
  positions    the positions the VENUE says the wallet holds (not `trade positions`, which is the node's own)

BOOK is the account, as in every trade verb: <venue> | <venue>/<LABEL> | <venue>/DEFAULT.

Read-only: it places, moves and cancels nothing, folds nothing into the node's books and is no reconcile pass, so \
it needs neither `flags.poly_reconcile` nor a `demo` tier, and it authenticates under the OBSERVE scope (the \
observe key alone). The account must be RUNNING on that node (`trade doctor` says why one is not).

Polymarket and Hyperliquid answer `balance` and `open-orders`, and only Polymarket answers `positions`; every other \
venue says `not offered by <venue>` and the command exits 1: an empty list is an answer (\"nothing rests\", \"the \
wallet holds nothing\"), and a read that could not be made is never shown as one. `balance` is the collateral \
(Polymarket pUSD). `positions` lists each holding with its market in words, the outcome, the shares, the average \
price paid, the venue's price and value now, and REDEEMABLE: the market has resolved, which a LOSING outcome is \
too (it pays nothing, so read NOW and VALUE). The gatewayhub holds reads of one account to one every couple of \
seconds and one at a time per venue, and says so in words.

--node is the node's OBSERVE address. --json prints one document: the venue, the account, the kind and the \
node's answer verbatim under `read`.";

// The `trade venue` command line, as `clap` reads it. `--node` is required and the two words are judged after
// the parse ([`VenueCmd::into_args`]), so `--help` never has to supply them.
#[derive(clap::Args, Debug)]
pub(super) struct VenueCmd {
    #[arg(value_name = "BOOK", help = "the account: <venue> | <venue>/<LABEL> | <venue>/DEFAULT")]
    book: String,
    #[arg(value_name = "READ", help = "balance | open-orders | positions")]
    read: String,
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address (required)"
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
    kind: WireVenueReadKind,
    json: bool,
}

impl VenueCmd {
    /// The rules after the parse: the book through the plane's one selector, the read word, and `--node`.
    fn into_args(self) -> Result<Args, CliError> {
        let book = selector::parse(&self.book)?;
        let kind = match self.read.trim().to_ascii_lowercase().as_str() {
            "balance" => WireVenueReadKind::Balance,
            "open-orders" => WireVenueReadKind::OpenOrders,
            "positions" => WireVenueReadKind::Positions,
            other => {
                return Err(CliError::usage(format!(
                    "`{other}` is not a read the venue can be asked for: expected balance | open-orders | positions"
                )));
            }
        };
        let node = self.node.ok_or_else(|| CliError::usage("--node <host:port> is required"))?;
        Ok(Args { node, book, kind, json: self.json })
    }
}

/// The account as the wire spells it: `DEFAULT` for the unlabelled one, else the label.
fn wire_account(book: &Book) -> &str {
    book.label.text().unwrap_or(RESERVED_DEFAULT_LABEL)
}

/// Entry point [`crate::cmd::trade::run`] routes `trade venue` to; `keys` is the [`HubKeyring`] the dispatcher
/// resolved - this READ verb uses only its observe half.
pub(super) fn run(cmd: VenueCmd, keys: &HubKeyring) -> ExitCode {
    let a = match cmd.into_args() {
        Ok(a) => a,
        Err(e) => return args::refuse(BIN, e.msg),
    };
    // The OBSERVE key specifically: a control key cannot authenticate a read.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND));
        return ExitCode::FAILURE;
    };
    let account = wire_account(&a.book);
    match vike_tradehub_client::venue_read(
        a.node.as_str(),
        key.as_bytes(),
        &a.book.venue,
        account,
        a.kind,
    ) {
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

/// The words for a failed ask, by the one kind of fact each `ErrorKind` carries
/// (`crate::cmd::trade::status::failure_exit` is the rung for the same match).
fn failure_lines(node: &str, err: &io::Error) -> Vec<String> {
    match err.kind() {
        io::ErrorKind::Unsupported => vec![
            format!("vike-cli {COMMAND}: {err}"),
            format!(
                "upgrade the node at {node} to a vike-tradehub build that serves the venue read, \
                 then re-run"
            ),
        ],
        io::ErrorKind::PermissionDenied => {
            hubkeys::observe_permission_denied_lines(COMMAND, node, err)
        }
        // The node ANSWERED and refused: its sentence is the operator's (not offered, not running here, asked
        // too soon, the venue's own), so it is printed as it came.
        io::ErrorKind::InvalidData => vec![format!("vike-cli {COMMAND}: {err}")],
        _ => vec![format!("vike-cli {COMMAND}: cannot query {node}: {err}")],
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

fn allowance_line(a: &WireVenueAllowance) -> String {
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

fn order_row(o: &WireVenueOpenOrder, now_ms: i64) -> [String; 9] {
    [
        o.venue_order_id.clone(),
        side_word(o.side).to_string(),
        opt_num(&o.price),
        verbs::fmt_num(o.qty),
        verbs::fmt_num(o.filled_qty),
        o.status.clone(),
        o.symbol.clone(),
        o.client_order_id.clone().unwrap_or_else(|| "-".to_string()),
        age_word(o.ts_ms, now_ms),
    ]
}

fn position_row(p: &WireVenuePosition) -> [String; 8] {
    let word = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
    [
        word(&p.title),
        word(&p.outcome),
        p.qty.to_string(),
        opt_num(&p.avg_price),
        opt_num(&p.price),
        opt_num(&p.value),
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
fn human_lines(book: &str, read: &WireVenueRead, now_ms: i64) -> Vec<String> {
    match read {
        WireVenueRead::Balance(b) => {
            let asset = if b.asset.is_empty() { "(the venue names no asset)" } else { &b.asset };
            let mut lines = vec![format!("{book} balance: {} {asset}", b.balance)];
            if b.allowances.is_empty() {
                lines.push("  (the venue states no per-exchange allowance)".to_string());
            }
            lines.extend(b.allowances.iter().map(allowance_line));
            lines
        }
        WireVenueRead::OpenOrders { orders, truncated } => {
            let mut lines = vec![format!("{book} open orders at the venue: {}", orders.len())];
            if orders.is_empty() {
                lines.push("  (none rests at the venue)".to_string());
            } else {
                let head = [
                    "ORDER ID", "SIDE", "PRICE", "QTY", "FILLED", "STATUS", "SYMBOL", "OUR ID",
                    "AGE",
                ]
                .map(String::from);
                let rows: Vec<[String; 9]> = orders.iter().map(|o| order_row(o, now_ms)).collect();
                let width: Vec<usize> = (0..9)
                    .map(|i| {
                        rows.iter()
                            .map(|r| r[i].chars().count())
                            .chain(std::iter::once(head[i].chars().count()))
                            .max()
                            .unwrap_or(0)
                    })
                    .collect();
                let render = |cells: &[String; 9]| {
                    let mut line = String::from(" ");
                    for (cell, w) in cells.iter().zip(&width) {
                        line.push_str(&format!(" {cell:<w$}"));
                    }
                    line.trim_end().to_string()
                };
                lines.push(render(&head));
                lines.extend(rows.iter().map(render));
            }
            if *truncated {
                lines.push(
                    "  (the venue has more open orders than this list holds: it is cut, not complete)"
                        .to_string(),
                );
            }
            lines
        }
        WireVenueRead::Positions { positions, truncated } => {
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
fn json_body(venue: &str, account: &str, read: &WireVenueRead) -> String {
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
    use vike_tradehub_client::wire::WireVenueBalance;

    fn parsed(argv: &[&str]) -> Result<Args, CliError> {
        let argv = std::iter::once("venue").chain(argv.iter().copied()).map(String::from);
        match super::super::parse(argv).expect("clap reads it").sub {
            Some(super::super::Sub::Venue(cmd)) => cmd.into_args(),
            other => panic!("did not read as `trade venue`: {other:?}"),
        }
    }

    fn balance() -> WireVenueRead {
        WireVenueRead::Balance(WireVenueBalance {
            asset: "pUSD".into(),
            balance: 20.0,
            allowances: vec![WireVenueAllowance {
                spender: "0xE111".into(),
                name: "CTF exchange".into(),
                amount: 0.0,
                unlimited: true,
            }],
        })
    }

    fn order() -> WireVenueOpenOrder {
        WireVenueOpenOrder {
            venue_order_id: "0xabc".into(),
            client_order_id: None,
            symbol: "6710259892".into(),
            side: 1,
            price: Some(0.2),
            qty: 5.0,
            filled_qty: 0.0,
            status: "ACCEPTED".into(),
            ts_ms: 1_000_000,
        }
    }

    /// The book, the read word and `--node` are read; the default account travels as the reserved word and a
    /// labelled one as its label.
    #[test]
    fn it_reads_the_book_the_read_and_the_node() {
        let a = parsed(&["polymarket", "balance", "--node", "127.0.0.1:7879"]).expect("parses");
        assert_eq!((a.book.venue.as_str(), wire_account(&a.book)), ("polymarket", "DEFAULT"));
        assert_eq!(
            (a.kind, a.node.as_str(), a.json),
            (WireVenueReadKind::Balance, "127.0.0.1:7879", false)
        );
        let b = parsed(&["--node", "n:1", "hyperliquid/TESTNET", "open-orders", "--json"])
            .expect("parses");
        assert_eq!((b.book.venue.as_str(), wire_account(&b.book)), ("hyperliquid", "TESTNET"));
        assert_eq!((b.kind, b.json), (WireVenueReadKind::OpenOrders, true));
        assert_eq!(
            parsed(&["polymarket/DEFAULT", "BALANCE", "--node", "n:1"]).unwrap().kind,
            WireVenueReadKind::Balance
        );
        assert_eq!(
            parsed(&["polymarket", "positions", "--node", "n:1"]).unwrap().kind,
            WireVenueReadKind::Positions
        );
    }

    /// Every malformed line is the parser's or the rule's refusal, before a socket opens: a route key, a word
    /// that is no read, a missing node, a missing read.
    #[test]
    fn a_malformed_line_is_refused_before_anything_is_sent() {
        let usage = |argv: &[&str]| parsed(argv).expect_err("refused").msg;
        assert!(usage(&["polymarket#ALT", "balance", "--node", "n:1"]).contains("ROUTE KEY"));
        assert!(
            usage(&["polymarket", "fills", "--node", "n:1"])
                .contains("balance | open-orders | positions")
        );
        assert!(usage(&["polymarket", "balance"]).contains("--node"));
        let missing = ["venue", "polymarket", "--node", "n:1"].into_iter().map(String::from);
        assert!(super::super::parse(missing).is_err(), "the read word is required");
    }

    /// The balance prints the figure and the asset, and each allowance by its contract's name and address.
    #[test]
    fn a_balance_prints_its_figure_and_its_allowances() {
        let lines = human_lines("polymarket", &balance(), 0);
        assert_eq!(lines[0], "polymarket balance: 20 pUSD");
        assert_eq!(lines[1], "  allowance to CTF exchange (0xE111): unlimited");
        let bare = WireVenueRead::Balance(WireVenueBalance {
            asset: String::new(),
            balance: 1.5,
            allowances: Vec::new(),
        });
        let lines = human_lines("hyperliquid/ALT", &bare, 0);
        assert!(lines[0].contains("(the venue names no asset)"), "{lines:?}");
        assert!(lines[1].contains("no per-exchange allowance"), "{lines:?}");
    }

    /// An empty list says nothing rests (an ANSWER); a cut list says it is cut, in its own line.
    #[test]
    fn open_orders_print_a_table_say_none_rest_and_say_when_the_list_is_cut() {
        let none = WireVenueRead::OpenOrders { orders: Vec::new(), truncated: false };
        let lines = human_lines("polymarket", &none, 0);
        assert_eq!(
            lines,
            ["polymarket open orders at the venue: 0", "  (none rests at the venue)"]
        );

        let some = WireVenueRead::OpenOrders { orders: vec![order()], truncated: true };
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

    fn position(title: &str, qty: f64, price: Option<f64>, redeemable: bool) -> WireVenuePosition {
        WireVenuePosition {
            symbol: "1484359444963781501765067677755406951259550728919736807361564486189514136251"
                .into(),
            title: title.into(),
            outcome: "Down".into(),
            qty,
            avg_price: Some(0.2099),
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
        let none = WireVenueRead::Positions { positions: Vec::new(), truncated: false };
        assert_eq!(
            human_lines("polymarket", &none, 0),
            ["polymarket positions at the venue: 0", "  (the wallet holds nothing)"]
        );

        let held = WireVenueRead::Positions {
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
        let open = WireVenueRead::Positions {
            positions: vec![WireVenuePosition {
                symbol: "7".into(),
                title: String::new(),
                outcome: String::new(),
                qty: 2.5,
                avg_price: None,
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
            serde_json::from_str(&json_body("polymarket", "DEFAULT", &balance())).expect("json");
        assert_eq!(
            (v["venue"].as_str(), v["account"].as_str(), v["kind"].as_str()),
            (Some("polymarket"), Some("DEFAULT"), Some("balance"))
        );
        assert_eq!(v["read"], serde_json::to_value(balance()).unwrap());
    }

    /// The words follow the kind: a node too old for the verb is told to upgrade; the node's own sentence is
    /// printed as it came; and the rung is the plane's one ladder (the node answered => 1, unreachable => 3).
    #[test]
    fn the_failure_words_and_rungs_follow_the_kind() {
        let old = io::Error::new(io::ErrorKind::Unsupported, "no venue-read capability");
        let lines = failure_lines("n:1", &old);
        assert!(lines[1].contains("upgrade the node at n:1"), "{lines:?}");
        let refused = io::Error::new(
            io::ErrorKind::InvalidData,
            "tradehub venue read error: not offered by binance",
        );
        let lines = failure_lines("n:1", &refused);
        assert_eq!(
            lines,
            ["vike-cli trade venue: tradehub venue read error: not offered by binance"]
        );
        let down = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        assert!(failure_lines("n:1", &down)[0].contains("cannot query n:1"));
        for answered in [&old, &refused] {
            assert_eq!(failure_exit(answered), crate::exit::Exit::Failed);
        }
        assert_eq!(failure_exit(&down), crate::exit::Exit::Connect);
    }
}
