//! The MCP write tools' DIALECT of the shared verb vocabulary: JSON arguments in, a
//! [`vike_client::verbs::Verb`] out. The vocabulary, the one [`CommandWire`] construction site, the
//! order client id mint and the guardrail are `vike_client::verbs`' and every surface shares them; what lives
//! here is the part only a tool call has, the reading of its arguments.
//!
//! Everything here is PURE: no network and no environment.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use vike_client::refusals::{RenamedArgumentRefusal, SubmitRuleRefusal, SubmitRuleRefusalId};
use vike_client::verbs::{
    Verb, modify_of, peg_of_word, release_price_from_word, release_venue_from_word,
    stop_adjust_from_terms, submit_rules_refusal, unsizeable_submit_refusal,
};
use vike_model::{OrderAttributes, OrderStopAdjust, OrderType, TradingState};
use vike_tradehub_client::wire::{CommandWire, OrderPegWire, OrderReleaseWire, OrderRequestWire};

use crate::tool_args::{
    CancelOrderArgs, CloseAllArgs, ComboLegArgs, FlattenArgs, MassCancelArgs, ModifyOrderArgs,
    MountStrategyArgs, SetSettingArgs, SetTradingStateArgs, SubmitComboArgs, SubmitOrderArgs,
    SubmitOrderListArgs, UnmountStrategyArgs, input_schema, parse_args,
};

/// Build the WRITE [`Verb`] an MCP tool names from its JSON arguments. PURE — no network — so the
/// preview path is fully testable. Missing/invalid required fields are a clean error. Tool names
/// are the MCP write roster (`mcp`'s `WRITE_TOOLS`): the seven order verbs, then the three
/// node-lifecycle ones.
///
/// `submit_order` also refuses an order that cannot be sized — a `qty` of zero or below, a
/// non-finite one, a non-finite `price`, a `limit` with no `price`, or a `market` with a `price` or
/// a `trigger_price` — through [`unsizeable_submit_refusal`], the same function the REPL and the
/// one-shot verb call, so the three never differ about what a malformed order is. Its TERMS are
/// then judged by `vike_client::verbs::submit_rules_refusal`, the one rule the CLI judges a built submit by
/// too, each the rule the node's door applies again: a `trail` on a kind that does not trail, the prices
/// (a stop, stop-limit or take-profit with no `trigger_price` above zero, a trailing order with no `trail`
/// above zero or with a `price`, a pegged order with no `peg` or with a price, a `peg` on any other kind),
/// the display, the attributes, a stop's adjustment, the lifetime and the release, in that order. A price
/// condition's venue (`release_when_price_on`) is read by `vike_client::verbs::release_venue_from_word`.
/// A `modify_order` that changes nothing is `vike_client::verbs::modify_of`'s refusal, the CLI's too.
///
/// Each lifecycle refusal below is an AUTHORING mistake rather than a policy one, and is spelled
/// as the wire variant's own doc spells the rule:
///
///   * `mount_strategy` requires its `strategy`, the strategy FILE under the node's `strategies/` folder
///     (the profile `[strategy]` vocabulary's one source; the wire still calls it `file`), refused here
///     rather than left for the daemon's edge to phrase. The retired `name` and `rhai` are no arguments
///     any more, ignored like any argument a tool does not name; the RENAMED `file` and `controller_id`
///     are refused by name ([`RENAMED_ARGUMENTS`]). The REPL, and the one-shot `vike-cli trade strategy
///     mount` that MIRRORS its grammar rather than inventing a second one (task 8 of the
///     trade-CLI-plane), refuse the same rule in that shared flag spelling (`--strategy`) — which is the
///     one thing about these three that is genuinely per-VOCABULARY rather than per-tool;
///   * `params` must be a JSON OBJECT when it is present. The wire carries it opaquely as a
///     `serde_json::Value` (the delegate-don't-mirror idiom — each strategy reads its own knobs),
///     which means nothing between here and the strategy would object to an ARRAY or a bare number
///     until the mount itself failed on the node;
///   * `set_setting`'s `value` is TEXT by declaration — see [`setting_value`].
///
/// ⚠ **A present value of the wrong type is REFUSED, never read as absent** ([`write_args`], every
/// tool's arguments are read through it): an absent `venue` means every engine, so `{"venue": 5}` read
/// as absent turned a one-venue request into the widest command the node takes. A blank `venue` or
/// `symbol` on `close_all` / `mass_cancel` is refused for the same reason ([`scope_arg`]).
pub(crate) fn verb_from_tool_args(name: &str, args: &Value) -> Result<Verb, String> {
    match name {
        "submit_order" => {
            let a: SubmitOrderArgs = write_args(name, args)?;
            let side = side_arg(name, a.side)?;
            // The INPUT door (`OrderType::parse`, trimmed and case-insensitive): an unknown kind is
            // refused HERE, naming the accepted words, rather than sent for the node to refuse —
            // and a known one travels as its canonical word, so `"LIMIT"` goes out as `"limit"`.
            let order_type = match a.order_type {
                None => OrderType::Market,
                Some(word) => OrderType::parse(&word)
                    .map_err(|e| format!("submit_order: `order_type` — {e}"))?,
            };
            // The LIFETIME, by the node's own reader (`TimeInForce::parse`: any case, `at_open` or
            // `at-open`); absent is the default `gtc`, which goes out as the absent field (the frame from
            // before the argument).
            let lifetime = a
                .time_in_force
                .map(|word| vike_model::TimeInForce::parse(&word))
                .transpose()
                .map_err(|e| format!("submit_order: `time_in_force` — {e}"))?
                .unwrap_or_default();
            let gtd_expiry = a.gtd_expiry;
            let req = OrderRequestWire {
                order_client_id: a
                    .order_client_id
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or_default(),
                venue: named_arg(name, "venue", a.venue)?,
                symbol: named_arg(name, "symbol", a.symbol)?,
                side,
                qty: a.qty.ok_or("submit_order requires `qty`")?,
                order_type: order_type.as_str().to_owned(),
                price: a.price,
                trigger_price: a.trigger_price,
                reduce_only: a.reduce_only.unwrap_or(false),
                account: account_arg(name, a.account)?,
                time_in_force: (lifetime != vike_model::TimeInForce::Gtc)
                    .then(|| lifetime.as_word().to_owned()),
                gtd_expiry,
                display_quantity: a.display_qty,
                release: release_arg(a.release_at, a.release_when_price, a.release_when_price_on)?,
                trail: a.trail,
                limit_offset: a.limit_offset,
                peg: peg_arg(a.peg, a.peg_offset)?,
                attributes: attributes_arg(a.all_or_none, a.sweep_to_fill, a.discretionary_amount),
                trail_percent: a.trail_percent,
                adjust: adjust_arg(
                    a.adjust_at,
                    a.adjust_to,
                    a.adjust_stop,
                    a.adjust_limit,
                    a.adjust_trail,
                    a.adjust_trail_percent,
                )?,
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
            // A PINNED id outside the charset every venue accepts, the CLI's refusal (D1 of 2026-11-24).
            if let Some(refusal) = (!req.order_client_id.is_empty())
                .then(|| vike_client::verbs::order_client_id_refusal(&req.order_client_id))
                .flatten()
            {
                return Err(format!("submit_order: {}", refusal.rendered()));
            }
            // The TERMS, by the one rule the CLI judges a built submit by too, each the rule the node's door
            // applies again: a `trail` off a trailing kind, the prices for the kind (a trigger kind with no
            // trigger above zero, which only this grammar can build), the display, the attributes, a stop's
            // adjustment, the lifetime and the release. Refused before a preview token is minted.
            if let Some(refusal) = submit_rules_refusal(&req) {
                return Err(submit_rule_line(&refusal));
            }
            Ok(Verb::Submit(req))
        }
        "submit_order_list" => order_list_verb(write_args(name, args)?),
        "submit_combo" => combo_verb(args),
        "cancel_order" => {
            let a: CancelOrderArgs = write_args(name, args)?;
            Ok(Verb::Cancel(named_arg(name, "order_client_id", a.order_client_id)?))
        }
        "modify_order" => {
            let a: ModifyOrderArgs = write_args(name, args)?;
            let order_client_id = named_arg(name, "order_client_id", a.order_client_id)?;
            // The one modify builder the CLI calls too: a modify that changes nothing is refused.
            modify_of(order_client_id, a.new_qty, a.new_price, a.new_trigger_price)
                .map_err(|refusal| format!("modify_order: {}", refusal.rendered()))
        }
        // The wire's account-carrying rules are argued in `crates/vike-cli/src/cmd/trade/oneshot.rs`'s module doc.
        // An ABSENT `account` stays `None` (the node then refuses a venue that mounts two accounts of
        // it, `crate::venue_gate`); a venue-less `close_all` / `mass_cancel` that names
        // an account is refused by the NODE's own edge (`account_refusal`), not here.
        "flatten" => {
            let a: FlattenArgs = write_args(name, args)?;
            Ok(Verb::Flatten {
                venue: named_arg(name, "venue", a.venue)?,
                symbol: named_arg(name, "symbol", a.symbol)?,
                account: account_arg(name, a.account)?,
            })
        }
        "close_all" => {
            let a: CloseAllArgs = write_args(name, args)?;
            Ok(Verb::MarketExit {
                venue: scope_arg(name, "venue", a.venue)?,
                account: account_arg(name, a.account)?,
            })
        }
        "set_trading_state" => {
            let a: SetTradingStateArgs = write_args(name, args)?;
            // The operator's word in any case (`TradingState::parse`, the one case rule).
            let word = a.state.ok_or("set_trading_state requires `state`")?;
            let Some(state) = TradingState::parse(&word) else {
                return Err(format!(
                    "unknown state {word:?} ({})",
                    TradingState::WORDS.join(" | ")
                ));
            };
            Ok(Verb::SetState(state))
        }
        "mass_cancel" => {
            let a: MassCancelArgs = write_args(name, args)?;
            Ok(Verb::MassCancel {
                venue: scope_arg(name, "venue", a.venue)?,
                symbol: scope_arg(name, "symbol", a.symbol)?,
                account: account_arg(name, a.account)?,
            })
        }
        "mount_strategy" => {
            let a: MountStrategyArgs = write_args(name, args)?;
            // The source first, as it always was; a blank one names no file.
            let file = a.strategy.filter(|file| !file.trim().is_empty()).ok_or(
                "mount_strategy requires `strategy`: the strategy file's path under the NODE's \
                 strategies/ folder (a .rs or .rhai file) — not script source, and not a path on \
                 this machine",
            )?;
            let account = account_arg(name, a.account)?;
            Ok(Verb::MountStrategy {
                venue: a.venue.ok_or("mount_strategy requires `venue`")?,
                account,
                symbol: a.symbol.ok_or("mount_strategy requires `symbol`")?,
                interval: a.interval.ok_or("mount_strategy requires `interval` (e.g. \"1m\")")?,
                controller_id: a.mount_id,
                file: Some(file),
                // Absent is an empty table; any other shape than an object `write_args` refused.
                params: a.params.map_or_else(|| json!({}), Value::Object),
            })
        }
        "unmount_strategy" => {
            let a: UnmountStrategyArgs = write_args(name, args)?;
            Ok(Verb::UnmountStrategy {
                controller_id: a.mount_id.ok_or("unmount_strategy requires `mount_id`")?,
            })
        }
        // No `file` and no `policy_confirm`: the key names its section, and the retype went with
        // `docs/decisions/0086` point 7. An agent that still passes either is ignored like any
        // argument a tool does not name — see [`Verb::SetSetting`] for what goes on the wire.
        "set_setting" => {
            let a: SetSettingArgs = write_args(name, args)?;
            Ok(Verb::SetSetting {
                key: a.key.ok_or("set_setting requires `key` (the full dotted key)")?,
                value: setting_value(a.value)?,
            })
        }
        other => Err(format!("no wire command for {other}")),
    }
}

/// A built submit's refusal (`vike_client::verbs::submit_rules_refusal`) as this tool shows it: `submit_order: error
/// [<id>]: <text>`, the text opening with the argument it judged where the rule is one argument's (this dialect's
/// half: the CLI names its flag).
fn submit_rule_line(refusal: &SubmitRuleRefusal) -> String {
    let argument = match refusal.id() {
        SubmitRuleRefusalId::Display => "`display_qty` — ",
        SubmitRuleRefusalId::Lifetime => "`time_in_force` / `gtd_expiry` — ",
        SubmitRuleRefusalId::Release => "`release_at` — ",
        _ => "",
    };
    let text = format!("{argument}{refusal}");
    format!("submit_order: {}", vike_utility::errors::render_refusal(refusal.id().word(), text))
}

/// The write tools' arguments renamed by the 2026-11-24 naming pass, `(tool, old, new)`. Each old name is
/// REFUSED naming the new one (`vike_client::refusals::RenamedArgumentRefusal`), never read as absent: an
/// explicit mount id read as absent mounts under the derived one, and no argument has two names.
const RENAMED_ARGUMENTS: [(&str, &str, &str); 3] = [
    ("mount_strategy", "file", "strategy"),
    ("mount_strategy", "controller_id", "mount_id"),
    ("unmount_strategy", "controller_id", "mount_id"),
];

/// A write tool's arguments read into its struct `T` ([`parse_args`]), the ONE reading of every
/// argument [`verb_from_tool_args`] takes: a missing or `null` key is `None`, and a value of the wrong
/// type is REFUSED naming the tool and the argument, so a refusal comes before a preview token can be
/// minted.
///
/// What it got is echoed through [`crate::trace::redact`], because a refusal lands in the agent
/// transcript's `detail`, which is not redacted: the echo must not be how a value leaves that promise.
///
/// An argument under its OLD name ([`RENAMED_ARGUMENTS`]) is refused first, naming the new one.
fn write_args<T: DeserializeOwned + JsonSchema>(tool: &str, args: &Value) -> Result<T, String> {
    let renamed =
        RENAMED_ARGUMENTS.into_iter().find(|&(t, old, _)| t == tool && args.get(old).is_some());
    if let Some((_, old, new)) = renamed {
        let refusal = RenamedArgumentRefusal::Renamed { old, new };
        return Err(vike_utility::errors::render_refusal(refusal.id().word(), &refusal));
    }
    parse_args(args, |key, got| match (tool, key) {
        ("mount_strategy", "params") => "mount_strategy's `params` must be an object (the \
                                         [strategy.params] table) — it is carried to the strategy \
                                         opaquely, so nothing below this would object to another \
                                         shape"
            .to_string(),
        (_, "account") => {
            format!("{}. Omit it to name no account", typed_refusal(tool, key, ACCOUNT_WANT, got))
        }
        _ => {
            let schema = input_schema::<T>();
            let declared = schema["properties"][key]["type"].as_str().unwrap_or_default();
            typed_refusal(tool, key, wanted(tool, key, declared), got)
        }
    })
}

/// `{tool}: `{key}` must be {want} - got {value}`, the value redacted ([`write_args`]).
fn typed_refusal(tool: &str, key: &str, want: &str, got: &Value) -> String {
    format!("{tool}: `{key}` must be {want} - got {}", crate::trace::redact(got))
}

/// What `side` must be: its type, and the only two values it takes.
const SIDE_WANT: &str = "an integer (1 buy / -1 sell)";

/// What `account` must be ([`account_arg`]).
const ACCOUNT_WANT: &str =
    "text - an account label such as LIVE, or \"DEFAULT\" for the venue's unlabelled account";

/// What a write tool's argument must be, in its refusal: the argument's own words where it has them,
/// else its declared type's.
fn wanted(tool: &str, key: &str, declared: &str) -> &'static str {
    match (tool, key, declared) {
        (_, "side", _) => SIDE_WANT,
        (_, "gtd_expiry" | "release_at", _) => "an integer (epoch milliseconds)",
        (_, "legs", _) => "an array of {symbol, ratio, limit}",
        ("submit_combo", "order_client_id", _) => "a string",
        (_, _, "number") => "a number",
        (_, _, "boolean") => "true or false",
        (_, _, "integer") => "an integer",
        _ => "text",
    }
}

/// `side`: 1 buy, -1 sell, and a number of any other value refused as a value of the wrong type is.
fn side_arg(tool: &str, side: Option<i64>) -> Result<i32, String> {
    match side {
        None => Err(format!("{tool} requires `side` (1 buy / -1 sell)")),
        Some(1) => Ok(1),
        Some(-1) => Ok(-1),
        Some(n) => Err(typed_refusal(tool, "side", SIDE_WANT, &json!(n))),
    }
}

/// A `venue` or `symbol` that narrows `close_all` / `mass_cancel`, and is omitted to mean all of them.
///
/// ⚠ A blank one is refused, because it names no scope and it does not mean "all" either. A blank
/// venue on a node that has not published yet passes both venue gates, and the core then gives it
/// engine 0 (`crates/vike-core/src/runtime/routing.rs`'s `exit_scope_engines`), whose whole book
/// goes. A blank symbol cancels only the orders that carry no symbol, and is answered `accepted`.
fn scope_arg(tool: &str, key: &str, value: Option<String>) -> Result<Option<String>, String> {
    match value {
        Some(v) if v.trim().is_empty() => Err(format!(
            "{tool}: `{key}` is blank, which names nothing: name one, or omit it to mean all of them"
        )),
        v => Ok(v),
    }
}

/// A `venue` or `symbol` an order must name, or the id a cancel or a modify must name. A blank one
/// names nothing, so it never reaches the node, where an empty symbol may resolve to the mounted one:
/// an order the agent did not name.
fn named_arg(tool: &str, key: &str, value: Option<String>) -> Result<String, String> {
    match value {
        None => Err(format!("{tool} requires `{key}`")),
        Some(v) if v.trim().is_empty() => {
            Err(format!("{tool}: `{key}` is blank, which names nothing"))
        }
        Some(v) => Ok(v),
    }
}

/// The OPTIONAL `account` argument of an MCP write tool, as the WIRE string the order/mount/reduce
/// variants carry (absent names none, `DEFAULT` names the unlabelled account deliberately, a label
/// names that account — [`vike_tradehub_client::wire::OrderRequestWire::account`] states the three).
///
/// Validated HERE so a typo costs a parse error rather than a round trip and a `Response::Error`,
/// and with the wire's own reader (`parse_wire_account`), so this edge adds a message rather than a
/// second grammar. A missing or `null` `account` is absent; a value that is present and is NOT text
/// (a number, a list, an object, a boolean) is REFUSED by [`write_args`], never read as absent: an
/// account-less command is not the safe direction here, because a `close_all` or `mass_cancel` with
/// no venue means EVERY engine, so `{"account": ["ALT"]}` must not quietly turn a one-account request
/// into the widest action the node can take (the node's `lower_command` refuses a typo the same way
/// instead of dropping it). The account rides INSIDE the [`CommandWire`], so the preview token
/// (bound to the whole command) is bound to it too: a token issued for one account cannot confirm
/// the same order on another.
fn account_arg(tool: &str, account: Option<String>) -> Result<Option<String>, String> {
    let Some(a) = account else {
        return Ok(None);
    };
    vike_model::accounts::account_keys::parse_wire_account(&a).map_err(|e| {
        format!(
            "{tool}: `account` — {e}. Omit it to name no account, or pass \"DEFAULT\" to name \
             the venue's unlabelled account deliberately"
        )
    })?;
    Ok(Some(a))
}

/// `submit_order`'s `release_at` (epoch ms) and `release_when_price` (`above:<price>` / `below:<price>`, the
/// CLI's `--when-price` word, read by the same `vike_client::verbs::release_price_from_word`) as the wire's
/// release, and `release_when_price_on`, the venue that condition's price is read on (the CLI's
/// `--when-price-on`, checked by the same `release_venue_from_word`). A price condition rides a moment, so
/// `release_when_price` alone is refused, and a venue rides a condition, so `release_when_price_on` alone is too.
fn release_arg(
    at: Option<i64>,
    when: Option<String>,
    on: Option<String>,
) -> Result<Option<OrderReleaseWire>, String> {
    let tool = "submit_order";
    let condition = when
        .as_deref()
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
            release_venue_from_word(&on, symbol.as_deref())
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
fn attributes_arg(
    all_or_none: Option<bool>,
    sweep_to_fill: Option<bool>,
    discretionary_amount: Option<f64>,
) -> Option<OrderAttributes> {
    let attributes = OrderAttributes {
        all_or_none: all_or_none.unwrap_or_default(),
        sweep_to_fill: sweep_to_fill.unwrap_or_default(),
        discretionary_amount,
    };
    (attributes != OrderAttributes::default()).then_some(attributes)
}

/// `submit_order`'s `adjust_at` and its target (`adjust_to`, `adjust_stop`, `adjust_limit`, `adjust_trail`,
/// `adjust_trail_percent`) as the wire's adjustment, through the reader the CLI shares
/// (`vike_client::verbs::stop_adjust_from_terms`). A target's term with no `adjust_at` is refused, never
/// dropped; whether the numbers hold is `vike_model::adjust_refusal`'s.
fn adjust_arg(
    at: Option<f64>,
    to: Option<String>,
    stop: Option<f64>,
    limit: Option<f64>,
    trail: Option<f64>,
    trail_percent: Option<f64>,
) -> Result<Option<OrderStopAdjust>, String> {
    let tool = "submit_order";
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
    stop_adjust_from_terms(at, &to, stop, limit, trail, trail_percent)
        .map(Some)
        .map_err(|e| format!("{tool}: {e}"))
}

/// `submit_order`'s `peg` (the anchor word, `best`, `mid`, `market` or `primary` in any case, read by the CLI's
/// `--peg` reader, `vike_client::verbs::peg_of_word`) and `peg_offset` (default 0) as the wire's peg. A `peg_offset`
/// with no `peg` is refused; whether the kind takes a peg is `vike_client::verbs::submit_rules_refusal`'s.
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
    peg_of_word(&anchor, offset.unwrap_or(0.0))
        .map(Some)
        .map_err(|refusal| format!("{tool}: {}", refusal.rendered()))
}

/// `submit_order_list`: the `shape`'s arguments onto the ONE builder each CLI spelling uses too (`vike_client::verbs`'
/// `oco_list` / `oto_list` / `ladder_list`), so the tool and `trade order oco|oto|ladder` build the same legs. Every
/// argument is read (and a wrong type refused) first; then one the `shape` does not take is REFUSED, never ignored.
/// (`shape`, not `kind`: `delete_series` names its selector `kind`, and the write tools share one argument union.)
fn order_list_verb(a: SubmitOrderListArgs) -> Result<Verb, String> {
    let tool = "submit_order_list";
    let side = side_arg(tool, a.side)?;
    let head = vike_client::verbs::OrderListHead {
        venue: named_arg(tool, "venue", a.venue)?,
        account: account_arg(tool, a.account)?,
        symbol: named_arg(tool, "symbol", a.symbol)?,
        side,
        qty: a.qty.ok_or("submit_order_list requires `qty`")?,
    };
    let (entry, limit, stop, stop_limit, trail) =
        (a.entry_price, a.limit_price, a.stop_price, a.stop_limit_price, a.trail);
    let reduce_only = a.reduce_only;
    // A ladder's stop and targets in the bracket's words, `stop_loss` and `take_profit`.
    let (stop_loss, take_profit) = (a.stop_loss, a.take_profit);
    // The caller's own leg ids, as `submit_order`'s `order_client_id`: a blank one reads as omitted (minted).
    let order_client_ids: Vec<String> = a
        .order_client_ids
        .filter(|ids| !ids.trim().is_empty())
        .map(|ids| ids.split(',').map(|id| id.trim().to_string()).collect())
        .unwrap_or_default();
    let shape = a.shape.ok_or("submit_order_list requires `shape` (oco | oto | ladder)")?;
    let takes: &[&str] = match shape.as_str() {
        "oco" => &["limit_price", "stop_price", "stop_limit_price", "trail", "reduce_only"],
        "oto" => &["entry_price", "limit_price", "stop_price", "trail"],
        "ladder" => &["entry_price", "stop_loss", "trail", "take_profit"],
        other => {
            return Err(format!("submit_order_list: `shape` is oco | oto | ladder, got {other:?}"));
        }
    };
    let optional = [
        ("entry_price", entry.is_some()),
        ("limit_price", limit.is_some()),
        ("stop_price", stop.is_some()),
        ("stop_limit_price", stop_limit.is_some()),
        ("trail", trail.is_some()),
        ("reduce_only", reduce_only.is_some()),
        ("stop_loss", stop_loss.is_some()),
        ("take_profit", take_profit.is_some()),
    ];
    if let Some((k, _)) = optional.iter().find(|(k, given)| *given && !takes.contains(k)) {
        return Err(format!("submit_order_list: `{k}` is not an argument of shape {shape}"));
    }
    let need = |v: Option<f64>, k: &str| {
        v.ok_or_else(|| format!("submit_order_list: a {shape} list requires `{k}`"))
    };
    // Which protective leg the call names, and that it names exactly one, is the shared builder's to refuse
    // (`vike_client::refusals::OrderListRefusal`), as for `trade order oco|oto|ladder`.
    let list = match shape.as_str() {
        "oco" => vike_client::verbs::oco_list(
            head,
            need(limit, "limit_price")?,
            stop,
            stop_limit,
            trail,
            reduce_only.unwrap_or(false),
        ),
        "oto" => vike_client::verbs::oto_list(head, entry, stop, limit, trail),
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
            vike_client::verbs::ladder_list(head, entry, stop_loss, trail, &targets)
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
    // No argument of the struct, so looked for in the call itself.
    if let Some(k) = ["price", "limit", "limit_price"]
        .iter()
        .find(|k| args.get(**k).is_some_and(|v| !v.is_null()))
    {
        return Err(format!(
            "{tool}: `{k}` is not an argument: a combo is priced by `net_limit` or by each leg's `limit`"
        ));
    }
    let a: SubmitComboArgs = write_args(tool, args)?;
    let side = side_arg(tool, a.side)?;
    let qty = a.qty.ok_or("submit_combo requires `qty`")?;
    let legs = a
        .legs
        .ok_or("submit_combo requires `legs` (two or more {symbol, ratio, limit})")?
        .iter()
        .enumerate()
        .map(|(i, leg)| combo_leg(i, leg))
        .collect::<Result<Vec<_>, _>>()?;
    let order_client_id = a.order_client_id.filter(|id| !id.trim().is_empty());
    if let Some(refusal) =
        order_client_id.as_deref().and_then(vike_client::verbs::order_client_id_refusal)
    {
        return Err(format!("{tool}: {}", refusal.rendered()));
    }
    // `submit_order`'s reader; absent is `gtc`, which `combo_of` sends as the absent field.
    let lifetime = a
        .time_in_force
        .map(|word| vike_model::TimeInForce::parse(&word))
        .transpose()
        .map_err(|e| format!("{tool}: `time_in_force` — {e}"))?
        .unwrap_or_default();
    let mut combo = vike_client::verbs::combo_of(
        named_arg(tool, "venue", a.venue)?,
        account_arg(tool, a.account)?,
        side,
        qty,
        legs,
        a.net_limit,
        lifetime,
    )
    .map_err(|e| format!("{tool}: {e}"))?;
    combo.order_client_id = order_client_id;
    Ok(Verb::Combo(combo))
}

/// One of `submit_combo`'s `legs`: an object of `symbol`, `ratio` and `limit` and nothing else, read into
/// [`ComboLegArgs`] as the tool's own arguments are, the refusal naming the leg.
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
    let leg: ComboLegArgs = write_args(&at, leg)?;
    let ratio = leg.ratio.ok_or_else(|| format!("{at} requires `ratio`"))?;
    Ok(vike_client::verbs::ComboLegArg {
        symbol: named_arg(&at, "symbol", leg.symbol)?,
        ratio,
        limit: leg.limit,
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
fn setting_value(value: Option<Value>) -> Result<String, String> {
    match value {
        Some(Value::String(v)) => Ok(v),
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

#[path = "tool_verbs_tests.rs"]
#[cfg(test)]
mod tool_verbs_tests;
