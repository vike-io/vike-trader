//! The actor loop: read and dispatch, drain commands, heartbeat + proactive refresh, reconnect.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use vike_data::LiveDataSink;
use vike_exec::EventSender;

use super::inbound::{emit_event, on_inbound};
use super::outbound::{
    SwapRecheck, close_command_coid, forget_close_on_write_failure, mark_new_order_sent,
    new_order_coid, orphan_cancel_commands, restore_bound_exit_on_write_failure, resubscribe,
    track_subscription, write_command, write_failure_event,
};
use super::reconcile::{read_positions_now, replay_after_reconnect};
use super::session::{
    ON_THE_SAME_SOCKET, OrderLane, RENEW_EVERY, SESSION_GONE, SESSION_GONE_AGAIN, SessionLapse,
    SessionWord, WAITING_FOR_A_SESSION, answers_the_reauth, reconnect_with_backoff, session_word,
    try_refresh_and_reauth,
};
use super::symbol_state::refresh_symbol_state;
use super::{
    CloseState, Command, ConnConfig, HEARTBEAT_INTERVAL, OrderIdMap, PositionMap, Reader, send,
};
use crate::proto::{ProtoHeartbeatEvent, ProtoOaTraderReq, ProtoOaTrendbarPeriod, pt};
use crate::symbols::SymbolMap;

#[cfg(doc)]
use super::open_and_handshake;

/// What the actor owes the position book and its own queue the moment a socket dies. The book stops being
/// evidence (`invalidate`), and the cancels written on the dead socket may not have arrived, so they are
/// owed again by the next orphan pass after the reconnect (`socket_died`). The bridge's own queue is
/// dropped: it predates the reconnect, and that pass rebuilds it from the book.
pub(super) fn forget_the_dead_socket(
    positions: &PositionMap,
    own_commands: &mut VecDeque<Command>,
) {
    if let Ok(mut book) = positions.lock() {
        book.invalidate();
        book.socket_died();
    }
    own_commands.clear();
}

/// How long [`read_what_is_left`] goes on reading before a socket is closed, and with the socket's own 1 s read timeout
/// about what it costs on a socket that has nothing more to say. The answer to an order already written is one round trip away.
const LAST_READS_FOR: Duration = Duration::from_secs(1);

/// **Read what the venue already answered, before the socket goes.** Closing it discards what is unread, and on a lapsed
/// session that is the refusal of every order but the first: two written together (a bracket's stop and take-profit) and
/// the first refusal starts the reconnect, so the second order stays `Submitted` until the engine's stuck-order ladder ends
/// it, on a protective leg too. An idle socket, a closed one and a failed one all end this at once or within the read timeout.
pub(super) fn read_what_is_left(
    reader: &mut Reader,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) {
    let until = Instant::now() + LAST_READS_FOR;
    while Instant::now() < until {
        match reader.next_frame() {
            // As the main loop reads a frame: a re-auth's refusal is nobody's order.
            Ok(Some(msg)) if session_word(&msg).is_none() || !answers_the_reauth(&msg) => {
                on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
            }
            Ok(Some(_)) => {}
            Ok(None) if reader.get_mut().eof.load(Ordering::SeqCst) => return,
            Ok(None) => {}
            Err(_) => return,
        }
    }
}

/// The actor loop: read a frame (dispatch), drain outbound commands, heartbeat every 10s. On a
/// socket error/EOF or a heartbeat/command write failure, backs off and reconnects
/// ([`reconnect_with_backoff`]) instead of exiting — replaying auth + symbol discovery and every
/// active subscription — and only truly stops when `Shutdown` is requested or the reconnect loop
/// gives up (channel gone). `symbols`/`sink` are held for the life of the actor so every inbound
/// `SPOT_EVENT` can be descaled and pushed without any per-frame lookup elsewhere; `symbols` is
/// itself refreshed from the venue on every reconnect (see [`open_and_handshake`]'s step 4/5) —
/// this refreshes the ACTOR's own decode-time view; the separate `Arc<SymbolMap>` handed to
/// `ActorHandle` (shared with `CtraderExec`/`CtraderData` at construction) is not swapped in place,
/// since cTrader's symbol universe for a live account is static reference data in practice — a
/// documented, deliberate scope limit (see the Task 6 report), not an oversight. The ONE part of
/// it that does follow the venue is each symbol's trading state (`SymbolMap::set_restrictions`): rewritten
/// at every reconnect and on every `ProtoOASymbolChangedEvent` (`super::symbol_state`). `money_digits`
/// (the account's commission/balance integer scaling exponent) is refreshed the same way on every
/// reconnect, for the same reason `symbols` is: `open_and_handshake` re-fetches it every time.
#[expect(clippy::too_many_arguments)]
pub(super) fn actor_loop(
    mut reader: Reader,
    rx: Receiver<Command>,
    mut cfg: ConnConfig,
    ctid: i64,
    mut symbols: Arc<SymbolMap>,
    mut money_digits: u32,
    sink: Arc<dyn LiveDataSink>,
    events: Option<EventSender>,
    orders: OrderIdMap,
    positions: PositionMap,
    closes: CloseState,
) {
    // The map the exec/data clients were handed: never swapped, so a reconnect and a symbol-changed event write the
    // trading state INTO it ([`SymbolMap::set_restrictions`]) instead of replacing it.
    let shared_symbols = symbols.clone();
    let mut last_heartbeat = Instant::now();
    let mut spot_subs: HashSet<i64> = HashSet::new();
    let mut trendbar_subs: HashSet<(i64, ProtoOaTrendbarPeriod)> = HashSet::new();
    // The bridge's OWN commands, written before anything the client queued: the cancels of resting
    // exits whose position is gone (`orphan_cancel_commands`). Never reaches `rx`.
    let mut own_commands: VecDeque<Command> = VecDeque::new();
    let mut lapse = SessionLapse::new(events.is_some(), ctid);
    // The handshake that built `reader` authorized this account, so a word an earlier actor of it left on the grant's cell
    // is over: it ended on a `Shutdown` inside its reconnect, with nothing after it to clear the word, and the cell lives for
    // the process (`account.active` off and on, a tradehub-only restart). Per account: a sibling's outage on one cell stays.
    lapse.healed(&cfg);
    // When a lapse last opened a new session on this actor.
    let mut session_opened_at: Option<Instant> = None;
    let mut last_auth_probe = Instant::now();
    let mut swap = SwapRecheck::default();

    // Local macro (not a closure) so `break $label` reaches the loop below from every call site —
    // including the one nested inside the command-drain loop — without fighting the borrow
    // checker over the half-dozen locals a reconnect touches (`reader`/`symbols`/subs/heartbeat).
    // The label is passed in explicitly (a `lifetime` macro fragment) because macro hygiene keeps
    // a bare `'outer` written inside the macro body from resolving to the loop label at each call
    // site otherwise.
    macro_rules! try_reconnect_or_break {
        ($label:lifetime) => {{
            // The socket is gone, so the position book stops being EVIDENCE right here — anything
            // could open or close while we are not listening. Its ENTRIES stay (routing is
            // unchanged, or a reconnect would become a hedge-opening window); only its
            // `fetched_at_ms` clears, and `reconcile_after_reconnect` restores it if — and only
            // if — the venue actually answers. See `crates/bridges/ctrader/src/positions.rs`'s
            // `PositionBook`.
            read_what_is_left(
                &mut reader,
                &symbols,
                money_digits,
                sink.as_ref(),
                events.as_ref(),
                &orders,
                &positions,
                &closes,
            );
            forget_the_dead_socket(&positions, &mut own_commands);
            reader.get_mut().close();
            let down_at = vike_model::now_ms();
            let mut pending = Vec::new();
            let lane =
                OrderLane { events: events.as_ref(), positions: &positions, closes: &closes };
            match reconnect_with_backoff(&mut cfg, ctid, &rx, &mut pending, &mut lapse, &lane) {
                Some((new_reader, new_symbols, new_money_digits)) => {
                    reader = new_reader;
                    symbols = Arc::new(new_symbols);
                    // The exec client holds the FIRST map and never sees this one: what the broker says about
                    // each symbol's trading state NOW goes into its shared cell.
                    shared_symbols.set_restrictions(symbols.restriction_table());
                    money_digits = new_money_digits;
                    replay_after_reconnect(
                        &mut reader,
                        ctid,
                        symbols.as_ref(),
                        money_digits,
                        sink.as_ref(),
                        &mut spot_subs,
                        &mut trendbar_subs,
                        pending,
                        events.as_ref(),
                        &orders,
                        &positions,
                        &closes,
                    );
                    last_heartbeat = Instant::now();
                    // The record of the hole: the store has no marker for a quote series
                    // (`crates/vike-store/src/live_rec.rs`'s `stream_status`), and this bridge's sockets disclose
                    // no `GapStart` (`crates/bridges/ctrader/CLAUDE.md`'s "A data-socket outage is a hole nothing marks").
                    if !spot_subs.is_empty() || !trendbar_subs.is_empty() {
                        tracing::warn!(
                            target: "ctrader",
                            from_ms = down_at,
                            to_ms = vike_model::now_ms(),
                            spots = spot_subs.len(),
                            trendbars = trendbar_subs.len(),
                            "cTrader data socket was down: these streams have a hole between these stamps"
                        );
                    }
                }
                None => break $label,
            }
        }};
    }

    // Cache the EOF flag so a closed socket triggers reconnect instead of busy-spinning on
    // `Ok(None)`. `get_mut()` reaches the `EofStream`; we read its flag directly below.
    'outer: loop {
        match reader.next_frame() {
            Ok(Some(msg)) => {
                let word = session_word(&msg);
                // The answer to the re-auth above, the only one this loop reads (a handshake reads its
                // own). The venue ended the account's session with the old token, and whether its
                // subscriptions outlive that is UNMEASURED: asked again, the worst answer is
                // `ALREADY_SUBSCRIBED`, a debug line; not asked, every key of this socket could go silent.
                if msg.payload_type == pt::ACCOUNT_AUTH_RES {
                    lapse.healed(&cfg);
                    resubscribe(&mut reader, ctid, &spot_subs, &trendbar_subs);
                }
                // A refusal that answers an order ends that order whatever else it says: it used to be
                // diverted to the refresh alone, and the order vanished.
                if word.is_none() || !answers_the_reauth(&msg) {
                    on_inbound(
                        &msg,
                        &symbols,
                        money_digits,
                        sink.as_ref(),
                        events.as_ref(),
                        &orders,
                        &positions,
                        &closes,
                    );
                }
                let session_holds = match word {
                    None => true,
                    // A new session refused in the same words is not cured by another: at most one a
                    // minute, and the doctor keeps saying so meanwhile (a refused order's words can be read
                    // here too, and a handshake is a whole symbol list).
                    Some(SessionWord::SessionGone) => {
                        let just_opened =
                            session_opened_at.is_some_and(|at| at.elapsed() < RENEW_EVERY);
                        if just_opened {
                            lapse.found(&cfg, SESSION_GONE_AGAIN, ON_THE_SAME_SOCKET);
                        }
                        just_opened
                    }
                    // The re-auth's OWN refusal is not answered by spending again here: that loop rotates at
                    // the speed of a round trip. The reconnect rations its spends.
                    Some(SessionWord::TokenRefused) => {
                        !answers_the_reauth(&msg)
                            && try_refresh_and_reauth(&mut cfg, &mut reader, ctid)
                    }
                };
                if !session_holds {
                    lapse.found(&cfg, SESSION_GONE, WAITING_FOR_A_SESSION);
                    try_reconnect_or_break!('outer);
                    session_opened_at = Some(Instant::now());
                }
            }
            Ok(None) => {
                if reader.get_mut().eof.load(Ordering::SeqCst) {
                    tracing::info!(target: "ctrader", "socket closed by peer; reconnecting");
                    try_reconnect_or_break!('outer);
                }
            }
            Err(e) => {
                tracing::warn!(target: "ctrader", error = %e, "read error; reconnecting");
                try_reconnect_or_break!('outer);
            }
        }

        // A position seen gone (by this frame, or by a reconcile) leaves its bound exits orphaned:
        // cancel them ourselves, the venue will not.
        own_commands.extend(orphan_cancel_commands(&positions, &orders));

        // The broker changed some symbols (`ProtoOASymbolChangedEvent`, noted by `on_inbound`): read their trading
        // state again, for the map the exec client holds (`super::symbol_state`). A data-only wiring has no exec
        // client to feed, so it only forgets them.
        let changed = symbols.take_changed();
        if !changed.is_empty() && events.is_some() {
            refresh_symbol_state(
                &changed,
                &mut reader,
                ctid,
                &shared_symbols,
                &symbols,
                money_digits,
                sink.as_ref(),
                events.as_ref(),
                &orders,
                &positions,
                &closes,
            );
        }

        // Drain all queued commands.
        loop {
            let next = match own_commands.pop_front() {
                Some(cmd) => Ok(cmd),
                None => rx.try_recv(),
            };
            match next {
                Ok(cmd) => {
                    // A positions read is the actor's OWN round trip (a `submit` is waiting on the reply),
                    // not a frame to write. A socket that dies under it is found by the next read; the
                    // rebuild it ran may have orphaned bound exits, whose cancels are owed on the next pass.
                    let cmd = match cmd {
                        Command::ReadPositions { reply } => {
                            let answered = read_positions_now(
                                &mut reader,
                                ctid,
                                &symbols,
                                money_digits,
                                sink.as_ref(),
                                events.as_ref(),
                                &orders,
                                &positions,
                                &closes,
                            );
                            own_commands.extend(orphan_cancel_commands(&positions, &orders));
                            let _ = reply.send(answered);
                            continue;
                        }
                        other => other,
                    };
                    swap.note(&cmd, Instant::now());
                    track_subscription(&cmd, &mut spot_subs, &mut trendbar_subs);
                    // Captured BEFORE `cmd` is consumed by `write_command` below (Fix 1): if the
                    // write fails, this is the only chance to read the coid out of the command
                    // that never reached the wire. `close_coid` is the paired capture for a
                    // ClosePosition's tracker-forget on failure (both are pure/inert on success).
                    let on_write_failure = write_failure_event(&cmd, &closes);
                    let close_coid = close_command_coid(&cmd);
                    let new_order = new_order_coid(&cmd);
                    match write_command(&mut reader, ctid, cmd) {
                        Ok(true) => mark_new_order_sent(&new_order, &positions),
                        Ok(false) => return, // Shutdown
                        Err(e) => {
                            tracing::warn!(target: "ctrader", error = %e, "write error; reconnecting");
                            // A bound exit that never reached the venue has no row to keep, and a cancel
                            // that never reached it is owed again.
                            restore_bound_exit_on_write_failure(&on_write_failure, &positions);
                            if let Some(event) = on_write_failure {
                                match events.as_ref() {
                                    Some(events) => emit_event(events, &positions, event),
                                    // Data-only wiring (`connect_and_auth`, no exec): order
                                    // commands never originate here, so this is unreachable in
                                    // practice — no lane to drop the reject on regardless.
                                    None => tracing::warn!(
                                        target: "ctrader",
                                        "write failed for an order command but no EventSender is wired; reject dropped"
                                    ),
                                }
                            }
                            // The close leg never reached the venue — drop its tracker state so a
                            // later leg's fill isn't misrouted and the next reduce isn't blocked.
                            forget_close_on_write_failure(&close_coid, &closes);
                            try_reconnect_or_break!('outer);
                            break; // resume at the top of 'outer against the fresh connection
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // A fresh-stream swap asks for its spots once more (`SwapRecheck`); a failed write is found by the next read.
        if !swap.is_idle() {
            for symbol_id in swap.take_due(Instant::now()) {
                if spot_subs.contains(&symbol_id) {
                    let _ = write_command(&mut reader, ctid, Command::SubscribeSpots { symbol_id });
                }
            }
        }

        // Is the account still authorized on this socket? EMPTY clientMsgId: the answer names no order, and a refusal
        // is read like any other frame by `session_word`. A failed write is found by the next read.
        if last_auth_probe.elapsed() >= cfg.auth_probe_every {
            last_auth_probe = Instant::now();
            let probe = ProtoOaTraderReq { ctid_trader_account_id: ctid, ..Default::default() };
            let _ = send(&mut reader, pt::TRADER_REQ, &probe, "");
        }

        // Heartbeat — and, on the same tick, the PROACTIVE token refresh.
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            // ⚠ A handshake of this process was refused for the very pair this socket holds (`pair_was_refused`: the
            // reconcile client's next pass, the listing, a rate ask), which is what tells a process with no expiry to read,
            // because `token_expires_at_ms` is `None` until it has refreshed once. The session may have ended unheard by then
            // and this socket delivered nothing since: a bound stop may have filled. So a NEW session, never a re-auth on this
            // socket, which would leave the book listing a closed position, the orphan rule silent and the stop's sibling
            // resting (and a stale bound stop can open a position). The handshake renews the grant and the reconnect re-reads
            // the book, the same whichever came first, this or the probe's "not authorized".
            if cfg.pair_was_refused() {
                tracing::info!(target: "ctrader", "the access token was refused elsewhere; opening a new session");
                lapse.found(&cfg, SESSION_GONE, WAITING_FOR_A_SESSION);
                try_reconnect_or_break!('outer);
                session_opened_at = Some(Instant::now());
                continue 'outer; // a new socket, whose heartbeat clock has just started
            }
            // ⚠ Rotate BEFORE the grant lapses rather than after the venue refuses it. The reactive
            // path below (`ACCOUNTS_TOKEN_INVALIDATED_EVENT` / an auth `ERROR_RES`) still exists and
            // is still the backstop, but by the time it fires a request has already failed — and at
            // the INITIAL connect the same expiry refuses daemon startup outright, which is the
            // operator pain this lane exists to remove.
            //
            // The cadence is the heartbeat's (10 s), so this costs one integer comparison per tick
            // and an OAuth round trip at most once per token lifetime. It runs on the ACTOR thread,
            // never on the fold. Same socket: nothing has lapsed yet, so nothing went unheard.
            if crate::token_store::refresh_due(cfg.token_expires_at_ms, vike_model::now_ms()) {
                tracing::info!(target: "ctrader", "access token is inside the refresh margin; refreshing");
                if !try_refresh_and_reauth(&mut cfg, &mut reader, ctid) {
                    // The refresh itself failed. `try_refresh_and_reauth` has already logged why
                    // (without secrets).
                    lapse.found(&cfg, SESSION_GONE, WAITING_FOR_A_SESSION);
                    try_reconnect_or_break!('outer);
                    continue 'outer; // a new socket, whose heartbeat clock has just started
                }
            }

            let hb = ProtoHeartbeatEvent::default();
            if let Err(e) = send(&mut reader, pt::HEARTBEAT_EVENT, &hb, "") {
                tracing::warn!(target: "ctrader", error = %e, "heartbeat write failed; reconnecting");
                try_reconnect_or_break!('outer);
            } else {
                last_heartbeat = Instant::now();
            }
        }
    }
}
