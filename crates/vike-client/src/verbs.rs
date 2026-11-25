//! The ONE node-write verb vocabulary shared by the two agent surfaces — the `trade` REPL (a human
//! typing verbs) and the `mcp` server (an agent calling tools). Both used to build [`CommandWire`]s
//! independently and had already drifted: the REPL supported `modify`/`mass-cancel` while the MCP
//! surface did not, and each carried its own copy of the client-side env-cap guardrail. This module
//! closes that drift structurally:
//!
//! - [`Verb`] (+ [`Verb::to_wire_command`]) is the SINGLE args→[`CommandWire`] construction site.
//!   The REPL's `parse_line` (still in `vike-cli`'s `cmd::trade` — the line GRAMMAR is REPL-specific)
//!   produces a `Verb`; the MCP write tools go JSON-args →
//!   `crates/vike-mcp/src/tool_verbs.rs`'s `verb_from_tool_args` → the SAME
//!   `Verb::to_wire_command`. A new write verb added here is visibly missing from whichever surface
//!   forgets to expose it, instead of silently absent from one.
//! - [`fill_order_client_id`] is the SINGLE order client id MINT. A vike-tradehub REFUSES a remote
//!   `Submit` whose `order_client_id` is empty (`lower_command`'s idempotency policy: a remote peer
//!   whose id the runtime minted has no stable handle to cancel or dedup by), so every client of
//!   that protocol must PRE-MINT one. Both surfaces used to leave it empty — the REPL always
//!   (`parse_submit` set `String::new()` with a comment saying "let the runtime mint one", an
//!   in-process assumption the wire refuses), the MCP tool whenever the agent omitted the optional
//!   argument. The REPL therefore could not place an order from the day that server rule landed.
//!   One filler, applied by both, using [`order_client_id_minter`].
//! - **The client half of the v2 order client id** (`docs/decisions/0270-client-order-ids-carry-source-strategy-account-intent-and-group.md`):
//!   [`order_client_id_minter`] is a surface's ONE session and its source letter (`C`, `M`, `T`);
//!   [`fill_order_client_id`] mints in the shape and account code of the block the command reaches on the node's frame
//!   (a Binance account gets the compact shape), with the intent that frame's position gives, one group stem per order
//!   list, and a bracket's stem only where the node takes one; [`decode_order_client_id`] reads any id back in words, with the
//!   names the frame gives (the account's label, the strategy behind a mount's code, the group resting now). The id
//!   format, the parser and the words are `vike_model::orders::client_id`'s; this is only where a client gets
//!   its inputs.
//! - [`guardrail_check`] is the SINGLE client-side advisory guardrail over the [`GuardrailCapabilities`],
//!   with two renderers off one [`Guardrail`] value: [`Guardrail::to_json`] (the MCP preview
//!   payload) and [`Guardrail::line`] (the REPL preview line). The node's server-side
//!   `ControlLimits` + `RiskGate` are the ENFORCING gate regardless — this check is UX, the node
//!   gate is truth.
//! - [`unsizeable_submit_refusal`] is the SINGLE answer to "can this `Submit` be sized at all?",
//!   called at PARSE time by all three paths that build one — the REPL's `parse_submit`, the
//!   one-shot verb's `parse_submit_args` and the tool dialect's `submit_order` arm. It
//!   exists because the advisory [`guardrail_check`] above has no honest verdict for an order it
//!   cannot size: with no cap set — the default — it printed `within limits` for a zero qty, an
//!   infinite one or a limit order with no price, and with a cap set the verdict was whatever a
//!   comparison against NaN or an infinity happened to return. The node (its core risk gate) or
//!   the venue behind it refuses every one of those anyway; this makes the CLIENT say so first,
//!   at the keyboard rather than a round trip later. It refuses a malformed order and nothing
//!   else — a sizeable order over every cap is still built. Both doors then refuse one whose
//!   notional [`guardrail_check`] measures over the notional cap, before the confirm
//!   (`crate::order_gate::judge`); only the quantity cap stays advisory.
//!
//! # The three node-LIFECYCLE verbs, and why they joined
//!
//! `mount` / `unmount` / `set-setting` (`mount_strategy` / `unmount_strategy` / `set_setting` as
//! tools) change what the node RUNS and how it is CONFIGURED rather than what its book holds. They
//! reached the two surfaces in the same week, from two authors working under file ownership, and
//! each built them OUTSIDE this module because neither owned it: the REPL parsed them straight to a
//! [`CommandWire`] and the MCP server rebuilt them in a `lifecycle_command` of its own. Each author
//! independently wrote the same note beside their copy — lift these the day the OTHER surface grows
//! them, because a verb with one surface has no second surface to drift from. Both surfaces had
//! them within days of each other, which made that day the same day, and the second construction
//! site is now gone.
//!
//! [`Verb`] therefore names the node-WRITE vocabulary rather than the order-write one, and the
//! widening costs nothing that was load-bearing: **nothing keys on a verb being an ORDER**.
//! [`Verb::is_write`] — the discriminator both surfaces route their preview gate on — is what this
//! type is actually for, and the three sites that genuinely care about order-ness
//! ([`guardrail_check`], [`fill_order_client_id`], [`order_client_id_charset_warning`]) all key on the
//! [`CommandWire`] VARIANT, and were already total over the lifecycle ones before they arrived here.
//!
//! # No retype confirm, on either surface (`docs/decisions/0086` point 7)
//!
//! ⚠ **This section used to say the `policy.toml` TYPED-CONFIRM was deliberately NOT shared.** The
//! REPL prompted the operator to retype the key after the `y/N`, even under `--yes`
//! (`vike-cli`'s `cmd::trade`'s `typed_key_confirm`), and the MCP server refused a policy write that
//! carried no `policy_confirm` before minting a preview token (`mcp`'s `typed_confirm_verdict`).
//! The owner deleted that ceremony for every key — *"confirmation over confirmation … a
//! nightmare"* — and the node had already stopped reading the wire field it filled
//! (`crates/vike-tradehub/src/server/settings.rs`'s `apply_set_setting`). So both clients now send `None`,
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
//! Everything here is PURE apart from [`guardrail_capabilities`]'s read of the row the boot installed — no
//! network, no environment — so the whole vocabulary is unit-testable without a node. The contract
//! value [`guardrail_check`] sizes by arrives as a parameter; `crate::addressed` reads it off the
//! node's frame.
//!
//! ⚠ The notional cap used to be `VIKE_MAX_ORDER_NOTIONAL`, read here on every check. Phase 5 of
//! the settings-unification design removed that variable: it is the same per-order ceiling the
//! trading binaries enforce, and a ceiling any exported variable can raise is not a ceiling. It now
//! comes from the `policy.max_notional_per_order` row of this machine's settings database,
//! resolved once by the dispatcher (`vike_cli::run`) and passed in. The quantity cap is the
//! `preferences.max_order_qty` row (decision 0111): it has no policy field and no enforcing
//! counterpart anywhere — a local typo-catcher for a human at a REPL, not a risk ceiling — so it
//! is a preference, and `VIKE_MAX_ORDER_QTY` refuses startup naming it.

use std::time::Duration;

use serde_json::{Value, json};
use vike_model::accounts::account_keys::label_of_route_key;
use vike_model::orders::client_id::{
    OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdIntent, OrderClientIdLabels,
    OrderClientIdLegRole, OrderClientIdPlan, OrderClientIdReading, OrderClientIdShape,
    OrderClientIdSource, OrderClientIdStrategyCode, describe_order_client_id,
    parse_order_client_id,
};
use vike_model::{ContractValue, NotionalKind, OrderType};
use vike_model::{OrderStopAdjust, OrderStopAdjustTo, TradingState};
use vike_tradehub_client::wire::{
    CommandWire, CoreSnapshotWire, MountViewWire, OrderBracketWire, OrderComboWire,
    OrderListLegWire, OrderListWire, OrderPegWire, OrderReleasePriceWire, OrderRequestWire,
    SettingsShowWire, VenueBlockWire, names_no_account_refusal,
};
use vike_utility::number_text::format_number;

use crate::refusals::{ModifyRefusal, OrderListRefusal, SubmitRuleRefusal};

/// A surface's order client id mint: ONE v2 session for this process and the [`OrderClientIdSource`] letter every id it mints carries.
/// Built by [`order_client_id_minter`], spent by [`fill_order_client_id`]; one per surface, so a surface cannot stamp two letters.
pub struct OrderClientIdMinter {
    order_client_ids: OrderClientIdGenerator,
    source: OrderClientIdSource,
}

/// A fresh order client id mint for THIS process whose ids say `source` made them (`OrderClientIdSource::Cli` for the CLI,
/// `OrderClientIdSource::Mcp` for the MCP server, `OrderClientIdSource::Telegram` for Telegram): decision 0270's v2 form,
/// `<src><strategy><account><U><intent>[<role>]`, the strategy always `0000` (a human or an agent, no strategy).
///
/// Reusing `vike_model::orders::client_id::OrderClientIdGenerator` rather than inventing a shape buys four
/// properties this surface needs, and every one of them was a stated requirement:
///
/// - **Unique per order.** `seq` increments, so two `submit`s in one session can never share an id
///   (the node refuses a reused order client id in words, at its door and again in the core, and before that it
///   booked NOTHING the second time in silence, the worst possible failure for an order surface).
/// - **No collision across sessions.** The session's base is drawn from OS entropy per PROCESS
///   (`OrderClientIdGenerator::new_v2`), the bar the live core's own generator is held to; two REPLs
///   open side by side, or one restarted while an order still rests at the venue, get different ones.
/// - **Read back by decode, not by eye.** 17 to 24 characters (21 for a plain order on a full-shape
///   account; a Binance account's compact id is shorter, a list leg's longer), upper-case labels
///   around a lower-case unique part, so no two ids ever differ only by case. The REPL prints it in
///   the preview and in `orders`, a human copies it back at `cancel <order client id>` / `modify <order client id>`, and
///   [`decode_order_client_id`] says what it means.
/// - **Venue-safe.** `is_valid_crypto_order_client_id` (`^[A-Za-z0-9]{1,32}$`) is the strictest common
///   denominator across Binance / Bybit / OKX and every mint asserts it; the shape is the one the
///   target account's block publishes, fitted to its venue's budget, so an id minted here survives
///   the node, the core and the venue edge unchanged.
///
/// SESSION-scoped, not per-command: the sequence is what makes ids unique without asking the OS for
/// entropy per order, and it is what the live core does. A client never carries an instance tag.
pub fn order_client_id_minter(source: OrderClientIdSource) -> OrderClientIdMinter {
    OrderClientIdMinter { order_client_ids: OrderClientIdGenerator::new_v2(None), source }
}

/// Fill a command's EMPTY order client id from `minter`; return everything else untouched.
///
/// The one place an order client id is minted on this side of the wire. Called BEFORE the preview on both
/// surfaces, deliberately: the id the operator (or the agent) is shown must be the id that gets
/// sent, or the preview is a different command from the one confirmed. The Telegram channel mints
/// through this function too, at its preview (`vike_telegram::TelegramDependencies::fill_order_client_id`).
///
/// What it mints, off `frame` (the node's newest snapshot; `None` when none was read):
///
/// - **The target account's shape and code.** The block the command reaches, resolved as
///   `crate::addressed` resolves it for the guardrail (the door's own rule): its
///   `VenueBlockWire::order_client_id_shape` (a Binance account's compact one) and `account_code`. A block that
///   says neither, an older node and no frame all mint the full shape with account `00`, labels
///   that decide nothing (a venue that cannot take a full id receives it bare).
/// - **The intent**, `OrderClientIdIntent::of` over the side, the reduce-only flag, whether the symbol is held
///   fully funded ([`vike_catalog::books_cash`]: on spot a sell only closes a long, `CL`, the owner's ruling) and the
///   net position that block holds in the symbol. Best-effort, as decision 0270's 4.6 says a client's
///   is: no frame reads as flat.
/// - **One group stem per order list**: every leg that names no id is the stem plus its intent and
///   its `OrderClientIdLegRole::of` role, so the list costs one sequence number and sorts together; a list whose
///   numbered roles pass 35 takes one plain id per leg, as before stems.
/// - **A bracket's stem, only where the node takes one**: `takes` is the node's gate asked without
///   sending (`TradehubControlHandle::takes`, so the feature word `FEATURE_BRACKET_STEM` is spelled
///   once, in its table), and a bracket an older node would refuse goes out without one, as before
///   (its core mints the three ids). A bracket that already names a stem keeps it, so a resend after
///   an unclear reply names the same group and is refused whole.
///
/// An id already present is NEVER overwritten — that is the explicit-override path (`submit …
/// --order-client-id <id>` in the REPL, `order_client_id` in the MCP tool arguments). Every other verb either
/// targets an existing order client id or is account-wide, so it passes through. PURE apart from the
/// generator's own counter.
pub fn fill_order_client_id(
    cmd: CommandWire,
    minter: &mut OrderClientIdMinter,
    frame: Option<&CoreSnapshotWire>,
    takes: impl Fn(&CommandWire) -> bool,
) -> CommandWire {
    let (plan, position) = mint_target(&cmd, frame);
    let src = minter.source;
    let label = |side: i32, reduce_only: bool, cash: bool| {
        OrderClientIdLabels::new(
            src,
            OrderClientIdStrategyCode::NONE,
            OrderClientIdIntent::of(side, reduce_only, cash, position),
        )
    };
    match cmd {
        CommandWire::Submit(mut req) if blank(Some(req.order_client_id.as_str())) => {
            let cash = vike_catalog::books_cash(&req.venue, &req.symbol);
            req.order_client_id =
                plan.mint(&mut minter.order_client_ids, label(req.side, req.reduce_only, cash));
            CommandWire::Submit(req)
        }
        // An ORDER LIST: every leg that names no id gets one, the submit's rule leg by leg, so a resend names the same
        // ids and the node refuses it whole rather than placing a second list.
        CommandWire::OrderList(mut list) => {
            let cash = vike_catalog::books_cash(&list.venue, &list.symbol);
            fill_list(&mut list.legs, &plan, minter, position, cash);
            CommandWire::OrderList(list)
        }
        // A COMBO's one id, the submit's rule. It names no one symbol, so it reads as flat, and no venue that takes
        // one books it fully funded.
        CommandWire::Combo(mut combo) if blank(combo.order_client_id.as_deref()) => {
            combo.order_client_id =
                Some(plan.mint(&mut minter.order_client_ids, label(combo.side, false, false)));
            CommandWire::Combo(combo)
        }
        // Asked with a probe BEFORE a sequence number is spent on a stem the node would refuse.
        CommandWire::Bracket(mut spec)
            if blank(spec.order_client_id.as_deref())
                && takes(&CommandWire::Bracket(OrderBracketWire {
                    order_client_id: Some(String::new()),
                    ..spec.clone()
                })) =>
        {
            let stem = plan.generate_stem(
                &mut minter.order_client_ids,
                src,
                OrderClientIdStrategyCode::NONE,
            );
            spec.order_client_id = stem.as_str().map(str::to_string);
            CommandWire::Bracket(spec)
        }
        other => other,
    }
}

/// No id: absent, empty or blank.
fn blank(id: Option<&str>) -> bool {
    id.is_none_or(|id| id.trim().is_empty())
}

/// The legs of one list that name no id, from ONE stem (decision 0270's 4.1).
fn fill_list(
    legs: &mut [OrderListLegWire],
    plan: &OrderClientIdPlan,
    minter: &mut OrderClientIdMinter,
    position: f64,
    cash: bool,
) {
    if !legs.iter().any(|leg| blank(leg.order_client_id.as_deref())) {
        return;
    }
    let src = minter.source;
    let roles = OrderClientIdLegRole::of(&role_legs(legs));
    let stem = roles.is_some().then(|| {
        plan.generate_stem(&mut minter.order_client_ids, src, OrderClientIdStrategyCode::NONE)
    });
    for (i, leg) in legs.iter_mut().enumerate() {
        if !blank(leg.order_client_id.as_deref()) {
            continue;
        }
        let intent = OrderClientIdIntent::of(leg.side, leg.reduce_only, cash, position);
        leg.order_client_id = Some(match (&stem, &roles) {
            (Some(stem), Some(roles)) => stem.leg(&mut minter.order_client_ids, intent, roles[i]),
            _ => plan.mint(
                &mut minter.order_client_ids,
                OrderClientIdLabels::new(src, OrderClientIdStrategyCode::NONE, intent),
            ),
        });
    }
}

/// The legs as `OrderClientIdLegRole::of` reads them (kind, reduce-only, parent), so a client's roles are the core's by the one
/// rule. A kind word no parser reads is a market leg here, a member; the node refuses the list anyway.
fn role_legs(legs: &[OrderListLegWire]) -> Vec<vike_model::OrderListLeg> {
    legs.iter()
        .map(|leg| vike_model::OrderListLeg {
            side: leg.side,
            qty: leg.qty,
            order_type: OrderType::parse(&leg.order_type).unwrap_or(OrderType::Market),
            price: leg.price,
            trigger_price: leg.trigger_price,
            trail: None,
            reduce_only: leg.reduce_only,
            time_in_force: vike_model::TimeInForce::Gtc,
            parent: leg.parent,
            contingency: None,
            linked: Vec::new(),
            order_client_id: None,
        })
        .collect()
}

/// The plan of the block `cmd` reaches on `frame` and the net position that block holds in the symbol `cmd` names;
/// with no block to read, [`plan_of`]'s defaults and flat.
fn mint_target(cmd: &CommandWire, frame: Option<&CoreSnapshotWire>) -> (OrderClientIdPlan, f64) {
    let target = frame.and_then(|frame| crate::addressed::order_client_id_target(cmd, frame));
    (plan_of(target.map(|(block, _)| block)), target.map_or(0.0, |(_, position)| position))
}

/// The mint plan of `block`'s account: its published shape and account code, the full shape and `00` for what it
/// does not say. A shape `OrderClientIdPlan::new` would panic on (`OrderClientIdShape::is_mintable` false) is not said either: the
/// node's word is a reading, never a reason to end the client.
fn plan_of(block: Option<&VenueBlockWire>) -> OrderClientIdPlan {
    let account = block
        .and_then(|block| block.account_code.as_deref())
        .and_then(OrderClientIdAccountCode::parse)
        .unwrap_or(OrderClientIdAccountCode::NONE);
    let shape = block
        .and_then(|block| block.order_client_id_shape)
        .filter(|shape| shape.is_mintable())
        .unwrap_or(OrderClientIdShape::FULL);
    OrderClientIdPlan::new(shape, account)
}

/// **What an order client id says, with the names the node gives it** (decision 0270's decode surfaces: `vike-cli
/// trade order decode` and the MCP tool `decode_order_id` both answer with this). The words are `vike_model`'s
/// `describe_order_client_id`; this adds what only the node knows: the label of the account whose `VenueBlockWire::account_code`
/// the id carries, the strategy of the mount whose `MountViewWire::order_client_id_code` it carries, and the other legs of its
/// group resting now. PURE: `frame` is the node's snapshot (`None` when none was read) and `mounts` its mount rows
/// (`Response::StrategyStatus`, empty when not read); with neither it gives the codes and the words alone.
///
/// `form` is `v2`, `v1` (minted before v2: it says nothing but its instance tag) or `other` (no form this platform
/// mints: a venue's own id, a foreign order's, one typed by hand). A `null` field is one the id or the node does not
/// say. Labels decide nothing: this is for a human or an agent to read.
pub fn decode_order_client_id(
    id: &str,
    frame: Option<&CoreSnapshotWire>,
    mounts: &[MountViewWire],
) -> Value {
    let Some(parsed) = parse_order_client_id(id) else {
        return json!({
            "id": id,
            "form": "other",
            "words": "not a form this platform mints (a venue's own id, a foreign order's, or one typed by hand)",
        });
    };
    let mut words = describe_order_client_id(&parsed);
    let OrderClientIdReading::V2(p) = parsed else {
        return json!({ "id": id, "form": "v1", "origin": words.origin, "words": words.to_string() });
    };
    let block = p.account.filter(|code| *code != OrderClientIdAccountCode::NONE).and_then(|code| {
        frame?.venues.iter().find(|block| block.account_code.as_deref() == Some(code.as_str()))
    });
    words.account_label =
        block.map(|block| block.account.clone().unwrap_or_else(|| "(no label)".to_string()));
    let mount = words.strat.and_then(|code| {
        mounts.iter().find(|m| m.order_client_id_code.as_deref() == Some(code.as_str()))
    });
    words.strategy_name = mount.map(|m| m.strategy.clone());
    let siblings: Vec<&str> = match (p.role, frame) {
        (Some(_), Some(frame)) => frame
            .orders
            .iter()
            // The status as every surface reads it, an older node's Debug spelling included.
            .filter(|o| {
                vike_tradehub_client::observe_decode::order_status_of(&o.status)
                    .is_some_and(|status| !status.is_terminal())
            })
            .map(|o| o.order_client_id.as_str())
            .chain(frame.held_exits.iter().map(|o| o.order_client_id.as_str()))
            .filter(|other| {
                *other != id
                    && matches!(parse_order_client_id(other), Some(OrderClientIdReading::V2(q)) if q.role.is_some() && q.stem == p.stem)
            })
            .collect(),
        _ => Vec::new(),
    };
    json!({
        "id": id,
        "form": "v2",
        "words": words.to_string(),
        "origin": p.origin,
        "source": char::from(p.src.letter()).to_string(),
        "strategy": words.strat.map(|code| code.to_string()),
        "strategy_name": words.strategy_name,
        "mount_id": mount.map(|m| m.mount_id.as_str()),
        "account": p.account.map(|code| code.to_string()),
        "account_number": p.account.and_then(OrderClientIdAccountCode::number),
        "account_label": words.account_label,
        "route_key": block.map(vike_tradehub_client::wire::block_key),
        "unique": p.unique,
        "intent": p.intent.code(),
        // The role's own text sits between the stem's intent and any `R<n>`.
        "role": p.role.map(|_| &p.chain[p.stem.len() + 2..]),
        "group": p.role.map(|_| p.stem),
        "replacement": p.generation,
        "siblings": siblings,
    })
}

/// The ACCOUNTS among `venue`'s running engines: `keys` without the venue's lane twin when its parent is up too
/// (`vike_venues::lane_twin`: one account's second lane, no second account). PURE. Told by the table's label,
/// so an operator's real account spelling that label reads as the twin here and is the door's to count
/// (`crates/vike-tradehub/src/server/refusal.rs`'s `one_account_addressed`, which knows): a client that lets a bare
/// venue past is corrected by the door, never the other way round.
#[must_use]
pub fn accounts_of_venue<'a>(venue: &str, keys: &[&'a str]) -> Vec<&'a str> {
    let twin = vike_venues::lane_twin(venue).map(vike_venues::AccountRowLaneTwin::route_key);
    let parent_up = keys.contains(&venue);
    keys.iter().copied().filter(|key| !(parent_up && twin.as_deref() == Some(*key))).collect()
}

/// The route keys of `venue` the frame lists, running or NOT RUNNING, a lane twin's included: what a client can see
/// of the accounts a bare venue is counted by. Sorted.
#[must_use]
pub fn venue_keys_on_frame(venue: &str, frame: &CoreSnapshotWire) -> Vec<String> {
    let mut keys: Vec<String> = frame
        .venues
        .iter()
        .map(vike_tradehub_client::wire::block_key)
        .chain(frame.not_running.iter().map(|row| row.route_key.clone()))
        .filter(|key| label_of_route_key(venue, key).is_some())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// **The early refusal of a command that names a venue and no account, where this client can already see the
/// venue has more than one, running or not** (decision 0254), in the door's own sentence. `None` for every other
/// command and for a frame that shows no such thing: the node's door is the enforcement, and this only saves the
/// round trip, the preview and the prompt.
#[must_use]
pub fn bare_venue_refusal(cmd: &CommandWire, frame: &CoreSnapshotWire) -> Option<String> {
    let (venue, subject) = cmd.names_no_account()?;
    let keys = venue_keys_on_frame(venue, frame);
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    (accounts_of_venue(venue, &keys).len() > 1)
        .then(|| names_no_account_refusal(subject, venue, &keys))
}

/// One parsed command against a running vike-tradehub. WRITE verbs ([`Verb::is_write`]) go
/// through each surface's mandatory preview gate and map to a [`CommandWire`] via
/// [`Verb::to_wire_command`]; READ/meta verbs render from the snapshot (REPL) or have their own
/// tools (MCP). Moved here from `trade.rs` so BOTH surfaces share one construction site.
///
/// The WRITE half splits into ORDERS (what the node's book holds) and the node LIFECYCLE (what the
/// node runs and how it is configured). That split is descriptive and nothing routes on it — see
/// this module's doc for why the lifecycle three are variants here rather than a sibling type.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one verb per operator line, never queued in bulk: the submit's size (the attributes wave's terms \
              took it past the lint) costs nothing, as on `CommandWire`"
)]
pub enum Verb {
    // ---- WRITE (gated) ----
    /// `submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only]`
    Submit(OrderRequestWire),
    /// `trade order bracket <account> <symbol> <buy|sell> <qty> [@<price>|@market] --stop-loss <px>
    /// --take-profit <px>` - ONE TP/SL bracket ([`CommandWire::Bracket`]), the command the desktop's
    /// TP/SL ticket sends. One-shot only: no REPL word and no `mcp` tool builds it (the gate's
    /// `NOT_ON_THE_MCP` row says why). It is built with no order client id: [`fill_order_client_id`]
    /// gives it its group stem where the node takes one, and the core derives the three legs' ids
    /// from that stem, or mints them itself after the node's answer.
    Bracket(OrderBracketWire),
    /// `oco` / `oto` / `ladder` (the REPL and `trade order <verb>` one-shot) and the `submit_order_list` tool - ONE
    /// order list ([`CommandWire::OrderList`]): an OCO pair, an OTO entry with its exit, a ladder, or (from the tool)
    /// any list of linked legs, every leg on the one book and symbol it names. Built by [`oco_list`], [`oto_list`]
    /// and [`ladder_list`]; every leg's order client id is the caller's ([`with_leg_order_client_ids`]) or minted by
    /// [`fill_order_client_id`] before the preview, as a submit's is, so a resend under the same ids is refused whole.
    OrderList(OrderListWire),
    /// `trade order combo <account> <buy|sell> <qty> --leg <symbol>:<ratio>[@<limit>]... [--net <px>] [--time-in-force <word>]
    /// [--order-client-id <id>]` and the `submit_combo` tool - ONE combo ([`CommandWire::Combo`]): two legs or more traded as one order on the one
    /// book it names, priced by its net, by a limit per leg, or at market. Built by [`combo_of`]; its ONE client order
    /// id is the caller's or minted by [`fill_order_client_id`] before the preview, as a submit's is, so a resend under
    /// the same id is refused whole.
    Combo(OrderComboWire),
    /// `cancel <order client id>`
    Cancel(String),
    /// `modify <order client id> [--qty Q] [--price P] [--trigger P]`
    Modify {
        order_client_id: String,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        /// The new trigger price of a trigger order ([`CommandWire::Modify`]'s `new_trigger_price`).
        new_trigger_price: Option<f64>,
    },
    /// `flatten <account> <symbol>`, account ABSENT for a bare venue (the `mcp` tool's omitted `account`), which
    /// the node accepts only where the venue has one account, running or not (decision 0254, [`bare_venue_refusal`]). See
    /// [`vike_tradehub_client::wire::OrderRequestWire::account`] for the three wire states this
    /// carries verbatim onto [`CommandWire::Flatten`]'s own `account` field.
    Flatten { venue: String, symbol: String, account: Option<String> },
    /// `close-all [account]` — same account note as [`Verb::Flatten`]; no venue at all is the node-wide button.
    MarketExit { venue: Option<String>, account: Option<String> },
    /// `halt` (Halted) / `resume` (Active) at the REPL and as `vike-cli trade <verb>`;
    /// `set_trading_state` as an MCP tool, which is where `Reducing` is still spelled.
    ///
    /// ⚠ The REPL used to reach this through `state <active|reducing|halted>`, an argument of the
    /// verb that PRINTED the mode — so "look" and "stop trading" differed by one token. Ruling 17
    /// of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` split them: the
    /// read is [`Verb::Status`] and each write is its own word, named for what it DOES.
    SetState(TradingState),
    /// `mass-cancel [account] [symbol]` — same account note as [`Verb::Flatten`]; no venue at all is the node-wide
    /// button.
    MassCancel { venue: Option<String>, symbol: Option<String>, account: Option<String> },
    // ---- WRITE: the node LIFECYCLE (gated the same way) ----
    /// `mount <venue> <symbol> <interval> --strategy <path> [--id <mount-id>] [--params <json>]` —
    /// the `mount_strategy` tool.
    ///
    /// ⚠ The strategy FILE is the one source, and this type does NOT require it: each VOCABULARY
    /// refuses its absence at its own edge — the REPL's `--strategy`, the one-shot
    /// `vike-cli trade strategy mount`'s SAME flag (it mirrors the REPL's grammar rather than
    /// inventing a second one), and the `mcp` tool's `strategy` — because a refusal phrased in
    /// another surface's spelling is one its reader cannot act on. `file` is a path under the
    /// **NODE's** `strategies/` folder, not this machine's. The wire's retired `name` and `rhai` are
    /// sent by no client ([`Verb::to_wire_command`] leaves them `None`).
    MountStrategy {
        venue: String,
        /// WHICH ACCOUNT of `venue` the mount trades and reads — absent names none, `DEFAULT` names
        /// the unlabelled account deliberately, a label names that account
        /// ([`vike_tradehub_client::wire::OrderRequestWire::account`] states the three). Carried as
        /// the WIRE string rather than an `AccountLabel` because that type cannot hold the middle
        /// state: its `Serialize` refuses the default variant outright, by design. Each surface
        /// validates with `vike_model::accounts::account_keys::parse_wire_account` — the one reader, so the
        /// grammar is never spelled twice.
        account: Option<String>,
        symbol: String,
        interval: String,
        /// `None` lets the NODE derive `{venue}__{symbol}__{interval}`. Deliberately not derived on
        /// this side: that rule is the node's, and a client copy would be a second one to keep in
        /// step for no gain.
        controller_id: Option<String>,
        /// The strategy FILE: its path under the NODE's `strategies/` folder.
        file: Option<String>,
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
    /// `trade reconcile confirm <id>` - resolve one HELD reconcile divergence by the id `trade reconcile ls`
    /// printed ([`CommandWire::ReconcileConfirm`]). One-shot only: no REPL word and no `mcp` tool
    /// builds it (the gate's `NOT_ON_THE_MCP` row says why).
    ReconcileConfirm { id: u64 },
    /// `trade reconcile cancel <id>` - send ONE cancel for the order a held `UnknownOrder` row names
    /// ([`CommandWire::ReconcileCancel`], decision 0196 rule (b)); the node refuses any account that is
    /// not `demo`. One-shot only, like [`Verb::ReconcileConfirm`]: no REPL word and no `mcp` tool builds
    /// it (the gate's `NOT_ON_THE_MCP` row says why).
    ReconcileCancel { id: u64 },
    /// `trade venue resume <venue> [--force]` - release the LINK DEAD-MAN's halt of ONE venue
    /// ([`CommandWire::ResumeVenue`]), which `resume` cannot do without also clearing every other
    /// venue's halt and the drawdown latch. One-shot only, like [`Verb::ReconcileConfirm`]: no REPL word
    /// and no `mcp` tool builds it (the gate's `NOT_ON_THE_MCP` row says why).
    ResumeVenue { venue: String, force: bool },
    /// `trade account halt|reduce|resume <account>` - the trading state of ONE account's engine
    /// ([`CommandWire::SetAccountTradingState`]), which the node-wide `halt`/`resume` cannot do without
    /// also reaching every other account. `account` is the WIRE string (`DEFAULT` or a label), the
    /// `parse_wire_account` grammar, and `None` a bare venue, which the node's door takes only where the venue
    /// has one account (decision 0254). One-shot only, like [`Verb::ResumeVenue`]: no REPL word and no `mcp`
    /// tool builds it (the gate's `NOT_ON_THE_MCP` row says why).
    SetAccountState { venue: String, account: Option<String>, state: TradingState },
    /// `trade account remove-symbol <account> <symbol>` - GIVE BACK one symbol the account's engine admitted on demand
    /// ([`CommandWire::UnadmitSymbol`], decision 0205), so its slot (one of sixteen) is free for the next one.
    /// `account` as on [`Verb::SetAccountState`]. One-shot only, like [`Verb::SetAccountState`]: no
    /// REPL word and no `mcp` tool builds it (the gate's `NOT_ON_THE_MCP` row says why).
    UnadmitSymbol { venue: String, account: Option<String>, symbol: String },
    /// `trade strategy reset-budget <mount>` - clear ONE mount's per-mount budget LATCH ([`CommandWire::UnlatchMount`], decision
    /// 0239): the mount trades again once it is flat and quiet, with its loss budget counting from where it stands.
    /// `mount_id` is the id `trade strategy ls` prints. One-shot only, like [`Verb::UnadmitSymbol`]: no REPL word and no
    /// `mcp` tool builds it (the gate's `NOT_ON_THE_MCP` row says why).
    UnlatchMount { mount_id: String },
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
    /// (`vike-cli`'s `cmd::trade::status`). It subsumes the old `state` read and the retired top-level
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
                | Verb::Bracket(_)
                | Verb::OrderList(_)
                | Verb::Combo(_)
                | Verb::Cancel(_)
                | Verb::Modify { .. }
                | Verb::Flatten { .. }
                | Verb::MarketExit { .. }
                | Verb::SetState(_)
                | Verb::MassCancel { .. }
                | Verb::MountStrategy { .. }
                | Verb::UnmountStrategy { .. }
                | Verb::SetSetting { .. }
                | Verb::ReconcileConfirm { .. }
                | Verb::ReconcileCancel { .. }
                | Verb::ResumeVenue { .. }
                | Verb::SetAccountState { .. }
                | Verb::UnadmitSymbol { .. }
                | Verb::UnlatchMount { .. }
        )
    }

    /// The [`CommandWire`] a WRITE verb sends, or `None` for a READ/meta verb. PURE — the one
    /// construction site both the REPL and the MCP write tools resolve through.
    pub fn to_wire_command(&self) -> Option<CommandWire> {
        match self {
            Verb::Submit(o) => Some(CommandWire::Submit(o.clone())),
            Verb::Bracket(b) => Some(CommandWire::Bracket(b.clone())),
            Verb::OrderList(l) => Some(CommandWire::OrderList(l.clone())),
            Verb::Combo(c) => Some(CommandWire::Combo(c.clone())),
            Verb::Cancel(order_client_id) => Some(CommandWire::Cancel(order_client_id.clone())),
            Verb::Modify { order_client_id, new_qty, new_price, new_trigger_price } => {
                Some(CommandWire::Modify {
                    order_client_id: order_client_id.clone(),
                    new_qty: *new_qty,
                    new_price: *new_price,
                    new_trigger_price: *new_trigger_price,
                })
            }
            Verb::Flatten { venue, symbol, account } => Some(CommandWire::Flatten {
                venue: venue.clone(),
                symbol: symbol.clone(),
                account: account.clone(),
            }),
            Verb::MarketExit { venue, account } => {
                Some(CommandWire::MarketExit { venue: venue.clone(), account: account.clone() })
            }
            Verb::SetState(s) => Some(CommandWire::SetTradingState(*s)),
            Verb::MassCancel { venue, symbol, account } => Some(CommandWire::MassCancel {
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
                file,
                params,
            } => {
                Some(CommandWire::MountStrategy {
                    venue: venue.clone(),
                    account: account.clone(),
                    symbol: symbol.clone(),
                    interval: interval.clone(),
                    controller_id: controller_id.clone(),
                    file: file.clone(),
                    // Retired: the node refuses either with the fix, and no client sends one.
                    name: None,
                    rhai: None,
                    params: params.clone(),
                })
            }
            Verb::UnmountStrategy { controller_id } => {
                Some(CommandWire::UnmountStrategy { controller_id: controller_id.clone() })
            }
            Verb::ReconcileConfirm { id } => Some(CommandWire::ReconcileConfirm { id: *id }),
            Verb::ReconcileCancel { id } => Some(CommandWire::ReconcileCancel { id: *id }),
            Verb::ResumeVenue { venue, force } => {
                Some(CommandWire::ResumeVenue { venue: venue.clone(), force: *force })
            }
            Verb::SetAccountState { venue, account, state } => {
                Some(CommandWire::SetAccountTradingState {
                    venue: venue.clone(),
                    account: account.clone(),
                    state: *state,
                })
            }
            Verb::UnadmitSymbol { venue, account, symbol } => Some(CommandWire::UnadmitSymbol {
                venue: venue.clone(),
                account: account.clone(),
                symbol: symbol.clone(),
            }),
            Verb::UnlatchMount { mount_id } => {
                Some(CommandWire::UnlatchMount { mount_id: mount_id.clone() })
            }
            // The two file-era wire fields, filled HERE and nowhere else: `file` is the key's own
            // section word (the released v0.1.35 daemon requires the field, and ignores its
            // content), and `confirm` is `None` (the node has ignored it since
            // `docs/decisions/0086` point 7). Deleting both from the wire is step 2, after a
            // tolerant daemon is released.
            Verb::SetSetting { key, value } => Some(CommandWire::SetSetting {
                file: section_of_key(key).to_string(),
                key: key.clone(),
                value: value.clone(),
                confirm: None,
            }),
            _ => None,
        }
    }
}

/// The SECTION a dotted settings key names — its first segment (`policy`, `config`, `preferences`,
/// `flags`), the word the node derives the section from and the value every client in this crate
/// puts in the wire's `file` ([`Verb::to_wire_command`]).
///
/// Deliberately a plain split, not `vike_config::SettingsSection::of_key`: that answers `None` for
/// a key naming no known section, and the wire field still needs a value for the released v0.1.35
/// daemon to decode — which then refuses the unknown key itself, in its own loader's words. A
/// client copy of the section vocabulary would be a second authority on a question the node
/// already answers. PURE.
pub fn section_of_key(key: &str) -> &str {
    key.split('.').next().unwrap_or(key)
}

/// Why a `Submit` cannot be SIZED at all, or `None` when it can — the one refusal the REPL's
/// `parse_submit`, the one-shot verb's `parse_submit_args` and the MCP tool dialect's
/// `verb_from_tool_args` (its `submit_order` arm) all make, at PARSE time, before a preview or a
/// node connection exists.
/// PURE, and the returned text is the REASON alone: each path puts its own prefix on it (`submit: `
/// at the two command lines, `submit_order: ` on the tool), so the three say the same thing in the
/// vocabulary their reader typed.
///
/// What it refuses, in the order it is checked:
///
/// - a **non-finite `qty`** (NaN or ±infinity) — there is no size to compare against any cap;
/// - a **`qty` of zero or below** — the core's risk gate refuses it (`non-positive-size`), so the
///   order could only ever be a round trip to a refusal;
/// - a **non-finite `price`**, when one is given — `price` is `None` for a market order;
/// - a **non-finite `trigger_price`**, when one is given (a stop's trigger rides there; only the
///   one-shot verb's `--trigger` can spell a non-finite one, since JSON cannot);
/// - a **`limit` order with no `price`** — there is nothing to rest. Read through
///   `OrderType::parse` (trimmed, ASCII-case-insensitive), the door the node lowers the wire word
///   through, so `LIMIT` is a limit here exactly as it is there. Only the MCP tool can build one: the two command-line grammars make a limit BY
///   naming its `@price`;
/// - a **`market` order carrying a `price`** — the venue fills it at the market, so the number prices
///   nothing and [`stated_size`] does not read it: an author who typed one meant something else. Only
///   the MCP tool can build one too, and its omitted `order_type` is `market`, so that spelling is
///   refused here as well;
/// - a **`market` order carrying a `trigger_price`** — the same hole at the node: its door and the
///   core's `RiskGate` size it at the trigger (`fixtures/r5/risk.json` case 133 freezes that, so the
///   gate stays), while Binance ignores the trigger and fills at the market, so `qty 10, trigger_price
///   1` walks past a notional ceiling. Again only the MCP tool can build one, spelled or by default:
///   the two command-line grammars take `--trigger` only with `--type`, which makes the order a stop,
///   a stop-limit or a take-profit (`vike-cli`'s `trigger_order`). Those kinds keep their trigger;
/// - a **`market_to_limit` order carrying a `trigger_price`** — a market order until its first fill, so the
///   same hole; only the MCP tool can build one (`vike-cli`'s `trigger_order` refuses `--trigger` with it).
///
/// ⚠ **What it deliberately does NOT refuse**, because each is the node's judgement rather than a
/// property of the number: an order OVER any cap (a cap is the write's to judge, after the guardrail
/// measured the order on the node's frame), a stop or take-profit with a price or a trigger and no price,
/// `reduce_only`, and a **zero or negative `price`**. The last was measured, not assumed: the core
/// risk gate checks only the SIZE for positivity, and a negative price is meaningful on a
/// venue's combo net, so refusing it here could refuse something the node accepts. Until the node
/// refuses a zero price outright, this does not either.
///
/// It keys on the qty and price the verb will SEND, so the guardrail and the wire never disagree
/// about which order was judged.
pub fn unsizeable_submit_refusal(o: &OrderRequestWire) -> Option<String> {
    if !o.qty.is_finite() {
        return Some(format!("qty must be a finite number, got {}", o.qty));
    }
    if o.qty <= 0.0 {
        return Some("qty must be > 0".to_string());
    }
    if let Some(p) = o.price
        && !p.is_finite()
    {
        return Some(format!("price must be a finite number, got {p}"));
    }
    if let Some(t) = o.trigger_price
        && !t.is_finite()
    {
        return Some(format!("trigger price must be a finite number, got {t}"));
    }
    if o.price.is_none() && OrderType::parse(&o.order_type) == Ok(OrderType::Limit) {
        return Some("a limit order needs a price".to_string());
    }
    if OrderType::parse(&o.order_type) == Ok(OrderType::Market) {
        if o.price.is_some() {
            return Some(
                "a market order takes no price, it fills at the market: drop the price, or use a \
                 limit order"
                    .to_string(),
            );
        }
        if o.trigger_price.is_some() {
            return Some(
                "a market order takes no trigger price, it fills at the market: drop the trigger \
                 price, or use a stop or take-profit order"
                    .to_string(),
            );
        }
    }
    // A market-to-limit is a market order until its first fill, so a trigger price is the same hole (its
    // stated price is the order-terms rule's).
    if OrderType::parse(&o.order_type) == Ok(OrderType::MarketToLimit) && o.trigger_price.is_some()
    {
        return Some(
            "a market_to_limit order takes no trigger price, it fills at the market first: drop \
             the trigger price"
                .to_string(),
        );
    }
    None
}

/// Why a sizeable `Submit`'s TERMS cannot be sent, or `None`: the one rule the REPL's `parse_submit`, the one-shot
/// `trade order submit` and the MCP tool's `submit_order` arm all judge a built submit by, after
/// [`unsizeable_submit_refusal`]. Each is the `vike_model` rule the node's door applies again, read off the wire
/// words (`OrderType::parse`, `TimeInForce::parse`, the peg's anchor exactly as [`peg_of_word`] wrote it), and they
/// bite in this order, [`SubmitRuleRefusal`]'s variants':
///
/// - a `trail` on a kind that does not trail ([`SubmitRuleRefusal::TrailOffKind`]: the node's door refuses it too,
///   and its paper book would lower it as a trailing stop);
/// - its prices for its kind, `vike_model::order_terms_refusal` (a trigger, a trail, a limit offset, a peg);
/// - its display, its attributes and a stop's adjustment (`display_refusal`, `attributes_refusal`,
///   `stop_adjust_refusal`);
/// - its lifetime and a `gtd` expiry, then its release moment with that lifetime (`lifetime_refusal`, twice).
///
/// Each surface keeps only its dialect: how it READS its flags or arguments into the wire, and the prefix it shows
/// the refusal under. PURE but for the clock (a `gtd` expiry and a release must be in the future).
pub fn submit_rules_refusal(o: &OrderRequestWire) -> Option<SubmitRuleRefusal> {
    use vike_model::{OrderPeg, OrderPegAnchor, OrderRelease, TimeInForce};
    let kind = OrderType::parse(&o.order_type).unwrap_or_default();
    if let Some(trail) = o.trail.filter(|_| !kind.is_trailing()) {
        return Some(SubmitRuleRefusal::TrailOffKind { kind: kind.to_string(), trail });
    }
    let peg = o.peg.as_ref().and_then(|p| {
        OrderPegAnchor::parse(&p.anchor).map(|anchor| OrderPeg { anchor, offset: p.offset })
    });
    let tif =
        o.time_in_force.as_deref().and_then(|w| TimeInForce::parse(w).ok()).unwrap_or_default();
    if let Some(why) = vike_model::order_terms_refusal(
        &kind,
        o.price,
        o.trigger_price,
        o.trail,
        o.limit_offset,
        peg.as_ref(),
        o.trail_percent,
    ) {
        return Some(SubmitRuleRefusal::Terms { why });
    }
    if let Some(why) = vike_model::display_refusal(&kind, o.qty, o.display_quantity) {
        return Some(SubmitRuleRefusal::Display { why });
    }
    if let Some(why) = vike_model::attributes_refusal(&kind, tif, o.attributes.as_ref()) {
        return Some(SubmitRuleRefusal::Attributes { why });
    }
    if let Some(why) =
        vike_model::stop_adjust_refusal(&kind, o.side, o.trigger_price, o.adjust.as_ref())
    {
        return Some(SubmitRuleRefusal::StopAdjust { why });
    }
    let now = vike_model::now_ms();
    if let Some(why) = vike_model::lifetime_refusal(&kind, tif, o.gtd_expiry, None, now) {
        return Some(SubmitRuleRefusal::Lifetime { why });
    }
    // The rule judges the moment; the price condition's word was read where the door read it.
    let release = o.release.as_ref().map(|r| OrderRelease { at: r.at, price: None });
    vike_model::lifetime_refusal(&kind, tif, o.gtd_expiry, release, now)
        .map(|why| SubmitRuleRefusal::Release { why })
}

/// A pegged order's wire peg from an operator's anchor WORD, in any case and spaces trimmed (`MID` is `mid`), and
/// its offset: the anchor goes out as the model's own word, the one the node's door reads exactly. The ONE reader the
/// CLI's `--peg` and the MCP tool's `peg` share. Whether the order's kind takes a peg is [`submit_rules_refusal`]'s.
/// PURE.
pub fn peg_of_word(word: &str, offset: f64) -> Result<OrderPegWire, SubmitRuleRefusal> {
    let anchor =
        vike_model::OrderPegAnchor::parse(&word.trim().to_ascii_lowercase()).ok_or_else(|| {
            SubmitRuleRefusal::PegAnchor {
                word: word.to_string(),
                accepted: vike_model::OrderPegAnchor::accepted_words(),
            }
        })?;
    Ok(OrderPegWire { anchor: anchor.as_word().to_string(), offset })
}

/// The one MODIFY builder both doors call (the REPL's `parse_modify`, the one-shot `trade order modify`, the MCP tool
/// `modify_order`): a modify that changes nothing is refused ([`ModifyRefusal::NothingToChange`]), never sent. Each
/// door reads its own numbers (and refuses a non-finite one in its own words) first. PURE.
pub fn modify_of(
    order_client_id: String,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    new_trigger_price: Option<f64>,
) -> Result<Verb, ModifyRefusal> {
    if new_qty.is_none() && new_price.is_none() && new_trigger_price.is_none() {
        return Err(ModifyRefusal::NothingToChange);
    }
    Ok(Verb::Modify { order_client_id, new_qty, new_price, new_trigger_price })
}

/// An operator's PRICE CONDITION word for a held order's release, `above:<price>` or `below:<price>`, or with the
/// instrument whose price it reads, `above:<SYMBOL>:<price>` (the side in any case, spaces trimmed), into the wire's
/// [`OrderReleasePriceWire`] and [`vike_tradehub_client::wire::OrderReleaseWire::symbol`] (`None`: the order's own symbol):
/// released once that price is at or above, or at or below, the level. The ONE reader the CLI's `--when-price` and
/// the MCP tool's `release_when_price` share, so the two cannot accept different words. The level is a price:
/// finite and above zero; the symbol is the node's spelling, which its door holds to the engine's list. PURE.
pub fn release_price_from_word(
    word: &str,
) -> Result<(OrderReleasePriceWire, Option<String>), String> {
    let refuse = || {
        format!(
            "{word:?} is not a price condition: write above:<price> or below:<price>, e.g. above:100, \
             or name the instrument, e.g. above:ETHUSDT:3500"
        )
    };
    let (side, rest) = word.trim().split_once(':').ok_or_else(refuse)?;
    let (symbol, level) = match rest.rsplit_once(':') {
        None => (None, rest),
        Some((symbol, _)) if symbol.trim().is_empty() => return Err(refuse()),
        Some((symbol, level)) => (Some(symbol.trim().to_string()), level),
    };
    let px: f64 = level.trim().parse().map_err(|_| refuse())?;
    if !px.is_finite() || px <= 0.0 {
        return Err(format!("the price condition's level {px} is not a price above zero"));
    }
    let price = match side.trim().to_ascii_lowercase().as_str() {
        "above" => OrderReleasePriceWire::Above(px),
        "below" => OrderReleasePriceWire::Below(px),
        _ => return Err(refuse()),
    };
    Ok((price, symbol))
}

/// The VENUE a release's price condition reads (decision 0267's 2.13: `--when-price-on`, the MCP tool's
/// `release_when_price_on`), checked against the condition it rides: a roster id exactly
/// (`vike_model::price_source_refusal`), and only beside a condition that NAMES its symbol (`symbol`, the second
/// half of [`release_price_from_word`]'s answer), since the venues spell one instrument differently. The node's door
/// applies the same rules again and asks whether it can read that price. The ONE check the CLI and the MCP tool
/// share. PURE.
pub fn release_venue_from_word(venue: &str, symbol: Option<&str>) -> Result<String, String> {
    if let Some(why) = vike_model::price_source_refusal(venue) {
        return Err(why);
    }
    if symbol.is_none() {
        return Err(format!(
            "a price read on `{venue}` names its symbol, e.g. above:BTCUSDT:100000 (the venues spell one \
             instrument differently)"
        ));
    }
    Ok(venue.to_string())
}

/// A stop's ADJUSTMENT from an operator's terms (`--adjust-at` / `--adjust-to` and its prices, the MCP tool's
/// `adjust_*` arguments): once the price reaches `at`, the order becomes the target `to` names, `stop`,
/// `stop-limit` (or `stop_limit`) or `trailing`, any case. The target's stop is required; a stop-limit target takes
/// `limit`, a trailing one `trail` or `trail_percent`, and nothing takes a term of another target. Whether the
/// numbers hold (`at` on the protected side, the target's stop on the near side of it, one trail) is
/// `vike_model::stop_adjust_refusal`'s, which the caller applies next and the node's door again. The ONE reader the CLI
/// and the MCP tool share. PURE.
pub fn stop_adjust_from_terms(
    at: f64,
    to: &str,
    stop: Option<f64>,
    limit: Option<f64>,
    trail: Option<f64>,
    trail_percent: Option<f64>,
) -> Result<OrderStopAdjust, String> {
    let stop =
        stop.ok_or_else(|| format!("an adjustment to `{to}` needs its target's stop price"))?;
    let word = to.trim().to_ascii_lowercase();
    let stray = |what: &str| format!("an adjustment to `{to}` takes no {what}");
    let to = match word.as_str() {
        "stop" | "stop-limit" | "stop_limit" if trail.is_some() || trail_percent.is_some() => {
            return Err(stray("trail"));
        }
        "stop" if limit.is_some() => return Err(stray("limit price")),
        "stop" => OrderStopAdjustTo::Stop { stop },
        "stop-limit" | "stop_limit" => OrderStopAdjustTo::StopLimit {
            stop,
            limit: limit.ok_or_else(|| format!("an adjustment to `{to}` needs its limit price"))?,
        },
        "trailing" if limit.is_some() => return Err(stray("limit price")),
        "trailing" => OrderStopAdjustTo::Trailing { stop, trail, trail_percent },
        _ => {
            return Err(format!(
                "`{to}` is not an adjustment target: write stop, stop-limit or trailing"
            ));
        }
    };
    Ok(OrderStopAdjust { at, to })
}

/// The result of the ONE client-side advisory guardrail check (see [`guardrail_check`]). Rendered
/// as JSON for the MCP preview payload ([`Guardrail::to_json`]) and as a line for the REPL preview
/// ([`Guardrail::line`]) — same semantics, two skins, so the two surfaces can never drift again.
#[derive(Debug, Clone, PartialEq)]
pub enum Guardrail {
    /// A verb that states a size: a `Submit`, a `Bracket`, a `Modify` naming a new qty.
    Order {
        qty: f64,
        /// `None` = NOT PRICED, and `unpriced` says why: the contract's value is not known here, or a
        /// linear contract's order carries no price.
        notional: Option<f64>,
        max_qty: Option<f64>,
        max_notional: Option<f64>,
        /// `None` = NOT CHECKED: an order that is not priced never reads as within limits. It is still
        /// `Some(false)` when the quantity alone breaches its cap.
        within_limits: Option<bool>,
        /// Why `notional` is `None`.
        unpriced: Option<String>,
    },
    /// Every other verb, and an order the node takes for a close, which it never sizes: trivially within limits
    /// (no order size to check).
    NoSize,
}

/// The two client-side advisory caps, resolved ONCE per process by [`guardrail_capabilities`] and carried
/// by each surface (the REPL's `Session`, the MCP `Server`). `Copy` so passing it costs nothing.
///
/// They come from different sections for a reason — see this module's doc: `max_notional` is a
/// POLICY ceiling, `max_qty` a local convenience PREFERENCE.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GuardrailCapabilities {
    /// The `preferences.max_order_qty` row of this machine's settings database. `None` = no qty
    /// cap.
    pub max_qty: Option<f64>,
    /// The `policy.max_notional_per_order` row of this machine's settings database. `None` = no
    /// notional cap.
    pub max_notional: Option<f64>,
}

/// Resolve the advisory caps: the POLICY ceiling the caller already loaded, plus the
/// `preferences.max_order_qty` row the boot installed ([`install_max_order_quantity_row`]) — finite and
/// above 0, or absent, which the loader enforces. Read here rather than in each surface so there
/// is exactly one read of it.
pub fn guardrail_capabilities(policy_max_notional: Option<f64>) -> GuardrailCapabilities {
    GuardrailCapabilities {
        max_qty: MAX_ORDER_QUANTITY_ROW.get().copied().flatten(),
        max_notional: policy_max_notional,
    }
}

/// The `preferences.max_order_qty` row of this machine's settings database, as the ONE boot walk
/// resolved it (`crates/vike-client/src/resolved.rs` installs it). A process-wide cell rather than another
/// parameter threaded through every `trade` and `vike-mcp` verb that already carries the policy ceiling:
/// it is a property of the machine, read at most once per surface.
static MAX_ORDER_QUANTITY_ROW: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();

/// Install the `preferences.max_order_qty` row — once, at boot. A second call is ignored.
pub fn install_max_order_quantity_row(row: Option<f64>) {
    let _ = MAX_ORDER_QUANTITY_ROW.set(row);
}

/// A wait spelled in SECONDS for the sentence "the node has not answered within …": whole seconds
/// as `2s`, anything finer with its fraction and the trailing zeros trimmed (`0.5s`, `2.25s`).
///
/// The ack wait is a row now (`config.node_ack_wait_ms`, from half a second up), and
/// `Duration::as_secs` — which both surfaces used while the wait was a two-second constant — prints
/// `0s` for half a second, which would tell an operator the node was given no time at all. At the
/// default wait the text is `2s`, byte for byte what it always said. The CLI and the `mcp` write
/// tools say a run-out ack wait in `vike_tradehub_client::UnansweredCommand`'s words instead.
pub fn wait_seconds_text(wait: Duration) -> String {
    let millis = wait.as_millis();
    let (secs, fraction) = (millis / 1_000, millis % 1_000);
    if fraction == 0 {
        return format!("{secs}s");
    }
    let fraction = format!("{fraction:03}");
    format!("{secs}.{}s", fraction.trim_end_matches('0'))
}

/// The `(qty, price)` a verb states, or `None` for a verb with no size. A `Submit` is sized at
/// `OrderRequestWire::own_price`, the price the node's door sizes it at and the core's `RiskGate`
/// reads: a market order's named price is no price, and a stop or take-profit is sized at its
/// trigger, so the guardrail cannot say `within limits` of an order the node refuses. A bracket
/// is ONE qty over three priced legs and the door caps each priced leg as a `Submit` at that price,
/// so its largest leg is the one that can breach. A `Modify` names a size only with a new qty, and the
/// door sizes that at the price it names; `crate::addressed::guardrail_of` fills a qty or a price it leaves out from
/// the resting order first.
///
/// `closes` says the door takes the order for a CLOSE (`crate::addressed`'s `closes`), which it never
/// sizes: such an order states no size this guardrail could hold against a cap. `value` picks an order
/// list's largest leg by the door's own notional (the default value when the contract's is unknown, for
/// an order that is then not checked anyway).
fn stated_size(
    cmd: &CommandWire,
    closes: bool,
    value: ContractValue,
) -> Option<(f64, Option<f64>)> {
    if closes {
        return None;
    }
    match cmd {
        CommandWire::Submit(o) => Some((o.qty, o.own_price())),
        CommandWire::Bracket(b) => Some((
            b.qty,
            b.entry_price
                .into_iter()
                .chain([b.stop_loss, b.take_profit])
                .map(f64::abs)
                .reduce(f64::max),
        )),
        // A modify naming a trigger and no price is sized at the trigger, as the door sizes it.
        CommandWire::Modify { new_qty: Some(qty), new_price, new_trigger_price, .. } => {
            Some((*qty, new_price.or(*new_trigger_price)))
        }
        // An order list: the door caps each priced leg as a `Submit` at its own price, so the leg with the
        // largest notional (`value`'s `order_notional`, as the door values it: on an inverse contract the
        // largest qty, whatever its price) is the one that can breach; with no priced leg, the largest qty,
        // unpriced.
        CommandWire::OrderList(l) => {
            let own = |leg: &OrderListLegWire| {
                let market = leg.order_type.trim().eq_ignore_ascii_case("market");
                if market { None } else { leg.price }.or(leg.trigger_price)
            };
            let priced = l
                .legs
                .iter()
                .filter_map(|leg| Some((leg.qty, own(leg)?)))
                .max_by(|a, b| {
                    value.order_notional(a.0, a.1).total_cmp(&value.order_notional(b.0, b.1))
                })
                .map(|(qty, price)| (qty, Some(price)));
            priced.or_else(|| l.legs.iter().map(|leg| leg.qty).reduce(f64::max).map(|q| (q, None)))
        }
        // A combo: each leg is `|ratio| × qty` of its own instrument and no one contract value sizes them all, so
        // it states its largest leg's size, unpriced (the door sizes every leg on its own instrument).
        CommandWire::Combo(c) => c
            .legs
            .iter()
            .map(|leg| c.qty * f64::from(leg.ratio.unsigned_abs()))
            .reduce(f64::max)
            .map(|q| (q, None)),
        _ => None,
    }
}

/// Whether an order list OPENS with a market leg, the leg the node's door sizes at the price it holds.
fn opens_at_market(l: &OrderListWire) -> bool {
    l.legs.iter().any(|leg| {
        !leg.reduce_only
            && leg.parent.is_none()
            && leg.order_type.trim().eq_ignore_ascii_case("market")
    })
}

/// Whether [`guardrail_check`] has a size to judge, so a surface asks for a contract value only then:
/// a verb that reduces risk does not wait on a read it has no use for. Every modify does: what it names is completed
/// from the resting order on the node's frame (`crate::addressed::guardrail_of`), so a price-only one is sized too.
pub fn states_a_size(cmd: &CommandWire) -> bool {
    matches!(cmd, CommandWire::Modify { .. })
        || stated_size(cmd, false, ContractValue::default()).is_some()
}

/// A client-side advisory guardrail check over [`GuardrailCapabilities`] (the node's own server-side
/// `ControlLimits` is what actually enforces). A verb with no stated size is trivially within
/// limits. PURE — the caps arrive resolved, and so does `value`, what one unit of the quantity is
/// worth, or why that is not known (`crate::addressed::contract_value_of`), and `closes`, whether the
/// node takes it for a close. A surface holding the node's frame calls `crate::addressed::guardrail_of`.
///
/// ⚠ **The notional is `ContractValue::order_notional`, the function the node's door and the core's
/// `RiskGate` call**, and never `qty × price`: that read a true $10 inverse `BTC-PERPETUAL` limit as
/// 381,235, and, the dangerous way round, 200,000 USD of it at a price of 0.1 as 20,000 and within a
/// ceiling of 100,000 the node refuses it under. An inverse contract is sized with no price at all, as
/// the door sizes it, so a market order on one is priced.
///
/// ⚠ **An order this cannot price is NOT CHECKED, and never reads as within limits**: the notional
/// and the verdict are `None` and `unpriced` says why. Only the qty cap can still answer, and then
/// only `false`. It judges SIZE against the caps and nothing else: an order with no size at all never
/// reaches it from the REPL, the one-shot verb or the MCP tool, because [`unsizeable_submit_refusal`]
/// refuses that at parse.
pub fn guardrail_check(
    cmd: &CommandWire,
    capabilities: GuardrailCapabilities,
    value: Result<ContractValue, String>,
    closes: bool,
) -> Guardrail {
    let Some((qty, price)) = stated_size(cmd, closes, value.as_ref().copied().unwrap_or_default())
    else {
        return Guardrail::NoSize;
    };
    // The leg the door sizes at the price it holds, which is not on this side of the wire: its exits can say such a
    // bracket is over a cap, and never that it is within one.
    let market_entry = match cmd {
        CommandWire::Bracket(b) => b.entry_price.is_none(),
        CommandWire::OrderList(l) => opens_at_market(l),
        _ => false,
    };
    guardrail_of_size(qty, price, market_entry, capabilities, value)
}

/// [`guardrail_check`]'s verdict on ONE stated size: `qty` at `price` (none for an order with no price of its own),
/// `market_entry` when the order opens at a market price this side cannot see. `crate::addressed` hands it a combo's
/// largest leg, valued on that leg's own instrument.
pub(crate) fn guardrail_of_size(
    qty: f64,
    price: Option<f64>,
    market_entry: bool,
    capabilities: GuardrailCapabilities,
    value: Result<ContractValue, String>,
) -> Guardrail {
    let GuardrailCapabilities { max_qty, max_notional } = capabilities;
    let sized = value.and_then(|value| match (value.kind, price) {
        // The price never enters an inverse notional, whichever one the order names.
        (NotionalKind::Inverse, _) => Ok(value.order_notional(qty, 0.0)),
        (NotionalKind::Linear, Some(price))
            if market_entry
                && max_notional.is_none_or(|max| value.order_notional(qty, price) <= max) =>
        {
            Err("its market entry carries no price, and this contract's value counts the price"
                .to_string())
        }
        (NotionalKind::Linear, Some(price)) => Ok(value.order_notional(qty, price)),
        (NotionalKind::Linear, None) => {
            Err("it carries no price, and this contract's value counts the price".to_string())
        }
    });
    let (notional, unpriced) = match sized {
        Ok(notional) => (Some(notional), None),
        Err(why) => (None, Some(why)),
    };
    let within_limits = if max_qty.is_some_and(|m| qty.abs() > m) {
        Some(false)
    } else {
        notional.map(|n| max_notional.is_none_or(|m| n <= m))
    };
    Guardrail::Order { qty: qty.abs(), notional, max_qty, max_notional, within_limits, unpriced }
}

impl Guardrail {
    /// The MCP preview payload. `within_limits` is `null`, never `false` or `true`, for an order that
    /// was not checked, and only that order carries `unpriced`, so every other answer keeps its keys.
    pub fn to_json(&self) -> Value {
        match self {
            Guardrail::Order { qty, notional, max_qty, max_notional, within_limits, unpriced } => {
                let mut fields = json!({
                    "qty": qty, "notional": notional,
                    "max_qty": max_qty, "max_notional": max_notional,
                    "within_limits": within_limits
                });
                if let Some(why) = unpriced {
                    fields["unpriced"] = json!(why);
                }
                fields
            }
            Guardrail::NoSize => {
                json!({ "within_limits": true, "note": "no order size to check for this verb" })
            }
        }
    }

    /// The REPL preview line.
    pub fn line(&self) -> String {
        match self {
            Guardrail::Order { qty, notional, max_qty, max_notional, within_limits, unpriced } => {
                format!(
                    "guardrail: qty={} notional={} max_qty={} max_notional={} → {}",
                    format_number(*qty),
                    // The notional is the one number that is COMPARED against a ceiling, so it gets
                    // the floor-aware renderer: it may never be shortened to a string that reads as
                    // at-or-below the cap it actually exceeds. See [`fmt_number_over`].
                    fmt_opt_over(notional, *max_notional),
                    fmt_opt(max_qty),
                    fmt_opt(max_notional),
                    match within_limits {
                        Some(true) => "within limits".to_string(),
                        // These caps are this box's, and the node holds its own or none, so this
                        // promises no rejection.
                        Some(false) => {
                            "OVER LIMIT (this box's own caps; the node's verdict is the one that \
                             counts)"
                                .to_string()
                        }
                        None => format!(
                            "not checked: {}; the node's verdict is the one that counts",
                            unpriced.as_deref().unwrap_or("not priced")
                        ),
                    },
                )
            }
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
/// (`crates/vike-cli/src/cmd/config/set.rs`'s `report_lines`). A REMOTE write's old value lives on
/// the node, so each surface reads it there — the node's `SettingsShow`, the effective row
/// `settings_show` serves — and hands the answer to [`SettingChange::from_show`]. This type is the
/// PURE half: what was read in, what is shown out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChange {
    /// The full dotted key being written.
    pub key: String,
    /// What the node runs with for `key` right now — or why that could not be read.
    pub old: OldSetting,
    /// The value as it will be sent: text, which the node parses as TOML.
    pub new: String,
}

/// The OLD half of a [`SettingChange`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OldSetting {
    /// The node's EFFECTIVE value as its `SettingsShow` renders it (`""` = unset), and the layer
    /// that set it — `SettingsShowRowWire::origin`: `default`, `db`, or `env:<VAR>`.
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
    pub fn from_show(key: &str, new: &str, show: Result<&SettingsShowWire, String>) -> Self {
        let old = match show {
            // The rows beside a refusal may be compiled-in defaults, not what the node holds.
            Ok(SettingsShowWire { refusal: Some(why), .. }) => OldSetting::Unread(why.clone()),
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
    pub fn line(&self) -> String {
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
    pub fn env_shadow(&self) -> Option<&str> {
        match &self.old {
            OldSetting::Read { origin, .. } if origin.starts_with("env:") => Some(origin),
            _ => None,
        }
    }

    /// The `mcp` preview's `change` object: the same facts, typed. `old` is null — never `""`, which
    /// would read as "unset" — whenever no old value was read, and `old_unread` then says why.
    pub fn to_json(&self) -> Value {
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

/// [`format_number`], but never rendered so coarsely that it reads as at-or-below `floor`.
///
/// The case this exists for: a notional of `42.500000000000004` against a `max_notional` of `42.5`
/// is genuinely OVER the ceiling, and [`format_number`] would shorten it to `42.5` — leaving a line that
/// says `notional=42.5 max_notional=42.5 → OVER LIMIT`, i.e. a verdict its own numbers appear to
/// contradict. The display may drop noise; it may not understate a value against the ceiling it is
/// being judged by. When shortening would cross the cap, the exact value is printed instead — ugly,
/// and correct, which is the right way round for a number about to become an order.
fn fmt_number_over(v: f64, floor: Option<f64>) -> String {
    let rendered = format_number(v);
    match floor {
        Some(cap) if v > cap && rendered.parse::<f64>().is_ok_and(|back| back <= cap) => {
            v.to_string()
        }
        _ => rendered,
    }
}

/// [`format_number`] over an `Option`, with `-` for "no value" (no cap set, or a market order with no
/// price to size).
fn fmt_opt(v: &Option<f64>) -> String {
    v.map(format_number).unwrap_or_else(|| "-".to_string())
}

/// [`fmt_number_over`] over an `Option`.
fn fmt_opt_over(v: &Option<f64>, floor: Option<f64>) -> String {
    v.map(|n| fmt_number_over(n, floor)).unwrap_or_else(|| "-".to_string())
}

/// The advisory order-client-id charset check, shared by every write surface.
///
/// `Some(warning)` when a command names an `order_client_id` that
/// [`vike_model::is_valid_crypto_order_client_id`] rejects — the `^[A-Za-z0-9]{1,32}$` charset the REPL's own
/// `submit --order-client-id` REFUSES and the one every minted id satisfies.
///
/// # Why `cancel` WARNS where `submit` REFUSES
///
/// The asymmetry a clean install found was real: `submit --order-client-id MYPINNED-001` was rejected with a
/// good message while `cancel MYPINNED-001` — for an order that never existed — answered "accepted
/// by the node". Two different answers about the same field is a trap, so both surfaces speak about
/// the charset now. They do not speak with the same force, and that is deliberate:
///
/// - **`submit` can refuse safely.** It is MINTING an id, so nothing is lost by insisting the id be
///   one every venue accepts. Refusing costs the operator one retype.
/// - **`cancel`/`modify` must not refuse.** They name an id that ALREADY EXISTS at the node, and
///   the two doors are not the only things that put orders there: the desktop, Telegram, a strategy and a build of
///   either door from before [`order_client_id_refusal`] place orders too, and the node's own `lower_command` requires
///   only that the id be non-empty. So an order with an order client id outside this charset CAN exist — and refusing to cancel it
///   would make a live order uncancellable from the operator's console to enforce a client-side
///   convention. That trade is unacceptable in a tool whose reason to exist includes flattening
///   something in a hurry.
///
/// So: say the id could not have been minted here (which is genuinely useful — it is almost always a
/// typo), and send it anyway. Cancel stays fire-and-forget, which is honest: the node cannot confirm
/// that an order EXISTS either way.
pub fn order_client_id_charset_warning(cmd: &CommandWire) -> Option<String> {
    let order_client_id = match cmd {
        CommandWire::Cancel(order_client_id) => order_client_id,
        CommandWire::Modify { order_client_id, .. } => order_client_id,
        // A `Submit` reaching here has already been through `parse_submit`'s refusal (an explicit
        // `--order-client-id`) or `fill_order_client_id`'s mint, both of which guarantee the charset.
        _ => return None,
    };
    (!vike_model::is_valid_crypto_order_client_id(order_client_id)).then(|| {
        format!(
            "note: {order_client_id:?} is not an order_client_id this node could have minted ({ORDER_CLIENT_ID_CHARSET}) \
             — sending it anyway, but check it: nothing will match, and a cancel that matches \
             nothing still reports as accepted"
        )
    })
}

/// The order-client-id charset, in one place, for every message and help screen that states it.
/// `vike_model::is_valid_crypto_order_client_id` is the enforcing authority; this is its prose.
pub const ORDER_CLIENT_ID_CHARSET: &str = "1..=32 alphanumeric characters, A-Z a-z 0-9";

/// **The one refusal of a caller-PINNED order client id outside [`ORDER_CLIENT_ID_CHARSET`]**, on both doors at parse
/// (D1 of 2026-11-24): a submit's, a combo's and every leg of an order list ([`with_leg_order_client_ids`] asks it).
/// A pinned id is being MINTED, so insisting on the charset every venue accepts costs one retype; an id a cancel or a
/// modify names already exists and is only warned of ([`order_client_id_charset_warning`]).
#[must_use]
pub fn order_client_id_refusal(id: &str) -> Option<crate::refusals::OrderGateRefusal> {
    (!vike_model::is_valid_crypto_order_client_id(id)).then(|| {
        crate::refusals::OrderGateRefusal::OrderClientIdCharset {
            id: id.to_string(),
            charset: ORDER_CLIENT_ID_CHARSET,
        }
    })
}

/// [`order_client_id_refusal`] for a bracket's pinned GROUP STEM (`OrderBracketWire::order_client_id`): the charset, then
/// the shape the core derives the three legs from (`OrderClientIdStem::given`), so a stem the core would refuse whole only
/// after the node's answer is refused at parse. A retry that names its first send's stem passes, and the node's door
/// refuses it whole when a leg of that send exists.
#[must_use]
pub fn bracket_stem_refusal(stem: &str) -> Option<crate::refusals::OrderGateRefusal> {
    order_client_id_refusal(stem).or_else(|| {
        vike_model::orders::client_id::OrderClientIdStem::given(stem)
            .is_none()
            .then(|| crate::refusals::OrderGateRefusal::BracketStemShape { id: stem.to_string() })
    })
}

// ---- the ORDER LIST verbs (wave 4 of the order kinds) ----------------------------------------------------------

/// What every order-list verb names first: the book (`venue` and the wire `account`, absent for a bare venue), the
/// symbol, the side (+1 buy / -1 sell) and the size.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderListHead {
    pub venue: String,
    pub account: Option<String>,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
}

/// `oco <account> <symbol> <buy|sell> <qty> --limit <px> (--stop <trigger> [--stop-limit <px>] | --trail <distance>)
/// [--reduce-only]`: TWO legs of one side and one size, a limit at `limit` and a stop at `stop` (a stop-limit at
/// `stop_limit` when it is named) or a TRAILING stop that keeps `trail` from the best price since it went live
/// (`OrderType::Trailing` with no activation), linked one-cancels-other: the first to fill cancels the other. With
/// `reduce_only`, an exit pair (a target above and a protective stop below a long); without, two entries. Refused
/// unless the second leg is named exactly one way ([`OrderListRefusal::OcoStopLeg`], rendered).
pub fn oco_list(
    head: OrderListHead,
    limit: f64,
    stop: Option<f64>,
    stop_limit: Option<f64>,
    trail: Option<f64>,
    reduce_only: bool,
) -> Result<OrderListWire, String> {
    let (stop_kind, stop_price, stop_trigger) = match (stop, stop_limit, trail) {
        (Some(trigger), None, None) => (OrderType::Stop, None, Some(trigger)),
        (Some(trigger), Some(at), None) => (OrderType::StopLimit, Some(at), Some(trigger)),
        (None, None, Some(_)) => (OrderType::Trailing, None, None),
        _ => return Err(OrderListRefusal::OcoStopLeg.rendered()),
    };
    let leg = |order_type, price, trigger_price, trail, linked| vike_model::OrderListLeg {
        side: head.side,
        qty: head.qty,
        order_type,
        price,
        trigger_price,
        trail,
        reduce_only,
        time_in_force: vike_model::TimeInForce::Gtc,
        parent: None,
        contingency: Some(vike_model::ContingencyType::Oco),
        linked: vec![linked],
        order_client_id: None,
    };
    let legs = vec![
        leg(OrderType::Limit, Some(limit), None, None, 1),
        leg(stop_kind, stop_price, stop_trigger, trail, 0),
    ];
    order_list_of(head, legs)
}

/// `oto <account> <symbol> <buy|sell> <qty> [@<price>|@market] (--stop <trigger> | --limit <px> | --trail <distance>)`:
/// an ENTRY (a limit at `entry`, a market order without one) and ONE exit of the same size on the other side,
/// reduce-only, held until the entry fills (one-triggers-other): a protective stop, a target limit, or a trailing stop
/// by `then_trail` (no activation: it trails from the moment the entry's fill sends it). Exactly one of the three
/// ([`OrderListRefusal::OtoOneExit`], rendered).
pub fn oto_list(
    head: OrderListHead,
    entry: Option<f64>,
    then_stop: Option<f64>,
    then_limit: Option<f64>,
    then_trail: Option<f64>,
) -> Result<OrderListWire, String> {
    let (kind, price, trigger_price) = match (then_stop, then_limit, then_trail) {
        (Some(trigger), None, None) => (OrderType::Stop, None, Some(trigger)),
        (None, Some(price), None) => (OrderType::Limit, Some(price), None),
        (None, None, Some(_)) => (OrderType::Trailing, None, None),
        _ => return Err(OrderListRefusal::OtoOneExit.rendered()),
    };
    let legs = vec![
        entry_leg(&head, entry),
        vike_model::OrderListLeg {
            side: -head.side,
            qty: head.qty,
            order_type: kind,
            price,
            trigger_price,
            trail: then_trail,
            reduce_only: true,
            time_in_force: vike_model::TimeInForce::Gtc,
            parent: Some(0),
            contingency: None,
            linked: Vec::new(),
            order_client_id: None,
        },
    ];
    order_list_of(head, legs)
}

/// `ladder <account> <symbol> <buy|sell> <qty> [@<price>|@market] (--stop-loss <trigger> | --trail <distance>)
/// --take-profit <px>:<qty>...`: an entry, ONE protective stop for its whole size (a stop at `stop_loss`, or a trailing
/// stop by `trail` with no activation) and the partial targets, `vike_model::build_ladder`'s list (the stop and each
/// target linked one-updates-other, so a target's fill shrinks the stop by what it filled). Exactly one of the two
/// ([`OrderListRefusal::LadderStopLeg`], rendered).
pub fn ladder_list(
    head: OrderListHead,
    entry: Option<f64>,
    stop_loss: Option<f64>,
    trail: Option<f64>,
    targets: &[(f64, f64)],
) -> Result<OrderListWire, String> {
    if stop_loss.is_some() == trail.is_some() {
        return Err(OrderListRefusal::LadderStopLeg.rendered());
    }
    let mut spec = vike_model::build_ladder(&vike_model::LadderSpec {
        venue: head.venue.clone(),
        symbol: head.symbol.clone(),
        side: head.side,
        qty: head.qty,
        entry_price: entry,
        // A trailing ladder's stop gets its terms below; the model's builder reads no trigger.
        stop_loss: stop_loss.unwrap_or_default(),
        targets: targets
            .iter()
            .map(|&(price, qty)| vike_model::LadderTarget { price, qty })
            .collect(),
        account: None,
    })?;
    if trail.is_some() {
        // Leg 1 is the model's stop for the whole size: it keeps its size and its links and trails instead.
        let stop = &mut spec.legs[1];
        (stop.order_type, stop.trigger_price, stop.trail) = (OrderType::Trailing, None, trail);
    }
    order_list_of(head, spec.legs)
}

/// The entry leg of an OTO or a ladder: a limit at `entry`, a market order without one.
fn entry_leg(head: &OrderListHead, entry: Option<f64>) -> vike_model::OrderListLeg {
    vike_model::OrderListLeg {
        side: head.side,
        qty: head.qty,
        order_type: if entry.is_some() { OrderType::Limit } else { OrderType::Market },
        price: entry,
        trigger_price: None,
        trail: None,
        reduce_only: false,
        time_in_force: vike_model::TimeInForce::Gtc,
        parent: None,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    }
}

/// The wire list of `legs`, refused in words first: a side other than ±1, a size, a price or a trail that is not a
/// finite number above zero (a non-finite one would go out as `null`), and links `vike_model::build_order_list` cannot
/// resolve, so the client says what the node's door would. The ids are placeholders; the core mints the real ones.
fn order_list_of(
    head: OrderListHead,
    legs: Vec<vike_model::OrderListLeg>,
) -> Result<OrderListWire, String> {
    if head.side != 1 && head.side != -1 {
        return Err(format!("side must be 1 (buy) or -1 (sell), got {}", head.side));
    }
    for (i, leg) in legs.iter().enumerate() {
        for (what, v) in [
            ("qty", Some(leg.qty)),
            ("price", leg.price),
            ("trigger", leg.trigger_price),
            ("trail", leg.trail),
        ] {
            if let Some(v) = v.filter(|v| !(v.is_finite() && *v > 0.0)) {
                return Err(format!(
                    "leg {i}'s {what} must be a finite number above zero, got {v}"
                ));
            }
        }
    }
    let spec = vike_model::OrderList {
        venue: head.venue.clone(),
        symbol: head.symbol.clone(),
        legs,
        account: None,
    };
    let placeholders: Vec<String> = (0..spec.legs.len()).map(|i| i.to_string()).collect();
    vike_model::build_order_list(&spec, &placeholders)?;
    Ok(OrderListWire {
        venue: head.venue,
        symbol: head.symbol,
        account: head.account,
        legs: spec
            .legs
            .into_iter()
            .map(|leg| OrderListLegWire {
                side: leg.side,
                qty: leg.qty,
                order_type: leg.order_type.as_str().to_owned(),
                price: leg.price,
                trigger_price: leg.trigger_price,
                trail: leg.trail,
                reduce_only: leg.reduce_only,
                time_in_force: (leg.time_in_force != vike_model::TimeInForce::Gtc)
                    .then(|| leg.time_in_force.as_word().to_owned()),
                parent: leg.parent,
                contingency: leg.contingency,
                linked: leg.linked,
                order_client_id: leg.order_client_id,
            })
            .collect(),
    })
}

/// The caller's own id for every leg of `list`, in leg order (`--order-client-id` repeated, the tool's `order_client_ids`), the
/// order-list twin of a submit's pinned id. None given leaves the legs for [`fill_order_client_id`] to mint; a
/// count that is not one per leg, a blank id, or one outside the charset ([`order_client_id_refusal`], rendered) is
/// refused in words.
pub fn with_leg_order_client_ids(
    mut list: OrderListWire,
    order_client_ids: &[String],
) -> Result<OrderListWire, String> {
    if order_client_ids.is_empty() {
        return Ok(list);
    }
    if order_client_ids.len() != list.legs.len() {
        return Err(format!(
            "{} order client ids for {} legs: name one per leg, in leg order, or none",
            order_client_ids.len(),
            list.legs.len()
        ));
    }
    if let Some(i) = order_client_ids.iter().position(|c| c.trim().is_empty()) {
        return Err(format!("leg {i}'s order client id is blank"));
    }
    if let Some(refusal) = order_client_ids.iter().find_map(|c| order_client_id_refusal(c.trim())) {
        return Err(refusal.rendered());
    }
    for (leg, order_client_id) in list.legs.iter_mut().zip(order_client_ids) {
        leg.order_client_id = Some(order_client_id.trim().to_string());
    }
    Ok(list)
}

// ---- the COMBO verb (wave 6 of the order kinds) ----------------------------------------------------------------

/// One combo leg as a surface reads it: the instrument, its SIGNED ratio, and its own limit when the combo is priced
/// by its legs.
#[derive(Debug, Clone, PartialEq)]
pub struct ComboLegArg {
    pub symbol: String,
    pub ratio: i32,
    pub limit: Option<f64>,
}

/// `combo <account> <buy|sell> <qty> --leg <symbol>:<ratio>[@<limit>]... [--net <px>] [--time-in-force <word>]`: the wire combo,
/// refused in words first by `vike_model::OrderCombo::validate`, the rule the node's door applies (two legs or more, no
/// zero ratio, a side of ±1, a size above zero, a net OR a limit on every leg, never a mix, every number finite), then
/// its lifetime by `vike_model::lifetime_refusal` on the `limit` or `market` order it lowers to, as the door does (a
/// lifetime but `gtc` needs a priced combo; a `gtd` is refused: a combo carries no expiry), so the client says what the
/// door would, and refuses `post-only` (no book models a maker-only combo) for both doors. ⚠ It is the SENDER's guard on a non-finite net
/// or leg limit, which would go out as `null`: a combo market order, or a leg the node reads as naming no limit.
pub fn combo_of(
    venue: String,
    account: Option<String>,
    side: i32,
    qty: f64,
    legs: Vec<ComboLegArg>,
    net_limit: Option<f64>,
    time_in_force: vike_model::TimeInForce,
) -> Result<OrderComboWire, String> {
    let spec = vike_model::OrderCombo {
        venue: venue.clone(),
        side,
        qty,
        legs: legs
            .iter()
            .map(|leg| vike_model::OrderComboLeg {
                symbol: leg.symbol.clone(),
                ratio: leg.ratio,
                limit: leg.limit,
            })
            .collect(),
        net_limit,
        time_in_force,
        account: None,
        order_client_id: None,
    };
    spec.validate().map_err(|e| e.to_string())?;
    // One rule for both doors (the CLI's `--time-in-force`, the tool's `time_in_force`): no book models a maker-only combo.
    if time_in_force == vike_model::TimeInForce::PostOnly {
        return Err("a combo cannot be post-only: no book models a maker-only combo".to_string());
    }
    let priced = net_limit.is_some() || legs.iter().any(|leg| leg.limit.is_some());
    let kind = if priced { vike_model::OrderType::Limit } else { vike_model::OrderType::Market };
    if let Some(why) =
        vike_model::lifetime_refusal(&kind, time_in_force, None, None, vike_model::now_ms())
    {
        let none = if time_in_force == vike_model::TimeInForce::Gtd {
            ", and a combo carries none"
        } else {
            ""
        };
        return Err(format!("{why}{none}"));
    }
    Ok(OrderComboWire {
        venue,
        side,
        qty,
        legs: spec.legs,
        net_limit,
        account,
        // `gtc` goes out as the absent field; the id is the caller's or `fill_order_client_id` mints it before the preview
        time_in_force: (time_in_force != vike_model::TimeInForce::Gtc)
            .then(|| time_in_force.as_word().to_owned()),
        order_client_id: None,
    })
}

#[path = "verbs_tests.rs"]
#[cfg(test)]
mod verbs_tests;
