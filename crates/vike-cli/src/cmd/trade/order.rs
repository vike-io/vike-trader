//! `vike-cli trade order` — the ORDER group's router. `ls` (the working-set read) and, since task 7
//! of the trade-CLI-plane, the four WRITE verbs (`submit` / `cancel` / `modify` / `mass-cancel`) are
//! wired; the node-lifecycle verbs stay elsewhere.
//!
//! # `ls`
//!
//! `vike-cli trade order ls [<book>] [--symbol SYM] --node <host:port> [--json]` — the working
//! order set read off the node's OBSERVE snapshot, optionally narrowed to one book's VENUE
//! (`crate::cmd::trade::selector`) and/or one symbol. Human (default): an aligned table whose
//! header survives an empty result. `--json`: the flat array `crate::cmd::trade::render` documents.
//!
//! ⚠ **A LABELLED book (`binance/ALT`) REFUSES rather than silently widening to every account of
//! the venue.** `crate::cmd::trade::selector::refuse_an_unaddressable_book` runs before a key
//! resolves or a connection opens, so the refusal costs no network round trip and prints nothing
//! ahead of itself — an operator who names an account the order read cannot attribute an order to
//! (a node that predates `WireOrderView::account` sends labelled orders without it) never sees a
//! table it did not actually narrow.
//!
//! Connects the OBSERVE half only — this is a read — through
//! [`crate::cmd::trade::connect_observe`], the same "connect, subscribe, wait up to
//! `FIRST_FRAME_WAIT` for a frame the node pushed" sequence the REPL's own reads use, so a one-shot
//! group verb and the REPL cannot disagree about how fresh a snapshot has to be before it is
//! trusted. A frame carrying nothing built yet is marked on stderr rather than read as an empty
//! working set (`crate::cmd::trade::pre_fold_line`).
//!
//! # The WRITE verbs, and the risk-direction law that shapes each one
//!
//! An act that ADDS risk names exactly one book; an act that REDUCES risk may name a venue, or
//! nothing (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md`
//! §4.5). That is why `submit`'s book is REQUIRED while `mass-cancel`'s is OPTIONAL, and it is not a
//! per-verb style choice — it is the same law `crate::cmd::trade::position`'s `flatten`/`close-all`
//! read off the other side of the ledger.
//!
//! - `submit <book> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]
//!   --node <host:port> [--yes] [--json] [--reason <text>]` — book REQUIRED, mirrors
//!   `crate::cmd::trade`'s own REPL grammar with the book moved to the front.
//! - `cancel <coid> --node … [--yes] [--json] [--reason <text>]` — no book: an order's own
//!   client-order-id already names the engine that owns it, so there is nothing left to address.
//! - `modify <coid> [--qty Q] [--price P] --node … […]` — same reasoning as `cancel`.
//! - `mass-cancel [<book>] [<symbol>] --node … […]` — book OPTIONAL: wider is safer for a
//!   risk-REDUCING act, so naming nothing reaches every account of every venue this node runs.
//!
//! `clap` reads the argv (the plane's one parser, `crate::cmd::trade`'s `Cli`: [`OrderVerb`] is this
//! group's enum); every one of the four then resolves to a [`crate::cmd::verbs::Verb`] through its
//! own rules (`submit_verb`/`cancel_verb`/`modify_verb`/`mass_cancel_verb`, all pure — no network)
//! and is executed by [`crate::cmd::trade::oneshot::execute_write`], which previews, gates on
//! `--yes`, sends, and folds the node's verdict onto an exit rung.
//!
//! ⚠ **A LABELLED book REACHES THE WIRE again on `submit`/`mass-cancel`, and this paragraph has now
//! said three different things.** It first read "a book's account reaches the wire directly …
//! these parsers do not re-implement that check" — true of the wire, false of the node, whose
//! `lower_command` built a `vike_model::OrderRequest` with no `account` field and dropped the label,
//! so a labelled book was routed by venue alone, silently. It then said both parsers REFUSE a
//! labelled book, through `crate::cmd::trade::selector`'s `refuse_an_unroutable_account`, called on
//! a `Some` book right after parsing it. Stage 5 of
//! `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md` DELETED that
//! function on 2026-09-26, once this branch carried main's node half, and each verb's labelled case
//! is now refused somewhere else rather than nowhere:
//!
//! - `submit` — refused CLIENT-side, before anything is enqueued, unless the node advertises
//!   `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_ACCOUNT_SCOPED_SUBMIT` (reported as
//!   [`crate::exit::Exit::Refused`]); a node that does reads the account back and routes to the
//!   engine it names, and refuses one it does not hold at its own edge before the Ack
//!   (`crates/vike-tradehub/src/server/refusal.rs`'s `account_refusal`), which this surface reports as
//!   [`crate::exit::Exit::Venue`]. ⚠ This bullet named only the node's edge until a review measured
//!   the release tags: six released nodes advertise the string the client gate then trusted
//!   (`account-routing`) while discarding the account, and against them it was the NODE that routed
//!   a labelled submit by venue alone. [`crate::cmd::trade::oneshot`]'s module doc carries it, and
//!   the empty-roster window in which even a new node Acks an unheld account.
//! - `mass-cancel` naming a book — refused CLIENT-side before anything is enqueued
//!   (`crates/vike-tradehub-client/src/remote_control.rs`'s `required_feature`) until a node
//!   advertises `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_ACCOUNT_SCOPED_REDUCE`,
//!   reported as [`crate::exit::Exit::Refused`]. ⚠ That includes a BARE venue: the book grammar
//!   names the default account POSITIVELY, so `mass-cancel binance` carries `"DEFAULT"` and meets
//!   the same gate — [`crate::cmd::trade::oneshot`]'s module doc carries the measurement.
//!
//! `refuse_an_unaddressable_book` (above, in `crate::cmd::trade::selector`) stays READ-side only
//! and is still never called from here.

use std::process::ExitCode;
use std::time::Duration;

use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

use crate::cmd::args;
use crate::cmd::hubkeys::{self, HubKeyring};
use crate::cmd::trade::oneshot::{self, WrapperArgs};
use crate::cmd::trade::render::{order_rows, rows_json, rows_table};
use crate::cmd::trade::selector;
use crate::cmd::verbs::{self, Verb};
use crate::exit::{CliError, Exit};

const COMMAND: &str = "trade order";

/// The program name a refusal of this group's own rules prints under.
const BIN: &str = "vike-cli trade order";

// The `order` group's verbs. A group needs a verb: a bare `trade order` is refused by `clap`, and
// the roster in that refusal and in `--help` is `clap`'s own — derived from this enum, so it
// cannot name a different set than the parser accepts.
//
// The four WRITE verbs flatten [`WrapperArgs`] (`--node`, `--yes`/`-y`, `--json`, `--reason`,
// recognised anywhere among the verb's own tokens); each resolves to a [`Verb`] through its own
// PURE rules ([`submit_verb`] and its siblings) once `clap` has read the argv.
#[derive(clap::Subcommand, Debug)]
pub(super) enum OrderVerb {
    #[command(about = "list the working order set from the node's latest snapshot")]
    Ls(selector::LsCmd),
    #[command(
        about = "place one order — the book is REQUIRED (an act that adds risk names exactly one)",
        long_about = submit_prose(),
        override_usage = "vike-cli trade order submit <BOOK> <SYMBOL> <buy|sell> <QTY> \
                          [@PRICE|@market] [OPTIONS]"
    )]
    Submit(SubmitCmd),
    #[command(
        about = "cancel one order by its client-order-id",
        long_about = CANCEL_PROSE,
        override_usage = "vike-cli trade order cancel <COID> [OPTIONS]"
    )]
    Cancel(CancelCmd),
    #[command(
        about = "change one resting order's qty and/or price",
        long_about = MODIFY_PROSE,
        override_usage = "vike-cli trade order modify <COID> [OPTIONS]"
    )]
    Modify(ModifyCmd),
    // ⚠ The `about` read "…, optionally scoped to one book (wider is safer)" until a review found
    // `--help` offering a narrowing the client gate refuses on every released node — see
    // `mass_cancel_prose`, which carries the whole sentence.
    #[command(
        about = "cancel every working order; a book needs a node serving account-scoped-reduce",
        long_about = mass_cancel_prose(),
        override_usage = "vike-cli trade order mass-cancel [BOOK] [SYMBOL] [OPTIONS]"
    )]
    MassCancel(MassCancelCmd),
}

/// Entry point [`crate::cmd::trade::run`] routes `trade order …` to, with the verb `clap` read.
/// `policy_max_notional` is threaded straight through to the WRITE verbs' advisory guardrail — see
/// [`oneshot::WriteCtx`] — and `ack_wait` (`config.node_ack_wait_ms`) with it, to the wait for the
/// node's verdict on the command a write verb sends.
pub(super) fn run(
    verb: OrderVerb,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    match verb {
        OrderVerb::Ls(cmd) => match cmd.into_args() {
            Ok(a) => run_ls(a, keys),
            Err(msg) => args::refuse(BIN, msg),
        },
        OrderVerb::Submit(c) => oneshot::run_verb(
            COMMAND,
            "submit",
            submit_verb(&c.tokens, c.reduce_only, c.coid.as_deref()),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::Cancel(c) => oneshot::run_verb(
            COMMAND,
            "cancel",
            cancel_verb(&c.ids),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::Modify(c) => oneshot::run_verb(
            COMMAND,
            "modify",
            modify_verb(&c.ids, c.qty.as_deref(), c.price.as_deref()),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::MassCancel(c) => oneshot::run_verb(
            COMMAND,
            "mass-cancel",
            mass_cancel_verb(&c.tokens),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
    }
}

fn run_ls(a: selector::LsArgs, keys: &HubKeyring) -> ExitCode {
    // A LABELLED book REFUSES here, before a key resolves or a connection opens — never silently
    // widened to every account of the venue. See `selector::refuse_an_unaddressable_book`'s doc for
    // why this crosses no network and prints nothing before answering.
    if let Some(book) = &a.book
        && let Err(e) = selector::refuse_an_unaddressable_book(book)
    {
        eprintln!("vike-cli {COMMAND}: {}", e.msg);
        return e.exit.into();
    }
    // The OBSERVE key specifically: this is a read, and a control key cannot authenticate one (the
    // node verifies each scope against its own key) — the same rule `trade status` states.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND));
        return ExitCode::FAILURE;
    };
    let snap = match crate::cmd::trade::connect_observe(
        &a.node,
        key.as_bytes(),
        keys.patience().observe_stall,
    ) {
        Ok((_handle, snap)) => snap,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            for line in hubkeys::observe_permission_denied_lines(COMMAND, &a.node, &e) {
                eprintln!("{line}");
            }
            return Exit::Failed.into();
        }
        Err(e) => {
            eprintln!("vike-cli {COMMAND}: cannot open observe connection to {}: {e}", a.node);
            return Exit::Connect.into();
        }
    };
    // A frame with nothing built is a placeholder, not an empty working set — said on STDERR, so a
    // `--json` stdout stays one bare document (`crate::cmd::trade::pre_fold_line`).
    if let Some(line) = crate::cmd::trade::pre_fold_line(&snap) {
        eprintln!("{line}");
    }
    let rows = order_rows(&snap, a.book.as_ref(), a.symbol.as_deref());
    if a.json {
        println!("{}", rows_json(&rows));
    } else {
        // Echo the parsed book back — human mode only, so `--json` stays a bare document a script
        // can pipe straight into `jq`. By this point a `Some` book is always the DEFAULT account —
        // a labelled one already refused above — so this can never claim a narrowing that did not
        // happen.
        if let Some(b) = &a.book {
            println!("book: {}", selector::render(b));
        }
        println!("{}", rows_table(&rows));
    }
    ExitCode::SUCCESS
}

// ---- the WRITE verbs (task 7) ----------------------------------------------------------------

// `submit`'s tokens and flags. The order-shape tokens are `<book> <symbol> <buy|sell> <qty>` and
// an optional `@<price>`/`@market`, in the order written; [`submit_verb`] sorts the `@` token out
// wherever it sits, so the `clap` positional is one list of raw tokens.
#[derive(clap::Args, Debug)]
pub(super) struct SubmitCmd {
    #[arg(value_name = "TOKEN", help = "<book> <symbol> <buy|sell> <qty> [@<price>|@market]")]
    tokens: Vec<String>,
    #[arg(long, help = "the order may only reduce an open position")]
    reduce_only: bool,
    #[arg(
        long,
        value_name = "ID",
        help = "pin the client order id (1..=32 alphanumeric characters)"
    )]
    coid: Option<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `cancel`'s one token.
#[derive(clap::Args, Debug)]
pub(super) struct CancelCmd {
    #[arg(value_name = "COID", help = "the client order id to cancel")]
    ids: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `modify`'s coid and its changes.
#[derive(clap::Args, Debug)]
pub(super) struct ModifyCmd {
    #[arg(value_name = "COID", help = "the client order id to change")]
    ids: Vec<String>,
    #[arg(long, value_name = "Q", help = "the new quantity")]
    qty: Option<String>,
    #[arg(long, value_name = "P", help = "the new price")]
    price: Option<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `mass-cancel`'s optional book and symbol.
#[derive(clap::Args, Debug)]
pub(super) struct MassCancelCmd {
    #[arg(value_name = "BOOK|SYMBOL", help = "[<book>] [<symbol>] — the book is OPTIONAL")]
    tokens: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// The shape line of a refusal for the wrong NUMBER of tokens (the verb's arity rule is
/// [`submit_verb`]'s own, not `clap`'s, because the `@price` token may sit among the positionals).
const SUBMIT_SHAPE: &str = "usage: vike-cli trade order submit <book> <symbol> <buy|sell> <qty> \
                            [@<price>|@market] [--reduce-only] [--coid <id>] --node <host:port> \
                            [--yes] [--json] [--reason <text to end of line>]";

/// `submit`'s long help, and the second half of its arity refusal ([`submit_usage`]). A function
/// rather than a `const`, so the forms it names come from [`selector::FORMS`] itself (stable Rust
/// has no `const`-time string concatenation without pulling in a crate to do it) — two of this
/// file's own tests exist because a hand-typed COPY of that list once fell out of step with it.
fn submit_prose() -> String {
    format!(
        "<book> is REQUIRED: an act that adds risk names exactly one venue account (a bare venue \
         names its DEFAULT account). The forms are: {}\n\n\
         Naming an account needs a node advertising `account-scoped-submit`; a node without it \
         may discard the account, so the submit is refused here before anything is sent (exit \
         4) — upgrade the node.\n\n\
         ⚠ --reason must be LAST — everything after it, to end of line, is taken verbatim as the \
         recorded rationale.",
        selector::FORMS
    )
}

fn submit_usage() -> String {
    format!("{SUBMIT_SHAPE}\n\n{}", submit_prose())
}

const CANCEL_SHAPE: &str = "usage: vike-cli trade order cancel <coid> --node <host:port> [--yes] \
                            [--json] [--reason <text to end of line>]";

const CANCEL_PROSE: &str = "No book: the client-order-id already names the engine that owns it.";

fn cancel_usage() -> String {
    format!("{CANCEL_SHAPE}\n\n{CANCEL_PROSE}")
}

const MODIFY_SHAPE: &str = "usage: vike-cli trade order modify <coid> [--qty Q] [--price P] \
                            --node <host:port> [--yes] [--json] [--reason <text to end of line>]";

const MODIFY_PROSE: &str = "No book, same reason as `cancel`. At least one of --qty/--price is \
                            required.";

fn modify_usage() -> String {
    format!("{MODIFY_SHAPE}\n\n{MODIFY_PROSE}")
}

const MASS_CANCEL_SHAPE: &str = "usage: vike-cli trade order mass-cancel [<book>] [<symbol>] \
                                 --node <host:port> [--yes] [--json] [--reason <text to end of \
                                 line>]";

/// See [`submit_prose`] for why this is a function rather than a `const`.
///
/// ⚠ The book sentence read only "<book> is OPTIONAL — wider is safer for a risk-REDUCING act;
/// naming nothing reaches every account of every venue this node runs" until a review found it
/// silent about the one thing an operator reaching for a book meets: ANY book names an account
/// (a bare venue names `DEFAULT`), and an account-scoped reduce is refused client-side against a
/// node that does not advertise `account-scoped-reduce` — every released node today. The sentence
/// is written against the CAPABILITY, so it stays true the day a node advertises it.
fn mass_cancel_prose() -> String {
    format!(
        "<book> is OPTIONAL — wider is safer for a risk-REDUCING act; naming nothing reaches \
         every account of every venue this node runs, and no capability gate refuses it. A book \
         names ONE account (a bare venue names its DEFAULT account, never every account of it) \
         and needs a node advertising `account-scoped-reduce`: against a node that does not, it \
         is refused here before anything is sent (exit 4). The forms are: {}",
        selector::FORMS
    )
}

fn mass_cancel_usage() -> String {
    format!("{MASS_CANCEL_SHAPE}\n\n{}", mass_cancel_prose())
}

/// `submit <book> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]` —
/// the order-shape rules ONLY (`--node`/`--yes`/`--json`/`--reason` are [`WrapperArgs`], read by
/// `clap`; so are `--reduce-only` and `--coid`, which arrive as `reduce_only` and `coid`).
/// `tokens` are the positionals in the order written, `@price` token included wherever it sat.
/// Mirrors `crate::cmd::trade`'s own `parse_submit` with the book moved to the front — the
/// risk-direction law makes it REQUIRED here. PURE: no network.
///
/// The coid is MINTED here (via [`verbs::fill_client_order_id`]) rather than left for the caller,
/// unlike the REPL's own parser: a one-shot invocation has no session-lived minter to defer to, and
/// the id shown at preview time must already be the id that gets sent.
///
/// An order that cannot be SIZED — a qty of zero, below or non-finite, a non-finite `@price` — is a
/// usage error here ([`verbs::unsizeable_submit_refusal`], the function the REPL's `parse_submit` and
/// the MCP `submit_order` tool call too), refused before the node is asked. A sizeable order over
/// every cap still parses: the preview's guardrail is advisory and the node is the gate.
fn submit_verb(tokens: &[String], reduce_only: bool, coid: Option<&str>) -> Result<Verb, CliError> {
    let usage = || CliError::usage(submit_usage());

    let mut positional: Vec<&str> = Vec::new();
    let mut price_tok: Option<&str> = None;
    for tok in tokens {
        if let Some(p) = tok.strip_prefix('@') {
            if price_tok.is_some() {
                return Err(CliError::usage("submit: more than one @price token".to_string()));
            }
            price_tok = Some(p);
        } else {
            positional.push(tok);
        }
    }
    let client_order_id = match coid {
        None => String::new(),
        Some(id) if vike_model::is_valid_crypto_coid(id) => id.to_string(),
        Some(id) => {
            return Err(CliError::usage(format!(
                "submit: --coid {id:?} is not a usable client_order_id — it must be {} (the \
                 strictest charset every venue accepts)",
                verbs::COID_CHARSET
            )));
        }
    };
    // <book> <symbol> <buy|sell> <qty> — exactly four positionals, book first. A count that would
    // have satisfied the OLD (book-less) grammar exactly (three positionals) is the single most
    // likely authoring mistake, so the same usage error covers it and names the book forms.
    if positional.len() != 4 {
        return Err(usage());
    }
    let book = selector::parse(positional[0])?;
    // ⚠ No local account refusal here any more. A labelled book goes onto the wire; the CLIENT's
    // capability gate keeps it from a node that does not advertise `account-scoped-submit`, and a
    // node that does routes it to the account it names or refuses one it does not hold at its own
    // edge — this module's doc records the stage-5 deletion of the refusal this line used to call.
    // (This comment named only the node until the review that found six released nodes
    // advertising the string the gate then trusted while discarding the account.)
    let symbol = positional[1];
    let side = super::side_arg(positional[2]).map_err(CliError::usage)?;
    let qty: f64 = positional[3]
        .parse()
        .map_err(|_| CliError::usage(format!("submit: not a qty: {:?}", positional[3])))?;
    let (order_type, price) = match price_tok {
        None | Some("market") => ("market", None),
        Some(p) => {
            let px: f64 =
                p.parse().map_err(|_| CliError::usage(format!("submit: not a price: @{p}")))?;
            ("limit", Some(px))
        }
    };

    let req = WireOrderRequest {
        client_order_id,
        venue: book.venue,
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: order_type.to_string(),
        price,
        trigger_price: None,
        reduce_only,
        // The book was ALWAYS given (it is required above), so this always names a POSITIVE
        // account — `AccountLabel`'s own `Display` is the wire spelling (`"DEFAULT"` for the
        // unlabelled account, the label text otherwise). See `oneshot`'s module doc.
        account: Some(book.label.to_string()),
    };
    // The REPL's refusal, by the same function and in this surface's own error type: an order that
    // cannot be SIZED is a usage error (exit 2) before a key resolves or a node is asked, so
    // `parse::<f64>` accepting `nan`/`inf` can no longer carry one into the preview.
    if let Some(why) = verbs::unsizeable_submit_refusal(&req) {
        return Err(CliError::usage(format!("submit: {why}")));
    }
    let mut minter = verbs::coid_minter();
    let WireCommand::Submit(req) =
        verbs::fill_client_order_id(WireCommand::Submit(req), &mut minter)
    else {
        unreachable!("a Submit went in, so a Submit comes out")
    };
    Ok(Verb::Submit(req))
}

/// `cancel <coid>` — no book, same grammar as the REPL's `cancel <coid>`. PURE.
fn cancel_verb(ids: &[String]) -> Result<Verb, CliError> {
    match ids {
        [id] => Ok(Verb::Cancel(id.clone())),
        _ => Err(CliError::usage(cancel_usage())),
    }
}

/// `modify <coid> [--qty Q] [--price P]` — no book, same reason as `cancel`. `qty` and `price` are
/// the RAW flag values, judged here so the refusal words stay this verb's own. Mirrors
/// `crate::cmd::trade`'s own `parse_modify`. PURE.
fn modify_verb(ids: &[String], qty: Option<&str>, price: Option<&str>) -> Result<Verb, CliError> {
    let coid = ids.first().ok_or_else(|| CliError::usage(modify_usage()))?;
    if let Some(other) = ids.get(1) {
        return Err(CliError::usage(format!(
            "modify: unexpected token {other:?}\n{}",
            modify_usage()
        )));
    }
    let new_qty = qty
        .map(|v| v.parse::<f64>().map_err(|_| CliError::usage(format!("modify: not a qty: {v:?}"))))
        .transpose()?;
    let new_price = price
        .map(|v| {
            v.parse::<f64>().map_err(|_| CliError::usage(format!("modify: not a price: {v:?}")))
        })
        .transpose()?;
    if new_qty.is_none() && new_price.is_none() {
        return Err(CliError::usage(
            "modify: nothing to change — pass --qty and/or --price".to_string(),
        ));
    }
    Ok(Verb::Modify { client_order_id: coid.clone(), new_qty, new_price })
}

/// `mass-cancel [<book>] [<symbol>]` — book OPTIONAL (wider is safer). Mirrors
/// `crate::cmd::trade`'s own REPL grammar. PURE.
fn mass_cancel_verb(tokens: &[String]) -> Result<Verb, CliError> {
    if tokens.len() > 2 {
        return Err(CliError::usage(mass_cancel_usage()));
    }
    let (venue, account) = match tokens.first() {
        None => (None, None),
        Some(book_tok) => {
            let book = selector::parse(book_tok)?;
            // ⚠ No local account refusal here any more (this module's doc records the stage-5
            // deletion). The account goes onto the wire, and a node that cannot confine a
            // risk-reducing verb to one account is kept from receiving it by the CLIENT's
            // capability gate, before anything is enqueued. Naming NO book at all is untouched —
            // the wider, risk-reducing spelling, carrying no account for any gate to see.
            (Some(book.venue), Some(book.label.to_string()))
        }
    };
    let symbol = tokens.get(1).cloned();
    Ok(Verb::MassCancel { venue, symbol, account })
}

// ---- test seams: the same argv tails the tests always wrote, read through the plane's parser ----

/// `word` and `tokens` read as `trade order <word> <tokens…>` by the plane's one `clap` parser.
#[cfg(test)]
fn read(word: &str, tokens: &[&str]) -> Result<OrderVerb, CliError> {
    let argv = ["order", word].into_iter().chain(tokens.iter().copied()).map(String::from);
    match super::parse(argv).map_err(|stop| CliError::usage(stop.to_string()))?.sub {
        Some(super::Sub::Order { verb }) => Ok(verb),
        other => panic!("`trade order {word}` did not read as an order verb: {other:?}"),
    }
}

/// [`submit_verb`] over an argv tail (no `trade order submit` words), through the plane's parser.
#[cfg(test)]
pub(crate) fn parse_submit_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("submit", tokens)? {
        OrderVerb::Submit(c) => submit_verb(&c.tokens, c.reduce_only, c.coid.as_deref()),
        other => unreachable!("`submit` read as {other:?}"),
    }
}

/// [`cancel_verb`] over an argv tail, through the plane's parser.
#[cfg(test)]
fn parse_cancel_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("cancel", tokens)? {
        OrderVerb::Cancel(c) => cancel_verb(&c.ids),
        other => unreachable!("`cancel` read as {other:?}"),
    }
}

/// [`modify_verb`] over an argv tail, through the plane's parser.
#[cfg(test)]
fn parse_modify_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("modify", tokens)? {
        OrderVerb::Modify(c) => modify_verb(&c.ids, c.qty.as_deref(), c.price.as_deref()),
        other => unreachable!("`modify` read as {other:?}"),
    }
}

/// [`mass_cancel_verb`] over an argv tail, through the plane's parser.
#[cfg(test)]
fn parse_mass_cancel_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("mass-cancel", tokens)? {
        OrderVerb::MassCancel(c) => mass_cancel_verb(&c.tokens),
        other => unreachable!("`mass-cancel` read as {other:?}"),
    }
}

#[path = "order_tests.rs"]
#[cfg(test)]
mod order_tests;
