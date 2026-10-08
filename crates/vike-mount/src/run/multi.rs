//! N strategies on ONE paper core: the multi-mount spec, its fill log, and its paper builder.

use std::sync::{Arc, Mutex};

use vike_core::{CoreConfig, CoreHandle, StrategyMount, spawn_core_multi};
use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate};
use vike_paper::{PaperExecutionClient, PaperFill};

use super::config::MountSpec;
use super::paper::{PaperMountOpts, paper_client_for};
#[cfg(doc)]
use super::paper::{build_maker, build_paper_strategy_core_with};

/// One strategy on one series: the unit the MULTI-mount builders take N of (split-plane I10:
/// strategies sharing a venue account share a PROCESS). The single-mount builders' `strategy` +
/// [`MountSpec`] pair, named so a call site says which slot is which.
///
/// ⚠ A distinct `spec.controller_id` per mount is the CALLER's job: `vike_core`'s `assemble_core`
/// PANICS on two mounts deriving one mount id (it would share a state sidecar and a journal key).
/// The daemon derives `{venue}__{symbol}__{interval}__{strategy-identity}` and refuses duplicates
/// at profile LOAD (`vike_tradehub::config::DaemonProfile::validate`), naming both rows.
pub struct StrategyMountSpec {
    /// What `vike_strategy::strategy_by_name::<vike_core::LiveBroker>` (or [`build_maker`], boxed)
    /// returns.
    pub strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    /// The strategy-free half: venue / symbol / interval / seed + the paper fee scalars.
    pub spec: MountSpec,
}

impl StrategyMountSpec {
    /// Lower into the core's [`StrategyMount`] (the mapping [`build_live_strategy_core`] and
    /// [`build_paper_strategy_core_with`] spell inline).
    pub(super) fn into_mount(self) -> StrategyMount {
        StrategyMount {
            account: self.spec.account.clone(),
            symbols: self.spec.legs.clone(),
            controller_id: self.spec.controller_id.clone(),
            venue: self.spec.venue.clone(),
            symbol: self.spec.symbol.clone(),
            interval: self.spec.interval.clone(),
            strategy: self.strategy,
            underlying_symbol: self.spec.underlying_symbol.clone(),
        }
    }
}

/// One paper BOOK's fill log, keyed by the `(venue, symbol)` it stamps on its fills
/// ([`MultiStrategyMount::fills`]' element; named for clippy's `type_complexity`).
pub type BookFills = ((String, String), Arc<Mutex<Vec<PaperFill>>>);

/// The spawned multi-strategy PAPER mount: the [`CoreHandle`] plus each paper book's fill log, the
/// attribution a multi-mount test asserts on ("mount B's fill landed in mount B's book").
pub struct MultiStrategyMount {
    pub handle: CoreHandle,
    /// One per distinct `(venue, symbol)`: two strategies on one series share one book.
    pub fills: Vec<BookFills>,
}

/// N strategies / venues / symbols on ONE paper core: the multi-mount twin of
/// [`build_paper_strategy_core_with`] (split-plane I10), the rehearsal path for a `[[mounts]]`
/// daemon profile. The layout answers the single-mount tripwire BY CONSTRUCTION:
///
/// * **one [`ExecutionEngine`] per DISTINCT venue**: `spawn_core_multi` routes events and
///   `OrderIntent::Submit` by venue; each engine keeps its own `Account`/[`RiskGate`] (the
///   cross-venue firewall);
/// * **one [`PaperExecutionClient`] per distinct `(venue, symbol)`**: a venue spanning several
///   symbols gets a [`vike_paper::MultiPaperExecutionClient`] (routed by `request.symbol`, a miss
///   synthesizing the terminal `OrderRejected`) and the extra symbols land in
///   [`ExecutionEngine::extra_symbols`] so their events fold. That is the switch [`MountSpec::legs`]
///   demands: a single-symbol book stamps its OWN symbol on every fill and would hide misrouting.
///
/// Per-mount `spec.legs` still trips the single-mount tripwire: multi-symbol is across MOUNTS.
///
/// A venue engine's seed is the SUM of its mounts' `seed_cash`; `CoreConfig::seed_cash` is the
/// primary venue's sum, so the drawdown denominator (`Σ seed + own PnL`) counts each row once.
///
/// PANICS on an empty `mounts` (programmer error) and, via `assemble_core`, on two mounts sharing
/// a derived mount id (the daemon refuses that at LOAD: [`StrategyMountSpec`]).
pub fn build_paper_multi_strategy_core_with(
    mounts: Vec<StrategyMountSpec>,
    opts: PaperMountOpts,
) -> MultiStrategyMount {
    assert!(!mounts.is_empty(), "a multi-strategy paper mount needs at least one mount");
    let PaperMountOpts {
        risk_limits,
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        halt,
        // answers runtime `Command::MountStrategy` as the single-mount builder does.
        strategy_factory,
        journal,
    } = opts;
    // The single-mount tripwire, per mount: a per-mount LEG needs a routed multi-book at the
    // MOUNT level, which nothing here builds.
    for m in &mounts {
        assert!(
            m.spec.legs.is_empty(),
            "paper rehearsal of a multi-symbol mount needs MultiPaperExecutionClient at the mount \
             level: a single-symbol paper book stamps its own symbol onto every fill and would \
             hide the misrouting this rehearsal exists to catch"
        );
    }
    // Distinct venues in declaration order: engine order is observable (snapshot rows; the
    // multi_mount reordering-stability property).
    let mut venues: Vec<String> = Vec::new();
    for m in &mounts {
        if !venues.contains(&m.spec.venue) {
            venues.push(m.spec.venue.clone());
        }
    }
    let mut fills: Vec<BookFills> = Vec::new();
    let mut engines: Vec<(f64, ExecutionEngine<Box<dyn ExecutionClient + Send>>)> = Vec::new();
    for venue in &venues {
        let venue_specs: Vec<&MountSpec> =
            mounts.iter().filter(|m| &m.spec.venue == venue).map(|m| &m.spec).collect();
        // Distinct symbols in mount order; the FIRST mount naming one supplies its book's
        // fee/slippage scalars.
        let mut symbol_specs: Vec<&MountSpec> = Vec::new();
        for s in venue_specs.iter().copied() {
            if !symbol_specs.iter().any(|p| p.symbol == s.symbol) {
                symbol_specs.push(s);
            }
        }
        let seed: f64 = venue_specs.iter().map(|s| s.seed_cash).sum();
        let books: Vec<PaperExecutionClient> =
            symbol_specs.iter().map(|s| paper_client_for(s, &halt)).collect();
        for b in &books {
            fills.push(((venue.clone(), b.symbol.clone()), Arc::clone(&b.fills)));
        }
        let primary_symbol = symbol_specs[0].symbol.clone();
        let extra_symbols: Vec<String> =
            symbol_specs[1..].iter().map(|s| s.symbol.clone()).collect();
        let client: Box<dyn ExecutionClient + Send> = if books.len() == 1 {
            Box::new(books.into_iter().next().expect("exactly one book"))
        } else {
            let mut multi = vike_paper::MultiPaperExecutionClient::new();
            for b in books {
                multi.add_book(b);
            }
            Box::new(multi)
        };
        let mut engine = ExecutionEngine::new(
            Account::new(1.0, venue, None, BalanceMode::Delta),
            RiskGate::new(risk_limits.clone()),
            client,
            venue,
            &primary_symbol,
        );
        engine.extra_symbols = extra_symbols;
        engines.push((seed, engine));
    }
    let (primary_seed, primary_engine) = engines.remove(0);
    let mut iter = mounts.into_iter();
    let first = iter.next().expect("asserted non-empty above");
    let config = CoreConfig {
        seed_cash: primary_seed,
        strategy: Some(first.into_mount()),
        extra_mounts: iter.map(StrategyMountSpec::into_mount).collect(),
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        // Journal exactly as the single-mount builder does.
        // ⚠ The CALLER's resolved answer first (how `config.journal_dir` enables the WAL on a
        // paper mount), else the process env (what every caller passing no `journal` gets).
        journal: journal.or_else(vike_core::journal_config_from_env),
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        // `None` ⇒ runtime `Command::MountStrategy` refuses (the opts field).
        strategy_factory,
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(primary_engine, engines, config);
    MultiStrategyMount { handle, fills }
}
