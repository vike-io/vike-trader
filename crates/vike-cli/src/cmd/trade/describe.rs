//! Human rendering: the one-line preview of a wire command, and the REPL's snapshot tables.

use vike_model::{TradingState, ledger_label};
use vike_tradehub_client::wire::{
    CommandWire, CoreSnapshotWire, KEY_CHANGED_WARNING, book_of_address,
};

use vike_utility::number_text::format_number;

use super::render::{
    HELD, attribute_words, emulated_rows, emulated_table, held_row, held_words, order_row,
};
use super::{is_pre_fold, width_of};

// ---- human rendering (of the pure command / of the snapshot) ---------------------------------

/// What an `accepted by the node (order client id ...)` line says after the dash, for the surface that prints it:
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
pub(super) fn accepted_tail(cmd: &CommandWire, orders: &str, positions: &str) -> String {
    match cmd {
        CommandWire::Cancel(_)
        | CommandWire::MassCancel { .. }
        | CommandWire::MarketExit { .. } => {
            format!(
                "this is the receipt for the COMMAND, not for the cancel: the node answers before \
                 it acts, and it cancels at the venue only an order its registry holds. For any \
                 other order it sends nothing and says so in its recent events (the REPL's \
                 `recent`): an `OrderCancelRejected` line for a cancel by id, a note naming the \
                 held rows it could not reach for a mass-cancel or a close-all; run `{orders}` \
                 to see whether the order is gone"
            )
        }
        // A bracket's three ids are minted by the core AFTER this receipt, and a venue that cannot
        // hold one of its exits refuses the WHOLE group there (`bracket for <venue> REFUSED WHOLE`).
        CommandWire::Bracket(_) => {
            format!(
                "this is the receipt for the COMMAND: the core mints the bracket's three ids \
                 after the node answers, and refuses the whole group (three rejected legs) in the \
                 node's recent events when its venue cannot hold one leg. The legs, when the \
                 core's word is seen, follow as `leg:` lines; run `{orders}`/`{positions}` to \
                 observe the result"
            )
        }
        // An order list's legs carry the ids this client minted (the receipt names them), and the core refuses the
        // WHOLE list after it when its venue cannot hold a leg.
        CommandWire::OrderList(_) => {
            format!(
                "this is the receipt for the COMMAND: the core refuses the whole list in the node's \
                 recent events when its venue cannot hold a leg; the legs, when the core's word is seen, \
                 follow as `leg:` lines; run `{orders}`/`{positions}` to observe the result"
            )
        }
        // A combo goes under the one id this client minted; a venue whose row takes no combo, or an engine that fills
        // no limit per leg, rejects it after this receipt.
        CommandWire::Combo(_) => {
            format!(
                "this is the receipt for the COMMAND: the core places the combo under its id and \
                 rejects it in the node's recent events when its venue takes no combo; run \
                 `{orders}`/`{positions}` to observe the result (a resend under the same id is refused)"
            )
        }
        CommandWire::Submit(_) | CommandWire::Modify { .. } | CommandWire::Flatten { .. } => {
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

/// A one-line human description of a resolved [`CommandWire`] for the preview.
///
/// ⚠ `qty` and `price` go through [`format_number`], the SAME renderer the guardrail line under
/// this one uses. They must: the two lines print the same quantities, and a preview reading `qty=3`
/// above a guardrail reading `qty=3.0000000000000004` would leave the operator deciding which of the
/// tool's own two lines to believe.
pub(super) fn describe_command(cmd: &CommandWire) -> String {
    match cmd {
        // `order_client_id=` is part of the preview because it is the HANDLE: it is what the operator types
        // back at `cancel <order client id>` / `modify <order client id>`, and (since the mint happens before this runs)
        // it is exactly the id that goes on the wire.
        //
        // ⚠ `account=` closes the Submit, Flatten, MarketExit and MassCancel lines, as it does the
        // Bracket and the mount: WHICH BOOK an order lands on is not a detail of the order. The
        // venue alone reads the same for `hyperliquid` and `hyperliquid/LIVE`, so the line a person
        // answers `y/N` to must say it. `-` is "the sender named no account" (not `DEFAULT`, which
        // is a name), the rendering the two lines above already use.
        CommandWire::Submit(o) => format!(
            "SUBMIT {} {} {} qty={} {}{}{}{}{}{}{}{}{} order_client_id={} account={}",
            side_word(o.side),
            o.venue,
            o.symbol,
            format_number(o.qty),
            match o.price {
                Some(p) => format!("{} @ {}", o.order_type, format_number(p)),
                None => o.order_type.clone(),
            },
            // The number a trigger order fires at belongs in the line the operator confirms.
            o.trigger_price.map(|t| format!(" trigger={}", format_number(t))).unwrap_or_default(),
            // ...and a trailing order's trail and limit offset. Nothing when absent.
            [("trail", o.trail), ("offset", o.limit_offset)]
                .iter()
                .filter_map(|(word, v)| v.map(|v| format!(" {word}={}", format_number(v))))
                .collect::<String>(),
            // ...or its trail as a percent.
            o.trail_percent.map(|p| format!(" trail={}%", format_number(p))).unwrap_or_default(),
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
            // ...and what the venue shows of it, and when the node sends it. Nothing when absent.
            o.display_quantity
                .map(|d| format!(" display={}", format_number(d)))
                .unwrap_or_default(),
            // ...and how a limit fills and what a stop becomes, in the order tables' words.
            attribute_words(o.attributes.as_ref(), o.adjust.as_ref())
                .iter()
                .map(|w| format!(" [{w}]"))
                .collect::<String>(),
            o.release.as_ref().map(|r| format!(" [{}]", held_words(r))).unwrap_or_default(),
            o.order_client_id,
            o.account.as_deref().unwrap_or("-"),
        ),
        // No REPL verb mints a bracket (the desktop's TP/SL ticket is its one producer), but the
        // preview renderer stays total for a command another caller built. `account=-` when the
        // bracket names none: the node then takes it only on a venue's one default account.
        // The group stem only when one was minted (a node offering `bracket-stem`), so every other line reads as before.
        CommandWire::Bracket(b) => format!(
            "BRACKET {} {} {} qty={} entry={} stop-loss={} take-profit={} account={}{}",
            side_word(b.side),
            b.venue,
            b.symbol,
            format_number(b.qty),
            b.entry_price.map(format_number).unwrap_or_else(|| "market".to_string()),
            format_number(b.stop_loss),
            format_number(b.take_profit),
            b.account.as_deref().unwrap_or("-"),
            b.order_client_id.as_deref().map(|stem| format!(" group={stem}")).unwrap_or_default(),
        ),
        // Every leg, with its kind, prices and links: the line the operator confirms is the whole list.
        CommandWire::OrderList(l) => l.say(),
        // Every leg with its signed ratio and limit, and the net: the line the operator confirms is the whole combo.
        CommandWire::Combo(c) => c.say(),
        CommandWire::Cancel(order_client_id) => format!("CANCEL {order_client_id}"),
        CommandWire::Modify { order_client_id, new_qty, new_price, new_trigger_price } => format!(
            "MODIFY {order_client_id} qty={} price={}{}",
            new_qty.map(format_number).unwrap_or_else(|| "-".to_string()),
            new_price.map(format_number).unwrap_or_else(|| "-".to_string()),
            // Only when named, so the line of every modify before the field reads as it did.
            new_trigger_price.map(|t| format!(" trigger={}", format_number(t))).unwrap_or_default(),
        ),
        CommandWire::MassCancel { venue, symbol, account } => format!(
            "MASS-CANCEL venue={} symbol={} account={}",
            opt_str(venue),
            opt_str(symbol),
            account.as_deref().unwrap_or("-")
        ),
        CommandWire::Flatten { venue, symbol, account } => {
            format!("FLATTEN {venue} {symbol} account={}", account.as_deref().unwrap_or("-"))
        }
        CommandWire::MarketExit { venue, account } => format!(
            "MARKET-EXIT venue={} account={}",
            opt_str(venue),
            account.as_deref().unwrap_or("-")
        ),
        // A node-wide word that is not `Halted` REPLACES every account's state, the ones an operator set
        // one by one with `trade account` included (a kept-up lane twin's closing-only hold is the one
        // exception); the preview says so, because the confirm is the last word before they are reopened.
        CommandWire::SetTradingState(s) if *s != TradingState::Halted => format!(
            "SET-STATE {} - EVERY account, including any halted or reduced on its own with \
             `trade account`: a node-wide word replaces them all (only a lane twin kept up to \
             be closed stays closing-only). The node names the accounts it reopens in its log",
            s.word()
        ),
        CommandWire::SetTradingState(s) => format!("SET-STATE {}", s.word()),
        // No `trade` REPL verb PRODUCES this yet (split-plane B4 wired the wire form + daemon
        // lowering; a CLI spelling is future work), but the preview renderer must stay total: show
        // the target mount and the raw params JSON, never panic on a verb another caller minted.
        CommandWire::UpdateParams { venue, symbol, interval, mount_id, params } => {
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
        // The retired `name` and `rhai` are sent by no client (`vike_client::verbs::Verb::to_wire_command`).
        CommandWire::MountStrategy {
            venue,
            account,
            symbol,
            interval,
            controller_id,
            file,
            params,
            ..
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
                file.as_deref().unwrap_or("-"),
            )
        }
        CommandWire::UnmountStrategy { controller_id } => {
            format!("UNMOUNT-STRATEGY {controller_id}")
        }
        // The REQ-7 settings write — spelled by [`parse_set_setting`]. Show the ASSIGNMENT, the one
        // thing it is: a row named by its key. ⚠ The two file-era wire fields are not rendered:
        // `file` is derived from the key (so printing it repeats the key's first word), and a
        // `confirm` — which only an older client fills — is ignored by the node since
        // `docs/decisions/0086` point 7, so a badge for it would advertise a guard that does not
        // exist. The `old → new` beside this line needs the node's current value, so [`run_write`]
        // prints it from a node read.
        CommandWire::SetSetting { key, value, .. } => format!("SET-SETTING {key} = {value}"),
        // `trade reconcile confirm`. The held row's own words (kind, book, detail) are printed by the
        // verb from the node's snapshot BEFORE this line; the wire form carries only the id.
        CommandWire::ReconcileConfirm { id } => format!(
            "RECONCILE-CONFIRM #{id} - the node folds the fix `trade reconcile ls` shows for it into \
             that row's account"
        ),
        // `trade reconcile cancel`. The scope is SAID in the one line the operator reads before `y`: one
        // venue order, on a demo account, sent by the id the row carries, and the row stays until a pass
        // no longer raises it.
        CommandWire::ReconcileCancel { id } => format!(
            "RECONCILE-CANCEL #{id} - send ONE cancel to the venue for the order the held row names \
             (demo accounts only; the row clears at the next reconcile pass if the venue cancelled it)"
        ),
        // `trade venue resume`. The override is SHOUTED: a forced resume trades on a link the
        // dead-man believes is down, and says so in the one line the operator reads before `y`.
        CommandWire::ResumeVenue { venue, force } => format!(
            "RESUME-VENUE {venue} - release the link dead-man's halt of {venue} ONLY (other venues \
             and the drawdown latch stay as they are){}",
            if *force { "; FORCE: resume even if a link is still down" } else { "" }
        ),
        // `trade account halt|reduce|resume`. The book and the state, and the scope SAID: this is one engine, not
        // the node-wide `halt`/`resume` it sits beside, and a `resume` of it never lifts what the
        // node holds (the drawdown latch, a node-wide halt: only `trade resume` clears those).
        CommandWire::SetAccountTradingState { venue, account, state } => format!(
            "ACCOUNT-{} {} - ONE account's engine only; every other \
             account keeps its state, and what the node holds (a latch, a node-wide halt) is never \
             lowered by this",
            match state {
                TradingState::Active => "RESUME",
                TradingState::Reducing => "REDUCE",
                TradingState::Halted => "HALT",
            },
            book_of_address(venue, account.as_deref()),
        ),
        // `trade account remove-symbol`. The book and the symbol, and what it does NOT do: it writes nothing at the venue and
        // the engine refuses a symbol it still holds a position or an order in.
        CommandWire::UnadmitSymbol { venue, account, symbol } => format!(
            "ACCOUNT-REMOVE-SYMBOL {} {symbol} - GIVE BACK one admitted symbol of ONE account's engine, \
             freeing its slot; nothing is written at the venue, and the engine refuses it while it holds \
             a position or an order in the symbol",
            book_of_address(venue, account.as_deref()),
        ),
        // `trade strategy reset-budget`. The mount, and what it does and does not do (decision 0239): it clears ONE mount's
        // budget latch only when the mount is flat and has no live order, re-arms the loss budget from where the
        // mount stands, and leaves the account's state, every other mount and every venue alone.
        CommandWire::UnlatchMount { mount_id } => format!(
            "STRATEGY-RESET-BUDGET {mount_id} - clear the budget LATCH of ONE mount so it trades again; refused while it \
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
/// ⚠ Through [`format_number`], like the preview line above the table: the engine holds a price as
/// `round_to_step`'s `n * tick`, one ULP off the grid point it names (a BTC-USDT-SWAP limit at
/// `80925.93` is `80925.93000000001` in the order table, while the venue holds `80925.93` and the wire
/// string is the Decimal grid multiple), and `f64::to_string` prints every digit of that.
pub(crate) fn opt_number(v: &Option<f64>) -> String {
    v.map(format_number).unwrap_or_else(|| "-".to_string())
}

fn opt_str(v: &Option<String>) -> String {
    v.clone().unwrap_or_else(|| "*".to_string())
}

/// A `[seq N]` staleness stamp for a snapshot render (the disconnected note is printed separately by
/// [`with_snapshot`] so it shows even when a render function does not include the stamp). A frame
/// that carries nothing built ([`is_pre_fold`]) is stamped `[seq 0 PRE-FOLD]`, so the mark rides
/// on every table header and a `(none)` below it cannot read as "there are none".
pub(super) fn stamp(snap: &CoreSnapshotWire) -> String {
    if is_pre_fold(snap) {
        format!("[seq {} PRE-FOLD]", snap.sequence)
    } else {
        format!("[seq {}]", snap.sequence)
    }
}

/// The friendly words of a v2 order client id, `vike_client::verbs::decode_order_client_id`'s (the account's label when `frame`
/// names it), or `None` for a v1 or foreign id, which shows as it always did. `pub(crate)`: `render`'s `rows_table`
/// (`trade order ls`) prints them too.
pub(crate) fn order_client_id_words(id: &str, frame: Option<&CoreSnapshotWire>) -> Option<String> {
    let doc = vike_client::verbs::decode_order_client_id(id, frame, &[]);
    (doc["form"] == "v2").then(|| doc["words"].as_str().unwrap_or_default().to_string())
}

pub(super) fn print_orders(snap: &CoreSnapshotWire, symbol: Option<&str>) {
    println!("{}", orders_table(snap, symbol));
}

/// Render the orders view. Returns the text rather than printing it so the two column rules below
/// are testable — the difference between an order client id an operator can type back at `cancel` and one they
/// cannot is not a cosmetic property.
pub(super) fn orders_table(snap: &CoreSnapshotWire, symbol: Option<&str>) -> String {
    let mut out = format!("{} orders", stamp(snap));
    let rows: Vec<_> = snap
        .orders
        .iter()
        .filter(|o| symbol.is_none_or(|s| o.symbol.eq_ignore_ascii_case(s)))
        .collect();
    // The orders the node HOLDS until a release (not yet at a venue), after the rest; a held bracket exit stays
    // out, as before.
    let held: Vec<_> = snap
        .held_exits
        .iter()
        .filter(|h| h.release.is_some())
        .filter(|h| symbol.is_none_or(|s| h.symbol.eq_ignore_ascii_case(s)))
        .collect();
    // Every order the node emulates, once, below the rest (`trade order ls`'s block, indented as this table is);
    // nothing when it emulates none.
    let emulated = emulated_rows(snap, None, symbol);
    let mut block = String::new();
    if !emulated.is_empty() {
        for line in emulated_table(&emulated).lines() {
            block.push_str("\n  ");
            block.push_str(line);
        }
    }
    if rows.is_empty() && held.is_empty() {
        out.push_str("\n  (none)");
        out.push_str(&block);
        return out;
    }
    // The order client id column is NEVER truncated. It is the HANDLE — what the operator reads off this table
    // and types back at `cancel <order client id>` / `modify <order client id>` — so a shortened one does not make the
    // table ugly, it makes it unusable. It was a fixed 20 while `submit --order-client-id` accepts up to 32
    // (`vike_model::is_valid_crypto_order_client_id`), and a node may report an id this side never minted.
    let order_client_ids = rows.iter().map(|o| o.order_client_id.as_str());
    let wc = width_of(
        "order_client_id",
        order_client_ids.chain(held.iter().map(|h| h.order_client_id.as_str())),
        None,
    );
    // A v2 id's words beside it, with the account's label off this frame ([`order_client_id_words`]); the column is there only
    // when a row has words, so a table of v1 or foreign ids reads as it always did.
    let ids = rows.iter().map(|o| o.order_client_id.as_str());
    let words: std::collections::HashMap<&str, String> = ids
        .chain(held.iter().map(|h| h.order_client_id.as_str()))
        .filter_map(|id| Some((id, order_client_id_words(id, Some(snap))?)))
        .collect();
    let ww =
        (!words.is_empty()).then(|| width_of("words", words.values().map(String::as_str), None));
    let lead = |id: &str, said: Option<&str>| match ww {
        None => format!("{id:<wc$}"),
        Some(ww) => {
            format!("{id:<wc$} {:<ww$}", said.or(words.get(id).map(String::as_str)).unwrap_or("-"))
        }
    };
    // The symbol column IS capped — no width fits a Polymarket token id (a decimal uint256, ~77
    // digits) — but it is truncated in the MIDDLE. Head-only truncation rendered `DUMMYTOKEN000…`,
    // which identifies no order at all: the token ids of one up/down family share a long prefix and
    // differ at the TAIL, so the end is the half that carries the identity.
    let symbols = rows.iter().map(|o| o.symbol.as_str());
    let ws =
        width_of("symbol", symbols.chain(held.iter().map(|h| h.symbol.as_str())), Some(SYMBOL_MAX));
    out.push_str(&format!(
        "\n  {} {:<10} {:<ws$} {:<5} {:>12} {:<10} {:>12} {:<12} {:>12}",
        lead("order_client_id", Some("words")),
        "venue",
        "symbol",
        "side",
        "qty",
        "type",
        "price",
        "status",
        "filled"
    ));
    for o in rows {
        out.push_str(&format!(
            "\n  {} {:<10} {:<ws$} {:<5} {:>12} {:<10} {:>12} {:<12} {:>12}",
            lead(&o.order_client_id, None),
            trunc(&o.venue, 10),
            trunc_mid(&o.symbol, ws),
            side_word(o.side),
            format_number(o.qty),
            trunc(&o.order_type, 10),
            opt_number(&o.price),
            trunc(&o.status, 12),
            format_number(o.filled_qty),
        ));
        // A resting order's terms after its row, as a held order's release is (nothing on a plain order), in
        // `trade order ls`'s words; an emulated order's live child names its parent.
        let row = order_row(snap, o);
        if let Some(terms) = row.terms() {
            out.push_str("  ");
            out.push_str(&terms);
        }
        if let Some(parent) = &row.child_of {
            out.push_str("  child of ");
            out.push_str(parent);
        }
    }
    for h in held {
        out.push_str(&format!(
            "\n  {} {:<10} {:<ws$} {:<5} {:>12} {:<10} {:>12} {:<12} {:>12}  {}",
            lead(&h.order_client_id, None),
            trunc(&h.venue, 10),
            trunc_mid(&h.symbol, ws),
            side_word(h.side),
            format_number(h.qty),
            trunc(&h.order_type, 10),
            opt_number(&h.price.or(h.trigger_price)),
            HELD,
            "-",
            held_row(h).held_cell().unwrap_or_default(),
        ));
    }
    out.push_str(&block);
    out
}

pub(super) fn print_positions(snap: &CoreSnapshotWire, venue: Option<&str>) {
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
        "venue", "symbol", "side", "size", "average_price", "unreal", "lev", "liq"
    );
    for p in rows {
        println!(
            "  {:<10} {:<ws$} {:<6} {:>12} {:>12} {:>12} {:>8} {:>12}",
            trunc(&p.venue, 10),
            trunc_mid(&p.symbol, ws),
            trunc(&p.position_side, 6),
            format_number(p.size),
            format_number(p.average_price),
            format_number(p.unrealized),
            p.leverage,
            p.liquidation_price,
        );
    }
}

pub(super) fn print_equity(snap: &CoreSnapshotWire) {
    println!("{}", equity_text(snap));
}

/// The distinct currency groups of the frame's blocks (`VenueBlockWire::ledger_currency`, EMPTY = the dollar
/// group), the PRIMARY first (the key the node states, `CoreSnapshotWire::primary_currency`: NOT the first block's,
/// which after a primary restart is another engine's), each with its blocks. One element for a node whose
/// engines share a currency, which is every node today.
fn ledger_groups(
    snap: &CoreSnapshotWire,
) -> Vec<(&str, Vec<&vike_tradehub_client::wire::VenueBlockWire>)> {
    let mut groups: Vec<(&str, Vec<_>)> = Vec::new();
    for v in &snap.venues {
        match groups.iter_mut().find(|(currency, _)| *currency == v.ledger_currency) {
            Some((_, blocks)) => blocks.push(v),
            None => groups.push((v.ledger_currency.as_str(), vec![v])),
        }
    }
    if !groups.is_empty() {
        // The stated primary leads, whether or not a block of it is in the frame.
        let at = groups.iter().position(|(currency, _)| *currency == snap.primary_currency);
        let lead =
            at.map_or_else(|| (snap.primary_currency.as_str(), Vec::new()), |i| groups.remove(i));
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
pub(super) fn equity_text(snap: &CoreSnapshotWire) -> String {
    let groups = ledger_groups(snap);
    let mut out = format!("{} equity", stamp(snap));
    out.push_str(&format!("\n  balance (primary): {}", snap.balance));
    if groups.len() > 1 {
        out.push_str(&format!(
            "\n  equity_total:      {} ({} only, the primary's currency)",
            snap.equity_total,
            ledger_label(groups[0].0)
        ));
        for (currency, blocks) in &groups {
            let sums =
                vike_model::ledger_sums(blocks.iter().map(|v| vike_model::AccountLedgerBlock {
                    equity: v.equity,
                    realized_pnl: v.realized_pnl,
                    fees_paid: v.fees_paid,
                    funding_paid: v.funding_paid,
                    unrealized: v.unrealized,
                    margin_used: v.margin_used,
                }));
            out.push_str(&format!(
                "\n  {:<4} equity {} realized {} unreal {} margin_used {}",
                ledger_label(currency),
                sums.equity,
                sums.realized_pnl,
                sums.unrealized,
                sums.margin_used,
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
            "free_buying_power",
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
                v.free_buying_power,
                if multi {
                    format!(" {}", ledger_label(&v.ledger_currency))
                } else {
                    String::new()
                },
            ));
        }
    }
    // A running account whose key in the database is no longer the one its session started with.
    for route_key in &snap.key_changed {
        out.push_str(&format!("\n  {route_key}: {KEY_CHANGED_WARNING}"));
    }
    // An account whose venue balance the node holds back from its engine: its equity above is the engine's own
    // book (seed + PnL), not the venue's wallet, and this is the only place that says so.
    for held in &snap.balance_withheld {
        out.push_str(&format!("\n  {}", held.line()));
    }
    // A `balance` above that is no venue balance reads as a wallet, and a negative one as a debt.
    for v in snap.venues.iter().filter(|v| v.balance_is_own_book) {
        out.push_str(&format!(
            "\n  {}: this engine keeps its own book and holds no venue balance: `balance` above is the fees and funding it \
             paid since it started, and equity is its configured capital plus that and its PnL, not the venue's wallet",
            who_of(v)
        ));
    }
    // An engine whose ledger currency was only the owner's ASSUMED USD when it started (decision 0189): every figure
    // above is in a currency the venue had not stated. It stays so for the engine's life (a later confirmation lifts
    // the balance hold, which has its own line, and does not clear this). Nothing is said for a frame that reports
    // none (or an older node's).
    for v in snap.venues.iter().filter(|v| v.currency_assumed) {
        out.push_str(&format!(
            "\n  {}: ledger currency {} was ASSUMED when this engine started (the venue stated none); it stays so until the account is started again",
            who_of(v),
            ledger_label(&v.ledger_currency)
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
fn who_of(v: &vike_tradehub_client::wire::VenueBlockWire) -> &str {
    if v.route_key.is_empty() { v.venue.as_str() } else { v.route_key.as_str() }
}

pub(super) fn print_recent(snap: &CoreSnapshotWire, n: Option<usize>) {
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
pub(super) fn print_state(snap: &CoreSnapshotWire) {
    for line in state_lines(snap) {
        println!("{line}");
    }
}

/// [`print_state`]'s lines: the mode, then what the one-shot `vike-cli trade status` prints beneath it, so this block
/// cannot read healthy while that verb lists a lost link or an account it does not run. PURE.
pub(super) fn state_lines(snap: &CoreSnapshotWire) -> Vec<String> {
    use crate::cmd::trade::status::{
        balance_held_lines, link_lines, mode_lines, venue_state_lines,
    };
    let mut lines = mode_lines(snap.trading_state, snap.fault.as_deref()).into_iter();
    let first = lines.next().map(|first| format!("{} {first}", stamp(snap)));
    first
        .into_iter()
        .chain(lines)
        .chain(venue_state_lines(snap))
        .chain(balance_held_lines(snap))
        .chain(link_lines(snap))
        .collect()
}

pub(super) fn print_snapshot(snap: &CoreSnapshotWire) {
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
