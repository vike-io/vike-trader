//! The order-verb grammar (pure): `submit` / `buy` / `sell`, `modify`, and the side words.

use vike_tradehub_client::wire::OrderRequestWire;

use vike_client::verbs;
use vike_client::verbs::Verb;

/// `submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--order-client-id <id>]
/// [--type stop|stop-limit|take-profit --trigger <price> | --type trailing|trailing-limit --trail <price>
/// (or --trail-percent <p>) [--offset <price>] [--trigger <activation>] | --type trailing-take-profit
/// --trail <price> (or --trail-percent <p>) [--offset <price>] | --type market-to-limit | --type pegged
/// --peg best|mid|market|primary [--offset <price>]] [--adjust-at <price> --adjust-to <target> …]
/// [--time-in-force day|gtc|ioc|fok|gtd|at-open|at-close [--expires <when>] | --post-only] [--display <qty>]
/// [--all-or-none] [--sweep-to-fill] [--discretion <amount>]
/// [--at <when> [--when-price above|below:<price> [--when-price-on <venue>]]]`. The leading verb may also be
/// `buy`/`sell` as sugar (then side is implied and the `<buy|sell>` token is omitted). Every flag past
/// `--type`/`--trigger` is read by the one-shot verb's own helper (`order::trail_flags`,
/// `order::attributes_flag`, `order::adjust_flag`, `order::release_flag`, ...) and the built order judged by
/// its `order::submit_rules_usage`, so the two grammars cannot differ on what they take or refuse. The retired
/// `--tif` (`--tif=…` too) is refused by name before this runs (`super::grammar`'s `retired_spelling`).
///
/// **A native trigger order and a named account are the one-shot `trade order submit`'s two pieces,
/// taken the same way.** `--type`/`--trigger` go through `order::trigger_order` (the one-shot verb's
/// own function, so the two grammars cannot differ on what a trigger order is, nor on what it
/// refuses: a zero, negative or NaN trigger, one flag without the other, a kind that is no trigger
/// kind). The account is the one-shot verb's BOOK form, read by `selector::addressed` as there: `<venue>`
/// names none, and the node's door takes that only where the venue has one account, running or not (decision 0254).
/// `<venue>/<LABEL>` and `<venue>/DEFAULT` name that account.
///
/// `--order-client-id <id>` PINS the order-client-id instead of letting the session mint one. It exists
/// because the sibling MCP tool already takes an optional `order_client_id` and the two surfaces
/// share one vocabulary by design — plus the two cases a human actually hits: adopting an id
/// somebody else (a script, a runbook, the venue-side reconciliation sheet) already wrote down, and
/// scripting `--yes` where the caller needs to know the id BEFORE the command runs.
///
/// It is validated with `vike_model::is_valid_crypto_order_client_id` — the same `^[A-Za-z0-9]{1,32}$` rule
/// the minter's own output is asserted against, and the strictest common denominator across venues.
/// Accepting something looser here would only move the failure to the venue edge, minutes later and
/// far from the typo. Left EMPTY (`--order-client-id ""`) it is rejected outright rather than silently falling
/// back to a mint: an operator who names an id and gets a different one has lost the handle they
/// asked for.
///
/// A qty or `@price` that cannot be SIZED — a qty of zero, below or non-finite, a non-finite price —
/// is REFUSED here ([`verbs::unsizeable_submit_refusal`], which the one-shot `trade order submit`
/// and the MCP `submit_order` tool call too), not previewed. The cap check that follows is
/// advisory and stays so: an order that IS sizeable and over every cap still parses.
pub(in crate::cmd::trade) fn parse_submit(verb: &str, rest: &[&str]) -> Result<Verb, String> {
    const USAGE: &str = "usage: submit <venue|venue/LABEL> <symbol> <buy|sell> <qty> \
                         [@<price>|@market] [--reduce-only] [--order-client-id <id>] \
                         [--type stop|stop-limit|take-profit --trigger <price> | --type \
                         trailing|trailing-limit --trail <price>|--trail-percent <p> [--offset \
                         <price>] [--trigger <activation>] | --type trailing-take-profit --trail \
                         <price>|--trail-percent <p> [--offset <price>] | --type market-to-limit \
                         | --type pegged --peg best|mid|market|primary [--offset <price>]] \
                         [--adjust-at <price> --adjust-to stop|stop-limit|trailing --adjust-stop \
                         <price> [--adjust-limit <price> | --adjust-trail <price> | \
                         --adjust-trail-percent <p>]] \
                         [--time-in-force day|gtc|ioc|fok|gtd|at-open|at-close [--expires <when>] \
                         | --post-only] [--display <qty>] [--all-or-none] [--sweep-to-fill] \
                         [--discretion <amount>] [--at <when> [--when-price \
                         above|below:[<symbol>:]<price> [--when-price-on <venue>]]]";
    let mut reduce_only = false;
    // a limit order's attributes, by the one-shot verb's own rule (`order::attributes_flag`)
    let (mut all_or_none, mut sweep_to_fill, mut discretion): (bool, bool, Option<&str>) =
        (false, false, None);
    // a percent trail (`order::trail_flags`), a release's price venue (`order::release_flag`) and a stop's
    // adjustment (`order::adjust_flag`)
    let (mut trail_percent, mut when_price_on): (Option<&str>, Option<&str>) = (None, None);
    let mut adjust = super::super::order::AdjustTerms::default();
    // the limit order's lifetime, by the one-shot verb's own rules (`order::lifetime_flags`)
    let (mut tif, mut expires, mut post_only): (Option<&str>, Option<&str>, bool) =
        (None, None, false);
    let mut positional: Vec<&str> = Vec::new();
    let mut price_token: Option<&str> = None;
    let mut order_client_id: Option<&str> = None;
    let mut expect_order_client_id = false;
    let (mut kind, mut trigger): (Option<&str>, Option<&str>) = (None, None);
    // the display and the release, by the one-shot verb's own rules (`order::display_flag`, `order::release_flag`)
    let (mut display, mut at, mut when_price): (Option<&str>, Option<&str>, Option<&str>) =
        (None, None, None);
    // a trailing order's trail and limit offset, by the one-shot verb's own rule (`order::trail_flags`), and a
    // pegged order's anchor (`order::peg_flag`), whose offset is `--offset` too
    let (mut trail, mut offset, mut peg): (Option<&str>, Option<&str>, Option<&str>) =
        (None, None, None);
    // The flag (`--type` / `--trigger` / ...) whose value is the next token.
    let mut expect_value: Option<&str> = None;
    for token in rest {
        if expect_order_client_id {
            expect_order_client_id = false;
            order_client_id = Some(token);
            continue;
        }
        if let Some(flag) = expect_value.take() {
            // A value that is itself a flag means the operator's value went missing.
            if token.starts_with("--") {
                return Err(format!("submit: {flag} needs a value, got {token:?}"));
            }
            match flag {
                "--type" => kind = Some(token),
                "--time-in-force" => tif = Some(token),
                "--expires" => expires = Some(token),
                "--display" => display = Some(token),
                "--at" => at = Some(token),
                "--when-price" => when_price = Some(token),
                "--trail" => trail = Some(token),
                "--offset" => offset = Some(token),
                "--peg" => peg = Some(token),
                "--trail-percent" => trail_percent = Some(token),
                "--discretion" => discretion = Some(token),
                "--when-price-on" => when_price_on = Some(token),
                "--adjust-at" => adjust.at = Some(token),
                "--adjust-to" => adjust.to = Some(token),
                "--adjust-stop" => adjust.stop = Some(token),
                "--adjust-limit" => adjust.limit = Some(token),
                "--adjust-trail" => adjust.trail = Some(token),
                "--adjust-trail-percent" => adjust.trail_percent = Some(token),
                _ => trigger = Some(token),
            }
            continue;
        }
        // The valued flags past `--type`/`--trigger`/`--time-in-force`/`--expires`/`--order-client-id`, each
        // `--flag value` or `--flag=value`.
        if let Some(flag) = [
            "--display",
            "--at",
            "--when-price",
            "--when-price-on",
            "--trail",
            "--trail-percent",
            "--offset",
            "--peg",
            "--discretion",
            "--adjust-at",
            "--adjust-to",
            "--adjust-stop",
            "--adjust-limit",
            "--adjust-trail",
            "--adjust-trail-percent",
        ]
        .into_iter()
        .find(|f| token == f)
        {
            expect_value = Some(flag);
            continue;
        }
        if let Some((flag, v)) = token.split_once('=')
            && let Some(slot) = match flag {
                "--display" => Some(&mut display),
                "--at" => Some(&mut at),
                "--when-price" => Some(&mut when_price),
                "--when-price-on" => Some(&mut when_price_on),
                "--trail" => Some(&mut trail),
                "--trail-percent" => Some(&mut trail_percent),
                "--offset" => Some(&mut offset),
                "--peg" => Some(&mut peg),
                "--discretion" => Some(&mut discretion),
                "--adjust-at" => Some(&mut adjust.at),
                "--adjust-to" => Some(&mut adjust.to),
                "--adjust-stop" => Some(&mut adjust.stop),
                "--adjust-limit" => Some(&mut adjust.limit),
                "--adjust-trail" => Some(&mut adjust.trail),
                "--adjust-trail-percent" => Some(&mut adjust.trail_percent),
                _ => None,
            }
        {
            *slot = Some(v);
            continue;
        }
        if *token == "--reduce-only" {
            reduce_only = true;
        } else if *token == "--post-only" {
            post_only = true;
        } else if *token == "--all-or-none" {
            all_or_none = true;
        } else if *token == "--sweep-to-fill" {
            sweep_to_fill = true;
        } else if let Some(v) = token.strip_prefix("--time-in-force=") {
            tif = Some(v);
        } else if *token == "--time-in-force" {
            expect_value = Some("--time-in-force");
        } else if let Some(v) = token.strip_prefix("--expires=") {
            expires = Some(v);
        } else if *token == "--expires" {
            expect_value = Some("--expires");
        } else if let Some(v) = token.strip_prefix("--order-client-id=") {
            order_client_id = Some(v);
        } else if *token == "--order-client-id" {
            expect_order_client_id = true;
        } else if let Some(v) = token.strip_prefix("--type=") {
            kind = Some(v);
        } else if *token == "--type" {
            expect_value = Some("--type");
        } else if let Some(v) = token.strip_prefix("--trigger=") {
            trigger = Some(v);
        } else if *token == "--trigger" {
            expect_value = Some("--trigger");
        } else if let Some(p) = token.strip_prefix('@') {
            if price_token.is_some() {
                return Err("submit: more than one @price token".to_string());
            }
            price_token = Some(p);
        } else {
            positional.push(token);
        }
    }
    if expect_order_client_id {
        return Err("submit: --order-client-id needs a value".to_string());
    }
    if let Some(flag) = expect_value {
        return Err(format!("submit: {flag} needs a value"));
    }
    let order_client_id = match order_client_id.map(|id| (id, verbs::order_client_id_refusal(id))) {
        None => String::new(), // the session mints one at preview time
        Some((id, None)) => id.to_string(),
        // The one refusal both doors make (`vike_client::verbs::order_client_id_refusal`); `help` states the charset.
        Some((_, Some(refusal))) => return Err(format!("submit: {}", refusal.rendered())),
    };

    // `buy`/`sell` sugar: `buy <venue> <symbol> <qty>` (3 positionals; side is the verb). Full form:
    // `submit <venue> <symbol> <buy|sell> <qty>` (4 positionals; side is the 3rd). The qty is the
    // LAST positional in BOTH shapes.
    let (venue, symbol, side) = match verb.to_ascii_lowercase().as_str() {
        "buy" | "sell" => {
            if positional.len() != 3 {
                return Err(format!("usage: {verb} <venue> <symbol> <qty> [@<price>|@market]"));
            }
            (positional[0], positional[1], side_arg(verb)?)
        }
        _ => {
            if positional.len() != 4 {
                return Err(USAGE.to_string());
            }
            (positional[0], positional[1], side_arg(positional[2])?)
        }
    };
    let quantity_token = positional.last().expect("checked non-empty above");
    let qty: f64 =
        quantity_token.parse().map_err(|_| format!("submit: not a qty: {quantity_token:?}"))?;

    // Resolve the order type from the @price token: absent or `@market` = market; `@<num>` = limit.
    let (order_type, price) = match price_token {
        None | Some("market") => ("market", None),
        Some(p) => {
            let px: f64 = p.parse().map_err(|_| format!("submit: not a price: @{p}"))?;
            ("limit", Some(px))
        }
    };
    // `--type` / `--trigger`, by the one-shot verb's own rules (its messages already say `submit:`).
    let trigger = super::super::order::trigger_order(kind, trigger, price.is_some())
        .map_err(|e| e.message)?;
    // `--time-in-force` / `--expires` / `--post-only`, `--at` / `--when-price` and `--display`, read by the
    // one-shot verb's own readers; the rules judge the built order below.
    let wire_kind = trigger.map_or(order_type, |(word, _)| word);
    let (time_in_force, gtd_expiry) =
        super::super::order::lifetime_flags(tif, expires, post_only).map_err(|e| e.message)?;
    let release = super::super::order::release_flag((at, when_price, when_price_on))
        .map_err(|e| e.message)?;
    let display_qty = super::super::order::display_flag(display).map_err(|e| e.message)?;
    let (trail, limit_offset, trail_percent) =
        super::super::order::trail_flags((trail, offset, trail_percent), wire_kind)
            .map_err(|e| e.message)?;
    let peg = super::super::order::peg_flag((peg, offset)).map_err(|e| e.message)?;
    let attributes = super::super::order::attributes_flag((all_or_none, sweep_to_fill, discretion))
        .map_err(|e| e.message)?;
    let adjust = super::super::order::adjust_flag(adjust).map_err(|e| e.message)?;
    let (venue, account) =
        super::super::selector::addressed(venue).map_err(|e| format!("submit: {}", e.message))?;

    let req = OrderRequestWire {
        // EMPTY unless `--order-client-id` pinned one. `run_write` fills it from the session's generator
        // before the preview prints — deliberately NOT here, so this parser stays PURE and every
        // grammar test below is deterministic. ⚠ It used to be left empty all the way onto the
        // wire "for the runtime to mint", which the node refuses outright.
        order_client_id,
        venue,
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: wire_kind.to_string(),
        price,
        trigger_price: trigger.and_then(|(_, px)| px),
        reduce_only,
        account,
        time_in_force,
        gtd_expiry,
        display_quantity: display_qty,
        release,
        trail,
        limit_offset,
        peg,
        attributes,
        trail_percent,
        adjust,
    };
    // An order that cannot be SIZED — a zero, negative or non-finite qty, a non-finite `@price` —
    // is refused HERE, by the function the one-shot verb and the MCP tool call too. ⚠ `parse::<f64>`
    // accepts `nan` and `inf`, so the parse above is not a finiteness check, and this used to test
    // only `qty.is_nan() || qty <= 0.0`: `+inf` and `@nan` reached the preview, which then printed
    // `within limits` for an order the node was certain to refuse.
    if let Some(why) = verbs::unsizeable_submit_refusal(&req) {
        return Err(format!("submit: {why}"));
    }
    // Its trail, prices, display, attributes, adjustment, lifetime and release, by the one rule both doors call
    // (`vike_client::verbs::submit_rules_refusal`), through the one-shot verb's own call and in its words.
    if let Some(why) = super::super::order::submit_rules_usage(&req) {
        return Err(why);
    }
    Ok(Verb::Submit(req))
}

/// Every flag of this line's `submit`: each changes WHAT order goes out — the two that make a trigger
/// order, the three of its lifetime (`--post-only` among them: swallowed by `--reason`, a maker-only order
/// would go out as a plain limit that takes liquidity), `--reduce-only` (swallowed, a close goes out as a
/// plain order that can OPEN a position) and `--order-client-id` (swallowed, the id the operator named is lost and
/// the session mints another). A `--reason` text containing one is refused ([`swallowed_flag`]).
const SUBMIT_FLAGS: [&str; 25] = [
    "--type",
    "--trigger",
    // swallowed, a trailing order goes out without its trail or its offset
    "--trail",
    "--trail-percent",
    "--offset",
    // swallowed, a pegged order is refused for want of its peg
    "--peg",
    "--time-in-force",
    // the retired spelling, refused by name before `--reason`: after it, still never swallowed
    "--tif",
    "--expires",
    "--post-only",
    "--reduce-only",
    "--order-client-id",
    // swallowed, an iceberg goes out wholly in view and a held order goes out NOW
    "--display",
    "--at",
    "--when-price",
    // swallowed, the condition reads the order's own venue's price
    "--when-price-on",
    // swallowed, a limit order goes out without its attribute: it fills in part, or at its price alone
    "--all-or-none",
    "--sweep-to-fill",
    "--discretion",
    // swallowed, a stop goes out that never adjusts
    "--adjust-at",
    "--adjust-to",
    "--adjust-stop",
    "--adjust-limit",
    "--adjust-trail",
    "--adjust-trail-percent",
];

/// The flags of this line's `modify` ([`parse_modify`]): swallowed, `--price` leaves only the qty changed.
const MODIFY_FLAGS: [&str; 3] = ["--qty", "--price", "--trigger"];

/// The flags of this line's `mount` (`grammar.rs`'s `parse_mount`): swallowed, `--params` starts the
/// strategy on defaults nobody chose and `--account` drops the account it was to trade.
const MOUNT_FLAGS: [&str; 4] = ["--strategy", "--account", "--id", "--params"];

/// The refusal for a `submit`/`buy`/`sell`, `modify` or `mount` line whose `--reason` text holds one of
/// its flags, or `None`. PURE.
///
/// `--reason` takes everything after it, so a flag typed there is rationale text and never reaches
/// the command: `submit … --reason why --type stop --trigger 100` would send a plain MARKET order. It is
/// refused instead, by the check and in the words of the one-shot verbs
/// (`crate::cmd::trade::oneshot`'s `refuse_flag_in_reason`).
pub(in crate::cmd::trade) fn swallowed_flag(line: &str, reason: Option<&str>) -> Option<String> {
    let verb = line.split_whitespace().next()?.to_ascii_lowercase();
    let (word, what, flags): (&str, &str, &[&str]) = match verb.as_str() {
        "submit" | "buy" | "sell" => ("submit", "the order", &SUBMIT_FLAGS),
        "modify" => ("modify", "the change", &MODIFY_FLAGS),
        "mount" => ("mount", "the mount", &MOUNT_FLAGS),
        _ => return None,
    };
    super::super::oneshot::refuse_flag_in_reason(word, what, reason, flags).err().map(|e| e.message)
}

/// `modify <order client id> [--qty Q] [--price P] [--trigger P]` — at least one of the three required
/// (`vike_client::verbs::modify_of`, the one-shot verb's and the MCP tool's builder too).
pub(in crate::cmd::trade) fn parse_modify(rest: &[&str]) -> Result<Verb, String> {
    const USAGE: &str = "usage: modify <order client id> [--qty Q] [--price P] [--trigger P]";
    let order_client_id = rest.first().ok_or(USAGE)?;
    let mut new_qty: Option<f64> = None;
    let mut new_price: Option<f64> = None;
    let mut new_trigger_price: Option<f64> = None;
    let mut i = 1;
    while i < rest.len() {
        match rest[i] {
            "--qty" => {
                let v = rest.get(i + 1).ok_or("modify: --qty needs a value")?;
                new_qty =
                    Some(super::super::order::modify_number("qty", v).map_err(|e| e.message)?);
                i += 2;
            }
            "--price" => {
                let v = rest.get(i + 1).ok_or("modify: --price needs a value")?;
                new_price =
                    Some(super::super::order::modify_number("price", v).map_err(|e| e.message)?);
                i += 2;
            }
            "--trigger" => {
                let v = rest.get(i + 1).ok_or("modify: --trigger needs a value")?;
                new_trigger_price =
                    Some(super::super::order::modify_number("trigger", v).map_err(|e| e.message)?);
                i += 2;
            }
            other => return Err(format!("modify: unexpected token {other:?}\n{USAGE}")),
        }
    }
    verbs::modify_of((*order_client_id).to_string(), new_qty, new_price, new_trigger_price)
        .map_err(|refusal| format!("modify: {}", refusal.rendered()))
}

/// A side argument → +1 / -1 through `vike_model::side_from_word` (the one side vocabulary every
/// order-placing surface reads), with this CLI's refusal wording.
pub(in crate::cmd::trade) fn side_arg(word: &str) -> Result<i32, String> {
    vike_model::side_from_word(word)
        .ok_or_else(|| format!("side must be buy|sell, got {:?}", word.to_ascii_lowercase()))
}

// ⚠ `parse_state` — `active`/`reducing`/`halted` → the wire trading state — was DELETED here by
// ruling 17. It existed only to read the argument of the `state` verb, and that argument is the
// defect: a word that turned a read into a halt. `halt` and `resume` name their own state, so
// nothing parses one from operator text on this surface any more. The MCP tool `set_trading_state`
// keeps its own three-way match (`crates/vike-mcp/src/tool_verbs.rs`'s `verb_from_tool_args`) — an agent naming a
// state in a JSON field is not a human adding a token to a read — and it is where `Reducing` is
// still spelled.
