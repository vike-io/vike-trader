//! Start, stop and the bind guard.
//!
//! The daemon holds sessions, so its stop (the flag the boot arms) must reach every connection: a
//! stream session writes its `Bye(ServerStopping)`, a running fetch is cancelled, the venue clients
//! are shut down, and `serve` returns only after the threads are joined - or after its drain budget,
//! with a connection stuck in a write to a peer that stopped reading cut at the socket.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_gatewayhub::server::bind::{BindRefusal, bind_loopback};
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::proto::{FetchRows, FetchWhat, Request, Response};
use vike_hub_proto::auth::Scope;
use vike_market_stream::wire::{ByeReason, StreamDown, StreamUp, SubOp};
use vike_marketdata::QuoteTick;

use harness::{Harness, test_keys};
use raw_peer::Raw;

#[test]
fn a_stop_reaches_every_connection_and_serve_returns_ok() {
    let mut h = Harness::start();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    h.rest.set_fetch(move |_, _, _, ctx| {
        let started = Instant::now();
        while !ctx.is_cancelled() && started.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if ctx.is_cancelled() {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(FetchRows::Bars(Vec::new()))
    });

    // A stream session holding a subscription...
    let mut stream = Raw::open(h.addr, &test_keys(), Scope::Read);
    stream.open_stream("binance");
    stream.up(&StreamUp::Sub {
        sid: 1,
        op: SubOp::Quotes { symbol: "BTCUSDT".into() },
        class: None,
    });
    assert!(matches!(stream.down(), StreamDown::SubOk { .. }));
    // ...a running fetch...
    let mut fetch = Raw::open(h.addr, &test_keys(), Scope::Write);
    fetch.send(&Request::Fetch {
        rid: 1,
        what: FetchWhat::Klines {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            start_ms: 0,
            end_ms: 1,
        },
    });
    assert!(matches!(fetch.recv::<Response>(), Response::FetchProgress(_)));
    // ...and an idle rpc connection.
    let mut idle = Raw::open(h.addr, &test_keys(), Scope::Read);

    let started = Instant::now();
    h.stop().expect("serve returned an error");
    assert!(started.elapsed() < Duration::from_secs(5), "the stop took {:?}", started.elapsed());

    let frames = stream.down_until(|f| matches!(f, StreamDown::Bye(_)));
    assert!(
        matches!(frames.last(), Some(StreamDown::Bye(ByeReason::ServerStopping))),
        "{frames:?}"
    );
    // The fetch is cancelled: told so, or cut (the daemon is gone either way).
    let session = h.data.session(0);
    assert!(session.shutdowns.load(Ordering::SeqCst) >= 1, "the venue client outlived the daemon");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !cancelled.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the lane never saw the stop as a cancel");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(idle.closed_within(Duration::from_secs(3)));
    // New connections find no listener.
    assert!(TcpStream::connect_timeout(&h.addr, Duration::from_millis(300)).is_err());
}

#[test]
fn a_consumer_that_stopped_reading_cannot_hold_the_stop_hostage() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.limits.queue_bytes = 128 * 1024;
    cfg.timings.stop_drain = Duration::from_secs(2);
    cfg.timings.write_timeout = Duration::from_secs(60);
    let mut h = Harness::start_config(cfg);
    let mut parked = Raw::open(h.addr, &test_keys(), Scope::Read);
    parked.open_stream("binance");
    parked.up(&StreamUp::Sub {
        sid: 1,
        op: SubOp::Quotes { symbol: "BTCUSDT".into() },
        class: None,
    });
    assert!(matches!(parked.down(), StreamDown::SubOk { .. }));
    let sink = h.data.session(0).sink.clone();
    // Fill the socket buffers until the writer is blocked in a write nobody will ever read.
    for ts in 0..4_000 {
        sink.quote(
            "binance",
            "BTCUSDT",
            QuoteTick {
                ts,
                local_ts: 0,
                bid: 1.0,
                ask: 2.0,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: format!("BTCUSDT{}", "x".repeat(8 * 1024)),
            },
        );
    }
    std::thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    h.stop().expect("serve returned an error");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "a writer blocked on a dead consumer held the stop for {:?}",
        started.elapsed()
    );
    drop(parked);
}

#[test]
fn a_stop_requested_before_serve_starts_still_ends_it() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let stop = Arc::new(AtomicBool::new(true));
    let lanes = vike_gatewayhub::server::lanes::Lanes {
        data: fake_lanes::FakeData::new(&["binance"]),
        rest: fake_lanes::FakeRest::new(harness::full_facts()),
    };
    let thread = std::thread::Builder::new()
        .name("test-early-stop".into())
        .spawn(move || {
            vike_gatewayhub::server::accept::serve(
                listener,
                test_keys(),
                lanes,
                ServerConfig::PRODUCTION,
                stop,
            )
        })
        .expect("spawn");
    let started = Instant::now();
    let result = thread.join().expect("join");
    assert!(result.is_ok(), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn the_bind_guard_refuses_every_non_loopback_address_and_opens_loopback() {
    for addr in ["0.0.0.0:0", "[::]:0", "192.0.2.1:7883"] {
        assert!(matches!(bind_loopback(addr), Err(BindRefusal::NotLoopback(_))), "{addr}");
    }
    let listener = bind_loopback("127.0.0.1:0").expect("loopback binds");
    assert!(listener.local_addr().expect("addr").ip().is_loopback());
}

#[test]
fn serve_refuses_a_public_listener_and_a_daemon_with_no_keys() {
    let lanes = || vike_gatewayhub::server::lanes::Lanes {
        data: fake_lanes::FakeData::new(&["binance"]),
        rest: fake_lanes::FakeRest::new(harness::full_facts()),
    };
    let public = TcpListener::bind("0.0.0.0:0").expect("wildcard binds");
    let err = vike_gatewayhub::server::accept::serve(
        public,
        test_keys(),
        lanes(),
        ServerConfig::PRODUCTION,
        Arc::new(AtomicBool::new(false)),
    )
    .expect_err("a public listener");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    let loopback = TcpListener::bind("127.0.0.1:0").expect("bind");
    let err = vike_gatewayhub::server::accept::serve(
        loopback,
        vike_hub_proto::auth::HubKeys::new(Vec::new(), Vec::new()),
        lanes(),
        ServerConfig::PRODUCTION,
        Arc::new(AtomicBool::new(false)),
    )
    .expect_err("no keys");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
