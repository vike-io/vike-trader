//! The ORDER hop, measured: what the gatewayhub's trade lane adds between a core's `submit` and the
//! venue adapter, and between the adapter's acknowledgement and the core (B.11, M1 of the wave-2 plan).
//!
//! A real gatewayhub server on loopback over a `ScriptedTrade` lane, the client crate's REAL `TradeLink`
//! and `RemoteExecClient` as the peer (real `serde_json` encode and decode, a real socket, the daemon's
//! reader, dedup, HALT gate, budget count and exec thread), and the scripted exec client standing in for
//! the venue. Every hop leg has an IN-PROCESS BASELINE over the same scripted client and the same payload
//! with no hop in it: a bare `ExecActor`-shaped channel (clone, `mpsc` send, a thread wake, the call). The
//! hop's cost is the paired difference, which the `HOP-LATENCY-DELTA` lines print.
//!
//! # Not a gate
//!
//! `#[ignore]`d and print-only, in the style of `tests/hop_latency.rs`: it asserts NOTHING about time (the
//! owner's ruling, D9: measure and publish, no CI ceiling; the targets are +250 us p99 for orders). The one
//! thing a short, non-ignored run asserts is that every sample of a small cell LANDED, so a rotted harness
//! is red; it asserts nothing about how long anything took.
//!
//! ```text
//! LANE=laneN just the CI box <branch> cargo test -p vike-gatewayhub --test order_hop --release \
//!     -- --ignored --nocapture
//! ```
//!
//! Numbers compare only on one quiet box (the coordinator's the latency box lane, `crates/vike-core/CLAUDE.md`'s
//! "Measuring a hot-path change honestly"); a shared box's numbers are a smoke.
//!
//! # The legs
//!
//! | leg | from -> to | baseline |
//! |---|---|---|
//! | `submit_to_adapter` | `RemoteExecClient::submit` -> the scripted adapter's `submit` is entered | `submit_to_adapter_direct` |
//! | `submit_to_cmdack` | a RAW peer writes `TradeUp::Submit` -> reads the daemon's `CmdAck` (and, from the same sample, `raw_submit_to_adapter`) | `submit_to_cmdack_direct` |
//! | `submit_to_accepted` | `submit` -> the adapter answers `OrderAccepted` -> `Ingest::GatewayEvent` in the core's channel (both hops) | `submit_to_accepted_direct` |
//!
//! Shapes: paced (the mean rate of an order path, one in flight) and a 16-deep burst. The raw ack leg is
//! paced only. `emit_*` in the line is what the sending call itself cost (`submit` is an enqueue on the
//! fold thread: that figure is the one the core-hop latency gate protects).
//!
//! Each cell is a fresh gateway and session, a warm-up that is discarded, then thousands of samples.
#![cfg(feature = "trade")]

use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use vike_exec::{ExecutionClient, Ingest};
use vike_gatewayhub::testkit::trade::{ScriptedAdapter, ScriptedTrade};
use vike_gatewayhub::trade::lane::{TradeLane, TradeTarget};
use vike_gatewayhub_client::proto::{AdapterReply, Request, Response, TradeDown, TradeUp};
use vike_gatewayhub_client::trade::TradeLink;
use vike_hub_proto::auth::Scope;
use vike_model::AccountTier;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/hop_stats.rs"]
mod hop_stats;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/raw_peer.rs"]
mod raw_peer;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use hop_stats::{Run, Shape, Spin, Stamps, box_line, delta_line, drive, line, report};
use raw_peer::Raw;
use trade_rig::{account, fast_trade_timings, order, rig, start_args};
use vike_gatewayhub::testkit::gateway::trade_keys;

/// A fresh order whose `ts` carries its sample index (the field the adapter's hook reads back).
fn payload(index: usize) -> OrderRequest {
    OrderRequest { client_order_id: format!("hop{index}"), ts: index as i64, ..order("unused") }
}

fn payloads(base: usize, count: usize) -> Vec<OrderRequest> {
    (base..base + count).map(payload).collect()
}

/// What the adapter does when a submit reaches it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hook {
    /// Stamp the arrival.
    Arrive,
    /// Answer `OrderAccepted` on the adapter's event lane (the venue's ack); the receiver stamps it.
    Accept,
}

/// Install `hook` on `adapter`, stamping into `stamps` (sample `i` is index `i`, base 0).
fn install(adapter: &ScriptedAdapter, hook: Hook, stamps: &Arc<Stamps>) {
    let stamps = Arc::clone(stamps);
    adapter.on_submit(move |request, events| match hook {
        Hook::Arrive => stamps.land(request.ts as usize),
        Hook::Accept => {
            let _ = events.blocking_send(Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: None,
                ts: request.ts,
            }));
        }
    });
}

/// The core's side of an `Accept` cell: stamp each `OrderAccepted` that reaches the ingest channel, over
/// the hop (`GatewayEvent`) or direct (`Event`).
fn stamp_accepted(stamps: &Arc<Stamps>) -> impl FnMut(Ingest) + Send + 'static {
    let stamps = Arc::clone(stamps);
    move |ingest| {
        if let Some(Event::OrderAccepted(a)) = ingest.venue_event() {
            stamps.land(a.ts as usize);
        }
    }
}

/// The HOP legs `submit_to_adapter` (`Hook::Arrive`) and `submit_to_accepted` (`Hook::Accept`): a real
/// lease, `RemoteExecClient::submit` as the sender.
fn hop_submit(hook: Hook, shape: Shape, count: usize, warmup: usize) -> Run {
    let stamps = Arc::new(Stamps::new(count));
    // Declared first, dropped last: the server outlives the client that talks to it.
    let rig = rig();
    let (events, mut rx) = vike_exec::event_channel(4096);
    let link = TradeLink::new(rig.gw.trade_link(), account("hop-orders"), events.routed("binance"))
        .with_timings(fast_trade_timings());
    let session = link.open().expect("the lease opens");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Live { .. }
    ));
    let adapter = rig.lane.adapter("binance", None).expect("the adapter was started");
    install(&adapter, hook, &stamps);
    // The core's ingest. An `Arrive` cell expects nothing on it but still drains it.
    let receiver = Spin::start("hop-core-rx", move || rx.try_recv().ok(), stamp_accepted(&stamps));
    let mut exec = session.exec_client();
    let run = drive(shape, 0, warmup, &stamps, payloads(0, count), |o| exec.submit(&o));
    drop(receiver);
    drop(exec);
    drop(session);
    run
}

/// The BASELINE legs: the same scripted exec client behind a bare `ExecActor`-shaped channel, no socket.
fn direct_submit(hook: Hook, shape: Shape, count: usize, warmup: usize) -> Run {
    let stamps = Arc::new(Stamps::new(count));
    let lane = ScriptedTrade::new(&["binance"]);
    let (events, mut rx) = vike_exec::event_channel(4096);
    let target = TradeTarget { venue: "binance".into(), account: None, tier: AccountTier::Demo };
    let started = lane
        .start_adapter(&target, &start_args(), events.routed("binance"), None)
        .expect("the scripted adapter starts");
    let adapter = lane.adapter("binance", None).expect("the adapter was started");
    install(&adapter, hook, &stamps);
    let receiver = Spin::start("hop-core-rx", move || rx.try_recv().ok(), stamp_accepted(&stamps));
    let (tx, queue) = channel::<OrderRequest>();
    let mut client = started.client;
    let exec = std::thread::Builder::new()
        .name("hop-direct-exec".into())
        .spawn(move || {
            while let Ok(request) = queue.recv() {
                client.submit(&request);
            }
        })
        .expect("spawn the direct exec thread");
    // `ExecActor::submit` clones the request into its channel; so does the hop's client.
    let run = drive(shape, 0, warmup, &stamps, payloads(0, count), |o| {
        let _ = tx.send(o.clone());
    });
    drop(tx);
    let _ = exec.join();
    drop(receiver);
    run
}

/// Open a raw lease on the rig's gateway and start the adapter; the connection.
fn raw_lease(rig: &trade_rig::Rig) -> Raw {
    let mut raw = Raw::open(rig.gw.addr(), &trade_keys(), Scope::Trade);
    raw.send(&Request::TradeOpen {
        client_id: "hop-raw".to_string(),
        venue: "binance".to_string(),
        account: None,
        tier: AccountTier::Demo,
        resume: None,
        takeover: false,
    });
    match raw.recv::<Response>() {
        Response::TradeOpened(_) => {}
        other => panic!("{other:?}"),
    }
    let mut start = start_args();
    start.rid = 1;
    start.route_key = "binance".into();
    raw.send(&TradeUp::AdapterStart(start));
    loop {
        match raw.recv::<TradeDown>() {
            TradeDown::AdapterReply { rid: 1, .. } => return raw,
            TradeDown::Heartbeat => {}
            other => panic!("{other:?}"),
        }
    }
}

/// The RAW ack leg: write `TradeUp::Submit`, read the daemon's `CmdAck`. Paced. Two runs from the same
/// samples: the ack, and the adapter's arrival (`raw_submit_to_adapter`).
fn raw_ack(count: usize, warmup: usize) -> (Run, Run) {
    let acked = Stamps::new(count);
    let arrived = Arc::new(Stamps::new(count));
    let rig = rig();
    let mut raw = raw_lease(&rig);
    let adapter = rig.lane.adapter("binance", None).expect("the adapter was started");
    install(&adapter, Hook::Arrive, &arrived);
    let mut emit = Vec::with_capacity(count);
    for k in 0..count {
        let order = payload(k);
        // Two `Stamps`, two bases: each run is internally consistent, and both are stamped now.
        acked.stamp_sent(k);
        arrived.stamp_sent(k);
        let started = Instant::now();
        raw.send(&TradeUp::Submit { cseq: k as u64 + 1, order });
        if k >= warmup {
            emit.push(started.elapsed().as_nanos() as u64);
        }
        loop {
            match raw.recv::<TradeDown>() {
                TradeDown::CmdAck { cseq } if cseq == k as u64 + 1 => break,
                TradeDown::Heartbeat | TradeDown::CmdAck { .. } => {}
                other => panic!("{other:?}"),
            }
        }
        acked.land(k);
        // The ack may precede the adapter call; wait for it (bounded) so the sample is whole.
        let waited = Instant::now();
        while !arrived.has_landed(k) && waited.elapsed() < Duration::from_secs(2) {
            std::hint::spin_loop();
        }
        std::thread::sleep(hop_stats::GAP);
    }
    let latency = |stamps: &Stamps| -> Vec<u64> {
        (warmup..count).filter_map(|k| stamps.latency(k)).collect()
    };
    let lost = |stamps: &Stamps| (warmup..count).filter(|&k| stamps.latency(k).is_none()).count();
    (
        Run { latency: latency(&acked), emit: emit.clone(), lost: lost(&acked), cut: None },
        Run { latency: latency(&arrived), emit, lost: lost(&arrived), cut: None },
    )
}

/// The BASELINE of the ack leg: a request queue to a thread that answers on a second queue.
fn direct_ack(count: usize, warmup: usize) -> Run {
    let acked = Stamps::new(count);
    let lane = ScriptedTrade::new(&["binance"]);
    let (events, _rx) = vike_exec::event_channel(16);
    let target = TradeTarget { venue: "binance".into(), account: None, tier: AccountTier::Demo };
    let started = lane
        .start_adapter(&target, &start_args(), events, None)
        .expect("the scripted adapter starts");
    let (tx, queue) = channel::<OrderRequest>();
    let (ack_tx, ack_rx) = channel::<()>();
    let mut client = started.client;
    let exec = std::thread::Builder::new()
        .name("hop-direct-ack".into())
        .spawn(move || {
            while let Ok(request) = queue.recv() {
                // The daemon acks when the command is QUEUED, before the adapter is entered.
                let _ = ack_tx.send(());
                client.submit(&request);
            }
        })
        .expect("spawn the direct ack thread");
    let mut emit = Vec::with_capacity(count);
    for k in 0..count {
        let order = payload(k);
        acked.stamp_sent(k);
        let started = Instant::now();
        let _ = tx.send(order);
        if k >= warmup {
            emit.push(started.elapsed().as_nanos() as u64);
        }
        ack_rx.recv().expect("the direct thread answers");
        acked.land(k);
        std::thread::sleep(hop_stats::GAP);
    }
    drop(tx);
    let _ = exec.join();
    Run {
        latency: (warmup..count).filter_map(|k| acked.latency(k)).collect(),
        emit,
        lost: 0,
        cut: None,
    }
}

/// `(shape, samples)` of a submit cell: a paced mean-rate cell and a 16-deep burst.
const SUBMIT_SHAPES: [(Shape, usize); 2] = [(Shape::Paced, 5_000), (Shape::Burst(16), 4_000)];

/// **The measurement.** Every leg against its baseline; one line per cell and a delta line per pair.
#[test]
#[ignore = "a measurement, not a gate: run on a quiet lane, `--ignored --nocapture`, and quote the numbers"]
fn the_order_hop_from_submit_to_adapter_ack_and_event() {
    report(&box_line("before"));
    for (leg, hook) in [("submit_to_adapter", Hook::Arrive), ("submit_to_accepted", Hook::Accept)] {
        for (shape, count) in SUBMIT_SHAPES {
            let warmup = count / 20 + 8;
            let hop = hop_submit(hook, shape, count, warmup);
            let base = direct_submit(hook, shape, count, warmup);
            let baseline = format!("{leg}_direct");
            report(&line(leg, shape, &hop));
            report(&line(&baseline, shape, &base));
            report(&delta_line(leg, &baseline, shape, &hop, &base));
        }
    }
    let (count, warmup) = (5_000, 258);
    let (ack, adapter) = raw_ack(count, warmup);
    let base = direct_ack(count, warmup);
    report(&line("submit_to_cmdack", Shape::Paced, &ack));
    report(&line("raw_submit_to_adapter", Shape::Paced, &adapter));
    report(&line("submit_to_cmdack_direct", Shape::Paced, &base));
    report(&delta_line("submit_to_cmdack", "submit_to_cmdack_direct", Shape::Paced, &ack, &base));
    report(&box_line("after"));
}

/// The harness's own plumbing, on a short run that rides every PR: every sample of a small cell of every
/// leg LANDED, and the lines carry their fields. Asserts nothing about how long anything took.
#[test]
fn the_harness_lands_every_sample_of_a_short_run() {
    vike_log::test_init();
    for hook in [Hook::Arrive, Hook::Accept] {
        for shape in [Shape::Paced, Shape::Burst(4)] {
            let (count, warmup) = (24, 4);
            for run in
                [hop_submit(hook, shape, count, warmup), direct_submit(hook, shape, count, warmup)]
            {
                assert_eq!(run.lost, 0, "samples never landed");
                assert_eq!(run.cut, None);
                assert_eq!(run.latency.len(), count - warmup);
                assert_eq!(run.emit.len(), count - warmup);
                let text = line("leg", shape, &run);
                for field in [
                    "HOP-LATENCY leg=leg",
                    "n=20",
                    "lost=0",
                    "p50_ns=",
                    "p99_ns=",
                    "p999_ns=",
                    "max_ns=",
                ] {
                    assert!(text.contains(field), "the line lacks `{field}`: {text}");
                }
            }
        }
    }
    let (ack, adapter) = raw_ack(20, 4);
    assert_eq!((ack.lost, adapter.lost), (0, 0));
    assert_eq!((ack.latency.len(), adapter.latency.len()), (16, 16));
    let base = direct_ack(20, 4);
    assert_eq!(base.latency.len(), 16);
    let delta = delta_line("a", "b", Shape::Paced, &ack, &base);
    assert!(
        delta.starts_with("HOP-LATENCY-DELTA leg=a baseline=b shape=paced dp50_ns="),
        "{delta}"
    );
}
