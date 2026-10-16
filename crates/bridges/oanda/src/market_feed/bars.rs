//! The bar lane: the REST poll-on-cadence thread body, its pure pieces and its cadence knobs.

use std::sync::atomic::Ordering;
use std::time::Duration;

use vike_bridge_core::poller::{STOP_POLL_SLICE, sleep_stop_aware};
use vike_bridge_core::stream_health::StreamHealth;
use vike_marketdata::Bar;
use vike_model::now_ms;

use super::{FeedCtx, RECONNECT_BACKOFF, VENUE};
use crate::market_data::parse_forming_candle;
use crate::rest::OandaRest;

/// REST warm-up seed depth for `subscribe_bars` (the binance family seeds 1000; OANDA candle
/// fetches are per-request priced, so a lighter seed keeps the poll lane cheap).
const SEED_BARS: u32 = 300;
/// Ceiling on one incremental candle re-fetch. `count=N` returns the NEWEST N candles, so an
/// outage longer than this many bars leaves a hole the poll lane cannot repair — the
/// `GapStart`/`Live` disclosure brackets it, and repairing further back is a backfill job, not a
/// live-feed job (the same honesty as any REST-seeded feed).
pub(super) const MAX_TAIL_COUNT: u32 = 500;
/// Poll-cadence clamp floor: bounds one bar subscription to ≤0.2 req/s against the venue's
/// documented 120 req/s per-token REST budget (shared with the exec side — negligible).
pub(super) const MIN_POLL: Duration = Duration::from_secs(5);
/// Poll-cadence clamp ceiling: even a daily/weekly series re-polls once a minute, so a close is
/// never disclosed more than a minute late.
pub(super) const MAX_POLL: Duration = Duration::from_secs(60);
/// Pacing fallback for the intervals `vike_marketdata::interval::interval_ms` cannot express (`1w`/`1mo`):
/// pace them as a day. Over-fetch-safe by construction — assuming a SHORTER interval only ever
/// raises [`tail_count`], never lowers it.
pub(super) const FALLBACK_PACE_MS: i64 = 86_400_000;

// --- The bar lane (REST poll) -----------------------------------------------------------------

/// Poll cadence for one bar subscription: `interval / 12`, clamped to
/// [[`MIN_POLL`], [`MAX_POLL`]] — a 1m series re-polls every 5 s (forming updates + a close
/// disclosed within seconds), a 1h+ series never faster than once a minute.
pub(super) fn bar_poll_cadence(interval_ms: i64) -> Duration {
    let ms = (interval_ms / 12).clamp(MIN_POLL.as_millis() as i64, MAX_POLL.as_millis() as i64);
    Duration::from_millis(ms as u64)
}

/// How many newest candles one incremental poll requests: enough to cover everything since the
/// last seen close (plus slack for the boundary candle and the forming tail), capped at
/// [`MAX_TAIL_COUNT`].
pub(super) fn tail_count(elapsed_ms: i64, interval_ms: i64) -> u32 {
    let needed = elapsed_ms.max(0) / interval_ms.max(1) + 2;
    needed.clamp(2, i64::from(MAX_TAIL_COUNT)) as u32
}

/// PURE fold of one candles response against the newest close already emitted: `(fresh closed
/// bars ts-ascending, the forming candle if any, the advanced watermark)`.
pub(super) fn fold_candles_response(
    v: &serde_json::Value,
    last_closed_ts: i64,
) -> (Vec<Bar>, Option<Bar>, i64) {
    let fresh: Vec<Bar> =
        crate::data::parse_candles(v).into_iter().filter(|b| b.ts > last_closed_ts).collect();
    let new_last = fresh.last().map_or(last_closed_ts, |b| b.ts);
    (fresh, parse_forming_candle(v), new_last)
}

/// `subscribe_bars` thread body: REST seed, then poll-on-cadence. Newly-complete candles ride
/// the lossless `close_bar` lane; the forming candle rides the conflating `forming_bar` lane;
/// the newest price snapshot rides `bar_close_tick` (candle-close semantics — OANDA has no mark
/// stream). A failed poll discloses `GapStart` (stream label = the interval); the next
/// successful poll emits the caught-up closes FIRST and `Live` after them, so the lossless lane
/// is already repaired when consumers see the recovery (the bar-lane `StreamStatus` contract).
pub(super) fn bars_main(
    instrument: String,
    series: String,
    interval: String,
    gran: &str,
    ctx: FeedCtx,
) {
    let rest = OandaRest::new(ctx.config.api_token.clone());
    let pace_ms = vike_marketdata::interval::interval_ms(&interval).unwrap_or(FALLBACK_PACE_MS);
    let cadence = bar_poll_cadence(pace_ms);
    // Freshness is not armed on this lane: the poll IS the clock, so "no fresh data" is either a
    // failed poll (→ the gap half below) or the venue honestly serving no new candle.
    let mut health = StreamHealth::new(i64::MAX);
    let mut last_closed_ts = 0_i64;
    let mut seeded = false;
    // ONE spelling for this lane's healthy text, written from TWO places (the seed and the
    // recovery edge below) — so the two cannot drift into a pair of strings that alternate on the
    // shared mutex.
    let live_status = format!("LIVE · OANDA bars {series}@{interval}");
    ctx.set_status(format!("connecting · OANDA bars {series}@{interval}"));

    while !ctx.stop.load(Ordering::Relaxed) {
        let count = if seeded { tail_count(now_ms() - last_closed_ts, pace_ms) } else { SEED_BARS };
        let query = format!("granularity={gran}&count={count}&price=M");
        let path = format!("/v3/instruments/{instrument}/candles");
        match rest.get(&ctx.config.rest_base, &path, &query) {
            Ok(v) => {
                let (fresh, forming, new_last) = fold_candles_response(&v, last_closed_ts);
                last_closed_ts = new_last;
                if !seeded {
                    seeded = true;
                    ctx.sink.seed_bars(VENUE, &series, &interval, fresh);
                    ctx.set_status(live_status.clone());
                } else {
                    for b in fresh {
                        ctx.sink.close_bar(VENUE, &series, &interval, b);
                    }
                }
                if let Some(f) = forming {
                    ctx.sink.bar_close_tick(VENUE, &series, f.close, f.ts);
                    ctx.sink.forming_bar(VENUE, &series, &interval, f);
                }
                if let Some(l) = health.recover() {
                    ctx.sink.stream_status(VENUE, &series, &interval, l);
                    // ⚠ **THE STATUS STRING'S RECOVERY EDGE — the second half of the same fact,
                    // and it was missing.** The healthy write above sits inside `if !seeded`, so
                    // once a poll had failed and written `"… poll failed (HTTP …); retrying"`,
                    // every LATER successful poll took the `else` branch and rewrote nothing: the
                    // string latched a fault permanently, exactly the shape that suppressed
                    // bybit's reconcile leg for 42 hours on the CI box — and strictly worse, because
                    // this lane has no session boundary to hang a disclosure on and shares one
                    // last-writer-wins mutex with `quotes_main`, so a latched bars string can mask
                    // a live quotes lane. `vike_bridge_core::market_pump`'s `SessionStatus::Live`
                    // cured that class for every venue on the shared driver; oanda is `OwnPump` in
                    // `vike_bridge_core::pump_spec` and the driver cannot reach it, so it is cured
                    // HERE, on the gap-close edge this lane already computes for its typed
                    // channel. Edge-triggered rather than per-poll for the same reason the typed
                    // disclosure is: nothing recovered if nothing was broken.
                    //
                    // `LiveFeeds::Oanda` returns no `recon_feed_statuses` row today, so nothing
                    // health-gated on this string — but that method's doc states what would EARN
                    // oanda one, and `crates/vike-ops/tests/venues/feed_success_disclosure_gate.rs`'s
                    // `OWN_PUMP_RESIDUALS` is where this venue's verdict is recorded, because that
                    // gate is structurally blind to every `OwnPump` venue.
                    ctx.set_status(live_status.clone());
                }
                (ctx.wake)();
            }
            Err(e) => {
                if let Some(g) = health.enter_gap(now_ms()) {
                    ctx.sink.stream_status(VENUE, &series, &interval, g);
                }
                tracing::warn!(
                    venue = VENUE,
                    symbol = %series,
                    interval = %interval,
                    status = e.status,
                    "candle poll failed; retrying"
                );
                ctx.set_status(format!(
                    "OANDA bars {series}@{interval}: poll failed (HTTP {}); retrying",
                    e.status
                ));
                if sleep_stop_aware(&ctx.stop, RECONNECT_BACKOFF, STOP_POLL_SLICE) {
                    return;
                }
                continue;
            }
        }
        if sleep_stop_aware(&ctx.stop, cadence, STOP_POLL_SLICE) {
            return;
        }
    }
}
