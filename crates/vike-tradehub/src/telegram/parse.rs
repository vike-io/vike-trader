//! The chat-string grammar: the `getUpdates` payload shape ([`TgUpdate`]/[`parse_updates`]) and the
//! ONE place a chat message becomes a [`WireCommand`] ([`parse_instruction`]), plus the operator-
//! facing renderings of a resolved command ([`describe`]) and of its audit rationale
//! ([`confirm_reason`]).
//!
//! Pure and allocation-only — no clock, no randomness, no I/O. A misparse here is caught by the
//! PREVIEW an operator reads before typing `/confirm`; the gating that makes that true lives in
//! `config.rs`, `confirm.rs` and `ledger.rs`.

use vike_tradehub_client::wire::{WireCommand, WireOrderRequest, WireTradingState};

// ---------------------------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------------------------

/// A sender id no `message.from` was present for — a channel post, an anonymous group admin, or a
/// non-`message` update. Telegram user ids are positive, so `0` cannot collide with a real one.
///
/// ⚠ It is NOT a wildcard: [`TelegramConfig::allows_user`](super::TelegramConfig::allows_user)
/// treats it like any other id, so a configured user allowlist refuses it (nobody can allowlist
/// "unknown") while an unconfigured one keeps accepting it, exactly as before this field existed.
pub const UNKNOWN_USER_ID: i64 = 0;

/// ONE Telegram update, reduced to the only things this channel uses.
///
/// A non-text update (a photo, a sticker, an `edited_message`, a `channel_post`) still yields a row
/// — with an EMPTY `text` and `chat_id: 0` — deliberately: it must still advance the ledger's
/// high-water mark, or Telegram would re-deliver it forever. It parses to [`Instruction::Ignore`]
/// and is never answered. Only the `message` field is read: an EDITED message must never re-execute.
///
/// ⚠ `from_id`/`from_username` are the WHO. They exist because authorization on this channel is
/// per-CHAT, and a chat is not a person: a `-100…` supergroup passes the allowlist for every one of
/// its members, so without these an accepted order could be attributed no further than "somebody in
/// chat -100123". An audit trail that cannot name who placed an order is not much of an audit trail
/// on a surface that signs real money. `from_username` is Telegram's mutable `@handle` and is
/// recorded for HUMAN legibility only — `from_id` is the stable identity, and the one
/// [`TelegramConfig::allows_user`](super::TelegramConfig::allows_user) checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgUpdate {
    pub update_id: i64,
    pub chat_id: i64,
    /// The sender's numeric Telegram user id, or [`UNKNOWN_USER_ID`] when the update carried no
    /// `message.from`.
    pub from_id: i64,
    /// The sender's `@handle`, when they have one. Mutable and non-unique — never an identity.
    pub from_username: Option<String>,
    pub text: String,
}

/// Parse a `getUpdates` response body into updates, in the order Telegram returned them.
///
/// Pinned fields (Bot API): `result[].update_id`, `result[].message.chat.id`,
/// `result[].message.from.id`, `result[].message.from.username`, `result[].message.text`. Anything
/// else in the payload is ignored.
pub fn parse_updates(v: &serde_json::Value) -> Vec<TgUpdate> {
    let Some(items) = v.get("result").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let update_id = it.get("update_id")?.as_i64()?;
            let msg = it.get("message");
            let chat_id = msg
                .and_then(|m| m.get("chat"))
                .and_then(|c| c.get("id"))
                .and_then(|i| i.as_i64())
                .unwrap_or(0);
            // `from` is ABSENT on a channel post and on an anonymous group admin's message — read
            // as UNKNOWN_USER_ID rather than defaulted to anything that could pass an allowlist.
            let from = msg.and_then(|m| m.get("from"));
            let from_id =
                from.and_then(|f| f.get("id")).and_then(|i| i.as_i64()).unwrap_or(UNKNOWN_USER_ID);
            let from_username = from
                .and_then(|f| f.get("username"))
                .and_then(|u| u.as_str())
                .filter(|u| !u.is_empty())
                .map(str::to_string);
            let text =
                msg.and_then(|m| m.get("text")).and_then(|t| t.as_str()).unwrap_or("").to_string();
            Some(TgUpdate { update_id, chat_id, from_id, from_username, text })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Instruction grammar
// ---------------------------------------------------------------------------------------------

/// The READ verbs, answered straight off the published snapshot (no core round-trip, no gate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadVerb {
    Status,
    Positions,
    Orders,
    Equity,
}

/// What one chat message asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    /// A read verb — answered immediately from the snapshot.
    Read(ReadVerb),
    /// A WRITE verb — PREVIEWED only. Never executed by the message that carried it.
    /// A `Submit`'s `client_order_id` is empty here and minted at preview time.
    ///
    /// ⚠ **BOXED, and the box is the lint rather than a design choice.** `WireCommand` is ~240
    /// bytes against this enum's next-largest variant at 24, so carrying it inline makes every
    /// `Instruction` — a `/help`, an `Ignore` for ordinary group chatter — pay the size of the
    /// largest order frame. `clippy::large_enum_variant` says so under `-D warnings`, and the
    /// `#[allow]`s for it inside `ibapi` 3.2.1 (a pinned registry dependency, not first-party
    /// code) are not a precedent for this enum. Construct through [`Instruction::write`] rather
    /// than spelling the `Box` at each site.
    Write(Box<WireCommand>),
    /// `/confirm <token>` — the ONLY instruction that can reach the core.
    Confirm(String),
    /// `/help`.
    Help,
    /// A recognized verb whose arguments do not satisfy it — answered with this reason +
    /// [`USAGE`](super::USAGE).
    Malformed(String),
    /// An unrecognized `/verb` — answered with [`USAGE`](super::USAGE).
    Unknown,
    /// Not addressed to the bot (no leading `/`, or a non-text update) — SILENTLY ignored, no
    /// reply. Ordinary chatter in a group must not produce bot noise.
    Ignore,
}

impl Instruction {
    /// Build an [`Instruction::Write`] — the one place the `Box` is spelled, so adding a field to
    /// any `WireCommand` variant never becomes an edit at fifteen construction sites.
    fn write(cmd: WireCommand) -> Self {
        Instruction::Write(Box::new(cmd))
    }
}

/// Parse ONE chat message into an [`Instruction`]. Pure — this is the whole grammar, and it is the
/// only place a chat string becomes a [`WireCommand`].
pub fn parse_instruction(text: &str) -> Instruction {
    let text = text.trim();
    let Some(rest) = text.strip_prefix('/') else {
        return Instruction::Ignore;
    };
    let mut it = rest.split_whitespace();
    let Some(raw_verb) = it.next() else {
        return Instruction::Ignore;
    };
    // In a group Telegram appends the bot's username: `/status@vike_node_bot`.
    let verb = raw_verb.split('@').next().unwrap_or(raw_verb).to_ascii_lowercase();
    let args: Vec<&str> = it.collect();

    match verb.as_str() {
        "status" => Instruction::Read(ReadVerb::Status),
        "positions" => Instruction::Read(ReadVerb::Positions),
        "orders" => Instruction::Read(ReadVerb::Orders),
        "equity" => Instruction::Read(ReadVerb::Equity),
        "help" | "start" => Instruction::Help,
        "confirm" => match args.first() {
            Some(token) if !token.is_empty() => Instruction::Confirm((*token).to_string()),
            _ => Instruction::Malformed("/confirm needs the token from the preview".into()),
        },
        "submit" => parse_submit(&args),
        "cancel" => match args.first() {
            Some(coid) if !coid.is_empty() => {
                Instruction::write(WireCommand::Cancel((*coid).to_string()))
            }
            _ => Instruction::Malformed("/cancel needs a client_order_id".into()),
        },
        "modify" => parse_modify(&args),
        "masscancel" => Instruction::write(WireCommand::MassCancel {
            venue: args.first().map(|s| (*s).to_string()),
            symbol: args.get(1).map(|s| (*s).to_string()),
            account: None,
        }),
        "flatten" => match (args.first(), args.get(1)) {
            (Some(v), Some(s)) => Instruction::write(WireCommand::Flatten {
                venue: (*v).to_string(),
                symbol: (*s).to_string(),
                account: None,
            }),
            _ => Instruction::Malformed("/flatten needs <venue> <symbol>".into()),
        },
        "marketexit" => Instruction::write(WireCommand::MarketExit {
            venue: args.first().map(|s| (*s).to_string()),
            account: None,
        }),
        "state" => match args.first().map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("active") => {
                Instruction::write(WireCommand::SetTradingState(WireTradingState::Active))
            }
            Some("reducing") => {
                Instruction::write(WireCommand::SetTradingState(WireTradingState::Reducing))
            }
            Some("halted") => {
                Instruction::write(WireCommand::SetTradingState(WireTradingState::Halted))
            }
            _ => Instruction::Malformed("/state needs one of: active | reducing | halted".into()),
        },
        _ => Instruction::Unknown,
    }
}

/// `/submit <venue> <symbol> <buy|sell> <qty> [price] [reduce]` — a bare price makes it a LIMIT
/// order, its absence a MARKET order. `reduce`/`reduce_only` may appear anywhere after the verb.
fn parse_submit(args: &[&str]) -> Instruction {
    let reduce_only = args
        .iter()
        .any(|a| a.eq_ignore_ascii_case("reduce") || a.eq_ignore_ascii_case("reduce_only"));
    let positional: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.eq_ignore_ascii_case("reduce") && !a.eq_ignore_ascii_case("reduce_only"))
        .collect();
    if positional.len() < 4 {
        return Instruction::Malformed(
            "/submit needs <venue> <symbol> <buy|sell> <qty> [price] [reduce]".into(),
        );
    }
    let side = match positional[2].to_ascii_lowercase().as_str() {
        "buy" | "long" => 1,
        "sell" | "short" => -1,
        other => return Instruction::Malformed(format!("side must be buy or sell, got {other:?}")),
    };
    let Ok(qty) = positional[3].parse::<f64>() else {
        return Instruction::Malformed(format!("qty is not a number: {:?}", positional[3]));
    };
    if !(qty.is_finite() && qty > 0.0) {
        return Instruction::Malformed("qty must be a positive, finite number".into());
    }
    let price = match positional.get(4) {
        None => None,
        Some(raw) => match raw.parse::<f64>() {
            Ok(p) if p.is_finite() && p > 0.0 => Some(p),
            _ => return Instruction::Malformed(format!("price is not a number: {raw:?}")),
        },
    };
    Instruction::write(WireCommand::Submit(WireOrderRequest {
        // Minted at PREVIEW time (see `poll_once`) so the token binds to a fixed coid; an empty one
        // reaching the core would be refused by `lower_command`'s remote-submit policy anyway.
        client_order_id: String::new(),
        venue: positional[0].to_string(),
        symbol: positional[1].to_string(),
        side,
        qty,
        order_type: if price.is_some() { "limit".into() } else { "market".into() },
        price,
        trigger_price: None,
        reduce_only,
        account: None,
    }))
}

/// `/modify <client_order_id> [qty=<q>] [price=<p>]` — key=value so a lone number can never be
/// silently read as the wrong field. At least one of the two is required.
fn parse_modify(args: &[&str]) -> Instruction {
    let Some(coid) = args.first().filter(|c| !c.is_empty()) else {
        return Instruction::Malformed(
            "/modify needs <client_order_id> [qty=<q>] [price=<p>]".into(),
        );
    };
    let (mut new_qty, mut new_price) = (None, None);
    for arg in &args[1..] {
        let Some((key, value)) = arg.split_once('=') else {
            return Instruction::Malformed(format!("expected qty=<q> or price=<p>, got {arg:?}"));
        };
        let Ok(parsed) = value.parse::<f64>() else {
            return Instruction::Malformed(format!("{key} is not a number: {value:?}"));
        };
        if !parsed.is_finite() || parsed <= 0.0 {
            return Instruction::Malformed(format!("{key} must be a positive, finite number"));
        }
        match key.to_ascii_lowercase().as_str() {
            "qty" | "size" => new_qty = Some(parsed),
            "price" | "px" => new_price = Some(parsed),
            other => return Instruction::Malformed(format!("unknown field {other:?}")),
        }
    }
    if new_qty.is_none() && new_price.is_none() {
        return Instruction::Malformed("/modify needs at least one of qty=<q> / price=<p>".into());
    }
    Instruction::write(WireCommand::Modify {
        client_order_id: (*coid).to_string(),
        new_qty,
        new_price,
    })
}

/// Fill a `Submit`'s empty `client_order_id` with the minted one. Every other verb is returned
/// unchanged (they either target an existing coid or are account-wide).
pub fn fill_coid(cmd: WireCommand, coid: &str) -> WireCommand {
    match cmd {
        WireCommand::Submit(mut req) if req.client_order_id.is_empty() => {
            req.client_order_id = coid.to_string();
            WireCommand::Submit(req)
        }
        other => other,
    }
}

/// A one-line, operator-readable rendering of the exact command a preview would execute. This is
/// what an operator checks before typing `/confirm`, so it must show everything that varies.
pub fn describe(cmd: &WireCommand) -> String {
    match cmd {
        WireCommand::Submit(r) => format!(
            "submit {} {} {} {} {}{} coid={}",
            r.venue,
            r.symbol,
            if r.side >= 0 { "buy" } else { "sell" },
            r.qty,
            match r.price {
                Some(p) => format!("limit @ {p}"),
                None => "market".to_string(),
            },
            if r.reduce_only { " reduce_only" } else { "" },
            r.client_order_id
        ),
        WireCommand::Cancel(coid) => format!("cancel {coid}"),
        WireCommand::Modify { client_order_id, new_qty, new_price } => format!(
            "modify {client_order_id} qty={} price={}",
            new_qty.map(|q| q.to_string()).unwrap_or_else(|| "-".into()),
            new_price.map(|p| p.to_string()).unwrap_or_else(|| "-".into())
        ),
        WireCommand::MassCancel { venue, symbol, .. } => format!(
            "mass_cancel venue={} symbol={}",
            venue.as_deref().unwrap_or("*"),
            symbol.as_deref().unwrap_or("*")
        ),
        WireCommand::Flatten { venue, symbol, .. } => format!("flatten {venue} {symbol}"),
        WireCommand::MarketExit { venue, .. } => {
            format!("market_exit venue={}", venue.as_deref().unwrap_or("*"))
        }
        WireCommand::SetTradingState(s) => format!("set_trading_state {s:?}"),
        // No Telegram grammar PRODUCES this verb (split-plane B4 wired the TCP surface only), but
        // `describe` must stay total: a preview of one — should a future parser arm mint it — must
        // render what varies (the target mount + the raw params JSON), never panic or elide.
        WireCommand::UpdateParams { venue, symbol, interval, params } => {
            format!("update_params {venue} {symbol} {interval} {params}")
        }
        // Same posture for the B5 mount verbs (TCP-surface only; no Telegram grammar mints them).
        WireCommand::MountStrategy {
            venue,
            account,
            symbol,
            interval,
            controller_id,
            name,
            rhai,
            params,
        } => {
            // The ACCOUNT rides here for the reason the comment above gives about the params: this
            // renders into the CONTROL AUDIT TRAIL, and a mount is every order that strategy will
            // ever place. A line that named the venue and not the book would describe a different
            // mount. Absent renders `-`, never `default` — the sender named nothing, and on a
            // two-engine venue the node refuses rather than choosing.
            format!(
                "mount_strategy {venue} account={} {symbol} {interval} id={} source={} {params}",
                account.as_deref().unwrap_or("-"),
                controller_id.as_deref().unwrap_or("-"),
                name.as_deref().or(rhai.as_deref()).unwrap_or("-"),
            )
        }
        WireCommand::UnmountStrategy { controller_id } => {
            format!("unmount_strategy {controller_id}")
        }
        // Same posture again for the REQ-7 settings write (TCP-surface only; no Telegram grammar
        // mints it, and the channel's `accept` passes no settings source, so even a minted one is
        // refused): render what varies — the confirm's PRESENCE, never a value to copy from.
        WireCommand::SetSetting { file, key, value, confirm } => {
            format!(
                "set_setting {file} {key} = {value}{}",
                if confirm.is_some() { " (confirmed)" } else { "" }
            )
        }
    }
}

/// Render ONE actor for the audit trail: the stable numeric id, plus the mutable `@handle` when
/// Telegram supplied one. [`UNKNOWN_USER_ID`] renders as `unknown` rather than `0`, so a reader is
/// never invited to believe there is a user 0.
pub fn describe_actor(from_id: i64, username: Option<&str>) -> String {
    let who = if from_id == UNKNOWN_USER_ID { "unknown".to_string() } else { from_id.to_string() };
    match username {
        Some(u) => format!("user {who} (@{u})"),
        None => format!("user {who}"),
    }
}

/// The rationale recorded in the AUDIT trail for a Telegram-origin command: the operator's LITERAL
/// instruction, prefixed with the chat AND the person that sent it.
///
/// The origin prefix is load-bearing, not decoration: [`crate::audit::record`] carries
/// `peer: Option<SocketAddr>`, which is `None` for a non-socket surface, so without this the audit
/// line could not say WHERE an accepted command came from. The literal text is preserved verbatim
/// (`accept_command` sanitizes it downstream — control characters stripped, length capped — exactly
/// as it does for a TCP peer's rationale).
///
/// ⚠ The ACTOR half is what makes it say WHO. The chat alone cannot: this channel authorizes per
/// chat, and an allowlisted `-100…` supergroup is every one of its members, so "telegram chat
/// -100123" attributes an order to a room. See [`TgUpdate::from_id`].
pub fn confirm_reason(chat_id: i64, from_id: i64, username: Option<&str>, text: &str) -> String {
    format!("telegram chat {chat_id} {}: {text}", describe_actor(from_id, username))
}

/// Extend a preview's [`confirm_reason`] with the actor who actually typed `/confirm`.
///
/// Preview and confirm are two separate messages, and in a group they can come from two different
/// people — the preview binds to the CHAT ([`PendingConfirms::take`](super::PendingConfirms::take)),
/// not to the sender. So the audit line names both: the person whose instruction this is, and the
/// person who authorized executing it. Appended unconditionally rather than only when they differ,
/// because "the field is missing" is a worse thing for a reader to have to interpret than "both
/// names are the same".
pub fn confirmed_by(preview_reason: &str, from_id: i64, username: Option<&str>) -> String {
    format!("{preview_reason} [/confirm by {}]", describe_actor(from_id, username))
}

#[path = "parse_tests.rs"]
#[cfg(test)]
mod parse_tests;
