//! `vike-cli trade position` — the POSITION group's router. `ls` (what is currently held) and,
//! since task 7 of the trade-CLI-plane, the two WRITE verbs (`flatten` / `close-all`) are wired.
//!
//! # `ls`
//!
//! `vike-cli trade position ls [<account>] [--symbol SYM] --node <host:port> [--json]` — every open
//! position across every venue block the node's OBSERVE snapshot carries, optionally narrowed to one
//! book (`crate::cmd::trade::selector`) and/or one symbol. Human (default): an aligned table whose
//! header survives an empty result, with a Σ-marked aggregate line when it has rows to sum. `--json`:
//! the flat array `crate::cmd::trade::render` documents, carrying no total row.
//!
//! A book naming an account (`bybit/LIVE`, `bybit/DEFAULT`) narrows to it. ⚠ A BARE venue reads EVERY
//! account of it, each row naming its own, the same as `order ls`: it read the default account alone,
//! so `position ls bybit` on a box with bybit at DEMO and LIVE could list nothing beside an open live
//! position. ⚠ The WRITE verbs below read a bare venue as naming no account (`selector::addressed`). Against a node that
//! predates `OrderViewWire::account`, `order ls` can refuse a book
//! (`selector::refuse_an_unaddressable_book`). A position lives inside a
//! `vike_tradehub_client::wire::VenueBlockWire`, and that block carries the account
//! (`VenueBlockWire::account`), so `crate::cmd::trade::render::position_rows` has no such gap and this
//! `run_ls` never calls that refusal.
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
//! - `flatten <account> <symbol> --node … [--yes] [--json] [--reason <text>]` — book REQUIRED: a
//!   flatten names what it closes, so there is no "every book" spelling of it.
//! - `close-all [<account>] --node … […]` — book OPTIONAL: a risk-REDUCING panic button, which naming
//!   no book at all sends to every account of every venue this node runs. ⚠ A book never widens to
//!   every account of a venue: a bare venue is taken only where the node has ONE account of it, running or not, and is
//!   refused elsewhere with the books to choose from (decision 0254; [`close_all_usage`] says so).
//!
//! `clap` reads the argv (the plane's one parser, `crate::cmd::trade`'s `Cli`: [`PositionVerb`] is
//! this group's enum); both then resolve to a [`vike_client::verbs::Verb`] through their own PURE
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
//! reason than it gave: `crates/vike-tradehub-client/src/tradehub_control.rs`'s `required_feature`
//! answers `FEATURE_ACCOUNT_SCOPED_REDUCE` for a `Flatten`/`MarketExit` naming an account, and a
//! node advertises that only once it can confine the verb to that one account — not merely once it
//! is new enough. So the refusal is [`crate::exit::Exit::Refused`], nothing is enqueued, and the
//! node never sees the frame.
//!
//! A BARE venue names no account on the wire and owes `FEATURE_ONE_ACCOUNT_VENUE` instead, so a node that
//! would fan it over every account of the venue never receives it; [`crate::cmd::trade::oneshot`]'s
//! module doc carries the ruling. `close-all` naming NO book carries neither and is untouched.

use std::process::ExitCode;
use std::time::Duration;

use crate::cmd::args;
use crate::cmd::trade::oneshot::{self, WrapperArgs};
use crate::cmd::trade::render::{position_rows, positions_json, positions_table};
use crate::cmd::trade::selector;
use crate::exit::CliError;
use vike_client::hubkeys::HubKeyring;
use vike_client::verbs::Verb;
use vike_model::accounts::account_keys::BOOK_FORMS;

const COMMAND: &str = "trade position";

/// The program name a refusal of this group's own rules prints under.
const BIN: &str = "vike-cli trade position";

// The `position` group's verbs. A group needs a verb: a bare `trade position` is refused by
// `clap`, and the roster in that refusal and in `--help` is `clap`'s own — derived from this
// enum, so it cannot name a different set than the parser accepts.
#[derive(clap::Subcommand, Debug)]
pub(super) enum PositionVerb {
    #[command(about = "list every open position from the node's latest snapshot")]
    Ls(selector::LsCommand),
    #[command(
        about = "close one (venue, symbol) position — the book is REQUIRED",
        long_about = flatten_prose(),
        override_usage = "vike-cli trade position flatten <ACCOUNT> <SYMBOL> [OPTIONS]"
    )]
    Flatten(FlattenCommand),
    #[command(
        about = "cancel every order, flatten every position; a named account needs account-scoped-reduce",
        long_about = close_all_prose(),
        override_usage = "vike-cli trade position close-all [ACCOUNT] [OPTIONS]"
    )]
    CloseAll(CloseAllCommand),
}

/// Entry point [`crate::cmd::trade::run`] routes `trade position …` to, with the verb `clap` read.
/// `policy_max_notional` is threaded to the WRITE verbs' guardrail — see
/// [`oneshot::WriteContext`] — and `ack_wait` (`config.node_ack_wait_ms`) with it, to the wait for the
/// node's verdict on the command a write verb sends.
pub(super) fn run(
    verb: PositionVerb,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    match verb {
        PositionVerb::Ls(cmd) => match cmd.into_args(keys.node_addr()) {
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
            ack_wait,
        ),
        PositionVerb::CloseAll(c) => oneshot::run_verb(
            COMMAND,
            "close-all",
            close_all_verb(&c.tokens),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
    }
}

fn run_ls(a: selector::LsArgs, keys: &HubKeyring) -> ExitCode {
    let snap = match crate::cmd::trade::observe_read(COMMAND, &a.node, keys) {
        Ok(snap) => snap,
        Err(exit) => return exit,
    };
    let mut rows = position_rows(&snap, a.book.as_ref(), a.symbol.as_deref());
    if let Some(book) = a.named_account() {
        rows.retain(|r| r.account.as_deref() == book.label.text());
    }
    if a.json {
        println!("{}", positions_json(&rows));
    } else {
        // Echo the book as it narrowed — human mode only, so `--json` stays a bare document a script
        // can pipe straight into `jq`.
        if let Some(line) = a.book_line() {
            println!("{line}");
        }
        println!("{}", positions_table(&rows));
    }
    ExitCode::SUCCESS
}

// ---- the WRITE verbs (task 7) ----------------------------------------------------------------

// `flatten`'s book and symbol.
#[derive(clap::Args, Debug)]
pub(super) struct FlattenCommand {
    #[arg(value_name = "ACCOUNT|SYMBOL", help = "<account> <symbol> — both are REQUIRED")]
    tokens: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `close-all`'s optional book.
#[derive(clap::Args, Debug)]
pub(super) struct CloseAllCommand {
    #[arg(value_name = "ACCOUNT", help = "[<account>] — OPTIONAL")]
    tokens: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// The shape line of a refusal for the wrong NUMBER of tokens: each verb's arity rule is its own,
/// not `clap`'s, so the refusal keeps the words it always had.
const FLATTEN_SHAPE: &str = "usage: vike-cli trade position flatten <account> <symbol> --node \
                             <host:port> [--yes] [--json] [--reason <text to end of line>]";

/// `flatten`'s long help, and the second half of its arity refusal. A function rather than a
/// `const`, so the forms it names come from [`BOOK_FORMS`] itself (stable Rust has no
/// `const`-time string concatenation without pulling in a crate to do it) — this file's own tests
/// exist because a hand-typed COPY of that list once fell out of step with it (see
/// `crate::cmd::trade::order`'s twin note). Written against the CAPABILITIES, so it stays true of
/// whichever node answers.
fn flatten_prose() -> String {
    format!(
        "<account> is REQUIRED — a flatten names what it closes, and that is ONE account. {} The \
         forms are: {}\n\n\
         A flatten needs a node advertising `one-account-venue` (bare) or `account-scoped-reduce` \
         (naming an account): against a node that does not, it is refused here before anything is \
         sent (exit 4). `close-all` naming no book is the way out that no capability gate \
         refuses.\n\n\
         ⚠ --reason must be LAST — everything after it, to end of line, is taken verbatim as the \
         recorded rationale.",
        selector::ONE_ACCOUNT_RULE,
        BOOK_FORMS
    )
}

fn flatten_usage() -> String {
    format!("{FLATTEN_SHAPE}\n\n{}", flatten_prose())
}

const CLOSE_ALL_SHAPE: &str = "usage: vike-cli trade position close-all [<account>] --node \
                               <host:port> [--yes] [--json] [--reason <text to end of line>]";

/// See [`flatten_prose`] for why this is a function rather than a `const`.
fn close_all_prose() -> String {
    format!(
        "<account> is OPTIONAL. Naming none is the panic button: every account of every venue this \
         node runs, and no capability gate refuses it. {} A book needs a node advertising \
         `one-account-venue` (bare) or `account-scoped-reduce` (naming an account): against a node \
         that does not, it is refused here before anything is sent (exit 4). The forms are: {}",
        selector::ONE_ACCOUNT_RULE,
        BOOK_FORMS
    )
}

fn close_all_usage() -> String {
    format!("{CLOSE_ALL_SHAPE}\n\n{}", close_all_prose())
}

/// `flatten <account> <symbol>` — book REQUIRED. Mirrors `crate::cmd::trade`'s own REPL grammar with
/// the book read through `crate::cmd::trade::selector::addressed`. PURE.
fn flatten_verb(tokens: &[String]) -> Result<Verb, CliError> {
    if tokens.len() != 2 {
        return Err(CliError::usage(flatten_usage()));
    }
    let (venue, account) = selector::addressed(&tokens[0])?;
    // ⚠ No local account refusal here any more (this module's doc records the stage-5 deletion).
    // The account goes onto the wire, and the CLIENT's capability gate keeps it from a node that
    // cannot confine a flatten to one account, before anything is enqueued.
    Ok(Verb::Flatten { venue, symbol: tokens[1].clone(), account })
}

/// `close-all [<account>]` — book OPTIONAL, mapping to [`Verb::MarketExit`] (the panic button). PURE.
fn close_all_verb(tokens: &[String]) -> Result<Verb, CliError> {
    if tokens.len() > 1 {
        return Err(CliError::usage(close_all_usage()));
    }
    let (venue, account) = selector::optional_book(tokens.first())?;
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
