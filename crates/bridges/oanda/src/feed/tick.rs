//! The quote lane: the pricing-stream thread body, on the shared market pump.
//!
//! The chunked-HTTP body is read as newline-delimited lines (`vike_bridge_core::websocket_session::user::line_stream`'s
//! `LineStream`) on `vike_bridge_core::websocket_session::market`'s `run_market_on`, whose knobs are this venue's
//! pump row (`vike_bridge_core::websocket_session::spec::row`, which reads the market lane of
//! `vike_venues::venue_timings::OANDA`): the pump redials on the row's fixed ladder after a failed open or an ended
//! stream, judges a silent body by the row's idle bound, and says when a session is live. The lane keeps what is its
//! own: the open (its agent, its URL, its words for a failed open), the fold of each line, and the stream's health.

use std::cell::RefCell;
use std::io::BufReader;

use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::stream_health::StreamHealth;
use vike_bridge_core::websocket_session::market::{
    WebsocketSessionFrameOutcome, WebsocketSessionStatus, run_market_on,
};
use vike_bridge_core::websocket_session::spec;
use vike_bridge_core::websocket_session::user::line_stream::{LineFraming, LineStream};
use vike_marketdata::TickQuote;
use vike_model::feed_status::DataClientConnectionState;
use vike_model::{StreamLabel, StreamStatus, now_ms};
use vike_venues::venue_timings;

use super::{FRESHNESS_THRESHOLD_QUOTES, VENUE};
use crate::config::OandaConfig;
use crate::execution::user_data::describe_dial_error;
use crate::feed::bar::seed::Seed;
use crate::feed::decode::{PricingFrame, decode_pricing_frame};

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
) -> (Option<TickQuote>, Vec<StreamStatus>) {
    let mut events = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return (None, events);
    };
    match decode_pricing_frame(&v) {
        PricingFrame::Quote(mut q) => {
            events.extend(health.recover());
            q.local_timestamp = now_ms; // machine receive time (dual-timestamp capture)
            q.symbol = series.into(); // relabel venue instrument → subscription series
            health.observe_data(q.timestamp);
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

/// The streaming agent: idle-bounded reads (the market lane's `idle_threshold`), bounded dial (its
/// `dial_timeout`, so a black-holed route can never hold a feed thread for the OS's own SYN
/// ladder), the wait for the response head (its `response_timeout`, which is NOT the dial: both are
/// 10 s today and they are separate facts) and, like the transactions stream's agent, non-2xx AS A
/// RESPONSE (`StatusPolicy::Response`), so a rejected open is classified by status instead of blurring into the
/// transport arm. The bounds are the market lane of `vike_venues::venue_timings::OANDA`.
/// ⚠ The head wait is `timeout_recv_response` and the idle bound `timeout_per_read`, never
/// `timeout_recv_body`: in ureq 3.5 that is a total budget on the whole body, which would end a healthy
/// stream that many seconds after its head (`crate::execution::user_data`'s `transactions_agent` has
/// the mechanism). The per-read bound caps the head wait too, which is why it is the idle bound and not the pump's
/// shorter poll: the pump reads one line per read, and the first read that waits it out trips the pump's idle
/// watchdog (the pump row's `read_timeout` and `idle_threshold` are this one bound).
/// Built by `crates/vike-transport/src/http.rs`'s `stream_agent` (no total timeout, no redirect: a 3xx on the
/// credentialed open is a non-2xx response, never a second request) for the same reason
/// [`crate::execution::user_data`]'s is: a chunked GET cannot live under
/// `vike_transport::http::blocking_agent`'s global request timeout.
pub(super) fn streaming_agent() -> ureq::Agent {
    let market = venue_timings::OANDA.market();
    vike_transport::http::stream_agent(
        market.dial_timeout(),
        Some(market.response_timeout()),
        market.idle_threshold(),
        vike_transport::http::StatusPolicy::Response,
    )
}

/// Open the pricing stream at `url`: the body's lines, or the failed open in words that name its cause (the status,
/// or the bound that expired), never the URL (it carries the account id).
fn open_pricing_stream(
    agent: &ureq::Agent,
    url: &str,
    bearer: &str,
) -> Result<LineStream<BufReader<ureq::BodyReader<'static>>>, String> {
    let market = venue_timings::OANDA.market();
    let response = agent
        .get(url)
        .header("Authorization", bearer)
        .header("Accept-Datetime-Format", "UNIX")
        .call()
        .map_err(|e| describe_dial_error(e, market))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(describe_dial_error(ureq::Error::StatusCode(status), market));
    }
    let reader = BufReader::new(response.into_body().into_reader());
    Ok(LineStream::new(reader, LineFraming::NewlineDelimitedJson))
}

/// `subscribe_quotes` thread body: hold the pricing stream on the shared market pump, fold each line, emit quotes +
/// health disclosures (stream label [`StreamLabel::Quotes`]). `seed` is this pair's hold on the REST
/// first-price lane (`crate::feed::bar::seed`), asked as the thread starts, BESIDE the stream open: a stream open
/// that hangs for its whole head wait must not hold the pair's first price.
pub(super) fn quotes_main(
    instrument: String,
    series: String,
    ctx: DataClientContext<OandaConfig>,
    seed: Option<Seed>,
) {
    if let Some(seed) = &seed {
        seed.ask(&instrument, &series);
    }
    let agent = streaming_agent();
    let url = format!(
        "{}/v3/accounts/{}/pricing/stream?instruments={}",
        ctx.own.stream_base, ctx.own.account_id, instrument
    );
    let bearer = format!("Bearer {}", ctx.own.api_token.expose());
    // Every hook below reads it, and the pump calls them one at a time on this thread.
    let health = RefCell::new(StreamHealth::new(FRESHNESS_THRESHOLD_QUOTES.as_millis() as i64));
    ctx.set_status(
        DataClientConnectionState::Connecting,
        format!("connecting · OANDA quotes {series}"),
    );

    run_market_on(
        || match open_pricing_stream(&agent, &url, &bearer) {
            Ok(lines) => {
                // Arm freshness per session (the depth feeds' arm-at-start): a session that then
                // receives no PRICE at all still ages toward `Stale` from here. In practice OANDA
                // replays a current-price snapshot on open, which re-seeds the clock immediately.
                health.borrow_mut().reset_freshness(now_ms());
                Ok(lines)
            }
            // A failed open is logged with its cause: the transactions stream's silent-retry trap (a bad token looping
            // with no log) is answered by describing every failed open (`describe_dial_error`).
            Err(e) => {
                tracing::warn!(
                    venue = VENUE,
                    symbol = %series,
                    error = %e,
                    "pricing-stream open failed; backing off"
                );
                Err(e)
            }
        },
        &spec::row(VENUE).opts(None, None),
        &ctx.stop,
        &now_ms,
        |line| {
            let (quote, events) =
                fold_pricing_line(line, &series, &mut health.borrow_mut(), now_ms());
            let emitted = quote.is_some() || !events.is_empty();
            for e in events {
                ctx.sink.stream_status(VENUE, &series, StreamLabel::Quotes.word(), e);
            }
            if let Some(q) = quote {
                // Before the sink, so a seed still in flight can never land after the stream's tick.
                if let Some(seed) = &seed {
                    seed.stream_ticked();
                }
                ctx.sink.quote(VENUE, &series, q);
            }
            if emitted {
                (ctx.wake)();
            }
            // Every line of a 2xx body is the venue speaking on the subscription its URL made.
            WebsocketSessionFrameOutcome::Confirm
        },
        || {},
        |status| match status {
            WebsocketSessionStatus::Live => ctx.set_status(
                DataClientConnectionState::Connected,
                format!("LIVE · OANDA quotes {series}"),
            ),
            // A failed open or an ended stream is a fault, as any session end is on the shared driver.
            WebsocketSessionStatus::Error(e) => {
                if let Some(g) = health.borrow_mut().enter_gap(now_ms()) {
                    ctx.sink.stream_status(VENUE, &series, StreamLabel::Quotes.word(), g);
                }
                ctx.set_status(
                    DataClientConnectionState::Error,
                    format!("OANDA quotes {series}: {e}; reconnecting"),
                );
            }
            // The row's opts arm no notice hook (the venue announces no close): no session ends announced.
            WebsocketSessionStatus::Announced(_) => {}
        },
    );
}
