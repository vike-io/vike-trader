//! Per-verb pump-thread bodies for `IbkrFeeds`. Each drains an ibapi `Subscription<T>` via
//! `next_timeout` (polling `ctx.stop`) and maps items through `super::map` to the sink.
//!
//! ⚠ `depth_pump` (the `reqMktDepth` row ladder that fed both the book and the depth lane) is
//! DELETED by docs/decisions/0140: the accounts hold no depth entitlement, and `subscribe_book` /
//! `subscribe_depth` now answer `Unsupported` off the caps row. Decision 0045 and the
//! order-level-replay spec cite it as it stood when they were written.
use std::sync::atomic::Ordering;
use std::time::Duration;

use ibapi::subscriptions::SubscriptionItem;
use vike_data::StreamLabel;

use super::map::{QuoteAccumulator, forward_quote_tick};
use super::{FeedCtx, VENUE};
use crate::contract::parse_simplified;

/// Bounded poll so the pump checks `ctx.stop` even when the market is quiet.
const POLL: Duration = Duration::from_millis(200);

pub(crate) fn quotes_pump(symbol: String, _interval: String, ctx: FeedCtx) {
    let contract = match parse_simplified(&symbol) {
        Ok(c) => c,
        Err(why) => {
            tracing::warn!(%symbol, reason = %why, "ibkr quotes: unusable symbol");
            ctx.sink.stream_status(VENUE, &symbol, StreamLabel::Quotes.as_str(), stale_now());
            return;
        }
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
                // The label rides beside the quote, read off this tick (decision 0140).
                forward_quote_tick(&mut acc, &tick, VENUE, &symbol, ctx.sink.as_ref());
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

fn stale_now() -> vike_model::StreamStatus {
    vike_model::StreamStatus::Stale { newest_data_ts_ms: 0, now_ms: vike_model::now_ms() }
}

pub(crate) fn trades_pump(symbol: String, _interval: String, ctx: FeedCtx) {
    let contract = match parse_simplified(&symbol) {
        Ok(c) => c,
        Err(why) => {
            tracing::warn!(%symbol, reason = %why, "ibkr trades: unusable symbol");
            return;
        }
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
/// aggregating them up to `interval` via `BarAggregator`. `interval_ms` is `interval` already
/// validated by `IbkrFeeds::subscribe_bars` (`super::live_bar_interval_ms`). The seed is
/// skipped, with a warning, when IB serves no historical bar of exactly that length
/// (`super::hist_bar_size`): seeding a nearby size would store bars of the wrong length under
/// this interval.
pub(crate) fn bars_pump(symbol: String, interval: String, interval_ms: i64, ctx: FeedCtx) {
    let contract = match parse_simplified(&symbol) {
        Ok(c) => c,
        Err(why) => {
            tracing::warn!(%symbol, reason = %why, "ibkr bars: unusable symbol");
            return;
        }
    };
    let ib = super::pump_contract(&contract);
    // Seed: one interval's worth of history via historical_data (best-effort; delayed-ok).
    let bar_size = super::hist_bar_size(interval_ms);
    if bar_size.is_none() {
        tracing::warn!(
            %symbol,
            %interval,
            "ibkr bars: IB serves no historical bar of this length — no seed, live bars only"
        );
    }
    if let Some(Ok(hist)) = bar_size.map(|size| {
        ctx.client
            .historical_data(&ib, size)
            .duration(ibapi::market_data::historical::Duration::days(1))
            .fetch()
    }) {
        let bars: Vec<vike_marketdata::Bar> =
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
    let mut agg = crate::agg::BarAggregator::new(interval_ms);
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
