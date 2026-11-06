//! Engine and core-config builders.

use std::path::Path;

use vike_core::{CoreConfig, JournalConfig, LiveBroker};
use vike_exec::testing::{RecordingClient, engine_on};
use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate};
use vike_journal::JournalFileConfig;
use vike_model::RiskLimits;
use vike_model::Strategy;

use vike_core::testkit::{mount_of, test_config};

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

/// [`sim_engine`] whose risk gate carries a price/size grid (`tick`, `lot`) and the engine-wide initial
/// margin `im_requirement` — the engine the per-symbol-margin suites admit their symbols onto.
///
/// DEFAULTS: those of [`sim_engine`], except the limits: every `RiskLimits` field other than the three
/// named here is `RiskLimits::new()`'s.
pub(crate) fn margined_sim_engine(
    tick: f64,
    lot: f64,
    im_requirement: f64,
) -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(tick),
        lot_size: Some(lot),
        im_requirement: Some(im_requirement),
        ..RiskLimits::new()
    };
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

/// A USD account on the sim venue: `quote_asset` is set BEFORE the core spawns, so the engine's
/// ledger currency is the dollar and a yen-quoted symbol is a FOREIGN one.
///
/// DEFAULTS: those of [`sim_engine`], with `quote_asset` `"USD"`.
pub(crate) fn usd_sim_engine() -> ExecutionEngine<RecordingClient> {
    let mut engine = sim_engine();
    engine.quote_asset = "USD".into();
    engine
}

/// A JOURNALED core config over `dir` with equity `seed_cash`, cutting a cadence `Snap` every
/// `snapshot_every` records (the cadence decides where the replay BASE falls, so it stays the
/// caller's), on `vike_core::testkit::test_config`'s self-advancing clock (message `k` dispatches at `now_ms == k`).
///
/// DEFAULTS: coid session `("cafef00d", 0)`, 1 MiB segments flushed every 8 records; every other field
/// is `CoreConfig::default()` (no strategy mount).
pub(crate) fn journaled_config(dir: &Path, seed_cash: f64, snapshot_every: u64) -> CoreConfig {
    CoreConfig {
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every,
        }),
        ..test_config(seed_cash)
    }
}

/// A core config mounting `strategy` on binance/BTCUSDT/1m, with `vike_core::testkit::test_config`'s self-advancing
/// clock.
///
/// DEFAULTS: the mount carries no account label, no extra legs, no controller id and no underlying
/// symbol; every other config field is `CoreConfig::default()`. A test whose mount shape is its
/// subject (legs, a controller, a venue of its own) builds its `StrategyMount` itself.
pub(crate) fn binance_mount_config(
    seed_cash: f64,
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
) -> CoreConfig {
    CoreConfig {
        strategy: Some(mount_of("binance", "BTCUSDT", "1m", strategy)),
        ..test_config(seed_cash)
    }
}
