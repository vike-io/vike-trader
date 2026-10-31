//! Per-verb pump-thread bodies for `IbkrFeeds`. Each drains an ibapi `Subscription<T>` via
//! `next_timeout` (polling `ctx.stop`) and maps items through `super::map` to the sink.
//!
//! ⚠ `depth_pump` (the `reqMktDepth` row ladder that fed both the book and the depth lane) is
//! DELETED by docs/decisions/0140: the accounts hold no depth entitlement, and `subscribe_book` /
//! `subscribe_depth` now answer `Unsupported` off the caps row. Decision 0045 and the
//! order-level-replay spec cite it as it stood when they were written.
use std::sync::atomic::Ordering;
use std::time::Duration;

use ibapi::client::blocking::Client;
use ibapi::market_data::realtime::TickTypes;
use ibapi::subscriptions::{Subscription, SubscriptionItem};
use vike_data::StreamLabel;

use super::quote_retry::{self, Next, QuoteDrain, QuoteWire};
use super::{FeedCtx, VENUE, apply_data_type};
use crate::config::MktDataType;
use crate::contract::parse_simplified;

/// Bounded poll so the pump checks `ctx.stop` even when the market is quiet.
const POLL: Duration = Duration::from_millis(200);

/// The shared `ibapi` client as the quote loop's wire (`super::quote_retry`).
struct ClientWire<'a> {
    client: &'a Client,
    contract: ibapi::contracts::Contract,
    mktdata: MktDataType,
}

impl QuoteWire for ClientWire<'_> {
    fn apply_data_type(&self) {
        apply_data_type(self.client, self.mktdata, "quotes");
    }

    fn open(&self) -> Result<Box<dyn QuoteDrain>, ibapi::Error> {
        let sub = self.client.market_data(&self.contract).streaming().subscribe()?;
        Ok(Box::new(ClientDrain(sub)))
    }
}

/// One `ibapi` quote subscription.
struct ClientDrain(Subscription<TickTypes>);

impl QuoteDrain for ClientDrain {
    fn next(&mut self, wait: Duration) -> Next {
        match self.0.next_timeout(wait) {
            Some(Ok(SubscriptionItem::Data(tick))) => Next::Tick(tick),
            Some(Ok(SubscriptionItem::Notice(notice))) => Next::Notice(notice),
            Some(Err(e)) => Next::Ended(e),
            None => Next::Quiet,
        }
    }
}

pub(crate) fn quotes_pump(symbol: String, _interval: String, ctx: FeedCtx) {
    let contract = match parse_simplified(&symbol) {
        Ok(c) => c,
        Err(why) => {
            tracing::warn!(%symbol, reason = %why, "ibkr quotes: unusable symbol");
            ctx.sink.stream_status(VENUE, &symbol, StreamLabel::Quotes.as_str(), stale_now());
            return;
        }
    };
    let wire = ClientWire {
        client: &ctx.client,
        contract: super::pump_contract(&contract),
        mktdata: ctx.mktdata,
    };
    quote_retry::run_quotes(
        &wire,
        &symbol,
        ctx.sink.as_ref(),
        &ctx.stop,
        &ctx.lost,
        &quote_retry::PRODUCTION,
    );
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
                ctx.ended("trades", &symbol, &e);
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
                ctx.ended("bars", &symbol, &e);
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
