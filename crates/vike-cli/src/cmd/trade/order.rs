//! `vike-cli trade order` — the ORDER group's router. `ls` (the working-set read) and, since task 7
//! of the trade-CLI-plane, the four WRITE verbs (`submit` / `cancel` / `modify` / `mass-cancel`) are
//! wired; the node-lifecycle verbs stay elsewhere.
//!
//! # `ls`
//!
//! `vike-cli trade order ls [<account>] [--symbol SYM] --node <host:port> [--json]` — the working
//! order set read off the node's OBSERVE snapshot, optionally narrowed to one book
//! (`crate::cmd::trade::selector`) and/or one symbol, each row naming its account. Human (default):
//! an aligned table whose header survives an empty result. `--json`: the flat array
//! `crate::cmd::trade::render` documents.
//!
//! A book that names an account (`binance/ALT`, `binance/DEFAULT`) narrows to that account's orders.
//! ⚠ A BARE venue (`binance`) lists EVERY account of it, as it always has and as `position ls` does:
//! the rows name theirs, so nothing is mixed unattributed. A WRITE's bare venue is another word: it
//! names no account at all (`selector::addressed`).
//!
//! ⚠ **A book that names an account REFUSES rather than guessing when the frame cannot attribute an
//! order of the venue** (a node that predates `OrderViewWire::account` sends labelled orders without
//! it): `crate::cmd::trade::selector::refuse_an_unaddressable_book`, so an operator never sees a table
//! it did not actually narrow. That needs the frame, so unlike a malformed book it costs a dial.
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
//! - `submit <account> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--order-client-id <id>]
//!   [--type stop|stop-limit|take-profit --trigger <price>] [--type trailing|trailing-limit --trail <price>
//!   [--offset <price>] [--trigger <activation>]] [--type trailing-take-profit --trail <price>
//!   [--offset <price>]] [--type pegged --peg <anchor> [--offset <price>]] [--adjust-at <price> --adjust-to
//!   <target> …] [--all-or-none] [--sweep-to-fill] [--discretion <amount>] --node <host:port> [--yes] [--json]
//!   [--no-wait] [--reason <text>]` — book REQUIRED, mirrors `crate::cmd::trade`'s own REPL grammar
//!   with the book moved to the front. `--type`/`--trigger` (the REPL's `submit` takes the same two,
//!   through the same `trigger_order`) set the wire's `order_type` and `trigger_price`; the rules
//!   are `trigger_order`'s. `--trail`/`--trail-percent`/`--offset` set the wire's `trail`, `trail_percent`
//!   and `limit_offset` (`trail_flags`); `--peg` with `--offset` the wire's `peg` (`peg_flag`); the three
//!   attribute flags its `attributes` (`attributes_flag`), the `--adjust-*` flags its `adjust`
//!   (`adjust_flag`), and `--when-price-on` the release's price venue (`release_flag`).
//! - `bracket <account> <symbol> <buy|sell> <qty> [@<price>|@market] --stop-loss <price> --take-profit
//!   <price> [--order-client-id <stem>] --node <host:port> [--yes] [--json] [--no-wait] [--reason <text>]` —
//!   ONE TP/SL bracket, the `CommandWire::Bracket` the desktop's TP/SL ticket sends, book REQUIRED like
//!   `submit`'s (it opens risk). A verb of its own rather than `submit --stop-loss/--take-profit` because it
//!   is a different wire command whose one id is a group STEM, with no `--reduce-only` and no `--type`:
//!   folding it into `submit` would need refusals for flags that mean nothing together, and a `Verb::Submit`
//!   that is sometimes not one. Where the node offers `bracket-stem` the client mints the STEM
//!   (`vike_client::verbs::fill_order_client_id`) or `--order-client-id` pins it (a retry's, judged by
//!   `vike_client::verbs::bracket_stem_refusal`), the preview prints it as `group=<stem>` and the core derives
//!   the three legs' ids from it, so a retry under the same stem is refused whole by the door; an older node's
//!   core mints them itself. Either way the legs' ids are made AFTER the node's answer, so the answer's
//!   `outcome:` look (`crate::cmd::trade::oneshot`'s `Target::of`) is what prints them, with the core's own
//!   refusal words when a venue cannot hold a leg.
//! - `oco` / `oto` / `ladder` — ONE order list (`CommandWire::OrderList`), book REQUIRED like `submit`'s;
//!   `crate::cmd::trade::order::list`'s module doc has the three grammars.
//! - `combo` — ONE combo (`CommandWire::Combo`), book REQUIRED; `crate::cmd::trade::order::combo`'s module doc has it.
//! - `cancel <order client id> --node … [--yes] [--json] [--reason <text>]` — no book: an order's own
//!   order-client-id already names the engine that owns it, so there is nothing left to address.
//! - `modify <order client id> [--qty Q] [--price P] [--trigger P] --node … […]` — same reasoning as `cancel`.
//! - `mass-cancel [<account>] [<symbol>] --node … […]` — book OPTIONAL: wider is safer for a
//!   risk-REDUCING act, so naming nothing reaches every account of every venue this node runs.
//! - `decode <id> [--node …] [--json]` — a READ, no node needed: what an order client id says, in
//!   words; `crate::cmd::trade::order::decode`'s module doc has it.
//!
//! `clap` reads the argv (the plane's one parser, `crate::cmd::trade`'s `Cli`: [`OrderVerb`] is this
//! group's enum); every one of the four then resolves to a [`vike_client::verbs::Verb`] through its
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
//!   (`crates/vike-tradehub-client/src/tradehub_control.rs`'s `required_feature`) until a node
//!   advertises `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_ACCOUNT_SCOPED_REDUCE`,
//!   reported as [`crate::exit::Exit::Refused`]. A BARE venue names NO account: it owes
//!   `FEATURE_ONE_ACCOUNT_VENUE`, and the node's door takes it only where the venue has one account, running or not
//!   (decision 0254, [`crate::cmd::trade::oneshot`]'s module doc).
//!
//! `refuse_an_unaddressable_book` (above, in `crate::cmd::trade::selector`) stays READ-side only
//! and is never called by a write verb.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use vike_model::{OrderAttributes, OrderStopAdjust, OrderType};
use vike_tradehub_client::wire::{
    OrderBracketWire, OrderPegWire, OrderReleaseWire, OrderRequestWire,
};

use crate::cmd::args;
use crate::cmd::trade::oneshot::{self, WrapperArgs};
use crate::cmd::trade::render::{emulated_rows, emulated_table, order_rows, rows_json, rows_table};
use crate::cmd::trade::selector;
use crate::exit::CliError;
use vike_client::hubkeys::HubKeyring;
use vike_client::refusals::SubmitRuleRefusalId;
use vike_client::verbs::{self, Verb};
use vike_model::accounts::account_keys::BOOK_FORMS;

// The group's verbs that are files of their own: the order lists, one combo, the core-held stops and
// the client-id decoder.
pub(super) mod combo;
pub(super) mod decode;
pub(super) mod list;
pub(super) mod stops;

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
#[expect(
    clippy::large_enum_variant,
    reason = "one parsed command line per process; boxing buys nothing"
)]
pub(super) enum OrderVerb {
    #[command(about = "list the working order set from the node's latest snapshot")]
    Ls(selector::LsCommand),
    #[command(
        about = "list the reduce-only stops the NODE holds (not the venue): armed, held, re-armed",
        long_about = stops::PROSE
    )]
    Stops(stops::StopsCommand),
    #[command(
        about = "place one order — the book is REQUIRED (an act that adds risk names exactly one)",
        long_about = submit_prose(),
        override_usage = "vike-cli trade order submit <ACCOUNT> <SYMBOL> <buy|sell> <QTY> \
                          [@PRICE|@market] [OPTIONS]"
    )]
    Submit(SubmitCommand),
    #[command(
        about = "place one TP/SL bracket: an entry with a stop-loss and a take-profit",
        long_about = bracket_prose(),
        override_usage = "vike-cli trade order bracket <ACCOUNT> <SYMBOL> <buy|sell> <QTY> \
                          [@PRICE|@market] --stop-loss <PRICE> --take-profit <PRICE> [OPTIONS]"
    )]
    Bracket(BracketCommand),
    #[command(
        about = "place one OCO pair: a limit and a stop, the first to fill cancels the other",
        long_about = list::oco_prose(),
        override_usage = "vike-cli trade order oco <ACCOUNT> <SYMBOL> <buy|sell> <QTY> --limit <PRICE> \
                          --stop <PRICE> [--stop-limit <PRICE>] [--reduce-only] [OPTIONS]"
    )]
    Oco(list::OcoCommand),
    #[command(
        about = "place one OTO: an entry and one exit held until it fills",
        long_about = list::oto_prose(),
        override_usage = "vike-cli trade order oto <ACCOUNT> <SYMBOL> <buy|sell> <QTY> \
                          [@PRICE|@market] (--stop <PRICE> | --limit <PRICE>) [OPTIONS]"
    )]
    Oto(list::OtoCommand),
    #[command(
        about = "place one ladder: an entry, one stop for its whole size and partial targets",
        long_about = list::ladder_prose(),
        override_usage = "vike-cli trade order ladder <ACCOUNT> <SYMBOL> <buy|sell> <QTY> \
                          [@PRICE|@market] --stop-loss <PRICE> --take-profit <PRICE:QTY>... [OPTIONS]"
    )]
    Ladder(list::LadderCommand),
    #[command(
        about = "place one combo: two legs or more traded as one order, priced by its net or by its legs",
        long_about = combo::combo_prose(),
        override_usage = "vike-cli trade order combo <ACCOUNT> <buy|sell> <QTY> --leg <SYMBOL:RATIO[@LIMIT]>... \
                          [--net <PRICE>] [OPTIONS]"
    )]
    Combo(combo::ComboCommand),
    #[command(
        about = "cancel one order by its order-client-id",
        long_about = CANCEL_PROSE,
        override_usage = "vike-cli trade order cancel <ORDER CLIENT ID> [OPTIONS]"
    )]
    Cancel(CancelCommand),
    #[command(
        about = "change one resting order's qty and/or price",
        long_about = MODIFY_PROSE,
        override_usage = "vike-cli trade order modify <ORDER CLIENT ID> [OPTIONS]"
    )]
    Modify(ModifyCommand),
    #[command(
        about = "cancel every working order; a named account needs a node serving account-scoped-reduce",
        long_about = mass_cancel_prose(),
        override_usage = "vike-cli trade order mass-cancel [ACCOUNT] [SYMBOL] [OPTIONS]"
    )]
    MassCancel(MassCancelCommand),
    #[command(
        about = "read an order client id back in words: who placed it, for what, in which group",
        long_about = decode::PROSE,
        override_usage = "vike-cli trade order decode <ID> [--node <HOST:PORT>] [--json]"
    )]
    Decode(decode::DecodeCommand),
}

/// Entry point [`crate::cmd::trade::run`] routes `trade order …` to, with the verb `clap` read.
/// `policy_max_notional` is threaded straight through to the WRITE verbs' guardrail — see
/// [`oneshot::WriteContext`] — and `ack_wait` (`config.node_ack_wait_ms`) with it, to the wait for the
/// node's verdict on the command a write verb sends. `settings_dir` is where `decode` looks for this
/// box's `strategy_code` rows.
pub(super) fn run(
    verb: OrderVerb,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
    settings_dir: Option<&Path>,
) -> ExitCode {
    match verb {
        OrderVerb::Decode(c) => decode::run(c, keys, settings_dir),
        OrderVerb::Ls(cmd) => match cmd.into_args(keys.node_addr()) {
            Ok(a) => run_ls(a, keys),
            Err(msg) => args::refuse(BIN, msg),
        },
        OrderVerb::Stops(c) => stops::run_stops(c, keys),
        OrderVerb::Submit(c) => oneshot::run_verb(
            COMMAND,
            "submit",
            submit_verb(&c),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::Bracket(c) => oneshot::run_verb(
            COMMAND,
            "bracket",
            bracket_verb(&c),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::Oco(c) => oneshot::run_verb(
            COMMAND,
            "oco",
            list::oco_verb(&c),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::Oto(c) => oneshot::run_verb(
            COMMAND,
            "oto",
            list::oto_verb(&c),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::Ladder(c) => oneshot::run_verb(
            COMMAND,
            "ladder",
            list::ladder_verb(&c),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        OrderVerb::Combo(c) => oneshot::run_verb(
            COMMAND,
            "combo",
            combo::combo_verb(&c),
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
            modify_verb(
                &c.ids,
                (c.qty.as_deref(), c.price.as_deref(), c.trigger.as_deref()),
                c.wrap.reason().as_deref(),
            ),
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
    let snap = match crate::cmd::trade::observe_read(COMMAND, &a.node, keys) {
        Ok(snap) => snap,
        Err(exit) => return exit,
    };
    let mut rows = order_rows(&snap, a.book.as_ref(), a.symbol.as_deref());
    let mut emulated = emulated_rows(&snap, a.book.as_ref(), a.symbol.as_deref());
    if let Some(book) = a.named_account() {
        if let Err(e) = selector::refuse_an_unaddressable_book(book, &rows) {
            eprintln!("vike-cli {COMMAND}: {}", e.message);
            return e.exit.into();
        }
        rows.retain(|r| r.account.as_deref() == book.label.text());
        emulated.retain(|e| e.account.as_deref() == book.label.text());
    }
    if a.json {
        println!("{}", rows_json(&rows, &emulated));
    } else {
        // Echo the book as it narrowed — human mode only, so `--json` stays a bare document a script
        // can pipe straight into `jq`.
        if let Some(line) = a.book_line() {
            println!("{line}");
        }
        println!("{}", rows_table(&rows));
        // Every order the node emulates, once, below the venue's orders; nothing when it emulates none.
        if !emulated.is_empty() {
            println!("{}", emulated_table(&emulated));
        }
    }
    ExitCode::SUCCESS
}

// ---- the WRITE verbs (task 7) ----------------------------------------------------------------

// `submit`'s tokens and flags. The order-shape tokens are `<account> <symbol> <buy|sell> <qty>` and
// an optional `@<price>`/`@market`, in the order written; [`submit_verb`] sorts the `@` token out
// wherever it sits, so the `clap` positional is one list of raw tokens.
#[derive(clap::Args, Debug)]
pub(super) struct SubmitCommand {
    #[arg(value_name = "TOKEN", help = "<account> <symbol> <buy|sell> <qty> [@<price>|@market]")]
    tokens: Vec<String>,
    #[arg(long, help = "the order may only reduce an open position")]
    reduce_only: bool,
    #[arg(
        long,
        value_name = "ID",
        help = "pin the order client id (1..=32 alphanumeric characters)"
    )]
    order_client_id: Option<String>,
    // `type` is a Rust keyword, hence the field name. The three words are the trigger kinds of
    // `vike_model::OrderType::is_trigger`, hyphenated as a flag value; a limit or market order is
    // still spelled by `@price` / `@market` and this flag does not take those words.
    #[arg(
        long = "type",
        value_name = "KIND",
        help = "a native trigger order: stop | stop-limit | take-profit (needs --trigger), trailing | trailing-limit (needs --trail or --trail-percent; --trigger is its optional activation), trailing-take-profit (needs --trail or --trail-percent; no --trigger, no @<price>), market-to-limit (no --trigger, no @<price>), or pegged (needs --peg; no --trigger, no @<price>)"
    )]
    kind: Option<String>,
    #[arg(
        long,
        value_name = "PRICE",
        help = "the trigger price of a --type order (a trailing order's activation)"
    )]
    trigger: Option<String>,
    #[arg(
        long,
        value_name = "PRICE",
        help = "a --type trailing, trailing-limit or trailing-take-profit order's trail: the distance in price its stop (a trailing take-profit's trigger) keeps from the extreme price it tracks"
    )]
    trail: Option<String>,
    #[arg(
        long,
        value_name = "P",
        help = "a --type trailing, trailing-limit or trailing-take-profit order's trail as a PERCENT of the extreme price it tracks, above 0 and below 100, in place of --trail"
    )]
    trail_percent: Option<String>,
    #[arg(
        long,
        value_name = "PRICE",
        help = "a --type trailing-limit order's limit offset in price, zero or more: its limit is the stop less it for a sell, plus it for a buy; a --type trailing-take-profit order's (optional: with it the order fires as a limit that far beyond its trigger); or a --type pegged order's distance in price from its anchor, positive aggressive (default 0)"
    )]
    offset: Option<String>,
    #[arg(
        long,
        value_name = "ANCHOR",
        help = "a --type pegged order's anchor, the price it is kept at (by the venue, or by the node where the venue holds no such peg): best | mid | market | primary"
    )]
    peg: Option<String>,
    // The LIFETIME of a limit order (`@<price>`). Three flags, one rule (`lifetime_flags`):
    // `--time-in-force` is the venue-neutral set the model has, `--expires` is the end of a `gtd`
    // order, and `--post-only` is its own lifetime (maker-only), so it does not combine with
    // `--time-in-force`.
    #[arg(
        long,
        value_name = "WORD",
        help = "how long a limit order lives: day | gtc | ioc | fok | gtd, or an auction for a market or limit order: at-open | at-close (at-open also for a --type market-to-limit; default gtc; a venue that cannot do it refuses by name)"
    )]
    time_in_force: Option<String>,
    // The retired spelling of `--time-in-force`, refused by name before any other rule.
    #[arg(long = "tif", hide = true, value_name = "WORD")]
    retired_tif: Option<String>,
    #[arg(
        long,
        value_name = "WHEN",
        help = "when a --time-in-force gtd order ends: a UTC instant like 2026-11-01T12:00:00Z, or epoch milliseconds"
    )]
    expires: Option<String>,
    #[arg(
        long,
        help = "a maker-only limit order: the venue refuses it, never executes it, if it would take liquidity (needs @<price>)"
    )]
    post_only: bool,
    #[arg(
        long,
        value_name = "QTY",
        help = "the quantity the venue SHOWS of a limit order (an iceberg's visible slice): zero (a HIDDEN order) or more, below the order's qty"
    )]
    display: Option<String>,
    // A limit order's ATTRIBUTES (`attributes_flag`): how it fills, never what kind it is.
    #[arg(
        long,
        help = "a limit order that fills whole in one execution or not at all: it rests until it can (all-or-none at once is --time-in-force fok)"
    )]
    all_or_none: bool,
    #[arg(
        long,
        help = "a limit order that takes every venue's liquidity at once up to its price (not with --post-only)"
    )]
    sweep_to_fill: bool,
    #[arg(
        long,
        value_name = "AMOUNT",
        help = "a limit order's discretionary amount, above zero: the market sees @<price>, and the order executes up to that price plus it for a buy, less it for a sell"
    )]
    discretion: Option<String>,
    // A stop's ADJUSTMENT (`adjust_flag`): what a `--type stop|stop-limit` order becomes, once, at a level.
    #[arg(
        long,
        value_name = "PRICE",
        help = "a --type stop or stop-limit order's adjustment level, on the protected side of its stop (a sell stop's above it): once the price reaches it, the order becomes --adjust-to, once"
    )]
    adjust_at: Option<String>,
    #[arg(
        long,
        value_name = "TARGET",
        help = "what an adjusted stop becomes at --adjust-at: stop (with --adjust-stop) | stop-limit (with --adjust-stop and --adjust-limit) | trailing (with --adjust-stop and --adjust-trail or --adjust-trail-percent)"
    )]
    adjust_to: Option<String>,
    #[arg(
        long,
        value_name = "PRICE",
        help = "the adjusted order's stop price, on the near side of --adjust-at (a sell's below it)"
    )]
    adjust_stop: Option<String>,
    #[arg(long, value_name = "PRICE", help = "an --adjust-to stop-limit order's limit price")]
    adjust_limit: Option<String>,
    #[arg(long, value_name = "PRICE", help = "an --adjust-to trailing order's trail, in price")]
    adjust_trail: Option<String>,
    #[arg(
        long,
        value_name = "P",
        help = "an --adjust-to trailing order's trail as a percent, above 0 and below 100, in place of --adjust-trail"
    )]
    adjust_trail_percent: Option<String>,
    #[arg(
        long,
        value_name = "WHEN",
        help = "hold the order on the node and send it at this moment: a UTC instant like 2026-11-01T12:00:00Z, or epoch milliseconds"
    )]
    at: Option<String>,
    #[arg(
        long = "when-price",
        value_name = "COND",
        help = "with --at, hold the order also until the price is above:<price> or below:<price>; above:<SYMBOL>:<price> reads another symbol's price (on another venue with --when-price-on)"
    )]
    when_price: Option<String>,
    #[arg(
        long = "when-price-on",
        value_name = "VENUE",
        help = "with --when-price, the venue whose price it reads when it is not the order's own: a venue id, exact and lower case (binance, okx, ...); the condition names its symbol, e.g. above:BTCUSDT:100000"
    )]
    when_price_on: Option<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `bracket`'s tokens and flags. The order-shape tokens are `submit`'s `<account> <symbol>
// <buy|sell> <qty>` and an optional `@<price>`/`@market` (the entry), in the order written; both
// exits are FLAGS so a stop-loss and a take-profit cannot be swapped by position.
#[derive(clap::Args, Debug)]
pub(super) struct BracketCommand {
    #[arg(value_name = "TOKEN", help = "<account> <symbol> <buy|sell> <qty> [@<price>|@market]")]
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
    #[arg(
        long,
        value_name = "STEM",
        help = "pin the bracket's group stem, the `group=<stem>` a first send showed, so a retry names the same \
                three legs and the node refuses it whole if they exist; omitted, the client mints one"
    )]
    order_client_id: Option<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `cancel`'s one token.
#[derive(clap::Args, Debug)]
pub(super) struct CancelCommand {
    #[arg(value_name = "ORDER_CLIENT_ID", help = "the order client id to cancel")]
    ids: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `modify`'s order client id and its changes.
#[derive(clap::Args, Debug)]
pub(super) struct ModifyCommand {
    #[arg(value_name = "ORDER_CLIENT_ID", help = "the order client id to change")]
    ids: Vec<String>,
    #[arg(long, value_name = "Q", help = "the new quantity")]
    qty: Option<String>,
    #[arg(long, value_name = "P", help = "the new price")]
    price: Option<String>,
    #[arg(
        long,
        value_name = "P",
        help = "the new trigger price of a stop, stop-limit or take-profit order"
    )]
    trigger: Option<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `mass-cancel`'s optional book and symbol.
#[derive(clap::Args, Debug)]
pub(super) struct MassCancelCommand {
    #[arg(
        value_name = "ACCOUNT|SYMBOL",
        help = "[<account>] [<symbol>] — the account is OPTIONAL"
    )]
    tokens: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// The shape line of a refusal for the wrong NUMBER of tokens (the verb's arity rule is
/// [`submit_verb`]'s own, not `clap`'s, because the `@price` token may sit among the positionals).
const SUBMIT_SHAPE: &str = "usage: vike-cli trade order submit <account> <symbol> <buy|sell> \
                            <qty> [@<price>|@market] [--reduce-only] [--order-client-id <id>] \
                            [--type stop|stop-limit|take-profit --trigger <price> | --type \
                            trailing|trailing-limit --trail <price>|--trail-percent <p> [--offset \
                            <price>] [--trigger <activation>] | --type trailing-take-profit --trail \
                            <price>|--trail-percent <p> [--offset <price>] | --type market-to-limit \
                            | --type pegged --peg best|mid|market|primary [--offset <price>]] \
                            [--adjust-at <price> --adjust-to stop|stop-limit|trailing --adjust-stop \
                            <price> [--adjust-limit <price> | --adjust-trail <price> | \
                            --adjust-trail-percent <p>]] \
                            [--time-in-force day|gtc|ioc|fok|gtd|at-open|at-close [--expires \
                            <when>] | --post-only] [--display <qty>] [--all-or-none] \
                            [--sweep-to-fill] [--discretion <amount>] [--at <when> [--when-price \
                            above|below:[<symbol>:]<price> [--when-price-on <venue>]]] \
                            --node <host:port> [--yes] [--json] [--reason <text to end of line>]";

/// `submit`'s long help, and the second half of its arity refusal ([`submit_usage`]). A function
/// rather than a `const`, so the forms it names come from [`BOOK_FORMS`] itself (stable Rust
/// has no `const`-time string concatenation without pulling in a crate to do it) — two of this
/// file's own tests exist because a hand-typed COPY of that list once fell out of step with it.
fn submit_prose() -> String {
    format!(
        "<account> is REQUIRED: an order goes to exactly one venue account. {} The forms are: {}\n\n\
         Naming an account needs a node advertising `account-scoped-submit`; a node without it \
         may discard the account, so the submit is refused here before anything is sent (exit \
         4) — upgrade the node.\n\n\
         A native trigger order is `--type stop|stop-limit|take-profit --trigger <price>`: the two \
         flags come together, `stop-limit` also takes the limit price as `@<price>`, \
         `take-profit` may take one (a limit-if-touched; without it the order is market-if-touched), \
         and `stop` takes none. `--type market-to-limit` takes neither flag nor price: a market \
         order whose unfilled remainder rests as a limit at the price of its first fill. Without \
         `--type` the order is a limit (`@<price>`) or a market order, as before.\n\n\
         A TRAILING stop is `--type trailing --trail <price>`: its stop keeps that distance, in \
         price, from the best price since it activated, and fires as a market order; \
         `--trail-percent <p>` in place of `--trail` keeps that percent of the best price instead \
         (above 0 and below 100). `--type trailing-limit` also takes `--offset <price>` (zero or \
         more): its limit is the stop less the offset for a sell, plus it for a buy. Neither takes \
         an `@<price>`; `--trigger <price>` is the optional ACTIVATION (nothing trails until the \
         price reaches it). A modify changes a trailing order's size only. Naming a trail needs a \
         node advertising `order-trailing`: against an older node the submit is refused here \
         before anything is sent (exit 4).\n\n\
         A TRAILING TAKE-PROFIT is `--type trailing-take-profit --trail <price>` (or \
         `--trail-percent <p>`): a sell tracks the lowest price since it rests and fires when the \
         price rises its trail above it (a buy mirrors it), as a market order, or with `--offset \
         <price>` as a limit that far beyond its trigger. It takes no `@<price>` and no \
         `--trigger`: an activation would run the other way from a trailing stop's, and this \
         build refuses one.\n\n\
         An ADJUSTABLE stop is a `--type stop` or `stop-limit` order with `--adjust-at <price> \
         --adjust-to stop|stop-limit|trailing --adjust-stop <price>`: once the price reaches the \
         level, on the protected side of the stop (a sell stop's above it, a buy stop's below), \
         the order becomes the target, once: a stop at `--adjust-stop`, a stop-limit with \
         `--adjust-limit`, or a trailing stop from `--adjust-stop` by `--adjust-trail` or \
         `--adjust-trail-percent`. The target's stop sits on the near side of the level.\n\n\
         A PEGGED order is `--type pegged --peg best|mid|market|primary [--offset <price>]`: it is \
         kept at its own side of the touch (`best`), the midpoint (`mid`), the far side \
         (`market`) or its own side of the national best bid and offer (`primary`), moved by the \
         offset (default 0; positive is aggressive, a buy pays more). It takes no `@<price>` and no \
         `--trigger`. A venue that holds the peg keeps it itself; on any other the node EMULATES \
         it, one plain limit at a time re-priced on every move of its anchor, and the order is \
         labelled emulated. An IOC or FOK lifetime, polymarket and aster are still refused by \
         name. Naming a peg needs a node advertising `order-peg`: against an older node the submit \
         is refused here before anything is sent (exit 4).\n\n\
         A limit order's LIFETIME is `--time-in-force day|gtc|ioc|fok|gtd` (default gtc; `gtd` \
         also takes `--expires`, a UTC instant like 2026-11-01T12:00:00Z or epoch milliseconds) \
         or `--post-only`, a maker-only order that the venue REFUSES, and never executes, if it \
         would take liquidity. A lifetime belongs to a limit order (`@<price>`); a venue that \
         cannot do the one you name refuses it by name, never sends a plain limit order in its \
         place. Naming one needs a node advertising `order-tif`: against an older node the \
         submit is refused here before anything is sent (exit 4). `--time-in-force \
         at-open|at-close` is an AUCTION: a market or limit order that takes part once, in the \
         auction that opens or closes the next session; `--time-in-force at-open` also takes a \
         `--type market-to-limit`, whose remainder rests as a limit at the auction's price.\n\n\
         `--display <qty>` shows only that much of a limit order at the venue (an iceberg): \
         zero or more and below the order's qty (0 is a HIDDEN order). A venue that shows a \
         visible size takes it natively; on any other the node EMULATES it, one plain limit of \
         the visible size at a time at the order's price, and the order is labelled emulated. A \
         hidden order, an IOC or FOK lifetime, polymarket and aster are still refused by name. A \
         limit order's ATTRIBUTES: `--all-or-none` fills \
         whole in one execution or not at all (it rests, so not with `--time-in-force ioc|fok`), \
         `--sweep-to-fill` takes every venue's liquidity at once up to its price (not with \
         `--post-only`), and `--discretion <amount>` lets an order the market sees at \
         `@<price>` execute up to that price plus the amount for a buy, less it for a sell; a \
         venue that cannot do one refuses the order by name. `--at <when>` holds the order on \
         the node and sends it then (a UTC instant or epoch milliseconds, in the future); \
         `--when-price above:<price>` or `below:<price>` beside it holds it also until the price \
         is there (`above:ETHUSDT:3500` reads another symbol the account's engine trades), and \
         `--when-price-on <venue>` reads it on another venue (a venue id, exact and lower case; \
         the condition names its symbol, `above:BTCUSDT:100000`, since the venues spell one \
         instrument differently, and the node refuses a venue none of its accounts trades it \
         on). Each needs a node advertising its word (`order-display`, `order-release`; \
         `order-attributes` for an attribute, a percent trail, an adjustment or a price venue): \
         against an older node the submit is refused here before anything is sent (exit 4).\n\n\
         ⚠ --reason must be LAST — everything after it, to end of line, is taken verbatim as the \
         recorded rationale.",
        selector::ONE_ACCOUNT_RULE,
        BOOK_FORMS
    )
}

fn submit_usage() -> String {
    format!("{SUBMIT_SHAPE}\n\n{}", submit_prose())
}

const CANCEL_SHAPE: &str = "usage: vike-cli trade order cancel <order client id> --node <host:port> [--yes] \
                            [--json] [--reason <text to end of line>]";

const CANCEL_PROSE: &str = "No book: the order-client-id already names the engine that owns it.";

fn cancel_usage() -> String {
    format!("{CANCEL_SHAPE}\n\n{CANCEL_PROSE}")
}

const MODIFY_SHAPE: &str = "usage: vike-cli trade order modify <order client id> [--qty Q] [--price P] \
                            [--trigger P] --node <host:port> [--yes] [--json] [--reason <text to \
                            end of line>]";

const MODIFY_PROSE: &str = "No book, same reason as `cancel`. At least one of \
                            --qty/--price/--trigger is required. --price is the TRIGGER of a stop \
                            and the limit price of a limit order. --trigger is the trigger price \
                            of a stop, stop-limit or take-profit (on a stop it wins over --price); \
                            the node refuses it on an order with no trigger, and it needs a node \
                            advertising `order-modify-trigger` (exit 4 against an older one). A \
                            trailing order takes --qty only: the node refuses --price and --trigger \
                            on one (cancel it and place a new one). A \
                            venue that cannot change a resting order in place has \
                            it cancelled and placed again under a new order client id that stays \
                            linked to this one (`cancel <order client id>` and `modify <order client id>` keep reaching \
                            it): a reduce-only stop is placed FIRST and the old one cancelled once \
                            the new one is acknowledged, so the position is never without a stop; \
                            a limit order is cancelled first and LOSES ITS PLACE IN THE QUEUE. A \
                            refused change leaves the old order standing and the node says why in \
                            its recent events.";

fn modify_usage() -> String {
    format!("{MODIFY_SHAPE}\n\n{MODIFY_PROSE}")
}

const MASS_CANCEL_SHAPE: &str = "usage: vike-cli trade order mass-cancel [<account>] [<symbol>] \
                                 --node <host:port> [--yes] [--json] [--reason <text to end of \
                                 line>]";

/// See [`submit_prose`] for why this is a function rather than a `const`. Written against the
/// CAPABILITIES, so it stays true of whichever node answers.
fn mass_cancel_prose() -> String {
    format!(
        "<account> is OPTIONAL. Naming none is the panic button's sibling: every account of every \
         venue this node runs, and no capability gate refuses it. {} A book needs a node \
         advertising `one-account-venue` (bare) or `account-scoped-reduce` (naming an account): \
         against a node that does not, it is refused here before anything is sent (exit 4). The \
         forms are: {}",
        selector::ONE_ACCOUNT_RULE,
        BOOK_FORMS
    )
}

fn mass_cancel_usage() -> String {
    format!("{MASS_CANCEL_SHAPE}\n\n{}", mass_cancel_prose())
}

/// `submit <account> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--order-client-id <id>]` —
/// the order-shape rules ONLY over what `clap` read into [`SubmitCommand`] (`--node`/`--yes`/`--json`/`--reason`
/// are its [`WrapperArgs`]; every other flag is a raw field, judged by the helper the REPL calls too).
/// `tokens` are the positionals in the order written, `@price` token included wherever it sat.
/// Mirrors `crate::cmd::trade`'s own `parse_submit` with the book moved to the front — the
/// risk-direction law makes it REQUIRED here. PURE: no network.
///
/// The order client id is left EMPTY here, as the REPL's own parser leaves it (`--order-client-id` pins one):
/// [`oneshot::execute_write`] mints it before the preview ([`verbs::fill_order_client_id`]), off the
/// node's frame, so the id shown is the id sent and it carries the target account's shape and code.
///
/// An order that cannot be SIZED — a qty of zero, below or non-finite, a non-finite `@price` — is a
/// usage error here ([`verbs::unsizeable_submit_refusal`], the function the REPL's `parse_submit` and
/// the MCP `submit_order` tool call too), refused before the node is asked. A sizeable order over
/// every cap still parses: the write judges a cap (an order over the notional cap is not sent).
fn submit_verb(c: &SubmitCommand) -> Result<Verb, CliError> {
    let usage = || CliError::usage(submit_usage());
    if c.retired_tif.is_some() {
        return Err(CliError::usage(super::plane::renamed("--tif", "--time-in-force")));
    }
    // `--reason` takes EVERYTHING after it, flags included (`oneshot`'s `WrapperArgs`), so any flag
    // of this verb written after it is rationale text and never reaches the order: a `--type` or
    // `--trigger` would send a plain market order, and a dropped `--reduce-only` would OPEN risk.
    // Refused rather than sent: the operator meant the flag.
    oneshot::refuse_swallowed_flag::<SubmitCommand>(
        "submit",
        "the order",
        c.wrap.reason().as_deref(),
    )?;

    let mut positional: Vec<&str> = Vec::new();
    let mut price_token: Option<&str> = None;
    for token in &c.tokens {
        if let Some(p) = token.strip_prefix('@') {
            if price_token.is_some() {
                return Err(CliError::usage("submit: more than one @price token".to_string()));
            }
            price_token = Some(p);
        } else {
            positional.push(token);
        }
    }
    let order_client_id = match c.order_client_id.as_deref() {
        None => String::new(),
        Some(id) => match verbs::order_client_id_refusal(id) {
            None => id.to_string(),
            Some(refusal) => {
                return Err(CliError::usage(format!("submit: {}", refusal.rendered())));
            }
        },
    };
    // <account> <symbol> <buy|sell> <qty> — exactly four positionals, account first. A count that
    // would have satisfied the OLD (book-less) grammar exactly (three positionals) is the single
    // most likely authoring mistake, so the same usage error covers it and names the book forms.
    if positional.len() != 4 {
        return Err(usage());
    }
    let (venue, account) = selector::addressed(positional[0])?;
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
    let (order_type, price) = match price_token {
        None | Some("market") => ("market", None),
        Some(p) => {
            let px: f64 =
                p.parse().map_err(|_| CliError::usage(format!("submit: not a price: @{p}")))?;
            ("limit", Some(px))
        }
    };
    let trigger = trigger_order(c.kind.as_deref(), c.trigger.as_deref(), price.is_some())?;
    let order_type = trigger.map_or(order_type, |(word, _)| word);
    let (time_in_force, gtd_expiry) =
        lifetime_flags(c.time_in_force.as_deref(), c.expires.as_deref(), c.post_only)?;
    let release =
        release_flag((c.at.as_deref(), c.when_price.as_deref(), c.when_price_on.as_deref()))?;
    let display_qty = display_flag(c.display.as_deref())?;
    let (trail, limit_offset, trail_percent) = trail_flags(
        (c.trail.as_deref(), c.offset.as_deref(), c.trail_percent.as_deref()),
        order_type,
    )?;
    let peg = peg_flag((c.peg.as_deref(), c.offset.as_deref()))?;
    let attributes = attributes_flag((c.all_or_none, c.sweep_to_fill, c.discretion.as_deref()))?;
    let adjust = adjust_flag(AdjustTerms {
        at: c.adjust_at.as_deref(),
        to: c.adjust_to.as_deref(),
        stop: c.adjust_stop.as_deref(),
        limit: c.adjust_limit.as_deref(),
        trail: c.adjust_trail.as_deref(),
        trail_percent: c.adjust_trail_percent.as_deref(),
    })?;

    let req = OrderRequestWire {
        order_client_id,
        venue,
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: order_type.to_string(),
        price,
        trigger_price: trigger.and_then(|(_, px)| px),
        reduce_only: c.reduce_only,
        time_in_force,
        gtd_expiry,
        // A bare venue names none, and the node's door takes it only where the venue has one
        // account (`selector::addressed`, `oneshot`'s module doc).
        account,
        display_quantity: display_qty,
        release,
        trail,
        limit_offset,
        peg,
        attributes,
        trail_percent,
        adjust,
    };
    // The REPL's refusal, by the same function and in this surface's own error type: an order that
    // cannot be SIZED is a usage error (exit 2) before a key resolves or a node is asked, so
    // `parse::<f64>` accepting `nan`/`inf` can no longer carry one into the preview.
    if let Some(why) = verbs::unsizeable_submit_refusal(&req) {
        return Err(CliError::usage(format!("submit: {why}")));
    }
    if let Some(why) = submit_rules_usage(&req) {
        return Err(CliError::usage(why));
    }
    Ok(Verb::Submit(req))
}

/// The one rule over a built submit's terms (`vike_client::verbs::submit_rules_refusal`: a trail off a trailing kind,
/// its prices, its display, its attributes, a stop's adjustment, its lifetime, its release, in that order, each the
/// `vike_model` rule the node's door applies again) as this door shows it, `submit: error [<id>]: <text>`, the text
/// opening with the flag it judged where the rule is one flag's (`--display`, `--at`), and for a lifetime named on an
/// order that is no limit (an auction aside) ending where this grammar puts a limit's price. The one-shot
/// `submit_verb` and the REPL's `parse_submit` each call it beside `verbs::unsizeable_submit_refusal`. PURE but for
/// the clock.
pub(super) fn submit_rules_usage(req: &OrderRequestWire) -> Option<String> {
    use vike_model::TimeInForce;
    let refusal = verbs::submit_rules_refusal(req)?;
    let tif =
        req.time_in_force.as_deref().and_then(|w| TimeInForce::parse(w).ok()).unwrap_or_default();
    let off_a_limit = tif != TimeInForce::Gtc
        && !tif.is_auction()
        && OrderType::parse(&req.order_type) != Ok(OrderType::Limit);
    let hint = if refusal.id() == SubmitRuleRefusalId::Lifetime && off_a_limit {
        " (a lifetime needs a limit order: give @<price>)"
    } else {
        ""
    };
    // The flag the rule judged, where it is one flag's (this dialect's half: the MCP tool names its argument).
    let flag = match refusal.id() {
        SubmitRuleRefusalId::Display => "--display: ",
        SubmitRuleRefusalId::Release => "--at: ",
        _ => "",
    };
    let text = format!("{flag}{refusal}{hint}");
    Some(format!("submit: {}", vike_utility::errors::render_refusal(refusal.id().word(), text)))
}

/// A submit's trail, limit offset and percent trail, in that order ([`trail_flags`]' answer).
pub(super) type TrailTerms = (Option<f64>, Option<f64>, Option<f64>);

/// `--trail` / `--offset` / `--trail-percent` → the wire trail, limit offset and percent trail, or a usage
/// error for a value that is no number. A `--type pegged` order's `--offset` is its peg's ([`peg_flag`]),
/// never a limit offset. Which kind takes them and what the numbers must be (one distance, a percent above 0
/// and below 100) is [`submit_rules_usage`]'s, by the shared rule. The REPL's `parse_submit` calls it too.
/// PURE.
pub(super) fn trail_flags(
    (trail, offset, trail_percent): (Option<&str>, Option<&str>, Option<&str>),
    order_type: &str,
) -> Result<TrailTerms, CliError> {
    // `order_type` is a wire word already (`trigger_order`'s), so the STORED door reads it exactly.
    let offset = offset.filter(|_| OrderType::from_stored(order_type) != OrderType::Pegged);
    let number = |what: &str, flag: &str, raw: Option<&str>| {
        raw.map(|raw| {
            raw.parse::<f64>()
                .map_err(|_| CliError::usage(format!("submit: not a {what}: {flag} {raw:?}")))
        })
        .transpose()
    };
    Ok((
        number("price", "--trail", trail)?,
        number("price", "--offset", offset)?,
        number("percent", "--trail-percent", trail_percent)?,
    ))
}

/// `--all-or-none` / `--sweep-to-fill` / `--discretion <amount>` → the wire attributes, or a usage error for
/// an amount that is no number. `None` unless one is named, so an order naming none is the frame it always
/// was (never an empty `attributes`). What they may ride (a limit order, a lifetime that fits) is
/// [`submit_rules_usage`]'s. The REPL's `parse_submit` calls it too. PURE.
pub(super) fn attributes_flag(
    (all_or_none, sweep_to_fill, discretion): (bool, bool, Option<&str>),
) -> Result<Option<OrderAttributes>, CliError> {
    let discretionary_amount = discretion
        .map(|raw| {
            raw.parse::<f64>().map_err(|_| {
                CliError::usage(format!("submit: not an amount: --discretion {raw:?}"))
            })
        })
        .transpose()?;
    let a = OrderAttributes { all_or_none, sweep_to_fill, discretionary_amount };
    Ok((a != OrderAttributes::default()).then_some(a))
}

/// The six `--adjust-*` values as typed, [`adjust_flag`]'s input: each grammar fills one.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct AdjustTerms<'a> {
    pub(super) at: Option<&'a str>,
    pub(super) to: Option<&'a str>,
    pub(super) stop: Option<&'a str>,
    pub(super) limit: Option<&'a str>,
    pub(super) trail: Option<&'a str>,
    pub(super) trail_percent: Option<&'a str>,
}

/// The `--adjust-*` flags → the wire adjustment of a stop, or a usage error: a target or a target's price
/// without `--adjust-at`, an `--adjust-at` without `--adjust-to`, a value that is no number, and what
/// `vike_client::verbs::stop_adjust_from_terms` refuses (an unknown target, its stop missing, a term of
/// another target). `None` without any. Whether the numbers hold (the level on the protected side of the
/// stop, the target's stop on the near side of it, one trail) is [`submit_rules_usage`]'s. The REPL's
/// `parse_submit` calls it too. PURE.
pub(super) fn adjust_flag(t: AdjustTerms<'_>) -> Result<Option<OrderStopAdjust>, CliError> {
    let usage = |msg: String| CliError::usage(format!("submit: {msg}"));
    let number = |flag: &str, raw: Option<&str>| {
        raw.map(|raw| raw.parse::<f64>().map_err(|_| usage(format!("not a price: {flag} {raw:?}"))))
            .transpose()
    };
    let Some(at) = number("--adjust-at", t.at)? else {
        let named = [t.to, t.stop, t.limit, t.trail, t.trail_percent].iter().any(Option::is_some);
        return if named {
            Err(usage(
                "--adjust-to and its prices need --adjust-at <price>: the level at which the stop \
                 is adjusted"
                    .to_string(),
            ))
        } else {
            Ok(None)
        };
    };
    let to = t.to.ok_or_else(|| {
        usage("--adjust-at needs --adjust-to stop|stop-limit|trailing".to_string())
    })?;
    verbs::stop_adjust_from_terms(
        at,
        to,
        number("--adjust-stop", t.stop)?,
        number("--adjust-limit", t.limit)?,
        number("--adjust-trail", t.trail)?,
        number("--adjust-trail-percent", t.trail_percent)?,
    )
    .map(Some)
    .map_err(|e| usage(format!("--adjust-to: {e}")))
}

/// `--peg <anchor>` / `--offset` → the wire peg, or a usage error: an anchor that is not `best | mid | market |
/// primary` (any case: `vike_client::verbs::peg_of_word`, the MCP tool's reader too), an offset that is no number.
/// No `--offset` is an offset of 0. Which kind takes a peg (`--type pegged`, and it needs one) and what the numbers
/// must be is [`submit_rules_usage`]'s, by the shared rule. The REPL's `parse_submit` calls it too. PURE.
pub(super) fn peg_flag(
    (peg, offset): (Option<&str>, Option<&str>),
) -> Result<Option<OrderPegWire>, CliError> {
    let Some(word) = peg else {
        return Ok(None);
    };
    let offset = offset
        .map(|raw| {
            raw.parse::<f64>()
                .map_err(|_| CliError::usage(format!("submit: not a price: --offset {raw:?}")))
        })
        .transpose()?
        .unwrap_or(0.0);
    verbs::peg_of_word(word, offset)
        .map(Some)
        .map_err(|refusal| CliError::usage(format!("submit: {}", refusal.rendered())))
}

/// `--display <qty>` → the wire display quantity, or a usage error for a value that is no number. What it
/// may be is [`submit_rules_usage`]'s. The REPL's `parse_submit` calls it too. PURE.
pub(super) fn display_flag(raw: Option<&str>) -> Result<Option<f64>, CliError> {
    raw.map(|raw| {
        raw.parse::<f64>().map_err(|_| {
            CliError::usage(format!("submit: not a display quantity: --display {raw:?}"))
        })
    })
    .transpose()
}

/// `--time-in-force` / `--expires` / `--post-only` → the wire lifetime word and the `gtd` expiry
/// (epoch ms), both `None` for the default `gtc` (so an order that names no lifetime is the frame it
/// always was). PURE.
///
/// What it reads, in the order it refuses:
///
/// - `--post-only` is its own lifetime (maker-only) and does not combine with `--time-in-force`;
///   `--time-in-force` takes the lifetimes `day | gtc | ioc | fok | gtd` and the auctions
///   `at-open | at-close`, any case, and nothing else (post-only is not a `--time-in-force` word: one
///   spelling);
/// - `--expires` is a UTC instant (`2026-11-01T12:00:00Z`) or epoch MILLISECONDS (a number small enough to be
///   seconds is refused: a good-till-date order that ends in 1970 is not what anyone typed).
///
/// Whether the pair hangs together with the order's kind (a lifetime other than `gtc` belongs to a limit order,
/// an auction to a market or a limit order, `gtd` needs an expiry in the future and nothing else takes one) is
/// [`submit_rules_usage`]'s, by `vike_model::lifetime_refusal`, the rule the node's door applies again. Whether
/// the VENUE can do it is not asked here: the node's capability row refuses by name. The REPL's
/// `parse_submit` calls it too, so the two grammars cannot come to differ.
pub(super) fn lifetime_flags(
    tif: Option<&str>,
    expires: Option<&str>,
    post_only: bool,
) -> Result<(Option<String>, Option<i64>), CliError> {
    use vike_model::TimeInForce;
    let usage = |msg: String| CliError::usage(format!("submit: {msg}"));
    let lifetime = match (post_only, tif) {
        (true, Some(_)) => {
            return Err(usage(
                "--post-only is a lifetime of its own and does not combine with --time-in-force"
                    .to_string(),
            ));
        }
        (true, None) => TimeInForce::PostOnly,
        (false, None) => TimeInForce::Gtc,
        (false, Some(word)) => match TimeInForce::parse(word) {
            Ok(TimeInForce::PostOnly) => {
                return Err(usage(
                    "post-only is its own flag: write --post-only, not --time-in-force post-only"
                        .to_string(),
                ));
            }
            Ok(t) => t,
            Err(_) => {
                return Err(usage(format!(
                    "--time-in-force {word:?} is not a time in force - expected day, gtc, ioc, \
                     fok, gtd, at-open or at-close"
                )));
            }
        },
    };
    let expiry = expires.map(|when| instant_ms("--expires", when)).transpose().map_err(usage)?;
    let word = (lifetime != TimeInForce::Gtc).then(|| lifetime.as_word().to_string());
    Ok((word, expiry))
}

/// `--at` / `--when-price` → the wire release (hold the order on the node until then, and until the price
/// condition holds). A price condition rides a moment, so `--when-price` needs `--at`
/// (`vike_client::verbs::release_price_from_word` reads its word). `--when-price-on` names the venue that
/// condition reads, so it needs `--when-price`, whose word must name the symbol
/// (`vike_client::verbs::release_venue_from_word`, the roster rule `vike_model::price_source_refusal`); it
/// goes out as typed. `None` without either, so an order that names none is the frame it always was. Whether
/// the moment holds (in the future, and before a `gtd` expiry) is [`submit_rules_usage`]'s, by
/// `vike_model::lifetime_refusal`, the rule the node's door applies again. PURE. The REPL's `parse_submit`
/// calls it too.
pub(super) fn release_flag(
    (at, when_price, when_price_on): (Option<&str>, Option<&str>, Option<&str>),
) -> Result<Option<OrderReleaseWire>, CliError> {
    let usage = |msg: String| CliError::usage(format!("submit: {msg}"));
    if when_price_on.is_some() && when_price.is_none() {
        return Err(usage(
            "--when-price-on needs --when-price <cond>: it names the venue whose price that \
             condition reads"
                .to_string(),
        ));
    }
    let when = match (at, when_price) {
        (None, None) => return Ok(None),
        (None, Some(_)) => {
            return Err(usage(
                "--when-price needs --at <when>: a held order is released at a moment, and the \
                 price condition is checked from then"
                    .to_string(),
            ));
        }
        (Some(when), _) => when,
    };
    let (price, symbol) = when_price
        .map(verbs::release_price_from_word)
        .transpose()
        .map_err(|e| usage(format!("--when-price: {e}")))?
        .unzip();
    let symbol = symbol.flatten();
    let venue = when_price_on
        .map(|venue| verbs::release_venue_from_word(venue, symbol.as_deref()))
        .transpose()
        .map_err(|e| usage(format!("--when-price-on: {e}")))?;
    let at = instant_ms("--at", when).map_err(usage)?;
    Ok(Some(OrderReleaseWire { at, price, symbol, venue }))
}

/// `--expires` / `--at` (`flag`): epoch MILLISECONDS or a UTC instant (`YYYY-MM-DDTHH:MM:SSZ`). A bare
/// number under 10^11 is seconds (10^11 ms is March 1973), and is refused rather than read as a 1970 instant.
fn instant_ms(flag: &str, when: &str) -> Result<i64, String> {
    let when = when.trim();
    if !when.is_empty() && when.bytes().all(|b| b.is_ascii_digit()) {
        return match when.parse::<i64>() {
            Ok(ms) if ms >= 100_000_000_000 => Ok(ms),
            Ok(_) => Err(format!(
                "{flag} {when} looks like epoch SECONDS; give milliseconds, or a UTC instant like \
                 2026-11-01T12:00:00Z"
            )),
            Err(_) => Err(format!("{flag} {when} is not a usable number")),
        };
    }
    vike_model::parse_utc_timestamp_ms(when).ok_or_else(|| {
        format!(
            "{flag} {when:?} is not a UTC instant like 2026-11-01T12:00:00Z (the Z is required) \
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
/// carries only the trigger, so an `@price` with it is refused rather than passed on for a venue
/// adapter to read as it likes. `take-profit` takes the `@price` or not: with it the order is a
/// limit-if-touched, without it a market-if-touched (`vike_model::OrderRequest`'s docs). The word
/// is the platform's own (`OrderType::as_str`); the flag value is hyphenated, and the underscore
/// spelling the node prints is accepted too. A non-finite trigger is left to
/// [`verbs::unsizeable_submit_refusal`], and a limit at zero or below on a trigger kind to
/// [`submit_rules_usage`], which `submit_verb` calls next. The REPL's `parse_submit` calls all
/// three too, so the two grammars cannot come to differ on a trigger order.
///
/// `--type market-to-limit` is the one `--type` that is no trigger kind: a market order whose unfilled
/// remainder rests as a limit at the price of its first fill, so it takes neither `--trigger` nor an
/// `@<price>` (its limit is that first fill's), and its trigger price is `None`.
///
/// `--type trailing` / `trailing-limit` are the trigger kinds whose `--trigger` is OPTIONAL: it is the
/// activation (nothing trails until the price reaches it), and without it the stop trails at once. Their
/// trail and limit offset are [`trail_flags`]'; an `@<price>`, a missing trail or a zero activation is
/// [`submit_rules_usage`]'s to refuse, in the shared rule's words.
///
/// `--type trailing-take-profit` is the trailing kind that takes NO `--trigger` (refused here): its
/// activation would run the other way from a trailing stop's, and `vike_model::order_terms_refusal` refuses
/// one in this build. Its trail and optional offset are [`trail_flags`]', an `@<price>` [`submit_rules_usage`]'s.
///
/// `--type pegged` takes no `--trigger` (refused here); its peg is [`peg_flag`]'s, and an `@<price>` is
/// [`submit_rules_usage`]'s to refuse.
pub(super) fn trigger_order(
    kind: Option<&str>,
    trigger: Option<&str>,
    has_price: bool,
) -> Result<Option<(&'static str, Option<f64>)>, CliError> {
    // `--type` is read ONCE, through the model's input door (`OrderType::parse`: trimmed, ASCII
    // case-insensitive), the CLI's dashes standing for the model's underscores; the `&'static` row
    // of `OrderType::KNOWN` it names gives the wire word. A word the model does not know is `None`.
    let known: Option<&'static OrderType> = kind
        .and_then(|k| OrderType::parse(&k.replace('-', "_")).ok())
        .and_then(|parsed| OrderType::KNOWN.iter().find(|row| **row == parsed));
    let trailing = known.filter(|t| t.is_trailing());
    if trailing == Some(&OrderType::TrailingTakeProfit) && trigger.is_some() {
        return Err(CliError::usage(
            "submit: --type trailing-take-profit takes no --trigger: its activation would run the \
             other way from a trailing stop's, and this build refuses one (it trails from the \
             moment it rests)"
                .to_string(),
        ));
    }
    if let Some(kind) = trailing {
        let activation = trigger
            .map(|raw| {
                raw.parse::<f64>().map_err(|_| {
                    CliError::usage(format!("submit: not an activation price: --trigger {raw:?}"))
                })
            })
            .transpose()?;
        return Ok(Some((kind.as_str(), activation)));
    }
    if let Some(row) = known.filter(|t| **t == OrderType::MarketToLimit) {
        return match (trigger, has_price) {
            (Some(_), _) => Err(CliError::usage(
                "submit: --type market-to-limit takes no --trigger: it is a market order whose \
                 remainder rests as a limit at its first fill's price"
                    .to_string(),
            )),
            (None, true) => Err(CliError::usage(
                "submit: --type market-to-limit takes no @<price>: its limit is the price of its \
                 first fill"
                    .to_string(),
            )),
            (None, false) => Ok(Some((row.as_str(), None))),
        };
    }
    if let Some(row) = known.filter(|t| **t == OrderType::Pegged) {
        return match trigger {
            Some(_) => Err(CliError::usage(
                "submit: --type pegged takes no --trigger: the venue prices it off its --peg anchor"
                    .to_string(),
            )),
            None => Ok(Some((row.as_str(), None))),
        };
    }
    let (kind, trigger) = match (kind, trigger) {
        (None, None) => return Ok(None),
        (Some(kind), Some(trigger)) => (kind, trigger),
        (Some(_), None) => {
            return Err(CliError::usage("submit: --type needs --trigger <price>".to_string()));
        }
        (None, Some(_)) => {
            return Err(CliError::usage(
                "submit: --trigger needs --type stop|stop-limit|take-profit|trailing|trailing-limit"
                    .to_string(),
            ));
        }
    };
    // The trailing kinds, market-to-limit and pegged returned above, so a TRIGGER kind left here is a
    // stop, a stop-limit or a take-profit; a market or limit `--type` is not a trigger kind.
    let word = match known.filter(|t| t.is_trigger()) {
        Some(row) => row.as_str(),
        None => {
            return Err(CliError::usage(format!(
                "submit: --type {kind:?} is not a trigger kind - expected stop, stop-limit, \
                 take-profit, trailing, trailing-limit or trailing-take-profit, or market-to-limit \
                 or pegged (a limit or market order is spelled @<price> / @market)"
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
    match (known, has_price) {
        (Some(OrderType::StopLimit), false) => Err(CliError::usage(
            "submit: --type stop-limit needs the limit price as @<price>".to_string(),
        )),
        (Some(OrderType::Stop), true) => Err(CliError::usage(format!(
            "submit: --type {kind} takes no @<price> - only --trigger (use stop-limit for a limit \
             fill)"
        ))),
        _ => Ok(Some((word, Some(px)))),
    }
}

/// The shape line of a refusal for a bracket that does not hang together.
const BRACKET_SHAPE: &str = "usage: vike-cli trade order bracket <account> <symbol> <buy|sell> \
                             <qty> [@<price>|@market] --stop-loss <price> --take-profit <price> \
                             [--order-client-id <stem>] [--node <host:port>] [--yes] [--json] \
                             [--no-wait] [--reason <text to end of line>]";

/// `bracket`'s long help, and the second half of its arity refusal. A function for the reason
/// [`submit_prose`] is one (`BOOK_FORMS` is named, not copied).
fn bracket_prose() -> String {
    format!(
        "<account> is REQUIRED, as for `submit`: a bracket opens risk, so it goes to exactly one venue \
         account. {} The forms are: {}\n\n\
         The entry is a limit at `@<price>`, or a market order with `@market` or no price. \
         --stop-loss and --take-profit are BOTH required: a buy needs stop-loss < take-profit (and \
         stop-loss < entry < take-profit for a limit entry), a sell the reverse, and the node \
         refuses an inverted bracket before it answers. The core sends the entry and HOLDS the two \
         exits until it fills, then releases them as reduce-only siblings.\n\n\
         A venue that cannot hold one of the exits refuses the WHOLE bracket and sends nothing: \
         three rejected legs, whose order client ids and reasons the answer prints under `outcome:` in the \
         core's own words.\n\n\
         The bracket's one order client id is its GROUP STEM, which the preview prints as `group=<stem>` and \
         the core derives the three legs' ids from: this client mints it, or --order-client-id <stem> pins \
         it ({}, and a group stem). A run without it mints a NEW stem, so a second run places a SECOND \
         bracket: after an OUTCOME UNKNOWN answer run `vike-cli trade order ls`, and send it again only \
         with --order-client-id <the stem it showed>, which the node refuses whole if the first one \
         landed. A stem needs a node advertising `bracket-stem` (an older node gets none and mints the \
         ids itself).\n\n\
         Naming an account needs a node advertising `bracket-account`, and the node takes the \
         bracket only on an engine that trades <symbol>.\n\n\
         --reason must be LAST: everything after it, to end of line, is taken verbatim as the \
         recorded rationale, so a --stop-loss or --take-profit written after it is missing and \
         refused, and so is an --order-client-id.",
        selector::ONE_ACCOUNT_RULE,
        BOOK_FORMS,
        verbs::ORDER_CLIENT_ID_CHARSET
    )
}

fn bracket_usage() -> String {
    format!("{BRACKET_SHAPE}\n\n{}", bracket_prose())
}

/// `bracket <account> <symbol> <buy|sell> <qty> [@<price>|@market] --stop-loss <px> --take-profit <px>`
/// — the order-shape rules ONLY (`--node`/`--yes`/`--json`/`--reason` are [`WrapperArgs`], read by
/// `clap`). `tokens` are the positionals in the order written, the `@price` token included wherever
/// it sat; `stop_loss`/`take_profit` are the RAW flag values. The book is REQUIRED and reads as
/// `submit`'s does ([`selector::addressed`]). PURE: no network.
///
/// Every number must be finite and above zero, refused here as a usage error: a NON-FINITE entry
/// price would serialise as `null`, which the node reads as a MARKET entry
/// (`OrderBracketWire::entry_price`), so this is the guard the wire type leaves to the sender. What
/// is NOT judged here is the bracket's SHAPE (an exit on the wrong side of its entry, a symbol the
/// engine does not trade): the node's door refuses those in its own words before its Ack, and a
/// client copy would be a second rule to keep in step.
///
/// `--order-client-id` pins the GROUP STEM ([`verbs::bracket_stem_refusal`] judges it): a retry after an OUTCOME UNKNOWN
/// names its first send's stem, so the door refuses it whole when the first landed and never places a second bracket.
/// Unpinned, [`oneshot::execute_write`] mints one before the preview where the node takes it.
fn bracket_verb(c: &BracketCommand) -> Result<Verb, CliError> {
    let (tokens, stop_loss, take_profit) =
        (&c.tokens, c.stop_loss.as_deref(), c.take_profit.as_deref());
    let mut positional: Vec<&str> = Vec::new();
    let mut entry_token: Option<&str> = None;
    for token in tokens {
        if let Some(p) = token.strip_prefix('@') {
            if entry_token.is_some() {
                return Err(CliError::usage("bracket: more than one @price token".to_string()));
            }
            entry_token = Some(p);
        } else {
            positional.push(token);
        }
    }
    if positional.len() != 4 {
        return Err(CliError::usage(bracket_usage()));
    }
    let (venue, account) = selector::addressed(positional[0])?;
    let side = super::side_arg(positional[2]).map_err(CliError::usage)?;
    let qty = bracket_number("qty", positional[3])?;
    let entry_price = match entry_token {
        None | Some("market") => None,
        Some(p) => Some(bracket_number("entry price", p)?),
    };
    let stop_loss = bracket_number("--stop-loss", bracket_flag("--stop-loss", stop_loss)?)?;
    let take_profit = bracket_number("--take-profit", bracket_flag("--take-profit", take_profit)?)?;
    // After the required exits, so a swallowed exit still reads as missing: a swallowed `--order-client-id` would
    // send a FRESH stem, the second bracket a retry exists to prevent.
    oneshot::refuse_swallowed_flag::<BracketCommand>(
        "bracket",
        "the bracket",
        c.wrap.reason().as_deref(),
    )?;
    if let Some(refusal) = c.order_client_id.as_deref().and_then(verbs::bracket_stem_refusal) {
        return Err(CliError::usage(format!("bracket: {}", refusal.rendered())));
    }
    Ok(Verb::Bracket(OrderBracketWire {
        order_client_id: c.order_client_id.clone(),
        venue,
        symbol: positional[1].to_string(),
        side,
        qty,
        entry_price,
        stop_loss,
        take_profit,
        account,
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

/// `cancel <order client id>` — no book, same grammar as the REPL's `cancel <order client id>`. PURE.
fn cancel_verb(ids: &[String]) -> Result<Verb, CliError> {
    match ids {
        [id] => Ok(Verb::Cancel(id.clone())),
        _ => Err(CliError::usage(cancel_usage())),
    }
}

/// `modify <order client id> [--qty Q] [--price P] [--trigger P]` — no book, same reason as `cancel`. The three
/// changes are the RAW flag values, read here ([`modify_number`]); one that changes nothing is
/// `vike_client::verbs::modify_of`'s refusal, the MCP tool's too. Mirrors `crate::cmd::trade`'s own
/// `parse_modify`. PURE.
fn modify_verb(
    ids: &[String],
    (qty, price, trigger): (Option<&str>, Option<&str>, Option<&str>),
    reason: Option<&str>,
) -> Result<Verb, CliError> {
    oneshot::refuse_swallowed_flag::<ModifyCommand>("modify", "the change", reason)?;
    let order_client_id = ids.first().ok_or_else(|| CliError::usage(modify_usage()))?;
    if let Some(other) = ids.get(1) {
        return Err(CliError::usage(format!(
            "modify: unexpected token {other:?}\n{}",
            modify_usage()
        )));
    }
    let new_qty = qty.map(|v| modify_number("qty", v)).transpose()?;
    let new_price = price.map(|v| modify_number("price", v)).transpose()?;
    let new_trigger_price = trigger.map(|v| modify_number("trigger", v)).transpose()?;
    verbs::modify_of(order_client_id.clone(), new_qty, new_price, new_trigger_price)
        .map_err(|refusal| CliError::usage(format!("modify: {}", refusal.rendered())))
}

/// One `modify` number; `what` names it in the refusal. The REPL's `parse_modify` calls it too. PURE.
///
/// ⚠ `parse::<f64>` accepts `nan` and `inf`, and `1e999` reads as inf. The JSON frame writes a non-finite
/// number as `null`, which the node reads as "no change", so `modify C --qty 5 --price 1e999` previewed
/// `price=inf` and changed only the qty. Refused, as `bracket_number` refuses one. A zero or a negative
/// value stays the node's to judge.
pub(super) fn modify_number(what: &str, raw: &str) -> Result<f64, CliError> {
    let v: f64 =
        raw.parse().map_err(|_| CliError::usage(format!("modify: not a {what}: {raw:?}")))?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(CliError::usage(format!("modify: {what} must be a finite number, got {raw:?}")))
    }
}

/// `mass-cancel [<account>] [<symbol>]` — book OPTIONAL (wider is safer). Mirrors
/// `crate::cmd::trade`'s own REPL grammar. PURE.
fn mass_cancel_verb(tokens: &[String]) -> Result<Verb, CliError> {
    if tokens.len() > 2 {
        return Err(CliError::usage(mass_cancel_usage()));
    }
    let (venue, account) = selector::optional_book(tokens.first())?;
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
        OrderVerb::Submit(c) => submit_verb(&c),
        other => unreachable!("`submit` read as {other:?}"),
    }
}

/// [`bracket_verb`] over an argv tail, through the plane's parser.
#[cfg(test)]
fn parse_bracket_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("bracket", tokens)? {
        OrderVerb::Bracket(c) => bracket_verb(&c),
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
        OrderVerb::Modify(c) => modify_verb(
            &c.ids,
            (c.qty.as_deref(), c.price.as_deref(), c.trigger.as_deref()),
            c.wrap.reason().as_deref(),
        ),
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
