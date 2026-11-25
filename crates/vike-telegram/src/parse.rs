//! The chat-string grammar: the `getUpdates` payload shape ([`TelegramUpdate`]/[`parse_updates`]) and the
//! ONE place a chat message becomes a command ([`parse_instruction`]), plus the operator-facing
//! renderings of a resolved command ([`describe`]) and of its audit rationale ([`confirm_reason`]).
//!
//! **This grammar turns a chat line into a `vike_client::verbs::Verb` and nothing more.** The
//! command is built by [`Verb::to_wire_command`], the construction site the CLI and the MCP server
//! resolve through; its order client id comes from the CLI's mint
//! ([`crate::TelegramDependencies::fill_order_client_id`]); and the verdict is the node's own, over the control wire
//! (`crates/vike-telegram/src/deps.rs`). What stays here is the chat's own words: the verbs, the
//! book token (`vike_model::accounts::account_keys::parse_book_token`, the CLI's reader) and the
//! number checks, which refuse a zero or negative price where the CLI's leave that to the node.
//!
//! Pure and allocation-only — no clock, no randomness, no I/O. A misparse here is caught by the
//! PREVIEW an operator reads before typing `/confirm`; the gating that makes that true lives in
//! `config.rs`, `confirm.rs` and `ledger.rs`.

use vike_client::verbs::Verb;
use vike_model::TradingState;
use vike_tradehub_client::wire::{CommandWire, OrderRequestWire};
use vike_utility::number_text::format_number;

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
pub struct TelegramUpdate {
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
pub fn parse_updates(v: &serde_json::Value) -> Vec<TelegramUpdate> {
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
            Some(TelegramUpdate { update_id, chat_id, from_id, from_username, text })
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
    /// A `Submit`'s `order_client_id` is empty here and minted at preview time.
    ///
    /// ⚠ **BOXED, and the box is the lint rather than a design choice.** `CommandWire` is ~240
    /// bytes against this enum's next-largest variant at 24, so carrying it inline makes every
    /// `Instruction` — a `/help`, an `Ignore` for ordinary group chatter — pay the size of the
    /// largest order frame. `clippy::large_enum_variant` says so under `-D warnings`, and the
    /// `#[allow]`s for it inside `ibapi` 3.2.1 (a pinned registry dependency, not first-party
    /// code) are not a precedent for this enum. Construct through [`Instruction::write`] rather
    /// than spelling the `Box` at each site.
    Write(Box<CommandWire>),
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
    /// Build an [`Instruction::Write`] out of a WRITE [`Verb`], through [`Verb::to_wire_command`] —
    /// the one place the `Box` is spelled and the one place the command is built.
    fn write(verb: Verb) -> Self {
        let cmd = verb.to_wire_command().expect("every verb this grammar builds is a write verb");
        Instruction::Write(Box::new(cmd))
    }
}

/// Parse ONE chat message into an [`Instruction`]. Pure — this is the whole grammar, and it is the
/// only place a chat string becomes a [`CommandWire`].
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
            Some(order_client_id) if !order_client_id.is_empty() => Instruction::write(Verb::Cancel((*order_client_id).to_string())),
            _ => Instruction::Malformed("/cancel needs an order_client_id".into()),
        },
        "modify" => parse_modify(&args),
        "masscancel" => match optional_book(args.first()) {
            Ok((venue, account)) => Instruction::write(Verb::MassCancel {
                venue,
                symbol: args.get(1).map(|s| (*s).to_string()),
                account,
            }),
            Err(why) => Instruction::Malformed(why),
        },
        "flatten" => match (args.first(), args.get(1)) {
            (Some(v), Some(s)) => match book(v) {
                Ok((venue, account)) => Instruction::write(Verb::Flatten {
                    venue,
                    symbol: (*s).to_string(),
                    account,
                }),
                Err(why) => Instruction::Malformed(why),
            },
            _ => Instruction::Malformed(
                "/flatten needs <venue>/<LABEL> <symbol> (a bare <venue> only where the node has one account of it)"
                    .into(),
            ),
        },
        "closeall" => match optional_book(args.first()) {
            Ok((venue, account)) => Instruction::write(Verb::MarketExit { venue, account }),
            Err(why) => Instruction::Malformed(why),
        },
        // The operator's word, in any case (`TradingState::parse`, the one case rule).
        "state" => match args.first().and_then(|s| TradingState::parse(s)) {
            Some(state) => Instruction::write(Verb::SetState(state)),
            None => Instruction::Malformed(format!(
                "/state needs one of: {}",
                TradingState::WORDS.join(" | ")
            )),
        },
        "resumevenue" => parse_resume_venue(&args),
        // A word this channel took once is refused naming the new one, never answered as unknown.
        other => match RENAMED_COMMANDS.iter().find(|(was, _)| *was == other) {
            Some(&(was, now)) => {
                let refusal = TelegramRefusal::Renamed { was, now };
                Instruction::Malformed(refusal.rendered())
            }
            None => Instruction::Unknown,
        },
    }
}

vike_utility::refusal_enum! {
    /// A refusal of a chat command: code decides on the variant, the chat reads its text.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum TelegramRefusal with ids TelegramRefusalId {
        /// A command this channel took once and takes under another word now (`RENAMED_COMMANDS`):
        /// refused naming the new one, never an alias.
        Renamed { was: &'static str, now: &'static str } => "telegram.command.renamed":
            "/{was} is now /{now}: use the new command",
    }
}

/// Every chat command this channel no longer takes, and the word it takes in its place.
const RENAMED_COMMANDS: &[(&str, &str)] = &[("marketexit", "closeall")];

/// A venue token as the `(venue, account)` it sends: `hyperliquid/LIVE` and `hyperliquid/DEFAULT` name that
/// account, and a bare `hyperliquid` names none, which the door takes only where the venue has one account, running or not
/// (decision 0254). Without this a phone could not name the account the door then asks it for. The
/// grammar and its refusals are `vike-cli`'s too, so a route key copied off a read is refused here by name.
fn book(token: &str) -> Result<(String, Option<String>), String> {
    let (venue, account) = vike_model::accounts::account_keys::parse_book_token(token)?;
    Ok((venue.to_string(), account.map(|label| label.to_string())))
}

/// [`book`] over an optional token: none is the whole node, which names neither.
fn optional_book(token: Option<&&str>) -> Result<(Option<String>, Option<String>), String> {
    let Some(token) = token else { return Ok((None, None)) };
    let (venue, account) = book(token)?;
    Ok((Some(venue), account))
}

/// `/resumevenue <venue> [force]` — release the LINK DEAD-MAN's halt of ONE venue (decision 0180),
/// which `/state active` cannot do without also clearing every other venue's halt and the drawdown
/// latch. The node refuses while the venue's link is still down unless `force` is spelled; the
/// word is a bare `force`, never a flag, so a stray token is a usage error and not an override.
fn parse_resume_venue(args: &[&str]) -> Instruction {
    const USAGE: &str = "/resumevenue needs <venue> [force]";
    let Some(venue) = args.first().filter(|v| !v.is_empty()) else {
        return Instruction::Malformed(USAGE.into());
    };
    let force = match args.get(1).map(|s| s.to_ascii_lowercase()).as_deref() {
        None => false,
        Some("force") => true,
        Some(other) => return Instruction::Malformed(format!("{USAGE}, got {other:?}")),
    };
    if args.len() > 2 {
        return Instruction::Malformed(USAGE.into());
    }
    Instruction::write(Verb::ResumeVenue { venue: (*venue).to_string(), force })
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
    let (venue, account) = match book(positional[0]) {
        Ok(addressed) => addressed,
        Err(why) => return Instruction::Malformed(why),
    };
    let Some(side) = vike_model::side_from_word(positional[2]) else {
        return Instruction::Malformed(format!(
            "side must be buy or sell, got {:?}",
            positional[2].to_ascii_lowercase()
        ));
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
    Instruction::write(Verb::Submit(OrderRequestWire {
        // Minted at PREVIEW time (see `poll_once`) so the token binds to a fixed order client id; an empty one
        // reaching the node would be refused by its remote-submit policy anyway.
        order_client_id: String::new(),
        venue,
        symbol: positional[1].to_string(),
        side,
        qty,
        order_type: if price.is_some() { "limit".into() } else { "market".into() },
        price,
        trigger_price: None,
        reduce_only,
        account,
        // `/submit` offers no lifetime: the default `gtc`
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    }))
}

/// `/modify <order_client_id> [qty=<q>] [price=<p>]` — key=value so a lone number can never be
/// silently read as the wrong field. At least one of the two is required.
fn parse_modify(args: &[&str]) -> Instruction {
    let Some(order_client_id) = args.first().filter(|c| !c.is_empty()) else {
        return Instruction::Malformed(
            "/modify needs <order_client_id> [qty=<q>] [price=<p>]".into(),
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
    Instruction::write(Verb::Modify {
        order_client_id: (*order_client_id).to_string(),
        new_qty,
        new_price,
        new_trigger_price: None,
    })
}

/// The account a command names, for the line an operator confirms; nothing when it names none.
fn account_words(account: Option<&str>) -> String {
    account.map(|a| format!(" account={a}")).unwrap_or_default()
}

/// A one-line, operator-readable rendering of the exact command a preview would execute. This is
/// what an operator checks before typing `/confirm`, so it must show everything that varies.
pub fn describe(cmd: &CommandWire) -> String {
    match cmd {
        CommandWire::Submit(r) => format!(
            "submit {} {} {} {} {}{} order_client_id={}{}",
            r.venue,
            r.symbol,
            if r.side >= 0 { "buy" } else { "sell" },
            format_number(r.qty),
            match r.price {
                Some(p) => format!("limit @ {}", format_number(p)),
                None => "market".to_string(),
            },
            if r.reduce_only { " reduce_only" } else { "" },
            r.order_client_id,
            account_words(r.account.as_deref())
        ),
        CommandWire::Cancel(order_client_id) => format!("cancel {order_client_id}"),
        CommandWire::Modify { order_client_id, new_qty, new_price, .. } => format!(
            "modify {order_client_id} qty={} price={}",
            new_qty.map(format_number).unwrap_or_else(|| "-".into()),
            new_price.map(format_number).unwrap_or_else(|| "-".into())
        ),
        CommandWire::MassCancel { venue, symbol, account } => format!(
            "mass_cancel venue={} symbol={}{}",
            venue.as_deref().unwrap_or("*"),
            symbol.as_deref().unwrap_or("*"),
            account_words(account.as_deref())
        ),
        CommandWire::Flatten { venue, symbol, account } => {
            format!("flatten {venue} {symbol}{}", account_words(account.as_deref()))
        }
        CommandWire::MarketExit { venue, account } => {
            format!(
                "market_exit venue={}{}",
                venue.as_deref().unwrap_or("*"),
                account_words(account.as_deref())
            )
        }
        CommandWire::SetTradingState(s) => format!("set_trading_state {s:?}"),
        // No Telegram grammar mints a bracket (the desktop's TP/SL ticket is its one producer), but
        // `describe` stays total, for the reason the `UpdateParams` arm below gives.
        CommandWire::Bracket(b) => format!(
            "bracket {} {} {} {} entry={} sl={} tp={} account={}",
            b.venue,
            b.symbol,
            if b.side >= 0 { "buy" } else { "sell" },
            format_number(b.qty),
            match b.entry_price {
                Some(p) => format!("limit @ {}", format_number(p)),
                None => "market".to_string(),
            },
            format_number(b.stop_loss),
            format_number(b.take_profit),
            b.account.as_deref().unwrap_or("-"),
        ),
        // No Telegram grammar mints an order list either; `describe` stays total, in the list's one wording.
        CommandWire::OrderList(l) => l.say(),
        // Nor a combo: `describe` stays total, in the combo's one wording.
        CommandWire::Combo(c) => c.say(),
        // No Telegram grammar PRODUCES this verb (split-plane B4 wired the TCP surface only), but
        // `describe` must stay total: a preview of one — should a future parser arm mint it — must
        // render what varies (the target mount + the raw params JSON), never panic or elide.
        CommandWire::UpdateParams { venue, symbol, interval, mount_id, params } => {
            // The mount id rides for the reason the mount verbs' account does: this line is the
            // audit trail, and two mounts on one series are told apart by nothing else. Absent
            // renders `-`, the sender named no mount.
            format!(
                "update_params {venue} {symbol} {interval} id={} {params}",
                mount_id.as_deref().unwrap_or("-")
            )
        }
        // Same posture for the B5 mount verbs (TCP-surface only; no Telegram grammar mints them).
        CommandWire::MountStrategy {
            venue,
            account,
            symbol,
            interval,
            controller_id,
            file,
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
                file.as_deref().or(name.as_deref()).or(rhai.as_deref()).unwrap_or("-"),
            )
        }
        CommandWire::UnmountStrategy { controller_id } => {
            format!("unmount_strategy {controller_id}")
        }
        // TCP-surface only (no Telegram grammar mints it): the id is all that varies.
        CommandWire::ReconcileConfirm { id } => format!("recon_confirm {id}"),
        // TCP-surface only as well, and deliberately: the Telegram channel gets no cancel-a-held-order grammar.
        CommandWire::ReconcileCancel { id } => format!("recon_cancel {id}"),
        // The venue and the override are everything that varies, and the override is SHOUTED: a
        // forced resume trades on a link the dead-man believes is down.
        CommandWire::ResumeVenue { venue, force } => {
            format!(
                "resume_venue {venue}{}",
                if *force { " FORCE (link may still be down)" } else { "" }
            )
        }
        // TCP-surface only (no Telegram grammar mints it): the book and the state are what vary, and
        // this renders into the audit trail and the previews.
        CommandWire::SetAccountTradingState { venue, account, state } => {
            let book = vike_tradehub_client::wire::book_of_address(venue, account.as_deref());
            format!("set_account_trading_state {book} {state:?}")
        }
        // TCP-surface only as well (no Telegram grammar mints it): the book and the symbol are what vary.
        CommandWire::UnadmitSymbol { venue, account, symbol } => {
            let book = vike_tradehub_client::wire::book_of_address(venue, account.as_deref());
            format!("unadmit_symbol {book} {symbol}")
        }
        // TCP-surface only as well (decision 0239): the mount is what varies.
        CommandWire::UnlatchMount { mount_id } => format!("unlatch_mount {mount_id}"),
        // Same posture again for the REQ-7 settings write (no Telegram grammar mints it, so a chat
        // can never write a setting): render what varies — the confirm's PRESENCE, never a value to
        // copy from.
        CommandWire::SetSetting { file, key, value, confirm } => {
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
/// The origin prefix is load-bearing, not decoration: it rides `Request::Command`'s `reason` to the
/// node's audit record (`crates/vike-tradehub/src/audit.rs`'s `record`), whose peer and key are
/// this process's loopback socket and its control key, the same for every chat. Without the prefix
/// the audit line could not say WHICH chat an accepted command came from. The literal text is
/// preserved verbatim (the node sanitizes it — control characters stripped, length capped — as it
/// does every peer's rationale).
///
/// ⚠ The ACTOR half is what makes it say WHO. The chat alone cannot: this channel authorizes per
/// chat, and an allowlisted `-100…` supergroup is every one of its members, so "telegram chat
/// -100123" attributes an order to a room. See [`TelegramUpdate::from_id`].
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
