//! `vike-cli trade order oco|oto|ladder` and the REPL's `oco|oto|ladder`: the three ORDER LIST verbs (wave 4 of the
//! order kinds), each ONE `CommandWire::OrderList` on ONE book and ONE symbol (decision 0254), built by
//! `vike_client::verbs`' `oco_list` / `oto_list` / `ladder_list` so the MCP tool `submit_order_list` builds the same
//! legs. The book is REQUIRED (a list opens risk) and reads as `submit`'s does (`selector::addressed`).
//!
//! - `oco <account> <symbol> <buy|sell> <qty> --limit <px> (--stop <trigger> [--stop-limit <px>] | --trail <distance>)
//!   [--reduce-only]`
//! - `oto <account> <symbol> <buy|sell> <qty> [@<price>|@market] (--stop <trigger> | --limit <px> | --trail <distance>)`
//! - `ladder <account> <symbol> <buy|sell> <qty> [@<price>|@market] (--stop-loss <trigger> | --trail <distance>)
//!   --take-profit <px>:<qty>...` (the bracket's two words for the same two things; an OCO's or an OTO's legs are no
//!   stop-loss nor take-profit)
//!
//! `--trail` makes the list's protective leg a TRAILING stop (`trailing`, no activation, no limit: the list leg carries
//! neither), as `submit --type trailing --trail` spells the trail. Which leg a line names, and that it names exactly
//! one, is the shared builder's decision (`vike_client::refusals::OrderListRefusal`), the MCP tool's too.
//!
//! Each takes `--order-client-id <id>` once per leg, in leg order, as `submit` takes one: omitted, the client mints every leg's
//! id before the preview (`vike_client::verbs::fill_order_client_id`), so the ids shown are the ids sent and a resend
//! under them is refused whole. The REPL reads the SAME `clap` structs through the plane's parser ([`repl_verb`]), so
//! the two spellings cannot differ; a one-shot wrapper flag (`--node`, `--yes`, `--json`, `--no-wait`) on a REPL line
//! is refused. PURE: no network.

use crate::cmd::trade::oneshot::WrapperArgs;
use crate::cmd::trade::selector;
use crate::exit::CliError;
use vike_client::verbs::{self, OrderListHead, Verb};

/// The shared head of the three verbs' help: what the book is.
fn book_prose() -> String {
    format!(
        "<account> is REQUIRED: an order list opens risk, so every leg goes to exactly one venue account and one \
         symbol. {} The forms are: {}\n\n\
         Every leg carries an order client id: --order-client-id <id> once per leg, in leg order, or none and this client mints \
         them, shown in the preview. The node refuses a list naming an id it already holds, WHOLE, so a resend after \
         an OUTCOME UNKNOWN answer under the same ids places nothing twice; a second run mints new ids and is a new \
         list. A venue that cannot hold a leg refuses the WHOLE list in the core's words. The node takes a list only \
         on an engine that already trades <symbol>, and needs to advertise `order-list`.\n\n\
         --reason must be LAST: everything after it, to end of line, is taken verbatim as the recorded rationale.",
        selector::ONE_ACCOUNT_RULE,
        vike_model::accounts::account_keys::BOOK_FORMS
    )
}

pub(in crate::cmd::trade) fn oco_prose() -> String {
    format!(
        "Two legs of one side and one size, linked one-cancels-other: a limit at --limit and a stop at --stop (a \
         stop-limit with its limit at --stop-limit), or in its place a trailing stop that keeps --trail from the \
         best price since it went live. The first to fill cancels the other. With --reduce-only the pair is an exit \
         (a target and a protective stop); without, two entries.\n\n{}",
        book_prose()
    )
}

pub(in crate::cmd::trade) fn oto_prose() -> String {
    format!(
        "An entry (a limit at `@<price>`, a market order with `@market` or no price) and ONE exit of the same size \
         on the other side, reduce-only and held until the entry fills: a protective stop at --stop, a target \
         limit at --limit, or a trailing stop that keeps --trail from the best price since the entry filled \
         (exactly one).\n\n{}",
        book_prose()
    )
}

pub(in crate::cmd::trade) fn ladder_prose() -> String {
    format!(
        "An entry (a limit at `@<price>`, a market order with `@market` or no price), ONE protective stop for its \
         whole size, at --stop-loss or trailing by --trail (exactly one), and the partial targets, each \
         --take-profit <price>:<qty> (one or more, summing to no more than <qty>). A target's fill shrinks the stop \
         by what it filled.\n\n{}",
        book_prose()
    )
}

/// `oco`'s tokens and flags.
#[derive(clap::Args, Debug)]
pub(in crate::cmd::trade) struct OcoCommand {
    #[arg(value_name = "TOKEN", help = "<account> <symbol> <buy|sell> <qty>")]
    tokens: Vec<String>,
    #[arg(long, value_name = "PRICE", help = "the limit leg's price (required)")]
    limit: Option<String>,
    #[arg(
        long,
        value_name = "PRICE",
        help = "the stop leg's trigger price (it or --trail is required)"
    )]
    stop: Option<String>,
    #[arg(
        long = "stop-limit",
        value_name = "PRICE",
        help = "make the stop leg a stop-limit at this limit"
    )]
    stop_limit: Option<String>,
    #[arg(long, value_name = "PRICE", help = TRAIL_HELP)]
    trail: Option<String>,
    #[arg(long, help = "both legs may only reduce an open position (an exit pair)")]
    reduce_only: bool,
    #[arg(long = "order-client-id", value_name = "ID", help = ORDER_CLIENT_ID_HELP)]
    order_client_ids: Vec<String>,
    #[command(flatten)]
    pub(in crate::cmd::trade) wrap: WrapperArgs,
}

/// `--order-client-id`'s help, one for the three verbs.
const ORDER_CLIENT_ID_HELP: &str = "pin one leg's order client id (1..=32 alphanumeric characters), once per leg in leg order; \
                         omitted, the client mints them";

/// `--trail`'s help, one for the three verbs.
const TRAIL_HELP: &str = "the protective leg is a trailing stop instead: the distance in price it keeps from the best \
                          price since it went live (no activation, no limit)";

/// `oto`'s tokens and flags.
#[derive(clap::Args, Debug)]
pub(in crate::cmd::trade) struct OtoCommand {
    #[arg(value_name = "TOKEN", help = "<account> <symbol> <buy|sell> <qty> [@<price>|@market]")]
    tokens: Vec<String>,
    #[arg(
        long,
        value_name = "PRICE",
        help = "the exit is a protective stop at this trigger price"
    )]
    stop: Option<String>,
    #[arg(long, value_name = "PRICE", help = "the exit is a target limit at this price")]
    limit: Option<String>,
    #[arg(long, value_name = "PRICE", help = TRAIL_HELP)]
    trail: Option<String>,
    #[arg(long = "order-client-id", value_name = "ID", help = ORDER_CLIENT_ID_HELP)]
    order_client_ids: Vec<String>,
    #[command(flatten)]
    pub(in crate::cmd::trade) wrap: WrapperArgs,
}

/// `ladder`'s tokens and flags: the bracket's `--stop-loss` and `--take-profit`, the take-profit repeated with its size.
#[derive(clap::Args, Debug)]
pub(in crate::cmd::trade) struct LadderCommand {
    #[arg(value_name = "TOKEN", help = "<account> <symbol> <buy|sell> <qty> [@<price>|@market]")]
    tokens: Vec<String>,
    #[arg(
        long = "stop-loss",
        value_name = "PRICE",
        help = "the stop-loss trigger price, for the whole size (it or --trail is required)"
    )]
    stop_loss: Option<String>,
    #[arg(long, value_name = "PRICE", help = TRAIL_HELP)]
    trail: Option<String>,
    #[arg(
        long = "take-profit",
        value_name = "PRICE:QTY",
        help = "one partial take-profit, a limit of QTY at PRICE (repeat it; one or more)"
    )]
    take_profits: Vec<String>,
    #[arg(long = "order-client-id", value_name = "ID", help = ORDER_CLIENT_ID_HELP)]
    order_client_ids: Vec<String>,
    #[command(flatten)]
    pub(in crate::cmd::trade) wrap: WrapperArgs,
}

pub(in crate::cmd::trade) fn oco_verb(c: &OcoCommand) -> Result<Verb, CliError> {
    let (head, entry) = head_of("oco", &c.tokens)?;
    if entry.is_some() {
        return Err(usage("oco", "takes no @<price>: its legs are priced by --limit and --stop"));
    }
    let limit = number("oco", "--limit", required("oco", "--limit", c.limit.as_deref())?)?;
    let stop = optional("oco", "--stop", c.stop.as_deref())?;
    let stop_limit = optional("oco", "--stop-limit", c.stop_limit.as_deref())?;
    let trail = optional("oco", "--trail", c.trail.as_deref())?;
    let list = verbs::oco_list(head, limit, stop, stop_limit, trail, c.reduce_only);
    built("oco", list, &c.order_client_ids)
}

pub(in crate::cmd::trade) fn oto_verb(c: &OtoCommand) -> Result<Verb, CliError> {
    let (head, entry) = head_of("oto", &c.tokens)?;
    let stop = optional("oto", "--stop", c.stop.as_deref())?;
    let limit = optional("oto", "--limit", c.limit.as_deref())?;
    let trail = optional("oto", "--trail", c.trail.as_deref())?;
    built("oto", verbs::oto_list(head, entry, stop, limit, trail), &c.order_client_ids)
}

pub(in crate::cmd::trade) fn ladder_verb(c: &LadderCommand) -> Result<Verb, CliError> {
    let (head, entry) = head_of("ladder", &c.tokens)?;
    let stop_loss = optional("ladder", "--stop-loss", c.stop_loss.as_deref())?;
    let trail = optional("ladder", "--trail", c.trail.as_deref())?;
    if c.take_profits.is_empty() {
        return Err(usage("ladder", "needs one --take-profit <price>:<qty> or more"));
    }
    let targets = c
        .take_profits
        .iter()
        .map(|t| {
            let (px, qty) = t.split_once(':').ok_or_else(|| {
                usage("ladder", &format!("--take-profit is <price>:<qty>, got {t:?}"))
            })?;
            Ok((
                number("ladder", "--take-profit price", px)?,
                number("ladder", "--take-profit qty", qty)?,
            ))
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    let list = verbs::ladder_list(head, entry, stop_loss, trail, &targets);
    built("ladder", list, &c.order_client_ids)
}

/// A REPL line `oco|oto|ladder <tokens…>`, read by the plane's own parser into the one-shot verb's struct, so the two
/// spellings are one grammar. A wrapper flag is the one-shot's and is refused here.
pub(in crate::cmd::trade) fn repl_verb(verb: &str, rest: &[&str]) -> Result<Verb, String> {
    let argv = ["order", verb].into_iter().chain(rest.iter().copied()).map(String::from);
    let cli = super::super::parse(argv).map_err(|stop| stop.to_string())?;
    let Some(super::super::Sub::Order { verb: order }) = cli.sub else {
        return Err(format!("{verb}: not an order verb"));
    };
    let (built, wrap) = match &order {
        super::OrderVerb::Oco(c) => (oco_verb(c), &c.wrap),
        super::OrderVerb::Oto(c) => (oto_verb(c), &c.wrap),
        super::OrderVerb::Ladder(c) => (ladder_verb(c), &c.wrap),
        _ => return Err(format!("{verb}: not an order-list verb")),
    };
    if wrap.node.is_some() || wrap.yes || wrap.json || wrap.no_wait {
        return Err(format!(
            "{verb}: --node, --yes, --json and --no-wait are the one-shot verb's flags, not a REPL line's"
        ));
    }
    built.map_err(|e| e.message)
}

/// `<account> <symbol> <buy|sell> <qty>` and an optional `@<price>`/`@market` wherever it sits.
fn head_of(verb: &str, tokens: &[String]) -> Result<(OrderListHead, Option<f64>), CliError> {
    let mut positional: Vec<&str> = Vec::new();
    let mut entry_token: Option<&str> = None;
    for token in tokens {
        if let Some(p) = token.strip_prefix('@') {
            if entry_token.is_some() {
                return Err(usage(verb, "more than one @price token"));
            }
            entry_token = Some(p);
        } else {
            positional.push(token);
        }
    }
    let [book, symbol, side, qty] = positional[..] else {
        return Err(usage(verb, "needs <account> <symbol> <buy|sell> <qty>"));
    };
    let (venue, account) = selector::addressed(book)?;
    let side = super::super::side_arg(side).map_err(CliError::usage)?;
    let qty = number(verb, "qty", qty)?;
    let entry = match entry_token {
        None | Some("market") => None,
        Some(p) => Some(number(verb, "entry price", p)?),
    };
    Ok((OrderListHead { venue, account, symbol: symbol.to_string(), side, qty }, entry))
}

fn required<'a>(verb: &str, flag: &str, value: Option<&'a str>) -> Result<&'a str, CliError> {
    value.ok_or_else(|| usage(verb, &format!("{flag} <price> is required")))
}

/// A flag's number when it is given ([`number`]'s rule).
fn optional(verb: &str, flag: &str, value: Option<&str>) -> Result<Option<f64>, CliError> {
    value.map(|v| number(verb, flag, v)).transpose()
}

/// One number, finite and above zero (a non-finite one would go out as `null`).
fn number(verb: &str, what: &str, raw: &str) -> Result<f64, CliError> {
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err(usage(verb, &format!("{what} must be a finite number above zero, got {raw:?}"))),
    }
}

fn usage(verb: &str, why: &str) -> CliError {
    CliError::usage(format!("{verb}: {why}"))
}

/// The built list with the caller's `--order-client-id`s pinned on its legs, each refused as `submit --order-client-id` refuses one
/// (`verbs::with_leg_order_client_ids` asks `verbs::order_client_id_refusal`, the MCP tool's builder too).
fn built(
    verb: &str,
    list: Result<vike_tradehub_client::wire::OrderListWire, String>,
    order_client_ids: &[String],
) -> Result<Verb, CliError> {
    list.and_then(|list| verbs::with_leg_order_client_ids(list, order_client_ids))
        .map(Verb::OrderList)
        .map_err(|why| usage(verb, &why))
}

#[path = "list_tests.rs"]
#[cfg(test)]
mod list_tests;
