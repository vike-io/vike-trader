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
//! REAL venue strings and the REAL `vike_model::link_deadman_default` table — a disclosed binance
//! drop pulls the binance book while a mounted `oanda`'s book positively SURVIVES, and an oanda
//! disconnect on the same core pulls nothing at all.

use super::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, RiskGate, RiskLimits, TradingState};
use vike_model::{FeedStatus, OrderRequest};

/// The two venues every test here mounts: `sim` is the ARMED one, `other` is the sibling whose
/// book must survive `sim`'s link death.
const ARMED: &str = "sim";
const SIBLING: &str = "other";
const SYMBOL: &str = "BTCUSDT";

fn engine_for(venue: &str) -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        SYMBOL,
    )
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
/// `deadman_tests::core_with` shape plus one EXTRA engine, which is what makes the venue-scoped
/// claim testable at all (a single-engine core cannot show that a sibling was spared) — and the
/// handle to its injected clock.
fn core_with(config: CoreConfig) -> (CoreThread<RecordingClient>, StepClock) {
    core_with_venues(config, ARMED, SIBLING)
}

/// [`core_with`] over NAMED venues — so one test can drive the same wiring with the venue strings a
/// real mount uses (`"binance"` beside `"oanda"`) and therefore fold the REAL
/// `vike_model::link_deadman_default` table into the config rather than a stand-in.
fn core_with_venues(
    mut config: CoreConfig,
    primary: &str,
    sibling: &str,
) -> (CoreThread<RecordingClient>, StepClock) {
    let clock: StepClock = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let read = Arc::clone(&clock);
    config.clock = Box::new(move || read.load(std::sync::atomic::Ordering::Relaxed));
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let engine = engine_for(primary);
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let core = assemble_core(
        engine,
        vec![(1.0, engine_for(sibling))],
        config,
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );
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
            order_type: "market".into(),
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

/// [`feed`] under a NAMED stream label. The label is informational to the latch (the dispatch arm
/// destructures it away and the key is `(venue, symbol)`), which is exactly why one test drives the
/// label a real CEX tick pump stamps (`"ticks"` — `crates/bridges/binance/src/family/depth.rs`'s
/// `disclose_link`): a latch that had grown a label condition would pass every other test here and
/// fail that one.
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

/// (j) ⚠ **THE NEWLY-COVERED VENUES, end to end, over the REAL per-venue table and the REAL stream
/// label a CEX tick pump stamps.** Every other test in this file names two invented venues and
/// hand-builds the armed set, which proves the latch and proves nothing about whether a venue a
/// real daemon mounts can reach it. This one mounts `binance` beside `oanda`, folds the armed set
/// out of `vike_model::link_deadman_default` exactly as `vike-tradehub`'s
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
        .filter(|v| vike_model::link_deadman_default(v).is_on())
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
    // ⚠ `Scratch`, not `env::temp_dir()`: `crates/vike-ops/tests/journal_scratch_gate.rs` allows
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
/// the claim becomes falsifiable: `apply.rs`'s `MassCancel` arm resolves the engine through
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
/// behind the one dead socket, so both books must be pulled. `apply.rs`'s `MassCancel { venue:
/// Some(v) }` arm resolves ONE engine through `route_of`, whose fallback is
/// `RouteKey::sole_account_of` — the venue's DEFAULT account — so the labelled account's book
/// survived. The shape is not hypothetical: `vike_mount::make_engine_accounts` builds it from an
/// `[accounts]` table, and the default account is frequently the paper one while the labelled one
/// carries the credentials, i.e. the old spelling cancelled a paper book and left the live orders
/// resting. `sweep_link_deadman` fans over `engines_of_venue` instead, and this test is what makes
/// the difference falsifiable.
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

    // One resting order on each account. The default account's goes through the payload route; the
    // labelled one is addressed by engine index, the only way to reach a second account.
    submit(&mut c, ARMED, "default-acct");
    c.apply_intent_routed(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "labelled-acct".into(),
            venue: ARMED.into(),
            symbol: SYMBOL.into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
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
