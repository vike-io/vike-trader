//! Human rendering: the one-line preview of a wire command, and the REPL's snapshot tables.

use vike_tradehub_client::wire::{
    KEY_CHANGED_WARNING, WireCommand, WireSnapshot, WireTradingState,
};

use crate::cmd::verbs;

use super::{is_pre_fold, width_of};

// ---- human rendering (of the pure command / of the snapshot) ---------------------------------

/// What an `accepted by the node (coid ...)` line says after the dash, for the surface that prints it:
/// `orders` and `positions` are the words that surface reads them with (`orders`/`positions` at the
/// REPL, `trade order ls`/`trade position ls` one-shot).
///
/// The node's Ack is for the COMMAND, never for what the core then does with it: the node answers
/// before the core acts, so a denial or a rejection shows up only afterwards, in the node's recent
/// events (the REPL's `recent`). Every order-writing command (submit, modify, flatten) says so after
/// its unchanged text; the rest keep that text alone.
///
/// A CANCEL, MASS-CANCEL or MARKET-EXIT (whose first leg is a mass-cancel) says more: the core cancels
/// at the venue only an order its registry holds. An order it does not hold (placed before a restart
/// and held as an `UnknownOrder`, or a mistyped id) gets no venue call and an `OrderCancelRejected`
/// line in the node's recent events instead, so the line says where to look rather than let
/// "accepted" read as "cancelled".
pub(super) fn accepted_tail(cmd: &WireCommand, orders: &str, positions: &str) -> String {
    match cmd {
        WireCommand::Cancel(_)
        | WireCommand::MassCancel { .. }
        | WireCommand::MarketExit { .. } => {
            format!(
                "this is the receipt for the COMMAND, not for the cancel: the node answers before \
                 it acts, and it cancels at the venue only an order its registry holds. For any \
                 other order it sends nothing and says so in its recent events (the REPL's \
                 `recent`): an `OrderCancelRejected` line for a cancel by id, a note naming the \
                 held rows it could not reach for a mass-cancel or a market-exit; run `{orders}` \
                 to see whether the order is gone"
            )
        }
        // A bracket's three ids are minted by the core AFTER this receipt, and a venue that cannot
        // hold one of its exits refuses the WHOLE group there (`bracket for <venue> REFUSED WHOLE`).
        WireCommand::Bracket(_) => {
            format!(
                "this is the receipt for the COMMAND: the core mints the bracket's three ids \
                 after the node answers, and refuses the whole group (three rejected legs) in the \
                 node's recent events when its venue cannot hold one leg. The legs, when the \
                 core's word is seen, follow as `leg:` lines; run `{orders}`/`{positions}` to \
                 observe the result"
            )
        }
        WireCommand::Submit(_) | WireCommand::Modify { .. } | WireCommand::Flatten { .. } => {
            format!(
                "run `{orders}`/`{positions}` to observe the result; the node's ControlLimits + \
                 RiskGate are the enforcing gate; this is only the receipt for the COMMAND: the \
                 node answers before its core acts, and a denial or a rejection shows up later, \
                 only in its recent events (the REPL's `recent`)"
            )
        }
        _ => format!(
            "run `{orders}`/`{positions}` to observe the result; the node's ControlLimits + \
             RiskGate are the enforcing gate"
        ),
    }
}

/// A one-line human description of a resolved [`WireCommand`] for the preview.
///
/// ⚠ `qty` and `price` go through [`verbs::fmt_num`], the SAME renderer the guardrail line under
/// this one uses. They must: the two lines print the same quantities, and a preview reading `qty=3`
/// above a guardrail reading `qty=3.0000000000000004` would leave the operator deciding which of the
/// tool's own two lines to believe.
pub(super) fn describe_command(cmd: &WireCommand) -> String {
    match cmd {
        // `coid=` is part of the preview because it is the HANDLE: it is what the operator types
        // back at `cancel <coid>` / `modify <coid>`, and (since the mint happens before this runs)
        // it is exactly the id that goes on the wire.
        //
        // ⚠ `account=` closes the Submit, Flatten, MarketExit and MassCancel lines, as it does the
        // Bracket and the mount: WHICH BOOK an order lands on is not a detail of the order. The
        // venue alone reads the same for `hyperliquid` and `hyperliquid/LIVE`, so the line a person
        // answers `y/N` to must say it. `-` is "the sender named no account" (not `DEFAULT`, which
        // is a name), the rendering the two lines above already use.
        WireCommand::Submit(o) => format!(
            "SUBMIT {} {} {} qty={} {}{}{}{} coid={} account={}",
            side_word(o.side),
            o.venue,
            o.symbol,
            verbs::fmt_num(o.qty),
            match o.price {
                Some(p) => format!("{} @ {}", o.order_type, verbs::fmt_num(p)),
                None => o.order_type.clone(),
            },
            // The number a trigger order fires at belongs in the line the operator confirms.
            o.trigger_price.map(|t| format!(" trigger={}", verbs::fmt_num(t))).unwrap_or_default(),
            if o.reduce_only { " [reduce-only]" } else { "" },
            // ...and so does the LIFETIME: `[post-only]` is the word that says this order will be
            // refused rather than take liquidity, `[gtd until <instant>]` when it ends. Nothing
            // for the default `gtc`, so the line of every order before the field reads as it did.
            match (&o.time_in_force, o.gtd_expiry) {
                (None, _) => String::new(),
                (Some(word), Some(ms)) => {
                    format!(" [{word} until {}]", vike_model::time::epoch_ms_to_utc_timestamp(ms))
                }
                (Some(word), None) => format!(" [{word}]"),
            },
            o.client_order_id,
            o.account.as_deref().unwrap_or("-"),
        ),
        // No REPL verb mints a bracket (the desktop's TP/SL ticket is its one producer), but the
        // preview renderer stays total for a command another caller built. `account=-` when the
        // bracket names none: the node then takes it only on a venue's one default account.
        WireCommand::Bracket(b) => format!(
            "BRACKET {} {} {} qty={} entry={} sl={} tp={} account={}",
            side_word(b.side),
            b.venue,
            b.symbol,
            verbs::fmt_num(b.qty),
            b.entry_price.map(verbs::fmt_num).unwrap_or_else(|| "market".to_string()),
            verbs::fmt_num(b.stop_loss),
            verbs::fmt_num(b.take_profit),
            b.account.as_deref().unwrap_or("-"),
        ),
        WireCommand::Cancel(coid) => format!("CANCEL {coid}"),
        WireCommand::Modify { client_order_id, new_qty, new_price } => format!(
            "MODIFY {client_order_id} qty={} price={}",
            new_qty.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string()),
            new_price.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string()),
        ),
        WireCommand::MassCancel { venue, symbol, account } => format!(
            "MASS-CANCEL venue={} symbol={} account={}",
            opt_str(venue),
            opt_str(symbol),
            account.as_deref().unwrap_or("-")
        ),
        WireCommand::Flatten { venue, symbol, account } => {
            format!("FLATTEN {venue} {symbol} account={}", account.as_deref().unwrap_or("-"))
        }
        WireCommand::MarketExit { venue, account } => format!(
            "MARKET-EXIT venue={} account={}",
            opt_str(venue),
            account.as_deref().unwrap_or("-")
        ),
        // A node-wide word that is not `Halted` REPLACES every account's state, the ones an operator set
        // one by one with `trade account-state` included (a kept-up lane twin's closing-only hold is the one
        // exception); the preview says so, because the confirm is the last word before they are reopened.
        WireCommand::SetTradingState(s) if *s != WireTradingState::Halted => format!(
            "SET-STATE {s:?} - EVERY account, including any halted or reduced on its own with \
             `trade account-state`: a node-wide word replaces them all (only a lane twin kept up to \
             be closed stays closing-only). The node names the accounts it reopens in its log"
        ),
        WireCommand::SetTradingState(s) => format!("SET-STATE {s:?}"),
        // No `trade` REPL verb PRODUCES this yet (split-plane B4 wired the wire form + daemon
        // lowering; a CLI spelling is future work), but the preview renderer must stay total: show
        // the target mount and the raw params JSON, never panic on a verb another caller minted.
        WireCommand::UpdateParams { venue, symbol, interval, mount_id, params } => {
            // The mount id is rendered like the mount verbs' account: two mounts on one series are
            // told apart by nothing else. Absent renders `-` (the sender named no mount).
            format!(
                "UPDATE-PARAMS {venue} {symbol} {interval} id={} {params}",
                mount_id.as_deref().unwrap_or("-")
            )
        }
        // The B5 mount verbs — spelled by [`parse_mount`] / [`parse_unmount`] since the lifecycle
        // grammar landed. `params` renders as the JSON object that goes on the wire, not a summary
        // of it: this line is the last thing between the operator and a strategy trading their
        // account, and a preview that elides the knobs is a preview of a different mount.
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
            // ⚠ The ACCOUNT is rendered for the reason stated just above about the knobs, only
            // sharper: a mount is not one order, it is every order that strategy will ever place,
            // and WHICH BOOK it places them on is the one thing this line cannot leave out. Absent
            // renders as `-` rather than as `default` — the operator named nothing, and printing a
            // book name they did not choose would be a reassurance the node has not agreed to (on
            // a two-engine venue it will refuse this mount outright).
            format!(
                "MOUNT-STRATEGY {venue} account={} {symbol} {interval} id={} source={} {params}",
                account.as_deref().unwrap_or("-"),
                controller_id.as_deref().unwrap_or("-"),
                name.as_deref().or(rhai.as_deref()).unwrap_or("-"),
            )
        }
        WireCommand::UnmountStrategy { controller_id } => {
            format!("UNMOUNT-STRATEGY {controller_id}")
        }
        // The REQ-7 settings write — spelled by [`parse_set_setting`]. Show the ASSIGNMENT, the one
        // thing it is: a row named by its key. ⚠ The two file-era wire fields are not rendered:
        // `file` is derived from the key (so printing it repeats the key's first word), and a
        // `confirm` — which only an older client fills — is ignored by the node since
        // `docs/decisions/0086` point 7, so a badge for it would advertise a guard that does not
        // exist. The `old → new` beside this line needs the node's current value, so [`run_write`]
        // prints it from a node read.
        WireCommand::SetSetting { key, value, .. } => format!("SET-SETTING {key} = {value}"),
        // `trade recon confirm`. The held row's own words (kind, book, detail) are printed by the
        // verb from the node's snapshot BEFORE this line; the wire form carries only the id.
        WireCommand::ReconConfirm { id } => format!(
            "RECON-CONFIRM #{id} - the node folds the fix `trade recon ls` shows for it into that \
             row's account"
        ),
        // `trade recon cancel`. The scope is SAID in the one line the operator reads before `y`: one
        // venue order, on a demo account, sent by the id the row carries, and the row stays until a pass
        // no longer raises it.
        WireCommand::ReconCancel { id } => format!(
            "RECON-CANCEL #{id} - send ONE cancel to the venue for the order the held row names \
             (demo accounts only; the row clears at the next reconcile pass if the venue cancelled it)"
        ),
        // `trade resume-venue`. The override is SHOUTED: a forced resume trades on a link the
        // dead-man believes is down, and says so in the one line the operator reads before `y`.
        WireCommand::ResumeVenue { venue, force } => format!(
            "RESUME-VENUE {venue} - release the link dead-man's halt of {venue} ONLY (other venues \
             and the drawdown latch stay as they are){}",
            if *force { "; FORCE: resume even if a link is still down" } else { "" }
        ),
        // `trade account-state`. The book and the state, and the scope SAID: this is one engine, not
        // the node-wide `halt`/`resume` it sits beside, and a `resume` of it never lifts what the
        // node holds (the drawdown latch, a node-wide halt: only `trade resume` clears those).
        WireCommand::SetAccountTradingState { venue, account, state } => format!(
            "ACCOUNT-STATE {venue}/{account} -> {state:?} - ONE account's engine only; every other \
             account keeps its state, and what the node holds (a latch, a node-wide halt) is never \
             lowered by this"
        ),
        // `trade unadmit`. The book and the symbol, and what it does NOT do: it writes nothing at the venue and
        // the engine refuses a symbol it still holds a position or an order in.
        WireCommand::UnadmitSymbol { venue, account, symbol } => format!(
            "UNADMIT {venue}/{account} {symbol} - GIVE BACK one admitted symbol of ONE account's engine, \
             freeing its slot; nothing is written at the venue, and the engine refuses it while it holds \
             a position or an order in the symbol"
        ),
        // `trade mount-unlatch`. The mount, and what it does and does not do (decision 0239): it clears ONE mount's
        // budget latch only when the mount is flat and has no live order, re-arms the loss budget from where the
        // mount stands, and leaves the account's state, every other mount and every venue alone.
        WireCommand::UnlatchMount { mount_id } => format!(
            "MOUNT-UNLATCH {mount_id} - clear the budget LATCH of ONE mount so it trades again; refused while it \
             holds a position or a live order, and its loss budget then counts from the mount's current realized \
             PnL (the node says the new base); the account's state and every other mount are untouched"
        ),
    }
}

/// `pub(crate)`: shared with `crate::cmd::trade::render`'s order-row projection, so the two
/// renderers cannot come to disagree about what a side prints as.
pub(crate) fn side_word(side: i32) -> &'static str {
    if side >= 0 { "buy" } else { "sell" }
}

/// `pub(crate)`: shared with `crate::cmd::trade::render`, for the same reason as [`side_word`].
///
/// ⚠ Through [`verbs::fmt_num`], like the preview line above the table: the engine holds a price as
/// `round_to_step`'s `n * tick`, one ULP off the grid point it names (a BTC-USDT-SWAP limit at
/// `80925.93` is `80925.93000000001` in the order table, while the venue holds `80925.93` and the wire
/// string is the Decimal grid multiple), and `f64::to_string` prints every digit of that.
pub(crate) fn opt_num(v: &Option<f64>) -> String {
    v.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string())
}

fn opt_str(v: &Option<String>) -> String {
    v.clone().unwrap_or_else(|| "*".to_string())
}

/// A `[seq N]` staleness stamp for a snapshot render (the disconnected note is printed separately by
/// [`with_snapshot`] so it shows even when a render function does not include the stamp). A frame
/// that carries nothing built ([`is_pre_fold`]) is stamped `[seq 0 PRE-FOLD]`, so the mark rides
/// on every table header and a `(none)` below it cannot read as "there are none".
pub(super) fn stamp(snap: &WireSnapshot) -> String {
    if is_pre_fold(snap) {
        format!("[seq {} PRE-FOLD]", snap.seq)
    } else {
        format!("[seq {}]", snap.seq)
    }
}

pub(super) fn print_orders(snap: &WireSnapshot, symbol: Option<&str>) {
    println!("{}", orders_table(snap, symbol));
}

/// Render the orders view. Returns the text rather than printing it so the two column rules below
/// are testable — the difference between a coid an operator can type back at `cancel` and one they
/// cannot is not a cosmetic property.
pub(super) fn orders_table(snap: &WireSnapshot, symbol: Option<&str>) -> String {
    let mut out = format!("{} orders", stamp(snap));
    let rows: Vec<_> = snap
        .orders
        .iter()
        .filter(|o| symbol.is_none_or(|s| o.symbol.eq_ignore_ascii_case(s)))
        .collect();
    if rows.is_empty() {
        out.push_str("\n  (none)");
        return out;
    }
    // The coid column is NEVER truncated. It is the HANDLE — what the operator reads off this table
    // and types back at `cancel <coid>` / `modify <coid>` — so a shortened one does not make the
    // table ugly, it makes it unusable. It was a fixed 20 while `submit --coid` accepts up to 32
    // (`vike_model::is_valid_crypto_coid`), and a node may report an id this side never minted.
    let wc = width_of("coid", rows.iter().map(|o| o.client_order_id.as_str()), None);
    // The symbol column IS capped — no width fits a Polymarket token id (a decimal uint256, ~77
    // digits) — but it is truncated in the MIDDLE. Head-only truncation rendered `DUMMYTOKEN000…`,
    // which identifies no order at all: the token ids of one up/down family share a long prefix and
    // differ at the TAIL, so the end is the half that carries the identity.
    let ws = width_of("symbol", rows.iter().map(|o| o.symbol.as_str()), Some(SYMBOL_MAX));
    out.push_str(&format!(
        "\n  {:<wc$} {:<10} {:<ws$} {:<5} {:>12} {:<10} {:>12} {:<12} {:>12}",
        "coid", "venue", "symbol", "side", "qty", "type", "price", "status", "filled"
    ));
    for o in rows {
        out.push_str(&format!(
            "\n  {:<wc$} {:<10} {:<ws$} {:<5} {:>12} {:<10} {:>12} {:<12} {:>12}",
            o.client_order_id,
            trunc(&o.venue, 10),
            trunc_mid(&o.symbol, ws),
            side_word(o.side),
            verbs::fmt_num(o.qty),
            trunc(&o.order_type, 10),
            opt_num(&o.price),
            trunc(&o.status, 12),
            verbs::fmt_num(o.filled_qty),
        ));
    }
    out
}

pub(super) fn print_positions(snap: &WireSnapshot, venue: Option<&str>) {
    println!("{} positions", stamp(snap));
    let rows: Vec<_> = snap
        .positions
        .iter()
        .filter(|p| venue.is_none_or(|v| p.venue.eq_ignore_ascii_case(v)))
        .collect();
    if rows.is_empty() {
        println!("  (none)");
        return;
    }
    // Same symbol rule as the orders table above, and for the same reason — a position in a
    // Polymarket token is as unidentifiable from a 14-char prefix as an order in one.
    let ws = width_of("symbol", rows.iter().map(|p| p.symbol.as_str()), Some(SYMBOL_MAX));
    println!(
        "  {:<10} {:<ws$} {:<6} {:>12} {:>12} {:>12} {:>8} {:>12}",
        "venue", "symbol", "side", "size", "avg_px", "unreal", "lev", "liq"
    );
    for p in rows {
        println!(
            "  {:<10} {:<ws$} {:<6} {:>12} {:>12} {:>12} {:>8} {:>12}",
            trunc(&p.venue, 10),
            trunc_mid(&p.symbol, ws),
            trunc(&p.position_side, 6),
            verbs::fmt_num(p.size),
            verbs::fmt_num(p.avg_px),
            verbs::fmt_num(p.unrealized),
            p.leverage,
            p.liq_price,
        );
    }
}

pub(super) fn print_equity(snap: &WireSnapshot) {
    println!("{}", equity_text(snap));
}

/// The distinct currency groups of the frame's blocks (`WireVenueBlock::ledger_ccy`, EMPTY = the dollar
/// group), the PRIMARY first (the key the node states, `WireSnapshot::primary_ccy`: NOT the first block's,
/// which after a primary restart is another engine's), each with its blocks. One element for a node whose
/// engines share a currency, which is every node today.
fn ledger_groups(
    snap: &WireSnapshot,
) -> Vec<(&str, Vec<&vike_tradehub_client::wire::WireVenueBlock>)> {
    let mut groups: Vec<(&str, Vec<_>)> = Vec::new();
    for v in &snap.venues {
        match groups.iter_mut().find(|(ccy, _)| *ccy == v.ledger_ccy) {
            Some((_, blocks)) => blocks.push(v),
            None => groups.push((v.ledger_ccy.as_str(), vec![v])),
        }
    }
    if !groups.is_empty() {
        // The stated primary leads, whether or not a block of it is in the frame.
        let at = groups.iter().position(|(ccy, _)| *ccy == snap.primary_ccy);
        let lead = at.map_or_else(|| (snap.primary_ccy.as_str(), Vec::new()), |i| groups.remove(i));
        groups.insert(0, lead);
    }
    groups
}

/// Render the equity view. Returns the text rather than printing it so the currency rule is testable.
///
/// A total across currencies is no money (decision 0183, item 13): `equity_total` is the PRIMARY
/// block's currency group alone (the node's own figure), and a frame with blocks in several currencies
/// adds one line per currency with that group's totals, never a sum across them. A node of one currency
/// prints exactly what it always did.
pub(super) fn equity_text(snap: &WireSnapshot) -> String {
    let code = |ccy: &str| if ccy.is_empty() { "USD".to_string() } else { ccy.to_string() };
    let groups = ledger_groups(snap);
    let mut out = format!("{} equity", stamp(snap));
    out.push_str(&format!("\n  balance (primary): {}", snap.balance));
    if groups.len() > 1 {
        out.push_str(&format!(
            "\n  equity_total:      {} ({} only, the primary's currency)",
            snap.equity_total,
            code(groups[0].0)
        ));
        for (ccy, blocks) in &groups {
            out.push_str(&format!(
                "\n  {:<4} equity {} realized {} unreal {} margin_used {}",
                code(ccy),
                vike_model::py_sum(blocks.iter().map(|v| v.equity)),
                vike_model::py_sum(blocks.iter().map(|v| v.realized_pnl)),
                vike_model::py_sum(blocks.iter().map(|v| v.unrealized)),
                blocks.iter().fold(0.0, |acc, v| acc + v.margin_used),
            ));
        }
    } else {
        out.push_str(&format!("\n  equity_total:      {}", snap.equity_total));
    }
    if !snap.venues.is_empty() {
        let multi = groups.len() > 1;
        out.push_str(&format!(
            "\n  {:<10} {:>12} {:>12} {:>12} {:>12} {:>12}{}",
            "venue",
            "balance",
            "realized",
            "equity",
            "unreal",
            "free_bp",
            if multi { " ccy" } else { "" }
        ));
        for v in &snap.venues {
            out.push_str(&format!(
                "\n  {:<10} {:>12} {:>12} {:>12} {:>12} {:>12}{}",
                trunc(&v.venue, 10),
                v.balance,
                v.realized_pnl,
                v.equity,
                v.unrealized,
                v.free_bp,
                if multi { format!(" {}", code(&v.ledger_ccy)) } else { String::new() },
            ));
        }
    }
    // A running account whose key in the database is no longer the one its session started with.
    for route_key in &snap.key_changed {
        out.push_str(&format!("\n  {route_key}: {KEY_CHANGED_WARNING}"));
    }
    // An account whose venue balance the node holds back from its engine: its equity above is the engine's own
    // book (seed + PnL), not the venue's wallet, and this is the only place that says so.
    for held in &snap.balance_held {
        out.push_str(&format!("\n  {}", held.line()));
    }
    // An engine whose ledger currency was only the owner's ASSUMED USD when it started (decision 0189): every figure
    // above is in a currency the venue had not stated. It stays so for the engine's life (a later confirmation lifts
    // the balance hold, which has its own line, and does not clear this). Nothing is said for a frame that reports
    // none (or an older node's).
    for v in snap.venues.iter().filter(|v| v.ccy_assumed) {
        out.push_str(&format!(
            "\n  {}: ledger currency {} was ASSUMED when this engine started (the venue stated none); it stays so until the account is started again",
            who_of(v),
            code(&v.ledger_ccy)
        ));
    }
    // The conversion rate behind every figure of an account that holds a symbol quoted in another currency
    // (decision 0183): `STALE` is the node's own verdict (past its staleness window), and a stale rate that
    // values an open position says so. Nothing is said for an account with no such symbol, nor for a node
    // that publishes no table.
    for v in snap.venues.iter().filter(|v| !v.quote_rates.is_empty()) {
        let rates: Vec<String> = v.quote_rates.iter().map(|r| r.say(&v.quote_rates_in)).collect();
        out.push_str(&format!("\n  {}: conversion rates: {}", who_of(v), rates.join("; ")));
    }
    out
}

/// What names an account's block on a line: its route key, the bare venue on a node that publishes none.
fn who_of(v: &vike_tradehub_client::wire::WireVenueBlock) -> &str {
    if v.route_key.is_empty() { v.venue.as_str() } else { v.route_key.as_str() }
}

pub(super) fn print_recent(snap: &WireSnapshot, n: Option<usize>) {
    println!("{} recent events", stamp(snap));
    let events = &snap.recent_events;
    if events.is_empty() {
        println!("  (none)");
        return;
    }
    let take = n.unwrap_or(events.len()).min(events.len());
    for e in &events[events.len() - take..] {
        println!("  {e}");
    }
}

/// The MODE block: the trading state, prefixed with this frame's staleness stamp, plus the fault
/// line when the core is in safe state.
///
/// The WORDING is [`crate::cmd::trade::status::mode_lines`]', not this file's — three surfaces render
/// the mode (`status` here, `snapshot` here, and the one-shot `vike-cli trade status`) and they may
/// not describe a kill switch differently. What stays here is the `[seq N]` prefix, which is a REPL
/// fact: this snapshot arrived by PUSH and may be old, while the one-shot's is a fresh round trip.
pub(super) fn print_state(snap: &WireSnapshot) {
    let mut lines =
        crate::cmd::trade::status::mode_lines(snap.trading_state, snap.fault.as_deref())
            .into_iter();
    if let Some(first) = lines.next() {
        println!("{} {first}", stamp(snap));
    }
    let held = crate::cmd::trade::status::balance_held_lines(snap);
    for line in lines.chain(crate::cmd::trade::status::venue_state_lines(snap)).chain(held) {
        println!("{line}");
    }
}

pub(super) fn print_snapshot(snap: &WireSnapshot) {
    print_state(snap);
    print_equity(snap);
    print_orders(snap, None);
    print_positions(snap, None);
    print_recent(snap, Some(5));
}

/// The widest a SYMBOL cell may grow before [`trunc_mid`] shortens it. A cap is unavoidable — a
/// Polymarket token id is a decimal uint256, ~77 digits, and one of them would push every column
/// after it off an 80-column terminal — but it is generous enough that no ordinary instrument
/// (`BTCUSDT`, `EUR/USD`, `BTC-30AUG26-120000-C`) is touched at all.
const SYMBOL_MAX: usize = 24;

/// Truncate a string to `max` chars for column display (keeps the aligned tables from smearing).
fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Truncate keeping BOTH ends — `1234567…8901` — for cells that are IDENTIFIERS rather than labels.
///
/// [`trunc`]'s head-only form is right for a venue or a status, where the first characters name the
/// thing. It is wrong for a symbol: Polymarket token ids are long decimal strings, the tokens of one
/// up/down family share a long prefix, and they differ at the TAIL — so a head-only cell rendered
/// `DUMMYTOKEN000…` for every order in the family and identified none of them.
pub(super) fn trunc_mid(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 1 {
        // Nothing but the marker fits — and at 0 not even that.
        return "…".chars().take(max).collect();
    }
    let keep = max - 1; // one column for the ellipsis
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(n - tail));
    out
}
