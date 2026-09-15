//! `backfill_wire::run_wire_backfill` driven end-to-end against SCRIPTED loopback servers — the
//! `crates/vike-datahub-client/tests/backfill_negotiation.rs` pattern (a hand-rolled fake speaks
//! the real frame protocol, because proving "nothing was sent" needs a server that can COUNT,
//! and proving the request stream needs one that can RECORD). Loopback + in-memory only — no
//! store on disk, no network, no GUI.
//!
//! What is pinned here, per split-plane REQ-9 (the GUI half):
//! - an ADVERTISING server receives one `Request::Backfill` per planned range, in plan order,
//!   and the report tallies jobs/rows the way the local arm tallies its collector calls;
//! - an UNADVERTISED (old / lean) server is refused CLIENT-side with ZERO frames sent after the
//!   handshake, and the rendered status says the server predates backfill;
//! - a SERVER-side refusal (the off-roster-venue answer) surfaces VERBATIM in the report and the
//!   rendered status — never a silent no-op;
//! - an unreachable datahub is a `ConnectFailed` report, not a panic or a hang.

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_app_core::backfill_plan::BackfillJob;
use vike_app_core::backfill_wire::{
    WireBackfillReport, render_wire_backfill_status, run_wire_backfill,
};
use vike_datahub_client::{
    BackfillDone, FEATURE_BACKFILL, PROTO_VERSION, Request, Response, read_frame, write_frame,
};

/// The `(venue, symbol, interval, start, end)` of one received `Request::Backfill` — what the
/// scripted server records for the request-stream assertions.
type ReceivedBackfill = (String, String, String, i64, i64);

/// Spawn a scripted server that answers the `Hello` with a `Welcome` advertising
/// [`FEATURE_BACKFILL`], then answers every `Request::Backfill` through `script` (its argument is
/// the 0-based arrival index), recording each one. Returns the address and the receiver the
/// recorded requests arrive on (one message per request, in order) once the client disconnects.
fn spawn_scripted_backfill_server(
    script: impl Fn(usize) -> Response + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<ReceivedBackfill>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("scripted server expected Hello, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: vec!["load_bars".to_string(), FEATURE_BACKFILL.to_string()],
                // A KEY-LESS server: no nonce on the wire at all (see the field's docs).
                nonce: None,
            },
        )
        .expect("scripted server writes Welcome");
        let mut received = Vec::new();
        while let Ok(request) = read_frame::<_, Request>(&mut stream) {
            match request {
                Request::Backfill { venue, symbol, interval, start, end } => {
                    let index = received.len();
                    received.push((venue, symbol, interval, start, end));
                    write_frame(&mut stream, &script(index)).expect("scripted server answers");
                }
                other => panic!("scripted server expected Backfill, got {other:?}"),
            }
        }
        tx.send(received).expect("report the recorded requests");
    });
    (addr, rx)
}

fn job(venue: &str, symbol: &str, interval: &str, ranges: Vec<(i64, i64)>) -> BackfillJob {
    BackfillJob {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        ranges,
    }
}

/// The happy path: every planned range becomes exactly one `Request::Backfill`, in plan order,
/// and the report tallies jobs (not ranges) and sums the server's per-range row counts.
#[test]
fn an_advertising_server_receives_every_planned_range_and_the_report_tallies_jobs_and_rows() {
    let (addr, rx) = spawn_scripted_backfill_server(|index| {
        Response::BackfillDone(BackfillDone {
            rows_written: (index as u64 + 1) * 10, // 10, 20, 30 — distinguishable per range
            first_ts: Some(0),
            last_ts: Some(1),
        })
    });

    let jobs = vec![
        job("binance", "BTCUSDT", "1m", vec![(100, 200), (400, 500)]),
        job("bybit", "ETHUSDT", "5m", vec![(0, 1_000)]),
    ];
    let report = run_wire_backfill(&addr.to_string(), &jobs, 7);

    assert_eq!(
        report,
        WireBackfillReport::Ran {
            ok: 2,
            failed: 0,
            skipped: 7,
            rows_written: 60, // 10 + 20 + 30
            first_error: None,
        }
    );
    let received = rx.recv().expect("scripted server reports what it saw");
    assert_eq!(
        received,
        vec![
            ("binance".into(), "BTCUSDT".into(), "1m".into(), 100, 200),
            ("binance".into(), "BTCUSDT".into(), "1m".into(), 400, 500),
            ("bybit".into(), "ETHUSDT".into(), "5m".into(), 0, 1_000),
        ]
    );
}

/// The old-server shape: a `Welcome` without [`FEATURE_BACKFILL`] is refused CLIENT-side —
/// zero frames after the handshake — and the rendered status tells the operator the server
/// predates backfill (rather than a per-job failure count that would read as a venue problem).
#[test]
fn an_unadvertised_server_is_refused_without_sending_and_the_status_says_it_predates_backfill() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel::<usize>();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("fake server expected Hello, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: vec!["backtest".to_string(), "load_bars".to_string()],
                // A KEY-LESS server: no nonce on the wire at all (see the field's docs).
                nonce: None,
            },
        )
        .expect("fake server writes Welcome");
        let mut frames_after_hello = 0usize;
        while read_frame::<_, Request>(&mut stream).is_ok() {
            frames_after_hello += 1;
        }
        tx.send(frames_after_hello).expect("report the count");
    });

    let jobs = vec![job("binance", "BTCUSDT", "1m", vec![(100, 200)])];
    let report = run_wire_backfill(&addr.to_string(), &jobs, 0);

    match &report {
        WireBackfillReport::ServerUnsupported { addr: reported, features } => {
            assert_eq!(reported, &addr.to_string());
            assert!(features.iter().any(|f| f == "backtest"), "advertised set kept: {features:?}");
        }
        other => panic!("expected ServerUnsupported, got {other:?}"),
    }
    assert_eq!(rx.recv().expect("count arrives"), 0, "nothing sent after the refused negotiation");
    let line = render_wire_backfill_status(&report);
    assert!(line.contains("predates backfill-on-demand"), "{line}");
}

/// A server-side refusal — here the off-roster-venue answer the real `backfill_verb` gives —
/// fails that job, leaves the others intact, and surfaces VERBATIM in the report and the line.
#[test]
fn a_server_side_venue_refusal_fails_that_job_and_surfaces_verbatim() {
    let refusal = "backfill: venue `deribit` has no collector in this build. \
                   Supported: [binance, bybit, okx]";
    let (addr, rx) = spawn_scripted_backfill_server(move |index| {
        if index == 0 {
            Response::Error(refusal.to_string())
        } else {
            Response::BackfillDone(BackfillDone {
                rows_written: 5,
                first_ts: Some(0),
                last_ts: Some(1),
            })
        }
    });

    let jobs = vec![
        job("deribit", "BTC-PERPETUAL", "1m", vec![(100, 200)]),
        job("binance", "BTCUSDT", "1m", vec![(100, 200)]),
    ];
    let report = run_wire_backfill(&addr.to_string(), &jobs, 0);

    match &report {
        WireBackfillReport::Ran { ok, failed, rows_written, first_error, .. } => {
            assert_eq!((*ok, *failed, *rows_written), (1, 1, 5));
            assert_eq!(first_error.as_deref(), Some(refusal), "the server's text, verbatim");
        }
        other => panic!("expected Ran, got {other:?}"),
    }
    let line = render_wire_backfill_status(&report);
    assert!(line.contains(refusal), "the refusal reaches the status slot: {line}");
    // Both jobs' requests were sent — the refusal did not strand the rest of the selection.
    assert_eq!(rx.recv().expect("recorded requests").len(), 2);
}

/// A dead address is a `ConnectFailed` report naming it — never a panic, never a hang.
#[test]
fn an_unreachable_datahub_reports_connect_failed() {
    // Bind then drop: the OS refuses connections to the just-freed port (racy in theory — another
    // process could claim it — but the standard idiom, and a false pass here would need a server
    // speaking our protocol to appear on it within the test's window).
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        listener.local_addr().expect("resolve assigned port")
    };

    let jobs = vec![job("binance", "BTCUSDT", "1m", vec![(100, 200)])];
    match run_wire_backfill(&addr.to_string(), &jobs, 0) {
        WireBackfillReport::ConnectFailed { addr: reported, .. } => {
            assert_eq!(reported, addr.to_string());
        }
        other => panic!("expected ConnectFailed, got {other:?}"),
    }
}
