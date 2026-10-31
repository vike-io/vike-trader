//! The order-verb grammar (pure): `submit` / `buy` / `sell`, `modify`, and the side words.

use vike_tradehub_client::wire::WireOrderRequest;

use crate::cmd::verbs;
use crate::cmd::verbs::Verb;

/// `submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]
/// [--type stop|stop-limit|take-profit --trigger <price>]
/// [--tif day|gtc|ioc|fok|gtd [--expires <when>] | --post-only]`. The leading verb may also be
/// `buy`/`sell` as sugar (then side is implied and the `<buy|sell>` token is omitted).
///
/// **A native trigger order and a named account are the one-shot `trade order submit`'s two pieces,
/// taken the same way.** `--type`/`--trigger` go through `order::trigger_order` (the one-shot verb's
/// own function, so the two grammars cannot differ on what a trigger order is, nor on what it
/// refuses: a zero, negative or NaN trigger, one flag without the other, a kind that is no trigger
/// kind). The account is the one-shot verb's BOOK form: `<venue>/<LABEL>` (or `<venue>/DEFAULT`) in
/// place of `<venue>`, read by `selector::parse`. ⚠ A BARE venue keeps naming NO account, as before
/// (the one-shot verb names `DEFAULT` there, positively; this line did not, and does not start now).
///
/// `--coid <id>` PINS the client-order-id instead of letting the session mint one. It exists
/// because the sibling MCP tool already takes an optional `client_order_id` and the two surfaces
/// share one vocabulary by design — plus the two cases a human actually hits: adopting an id
/// somebody else (a script, a runbook, the venue-side reconciliation sheet) already wrote down, and
/// scripting `--yes` where the caller needs to know the id BEFORE the command runs.
///
/// It is validated with `vike_model::is_valid_crypto_coid` — the same `^[A-Za-z0-9]{1,32}$` rule
/// the minter's own output is asserted against, and the strictest common denominator across venues.
/// Accepting something looser here would only move the failure to the venue edge, minutes later and
/// far from the typo. Left EMPTY (`--coid ""`) it is rejected outright rather than silently falling
/// back to a mint: an operator who names an id and gets a different one has lost the handle they
/// asked for.
///
/// A qty or `@price` that cannot be SIZED — a qty of zero, below or non-finite, a non-finite price —
/// is REFUSED here ([`verbs::unsizeable_submit_refusal`], which the one-shot `trade order submit`
/// and the MCP `submit_order` tool call too), not previewed. The cap check that follows is
/// advisory and stays so: an order that IS sizeable and over every cap still parses.
pub(super) fn parse_submit(verb: &str, rest: &[&str]) -> Result<Verb, String> {
    const USAGE: &str = "usage: submit <venue|venue/LABEL> <symbol> <buy|sell> <qty> \
                         [@<price>|@market] [--reduce-only] [--coid <id>] \
                         [--type stop|stop-limit|take-profit --trigger <price>] \
                         [--tif day|gtc|ioc|fok|gtd [--expires <when>] | --post-only]";
    let mut reduce_only = false;
    // the limit order's lifetime, by the one-shot verb's own rules (`order::lifetime_flags`)
    let (mut tif, mut expires, mut post_only): (Option<&str>, Option<&str>, bool) =
        (None, None, false);
    let mut positional: Vec<&str> = Vec::new();
    let mut price_tok: Option<&str> = None;
    let mut coid: Option<&str> = None;
    let mut expect_coid = false;
    let (mut kind, mut trigger): (Option<&str>, Option<&str>) = (None, None);
    // The flag (`--type` / `--trigger`) whose value is the next token.
    let mut expect_value: Option<&str> = None;
    for tok in rest {
        if expect_coid {
            expect_coid = false;
            coid = Some(tok);
            continue;
        }
        if let Some(flag) = expect_value.take() {
            // A value that is itself a flag means the operator's value went missing.
            if tok.starts_with("--") {
                return Err(format!("submit: {flag} needs a value, got {tok:?}"));
            }
            match flag {
                "--type" => kind = Some(tok),
                "--tif" => tif = Some(tok),
                "--expires" => expires = Some(tok),
                _ => trigger = Some(tok),
            }
            continue;
        }
        if *tok == "--reduce-only" {
            reduce_only = true;
        } else if *tok == "--post-only" {
            post_only = true;
        } else if let Some(v) = tok.strip_prefix("--tif=") {
            tif = Some(v);
        } else if *tok == "--tif" {
            expect_value = Some("--tif");
        } else if let Some(v) = tok.strip_prefix("--expires=") {
            expires = Some(v);
        } else if *tok == "--expires" {
            expect_value = Some("--expires");
        } else if let Some(v) = tok.strip_prefix("--coid=") {
            coid = Some(v);
        } else if *tok == "--coid" {
            expect_coid = true;
        } else if let Some(v) = tok.strip_prefix("--type=") {
            kind = Some(v);
        } else if *tok == "--type" {
            expect_value = Some("--type");
        } else if let Some(v) = tok.strip_prefix("--trigger=") {
            trigger = Some(v);
        } else if *tok == "--trigger" {
            expect_value = Some("--trigger");
        } else if let Some(p) = tok.strip_prefix('@') {
            if price_tok.is_some() {
                return Err("submit: more than one @price token".to_string());
            }
            price_tok = Some(p);
        } else {
            positional.push(tok);
        }
    }
    if expect_coid {
        return Err("submit: --coid needs a value".to_string());
    }
    if let Some(flag) = expect_value {
        return Err(format!("submit: {flag} needs a value"));
    }
    let client_order_id = match coid {
        None => String::new(), // the session mints one at preview time
        Some(id) if vike_model::is_valid_crypto_coid(id) => id.to_string(),
        Some(id) => {
            return Err(format!(
                "submit: --coid {id:?} is not a usable client_order_id — it must be {} (the \
                 strictest charset every venue accepts; `help` states it too)",
                verbs::COID_CHARSET
            ));
        }
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
    let qty_tok = positional.last().expect("checked non-empty above");
    let qty: f64 = qty_tok.parse().map_err(|_| format!("submit: not a qty: {qty_tok:?}"))?;

    // Resolve the order type from the @price token: absent or `@market` = market; `@<num>` = limit.
    let (order_type, price) = match price_tok {
        None | Some("market") => ("market", None),
        Some(p) => {
            let px: f64 = p.parse().map_err(|_| format!("submit: not a price: @{p}"))?;
            ("limit", Some(px))
        }
    };
    // `--type` / `--trigger`, by the one-shot verb's own rules (its messages already say `submit:`).
    let trigger = super::order::trigger_order(kind, trigger, price.is_some()).map_err(|e| e.msg)?;
    // `--tif` / `--expires` / `--post-only`, by the one-shot verb's own rules, judged with the kind
    // the order ends up as.
    let wire_kind = trigger.map_or(order_type, |(word, _)| word);
    let (time_in_force, gtd_expiry) =
        super::order::lifetime_flags(tif, expires, post_only, wire_kind).map_err(|e| e.msg)?;
    // The one-shot verb's BOOK form names the account: `<venue>/<LABEL>`. Only a token with a `/`
    // does; a bare venue names none, exactly as this line always did.
    let (venue, account) = match venue.split_once('/') {
        None => (venue.to_string(), None),
        Some(_) => {
            let book = super::selector::parse(venue).map_err(|e| format!("submit: {}", e.msg))?;
            (book.venue, Some(book.label.to_string()))
        }
    };

    let req = WireOrderRequest {
        // EMPTY unless `--coid` pinned one. `run_write` fills it from the session's generator
        // before the preview prints — deliberately NOT here, so this parser stays PURE and every
        // grammar test below is deterministic. ⚠ It used to be left empty all the way onto the
        // wire "for the runtime to mint", which the node refuses outright.
        client_order_id,
        venue,
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: wire_kind.to_string(),
        price,
        trigger_price: trigger.map(|(_, px)| px),
        reduce_only,
        account,
        time_in_force,
        gtd_expiry,
    };
    // An order that cannot be SIZED — a zero, negative or non-finite qty, a non-finite `@price` —
    // is refused HERE, by the function the one-shot verb and the MCP tool call too. ⚠ `parse::<f64>`
    // accepts `nan` and `inf`, so the parse above is not a finiteness check, and this used to test
    // only `qty.is_nan() || qty <= 0.0`: `+inf` and `@nan` reached the preview, which then printed
    // `within limits` for an order the node was certain to refuse.
    if let Some(why) = verbs::unsizeable_submit_refusal(&req) {
        return Err(format!("submit: {why}"));
    }
    Ok(Verb::Submit(req))
}

/// The flags of `submit` that change WHAT order goes out — the two that make a trigger order and the three
/// of its lifetime (`--post-only` among them: swallowed by `--reason`, a maker-only order would go out as a
/// plain limit that takes liquidity). A `--reason` text containing one is refused
/// ([`swallowed_trigger_flag`]).
const TRIGGER_FLAGS: [&str; 5] = ["--type", "--trigger", "--tif", "--expires", "--post-only"];

/// The refusal for a `submit`/`buy`/`sell` line whose `--reason` text holds `--type` or `--trigger`,
/// or `None`. PURE.
///
/// `--reason` takes everything after it, so a flag typed there is rationale text and never reaches
/// the order: `submit … --reason why --type stop --trigger 100` would send a plain MARKET order. It is
/// refused instead, in the words of the one-shot verb's own refusal
/// (`crate::cmd::trade::order`'s `submit_verb`). ⚠ Only the two NEW flags are covered here: the one-shot
/// verb refuses every `submit` flag after `--reason`, `--reduce-only` and `--coid` included, and this
/// line has never done that — widening it is one more entry in [`TRIGGER_FLAGS`], a decision left open.
pub(super) fn swallowed_trigger_flag(line: &str, reason: Option<&str>) -> Option<String> {
    let verb = line.split_whitespace().next()?.to_ascii_lowercase();
    if !matches!(verb.as_str(), "submit" | "buy" | "sell") {
        return None;
    }
    let flag = reason?.split_whitespace().find_map(|word| {
        TRIGGER_FLAGS.into_iter().find(|flag| {
            word == *flag || word.strip_prefix(*flag).is_some_and(|rest| rest.starts_with('='))
        })
    })?;
    Some(format!(
        "submit: `{flag}` appears after --reason, which takes everything after it as the \
         rationale, so it would be ignored and the order would go out without it. Put `{flag}` \
         BEFORE --reason"
    ))
}

/// `modify <coid> [--qty Q] [--price P]` — at least one of `--qty`/`--price` required.
pub(super) fn parse_modify(rest: &[&str]) -> Result<Verb, String> {
    const USAGE: &str = "usage: modify <coid> [--qty Q] [--price P]";
    let coid = rest.first().ok_or(USAGE)?;
    let mut new_qty: Option<f64> = None;
    let mut new_price: Option<f64> = None;
    let mut i = 1;
    while i < rest.len() {
        match rest[i] {
            "--qty" => {
                let v = rest.get(i + 1).ok_or("modify: --qty needs a value")?;
                new_qty = Some(v.parse().map_err(|_| format!("modify: not a qty: {v:?}"))?);
                i += 2;
            }
            "--price" => {
                let v = rest.get(i + 1).ok_or("modify: --price needs a value")?;
                new_price = Some(v.parse().map_err(|_| format!("modify: not a price: {v:?}"))?);
                i += 2;
            }
            other => return Err(format!("modify: unexpected token {other:?}\n{USAGE}")),
        }
    }
    if new_qty.is_none() && new_price.is_none() {
        return Err("modify: nothing to change — pass --qty and/or --price".to_string());
    }
    Ok(Verb::Modify { client_order_id: (*coid).to_string(), new_qty, new_price })
}

/// A side argument → +1 / -1 through `vike_model::side_from_word` (the one side vocabulary every
/// order-placing surface reads), with this CLI's refusal wording.
pub(super) fn side_arg(word: &str) -> Result<i32, String> {
    vike_model::side_from_word(word)
        .ok_or_else(|| format!("side must be buy|sell, got {:?}", word.to_ascii_lowercase()))
}

// ⚠ `parse_state` — `active`/`reducing`/`halted` → the wire trading state — was DELETED here by
// ruling 17. It existed only to read the argument of the `state` verb, and that argument is the
// defect: a word that turned a read into a halt. `halt` and `resume` name their own state, so
// nothing parses one from operator text on this surface any more. The MCP tool `set_trading_state`
// keeps its own three-way match (`crate::cmd::verbs`'s `verb_from_tool_args`) — an agent naming a
// state in a JSON field is not a human adding a token to a read — and it is where `Reducing` is
// still spelled.
