//! [`LineStream`] over scripted bodies, alone and on the real pump.

use std::collections::VecDeque;
use std::io::{self, BufReader, Read};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::Value;
use vike_model::events::Event;
use vike_venues::reconnect::ReconnectBackoff;

use super::super::stream_link::{report_on_this_thread, stream_link};
use super::super::{
    WebsocketSessionUserError, WebsocketSessionUserMessage, WebsocketSessionUserOpenOutcome,
    WebsocketSessionUserStream, fill_at, run_user,
};
use super::{LineFraming, LineStream};
use crate::venue_link::venue_link;

use WebsocketSessionUserError::{Closed, Timeout};
use WebsocketSessionUserMessage::{Other, Text};

/// A body that answers each read with the next step, then ends. A timed-out step waits a millisecond first, on the
/// pump's clock (`crate::pump_clock`), as a read that waits out its bound does, so a silent body lets that clock run.
struct Body(VecDeque<io::Result<&'static str>>);

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.pop_front() {
            None => Ok(0),
            Some(Ok(text)) => {
                buf[..text.len()].copy_from_slice(text.as_bytes());
                Ok(text.len())
            }
            Some(Err(e)) => {
                if e.kind() == io::ErrorKind::TimedOut {
                    crate::pump_clock::sleep(Duration::from_millis(1));
                }
                Err(e)
            }
        }
    }
}

fn stream(
    framing: LineFraming,
    steps: Vec<io::Result<&'static str>>,
) -> LineStream<BufReader<Body>> {
    LineStream::new(BufReader::new(Body(steps.into())), framing)
}

/// Every read up to and including the first close.
fn reads(
    mut stream: LineStream<BufReader<Body>>,
) -> Vec<Result<WebsocketSessionUserMessage, WebsocketSessionUserError>> {
    let mut out = Vec::new();
    loop {
        let read = stream.recv();
        let closed = matches!(read, Err(Closed(_)));
        out.push(read);
        if closed {
            return out;
        }
    }
}

fn text(payload: &str) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
    Ok(Text(payload.to_string()))
}

fn ended() -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
    Err(Closed("the stream's body ended".to_string()))
}

/// **Server-sent events: a `data:` field is the payload, every other line is proof of life**, alpaca's
/// `/v2/events/trades` as its own read loop takes it (a comment, an `event:` field, the blank line that ends an event,
/// `[DONE]`), and the body's end closes the session.
#[test]
fn a_server_sent_events_body_gives_its_data_fields_and_says_life_for_the_rest() {
    let body = ": heartbeat\nevent: trade_updates\ndata: {\"event\":\"fill\"}\n\ndata:{\"n\":2}\r\ndata: [DONE]\ndata:\n";
    assert_eq!(
        reads(stream(LineFraming::ServerSentEvents, vec![Ok(body)])),
        [
            Ok(Other),
            Ok(Other),
            text(r#"{"event":"fill"}"#),
            Ok(Other),
            text(r#"{"n":2}"#),
            Ok(Other),
            Ok(Other),
            ended(),
        ]
    );
}

/// **Newline-delimited JSON (a chunked HTTP body): a non-blank line is the payload**, oanda's transactions stream with
/// its HEARTBEAT lines (decoded to nothing by the venue, but proof of life), a blank keep-alive line, and a last line
/// the body ends without a newline.
#[test]
fn a_json_lines_body_gives_each_line_and_its_last_line_without_a_newline() {
    let steps = vec![Ok("{\"type\":\"HEARTBEAT\"}\r\n\n"), Ok(r#"{"type":"ORDER_FILL","id":"7"}"#)];
    assert_eq!(
        reads(stream(LineFraming::NewlineDelimitedJson, steps)),
        [
            text(r#"{"type":"HEARTBEAT"}"#),
            Ok(Other),
            text(r#"{"type":"ORDER_FILL","id":"7"}"#),
            ended(),
        ]
    );
}

/// **A read that waited out its bound is the pump's poll tick, and the part line it cut survives it**: the pump's
/// watchdog judges the silence, and a body that carries on after the timed-out read hands out the whole line. Windows
/// says `TimedOut` and unix `WouldBlock` for one socket read timeout; both are the tick.
#[test]
fn a_timed_out_read_is_a_tick_and_keeps_the_part_line() {
    let steps = vec![
        Ok(r#"{"type":"ORDER_"#),
        Err(io::ErrorKind::TimedOut.into()),
        Ok(r#"FILL","id":"#),
        Err(io::ErrorKind::WouldBlock.into()),
        Ok("\"8\"}\n"),
    ];
    assert_eq!(
        reads(stream(LineFraming::NewlineDelimitedJson, steps)),
        [Err(Timeout), Err(Timeout), text(r#"{"type":"ORDER_FILL","id":"8"}"#), ended()]
    );
}

/// **A `ureq` body's per-read timeout is the tick too, and keeps the part line**: ureq 3.5 hands it to the body's
/// reader as kind `Other` wrapping its own `ureq::Error::Timeout`, which read as the body's end would make a silent
/// stream a clean close the idle watchdog never judges (alpaca's SSE and oanda's transactions read through ureq).
///
/// MUTATION PROOF: drop the downcast from `line_stream.rs`'s `timed_out` — the read is `Closed("timeout: per read")`.
#[test]
fn a_ureq_per_read_timeout_is_a_tick_and_keeps_the_part_line() {
    let ureq_timeout = || ureq::Error::Timeout(ureq::Timeout::PerRead).into_io();
    assert_eq!(ureq_timeout().kind(), io::ErrorKind::Other, "the shape ureq hands a body's reader");
    let steps = vec![Ok("data: {\"n\""), Err(ureq_timeout()), Ok(":3}\n")];
    assert_eq!(
        reads(stream(LineFraming::ServerSentEvents, steps)),
        [Err(Timeout), text(r#"{"n":3}"#), ended()]
    );
    let other = || ureq::Error::ConnectionFailed.into_io();
    assert_eq!(
        reads(stream(LineFraming::ServerSentEvents, vec![Err(other())])),
        [Err(Closed(other().to_string()))],
        "another ureq error still closes the session"
    );
}

/// **Any other read error closes the session in its own words**, so the pump reopens and the lane says why.
#[test]
fn another_read_error_closes_in_its_words() {
    let steps = vec![
        Ok("data: {\"n\":1}\n"),
        Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset by peer")),
    ];
    assert_eq!(
        reads(stream(LineFraming::ServerSentEvents, steps)),
        [text(r#"{"n":1}"#), Err(Closed("reset by peer".to_string()))]
    );
    assert_eq!(
        stream(LineFraming::ServerSentEvents, vec![]).pong(b"ping".to_vec()),
        Ok(()),
        "HTTP has no ping, so there is nothing to answer"
    );
}

/// **On the real pump, a line stream is a session like a socket's**: each payload is decoded and emitted, the body's
/// end reopens it (and fires the replay's hook), and a body that goes silent trips the idle watchdog. The C wave moves
/// alpaca's SSE and oanda's chunked transactions here.
#[test]
fn the_pump_decodes_a_line_stream_reopens_at_its_end_and_trips_on_its_silence() {
    let route = "oanda#LINE-STREAM-PUMP";
    report_on_this_thread(Some(stream_link(route)));
    let stop = AtomicBool::new(false);
    let (mut opens, mut reconnects, mut traded) = (0, 0, Vec::new());
    let mut read_at_reopen = Vec::new();
    let ended = run_user(
        || {
            opens += 1;
            if opens > 1 {
                read_at_reopen.push(venue_link(route).state().connection_state());
            }
            let steps = match opens {
                1 => vec![Ok("data: {\"trade\":\"t1\"}\n: keep-alive\n")],
                // Silent: every read waits out its bound, the shape a dead route behind a live TCP session has.
                2 => (0..1_000).map(|_| Err(io::ErrorKind::TimedOut.into())).collect(),
                3 => vec![Ok("data: {\"trade\":\"t2\"}\n")],
                _ => return WebsocketSessionUserOpenOutcome::Stopped,
            };
            WebsocketSessionUserOpenOutcome::Ready(stream(LineFraming::ServerSentEvents, steps))
        },
        |frame: &Value| vec![fill_at("ours", frame["trade"].as_str().unwrap_or_default(), 1)],
        |event| {
            if let Event::Fill(fill) = event {
                traded.push(fill.trade_id.to_string());
            }
            true
        },
        &stop,
        Duration::from_millis(1),
        ReconnectBackoff::new(Duration::from_millis(1), Duration::from_millis(1)),
        None,
        || reconnects += 1,
        Some(Duration::from_millis(20)),
        None,
    );
    report_on_this_thread(None);
    ended.expect("no refusal");

    use vike_model::feed_status::DataClientConnectionState::{Connecting, Error};
    assert_eq!(traded, ["t1", "t2"]);
    assert_eq!(reconnects, 2, "two reopens: after the body's end and after the stall");
    assert_eq!(
        read_at_reopen,
        [Connecting, Error, Connecting],
        "a body's end is a reopen, its silence a failure"
    );
}

/// **On the market pump, a line stream is a session like a socket's** (a pricing stream over chunked HTTP): each
/// payload line is the venue's frame and the first one says the session is live, a blank line is read past, a body
/// gone silent is judged by the idle watchdog from its last whole line (the timed-out reads before that are the pump's
/// ticks), the body's end is the session's fault, and nothing is ever written to it.
#[test]
fn the_market_pump_reads_a_line_stream_and_trips_on_its_silence() {
    use crate::websocket_session::market::{
        WebsocketSessionFrameOutcome, WebsocketSessionMarketOptions, WebsocketSessionMarketStream,
        WebsocketSessionStatus, run_market_on,
    };
    use vike_venues::venue_timings::VenueTimingsReconnect;
    // On the wall clock a loaded box could stall this thread past the 20 ms threshold before the first silent read,
    // so the trip came with no tick (CI 2026-11-23); the virtual clock moves only with the body's reads.
    let clock = crate::pump_clock::VirtualClock::new(1);
    let _seat = clock.enter();
    let silent: Vec<io::Result<&'static str>> =
        (0..1_000).map(|_| Err(io::ErrorKind::TimedOut.into())).collect();
    let mut bodies = VecDeque::from([
        [vec![Ok("{\"n\":1}\n\n")], silent].into_iter().flatten().collect::<Vec<_>>(),
        vec![Ok("{\"n\":2}")],
    ]);
    let stop = AtomicBool::new(false);
    let opts = WebsocketSessionMarketOptions {
        subscribe: None,
        keepalive: None,
        ack_timeout: None,
        idle_threshold: Some(Duration::from_millis(20)),
        read_timeout: Duration::from_millis(1),
        backoff: VenueTimingsReconnect::fixed(Duration::from_millis(1), "a test's 1 ms redial"),
        connect_timeout: None,
        notice: None,
    };
    let (mut frames, mut ticks, mut said) = (Vec::new(), 0, Vec::new());
    run_market_on(
        || match bodies.pop_front() {
            Some(steps) => Ok(stream(LineFraming::NewlineDelimitedJson, steps)),
            None => {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                Err("no more bodies".to_string())
            }
        },
        &opts,
        &stop,
        &|| 0,
        |frame| {
            frames.push(frame.to_string());
            WebsocketSessionFrameOutcome::Confirm
        },
        || ticks += 1,
        |status| {
            said.push(match status {
                WebsocketSessionStatus::Live => "live".to_string(),
                WebsocketSessionStatus::Error(why) => why.to_string(),
                WebsocketSessionStatus::Announced(why) => format!("announced: {why}"),
            });
        },
    );
    assert_eq!(frames, [r#"{"n":1}"#, r#"{"n":2}"#]);
    assert_eq!(
        said,
        [
            "live",
            "no frames within 0s (silent stall)",
            "live",
            "the stream's body ended",
            "no more bodies",
        ]
    );
    assert!(ticks > 0, "the silent reads before the trip are ticks, not faults");
    assert_eq!(
        stream(LineFraming::NewlineDelimitedJson, vec![]).send_text("ping"),
        Err(Closed("an HTTP body takes no frame: a line stream sends nothing".to_string())),
    );
}
