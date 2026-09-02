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
//! Now an unresolvable event is **parked** ([`crate::pending_events`]) under the id it is waiting
//! on and replayed the instant the registry gains that id — atomically, so the exec thread cannot
//! slip its `on_accept` into the gap. What was never claimed inside the TTL is the genuine "not
//! ours" case and dies with a `warn!` and a counter, never silently. [`crate::registry`]'s doc is
//! the authority on the two races that produce an absent id and on the settling grace that covers
//! the second one.
//!
//! maker vs taker: a trade lists our order as `taker_order_id` (top-level
//! size/price) or inside `maker_orders[]` (that entry's matched_amount/price). Fills emit on the
//! good statuses (MATCHED/MINED/CONFIRMED); RETRYING/FAILED are skipped — the A3 resync repairs the
//! rare MATCHED-then-FAILED divergence (Nautilus-aligned; no fill reversal). A stable composite
//! `trade_id` = `"{trade_id}:{order_id}"` makes the repeated status messages dedup in the core
//! (both its bare-`Fill` `seen_trade_ids` and its wrapper `seen_fsm_trade_ids`).

use super::config::PolymarketCreds;
use super::fill_tracker::{FillTracker, SnapOutcome, DUST_TRADE_ID_SUFFIX};
use super::pending_events::{ParkedEvent, UserEventKind};
use super::registry::PolymarketRegistry;
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
/// (`crate::fill_tracker`): a dust overfill is capped down to the submitted qty, and ONLY a fill
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
/// status-repeat key). It is also the exact string `crate::recon_client::parse_fill_reports` stamps,
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
/// resolves. Called by `crate::client`'s submit arm — the ONE place that learns "this id is ours"
/// — which sends the result into the same lossless event lane the pump uses.
///
/// Replaying the stored frame through the real decoder (rather than reconstructing a fill from
/// extracted fields) means a recovered event is byte-identical to what the live path would have
/// emitted had the id been present, with no parallel extraction to drift. Re-delivery is safe by
/// construction: the core dedups bare fills on `trade_id` and the FSM wrapper on the composite
/// `"{trade_id}:{order_id}"`, and [`crate::fill_tracker`] holds its own per-order seen set on that
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

/// [`decode_user`] plus the optional dust-snap [`FillTracker`] (`crate::fill_tracker`) applied to
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PolymarketCreds;
    use crate::registry::PolymarketRegistry;
    use vike_model::events::LiquiditySide;

    // ---- dust-snap tracker, scripted through the real decoder ----

    fn taker_trade(id: &str, size: &str, price: &str) -> serde_json::Value {
        serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": id, "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": size, "price": price,
            "taker_order_id": "0xORD", "maker_orders": []
        })
    }

    fn order_update(matched: &str) -> serde_json::Value {
        serde_json::json!({
            "event_type": "order", "type": "UPDATE", "id": "0xORD", "asset_id": "111",
            "original_size": "100", "size_matched": matched
        })
    }

    /// Every emitted bare `Fill`'s (trade_id, qty) in order.
    fn fills(evs: &[Event]) -> Vec<(String, f64)> {
        evs.iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some((f.trade_id.to_string(), f.last_qty)),
                _ => None,
            })
            .collect()
    }

    fn tracked_reg() -> (PolymarketRegistry, FillTracker) {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("coid-1", "0xORD", 1);
        let t = FillTracker::new();
        t.register("coid-1", 100.0);
        (reg, t)
    }

    #[test]
    fn dust_overfill_is_snapped_before_emission() {
        let (reg, t) = tracked_reg();
        let a = decode_user_with_tracker(&taker_trade("t1", "60", "0.50"), &reg, Some(&t));
        assert_eq!(fills(&a), vec![("t1:0xORD".to_string(), 60.0)]);
        // venue overfills the tail by 0.02 → snapped down to the exact 40.0 remaining
        let b = decode_user_with_tracker(&taker_trade("t2", "40.02", "0.51"), &reg, Some(&t));
        assert_eq!(fills(&b), vec![("t2:0xORD".to_string(), 40.0)]);
        // fully filled: a further dust match snaps to zero and emits NOTHING
        assert!(
            decode_user_with_tracker(&taker_trade("t3", "0.01", "0.51"), &reg, Some(&t)).is_empty()
        );
    }

    #[test]
    fn untracked_tracker_none_is_byte_identical() {
        let (reg, t) = tracked_reg();
        let frame = taker_trade("t1", "100.02", "0.50");
        let plain = decode_user(&frame, &reg);
        assert_eq!(fills(&plain), vec![("t1:0xORD".to_string(), 100.02)]);
        // ...and the same via the explicit-None entry point
        assert_eq!(fills(&decode_user_with_tracker(&frame, &reg, None)), fills(&plain));
        // tracked, the same frame snaps
        assert_eq!(
            fills(&decode_user_with_tracker(&frame, &reg, Some(&t))),
            vec![("t1:0xORD".to_string(), 100.0)]
        );
    }

    /// FINDING 3 regression (this is the test the old
    /// `residual_seen_first_at_the_terminal_update_completes_there` claimed to be): the dust
    /// residual is minted at the TERMINAL UPDATE, and exactly once across a duplicate terminal.
    #[test]
    fn dust_residual_is_minted_at_the_terminal_update_exactly_once() {
        let (reg, t) = tracked_reg();
        // venue's matches stop 0.02 short of our 100 — but the order may still be RESTING, so
        // nothing is synthesized at the trade event.
        let a = decode_user_with_tracker(&taker_trade("t1", "99.98", "0.62"), &reg, Some(&t));
        assert_eq!(fills(&a), vec![("t1:0xORD".to_string(), 99.98)], "no dust mid-life");

        // the venue now calls it done → ONE synthetic completing fill, then the terminal wrap
        let term = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
        let got = fills(&term);
        assert_eq!(got.len(), 1, "exactly one dust fill: {term:?}");
        assert_eq!(got[0].0, "0xORD:dust");
        assert!((got[0].1 - 0.02).abs() < 1e-9, "{got:?}");
        if let Event::Fill(f) = &term[0] {
            assert_eq!(f.last_px, 0.62, "synthetic fill mints at the last fill price");
            assert_eq!(f.side, 1);
        } else {
            panic!("expected the dust Fill first: {term:?}");
        }
        assert!(matches!(term[2], Event::OrderFilled(_)));

        // a duplicate terminal must NOT mint a second dust fill
        let dup = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
        assert!(fills(&dup).is_empty(), "duplicate terminal re-minted: {dup:?}");
        assert!(matches!(dup[0], Event::OrderFilled(_)));
        assert!(t.is_empty(), "completed order evicted");
    }

    /// FINDING 2 regression: a sub-tolerance remainder that is still RESTING must not be force
    /// completed mid-life — otherwise the venue's later real fill of that remainder is folded on
    /// top of an already-minted synthetic one (double-counted position).
    #[test]
    fn a_resting_sub_threshold_remainder_is_not_force_completed() {
        let (reg, t) = tracked_reg();
        let a = decode_user_with_tracker(&taker_trade("t1", "99.97", "0.62"), &reg, Some(&t));
        assert_eq!(fills(&a), vec![("t1:0xORD".to_string(), 99.97)], "no dust: {a:?}");
        assert_eq!(t.len(), 1, "still tracked — the 0.03 may still be working");

        // the venue really fills the remainder later: it is emitted ONCE, as a real fill
        let b = decode_user_with_tracker(&taker_trade("t2", "0.03", "0.62"), &reg, Some(&t));
        assert_eq!(fills(&b), vec![("t2:0xORD".to_string(), 0.03)]);
        // and the terminal now has nothing left to synthesize
        let term = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
        assert!(fills(&term).is_empty(), "no dust after a complete fill: {term:?}");
        assert!(matches!(term[0], Event::OrderFilled(_)));
    }

    /// FINDING 1 regression, through the real decoder: the venue sends MATCHED, MINED and
    /// CONFIRMED for the SAME match. Each is re-emitted (the core dedups on the composite id) but
    /// the tracker's cumulative must advance only once — otherwise the tail fill gets snapped and
    /// real qty is silently discarded.
    #[test]
    fn status_repeats_do_not_inflate_the_cumulative() {
        let (reg, t) = tracked_reg();
        for status in ["MATCHED", "MINED", "CONFIRMED"] {
            let mut ev = taker_trade("t1", "0.02", "0.50");
            ev["status"] = serde_json::json!(status);
            let evs = decode_user_with_tracker(&ev, &reg, Some(&t));
            assert_eq!(fills(&evs), vec![("t1:0xORD".to_string(), 0.02)], "{status}");
        }
        // the real tail: 99.98 remains, so it must pass through UNSNAPPED and in full
        let tail = decode_user_with_tracker(&taker_trade("t2", "99.98", "0.50"), &reg, Some(&t));
        assert_eq!(fills(&tail), vec![("t2:0xORD".to_string(), 99.98)], "tail was snapped away");
        let term = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
        assert!(fills(&term).is_empty(), "nothing left to synthesize: {term:?}");
    }

    /// TEST GAP: the maker branch (`maker_orders[].matched_amount`) is the path a mounted
    /// SpreadMaker actually takes; prove the tracker applies there too.
    #[test]
    fn maker_branch_snaps_and_completes_through_the_tracker() {
        let (reg, t) = tracked_reg();
        let maker_trade = |id: &str, amt: &str| {
            serde_json::json!({
                "event_type": "trade", "type": "TRADE", "id": id, "status": "MATCHED",
                "asset_id": "111", "side": "BUY", "size": "5", "price": "0.9",
                "taker_order_id": "0xSOMEONE",
                "maker_orders": [{ "order_id": "0xORD", "matched_amount": amt, "price": "0.62" }]
            })
        };
        let a = decode_user_with_tracker(&maker_trade("m1", "60"), &reg, Some(&t));
        assert_eq!(fills(&a), vec![("m1:0xORD".to_string(), 60.0)]);
        // 40.02 for the tail → snapped down to the exact 40.0 remaining
        let b = decode_user_with_tracker(&maker_trade("m2", "40.02"), &reg, Some(&t));
        assert_eq!(fills(&b), vec![("m2:0xORD".to_string(), 40.0)]);
        // and a repeat of that maker match folds nothing further
        let c = decode_user_with_tracker(&maker_trade("m2", "40.02"), &reg, Some(&t));
        assert_eq!(fills(&c), vec![("m2:0xORD".to_string(), 40.0)]);
        assert_eq!(t.check_dust_residual("coid-1"), None, "exactly full");
    }

    #[test]
    fn non_dust_remainder_is_never_synthesized_at_the_terminal() {
        let (reg, t) = tracked_reg();
        decode_user_with_tracker(&taker_trade("t1", "40", "0.5"), &reg, Some(&t));
        let term = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
        assert!(
            fills(&term).is_empty(),
            "60 short is a real remainder, never synthesized: {term:?}"
        );
        assert!(matches!(term[0], Event::OrderFilled(_)));
        assert!(t.is_empty(), "terminal evicts even without a dust mint");
    }

    #[test]
    fn cancellation_evicts_and_never_mints_dust() {
        let (reg, t) = tracked_reg();
        decode_user_with_tracker(&taker_trade("t1", "90", "0.62"), &reg, Some(&t)); // 10 short
        assert_eq!(t.len(), 1);
        let cancel =
            serde_json::json!({ "event_type": "order", "type": "CANCELLATION", "id": "0xORD" });
        let evs = decode_user_with_tracker(&cancel, &reg, Some(&t));
        assert!(fills(&evs).is_empty(), "a cancel never mints a completing fill: {evs:?}");
        assert!(matches!(&evs[0], Event::OrderCanceled(c) if c.client_order_id == "coid-1"));
        assert!(t.is_empty(), "cancel evicts the ledger entry");
    }

    #[test]
    fn subscribe_carries_auth() {
        let creds = PolymarketCreds {
            api_key: "k".into(),
            secret: "s".into(),
            passphrase: "p".into(),
            ..Default::default()
        };
        let v: serde_json::Value =
            serde_json::from_str(&user_subscribe_message(&creds, &["111".into()])).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["auth"]["apiKey"], "k");
        assert_eq!(v["markets"][0], "111");
    }

    #[test]
    fn taker_fill_is_rekeyed_to_coid() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("my-coid", "0xTAKER", 1); // we are the taker, BUY
        let trade = serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": "trd1", "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "100", "price": "0.52",
            "taker_order_id": "0xTAKER", "maker_orders": []
        });
        let evs = decode_user(&trade, &reg);
        assert_eq!(evs.len(), 2);
        match (&evs[0], &evs[1]) {
            (Event::Fill(f), Event::OrderPartiallyFilled(w)) => {
                assert_eq!(f.client_order_id, "my-coid");
                assert_eq!(f.trade_id, "trd1:0xTAKER");
                assert_eq!(f.side, 1);
                assert_eq!(f.last_qty, 100.0);
                assert_eq!(f.last_px, 0.52);
                assert_eq!(f.liquidity_side, LiquiditySide::Taker);
                assert_eq!(w.client_order_id, "my-coid");
            }
            other => panic!("expected Fill + OrderPartiallyFilled, got {other:?}"),
        }
    }

    /// A trade frame with no wire `id` emits NOTHING — the bare `Fill` and its
    /// `OrderPartiallyFilled` wrap stand or fall together, because the composite
    /// `"{id}:{order_id}"` is the dedup key on both core paths and `":{order_id}"` is per-ORDER, so
    /// the MATCHED → MINED → CONFIRMED repeats of one match would each book again.
    ///
    /// This gates the refusal, not the type: reverting `decode_trade`'s empty-`id` guard to a
    /// permissive composite turns the asserted 0 back into 2 per frame. Absent, `null` and `""` all
    /// reach the guard, and the taker AND maker legs are both covered.
    #[test]
    fn a_trade_frame_without_an_id_emits_no_fill_on_either_leg() {
        for id in [None, Some(serde_json::Value::Null), Some(serde_json::json!(""))] {
            for (label, taker, maker_oid) in
                [("taker", "0xTAKER", "0xSTRANGER"), ("maker", "0xSTRANGER", "0xMAKER")]
            {
                let reg = PolymarketRegistry::new();
                let _ = reg.on_accept("my-coid", "0xTAKER", 1);
                let _ = reg.on_accept("maker-coid", "0xMAKER", -1);
                let mut trade = serde_json::json!({
                    "event_type": "trade", "type": "TRADE", "status": "MATCHED",
                    "asset_id": "111", "side": "BUY", "size": "100", "price": "0.52",
                    "taker_order_id": taker,
                    "maker_orders": [{"order_id": maker_oid, "matched_amount": "100", "price": "0.52"}]
                });
                if let Some(v) = id.clone() {
                    trade["id"] = v;
                }
                let evs = decode_user(&trade, &reg);
                assert!(
                    evs.is_empty(),
                    "an id-less trade frame must emit nothing on the {label} leg (id={id:?}), got \
                     {evs:?}"
                );
            }
        }
    }

    /// The sibling guard on the ORDER lane: a terminal `UPDATE` with no `id` mints neither the
    /// `:dust` completion nor the `:filled` terminal marker. Reverting either to a permissive
    /// constructor turns the asserted 0 back into 1-3 events.
    ///
    /// ⚠ Reached here by registering the EMPTY clob id, which is the only way an order frame can
    /// re-key to a coid and still carry no id — the shape the guard exists for.
    #[test]
    fn a_terminal_order_update_without_an_id_emits_nothing() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("my-coid", "", 1);
        let update = serde_json::json!({
            "event_type": "order", "type": "UPDATE", "id": "", "asset_id": "111",
            "original_size": "100", "size_matched": "100"
        });
        let evs = decode_user(&update, &reg);
        assert!(evs.is_empty(), "no id ⇒ no terminal wrap and no dust mint, got {evs:?}");
    }

    /// ...and the two minted ids keep their exact byte shapes, which the dedup sets and
    /// `tests/offline/dust_snap_engine.rs` both depend on.
    #[test]
    fn the_minted_terminal_ids_keep_their_byte_shapes() {
        let (reg, t) = tracked_reg();
        decode_user_with_tracker(&taker_trade("t1", "99.999", "0.62"), &reg, Some(&t));
        let evs = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
        let ids: Vec<String> = evs
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f.trade_id.to_string()),
                Event::OrderFilled(w) => Some(w.fill.trade_id.to_string()),
                Event::OrderPartiallyFilled(w) => Some(w.fill.trade_id.to_string()),
                _ => None,
            })
            .collect();
        assert!(ids.contains(&"0xORD:dust".to_string()), "the dust mint's shape: {ids:?}");
        assert!(ids.contains(&"0xORD:filled".to_string()), "the terminal marker's shape: {ids:?}");
    }

    #[test]
    fn maker_fill_uses_maker_orders_entry_and_registered_side() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("maker-coid", "0xMAKER", -1); // our resting SELL was hit
        let trade = serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": "trd2", "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "100", "price": "0.99",
            "taker_order_id": "0xSOMEONE",
            "maker_orders": [{ "order_id": "0xMAKER", "matched_amount": "40", "price": "0.55" }]
        });
        let evs = decode_user(&trade, &reg);
        assert_eq!(evs.len(), 2);
        if let Event::Fill(f) = &evs[0] {
            assert_eq!(f.client_order_id, "maker-coid");
            assert_eq!(f.trade_id, "trd2:0xMAKER");
            assert_eq!(f.side, -1); // our registered side, not the taker's BUY
            assert_eq!(f.last_qty, 40.0); // the maker entry's matched_amount
            assert_eq!(f.last_px, 0.55); // the maker entry's price
            assert_eq!(f.liquidity_side, LiquiditySide::Maker);
        } else {
            panic!("expected a maker Fill");
        }
    }

    #[test]
    fn failed_and_retrying_trades_emit_nothing() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("c", "0xTAKER", 1);
        for status in ["FAILED", "RETRYING"] {
            let trade = serde_json::json!({
                "event_type": "trade", "id": "t", "status": status,
                "asset_id": "111", "side": "BUY", "size": "1", "price": "0.5",
                "taker_order_id": "0xTAKER", "maker_orders": []
            });
            assert!(decode_user(&trade, &reg).is_empty(), "{status} must emit nothing");
        }
    }

    /// Unknown ids still emit NOTHING at decode time (this is the pre-existing contract, unchanged)
    /// — but they are now STAGED rather than discarded, which is the whole fix. Previously named
    /// `unknown_orders_are_dropped`, and that name was the bug: "unknown" conflated "not ours" with
    /// "not yet ours".
    #[test]
    fn unknown_orders_emit_nothing_but_are_staged_not_dropped() {
        let reg = PolymarketRegistry::new(); // empty — nothing is ours (yet)
        let trade = serde_json::json!({
            "event_type": "trade", "id": "t", "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "1", "price": "0.5",
            "taker_order_id": "0xNOTOURS",
            "maker_orders": [{ "order_id": "0xALSONOT", "matched_amount": "1", "price": "0.5" }]
        });
        assert!(decode_user(&trade, &reg).is_empty(), "nothing can be emitted without a coid");
        // …but the frame is held under BOTH ids it names, since either could turn out to be ours.
        assert_eq!(reg.pending_len(), 2);
        assert_eq!(reg.pending_stats().parked, 2);
        assert_eq!(reg.pending_stats().expired, 0, "held, not lost");
    }

    #[test]
    fn order_update_full_fill_is_terminal_and_cancel_rekeys() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("coid-x", "0xORD", 1);
        let full = serde_json::json!({
            "event_type": "order", "type": "UPDATE", "id": "0xORD",
            "original_size": "100", "size_matched": "100"
        });
        assert!(matches!(&decode_user(&full, &reg)[0],
            Event::OrderFilled(f) if f.client_order_id == "coid-x" && f.fill.last_qty == 0.0));

        let partial = serde_json::json!({
            "event_type": "order", "type": "UPDATE", "id": "0xORD",
            "original_size": "100", "size_matched": "40"
        });
        assert!(decode_user(&partial, &reg).is_empty()); // partial handled by trade events, not here

        let cancel =
            serde_json::json!({ "event_type": "order", "type": "CANCELLATION", "id": "0xORD" });
        assert!(matches!(&decode_user(&cancel, &reg)[0],
            Event::OrderCanceled(c) if c.client_order_id == "coid-x"));
    }

    // ---- the ack race: an executed fill that arrives before its order is registered ----

    mod staging {
        use super::*;
        use crate::pending_events::{PendingStats, DEFAULT_TTL_MS};
        use crate::registry::DEFAULT_SETTLING_GRACE_MS;

        fn taker_trade_for(order_id: &str) -> serde_json::Value {
            serde_json::json!({
                "event_type": "trade", "type": "TRADE", "id": "trdX", "status": "MATCHED",
                "asset_id": "111", "side": "BUY", "size": "25", "price": "0.43",
                "timestamp": "1700",
                "taker_order_id": order_id, "maker_orders": []
            })
        }

        /// Every emitted bare `Fill` as (coid, trade_id, side, qty, px).
        fn shape(evs: &[Event]) -> Vec<(String, String, i32, f64, f64)> {
            evs.iter()
                .filter_map(|e| match e {
                    Event::Fill(f) => Some((
                        f.client_order_id.to_string(),
                        f.trade_id.to_string(),
                        f.side,
                        f.last_qty,
                        f.last_px,
                    )),
                    _ => None,
                })
                .collect()
        }

        /// **THE BUG.** A trade event that beats the HTTP ack used to be discarded at the re-key
        /// site, taking its position and realized PnL with it. It is now staged and delivered in
        /// full the moment `on_accept` writes the id.
        #[test]
        fn a_fill_that_beats_the_ack_is_delivered_when_the_order_registers() {
            let reg = PolymarketRegistry::new();
            let trade = taker_trade_for("0xORD");

            // t0 — the venue matched before our submit's HTTP response got home.
            assert!(decode_user(&trade, &reg).is_empty(), "nothing can be keyed yet");
            assert_eq!(reg.pending_stats().parked, 1);
            assert_eq!(reg.pending_stats().expired, 0, "NOT discarded");

            // t1 — the ack lands and the exec thread registers the id.
            let claimed = reg.on_accept("coid-1", "0xORD", 1);
            assert_eq!(claimed.len(), 1, "the staged frame comes back: {claimed:?}");

            // t2 — `crate::client::emit_replayed` decodes and sends it.
            let evs = replay_parked(&claimed, &reg, None);
            assert_eq!(
                shape(&evs),
                vec![("coid-1".to_string(), "trdX:0xORD".to_string(), 1, 25.0, 0.43)],
                "the fill is recovered whole: {evs:?}"
            );
            assert!(matches!(evs[1], Event::OrderPartiallyFilled(_)), "and its FSM wrap");
            assert_eq!(reg.pending_stats().replayed, 1);
            assert_eq!(reg.pending_len(), 0, "the park is drained");
        }

        /// The recovered event must be INDISTINGUISHABLE from what the live path would have emitted
        /// had the id been present — that is why the raw frame is stored and re-decoded rather than
        /// a fill being reconstructed from extracted fields.
        #[test]
        fn a_replayed_fill_is_identical_to_the_one_the_live_path_would_have_emitted() {
            let trade = taker_trade_for("0xORD");

            let raced = PolymarketRegistry::new();
            assert!(decode_user(&trade, &raced).is_empty());
            let replayed = replay_parked(&raced.on_accept("c", "0xORD", -1), &raced, None);

            let clean = PolymarketRegistry::new();
            let _ = clean.on_accept("c", "0xORD", -1);
            let live = decode_user(&trade, &clean);

            assert_eq!(shape(&replayed), shape(&live));
            assert_eq!(replayed.len(), live.len());
            assert_eq!(format!("{replayed:?}"), format!("{live:?}"), "byte-for-byte");
        }

        /// On this venue every post-acceptance terminal arrives ONLY on the user channel, so a
        /// terminal UPDATE lost to the ack race strands the order at PartiallyFilled forever — the
        /// order-shaped way to silently vanish.
        #[test]
        fn a_terminal_order_update_that_beats_the_ack_is_delivered_too() {
            let reg = PolymarketRegistry::new();
            let terminal = serde_json::json!({
                "event_type": "order", "type": "UPDATE", "id": "0xORD", "asset_id": "111",
                "original_size": "100", "size_matched": "100"
            });
            assert!(decode_user(&terminal, &reg).is_empty());
            assert_eq!(reg.pending_stats().parked, 1);

            let evs = replay_parked(&reg.on_accept("coid-t", "0xORD", 1), &reg, None);
            assert!(
                matches!(&evs[0], Event::OrderFilled(f) if f.client_order_id == "coid-t"),
                "the terminal is recovered: {evs:?}"
            );
        }

        /// A CANCELLATION that beats the ack is likewise recovered.
        #[test]
        fn a_cancellation_that_beats_the_ack_is_delivered_too() {
            let reg = PolymarketRegistry::new();
            let cancel =
                serde_json::json!({ "event_type": "order", "type": "CANCELLATION", "id": "0xORD" });
            assert!(decode_user(&cancel, &reg).is_empty());
            let evs = replay_parked(&reg.on_accept("coid-c", "0xORD", 1), &reg, None);
            assert!(matches!(&evs[0], Event::OrderCanceled(c) if c.client_order_id == "coid-c"));
        }

        /// A PLACEMENT decodes to nothing, so staging it would burn the bound to replay a no-op —
        /// and it is the MOST common frame in the ack race, arriving at acceptance by definition.
        #[test]
        fn a_placement_frame_is_never_staged() {
            let reg = PolymarketRegistry::new();
            let placement = serde_json::json!({
                "event_type": "order", "type": "PLACEMENT", "id": "0xORD",
                "original_size": "100", "size_matched": "0"
            });
            assert!(decode_user(&placement, &reg).is_empty());
            assert_eq!(reg.pending_len(), 0, "nothing worth holding");
            // …nor is a NON-terminal update (the trade events carry those partials)
            let partial = serde_json::json!({
                "event_type": "order", "type": "UPDATE", "id": "0xORD",
                "original_size": "100", "size_matched": "40"
            });
            assert!(decode_user(&partial, &reg).is_empty());
            assert_eq!(reg.pending_len(), 0);
        }

        /// A non-fillable status carries no money, so there is nothing to lose and nothing to hold.
        #[test]
        fn a_failed_trade_for_an_unknown_id_is_not_staged() {
            let reg = PolymarketRegistry::new();
            let mut t = taker_trade_for("0xORD");
            t["status"] = serde_json::json!("FAILED");
            assert!(decode_user(&t, &reg).is_empty());
            assert_eq!(reg.pending_len(), 0);
        }

        /// THE DISCRIMINATOR. When we take liquidity, `maker_orders` lists STRANGERS. Staging them
        /// would fire an expiry `warn!` on every ordinary taker fill and drown the signal — so a
        /// frame with a resolved leg is treated as fully attributed.
        #[test]
        fn a_trade_with_one_resolved_leg_does_not_stage_its_counterparties() {
            let reg = PolymarketRegistry::new();
            let _ = reg.on_accept("coid-1", "0xOURS", 1);
            let trade = serde_json::json!({
                "event_type": "trade", "type": "TRADE", "id": "t9", "status": "MATCHED",
                "asset_id": "111", "side": "BUY", "size": "10", "price": "0.5",
                "taker_order_id": "0xOURS",
                "maker_orders": [
                    { "order_id": "0xSTRANGER1", "matched_amount": "6", "price": "0.5" },
                    { "order_id": "0xSTRANGER2", "matched_amount": "4", "price": "0.5" }
                ]
            });
            let evs = decode_user(&trade, &reg);
            assert_eq!(shape(&evs).len(), 1, "only our leg emits: {evs:?}");
            assert_eq!(reg.pending_len(), 0, "counterparties are not 'not YET ours'");
            assert_eq!(reg.pending_stats().parked, 0);
        }

        /// …but when NOTHING resolves we cannot yet tell which id is ours, so the frame is held
        /// under every one of them and whichever registers claims it.
        #[test]
        fn a_fully_unresolved_trade_is_staged_under_every_id_and_the_right_one_claims_it() {
            let reg = PolymarketRegistry::new();
            let trade = serde_json::json!({
                "event_type": "trade", "type": "TRADE", "id": "t9", "status": "MATCHED",
                "asset_id": "111", "side": "BUY", "size": "10", "price": "0.5",
                "taker_order_id": "0xTAKER",
                "maker_orders": [{ "order_id": "0xMINE", "matched_amount": "10", "price": "0.5" }]
            });
            assert!(decode_user(&trade, &reg).is_empty());
            assert_eq!(reg.pending_len(), 2);

            // our resting MAKER order was the one that got hit
            let evs = replay_parked(&reg.on_accept("coid-m", "0xMINE", -1), &reg, None);
            assert_eq!(
                shape(&evs),
                vec![("coid-m".to_string(), "t9:0xMINE".to_string(), -1, 10.0, 0.5)],
                "our maker leg, with OUR side — not the taker's BUY: {evs:?}"
            );
            // the copy staged under the taker id is untouched, and re-decoding did not re-stage
            assert_eq!(reg.pending_len(), 1);
            assert_eq!(reg.pending_stats().parked, 2, "no re-park during replay");
        }

        /// Race 2, end to end: `crate::client` reads any HTTP 200 from `cancel_order` as success and
        /// runs `registry.remove`, so an order the venue JUST MATCHED is torn down like one that
        /// really rested. The settling grace keeps that already-executed fill foldable.
        #[test]
        fn a_match_that_raced_its_own_cancel_still_folds() {
            let reg = PolymarketRegistry::new();
            let _ = reg.on_accept("coid-r", "0xRACED", -1);
            reg.remove("coid-r"); // the cancel "succeeded"…

            let evs = decode_user(&taker_trade_for("0xRACED"), &reg); // …but the match was first
            assert_eq!(
                shape(&evs),
                vec![("coid-r".to_string(), "trdX:0xRACED".to_string(), -1, 25.0, 0.43)],
                "real money must fold, not vanish: {evs:?}"
            );
            assert_eq!(reg.pending_stats().settled, 1);
            assert_eq!(reg.pending_len(), 0, "resolved outright — never staged");
        }

        /// "not ours": no registration ever arrives, so the TTL discards it — LOUDLY (a `warn!` in
        /// `expire_pending_at`) and COUNTED, which is the difference from the bug being fixed.
        #[test]
        fn a_genuinely_unknown_trade_expires_and_is_counted() {
            // ttl 0 ⇒ already past it on the next sweep; no sleeping, no clock injection.
            let reg = PolymarketRegistry::with_limits(64, 8, 0, DEFAULT_SETTLING_GRACE_MS);
            assert!(decode_user(&taker_trade_for("0xFOREIGN"), &reg).is_empty());
            assert_eq!(reg.pending_len(), 1);
            assert_eq!(reg.pending_stats().expired, 0, "not swept yet");

            // the NEXT inbound frame sweeps (see decode_user_with_tracker)
            let _ = decode_user(&serde_json::json!({ "event_type": "noise" }), &reg);
            assert_eq!(reg.pending_len(), 0);
            assert_eq!(reg.pending_stats().expired, 1, "counted, not silent");
            assert_eq!(reg.pending_stats().replayed, 0);
            // and it is really gone — a late registration finds nothing
            assert!(reg.on_accept("coid-late", "0xFOREIGN", 1).is_empty());
        }

        /// The bound behaves at the decoder seam: at capacity the OLDEST staged order is shed and
        /// counted as `evicted` (a bound problem), never as `expired` (a foreign event).
        #[test]
        fn the_bound_sheds_the_oldest_and_keeps_the_newest_claimable() {
            let reg =
                PolymarketRegistry::with_limits(2, 8, DEFAULT_TTL_MS, DEFAULT_SETTLING_GRACE_MS);
            for id in ["0xA", "0xB", "0xC"] {
                assert!(decode_user(&taker_trade_for(id), &reg).is_empty());
            }
            assert_eq!(reg.pending_len(), 2, "held at the bound");
            assert_eq!(reg.pending_stats().evicted, 1);
            assert_eq!(reg.pending_stats().expired, 0, "an eviction is not an expiry");
            assert!(reg.on_accept("c", "0xA", 1).is_empty(), "the oldest was shed");
            assert_eq!(reg.on_accept("c", "0xC", 1).len(), 1, "the newest survived");
        }

        /// REGRESSION: the steady state — a registered order's fill — is completely untouched. No
        /// staging, no counter movement, no extra events.
        #[test]
        fn a_normal_registered_fill_is_unchanged() {
            let reg = PolymarketRegistry::new();
            let _ = reg.on_accept("coid-n", "0xORD", 1);
            let evs = decode_user(&taker_trade_for("0xORD"), &reg);
            assert_eq!(
                shape(&evs),
                vec![("coid-n".to_string(), "trdX:0xORD".to_string(), 1, 25.0, 0.43)]
            );
            assert_eq!(evs.len(), 2, "Fill + wrap, nothing else");
            assert_eq!(reg.pending_len(), 0);
            assert_eq!(
                reg.pending_stats(),
                PendingStats::default(),
                "the whole staging lane is inert in the steady state"
            );
        }

        /// The A3 resync used to replay through the SAME re-key gate, which is why it could never
        /// recover what the live path dropped. It now stages too.
        #[test]
        fn the_history_resync_path_stages_instead_of_dropping() {
            let reg = PolymarketRegistry::new();
            let row = serde_json::json!({
                "id": "h1", "status": "CONFIRMED", "asset_id": "111",
                "side": "BUY", "size": "10", "price": "0.5",
                "taker_order_id": "0xLATE", "maker_orders": []
            });
            assert!(decode_typed("trade", &row, &reg).is_empty());
            assert_eq!(reg.pending_stats().parked, 1);
            assert_eq!(replay_parked(&reg.on_accept("coid-h", "0xLATE", 1), &reg, None).len(), 2);
        }

        /// The dust-snap tracker still applies to a recovered fill — replay goes through the real
        /// `emit_fill`, so nothing about the tracked path is special-cased.
        #[test]
        fn a_replayed_fill_still_goes_through_the_dust_snap_tracker() {
            let reg = PolymarketRegistry::new();
            let t = FillTracker::new();
            let mut trade = taker_trade_for("0xORD");
            trade["size"] = serde_json::json!("25.02"); // a cent-tick overfill of our 25

            assert!(decode_user_with_tracker(&trade, &reg, Some(&t)).is_empty());
            t.register("coid-1", 25.0); // the exec thread registers on the ack, then …
            let claimed = reg.on_accept("coid-1", "0xORD", 1);
            let evs = replay_parked(&claimed, &reg, Some(&t));
            assert_eq!(shape(&evs)[0].3, 25.0, "snapped down on replay: {evs:?}");
        }
    }
}
