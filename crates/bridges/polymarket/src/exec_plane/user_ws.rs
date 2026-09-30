//! Polymarket CLOB **user** channel protocol (authenticated): the subscribe frame (carries the L2
//! creds) + a pure, registry-aware decoder of trade/order events → vike Events. Transport-free like
//! the market channel; a driver ([`spawn_polymarket_user_data`](super::user_data::spawn_polymarket_user_data))
//! supplies the socket.
//!
//! Keying: the CLOB server assigns the order `id` (no client-id echo), so every event is re-keyed
//! CLOB→coid through the shared [`PolymarketRegistry`].
//!
//! **An id this registry cannot resolve is not necessarily "not ours".** It is either not ours or
//! not *yet* ours, and this decoder used to conflate the two — all three re-key sites simply fell
//! through with no `else` arm, so an executed fill whose id had not been written yet was discarded
//! outright, and the A3 history resync replayed through the very same gate and so never recovered
//! it. Since the bare `Event::Fill` folds `Account` independently of the order FSM, that left the
//! platform's position and realized PnL permanently disagreeing with the venue's, silently.
//!
//! Now an unresolvable event is **parked** ([`crate::exec_plane::pending_events`]) under the id it is waiting
//! on and replayed the instant the registry gains that id — atomically, so the exec thread cannot
//! slip its `on_accept` into the gap. What was never claimed inside the TTL is the genuine "not
//! ours" case and dies with a `warn!` and a counter, never silently. [`crate::exec_plane::registry`]'s doc is
//! the authority on the two races that produce an absent id and on the settling grace that covers
//! the second one.
//!
//! maker vs taker: a trade lists our order as `taker_order_id` (top-level
//! size/price) or inside `maker_orders[]` (that entry's matched_amount/price). Fills emit on the
//! good statuses (MATCHED/MINED/CONFIRMED); RETRYING/FAILED are skipped — the A3 resync repairs the
//! rare MATCHED-then-FAILED divergence (Nautilus-aligned; no fill reversal). A stable composite
//! `trade_id` = `"{trade_id}:{order_id}"` makes the repeated status messages dedup in the core
//! (both its bare-`Fill` `seen_trade_ids` and its wrapper `seen_fsm_trade_ids`).

use super::fill_tracker::{DUST_TRADE_ID_SUFFIX, FillTracker, SnapOutcome};
use super::pending_events::{ParkedEvent, UserEventKind};
use super::registry::PolymarketRegistry;
use crate::config::PolymarketCreds;
use vike_model::events::{
    Event, FillEvent, OrderCanceled, OrderFilled, OrderPartiallyFilled, TradeId,
};

/// Whether a decode may park what it cannot re-key.
///
/// [`Park::Off`] exists for exactly one caller — [`replay_parked`], which re-runs the real decoder
/// over a frame the registry has just claimed. A replayed trade frame can still name ids that are
/// not ours (the counterparty legs of that match), and those already have their own park entries
/// from the original decode; re-parking them would let one frame ratchet the park upward on every
/// replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Park {
    On,
    Off,
}

/// The user-channel subscribe frame — the first frame carries the L2 auth trio.
pub fn user_subscribe_message(creds: &PolymarketCreds, markets: &[String]) -> String {
    serde_json::json!({
        "auth": { "apiKey": creds.api_key, "secret": creds.secret, "passphrase": creds.passphrase },
        "markets": markets,
        "type": "user",
    })
    .to_string()
}

/// A confirmed-good trade status (emit a fill). RETRYING/FAILED are skipped — the A3 resync repairs
/// the rare MATCHED-then-FAILED divergence (Nautilus-aligned; no fill reversal).
fn is_fillable_status(status: &str) -> bool {
    matches!(status, "MATCHED" | "MINED" | "CONFIRMED")
}

/// Build a bare Fill + its OrderPartiallyFilled wrap for one matched order of ours. The bare Fill
/// folds `Account` (position/PnL); the wrap feeds the order FSM's `accumulate_fill`. Both carry the
/// same composite `trade_id` so a status-repeat dedups on both core paths.
#[allow(clippy::too_many_arguments)]
fn fill_pair(
    coid: String,
    trade_id: TradeId,
    asset_id: &str,
    side: i32,
    qty: f64,
    px: f64,
    liquidity: &str,
    ts: i64,
) -> [Event; 2] {
    let fill = FillEvent {
        trade_id,
        client_order_id: coid.clone(),
        venue: "polymarket".to_string().into(),
        symbol: asset_id.to_string().into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: liquidity.to_string().into(),
        ts,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    };
    [
        Event::Fill(fill.clone()),
        Event::OrderPartiallyFilled(OrderPartiallyFilled { client_order_id: coid, fill, ts }),
    ]
}

/// Emit one matched order's fill pair, applying the optional dust-snap tracker
/// (`crate::exec_plane::fill_tracker`): a dust overfill is capped down to the submitted qty, and ONLY a fill
/// the snap itself reduced to zero emits nothing. `trade_key` — the composite
/// `"{trade_id}:{order_id}"` — is also the tracker's status-repeat dedup key, so a MATCHED/MINED/
/// CONFIRMED repeat of one match folds the cumulative exactly once. `tracker == None` (or an
/// untracked coid) is a byte-identical no-op.
///
/// NOTE the dust RESIDUAL is deliberately not minted here — only from the terminal `decode_order`
/// UPDATE arm. Mid-life, a sub-tolerance remainder may still be resting on the book; completing it
/// early and evicting the entry would double-count the position when the venue later fills it.
#[allow(clippy::too_many_arguments)]
fn emit_fill(
    out: &mut Vec<Event>,
    tracker: Option<&FillTracker>,
    coid: String,
    trade_key: TradeId,
    asset_id: &str,
    side: i32,
    qty: f64,
    px: f64,
    liquidity: &str,
    ts: i64,
) {
    let qty = match tracker {
        Some(t) => match t.snap_fill_qty(&coid, &trade_key, qty, px) {
            SnapOutcome::Emit(q) => q,
            SnapOutcome::Suppress => return,
        },
        None => qty,
    };
    out.extend(fill_pair(coid, trade_key, asset_id, side, qty, px, liquidity, ts));
}

/// The composite `"{trade_id}:{order_id}"` BOTH core dedup sets key on (and the tracker's
/// status-repeat key). It is also the exact string `crate::exec_plane::recon_client::parse_fill_reports` stamps,
/// which is what lets a fill both paths saw dedup — so the shape is a contract, not a format choice.
///
/// [`None`] is unreachable from a frame [`decode_trade`] admitted (it refuses an empty wire `id`
/// first). It stays fallible rather than becoming an `expect` because a venue frame must never be
/// able to panic the ingest thread — a malformed frame costs a dropped fill, never the process.
fn composite_key(trade_id: &str, order_id: &str) -> Option<TradeId> {
    TradeId::new(format!("{trade_id}:{order_id}")).ok()
}

fn decode_trade(
    ev: &serde_json::Value,
    reg: &PolymarketRegistry,
    tracker: Option<&FillTracker>,
    out: &mut Vec<Event>,
    park: Park,
) {
    let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).unwrap_or_default();
    if !is_fillable_status(s("status")) {
        return; // RETRYING/FAILED carry no money — nothing to lose, nothing to park.
    }
    let trade_id = s("id");
    // The WIRE half of every composite this frame mints. Refuse the whole frame without it — and
    // refuse it BEFORE the park staging below, because a parked malformed frame replays into the
    // same refusal. The composite `"{id}:{order_id}"` is the dedup key on BOTH core paths (bare
    // `Fill`'s `seen_trade_ids` and the wrap's `seen_fsm_trade_ids`), and Polymarket repeats one
    // match as MATCHED → MINED → CONFIRMED: without a real `id` the composite degrades to
    // `":{order_id}"`, which is per-ORDER, so those three repeats look like ONE fill to the tracker
    // while successive genuine fills of the same order also collapse into it. Nothing here is a
    // stable substitute identity (size/price/timestamp all repeat across the status sequence), so
    // the frame is dropped rather than folded under a synthetic id.
    if trade_id.is_empty() {
        tracing::warn!(
            venue = "polymarket",
            status = s("status"),
            "user-channel trade frame carries no `id` — dropping it; its fills could not be \
             deduplicated, so the MATCHED/MINED/CONFIRMED repeats would each book again"
        );
        return;
    }
    let asset_id = s("asset_id");
    let ts = s("timestamp").parse::<i64>().unwrap_or(0);
    let parse = |v: &str| v.parse::<f64>().unwrap_or(0.0);

    // Every CLOB id this frame names that we could NOT re-key. Collected rather than acted on
    // in-line because whether they are worth parking depends on the WHOLE frame — see below.
    let mut unresolved: Vec<String> = Vec::new();
    let mut resolved_any = false;

    // taker side: our order == taker_order_id → top-level size/price.
    let taker = s("taker_order_id");
    if !taker.is_empty() {
        match reg.rekey_for_decode(taker) {
            Some((coid, side)) => {
                resolved_any = true;
                if let Some(key) = composite_key(trade_id, taker) {
                    emit_fill(
                        out,
                        tracker,
                        coid,
                        key,
                        asset_id,
                        side,
                        parse(s("size")),
                        parse(s("price")),
                        "taker",
                        ts,
                    );
                }
            }
            None => unresolved.push(taker.to_string()),
        }
    }
    // maker side: each maker_orders[].order_id that is ours → that entry's matched_amount/price.
    if let Some(makers) = ev.get("maker_orders").and_then(|m| m.as_array()) {
        for mo in makers {
            let oid = mo.get("order_id").and_then(|v| v.as_str()).unwrap_or_default();
            if oid.is_empty() {
                continue;
            }
            match reg.rekey_for_decode(oid) {
                Some((coid, side)) => {
                    resolved_any = true;
                    let mp = |k: &str| mo.get(k).and_then(|v| v.as_str()).unwrap_or_default();
                    if let Some(key) = composite_key(trade_id, oid) {
                        emit_fill(
                            out,
                            tracker,
                            coid,
                            key,
                            asset_id,
                            side,
                            parse(mp("matched_amount")),
                            parse(mp("price")),
                            "maker",
                            ts,
                        );
                    }
                }
                None => unresolved.push(oid.to_string()),
            }
        }
    }
    // THE DISCRIMINATOR between "not ours" and "not YET ours", and the reason this is not just
    // "park everything that failed to resolve". The user channel only pushes trades we were a party
    // to, and a match names its counterparties: if we took liquidity, `maker_orders` is a list of
    // STRANGERS' order ids, none of which will ever be registered here. So a frame in which
    // something DID resolve is already fully attributed — its remaining ids are counterparties, not
    // late arrivals, and parking them would spend the bound on entries guaranteed to expire and
    // fire a `warn!` on every ordinary taker fill, drowning the signal this lane exists to raise.
    //
    // A frame in which NOTHING resolved is the ambiguous one, and only then do we stage it: under
    // EVERY id it names, since we cannot yet tell which is ours. Whichever one the exec thread
    // registers claims the frame; the other copies expire quietly.
    //
    // (Accepted residual, stated rather than hidden: a genuine self-match — our resting maker order
    // filled by our own taker order — where only ONE of the two is registered would attribute the
    // registered leg and skip the other. That needs an ack race on an order that has been RESTING
    // long enough to be hit, which is a contradiction in timing.)
    if park == Park::On && !resolved_any && !unresolved.is_empty() {
        reg.park(&unresolved, UserEventKind::Trade, ev);
    }
}

/// Would this `order` frame decode to anything if its id resolved? Only CANCELLATION and a TERMINAL
/// UPDATE do; PLACEMENT is `decode_order`'s ignored arm (submit already emitted `OrderAccepted`) and
/// a non-terminal UPDATE is handled by the trade events instead.
///
/// Parking is gated on this because PLACEMENT is the single most common frame in the ack race — it
/// arrives at acceptance, by definition before the ack gets home — so parking it would burn the
/// bound on frames that replay into nothing, evicting the trade frames that carry the money.
fn order_frame_is_actionable(ev: &serde_json::Value) -> bool {
    let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).unwrap_or_default();
    match s("type") {
        "CANCELLATION" => true,
        "UPDATE" => {
            let parse = |k: &str| s(k).parse::<f64>().unwrap_or(0.0);
            let orig = parse("original_size");
            orig > 0.0 && parse("size_matched") >= orig
        }
        _ => false,
    }
}

fn decode_order(
    ev: &serde_json::Value,
    reg: &PolymarketRegistry,
    tracker: Option<&FillTracker>,
    out: &mut Vec<Event>,
    park: Park,
) {
    let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).unwrap_or_default();
    let Some((coid, side)) = reg.rekey_for_decode(s("id")) else {
        // Same staging as a trade frame, for the same reason: on this venue EVERY post-acceptance
        // terminal arrives only on the user channel, so dropping a terminal UPDATE that lost the
        // ack race strands the order at PartiallyFilled forever — a different way for an order to
        // silently vanish, and equally forbidden. Only actionable frames are worth the capacity.
        if park == Park::On && !s("id").is_empty() && order_frame_is_actionable(ev) {
            reg.park(&[s("id").to_string()], UserEventKind::Order, ev);
        }
        return;
    };
    match s("type") {
        "CANCELLATION" => {
            if let Some(t) = tracker {
                t.remove(&coid); // terminal: stop tracking (and never mint dust for a cancel)
            }
            out.push(Event::OrderCanceled(OrderCanceled {
                client_order_id: coid,
                reason: String::new().into(),
                ts: 0,
            }))
        }
        "UPDATE" => {
            let parse = |k: &str| s(k).parse::<f64>().unwrap_or(0.0);
            let orig = parse("original_size");
            if orig > 0.0 && parse("size_matched") >= orig {
                // Both ids this arm mints are seeded on the order's own CLOB `id` — replay-stable
                // (never a clock or a counter), and non-empty in practice because `s("id")` is the
                // key that just re-keyed to `coid` through the registry. The guard is here anyway so
                // no site can reach a panicking constructor, and its `else` refuses BOTH mints
                // together: an un-dedupable synthetic completion would re-add the dust residual on
                // every terminal repeat, and an un-dedupable zero-qty terminal marker would re-flip
                // the FSM. A stranded non-terminal order is the confirm-grace watchdog's and
                // reconcile's business; a double-booked fill is nobody's.
                // ⚠ The WIRE half of both minted ids is checked EXPLICITLY, because `TradeId` alone
                // cannot see this defect: with an empty `id` the composites render `":filled"` and
                // `":dust"` — non-empty, so the constructor is satisfied — yet they are the SAME
                // string for every id-less order, and both dedup sets are global. The first such
                // order would terminalize and every later one would be swallowed as a duplicate,
                // and a `:dust` collision would silently drop a real completing fill. So the seed
                // must be a genuine order identity, not merely a non-empty rendering.
                //
                // In practice it always is: `s("id")` is the key that just re-keyed to `coid`
                // through the registry. The guard covers the one way that can still be empty — an
                // empty CLOB id registered at accept — and refuses BOTH mints together, since the
                // dust completion and the terminal wrap describe one order. A stranded non-terminal
                // order is the confirm-grace watchdog's and reconcile's business; a double-booked or
                // silently-dropped fill is nobody's.
                //
                // Both strings are otherwise BYTE-IDENTICAL to the `format!`s they replace: `:dust`
                // is the tag `tests/offline/dust_snap_engine.rs` relies on never colliding with a
                // real trade id, and `:filled` is what keeps the zero-qty terminal marker out of
                // `seen_fsm_trade_ids`' way so it can still flip the FSM.
                let id = s("id");
                let (false, Ok(dust_marker), Ok(filled_marker)) = (
                    id.is_empty(),
                    TradeId::new(format!("{id}{DUST_TRADE_ID_SUFFIX}")),
                    TradeId::new(format!("{id}:filled")),
                ) else {
                    tracing::warn!(
                        venue = "polymarket",
                        coid = %coid,
                        "terminal order UPDATE carries no `id` — emitting no terminal wrap and no \
                         dust mint; a `:filled`/`:dust` id with an empty wire half is shared by \
                         every id-less order, so the global dedup sets would collapse them"
                    );
                    return;
                };
                // The venue calls it done. If OUR emitted cumulative qty is a dust unit short,
                // mint ONE synthetic completing fill first (evicting the entry, so a repeated
                // terminal cannot mint a second) — otherwise the FSM never reaches Filled.
                if let Some((residual, dust_px)) =
                    tracker.and_then(|t| t.check_dust_residual(&coid))
                {
                    out.extend(fill_pair(
                        coid.clone(),
                        dust_marker,
                        s("asset_id"),
                        side,
                        residual,
                        dust_px,
                        "",
                        0,
                    ));
                } else if let Some(t) = tracker {
                    t.remove(&coid);
                }
                // Fully filled → the terminal wrap. The money + qty already came via the trade-event
                // fills (and their OrderPartiallyFilled wraps), so this carries a ZERO-qty fill (no
                // double `accumulate_fill`) with a DISTINCT trade_id (so it is not deduped by
                // `seen_fsm_trade_ids` and does flip the FSM to Filled).
                out.push(Event::OrderFilled(OrderFilled {
                    client_order_id: coid.clone(),
                    fill: FillEvent {
                        trade_id: filled_marker,
                        client_order_id: coid,
                        venue: "polymarket".to_string().into(),
                        symbol: s("asset_id").to_string().into(),
                        side: 0,
                        last_qty: 0.0,
                        last_px: 0.0,
                        commission: 0.0,
                        commission_asset: String::new().into(),
                        liquidity_side: String::new().into(),
                        ts: 0,
                        mark_price: None,
                        position_side: "BOTH".to_string().into(),
                    },
                    ts: 0,
                }));
            }
        }
        _ => {} // PLACEMENT ignored — submit already emitted OrderAccepted.
    }
}

fn decode_one(
    ev: &serde_json::Value,
    reg: &PolymarketRegistry,
    tracker: Option<&FillTracker>,
    out: &mut Vec<Event>,
    park: Park,
) {
    match ev.get("event_type").and_then(|v| v.as_str()) {
        Some("trade") => decode_trade(ev, reg, tracker, out, park),
        Some("order") => decode_order(ev, reg, tracker, out, park),
        _ => {}
    }
}

/// Decode a single history/user event object of a known kind ("trade" | "order"), used by the A3
/// replay mapper — `/data/*` history rows carry the same field shapes but may omit `event_type`.
///
/// This path STAGES like the live one. It was previously the reason the bug was unrecoverable: the
/// resync replayed through the identical re-key gate, so the one mechanism that could have repaired
/// a dropped fill dropped it a second time. It cannot repair a previous *process*'s orders — the
/// registry is in-memory — but a row for an order this process is mid-acknowledgement of is exactly
/// the "not YET ours" case, and it now waits instead of vanishing.
pub(crate) fn decode_typed(
    kind: &str,
    ev: &serde_json::Value,
    reg: &PolymarketRegistry,
) -> Vec<Event> {
    let mut out = Vec::new();
    match kind {
        // History replay is a REPAIR path whose rows overlap live events the tracker already
        // folded; running it through the tracker would double-count, so it decodes untracked.
        "trade" => decode_trade(ev, reg, None, &mut out, Park::On),
        "order" => decode_order(ev, reg, None, &mut out, Park::On),
        _ => {}
    }
    out
}

/// Re-decode the frames [`PolymarketRegistry::on_accept`] just handed back, now that their id
/// resolves. Called by `crate::exec_plane::client`'s submit arm — the ONE place that learns "this id is ours"
/// — which sends the result into the same lossless event lane the pump uses.
///
/// Replaying the stored frame through the real decoder (rather than reconstructing a fill from
/// extracted fields) means a recovered event is byte-identical to what the live path would have
/// emitted had the id been present, with no parallel extraction to drift. Re-delivery is safe by
/// construction: the core dedups bare fills on `trade_id` and the FSM wrapper on the composite
/// `"{trade_id}:{order_id}"`, and [`crate::exec_plane::fill_tracker`] holds its own per-order seen set on that
/// same composite — the property the A3 resync already leans on.
pub(crate) fn replay_parked(
    parked: &[ParkedEvent],
    reg: &PolymarketRegistry,
    tracker: Option<&FillTracker>,
) -> Vec<Event> {
    let mut out = Vec::new();
    for p in parked {
        match p.kind {
            UserEventKind::Trade => decode_trade(&p.frame, reg, tracker, &mut out, Park::Off),
            UserEventKind::Order => decode_order(&p.frame, reg, tracker, &mut out, Park::Off),
        }
    }
    out
}

/// Decode a user-channel frame (single object or array), re-keying CLOB ids to coids via `reg`.
///
/// An event whose CLOB id `reg` cannot resolve is **parked**, not dropped — see this module's doc
/// for why "absent from the registry" and "not ours" are different claims. Every call also sweeps
/// the park's TTL, which is what turns the genuinely-not-ours remainder into a `warn!` + counter
/// instead of an unbounded pile; the sweep short-circuits on an empty park, so the steady state
/// costs one uncontended lock acquisition per frame.
pub fn decode_user(frame: &serde_json::Value, reg: &PolymarketRegistry) -> Vec<Event> {
    decode_user_with_tracker(frame, reg, None)
}

/// [`decode_user`] plus the optional dust-snap [`FillTracker`] (`crate::exec_plane::fill_tracker`) applied to
/// every fill before emission. `None` is byte-identical to [`decode_user`].
pub fn decode_user_with_tracker(
    frame: &serde_json::Value,
    reg: &PolymarketRegistry,
    tracker: Option<&FillTracker>,
) -> Vec<Event> {
    // Age out the genuinely-foreign remainder. Driven off the inbound frame rather than a timer
    // because this lane owns no clock: the pump's 60s idle watchdog guarantees a socket that has
    // gone quiet is torn down and resubscribed, so frames always resume and the sweep always runs.
    reg.expire_pending();
    let mut out = Vec::new();
    match frame {
        serde_json::Value::Array(a) => {
            a.iter().for_each(|e| decode_one(e, reg, tracker, &mut out, Park::On))
        }
        obj => decode_one(obj, reg, tracker, &mut out, Park::On),
    }
    out
}

#[path = "user_ws_tests.rs"]
#[cfg(test)]
mod user_ws_tests;
