//! Outbound command encoding, subscription tracking and the write-failure reject synthesis.

use std::collections::HashSet;
use std::io;

use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};
use vike_model::now_ms;

use super::inbound::close_terminal_event;
use super::{CloseState, Command, Reader, send};
use crate::proto::{
    ProtoOaCancelOrderReq, ProtoOaClosePositionReq, ProtoOaGetTrendbarsReq,
    ProtoOaSubscribeLiveTrendbarReq, ProtoOaSubscribeSpotsReq, ProtoOaTrendbarPeriod,
    ProtoOaUnsubscribeLiveTrendbarReq, ProtoOaUnsubscribeSpotsReq, pt,
};

#[cfg(doc)]
use super::reconcile::replay_after_reconnect;
#[cfg(doc)]
use super::session::reconnect_with_backoff;

/// Encode+write one outbound command. Returns `false` on Shutdown (caller breaks the loop).
///
/// Only the three ORDER commands (`NewOrder`/`CancelOrder`/`AmendOrder`) set the envelope
/// `clientMsgId` to the real `client_order_id` — that is the ONLY id `on_error_res`/
/// `on_order_error_event` may correlate to a synthesized order event. Every non-order command
/// (Subscribe/Unsubscribe spots+trendbar, GetTrendbars) sends an EMPTY `clientMsgId`: its response
/// is routed by `payload_type` in `on_inbound`, never by envelope id, so there is nothing to
/// correlate — and a static per-verb label here (the old "sub"/"unsub"/"subtb"/"unsubtb"/"gtb")
/// would otherwise get echoed back on a failing request's `ERROR_RES` and be misread by
/// `on_error_res` as a genuine order's coid, fabricating a phantom `Event::OrderRejected` on a
/// combined data+exec connection (the bug this comment documents the fix for).
pub(super) fn write_command(reader: &mut Reader, ctid: i64, cmd: Command) -> io::Result<bool> {
    match cmd {
        Command::SubscribeSpots { symbol_id } => {
            let req = ProtoOaSubscribeSpotsReq {
                ctid_trader_account_id: ctid,
                symbol_id: vec![symbol_id],
                ..Default::default()
            };
            send(reader, pt::SUBSCRIBE_SPOTS_REQ, &req, "")?;
        }
        Command::UnsubscribeSpots { symbol_id } => {
            let req = ProtoOaUnsubscribeSpotsReq {
                ctid_trader_account_id: ctid,
                symbol_id: vec![symbol_id],
                ..Default::default()
            };
            send(reader, pt::UNSUBSCRIBE_SPOTS_REQ, &req, "")?;
        }
        Command::SubscribeTrendbar { symbol_id, period } => {
            let req = ProtoOaSubscribeLiveTrendbarReq {
                ctid_trader_account_id: ctid,
                period: period as i32,
                symbol_id,
                ..Default::default()
            };
            send(reader, pt::SUBSCRIBE_LIVE_TRENDBAR_REQ, &req, "")?;
        }
        Command::UnsubscribeTrendbar { symbol_id, period } => {
            let req = ProtoOaUnsubscribeLiveTrendbarReq {
                ctid_trader_account_id: ctid,
                period: period as i32,
                symbol_id,
                ..Default::default()
            };
            send(reader, pt::UNSUBSCRIBE_LIVE_TRENDBAR_REQ, &req, "")?;
        }
        Command::GetTrendbars { symbol_id, period, from_ts, to_ts, count } => {
            let req = ProtoOaGetTrendbarsReq {
                ctid_trader_account_id: ctid,
                from_timestamp: Some(from_ts),
                to_timestamp: Some(to_ts),
                period: period as i32,
                symbol_id,
                count: Some(count),
                ..Default::default()
            };
            send(reader, pt::GET_TRENDBARS_REQ, &req, "")?;
        }
        // The envelope `clientMsgId` is set to the order's `client_order_id` (not a static per-verb
        // label) so cTrader's echo of it on the response/`ERROR_RES` correlates the reply back to
        // the order (see `on_error_res`). NewOrder ALSO keeps the coid in the wire `label`/
        // `client_order_id` fields (set by the mapper) for the exec-event correlation path.
        Command::NewOrder(req) => {
            let msg_id =
                req.client_order_id.clone().or_else(|| req.label.clone()).unwrap_or_default();
            send(reader, pt::NEW_ORDER_REQ, &req, &msg_id)?
        }
        Command::CancelOrder { order_id, client_order_id } => {
            let req = ProtoOaCancelOrderReq {
                ctid_trader_account_id: ctid,
                order_id,
                ..Default::default()
            };
            send(reader, pt::CANCEL_ORDER_REQ, &req, &client_order_id)?;
        }
        Command::AmendOrder { req, client_order_id } => {
            send(reader, pt::AMEND_ORDER_REQ, &req, &client_order_id)?
        }
        // Close-by-position-id (reduce/flatten). The envelope `clientMsgId` is the reducing submit's
        // coid (like every other order command) so a venue `ERROR_RES` correlates back to it; the
        // successful close's execution events correlate by `positionId` instead (the closing order
        // carries no coid of ours) — see `map_close_event`.
        Command::ClosePosition { position_id, volume, client_order_id } => {
            let req = ProtoOaClosePositionReq {
                ctid_trader_account_id: ctid,
                position_id,
                volume,
                ..Default::default()
            };
            send(reader, pt::CLOSE_POSITION_REQ, &req, &client_order_id)?;
        }
        Command::Shutdown => return Ok(false),
    }
    Ok(true)
}

/// Track/untrack a (de)subscription command against the actor's own view of "what should be live
/// right now" — the set [`reconnect_with_backoff`]'s caller replays after a reconnect (cTrader does
/// not remember subscriptions across a dropped socket).
pub(super) fn track_subscription(
    cmd: &Command,
    spot_subs: &mut HashSet<i64>,
    trendbar_subs: &mut HashSet<(i64, ProtoOaTrendbarPeriod)>,
) {
    match cmd {
        Command::SubscribeSpots { symbol_id } => {
            spot_subs.insert(*symbol_id);
        }
        Command::UnsubscribeSpots { symbol_id } => {
            spot_subs.remove(symbol_id);
        }
        Command::SubscribeTrendbar { symbol_id, period } => {
            trendbar_subs.insert((*symbol_id, *period));
        }
        Command::UnsubscribeTrendbar { symbol_id, period } => {
            trendbar_subs.remove(&(*symbol_id, *period));
        }
        Command::GetTrendbars { .. }
        | Command::NewOrder(_)
        | Command::CancelOrder { .. }
        | Command::AmendOrder { .. }
        | Command::ClosePosition { .. } => {}
        Command::Shutdown => {}
    }
}

/// Reason text embedded in a synthesized reject when a command's write to the socket failed
/// mid-send (Task 6, Fix 1). Kept as static text — never interpolate the underlying `io::Error`
/// (mirrors `try_refresh_and_reauth`'s no-secrets-in-messages discipline elsewhere in this file).
const WRITE_FAILED_NEW_ORDER: &str = "submit write failed; connection reset";
const WRITE_FAILED_CANCEL: &str = "cancel write failed; connection reset";
const WRITE_FAILED_AMEND: &str = "amend write failed; connection reset";
const WRITE_FAILED_CLOSE: &str = "close-position write failed; connection reset";

/// Build the canonical event to synthesize when `cmd`'s write to the socket failed, so the
/// specific order-affecting command that never reached the wire never vanishes from the core's
/// event stream (the venue-adapter contract: "No order may silently vanish"). Called on a
/// `&Command` BEFORE it is handed to [`write_command`] (which consumes it), so the coid is still
/// available even though the write itself fails.
///
/// `NewOrder` synthesizes a TERMINAL `OrderRejected` — a partially-sent order could double-fill if
/// blindly retried, so this rejects and leaves resubmission to the strategy rather than
/// auto-retrying. `CancelOrder`/`AmendOrder` synthesize their NON-terminal advisory reject (the
/// order stays live under its existing terms), matching [`crate::exec::CtraderExec`]'s existing
/// dead-actor-channel handling for the same two commands. Subscribe/Unsubscribe/GetTrendbars/
/// Shutdown carry no order intent and need no synthesis (a failed subscribe still self-heals via
/// the tracked-subscription replay on reconnect, since `track_subscription` runs before the write
/// — see [`replay_after_reconnect`]; a failed `GetTrendbars` seed is simply lost — best-effort,
/// per that command's own doc — while the independently-sent `SubscribeTrendbar` still starts the
/// live stream).
///
/// `ClosePosition` picks a LEGAL terminal via [`close_terminal_event`] (Filled-if-an-earlier-leg-
/// progressed, else Rejected) so a close leg that dies on the wire never strands the coid. This
/// function is PURE — it is called for EVERY drained command BEFORE the write is even attempted, so
/// it must NOT mutate `closes` (an earlier bug forgot the coid here, wiping the just-registered
/// mapping on a SUCCESSFUL write); the tracker `forget` happens only at the actual failure site (see
/// [`forget_close_on_write_failure`]). It reads `closes` only for the non-mutating progress check.
/// The coid of a `ClosePosition` command (else `None`), captured BEFORE `write_command` consumes it
/// so a write FAILURE can [`forget`](crate::positions::CloseTracker::forget) it from `closes`: that
/// leg will never reach the venue, and a stale `by_position` entry would otherwise misroute a later
/// leg's fill or block the next reduce on the position. No-op for every non-close command.
pub(super) fn close_command_coid(cmd: &Command) -> Option<String> {
    match cmd {
        Command::ClosePosition { client_order_id, .. } => Some(client_order_id.clone()),
        _ => None,
    }
}

/// FORGET a `ClosePosition`'s coid from the tracker at the actual write-failure site (paired with
/// [`close_command_coid`]'s capture). A no-op when `close_coid` is `None` (non-close command).
pub(super) fn forget_close_on_write_failure(close_coid: &Option<String>, closes: &CloseState) {
    if let Some(coid) = close_coid
        && let Ok(mut t) = closes.lock()
    {
        t.forget(coid);
    }
}

pub(super) fn write_failure_event(cmd: &Command, closes: &CloseState) -> Option<Event> {
    match cmd {
        Command::NewOrder(req) => {
            let coid = req.client_order_id.clone().or_else(|| req.label.clone())?;
            Some(Event::OrderRejected(OrderRejected {
                client_order_id: coid,
                reason: WRITE_FAILED_NEW_ORDER.into(),
                ts: now_ms(),
            }))
        }
        Command::CancelOrder { client_order_id, .. } => {
            Some(Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: client_order_id.clone(),
                reason: WRITE_FAILED_CANCEL.into(),
                ts: now_ms(),
            }))
        }
        Command::AmendOrder { client_order_id, .. } => {
            Some(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: client_order_id.clone(),
                reason: WRITE_FAILED_AMEND.into(),
                ts: now_ms(),
            }))
        }
        // A reduce whose close leg never reached the wire: a LEGAL terminal (Filled if an earlier
        // leg already progressed the FSM, else Rejected) — built WITHOUT forgetting (this runs at
        // capture time, even for a write that will SUCCEED). The forget is done at the failure site
        // by `forget_close_on_write_failure`.
        Command::ClosePosition { client_order_id, .. } => {
            let progressed = closes.lock().ok().is_some_and(|t| t.has_progress(client_order_id));
            Some(close_terminal_event(client_order_id, progressed, WRITE_FAILED_CLOSE, now_ms()))
        }
        Command::SubscribeSpots { .. }
        | Command::UnsubscribeSpots { .. }
        | Command::SubscribeTrendbar { .. }
        | Command::UnsubscribeTrendbar { .. }
        | Command::GetTrendbars { .. }
        | Command::Shutdown => None,
    }
}
