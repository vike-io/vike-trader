//! The MCP write tools' DIALECT of the shared verb vocabulary: JSON arguments in, a
//! [`vike_client::verbs::Verb`] out. The vocabulary, the one [`CommandWire`] construction site, the
//! order client id mint and the guardrail are `vike_client::verbs`' and every surface shares them; what lives
//! here is the part only a tool call has, the reading of its arguments.
//!
//! Everything here is PURE: no network and no environment.

use serde_json::{Value, json};
use vike_client::verbs::{
    Verb, release_price_from_word, release_venue_from_word, stop_adjust_from_terms,
    unsizeable_submit_refusal,
};
use vike_model::{OrderAttributes, OrderStopAdjust, OrderType, TradingState};
use vike_tradehub_client::wire::{CommandWire, OrderPegWire, OrderReleaseWire, OrderRequestWire};

/// Build the WRITE [`Verb`] an MCP tool names from its JSON arguments. PURE — no network — so the
/// preview path is fully testable. Missing/invalid required fields are a clean error. Tool names
/// are the MCP write roster (`mcp`'s `WRITE_TOOLS`): the seven order verbs, then the three
/// node-lifecycle ones.
///
/// `submit_order` also refuses an order that cannot be sized — a `qty` of zero or below, a
/// non-finite one, a non-finite `price`, a `limit` with no `price`, or a `market` with a `price` or
/// a `trigger_price` — through [`unsizeable_submit_refusal`], the same function the REPL and the
/// one-shot verb call, so the three never differ about what a malformed order is. Its PRICES are
/// then judged by `vike_model::order_terms_refusal`, the rule the node's door applies again: a stop,
/// stop-limit or take-profit with no `trigger_price` above zero is refused here, and so are a trailing
/// order with no `trail` above zero or a `price`, a `trail` on any other kind, a pegged order with no
/// `peg` or with a price, and a `peg` on any other kind. Its ATTRIBUTES and a stop's ADJUSTMENT are judged
/// by `vike_model::attributes_refusal` and `vike_model::adjust_refusal`, and a price condition's venue
/// (`release_when_price_on`) by `vike_client::verbs::release_venue_from_word`, each the door's rule too.
///
/// Each lifecycle refusal below is an AUTHORING mistake rather than a policy one, and is spelled
/// as the wire variant's own doc spells the rule:
///
///   * `mount_strategy` takes exactly one of `name` / `rhai` — the profile `[strategy]` vocabulary
///     verbatim (`docs/decisions/0024-rhai-strategies-live.md`), so "both" and "neither" are both
///     refused here rather than left for the daemon's edge to phrase. The REPL, and the one-shot
///     `vike-cli trade strategy mount` that MIRRORS its grammar rather than inventing a second one
///     (task 8 of the trade-CLI-plane), refuse the same rule in that shared flag spelling — which
///     is the one thing about these three that is genuinely per-VOCABULARY rather than per-tool;
///   * `params` must be a JSON OBJECT when it is present. The wire carries it opaquely as a
///     `serde_json::Value` (the delegate-don't-mirror idiom — each strategy reads its own knobs),
///     which means nothing between here and the strategy would object to an ARRAY or a bare number
///     until the mount itself failed on the node;
///   * `set_setting`'s `value` is TEXT by declaration — see [`setting_value`].
///
/// ⚠ **A present value of the wrong type is REFUSED, never read as absent** ([`typed_arg`], every
/// argument below goes through it): an absent `venue` means every engine, so `{"venue": 5}` read as
/// absent turned a one-venue request into the widest command the node takes. A blank `venue` or
/// `symbol` on `market_exit` / `mass_cancel` is refused for the same reason ([`scope_arg`]).
pub(crate) fn verb_from_tool_args(name: &str, args: &Value) -> Result<Verb, String> {
    let s = |k: &str| typed_arg(name, args, k, "text", Value::as_str).map(|v| v.map(str::to_owned));
    let f = |k: &str| typed_arg(name, args, k, "a number", Value::as_f64);
    match name {
        "submit_order" => {
            let side = typed_arg(name, args, "side", "an integer (1 buy / -1 sell)", |v| {
                v.as_i64().filter(|n| matches!(n, 1 | -1)).and_then(|n| i32::try_from(n).ok())
            })?
            .ok_or("submit_order requires `side` (1 buy / -1 sell)")?;
            // The INPUT door (`OrderType::parse`, trimmed and case-insensitive): an unknown kind is
            // refused HERE, naming the accepted words, rather than sent for the node to refuse —
            // and a known one travels as its canonical word, so `"LIMIT"` goes out as `"limit"`.
            let order_type = match s("order_type")? {
                None => OrderType::Market,
                Some(word) => OrderType::parse(&word)
                    .map_err(|e| format!("submit_order: `order_type` — {e}"))?,
            };
            // The LIFETIME, by the node's own reader (`TimeInForce::parse`: any case, `at_open` or
            // `at-open`); absent is the default `gtc`, which goes out as the absent field (the frame from
            // before the argument).
            let lifetime = s("time_in_force")?
                .map(|word| vike_model::TimeInForce::parse(&word))
                .transpose()
                .map_err(|e| format!("submit_order: `time_in_force` — {e}"))?
                .unwrap_or_default();
            let gtd_expiry = typed_arg(
                name,
                args,
                "gtd_expiry",
                "an integer (epoch milliseconds)",
                Value::as_i64,
            )?;
            let req = OrderRequestWire {
                order_client_id: s("order_client_id")?
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or_default(),
                venue: named_arg(name, args, "venue")?,
                symbol: named_arg(name, args, "symbol")?,
                side,
                qty: f("qty")?.ok_or("submit_order requires `qty`")?,
                order_type: order_type.as_str().to_owned(),
                price: f("price")?,
                trigger_price: f("trigger_price")?,
                reduce_only: typed_arg(name, args, "reduce_only", "true or false", Value::as_bool)?
                    .unwrap_or(false),
                account: account_arg("submit_order", args)?,
                time_in_force: (lifetime != vike_model::TimeInForce::Gtc)
                    .then(|| lifetime.as_word().to_owned()),
                gtd_expiry,
                display_quantity: f("display_qty")?,
                release: release_arg(args)?,
                trail: f("trail")?,
                limit_offset: f("limit_offset")?,
                peg: peg_arg(s("peg")?, f("peg_offset")?)?,
                attributes: attributes_arg(args)?,
                trail_percent: f("trail_percent")?,
                adjust: adjust_arg(args)?,
            };
            // The same refusal the REPL and the one-shot verb make at their own parse — and the
            // only one of the three that can see a `limit` with no `price` or a `market` with a
            // `price` or a `trigger_price`, because the other two grammars make a limit BY naming
            // its price and a trigger order BY naming its kind. JSON has no non-finite number, so on
            // this path the live rows are a zero or negative `qty` and those, an omitted
            // `order_type` reading as `market`.
            if let Some(why) = unsizeable_submit_refusal(&req) {
                return Err(format!("submit_order: {why}"));
            }
            // The PRICES, by the rule the node's door applies again: a trigger kind with no trigger
            // above zero (only this grammar can build one), a stop-limit with no limit above zero, a
            // trigger kind's stated limit at zero or below, and a trailing order's trail, offset and
            // activation. A `trail` on any other kind is refused too, as the node's door refuses it (its
            // paper book would lower it as a trailing stop). Refused before a preview token is minted.
            if let Some(t) = req.trail
                && !order_type.is_trailing()
            {
                return Err(format!(
                    "submit_order: `trail` belongs to a trailing or trailing_limit order; this \
                     {order_type} order states {t}"
                ));
            }
            // `peg_arg` read the anchor word, so a wire peg here always names one of the four
            let peg = req.peg.as_ref().and_then(|p| {
                let anchor = vike_model::OrderPegAnchor::parse(&p.anchor)?;
                Some(vike_model::OrderPeg { anchor, offset: p.offset })
            });
            if let Some(why) = vike_model::order_terms_refusal(
                &order_type,
                req.price,
                req.trigger_price,
                req.trail,
                req.limit_offset,
                peg.as_ref(),
                req.trail_percent,
            ) {
                return Err(format!("submit_order: {why}"));
            }
            // The display, the attributes, the adjustment, the lifetime and the release, by the rules the node's
            // door applies again (the lifetime first, then the release judged with it).
            if let Some(why) =
                vike_model::display_refusal(&order_type, req.qty, req.display_quantity)
            {
                return Err(format!("submit_order: `display_qty` — {why}"));
            }
            if let Some(why) =
                vike_model::attributes_refusal(&order_type, lifetime, req.attributes.as_ref())
            {
                return Err(format!("submit_order: {why}"));
            }
            if let Some(why) = vike_model::stop_adjust_refusal(
                &order_type,
                side,
                req.trigger_price,
                req.adjust.as_ref(),
            ) {
                return Err(format!("submit_order: {why}"));
            }
            let now = vike_model::now_ms();
            if let Some(why) =
                vike_model::lifetime_refusal(&order_type, lifetime, gtd_expiry, None, now)
            {
                // The rule judges the pair, so the refusal names both arguments.
                return Err(format!("submit_order: `time_in_force` / `gtd_expiry` — {why}"));
            }
            // The rule judges the moment; the price condition's word was read by `release_arg`.
            let release =
                req.release.as_ref().map(|r| vike_model::OrderRelease { at: r.at, price: None });
            if let Some(why) =
                vike_model::lifetime_refusal(&order_type, lifetime, gtd_expiry, release, now)
            {
                return Err(format!("submit_order: `release_at` — {why}"));
            }
            Ok(Verb::Submit(req))
        }
        "submit_order_list" => order_list_verb(args),
        "submit_combo" => combo_verb(args),
        "cancel_order" => Ok(Verb::Cancel(named_arg(name, args, "order_client_id")?)),
        "modify" => {
            let order_client_id = named_arg(name, args, "order_client_id")?;
            let (new_qty, new_price) = (f("new_qty")?, f("new_price")?);
            let new_trigger_price = f("new_trigger_price")?;
            if new_qty.is_none() && new_price.is_none() && new_trigger_price.is_none() {
                // Mirrors the REPL's "nothing to change" rule — a Modify that changes nothing is
                // an authoring mistake, not a command.
                return Err("modify requires at least one of `new_qty` / `new_price` / \
                            `new_trigger_price` (nothing to change)"
                    .to_string());
            }
            Ok(Verb::Modify { order_client_id, new_qty, new_price, new_trigger_price })
        }
        // The wire's account-carrying rules are argued in `crates/vike-cli/src/cmd/trade/oneshot.rs`'s module doc.
        // An ABSENT `account` stays `None` (the node then refuses a venue that mounts two accounts of
        // it, `crate::venue_gate`); a venue-less `market_exit` / `mass_cancel` that names
        // an account is refused by the NODE's own edge (`account_refusal`), not here.
        "flatten" => Ok(Verb::Flatten {
            venue: named_arg(name, args, "venue")?,
            symbol: named_arg(name, args, "symbol")?,
            account: account_arg("flatten", args)?,
        }),
        "market_exit" => Ok(Verb::MarketExit {
            venue: scope_arg(name, args, "venue")?,
            account: account_arg("market_exit", args)?,
        }),
        "set_trading_state" => {
            let state = match s("state")?.ok_or("set_trading_state requires `state`")?.as_str() {
                "active" => TradingState::Active,
                "reducing" => TradingState::Reducing,
                "halted" => TradingState::Halted,
                other => {
                    return Err(format!("unknown state {other:?} (active | reducing | halted)"));
                }
            };
            Ok(Verb::SetState(state))
        }
        "mass_cancel" => Ok(Verb::MassCancel {
            venue: scope_arg(name, args, "venue")?,
            symbol: scope_arg(name, args, "symbol")?,
            account: account_arg("mass_cancel", args)?,
        }),
        "mount_strategy" => {
            let (name_src, rhai_src) = (s("name")?, s("rhai")?);
            match (&name_src, &rhai_src) {
                (Some(_), Some(_)) => {
                    return Err(
                        "mount_strategy takes EXACTLY ONE of `name` (a registry strategy) \
                                and `rhai` (a script path on the node) — both were given"
                            .to_string(),
                    );
                }
                (None, None) => {
                    return Err(
                        "mount_strategy requires a strategy source: exactly one of `name` \
                                (a registry strategy) or `rhai` (a script PATH on the node's \
                                filesystem — not script source)"
                            .to_string(),
                    );
                }
                _ => {}
            }
            let params = match args.get("params") {
                None | Some(Value::Null) => json!({}),
                Some(v) if v.is_object() => v.clone(),
                Some(_) => {
                    return Err("mount_strategy's `params` must be an object (the \
                                [strategy.params] table) — it is carried to the strategy opaquely, \
                                so nothing below this would object to another shape"
                        .to_string());
                }
            };
            let account = account_arg("mount_strategy", args)?;
            Ok(Verb::MountStrategy {
                venue: s("venue")?.ok_or("mount_strategy requires `venue`")?,
                account,
                symbol: s("symbol")?.ok_or("mount_strategy requires `symbol`")?,
                interval: s("interval")?
                    .ok_or("mount_strategy requires `interval` (e.g. \"1m\")")?,
                controller_id: s("controller_id")?,
                // MCP work is deferred (owner 11-10): the tool keeps its two retired fields, which the
                // node refuses with the fix, until the tool is given a `file` of its own.
                file: None,
                name: name_src,
                rhai: rhai_src,
                params,
            })
        }
        "unmount_strategy" => Ok(Verb::UnmountStrategy {
            controller_id: s("controller_id")?
                .ok_or("unmount_strategy requires `controller_id` (the mount id)")?,
        }),
        // No `file` and no `policy_confirm`: the key names its section, and the retype went with
        // `docs/decisions/0086` point 7. An agent that still passes either is ignored like any
        // argument a tool does not name — see [`Verb::SetSetting`] for what goes on the wire.
        "set_setting" => Ok(Verb::SetSetting {
            key: s("key")?.ok_or("set_setting requires `key` (the full dotted key)")?,
            value: setting_value(args)?,
        }),
        other => Err(format!("no wire command for {other}")),
    }
}

/// One argument of a write tool, the ONE reading of every argument [`verb_from_tool_args`] takes: a
/// missing or `null` key is `None`, a value `read` takes is `Some`, and any other value is REFUSED
/// naming the tool and the argument. So a refusal comes before a preview token can be minted.
///
/// What it got is echoed through [`crate::trace::redact`], because a refusal lands in the agent
/// transcript's `detail`, which is not redacted: the echo must not be how a value leaves that promise.
fn typed_arg<'a, T>(
    tool: &str,
    args: &'a Value,
    key: &str,
    want: &str,
    read: impl FnOnce(&'a Value) -> Option<T>,
) -> Result<Option<T>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => read(v).map(Some).ok_or_else(|| {
            format!("{tool}: `{key}` must be {want} - got {}", crate::trace::redact(v))
        }),
    }
}

/// A `venue` or `symbol` that narrows `market_exit` / `mass_cancel`, and is omitted to mean all of them.
///
/// ⚠ A blank one is refused, because it names no scope and it does not mean "all" either. A blank
/// venue on a node that has not published yet passes both venue gates, and the core then gives it
/// engine 0 (`crates/vike-core/src/runtime/routing.rs`'s `exit_scope_engines`), whose whole book
/// goes. A blank symbol cancels only the orders that carry no symbol, and is answered `accepted`.
fn scope_arg(tool: &str, args: &Value, key: &str) -> Result<Option<String>, String> {
    match typed_arg(tool, args, key, "text", Value::as_str)? {
        Some(v) if v.trim().is_empty() => Err(format!(
            "{tool}: `{key}` is blank, which names nothing: name one, or omit it to mean all of them"
        )),
        v => Ok(v.map(str::to_owned)),
    }
}

/// A `venue` or `symbol` an order must name, or the id a cancel or a modify must name. A blank one
/// names nothing, so it never reaches the node, where an empty symbol may resolve to the mounted one:
/// an order the agent did not name.
fn named_arg(tool: &str, args: &Value, key: &str) -> Result<String, String> {
    match typed_arg(tool, args, key, "text", Value::as_str)? {
        None => Err(format!("{tool} requires `{key}`")),
        Some(v) if v.trim().is_empty() => {
            Err(format!("{tool}: `{key}` is blank, which names nothing"))
        }
        Some(v) => Ok(v.to_owned()),
    }
}

/// The OPTIONAL `account` argument of an MCP write tool, as the WIRE string the order/mount/reduce
/// variants carry (absent names none, `DEFAULT` names the unlabelled account deliberately, a label
/// names that account — [`vike_tradehub_client::wire::OrderRequestWire::account`] states the three).
///
/// Validated HERE so a typo costs a parse error rather than a round trip and a `Response::Error`,
/// and with the wire's own reader (`parse_wire_account`), so this edge adds a message rather than a
/// second grammar. A missing or `null` `account` is absent; a value that is present and is NOT text
/// (a number, a list, an object, a boolean) is REFUSED, never read as absent: an account-less
/// command is not the safe direction here, because a `market_exit` or `mass_cancel` with no venue
/// means EVERY engine, so `{"account": ["ALT"]}` must not quietly turn a one-account request into
/// the widest action the node can take (the node's `lower_command` refuses a typo the same way
/// instead of dropping it). The account rides INSIDE the [`CommandWire`], so the preview token
/// (bound to the whole command) is bound to it too: a token issued for one account cannot confirm
/// the same order on another.
fn account_arg(tool: &str, args: &Value) -> Result<Option<String>, String> {
    let want = "text - an account label such as LIVE, or \"DEFAULT\" for the venue's unlabelled \
                account";
    let Some(a) = typed_arg(tool, args, "account", want, Value::as_str)
        .map_err(|e| format!("{e}. Omit it to name no account"))?
    else {
        return Ok(None);
    };
    vike_model::accounts::account_keys::parse_wire_account(a).map(|_| Some(a.to_string())).map_err(
        |e| {
            format!(
                "{tool}: `account` — {e}. Omit it to name no account, or pass \"DEFAULT\" to name \
                 the venue's unlabelled account deliberately"
            )
        },
    )
}

/// `submit_order`'s `release_at` (epoch ms) and `release_when_price` (`above:<price>` / `below:<price>`, the
/// CLI's `--when-price` word, read by the same `vike_client::verbs::release_price_from_word`) as the wire's
/// release, and `release_when_price_on`, the venue that condition's price is read on (the CLI's
/// `--when-price-on`, checked by the same `release_venue_from_word`). A price condition rides a moment, so
/// `release_when_price` alone is refused, and a venue rides a condition, so `release_when_price_on` alone is too.
fn release_arg(args: &Value) -> Result<Option<OrderReleaseWire>, String> {
    let tool = "submit_order";
    let at = typed_arg(tool, args, "release_at", "an integer (epoch milliseconds)", Value::as_i64)?;
    let when = typed_arg(tool, args, "release_when_price", "text", Value::as_str)?;
    let on = typed_arg(tool, args, "release_when_price_on", "text", Value::as_str)?;
    let condition = when
        .map(release_price_from_word)
        .transpose()
        .map_err(|e| format!("{tool}: `release_when_price` — {e}"))?;
    let venue = match (on, &condition) {
        (None, _) => None,
        (Some(_), None) => {
            return Err(format!(
                "{tool}: `release_when_price_on` needs `release_when_price`: it names the venue whose \
                 price that condition reads"
            ));
        }
        (Some(on), Some((_, symbol))) => Some(
            release_venue_from_word(on, symbol.as_deref())
                .map_err(|e| format!("{tool}: `release_when_price_on` — {e}"))?,
        ),
    };
    match (at, condition) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(format!(
            "{tool}: `release_when_price` needs `release_at`: a held order is released at a moment, \
             and the price condition is checked from then"
        )),
        (Some(at), condition) => {
            let (price, symbol) = condition.unzip();
            Ok(Some(OrderReleaseWire { at, price, symbol: symbol.flatten(), venue }))
        }
    }
}

/// `submit_order`'s `all_or_none`, `sweep_to_fill` and `discretionary_amount` as the wire's attributes: `None`
/// unless one is set, never an empty set. Whether the order takes them is `vike_model::attributes_refusal`'s.
fn attributes_arg(args: &Value) -> Result<Option<OrderAttributes>, String> {
    let tool = "submit_order";
    let flag = |k: &str| {
        typed_arg(tool, args, k, "true or false", Value::as_bool).map(Option::unwrap_or_default)
    };
    let attributes = OrderAttributes {
        all_or_none: flag("all_or_none")?,
        sweep_to_fill: flag("sweep_to_fill")?,
        discretionary_amount: typed_arg(
            tool,
            args,
            "discretionary_amount",
            "a number",
            Value::as_f64,
        )?,
    };
    Ok((attributes != OrderAttributes::default()).then_some(attributes))
}

/// `submit_order`'s `adjust_at` and its target (`adjust_to`, `adjust_stop`, `adjust_limit`, `adjust_trail`,
/// `adjust_trail_percent`) as the wire's adjustment, through the reader the CLI shares
/// (`vike_client::verbs::stop_adjust_from_terms`). A target's term with no `adjust_at` is refused, never
/// dropped; whether the numbers hold is `vike_model::adjust_refusal`'s.
fn adjust_arg(args: &Value) -> Result<Option<OrderStopAdjust>, String> {
    let tool = "submit_order";
    let f = |k: &str| typed_arg(tool, args, k, "a number", Value::as_f64);
    let at = f("adjust_at")?;
    let to = typed_arg(tool, args, "adjust_to", "text", Value::as_str)?;
    let (stop, limit) = (f("adjust_stop")?, f("adjust_limit")?);
    let (trail, trail_percent) = (f("adjust_trail")?, f("adjust_trail_percent")?);
    let Some(at) = at else {
        let terms = [
            ("adjust_to", to.is_some()),
            ("adjust_stop", stop.is_some()),
            ("adjust_limit", limit.is_some()),
            ("adjust_trail", trail.is_some()),
            ("adjust_trail_percent", trail_percent.is_some()),
        ];
        return match terms.iter().find(|(_, given)| *given) {
            None => Ok(None),
            Some((k, _)) => Err(format!(
                "{tool}: `{k}` needs `adjust_at`, the level at which the stop becomes its target"
            )),
        };
    };
    let to = to.ok_or_else(|| {
        format!(
            "{tool}: `adjust_at` needs `adjust_to` (stop | stop_limit | trailing), what the stop \
             becomes"
        )
    })?;
    stop_adjust_from_terms(at, to, stop, limit, trail, trail_percent)
        .map(Some)
        .map_err(|e| format!("{tool}: {e}"))
}

/// `submit_order`'s `peg` (the anchor word, exactly `best`, `mid`, `market` or `primary`) and `peg_offset`
/// (default 0) as the wire's peg. A `peg_offset` with no `peg` is refused; whether the kind takes a peg is
/// `vike_model::order_terms_refusal`'s.
fn peg_arg(anchor: Option<String>, offset: Option<f64>) -> Result<Option<OrderPegWire>, String> {
    let tool = "submit_order";
    let Some(anchor) = anchor else {
        return match offset {
            None => Ok(None),
            Some(_) => {
                Err(format!("{tool}: `peg_offset` needs `peg`, the anchor it is measured from"))
            }
        };
    };
    if vike_model::OrderPegAnchor::parse(&anchor).is_none() {
        return Err(format!(
            "{tool}: `peg` — the anchor {anchor:?} is not one of {}",
            vike_model::OrderPegAnchor::accepted_words()
        ));
    }
    Ok(Some(OrderPegWire { anchor, offset: offset.unwrap_or(0.0) }))
}

/// `submit_order_list`: the `shape`'s arguments onto the ONE builder each CLI spelling uses too (`vike_client::verbs`'
/// `oco_list` / `oto_list` / `ladder_list`), so the tool and `trade order oco|oto|ladder` build the same legs. Every
/// argument is read (and a wrong type refused) first; then one the `shape` does not take is REFUSED, never ignored.
/// (`shape`, not `kind`: `delete_series` names its selector `kind`, and the write tools share one argument union.)
fn order_list_verb(args: &Value) -> Result<Verb, String> {
    let tool = "submit_order_list";
    let s = |k: &str| typed_arg(tool, args, k, "text", Value::as_str).map(|v| v.map(str::to_owned));
    let f = |k: &str| typed_arg(tool, args, k, "a number", Value::as_f64);
    let side = typed_arg(tool, args, "side", "an integer (1 buy / -1 sell)", |v| {
        v.as_i64().filter(|n| matches!(n, 1 | -1)).and_then(|n| i32::try_from(n).ok())
    })?
    .ok_or("submit_order_list requires `side` (1 buy / -1 sell)")?;
    let head = vike_client::verbs::OrderListHead {
        venue: named_arg(tool, args, "venue")?,
        account: account_arg(tool, args)?,
        symbol: named_arg(tool, args, "symbol")?,
        side,
        qty: f("qty")?.ok_or("submit_order_list requires `qty`")?,
    };
    let (entry, limit, stop, stop_limit) =
        (f("entry_price")?, f("limit_price")?, f("stop_price")?, f("stop_limit_price")?);
    let reduce_only = typed_arg(tool, args, "reduce_only", "true or false", Value::as_bool)?;
    // A ladder's stop and targets in the bracket's words, `stop_loss` and `take_profit`.
    let (stop_loss, take_profit) = (f("stop_loss")?, s("take_profit")?);
    // The caller's own leg ids, as `submit_order`'s `order_client_id`: a blank one reads as omitted (minted).
    let order_client_ids: Vec<String> = s("order_client_ids")?
        .filter(|ids| !ids.trim().is_empty())
        .map(|ids| ids.split(',').map(|id| id.trim().to_string()).collect())
        .unwrap_or_default();
    let shape = s("shape")?.ok_or("submit_order_list requires `shape` (oco | oto | ladder)")?;
    let takes: &[&str] = match shape.as_str() {
        "oco" => &["limit_price", "stop_price", "stop_limit_price", "reduce_only"],
        "oto" => &["entry_price", "limit_price", "stop_price"],
        "ladder" => &["entry_price", "stop_loss", "take_profit"],
        other => {
            return Err(format!("submit_order_list: `shape` is oco | oto | ladder, got {other:?}"));
        }
    };
    let optional = [
        "entry_price",
        "limit_price",
        "stop_price",
        "stop_limit_price",
        "reduce_only",
        "stop_loss",
        "take_profit",
    ];
    if let Some(k) =
        optional.iter().find(|k| !takes.contains(*k) && args.get(**k).is_some_and(|v| !v.is_null()))
    {
        return Err(format!("submit_order_list: `{k}` is not an argument of shape {shape}"));
    }
    let need = |v: Option<f64>, k: &str| {
        v.ok_or_else(|| format!("submit_order_list: a {shape} list requires `{k}`"))
    };
    let list = match shape.as_str() {
        "oco" => vike_client::verbs::oco_list(
            head,
            need(limit, "limit_price")?,
            need(stop, "stop_price")?,
            stop_limit,
            reduce_only.unwrap_or(false),
        ),
        "oto" => vike_client::verbs::oto_list(head, entry, stop, limit),
        _ => {
            let targets = take_profit
                .ok_or(
                    "submit_order_list: a ladder list requires `take_profit` (price:qty,price:qty)",
                )?
                .split(',')
                .map(|t| {
                    let (px, qty) = t.trim().split_once(':')?;
                    Some((px.trim().parse().ok()?, qty.trim().parse().ok()?))
                })
                .collect::<Option<Vec<(f64, f64)>>>()
                .ok_or("submit_order_list: `take_profit` is price:qty,price:qty")?;
            vike_client::verbs::ladder_list(head, entry, need(stop_loss, "stop_loss")?, &targets)
        }
    };
    list.and_then(|list| vike_client::verbs::with_leg_order_client_ids(list, &order_client_ids))
        .map(Verb::OrderList)
        .map_err(|e| format!("submit_order_list: {e}"))
}

/// `submit_combo`: the arguments onto `vike_client::verbs::combo_of`, the builder `trade order combo` uses too, which
/// refuses in the model's words what the node's door refuses (a mix of a net and leg limits, a leg with no limit while
/// another has one, one leg, a zero ratio). Every argument and every leg's fields are read (and a wrong type refused)
/// first. ⚠ A price under another name is REFUSED, never ignored: a combo naming none is a combo MARKET order, so a
/// `price` (or a leg's `price`) the tool dropped would send one. The combo's ONE `order_client_id` is read as
/// `submit_order`'s (a blank one is none); with none, the server mints it before the preview
/// (`vike_client::verbs::fill_order_client_id`).
fn combo_verb(args: &Value) -> Result<Verb, String> {
    let tool = "submit_combo";
    if let Some(k) = ["price", "limit", "limit_price"]
        .iter()
        .find(|k| args.get(**k).is_some_and(|v| !v.is_null()))
    {
        return Err(format!(
            "{tool}: `{k}` is not an argument: a combo is priced by `net_limit` or by each leg's `limit`"
        ));
    }
    let side = typed_arg(tool, args, "side", "an integer (1 buy / -1 sell)", |v| {
        v.as_i64().filter(|n| matches!(n, 1 | -1)).and_then(|n| i32::try_from(n).ok())
    })?
    .ok_or("submit_combo requires `side` (1 buy / -1 sell)")?;
    let qty = typed_arg(tool, args, "qty", "a number", Value::as_f64)?
        .ok_or("submit_combo requires `qty`")?;
    let net_limit = typed_arg(tool, args, "net_limit", "a number", Value::as_f64)?;
    let legs =
        typed_arg(tool, args, "legs", "an array of {symbol, ratio, limit}", Value::as_array)?
            .ok_or("submit_combo requires `legs` (two or more {symbol, ratio, limit})")?
            .iter()
            .enumerate()
            .map(|(i, leg)| combo_leg(i, leg))
            .collect::<Result<Vec<_>, _>>()?;
    let order_client_id =
        typed_arg(tool, args, "order_client_id", "a string", |v| v.as_str().map(str::to_owned))?
            .filter(|id| !id.trim().is_empty());
    // `submit_order`'s reader; absent is `gtc`, which `combo_of` sends as the absent field.
    let lifetime =
        typed_arg(tool, args, "time_in_force", "text", |v| v.as_str().map(str::to_owned))?
            .map(|word| vike_model::TimeInForce::parse(&word))
            .transpose()
            .map_err(|e| format!("{tool}: `time_in_force` — {e}"))?
            .unwrap_or_default();
    let mut combo = vike_client::verbs::combo_of(
        named_arg(tool, args, "venue")?,
        account_arg(tool, args)?,
        side,
        qty,
        legs,
        net_limit,
        lifetime,
    )
    .map_err(|e| format!("{tool}: {e}"))?;
    combo.order_client_id = order_client_id;
    Ok(Verb::Combo(combo))
}

/// One of `submit_combo`'s `legs`: an object of `symbol`, `ratio` and `limit` and nothing else, each read as the
/// tool's own arguments are, the refusal naming the leg.
fn combo_leg(i: usize, leg: &Value) -> Result<vike_client::verbs::ComboLegArg, String> {
    let at = format!("submit_combo: `legs` item {i}");
    let Some(fields) = leg.as_object() else {
        return Err(format!(
            "{at}: a leg must be an object {{symbol, ratio, limit}} - got {}",
            crate::trace::redact(leg)
        ));
    };
    if let Some(k) = fields.keys().find(|k| !["symbol", "ratio", "limit"].contains(&k.as_str())) {
        return Err(format!("{at}: `{k}` is not a field of a leg (symbol, ratio, limit)"));
    }
    let ratio = typed_arg(&at, leg, "ratio", "an integer", |v| {
        v.as_i64().and_then(|n| i32::try_from(n).ok())
    })?
    .ok_or_else(|| format!("{at} requires `ratio`"))?;
    Ok(vike_client::verbs::ComboLegArg {
        symbol: named_arg(&at, leg, "symbol")?,
        ratio,
        limit: typed_arg(&at, leg, "limit", "a number", Value::as_f64)?,
    })
}

/// `set_setting`'s `value`, as the TEXT the wire carries.
///
/// A JSON number or boolean is accepted and RENDERED, because an agent writing `250` rather than
/// `"250"` has expressed the right intent and the node parses the text as TOML anyway — refusing it
/// would be a schema quibble with a round trip attached. Anything structured (an object, an array,
/// null) is refused: there is no honest one-line rendering of it, and `serde_json`'s would not be
/// TOML. (The REPL has no equivalent: its value is already a rest-of-line string, taken verbatim
/// precisely because quotes are what tell the node's TOML parse a type.)
fn setting_value(args: &Value) -> Result<String, String> {
    match args.get("value") {
        Some(Value::String(v)) => Ok(v.clone()),
        Some(v @ (Value::Number(_) | Value::Bool(_))) => Ok(v.to_string()),
        Some(_) => Err("set_setting's `value` must be text (a JSON number or boolean is accepted \
                        and rendered); the node parses it as a TOML value and validates the row \
                        with its own loader"
            .to_string()),
        None => Err("set_setting requires `value`".to_string()),
    }
}

/// [`verb_from_tool_args`] resolved through [`Verb::to_wire_command`] — the MCP write tools' one
/// entry: JSON args in, the SAME wire command the REPL would build out.
pub(crate) fn wire_command_for(name: &str, args: &Value) -> Result<CommandWire, String> {
    let verb = verb_from_tool_args(name, args)?;
    verb.to_wire_command().ok_or_else(|| format!("no wire command for {name}"))
}

/// The OPTIONAL operator/agent RATIONALE an MCP write tool may carry (`"reason"`), normalized:
/// trimmed, and blank ⇒ `None`. Deliberately SEPARATE from [`verb_from_tool_args`] — the rationale
/// rides BESIDE the command on the wire (`Request::Command { cmd, reason }`), never inside it, so a
/// `reason` argument can never alter the [`CommandWire`] that gets built. The node sanitizes it
/// again server-side before recording it (`vike_tradehub::audit::sanitize_reason` is the authority);
/// this is only the client-side normalization. PURE.
pub(crate) fn reason_from_tool_args(args: &Value) -> Option<String> {
    args.get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[path = "tool_verbs_tests.rs"]
#[cfg(test)]
mod tool_verbs_tests;
