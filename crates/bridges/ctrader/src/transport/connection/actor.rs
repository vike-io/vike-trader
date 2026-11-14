//! The actor loop: read and dispatch, drain commands, heartbeat + proactive refresh, reconnect.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_bridge_core::link_history::VenueLinkRecord;
use vike_data::DataClientSink;
use vike_execution::EventSender;

use super::inbound::{emit_event, on_inbound};
use super::outbound::{
    SwapRecheck, close_command_order_client_id, forget_close_on_write_failure, mark_close_written,
    mark_new_order_sent, new_order_order_client_id, orphan_cancel_commands,
    restore_bound_exit_on_write_failure, resubscribe, track_subscription, write_command,
    write_failure_event,
};
use super::reconcile::{read_positions_now, replay_after_reconnect};
use super::session::{
    ON_THE_SAME_SOCKET, OrderLane, PROBE_MESSAGE_ID, SESSION_GONE, SESSION_GONE_AGAIN,
    SessionLapse, SessionWord, WAITING_FOR_A_SESSION, answers_the_reauth, reconnect_with_backoff,
    refuse_the_queue, session_word, try_refresh_and_reauth,
};
use super::symbol_state::refresh_symbol_state;
use super::tape_hole::{close_tape_holes, open_tape_holes};
use super::trendbar_fill::{FILL_EVERY, TrendbarFills};
use super::{
    CloseState, ConnectionCommand, ConnectionConfig, EXECUTION_READ_POLL, PositionsByIdShared,
    Reader, send,
};
use crate::execution::positions::StrandedClose;
use crate::feed::bar::TrendbarCloses;
use crate::feed::decode::interval_for_trendbar_period;
use crate::instrument::symbols::SymbolMap;
use crate::transport::proto::{
    ProtoHeartbeatEvent, ProtoMessage, ProtoOaTraderReq, ProtoOaTrendbarPeriod, pt,
};

#[cfg(doc)]
use super::open_and_handshake;

/// What the actor owes the position book and its own queue the moment a socket dies. The book stops being
/// evidence (`invalidate`), and the cancels written on the dead socket may not have arrived, so they are
/// owed again by the next orphan pass after the reconnect (`socket_died`). The bridge's own queue is
/// dropped: it predates the reconnect, and that pass rebuilds it from the book. The closes written on it are
/// forgotten and handed back, for the reconnect's reconcile to end
/// (`crate::execution::positions::CloseTracker::take_written`).
pub(super) fn forget_the_dead_socket(
    positions: &PositionsByIdShared,
    closes: &CloseState,
    own_commands: &mut VecDeque<ConnectionCommand>,
) -> Vec<StrandedClose> {
    if let Ok(mut book) = positions.lock() {
        book.invalidate();
        book.socket_died();
    }
    own_commands.clear();
    closes.lock().map(|mut t| t.take_written()).unwrap_or_default()
}

/// What one read blocks for in this actor's loop. An exec socket carries no spots, so its queue would wait out the whole
/// read on a quiet socket: [`EXECUTION_READ_POLL`]. A data socket's queue holds only subscribes, so it keeps the handshake's read
/// and adds no wake-ups.
pub(super) fn actor_read_timeout(config: &ConnectionConfig, exec: bool) -> Duration {
    if exec { EXECUTION_READ_POLL } else { config.read_timeout }
}

/// Put a socket the handshake just opened on the actor's read, or, while a reconnect's trendbar fills are left
/// (`super::trendbar_fill`), on a read of [`FILL_EVERY`] at most, so a quiet socket still gives them their turns. A
/// socket this fails on is found dead by the next read.
fn read_as_the_actor(reader: &mut Reader, config: &ConnectionConfig, exec: bool, filling: bool) {
    let timeout = actor_read_timeout(config, exec);
    let timeout = if filling { timeout.min(FILL_EVERY) } else { timeout };
    if let Err(e) = reader.get_mut().set_read_timeout(timeout) {
        tracing::warn!(target: "ctrader", error = %e, "could not set the actor's read timeout");
    }
}

/// How long [`read_what_is_left`] goes on reading before a socket is closed, which is what it costs on a socket that has
/// nothing more to say. The answer to an order already written is one round trip away.
const LAST_READS_FOR: Duration = Duration::from_secs(1);

/// **Read what the venue already answered, before the socket goes.** Closing it discards what is unread, and on a lapsed
/// session that is the refusal of every order but the first: two written together (a bracket's stop and take-profit) and
/// the first refusal starts the reconnect, so the second order stays `Submitted` until the engine's stuck-order ladder ends
/// it, on a protective leg too. A closed socket and a failed one end this at once. Returns when it read the last price, for
/// the tape's hole ([`hole_starts_at`]).
pub(super) fn read_what_is_left(
    reader: &mut Reader,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn DataClientSink,
    events: Option<&EventSender>,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    positions: &PositionsByIdShared,
    closes: &CloseState,
) -> Option<Instant> {
    let until = Instant::now() + LAST_READS_FOR;
    let mut last_quote = None;
    while Instant::now() < until {
        match reader.next_frame() {
            // As the main loop reads a frame: a re-auth's refusal is nobody's order.
            Ok(Some(msg)) if session_word(&msg).is_none() || !answers_the_reauth(&msg) => {
                if msg.payload_type == pt::SPOT_EVENT {
                    last_quote = Some(Instant::now());
                }
                on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
            }
            Ok(Some(_)) => {}
            Ok(None) if reader.get_mut().eof.load(Ordering::SeqCst) => return last_quote,
            Ok(None) => {}
            Err(_) => return last_quote,
        }
    }
    last_quote
}

/// **Does this frame ANSWER the socket?** The probe's two answers: its record, or a refusal (an `ERROR_RES` answers a
/// request whatever it names). Only an answer proves the venue reads what this socket writes. What the venue sends
/// unasked (its `ProtoHeartbeatEvent`, a spot, an account's events) proves only the way IN, and a way out blackholed
/// still delivers it. MEASURED on the CI box 2026-11-09: the outgoing packets cut for 45 s with a close written, the exec
/// socket was never called dead, and the 166 bytes it wrote meanwhile are four heartbeats and ONE probe, the minute's,
/// where a socket that hears nothing asks at two thirds of [`super::READ_IDLE_AFTER`] and again a third later. So frames
/// kept coming in; which ones is not logged.
pub(super) fn answers_the_socket(msg: &ProtoMessage) -> bool {
    matches!(msg.payload_type, pt::TRADER_RES | pt::ERROR_RES)
}

/// **Where a dead socket's tape hole starts, in wall-clock ms: the later of the last price and the last answer it
/// received.** Prices may stop while frames still come in (the cut above: the tape read as quiet for 39 s before the
/// socket was called dead), and an answer proves every price the venue sent before it has arrived, so a quiet market
/// that was still answering is no hole.
pub(super) fn hole_starts_at(last_quote: Instant, last_answer: Instant) -> i64 {
    let ago = last_quote.max(last_answer).elapsed().as_millis();
    vike_model::now_ms().saturating_sub(i64::try_from(ago).unwrap_or(i64::MAX))
}

/// The actor loop: read a frame (dispatch), drain outbound commands, heartbeat every 10s. On a
/// socket error/EOF or a heartbeat/command write failure, backs off and reconnects
/// ([`reconnect_with_backoff`]) instead of exiting — replaying auth + symbol discovery and every
/// active subscription — and only truly stops when `Shutdown` is requested or the reconnect loop
/// gives up (channel gone). `symbols`/`sink` are held for the life of the actor so every inbound
/// `SPOT_EVENT` can be descaled and pushed without any per-frame lookup elsewhere; `symbols` is
/// itself refreshed from the venue on every reconnect (see [`open_and_handshake`]'s step 4/5) —
/// this refreshes the ACTOR's own decode-time view; the separate `Arc<SymbolMap>` handed to
/// `ActorHandle` (shared with `ExecutionClientCtrader`/`DataClientBusSpecCtrader` at construction) is not swapped in place,
/// since cTrader's symbol universe for a live account is static reference data in practice — a
/// documented, deliberate scope limit (see the Task 6 report), not an oversight. The ONE part of
/// it that does follow the venue is each symbol's trading state (`SymbolMap::set_restrictions`): rewritten
/// at every reconnect and on every `ProtoOASymbolChangedEvent` (`super::symbol_state`). `money_digits`
/// (the account's commission/balance integer scaling exponent) is refreshed the same way on every
/// reconnect, for the same reason `symbols` is: `open_and_handshake` re-fetches it every time.
#[expect(clippy::too_many_arguments)]
pub(super) fn actor_loop(
    mut reader: Reader,
    rx: Receiver<ConnectionCommand>,
    mut config: ConnectionConfig,
    ctid: i64,
    mut symbols: Arc<SymbolMap>,
    mut money_digits: u32,
    sink: Arc<dyn DataClientSink>,
    events: Option<EventSender>,
    orders: Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    positions: PositionsByIdShared,
    closes: CloseState,
) {
    // A live trendbar's close, which the venue never marks (`crate::feed::bar`): every frame's sink verb passes
    // through it, a key's (un)subscribe forgets what it holds, and a reconnect fills the periods the outage hid.
    let bars = Arc::new(TrendbarCloses::new(sink));
    let sink: Arc<dyn DataClientSink> = bars.clone();
    // The map the exec/data clients were handed: never swapped, so a reconnect and a symbol-changed event write the
    // trading state INTO it ([`SymbolMap::set_restrictions`]) instead of replacing it.
    let shared_symbols = symbols.clone();
    let mut last_heartbeat = Instant::now();
    // The liveness clock ([`answers_the_socket`]) and where a hole in the tape would start ([`hole_starts_at`]).
    let mut last_answer = Instant::now();
    let mut last_quote = Instant::now();
    let mut spot_subs: HashSet<i64> = HashSet::new();
    let mut trendbar_subs: HashSet<(i64, ProtoOaTrendbarPeriod)> = HashSet::new();
    // The bridge's OWN commands, written before anything the client queued: the cancels of resting
    // exits whose position is gone (`orphan_cancel_commands`). Never reaches `rx`.
    let mut own_commands: VecDeque<ConnectionCommand> = VecDeque::new();
    // The handshake that built `reader` authorized this account, so the word an earlier actor of it left on the grant's cell
    // is over: it ended on a `Shutdown` inside its reconnect, with nothing after it to clear the word, and the cell lives for
    // the process (`account.active` off and on, a tradehub-only restart). Per account: a sibling's outage on one cell stays.
    let mut lapse = SessionLapse::new(&config, events.is_some(), ctid);
    // The exec socket's row in the session history (a data socket has no `events`). The handshake that built `reader` is its
    // open; a drop when the actor ends is our own stop.
    let mut venue_session = events.as_ref().map(|e| VenueLinkRecord::of(e, "exec"));
    // When a lapse last opened a new session on this actor.
    let mut session_opened_at: Option<Instant> = None;
    let mut last_auth_probe = Instant::now();
    let probe = ProtoOaTraderReq { ctid_trader_account_id: ctid, ..Default::default() };
    let mut swap = SwapRecheck::default();
    // A reconnect's trendbar fills, asked one at a time between reads, never inside one (`super::trendbar_fill`).
    let mut fills = TrendbarFills::default();
    read_as_the_actor(&mut reader, &config, events.is_some(), false);

    // Local macro (not a closure) so `break $label` reaches the loop below from every call site —
    // including the one nested inside the command-drain loop — without fighting the borrow
    // checker over the half-dozen locals a reconnect touches (`reader`/`symbols`/subs/heartbeat).
    // The label is passed in explicitly (a `lifetime` macro fragment) because macro hygiene keeps
    // a bare `'outer` written inside the macro body from resolving to the loop label at each call
    // site otherwise. `$end` is how the old socket goes: `abort` when it is DEAD (no answer, a read or
    // a write failed), so what it holds unsent is dropped and never reaches the venue after the
    // reconnect ended it by name; `close` when the way to the venue works and only the session is over.
    macro_rules! try_reconnect_or_break {
        ($label:lifetime, $why:expr, $end:ident) => {{
            if let Some(session) = venue_session.take() {
                session.ended($why);
            }
            // The socket is gone, so the position book stops being EVIDENCE right here — anything
            // could open or close while we are not listening. Its ENTRIES stay (routing is
            // unchanged, or a reconnect would become a hedge-opening window); only its
            // `fetched_at_ms` clears, and `reconcile_after_reconnect` restores it if — and only
            // if — the venue actually answers. See `crates/bridges/ctrader/src/execution/positions.rs`'s
            // `PositionsById`.
            if let Some(at) = read_what_is_left(
                &mut reader,
                &symbols,
                money_digits,
                sink.as_ref(),
                events.as_ref(),
                &orders,
                &positions,
                &closes,
            ) {
                last_quote = at;
            }
            let stranded = forget_the_dead_socket(&positions, &closes, &mut own_commands);
            // A period that straddles the outage is never closed from its snapshot before it: the venue's stored bar
            // of it is asked for once the new socket is replayed (`TrendbarFills`).
            let holes = bars.forget_all();
            reader.get_mut().$end();
            let down_at = hole_starts_at(last_quote, last_answer);
            let holed = open_tape_holes(sink.as_ref(), &symbols, &spot_subs, down_at);
            let mut pending = Vec::new();
            let lane =
                OrderLane { events: events.as_ref(), positions: &positions, closes: &closes };
            match reconnect_with_backoff(&mut config, ctid, &rx, &mut pending, &mut lapse, &lane) {
                Some((new_reader, new_symbols, new_money_digits)) => {
                    reader = new_reader;
                    venue_session = events.as_ref().map(|e| VenueLinkRecord::of(e, "exec"));
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
                        stranded,
                        events.as_ref(),
                        &orders,
                        &positions,
                        &closes,
                    );
                    // The fills go to their queue and the actor goes back to reading: the first is asked now, the
                    // rest between reads.
                    fills.start(holes);
                    fills.tick(&mut reader, ctid, symbols.as_ref(), &bars, &trendbar_subs);
                    read_as_the_actor(&mut reader, &config, events.is_some(), !fills.is_idle());
                    last_heartbeat = Instant::now();
                    last_answer = Instant::now();
                    last_quote = Instant::now();
                    close_tape_holes(sink.as_ref(), &holed, down_at);
                    // The store marks a recorded symbol's hole; this line is every stream's, the trendbars' too.
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
                if answers_the_socket(&msg) {
                    last_answer = Instant::now();
                }
                if msg.payload_type == pt::SPOT_EVENT {
                    last_quote = Instant::now();
                }
                let word = session_word(&msg);
                // The answer to the heartbeat tick's re-auth, the only one this loop reads (a handshake reads its
                // own). The venue ended the account's session with the old token, and whether its
                // subscriptions outlive that is UNMEASURED: asked again, the worst answer is
                // `ALREADY_SUBSCRIBED`, a debug line; not asked, every key of this socket could go silent.
                if msg.payload_type == pt::ACCOUNT_AUTH_RES {
                    lapse.healed(&config);
                    resubscribe(&mut reader, ctid, &spot_subs, &trendbar_subs);
                }
                // The probe's answer proves the session is authorized, because an ended session refuses that request
                // (measured: `crates/bridges/ctrader/CLAUDE.md`'s "A lapsed account session"). So a word whose refusal only
                // looked like a lapse ends here, and a true one is said again by the probe's refusal.
                //
                // shortcut: a probe written just before a word can answer just after it, and a true word then drops until the
                // next probe says it again. Compare the two instants if a WARN that drops for a minute is ever seen.
                if msg.payload_type == pt::TRADER_RES {
                    lapse.healed(&config);
                }
                // An id-less refusal read while a reconnect's fill is outstanding is that fill's (the probe's carries its
                // own id); dispatched below too.
                if !fills.is_idle() {
                    fills.refused(&msg, &bars);
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
                    // The re-auth's OWN refusal is not answered by spending again here: that loop rotates at
                    // the speed of a round trip. The reconnect rations its spends.
                    Some(SessionWord::TokenRefused) if answers_the_reauth(&msg) => false,
                    // A new session, not a re-auth on this socket, for a token's word as for a session's: the
                    // session may have ended unheard, and a bound stop filled with this socket told nothing (the
                    // heartbeat's `pair_was_refused` argues it). The handshake spends only if the venue refuses the
                    // pair, so a word about a sibling's rotation costs no token.
                    //
                    // A new session refused in the same words is not cured by another: at most one a
                    // minute, and `trade health` keeps saying so meanwhile (a refused order's words can be read
                    // here too, and a handshake is a whole symbol list). The probe settles what this holds back:
                    // its answer ends the word, its refusal past the minute opens the session. Without the
                    // ration a token word on a fresh session would spend at the speed of a handshake.
                    Some(_) => {
                        let just_opened = session_opened_at
                            .is_some_and(|at| at.elapsed() < config.new_session_every);
                        if just_opened {
                            lapse.found(&config, SESSION_GONE_AGAIN, ON_THE_SAME_SOCKET);
                        }
                        just_opened
                    }
                };
                if !session_holds {
                    lapse.found(&config, SESSION_GONE, WAITING_FOR_A_SESSION);
                    try_reconnect_or_break!('outer, SESSION_GONE, close);
                    session_opened_at = Some(Instant::now());
                }
            }
            Ok(None) => {
                if reader.get_mut().eof.load(Ordering::SeqCst) {
                    tracing::info!(target: "ctrader", "socket closed by peer; reconnecting");
                    // The venue's FIN arrived, so the way is up: nothing unsent waits for it to heal.
                    try_reconnect_or_break!('outer, "socket closed by peer", close);
                }
            }
            Err(e) => {
                tracing::warn!(target: "ctrader", error = %e, "read error; reconnecting");
                try_reconnect_or_break!('outer, &e.to_string(), abort);
            }
        }

        // A reconnect's trendbar fills take their turn between reads (`super::trendbar_fill`): one asked at a time, at
        // the vendor's pace. Nothing read waits for them. One `is_idle` read on every other turn.
        if !fills.is_idle() {
            fills.tick(&mut reader, ctid, &symbols, &bars, &trendbar_subs);
            if fills.is_idle() {
                read_as_the_actor(&mut reader, &config, events.is_some(), false);
            }
        }

        // A blackholed socket answers nothing, so a read that times out looks like a quiet market, and what the venue still
        // sends unasked looks like a live one (`answers_the_socket`). The probe below has asked it something by now: no
        // answer means our frames reach nobody, a close or an order written meanwhile among them.
        if last_answer.elapsed() >= config.read_idle_after {
            tracing::warn!(
                target: "ctrader",
                idle_s = last_answer.elapsed().as_secs(),
                "no answer from the venue on this socket, not even to the question asked of it; reconnecting"
            );
            try_reconnect_or_break!('outer, "no answer from the venue on this socket", abort);
            continue 'outer; // a new socket, whose clocks have just started
        }

        // A position seen gone (by this frame, or by a reconcile) leaves its bound exits orphaned:
        // cancel them ourselves, the venue will not.
        own_commands.extend(orphan_cancel_commands(&positions, &orders));

        // The broker changed some symbols (`ProtoOASymbolChangedEvent`, noted by `on_inbound`): read their trading
        // state again, for the map the exec client holds (`super::symbol_state`). A data-only wiring has no exec
        // client to feed, so it only forgets them.
        let changed = symbols.take_changed();
        // An answer read inside that round trip (the probe's among them) is still an answer.
        if !changed.is_empty()
            && events.is_some()
            && refresh_symbol_state(
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
            )
        {
            last_answer = Instant::now();
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
                        ConnectionCommand::ReadPositions { reply } => {
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
                            // The read's round trip keeps its own refusal (`read_reply`), and on an ended session it is
                            // the probe's, word for word (measured). The probe's is read like any frame, so ask it now.
                            // An answer is an answer, and one read inside the round trip may have been the probe's.
                            if answered {
                                last_answer = Instant::now();
                            } else {
                                last_auth_probe = Instant::now();
                                let _ = send(&mut reader, pt::TRADER_REQ, &probe, PROBE_MESSAGE_ID);
                            }
                            continue;
                        }
                        other => other,
                    };
                    swap.note(&cmd, Instant::now());
                    track_subscription(&cmd, &mut spot_subs, &mut trendbar_subs);
                    // A trend-bar stream asked for or released starts from nothing held.
                    if let ConnectionCommand::SubscribeTrendbar { symbol_id, period }
                    | ConnectionCommand::UnsubscribeTrendbar { symbol_id, period } = &cmd
                        && let Some(name) = symbols.name_of(*symbol_id)
                    {
                        bars.forget(name, interval_for_trendbar_period(*period));
                    }
                    // Captured BEFORE `cmd` is consumed by `write_command` below (Fix 1): if the
                    // write fails, this is the only chance to read the order client id out of the command
                    // that never reached the wire. `close_order_client_id` is the paired capture for a
                    // ClosePosition's tracker-forget on failure (both are pure/inert on success).
                    let on_write_failure = write_failure_event(&cmd, &closes);
                    let close_order_client_id = close_command_order_client_id(&cmd);
                    let new_order = new_order_order_client_id(&cmd);
                    match write_command(&mut reader, ctid, cmd) {
                        Ok(true) => {
                            mark_new_order_sent(&new_order, &positions);
                            mark_close_written(&close_order_client_id, &closes);
                        }
                        Ok(false) => {
                            // Shutdown, with whatever was sent behind it still queued.
                            let lane = OrderLane {
                                events: events.as_ref(),
                                positions: &positions,
                                closes: &closes,
                            };
                            refuse_the_queue(&rx, &lane);
                            return;
                        }
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
                            forget_close_on_write_failure(&close_order_client_id, &closes);
                            try_reconnect_or_break!('outer, "a write to the venue failed", abort);
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
                    let _ = write_command(
                        &mut reader,
                        ctid,
                        ConnectionCommand::SubscribeSpots { symbol_id },
                    );
                }
            }
        }

        // Is the account still authorized on this socket? Its own clientMsgId (`PROBE_MESSAGE_ID`): a refusal is the
        // probe's, never a trendbar fill's waiting meanwhile, and names no order; `session_word` reads it like any other
        // frame. A failed write is found by the next read. A socket the venue
        // answered nothing for two thirds of `read_idle_after` is asked too, so that no answer past it is proof and never
        // only a quiet market: on a healthy socket that is a probe every two thirds of it, whatever else arrives.
        let too_quiet = last_answer.elapsed() >= config.read_idle_after * 2 / 3
            && last_auth_probe.elapsed() >= config.read_idle_after / 3;
        if too_quiet || last_auth_probe.elapsed() >= config.auth_probe_every {
            last_auth_probe = Instant::now();
            let _ = send(&mut reader, pt::TRADER_REQ, &probe, PROBE_MESSAGE_ID);
        }

        // Heartbeat — and, on the same tick, the PROACTIVE token refresh.
        if last_heartbeat.elapsed() >= config.heartbeat_every {
            // ⚠ A handshake of this process was refused for the very pair this socket holds (`pair_was_refused`: the
            // reconcile client's next pass, the listing, a rate ask), which is what tells a process with no expiry to read,
            // because `token_expires_at_ms` is `None` until it has refreshed once. The session may have ended unheard by then
            // and this socket delivered nothing since: a bound stop may have filled. So a NEW session, never a re-auth on this
            // socket, which would leave the book listing a closed position, the orphan rule silent and the stop's sibling
            // resting (and a stale bound stop can open a position). The handshake renews the grant and the reconnect re-reads
            // the book, the same whichever came first, this or the probe's "not authorized".
            if config.pair_was_refused() {
                tracing::info!(target: "ctrader", "the access token was refused elsewhere; opening a new session");
                lapse.found(&config, SESSION_GONE, WAITING_FOR_A_SESSION);
                try_reconnect_or_break!('outer, SESSION_GONE, close);
                session_opened_at = Some(Instant::now());
                continue 'outer; // a new socket, whose heartbeat clock has just started
            }
            // ⚠ Rotate BEFORE the grant lapses rather than after the venue refuses it. The reactive
            // path (`ACCOUNTS_TOKEN_INVALIDATED_EVENT` / an auth `ERROR_RES`, a new session) still exists and
            // is still the backstop, but by the time it fires a request has already failed — and at
            // the INITIAL connect the same expiry refuses daemon startup outright, which is the
            // operator pain this lane exists to remove.
            //
            // The cadence is the heartbeat's (10 s), so this costs one integer comparison per tick
            // and an OAuth round trip at most once per token lifetime. It runs on the ACTOR thread,
            // never on the fold. Same socket: nothing has lapsed yet, so nothing went unheard.
            if vike_bridge_core::transport::auth::token_refresh_due(
                config.token_expires_at_ms,
                vike_model::now_ms(),
                crate::transport::token_store::REFRESH_MARGIN_MS,
            ) {
                tracing::info!(target: "ctrader", "access token is inside the refresh margin; refreshing");
                if !try_refresh_and_reauth(&mut config, &mut reader, ctid) {
                    // The refresh itself failed. `try_refresh_and_reauth` has already logged why
                    // (without secrets).
                    lapse.found(&config, SESSION_GONE, WAITING_FOR_A_SESSION);
                    try_reconnect_or_break!('outer, SESSION_GONE, close);
                    continue 'outer; // a new socket, whose heartbeat clock has just started
                }
            }

            let heartbeat = ProtoHeartbeatEvent::default();
            if let Err(e) = send(&mut reader, pt::HEARTBEAT_EVENT, &heartbeat, "") {
                tracing::warn!(target: "ctrader", error = %e, "heartbeat write failed; reconnecting");
                try_reconnect_or_break!('outer, &e.to_string(), abort);
            } else {
                last_heartbeat = Instant::now();
            }
        }
    }
}
