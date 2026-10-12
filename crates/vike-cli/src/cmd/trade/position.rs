//! `vike-cli trade position` — the POSITION group's router. `ls` (what is currently held) and,
//! since task 7 of the trade-CLI-plane, the two WRITE verbs (`flatten` / `close-all`) are wired.
//!
//! # `ls`
//!
//! `vike-cli trade position ls [<book>] [--symbol SYM] --node <host:port> [--json]` — every open
//! position across every venue block the node's OBSERVE snapshot carries, optionally narrowed to one
//! book (`crate::cmd::trade::selector`) and/or one symbol. Human (default): an aligned table whose
//! header survives an empty result, with a Σ-marked aggregate line when it has rows to sum. `--json`:
//! the flat array `crate::cmd::trade::render` documents, carrying no total row.
//!
//! ⚠ **A LABELLED book (`binance/ALT`) NARROWS here, and that is the opposite of `order ls`.**
//! `crate::cmd::trade::order`'s `ls` must call `selector::refuse_an_unaddressable_book` because an
//! order row's `account` (`WireOrderView::account`) is optional and its absence cannot tell the
//! default account from a node that predates the field, which that read is not taught to separate
//! (the guard is not lifted: a labelled book is still refused there). A position lives inside a
//! `vike_tradehub_client::wire::WireVenueBlock`, and that block DOES carry the account
//! (`WireVenueBlock::account`) — measured directly off `crates/vike-tradehub-client/src/wire.rs`
//! rather than assumed from the order-side gap, since the two row types turn out NOT to share it.
//! So `crate::cmd::trade::render::position_rows` answers the label question for real instead of
//! refusing to ask it, and this `run_ls` never calls the order-side refusal at all — refusing what
//! this read can actually attribute would be inventing a limitation the wire does not have.
//!
//! Connects the OBSERVE half only — this is a read — through
//! [`crate::cmd::trade::connect_observe`], the same "connect, subscribe, wait up to
//! `FIRST_FRAME_WAIT` for a frame the node pushed" sequence `crate::cmd::trade::order`'s `ls` and
//! the REPL's own reads use, so a one-shot group verb and the REPL cannot disagree about how fresh a
//! snapshot has to be before it is trusted. A frame carrying nothing built yet is marked on stderr
//! rather than read as "nothing is held" (`crate::cmd::trade::pre_fold_line`).
//!
//! # The WRITE verbs, and the risk-direction law
//!
//! An act that ADDS risk names exactly one book; an act that REDUCES risk may name a venue, or
//! nothing (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md`
//! §4.5) — the same law `crate::cmd::trade::order`'s write verbs read off the other side:
//!
//! - `flatten <book> <symbol> --node … [--yes] [--json] [--reason <text>]` — book REQUIRED: a
//!   flatten names what it closes, so there is no "every book" spelling of it.
//! - `close-all [<book>] --node … […]` — book OPTIONAL: a risk-REDUCING panic button, which naming
//!   no book at all sends to every account of every venue this node runs. ⚠ This bullet also said
//!   a book "may widen to every account of a venue", and the `--help` text said the same; neither
//!   is true of this grammar — a bare venue names its DEFAULT account, never every account of it,
//!   and any book meets the capability gate described below. [`close_all_usage`] now says so.
//!
//! Both resolve to a [`crate::cmd::verbs::Verb`] through their own PURE parser
//! (`parse_flatten_args`/`parse_close_all_args`) and are executed by
//! [`crate::cmd::trade::oneshot::execute_write`] — see that module's doc for the preview/confirm/send
//! path both verbs share with `crate::cmd::trade::order`'s.
//!
//! ⚠ **A book on either verb REACHES THE WIRE again, and is refused there by the CLIENT's capability
//! gate — this paragraph has now said three different things.** It first read "the book's account
//! reaches the wire directly … the node's own capability gate is what refuses a labelled book
//! against a node too old to route it" — true of the wire, and one layer short of the node, whose
//! `lower_command` dropped the account on `Flatten`/`MarketExit` while the node still answered
//! `accepted`. It then said both parsers REFUSE a labelled book locally, through
//! `crate::cmd::trade::selector`'s `refuse_an_unroutable_account`. Stage 5 of
//! `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md` DELETED that
//! function on 2026-09-26, and the first version's MECHANISM is now the true one, for a different
//! reason than it gave: `crates/vike-tradehub-client/src/remote_control.rs`'s `required_feature`
//! answers `FEATURE_ACCOUNT_SCOPED_REDUCE` for a `Flatten`/`MarketExit` naming an account, and a
//! node advertises that only once it can confine the verb to that one account — not merely once it
//! is new enough. So the refusal is [`crate::exit::Exit::Refused`], nothing is enqueued, and the
//! node never sees the frame.
//!
//! ⚠ **That gate sees a BARE venue too.** The book grammar names the default account POSITIVELY, so
//! `flatten binance BTCUSDT` carries `"DEFAULT"` and is refused by the same gate as
//! `flatten binance/ALT BTCUSDT` — which makes `flatten` unsendable from this surface until a node
//! advertises the capability. [`crate::cmd::trade::oneshot`]'s module doc carries the measurement
//! and the decision it leaves open; `close-all` naming NO book carries no account and is untouched.

use std::process::ExitCode;

use crate::cmd::args::{exit_for_parse_error, help_requested};
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::trade::oneshot;
use crate::cmd::trade::render::{position_rows, positions_json, positions_table};
use crate::cmd::trade::selector;
use crate::cmd::verbs::Verb;
use crate::exit::{CliError, Exit};

/// The group's own verb roster: name, one-line description. [`usage`] and [`verb_names`] are both
/// DERIVED from this, so a verb cannot be listed in one place and missing from another.
pub(crate) const VERBS: &[(&str, &str)] = &[
    ("ls", "list every open position from the node's latest snapshot"),
    ("flatten", "close one (venue, symbol) position — the book is REQUIRED"),
    // ⚠ This row ended "…, optionally scoped to one book" until a review found `--help` offering a
    // narrowing the client gate refuses on every released node — see `close_all_usage`.
    ("close-all", "cancel every order, flatten every position; a book needs account-scoped-reduce"),
];

const COMMAND: &str = "trade position";

/// The group-level usage/help block, with the verb list rendered from [`VERBS`] rather than typed
/// out a second time.
fn usage() -> String {
    let verbs = VERBS
        .iter()
        .map(|(name, desc)| format!("  {name:<12} {desc}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "usage: vike-cli trade position <verb> [options]\n\nverbs:\n{verbs}\n\n  -h, --help   this \
         message"
    )
}

/// The verb names alone, `|`-joined — the roster half of the "needs a verb" / "unknown verb"
/// messages, derived from [`VERBS`] so it cannot name a different set than [`usage`] does.
fn verb_names() -> String {
    VERBS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(" | ")
}

/// One resolved READ/meta `position` sub-command — the WRITE verbs bypass this type, exactly as
/// `crate::cmd::trade::order`'s do and for the same reason (see that module's doc).
#[derive(Debug)]
enum PositionVerb {
    Ls(selector::LsArgs),
}

/// Parse everything after the `position` word: the verb, then that verb's own flags. PURE.
fn parse(mut args: impl Iterator<Item = String>) -> Result<PositionVerb, String> {
    let Some(verb) = args.next() else {
        return Err(format!("`trade position` needs a verb ({})", verb_names()));
    };
    match verb.as_str() {
        "-h" | "--help" | "help" => help_requested(),
        "ls" => Ok(PositionVerb::Ls(selector::parse_ls(args)?)),
        other => Err(format!("unknown `trade position` verb '{other}' ({})", verb_names())),
    }
}

/// Entry point [`crate::cmd::trade::run`] routes `trade position …` to. `args` is everything AFTER
/// the `position` word. `policy_max_notional` is threaded to the WRITE verbs' advisory guardrail —
/// see [`oneshot::WriteCtx`]. The two WRITE verbs are claimed BEFORE [`parse`], for the same reason
/// `crate::cmd::trade::order`'s are.
pub(crate) fn run(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let mut args = args.peekable();
    match args.peek().map(String::as_str) {
        Some("flatten") => {
            args.next();
            return run_flatten(args, keys, policy_max_notional);
        }
        Some("close-all") => {
            args.next();
            return run_close_all(args, keys, policy_max_notional);
        }
        _ => {}
    }
    match parse(args) {
        Ok(PositionVerb::Ls(a)) => run_ls(a, keys),
        Err(msg) => exit_for_parse_error(COMMAND, &usage(), &msg),
    }
}

fn run_ls(a: selector::LsArgs, keys: &NodeKeyring) -> ExitCode {
    // The OBSERVE key specifically: this is a read, and a control key cannot authenticate one (the
    // node verifies each scope against its own key) — the same rule `trade status`/`order ls` state.
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
    // A frame with nothing built is a placeholder, not "nothing is held" — said on STDERR, so a
    // `--json` stdout stays one bare document (`crate::cmd::trade::pre_fold_line`).
    if let Some(line) = crate::cmd::trade::pre_fold_line(&snap) {
        eprintln!("{line}");
    }
    let rows = position_rows(&snap, a.book.as_ref(), a.symbol.as_deref());
    if a.json {
        println!("{}", positions_json(&rows));
    } else {
        // Echo the parsed book back — human mode only, so `--json` stays a bare document a script
        // can pipe straight into `jq`. Unlike `order ls`'s echo, a labelled book here really did
        // narrow the read, not just the default account of a venue.
        if let Some(b) = &a.book {
            println!("book: {}", selector::render(b));
        }
        println!("{}", positions_table(&rows));
    }
    ExitCode::SUCCESS
}

// ---- the WRITE verbs (task 7) ----------------------------------------------------------------

/// A function rather than a `const`, so the forms it names come from [`selector::FORMS`] itself
/// (stable Rust has no `const`-time string concatenation without pulling in a crate to do it) —
/// this file's own tests exist because a hand-typed COPY of that list once fell out of step with
/// it (see `crate::cmd::trade::order`'s twin note).
///
/// ⚠ The book sentence read only "<book> is REQUIRED — a flatten names what it closes" until a
/// review found `--help` silent about the gate every flatten meets: its required book always names
/// an account (a bare venue names `DEFAULT`), so against a node that does not advertise
/// `account-scoped-reduce` — every released node today — no flatten can be sent from here at all.
/// Written against the CAPABILITY, so it stays true the day a node advertises it.
fn flatten_usage() -> String {
    format!(
        "usage: vike-cli trade position flatten <book> <symbol> --node <host:port> [--yes] \
         [--json] [--reason <text to end of line>]\n\n\
         <book> is REQUIRED — a flatten names what it closes, and that is ONE account (a bare \
         venue names its DEFAULT account). The forms are: {}\n\n\
         An account-scoped flatten needs a node advertising `account-scoped-reduce`: against a \
         node that does not, it is refused here before anything is sent (exit 4). `close-all` \
         naming no book is the way out that no capability gate refuses.\n\n\
         ⚠ --reason must be LAST — everything after it, to end of line, is taken verbatim as the \
         recorded rationale.",
        selector::FORMS
    )
}

/// See [`flatten_usage`] for why this is a function rather than a `const`.
///
/// ⚠ The book sentence read "<book> is OPTIONAL — a risk-REDUCING panic button may widen to every
/// account of a venue, or (naming nothing) every venue this node runs" until a review found the
/// first half false twice over: a bare venue names its DEFAULT account, never every account of it,
/// and any named account meets the `account-scoped-reduce` gate. The second half was, and is, true.
fn close_all_usage() -> String {
    format!(
        "usage: vike-cli trade position close-all [<book>] --node <host:port> [--yes] [--json] \
         [--reason <text to end of line>]\n\n\
         <book> is OPTIONAL. Naming none is the panic button: every account of every venue this \
         node runs, and no capability gate refuses it. A book names ONE account (a bare venue \
         names its DEFAULT account, never every account of it) and needs a node advertising \
         `account-scoped-reduce`: against a node that does not, it is refused here before \
         anything is sent (exit 4). The forms are: {}",
        selector::FORMS
    )
}

/// `flatten <book> <symbol>` — book REQUIRED. Mirrors `crate::cmd::trade`'s own REPL grammar with
/// the book validated (and its account threaded) through `crate::cmd::trade::selector::parse`. PURE.
pub(crate) fn parse_flatten_args(tokens: &[&str]) -> Result<Verb, CliError> {
    if tokens.len() != 2 {
        return Err(CliError::usage(flatten_usage()));
    }
    let book = selector::parse(tokens[0])?;
    // ⚠ No local account refusal here any more (this module's doc records the stage-5 deletion).
    // The account goes onto the wire, and the CLIENT's capability gate keeps it from a node that
    // cannot confine a flatten to one account, before anything is enqueued.
    Ok(Verb::Flatten {
        venue: book.venue,
        symbol: tokens[1].to_string(),
        // The book was ALWAYS given (required above), so this always names a POSITIVE account —
        // see `crate::cmd::trade::oneshot`'s module doc.
        account: Some(book.label.to_string()),
    })
}

fn run_flatten(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let (rest, flags) = match oneshot::take_wrapper_flags(args) {
        Ok(v) => v,
        Err(msg) => {
            return exit_for_parse_error(&format!("{COMMAND} flatten"), &flatten_usage(), &msg);
        }
    };
    if flags.help {
        println!("{}", flatten_usage());
        return ExitCode::SUCCESS;
    }
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let verb = match parse_flatten_args(&refs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} flatten: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = flags.node else {
        eprintln!(
            "vike-cli {COMMAND} flatten: --node <host:port> is required\n{}",
            flatten_usage()
        );
        return Exit::Usage.into();
    };
    oneshot::run_write(
        COMMAND,
        verb,
        node,
        keys,
        policy_max_notional,
        flags.yes,
        flags.json,
        flags.reason,
        "flatten",
    )
}

/// `close-all [<book>]` — book OPTIONAL, mapping to [`Verb::MarketExit`] (the panic button). PURE.
pub(crate) fn parse_close_all_args(tokens: &[&str]) -> Result<Verb, CliError> {
    if tokens.len() > 1 {
        return Err(CliError::usage(close_all_usage()));
    }
    let (venue, account) = match tokens.first() {
        None => (None, None),
        Some(book_tok) => {
            let book = selector::parse(book_tok)?;
            // ⚠ No local account refusal here any more — the same stage-5 deletion as
            // `parse_flatten_args`, and the same client-side capability gate in its place. Naming
            // NO book at all stays untouched: the widest, risk-reducing spelling, carrying no
            // account for any gate to see.
            (Some(book.venue), Some(book.label.to_string()))
        }
    };
    Ok(Verb::MarketExit { venue, account })
}

fn run_close_all(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let (rest, flags) = match oneshot::take_wrapper_flags(args) {
        Ok(v) => v,
        Err(msg) => {
            return exit_for_parse_error(&format!("{COMMAND} close-all"), &close_all_usage(), &msg);
        }
    };
    if flags.help {
        println!("{}", close_all_usage());
        return ExitCode::SUCCESS;
    }
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let verb = match parse_close_all_args(&refs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} close-all: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = flags.node else {
        eprintln!(
            "vike-cli {COMMAND} close-all: --node <host:port> is required\n{}",
            close_all_usage()
        );
        return Exit::Usage.into();
    };
    oneshot::run_write(
        COMMAND,
        verb,
        node,
        keys,
        policy_max_notional,
        flags.yes,
        flags.json,
        flags.reason,
        "close-all",
    )
}

#[path = "position_tests.rs"]
#[cfg(test)]
mod position_tests;
