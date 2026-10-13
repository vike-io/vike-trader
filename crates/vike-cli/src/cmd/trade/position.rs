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
//! `clap` reads the argv (the plane's one parser, `crate::cmd::trade`'s `Cli`: [`PositionVerb`] is
//! this group's enum); both then resolve to a [`crate::cmd::verbs::Verb`] through their own PURE
//! rules (`flatten_verb`/`close_all_verb`) and are executed by
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

use crate::cmd::args;
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::trade::oneshot::{self, WrapperArgs};
use crate::cmd::trade::render::{position_rows, positions_json, positions_table};
use crate::cmd::trade::selector;
use crate::cmd::verbs::Verb;
use crate::exit::{CliError, Exit};

const COMMAND: &str = "trade position";

/// The program name a refusal of this group's own rules prints under.
const BIN: &str = "vike-cli trade position";

// The `position` group's verbs. A group needs a verb: a bare `trade position` is refused by
// `clap`, and the roster in that refusal and in `--help` is `clap`'s own — derived from this
// enum, so it cannot name a different set than the parser accepts.
#[derive(clap::Subcommand, Debug)]
pub(super) enum PositionVerb {
    #[command(about = "list every open position from the node's latest snapshot")]
    Ls(selector::LsCmd),
    #[command(
        about = "close one (venue, symbol) position — the book is REQUIRED",
        long_about = flatten_prose(),
        override_usage = "vike-cli trade position flatten <BOOK> <SYMBOL> [OPTIONS]"
    )]
    Flatten(FlattenCmd),
    // ⚠ The `about` ended "…, optionally scoped to one book" until a review found `--help` offering
    // a narrowing the client gate refuses on every released node — see `close_all_prose`.
    #[command(
        about = "cancel every order, flatten every position; a book needs account-scoped-reduce",
        long_about = close_all_prose(),
        override_usage = "vike-cli trade position close-all [BOOK] [OPTIONS]"
    )]
    CloseAll(CloseAllCmd),
}

/// Entry point [`crate::cmd::trade::run`] routes `trade position …` to, with the verb `clap` read.
/// `policy_max_notional` is threaded to the WRITE verbs' advisory guardrail — see
/// [`oneshot::WriteCtx`].
pub(super) fn run(
    verb: PositionVerb,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    match verb {
        PositionVerb::Ls(cmd) => match cmd.into_args() {
            Ok(a) => run_ls(a, keys),
            Err(msg) => args::refuse(BIN, msg),
        },
        PositionVerb::Flatten(c) => oneshot::run_verb(
            COMMAND,
            "flatten",
            flatten_verb(&c.tokens),
            &c.wrap,
            keys,
            policy_max_notional,
        ),
        PositionVerb::CloseAll(c) => oneshot::run_verb(
            COMMAND,
            "close-all",
            close_all_verb(&c.tokens),
            &c.wrap,
            keys,
            policy_max_notional,
        ),
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

// `flatten`'s book and symbol.
#[derive(clap::Args, Debug)]
pub(super) struct FlattenCmd {
    #[arg(value_name = "BOOK|SYMBOL", help = "<book> <symbol> — both are REQUIRED")]
    tokens: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `close-all`'s optional book.
#[derive(clap::Args, Debug)]
pub(super) struct CloseAllCmd {
    #[arg(value_name = "BOOK", help = "[<book>] — OPTIONAL")]
    tokens: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// The shape line of a refusal for the wrong NUMBER of tokens: each verb's arity rule is its own,
/// not `clap`'s, so the refusal keeps the words it always had.
const FLATTEN_SHAPE: &str = "usage: vike-cli trade position flatten <book> <symbol> --node \
                             <host:port> [--yes] [--json] [--reason <text to end of line>]";

/// `flatten`'s long help, and the second half of its arity refusal. A function rather than a
/// `const`, so the forms it names come from [`selector::FORMS`] itself (stable Rust has no
/// `const`-time string concatenation without pulling in a crate to do it) — this file's own tests
/// exist because a hand-typed COPY of that list once fell out of step with it (see
/// `crate::cmd::trade::order`'s twin note).
///
/// ⚠ The book sentence read only "<book> is REQUIRED — a flatten names what it closes" until a
/// review found `--help` silent about the gate every flatten meets: its required book always names
/// an account (a bare venue names `DEFAULT`), so against a node that does not advertise
/// `account-scoped-reduce` — every released node today — no flatten can be sent from here at all.
/// Written against the CAPABILITY, so it stays true the day a node advertises it.
fn flatten_prose() -> String {
    format!(
        "<book> is REQUIRED — a flatten names what it closes, and that is ONE account (a bare \
         venue names its DEFAULT account). The forms are: {}\n\n\
         An account-scoped flatten needs a node advertising `account-scoped-reduce`: against a \
         node that does not, it is refused here before anything is sent (exit 4). `close-all` \
         naming no book is the way out that no capability gate refuses.\n\n\
         ⚠ --reason must be LAST — everything after it, to end of line, is taken verbatim as the \
         recorded rationale.",
        selector::FORMS
    )
}

fn flatten_usage() -> String {
    format!("{FLATTEN_SHAPE}\n\n{}", flatten_prose())
}

const CLOSE_ALL_SHAPE: &str = "usage: vike-cli trade position close-all [<book>] --node \
                               <host:port> [--yes] [--json] [--reason <text to end of line>]";

/// See [`flatten_prose`] for why this is a function rather than a `const`.
///
/// ⚠ The book sentence read "<book> is OPTIONAL — a risk-REDUCING panic button may widen to every
/// account of a venue, or (naming nothing) every venue this node runs" until a review found the
/// first half false twice over: a bare venue names its DEFAULT account, never every account of it,
/// and any named account meets the `account-scoped-reduce` gate. The second half was, and is, true.
fn close_all_prose() -> String {
    format!(
        "<book> is OPTIONAL. Naming none is the panic button: every account of every venue this \
         node runs, and no capability gate refuses it. A book names ONE account (a bare venue \
         names its DEFAULT account, never every account of it) and needs a node advertising \
         `account-scoped-reduce`: against a node that does not, it is refused here before \
         anything is sent (exit 4). The forms are: {}",
        selector::FORMS
    )
}

fn close_all_usage() -> String {
    format!("{CLOSE_ALL_SHAPE}\n\n{}", close_all_prose())
}

/// `flatten <book> <symbol>` — book REQUIRED. Mirrors `crate::cmd::trade`'s own REPL grammar with
/// the book validated (and its account threaded) through `crate::cmd::trade::selector::parse`. PURE.
fn flatten_verb(tokens: &[String]) -> Result<Verb, CliError> {
    if tokens.len() != 2 {
        return Err(CliError::usage(flatten_usage()));
    }
    let book = selector::parse(&tokens[0])?;
    // ⚠ No local account refusal here any more (this module's doc records the stage-5 deletion).
    // The account goes onto the wire, and the CLIENT's capability gate keeps it from a node that
    // cannot confine a flatten to one account, before anything is enqueued.
    Ok(Verb::Flatten {
        venue: book.venue,
        symbol: tokens[1].clone(),
        // The book was ALWAYS given (required above), so this always names a POSITIVE account —
        // see `crate::cmd::trade::oneshot`'s module doc.
        account: Some(book.label.to_string()),
    })
}

/// `close-all [<book>]` — book OPTIONAL, mapping to [`Verb::MarketExit`] (the panic button). PURE.
fn close_all_verb(tokens: &[String]) -> Result<Verb, CliError> {
    if tokens.len() > 1 {
        return Err(CliError::usage(close_all_usage()));
    }
    let (venue, account) = match tokens.first() {
        None => (None, None),
        Some(book_tok) => {
            let book = selector::parse(book_tok)?;
            // ⚠ No local account refusal here any more — the same stage-5 deletion as
            // `flatten_verb`, and the same client-side capability gate in its place. Naming
            // NO book at all stays untouched: the widest, risk-reducing spelling, carrying no
            // account for any gate to see.
            (Some(book.venue), Some(book.label.to_string()))
        }
    };
    Ok(Verb::MarketExit { venue, account })
}

// ---- test seams: the same argv tails the tests always wrote, read through the plane's parser ----

/// `word` and `tokens` read as `trade position <word> <tokens…>` by the plane's one `clap` parser.
#[cfg(test)]
fn read(word: &str, tokens: &[&str]) -> Result<PositionVerb, CliError> {
    let argv = ["position", word].into_iter().chain(tokens.iter().copied()).map(String::from);
    match super::parse(argv).map_err(|stop| CliError::usage(stop.to_string()))?.sub {
        Some(super::Sub::Position { verb }) => Ok(verb),
        other => panic!("`trade position {word}` did not read as a position verb: {other:?}"),
    }
}

/// [`flatten_verb`] over an argv tail (no `trade position flatten` words), through the plane's
/// parser.
#[cfg(test)]
fn parse_flatten_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("flatten", tokens)? {
        PositionVerb::Flatten(c) => flatten_verb(&c.tokens),
        other => unreachable!("`flatten` read as {other:?}"),
    }
}

/// [`close_all_verb`] over an argv tail, through the plane's parser.
#[cfg(test)]
fn parse_close_all_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("close-all", tokens)? {
        PositionVerb::CloseAll(c) => close_all_verb(&c.tokens),
        other => unreachable!("`close-all` read as {other:?}"),
    }
}

#[path = "position_tests.rs"]
#[cfg(test)]
mod position_tests;
