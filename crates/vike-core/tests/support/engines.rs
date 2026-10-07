//! Engine and core-config builders.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, LiveBroker};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::Strategy;

use crate::kit::mounts::mount_of;

/// An engine on `venue`/`symbol` driving `client` — the ONE spelling of the default engine shape
/// every builder below goes through.
///
/// DEFAULTS: contract multiplier `1.0` (`Account::new`'s first argument — NOT a seed: `spawn_core`
/// sets the equity seed from `CoreConfig::seed_cash`), `BalanceMode::Delta`, `RiskLimits::new()`
/// (no limits), and the account labelled with the engine's own `venue` (so `route_key` is `venue`
/// too). A test whose subject is one of those — an Authoritative wallet, a risk ceiling, a
/// multiplier, an overridden `route_key`, `extra_symbols` — builds its engine itself.
pub(crate) fn engine_on<C: ExecutionClient>(
    venue: &str,
    symbol: &str,
    client: C,
) -> ExecutionEngine<C> {
    ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        venue,
        symbol,
    )
}

/// [`engine_on`] over a BOXED client, so engines whose clients differ in type share one
/// `ExecutionEngine<Box<dyn ExecutionClient + Send>>` and can ride one multi-engine core.
///
/// DEFAULTS: those of [`engine_on`]. A separate builder rather than [`engine_on`] called with a box,
/// because the parameter's type is what makes `Box::new(SomeClient)` coerce at the call site: a
/// generic parameter would infer `Box<SomeClient>`, which is not an `ExecutionClient` — only the
/// `dyn` box is.
pub(crate) fn dyn_engine(
    venue: &str,
    symbol: &str,
    client: Box<dyn ExecutionClient + Send>,
) -> ExecutionEngine<Box<dyn ExecutionClient + Send>> {
    engine_on(venue, symbol, client)
}

/// [`engine_on`] on the sim venue, for a test that brings its own client.
///
/// DEFAULTS: those of [`engine_on`], with venue `"sim"` and symbol `"BTCUSDT"`.
pub(crate) fn sim_engine_with<C: ExecutionClient>(client: C) -> ExecutionEngine<C> {
    engine_on("sim", "BTCUSDT", client)
}

/// [`sim_engine_with`] a fresh `RecordingClient`.
///
/// DEFAULTS: those of [`sim_engine_with`], and the client. `RecordingClient` records but emits
/// NOTHING of its own, which is what makes it the right client for most core tests: an order it
/// accepted stays non-terminal until a test says otherwise, i.e. it RESTS — the state a book is in
/// when a daemon is stopped (`TestExecutionClient` fills every submit immediately, so nothing would
/// ever be resting) — and every state change a scenario makes rides the exec lane as a JOURNALED
/// `Ingest` record, which is what lets a no-op replay client re-derive it.
pub(crate) fn sim_engine() -> ExecutionEngine<RecordingClient> {
    sim_engine_with(RecordingClient::default())
}

/// A core config with a self-advancing clock: every call advances an `AtomicI64` by one, so message
/// `k` dispatches at `now_ms == k` and no real wall-clock wait is needed to reach a future instant.
///
/// DEFAULTS: every other field is `CoreConfig::default()` — no strategy mount, no journal. Built
/// with struct-update rather than by reassigning fields of a `Default::default()` value, which the
/// workspace clippy gate forbids.
pub(crate) fn test_config(seed_cash: f64) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    }
}

/// A core config mounting `strategy` on binance/BTCUSDT/1m, with [`test_config`]'s self-advancing
/// clock.
///
/// DEFAULTS: the mount carries no account label, no extra legs, no controller id and no underlying
/// symbol; every other config field is `CoreConfig::default()`. A test whose mount shape is its
/// subject (legs, a controller, a venue of its own) builds its `StrategyMount` itself.
pub(crate) fn binance_mount_config(
    seed_cash: f64,
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(mount_of("binance", "BTCUSDT", "1m", strategy)),
        ..CoreConfig::default()
    }
}
