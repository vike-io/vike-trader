//! The market-data HUB suite (§8 item 15) — driven over a scripted `DataClient` double, with no
//! network and no venue.
//!
//! # ⚠ It runs on the DEFAULT build, and that is a property worth stating
//!
//! `crate::md` is FEATURE-FREE (the per-venue arms that ARE gated live in
//! `feeds::venues::build_client`), so this file compiles and RUNS in the derived roster lane on every PR rather than only in the new
//! `live-feeds` lane. That matters because a suite behind a feature is a suite that reads green when
//! its lane is mis-spelled: `feature_lane_coverage.rs` asks "is it ever BUILT", not "are its tests
//! RUN".
//!
//! # Two structural properties the whole suite rests on
//!
//! 1. **The publish tick and the reconcile pass are CALLABLE FUNCTIONS**
//!    (`MdHub::publish_tick`, `MdHub::reconcile`), not threads with timers. Every property here is a
//!    statement about *what one tick produced*; against a free-running 100 ms thread each becomes
//!    sleep-and-hope, which on a loaded the CI box runner is the flake shape this repo has already paid
//!    for twice.
//! 2. **The reconcile clock is INJECTED** (`reconcile(now_ms)`, `SessionGuard::release_at`).
//!    `MD_LINGER` is 60 s; a suite that read the wall clock inside the hub could test it only by
//!    sleeping a minute or by not testing it.
//!
//! # ⚠ What this suite deliberately does NOT prove
//!
//! **No venue wire is exercised.** Every test runs over `ScriptedFeed`. Whether binance's
//! `subscribe_depth` actually produces what the hub assumes is proven by the live smokes and by
//! nothing here — §12.5 is the standing warning, where the binance depth series in the store turned
//! out to be a reconnect artefact nobody noticed for forty days, and a scripted double would have
//! reproduced the INTENDED cadence perfectly. A green here is "the hub is correct", never "the
//! market-data plane is verified".

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};
use vike_datahub::md::{MarketClientBuilder, MdHub};
use vike_datahub_client::market::{MdFrame, MdLane, MdSpec};
use vike_datahub_client::proto::Response;
use vike_model::BookLevel;

// ------------------------------------------------------------------------------------------------
// The double
// ------------------------------------------------------------------------------------------------

/// Every `DataClient` call the hub made, in order.
///
/// ⚠ ORDER matters and is asserted on, not just membership: §5.3's two-phase teardown claims
/// `begin_shutdown` comes BEFORE the first join, and the §0 teardown hazard is precisely a
/// `begin_shutdown` appearing where only per-key `unsubscribe`s belong.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Client(String),
    Depth(String, String),
    Book(String, String),
    Trades(String, String),
    Unsubscribe(String, u64),
    BeginShutdown(String),
    Shutdown(String),
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Call>>>);

impl Log {
    fn push(&self, c: Call) {
        self.0.lock().unwrap().push(c);
    }
    fn all(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
    fn count(&self, pred: impl Fn(&Call) -> bool) -> usize {
        self.all().iter().filter(|c| pred(c)).count()
    }
}

struct ScriptedFeed {
    venue: String,
    log: Log,
    next: Arc<AtomicU64>,
    /// When set, every `subscribe_*` fails — the "a failed venue subscribe leaves no phantom
    /// refcount and no phantom sub_id" case.
    fail: bool,
}

impl ScriptedFeed {
    fn issue(&mut self) -> Result<SubscriptionId, LiveDataError> {
        if self.fail {
            return Err(LiveDataError::Subscribe("scripted failure".into()));
        }
        Ok(SubscriptionId(self.next.fetch_add(1, Ordering::AcqRel)))
    }
}

impl DataClient for ScriptedFeed {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("scripted feed serves no bars"))
    }
    fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("scripted feed serves no quotes"))
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.log.push(Call::Trades(self.venue.clone(), symbol.into()));
        self.issue()
    }
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.log.push(Call::Book(self.venue.clone(), symbol.into()));
        self.issue()
    }
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.log.push(Call::Depth(self.venue.clone(), symbol.into()));
        self.issue()
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.log.push(Call::Unsubscribe(self.venue.clone(), id.0));
    }
    fn begin_shutdown(&mut self) {
        self.log.push(Call::BeginShutdown(self.venue.clone()));
    }
    fn shutdown(&mut self) {
        self.log.push(Call::Shutdown(self.venue.clone()));
    }
}

fn builder(log: Log, fail: bool) -> MarketClientBuilder {
    let next = Arc::new(AtomicU64::new(1));
    Box::new(move |venue: &str, _sink: Arc<dyn LiveDataSink>| {
        log.push(Call::Client(venue.to_string()));
        Ok(Box::new(ScriptedFeed {
            venue: venue.to_string(),
            log: log.clone(),
            next: Arc::clone(&next),
            fail,
        }) as Box<dyn DataClient + Send>)
    })
}

/// A hub over the double, serving the venues this suite names.
fn hub(log: &Log) -> Arc<MdHub> {
    MdHub::new(
        builder(log.clone(), false),
        vec!["binance".into(), "polymarket".into(), "bybit".into()],
    )
}

fn spec(venue: &str, symbol: &str, lane: MdLane) -> MdSpec {
    MdSpec { venue: venue.into(), symbol: symbol.into(), lane, depth_levels: None }
}

fn now() -> i64 {
    vike_model::now_ms()
}

/// Decode one framed mailbox payload back into the `MdFrame` it carries. The publisher writes
/// through the wire's own `write_frame`, so a length prefix rides in front.
fn decode(bytes: &[u8]) -> MdFrame {
    assert!(bytes.len() > 4, "a framed payload carries a 4-byte length prefix");
    let resp: Response = serde_json::from_slice(&bytes[4..]).expect("decode Response");
    match resp {
        Response::Md(f) => *f,
        other => panic!("a stream mailbox carries only Response::Md, got {other:?}"),
    }
}

/// Drain a session's mailbox into decoded frames, in delivery order, synthesizing the writer's
/// `TapeGap` exactly where `run_market_writer` would.
fn drain(mb: &vike_datahub::md::mailbox::Mailbox) -> Vec<MdFrame> {
    use vike_datahub::md::mailbox::Recv;
    let mut out = Vec::new();
    loop {
        match mb.recv_timeout(std::time::Duration::from_millis(1)) {
            Recv::Frame { bytes, owed_gap } => {
                if let Some((key, dropped, from_seq, to_seq)) = owed_gap {
                    out.push(MdFrame::TapeGap {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        dropped,
                        from_seq,
                        to_seq,
                    });
                }
                out.push(decode(&bytes));
            }
            _ => return out,
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The suite's own floor
// ------------------------------------------------------------------------------------------------

/// ⚠ **The floor that stops this suite decaying into checking nothing** — the
/// `graceful_stop_pin.rs` `the_pin_has_a_non_empty_input` pattern, which exists because three gates
/// in this repo turned out to be unable to fail at all. It asserts the harness can construct a hub,
/// acquire a key, drive a venue subscription and produce a frame; if THIS goes red, every "asserts
/// an absence" test below is meaningless rather than passing.
#[test]
fn the_suite_has_a_non_empty_input() {
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().expect("open a session");
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    h.acquire(g.id(), &s).expect("binance depth is servable");
    let r = h.reconcile(now());
    assert_eq!(r.started, 1, "the reconciler started the venue subscription: {r:?}");
    h.sink().l2_snapshot(
        "binance",
        "BTCUSDT.P",
        0.1,
        vec![BookLevel::new(1.0, 2.0)],
        vec![BookLevel::new(1.1, 3.0)],
        7,
    );
    let tick = h.publish_tick();
    assert_eq!(tick.frames_produced, 1, "one dirty key produced one frame: {tick:?}");
    assert_eq!(drain(g.mailbox()).len(), 1);
    g.release_at(now());
}

#[path = "md_hub/admission_and_caps.rs"]
mod admission_and_caps;
#[path = "md_hub/depth_and_ctrl_lane.rs"]
mod depth_and_ctrl_lane;
#[path = "md_hub/late_add.rs"]
mod late_add;
#[path = "md_hub/subscription_lifecycle.rs"]
mod subscription_lifecycle;
#[path = "md_hub/tape_and_attach.rs"]
mod tape_and_attach;
