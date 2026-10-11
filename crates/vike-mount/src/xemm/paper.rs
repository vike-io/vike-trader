//! The PAPER cross-exchange mount: two engines over two single-symbol paper books, one per venue.

use std::sync::Arc;

use vike_core::{CoreConfig, CoreHandle, StrategyMount, spawn_core_multi};
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
#[cfg(doc)]
use vike_model::FeeSchedule;
use vike_model::RiskLimits;
use vike_paper::{PaperExecutionClient, PaperFill};

use super::{XemmConfigError, XemmMountConfig, build_xemm_maker, xemm_mount_legs};
use crate::run::PaperHalt;

/// A spawned PAPER cross-exchange mount: the live core with TWO engines and TWO paper books.
pub struct PaperXemmMount {
    pub handle: CoreHandle,
    /// Fills booked on the MAKER venue's paper book.
    pub maker_fills: Arc<std::sync::Mutex<Vec<PaperFill>>>,
    /// Fills booked on the TAKER venue's paper book; a misrouted hedge shows as empty here.
    pub hedge_fills: Arc<std::sync::Mutex<Vec<PaperFill>>>,
}

/// Stand the cross-exchange maker up on the PRODUCTION live core over TWO paper exchanges, one per
/// venue with its own `Account`/`RiskGate`/[`FeeSchedule`], through [`spawn_core_multi`] (the real
/// cross-venue firewall). No money, credentials or network: the caller drives both venues' ticks.
///
/// Per-leg fees come from the registry; `SimBroker` collapses maker and taker to one rate, so this
/// is the only honest offline rehearsal of `maker_fee_A + taker_fee_B` economics.
///
/// ⚠ **BOTH books arm the operator HALT sentinel, and this seam is why arming has a GATE.** It was
/// the unarmed THIRD paper mount seam while `crates/vike-paper/src/lib.rs`'s `halt_path` doc claimed
/// per-seam assertions in `crates/vike-mount/src/paper_fallback.rs`'s `paper_client` and
/// `crates/vike-mount/src/run/paper.rs`'s `paper_client_for` made that impossible;
/// `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs` now fails on an unclassified site.
///
/// ONE resolved path (`vike_bridge_core::halt::halt_path_from_env`, as every mount): one file to
/// `touch`. The hedge leg matters as much: a halt stopping quotes but not hedges would drift
/// inventory as the real run never would. A TEST expecting fills uses
/// [`build_paper_xemm_core_with`]; [`PaperHalt`]'s doc has the measurement.
pub fn build_paper_xemm_core(cfg: &XemmMountConfig) -> Result<PaperXemmMount, XemmConfigError> {
    build_paper_xemm_core_with(cfg, &PaperHalt::ProcessWide)
}

/// The MAKER and HEDGE paper books [`build_paper_xemm_core`] mounts, both armed with `halt`.
/// Separate so `the_xemm_paper_mount_arms_both_books` asserts on the CONSTRUCTED books (as for
/// `paper_fallback::paper_client` and `run::paper_client_for`): once moved into their
/// `ExecutionEngine`s they are unreachable from [`PaperXemmMount`].
pub(super) fn xemm_paper_books(
    cfg: &XemmMountConfig,
    halt: &PaperHalt,
) -> (PaperExecutionClient, PaperExecutionClient) {
    let (maker_schedule, hedge_schedule) = cfg.schedules();
    // Resolve ONCE and clone: says "one path" at the call site rather than relying on memoization.
    let halt_path = halt.resolve();
    let maker = PaperExecutionClient::with_fee_schedule(
        &cfg.maker_venue,
        &cfg.maker_symbol,
        cfg.slippage,
        maker_schedule,
    )
    .with_halt_path(halt_path.clone());
    let hedge = PaperExecutionClient::with_fee_schedule(
        &cfg.taker_venue,
        &cfg.hedge_symbol,
        cfg.slippage,
        hedge_schedule,
    )
    .with_halt_path(halt_path);
    (maker, hedge)
}

/// [`build_paper_xemm_core`] with the HALT sentinel named by the caller. For TESTS:
/// `crates/vike-mount/tests/xemm_scripted.rs` fails on any box holding an operator HALT file —
/// MEASURED on the CI box (see [`PaperHalt`]). A test pins a path it never creates; a daemon gets
/// [`PaperHalt::ProcessWide`].
pub fn build_paper_xemm_core_with(
    cfg: &XemmMountConfig,
    halt: &PaperHalt,
) -> Result<PaperXemmMount, XemmConfigError> {
    let total_fee = cfg.validate(None)?;

    // The books fill with the SAME `schedules()` `validate` priced `total_fee` off, so realized
    // costs and the break-even offset agree on each leg's LANE.
    let (maker_client, hedge_client) = xemm_paper_books(cfg, halt);
    let maker_fills = Arc::clone(&maker_client.fills);
    let hedge_fills = Arc::clone(&hedge_client.fills);

    let maker_engine = ExecutionEngine::new(
        Account::new(1.0, &cfg.maker_venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        maker_client,
        &cfg.maker_venue,
        &cfg.maker_symbol,
    );
    let hedge_engine = ExecutionEngine::new(
        Account::new(1.0, &cfg.taker_venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        hedge_client,
        &cfg.taker_venue,
        &cfg.hedge_symbol,
    );

    let config = CoreConfig {
        seed_cash: cfg.maker_seed_cash,
        strategy: Some(StrategyMount {
            account: None,
            venue: cfg.maker_venue.clone(),
            symbol: cfg.maker_symbol.clone(),
            interval: cfg.interval.clone(),
            strategy: Box::new(build_xemm_maker(cfg, total_fee)),
            symbols: xemm_mount_legs(cfg),
            underlying_symbol: None,
            controller_id: None,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(maker_engine, vec![(cfg.taker_seed_cash, hedge_engine)], config);
    Ok(PaperXemmMount { handle, maker_fills, hedge_fills })
}
