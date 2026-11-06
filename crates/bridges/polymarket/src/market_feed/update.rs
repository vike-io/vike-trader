//! The per-frame protocol: route each decoded update to its token's slot, fold it, emit per mode.

use std::sync::Arc;

use vike_bridge_core::market_pump::FrameOutcome;
use vike_bridge_core::stream_health::StreamHealth;
use vike_data::LiveDataSink;
use vike_marketdata::{BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};
use vike_model::now_ms;

use super::VENUE;
use super::state::{PumpMode, TokenSlot, TokenState};
use crate::ws::{MarketUpdate, decode_market};

/// Synthetic-anchor cadence: after this many applied deltas since the last anchor, the pump
/// emits a full-book `Snapshot` (from its own folded state) so a replay never has to seek
/// further back than this many events to re-anchor. Venue snapshots also reset the counter.
const ANCHOR_EVERY_DELTAS: u64 = 512;

/// The token a decoded update belongs to — the ROUTING key of a batched socket. Every
/// market-channel frame names its asset (`book`/`last_trade_price` at the top level, `price_change`
/// per `price_changes` entry, which [`decode_market`] has already split into one update per asset).
fn update_asset_id(up: &MarketUpdate) -> &str {
    match up {
        MarketUpdate::Book { book, .. } => &book.asset_id,
        MarketUpdate::PriceChange { asset_id, .. } => asset_id,
        MarketUpdate::LastTrade { asset_id, .. } => asset_id,
    }
}

/// The slot this socket keeps for `asset_id`, or `None` when no subscription on this socket owns it
/// (a stray cross-token push, or a token whose seat was just released — ignored, never applied).
/// Linear over the shard's handful of seats, in subscribe order, so disclosure order is stable.
fn slot_for<'s>(slots: &'s mut [TokenSlot], asset_id: &str) -> Option<&'s mut TokenSlot> {
    slots.iter_mut().find(|s| s.token_id == asset_id)
}

/// Apply one decoded update to `state`/`sink` per `mode`'s emission rules (module doc). Updates for
/// an `asset_id` other than `token_id` are ignored defensively — the caller has already ROUTED this
/// update to the slot owning `token_id` ([`slot_for`]), so the guard is redundant-by-construction
/// and kept as the belt-and-suspenders it always was.
fn handle_update(
    mode: PumpMode,
    state: &mut TokenState,
    sink: &dyn LiveDataSink,
    token_id: &str,
    up: &MarketUpdate,
    health: &mut StreamHealth,
) {
    match up {
        MarketUpdate::Book { book: pb, ts }
            if pb.asset_id == token_id && mode != PumpMode::Trades =>
        {
            state.seq += 1;
            Arc::make_mut(&mut state.book).apply_snapshot(state.seq, &pb.bids, &pb.asks);
            state.deltas_since_anchor = 0;
            if mode == PumpMode::Book {
                // Recordable snapshot anchor: the RAW wire levels + the freshly bumped feed-local
                // seq, emitted BEFORE `after_book_change`'s derived quote so the recorded order
                // (book_update then quote) matches the replay tie-break (Book < Quote).
                sink.book_update(
                    VENUE,
                    token_id,
                    BookUpdate {
                        ts: ts.unwrap_or_else(now_ms),
                        local_ts: now_ms(),
                        seq: state.seq,
                        kind: BookUpdateKind::Snapshot,
                        tick_size: state.book.tick_size,
                        bids: pb.bids.clone(),
                        asks: pb.asks.clone(),
                        symbol: String::new(),
                    },
                );
            }
            after_book_change(mode, state, sink, token_id, *ts, health);
        }
        MarketUpdate::PriceChange { asset_id, changes, ts }
            if asset_id == token_id && mode != PumpMode::Trades =>
        {
            let bids: Vec<BookLevel> = changes
                .iter()
                .filter(|c| c.is_bid)
                .map(|c| BookLevel { price: c.price, qty: c.size })
                .collect();
            let asks: Vec<BookLevel> = changes
                .iter()
                .filter(|c| !c.is_bid)
                .map(|c| BookLevel { price: c.price, qty: c.size })
                .collect();
            state.seq += 1;
            Arc::make_mut(&mut state.book).apply_delta(state.seq, &bids, &asks);
            if mode == PumpMode::Book {
                // Recordable delta: the RAW changed levels + the freshly bumped feed-local seq,
                // emitted BEFORE `after_book_change`'s derived quote (recorded order matches the
                // replay tie-break Book < Quote).
                sink.book_update(
                    VENUE,
                    token_id,
                    BookUpdate {
                        ts: ts.unwrap_or_else(now_ms),
                        local_ts: now_ms(),
                        seq: state.seq,
                        kind: BookUpdateKind::Delta,
                        tick_size: state.book.tick_size,
                        bids: bids.clone(),
                        asks: asks.clone(),
                        symbol: String::new(),
                    },
                );
                state.deltas_since_anchor += 1;
                if state.deltas_since_anchor >= ANCHOR_EVERY_DELTAS {
                    state.deltas_since_anchor = 0;
                    state.seq += 1; // the anchor is its own event in the seq chain
                    // The synthetic anchor bumps `state.seq` WITHOUT a book mutation —
                    // `apply_delta`'s next-seq check only requires `seq > last_seq`, and the live
                    // book's `last_seq` now lags the emitted chain by the anchor bumps. That is fine
                    // for the LIVE book (its seq only gates staleness) but replay's contiguity rule
                    // needs the emitted chain contiguous — which it is: every emitted event's seq
                    // increments by exactly 1.
                    let (ab, aa) = state.book.top_n(usize::MAX);
                    sink.book_update(
                        VENUE,
                        token_id,
                        BookUpdate {
                            ts: ts.unwrap_or_else(now_ms),
                            local_ts: now_ms(),
                            seq: state.seq,
                            kind: BookUpdateKind::Snapshot,
                            tick_size: state.book.tick_size,
                            bids: ab,
                            asks: aa,
                            symbol: String::new(),
                        },
                    );
                }
            }
            after_book_change(mode, state, sink, token_id, *ts, health);
        }
        MarketUpdate::LastTrade { asset_id, price, size, taker_is_buy, ts }
            if asset_id == token_id && mode == PumpMode::Trades =>
        {
            // The SAME ts value handed to `sink.trade` feeds the data-freshness timer (§B): a real
            // trade frame is fresh DATA (unlike a keepalive PONG, which never reaches here).
            let tick_ts = ts.unwrap_or_else(now_ms);
            health.observe_data(tick_ts);
            sink.trade(
                VENUE,
                token_id,
                TradeTick {
                    ts: tick_ts,
                    local_ts: now_ms(),
                    price: *price,
                    size: *size,
                    // taker buy ⇒ the resting (maker) side was a sell ⇒ is_buyer_maker = false
                    is_buyer_maker: !taker_is_buy.unwrap_or(true),
                    symbol: String::new(),
                },
            );
        }
        // wrong asset_id, wrong mode for this frame type, or (defensively) an unmatched shape
        _ => {}
    }
}

/// Post book-mutation hook shared by the `Book`/`PriceChange` arms: pushes `sink.book()` (Book
/// mode only) then the derived-quote emission (`Book`+`Quotes`), change-gated on `last_top`.
/// `frame_ts` is the wire ts of the book/price_change frame that triggered this call (module doc's
/// Timestamps section) — used for the derived `QuoteTick.ts`, falling back to `now_ms()` when the
/// frame carried none.
fn after_book_change(
    mode: PumpMode,
    state: &mut TokenState,
    sink: &dyn LiveDataSink,
    token_id: &str,
    frame_ts: Option<i64>,
    health: &mut StreamHealth,
) {
    // Resolve the frame ts ONCE so the data-freshness timer (§B) observes the exact value the
    // derived quote carries. Observe per APPLIED book/price_change frame — not per emitted quote —
    // so a busy book whose top happens to stay put (deep-level churn only, no `sink.quote`) still
    // counts as live DATA and never false-trips staleness. Keepalive PONGs never reach here.
    let ts = frame_ts.unwrap_or_else(now_ms);
    health.observe_data(ts);
    if mode == PumpMode::Book {
        // Refcount bump, NOT a deep copy of both `BTreeMap`s (perf audit finding #1): the pump
        // keeps folding into the same allocation until a consumer still holds it at the next
        // mutation, at which point `Arc::make_mut` copies ON THIS THREAD.
        sink.book(VENUE, token_id, Arc::clone(&state.book));
    }
    if (mode == PumpMode::Book || mode == PumpMode::Quotes)
        && let (
            Some(BookLevel { price: bid, qty: bid_size }),
            Some(BookLevel { price: ask, qty: ask_size }),
        ) = (state.book.best_bid(), state.book.best_ask())
    {
        let top = (bid, ask, bid_size, ask_size);
        if state.last_top != Some(top) {
            state.last_top = Some(top);
            sink.quote(
                VENUE,
                token_id,
                QuoteTick {
                    ts,
                    local_ts: now_ms(),
                    bid,
                    ask,
                    bid_size,
                    ask_size,
                    symbol: String::new(),
                },
            );
        }
    }
}

/// One inbound TEXT frame → the driver's [`FrameOutcome`], running the whole per-frame protocol —
/// the `on_text` body shared VERBATIM by [`run_session`] (the test/replay seam) and [`feed_main`]
/// (production), so the two cannot drift:
/// - §B transport recovery: the FIRST inbound frame after a gap — market data OR the `"PONG"`
///   keepalive, i.e. proof the transport is responsive again — closes any open gap (`Live`),
///   ordered BEFORE this frame's own data so the consumer sees "recovered" first (`recover()` is a
///   no-op when no gap is open).
/// - a malformed (non-JSON) frame — junk or the `"PONG"` keepalive — is skipped as data
///   (`Ignore`), never an error; it already counted as a liveness tick inside the stream.
/// - a JSON frame's updates are ROUTED per `asset_id` to the owning [`TokenSlot`] ([`slot_for`])
///   and fold through [`handle_update`] there (each APPLIED data frame observes its ts into that
///   token's own freshness half), then `wake` fires once per PROCESSED frame (a frame may yield zero
///   or more sink calls, for zero or more of this socket's tokens) — `Confirm`, though the
///   polymarket row arms no ack watchdog, so the outcome is inert to the lifecycle.
///
/// With one slot this is byte-for-byte the pre-batching body (the recover loop runs once, the router
/// resolves to the only slot, and an unowned `asset_id` is dropped exactly as the old `asset_id ==
/// token_id` guard dropped it).
pub(super) fn on_frame(
    txt: &str,
    mode: PumpMode,
    slots: &mut [TokenSlot],
    sink: &dyn LiveDataSink,
    stream_label: &str,
    wake: &dyn Fn(),
) -> FrameOutcome {
    // Transport recovery is a SOCKET fact: any inbound frame proves the shared transport is
    // responsive again, so every token riding it closes its own open gap (one `Live` each).
    for slot in slots.iter_mut() {
        if let Some(ev) = slot.health.recover() {
            sink.stream_status(VENUE, &slot.token_id, stream_label, ev);
        }
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(txt) else {
        return FrameOutcome::Ignore; // malformed frame / "PONG" keepalive — ignored as data
    };
    for up in decode_market(&value) {
        let Some(slot) = slot_for(slots, update_asset_id(&up)) else { continue };
        handle_update(mode, &mut slot.state, sink, &slot.token_id, &up, &mut slot.health);
    }
    wake();
    FrameOutcome::Confirm
}

/// The driver's transport-ALIVE read-timeout tick (`on_tick` — the knob added FOR this venue):
/// the moment to judge DATA freshness, distinct from the transport gap the driver's idle watchdog
/// owns. Discloses `Stale` ONCE when the newest applied data ts ages past the mode's freshness
/// threshold ([`PumpMode::freshness_threshold`]) behind the live socket, and `Live` once on
/// recovery; a no-op until then. The driver only fires this AFTER the idle watchdog passes, so it
/// never races the transport gap — `StreamHealth::check_freshness` also gates internally on
/// `in_gap()`, belt-and-suspenders, so this can never double-signal. This catches the failure the
/// transport watchdog structurally CANNOT: a reconnect whose re-subscribe silently failed, so the
/// socket answers keepalives (reads alive) yet no book/trade frame ever arrives.
/// Judged PER TOKEN on a batched socket: a shard whose busy token keeps the wire hot while a quiet
/// co-tenant's book has frozen must still disclose that co-tenant's staleness, so each slot runs its
/// own `check_freshness` against its own newest applied data ts.
pub(super) fn on_alive_tick(slots: &mut [TokenSlot], sink: &dyn LiveDataSink, stream_label: &str) {
    let now = now_ms();
    for slot in slots.iter_mut() {
        if let Some(ev) = slot.health.check_freshness(now) {
            sink.stream_status(VENUE, &slot.token_id, stream_label, ev);
        }
    }
}
