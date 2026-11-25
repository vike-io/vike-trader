//! Dukascopy's price stream behind the feed broker (decision 0193): the REAL broker over the REAL
//! `DataClientBus` and `QuoteBus`, with a recording pipe where the sidecar's stdin would be. The sidecar is a
//! child of the gatewayhub's trade lane: nothing here may start one, or take the bus's one listener twice.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use vike_data::DataClientBuilder;

use vike_bridge_core::data_client::bus::DataClientBus;
use vike_data::{DataClient, DataClientSink, RecordingSink};
use vike_datahub::feeds::venues::sharing_for;
use vike_datahub::feeds::{
    DataClientBrokerHandle, DataClientBrokerHolder, DataClientBrokerSharing, FeedBroker,
};
use vike_dukascopy::feed::{FEATURE_TICKS, LinePort, QuoteBus};
use vike_dukascopy::test_support::Wire;
use vike_gatewayhub_client::credentialed_limits::DUKASCOPY;
use vike_log::capture::captured;
use vike_log::poll::wait_for;

const VENUE: &str = "dukascopy";
const WAIT_FOR: Duration = Duration::from_secs(10);

struct Rig {
    broker: Arc<FeedBroker>,
    bus: Arc<QuoteBus>,
    wire: Arc<Wire>,
    sidecar: u64,
    /// How many clients the broker built, each a `QuoteBus::listen`.
    built: Arc<AtomicUsize>,
    strat: Arc<RecordingSink>,
    rec: Arc<RecordingSink>,
}

impl Rig {
    fn new(sharing: fn(&str) -> DataClientBrokerSharing) -> Self {
        // The stream subscribes a USER name on its kept row's wire name (decision 0290): the bridge's fixture list.
        vike_dukascopy::test_support::hold_kept_list_for_tests();
        let bus = QuoteBus::new();
        let wire = Arc::new(Wire::default());
        let sidecar =
            bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &[FEATURE_TICKS.to_string()]);
        let built = Arc::new(AtomicUsize::new(0));
        let (for_table, counted) = (Arc::clone(&bus), Arc::clone(&built));
        // The return type is spelled for `bare_broker`'s reason in `feed_broker.rs`.
        let table: DataClientBuilder = Box::new(
            move |_: &str,
                  sink: Arc<dyn DataClientSink>|
                  -> Result<Box<dyn DataClient + Send>, String> {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(DataClientBus::on(&for_table, sink)))
            },
        );
        let broker = FeedBroker::new(table, sharing);
        let (strat, rec) = (Arc::new(RecordingSink::default()), Arc::new(RecordingSink::default()));
        broker.attach_sink(
            DataClientBrokerHolder::Strategy,
            Arc::clone(&strat) as Arc<dyn DataClientSink>,
        );
        broker
            .attach_sink(DataClientBrokerHolder::Rec, Arc::clone(&rec) as Arc<dyn DataClientSink>);
        Self { broker, bus, wire, sidecar, built, strat, rec }
    }

    fn holders(&self) -> (DataClientBrokerHandle, DataClientBrokerHandle) {
        (
            self.broker.handle(DataClientBrokerHolder::Strategy, VENUE),
            self.broker.handle(DataClientBrokerHolder::Rec, VENUE),
        )
    }

    /// A current two-sided tick of `symbol`, from the sidecar.
    fn tick(&self, symbol: &str) {
        let now = vike_model::now_ms();
        self.bus.deliver(symbol, 1.1, 1.1002, now, now);
    }
}

fn subscribe(symbol: &str) -> String {
    format!("{{\"command\":\"subscribe\",\"symbols\":[\"{symbol}\"]}}")
}

fn unsubscribe(symbol: &str) -> String {
    format!("{{\"command\":\"unsubscribe\",\"symbols\":[\"{symbol}\"]}}")
}

/// **The strategy lane and the recorder behind the `Shared` row are ONE client, ONE listener and ONE
/// `subscribe`**, and the sidecar hears of a key only when its last holder leaves. The takeover warning is
/// the witness of a second listener, and it never fires.
#[test]
fn two_holders_are_one_listener_and_one_subscribe_and_only_the_last_release_reaches_the_sidecar() {
    assert_eq!(sharing_for(VENUE), DataClientBrokerSharing::Shared);
    let rig = Rig::new(sharing_for);
    let (mut strat, mut rec) = rig.holders();

    let ((held_by_strategy, held_by_rec), events) = captured(|| {
        (strat.subscribe_quotes("EURUSD").unwrap(), rec.subscribe_quotes("EURUSD").unwrap())
    });
    assert_eq!(rig.built.load(Ordering::SeqCst), 1, "one client, so one `listen`");
    assert_eq!(rig.wire.lines(), [subscribe("EURUSD")]);
    assert!(events.iter().all(|e| !e.message.contains("took over")), "{events:?}");

    rig.tick("EURUSD");
    wait_for(WAIT_FOR, "the tick at both holders", || {
        rig.strat.quotes().len() == 1 && rig.rec.quotes().len() == 1
    });

    rec.unsubscribe(held_by_rec);
    assert_eq!(
        rig.wire.lines().len(),
        1,
        "the strategy lane still holds it: the sidecar is not told"
    );
    rig.tick("EURUSD");
    wait_for(WAIT_FOR, "the strategy's next price", || rig.strat.quotes().len() == 2);
    assert_eq!(rig.rec.quotes().len(), 1, "the recorder takes nothing it let go of");

    let held_again = rec.subscribe_quotes("EURUSD").unwrap();
    strat.unsubscribe(held_by_strategy);
    assert_eq!(rig.wire.lines().len(), 1, "the recorder holds it now");
    rec.unsubscribe(held_again);
    assert_eq!(rig.wire.lines(), [subscribe("EURUSD"), unsubscribe("EURUSD")]);
    assert_eq!(rig.built.load(Ordering::SeqCst), 1);
}

/// **Why the row is a necessity.** With one client per holder the recorder's first subscribe builds a
/// second stream, which takes the bus: the strategy lane loses its prices and is refused from then on.
#[test]
fn one_client_per_holder_would_take_the_strategys_prices_away() {
    let rig = Rig::new(|_| DataClientBrokerSharing::PerHolder);
    let (mut strat, mut rec) = rig.holders();
    strat.subscribe_quotes("EURUSD").unwrap();

    let (_, events) = captured(|| rec.subscribe_quotes("EURUSD").unwrap());
    assert_eq!(rig.built.load(Ordering::SeqCst), 2);
    assert!(events.iter().any(|e| e.message.contains("took over")), "{events:?}");

    rig.tick("EURUSD");
    wait_for(WAIT_FOR, "the newer stream's quote", || rig.rec.quotes().len() == 1);
    assert!(rig.strat.quotes().is_empty(), "the strategy's prices are gone");
    let why = strat.subscribe_quotes("GBPUSD").expect_err("replaced").to_string();
    assert!(why.contains("replaced"), "{why}");
}

/// **A dead sidecar is silence for the recorder, and the next sidecar resumes it.** No row and no status is
/// made up while it is down, a held key is still joined, a new key is refused in words and is no cap, the
/// next sidecar is sent the held key once, and the stream is not labelled a second time.
#[test]
fn the_recorder_gets_silence_from_a_dead_sidecar_and_the_next_one_resumes_it() {
    let rig = Rig::new(sharing_for);
    let (mut strat, mut rec) = rig.holders();
    strat.subscribe_quotes("EURUSD").unwrap();
    rec.subscribe_quotes("EURUSD").unwrap();
    rig.tick("EURUSD");
    wait_for(WAIT_FOR, "the first row", || rig.rec.quotes().len() == 1);

    rig.bus.detach(rig.sidecar);
    assert!(rig.rec.stream_statuses().is_empty(), "the lane discloses no link state");
    rec.subscribe_quotes("EURUSD").expect("a held key is joined without asking the bus");
    let refused = strat.subscribe_quotes("GBPUSD").expect_err("no sidecar");
    assert!(!matches!(refused, vike_data::DataClientError::Cap(_)), "not a cap: {refused:?}");
    let why = refused.to_string();
    assert!(why.contains("no JForex sidecar is running"), "{why}");
    assert_eq!(rig.built.load(Ordering::SeqCst), 1, "nothing was built, nothing was started");

    let next = Arc::new(Wire::default());
    rig.bus.attach(Arc::clone(&next) as Arc<dyn LinePort>, &[FEATURE_TICKS.to_string()]);
    assert_eq!(next.lines(), [subscribe("EURUSD")], "the held key, and not the refused one");
    rig.tick("EURUSD");
    wait_for(WAIT_FOR, "the row after the respawn", || rig.rec.quotes().len() == 2);
    let labels = rig.strat.calls().iter().filter(|c| c.starts_with("data_delay")).count();
    assert_eq!(labels, 1, "the stream is labelled once");
}

/// **A closed market records nothing, the tick the SDK may replay included.** A cached last tick is a
/// minute old by its own stamp, so the recorder never stores it; the first current tick is the first row.
#[test]
fn a_closed_market_records_nothing_and_a_cached_tick_is_never_a_row() {
    let rig = Rig::new(sharing_for);
    let (mut strat, mut rec) = rig.holders();
    strat.subscribe_quotes("EURUSD").unwrap();
    rec.subscribe_quotes("EURUSD").unwrap();

    let now = vike_model::now_ms();
    rig.bus.deliver("EURUSD", 1.1, 1.1002, now - 60_000, now);
    rig.tick("EURUSD");
    wait_for(WAIT_FOR, "the open's first tick", || rig.rec.quotes().len() == 1);
    let (_, _, row) = rig.rec.quotes().remove(0);
    assert!(
        row.timestamp >= now,
        "the row is the current tick, and not the cached one: {}",
        row.timestamp
    );
    assert_eq!(rig.strat.quotes().len(), 1, "the strategy lane was not sent it either");
}

/// **The ceiling counts both holders' keys, and the sidecar is never asked past it.** The row is the bridge's
/// own cap, which `crates/vike-gatewayhub/src/data/session_guard_bus_tests.rs` holds equal.
#[test]
fn the_ceiling_counts_both_holders_and_the_sidecar_is_not_asked_past_it() {
    let rig = Rig::new(sharing_for);
    let (mut strat, mut rec) = rig.holders();
    let pairs = ["EURUSD", "GBPUSD", "AUDUSD", "NZDUSD", "USDJPY", "USDCAD", "USDCHF", "EURGBP"];
    assert_eq!(pairs.len(), DUKASCOPY.max_streams);
    for (i, pair) in pairs.iter().enumerate() {
        let holder = if i % 2 == 0 { &mut strat } else { &mut rec };
        holder.subscribe_quotes(pair).unwrap_or_else(|e| panic!("{pair}: {e}"));
    }
    let refused = rec.subscribe_quotes("EURJPY").expect_err("past the ceiling");
    assert!(
        matches!(refused, vike_data::DataClientError::Cap(_)),
        "a cap, not a link: {refused:?}"
    );
    assert_eq!(rig.wire.lines().len(), pairs.len(), "the sidecar was not asked");
}
