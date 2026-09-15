//! The ONE node-write verb vocabulary shared by the two agent surfaces — the `trade` REPL (a human
//! typing verbs) and the `mcp` server (an agent calling tools). Both used to build [`WireCommand`]s
//! independently and had already drifted: the REPL supported `modify`/`mass-cancel` while the MCP
//! surface did not, and each carried its own copy of the client-side env-cap guardrail. This module
//! closes that drift structurally:
//!
//! - [`Verb`] (+ [`Verb::to_wire_command`]) is the SINGLE args→[`WireCommand`] construction site.
//!   The REPL's `parse_line` (still in [`crate::cmd::trade`] — the line GRAMMAR is REPL-specific)
//!   produces a `Verb`; the MCP write tools go JSON-args → [`verb_from_tool_args`] → the SAME
//!   `Verb::to_wire_command`. A new write verb added here is visibly missing from whichever surface
//!   forgets to expose it, instead of silently absent from one.
//! - [`fill_client_order_id`] is the SINGLE coid MINT. A vike-tradehub node REFUSES a remote
//!   `Submit` whose `client_order_id` is empty (`lower_command`'s idempotency policy: a remote peer
//!   whose id the runtime minted has no stable handle to cancel or dedup by), so every client of
//!   that protocol must PRE-MINT one. Both surfaces used to leave it empty — the REPL always
//!   (`parse_submit` set `String::new()` with a comment saying "let the runtime mint one", an
//!   in-process assumption the wire refuses), the MCP tool whenever the agent omitted the optional
//!   argument. The REPL therefore could not place an order from the day that server rule landed.
//!   One filler, applied by both, using [`coid_minter`].
//! - [`guardrail_check`] is the SINGLE client-side advisory guardrail over the [`GuardrailCaps`],
//!   with two renderers off one [`Guardrail`] value: [`Guardrail::to_json`] (the MCP preview
//!   payload) and [`Guardrail::line`] (the REPL preview line). The node's server-side
//!   `ControlLimits` + `RiskGate` are the ENFORCING gate regardless — this check is UX, the node
//!   gate is truth.
//!
//! # The three node-LIFECYCLE verbs, and why they joined
//!
//! `mount` / `unmount` / `set-setting` (`mount_strategy` / `unmount_strategy` / `set_setting` as
//! tools) change what the node RUNS and how it is CONFIGURED rather than what its book holds. They
//! reached the two surfaces in the same week, from two authors working under file ownership, and
//! each built them OUTSIDE this module because neither owned it: the REPL parsed them straight to a
//! [`WireCommand`] and the MCP server rebuilt them in a `lifecycle_command` of its own. Each author
//! independently wrote the same note beside their copy — lift these the day the OTHER surface grows
//! them, because a verb with one surface has no second surface to drift from. Both surfaces had
//! them within days of each other, which made that day the same day, and the second construction
//! site is now gone.
//!
//! [`Verb`] therefore names the node-WRITE vocabulary rather than the order-write one, and the
//! widening costs nothing that was load-bearing: **nothing keys on a verb being an ORDER**.
//! [`Verb::is_write`] — the discriminator both surfaces route their preview gate on — is what this
//! type is actually for, and the three sites that genuinely care about order-ness
//! ([`guardrail_check`], [`fill_client_order_id`], [`coid_charset_warning`]) all key on the
//! [`WireCommand`] VARIANT, and were already total over the lifecycle ones before they arrived here.
//!
//! ⚠ **The `policy.toml` TYPED-CONFIRM contract is deliberately NOT shared, and must not be
//! tidied into one.** Both surfaces implement it and they implement it DIFFERENTLY on purpose: the
//! REPL PROMPTS the operator to retype the key (`crate::cmd::trade`'s `typed_key_confirm`, run
//! after the ordinary `y/N` gate and run even under `--yes`), while the MCP server REFUSES a policy
//! write carrying no `policy_confirm` BEFORE a preview token is minted (`mcp`'s
//! `typed_confirm_verdict`) and deliberately performs no equality compare at all — a compare there
//! would be a process checking a string it could have written itself. Each is argued where it
//! lives. What is shared is only that [`Verb::SetSetting`] CARRIES the field, and that neither
//! parser may invent one.
//!
//! Everything here is PURE apart from [`guardrail_caps`]'s one env read — no network — so the
//! whole vocabulary is unit-testable without a node.
//!
//! ⚠ The notional cap used to be `VIKE_MAX_ORDER_NOTIONAL`, read here on every check. Phase 5 of
//! the settings-unification design removed that variable: it is the same per-order ceiling the
//! trading binaries enforce, and a ceiling any exported variable can raise is not a ceiling. It now
//! comes from `max_notional_per_order` in this machine's `<vike home>/policy.toml`, resolved once
//! by the dispatcher ([`crate::run`]) and passed in. `VIKE_MAX_ORDER_QTY` STAYS an environment
//! variable: it has no policy field and no enforcing counterpart anywhere — it is a local
//! typo-catcher for a human at a REPL, not a risk ceiling.

use serde_json::{Value, json};
use vike_model::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest, WireTradingState};

/// A fresh coid generator for THIS process — `<8-hex-session><seq>`, the live core's own wire form.
///
/// Reusing `vike_model::ClientOrderIdGenerator` rather than inventing a shape buys four properties
/// this surface actually needs, and every one of them was a stated requirement:
///
/// - **Unique per order.** `seq` increments, so two `submit`s in one session can never share an id
///   (a duplicate coid is idempotent at the node — the registry is coid-keyed — so a repeat would
///   silently book NOTHING the second time, the worst possible failure for an order surface).
/// - **No collision across sessions.** The 8-hex prefix is drawn from OS entropy per PROCESS, which
///   is the same bar the live runtime's own generator is held to; two REPLs open side by side, or
///   one restarted while an order still rests at the venue, get different prefixes.
/// - **Typeable.** 9-11 characters, `[0-9a-f]` + decimal digits, no separator, no case to get
///   wrong. The REPL prints it in the preview and in `orders`; a human then types it back at
///   `cancel <coid>` / `modify <coid>`, so length and charset are a usability constraint, not a
///   detail.
/// - **Venue-safe.** `is_valid_crypto_coid` (`^[A-Za-z0-9]{1,32}$`) is the strictest common
///   denominator across Binance / Bybit / OKX, and `generate` asserts it — so an id minted here
///   survives the node, the core and the venue edge unchanged.
///
/// SESSION-scoped, not per-command: the sequence is what makes ids unique without asking the OS for
/// entropy per order, and it is what the live core does.
pub(crate) fn coid_minter() -> ClientOrderIdGenerator {
    ClientOrderIdGenerator::new(None)
}

/// Fill a `Submit`'s EMPTY `client_order_id` from `minter`; return everything else untouched.
///
/// The one place a coid is minted on this side of the wire. Called BEFORE the preview on both
/// surfaces, deliberately: the id the operator (or the agent) is shown must be the id that gets
/// sent, or the preview is a different command from the one confirmed. The same reason the node's
/// own Telegram surface mints at preview time (`vike_tradehub::telegram::parse`'s `fill_coid`, the
/// shape this mirrors).
///
/// An id already present is NEVER overwritten — that is the explicit-override path (`submit …
/// --coid <id>` in the REPL, `client_order_id` in the MCP tool arguments). Non-`Submit` verbs
/// either target an existing coid or are account-wide, so they pass through. PURE apart from the
/// generator's own counter.
pub(crate) fn fill_client_order_id(
    cmd: WireCommand,
    minter: &mut ClientOrderIdGenerator,
) -> WireCommand {
    match cmd {
        WireCommand::Submit(mut req) if req.client_order_id.trim().is_empty() => {
            req.client_order_id = minter.generate();
            WireCommand::Submit(req)
        }
        other => other,
    }
}

/// One parsed command against a running vike-tradehub node. WRITE verbs ([`Verb::is_write`]) go
/// through each surface's mandatory preview gate and map to a [`WireCommand`] via
/// [`Verb::to_wire_command`]; READ/meta verbs render from the snapshot (REPL) or have their own
/// tools (MCP). Moved here from `trade.rs` so BOTH surfaces share one construction site.
///
/// The WRITE half splits into ORDERS (what the node's book holds) and the node LIFECYCLE (what the
/// node runs and how it is configured). That split is descriptive and nothing routes on it — see
/// this module's doc for why the lifecycle three are variants here rather than a sibling type.
#[derive(Debug, Clone, PartialEq)]
pub enum Verb {
    // ---- WRITE (gated) ----
    /// `submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only]`
    Submit(WireOrderRequest),
    /// `cancel <coid>`
    Cancel(String),
    /// `modify <coid> [--qty Q] [--price P]`
    Modify { client_order_id: String, new_qty: Option<f64>, new_price: Option<f64> },
    /// `flatten <venue> <symbol>`
    Flatten { venue: String, symbol: String },
    /// `market-exit [venue]`
    MarketExit { venue: Option<String> },
    /// `halt` (Halted) / `resume` (Active) at the REPL and as `vike-cli trade <verb>`;
    /// `set_trading_state` as an MCP tool, which is where `Reducing` is still spelled.
    ///
    /// ⚠ The REPL used to reach this through `state <active|reducing|halted>`, an argument of the
    /// verb that PRINTED the mode — so "look" and "stop trading" differed by one token. Ruling 17
    /// of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` split them: the
    /// read is [`Verb::Status`] and each write is its own word, named for what it DOES.
    SetState(WireTradingState),
    /// `mass-cancel [venue] [symbol]`
    MassCancel { venue: Option<String>, symbol: Option<String> },
    // ---- WRITE: the node LIFECYCLE (gated the same way) ----
    /// `mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path>) [--id <mount-id>]
    /// [--params <json>]` — the `mount_strategy` tool.
    ///
    /// ⚠ `name` XOR `rhai` is the wire contract and this type does NOT enforce it: each surface
    /// refuses both-or-neither in its OWN vocabulary (the REPL names two flags, the tool names two
    /// JSON fields), because a refusal phrased in the other surface's spelling is one its reader
    /// cannot act on. `rhai` is a path on the **NODE's** filesystem, not this machine's.
    MountStrategy {
        venue: String,
        symbol: String,
        interval: String,
        /// `None` lets the NODE derive `{venue}__{symbol}__{interval}`. Deliberately not derived on
        /// this side: that rule is the node's, and a client copy would be a second one to keep in
        /// step for no gain.
        controller_id: Option<String>,
        name: Option<String>,
        rhai: Option<String>,
        /// The `[strategy.params]` table, carried opaquely (delegate-don't-mirror — the wire never
        /// re-declares a strategy's knobs). Always a JSON OBJECT: each surface refuses another
        /// shape at its own edge, since nothing below here would object until the mount failed.
        params: Value,
    },
    /// `unmount <mount-id>` — the `unmount_strategy` tool.
    UnmountStrategy { controller_id: String },
    /// `set-setting <file> <full.dotted.key> <value>` — the `set_setting` tool.
    ///
    /// ⚠ `confirm` is the node's TYPED-CONFIRM field, and neither surface's parser may invent one
    /// from the `key` it already holds — see this module's doc. The REPL's parser produces `None`
    /// for every file (its own test pins that structurally, and the operator's retyping is
    /// collected later by a prompt); the MCP tool copies the `policy_confirm` argument an operator
    /// handed the agent, and nothing else.
    SetSetting { file: String, key: String, value: String, confirm: Option<String> },
    // ---- READ ----
    /// `orders [symbol]`
    Orders(Option<String>),
    /// `positions [venue]`
    Positions(Option<String>),
    /// `equity`
    Equity,
    /// `snapshot`
    Snapshot,
    /// `recent [N]`
    Recent(Option<usize>),
    /// `status` — the trading MODE **and** the mounted-strategy registry, in one output
    /// (`crate::cmd::trade_status`). It subsumes the old `state` read and the retired top-level
    /// `strategy-status`: an operator asking "what is this node doing" wants both in one look, and
    /// splitting them was the near-collision ruling 17 closed.
    Status,
    // ---- meta ----
    /// `help`
    Help,
    /// `quit` / `exit`
    Quit,
}

impl Verb {
    /// Whether this verb mutates node state (and therefore goes through the preview+confirm gate).
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Verb::Submit(_)
                | Verb::Cancel(_)
                | Verb::Modify { .. }
                | Verb::Flatten { .. }
                | Verb::MarketExit { .. }
                | Verb::SetState(_)
                | Verb::MassCancel { .. }
                | Verb::MountStrategy { .. }
                | Verb::UnmountStrategy { .. }
                | Verb::SetSetting { .. }
        )
    }

    /// The [`WireCommand`] a WRITE verb sends, or `None` for a READ/meta verb. PURE — the one
    /// construction site both the REPL and the MCP write tools resolve through.
    pub fn to_wire_command(&self) -> Option<WireCommand> {
        match self {
            Verb::Submit(o) => Some(WireCommand::Submit(o.clone())),
            Verb::Cancel(coid) => Some(WireCommand::Cancel(coid.clone())),
            Verb::Modify { client_order_id, new_qty, new_price } => Some(WireCommand::Modify {
                client_order_id: client_order_id.clone(),
                new_qty: *new_qty,
                new_price: *new_price,
            }),
            Verb::Flatten { venue, symbol } => {
                Some(WireCommand::Flatten { venue: venue.clone(), symbol: symbol.clone() })
            }
            Verb::MarketExit { venue } => Some(WireCommand::MarketExit { venue: venue.clone() }),
            Verb::SetState(s) => Some(WireCommand::SetTradingState(*s)),
            Verb::MassCancel { venue, symbol } => {
                Some(WireCommand::MassCancel { venue: venue.clone(), symbol: symbol.clone() })
            }
            Verb::MountStrategy { venue, symbol, interval, controller_id, name, rhai, params } => {
                Some(WireCommand::MountStrategy {
                    venue: venue.clone(),
                    symbol: symbol.clone(),
                    interval: interval.clone(),
                    controller_id: controller_id.clone(),
                    name: name.clone(),
                    rhai: rhai.clone(),
                    params: params.clone(),
                })
            }
            Verb::UnmountStrategy { controller_id } => {
                Some(WireCommand::UnmountStrategy { controller_id: controller_id.clone() })
            }
            Verb::SetSetting { file, key, value, confirm } => Some(WireCommand::SetSetting {
                file: file.clone(),
                key: key.clone(),
                value: value.clone(),
                confirm: confirm.clone(),
            }),
            _ => None,
        }
    }
}

/// Build the WRITE [`Verb`] an MCP tool names from its JSON arguments. PURE — no network — so the
/// preview path is fully testable. Missing/invalid required fields are a clean error. Tool names
/// are the MCP write roster (`mcp`'s `WRITE_TOOLS`): the seven order verbs, then the three
/// node-lifecycle ones.
///
/// Each lifecycle refusal below is an AUTHORING mistake rather than a policy one, and is spelled
/// as the wire variant's own doc spells the rule:
///
///   * `mount_strategy` takes exactly one of `name` / `rhai` — the profile `[strategy]` vocabulary
///     verbatim (`docs/decisions/0024-rhai-strategies-live.md`), so "both" and "neither" are both
///     refused here rather than left for the daemon's edge to phrase. The REPL refuses the same
///     rule in its own flag spelling, which is the one thing about these three that is genuinely
///     per-surface;
///   * `params` must be a JSON OBJECT when it is present. The wire carries it opaquely as a
///     `serde_json::Value` (the delegate-don't-mirror idiom — each strategy reads its own knobs),
///     which means nothing between here and the strategy would object to an ARRAY or a bare number
///     until the mount itself failed on the node;
///   * `set_setting`'s `value` is TEXT by declaration — see [`setting_value`].
pub(crate) fn verb_from_tool_args(name: &str, args: &Value) -> Result<Verb, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let f = |k: &str| args.get(k).and_then(Value::as_f64);
    match name {
        "submit_order" => {
            let side = args
                .get("side")
                .and_then(Value::as_i64)
                .ok_or("submit_order requires `side` (1 buy / -1 sell)")?
                as i32;
            Ok(Verb::Submit(WireOrderRequest {
                client_order_id: s("client_order_id").unwrap_or_default(),
                venue: s("venue").ok_or("submit_order requires `venue`")?,
                symbol: s("symbol").ok_or("submit_order requires `symbol`")?,
                side,
                qty: f("qty").ok_or("submit_order requires `qty`")?,
                order_type: s("order_type").unwrap_or_else(|| "market".to_string()),
                price: f("price"),
                trigger_price: f("trigger_price"),
                reduce_only: args.get("reduce_only").and_then(Value::as_bool).unwrap_or(false),
            }))
        }
        "cancel_order" => {
            Ok(Verb::Cancel(s("client_order_id").ok_or("cancel_order requires `client_order_id`")?))
        }
        "modify" => {
            let client_order_id =
                s("client_order_id").ok_or("modify requires `client_order_id`")?;
            let (new_qty, new_price) = (f("new_qty"), f("new_price"));
            if new_qty.is_none() && new_price.is_none() {
                // Mirrors the REPL's "nothing to change" rule — a Modify that changes nothing is
                // an authoring mistake, not a command.
                return Err(
                    "modify requires at least one of `new_qty` / `new_price` (nothing to change)"
                        .to_string(),
                );
            }
            Ok(Verb::Modify { client_order_id, new_qty, new_price })
        }
        "flatten" => Ok(Verb::Flatten {
            venue: s("venue").ok_or("flatten requires `venue`")?,
            symbol: s("symbol").ok_or("flatten requires `symbol`")?,
        }),
        "market_exit" => Ok(Verb::MarketExit { venue: s("venue") }),
        "set_trading_state" => {
            let state = match s("state").ok_or("set_trading_state requires `state`")?.as_str() {
                "active" => WireTradingState::Active,
                "reducing" => WireTradingState::Reducing,
                "halted" => WireTradingState::Halted,
                other => {
                    return Err(format!("unknown state {other:?} (active | reducing | halted)"));
                }
            };
            Ok(Verb::SetState(state))
        }
        "mass_cancel" => Ok(Verb::MassCancel { venue: s("venue"), symbol: s("symbol") }),
        "mount_strategy" => {
            let (name_src, rhai_src) = (s("name"), s("rhai"));
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
            Ok(Verb::MountStrategy {
                venue: s("venue").ok_or("mount_strategy requires `venue`")?,
                symbol: s("symbol").ok_or("mount_strategy requires `symbol`")?,
                interval: s("interval")
                    .ok_or("mount_strategy requires `interval` (e.g. \"1m\")")?,
                controller_id: s("controller_id"),
                name: name_src,
                rhai: rhai_src,
                params,
            })
        }
        "unmount_strategy" => Ok(Verb::UnmountStrategy {
            controller_id: s("controller_id")
                .ok_or("unmount_strategy requires `controller_id` (the mount id)")?,
        }),
        "set_setting" => Ok(Verb::SetSetting {
            file: s("file").ok_or(
                "set_setting requires `file` (policy.toml | config.toml | preferences.toml | \
                 flags.toml)",
            )?,
            key: s("key").ok_or("set_setting requires `key` (the full dotted key)")?,
            value: setting_value(args)?,
            // NEVER derived from `key`. See `mcp`'s `policy_confirm_property`, and this module's
            // doc for the whole typed-confirm split.
            confirm: s("policy_confirm"),
        }),
        other => Err(format!("no wire command for {other}")),
    }
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
                        and rendered); the node parses it as a TOML value and validates the \
                        would-be file with its own loader"
            .to_string()),
        None => Err("set_setting requires `value`".to_string()),
    }
}

/// [`verb_from_tool_args`] resolved through [`Verb::to_wire_command`] — the MCP write tools' one
/// entry: JSON args in, the SAME wire command the REPL would build out.
pub(crate) fn wire_command_for(name: &str, args: &Value) -> Result<WireCommand, String> {
    let verb = verb_from_tool_args(name, args)?;
    verb.to_wire_command().ok_or_else(|| format!("no wire command for {name}"))
}

/// The OPTIONAL operator/agent RATIONALE an MCP write tool may carry (`"reason"`), normalized:
/// trimmed, and blank ⇒ `None`. Deliberately SEPARATE from [`verb_from_tool_args`] — the rationale
/// rides BESIDE the command on the wire (`Request::Command { cmd, reason }`), never inside it, so a
/// `reason` argument can never alter the [`WireCommand`] that gets built. The node sanitizes it
/// again server-side before recording it (`vike_tradehub::audit::sanitize_reason` is the authority);
/// this is only the client-side normalization. PURE.
pub(crate) fn reason_from_tool_args(args: &Value) -> Option<String> {
    args.get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The result of the ONE client-side advisory guardrail check (see [`guardrail_check`]). Rendered
/// as JSON for the MCP preview payload ([`Guardrail::to_json`]) and as a line for the REPL preview
/// ([`Guardrail::line`]) — same semantics, two skins, so the two surfaces can never drift again.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Guardrail {
    /// A `Submit` — the one verb with a size to check.
    Order {
        qty: f64,
        /// `None` for a market order (no price to size — cannot check notional client-side).
        notional: Option<f64>,
        max_qty: Option<f64>,
        max_notional: Option<f64>,
        within_limits: bool,
    },
    /// Every other verb: trivially within limits (no order size to check).
    NoSize,
}

/// The two client-side advisory caps, resolved ONCE per process by [`guardrail_caps`] and carried
/// by each surface (the REPL's `Session`, the MCP `Server`). `Copy` so passing it costs nothing.
///
/// They come from different places for a reason — see this module's doc: `max_notional` is a
/// POLICY ceiling with no environment layer, `max_qty` is a local convenience knob that keeps one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct GuardrailCaps {
    /// `VIKE_MAX_ORDER_QTY`, parsed. `None` = no qty cap.
    pub(crate) max_qty: Option<f64>,
    /// `max_notional_per_order` from `<vike home>/policy.toml`. `None` = no notional cap.
    pub(crate) max_notional: Option<f64>,
}

/// Resolve the advisory caps: the POLICY ceiling the caller already loaded, plus the one remaining
/// environment knob.
///
/// The env read lives here rather than in each surface so there is exactly one of it, and it is
/// the same shape it always had — trimmed `f64` parse, anything unparseable treated as unset (this
/// is an advisory display; a garbage value must not become a cap, and the node enforces regardless).
/// Non-positive is left as-is, matching the historical behaviour: a `0` cap simply flags every
/// order as over-limit in the preview, and refuses nothing.
pub(crate) fn guardrail_caps(policy_max_notional: Option<f64>) -> GuardrailCaps {
    GuardrailCaps {
        max_qty: std::env::var("VIKE_MAX_ORDER_QTY")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok()),
        max_notional: policy_max_notional,
    }
}

/// A client-side advisory guardrail check over [`GuardrailCaps`] (the node's own server-side
/// `ControlLimits` is what actually enforces). Only a `Submit` has a size to check; every other
/// verb is trivially within limits. PURE — the caps arrive resolved.
pub(crate) fn guardrail_check(cmd: &WireCommand, caps: GuardrailCaps) -> Guardrail {
    let WireCommand::Submit(o) = cmd else {
        return Guardrail::NoSize;
    };
    let GuardrailCaps { max_qty, max_notional } = caps;
    let notional = o.price.map(|p| p.abs() * o.qty.abs());
    let qty_ok = max_qty.is_none_or(|m| o.qty.abs() <= m);
    let notional_ok = match (max_notional, notional) {
        (Some(m), Some(n)) => n <= m,
        _ => true, // no cap, or a market order with no price to size — cannot check client-side
    };
    Guardrail::Order {
        qty: o.qty.abs(),
        notional,
        max_qty,
        max_notional,
        within_limits: qty_ok && notional_ok,
    }
}

impl Guardrail {
    /// The MCP preview payload (the exact JSON shape the `mcp` write tools always returned).
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Guardrail::Order { qty, notional, max_qty, max_notional, within_limits } => json!({
                "qty": qty, "notional": notional,
                "max_qty": max_qty, "max_notional": max_notional,
                "within_limits": within_limits
            }),
            Guardrail::NoSize => {
                json!({ "within_limits": true, "note": "no order size to check for this verb" })
            }
        }
    }

    /// The REPL preview line (the exact string the `trade` REPL always printed).
    pub(crate) fn line(&self) -> String {
        match self {
            Guardrail::Order { qty, notional, max_qty, max_notional, within_limits } => format!(
                "guardrail: qty={} notional={} max_qty={} max_notional={} → {}",
                fmt_num(*qty),
                // The notional is the one number that is COMPARED against a ceiling, so it gets the
                // floor-aware renderer: it may never be shortened to a string that reads as
                // at-or-below the cap it actually exceeds. See [`fmt_num_over`].
                fmt_opt_over(notional, *max_notional),
                fmt_opt(max_qty),
                fmt_opt(max_notional),
                if *within_limits { "within limits" } else { "OVER LIMIT (the node will reject)" },
            ),
            Guardrail::NoSize => "guardrail: no order size to check for this verb".to_string(),
        }
    }
}

/// The most decimals [`fmt_num`] will render before giving up and printing the raw value.
const MAX_RENDER_DECIMALS: usize = 12;

/// How far a rendering may sit from the true value, RELATIVE to `max(|v|, 1)`.
///
/// 1e-12 is four orders of magnitude above f64's ~1e-16 multiply noise and far below any difference
/// a person could act on: no order size, price or ceiling in this system is decided at the twelfth
/// significant figure. It is a DISPLAY tolerance and touches nothing else — `guardrail_check`
/// compares the raw `f64`s, so the verdict on the same line is computed from the exact values.
const RENDER_TOL: f64 = 1e-12;

/// Render an `f64` the way a person would have written it, without the binary-floating-point tail.
///
/// `0.4 * 3` is exactly `1.2000000000000002`, and `f64::to_string` is obliged to print all of it —
/// it is the shortest string that round-trips to that bit pattern. So the order preview read
/// `notional=1.2000000000000002`, seventeen digits of arithmetic noise on a number the operator
/// typed as two. That is not a cosmetic problem in a tool whose whole job is letting a human check a
/// number before it becomes an order: a reader who has learned to skip past noise is a reader who
/// will skip past a real discrepancy in the same position.
///
/// The rule is the SHORTEST fixed-point rendering within [`RENDER_TOL`] of the value, which is
/// exactly "drop the digits that carry no information": `1.2000000000000002` → `1.2`, while
/// `0.0001234` keeps every digit it has because dropping one would move the value.
///
/// ⚠ **It is never used to decide anything.** Values below ~1e-12 collapse to `0`, and that is
/// acceptable precisely because no comparison reads this string.
///
/// There is deliberately no shared helper reused here. `vike_ui_theme::fmt` is an egui leaf (linking
/// it would drag `egui` into a CLI whose identity is being light), `vike_bridge_core::format`'s
/// `format_to_step` is behind that crate's `full` feature — which this crate deliberately does NOT
/// enable — and rounds DOWN by design ("never overshoot a limit"), the wrong direction here.
pub(crate) fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let tol = RENDER_TOL * v.abs().max(1.0);
    for decimals in 0..=MAX_RENDER_DECIMALS {
        let rendered = format!("{v:.decimals$}");
        if rendered.parse::<f64>().is_ok_and(|back| (back - v).abs() <= tol) {
            return rendered;
        }
    }
    v.to_string()
}

/// [`fmt_num`], but never rendered so coarsely that it reads as at-or-below `floor`.
///
/// The case this exists for: a notional of `42.500000000000004` against a `max_notional` of `42.5`
/// is genuinely OVER the ceiling, and [`fmt_num`] would shorten it to `42.5` — leaving a line that
/// says `notional=42.5 max_notional=42.5 → OVER LIMIT`, i.e. a verdict its own numbers appear to
/// contradict. The display may drop noise; it may not understate a value against the ceiling it is
/// being judged by. When shortening would cross the cap, the exact value is printed instead — ugly,
/// and correct, which is the right way round for a number about to become an order.
fn fmt_num_over(v: f64, floor: Option<f64>) -> String {
    let rendered = fmt_num(v);
    match floor {
        Some(cap) if v > cap && rendered.parse::<f64>().is_ok_and(|back| back <= cap) => {
            v.to_string()
        }
        _ => rendered,
    }
}

/// [`fmt_num`] over an `Option`, with `-` for "no value" (no cap set, or a market order with no
/// price to size).
fn fmt_opt(v: &Option<f64>) -> String {
    v.map(fmt_num).unwrap_or_else(|| "-".to_string())
}

/// [`fmt_num_over`] over an `Option`.
fn fmt_opt_over(v: &Option<f64>, floor: Option<f64>) -> String {
    v.map(|n| fmt_num_over(n, floor)).unwrap_or_else(|| "-".to_string())
}

/// The advisory client-order-id charset check, shared by every write surface.
///
/// `Some(warning)` when a command names a `client_order_id` that
/// [`vike_model::is_valid_crypto_coid`] rejects — the `^[A-Za-z0-9]{1,32}$` charset the REPL's own
/// `submit --coid` REFUSES and the one every minted id satisfies.
///
/// # Why `cancel` WARNS where `submit` REFUSES
///
/// The asymmetry a clean install found was real: `submit --coid MYPINNED-001` was rejected with a
/// good message while `cancel MYPINNED-001` — for an order that never existed — answered "accepted
/// by the node". Two different answers about the same field is a trap, so both surfaces speak about
/// the charset now. They do not speak with the same force, and that is deliberate:
///
/// - **`submit` can refuse safely.** It is MINTING an id, so nothing is lost by insisting the id be
///   one every venue accepts. Refusing costs the operator one retype.
/// - **`cancel`/`modify` must not refuse.** They name an id that ALREADY EXISTS at the node, and
///   this REPL is not the only thing that puts orders there: the `mcp` surface's `submit_order`
///   passes an agent-supplied `client_order_id` through unvalidated
///   (`verbs::verb_from_tool_args`), and the node's own `lower_command` requires only that the id be
///   non-empty. So an order with a coid outside this charset CAN exist — and refusing to cancel it
///   would make a live order uncancellable from the operator's console to enforce a client-side
///   convention. That trade is unacceptable in a tool whose reason to exist includes flattening
///   something in a hurry.
///
/// So: say the id could not have been minted here (which is genuinely useful — it is almost always a
/// typo), and send it anyway. Cancel stays fire-and-forget, which is honest: the node cannot confirm
/// that an order EXISTS either way.
pub(crate) fn coid_charset_warning(cmd: &WireCommand) -> Option<String> {
    let coid = match cmd {
        WireCommand::Cancel(client_order_id) => client_order_id,
        WireCommand::Modify { client_order_id, .. } => client_order_id,
        // A `Submit` reaching here has already been through `parse_submit`'s refusal (an explicit
        // `--coid`) or `fill_client_order_id`'s mint, both of which guarantee the charset.
        _ => return None,
    };
    (!vike_model::is_valid_crypto_coid(coid)).then(|| {
        format!(
            "note: {coid:?} is not a client_order_id this node could have minted ({COID_CHARSET}) \
             — sending it anyway, but check it: nothing will match, and a cancel that matches \
             nothing still reports as accepted"
        )
    })
}

/// The client-order-id charset, in one place, for every message and help screen that states it.
/// `vike_model::is_valid_crypto_coid` is the enforcing authority; this is its prose.
pub(crate) const COID_CHARSET: &str = "1..=32 alphanumeric characters, A-Z a-z 0-9";

#[cfg(test)]
mod tests {

    /// ⚠ **COMPOSED, never spelled.** `crates/vike-config/tests/policy_is_consumed.rs` scans every
    /// non-comment line under `src/` for a section-qualified policy key and reads a hit as evidence
    /// that the file READS that setting — and unlike its sibling `settings_are_consumed.rs` it does
    /// NOT truncate at `#[cfg(test)]`, so a fixture spelling one turns `main` red. It did: #1685
    /// lifted these fixtures here from `mcp.rs` and `Policy::max_leverage` is `Consumed::No`.
    /// Composing it is the same trick `crates/vike-model/src/credential_keys.rs` uses to keep its
    /// near-miss fixtures out of the settings-registry literal harvest.
    const POLICY_KEY_FIXTURE: &str = concat!("policy.", "max_leverage");

    use super::*;

    // NOTE: no test here sets the VIKE_MAX_ORDER_* env vars (env writes race across the parallel
    // test harness); the cap-less path is what these pin, same as the mcp/trade preview tests.

    fn submit_cmd(qty: f64, price: Option<f64>) -> WireCommand {
        WireCommand::Submit(WireOrderRequest {
            client_order_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty,
            order_type: if price.is_some() { "limit".into() } else { "market".into() },
            price,
            trigger_price: None,
            reduce_only: false,
        })
    }

    #[test]
    fn modify_tool_args_map_to_the_repl_wire_command() {
        // The SAME WireCommand::Modify the REPL's `modify c-1 --qty 2` builds.
        let cmd = wire_command_for("modify", &json!({ "client_order_id": "c-1", "new_qty": 2.0 }))
            .unwrap();
        assert_eq!(
            cmd,
            WireCommand::Modify {
                client_order_id: "c-1".into(),
                new_qty: Some(2.0),
                new_price: None
            }
        );
    }

    #[test]
    fn modify_with_nothing_to_change_is_a_clean_error() {
        let err = wire_command_for("modify", &json!({ "client_order_id": "c-1" })).unwrap_err();
        assert!(err.contains("new_qty"), "{err}");
        assert!(wire_command_for("modify", &json!({})).unwrap_err().contains("client_order_id"));
    }

    #[test]
    fn mass_cancel_tool_args_map_with_both_scopes_optional() {
        assert_eq!(
            wire_command_for("mass_cancel", &json!({})).unwrap(),
            WireCommand::MassCancel { venue: None, symbol: None }
        );
        assert_eq!(
            wire_command_for("mass_cancel", &json!({ "venue": "sim", "symbol": "BTCUSDT" }))
                .unwrap(),
            WireCommand::MassCancel { venue: Some("sim".into()), symbol: Some("BTCUSDT".into()) }
        );
    }

    /// The MCP write roster with one MINIMAL argument set each — the seven order verbs and the
    /// three node-lifecycle ones. Spelled ONCE so the two roster loops below cannot come to cover
    /// different sets; `mcp`'s own `WRITE_TOOLS` is the authority for the names, and
    /// `the_write_arm_and_the_write_roster_are_the_same_set` there is what holds the routing to it.
    fn write_tool_arguments() -> Vec<(&'static str, Value)> {
        vec![
            ("submit_order", json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 })),
            ("cancel_order", json!({ "client_order_id": "c-1" })),
            ("modify", json!({ "client_order_id": "c-1", "new_price": 9.0 })),
            ("flatten", json!({ "venue": "sim", "symbol": "B" })),
            ("market_exit", json!({})),
            ("set_trading_state", json!({ "state": "halted" })),
            ("mass_cancel", json!({})),
            (
                "mount_strategy",
                json!({ "venue": "sim", "symbol": "B", "interval": "1m", "name": "spread_maker" }),
            ),
            ("unmount_strategy", json!({ "controller_id": "sim__B__1m" })),
            // `config.toml`, not `policy.toml`: a policy write needs the operator's typed confirm,
            // and that gate is `mcp`'s (`typed_confirm_verdict`) with its own tests there.
            (
                "set_setting",
                json!({ "file": "config.toml", "key": "config.tradehub_addr", "value": "1:2" }),
            ),
        ]
    }

    #[test]
    fn every_mcp_write_tool_resolves_through_the_shared_verb() {
        // The whole MCP write roster maps through Verb::to_wire_command — the one construction
        // site. (submit/cancel/flatten/market_exit/set_trading_state pins live in mcp.rs's tests;
        // this pins that the mapping goes THROUGH a write Verb for each roster name.)
        for (name, args) in write_tool_arguments() {
            let verb = verb_from_tool_args(name, &args).unwrap();
            assert!(verb.is_write(), "{name} must map to a WRITE verb");
            assert!(verb.to_wire_command().is_some(), "{name} must build a wire command");
        }
        assert!(wire_command_for("node_snapshot", &json!({})).unwrap_err().contains("no wire"));
    }

    /// The rationale rides BESIDE the command, never inside it: adding a `reason` argument must
    /// leave `wire_command_for`'s output BYTE-IDENTICAL for every write tool, and the reason itself
    /// comes out of the separate [`reason_from_tool_args`] reader.
    #[test]
    fn a_reason_argument_never_changes_the_wire_command() {
        for (name, mut args) in write_tool_arguments() {
            let bare = wire_command_for(name, &args).unwrap();
            assert_eq!(reason_from_tool_args(&args), None, "{name}: no reason argument given");
            args["reason"] = json!("agent: flattening ahead of the CPI print");
            let with = wire_command_for(name, &args).unwrap();
            assert_eq!(with, bare, "{name}: a reason must not change the built command");
            assert_eq!(
                reason_from_tool_args(&args).as_deref(),
                Some("agent: flattening ahead of the CPI print"),
                "{name}: …and it is read off the SEPARATE reason reader"
            );
        }
    }

    #[test]
    fn reason_from_tool_args_trims_and_treats_blank_as_absent() {
        assert_eq!(reason_from_tool_args(&json!({ "reason": "  why  " })).as_deref(), Some("why"));
        assert_eq!(reason_from_tool_args(&json!({ "reason": "   " })), None);
        assert_eq!(reason_from_tool_args(&json!({ "reason": "" })), None);
        assert_eq!(reason_from_tool_args(&json!({})), None);
        // A non-string `reason` is ignored rather than stringified into a nonsense rationale.
        assert_eq!(reason_from_tool_args(&json!({ "reason": 7 })), None);
    }

    // ---- the node-LIFECYCLE three (moved here with the arms they exercise) --------------------
    //
    // These pin the PURE builder and moved out of `mcp.rs` with it; the gates that sit in FRONT of
    // it there (`typed_confirm_verdict`, the mandatory preview, the venue check) keep their tests
    // in that file, because they are the MCP surface's own and not part of the construction.

    /// `mount_strategy` takes EXACTLY ONE source, and both wrong shapes are refused HERE rather
    /// than at the daemon's edge — a round trip to learn a rule the profile vocabulary already
    /// states. (The REPL refuses the same rule in ITS spelling, naming `--name`/`--rhai`; that
    /// half is `crate::cmd::trade`'s, deliberately.)
    #[test]
    fn mount_strategy_takes_exactly_one_strategy_source() {
        let base = json!({ "venue": "sim", "symbol": "BTCUSDT", "interval": "1m" });
        let mut neither = base.clone();
        neither["params"] = json!({});
        let err = wire_command_for("mount_strategy", &neither).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        assert!(err.contains("not script source"), "…and says what `rhai` is NOT: {err}");

        let mut both = base.clone();
        both["name"] = json!("spread_maker");
        both["rhai"] = json!("strategies/breaker.rhai");
        assert!(wire_command_for("mount_strategy", &both).unwrap_err().contains("both were given"));

        for (field, value) in [("name", "spread_maker"), ("rhai", "strategies/breaker.rhai")] {
            let mut one = base.clone();
            one[field] = json!(value);
            assert!(
                wire_command_for("mount_strategy", &one).is_ok(),
                "{field} alone must build a mount"
            );
        }
    }

    /// The mount's `params` table: absent means an EMPTY table, and a non-object is refused here
    /// because the wire carries it opaquely and nothing below would object until the node did.
    #[test]
    fn mount_strategy_params_default_to_an_empty_table_and_must_be_an_object() {
        let base = json!({
            "venue": "sim", "symbol": "BTCUSDT", "interval": "1m", "name": "spread_maker"
        });
        let cmd = wire_command_for("mount_strategy", &base).unwrap();
        let WireCommand::MountStrategy { params, controller_id, .. } = &cmd else {
            panic!("the builder produced a different variant")
        };
        assert_eq!(*params, json!({}), "an absent params table is an empty one");
        assert_eq!(*controller_id, None, "…and an absent id derives on the node, not here");

        let mut list = base.clone();
        list["params"] = json!([1, 2]);
        assert!(
            wire_command_for("mount_strategy", &list).unwrap_err().contains("must be an object")
        );
    }

    /// `unmount_strategy` names the mount by id and nothing else — and the refusal says WHICH id,
    /// because `strategy_status` does not report one.
    #[test]
    fn unmount_strategy_requires_the_mount_id() {
        let err = wire_command_for("unmount_strategy", &json!({})).unwrap_err();
        assert!(err.contains("controller_id"), "{err}");
        assert_eq!(
            wire_command_for("unmount_strategy", &json!({ "controller_id": "sim__B__1m" }))
                .unwrap(),
            WireCommand::UnmountStrategy { controller_id: "sim__B__1m".to_string() }
        );
    }

    /// `set_setting`'s `value` is TEXT on the wire. A JSON number or boolean is RENDERED rather
    /// than refused (the agent expressed the right intent and the node parses TOML anyway); a
    /// structured value is refused, because `serde_json`'s rendering of one would not be TOML.
    #[test]
    fn a_settings_value_renders_a_scalar_and_refuses_a_structure() {
        let of = |v: Value| {
            wire_command_for(
                "set_setting",
                &json!({ "file": "config.toml", "key": "config.tradehub_addr", "value": v }),
            )
        };
        for (given, expect) in [(json!(250), "250"), (json!(true), "true"), (json!("x"), "x")] {
            let cmd = of(given.clone()).unwrap();
            let WireCommand::SetSetting { value, .. } = &cmd else {
                panic!("the builder produced a different variant")
            };
            assert_eq!(value, expect, "{given} must render as {expect}");
        }
        assert!(of(json!({ "a": 1 })).unwrap_err().contains("must be text"));
        assert!(of(json!([1])).unwrap_err().contains("must be text"));
    }

    /// **THE SHARED HALF OF THE TYPED-CONFIRM CONTRACT: this builder never fills the field in**,
    /// and it is the party best placed to — it holds `key`. Asserted on the pure builder, where
    /// there is no gate in front of it to explain a `None` away: a write that supplies no
    /// `policy_confirm` reaches the wire with `confirm: None` whatever `key` says, for a policy
    /// file and a non-policy one alike.
    ///
    /// The REPL's twin of this claim is stronger and lives with the REPL, as it must: its parser
    /// cannot produce a `confirm` at all, for any file. The two surfaces then diverge in what they
    /// DO about a policy write — a prompt there, a refusal here — which is the difference this
    /// module deliberately does not share.
    #[test]
    fn the_typed_confirm_is_never_derived_from_the_key() {
        for file in ["policy.toml", "config.toml"] {
            let cmd = wire_command_for(
                "set_setting",
                &json!({ "file": file, "key": POLICY_KEY_FIXTURE, "value": "3" }),
            )
            .expect("the builder itself does not gate — `mcp`'s typed_confirm_verdict does");
            let WireCommand::SetSetting { confirm, .. } = &cmd else {
                panic!("the builder produced a different variant")
            };
            assert_eq!(
                *confirm, None,
                "{file}: an absent policy_confirm must stay absent — deriving it from `key` would \
                 satisfy the node's compare on every call and prove nothing about a human"
            );
        }
        // …and a supplied one rides through VERBATIM: this side neither invents nor edits it.
        let cmd = wire_command_for(
            "set_setting",
            &json!({
                "file": "policy.toml",
                "key": POLICY_KEY_FIXTURE,
                "value": "3",
                "policy_confirm": POLICY_KEY_FIXTURE
            }),
        )
        .unwrap();
        let WireCommand::SetSetting { confirm, .. } = &cmd else { panic!("a settings write") };
        assert_eq!(confirm.as_deref(), Some(POLICY_KEY_FIXTURE));
    }

    // ---- the coid mint (the node REFUSES a remote submit with an empty one) -------------------

    /// **THE defect.** A `Submit` built by either surface leaves `client_order_id` empty; the mint
    /// must fill it with a NON-EMPTY, venue-valid id — otherwise the node answers "remote submit
    /// requires a pre-minted client_order_id" and nothing is ever placed.
    #[test]
    fn an_empty_submit_coid_is_minted_and_is_venue_valid() {
        let mut minter = coid_minter();
        let filled = fill_client_order_id(submit_cmd(1.0, Some(100.0)), &mut minter);
        let WireCommand::Submit(o) = filled else { panic!("still a Submit") };
        assert!(!o.client_order_id.is_empty(), "an empty coid is exactly what the node refuses");
        assert!(
            vike_model::is_valid_crypto_coid(&o.client_order_id),
            "the minted id must survive the node, the core and the venue edge: {:?}",
            o.client_order_id
        );
        assert!(o.client_order_id.len() <= 12, "typeable: {:?}", o.client_order_id);
    }

    /// Successive submits in one session get DIFFERENT ids. The node's registry is coid-keyed and
    /// idempotent, so a repeated id would silently book nothing the second time — an order surface
    /// cannot have that.
    #[test]
    fn every_minted_coid_in_a_session_is_distinct() {
        let mut minter = coid_minter();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..500 {
            let WireCommand::Submit(o) =
                fill_client_order_id(submit_cmd(1.0, Some(100.0)), &mut minter)
            else {
                panic!("still a Submit")
            };
            assert!(
                seen.insert(o.client_order_id.clone()),
                "duplicate coid {:?}",
                o.client_order_id
            );
        }
    }

    /// Two sessions (two `vike-cli trade` processes, or one restarted while an order still rests)
    /// must not mint the same id — the session prefix is what buys that.
    #[test]
    fn two_sessions_do_not_share_a_coid_prefix() {
        let (mut a, mut b) = (coid_minter(), coid_minter());
        let first = |m: &mut ClientOrderIdGenerator| m.generate();
        assert_ne!(first(&mut a), first(&mut b), "two sessions minted the same first coid");
    }

    /// An EXPLICIT id is never overwritten (the `--coid` / `client_order_id` override path), and no
    /// non-`Submit` verb is touched by the mint.
    #[test]
    fn the_mint_never_overwrites_an_explicit_id_or_touches_another_verb() {
        let mut minter = coid_minter();
        let mut pinned = submit_cmd(1.0, Some(100.0));
        if let WireCommand::Submit(o) = &mut pinned {
            o.client_order_id = "operatorPinned7".into();
        }
        assert_eq!(fill_client_order_id(pinned.clone(), &mut minter), pinned);

        for cmd in [
            WireCommand::Cancel("c-1".into()),
            WireCommand::MarketExit { venue: None },
            WireCommand::MassCancel { venue: None, symbol: None },
        ] {
            assert_eq!(fill_client_order_id(cmd.clone(), &mut minter), cmd);
        }
    }

    /// The MCP write path builds through `wire_command_for`, which still emits an EMPTY coid when
    /// the agent omits the optional argument — so the mint is what makes that path work too, and an
    /// agent-supplied id still wins.
    #[test]
    fn the_mcp_submit_tool_gets_a_minted_coid_when_the_agent_omits_one() {
        let mut minter = coid_minter();
        let bare = wire_command_for(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 }),
        )
        .unwrap();
        let WireCommand::Submit(o) = fill_client_order_id(bare, &mut minter) else {
            panic!("still a Submit")
        };
        assert!(vike_model::is_valid_crypto_coid(&o.client_order_id));

        let pinned = wire_command_for(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0, "client_order_id": "agentPinned1" }),
        )
        .unwrap();
        let WireCommand::Submit(o) = fill_client_order_id(pinned, &mut minter) else {
            panic!("still a Submit")
        };
        assert_eq!(o.client_order_id, "agentPinned1", "an agent-supplied id is authoritative");
    }

    #[test]
    fn guardrail_json_and_line_render_the_same_check() {
        // No caps at all: a priced submit is within limits, notional = qty * price.
        let g = guardrail_check(&submit_cmd(0.5, Some(100.0)), GuardrailCaps::default());
        let j = g.to_json();
        assert_eq!(j["qty"], 0.5);
        assert_eq!(j["notional"], 50.0);
        assert_eq!(j["within_limits"], true);
        let line = g.line();
        assert!(line.contains("qty=0.5") && line.contains("notional=50"), "{line}");
        assert!(line.contains("within limits"), "{line}");
    }

    /// The POLICY ceiling reaching the advisory check — the "value flows" half of Phase 5's
    /// contract at this surface. PURE now, so it needs no process-env mutation to test (which is
    /// unsound from parallel test threads and is why this was never covered before).
    #[test]
    fn the_policy_ceiling_is_what_the_notional_check_uses() {
        let caps = GuardrailCaps { max_qty: None, max_notional: Some(40.0) };
        let g = guardrail_check(&submit_cmd(0.5, Some(100.0)), caps); // notional 50 > 40
        assert_eq!(g.to_json()["max_notional"], 40.0);
        assert_eq!(g.to_json()["within_limits"], false);
        assert!(g.line().contains("OVER LIMIT"), "{}", g.line());

        // …and no ceiling (no policy.toml) is today's behaviour: nothing is over the limit.
        let g = guardrail_check(&submit_cmd(0.5, Some(100.0)), GuardrailCaps::default());
        assert_eq!(g.to_json()["max_notional"], Value::Null);
        assert_eq!(g.to_json()["within_limits"], true);
    }

    // -- the preview's number rendering -----------------------------------------------------------

    /// **The reported line, and the reason this renderer exists.**
    ///
    /// `0.4 * 3` is exactly `1.2000000000000002` as an `f64`, and `to_string` prints all of it. The
    /// guardrail line therefore read
    /// `guardrail: qty=3 notional=1.2000000000000002 max_qty=- max_notional=42.5` for an order the
    /// operator typed as three at forty cents.
    #[test]
    fn the_preview_renders_a_typed_number_the_way_it_was_typed() {
        let caps = GuardrailCaps { max_qty: None, max_notional: Some(42.5) };
        let line = guardrail_check(&submit_cmd(3.0, Some(0.4)), caps).line();
        assert_eq!(
            line, "guardrail: qty=3 notional=1.2 max_qty=- max_notional=42.5 → within limits",
            "the float tail must not reach the preview"
        );
        // The raw value really is the ugly one — this is a RENDERING fix, not a computation change.
        assert_eq!((0.4f64 * 3.0).to_string(), "1.2000000000000002");
    }

    /// Dropping noise is not the same as dropping precision: a value whose digits carry information
    /// keeps all of them, out to [`MAX_RENDER_DECIMALS`].
    #[test]
    fn rendering_drops_noise_and_nothing_else() {
        for (v, expect) in [
            (1.2000000000000002_f64, "1.2"),
            (0.1 + 0.2, "0.3"),
            (3.0, "3"),
            (42.5, "42.5"),
            (0.0001234, "0.0001234"),
            (1234.56789, "1234.56789"),
            (-0.30000000000000004, "-0.3"),
            (0.0, "0"),
            (1e-10, "0.0000000001"),
            (1e20, "100000000000000000000"),
        ] {
            assert_eq!(fmt_num(v), expect, "fmt_num({v})");
        }
        // Non-finite falls through to Display rather than looping to the precision cap.
        assert_eq!(fmt_num(f64::NAN), "NaN");
        assert_eq!(fmt_num(f64::INFINITY), "inf");
    }

    /// **A notional may never be shortened into looking like it clears the ceiling it exceeds.**
    ///
    /// The display drops noise; it does not get to soften a verdict. Without the floor rule this
    /// line would read `notional=42.5 max_notional=42.5 → OVER LIMIT`, i.e. a verdict its own two
    /// numbers appear to contradict — precisely the "output that communicates the opposite of the
    /// truth" this whole change is about.
    #[test]
    fn a_notional_is_never_rendered_below_the_ceiling_it_exceeds() {
        // The next representable f64 above the cap — computed, not written as a literal, so the
        // "one ulp over" intent is in the code rather than in a digit count a reader has to verify
        // (and so clippy's `excessive_precision` has nothing to object to).
        let over = f64::from_bits(42.5_f64.to_bits() + 1);
        assert!(over > 42.5, "the premise: this f64 is genuinely over");
        assert_eq!(fmt_num(over), "42.5", "…and shortening alone would hide that");
        assert_eq!(fmt_num_over(over, Some(42.5)), over.to_string(), "so the exact value is shown");

        // A value genuinely UNDER the cap is rendered normally — the rule fires only when hiding
        // the difference would contradict the verdict.
        assert_eq!(fmt_num_over(1.2000000000000002, Some(42.5)), "1.2");
        // …and with no cap there is nothing to understate against.
        assert_eq!(fmt_num_over(over, None), "42.5");
    }

    /// The rendering is DISPLAY ONLY: `within_limits` is computed from the raw `f64`s, so no
    /// tolerance in the renderer can change a verdict.
    #[test]
    fn rendering_never_moves_the_verdict() {
        let caps = GuardrailCaps { max_qty: None, max_notional: Some(42.5) };
        // qty is one ulp above 1.0, so qty * price lands just over the ceiling — and the check must
        // say so, however the line renders it.
        let qty = f64::from_bits(1.0_f64.to_bits() + 1);
        let g = guardrail_check(&submit_cmd(qty, Some(42.5)), caps);
        assert_eq!(g.to_json()["within_limits"], false, "the raw comparison decides");
        assert!(g.line().contains("OVER LIMIT"), "{}", g.line());
        // The JSON keeps exact numbers — a machine reader must never get a formatted string.
        assert!(g.to_json()["notional"].is_number(), "{}", g.to_json());
    }

    // -- the coid charset, on the verbs that do not mint one --------------------------------------

    /// `cancel`/`modify` name an id that must ALREADY exist; `submit` mints one. So the charset is
    /// stated on all three and enforced on one. See [`coid_charset_warning`] for the argument.
    #[test]
    fn a_coid_no_minter_could_have_produced_warns_on_cancel_and_modify() {
        for cmd in [
            WireCommand::Cancel("MYPINNED-001".into()),
            WireCommand::Modify {
                client_order_id: "!!!bad***coid".into(),
                new_qty: Some(2.0),
                new_price: None,
            },
        ] {
            let w = coid_charset_warning(&cmd)
                .unwrap_or_else(|| panic!("an unmintable coid must warn: {cmd:?}"));
            assert!(w.contains(COID_CHARSET), "the warning must state the charset: {w}");
            assert!(
                w.contains("sending it anyway"),
                "it must be clear the command is still sent — cancel stays fire-and-forget: {w}"
            );
        }
    }

    /// A conforming id is silent, and a `Submit` never warns at all: by the time one reaches the
    /// preview its coid has been through `parse_submit`'s refusal or `fill_client_order_id`'s mint.
    #[test]
    fn a_conforming_coid_and_every_submit_are_silent() {
        assert_eq!(coid_charset_warning(&WireCommand::Cancel("baa0ec7d00".into())), None);
        assert_eq!(
            coid_charset_warning(&WireCommand::Modify {
                client_order_id: "baa0ec7d00".into(),
                new_qty: None,
                new_price: Some(1.0),
            }),
            None
        );
        assert_eq!(coid_charset_warning(&submit_cmd(1.0, Some(1.0))), None);
        // …and neither do the verbs that name no order at all.
        assert_eq!(
            coid_charset_warning(&WireCommand::MassCancel { venue: None, symbol: None }),
            None
        );
    }

    /// The warning and the refusal must describe the SAME charset, or the two surfaces teach
    /// different rules again — which is the defect, one level up.
    #[test]
    fn the_warning_agrees_with_the_validator_it_describes() {
        for id in ["MYPINNED-001", "!!!bad***coid", "", &"a".repeat(33)] {
            assert!(!vike_model::is_valid_crypto_coid(id), "premise: {id:?} is invalid");
            assert!(coid_charset_warning(&WireCommand::Cancel(id.into())).is_some(), "{id:?}");
        }
        for id in ["baa0ec7d00", "A", &"z".repeat(32)] {
            assert!(vike_model::is_valid_crypto_coid(id), "premise: {id:?} is valid");
            assert!(coid_charset_warning(&WireCommand::Cancel(id.into())).is_none(), "{id:?}");
        }
    }

    #[test]
    fn guardrail_market_order_has_no_notional_to_check() {
        // Even WITH a ceiling: a market order carries no price, so nothing can be sized
        // client-side — the node is the one that will know.
        let caps = GuardrailCaps { max_qty: None, max_notional: Some(1.0) };
        let g = guardrail_check(&submit_cmd(2.0, None), caps);
        assert_eq!(g.to_json()["notional"], Value::Null);
        assert_eq!(g.to_json()["within_limits"], true);
        assert!(g.line().contains("notional=-"), "{}", g.line());
    }

    #[test]
    fn guardrail_non_submit_verbs_have_no_size_to_check() {
        let g = guardrail_check(&WireCommand::Cancel("c-1".into()), GuardrailCaps::default());
        assert_eq!(g, Guardrail::NoSize);
        assert_eq!(g.to_json()["within_limits"], true);
        assert_eq!(g.line(), "guardrail: no order size to check for this verb");
    }
}
