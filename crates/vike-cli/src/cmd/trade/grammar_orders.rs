//! The order-verb grammar (pure): `submit` / `buy` / `sell`, `modify`, and the side words.

use vike_tradehub_client::wire::WireOrderRequest;

use crate::cmd::verbs;
use crate::cmd::verbs::Verb;

/// `submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]`. The
/// leading verb may also be `buy`/`sell` as sugar (then side is implied and the `<buy|sell>` token
/// is omitted).
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
    const USAGE: &str = "usage: submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] \
                         [--reduce-only] [--coid <id>]";
    let mut reduce_only = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut price_tok: Option<&str> = None;
    let mut coid: Option<&str> = None;
    let mut expect_coid = false;
    for tok in rest {
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
            (positional[0], positional[1], side_from_word(verb)?)
        }
        _ => {
            if positional.len() != 4 {
                return Err(USAGE.to_string());
            }
            (positional[0], positional[1], side_from_word(positional[2])?)
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

    let req = WireOrderRequest {
        // EMPTY unless `--coid` pinned one. `run_write` fills it from the session's generator
        // before the preview prints — deliberately NOT here, so this parser stays PURE and every
        // grammar test below is deterministic. ⚠ It used to be left empty all the way onto the
        // wire "for the runtime to mint", which the node refuses outright.
        client_order_id,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: order_type.to_string(),
        price,
        trigger_price: None,
        reduce_only,
        account: None,
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

/// `buy`/`sell` (or `b`/`s`) → +1 / -1.
pub(super) fn side_from_word(word: &str) -> Result<i32, String> {
    match word.to_ascii_lowercase().as_str() {
        "buy" | "b" | "long" => Ok(1),
        "sell" | "s" | "short" => Ok(-1),
        other => Err(format!("side must be buy|sell, got {other:?}")),
    }
}

// ⚠ `parse_state` — `active`/`reducing`/`halted` → the wire trading state — was DELETED here by
// ruling 17. It existed only to read the argument of the `state` verb, and that argument is the
// defect: a word that turned a read into a halt. `halt` and `resume` name their own state, so
// nothing parses one from operator text on this surface any more. The MCP tool `set_trading_state`
// keeps its own three-way match (`crate::cmd::verbs`'s `verb_from_tool_args`) — an agent naming a
// state in a JSON field is not a human adding a token to a read — and it is where `Reducing` is
// still spelled.
