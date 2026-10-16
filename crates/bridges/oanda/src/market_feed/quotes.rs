//! The quote lane: the stop-aware chunked-line pump and the pricing-stream thread body it drives.

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_bridge_core::poller::{STOP_POLL_SLICE, sleep_stop_aware};
use vike_bridge_core::stream_health::StreamHealth;
use vike_data::StreamLabel;
use vike_marketdata::QuoteTick;
use vike_model::{StreamStatus, now_ms};
use vike_venues::venue_timings;

use super::{FRESHNESS_THRESHOLD_QUOTES, FeedCtx, RECONNECT_BACKOFF, VENUE};
use crate::market_data::{PricingFrame, decode_pricing_frame};

// --- The generic stop-aware chunked-line pump (pure lifecycle; I/O injected) ------------------

/// One lifecycle event out of [`pump_lines`] — the seam that keeps the reconnect loop testable
/// without a socket (the scripted-dial tests below drive it with in-memory readers).
pub(super) enum PumpEvent<'a> {
    /// A dial succeeded; lines follow.
    Connected,
    /// One non-empty, trimmed line.
    Line(&'a str),
    /// A dial failed (transport error or a non-2xx stream open, pre-described by the dialer).
    ConnectFailed(&'a str),
    /// The stream ended or errored mid-read (idle bound included) — a reconnect follows.
    Disconnected,
}

/// Stop-aware dial → read-lines → backoff → reconnect, until `stop`. The chunked-HTTP twin of
/// the shared WS market pump's lifecycle: `backoff` is walked in [`STOP_POLL_SLICE`] naps after
/// BOTH a failed dial and a disconnect (the same shape as [`crate::stream::stream_transactions`],
/// which waits the exec row's fixed pause after a dropped stream before it redials).
pub(super) fn pump_lines<R: BufRead>(
    stop: &AtomicBool,
    backoff: Duration,
    mut dial: impl FnMut() -> Result<R, String>,
    mut on_event: impl FnMut(PumpEvent<'_>),
) {
    while !stop.load(Ordering::Relaxed) {
        let reader = match dial() {
            Ok(r) => r,
            Err(e) => {
                on_event(PumpEvent::ConnectFailed(&e));
                if sleep_stop_aware(stop, backoff, STOP_POLL_SLICE) {
                    return;
                }
                continue;
            }
        };
        on_event(PumpEvent::Connected);
        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(line) = line else { break }; // idle bound / disconnect → reconnect
            let line = line.trim();
            if !line.is_empty() {
                on_event(PumpEvent::Line(line));
            }
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        on_event(PumpEvent::Disconnected);
        if sleep_stop_aware(stop, backoff, STOP_POLL_SLICE) {
            return;
        }
    }
}

// --- The quote lane (pricing stream) ----------------------------------------------------------

/// PURE fold of one pricing-stream line into (quote to emit, health disclosures to emit), in
/// sink order. Any decoded venue frame — PRICE or HEARTBEAT — closes an open transport gap
/// (`Live`); only a PRICE advances the freshness clock; every decoded frame re-judges freshness
/// (`Stale` once when the socket heartbeats on while no fresh PRICE arrives, `Live` on recovery —
/// [`StreamHealth::check_freshness`] owns the episode dedup and the gap-gating).
pub(super) fn fold_pricing_line(
    line: &str,
    series: &str,
    health: &mut StreamHealth,
    now_ms: i64,
) -> (Option<QuoteTick>, Vec<StreamStatus>) {
    let mut events = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return (None, events);
    };
    match decode_pricing_frame(&v) {
        PricingFrame::Quote(mut q) => {
            events.extend(health.recover());
            q.local_ts = now_ms; // machine receive time (dual-timestamp capture)
            q.symbol = series.to_string(); // relabel venue instrument → subscription series
            health.observe_data(q.ts);
            events.extend(health.check_freshness(now_ms));
            (Some(q), events)
        }
        PricingFrame::Heartbeat { .. } => {
            events.extend(health.recover());
            events.extend(health.check_freshness(now_ms));
            (None, events)
        }
        PricingFrame::Other => (None, events),
    }
}

/// Describe a failed stream open WITHOUT echoing the URL (it carries the account id). A non-2xx
/// is surfaced BY STATUS — the transactions stream's documented silent-retry trap (a bad/expired
/// token looping forever with no log, `crates/bridges/oanda/CLAUDE.md`) is answered here by
/// logging every failed dial, with the status code when there is one.
fn describe_dial_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(code) => {
            format!("HTTP {code} on stream open (401/403: bad or expired token / wrong account)")
        }
        _ => "transport error on stream open".to_string(),
    }
}

/// The streaming agent: idle-bounded reads (the market lane's `idle_threshold`), bounded dial (its
/// `dial_timeout`, so a black-holed route can never hold a feed thread for the OS's own SYN
/// ladder), the wait for the response head (its `response_timeout`, which is NOT the dial: both are
/// 10 s today and they are separate facts) and — unlike the transactions stream's hand-built agent
/// — non-2xx AS A RESPONSE, so a rejected open is classified by status instead of blurring into the
/// transport arm. The bounds are the market lane of `vike_venues::venue_timings::OANDA`.
/// ⚠ The head wait is `timeout_send_request`, not `timeout_recv_response`: ureq 3.4.0 caps the body
/// read at `<head arrival> + recv_response`, which would end a healthy stream after that many seconds
/// (`crate::stream`'s `transactions_agent` has the mechanism).
/// Hand-built for the same reason [`crate::stream`]'s is: a chunked GET cannot live under
/// `vike_bridge_core::http::blocking_agent`'s global request timeout.
pub(super) fn streaming_agent() -> ureq::Agent {
    let market = venue_timings::OANDA.market();
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(market.dial_timeout()))
        .timeout_send_request(Some(market.response_timeout()))
        .timeout_recv_body(Some(market.idle_threshold()))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// `subscribe_quotes` thread body: hold the pricing stream, fold each line, emit quotes +
/// health disclosures (stream label [`StreamLabel::Quotes`]).
pub(super) fn quotes_main(instrument: String, series: String, ctx: FeedCtx) {
    let agent = streaming_agent();
    let url = format!(
        "{}/v3/accounts/{}/pricing/stream?instruments={}",
        ctx.config.stream_base, ctx.config.account_id, instrument
    );
    let bearer = format!("Bearer {}", ctx.config.api_token);
    let mut health = StreamHealth::new(FRESHNESS_THRESHOLD_QUOTES.as_millis() as i64);
    ctx.set_status(format!("connecting · OANDA quotes {series}"));

    let dial = || {
        let resp = agent
            .get(&url)
            .header("Authorization", &bearer)
            .header("Accept-Datetime-Format", "UNIX")
            .call()
            .map_err(describe_dial_error)?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(describe_dial_error(ureq::Error::StatusCode(status)));
        }
        Ok(BufReader::new(resp.into_body().into_reader()))
    };

    pump_lines(&ctx.stop, RECONNECT_BACKOFF, dial, |ev| match ev {
        PumpEvent::Connected => {
            // Arm freshness per session (the depth feeds' arm-at-start): a session that then
            // receives no PRICE at all still ages toward `Stale` from here. In practice OANDA
            // replays a current-price snapshot on open, which re-seeds the clock immediately.
            health.reset_freshness(now_ms());
            ctx.set_status(format!("LIVE · OANDA quotes {series}"));
        }
        PumpEvent::Line(line) => {
            let (quote, events) = fold_pricing_line(line, &series, &mut health, now_ms());
            let emitted = quote.is_some() || !events.is_empty();
            for e in events {
                ctx.sink.stream_status(VENUE, &series, StreamLabel::Quotes.as_str(), e);
            }
            if let Some(q) = quote {
                ctx.sink.quote(VENUE, &series, q);
            }
            if emitted {
                (ctx.wake)();
            }
        }
        PumpEvent::ConnectFailed(e) => {
            if let Some(g) = health.enter_gap(now_ms()) {
                ctx.sink.stream_status(VENUE, &series, StreamLabel::Quotes.as_str(), g);
            }
            tracing::warn!(
                venue = VENUE,
                symbol = %series,
                error = %e,
                "pricing-stream open failed; backing off"
            );
            ctx.set_status(format!("OANDA quotes {series}: {e}; reconnecting"));
        }
        PumpEvent::Disconnected => {
            if let Some(g) = health.enter_gap(now_ms()) {
                ctx.sink.stream_status(VENUE, &series, StreamLabel::Quotes.as_str(), g);
            }
            ctx.set_status(format!("OANDA quotes {series}: stream ended; reconnecting"));
        }
    });
}
