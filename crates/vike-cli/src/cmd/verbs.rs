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
//! # No retype confirm, on either surface (`docs/decisions/0086` point 7)
//!
//! ⚠ **This section used to say the `policy.toml` TYPED-CONFIRM was deliberately NOT shared.** The
//! REPL prompted the operator to retype the key after the `y/N`, even under `--yes`
//! (`crate::cmd::trade`'s `typed_key_confirm`), and the MCP server refused a policy write that
//! carried no `policy_confirm` before minting a preview token (`mcp`'s `typed_confirm_verdict`).
//! The owner deleted that ceremony for every key — *"confirmation over confirmation … a
//! nightmare"* — and the node had already stopped reading the wire field it filled
//! (`crates/vike-tradehub/src/server.rs`'s `apply_set_setting`). So both clients now send `None`,
//! and that is decided HERE, in [`Verb::to_wire_command`], where neither surface can decide
//! otherwise: [`Verb::SetSetting`] carries no confirm at all.
//!
//! The same site derives the wire's `file` from the KEY ([`section_of_key`]) — step 1 of taking the
//! file era out of the settings wire. A write is one row in the node's settings database, named by
//! its key; the REPL takes `set-setting <key> <value>` and the tool takes `key` + `value`, so no
//! caller can hand a file name that disagrees with the key it names. Whether a write is a POLICY
//! write is likewise read off the key's `policy.` section, never off a `file` (the `mcp` server's
//! unattended gate is the one site left that asks).
//!
//! What each surface keeps is its ORDINARY write gate, shared by every write verb — the REPL's
//! `confirm? [y/N]` (and `--yes`), the MCP server's mandatory preview token. What they now SHARE is
//! the guard 0086 names in the retype's place: [`SettingChange`], the `old → new` both show before a
//! settings write goes out, read off the node the write is aimed at.
//!
//! Everything here is PURE apart from [`guardrail_caps`]'s one env read — no network — so the
//! whole vocabulary is unit-testable without a node.
//!
//! ⚠ The notional cap used to be `VIKE_MAX_ORDER_NOTIONAL`, read here on every check. Phase 5 of
//! the settings-unification design removed that variable: it is the same per-order ceiling the
//! trading binaries enforce, and a ceiling any exported variable can raise is not a ceiling. It now
//! comes from the `policy.max_notional_per_order` row of this machine's settings database,
//! resolved once by the dispatcher ([`crate::run`]) and passed in. `VIKE_MAX_ORDER_QTY` STAYS an
//! environment variable: it has no policy field and no enforcing counterpart anywhere — it is a
//! local typo-catcher for a human at a REPL, not a risk ceiling.

use serde_json::{Value, json};
use vike_model::orders::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::wire::{
    WireCommand, WireOrderRequest, WireSettingsShow, WireTradingState,
};

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
    /// `flatten <venue> <symbol>` — book REQUIRED at the one-shot CLI's own grammar
    /// (`crate::cmd::trade::position`'s `flatten`), account OPTIONAL here because the REPL's and the
    /// `mcp` tool's own grammars still carry none (task 7 of the trade-CLI-plane widened this field;
    /// neither of those two surfaces was in that task's scope). See
    /// [`vike_tradehub_client::wire::WireOrderRequest::account`] for the three wire states this
    /// carries verbatim onto [`WireCommand::Flatten`]'s own `account` field.
    Flatten { venue: String, symbol: String, account: Option<String> },
    /// `market-exit [venue]` — same account note as [`Verb::Flatten`].
    MarketExit { venue: Option<String>, account: Option<String> },
    /// `halt` (Halted) / `resume` (Active) at the REPL and as `vike-cli trade <verb>`;
    /// `set_trading_state` as an MCP tool, which is where `Reducing` is still spelled.
    ///
    /// ⚠ The REPL used to reach this through `state <active|reducing|halted>`, an argument of the
    /// verb that PRINTED the mode — so "look" and "stop trading" differed by one token. Ruling 17
    /// of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` split them: the
    /// read is [`Verb::Status`] and each write is its own word, named for what it DOES.
    SetState(WireTradingState),
    /// `mass-cancel [venue] [symbol]` — same account note as [`Verb::Flatten`].
    MassCancel { venue: Option<String>, symbol: Option<String>, account: Option<String> },
    // ---- WRITE: the node LIFECYCLE (gated the same way) ----
    /// `mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path>) [--id <mount-id>]
    /// [--params <json>]` — the `mount_strategy` tool.
    ///
    /// ⚠ `name` XOR `rhai` is the wire contract and this type does NOT enforce it: each VOCABULARY
    /// refuses both-or-neither at its own edge — the REPL's two flags, the one-shot
    /// `vike-cli trade strategy mount`'s SAME two flags (it mirrors the REPL's grammar rather than
    /// inventing a second one), and the `mcp` tool's two JSON fields — because a refusal phrased in
    /// another surface's spelling is one its reader cannot act on. `rhai` is a path on the
    /// **NODE's** filesystem, not this machine's.
    MountStrategy {
        venue: String,
        /// WHICH ACCOUNT of `venue` the mount trades and reads — absent names none, `DEFAULT` names
        /// the unlabelled account deliberately, a label names that account
        /// ([`vike_tradehub_client::wire::WireOrderRequest::account`] states the three). Carried as
        /// the WIRE string rather than an `AccountLabel` because that type cannot hold the middle
        /// state: its `Serialize` refuses the default variant outright, by design. Each surface
        /// validates with `vike_model::account_keys::parse_wire_account` — the one reader, so the
        /// grammar is never spelled twice.
        account: Option<String>,
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
    /// `set-setting <full.dotted.key> <value>` — the `set_setting` tool. A write is ONE ROW in the
    /// node's settings database, named by its KEY (`docs/decisions/0086`).
    ///
    /// ⚠ **No `file` and no `confirm` field, deliberately.** Both are file-era: the key's first
    /// segment already names its section, and the typed confirm went with 0086 point 7. The WIRE
    /// still carries both — the released v0.1.35 daemon requires `file` on decode — so
    /// [`Verb::to_wire_command`] derives `file` from the key ([`section_of_key`]) and fills
    /// `confirm` with `None`. Neither is an input any surface built on this type can get wrong.
    SetSetting { key: String, value: String },
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
    /// (`crate::cmd::trade::status`). It subsumes the old `state` read and the retired top-level
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
            Verb::Flatten { venue, symbol, account } => Some(WireCommand::Flatten {
                venue: venue.clone(),
                symbol: symbol.clone(),
                account: account.clone(),
            }),
            Verb::MarketExit { venue, account } => {
                Some(WireCommand::MarketExit { venue: venue.clone(), account: account.clone() })
            }
            Verb::SetState(s) => Some(WireCommand::SetTradingState(*s)),
            Verb::MassCancel { venue, symbol, account } => Some(WireCommand::MassCancel {
                venue: venue.clone(),
                symbol: symbol.clone(),
                account: account.clone(),
            }),
            Verb::MountStrategy {
                venue,
                account,
                symbol,
                interval,
                controller_id,
                name,
                rhai,
                params,
            } => Some(WireCommand::MountStrategy {
                venue: venue.clone(),
                account: account.clone(),
                symbol: symbol.clone(),
                interval: interval.clone(),
                controller_id: controller_id.clone(),
                name: name.clone(),
                rhai: rhai.clone(),
                params: params.clone(),
            }),
            Verb::UnmountStrategy { controller_id } => {
                Some(WireCommand::UnmountStrategy { controller_id: controller_id.clone() })
            }
            // The two file-era wire fields, filled HERE and nowhere else: `file` is the key's own
            // section word (the released v0.1.35 daemon requires the field, and ignores its
            // content), and `confirm` is `None` (the node has ignored it since
            // `docs/decisions/0086` point 7). Deleting both from the wire is step 2, after a
            // tolerant daemon is released.
            Verb::SetSetting { key, value } => Some(WireCommand::SetSetting {
                file: section_of_key(key).to_string(),
                key: key.clone(),
                value: value.clone(),
                confirm: None,
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
///     refused here rather than left for the daemon's edge to phrase. The REPL, and the one-shot
///     `vike-cli trade strategy mount` that MIRRORS its grammar rather than inventing a second one
///     (task 8 of the trade-CLI-plane), refuse the same rule in that shared flag spelling — which
///     is the one thing about these three that is genuinely per-VOCABULARY rather than per-tool;
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
                account: None,
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
        // ⚠ `account: None` here is NOT a design decision, only an unwidened surface: task 7 of the
        // trade-CLI-plane threaded the field through `crate::cmd::trade::order`/`position`'s own
        // one-shot grammars, which is where the wire's account-carrying rules are argued
        // (`crate::cmd::trade::oneshot`'s module doc). Widening this MCP tool the same way is future
        // work, not a gap this task's scope covers.
        "flatten" => Ok(Verb::Flatten {
            venue: s("venue").ok_or("flatten requires `venue`")?,
            symbol: s("symbol").ok_or("flatten requires `symbol`")?,
            account: None,
        }),
        "market_exit" => Ok(Verb::MarketExit { venue: s("venue"), account: None }),
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
        // Same note as `flatten`/`market_exit` above: unwidened, not a design decision.
        "mass_cancel" => {
            Ok(Verb::MassCancel { venue: s("venue"), symbol: s("symbol"), account: None })
        }
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
            // Validated HERE so a typo costs a parse error rather than a round trip and a
            // `Response::Error` — and validated with the wire's own reader, so this edge adds a
            // message rather than a second grammar.
            let account = match s("account") {
                Some(a) => match vike_model::account_keys::parse_wire_account(&a) {
                    Ok(_) => Some(a),
                    Err(e) => {
                        return Err(format!(
                            "mount_strategy: `account` — {e}. Omit it to name no account, or pass \
                             \"DEFAULT\" to name the venue's unlabelled account deliberately"
                        ));
                    }
                },
                None => None,
            };
            Ok(Verb::MountStrategy {
                venue: s("venue").ok_or("mount_strategy requires `venue`")?,
                account,
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
        // No `file` and no `policy_confirm`: the key names its section, and the retype went with
        // `docs/decisions/0086` point 7. An agent that still passes either is ignored like any
        // argument a tool does not name — see [`Verb::SetSetting`] for what goes on the wire.
        "set_setting" => Ok(Verb::SetSetting {
            key: s("key").ok_or("set_setting requires `key` (the full dotted key)")?,
            value: setting_value(args)?,
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
                        and rendered); the node parses it as a TOML value and validates the row \
                        with its own loader"
            .to_string()),
        None => Err("set_setting requires `value`".to_string()),
    }
}

/// The SECTION a dotted settings key names — its first segment (`policy`, `config`, `preferences`,
/// `flags`), the word the node derives the section from and the value every client in this crate
/// puts in the wire's `file` ([`Verb::to_wire_command`]).
///
/// Deliberately a plain split, not `vike_config::SettingsFile::of_key`: that answers `None` for a
/// key naming no known section, and the wire field still needs a value for the released v0.1.35
/// daemon to decode — which then refuses the unknown key itself, in its own loader's words. A
/// client copy of the section vocabulary would be a second authority on a question the node
/// already answers. PURE.
pub(crate) fn section_of_key(key: &str) -> &str {
    key.split('.').next().unwrap_or(key)
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
    /// The `policy.max_notional_per_order` row of this machine's settings database. `None` = no
    /// notional cap.
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

/// **A settings write's `old → new`** — shown BEFORE the write goes out, on both surfaces: the REPL
/// prints [`SettingChange::line`] with its preview, the `mcp` server carries
/// [`SettingChange::to_json`] as its preview's `change`.
///
/// It is the guard `docs/decisions/0086` point 7 puts where the retyped key was: the node's loader
/// bounds the value, whoever says yes sees *old → new* first, and the node's journal records who
/// asked. `vike-cli config set` prints the same pair for a LOCAL write, from the row it just wrote
/// (`crates/vike-cli/src/cmd/config_set.rs`'s `report_lines`). A REMOTE write's old value lives on
/// the node, so each surface reads it there — the node's `SettingsShow`, the effective row
/// `settings_show` serves — and hands the answer to [`SettingChange::from_show`]. This type is the
/// PURE half: what was read in, what is shown out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingChange {
    /// The full dotted key being written.
    pub(crate) key: String,
    /// What the node runs with for `key` right now — or why that could not be read.
    pub(crate) old: OldSetting,
    /// The value as it will be sent: text, which the node parses as TOML.
    pub(crate) new: String,
}

/// The OLD half of a [`SettingChange`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OldSetting {
    /// The node's EFFECTIVE value as its `SettingsShow` renders it (`""` = unset), and the layer
    /// that set it — `WireSettingsRow::origin`: `default`, `db`, or `env:<VAR>`.
    Read { value: String, origin: String },
    /// The node answered and carries no row for this key. Its loader will refuse the write and say
    /// why; the preview only says there was nothing to show.
    NoSuchKey,
    /// The node could not be asked — no node, no observe key, an older node, a transport fault. The
    /// old value is UNKNOWN and every rendering says so: a fabricated one is the one field an agent
    /// would relay to the owner as fact. Not a refusal either — the write is still the caller's to
    /// confirm, against a line that says what it does not know.
    Unread(String),
}

impl SettingChange {
    /// Pair `key`'s current value, out of the node's `SettingsShow` answer (or the reason there is
    /// none), with the `new` value about to be sent. PURE.
    pub(crate) fn from_show(key: &str, new: &str, show: Result<&WireSettingsShow, String>) -> Self {
        let old = match show {
            Ok(show) => {
                show.rows.iter().find(|row| row.key == key).map_or(OldSetting::NoSuchKey, |row| {
                    OldSetting::Read { value: row.value.clone(), origin: row.origin.clone() }
                })
            }
            Err(why) => OldSetting::Unread(why),
        };
        SettingChange { key: key.to_string(), old, new: new.to_string() }
    }

    /// `policy.max_notional_per_order: 100.0 → 250` — the one line both surfaces show, with the
    /// reason in the OLD slot when there is no old value to show.
    pub(crate) fn line(&self) -> String {
        let old = match &self.old {
            OldSetting::Read { value, .. } if value.is_empty() => "(unset)".to_string(),
            OldSetting::Read { value, .. } => value.clone(),
            OldSetting::NoSuchKey => "(the node carries no such key)".to_string(),
            OldSetting::Unread(why) => format!("(current value not read: {why})"),
        };
        format!("{}: {old} → {}", self.key, self.new)
    }

    /// The environment variable that sets `key`'s current value on the node, when one does.
    ///
    /// ⚠ The ENVIRONMENT outranks a settings row (`docs/decisions/0086`'s first declared residual),
    /// so such a key does not follow the row this write lands for as long as the variable is set. A
    /// surface prints this beside [`Self::line`] — an `old → new` that cannot come true is exactly
    /// the believed-but-false outcome the line exists to prevent.
    pub(crate) fn env_shadow(&self) -> Option<&str> {
        match &self.old {
            OldSetting::Read { origin, .. } if origin.starts_with("env:") => Some(origin),
            _ => None,
        }
    }

    /// The `mcp` preview's `change` object: the same facts, typed. `old` is null — never `""`, which
    /// would read as "unset" — whenever no old value was read, and `old_unread` then says why.
    pub(crate) fn to_json(&self) -> Value {
        let (old, origin, unread) = match &self.old {
            OldSetting::Read { value, origin } => {
                (Some(value.as_str()), Some(origin.as_str()), None)
            }
            OldSetting::NoSuchKey => (
                None,
                None,
                Some("the node carries no such key — its loader will refuse the write"),
            ),
            OldSetting::Unread(why) => (None, None, Some(why.as_str())),
        };
        json!({
            "key": self.key,
            "old": old,
            "old_origin": origin,
            "old_unread": unread,
            "new": self.new,
            "line": self.line(),
        })
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

#[path = "verbs_tests.rs"]
#[cfg(test)]
mod verbs_tests;
