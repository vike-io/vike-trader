//! The REST first price: a newly subscribed pair's FIRST tick, read from OANDA's pricing ENDPOINT
//! (`GET /v3/accounts/{id}/pricing?instruments=…`) instead of waited for on the pricing STREAM.
//!
//! **Why.** The stream host sits behind Cloudflare and answers an open at random: measured on the CI box,
//! about two opens in three hang with no response head until the market lane's `response_timeout` of
//! `vike_venues::venue_timings::OANDA` expires, while the REST host answered every probe inside a
//! second. The control door holds an opening order on a NEW pair for its first price only
//! `crates/vike-tradehub/src/server/symbol_admission.rs`'s `PRICE_WAIT`, so the first order on a new
//! pair was refused with "no price yet" while the stream was still hanging.
//!
//! **What.** Each quote thread asks this lane for its pair as it starts ([`Seed::ask`]), BESIDE the
//! stream open and never in front of it. One thread ([`seed_main`]) collects the asks of a burst for
//! [`COLLECT`], makes ONE request for all of them (up to [`MAX_PER_CALL`] instruments), and hands each
//! pair that came back tradeable, two-sided and fresh to the same sink the stream feeds
//! ([`seed_tick`]). The tick keeps the venue's own `time` as its `ts`, so nothing here makes a price
//! look newer than the venue said. The stream keeps running and its ticks take over; a seed the stream
//! has beaten is never sent ([`Want::done`]).
//!
//! **No seed is better than a made-up one.** A pair the venue marks non-tradeable (market closed), a
//! reply that is an error, late, one-sided, crossed or older than [`MAX_SEED_AGE_MS`] feeds NOTHING:
//! the order is refused in words, as it was before this lane existed.
//!
//! **Bounded.** One read is at most [`SEED_BOUND`]; a pair is asked at most once per
//! [`PER_SYMBOL_FLOOR`]; the whole lane spends from a bucket of [`BURST`] requests refilled one per
//! [`REFILL`]; a failed read is not retried (the floor holds it off), so a venue that is down costs
//! one request per burst, not a loop.
//!
//! ⚠ Not a price feed. It prices a pair ONCE, as it is subscribed; a stream that stays down past the
//! door's price age bound (`crates/vike-tradehub/src/feeds/price_tap.rs`'s `MAX_PRICE_AGE`) leaves the
//! pair unpriced again, as before.
//!
//! The `QuoteTick` carries no source field, so a REST tick is told apart in the log
//! (`source = "rest"`), not on the wire.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::poller::STOP_POLL_SLICE;
use vike_marketdata::QuoteTick;
use vike_model::now_ms;

use super::{FeedCtx, VENUE};
use crate::config::OandaConfig;
use crate::market_data::{PricingFrame, decode_pricing_frame};

/// The whole REST read, dial to last body byte (a global timeout): the answer is wanted inside the
/// door's price wait or not at all.
pub(super) const SEED_BOUND: Duration = Duration::from_secs(3);

/// How long the first ask of a burst waits for the others before the request goes out, so a burst of
/// new pairs (a boot re-admission, an operator's list) costs one request.
pub(super) const COLLECT: Duration = Duration::from_millis(150);

/// A pair is asked about at most once per this long: a re-subscribe a moment after a read is covered
/// by that read (or by the stream), not by another one.
pub(super) const PER_SYMBOL_FLOOR: Duration = Duration::from_secs(5);

/// The request bucket: this many requests at once...
pub(super) const BURST: f64 = 3.0;

/// ...refilled one per this long.
pub(super) const REFILL: Duration = Duration::from_secs(2);

/// The most instruments one request names; the rest wait for the next request.
pub(super) const MAX_PER_CALL: usize = 20;

/// An ask nobody is served for within this long is dropped (the bucket stayed empty): the door that
/// asked has long refused, and the pair's stream prices its retry.
pub(super) const WANT_TTL: Duration = Duration::from_secs(10);

/// How old the venue's own `time` may be and the price still be a seed. A major ticks every second
/// in session, so past this the pair is quiet and a price from it is not a mark; the tap that
/// receives the tick ages it from ARRIVAL, which is why the bound is applied here, on the venue's clock.
pub(super) const MAX_SEED_AGE_MS: i64 = 20_000;

/// How far AHEAD of this machine's clock the venue's `time` may read before the price is refused.
const MAX_SEED_AHEAD_MS: i64 = 5_000;

/// The loop's nap while asks wait (for [`COLLECT`] to pass or for a token).
const POLL: Duration = Duration::from_millis(25);

/// One quote thread's ask: the venue instrument to read, the subscription's own label for the tick,
/// and whether the stream has already made the ask pointless.
pub(super) struct Want {
    pub(super) instrument: String,
    pub(super) series: String,
    /// Raised by the stream's first tick, or when the quote thread ends: a seed never lands after
    /// either. shortcut: a plain flag, so a seed that wins the race by microseconds is overwritten by
    /// the stream's next tick; a lock around both emits would close it, nothing needs it.
    pub(super) done: Arc<AtomicBool>,
}

/// A quote thread's hold on the seed lane: its ask line and its [`Want::done`] flag.
pub(super) struct Seed {
    line: Sender<Want>,
    done: Arc<AtomicBool>,
}

impl Seed {
    pub(super) fn new(line: Sender<Want>) -> Self {
        Self { line, done: Arc::default() }
    }

    /// Ask for `instrument`'s first price (labelled `series`). Never blocks; a lane that is gone
    /// leaves the stream to price the pair.
    pub(super) fn ask(&self, instrument: &str, series: &str) {
        let _ = self.line.send(Want {
            instrument: instrument.to_string(),
            series: series.to_string(),
            done: Arc::clone(&self.done),
        });
    }

    /// The stream delivered a tick: call BEFORE handing it to the sink.
    pub(super) fn stream_ticked(&self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

impl Drop for Seed {
    /// The quote thread ended (unsubscribed or shut down): an ask still queued must not tick a pair
    /// nobody holds.
    fn drop(&mut self) {
        self.stream_ticked();
    }
}

/// An ask in the lane, and when it arrived.
pub(super) struct Pending {
    pub(super) since: Instant,
    pub(super) want: Want,
}

/// The lane's pacing: the request bucket and the per-instrument floor.
pub(super) struct Pace {
    tokens: f64,
    at: Instant,
    asked: HashMap<String, Instant>,
}

impl Pace {
    pub(super) fn new(now: Instant) -> Self {
        Self { tokens: BURST, at: now, asked: HashMap::new() }
    }

    pub(super) fn asked_lately(&self, instrument: &str, now: Instant) -> bool {
        self.asked
            .get(instrument)
            .is_some_and(|at| now.saturating_duration_since(*at) < PER_SYMBOL_FLOOR)
    }

    /// Spend one token on a request for `instruments` and stamp them asked. `false`, with nothing
    /// spent or stamped, when the bucket is empty.
    pub(super) fn take(&mut self, instruments: &[String], now: Instant) -> bool {
        let earned = now.saturating_duration_since(self.at).as_secs_f64() / REFILL.as_secs_f64();
        self.tokens = (self.tokens + earned).min(BURST);
        self.at = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        self.asked.retain(|_, at| now.saturating_duration_since(*at) < PER_SYMBOL_FLOOR);
        self.asked.extend(instruments.iter().map(|i| (i.clone(), now)));
        true
    }
}

/// The text of a request's query is the instrument names and nothing else: only that alphabet reaches it.
fn is_instrument_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// PURE: the price entries of a pricing reply, by instrument.
fn by_instrument(body: &Value) -> HashMap<&str, &Value> {
    body.get("prices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some((entry.get("instrument")?.as_str()?, entry)))
        .collect()
}

/// PURE: one entry of the REST pricing reply to the tick it seeds, or the reason it seeds nothing.
///
/// The entry is the stream's `PRICE` frame (`crate::market_data::decode_pricing_frame` reads it; the
/// REST copy need not carry the `type`). It must be TRADEABLE (the `tradeable` flag, else the
/// `status` word), two-sided and not crossed, and its `time` within [`MAX_SEED_AGE_MS`] of `now_ms`.
/// The tick keeps that venue `time` as `ts`; `local_ts` is `now_ms` and the symbol is the
/// subscription's `series`, exactly as the stream's fold stamps them.
pub(super) fn seed_tick(entry: &Value, series: &str, now_ms: i64) -> Result<QuoteTick, String> {
    let mut frame = entry.as_object().cloned().ok_or("the entry is not an object")?;
    let tradeable = frame
        .get("tradeable")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| frame.get("status").and_then(Value::as_str) == Some("tradeable"));
    if !tradeable {
        return Err("the venue marks it non-tradeable (market closed or halted)".to_string());
    }
    frame.insert("type".to_string(), Value::from("PRICE"));
    let PricingFrame::Quote(mut quote) = decode_pricing_frame(&Value::Object(frame)) else {
        return Err("the price is one-sided or unreadable".to_string());
    };
    if !(quote.bid.is_finite()
        && quote.ask.is_finite()
        && quote.bid > 0.0
        && quote.ask >= quote.bid)
    {
        return Err(format!(
            "the price is crossed or not positive ({} / {})",
            quote.bid, quote.ask
        ));
    }
    let age = now_ms - quote.ts;
    if quote.ts <= 0 || !(-MAX_SEED_AHEAD_MS..=MAX_SEED_AGE_MS).contains(&age) {
        return Err(format!(
            "the venue's time on it is {age} ms from now (it must be within {MAX_SEED_AGE_MS} ms behind)"
        ));
    }
    quote.local_ts = now_ms;
    quote.symbol = series.to_string();
    Ok(quote)
}

/// One pass of the lane over its `pending` asks at `now`: drop what is spent, make ONE request for
/// what is left (when the bucket has a token), and `emit` the tick of each pair that seeds.
///
/// Dropped without a request: an ask older than [`WANT_TTL`], one the stream has already beaten
/// ([`Want::done`]), one for a pair read inside [`PER_SYMBOL_FLOOR`], and one whose name is no
/// instrument name. Asks beyond [`MAX_PER_CALL`] distinct instruments, and every ask while the bucket
/// is empty, stay for a later pass. `fetch` makes the request for the (distinct) instruments.
pub(super) fn serve(
    pending: &mut Vec<Pending>,
    pace: &mut Pace,
    now: Instant,
    now_ms: i64,
    fetch: impl FnOnce(&[String]) -> Result<Value, String>,
    mut emit: impl FnMut(&Want, QuoteTick),
) {
    pending.retain(|p| {
        now.saturating_duration_since(p.since) < WANT_TTL
            && !p.want.done.load(Ordering::Relaxed)
            && !pace.asked_lately(&p.want.instrument, now)
            && is_instrument_name(&p.want.instrument)
    });
    let mut instruments: Vec<String> = Vec::new();
    for p in pending.iter() {
        if instruments.len() < MAX_PER_CALL && !instruments.contains(&p.want.instrument) {
            instruments.push(p.want.instrument.clone());
        }
    }
    if instruments.is_empty() || !pace.take(&instruments, now) {
        return;
    }
    let (served, rest): (Vec<_>, Vec<_>) =
        std::mem::take(pending).into_iter().partition(|p| instruments.contains(&p.want.instrument));
    *pending = rest;
    let body = match fetch(&instruments) {
        Ok(body) => body,
        Err(why) => {
            tracing::warn!(
                venue = VENUE,
                instruments = %instruments.join(","),
                error = %why,
                "REST first-price read failed; the pairs are priced by their streams"
            );
            return;
        }
    };
    let prices = by_instrument(&body);
    for Pending { want, .. } in served {
        let tick = prices
            .get(want.instrument.as_str())
            .ok_or_else(|| "the reply carries no price for it".to_string())
            .and_then(|entry| seed_tick(entry, &want.series, now_ms));
        match tick {
            // The stream may have priced the pair while the read was in flight: its tick stands.
            Ok(quote) if !want.done.load(Ordering::Relaxed) => emit(&want, quote),
            Ok(_) => {}
            Err(why) => tracing::info!(
                venue = VENUE,
                symbol = %want.series,
                reason = %why,
                "no REST first price; the stream prices the pair"
            ),
        }
    }
}

/// The REST agent: ONE global bound ([`SEED_BOUND`]) on dial, request and reply, no redirect. It
/// answers a non-2xx as a response (the caller words the status).
pub(super) fn seed_agent() -> ureq::Agent {
    vike_bridge_core::http::blocking_agent_with_timeout(SEED_BOUND)
}

/// `GET /v3/accounts/{id}/pricing?instruments=A,B` on the account's REST host: the parsed body.
/// The error is words an operator reads (no token, no URL).
fn fetch_prices(
    agent: &ureq::Agent,
    config: &OandaConfig,
    instruments: &[String],
) -> Result<Value, String> {
    let url = format!(
        "{}/v3/accounts/{}/pricing?instruments={}",
        config.rest_base,
        config.account_id,
        instruments.join(",")
    );
    let bearer = format!("Bearer {}", config.api_token);
    let mut resp = agent
        .get(&url)
        .header("Authorization", &bearer)
        .header("Accept-Datetime-Format", "UNIX")
        .call()
        .map_err(|e| match e {
            ureq::Error::Timeout(_) => format!("no reply within {} s", SEED_BOUND.as_secs()),
            other => format!("transport error: {other}"),
        })?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    let text = resp.body_mut().read_to_string().map_err(|e| format!("reading the reply: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("unreadable reply: {e}"))
}

/// The seed lane's thread body: collect asks, serve them, until the stop flag (or until every
/// asker, the [`super::Feeds`] included, is gone).
pub(super) fn seed_main(line: Receiver<Want>, ctx: FeedCtx) {
    let agent = seed_agent();
    let mut pace = Pace::new(Instant::now());
    let mut pending: Vec<Pending> = Vec::new();
    while !ctx.stop.load(Ordering::Relaxed) {
        let nap = if pending.is_empty() { STOP_POLL_SLICE } else { POLL };
        match line.recv_timeout(nap) {
            Ok(want) => pending.push(Pending { since: Instant::now(), want }),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if pending.first().is_some_and(|p| p.since.elapsed() >= COLLECT) {
            serve(
                &mut pending,
                &mut pace,
                Instant::now(),
                now_ms(),
                |instruments| fetch_prices(&agent, &ctx.config, instruments),
                |want, quote| {
                    tracing::info!(
                        venue = VENUE,
                        symbol = %want.series,
                        source = "rest",
                        bid = quote.bid,
                        ask = quote.ask,
                        age_ms = now_ms() - quote.ts,
                        "first price from the REST pricing endpoint (the stream has delivered none yet)"
                    );
                    ctx.sink.quote(VENUE, &want.series, quote);
                    (ctx.wake)();
                },
            );
        }
    }
}
