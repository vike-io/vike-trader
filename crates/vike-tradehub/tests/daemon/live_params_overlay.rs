//! The STRUCTURED live-params read end-to-end (`FEATURE_STRATEGY_PARAMS`): a real core with a real
//! mounted `SpreadMaker` + the real [`vike_tradehub::server`], driven through the REAL client verb
//! ([`vike_tradehub_client::remote_handle::strategy_params`]) under `Scope::Observe`.
//!
//! Modelled on `settings_show.rs` beside this file — the same "advertise, then serve the real
//! thing" shape — because this verb has the same two halves: a capability a client negotiates on,
//! and an answer that must come from the LIVE process rather than from anything captured at boot.
//!
//! What is proven:
//! - **Advertisement:** `Welcome.features` carries `"strategy-params"`, so the client verb sends.
//! - **The addressing key is real:** each row carries the venue/symbol/interval a
//!   `WireCommand::UpdateParams` targets, read off the CORE's own mount rows — not parsed back out
//!   of the rendered `params` prefix, which is the string-parsing trap this read replaces.
//! - **The params are the LIVE ones:** after an `UpdateParams` lands, the row reports the NEW
//!   value. This is the whole defect: the daemon's process-static mount block is stale from the
//!   first re-tune onward, so a read-modify-write built on it silently reverts the first writer.
//! - **The round trip is exact:** the JSON the read returns deserializes straight back into the
//!   core's own `StrategyParams`, which is what lets a client patch one leaf and hand the rest
//!   back without understanding any of it.
//!
//! Grouped into the `daemon` binary: plain tests, no `#[ignore]`, no crate-level `#![cfg]`, and no
//! process-global mutation of any kind.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, CoreHandle, StrategyMount, spawn_core};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, ParamsUpdate, QuoteUpdate, RiskGate, RiskLimits,
};
use vike_model::{QuoteTick, StrategyParams};
use vike_tradehub::{publish, server};
use vike_tradehub_client::NodeKeys;
use vike_tradehub_client::proto::{
    FEATURE_STRATEGY_PARAMS, NODE_PROTO_VERSION, Request, Response, read_frame, write_frame,
};
use vike_tradehub_client::remote_handle::strategy_params;
use vike_tradehub_client::wire::{WireMountRow, WireNodeIdentity};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";
const OBSERVE_KEY: &[u8] = b"live-params-overlay-observe-key";

/// Poll `cond` for up to `secs` — the `observe_roundtrip.rs` helper's shape. A snapshot publish is
/// asynchronous to the tick that made it dirty, so nothing here may assume one has landed.
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// A real core with one mounted `SpreadMaker`, plus the real observe server over it. The publisher
/// is spawned WITH the process-static row the daemon's `main` would publish, so the overlay under
/// test is exercised in its production shape (a static base plus a live overlay) rather than in the
/// identity-fallback one.
fn node() -> (CoreHandle, SocketAddr) {
    let engine = ExecutionEngine::new(
        Account::new(10_000.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        // Records submits and emits nothing — this suite is about the READ verb, so the maker's
        // quotes must never become resting orders the snapshot has to carry.
        vike_exec::testing::RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: INTERVAL.into(),
            strategy: Box::new(vike_mm::SpreadMaker::new(1.0, 0.5)),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let identity = WireNodeIdentity {
        name: "live-params".into(),
        strategy: "spread_maker".into(),
        params: "qty=1 half_spread=0.5".into(),
        live: false,
        build: "test-build".into(),
    };
    // The BOOT block: an empty key and no typed params, exactly as `tradehub_cli`'s
    // `wire_mount_rows` builds it — the overlay is the only thing that fills either in.
    let rows = vec![WireMountRow {
        strategy: "spread_maker".into(),
        params: "qty=1 half_spread=0.5".into(),
        live: false,
        venue: String::new(),
        symbol: String::new(),
        interval: String::new(),
        typed_params: None,
    }];
    let publisher = publish::spawn_with_mounts(handle.snapshot_cell(), Some(identity), rows);
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), Vec::new());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            None,
            server::ControlLimitsConfig::default(),
            None,
            None,
        );
    });
    (handle, addr)
}

fn quote(ts: i64) -> QuoteUpdate {
    QuoteUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    }
}

/// Read the one mount row's typed params back as the core's own union, or `None` while the node
/// cannot yet answer. Everything the client has to do is here: fetch, take the row, deserialize.
fn read_params(addr: SocketAddr) -> Option<StrategyParams> {
    let status = strategy_params(addr, OBSERVE_KEY).ok()?;
    let typed = status.mounts.first()?.typed_params.clone()?;
    serde_json::from_value::<StrategyParams>(typed).ok()
}

/// THE gate: an operator re-tunes a running mount, and the structured read reports the NEW value.
///
/// Both halves are asserted on the same connection shape a client will use, because both are the
/// point — the addressing key makes the row targetable, and the typed params make it patchable.
/// Reading the boot block instead would answer `half_spread=0.5` forever.
#[test]
fn the_structured_read_reports_what_the_mount_holds_now() {
    let (handle, addr) = node();

    // Warm the mount so a snapshot carrying mount rows has been published at least once.
    handle.tick_sender().quote(quote(1)).expect("tick lane accepts a quote");
    assert!(
        wait_until(5, || !handle.snapshot().mounts.is_empty()),
        "the core must publish its mount rows before the read can overlay them"
    );

    // Pre-tune: the read already answers, with the maker's CONSTRUCTED tuning.
    let before = strategy_params(addr, OBSERVE_KEY).expect("advertised ⇒ served");
    let row = before.mounts.first().expect("one mounted strategy");
    assert_eq!(
        (row.venue.as_str(), row.symbol.as_str(), row.interval.as_str()),
        (VENUE, SYMBOL, INTERVAL),
        "the row carries the key an UpdateParams targets, off the CORE's own mount"
    );
    // ⚠ The ROUND TRIP is the contract, not the field layout: what the read returns deserializes
    // straight back into the core's own union, so a client patches a leaf and hands back bytes it
    // never had to understand.
    let Some(StrategyParams::SpreadMaker(p)) = read_params(addr) else {
        panic!("a mounted SpreadMaker publishes its own variant, as StrategyParams JSON");
    };
    assert_eq!(p.half_spread.to_bits(), 0.5_f64.to_bits(), "the maker's constructed tuning");

    // Re-tune on the real command lane, then read again.
    handle.send_command(Command::UpdateParams(Box::new(ParamsUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        params: StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
            half_spread: 1.25,
            ..p
        }),
    })));
    handle.tick_sender().quote(quote(2)).expect("tick lane accepts a quote");

    let widened = wait_until(5, || {
        matches!(
            read_params(addr),
            Some(StrategyParams::SpreadMaker(p)) if p.half_spread.to_bits() == 1.25_f64.to_bits()
        )
    });
    assert!(
        widened,
        "the read must report the RE-TUNED half_spread — the boot block would answer 0.5 forever"
    );

    handle.shutdown_and_join();
}

/// The feature is advertised in the REAL server's `Welcome`, UNCONDITIONALLY — driven at the frame
/// level so the assertion is on the advertisement itself, not on the client verb above it. The
/// `settings-show` argument: a node whose mounts all answer `None` is still answering honestly, and
/// a capability that flickered with a construction detail would be worse than one that always means
/// "this build can answer the question".
#[test]
fn the_welcome_advertises_strategy_params_without_a_version_bump() {
    let (handle, addr) = node();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { features, proto_version, .. } => {
            assert!(
                features.iter().any(|f| f == FEATURE_STRATEGY_PARAMS),
                "the node must advertise {FEATURE_STRATEGY_PARAMS}, got {features:?}"
            );
            // ...and it rode the feature list rather than the version, which is the whole reason
            // this is a capability at all: the version is folded into the signed auth MAC, so a
            // bump breaks the handshake against every running node.
            assert_eq!(proto_version, NODE_PROTO_VERSION, "the params read bumps no proto version");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    handle.shutdown_and_join();
}
