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
//! ahead of itself — an operator who names an account this wire cannot yet address never sees a
//! table it did not actually narrow.
//!
//! Connects the OBSERVE half only — this is a read — through
//! [`crate::cmd::trade::connect_observe`], the same "connect, subscribe, wait up to
//! `FIRST_FRAME_WAIT` for a real frame" sequence the REPL's own reads use, so a one-shot group verb
//! and the REPL cannot disagree about how fresh a snapshot has to be before it is trusted.
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
//! Every one of the four resolves to a [`crate::cmd::verbs::Verb`] through its own parser
//! (`parse_submit_args`/`parse_cancel_args`/`parse_modify_args`/`parse_mass_cancel_args`, all pure —
//! no network) and is executed by [`crate::cmd::trade::oneshot::execute_write`], which previews,
//! gates on `--yes`, sends, and folds the node's verdict onto an exit rung.
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
//!   (`crates/vike-tradehub/src/server.rs`'s `account_refusal`), which this surface reports as
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

use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::trade::oneshot::{self, WriteCtx};
use crate::cmd::trade::render::{order_rows, rows_json, rows_table};
use crate::cmd::trade::selector::{self, Book};
use crate::cmd::verbs::{self, Verb};
use crate::exit::{CliError, Exit};

/// The group's own verb roster: name, one-line description. [`usage`] and [`verb_names`] are both
/// DERIVED from this, so a verb cannot be listed in one place and missing from another.
pub(crate) const VERBS: &[(&str, &str)] = &[
    ("ls", "list the working order set from the node's latest snapshot"),
    ("submit", "place one order — the book is REQUIRED (an act that adds risk names exactly one)"),
    ("cancel", "cancel one order by its client-order-id"),
    ("modify", "change one resting order's qty and/or price"),
    // ⚠ This row read "…, optionally scoped to one book (wider is safer)" until a review found
    // `--help` offering a narrowing the client gate refuses on every released node — see
    // `mass_cancel_usage`, which carries the whole sentence.
    (
        "mass-cancel",
        "cancel every working order; a book needs a node serving account-scoped-reduce",
    ),
];

const COMMAND: &str = "trade order";

/// The group-level usage/help block, with the verb list rendered from [`VERBS`] rather than typed
/// out a second time. Printed both for `trade order --help` and, via [`exit_for_parse_error`],
/// alongside a usage error.
fn usage() -> String {
    let verbs = VERBS
        .iter()
        .map(|(name, desc)| format!("  {name:<12} {desc}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "usage: vike-cli trade order <verb> [options]\n\nverbs:\n{verbs}\n\n  -h, --help   this message"
    )
}

/// The verb names alone, `|`-joined — the roster half of the "needs a verb" / "unknown verb"
/// messages, derived from [`VERBS`] so it cannot name a different set than [`usage`] does.
fn verb_names() -> String {
    VERBS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(" | ")
}

/// One resolved READ/meta `order` sub-command — the WRITE verbs bypass this type entirely (see
/// [`run`]) because their result is a [`crate::cmd::verbs::Verb`], the SAME shared wire-vocabulary
/// type the REPL and the `mcp` tools already construct through; wrapping it a second time here
/// would be exactly the second construction site that type exists to prevent.
#[derive(Debug)]
enum OrderVerb {
    Ls(LsArgs),
}

/// Parse everything after the `order` word: the verb, then that verb's own flags. PURE — mirrors
/// `crate::cmd::data`'s "a group needs a verb" shape: no verb and an unknown verb are both usage
/// errors naming the roster; `-h`/`--help`/`help` short-circuit through [`help_requested`].
fn parse(mut args: impl Iterator<Item = String>) -> Result<OrderVerb, String> {
    let Some(verb) = args.next() else {
        return Err(format!("`trade order` needs a verb ({})", verb_names()));
    };
    match verb.as_str() {
        "-h" | "--help" | "help" => help_requested(),
        "ls" => Ok(OrderVerb::Ls(parse_ls(args)?)),
        other => Err(format!("unknown `trade order` verb '{other}' ({})", verb_names())),
    }
}

/// `ls`'s own parsed line.
#[derive(Debug)]
struct LsArgs {
    node: String,
    /// The optional positional book selector — narrows to one VENUE (see
    /// [`crate::cmd::trade::render::order_rows`] for why it cannot yet narrow to one ACCOUNT of
    /// that venue).
    book: Option<Book>,
    symbol: Option<String>,
    json: bool,
}

/// `ls`'s own grammar: an optional LEADING positional (the book selector — recognised because it is
/// the one token that does not start with `-`), then ordinary flags. PURE.
fn parse_ls(args: impl Iterator<Item = String>) -> Result<LsArgs, String> {
    let mut it = args.peekable();
    let book = match it.peek() {
        Some(tok) if !tok.starts_with('-') => {
            let raw = it.next().expect("just peeked Some");
            Some(selector::parse(&raw).map_err(|e| e.msg)?)
        }
        _ => None,
    };
    let mut node: Option<String> = None;
    let mut symbol: Option<String> = None;
    let mut json = false;
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--symbol" => symbol = Some(flags.value(&flag, inline)?),
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let node = node.ok_or("--node <host:port> is required")?;
    Ok(LsArgs { node, book, symbol, json })
}

/// Entry point [`crate::cmd::trade::run`] routes `trade order …` to. `args` is everything AFTER the
/// `order` word. `policy_max_notional` is threaded straight through to the WRITE verbs' advisory
/// guardrail — see [`oneshot::WriteCtx`].
///
/// The four WRITE verbs are claimed HERE, before [`parse`], because each resolves to a
/// [`crate::cmd::verbs::Verb`] and classifies its own failures onto an exit rung
/// ([`crate::exit::CliError`]) rather than the flat `String` [`parse`]'s READ/meta grammar uses —
/// the same reason `crate::cmd::trade::run` claims `status`/`halt`/`resume` before its own REPL
/// grammar.
pub(crate) fn run(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let mut args = args.peekable();
    match args.peek().map(String::as_str) {
        Some("submit") => {
            args.next();
            return run_submit(args, keys, policy_max_notional);
        }
        Some("cancel") => {
            args.next();
            return run_cancel(args, keys, policy_max_notional);
        }
        Some("modify") => {
            args.next();
            return run_modify(args, keys, policy_max_notional);
        }
        Some("mass-cancel") => {
            args.next();
            return run_mass_cancel(args, keys, policy_max_notional);
        }
        _ => {}
    }
    match parse(args) {
        Ok(OrderVerb::Ls(a)) => run_ls(a, keys),
        Err(msg) => exit_for_parse_error(COMMAND, &usage(), &msg),
    }
}

fn run_ls(a: LsArgs, keys: &NodeKeyring) -> ExitCode {
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
    let snap = match crate::cmd::trade::connect_observe(&a.node, key.as_bytes()) {
        Ok((_handle, snap)) => snap,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            for line in nodekeys::observe_permission_denied_lines(COMMAND, &a.node, &e) {
                eprintln!("{line}");
            }
            return Exit::Failed.into();
        }
        Err(e) => {
            eprintln!("vike-cli {COMMAND}: cannot open observe connection to {}: {e}", a.node);
            return Exit::Connect.into();
        }
    };
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

/// A function rather than a `const`, so the forms it names come from [`selector::FORMS`] itself
/// (stable Rust has no `const`-time string concatenation without pulling in a crate to do it) —
/// two of this file's own tests exist because a hand-typed COPY of that list once fell out of step
/// with it.
fn submit_usage() -> String {
    format!(
        "usage: vike-cli trade order submit <book> <symbol> <buy|sell> <qty> [@<price>|@market] \
         [--reduce-only] [--coid <id>] --node <host:port> [--yes] [--json] [--reason <text to end \
         of line>]\n\n\
         <book> is REQUIRED: an act that adds risk names exactly one venue account (a bare venue \
         names its DEFAULT account). The forms are: {}\n\n\
         Naming an account needs a node advertising `account-scoped-submit`; a node without it \
         may discard the account, so the submit is refused here before anything is sent (exit \
         4) — upgrade the node.\n\n\
         ⚠ --reason must be LAST — everything after it, to end of line, is taken verbatim as the \
         recorded rationale.",
        selector::FORMS
    )
}

const CANCEL_USAGE: &str = "usage: vike-cli trade order cancel <coid> --node <host:port> [--yes] \
                             [--json] [--reason <text to end of line>]\n\n\
                             No book: the client-order-id already names the engine that owns it.";

const MODIFY_USAGE: &str = "usage: vike-cli trade order modify <coid> [--qty Q] [--price P] \
                             --node <host:port> [--yes] [--json] [--reason <text to end of line>]\n\n\
                             No book, same reason as `cancel`. At least one of --qty/--price is \
                             required.";

/// See [`submit_usage`] for why this is a function rather than a `const`.
///
/// ⚠ The book sentence read only "<book> is OPTIONAL — wider is safer for a risk-REDUCING act;
/// naming nothing reaches every account of every venue this node runs" until a review found it
/// silent about the one thing an operator reaching for a book meets: ANY book names an account
/// (a bare venue names `DEFAULT`), and an account-scoped reduce is refused client-side against a
/// node that does not advertise `account-scoped-reduce` — every released node today. The sentence
/// is written against the CAPABILITY, so it stays true the day a node advertises it.
fn mass_cancel_usage() -> String {
    format!(
        "usage: vike-cli trade order mass-cancel [<book>] [<symbol>] --node <host:port> [--yes] \
         [--json] [--reason <text to end of line>]\n\n\
         <book> is OPTIONAL — wider is safer for a risk-REDUCING act; naming nothing reaches \
         every account of every venue this node runs, and no capability gate refuses it. A book \
         names ONE account (a bare venue names its DEFAULT account, never every account of it) \
         and needs a node advertising `account-scoped-reduce`: against a node that does not, it \
         is refused here before anything is sent (exit 4). The forms are: {}",
        selector::FORMS
    )
}

/// `submit <book> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]` — the
/// order-shape grammar ONLY (no `--node`/`--yes`/`--json`/`--reason`, which [`run_submit`] extracts
/// first via [`oneshot::take_wrapper_flags`]). Mirrors `crate::cmd::trade`'s own `parse_submit` with
/// the book moved to the front — the risk-direction law makes it REQUIRED here. PURE: no network.
///
/// The coid is MINTED here (via [`verbs::fill_client_order_id`]) rather than left for the caller,
/// unlike the REPL's own parser: a one-shot invocation has no session-lived minter to defer to, and
/// the id shown at preview time must already be the id that gets sent.
pub(crate) fn parse_submit_args(tokens: &[&str]) -> Result<Verb, CliError> {
    let usage = || CliError::usage(submit_usage());

    let mut reduce_only = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut price_tok: Option<&str> = None;
    let mut coid: Option<&str> = None;
    let mut expect_coid = false;
    for tok in tokens {
        if expect_coid {
            expect_coid = false;
            coid = Some(tok);
            continue;
        }
        if *tok == "--reduce-only" {
            reduce_only = true;
        } else if let Some(v) = tok.strip_prefix("--coid=") {
            coid = Some(v);
        } else if *tok == "--coid" {
            expect_coid = true;
        } else if let Some(p) = tok.strip_prefix('@') {
            if price_tok.is_some() {
                return Err(CliError::usage("submit: more than one @price token".to_string()));
            }
            price_tok = Some(p);
        } else {
            positional.push(tok);
        }
    }
    if expect_coid {
        return Err(CliError::usage("submit: --coid needs a value".to_string()));
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
    let side = super::side_from_word(positional[2]).map_err(CliError::usage)?;
    let qty: f64 = positional[3]
        .parse()
        .map_err(|_| CliError::usage(format!("submit: not a qty: {:?}", positional[3])))?;
    if qty.is_nan() || qty <= 0.0 {
        return Err(CliError::usage("submit: qty must be > 0".to_string()));
    }
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
    let mut minter = verbs::coid_minter();
    let WireCommand::Submit(req) =
        verbs::fill_client_order_id(WireCommand::Submit(req), &mut minter)
    else {
        unreachable!("a Submit went in, so a Submit comes out")
    };
    Ok(Verb::Submit(req))
}

fn run_submit(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let (rest, flags) = match oneshot::take_wrapper_flags(args) {
        Ok(v) => v,
        Err(msg) => {
            return exit_for_parse_error(&format!("{COMMAND} submit"), &submit_usage(), &msg);
        }
    };
    if flags.help {
        println!("{}", submit_usage());
        return ExitCode::SUCCESS;
    }
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let verb = match parse_submit_args(&refs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} submit: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = flags.node else {
        eprintln!("vike-cli {COMMAND} submit: --node <host:port> is required\n{}", submit_usage());
        return Exit::Usage.into();
    };
    run_write(verb, node, keys, policy_max_notional, flags.yes, flags.json, flags.reason, "submit")
}

/// `cancel <coid>` — no book, same grammar as the REPL's `cancel <coid>`. PURE.
pub(crate) fn parse_cancel_args(tokens: &[&str]) -> Result<Verb, CliError> {
    if tokens.len() != 1 {
        return Err(CliError::usage(CANCEL_USAGE.to_string()));
    }
    Ok(Verb::Cancel(tokens[0].to_string()))
}

fn run_cancel(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let (rest, flags) = match oneshot::take_wrapper_flags(args) {
        Ok(v) => v,
        Err(msg) => return exit_for_parse_error(&format!("{COMMAND} cancel"), CANCEL_USAGE, &msg),
    };
    if flags.help {
        println!("{CANCEL_USAGE}");
        return ExitCode::SUCCESS;
    }
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let verb = match parse_cancel_args(&refs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} cancel: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = flags.node else {
        eprintln!("vike-cli {COMMAND} cancel: --node <host:port> is required\n{CANCEL_USAGE}");
        return Exit::Usage.into();
    };
    run_write(verb, node, keys, policy_max_notional, flags.yes, flags.json, flags.reason, "cancel")
}

/// `modify <coid> [--qty Q] [--price P]` — no book, same reason as `cancel`. Mirrors
/// `crate::cmd::trade`'s own `parse_modify`. PURE.
pub(crate) fn parse_modify_args(tokens: &[&str]) -> Result<Verb, CliError> {
    let coid = tokens.first().ok_or_else(|| CliError::usage(MODIFY_USAGE.to_string()))?;
    let mut new_qty: Option<f64> = None;
    let mut new_price: Option<f64> = None;
    let mut i = 1;
    while i < tokens.len() {
        match tokens[i] {
            "--qty" => {
                let v = tokens
                    .get(i + 1)
                    .ok_or_else(|| CliError::usage("modify: --qty needs a value".to_string()))?;
                new_qty = Some(
                    v.parse().map_err(|_| CliError::usage(format!("modify: not a qty: {v:?}")))?,
                );
                i += 2;
            }
            "--price" => {
                let v = tokens
                    .get(i + 1)
                    .ok_or_else(|| CliError::usage("modify: --price needs a value".to_string()))?;
                new_price = Some(
                    v.parse()
                        .map_err(|_| CliError::usage(format!("modify: not a price: {v:?}")))?,
                );
                i += 2;
            }
            other => {
                return Err(CliError::usage(format!(
                    "modify: unexpected token {other:?}\n{MODIFY_USAGE}"
                )));
            }
        }
    }
    if new_qty.is_none() && new_price.is_none() {
        return Err(CliError::usage(
            "modify: nothing to change — pass --qty and/or --price".to_string(),
        ));
    }
    Ok(Verb::Modify { client_order_id: (*coid).to_string(), new_qty, new_price })
}

fn run_modify(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let (rest, flags) = match oneshot::take_wrapper_flags(args) {
        Ok(v) => v,
        Err(msg) => return exit_for_parse_error(&format!("{COMMAND} modify"), MODIFY_USAGE, &msg),
    };
    if flags.help {
        println!("{MODIFY_USAGE}");
        return ExitCode::SUCCESS;
    }
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let verb = match parse_modify_args(&refs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} modify: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = flags.node else {
        eprintln!("vike-cli {COMMAND} modify: --node <host:port> is required\n{MODIFY_USAGE}");
        return Exit::Usage.into();
    };
    run_write(verb, node, keys, policy_max_notional, flags.yes, flags.json, flags.reason, "modify")
}

/// `mass-cancel [<book>] [<symbol>]` — book OPTIONAL (wider is safer). Mirrors
/// `crate::cmd::trade`'s own REPL grammar. PURE.
pub(crate) fn parse_mass_cancel_args(tokens: &[&str]) -> Result<Verb, CliError> {
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
    let symbol = tokens.get(1).map(|s| s.to_string());
    Ok(Verb::MassCancel { venue, symbol, account })
}

fn run_mass_cancel(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let (rest, flags) = match oneshot::take_wrapper_flags(args) {
        Ok(v) => v,
        Err(msg) => {
            return exit_for_parse_error(
                &format!("{COMMAND} mass-cancel"),
                &mass_cancel_usage(),
                &msg,
            );
        }
    };
    if flags.help {
        println!("{}", mass_cancel_usage());
        return ExitCode::SUCCESS;
    }
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let verb = match parse_mass_cancel_args(&refs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} mass-cancel: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = flags.node else {
        eprintln!(
            "vike-cli {COMMAND} mass-cancel: --node <host:port> is required\n{}",
            mass_cancel_usage()
        );
        return Exit::Usage.into();
    };
    run_write(
        verb,
        node,
        keys,
        policy_max_notional,
        flags.yes,
        flags.json,
        flags.reason,
        "mass-cancel",
    )
}

/// Build a [`WriteCtx`] and hand `verb` to [`oneshot::execute_write`] — the ONE tail every write
/// verb's `run_*` function shares once its own grammar has resolved a `Verb` and a `--node`.
#[allow(clippy::too_many_arguments)]
fn run_write(
    verb: Verb,
    node: String,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
    yes: bool,
    json: bool,
    reason: Option<String>,
    verb_word: &str,
) -> ExitCode {
    let ctx = WriteCtx { node, keys, policy_max_notional, yes, json, reason };
    match oneshot::execute_write(verb, &ctx) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} {verb_word}: {}", e.msg);
            e.exit.into()
        }
    }
}

#[path = "order_tests.rs"]
#[cfg(test)]
mod order_tests;
