//! The white-box test support of `crate::runtime`: ONE spelling of the synchronous `CoreThread`
//! assembly that the runtime's in-crate test modules had each re-typed — the engine, the `Conflated`
//! market slot, the `ArcSwap` snapshot cell and the rejected-command counter `spawn_core` builds,
//! handed to `assemble_core` with no OS thread — plus the one read every multi-mount test made of
//! the coid attribution map.
//!
//! Declared ONCE, in `crates/vike-core/src/runtime/mod.rs`, as a `#[cfg(test)]` module behind a
//! `#[path]`, so no production build compiles it. Every item is `pub(super)`: anything under
//! `crate::runtime` — `apply`'s and `safe_state_tests`' child modules included — names it as
//! `crate::runtime::test_support::NAME`. The items need the crate-private `assemble_core` and
//! `CoreThread`, which is why they cannot live in the integration kit
//! (`crates/vike-core/tests/support/mod.rs`) or in `vike_exec::testing`. Before this file each copy
//! said it was "duplicated rather than shared because a private helper in a sibling test module is
//! not reachable from here"; this module is the shared home they lacked.
//!
//! # The contract: every builder fixes its DEFAULTS, and says which
//!
//! The contract of `vike_marketdata::test_support`, applied to this crate's vocabulary. A builder
//! takes only what its callers vary and pins everything else to the value its own doc names, so a
//! hand-written copy may be replaced by the builder exactly when its signature AND every pinned value
//! match. A test that READS a pinned value — a venue string, a multiplier, a `BalanceMode`, a risk limit —
//! keeps building that engine itself, so the value it depends on stays visible at its own site.
//! Where two shapes differ in one pinned value, each gets its own NAME, never one builder with a
//! default that is wrong for half its callers.
//!
//! ⚠ **A change to a default here moves every test that takes it, at once.** That is the cost of one
//! spelling, and this paragraph is the only guard against it: no test asserts that a default is
//! unread, only that the scenarios which DO read one build their engine by hand. Widening a default
//! is a change to every caller, so give the new shape a new name instead.
//!
//! # What is deliberately NOT here
//!
//! - The wrappers whose SUBJECT is their difference. They stay in their own files and build ON this
//!   one: `apply`'s `test_core_with` (any client, extra engines), `recon_held_tests`' bybit/`1_000`
//!   `core`, `link_deadman_tests`' two-venue `core_with_venues` with its `StepClock`,
//!   `drawdown_latch`'s `dd_core_with_venue_wallet`, and the route-key engines of `route_key_tests`
//!   and `mount_account_tests`.
//! - The integration kit's twin of [`sim_engine_with`] (`crates/vike-core/tests/support/engines.rs`'s
//!   `sim_engine_with`, same name, same signature, same defaults): the two small copies are accepted
//!   rather than shared. A `src/` file may not include one under `tests/` (the direction
//!   `xtask/src/ci/graph.rs`'s `is_test_target_source` classifies by path), and lifting the shape
//!   into `vike_exec::testing` is a coordinated shared-home change of its own.
//! - What needs no crate-private item: the default strategy mount (`mount_of`) and the
//!   `AlwaysSubmitStrategy` double are `crate::testkit`'s, which both test homes reach.
//! - Any clock, scratch directory or compile-time path. Nothing here needs one, and a test that does
//!   keeps it at its own site, where the gates that police those reads can name it.

use super::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate};
use vike_model::RiskLimits;

/// One engine on the sim venue driving `client` — the shape every white-box core here starts from.
///
/// DEFAULTS: venue `"sim"`, symbol `"BTCUSDT"`, contract multiplier `1.0` on an account labelled `"sim"` (so
/// `route_key` is `"sim"` too), `BalanceMode::Delta`, and `RiskLimits::new()` — every limit
/// disarmed, `max_orders_per_window` among them. A test whose subject is one of those builds its
/// engine itself.
pub(super) fn sim_engine_with<C: ExecutionClient>(client: C) -> ExecutionEngine<C> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "sim",
        "BTCUSDT",
    )
}

/// Assemble a `CoreThread` exactly the way [`spawn_core`] does, minus the OS-thread spawn — so a
/// private method like [`CoreThread::enter_safe_state`] can be driven synchronously ON THE TEST
/// THREAD. That matters for `#[traced_test]`: its subscriber is a thread-local default, so the
/// fault event must fire on this thread to be captured.
///
/// DEFAULTS: a fresh `Conflated` market slot with zero drops, a snapshot cell holding
/// `CoreSnapshot::empty` for `primary`'s own venue and symbol, and a rejected-command counter at
/// zero — what `spawn_core` builds. `primary`, `extras` and `config` reach `assemble_core` as given.
pub(super) fn core_of<C: ExecutionClient>(
    primary: ExecutionEngine<C>,
    extras: Vec<(f64, ExecutionEngine<C>)>,
    config: CoreConfig,
) -> CoreThread<C> {
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&primary.venue, &primary.symbol)));
    assemble_core(primary, extras, config, market, snapshot, Arc::new(AtomicU64::new(0)))
}

/// [`core_of`] over one [`sim_engine_with`] `RecordingClient` engine and a caller-supplied
/// [`CoreConfig`] — for the tests that need what `CoreConfig::default()` does not allow: a mounted
/// strategy plus `readiness_gate`, a pre-set `deadman`, the `oco_cancel_sibling_on_dead_exit` knob,
/// a `state_dir`, a strategy factory, mount budgets.
///
/// DEFAULTS: those of [`sim_engine_with`], a fresh `RecordingClient` (it records but emits nothing
/// of its own, so an accepted order RESTS until a test says otherwise), and no extra engine. Every
/// config field the caller does not set stays at its inert default.
pub(super) fn core_with(config: CoreConfig) -> CoreThread<RecordingClient> {
    core_of(sim_engine_with(RecordingClient::default()), Vec::new(), config)
}

/// [`core_with`] over `CoreConfig::default()`.
///
/// DEFAULTS: those of [`core_with`], and every config field at its default — no mount, no journal,
/// no dead-man, no budget.
pub(super) fn test_core() -> CoreThread<RecordingClient> {
    core_with(CoreConfig::default())
}

/// The coid `mount_idx` owns — the strategy-submit path tags it.
///
/// ⚠ The three copies this replaced said "its first, in insertion order", and `coid_mount` is a
/// `HashMap`: with more than one coid attributed to the mount this answers ONE of them, not the
/// first. Every caller asks right after driving its mount into a single submit, where the two agree.
/// Panics when the mount has submitted nothing — for these callers that is the defect under test.
pub(super) fn coid_of(core: &CoreThread<RecordingClient>, mount_idx: usize) -> String {
    core.coid_mount
        .iter()
        .find(|&(_, &m)| m == mount_idx)
        .map(|(c, _)| c.clone())
        .expect("mount submitted at least one order")
}

/// Publish through the real guarded path and return the snapshot that landed in the cell.
pub(super) fn publish_now(
    core: &mut CoreThread<RecordingClient>,
) -> std::sync::Arc<vike_exec::CoreSnapshot> {
    core.publish_guarded();
    core.snapshot.load_full()
}

/// The signed `BOTH` size the engine at routing index `idx` holds under the position key
/// `(key, symbol)`, `0.0` when it holds none — the one read of a book's size the routing and
/// reconcile suites each spelled (their `held`/`held_qty` pin the key or the symbol on top of it).
pub(super) fn held_at(
    core: &CoreThread<RecordingClient>,
    idx: usize,
    key: &str,
    symbol: &str,
) -> f64 {
    core.eng(idx)
        .account
        .positions
        .get(&(
            ustr::Ustr::from(key),
            ustr::Ustr::from(symbol),
            vike_model::events::PositionSide::Both,
        ))
        .map(|p| p.size)
        .unwrap_or(0.0)
}
