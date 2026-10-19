use super::bars::{
    FALLBACK_PACE_MS, MAX_POLL, MAX_TAIL_COUNT, MIN_POLL, bar_poll_cadence, fold_candles_response,
    tail_count,
};
use super::quotes::{PumpEvent, fold_pricing_line, pump_lines, streaming_agent};
use super::seed::{
    BURST, COLLECT, MAX_PER_CALL, MAX_SEED_AGE_MS, PER_SYMBOL_FLOOR, Pace, Pending, REFILL,
    SEED_BOUND, WANT_TTL, seed_agent, seed_tick, serve,
};
use super::*;
use serde_json::{Value, json};
use std::assert_matches;
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::time::Instant;
use vike_bridge_core::stream_health::StreamHealth;
use vike_data::{NoopSink, RecordingSink};
use vike_marketdata::QuoteTick;
use vike_model::{StreamStatus, now_ms};

fn health(threshold_ms: i64) -> StreamHealth {
    StreamHealth::new(threshold_ms)
}

fn price_line(ts_secs: &str) -> String {
    format!(
        r#"{{"type":"PRICE","time":"{ts_secs}","instrument":"EUR_USD",
                "bids":[{{"price":"1.09000","liquidity":1000000}}],
                "asks":[{{"price":"1.09010","liquidity":1000000}}]}}"#
    )
}

// --- fold_pricing_line: the heartbeat→freshness contract ---------------------------------

#[test]
fn a_price_line_yields_a_relabelled_stamped_quote() {
    let mut h = health(300_000);
    let (q, events) = fold_pricing_line(&price_line("100.000"), "eurusd", &mut h, 100_500);
    let q = q.expect("price → quote");
    assert_eq!(q.symbol, "eurusd", "venue instrument relabelled to the subscription series");
    assert_eq!(q.local_ts, 100_500, "receive time stamped by the fold");
    assert_eq!(q.ts, 100_000);
    assert!(events.is_empty(), "fresh data on a healthy stream discloses nothing");
}

#[test]
fn heartbeats_keep_transport_alive_but_age_to_stale_once() {
    let mut h = health(300_000);
    // t=0: a real price seeds the freshness clock
    let (q, ev) = fold_pricing_line(&price_line("0.000"), "eurusd", &mut h, 0);
    assert!(q.is_some() && ev.is_empty());
    // heartbeats keep flowing: fresh at +200s, stale at +301s — disclosed ONCE
    let hb = r#"{"type":"HEARTBEAT","time":"200.000"}"#;
    let (q, ev) = fold_pricing_line(hb, "eurusd", &mut h, 200_000);
    assert!(q.is_none() && ev.is_empty());
    let (_, ev) = fold_pricing_line(hb, "eurusd", &mut h, 301_000);
    assert_eq!(ev, vec![StreamStatus::Stale { newest_data_ts_ms: 0, now_ms: 301_000 }]);
    let (_, ev) = fold_pricing_line(hb, "eurusd", &mut h, 400_000);
    assert!(ev.is_empty(), "an open stale episode is not re-disclosed");
    // a fresh price closes the episode
    let (q, ev) = fold_pricing_line(&price_line("401.000"), "eurusd", &mut h, 401_000);
    assert!(q.is_some());
    assert_eq!(ev, vec![StreamStatus::Live { gap_started_ts_ms: Some(301_000) }]);
}

#[test]
fn the_first_line_after_a_gap_recovers_even_a_heartbeat() {
    let mut h = health(300_000);
    assert!(h.enter_gap(50).is_some());
    let (q, ev) = fold_pricing_line(r#"{"type":"HEARTBEAT","time":"1.0"}"#, "s", &mut h, 60);
    assert!(q.is_none());
    assert_eq!(ev, vec![StreamStatus::Live { gap_started_ts_ms: Some(50) }]);
}

#[test]
fn garbage_and_other_frames_do_nothing() {
    let mut h = health(300_000);
    for line in ["not json", r#"{"type":"UNKNOWN"}"#, ""] {
        let (q, ev) = fold_pricing_line(line, "s", &mut h, 0);
        assert!(q.is_none() && ev.is_empty(), "line {line:?}");
    }
    // …and an un-decodable line after a gap does NOT recover it (no venue frame was seen)
    let _ = h.enter_gap(10);
    let (_, ev) = fold_pricing_line("not json", "s", &mut h, 20);
    assert!(ev.is_empty() && h.in_gap());
}

// --- pump_lines: the scripted-dial reconnect/backoff lifecycle ---------------------------

/// Drive the pump with a script of dial outcomes; record the event tags in order. The pump
/// is stopped by the script running dry (the dial raises the stop flag).
fn run_scripted(script: Vec<Result<&'static str, String>>) -> Vec<String> {
    let stop = AtomicBool::new(false);
    let mut dials = script.into_iter();
    let log = std::cell::RefCell::new(Vec::new());
    let dial = || match dials.next() {
        Some(Ok(body)) => Ok(Cursor::new(body.as_bytes().to_vec())),
        Some(Err(e)) => Err(e),
        None => {
            stop.store(true, Ordering::Relaxed);
            Err("script over".into())
        }
    };
    pump_lines(&stop, Duration::ZERO, dial, |ev| {
        log.borrow_mut().push(match ev {
            PumpEvent::Connected => "connect".to_string(),
            PumpEvent::Line(l) => format!("line:{l}"),
            PumpEvent::ConnectFailed(e) => format!("fail:{e}"),
            PumpEvent::Disconnected => "disconnect".to_string(),
        });
    });
    log.into_inner()
}

#[test]
fn pump_delivers_trimmed_lines_and_reconnects_after_disconnect_and_dial_failure() {
    let got = run_scripted(vec![
        Ok("a\n\n  b  \n"), // blank skipped, whitespace trimmed
        Err("HTTP 401 on stream open".into()),
        Ok("c\n"),
    ]);
    assert_eq!(
        got,
        vec![
            "connect",
            "line:a",
            "line:b",
            "disconnect",
            "fail:HTTP 401 on stream open",
            "connect",
            "line:c",
            "disconnect",
            "fail:script over",
        ]
    );
}

#[test]
fn a_stop_raised_during_backoff_ends_the_pump() {
    // Stop raised by the dial itself (the script-over arm) — the pump must return without
    // another dial; with a REAL backoff the sleep is stop-aware (sleep_stop_aware returns
    // true within ~one slice), covered by vike-bridge-core's own poller tests.
    let got = run_scripted(vec![]);
    assert_eq!(got, vec!["fail:script over"]);
}

#[test]
fn a_stop_raised_mid_stream_stops_before_the_next_line() {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_in = Arc::clone(&stop);
    let mut seen = Vec::new();
    let mut dialed = false;
    pump_lines(
        &stop,
        Duration::ZERO,
        move || {
            assert!(!dialed, "no redial after a mid-stream stop");
            dialed = true;
            Ok(Cursor::new(b"one\ntwo\nthree\n".to_vec()))
        },
        |ev| {
            if let PumpEvent::Line(l) = ev {
                seen.push(l.to_string());
                stop_in.store(true, Ordering::Relaxed); // raise on the FIRST line
            }
        },
    );
    assert_eq!(seen, vec!["one"], "reads stop at the flag, mid-body");
}

// --- the connection bounds: the market row's, observed where they take effect -------------

/// The bounds `streaming_agent` builds are the market lane's row, each on its own builder call —
/// observed on the BUILT agent (`agent.config().timeouts()`). `connect` and `send_request` are both
/// 10 s today and are two facts: the dial and the wait for the response head.
#[test]
fn the_streaming_agent_carries_the_market_rows_bounds() {
    let timeouts = streaming_agent().config().timeouts();
    assert_eq!(timeouts.connect, Some(Duration::from_secs(10)), "the dial bound");
    assert_eq!(timeouts.send_request, Some(Duration::from_secs(10)), "the response-head wait");
    assert_eq!(timeouts.recv_response, None, "a recv_response bound would cap the whole body");
    assert_eq!(timeouts.recv_body, Some(Duration::from_secs(20)), "the idle bound per body read");
}

/// The quote pump and the bar poll's retry both wait [`RECONNECT_BACKOFF`], which is the row's
/// `initial`: that is the whole policy only while the row is a FIXED wait, so a row that started to
/// grow fails here rather than silently doing only its first step.
#[test]
fn reconnect_backoff_is_the_market_rows_fixed_three_seconds() {
    assert_eq!(RECONNECT_BACKOFF, Duration::from_secs(3));
    assert!(venue_timings::OANDA.market().reconnect().is_fixed());
}

// --- the bar lane's pure pieces ----------------------------------------------------------

#[test]
fn bar_poll_cadence_clamps_to_the_declared_window() {
    assert_eq!(bar_poll_cadence(60_000), MIN_POLL); // 1m → floor
    assert_eq!(bar_poll_cadence(300_000), Duration::from_secs(25)); // 5m → interval/12
    assert_eq!(bar_poll_cadence(3_600_000), MAX_POLL); // 1h+ → ceiling
    assert_eq!(bar_poll_cadence(FALLBACK_PACE_MS), MAX_POLL);
}

#[test]
fn tail_count_covers_the_outage_and_caps() {
    assert_eq!(tail_count(0, 60_000), 2); // steady state: boundary + forming
    assert_eq!(tail_count(60_000, 60_000), 3);
    assert_eq!(tail_count(10 * 60_000, 60_000), 12);
    assert_eq!(tail_count(i64::MAX / 2, 60_000), MAX_TAIL_COUNT); // capped
    assert_eq!(tail_count(-5, 60_000), 2, "clock skew never underflows");
}

#[test]
fn fold_candles_emits_only_closes_past_the_watermark() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"candles": [
                {"complete": true,  "volume": 1, "time": "60.0",
                 "mid": {"o": "1", "h": "1", "l": "1", "c": "1.1"}},
                {"complete": true,  "volume": 1, "time": "120.0",
                 "mid": {"o": "1", "h": "1", "l": "1", "c": "1.2"}},
                {"complete": false, "volume": 1, "time": "180.0",
                 "mid": {"o": "1", "h": "1", "l": "1", "c": "1.3"}}
            ]}"#,
    )
    .unwrap();
    // seed shape: everything complete is fresh, watermark advances to the newest close
    let (fresh, forming, last) = fold_candles_response(&v, 0);
    assert_eq!(fresh.len(), 2);
    assert_eq!(last, 120_000);
    assert_eq!(forming.as_ref().map(|b| b.ts), Some(180_000));
    // steady state: nothing new past the watermark → no closes, watermark holds
    let (fresh, _, last) = fold_candles_response(&v, 120_000);
    assert!(fresh.is_empty());
    assert_eq!(last, 120_000);
    // one candle closed since
    let (fresh, _, last) = fold_candles_response(&v, 60_000);
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].close, 1.2);
    assert_eq!(last, 120_000);
}

// --- Feeds: refusals + lifecycle (no sockets) --------------------------------------------

fn feeds() -> Feeds {
    Feeds::new(Arc::new(NoopSink), || {})
}

fn dummy_config() -> OandaConfig {
    let fields = vike_secrets::account_credentials::CredentialFields::from_pairs([
        ("API_KEY", "tok"),
        ("ACCOUNT_ID", "acct-1"),
    ]);
    let creds =
        vike_secrets::account_credentials::AccountCredentials::new(Some(1), Some(&fields), None);
    crate::config::load_oanda_config_from(vike_model::AccountTier::Demo, &creds).unwrap()
}

#[test]
fn uncredentialed_subscribes_refuse_with_the_live_gate() {
    let mut f = feeds();
    for verdict in
        [f.subscribe_quotes("eurusd").unwrap_err(), f.subscribe_bars("eurusd", "1m").unwrap_err()]
    {
        let LiveDataError::Subscribe(msg) = verdict else {
            panic!(
                "uncredentialed refusal is Subscribe, not Unsupported (the venue DOES \
                        serve the verb)"
            );
        };
        assert!(msg.contains("no credentials"), "{msg}");
    }
    f.shutdown(); // nothing spawned — a no-op
}

#[test]
fn caps_refusals_stay_in_lockstep_with_the_declared_table() {
    // Credentialed, so the caps refusal (not the credential gate) is what answers.
    let mut f = feeds().with_config(dummy_config());
    for err in [
        f.subscribe_trades("eurusd").unwrap_err(),
        f.subscribe_book("eurusd").unwrap_err(),
        f.subscribe_depth("eurusd").unwrap_err(),
    ] {
        assert_matches!(err, LiveDataError::Unsupported(_), "{err}");
    }
}

#[test]
fn an_interval_without_a_granularity_is_refused_before_any_thread_exists() {
    let mut f = feeds().with_config(dummy_config());
    let err = f.subscribe_bars("eurusd", "7s").unwrap_err();
    assert_matches!(err, LiveDataError::Subscribe(ref m) if m.contains("granularity"));
    f.shutdown();
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_noop() {
    let mut f = feeds();
    f.unsubscribe(SubscriptionId(41));
    f.begin_shutdown();
    f.shutdown();
}

/// A loopback origin answering every request with a 302 to a SECOND loopback listener, plus that
/// listener's hit counter: the stand-in for the host a followed redirect would replay credentials
/// to.
fn redirecting_origin() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering::SeqCst};
    fn serve(reply: String) -> (u16, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                seen.fetch_add(1, SeqCst);
                let _ = conn.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                let mut buf = [0u8; 8192];
                for _ in 0..2 {
                    let _ = conn.read(&mut buf);
                }
                let _ = conn.write_all(reply.as_bytes());
            }
        });
        (port, hits)
    }
    let (target, target_hits) =
        serve("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into());
    let (origin, _) = serve(format!(
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target}/stolen\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    ));
    (format!("http://127.0.0.1:{origin}"), target_hits)
}

/// **A 3xx on the credentialed pricing-stream open surfaces as a response and the `Location` is
/// never dialled.** ureq strips the Bearer token on a redirect, so this is defence in depth: no
/// redirect is followed from a credentialed open at all.
///
/// MUTATION PROOF: delete `.max_redirects(0)` from `streaming_agent` — the 302 is followed.
#[test]
fn the_pricing_stream_agent_never_follows_a_redirect() {
    let (base, target_hits) = redirecting_origin();
    let resp = streaming_agent()
        .get(&format!("{base}/v3/accounts/101-004-1234567-001/pricing/stream"))
        .header("Authorization", "Bearer test-token")
        .call();
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "the target was dialled");
    assert_eq!(resp.expect("a 302 is a response").status().as_u16(), 302);
}

// --- the REST first price (`seed`): a new pair is priced without waiting for the stream open ----

/// The clock every pure seed test judges against, epoch ms.
const NOW_MS: i64 = 1_700_000_000_000;

/// One entry of OANDA's REST pricing reply, as the practice REST host shapes it: the stream's `PRICE`
/// frame, here WITHOUT a `type` (the field is the stream's marker and the REST copy need not carry it).
fn rest_price(instrument: &str, time_ms: i64, tradeable: bool) -> Value {
    json!({
        "instrument": instrument,
        "time": format!("{}.{:09}", time_ms / 1000, (time_ms % 1000) * 1_000_000),
        "bids": [{"price": "1.09000", "liquidity": 10_000_000}],
        "asks": [{"price": "1.09010", "liquidity": 10_000_000}],
        "closeoutBid": "1.08990", "closeoutAsk": "1.09020",
        "status": if tradeable { "tradeable" } else { "non-tradeable" },
        "tradeable": tradeable,
    })
}

fn reply_of(entries: Vec<Value>) -> Value {
    json!({ "prices": entries })
}

/// **The tick a REST price seeds is the venue's own**: relabelled to the subscription, stamped with
/// the receive time, and its `ts` is the venue's `time` (so it is never made to look newer).
#[test]
fn a_tradeable_fresh_rest_price_seeds_a_tick_with_the_venues_own_time() {
    let quote = seed_tick(&rest_price("EUR_USD", NOW_MS - 1_500, true), "eurusd", NOW_MS)
        .expect("a tradeable, two-sided, fresh price seeds");
    assert_eq!(quote.symbol, "eurusd", "relabelled to the subscription's series");
    assert_eq!(quote.ts, NOW_MS - 1_500, "the venue's own time, not the receive time");
    assert_eq!(quote.local_ts, NOW_MS, "receive time stamped");
    assert_eq!((quote.bid, quote.ask), (1.09, 1.0901));
    assert_eq!((quote.bid_size, quote.ask_size), (10_000_000.0, 10_000_000.0));
    // A copy that does carry the stream's `type` decodes the same.
    let mut typed = rest_price("EUR_USD", NOW_MS - 1_500, true);
    typed["type"] = json!("PRICE");
    assert_eq!(seed_tick(&typed, "eurusd", NOW_MS), Ok(quote));
}

/// **No seed rather than a made-up one**: a market the venue marks closed, a one-sided or crossed
/// price, a price whose venue time is past the age bound (or in the future), and anything that is not
/// an entry each feed NOTHING.
#[test]
fn a_closed_one_sided_crossed_or_stale_price_seeds_nothing() {
    let refused = |entry: Value| seed_tick(&entry, "eurusd", NOW_MS).expect_err("no seed");
    assert!(refused(rest_price("EUR_USD", NOW_MS, false)).contains("non-tradeable"));
    // The flag is the verdict; the status word decides only where the flag is absent.
    let mut no_flag = rest_price("EUR_USD", NOW_MS, true);
    no_flag.as_object_mut().unwrap().remove("tradeable");
    assert!(seed_tick(&no_flag, "eurusd", NOW_MS).is_ok(), "status `tradeable` stands in");
    no_flag["status"] = json!("non-tradeable");
    assert!(refused(no_flag.clone()).contains("non-tradeable"));
    no_flag.as_object_mut().unwrap().remove("status");
    assert!(refused(no_flag).contains("non-tradeable"), "neither flag nor status: not tradeable");

    let mut one_sided = rest_price("EUR_USD", NOW_MS, true);
    one_sided["asks"] = json!([]);
    assert!(refused(one_sided).contains("one-sided"));
    let mut crossed = rest_price("EUR_USD", NOW_MS, true);
    crossed["bids"] = json!([{"price": "1.09100", "liquidity": 1}]);
    assert!(refused(crossed).contains("crossed"));
    let mut zero = rest_price("EUR_USD", NOW_MS, true);
    zero["bids"] = json!([{"price": "0", "liquidity": 1}]);
    assert!(refused(zero).contains("crossed or not positive"));

    // The age bound is the venue's time against ours: the edge stands, past it does not.
    assert!(seed_tick(&rest_price("EUR_USD", NOW_MS - MAX_SEED_AGE_MS, true), "x", NOW_MS).is_ok());
    assert!(
        refused(rest_price("EUR_USD", NOW_MS - MAX_SEED_AGE_MS - 1, true)).contains("ms from now")
    );
    assert!(refused(rest_price("EUR_USD", NOW_MS + 6_000, true)).contains("ms from now"));
    let mut no_time = rest_price("EUR_USD", NOW_MS, true);
    no_time.as_object_mut().unwrap().remove("time");
    assert!(refused(no_time).contains("ms from now"), "an unparseable time is no fresh time");
    assert!(refused(json!("EUR_USD")).contains("not an object"));
    assert!(refused(Value::Null).contains("not an object"));
}

/// The pacing alone, on injected instants: the bucket (burst, then one per refill) and the per-pair
/// floor, and a refused take spends and stamps nothing.
#[test]
fn the_pace_is_a_bucket_and_a_per_pair_floor() {
    let t0 = Instant::now();
    let mut pace = Pace::new(t0);
    let one = ["EUR_USD".to_string()];
    for n in 0..BURST as usize {
        assert!(pace.take(&[format!("P{n}")], t0), "burst request {n}");
    }
    assert!(!pace.take(&one, t0), "the bucket is empty");
    assert!(!pace.asked_lately("EUR_USD", t0), "a refused take stamps nothing");
    assert!(!pace.take(&one, t0 + REFILL / 2), "half a refill is not a token");
    assert!(pace.take(&one, t0 + REFILL), "one token per refill");
    assert!(pace.asked_lately("EUR_USD", t0 + REFILL), "the pair is stamped");
    assert!(
        pace.asked_lately("EUR_USD", t0 + REFILL + PER_SYMBOL_FLOOR - Duration::from_millis(1))
    );
    assert!(!pace.asked_lately("EUR_USD", t0 + REFILL + PER_SYMBOL_FLOOR), "the floor ends");
    // A long idle never banks more than the burst.
    let mut idle = Pace::new(t0);
    let later = t0 + Duration::from_secs(3600);
    let spent = (0..10).take_while(|n| idle.take(&[format!("Q{n}")], later)).count();
    assert_eq!(spent, BURST as usize);
}

fn ask(instrument: &str, series: &str, since: Instant) -> Pending {
    Pending {
        since,
        want: Want {
            instrument: instrument.to_string(),
            series: series.to_string(),
            done: Arc::default(),
        },
    }
}

/// One pass of [`serve`]: the instruments its single request named (`None`: no request was made)
/// and the ticks it emitted, as `(series, tick)`.
fn pass(
    pending: &mut Vec<Pending>,
    pace: &mut Pace,
    now: Instant,
    reply: Result<Value, String>,
) -> (Option<Vec<String>>, Vec<(String, QuoteTick)>) {
    let (mut requested, mut ticks) = (None, Vec::new());
    serve(
        pending,
        pace,
        now,
        || NOW_MS,
        |instruments| {
            requested = Some(instruments.to_vec());
            reply
        },
        |want, quote| ticks.push((want.series.clone(), quote)),
    );
    (requested, ticks)
}

/// **A new pair whose stream is silent gets its first tick from ONE REST read.** The pass is all it
/// takes: no stream is involved anywhere.
#[test]
fn a_new_pair_with_a_silent_stream_is_seeded_by_one_rest_read() {
    let t0 = Instant::now();
    let mut pace = Pace::new(t0);
    let mut pending = vec![ask("USD_JPY", "USDJPY", t0)];
    let reply = Ok(reply_of(vec![rest_price("USD_JPY", NOW_MS - 300, true)]));
    let (requested, ticks) = pass(&mut pending, &mut pace, t0, reply);
    assert_eq!(requested, Some(vec!["USD_JPY".to_string()]));
    assert_eq!(ticks.len(), 1);
    assert_eq!(ticks[0].0, "USDJPY");
    assert_eq!(ticks[0].1.ts, NOW_MS - 300, "stamped with the venue's own time");
    assert!(pending.is_empty(), "the ask is served");
}

/// **A market the venue marks closed seeds nothing**, a reply with no entry for the pair seeds
/// nothing, and a stale-timed price seeds nothing: the order is refused in words as before.
#[test]
fn a_closed_missing_or_stale_pair_is_not_seeded() {
    for (name, entry) in [
        ("closed", rest_price("EUR_USD", NOW_MS, false)),
        ("other pair", rest_price("GBP_USD", NOW_MS, true)),
        ("stale", rest_price("EUR_USD", NOW_MS - MAX_SEED_AGE_MS - 1_000, true)),
    ] {
        let t0 = Instant::now();
        let mut pending = vec![ask("EUR_USD", "EURUSD", t0)];
        let (requested, ticks) =
            pass(&mut pending, &mut Pace::new(t0), t0, Ok(reply_of(vec![entry])));
        assert!(requested.is_some(), "{name}: the read was made");
        assert!(ticks.is_empty(), "{name}: nothing may be seeded");
    }
}

/// **A failed read seeds nothing and is not retried**: the pair is held off for the floor, so a
/// venue that is down costs one request, not a loop; past the floor the pair may be asked again.
#[test]
fn a_failed_rest_read_seeds_nothing_and_is_not_repeated_inside_the_floor() {
    let t0 = Instant::now();
    let mut pace = Pace::new(t0);
    let mut pending = vec![ask("EUR_USD", "EURUSD", t0)];
    let (requested, ticks) = pass(&mut pending, &mut pace, t0, Err("HTTP 503".into()));
    assert!(requested.is_some() && ticks.is_empty());

    let soon = t0 + Duration::from_secs(1);
    let mut again = vec![ask("EUR_USD", "EURUSD", soon)];
    let (requested, _) = pass(&mut again, &mut pace, soon, Ok(reply_of(vec![])));
    assert_eq!(requested, None, "inside the floor: no second request");
    assert!(again.is_empty(), "the ask is dropped, the stream prices the pair");

    let after = t0 + PER_SYMBOL_FLOOR;
    let mut later = vec![ask("EUR_USD", "EURUSD", after)];
    let ok = Ok(reply_of(vec![rest_price("EUR_USD", NOW_MS, true)]));
    let (requested, ticks) = pass(&mut later, &mut pace, after, ok);
    assert!(requested.is_some() && ticks.len() == 1, "past the floor the pair is asked again");
}

/// **A burst of new pairs costs ONE combined request**: distinct instruments in ask order, a pair
/// asked twice named once (and seeded under both labels), at most [`MAX_PER_CALL`] per request, the
/// rest served by the next one, and a name that is no instrument never reaches the query.
#[test]
fn a_burst_of_new_pairs_is_one_combined_request() {
    let t0 = Instant::now();
    let mut pace = Pace::new(t0);
    let names = ["EUR_USD", "GBP_USD", "NZD_USD", "USD_JPY", "AUD_USD"];
    let mut pending: Vec<Pending> = names.iter().map(|n| ask(n, &n.replace('_', ""), t0)).collect();
    pending.push(ask("EUR_USD", "eurusd", t0));
    pending.push(ask("EUR_USD&x=1", "bad", t0));
    let reply = reply_of(names.iter().map(|n| rest_price(n, NOW_MS, true)).collect());
    let (requested, ticks) = pass(&mut pending, &mut pace, t0, Ok(reply));
    assert_eq!(requested, Some(names.iter().map(ToString::to_string).collect::<Vec<_>>()));
    let series: Vec<&str> = ticks.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(series, ["EURUSD", "GBPUSD", "NZDUSD", "USDJPY", "AUDUSD", "eurusd"]);
    assert!(pending.is_empty(), "the badly named ask is dropped, not queried");

    // Over the per-request cap: the cap's worth go out, the rest wait for the next request.
    let many: Vec<String> = (0..MAX_PER_CALL + 5).map(|n| format!("AAA_{n:03}")).collect();
    let mut pending: Vec<Pending> = many.iter().map(|n| ask(n, n, t0)).collect();
    let mut pace = Pace::new(t0);
    let (first, _) = pass(&mut pending, &mut pace, t0, Ok(reply_of(vec![])));
    assert_eq!(first.as_ref().map(Vec::len), Some(MAX_PER_CALL));
    assert_eq!(pending.len(), 5);
    let (second, _) = pass(&mut pending, &mut pace, t0, Ok(reply_of(vec![])));
    assert_eq!(second, Some(many[MAX_PER_CALL..].to_vec()));
    assert!(pending.is_empty());
}

/// **The bucket bounds the requests**: with it empty the asks wait (nothing is requested), they are
/// served once a token is back, and an ask that waited past [`WANT_TTL`] is dropped unserved.
#[test]
fn an_empty_bucket_holds_the_asks_until_a_token_and_drops_them_past_the_ttl() {
    let t0 = Instant::now();
    let mut pace = Pace::new(t0);
    for n in 0..BURST as usize {
        let mut one = vec![ask(&format!("P{n}"), "p", t0)];
        assert!(pass(&mut one, &mut pace, t0, Ok(reply_of(vec![]))).0.is_some());
    }
    let mut waiting = vec![ask("EUR_USD", "EURUSD", t0)];
    let (requested, _) = pass(&mut waiting, &mut pace, t0, Ok(reply_of(vec![])));
    assert_eq!(requested, None, "no token, no request");
    assert_eq!(waiting.len(), 1, "the ask waits");

    let ok = Ok(reply_of(vec![rest_price("EUR_USD", NOW_MS, true)]));
    let (requested, ticks) = pass(&mut waiting, &mut pace, t0 + REFILL, ok);
    assert!(requested.is_some() && ticks.len() == 1, "served as soon as a token is back");

    let mut starved = vec![ask("GBP_USD", "GBPUSD", t0)];
    let mut empty = Pace::new(t0);
    for n in 0..BURST as usize {
        assert!(empty.take(&[format!("Q{n}")], t0));
    }
    let (requested, _) = pass(&mut starved, &mut empty, t0 + WANT_TTL, Ok(reply_of(vec![])));
    assert_eq!(requested, None);
    assert!(starved.is_empty(), "past the TTL the ask is dropped, not served late");
}

/// **A stream tick beats the seed, and a late seed never replaces it.** An ask whose stream has
/// already ticked is dropped with no request; one whose stream ticks while the read is in flight
/// is read but not sent.
#[test]
fn a_stream_tick_beats_the_seed_and_a_late_seed_never_lands_after_it() {
    let t0 = Instant::now();
    let mut ticked = vec![ask("EUR_USD", "EURUSD", t0)];
    ticked[0].want.done.set();
    let (requested, ticks) = pass(&mut ticked, &mut Pace::new(t0), t0, Ok(reply_of(vec![])));
    assert_eq!((requested, ticks.len()), (None, 0), "the stream got there first");
    assert!(ticked.is_empty());

    let mut racing = vec![ask("EUR_USD", "EURUSD", t0)];
    let done = Arc::clone(&racing[0].want.done);
    let mut emitted = 0;
    serve(
        &mut racing,
        &mut Pace::new(t0),
        t0,
        || NOW_MS,
        |_| {
            done.set(); // the stream's first tick, while the read is out
            Ok(reply_of(vec![rest_price("EUR_USD", NOW_MS, true)]))
        },
        |_, _| emitted += 1,
    );
    assert_eq!(emitted, 0, "the stream's tick stands");

    // The quote thread ending (its `Seed` dropped) cancels an ask still queued.
    let (line, asks) = std::sync::mpsc::channel();
    let seed = Seed::new(line);
    seed.ask("EUR_USD", "EURUSD");
    drop(seed);
    let queued = asks.try_recv().expect("the ask was queued");
    assert!(queued.done.is_set(), "a dropped Seed cancels its ask");
}

/// **FORCED INTERLEAVING: a seed that is mid-emit when the stream ticks lands BEFORE the stream's
/// tick, never after it.** The seed's emit holds still (the stream has been told to tick) and the
/// stream's [`Done::set`] must not return until it is over, so the board, which keeps whatever the
/// sink was handed last, ends on the stream's price. MUTATION PROOF: delete the lock in `Done::set`
/// and the order flips to `["stream", "seed"]`.
#[test]
fn a_seed_mid_emit_lands_before_the_streams_first_tick_not_after() {
    let t0 = Instant::now();
    let mut pending = vec![ask("EUR_USD", "EURUSD", t0)];
    let done = Arc::clone(&pending[0].want.done);
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let (inside_tx, inside_rx) = std::sync::mpsc::channel();
    let lane = {
        let order = Arc::clone(&order);
        std::thread::spawn(move || {
            serve(
                &mut pending,
                &mut Pace::new(t0),
                t0,
                || NOW_MS,
                |_| Ok(reply_of(vec![rest_price("EUR_USD", NOW_MS, true)])),
                |_, _| {
                    inside_tx.send(()).expect("the test is listening");
                    std::thread::sleep(Duration::from_millis(150)); // the stream gets time to try
                    order.lock().unwrap().push("seed");
                },
            );
        })
    };
    inside_rx.recv().expect("the seed reached its emit");
    done.set(); // the stream's first tick, about to be handed to the sink
    order.lock().unwrap().push("stream");
    lane.join().expect("the lane finished");
    assert_eq!(*order.lock().unwrap(), ["seed", "stream"]);
    let mut sent = false;
    done.unless_set(|| sent = true);
    assert!(!sent, "once the stream has ticked a seed is never sent");
}

/// **The age bound is judged at the reply's ARRIVAL, and it is ten seconds.** The clock is read
/// after the read returns, so a read that takes 3 s ages the price by 3 s: a price 9 s old at arrival
/// seeds, one 11 s old at arrival does not, whatever its age was when the read began.
/// MUTATION PROOF: read the clock before `fetch` and the 8 s-old price seeds after a 3 s read.
#[test]
fn the_seed_age_is_judged_at_arrival_and_is_at_most_ten_seconds() {
    assert_eq!(MAX_SEED_AGE_MS, 10_000);
    let seeded = |venue_age_ms: i64, read_ms: i64| {
        let now = std::cell::Cell::new(NOW_MS);
        let t0 = Instant::now();
        let mut pending = vec![ask("EUR_USD", "EURUSD", t0)];
        let mut ticks = Vec::new();
        serve(
            &mut pending,
            &mut Pace::new(t0),
            t0,
            || now.get(),
            |_| {
                now.set(NOW_MS + read_ms); // the read takes this long
                Ok(reply_of(vec![rest_price("EUR_USD", NOW_MS - venue_age_ms, true)]))
            },
            |_, quote| ticks.push(quote),
        );
        ticks
    };
    let nine = seeded(9_000, 0);
    assert_eq!(nine.len(), 1, "9 s old at arrival seeds");
    assert_eq!(nine[0].local_ts - nine[0].ts, 9_000);
    assert!(seeded(11_000, 0).is_empty(), "11 s old at arrival does not");
    assert_eq!(seeded(8_000, 0).len(), 1, "8 s old and a quick read seeds");
    assert!(
        seeded(8_000, 3_000).is_empty(),
        "8 s old when the read began is 11 s old when it ended"
    );
    assert_eq!(seeded(6_000, 3_000).len(), 1, "6 s + a 3 s read is 9 s at arrival: seeds");
    let arrived = seeded(6_000, 3_000);
    assert_eq!(arrived[0].local_ts, NOW_MS + 3_000, "stamped at arrival, not at the start");
}

/// The bound of the one REST read is a GLOBAL timeout on the built agent (dial, request and reply),
/// inside the door's price wait.
#[test]
fn the_rest_read_is_bounded_to_three_seconds_globally() {
    assert_eq!(seed_agent().config().timeouts().global, Some(SEED_BOUND));
    assert_eq!(SEED_BOUND, Duration::from_secs(3));
    assert!(COLLECT < Duration::from_secs(1), "a burst's window must not eat the door's wait");
}

// --- the lane over a loopback OANDA: the real request, the real threads --------------------------

/// A loopback OANDA host: REST pricing answered by `rest` (given the request target), every pricing
/// STREAM open accepted and held UNANSWERED (no response head: the hung open of the evidence) until
/// the test answers or releases it.
struct Loopback {
    base: String,
    /// The head of every request, in arrival order.
    heads: Arc<Mutex<Vec<String>>>,
    held: Arc<Mutex<Vec<TcpStream>>>,
    released: Arc<AtomicBool>,
}

fn loopback(rest: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> Loopback {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local addr"));
    let server =
        Loopback { base, heads: Arc::default(), held: Arc::default(), released: Arc::default() };
    let (heads, held, released) =
        (Arc::clone(&server.heads), Arc::clone(&server.held), Arc::clone(&server.released));
    let rest = Arc::new(rest);
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let (heads, held, released, rest) =
                (heads.clone(), held.clone(), released.clone(), rest.clone());
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut byte = [0_u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if conn.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let target = head.split(' ').nth(1).unwrap_or_default().to_string();
                heads.lock().unwrap().push(head);
                if target.contains("/pricing/stream") {
                    if !released.load(Ordering::Relaxed) {
                        held.lock().unwrap().push(conn);
                    }
                    return;
                }
                let (status, body) = rest(&target);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(reply.as_bytes());
            });
        }
    });
    server
}

impl Loopback {
    /// The heads of the REST pricing requests (not the stream opens).
    fn pricing_requests(&self) -> Vec<String> {
        self.heads
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.contains("/pricing?") && !h.contains("/pricing/stream"))
            .cloned()
            .collect()
    }

    fn streams_held(&self) -> usize {
        self.held.lock().unwrap().len()
    }

    /// Answer every held stream open with a chunked 200 carrying this one line.
    fn stream_line(&self, line: &str) {
        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\n\
                    transfer-encoding: chunked\r\n\r\n";
        let chunk = format!("{:x}\r\n{line}\n\r\n", line.len() + 1);
        for stream in self.held.lock().unwrap().iter_mut() {
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(chunk.as_bytes());
            let _ = stream.flush();
        }
    }

    /// Close every held open and refuse (drop) later ones: the quote threads' dials fail at once.
    fn release(&self) {
        self.released.store(true, Ordering::Relaxed);
        self.held.lock().unwrap().clear();
    }
}

/// A REST host that answers the instruments the request names, each tradeable and 400 ms old.
fn echo_prices(target: &str) -> (u16, String) {
    let names = target.split("instruments=").nth(1).unwrap_or_default();
    let entries = names.split(',').map(|n| rest_price(n, now_ms() - 400, true)).collect();
    (200, reply_of(entries).to_string())
}

fn config_on(base: &str) -> OandaConfig {
    OandaConfig {
        api_token: "tok".to_string(),
        account_id: "acct-1".to_string(),
        rest_base: base.to_string(),
        stream_base: base.to_string(),
    }
}

/// Poll until `done` holds (10 s at most), failing with `what` rather than hanging.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A seed lane thread over `server`, fed by `line`; stop it with the returned flag.
fn run_lane(
    server: &Loopback,
    sink: &Arc<RecordingSink>,
) -> (std::sync::mpsc::Sender<Want>, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let (line, asks) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = FeedCtx {
        config: config_on(&server.base),
        sink: sink.clone(),
        status: Arc::default(),
        wake: Arc::new(|| {}),
        stop: Arc::clone(&stop),
    };
    (line, stop, std::thread::spawn(move || seed_main(asks, ctx)))
}

fn want(instrument: &str, series: &str) -> Want {
    Want { instrument: instrument.to_string(), series: series.to_string(), done: Arc::default() }
}

/// **THE EVIDENCE, end to end:** a new pair is subscribed through the real `Feeds`; its pricing
/// stream open hangs with no response head (the host that answers two opens in three with nothing);
/// the first tick arrives from the REST pricing endpoint inside the bound, with the request, auth and
/// label right, and the stream's own tick, when it does come, REPLACES the seed and is not followed by
/// a late one.
#[test]
fn a_new_pair_is_priced_from_rest_while_the_stream_open_hangs_and_the_stream_takes_over() {
    let server = loopback(echo_prices);
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink.clone(), || {}).with_config(config_on(&server.base));
    let started = Instant::now();
    feeds.subscribe_quotes("USDJPY").expect("subscribed");

    wait_until("the REST seed", || !sink.quotes().is_empty());
    assert!(
        started.elapsed() < SEED_BOUND + COLLECT * 2,
        "the first tick took {:?}: it must not ride the stream open",
        started.elapsed()
    );
    wait_until("the stream open to be dialled", || server.streams_held() == 1);
    let quotes = sink.quotes();
    assert_eq!(quotes.len(), 1, "exactly the seed while the stream is silent");
    let (venue, symbol, seed) = &quotes[0];
    assert_eq!((venue.as_str(), symbol.as_str()), ("oanda", "USDJPY"), "labelled like the stream");
    assert_eq!((seed.bid, seed.ask), (1.09, 1.0901));
    assert!(now_ms() - seed.ts < MAX_SEED_AGE_MS, "the venue's own time, and fresh");

    let requests = server.pricing_requests();
    assert_eq!(requests.len(), 1, "one read: {requests:?}");
    let head = requests[0].to_ascii_lowercase();
    assert!(head.starts_with("get /v3/accounts/acct-1/pricing?instruments=usd_jpy "), "{head}");
    assert!(head.contains("authorization: bearer tok"), "{head}");
    assert!(head.contains("accept-datetime-format: unix"), "{head}");

    // The stream finally answers: its tick is the next one, and nothing follows it.
    let stream_line = json!({
        "type": "PRICE", "time": format!("{}.000000000", now_ms() / 1000), "instrument": "USD_JPY",
        "bids": [{"price": "1.09500", "liquidity": 1}], "asks": [{"price": "1.09510", "liquidity": 1}],
    });
    server.stream_line(&stream_line.to_string());
    wait_until("the stream's tick", || sink.quotes().len() == 2);
    assert_eq!(sink.quotes()[1].2.bid, 1.095, "the stream's price replaces the seed");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(sink.quotes().len(), 2, "no late seed after the stream");

    server.release();
    feeds.shutdown();
}

/// **Many new pairs at once go out as ONE request** through the real lane thread: five asks queued
/// together are one `instruments=` list, and every pair is seeded under its own label.
#[test]
fn five_new_pairs_at_once_are_one_request_and_five_seeds() {
    let server = loopback(echo_prices);
    let sink = Arc::new(RecordingSink::default());
    let (line, stop, lane) = run_lane(&server, &sink);
    let pairs = [("EUR_USD", "EURUSD"), ("GBP_USD", "GBPUSD"), ("NZD_USD", "NZDUSD")];
    for (instrument, series) in pairs.iter().chain(&[("USD_JPY", "USDJPY"), ("AUD_USD", "AUDUSD")])
    {
        line.send(want(instrument, series)).unwrap();
    }
    wait_until("five seeds", || sink.quotes().len() == 5);
    let requests = server.pricing_requests();
    assert_eq!(requests.len(), 1, "one combined request, not five: {requests:?}");
    assert!(
        requests[0].contains("instruments=EUR_USD,GBP_USD,NZD_USD,USD_JPY,AUD_USD "),
        "{}",
        requests[0]
    );
    let mut labels: Vec<String> = sink.quotes().into_iter().map(|(_, s, _)| s).collect();
    labels.sort();
    assert_eq!(labels, ["AUDUSD", "EURUSD", "GBPUSD", "NZDUSD", "USDJPY"]);
    stop.store(true, Ordering::Relaxed);
    lane.join().unwrap();
}

/// **A REST error and a closed market seed nothing, and neither is retried**: one request each, no
/// tick, and asking the same pair again inside the floor makes no second request.
#[test]
fn a_rest_error_or_a_closed_market_seeds_nothing_and_is_not_retried() {
    let closed = |target: &str| {
        let names = target.split("instruments=").nth(1).unwrap_or_default();
        let entries = names.split(',').map(|n| rest_price(n, now_ms(), false)).collect();
        (200, reply_of(entries).to_string())
    };
    type RestScript = Box<dyn Fn(&str) -> (u16, String) + Send + Sync>;
    let failing: [(&str, RestScript); 2] = [
        ("HTTP 500", Box::new(|_| (500, r#"{"errorMessage":"boom"}"#.to_string()))),
        ("closed market", Box::new(closed)),
    ];
    for (name, rest) in failing {
        let server = loopback(rest);
        let sink = Arc::new(RecordingSink::default());
        let (line, stop, lane) = run_lane(&server, &sink);
        line.send(want("EUR_USD", "EURUSD")).unwrap();
        wait_until("the one request", || server.pricing_requests().len() == 1);
        line.send(want("EUR_USD", "EURUSD")).unwrap();
        std::thread::sleep(COLLECT * 3);
        assert_eq!(server.pricing_requests().len(), 1, "{name}: no retry inside the floor");
        assert!(sink.quotes().is_empty(), "{name}: nothing may be seeded");
        stop.store(true, Ordering::Relaxed);
        lane.join().unwrap();
    }
}

/// **A REST host that never answers is given up on at the bound** and feeds nothing; the lane is
/// back at its loop (a stop ends it at once) rather than held behind the dead read.
#[test]
fn a_rest_host_that_never_answers_is_abandoned_at_the_bound() {
    let server = loopback(|_| {
        std::thread::sleep(SEED_BOUND * 3);
        (200, reply_of(vec![]).to_string())
    });
    let sink = Arc::new(RecordingSink::default());
    let (line, stop, lane) = run_lane(&server, &sink);
    line.send(want("EUR_USD", "EURUSD")).unwrap();
    wait_until("the request", || server.pricing_requests().len() == 1);
    let asked = Instant::now();
    std::thread::sleep(SEED_BOUND + Duration::from_millis(500));
    stop.store(true, Ordering::Relaxed);
    lane.join().unwrap();
    assert!(asked.elapsed() < SEED_BOUND + Duration::from_secs(2), "{:?}", asked.elapsed());
    assert!(sink.quotes().is_empty(), "a read that timed out feeds nothing");
}
