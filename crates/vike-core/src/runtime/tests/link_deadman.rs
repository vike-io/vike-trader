//! Runtime wiring tests for the CONNECTION-state dead-man (M13) — the on-`CoreThread` half of the
//! feature (the pure latch is unit-tested in [`super::link_deadman`]). The sibling of
//! [`super::deadman_tests`], and deliberately the same shape: a synchronous `CoreThread` (no OS
//! thread) with a `RecordingClient` so the exact cancels a trip issued are observable, driven with
//! injected timestamps and zero wall-clock sleeps.
//!
//! What is covered here and NOT in the latch's own tests, because it needs an engine and a client:
//! (a) the feature-absent config builds nothing and the sweep is a no-op; (b) a trip on venue A
//! cancels A's resting orders and leaves B's untouched — the venue-SCOPED cancel; (c)
//! `CancelAllAndHalt` engages the in-process halt on EVERY engine and writes the one sentinel;
//! (d) a `Live` inside the grace is silent, over the real `Ingest::StreamStatus` dispatch arm;
//! (e) a weekend of `Stale` through that same arm cancels nothing; (f) an unarmed venue's link
//! death is dropped; (g) a trip on a venue that is NOT engine 0 cancels THAT engine's book — the
//! one claim every other test here would pass on a mis-route; (h) a venue with TWO ACCOUNTS has
//! BOTH books pulled, which the account-scoped `MassCancel` routing this trip used first could
//! not do; (i) the FIRST link death of a mount trips with no prior `Live` — the production
//! sequence, since no bridge here discloses `Live` on a first connect; (j) the same trip over the
//! REAL venue strings and the REAL `vike_venues::link_deadman_default` table — a disclosed binance
//! drop pulls the binance book while a mounted `oanda`'s book positively SURVIVES, and an oanda
//! disconnect on the same core pulls nothing at all.

use super::*;
use tracing_test::traced_test;
use vike_exec::testing::RecordingClient;
use vike_model::TradingState;
use vike_model::{FeedStatus, OrderRequest};

// The runtime's shared white-box assembly; `core_with_venues` below keeps its own two-venue engines
// and injected clock, which are this file's subject.
use crate::runtime::test_support::{core_of, engine_on};

/// The two venues every test here mounts: `sim` is the ARMED one, `other` is the sibling whose
/// book must survive `sim`'s link death.
const ARMED: &str = "sim";
const SIBLING: &str = "other";
const SYMBOL: &str = "BTCUSDT";

fn engine_for(venue: &str) -> ExecutionEngine<RecordingClient> {
    engine_on(venue, SYMBOL, RecordingClient::default())
}

/// The injected clock. ⚠ Load-bearing rather than tidiness: [`CoreThread::dispatch`] re-stamps
/// `engine.now_ms` from `config.clock` at the top of EVERY message, so the observe hook reads the
/// CLOCK's answer and not whatever a test wrote onto the engine — under the default `LiveClock` a
/// disconnect is stamped with a real epoch timestamp and no injected sweep time can ever be past
/// it. Measured: with the wall clock, `a_trip_cancels_only_the_venue_whose_link_died` records zero
/// cancels and the two acting tests fail while the three "must not trip" ones pass, i.e. the
/// harness reads green for the wrong reason. `AtomicI64` rather than `TestClock` because the
/// config's clock is `Box<dyn Clock + Send>` and the test needs a second handle to advance it.
type StepClock = Arc<std::sync::atomic::AtomicI64>;

/// Build a synchronous two-venue `CoreThread<RecordingClient>` with the given `CoreConfig` — the
/// `test_support::core_with` shape plus one EXTRA engine, which is what makes the venue-scoped
/// claim testable at all (a single-engine core cannot show that a sibling was spared) — and the
/// handle to its injected clock.
fn core_with(config: CoreConfig) -> (CoreThread<RecordingClient>, StepClock) {
    core_with_venues(config, ARMED, SIBLING)
}

/// [`core_with`] over NAMED venues — so one test can drive the same wiring with the venue strings a
/// real mount uses (`"binance"` beside `"oanda"`) and therefore fold the REAL
/// `vike_venues::link_deadman_default` table into the config rather than a stand-in.
fn core_with_venues(
    mut config: CoreConfig,
    primary: &str,
    sibling: &str,
) -> (CoreThread<RecordingClient>, StepClock) {
    let clock: StepClock = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let read = Arc::clone(&clock);
    config.clock = Box::new(move || read.load(std::sync::atomic::Ordering::Relaxed));
    let core = core_of(engine_for(primary), vec![(1.0, engine_for(sibling))], config);
    (core, clock)
}

fn link_cfg(
    grace_ms: u64,
    action: DeadManAction,
    halt_file: Option<std::path::PathBuf>,
) -> CoreConfig {
    CoreConfig {
        link_deadman: Some(LinkDeadManConfig {
            grace: std::time::Duration::from_millis(grace_ms),
            action,
            venues: [ARMED.to_string()].into_iter().collect(),
            halt_file,
        }),
        ..Default::default()
    }
}

/// Submit a resting order on `venue` under `coid` through the ONE order-write path; it registers
/// live (`RecordingClient` never acks, so it stays cancelable).
fn submit(c: &mut CoreThread<RecordingClient>, venue: &str, coid: &str) {
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: coid.into(),
            venue: venue.into(),
            symbol: SYMBOL.into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        })),
        0,
    );
}

/// Drive a feed-status transition through the REAL dispatch arm at simulated time `now`, so these
/// tests exercise the same path a bridge's `stream_status` disclosure takes rather than poking the
/// latch directly. Advancing the injected clock is what makes the timestamp `dispatch` stamps the
/// one the test means.
fn feed(
    c: &mut CoreThread<RecordingClient>,
    clock: &StepClock,
    venue: &str,
    status: FeedStatus,
    now: i64,
) {
    feed_stream(c, clock, venue, "depth", status, now);
}

/// [`feed`] under a NAMED stream label. The label decides the SOURCE of the disclosure
/// (`vike_venues::link_deadman::LinkSource::of`): every venue lane word is the venue's own socket and the
/// two reserved words are the hub links, so one test drives the label a real CEX tick pump stamps
/// (`"ticks"` — `crates/bridges/binance/src/family/depth.rs`'s `disclose_link`) and another the
/// trade link's: a latch that had grown a condition on the venue words would pass every other test
/// here and fail those.
fn feed_stream(
    c: &mut CoreThread<RecordingClient>,
    clock: &StepClock,
    venue: &str,
    stream: &str,
    status: FeedStatus,
    now: i64,
) {
    clock.store(now, std::sync::atomic::Ordering::Relaxed);
    c.dispatch(Ingest::StreamStatus(Box::new(StreamStatusUpdate {
        venue: venue.to_string(),
        symbol: SYMBOL.to_string(),
        stream: stream.to_string(),
        status,
    })));
}

/// (a) FEATURE ABSENT builds nothing. The default config constructs no latch, and the sweep is a
/// complete no-op even called directly — the byte-identical claim, asserted rather than assumed.
#[test]
fn an_absent_config_builds_no_latch_and_the_sweep_does_nothing() {
    let (mut c, clock) = core_with(CoreConfig::default());
    assert!(c.link_deadman.is_none(), "default config builds no link dead-man");
    submit(&mut c, ARMED, "c1");
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 0);
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(1_000_000_000);
    assert!(c.engine.client.cancels.is_empty(), "a disabled switch must never cancel anything");
    assert_eq!(c.engine.trading_state, TradingState::Active, "...and must never halt");
}

/// (b) ⚠ **The scoping claim.** A link death on venue A cancels A's resting orders and leaves B's
/// alone. This is the one behaviour that distinguishes this switch from the silence one, whose
/// trip is a core-wide `MassCancel { venue: None }`.
#[test]
fn a_trip_cancels_only_the_venue_whose_link_died() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    submit(&mut c, ARMED, "a2");
    submit(&mut c, SIBLING, "b1");

    feed(&mut c, &clock, ARMED, FeedStatus::Live, 0);
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(500);
    assert!(c.engine.client.cancels.is_empty(), "still inside the grace ⇒ no cancel");

    c.sweep_link_deadman(2000);
    let mut cancelled = c.engine.client.cancels.clone();
    cancelled.sort();
    assert_eq!(cancelled, vec!["a1".to_string(), "a2".to_string()], "the dead venue's whole book");
    assert!(
        c.extra_engines[0].1.client.cancels.is_empty(),
        "the SIBLING venue's link is up — its book must survive, or this is the silence switch \
         with extra steps"
    );
    assert_eq!(c.engine.trading_state, TradingState::Active, "CancelAll does not engage HALT");

    c.sweep_link_deadman(3000);
    assert_eq!(c.engine.client.cancels.len(), 2, "no re-fire within the same outage");
}

/// (i) ⚠ **THE PRODUCTION SEQUENCE: the mount's very FIRST `StreamStatus` is the `Disconnected`,
/// and it trips.** Every other test here injects a `Live` first, which is convenient and is NOT
/// what a bridge does: `vike_bridge_core::stream_health`'s `StreamHealth::recover` returns `None`
/// when no gap is open, so a first successful connect discloses nothing and `Live` exists only to
/// CLOSE a gap. A latch gated on a prior `Live` was therefore blind to the first socket death of a
/// mount — the outage that never recovers, i.e. the one this switch exists for. Nothing else in
/// this file can fail when that regresses.
#[test]
fn the_first_link_death_of_a_mount_trips_with_no_prior_live() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    submit(&mut c, SIBLING, "b1");

    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0); // the FIRST status of the process
    c.sweep_link_deadman(500);
    assert!(c.engine.client.cancels.is_empty(), "still inside the grace");

    c.sweep_link_deadman(2000);
    assert_eq!(
        c.engine.client.cancels,
        vec!["a1".to_string()],
        "a socket that came up, died and never came back must still pull the book"
    );
    assert!(c.extra_engines[0].1.client.cancels.is_empty(), "...and only that venue's");
}

/// The source a tick pump's own `"ticks"` word names.
const VENUE_SOURCE: vike_venues::link_deadman::LinkSource =
    vike_venues::link_deadman::LinkSource::Venue;

/// The config of a core whose venues `price_only` are priced by a price-only feed (no mount): the
/// link dead-man hears `armed` (a price-only venue must be listed there too, or it is not heard).
fn price_only_cfg(action: DeadManAction, armed: &[&str], price_only: &[&str]) -> CoreConfig {
    let set = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect();
    CoreConfig {
        link_deadman: Some(LinkDeadManConfig {
            grace: std::time::Duration::from_millis(1000),
            action,
            venues: set(armed),
            halt_file: None,
        }),
        price_only: Some(PriceOnlyConfig {
            venues: set(price_only),
            sweep_interval: std::time::Duration::from_secs(30),
            max_price_age: std::time::Duration::from_secs(60),
            price_source: std::collections::BTreeMap::new(),
        }),
        ..Default::default()
    }
}

/// Book a non-flat position on `venue` in `c`'s primary engine (what a fill leaves).
fn hold_position(c: &mut CoreThread<RecordingClient>, venue: &str) {
    c.engine.account.positions.insert(
        (venue.into(), SYMBOL.into(), vike_model::events::PositionSide::Both),
        vike_exec::PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
    );
}

/// (k) ⚠ **A price-only venue is watched WHILE IT HOLDS A LIVE ORDER**: its pump's disclosed death
/// outlasts the grace and the venue's book is pulled and the core halted, as on a mounted venue.
#[test]
fn a_price_only_venue_with_a_resting_order_trips_on_a_dead_pump() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(500);
    assert!(c.engine.client.cancels.is_empty(), "inside the grace");
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.client.cancels, vec!["a1".to_string()], "the resting order is pulled");
    assert_eq!(c.engine.trading_state, TradingState::Halted, "CancelAllAndHalt engages HALT");
}

/// (k2) …and WHILE IT HOLDS AN OPEN POSITION, with no order resting: a position is marked off that
/// pump, so its death is the outage the switch exists for; there is nothing to cancel, and the HALT
/// keeps a new order from being placed on a venue with no live price.
#[test]
fn a_price_only_venue_with_an_open_position_trips_on_a_dead_pump() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    hold_position(&mut c, ARMED);
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(2000);
    assert!(c.link_deadman.as_ref().expect("armed").is_tripped(ARMED, SYMBOL));
    assert_eq!(c.engine.trading_state, TradingState::Halted);
}

/// (k2b) ⚠ **A PAST ADMISSION IS NOT A HOLDING** (the dead-man's blast radius): admitted symbols are
/// never released, so counting one kept a price-only venue armed for good with no position and no
/// order, and a hub outage past the grace halted the WHOLE node over a venue with nothing at risk. A
/// FLAT position row (the entry a closed position leaves behind) holds nothing either.
#[test]
fn an_admitted_symbol_alone_does_not_arm_a_price_only_venue() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    c.engine.extra_symbols.push("ETHUSDT".to_string());
    c.engine.account.positions.insert(
        (ARMED.into(), "ETHUSDT".into(), vike_model::events::PositionSide::Both),
        vike_exec::PositionEntry { size: 0.0, avg_px: 100.0, ..Default::default() },
    );
    assert!(!c.venue_holds(ARMED), "an admission and a flat row hold nothing");
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, "gateway-trade", FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(60_000);
    assert_eq!(c.engine.trading_state, TradingState::Active, "no position, no order, no halt");
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(!ldm.is_down(ARMED, SYMBOL, VENUE_SOURCE) && !ldm.is_tripped(ARMED, SYMBOL));
}

/// (k2c) ⚠ **A WINDOW IS NOT OPENED WHILE THE VENUE IS IDLE, so it cannot survive into the sweep that
/// finds the venue held**: the sweep-time discard alone let a window opened at t=0 (idle) trip at the
/// first sweep after an order was placed at t=900, with the link down less than the grace of anything
/// held. The disclosure is dropped at the observe hook; a later one, with the order resting, opens the
/// window and the grace runs from THERE.
#[test]
fn a_disconnect_disclosed_while_idle_cannot_trip_the_venue_once_something_rests() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);
    submit(&mut c, ARMED, "a1"); // before ANY sweep ran: the idle-time window used to survive this
    c.sweep_link_deadman(1_500);
    assert!(c.engine.client.cancels.is_empty(), "nothing watched the outage that began idle");
    assert_eq!(c.engine.trading_state, TradingState::Active);

    // The link is still down and is disclosed again while the order rests: now it is watched.
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 2_000);
    c.sweep_link_deadman(2_500);
    assert!(c.engine.client.cancels.is_empty(), "inside the grace of the held window");
    c.sweep_link_deadman(3_500);
    assert_eq!(c.engine.client.cancels, vec!["a1".to_string()]);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
}

/// (k3) ⚠ **With nothing admitted and nothing resting the venue is NOT armed**: the outage trips
/// nothing, halts nothing, and is FORGOTTEN, so an order placed afterwards does not inherit it.
#[test]
fn a_price_only_venue_holding_nothing_does_not_trip_and_forgets_the_outage() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Active, "nothing to protect, nothing halted");
    assert!(
        !c.link_deadman.as_ref().expect("armed").is_down(ARMED, SYMBOL, VENUE_SOURCE),
        "the idle venue's window is discarded, not deferred"
    );

    submit(&mut c, ARMED, "a1");
    c.sweep_link_deadman(9000);
    assert!(c.engine.client.cancels.is_empty(), "the old outage is not held against the new order");
    assert_eq!(c.engine.trading_state, TradingState::Active);
}

/// (k4) ⚠ **A MOUNTED venue beside it keeps every window it had**: the same sweep that discards the idle
/// price-only venue's window trips the mounted one, which holds nothing at all (a mounted venue is armed
/// whether or not anything rests on it, as before).
#[test]
fn a_mounted_venue_beside_a_price_only_one_is_not_touched() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAll, &[ARMED, SIBLING], &[ARMED]));
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, SIBLING, "ticks", FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(2000);
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(ldm.is_tripped(SIBLING, SYMBOL), "the mounted venue trips with nothing resting");
    assert!(!ldm.is_down(ARMED, SYMBOL, VENUE_SOURCE), "the idle price-only window is gone");
    assert!(!ldm.is_tripped(ARMED, SYMBOL));
}

/// (j) ⚠ **THE NEWLY-COVERED VENUES, end to end, over the REAL per-venue table and the REAL stream
/// label a CEX tick pump stamps.** Every other test in this file names two invented venues and
/// hand-builds the armed set, which proves the latch and proves nothing about whether a venue a
/// real daemon mounts can reach it. This one mounts `binance` beside `oanda`, folds the armed set
/// out of `vike_venues::link_deadman_default` exactly as `vike-tradehub`'s
/// `link_deadman_config_from_policy` does, and drives the disconnect under the `"ticks"` label the
/// pump actually sends.
///
/// The claim, in the direction that can silently go wrong: the CRYPTO venue's book is pulled and
/// the FX venue's book SURVIVES — asserted positively, because a switch that pulled both would look
/// identical to a working one from the tripping venue's side, and pulling an FX book on a crypto
/// link death is precisely the class of over-reach decision 0038 exists to refuse.
#[test]
fn a_disclosed_binance_drop_pulls_its_book_and_leaves_the_fx_venue_alone() {
    // The fold the composition root performs, over the REAL table: `binance` is Armed,
    // `oanda` is SessionBounded and must not join the set however the mount hears it.
    let venues: std::collections::BTreeSet<String> = ["binance", "oanda"]
        .into_iter()
        .filter(|v| vike_venues::link_deadman_default(v).is_on())
        .map(str::to_string)
        .collect();
    assert_eq!(
        venues,
        ["binance".to_string()].into_iter().collect::<std::collections::BTreeSet<_>>(),
        "the table itself must arm binance and refuse oanda — if this line moved, the rest of \
         this test is measuring something else"
    );

    let (mut c, clock) = core_with_venues(
        CoreConfig {
            link_deadman: Some(LinkDeadManConfig {
                grace: std::time::Duration::from_millis(1000),
                action: DeadManAction::CancelAll,
                venues,
                halt_file: None,
            }),
            ..Default::default()
        },
        "binance",
        "oanda",
    );
    submit(&mut c, "binance", "btc-bid");
    submit(&mut c, "binance", "btc-ask");
    submit(&mut c, "oanda", "eur-bid");

    // The production sequence for this lane: the pump's FIRST disclosure is the disconnect (it
    // emits `Live` only to close a gap), stamped with the label it really sends.
    feed_stream(&mut c, &clock, "binance", "ticks", FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(500);
    assert!(c.engine.client.cancels.is_empty(), "still inside the grace");

    c.sweep_link_deadman(2000);
    let mut cancelled = c.engine.client.cancels.clone();
    cancelled.sort();
    assert_eq!(
        cancelled,
        vec!["btc-ask".to_string(), "btc-bid".to_string()],
        "the dead venue's whole book is pulled"
    );
    assert!(
        c.extra_engines[0].1.client.cancels.is_empty(),
        "⚠ the FX venue's orders SURVIVE — its own link never died, and a crypto socket death may \
         not reach across venues"
    );

    // …and an oanda disconnect on the same core cancels NOTHING at all, because that venue is not
    // in the armed set: the FX half of the claim, in the other direction.
    feed_stream(&mut c, &clock, "oanda", "quotes", FeedStatus::Disconnected, 3000);
    c.sweep_link_deadman(1_000_000);
    assert!(
        c.extra_engines[0].1.client.cancels.is_empty(),
        "a session-bounded venue's disconnect must never pull a book — the whole re-ruling"
    );
}

/// (c) `CancelAllAndHalt` halts EVERY engine and writes the one cross-process sentinel — the
/// deliberate asymmetry: the cancel is per-venue, the halt is process-wide because the file is.
#[test]
fn cancel_all_and_halt_halts_every_engine_and_writes_the_sentinel() {
    // ⚠ `Scratch`, not `env::temp_dir()`: `crates/vike-ops/tests/hygiene/journal_scratch_gate.rs` allows
    // that call from ONE guard file in this crate, and the 211 GB of leaked `/tmp` directories that
    // gate was written for came from exactly this shape — a test that removes its own artefact on
    // the happy path and leaks it on every failing run. `reserved` hands back a path inside an
    // OWNED root that does not exist yet, which is what this test needs: the sweep must be the
    // thing that creates the sentinel.
    let scratch = crate::scratch::Scratch::reserved("link-deadman-halt");
    let path = scratch.path().to_path_buf();
    assert!(!path.exists(), "the sentinel must not exist before the trip");

    let (mut c, clock) =
        core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt, Some(path.clone())));
    submit(&mut c, ARMED, "a1");
    submit(&mut c, SIBLING, "b1");
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 0);
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(2000);
    assert_eq!(
        c.engine.client.cancels,
        vec!["a1".to_string()],
        "the halt trip still cancels first"
    );
    assert!(
        c.extra_engines[0].1.client.cancels.is_empty(),
        "the sibling's ORDERS are still not cancelled — only its trading state is halted"
    );
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert_eq!(
        c.extra_engines[0].1.trading_state,
        TradingState::Halted,
        "HALT is process-wide because the SENTINEL is — a half-halted daemon is not a state"
    );
    assert!(path.exists(), "the cross-process HALT sentinel file was written");
}

/// (d) An ordinary reconnect — `Disconnected` then `Live` inside the grace — is silent all the way
/// through the real dispatch arm, and a LATER sweep cannot resurrect the closed window.
#[test]
fn an_ordinary_reconnect_through_the_dispatch_arm_cancels_nothing() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt, None));
    submit(&mut c, ARMED, "a1");
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 0);
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 100);
    c.sweep_link_deadman(600);
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 700);

    c.sweep_link_deadman(10_000);
    assert!(c.engine.client.cancels.is_empty(), "a reconnect inside the grace must cancel nothing");
    assert_eq!(c.engine.trading_state, TradingState::Active);
}

/// (e) ⚠ **`Stale` through the real arm.** A weekend of `Stale` on an armed venue cancels nothing
/// — the property the whole re-ruling turns on, asserted at the RUNTIME level and not only on the
/// latch, because the dispatch arm is where a future "handle Stale too" edit would land.
#[test]
fn a_weekend_of_stale_cancels_nothing() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt, None));
    submit(&mut c, ARMED, "a1");
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 0);
    for t in [1_000, 60_000, 3_600_000, 172_800_000] {
        feed(&mut c, &clock, ARMED, FeedStatus::Stale, t);
        c.sweep_link_deadman(t);
    }
    assert!(
        c.engine.client.cancels.is_empty(),
        "48 h of Stale is a closed market, not a dead link"
    );
    assert_eq!(c.engine.trading_state, TradingState::Active);
}

/// (g) ⚠ **The trip ROUTES by venue, and this is the direction that can silently go wrong.** Every
/// other test here arms the venue that happens to be engine 0, so a `MassCancel` mis-routed to
/// engine 0 would still look right. Arm the SIBLING instead — the venue on an EXTRA engine — and
/// the claim becomes falsifiable: `apply/exit.rs`'s `lower_mass_cancel` resolves the engine through
/// `route_of`, whose `unwrap_or(0)` fallback is exactly what would cancel the wrong venue's book
/// while reporting success.
#[test]
fn a_trip_on_a_venue_that_is_not_engine_zero_cancels_that_engines_book() {
    let (mut c, clock) = core_with(CoreConfig {
        link_deadman: Some(LinkDeadManConfig {
            grace: std::time::Duration::from_millis(1000),
            action: DeadManAction::CancelAll,
            venues: [SIBLING.to_string()].into_iter().collect(),
            halt_file: None,
        }),
        ..Default::default()
    });
    submit(&mut c, ARMED, "a1");
    submit(&mut c, SIBLING, "b1");
    feed(&mut c, &clock, SIBLING, FeedStatus::Live, 0);
    feed(&mut c, &clock, SIBLING, FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(2000);
    assert_eq!(
        c.extra_engines[0].1.client.cancels,
        vec!["b1".to_string()],
        "the cancel must land on the engine whose venue's link died, not on engine 0"
    );
    assert!(
        c.engine.client.cancels.is_empty(),
        "engine 0's venue is still up — its book must survive"
    );
}

/// (h) ⚠ **TWO ACCOUNTS OF ONE EXCHANGE, and the reason the trip does not reuse the account-scoped
/// `MassCancel` routing.** A link death is a fact about the EXCHANGE: both engines' orders rest
/// behind the one dead socket, so both books must be pulled. `apply/exit.rs`'s `MassCancel { venue:
/// Some(v) }` arm resolves ONE engine through `route_of`, whose fallback is
/// `RouteKey::sole_account_of` — the venue's DEFAULT account — so the labelled account's book
/// survived. The shape is not hypothetical: `vike_tradehub_core::make_engine_accounts` builds it from an
/// `[accounts]` table, and the default account is frequently the paper one while the labelled one
/// carries the credentials, i.e. the old spelling cancelled a paper book and left the live orders
/// resting. `sweep_link_deadman` fans over `engines_of_venue` instead, and this test is what makes
/// the difference falsifiable.
///
/// ⚠ That description of the arm is HISTORY: it fans over `CoreThread::exit_scope_engines` now,
/// because the panic button carried the identical defect
/// (`crates/vike-core/src/runtime/tests/mount_account/core_minted_orders.rs`'s `a_venue_scoped_market_exit_reaches_each_account_of_the_exchange`).
/// This test is unchanged and still load-bearing — the sweep names each engine explicitly, so what
/// it asserts is that the SWEEP reaches both accounts, not that the arm would have.
#[test]
fn a_trip_cancels_every_account_of_the_dead_venue() {
    let clock: StepClock = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let read = Arc::clone(&clock);
    let mut config = link_cfg(1000, DeadManAction::CancelAll, None);
    config.clock = Box::new(move || read.load(std::sync::atomic::Ordering::Relaxed));
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    // Both engines carry the SAME `venue` (the canonical exchange id the payload routes on) and
    // DIFFERENT `route_key`s — exactly what `make_engine_accounts` stamps, and what makes
    // `CoreThread::multi_account` true.
    let engine = engine_for(ARMED);
    let mut labelled = engine_for(ARMED);
    labelled.route_key = format!("{ARMED}#ALT");
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let mut c = assemble_core(
        engine,
        vec![(1.0, labelled)],
        config,
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );

    // One resting order on each account. ⚠ BOTH are addressed by ENGINE INDEX, and the default
    // account's used to go through the payload route: since Stage 1 of the account-routing spec an
    // account-less submit naming a venue this process runs two accounts of is REFUSED rather than
    // sent to the default book, so `submit(&mut c, ARMED, ..)` would place no order here and this
    // test would prove nothing. Nothing about the DEAD-MAN changed — it cancels per engine through
    // `EngineRoute::Engine`, which is the whole point of the assertions below.
    c.apply_intent_routed(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "default-acct".into(),
            venue: ARMED.into(),
            symbol: SYMBOL.into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        })),
        0,
        CancelIntent::Unspecified,
        EngineRoute::Engine(0),
    );
    c.apply_intent_routed(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "labelled-acct".into(),
            venue: ARMED.into(),
            symbol: SYMBOL.into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        })),
        0,
        CancelIntent::Unspecified,
        EngineRoute::Engine(1),
    );
    assert_eq!(
        c.extra_engines[0].1.client.submissions.len(),
        1,
        "the labelled account really is holding an order — otherwise this test proves nothing"
    );

    feed(&mut c, &clock, ARMED, FeedStatus::Live, 0);
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);

    assert_eq!(
        c.engine.client.cancels,
        vec!["default-acct".to_string()],
        "the default account's book is pulled"
    );
    assert_eq!(
        c.extra_engines[0].1.client.cancels,
        vec!["labelled-acct".to_string()],
        "...and so is the LABELLED account's — its orders rest behind the same dead socket"
    );
}

/// (f) A status change on an UNARMED venue reaches the latch and is dropped: the sibling's link
/// can die for as long as it likes without ever cancelling anything.
#[test]
fn an_unarmed_venues_link_death_is_ignored() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt, None));
    submit(&mut c, ARMED, "a1");
    submit(&mut c, SIBLING, "b1");
    feed(&mut c, &clock, SIBLING, FeedStatus::Live, 0);
    feed(&mut c, &clock, SIBLING, FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(1_000_000);
    assert!(c.engine.client.cancels.is_empty());
    assert!(c.extra_engines[0].1.client.cancels.is_empty(), "an unarmed venue can never trip");
    assert_eq!(c.engine.trading_state, TradingState::Active);
}

// ───────────────────────── the trade link and the strat link, over the real dispatch arm ─────────

use vike_venues::link_deadman::{STRAT_LINK_STREAM, TRADE_LINK_STREAM};

/// (k) ⚠ **A trade-link loss is a dead-man outage in its own right**, over the REAL `StreamStatus`
/// arm and the reserved word the remote execution client stamps: the venue's own sockets never
/// said a word, the link to the gateway that holds the orders died, and the grace expiring pulls
/// that venue's book and (under `CancelAllAndHalt`) engages HALT. The `stream` the arm used to
/// destructure away is what routes it to the trade-link window.
#[test]
fn a_trade_link_loss_trips_the_dead_man_over_the_dispatch_arm() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt, None));
    submit(&mut c, ARMED, "a1");
    submit(&mut c, SIBLING, "b1");

    feed_stream(&mut c, &clock, ARMED, TRADE_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(500);
    assert!(c.engine.client.cancels.is_empty(), "inside the grace");
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.client.cancels, vec!["a1".to_string()], "the venue's book is pulled");
    assert!(c.extra_engines[0].1.client.cancels.is_empty(), "...and only that venue's");
    assert_eq!(c.engine.trading_state, TradingState::Halted);
}

/// (l) ⚠ **A `Live` from another source closes nothing, over the real arm.** The venue's book socket
/// disclosed `Disconnected`; the strat link and the trade link both report `Live` (they are fine),
/// and the venue outage still trips at its grace. Before the source split, the first of those two
/// `Live`s would have erased it and the venue's orders would have rested behind a dead socket.
#[test]
fn a_live_from_a_hub_link_does_not_erase_a_venue_outage() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Live, 100);
    feed_stream(&mut c, &clock, ARMED, TRADE_LINK_STREAM, FeedStatus::Live, 200);

    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.client.cancels, vec!["a1".to_string()], "the venue outage still trips");
}

/// How many cancels the primary engine's client has been asked for `coid`. `RecordingClient` never
/// terminalizes a cancel, so every ACT of the acting half on a resting order shows as one more.
fn cancels_of(c: &CoreThread<RecordingClient>, coid: &str) -> usize {
    c.engine.client.cancels.iter().filter(|x| x.as_str() == coid).count()
}

/// (m) ⚠ **Each source acts ONCE for its OWN outage; no source's trip silences another's.** The venue's
/// socket and then the strat link report `Disconnected` for one key; the first window expires and the
/// book is pulled; the second expires in a LATER sweep while the first still holds its trip and acts
/// AGAIN (the cancel is idempotent, and the first act cannot have covered an order placed since).
/// Neither source acts a second time for the SAME outage in the sweeps that follow.
#[test]
fn each_source_of_one_key_acts_once_for_its_own_outage_across_sweeps() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 600);

    c.sweep_link_deadman(1001);
    assert_eq!(cancels_of(&c, "a1"), 1, "the first window expired and acted");
    c.sweep_link_deadman(1700);
    assert_eq!(cancels_of(&c, "a1"), 2, "the second source's own expiry acts too");
    for t in [2000, 5000, 100_000] {
        c.sweep_link_deadman(t);
    }
    assert_eq!(cancels_of(&c, "a1"), 2, "no source acts twice for one outage");
}

/// (m2) Two sources of one key expiring in the SAME sweep are one exchange fact: ONE cancel pass for the
/// venue (the sweep's per-venue dedupe), one HALT.
#[test]
fn two_sources_expiring_in_one_sweep_pull_the_venues_book_once() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, TRADE_LINK_STREAM, FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(1001);
    assert_eq!(cancels_of(&c, "a1"), 1, "three windows, one sweep: one cancel pass for the venue");
    c.sweep_link_deadman(9000);
    assert_eq!(cancels_of(&c, "a1"), 1, "...and none later");
}

/// (o) ⚠ **THE REVIEW'S TRACE 1: the operator clears HALT while the first outage is still open, and the
/// NEXT, independent outage must trip.** The strat link (datahub) dies and trips: cancel plus HALT. The
/// operator, with the datahub still down, deliberately un-halts (state cleared, sentinel removed) and
/// places an order. The venue's own book socket then dies and outlasts the grace. Before the fix the
/// strat window's still-held trip vetoed the venue's expiry: no cancel, no HALT, the new order resting
/// behind a dead socket.
#[test]
fn a_venue_outage_trips_after_the_operator_cleared_halt_with_the_strat_link_still_down() {
    let scratch = crate::scratch::Scratch::reserved("link-deadman-two-outages");
    let path = scratch.path().to_path_buf();
    let (mut c, clock) =
        core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt, Some(path.clone())));
    submit(&mut c, ARMED, "a1");

    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(1001);
    assert_eq!(cancels_of(&c, "a1"), 1, "the strat-link trip pulls the book");
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert!(path.exists(), "...and writes the sentinel");

    // The operator un-halts with the datahub STILL down, and trades on.
    c.engine.trading_state = TradingState::Active;
    c.extra_engines[0].1.trading_state = TradingState::Active;
    std::fs::remove_file(&path).expect("the operator removes the sentinel");
    submit(&mut c, ARMED, "a2");

    // The venue's book socket now dies and outlasts the grace; the strat link is down throughout.
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 3000);
    c.sweep_link_deadman(3500);
    assert_eq!(cancels_of(&c, "a2"), 0, "inside the venue window's grace");
    c.sweep_link_deadman(4001);
    assert_eq!(
        cancels_of(&c, "a2"),
        1,
        "the venue outage cancels the order placed since the first trip"
    );
    assert_eq!(c.engine.trading_state, TradingState::Halted, "...and engages HALT again");
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Halted);
    assert!(path.exists(), "...and re-writes the sentinel");
}

/// (p) ⚠ **THE REVIEW'S TRACE 2: `cancel_all` has no HALT at all**, so nothing stops a new order after the
/// first trip; the second source's outage must still cancel it.
#[test]
fn under_cancel_all_a_second_sources_outage_cancels_the_order_placed_after_the_first_trip() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, TRADE_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(1001);
    assert_eq!(c.engine.client.cancels, vec!["a1".to_string()]);
    assert_eq!(c.engine.trading_state, TradingState::Active, "CancelAll engages no HALT");

    submit(&mut c, ARMED, "a2"); // nothing stops it
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 2000);
    c.sweep_link_deadman(3001);
    assert_eq!(cancels_of(&c, "a2"), 1, "the dead-man's whole purpose: the new order is pulled");
}

/// (q) ⚠ **THE REVIEW'S TRACE 3: a source that never reports `Live` shadows nobody.** The strat link trips
/// and never recovers (a producer bug, a process that exited mid-gap). A venue outage, its recovery, and a
/// SECOND venue outage each still trip, for the life of the process.
#[test]
fn a_source_that_never_reports_live_does_not_shadow_another_source() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(1001);
    assert_eq!(cancels_of(&c, "a1"), 1);

    submit(&mut c, ARMED, "a2");
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 2000);
    c.sweep_link_deadman(3001);
    assert_eq!(cancels_of(&c, "a2"), 1, "first venue outage trips");

    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Live, 4000);
    submit(&mut c, ARMED, "a3");
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 5000);
    c.sweep_link_deadman(6001);
    assert_eq!(
        cancels_of(&c, "a3"),
        1,
        "...and so does the SECOND, with the strat link down throughout"
    );
}

/// (r) The reverse order of the review's trace 4: A trips, A comes back, THEN B (down since before A
/// recovered) expires. A and B act the same way whichever order the `Live` and the expiry arrive in.
#[test]
fn a_source_acts_the_same_whether_or_not_another_source_already_recovered() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 500);
    c.sweep_link_deadman(1001);
    assert_eq!(cancels_of(&c, "a1"), 1);
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Live, 1200); // A back
    c.sweep_link_deadman(1600); // B's own expiry
    assert_eq!(cancels_of(&c, "a1"), 2, "B acts for its own outage, as it does with A still down");
}

/// (n) The gap-closure invariant through the real arm: a strat-link `Disconnected` closed by the
/// strat link's own `Live` (what a producer owes when a key is unsubscribed mid-outage) never trips.
#[test]
fn a_strat_link_gap_closed_by_its_own_live_never_trips() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Live, 400);
    c.sweep_link_deadman(1_000_000);
    assert!(c.engine.client.cancels.is_empty());
}

// ───────────────────────────── diagnostics: WHICH source tripped, and when the key is really clear ──────

/// (s) The trip's `warn!` names the SOURCE that tripped (the 3 a.m. question), and a further source
/// tripping while another is still down is said out loud rather than acted on in silence.
#[traced_test]
#[test]
fn a_trip_names_its_source_and_a_further_source_is_said_out_loud() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(1001);
    assert!(logs_contain("LINK DEAD-MAN TRIPPED"));
    assert!(logs_contain("source=StratLink"), "the trip names the datahub link as the source");
    assert!(!logs_contain("a further source"), "a first trip is not a further source");

    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 2000);
    c.sweep_link_deadman(3001);
    assert!(logs_contain("source=Venue"), "...and the venue socket as the next");
    assert!(
        logs_contain("a further source of this venue/symbol tripped"),
        "the second source's act is logged, not silent"
    );
}

/// (t) ⚠ "LINK RECOVERED ... the switch re-arms" is printed only when the KEY is clear. One source
/// coming back while another still holds its trip says so instead.
#[traced_test]
#[test]
fn the_recovered_line_is_not_printed_while_another_source_is_still_tripped() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(1001);

    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Live, 2000);
    assert!(logs_contain("LINK SOURCE RECOVERED"), "the venue socket's own recovery is reported");
    assert!(
        !logs_contain("LINK RECOVERED after"),
        "...but the key is NOT clear: the strat link is still tripped"
    );

    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Live, 2100);
    assert!(logs_contain("LINK RECOVERED after"), "the last source back clears the key");
}
