//! Runtime wiring tests for the CONNECTION-state dead-man (M13) — the on-`CoreThread` half of the
//! feature (the pure latch is unit-tested in [`super::link_deadman`]). The sibling of
//! [`super::deadman_tests`], and deliberately the same shape: a synchronous `CoreThread` (no OS
//! thread) with a `RecordingClient` so the exact cancels a trip issued are observable, driven with
//! injected timestamps and zero wall-clock sleeps.
//!
//! What is covered here and NOT in the latch's own tests, because it needs an engine and a client:
//! (a) the feature-absent config builds nothing and the sweep is a no-op; (b) a trip on venue A
//! cancels A's resting orders and leaves B's untouched — the venue-SCOPED cancel; (c)
//! `CancelAllAndHalt` halts ONLY the dead venue's engines and writes no sentinel, and a `Live`
//! re-arms the venue (c2..c6: prior state kept, operator override, faulted core, a boot with a
//! down link);
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

fn link_cfg(grace_ms: u64, action: DeadManAction) -> CoreConfig {
    CoreConfig {
        link_deadman: Some(LinkDeadManConfig {
            grace: std::time::Duration::from_millis(grace_ms),
            action,
            venues: [ARMED.to_string()].into_iter().collect(),
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    assert_eq!(c.engine.trading_state, TradingState::Halted, "CancelAllAndHalt halts the venue");
}

/// (k5) ⚠ **A PRICE-ONLY venue behaves exactly like a mounted one**: the trip halts that venue's
/// engine and leaves the sibling trading, the same rows `a_trip_halts_only_the_venue_whose_link_died`
/// pins for a mounted venue; the venue is released when its pump is Live again.
#[test]
fn a_price_only_trip_halts_only_that_venue_and_releases_it_on_live() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED, SIBLING], &[ARMED]));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Active, "the sibling trades on");

    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Live, 2500);
    c.sweep_link_deadman(3000);
    assert_eq!(c.engine.trading_state, TradingState::Active, "Live again re-arms the venue");
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
    assert!(
        c.engine.client.cancels.is_empty(),
        "the outage that began idle is remembered, but its grace starts at THIS sweep"
    );
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
/// nothing and halts nothing, however long it lasts. It is REMEMBERED (the link IS down), but never
/// as a clock: an order placed afterwards does not inherit the idle time, its grace starts at the
/// sweep that finds it resting (`a_link_already_down_when_the_venue_first_holds_something_is_watched_from_then`).
#[test]
fn a_price_only_venue_holding_nothing_does_not_trip_and_its_idle_time_is_no_clock() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Active, "nothing to protect, nothing halted");
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(!ldm.is_down(ARMED, SYMBOL, VENUE_SOURCE), "no window is open for an idle venue");
    assert!(ldm.is_dormant(ARMED, SYMBOL, VENUE_SOURCE), "...but the link is remembered down");

    submit(&mut c, ARMED, "a1");
    c.sweep_link_deadman(9000);
    assert!(c.engine.client.cancels.is_empty(), "the idle time is not held against the new order");
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

/// (c) ⚠ **`CancelAllAndHalt` halts ONLY the venue whose link died** (owner ruling 2026-10-18,
/// decision 0180): the dead venue's engine is `Halted`, the sibling keeps trading — an order is
/// ADMITTED on it, and refused on the dead venue — and no sentinel file is anywhere (the config no
/// longer even names one). The venue is released when its link is Live again.
#[test]
fn a_trip_halts_only_the_venue_whose_link_died() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
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
        "the sibling's orders are not cancelled"
    );
    assert_eq!(c.engine.trading_state, TradingState::Halted, "the dead venue is halted");
    assert_eq!(
        c.extra_engines[0].1.trading_state,
        TradingState::Active,
        "⚠ the sibling venue is NOT halted: a link loss on one venue never stops another"
    );
    assert!(c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED));

    // Fail closed for THAT venue, open for the other: the gate refuses an opening order on the
    // dead venue and admits one on the sibling.
    let before = c.engine.client.submissions.len();
    submit(&mut c, ARMED, "a2");
    assert_eq!(
        c.engine.client.submissions.len(),
        before,
        "the halted venue's opening order is refused"
    );
    let before = c.extra_engines[0].1.client.submissions.len();
    submit(&mut c, SIBLING, "b2");
    assert_eq!(
        c.extra_engines[0].1.client.submissions.len(),
        before + 1,
        "the sibling still trades"
    );

    // Still down at later sweeps: still halted, and the cancel is not re-fired for this outage.
    c.sweep_link_deadman(5000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert_eq!(cancels_of(&c, "a1"), 1);
}

/// (c2) ⚠ **A `Live` re-arms the venue**: once every link of the halted venue is up, the next sweep
/// puts its engines back to the state each held BEFORE the halt, and the venue trades again. While a
/// second source of the venue is still down the venue stays halted.
#[test]
fn a_halted_venue_is_released_when_its_links_are_live_again() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);

    // One source back, the other still down: the venue stays halted.
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Live, 2100);
    c.sweep_link_deadman(2200);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "a link of the venue is still down");

    // The last link back: released at the next sweep, and the venue is no longer marked halted.
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Live, 2300);
    c.sweep_link_deadman(2400);
    assert_eq!(c.engine.trading_state, TradingState::Active, "re-armed");
    assert!(!c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED));
    let before = c.engine.client.submissions.len();
    submit(&mut c, ARMED, "a2");
    assert_eq!(c.engine.client.submissions.len(), before + 1, "the re-armed venue trades again");

    // A NEW outage trips and halts again: the re-arm left the switch armed.
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 3000);
    c.sweep_link_deadman(4500);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
}

/// (c3) ⚠ **Re-arming never loosens**: an engine that was already `Reducing` (the drawdown latch) or
/// `Halted` (the operator) when the link died returns to THAT state, not to `Active`; and a
/// node-wide raise (the drawdown latch) while the venue is link-halted raises the state it returns to.
#[test]
fn a_released_venue_returns_to_the_state_it_held_before_the_halt() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    c.engine.trading_state = TradingState::Reducing;
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert_eq!(c.node_trading_state(), TradingState::Reducing, "the node's state is the prior one");
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 2100);
    c.sweep_link_deadman(2200);
    assert_eq!(c.engine.trading_state, TradingState::Reducing, "back to Reducing, never Active");

    // An operator halt that predates the trip survives the re-arm.
    c.engine.trading_state = TradingState::Halted;
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 3000);
    c.sweep_link_deadman(5000);
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 5100);
    c.sweep_link_deadman(5200);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "an earlier halt is not loosened");

    // A node-wide raise while the venue is link-halted: the engine stays Halted, the prior rises.
    c.engine.trading_state = TradingState::Active;
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 6000);
    c.sweep_link_deadman(8000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    c.raise_trading_state(TradingState::Reducing);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Reducing, "the sibling is raised");
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 8100);
    c.sweep_link_deadman(8200);
    assert_eq!(c.engine.trading_state, TradingState::Reducing, "released into the raised state");
}

/// (c4) ⚠ **The operator's `SetTradingState` is the operator's**: an explicit state is applied to every
/// engine and nothing re-arms over it, not even on a later `Live` (while the link is down the venue's
/// overlay keeps the operator's state as its prior, so the re-arm is a no-op). The resume verb
/// (`Active`), with the link up, clears it, and a fresh outage after it halts the venue again.
#[test]
fn an_explicit_trading_state_replaces_the_venue_halt() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);

    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Halted)));
    assert_eq!(
        c.link_deadman.as_ref().expect("armed").prior_of(0),
        Some(TradingState::Halted),
        "overlay replaced: the operator's state is what the venue returns to"
    );
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 2100);
    c.sweep_link_deadman(2200);
    assert_eq!(
        c.engine.trading_state,
        TradingState::Halted,
        "the operator's halt is the operator's"
    );
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Halted);

    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));
    assert_eq!(c.engine.trading_state, TradingState::Active, "the resume verb clears it");
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 3000);
    c.sweep_link_deadman(5000);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "a later outage halts again");
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Active);
}

/// (c5) ⚠ **A faulted core is never re-armed**: a primary halted for its venue's dead link is not a
/// halted node (`node_trading_state` reads the prior), but once the panic safe state has halted
/// the core, a later `Live` must not release anything.
#[test]
fn a_faulted_core_is_not_rearmed_by_a_live() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.node_trading_state(), TradingState::Active, "one dead venue is not a halted node");

    c.enter_safe_state("test fault".to_string());
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 2100);
    c.sweep_link_deadman(2200);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "a faulted core is not re-armed");
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Halted);
}

/// (c6) ⚠ **A restart with the link down**: a fresh core (no latch history, as after a process
/// restart) whose very first status is `Disconnected` trips after the grace and halts that venue
/// alone — and an engine the journal restored `Halted` is still `Halted` after the link comes
/// back (the restart never loosens, `boot_restore`'s rule).
#[test]
fn a_boot_with_a_down_link_halts_that_venue_after_the_grace_and_a_restored_halt_stays() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    c.engine.trading_state = TradingState::Halted; // as restored from the journal Snap
    submit(&mut c, SIBLING, "b1");
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0); // the first status of the process
    c.sweep_link_deadman(500);
    assert!(!c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED), "inside the grace");
    c.sweep_link_deadman(2000);
    assert!(c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED));
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Active, "the sibling trades on");
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 2100);
    c.sweep_link_deadman(2200);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "the restored halt is not loosened");
}

/// (d) An ordinary reconnect — `Disconnected` then `Live` inside the grace — is silent all the way
/// through the real dispatch arm, and a LATER sweep cannot resurrect the closed window.
#[test]
fn an_ordinary_reconnect_through_the_dispatch_arm_cancels_nothing() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
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
    let mut config = link_cfg(1000, DeadManAction::CancelAll);
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, TRADE_LINK_STREAM, FeedStatus::Disconnected, 0);

    c.sweep_link_deadman(1001);
    assert_eq!(cancels_of(&c, "a1"), 1, "three windows, one sweep: one cancel pass for the venue");
    c.sweep_link_deadman(9000);
    assert_eq!(cancels_of(&c, "a1"), 1, "...and none later");
}

/// (o) ⚠ **THE REVIEW'S TRACE 1, as it reads now: a node-wide `resume` with the first outage still open
/// leaves that venue HALTED, and the NEXT, independent outage still trips.** The strat link (datahub)
/// dies and trips: cancel plus HALT. The operator's node-wide `resume` with the datahub STILL down no
/// longer reopens the venue (it used to empty the overlay and leave the tripped window unable to trip
/// again): `RESUME LEFT <venue> HALTED` in the ring, the sibling venue resumed. The venue's own book
/// socket then dies and outlasts the grace and its window trips on its own (the strat window's held trip
/// vetoes nothing); when both links are back the venue returns to the OPERATOR's state, `Active`.
#[test]
fn a_node_wide_resume_leaves_a_venue_with_its_strat_link_down_halted_and_the_next_outage_trips() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    submit(&mut c, ARMED, "a1");

    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(1001);
    assert_eq!(cancels_of(&c, "a1"), 1, "the strat-link trip pulls the book");
    assert_eq!(c.engine.trading_state, TradingState::Halted);

    // The operator resumes the node with the datahub STILL down.
    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));
    assert_eq!(c.engine.trading_state, TradingState::Halted, "the dead venue stays halted");
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Active, "the sibling resumed");
    assert!(c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED));
    assert!(
        noted(&c, &["RESUME LEFT", ARMED, "HALTED", "resume-venue", "--force"]),
        "{:?}",
        c.recent
    );

    // The venue's book socket now dies and outlasts the grace; the strat link is down throughout.
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Disconnected, 3000);
    c.sweep_link_deadman(4001);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "still halted");

    // Both links come back: the venue goes to the state the operator set, not to the old prior.
    feed_stream(&mut c, &clock, ARMED, STRAT_LINK_STREAM, FeedStatus::Live, 4100);
    c.sweep_link_deadman(4150);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "the book socket is still down");
    feed_stream(&mut c, &clock, ARMED, "book", FeedStatus::Live, 4200);
    c.sweep_link_deadman(4250);
    assert_eq!(c.engine.trading_state, TradingState::Active, "released into the operator's state");
}

/// (o2) ⚠ **THE REVIEW'S FINDING 1: a node-wide `resume` must not reopen a venue whose trade link is
/// STILL DEAD** (it emptied the overlay and left the window `tripped`, so the venue could not trip
/// again until its own `Live` arrived and the link dropped again). One `resume` meant for venue B also
/// reopened A. Now: the dead venue stays halted and says so, the other venue resumes, a drawdown
/// latch that fired under the halt does not come back as the venue's prior (the operator's `Active`
/// is), and `resume-venue --force` stays the override.
#[traced_test]
#[test]
fn a_node_wide_resume_leaves_a_venue_with_a_dead_trade_link_halted() {
    let (mut c, clock) = core_with(both_armed_cfg(DeadManAction::CancelAllAndHalt));
    feed_stream(&mut c, &clock, ARMED, TRADE_LINK_STREAM, FeedStatus::Disconnected, 0);
    feed(&mut c, &clock, SIBLING, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    c.raise_trading_state(TradingState::Reducing); // the drawdown latch fires under both halts
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Halted);

    // The sibling's link is back (not yet swept); ARMED's trade link is still dead.
    feed(&mut c, &clock, SIBLING, FeedStatus::Live, 2100);
    clock.store(2150, std::sync::atomic::Ordering::Relaxed);
    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));

    assert_eq!(c.engine.trading_state, TradingState::Halted, "⚠ the dead trade link stays halted");
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Active, "the other venue resumed");
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(ldm.is_venue_halted(ARMED) && !ldm.is_venue_halted(SIBLING));
    assert!(logs_contain("RESUME LEFT sim HALTED: its link is still down"));
    assert!(noted(&c, &["RESUME LEFT sim HALTED", "resume-venue sim --force"]), "{:?}", c.recent);

    // Nothing re-arms it while the link is down...
    c.sweep_link_deadman(2200);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    // ...a node-wide resume again changes nothing for it...
    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));
    assert_eq!(c.engine.trading_state, TradingState::Halted);

    // ...and the per-venue force is the override: it goes to the operator's state (`Active`), not to
    // the latch's `Reducing` the halt was recorded under.
    resume(&mut c, &clock, ARMED, true, 2300);
    assert_eq!(c.engine.trading_state, TradingState::Active);
}

/// (o3) A node-wide `resume` with the trade link UP (or never down) releases the venue exactly as it
/// always did (the overlay forgotten, every engine at the operator's state, nothing noted).
#[test]
fn a_node_wide_resume_reopens_a_venue_whose_link_is_up() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    feed_stream(&mut c, &clock, ARMED, TRADE_LINK_STREAM, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    feed_stream(&mut c, &clock, ARMED, TRADE_LINK_STREAM, FeedStatus::Live, 2100);

    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));
    assert_eq!(c.engine.trading_state, TradingState::Active, "the link is up: resumed");
    assert!(!c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED));
    assert!(!noted(&c, &["RESUME LEFT"]), "{:?}", c.recent);
}

/// (p) ⚠ **THE REVIEW'S TRACE 2: `cancel_all` has no HALT at all**, so nothing stops a new order after the
/// first trip; the second source's outage must still cancel it.
#[test]
fn under_cancel_all_a_second_sources_outage_cancels_the_order_placed_after_the_first_trip() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAll));
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

// ───────────────────── a link that is ALREADY down when a price-only venue first holds something ──────

/// (k6) ⚠ **THE HEADLINE.** A price-only venue's link dies while the venue holds NOTHING. Nothing
/// re-discloses a link that is already down, so the first order placed into the outage used to be
/// unwatched for as long as it lasted. The outage is remembered now: the sweep that finds the venue
/// holding something opens a window THERE (the grace runs from the moment there is something to
/// protect, never from the idle disclosure), and a link still down past it trips like any other.
#[test]
fn a_link_already_down_when_the_venue_first_holds_something_is_watched_from_then() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0); // idle: nothing rests
    c.sweep_link_deadman(500);
    assert!(c.link_deadman.as_ref().expect("armed").is_dormant(ARMED, SYMBOL, VENUE_SOURCE));

    submit(&mut c, ARMED, "a1"); // the first order, into an outage nobody re-discloses
    c.sweep_link_deadman(1_500); // the sweep that finds the venue held opens the window HERE
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(
        ldm.is_down(ARMED, SYMBOL, VENUE_SOURCE) && !ldm.is_dormant(ARMED, SYMBOL, VENUE_SOURCE)
    );
    assert!(
        c.engine.client.cancels.is_empty(),
        "no grace has passed since there was anything held"
    );

    c.sweep_link_deadman(2_400); // 900 into the window
    assert!(c.engine.client.cancels.is_empty(), "still inside the grace");
    c.sweep_link_deadman(2_600); // 1100 into it
    assert_eq!(c.engine.client.cancels, vec!["a1".to_string()], "the order is pulled");
    assert_eq!(c.engine.trading_state, TradingState::Halted, "...and the venue halted");
}

/// (k7) A `Live` while the venue is still idle ends the remembered outage: nothing is carried into the
/// order placed afterwards.
#[test]
fn a_live_while_idle_forgets_the_remembered_outage() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Live, 400);
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(
        !ldm.is_dormant(ARMED, SYMBOL, VENUE_SOURCE) && !ldm.is_down(ARMED, SYMBOL, VENUE_SOURCE)
    );

    submit(&mut c, ARMED, "a1");
    c.sweep_link_deadman(1_000_000);
    assert!(c.engine.client.cancels.is_empty() && c.engine.trading_state == TradingState::Active);
}

/// (k8) A window that is OPEN when its venue goes idle is parked, not dropped: the link is still down,
/// and the next order into it is watched from the sweep that finds it resting.
#[test]
fn a_window_open_when_the_venue_goes_idle_is_remembered_not_dropped() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0); // held: a window opens
    assert!(c.link_deadman.as_ref().expect("armed").is_down(ARMED, SYMBOL, VENUE_SOURCE));
    c.engine.registry.clear(); // the order is gone and nothing else is held: the venue is idle

    c.sweep_link_deadman(500);
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(
        !ldm.is_down(ARMED, SYMBOL, VENUE_SOURCE) && ldm.is_dormant(ARMED, SYMBOL, VENUE_SOURCE)
    );

    submit(&mut c, ARMED, "a2");
    c.sweep_link_deadman(700); // held again: a fresh window from here
    c.sweep_link_deadman(1_600);
    assert!(c.engine.client.cancels.is_empty(), "900 into the fresh window");
    c.sweep_link_deadman(1_800);
    assert_eq!(c.engine.client.cancels, vec!["a2".to_string()]);
}

/// (k9) ⚠ **A MOUNTED venue is untouched by all of it**: it is never idle, so a `Disconnected` opens a
/// window at once (the disclosure's own instant is the clock), remembers nothing, and trips with
/// nothing resting, as it always did.
#[test]
fn a_mounted_venue_opens_its_window_at_the_disclosure_and_remembers_nothing() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(
        ldm.is_down(ARMED, SYMBOL, VENUE_SOURCE) && !ldm.is_dormant(ARMED, SYMBOL, VENUE_SOURCE)
    );
    c.sweep_link_deadman(2_000);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "trips with nothing resting");
}

// ───────────────────────────── the operator's per-venue resume (decision 0180) ────────────────────

/// A core whose latch arms BOTH venues, so two venues can be halted at once and one released.
fn both_armed_cfg(action: DeadManAction) -> CoreConfig {
    CoreConfig {
        link_deadman: Some(LinkDeadManConfig {
            grace: std::time::Duration::from_millis(1000),
            action,
            venues: [ARMED.to_string(), SIBLING.to_string()].into_iter().collect(),
        }),
        ..Default::default()
    }
}

/// The operator's `resume-venue` through the REAL dispatch arm, at simulated time `now`.
fn resume(
    c: &mut CoreThread<RecordingClient>,
    clock: &StepClock,
    venue: &str,
    force: bool,
    now: i64,
) {
    clock.store(now, std::sync::atomic::Ordering::Relaxed);
    c.dispatch(Ingest::Command(Command::ResumeVenue { venue: venue.to_string(), force }));
}

/// Whether a ring note carries every needle.
fn noted(c: &CoreThread<RecordingClient>, needles: &[&str]) -> bool {
    c.recent.iter().any(|l| needles.iter().all(|n| l.contains(n)))
}

/// (v1) ⚠ **A resume REFUSES while the venue's link is still down**, and the refusal changes nothing:
/// the engine stays `Halted`, the latch keeps its overlay, and the verdict is a ring note AND a WARN
/// (the hub's audit line says only that the command reached the queue).
#[traced_test]
#[test]
fn a_resume_refuses_while_the_venues_link_is_down_and_changes_nothing() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    submit(&mut c, ARMED, "a1");
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);

    resume(&mut c, &clock, ARMED, false, 2100);

    assert_eq!(c.engine.trading_state, TradingState::Halted, "refused: still halted");
    assert!(c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED), "the overlay is kept");
    assert!(noted(&c, &["REFUSED", "still down"]), "{:?}", c.recent);
    assert!(logs_contain("OPERATOR RESUME OF THE VENUE REFUSED"));
}

/// (v2) ⚠ **A resume with the link up releases THAT venue alone**, to the state it held before the halt:
/// the other dead venue stays halted (its overlay kept), and the drawdown latch that fired while both
/// were halted is not lifted (the prior rose with it, `raise_priors`). This is what `SetTradingState`
/// cannot do: it clears every venue's halt and the latch with it.
#[traced_test]
#[test]
fn a_resume_releases_only_that_venue_to_its_prior_state_and_keeps_the_latch() {
    let (mut c, clock) = core_with(both_armed_cfg(DeadManAction::CancelAllAndHalt));
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    feed(&mut c, &clock, SIBLING, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Halted);
    c.raise_trading_state(TradingState::Reducing); // the drawdown latch fires under both halts

    feed(&mut c, &clock, ARMED, FeedStatus::Live, 2100); // ARMED's link is back; the operator does not wait
    resume(&mut c, &clock, ARMED, false, 2150);

    assert_eq!(
        c.engine.trading_state,
        TradingState::Reducing,
        "back to the prior state, which the latch raised: never Active"
    );
    assert_eq!(
        c.extra_engines[0].1.trading_state,
        TradingState::Halted,
        "⚠ the OTHER dead venue stays halted"
    );
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(!ldm.is_venue_halted(ARMED) && ldm.is_venue_halted(SIBLING));
    assert!(logs_contain("OPERATOR RESUMED THE VENUE"));
    assert!(noted(&c, &["RESUMED", ARMED]), "{:?}", c.recent);

    // Nothing re-arms over it, and the sibling's own halt is still the latch's to release.
    c.sweep_link_deadman(2200);
    assert_eq!(c.engine.trading_state, TradingState::Reducing);
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Halted);
}

/// (v3) ⚠ **A forced resume with the link still down** resumes the venue AND starts its windows over,
/// so a link that stays down is acted on again after another grace instead of never (a window acts
/// once per outage, and the old one had acted).
#[test]
fn a_forced_resume_with_the_link_down_restarts_the_windows_and_trips_again() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    submit(&mut c, ARMED, "a1");
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(cancels_of(&c, "a1"), 1);
    assert_eq!(c.engine.trading_state, TradingState::Halted);

    resume(&mut c, &clock, ARMED, true, 2500);
    assert_eq!(c.engine.trading_state, TradingState::Active, "forced: the venue trades again");
    assert!(!c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED));

    submit(&mut c, ARMED, "a2");
    c.sweep_link_deadman(3000); // 500 into the restarted window
    assert_eq!(cancels_of(&c, "a2"), 0);
    assert_eq!(c.engine.trading_state, TradingState::Active);
    c.sweep_link_deadman(3600); // 1100 into it, the link never came back
    assert_eq!(cancels_of(&c, "a2"), 1, "the new order is pulled");
    assert_eq!(c.engine.trading_state, TradingState::Halted, "...and the venue halted again");
}

/// (v4) ⚠ **After a restart there is no overlay** (it is not journaled), only a Snap that restored the
/// engine `Halted`. The resume then takes the state from the OTHER venues: `Reducing` here, because the
/// drawdown latch holds the sibling there. The sibling is untouched.
#[test]
fn a_resume_after_a_restart_takes_the_state_from_the_other_venues() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    c.engine.trading_state = TradingState::Halted; // as restored from the journal Snap
    c.extra_engines[0].1.trading_state = TradingState::Reducing;
    assert!(!c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED), "PREMISE: no overlay");

    resume(&mut c, &clock, ARMED, false, 10);

    assert_eq!(c.engine.trading_state, TradingState::Reducing, "never looser than the other venue");
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Reducing);
    assert!(noted(&c, &["RESUMED", "no recorded prior"]), "{:?}", c.recent);

    // Active elsewhere: Active here.
    c.engine.trading_state = TradingState::Halted;
    c.extra_engines[0].1.trading_state = TradingState::Active;
    resume(&mut c, &clock, ARMED, false, 20);
    assert_eq!(c.engine.trading_state, TradingState::Active);
}

/// (v5) The refusals that need no link: another venue's halt is a NODE-WIDE halt (`resume` lifts it, this
/// verb does not pretend to), and a venue with no `Halted` engine has nothing to resume.
#[test]
fn a_resume_refuses_a_node_wide_halt_and_a_venue_that_is_not_halted() {
    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    c.engine.trading_state = TradingState::Halted;
    c.extra_engines[0].1.trading_state = TradingState::Halted; // the operator's `halt`: every engine

    resume(&mut c, &clock, ARMED, false, 10);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "left alone");
    assert!(noted(&c, &["REFUSED", "node-wide halt"]), "{:?}", c.recent);

    c.engine.trading_state = TradingState::Active;
    c.extra_engines[0].1.trading_state = TradingState::Active;
    resume(&mut c, &clock, ARMED, false, 20);
    assert!(noted(&c, &["REFUSED", "nothing to resume"]), "{:?}", c.recent);
}

/// (v6) A resume on a core with no link dead-man, for a venue the core does not run, or on a FAULTED core
/// (only an operator `resume` loosens the panic safe state) changes nothing and says why.
#[test]
fn a_resume_is_refused_without_the_switch_for_an_unknown_venue_and_on_a_faulted_core() {
    let (mut c, clock) = core_with(CoreConfig::default());
    c.engine.trading_state = TradingState::Halted;
    resume(&mut c, &clock, ARMED, false, 10);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    assert!(noted(&c, &["REFUSED", "not armed"]), "{:?}", c.recent);

    let (mut c, clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    resume(&mut c, &clock, "nosuch", false, 10);
    assert!(noted(&c, &["REFUSED", "runs no engine for nosuch"]), "{:?}", c.recent);

    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    c.enter_safe_state("test fault".to_string());
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 2100);
    resume(&mut c, &clock, ARMED, false, 2200);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "a faulted core is not resumed");
    assert!(noted(&c, &["REFUSED", "faulted"]), "{:?}", c.recent);
}

/// (v7) The verb is NOT journaled: the halt it releases is an overlay the journal never held, so a
/// replay core could not re-apply it (`dispatch`'s `journaled` match).
#[test]
fn a_resume_is_not_journaled() {
    let dir = crate::scratch::Scratch::reserved("resume-venue-not-journaled");
    let mut config = link_cfg(1000, DeadManAction::CancelAllAndHalt);
    config.journal = Some(crate::runtime::JournalConfig::at(dir.to_path_buf()));
    let (mut c, clock) = core_with(config);
    let before = c.journaled_since_snap;
    resume(&mut c, &clock, ARMED, false, 10);
    assert_eq!(c.journaled_since_snap, before, "ResumeVenue appends nothing to the journal");
    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));
    assert_eq!(c.journaled_since_snap, before + 1, "PREMISE: an ordinary command does");
}

// ───────────────────────── the third review of the follow-ups (decision 0180) ─────────────────────────

/// (w1) ⚠ **FINDING 1: `resume-venue B` must not lift a node-wide halt that landed while A was link-halted.**
/// A is link-tripped (prior `Active`); the operator's `halt` keeps A halted (its link is down), makes A's
/// prior `Halted` and halts B. B has no overlay entry, so the verb reads the OTHER venues for the state to
/// return to: A's engine is `Halted` by the latch, and its RECORDED prior is the fact that counts. The
/// old fallback skipped every engine the latch held and read `Active`, so B went `Active` mid-halt.
#[test]
fn a_resume_venue_does_not_lift_a_node_wide_halt_that_landed_on_a_link_halted_venue() {
    let (mut c, clock) = core_with(both_armed_cfg(DeadManAction::CancelAllAndHalt));
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "PREMISE: A is link-halted");
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Active, "PREMISE: B trades");

    clock.store(2100, std::sync::atomic::Ordering::Relaxed);
    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Halted))); // the operator's `halt`
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Halted, "PREMISE: B halted");
    assert_eq!(c.link_deadman.as_ref().expect("armed").prior_of(0), Some(TradingState::Halted));

    resume(&mut c, &clock, SIBLING, false, 2200);

    assert_eq!(
        c.extra_engines[0].1.trading_state,
        TradingState::Halted,
        "⚠ the node-wide halt holds"
    );
    assert!(noted(&c, &["REFUSED", "node-wide halt"]), "{:?}", c.recent);
}

/// (w1b) The same fallback reads A's RECORDED prior when it is not `Halted`: a link-halted A that held
/// `Reducing` (the drawdown latch) before the trip makes B return to `Reducing`, not `Active`; a link-halted
/// A that held `Active` still does not count its own `Halted` (a venue halt is not a fact about the node).
#[test]
fn a_resume_venue_without_an_overlay_reads_the_other_halted_venues_recorded_prior() {
    let (mut c, clock) = core_with(both_armed_cfg(DeadManAction::CancelAllAndHalt));
    c.engine.trading_state = TradingState::Reducing; // the drawdown latch held A
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
    c.extra_engines[0].1.trading_state = TradingState::Halted; // B as restored from a Snap, no overlay

    resume(&mut c, &clock, SIBLING, false, 2100);
    assert_eq!(
        c.extra_engines[0].1.trading_state,
        TradingState::Reducing,
        "never looser than A's prior"
    );

    let (mut c, clock) = core_with(both_armed_cfg(DeadManAction::CancelAllAndHalt));
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    c.extra_engines[0].1.trading_state = TradingState::Halted;
    resume(&mut c, &clock, SIBLING, false, 2100);
    assert_eq!(
        c.extra_engines[0].1.trading_state,
        TradingState::Active,
        "A's own halt is not the node's"
    );
}

/// (w2) ⚠ **FINDING 2: a node-wide resume that KEEPS a venue halted leaves a `Snap` holding it halted.** The
/// journaled `SetTradingState(Active)` replays as "every engine" (a replay core has no link dead-man), so a
/// restore of the Snap plus that tail would reopen the venue the live core kept halted. A `Snap` written
/// after the fold is the latest base with nothing after it. A resume that keeps nothing writes none.
#[test]
fn a_node_wide_resume_that_keeps_a_venue_halted_leaves_a_snap_holding_it_halted() {
    let dir = crate::scratch::Scratch::reserved("resume-keeps-halt-snap");
    let mut config = both_armed_cfg(DeadManAction::CancelAllAndHalt);
    config.journal = Some(crate::runtime::JournalConfig::at(dir.to_path_buf()));
    let (mut c, clock) = core_with(config);
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);

    clock.store(2100, std::sync::atomic::Ordering::Relaxed);
    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));
    assert_eq!(c.engine.trading_state, TradingState::Halted, "A stays halted: its link is down");
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Active);
    assert_eq!(c.journaled_since_snap, 0, "a Snap was written after the partial resume");

    c.journal.as_mut().expect("journal").flush().expect("flush");
    drop(c); // release the journal's handle before reading it back
    let records = vike_journal::CommandJournal::read_all(&dir).expect("readable journal");
    let cmd = records
        .iter()
        .rposition(|r| {
            matches!(
                r,
                vike_journal::JournalRecord::Cmd {
                    msg: Ingest::Command(Command::SetTradingState(_)),
                    ..
                }
            )
        })
        .expect("the resume is journaled");
    let snap = records
        .iter()
        .rposition(|r| matches!(r, vike_journal::JournalRecord::Snap { .. }))
        .expect("a Snap");
    assert!(
        snap > cmd,
        "the Snap is the LATEST base, after the command: the restore re-folds nothing"
    );
    let vike_journal::JournalRecord::Snap { engines, .. } = &records[snap] else { unreachable!() };
    assert_eq!(engines.len(), 2);
    assert_eq!(engines[0].trading_state, TradingState::Halted, "A is journaled halted");
    assert_eq!(engines[1].trading_state, TradingState::Active, "B resumed");
}

/// (w2b) A node-wide resume that keeps NOTHING (every link up) writes no extra `Snap`: the cadence is
/// untouched for the ordinary verb.
#[test]
fn a_node_wide_resume_that_keeps_nothing_writes_no_snap() {
    let dir = crate::scratch::Scratch::reserved("resume-keeps-nothing-no-snap");
    let mut config = both_armed_cfg(DeadManAction::CancelAllAndHalt);
    config.journal = Some(crate::runtime::JournalConfig::at(dir.to_path_buf()));
    let (mut c, clock) = core_with(config);
    feed(&mut c, &clock, ARMED, FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    feed(&mut c, &clock, ARMED, FeedStatus::Live, 2050); // the link is back (not yet swept)
    let before = c.journaled_since_snap;
    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));
    assert_eq!(c.engine.trading_state, TradingState::Active);
    assert_eq!(c.journaled_since_snap, before + 1, "just the journaled command, no Snap");
}

/// A third-venue engine, for the hot start below.
fn third_engine() -> ExecutionEngine<RecordingClient> {
    engine_on("third", "SOLUSDT", RecordingClient::default())
}

/// (w3) ⚠ **FINDING 3: a link-halted PRIMARY restored from a Snap is not a node-wide halt.** After a restart
/// the overlay is gone, so engine 0 reads `Halted` with nothing recording why. A node-wide halt halts EVERY
/// engine, so a `Halted` primary beside an `Active` engine of another venue is that venue's halt: the node
/// state is the loosest the others show. Without it the drawdown latch returned early for the whole node and
/// a hot-started account of ANOTHER venue inherited `Halted`.
#[test]
fn a_restored_halted_primary_beside_an_active_venue_is_not_a_node_wide_halt() {
    let mut config = link_cfg(1000, DeadManAction::CancelAllAndHalt);
    config.seed_cash = 1000.0;
    config.max_drawdown = Some(0.01);
    let (mut c, _clock) = core_with(config);
    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    c.engine.trading_state = TradingState::Halted; // as restored from the journal Snap
    assert!(!c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED), "PREMISE: no overlay");
    assert_eq!(
        c.node_trading_state(),
        TradingState::Active,
        "the other venue shows the node's state"
    );

    // The drawdown latch is armed for the node: a loss on B's book trips it, A stays Halted.
    c.extra_engines[0].1.account.positions.insert(
        (SIBLING.into(), SYMBOL.into(), vike_model::events::PositionSide::Both),
        vike_exec::PositionEntry { size: 100.0, avg_px: 100.0, ..Default::default() },
    );
    c.extra_engines[0].1.price_board.set_quote(SIBLING, SYMBOL, 100.0, 100.5, 1);
    c.sweep_drawdown_latch(0.01);
    c.extra_engines[0].1.price_board.set_quote(SIBLING, SYMBOL, 50.0, 50.5, 2);
    c.sweep_drawdown_latch(0.01);
    assert_eq!(c.extra_engines[0].1.trading_state, TradingState::Reducing, "the latch tripped");
    assert_eq!(c.engine.trading_state, TradingState::Halted, "...and A is still halted");

    // An account of a THIRD venue hot-started now does not inherit A's halt, only the latch.
    c.apply_account_change(AccountChange::add_engine(100.0, third_engine()), 20);
    let added = c.extra_engines.len();
    assert_eq!(c.eng(added).trading_state, TradingState::Reducing);
}

/// (w3b) The other side: a primary `Halted` beside engines that are `Halted` too (the operator's `halt`, the
/// silence dead-man, the panic safe state), or alone on its venue, IS the node's halt: nothing latches and a
/// hot-started account of another venue inherits `Halted`.
#[test]
fn a_halted_primary_beside_halted_engines_or_alone_is_still_a_node_wide_halt() {
    let (mut c, _clock) = core_with(link_cfg(1000, DeadManAction::CancelAllAndHalt));
    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    c.engine.trading_state = TradingState::Halted;
    c.extra_engines[0].1.trading_state = TradingState::Halted;
    assert_eq!(c.node_trading_state(), TradingState::Halted);
    c.apply_account_change(AccountChange::add_engine(100.0, third_engine()), 20);
    assert_eq!(c.eng(c.extra_engines.len()).trading_state, TradingState::Halted);

    // A node with one venue has no other to read.
    let mut solo =
        core_of(engine_for(ARMED), Vec::new(), link_cfg(1000, DeadManAction::CancelAllAndHalt));
    solo.engine.trading_state = TradingState::Halted;
    assert_eq!(solo.node_trading_state(), TradingState::Halted);
}

/// (w4) ⚠ **FINDING 4: the node-wide resume and the sweep agree about a link remembered down across
/// idleness.** A price-only venue trips, goes idle, and a `Disconnected` disclosed while it holds nothing is
/// only REMEMBERED (a dormant entry, no window). `take_recovered` releases such a venue at the next sweep
/// (decision 0180's "discarded as idle is a release"), so a node-wide `resume` must not log
/// `RESUME LEFT <venue> HALTED` for a halt that ends within a sweep: it simply resumes the venue.
#[test]
fn a_node_wide_resume_does_not_claim_to_keep_a_venue_whose_link_is_only_remembered_down() {
    let (mut c, clock) =
        core_with(price_only_cfg(DeadManAction::CancelAllAndHalt, &[ARMED], &[ARMED]));
    submit(&mut c, ARMED, "a1");
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 0);
    c.sweep_link_deadman(2000);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "PREMISE: the venue tripped");
    c.engine.registry.clear(); // nothing is held any more: the venue is idle
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Live, 2100); // the window closes...
    feed_stream(&mut c, &clock, ARMED, "ticks", FeedStatus::Disconnected, 2200); // ...and the link drops idle
    let ldm = c.link_deadman.as_ref().expect("armed");
    assert!(
        ldm.is_dormant(ARMED, SYMBOL, VENUE_SOURCE) && !ldm.is_down(ARMED, SYMBOL, VENUE_SOURCE)
    );
    assert!(ldm.is_venue_halted(ARMED), "PREMISE: no sweep has released the venue yet");

    clock.store(2300, std::sync::atomic::Ordering::Relaxed);
    c.dispatch(Ingest::Command(Command::SetTradingState(TradingState::Active)));

    assert_eq!(c.engine.trading_state, TradingState::Active, "the resume reopens the idle venue");
    assert!(!noted(&c, &["RESUME LEFT"]), "{:?}", c.recent);
    assert!(!c.link_deadman.as_ref().expect("armed").is_venue_halted(ARMED));
    c.sweep_link_deadman(2400);
    assert_eq!(c.engine.trading_state, TradingState::Active, "the sweep agrees");
}
