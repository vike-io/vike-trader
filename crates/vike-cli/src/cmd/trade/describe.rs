//! Human rendering: the one-line preview of a wire command, and the REPL's snapshot tables.

use vike_tradehub_client::wire::{WireCommand, WireSnapshot};

use crate::cmd::verbs;

use super::{is_pre_fold, width_of};

// ---- human rendering (of the pure command / of the snapshot) ---------------------------------

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
        WireCommand::Submit(o) => format!(
            "SUBMIT {} {} {} qty={} {}{} coid={}",
            side_word(o.side),
            o.venue,
            o.symbol,
            verbs::fmt_num(o.qty),
            match o.price {
                Some(p) => format!("{} @ {}", o.order_type, verbs::fmt_num(p)),
                None => o.order_type.clone(),
            },
            if o.reduce_only { " [reduce-only]" } else { "" },
            o.client_order_id,
        ),
        // No REPL verb mints a bracket (the desktop's TP/SL ticket is its one producer), but the
        // preview renderer stays total for a command another caller built. `account=-` because a
        // bracket names none: the node takes it only on a venue's one default account.
        WireCommand::Bracket(b) => format!(
            "BRACKET {} {} {} qty={} entry={} sl={} tp={} account=-",
            side_word(b.side),
            b.venue,
            b.symbol,
            verbs::fmt_num(b.qty),
            b.entry_price.map(verbs::fmt_num).unwrap_or_else(|| "market".to_string()),
            verbs::fmt_num(b.stop_loss),
            verbs::fmt_num(b.take_profit),
        ),
        WireCommand::Cancel(coid) => format!("CANCEL {coid}"),
        WireCommand::Modify { client_order_id, new_qty, new_price } => format!(
            "MODIFY {client_order_id} qty={} price={}",
            new_qty.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string()),
            new_price.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string()),
        ),
        WireCommand::MassCancel { venue, symbol, .. } => {
            format!("MASS-CANCEL venue={} symbol={}", opt_str(venue), opt_str(symbol))
        }
        WireCommand::Flatten { venue, symbol, .. } => format!("FLATTEN {venue} {symbol}"),
        WireCommand::MarketExit { venue, .. } => format!("MARKET-EXIT venue={}", opt_str(venue)),
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
    }
}

/// `pub(crate)`: shared with `crate::cmd::trade::render`'s order-row projection, so the two
/// renderers cannot come to disagree about what a side prints as.
pub(crate) fn side_word(side: i32) -> &'static str {
    if side >= 0 { "buy" } else { "sell" }
}

/// `pub(crate)`: shared with `crate::cmd::trade::render`, for the same reason as [`side_word`].
pub(crate) fn opt_num(v: &Option<f64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string())
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
            o.qty,
            trunc(&o.order_type, 10),
            opt_num(&o.price),
            trunc(&o.status, 12),
            o.filled_qty,
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
            p.size,
            p.avg_px,
            p.unrealized,
            p.leverage,
            p.liq_price,
        );
    }
}

pub(super) fn print_equity(snap: &WireSnapshot) {
    println!("{} equity", stamp(snap));
    println!("  balance (primary): {}", snap.balance);
    println!("  equity_total:      {}", snap.equity_total);
    if !snap.venues.is_empty() {
        println!(
            "  {:<10} {:>12} {:>12} {:>12} {:>12} {:>12}",
            "venue", "balance", "realized", "equity", "unreal", "free_bp"
        );
        for v in &snap.venues {
            println!(
                "  {:<10} {:>12} {:>12} {:>12} {:>12} {:>12}",
                trunc(&v.venue, 10),
                v.balance,
                v.realized_pnl,
                v.equity,
                v.unrealized,
                v.free_bp,
            );
        }
    }
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
    for line in lines {
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
