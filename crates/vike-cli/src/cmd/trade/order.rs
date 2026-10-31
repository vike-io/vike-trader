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
//!   [--type stop|stop-limit|take-profit --trigger <price>] --node <host:port> [--yes] [--json]
//!   [--no-wait] [--reason <text>]` — book REQUIRED, mirrors `crate::cmd::trade`'s own REPL grammar
//!   with the book moved to the front. `--type`/`--trigger` (the REPL's `submit` takes the same two,
//!   through the same `trigger_order`) set the wire's `order_type` and `trigger_price`; the rules
//!   are `trigger_order`'s.
//! - `bracket <book> <symbol> <buy|sell> <qty> [@<price>|@market] --stop-loss <price> --take-profit
//!   <price> --node <host:port> [--yes] [--json] [--no-wait] [--reason <text>]` — ONE TP/SL bracket,
//!   the `WireCommand::Bracket` the desktop's TP/SL ticket sends, book REQUIRED like `submit`'s (it
//!   opens risk). A verb of its own rather than `submit --stop-loss/--take-profit` because it is a
//!   different wire command with no client order id, no `--reduce-only`, no `--type`, no `--coid`:
//!   folding it into `submit` would need four refusals for flags that mean nothing together, and a
//!   `Verb::Submit` that is sometimes not one. The core mints the three legs' ids AFTER the node's
//!   answer, so the answer's `outcome:` look (`crate::cmd::trade::oneshot`'s `Target::of`) is what
//!   prints them, with the core's own refusal words when a venue cannot hold a leg.
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

use vike_tradehub_client::wire::{WireBracketSpec, WireCommand, WireOrderRequest};

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
        about = "list the reduce-only stops the NODE holds (not the venue): armed, held, re-armed",
        long_about = super::stops::PROSE
    )]
    Stops(super::stops::StopsCmd),
    #[command(
        about = "place one order — the book is REQUIRED (an act that adds risk names exactly one)",
        long_about = submit_prose(),
        override_usage = "vike-cli trade order submit <BOOK> <SYMBOL> <buy|sell> <QTY> \
                          [@PRICE|@market] [OPTIONS]"
    )]
    Submit(SubmitCmd),
    #[command(
        about = "place one TP/SL bracket: an entry with a stop-loss and a take-profit",
        long_about = bracket_prose(),
        override_usage = "vike-cli trade order bracket <BOOK> <SYMBOL> <buy|sell> <QTY> \
                          [@PRICE|@market] --stop-loss <PRICE> --take-profit <PRICE> [OPTIONS]"
    )]
    Bracket(BracketCmd),
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
        OrderVerb::Stops(c) => super::stops::run_stops(c, keys),
        OrderVerb::Submit(c) => oneshot::run_verb(
            COMMAND,
            "submit",
            submit_verb(
                &c.tokens,
                c.reduce_only,
                c.coid.as_deref(),
                c.kind.as_deref(),
                c.trigger.as_deref(),
                (c.tif.as_deref(), c.expires.as_deref(), c.post_only),
                c.wrap.reason().as_deref(),
            ),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::Bracket(c) => oneshot::run_verb(
            COMMAND,
            "bracket",
            bracket_verb(&c.tokens, c.stop_loss.as_deref(), c.take_profit.as_deref()),
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
    // `type` is a Rust keyword, hence the field name. The three words are the trigger kinds of
    // `vike_model::OrderType::is_trigger`, hyphenated as a flag value; a limit or market order is
    // still spelled by `@price` / `@market` and this flag does not take those words.
    #[arg(
        long = "type",
        value_name = "KIND",
        help = "a native trigger order: stop | stop-limit | take-profit (needs --trigger)"
    )]
    kind: Option<String>,
    #[arg(long, value_name = "PRICE", help = "the trigger price of a --type order")]
    trigger: Option<String>,
    // The LIFETIME of a limit order (`@<price>`). Three flags, one rule (`lifetime_flags`): `--tif` is
    // the venue-neutral set the model has, `--expires` is the end of a `gtd` order, and `--post-only`
    // is its own lifetime (maker-only), so it does not combine with `--tif`.
    #[arg(
        long,
        value_name = "TIF",
        help = "how long a limit order lives: day | gtc | ioc | fok | gtd (default gtc; a venue that cannot do it refuses by name)"
    )]
    tif: Option<String>,
    #[arg(
        long,
        value_name = "WHEN",
        help = "when a --tif gtd order ends: a UTC instant like 2026-11-01T12:00:00Z, or epoch milliseconds"
    )]
    expires: Option<String>,
    #[arg(
        long,
        help = "a maker-only limit order: the venue refuses it, never executes it, if it would take liquidity (needs @<price>)"
    )]
    post_only: bool,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `bracket`'s tokens and flags. The order-shape tokens are `submit`'s `<book> <symbol> <buy|sell>
// <qty>` and an optional `@<price>`/`@market` (the entry), in the order written; both exits are
// FLAGS so a stop-loss and a take-profit cannot be swapped by position.
#[derive(clap::Args, Debug)]
pub(super) struct BracketCmd {
    #[arg(value_name = "TOKEN", help = "<book> <symbol> <buy|sell> <qty> [@<price>|@market]")]
    tokens: Vec<String>,
    #[arg(
        long = "stop-loss",
        value_name = "PRICE",
        help = "the stop-loss trigger price (required)"
    )]
    stop_loss: Option<String>,
    #[arg(
        long = "take-profit",
        value_name = "PRICE",
        help = "the take-profit limit price (required)"
    )]
    take_profit: Option<String>,
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
                            [@<price>|@market] [--reduce-only] [--coid <id>] \
                            [--type stop|stop-limit|take-profit --trigger <price>] \
                            [--tif day|gtc|ioc|fok|gtd [--expires <when>] | --post-only] \
                            --node <host:port> [--yes] [--json] [--reason <text to end of line>]";

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
         A native trigger order is `--type stop|stop-limit|take-profit --trigger <price>`: the two \
         flags come together, `stop-limit` also takes the limit price as `@<price>`, and `stop` \
         and `take-profit` take none. Without `--type` the order is a limit (`@<price>`) or a \
         market order, as before.\n\n\
         A limit order's LIFETIME is `--tif day|gtc|ioc|fok|gtd` (default gtc; `gtd` also takes \
         `--expires`, a UTC instant like 2026-11-01T12:00:00Z or epoch milliseconds) or \
         `--post-only`, a maker-only order that the venue REFUSES, and never executes, if it \
         would take liquidity. A lifetime belongs to a limit order (`@<price>`); a venue that \
         cannot do the one you name refuses it by name, never sends a plain limit order in its \
         place. Naming one needs a node advertising `order-tif`: against an older node the \
         submit is refused here before anything is sent (exit 4).\n\n\
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
                            required. --price is the TRIGGER of a stop and the limit price of a \
                            limit order. A venue that cannot change a resting order in place has \
                            it cancelled and placed again under a new client order id that stays \
                            linked to this one (`cancel <coid>` and `modify <coid>` keep reaching \
                            it): a reduce-only stop is placed FIRST and the old one cancelled once \
                            the new one is acknowledged, so the position is never without a stop; \
                            a limit order is cancelled first and LOSES ITS PLACE IN THE QUEUE. A \
                            refused change leaves the old order standing and the node says why in \
                            its recent events.";

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
fn submit_verb(
    tokens: &[String],
    reduce_only: bool,
    coid: Option<&str>,
    kind: Option<&str>,
    trigger: Option<&str>,
    lifetime: (Option<&str>, Option<&str>, bool),
    reason: Option<&str>,
) -> Result<Verb, CliError> {
    let usage = || CliError::usage(submit_usage());
    // `--reason` takes EVERYTHING after it, flags included (`oneshot`'s `WrapperArgs`), so any flag
    // of this verb written after it is rationale text and never reaches the order: a `--type` or
    // `--trigger` would send a plain market order, and a dropped `--reduce-only` would OPEN risk.
    // Refused rather than sent: the operator meant the flag.
    if let Some(flag) = reason.and_then(swallowed_submit_flag) {
        return Err(CliError::usage(format!(
            "submit: `{flag}` appears after --reason, which takes everything after it as the \
             rationale, so it would be ignored and the order would go out without it. Put `{flag}` \
             BEFORE --reason"
        )));
    }

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
    let trigger = trigger_order(kind, trigger, price.is_some())?;
    let order_type = trigger.map_or(order_type, |(word, _)| word);
    let (tif, expires, post_only) = lifetime;
    let (time_in_force, gtd_expiry) = lifetime_flags(tif, expires, post_only, order_type)?;

    let req = WireOrderRequest {
        client_order_id,
        venue: book.venue,
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: order_type.to_string(),
        price,
        trigger_price: trigger.map(|(_, px)| px),
        reduce_only,
        time_in_force,
        gtd_expiry,
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

/// Every flag `submit` itself defines (its own and the shared wrapper flags), long and short, read
/// off the `clap` definition so a flag added to [`SubmitCmd`] is covered without a second list. The
/// `--reason` flag is left out: a `--reason` inside the text is only more text.
fn submit_flags() -> Vec<String> {
    use clap::Args as _;
    SubmitCmd::augment_args(clap::Command::new("submit"))
        .get_arguments()
        .filter(|arg| arg.get_id().as_str() != "reason")
        .flat_map(|arg| {
            let long = arg.get_long().map(|l| format!("--{l}"));
            long.into_iter().chain(arg.get_short().map(|s| format!("-{s}")))
        })
        .collect()
}

/// The flag of `submit` (see [`submit_flags`]) that a `--reason` text contains as a whole word, or
/// in its `--flag=value` spelling, or `None`. PURE.
fn swallowed_submit_flag(reason: &str) -> Option<String> {
    let flags = submit_flags();
    reason.split_whitespace().find_map(|word| {
        flags
            .iter()
            .find(|flag| {
                word == flag.as_str()
                    || (flag.starts_with("--")
                        && word.strip_prefix(flag.as_str()).is_some_and(|r| r.starts_with('=')))
            })
            .cloned()
    })
}

/// `--tif` / `--expires` / `--post-only` → the wire lifetime word and the `gtd` expiry (epoch ms), both
/// `None` for the default `gtc` (so an order that names no lifetime is the frame it always was). PURE.
///
/// The rules, in the order they bite:
///
/// - `--post-only` is its own lifetime (maker-only) and does not combine with `--tif`; `--tif` takes the
///   five lifetimes `day | gtc | ioc | fok | gtd`, any case, and nothing else (post-only is not a `--tif`
///   word: one spelling);
/// - `--tif gtd` needs `--expires`, and nothing but `gtd` takes it; `--expires` is a UTC instant
///   (`2026-11-01T12:00:00Z`) or epoch MILLISECONDS (a number small enough to be seconds is refused: a
///   good-till-date order that ends in 1970 is not what anyone typed);
/// - the pair is then judged with the order's kind by `vike_model::lifetime_refusal` — the rule the node's
///   door applies again: a lifetime other than `gtc` belongs to a limit order (`@<price>`), a `gtd`
///   expiry must be in the future.
///
/// Whether the VENUE can do it is not asked here: the node's capability row refuses by name.
/// The REPL's `parse_submit` calls it too, so the two grammars cannot come to differ.
pub(super) fn lifetime_flags(
    tif: Option<&str>,
    expires: Option<&str>,
    post_only: bool,
    order_type: &str,
) -> Result<(Option<String>, Option<i64>), CliError> {
    use vike_model::TimeInForce;
    let usage = |msg: String| CliError::usage(format!("submit: {msg}"));
    let lifetime = match (post_only, tif) {
        (true, Some(_)) => {
            return Err(usage(
                "--post-only is a lifetime of its own and does not combine with --tif".to_string(),
            ));
        }
        (true, None) => TimeInForce::PostOnly,
        (false, None) => TimeInForce::Gtc,
        (false, Some(word)) => match TimeInForce::parse(word) {
            Ok(TimeInForce::PostOnly) => {
                return Err(usage(
                    "post-only is its own flag: write --post-only, not --tif post-only".to_string(),
                ));
            }
            Ok(t) => t,
            Err(_) => {
                return Err(usage(format!(
                    "--tif {word:?} is not a time in force - expected day, gtc, ioc, fok or gtd"
                )));
            }
        },
    };
    let expiry = match (lifetime, expires) {
        (TimeInForce::Gtd, None) => {
            return Err(usage("--tif gtd needs --expires <WHEN>".to_string()));
        }
        (TimeInForce::Gtd, Some(when)) => Some(expiry_ms(when).map_err(usage)?),
        (_, Some(_)) => return Err(usage("--expires belongs to --tif gtd".to_string())),
        (_, None) => None,
    };
    let kind = vike_model::OrderType::parse(order_type).unwrap_or_default();
    if let Some(why) = vike_model::lifetime_refusal(&kind, lifetime, expiry, vike_model::now_ms()) {
        let hint = if kind == vike_model::OrderType::Limit {
            ""
        } else {
            " (a lifetime needs a limit order: give @<price>)"
        };
        return Err(usage(format!("{why}{hint}")));
    }
    let word = (lifetime != TimeInForce::Gtc).then(|| lifetime.as_word().to_string());
    Ok((word, expiry))
}

/// `--expires`: epoch MILLISECONDS or a UTC instant (`YYYY-MM-DDTHH:MM:SSZ`). A bare number under
/// 10^11 is seconds (10^11 ms is March 1973), and is refused rather than read as a 1970 instant.
fn expiry_ms(when: &str) -> Result<i64, String> {
    let when = when.trim();
    if !when.is_empty() && when.bytes().all(|b| b.is_ascii_digit()) {
        return match when.parse::<i64>() {
            Ok(ms) if ms >= 100_000_000_000 => Ok(ms),
            Ok(_) => Err(format!(
                "--expires {when} looks like epoch SECONDS; give milliseconds, or a UTC instant like \
                 2026-11-01T12:00:00Z"
            )),
            Err(_) => Err(format!("--expires {when} is not a usable number")),
        };
    }
    vike_model::parse_utc_timestamp_ms(when).ok_or_else(|| {
        format!(
            "--expires {when:?} is not a UTC instant like 2026-11-01T12:00:00Z (the Z is required) \
             or epoch milliseconds"
        )
    })
}

/// `--type` / `--trigger` → the wire order-type word and the trigger price of a native trigger
/// order, or `None` when neither flag was given (the order stays the limit or market order its
/// `@price` token made). PURE.
///
/// The two flags come together: a trigger kind without a price to fire at, and a trigger price
/// without a kind, are both usage errors — `stop` and `take-profit` cannot be told apart from the
/// price alone. `stop-limit` is a stop whose fill is a limit, so it REQUIRES the `@price`; `stop`
/// and `take-profit` carry only the trigger (`vike_model::OrderType`'s docs), so an `@price` with
/// them is refused rather than passed on for a venue adapter to read as it likes. The word is the
/// platform's own (`OrderType::as_str`); the flag value is hyphenated, and the underscore spelling
/// the node prints is accepted too. A non-finite trigger is left to
/// [`verbs::unsizeable_submit_refusal`], which `submit_verb` calls next. The REPL's `parse_submit`
/// calls it too, so the two grammars cannot come to differ on a trigger order.
pub(super) fn trigger_order(
    kind: Option<&str>,
    trigger: Option<&str>,
    has_price: bool,
) -> Result<Option<(&'static str, f64)>, CliError> {
    let (kind, trigger) = match (kind, trigger) {
        (None, None) => return Ok(None),
        (Some(kind), Some(trigger)) => (kind, trigger),
        (Some(_), None) => {
            return Err(CliError::usage("submit: --type needs --trigger <price>".to_string()));
        }
        (None, Some(_)) => {
            return Err(CliError::usage(
                "submit: --trigger needs --type stop|stop-limit|take-profit".to_string(),
            ));
        }
    };
    let word = match kind.to_ascii_lowercase().as_str() {
        "stop" => "stop",
        "stop-limit" | "stop_limit" => "stop_limit",
        "take-profit" | "take_profit" => "take_profit",
        _ => {
            return Err(CliError::usage(format!(
                "submit: --type {kind:?} is not a trigger kind - expected stop, stop-limit or \
                 take-profit (a limit or market order is spelled @<price> / @market)"
            )));
        }
    };
    let px: f64 = trigger
        .parse()
        .map_err(|_| CliError::usage(format!("submit: not a trigger price: {trigger:?}")))?;
    // A trigger is a PRICE: zero and below mean nothing to fire at (and NaN is no price). An
    // infinity is left to `verbs::unsizeable_submit_refusal`, which words it.
    if px.is_nan() || px <= 0.0 {
        return Err(CliError::usage(format!(
            "submit: --trigger must be a price above zero, got {trigger:?}"
        )));
    }
    match (word, has_price) {
        ("stop_limit", false) => Err(CliError::usage(
            "submit: --type stop-limit needs the limit price as @<price>".to_string(),
        )),
        ("stop" | "take_profit", true) => Err(CliError::usage(format!(
            "submit: --type {kind} takes no @<price> - only --trigger (use stop-limit for a limit \
             fill)"
        ))),
        _ => Ok(Some((word, px))),
    }
}

/// The shape line of a refusal for a bracket that does not hang together.
const BRACKET_SHAPE: &str = "usage: vike-cli trade order bracket <book> <symbol> <buy|sell> <qty> \
                             [@<price>|@market] --stop-loss <price> --take-profit <price> \
                             --node <host:port> [--yes] [--json] [--no-wait] \
                             [--reason <text to end of line>]";

/// `bracket`'s long help, and the second half of its arity refusal. A function for the reason
/// [`submit_prose`] is one (`selector::FORMS` is named, not copied).
fn bracket_prose() -> String {
    format!(
        "<book> is REQUIRED, as for `submit`: a bracket opens risk, so it names exactly one venue \
         account (a bare venue names its DEFAULT account). The forms are: {}\n\n\
         The entry is a limit at `@<price>`, or a market order with `@market` or no price. \
         --stop-loss and --take-profit are BOTH required: a buy needs stop-loss < take-profit (and \
         stop-loss < entry < take-profit for a limit entry), a sell the reverse, and the node \
         refuses an inverted bracket before it answers. The core sends the entry and HOLDS the two \
         exits until it fills, then releases them as reduce-only siblings.\n\n\
         A venue that cannot hold one of the exits refuses the WHOLE bracket and sends nothing: \
         three rejected legs, whose coids and reasons the answer prints under `outcome:` in the \
         core's own words. The core mints those three ids AFTER the node answers and a bracket has \
         no client order id of its own, so a second run places a SECOND bracket: after a `not yet \
         known` answer run `vike-cli trade order ls` before sending it again.\n\n\
         Naming an account needs a node advertising `bracket-account`, and the node takes the \
         bracket only on an engine that trades <symbol>.\n\n\
         --reason must be LAST: everything after it, to end of line, is taken verbatim as the \
         recorded rationale, so a --stop-loss or --take-profit written after it is missing and \
         refused.",
        selector::FORMS
    )
}

fn bracket_usage() -> String {
    format!("{BRACKET_SHAPE}\n\n{}", bracket_prose())
}

/// `bracket <book> <symbol> <buy|sell> <qty> [@<price>|@market] --stop-loss <px> --take-profit <px>`
/// — the order-shape rules ONLY (`--node`/`--yes`/`--json`/`--reason` are [`WrapperArgs`], read by
/// `clap`). `tokens` are the positionals in the order written, the `@price` token included wherever
/// it sat; `stop_loss`/`take_profit` are the RAW flag values. The book is REQUIRED and always names
/// its account positively (`DEFAULT` for a bare venue), as `submit` does. PURE: no network.
///
/// Every number must be finite and above zero, refused here as a usage error: a NON-FINITE entry
/// price would serialise as `null`, which the node reads as a MARKET entry
/// (`WireBracketSpec::entry_price`), so this is the guard the wire type leaves to the sender. What
/// is NOT judged here is the bracket's SHAPE (an exit on the wrong side of its entry, a symbol the
/// engine does not trade): the node's door refuses those in its own words before its Ack, and a
/// client copy would be a second rule to keep in step.
fn bracket_verb(
    tokens: &[String],
    stop_loss: Option<&str>,
    take_profit: Option<&str>,
) -> Result<Verb, CliError> {
    let mut positional: Vec<&str> = Vec::new();
    let mut entry_tok: Option<&str> = None;
    for tok in tokens {
        if let Some(p) = tok.strip_prefix('@') {
            if entry_tok.is_some() {
                return Err(CliError::usage("bracket: more than one @price token".to_string()));
            }
            entry_tok = Some(p);
        } else {
            positional.push(tok);
        }
    }
    if positional.len() != 4 {
        return Err(CliError::usage(bracket_usage()));
    }
    let book = selector::parse(positional[0])?;
    let side = super::side_arg(positional[2]).map_err(CliError::usage)?;
    let qty = bracket_number("qty", positional[3])?;
    let entry_price = match entry_tok {
        None | Some("market") => None,
        Some(p) => Some(bracket_number("entry price", p)?),
    };
    let stop_loss = bracket_number("--stop-loss", bracket_flag("--stop-loss", stop_loss)?)?;
    let take_profit = bracket_number("--take-profit", bracket_flag("--take-profit", take_profit)?)?;
    Ok(Verb::Bracket(WireBracketSpec {
        venue: book.venue,
        symbol: positional[1].to_string(),
        side,
        qty,
        entry_price,
        stop_loss,
        take_profit,
        account: Some(book.label.to_string()),
    }))
}

/// A bracket's required flag, or the usage error naming it.
fn bracket_flag<'a>(flag: &str, value: Option<&'a str>) -> Result<&'a str, CliError> {
    value.ok_or_else(|| {
        CliError::usage(format!(
            "bracket: {flag} <price> is required - a bracket is an entry WITH a stop-loss and a \
             take-profit"
        ))
    })
}

/// One bracket number, finite and above zero; `what` names it in the refusal.
fn bracket_number(what: &str, raw: &str) -> Result<f64, CliError> {
    let v: f64 = raw
        .parse()
        .map_err(|_| CliError::usage(format!("bracket: not a number for {what}: {raw:?}")))?;
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(CliError::usage(format!(
            "bracket: {what} must be a finite number above zero, got {raw:?}"
        )))
    }
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
        OrderVerb::Submit(c) => submit_verb(
            &c.tokens,
            c.reduce_only,
            c.coid.as_deref(),
            c.kind.as_deref(),
            c.trigger.as_deref(),
            (c.tif.as_deref(), c.expires.as_deref(), c.post_only),
            c.wrap.reason().as_deref(),
        ),
        other => unreachable!("`submit` read as {other:?}"),
    }
}

/// [`bracket_verb`] over an argv tail, through the plane's parser.
#[cfg(test)]
fn parse_bracket_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("bracket", tokens)? {
        OrderVerb::Bracket(c) => {
            bracket_verb(&c.tokens, c.stop_loss.as_deref(), c.take_profit.as_deref())
        }
        other => unreachable!("`bracket` read as {other:?}"),
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
