//! `vike-cli trade order combo`: ONE combo (wave 6 of the order kinds), two legs or more traded as ONE order on ONE
//! book (decision 0254), built by `vike_client::verbs`' `combo_of`, the one builder of a combo, which the MCP tool
//! `submit_combo` builds through too. The book is REQUIRED (a combo opens risk) and reads as `submit`'s does
//! (`selector::addressed`).
//!
//! - `combo <account> <buy|sell> <qty> --leg <symbol>:<ratio>[@<limit>] --leg ... [--net <px>] [--time-in-force <word>] [--order-client-id <id>]`
//!
//! The lifetime is `--time-in-force`, `submit --time-in-force`'s words, judged by `combo_of` as the node's door does
//! (a `gtd` is refused: the combo carries no expiry). The retired `--tif` is refused by name, before any other rule.
//!
//! The combo's ONE order client id is `--order-client-id`, refused outside the charset every venue accepts as `submit --order-client-id` is,
//! or minted by the client before the preview (`vike_client::verbs::fill_order_client_id`), so a resend under the same
//! id is refused whole by the node. One-shot only, like `bracket`: no REPL word. PURE: no network.

use crate::cmd::trade::oneshot::{self, WrapperArgs};
use crate::cmd::trade::selector;
use crate::exit::CliError;
use vike_client::verbs::{self, ComboLegArg, Verb};

pub(in crate::cmd::trade) fn combo_prose() -> String {
    format!(
        "Two legs or more traded as ONE order: each --leg <symbol>:<ratio>[@<limit>], the ratio SIGNED (a positive \
         one goes the combo's way, a negative one the other: `buy` with C1:1 and C2:-1 buys C1 and sells C2), <qty> \
         in combo units (a leg trades |ratio| x <qty>). Priced by its NET (--net <px>, signed: a debit above zero, a \
         credit below), by a limit on EVERY leg (`@<limit>`, a price of that leg's instrument), or, with neither, at \
         market; a net beside leg limits, or a limit on some legs only, is refused. Its lifetime is --time-in-force, \
         submit's words (default gtc; a lifetime but gtc on a priced combo only; gtd is refused: a combo carries no \
         expiry).\n\n\
         <account> is REQUIRED: a combo opens risk, so it goes to exactly one venue account. {} The forms are: {}\n\n\
         The combo has ONE order client id: --order-client-id <id> ({}), or one this client mints. A resend under the same id is \
         refused whole by the node, never placed twice. Only a venue whose row takes a combo places one (deribit, by \
         its net), and only a paper book fills one priced by its legs: every other engine rejects it after the answer, \
         in the core's words. The node needs to advertise `combo`.\n\n\
         --reason must be LAST: everything after it, to end of line, is taken verbatim as the recorded rationale.",
        selector::ONE_ACCOUNT_RULE,
        vike_model::accounts::account_keys::BOOK_FORMS,
        verbs::ORDER_CLIENT_ID_CHARSET
    )
}

/// `combo`'s tokens and flags.
#[derive(clap::Args, Debug)]
pub(in crate::cmd::trade) struct ComboCommand {
    #[arg(value_name = "TOKEN", help = "<account> <buy|sell> <qty>")]
    tokens: Vec<String>,
    #[arg(
        long = "leg",
        value_name = "SYMBOL:RATIO[@LIMIT]",
        help = "one leg: its instrument, its signed ratio and, on a combo priced by its legs, its own limit \
                (repeat it; two or more)"
    )]
    legs: Vec<String>,
    #[arg(
        long,
        value_name = "PRICE",
        help = "the signed net limit per combo unit (a credit is negative); omitted with no leg limit, a market combo"
    )]
    net: Option<String>,
    #[arg(
        long,
        value_name = "ID",
        help = "pin the combo's one order client id (1..=32 alphanumeric characters); omitted, the client mints it"
    )]
    order_client_id: Option<String>,
    #[arg(
        long,
        value_name = "WORD",
        help = "how long the combo lives, submit --time-in-force's words: day | gtc | ioc | fok | at-open | at-close, a \
                lifetime but gtc on a priced combo only (default gtc; gtd is refused: a combo carries no expiry; a book \
                or a venue that cannot do it refuses by name)"
    )]
    time_in_force: Option<String>,
    // The retired spelling of `--time-in-force`, refused by name before any other rule.
    #[arg(long = "tif", hide = true, value_name = "WORD")]
    retired_tif: Option<String>,
    #[command(flatten)]
    pub(in crate::cmd::trade) wrap: WrapperArgs,
}

pub(in crate::cmd::trade) fn combo_verb(c: &ComboCommand) -> Result<Verb, CliError> {
    if c.retired_tif.is_some() {
        return Err(CliError::usage(super::super::plane::renamed("--tif", "--time-in-force")));
    }
    // A `--leg`, `--net` or `--order-client-id` swallowed by `--reason` would send another instrument, a market combo, or a combo
    // under an id nobody named.
    oneshot::refuse_swallowed_flag::<ComboCommand>(
        "combo",
        "the combo",
        c.wrap.reason().as_deref(),
    )?;
    let [book, side, qty] = c.tokens.iter().map(String::as_str).collect::<Vec<_>>()[..] else {
        return Err(usage("needs <account> <buy|sell> <qty>"));
    };
    // `submit --order-client-id`'s rule, the one both doors make: the strictest charset every venue accepts.
    if let Some(refusal) = c.order_client_id.as_deref().and_then(verbs::order_client_id_refusal) {
        return Err(usage(&refusal.rendered()));
    }
    let (venue, account) = selector::addressed(book)?;
    let side = super::super::side_arg(side).map_err(CliError::usage)?;
    let qty = number("qty", qty)?;
    let legs = c.legs.iter().map(|raw| leg_of(raw)).collect::<Result<Vec<_>, _>>()?;
    let net = c.net.as_deref().map(|raw| number("--net", raw)).transpose()?;
    // `submit --time-in-force`'s words; a post-only one is refused by `combo_of`, in the words the MCP tool gives too.
    let tif = match c.time_in_force.as_deref().map(|w| (w, vike_model::TimeInForce::parse(w))) {
        None => vike_model::TimeInForce::Gtc,
        Some((_, Ok(t))) => t,
        Some((word, Err(_))) => {
            return Err(usage(&format!(
                "--time-in-force {word:?} is not a time in force - expected day, gtc, ioc, fok, gtd, at-open or \
                 at-close"
            )));
        }
    };
    let mut combo =
        verbs::combo_of(venue, account, side, qty, legs, net, tif).map_err(|why| usage(&why))?;
    combo.order_client_id = c.order_client_id.clone();
    Ok(Verb::Combo(combo))
}

/// `<symbol>:<ratio>[@<limit>]`, split at the LAST `@` and the last `:` so a symbol keeps any of its own.
fn leg_of(raw: &str) -> Result<ComboLegArg, CliError> {
    let (body, limit) = match raw.rsplit_once('@') {
        Some((body, limit)) => (body, Some(number("--leg limit", limit)?)),
        None => (raw, None),
    };
    let bad = || usage(&format!("--leg is <symbol>:<ratio>[@<limit>], got {raw:?}"));
    let (symbol, ratio) = body.rsplit_once(':').ok_or_else(bad)?;
    let ratio = ratio.parse::<i32>().map_err(|_| bad())?;
    Ok(ComboLegArg { symbol: symbol.to_string(), ratio, limit })
}

/// A number; whether it may be zero, negative or must be finite is `vike_model::OrderCombo::validate`'s, in `combo_of`.
fn number(what: &str, raw: &str) -> Result<f64, CliError> {
    raw.parse::<f64>().map_err(|_| usage(&format!("{what} is not a number: {raw:?}")))
}

fn usage(why: &str) -> CliError {
    CliError::usage(format!("combo: {why}"))
}

#[path = "combo_tests.rs"]
#[cfg(test)]
mod combo_tests;
