use super::*;
use std::io::Cursor;
use vike_data::NoopSink;

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
    assert_eq!(ev, vec![HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 301_000 }]);
    let (_, ev) = fold_pricing_line(hb, "eurusd", &mut h, 400_000);
    assert!(ev.is_empty(), "an open stale episode is not re-disclosed");
    // a fresh price closes the episode
    let (q, ev) = fold_pricing_line(&price_line("401.000"), "eurusd", &mut h, 401_000);
    assert!(q.is_some());
    assert_eq!(ev, vec![HealthEvent::Live { gap_started_ts_ms: Some(301_000) }]);
}

#[test]
fn the_first_line_after_a_gap_recovers_even_a_heartbeat() {
    let mut h = health(300_000);
    assert!(h.enter_gap(50).is_some());
    let (q, ev) = fold_pricing_line(r#"{"type":"HEARTBEAT","time":"1.0"}"#, "s", &mut h, 60);
    assert!(q.is_none());
    assert_eq!(ev, vec![HealthEvent::Live { gap_started_ts_ms: Some(50) }]);
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
    let vars: std::collections::HashMap<String, String> = [
        ("OANDA_DEMO_API_KEY".to_string(), "tok".to_string()),
        ("OANDA_DEMO_ACCOUNT_ID".to_string(), "acct-1".to_string()),
    ]
    .into();
    crate::config::load_oanda_config_from(vike_bridge_core::credentials::Environment::Demo, &vars)
        .unwrap()
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
        assert!(matches!(err, LiveDataError::Unsupported(_)), "{err}");
    }
}

#[test]
fn an_interval_without_a_granularity_is_refused_before_any_thread_exists() {
    let mut f = feeds().with_config(dummy_config());
    let err = f.subscribe_bars("eurusd", "7s").unwrap_err();
    assert!(matches!(err, LiveDataError::Subscribe(ref m) if m.contains("granularity")));
    f.shutdown();
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_noop() {
    let mut f = feeds();
    f.unsubscribe(SubscriptionId(41));
    f.begin_shutdown();
    f.shutdown();
}
