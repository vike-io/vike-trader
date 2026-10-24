//! The EVENT hop, measured: what the gatewayhub's trade lane adds between a venue adapter emitting an
//! event and the core's ingest channel receiving it (B.11, M2 of the wave-2 plan).
//!
//! The path under test: the adapter's `EventSender` (`blocking_send`, as a bridge pump calls it) -> the
//! daemon's forwarder, which stamps `seq` and appends to the per-account ring -> the lease writer, which
//! pulls from the ring and writes one frame (real `serde_json` encode) -> a loopback socket -> the client
//! crate's REAL reader (real decode) -> `EventSender::blocking_send_gateway` into the core's
//! `event_channel`, where it lands as `Ingest::GatewayEvent`. The receiver is a spin-waiting thread
//! standing for the core's ingest.
//!
//! The BASELINE of each leg is the same `ScriptedAdapter::emit` into a plain `event_channel` with no
//! gateway behind it (`Ingest::Event`, the path a mount takes today): same payload, same sending call.
//! The hop's cost is the paired difference, which the `HOP-LATENCY-DELTA` lines print.
//!
//! # Not a gate
//!
//! `#[ignore]`d and print-only, in the style of `tests/hop_latency.rs`: it asserts NOTHING about time (the
//! owner's ruling, D9: measure and publish, no CI ceiling; the target is +250 us p99 for events). A short,
//! non-ignored run asserts only that every sample of a small cell LANDED, so a rotted harness is red.
//!
//! ```text
//! LANE=laneN just the CI box <branch> cargo test -p vike-gatewayhub --test event_hop --release \
//!     -- --ignored --nocapture
//! ```
//!
//! Numbers compare only on one quiet box (the coordinator's the latency box lane, `crates/vike-core/CLAUDE.md`'s
//! "Measuring a hot-path change honestly"); a shared box's numbers are a smoke.
//!
//! # The legs and shapes
//!
//! Two payloads: `accepted` (the smallest lifecycle event) and `fill` (the largest steady event, a full
//! `FillEvent`). Shapes: paced (one in flight, the queue empty), a 16- and a 128-deep burst (a fill storm:
//! the k-th has up to k-1 frames ahead of it in the ring) and an open loop at 2,000 events a second.
//! `emit_*` in the line is what the adapter's sending call cost (the venue pump must never wait for the
//! hop).
#![cfg(feature = "trade")]

use std::sync::Arc;

use vike_exec::ExecutionClient;
use vike_gatewayhub::testkit::trade::ScriptedTrade;
use vike_gatewayhub::trade::lane::{TradeLane, TradeTarget};
use vike_gatewayhub_client::proto::AdapterReply;
use vike_gatewayhub_client::trade::TradeLink;
use vike_model::AccountTier;
use vike_model::events::{Event, FillEvent, LiquiditySide, OrderAccepted, PositionSide, TradeId};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/hop_stats.rs"]
mod hop_stats;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use hop_stats::{Run, Shape, Spin, Stamps, box_line, delta_line, drive, line, report};
use trade_rig::{account, fast_trade_timings, rig, start_args};

/// The event payloads measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Payload {
    Accepted,
    Fill,
}

const PAYLOADS: [Payload; 2] = [Payload::Accepted, Payload::Fill];

impl Payload {
    fn word(self) -> &'static str {
        match self {
            Payload::Accepted => "accepted",
            Payload::Fill => "fill",
        }
    }

    /// The event for sample `index`, which it carries in its `ts` (the field the receiver reads back).
    fn event(self, index: usize) -> Event {
        let ts = index as i64;
        let coid = format!("hop{index}");
        match self {
            Payload::Accepted => Event::OrderAccepted(OrderAccepted {
                client_order_id: coid,
                venue_order_id: Some("venue-1234567".into()),
                ts,
            }),
            Payload::Fill => Event::Fill(FillEvent {
                trade_id: TradeId::new(format!("t{index}")).expect("a non-empty id"),
                client_order_id: coid,
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                last_qty: 0.001 + index as f64 * 1e-9,
                last_px: 64_123.45,
                commission: 0.0123,
                commission_asset: "USDT".into(),
                liquidity_side: LiquiditySide::Taker,
                ts,
                mark_price: Some(64_123.5),
                position_side: PositionSide::Both,
            }),
        }
    }
}

/// The sample index an event carries, if it is one of ours.
fn index_of(event: &Event) -> Option<usize> {
    match event {
        Event::OrderAccepted(a) => Some(a.ts as usize),
        Event::Fill(f) => Some(f.ts as usize),
        _ => None,
    }
}

fn events(payload: Payload, count: usize) -> Vec<Event> {
    (0..count).map(|i| payload.event(i)).collect()
}

/// The HOP leg: a real lease on a real gateway; the adapter emits, the client delivers.
fn hop(payload: Payload, shape: Shape, count: usize, warmup: usize) -> Run {
    let stamps = Arc::new(Stamps::new(count));
    // Declared first, dropped last: the server outlives the client that talks to it.
    let rig = rig();
    let (tx, mut rx) = vike_exec::event_channel(4096);
    let link = TradeLink::new(rig.gw.trade_link(), account("hop-events"), tx.routed("binance"))
        .with_timings(fast_trade_timings());
    let session = link.open().expect("the lease opens");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Live { .. }
    ));
    let adapter = rig.lane.adapter("binance", None).expect("the adapter was started");
    let seen = Arc::clone(&stamps);
    let receiver = Spin::start(
        "hop-core-rx",
        move || rx.try_recv().ok(),
        move |ingest| {
            if let Some(i) = ingest.venue_event().and_then(index_of) {
                seen.land(i);
            }
        },
    );
    let run = drive(shape, 0, warmup, &stamps, events(payload, count), |e| {
        let _ = adapter.emit(e);
    });
    drop(receiver);
    drop(session);
    run
}

/// The BASELINE: the same adapter, the same `emit`, a plain `event_channel` and no gateway.
fn direct(payload: Payload, shape: Shape, count: usize, warmup: usize) -> Run {
    let stamps = Arc::new(Stamps::new(count));
    let lane = ScriptedTrade::new(&["binance"]);
    let (tx, mut rx) = vike_exec::event_channel(4096);
    let target = TradeTarget { venue: "binance".into(), account: None, tier: AccountTier::Demo };
    let mut started = lane
        .start_adapter(&target, &start_args(), tx.routed("binance"), None)
        .expect("the scripted adapter starts");
    let adapter = lane.adapter("binance", None).expect("the adapter was started");
    let seen = Arc::clone(&stamps);
    let receiver = Spin::start(
        "hop-core-rx",
        move || rx.try_recv().ok(),
        move |ingest| {
            if let Some(i) = ingest.venue_event().and_then(index_of) {
                seen.land(i);
            }
        },
    );
    let run = drive(shape, 0, warmup, &stamps, events(payload, count), |e| {
        let _ = adapter.emit(e);
    });
    drop(receiver);
    started.client.detach();
    run
}

/// `(shape, samples)` of a cell. A ring holds 20,000 events, so no cell here outruns it.
const SHAPES: [(Shape, usize); 4] = [
    (Shape::Paced, 5_000),
    (Shape::Burst(16), 4_000),
    (Shape::Burst(128), 4_096),
    (Shape::Rate(2_000), 6_000),
];

/// **The measurement.** Every payload in every shape against its baseline; one line per cell and a delta
/// line per pair.
#[test]
#[ignore = "a measurement, not a gate: run on a quiet lane, `--ignored --nocapture`, and quote the numbers"]
fn the_event_hop_from_the_adapter_to_the_cores_ingest() {
    report(&box_line("before"));
    for payload in PAYLOADS {
        for (shape, count) in SHAPES {
            let warmup = count / 20 + 8;
            let leg = format!("adapter_emit_to_ingest_{}", payload.word());
            let baseline = format!("{leg}_direct");
            let through = hop(payload, shape, count, warmup);
            let plain = direct(payload, shape, count, warmup);
            report(&line(&leg, shape, &through));
            report(&line(&baseline, shape, &plain));
            report(&delta_line(&leg, &baseline, shape, &through, &plain));
        }
    }
    report(&box_line("after"));
}

/// The harness's own plumbing, on a short run that rides every PR: every sample of a small cell of every
/// payload LANDED over the hop and direct, and the lines carry their fields. Asserts nothing about how
/// long anything took.
#[test]
fn the_harness_lands_every_sample_of_a_short_run() {
    vike_log::test_init();
    for payload in PAYLOADS {
        for shape in [Shape::Paced, Shape::Burst(4), Shape::Rate(1_000)] {
            let (count, warmup) = (24, 4);
            let (through, plain) =
                (hop(payload, shape, count, warmup), direct(payload, shape, count, warmup));
            for run in [&through, &plain] {
                assert_eq!(run.lost, 0, "{} samples never landed", payload.word());
                assert_eq!(run.cut, None);
                assert_eq!(run.latency.len(), count - warmup);
                assert_eq!(run.emit.len(), count - warmup);
                let text = line("leg", shape, run);
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
            let delta = delta_line("a", "b", shape, &through, &plain);
            assert!(delta.starts_with("HOP-LATENCY-DELTA leg=a baseline=b shape="), "{delta}");
        }
    }
}
