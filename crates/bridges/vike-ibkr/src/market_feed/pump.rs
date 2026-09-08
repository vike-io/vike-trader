//! Per-verb pump-thread bodies for `IbkrFeeds`. Each drains an ibapi `Subscription<T>` via
//! `next_timeout` (polling `ctx.stop`) and maps items through `super::map` to the sink.
use std::sync::atomic::Ordering;
use std::time::Duration;

use ibapi::subscriptions::SubscriptionItem;

use super::map::{QuoteAccumulator, now_ms};
use super::{FeedCtx, VENUE};
use crate::contract::parse_simplified;

/// Bounded poll so the pump checks `ctx.stop` even when the market is quiet.
const POLL: Duration = Duration::from_millis(200);

pub(crate) fn quotes_pump(symbol: String, _interval: String, ctx: FeedCtx) {
    let Some(contract) = parse_simplified(&symbol) else {
        ctx.sink.stream_status(VENUE, &symbol, "quotes", stale_now());
        return;
    };
    let ib = super::pump_contract(&contract);
    let sub = match ctx.client.market_data(&ib).streaming().subscribe() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%symbol, error = %e, "ibkr market_data subscribe failed");
            return;
        }
    };
    let mut acc = QuoteAccumulator::default();
    while !ctx.stop.load(Ordering::Relaxed) {
        match sub.next_timeout(POLL) {
            Some(Ok(SubscriptionItem::Data(tick))) => {
                if let Some(mut q) = acc.apply(&tick) {
                    q.symbol.clear();
                    ctx.sink.quote(VENUE, &symbol, q);
                }
            }
            Some(Ok(SubscriptionItem::Notice(_))) => {} // advisory; stream stays open
            Some(Err(e)) => {
                tracing::warn!(%symbol, error = %e, "ibkr quotes stream ended");
                break;
            }
            None => {
                // Bounded sleep: on a clean stream-end, next_timeout returns None instantly,
                // so a bare `None => {}` would busy-spin a core. Cap it at POLL.
                std::thread::sleep(POLL);
            }
        }
    }
}

fn stale_now() -> vike_data::StreamStatus {
    vike_data::StreamStatus::Stale { newest_data_ts_ms: 0, now_ms: now_ms() }
}

/// Apply one IB depth message to a position-indexed side ladder. `op`: 0 = insert a new row at
/// `pos` (existing rows shift down), 1 = update the row at `pos`, 2 = delete the row at `pos`
/// (rows below shift up). Out-of-range positions are clamped/ignored so a malformed row can never
/// panic or unbound the ladder (kept ≤ `cap`).
fn apply_depth_row(
    ladder: &mut Vec<vike_model::Level>,
    op: i32,
    pos: i32,
    price: f64,
    size: f64,
    cap: usize,
) {
    let Ok(idx) = usize::try_from(pos) else { return };
    match op {
        0 => {
            // insert: place at idx, shifting the rest down; drop anything past cap.
            if idx <= ladder.len() {
                ladder.insert(idx, (price, size));
                ladder.truncate(cap);
            }
        }
        1 => {
            // update: replace at idx; treat an update one-past-end as an append (IB seeds rows this
            // way in practice).
            if idx < ladder.len() {
                ladder[idx] = (price, size);
            } else if idx == ladder.len() && ladder.len() < cap {
                ladder.push((price, size));
            }
        }
        // delete: remove the row at idx, shifting those below up.
        2 if idx < ladder.len() => {
            ladder.remove(idx);
        }
        _ => {} // delete-out-of-range or unknown op → ignore
    }
}

pub(crate) fn trades_pump(symbol: String, _interval: String, ctx: FeedCtx) {
    let Some(contract) = parse_simplified(&symbol) else {
        return;
    };
    let ib = super::pump_contract(&contract);
    // number_of_ticks = 0 → live streaming (no historical backfill of ticks).
    let sub = match ctx.client.tick_by_tick(&ib, 0).all_last() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%symbol, error = %e, "ibkr tick_by_tick(all_last) failed");
            return;
        }
    };
    while !ctx.stop.load(Ordering::Relaxed) {
        match sub.next_timeout(POLL) {
            Some(Ok(SubscriptionItem::Data(tr))) => {
                let mut t = super::map::trade_from(&tr);
                t.symbol.clear();
                ctx.sink.trade(VENUE, &symbol, t);
            }
            Some(Ok(SubscriptionItem::Notice(_))) => {} // advisory; stream stays open
            Some(Err(e)) => {
                tracing::warn!(%symbol, error = %e, "ibkr trades stream ended");
                break;
            }
            None => {
                // Bounded sleep: on a clean stream-end, next_timeout returns None instantly,
                // so a bare `None => {}` would busy-spin a core. Cap it at POLL.
                std::thread::sleep(POLL);
            }
        }
    }
}
/// Seeds one interval's worth of history via `historical_data` (best-effort — a failed/empty
/// fetch does not stop the live stream), then streams IBKR's fixed 5-second realtime bars,
/// aggregating them up to `interval` via `BarAggregator`.
pub(crate) fn bars_pump(symbol: String, interval: String, ctx: FeedCtx) {
    let Some(contract) = parse_simplified(&symbol) else {
        return;
    };
    let ib = super::pump_contract(&contract);
    let interval_ms = super::interval_ms(&interval).unwrap_or(60_000);

    // Seed: one interval's worth of history via historical_data (best-effort; delayed-ok).
    if let Ok(hist) = ctx
        .client
        .historical_data(&ib, super::hist_bar_size(&interval))
        .duration(ibapi::market_data::historical::Duration::days(1))
        .fetch()
    {
        let bars: Vec<vike_model::Bar> =
            hist.bars.iter().map(super::map::bar_from_historical).collect();
        if !bars.is_empty() {
            ctx.sink.seed_bars(VENUE, &symbol, &interval, bars);
        }
    }

    let sub = match ctx.client.realtime_bars(&ib).subscribe() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%symbol, error = %e, "ibkr realtime_bars failed");
            return;
        }
    };
    let mut agg = super::agg::BarAggregator::new(interval_ms);
    while !ctx.stop.load(Ordering::Relaxed) {
        match sub.next_timeout(POLL) {
            Some(Ok(SubscriptionItem::Data(b5))) => {
                let vb5 = super::map::bar_from_realtime(&b5);
                // `bar_close_tick`, NOT `mark_tick`: a 5s-bar close is not a venue mark
                // (mark-slot semantics); IBKR streams no mark price on this pump, so the
                // bar-close slot is this venue's only conflated price lane.
                ctx.sink.bar_close_tick(VENUE, &symbol, vb5.close, vb5.ts);
                let (forming, closed) = agg.fold(&vb5);
                if let Some(c) = closed {
                    ctx.sink.close_bar(VENUE, &symbol, &interval, c);
                }
                ctx.sink.forming_bar(VENUE, &symbol, &interval, forming);
            }
            Some(Ok(SubscriptionItem::Notice(_))) => {} // advisory; stream stays open
            Some(Err(e)) => {
                tracing::warn!(%symbol, error = %e, "ibkr bars stream ended");
                break;
            }
            None => {
                // Bounded sleep: on a clean stream-end, next_timeout returns None instantly,
                // so a bare `None => {}` would busy-spin a core. Cap it at POLL.
                std::thread::sleep(POLL);
            }
        }
    }
}
/// Streams `reqMktDepth` (L1 or L2 depending on entitlement) and maintains per-side, POSITION-
/// indexed row ladders — IB depth messages identify a *row* (`position`), not a price, and
/// `operation` 0/1/2 = insert/update/delete AT that row. Each change rebuilds a bounded (10-row)
/// `L2Book`, emitted both as `l2_snapshot` (DOM, plain levels) and `book` (folded lane). IB
/// delivers each side best-first (position 0 = top of book), so bids come descending and asks
/// ascending — exactly the order `L2Book` expects, no re-sort.
pub(crate) fn depth_pump(symbol: String, _interval: String, ctx: FeedCtx) {
    use ibapi::market_data::realtime::MarketDepths;
    let Some(contract) = parse_simplified(&symbol) else {
        return;
    };
    let ib = super::pump_contract(&contract);
    // `IbkrContract` (Phase 1) carries no `min_tick` — contractDetails resolution isn't wired
    // into the market-data path yet, so default to the common US-equity tick (L2Book metadata only;
    // the ladder itself is position-indexed and tick-independent).
    let tick = 0.01;
    const ROWS: usize = 10;
    let sub = match ctx.client.market_depth(&ib, ROWS as i32).subscribe() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%symbol, error = %e, "ibkr market_depth failed (unentitled?); no book");
            return;
        }
    };
    // Per-side row ladders indexed by IB `position` (row 0 = top of book). Apply insert/update/
    // delete at the given row; a stale price at a row is evicted because the row — not the price —
    // is the identity.
    let mut bids: Vec<vike_model::Level> = Vec::with_capacity(ROWS);
    let mut asks: Vec<vike_model::Level> = Vec::with_capacity(ROWS);
    let mut seq = 0u64;
    while !ctx.stop.load(Ordering::Relaxed) {
        match sub.next_timeout(POLL) {
            Some(Ok(SubscriptionItem::Data(d))) => {
                let (op, side, pos, price, size) = match d {
                    MarketDepths::MarketDepth(m) => {
                        (m.operation, m.side, m.position, m.price, m.size)
                    }
                    MarketDepths::MarketDepthL2(m) => {
                        (m.operation, m.side, m.position, m.price, m.size)
                    }
                };
                let ladder = if side == 1 { &mut bids } else { &mut asks };
                apply_depth_row(ladder, op, pos, price, size, ROWS);
                seq += 1;
                let bid_lv: Vec<vike_model::Level> = bids.iter().take(ROWS).copied().collect();
                let ask_lv: Vec<vike_model::Level> = asks.iter().take(ROWS).copied().collect();
                let mut book = vike_model::L2Book::new(tick);
                book.apply_snapshot(seq, &bid_lv, &ask_lv);
                ctx.sink.l2_snapshot(VENUE, &symbol, tick, bid_lv, ask_lv, now_ms());
                ctx.sink.book(VENUE, &symbol, std::sync::Arc::new(book));
            }
            Some(Ok(SubscriptionItem::Notice(_))) => {} // advisory; stream stays open
            Some(Err(e)) => {
                tracing::warn!(%symbol, error = %e, "ibkr depth stream ended");
                break;
            }
            None => {
                // Bounded sleep: on a clean stream-end, next_timeout returns None instantly,
                // so a bare `None => {}` would busy-spin a core. Cap it at POLL.
                std::thread::sleep(POLL);
            }
        }
    }
}

#[cfg(test)]
mod depth_tests {
    use super::apply_depth_row;

    #[test]
    fn update_replaces_row_no_ghost_level() {
        // Row 0 seeded at 100.05, then IB moves that row to 100.06 via an UPDATE (op=1) at the
        // SAME position. The old price must vanish (row identity), not linger as a phantom level.
        let mut bids = Vec::new();
        apply_depth_row(&mut bids, 0, 0, 100.05, 10.0, 10); // insert row 0
        apply_depth_row(&mut bids, 1, 0, 100.06, 12.0, 10); // update row 0's price
        assert_eq!(bids, vec![(100.06, 12.0)], "stale 100.05 must not survive an update");
    }

    #[test]
    fn insert_shifts_down_delete_shifts_up_bounded() {
        let mut asks = Vec::new();
        apply_depth_row(&mut asks, 0, 0, 100.10, 1.0, 3); // [100.10]
        apply_depth_row(&mut asks, 0, 0, 100.09, 2.0, 3); // insert at top → [100.09, 100.10]
        apply_depth_row(&mut asks, 0, 1, 100.095, 3.0, 3); // insert middle → [100.09, 100.095, 100.10]
        assert_eq!(asks, vec![(100.09, 2.0), (100.095, 3.0), (100.10, 1.0)]);
        // A 4th insert would exceed cap=3 → truncated to the top 3 rows.
        apply_depth_row(&mut asks, 0, 0, 100.08, 4.0, 3);
        assert_eq!(asks.len(), 3);
        assert_eq!(asks[0], (100.08, 4.0));
        // Delete the top row → rows below shift up.
        apply_depth_row(&mut asks, 2, 0, 0.0, 0.0, 3);
        assert_eq!(asks, vec![(100.09, 2.0), (100.095, 3.0)]);
    }

    #[test]
    fn out_of_range_position_is_ignored() {
        let mut bids = vec![(100.0, 1.0)];
        apply_depth_row(&mut bids, 1, 9, 99.0, 5.0, 10); // update far past end → no-op
        apply_depth_row(&mut bids, 2, 9, 0.0, 0.0, 10); // delete far past end → no-op
        apply_depth_row(&mut bids, 1, -1, 0.0, 0.0, 10); // negative → no-op
        assert_eq!(bids, vec![(100.0, 1.0)]);
    }
}
