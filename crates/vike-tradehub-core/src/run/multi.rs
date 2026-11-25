//! N strategies on ONE paper core: the multi-mount spec, its fill log, and its paper builder.

use std::sync::{Arc, Mutex};

use vike_core::{CoreConfig, CoreHandle, Mount, spawn_core_multi};
use vike_execution::{Account, BalanceMode, Engine, ExecutionClient, RiskGate};
use vike_model::Fill;
use vike_paper::ExecutionClientPaper;

use super::config::MountPlacement;
#[cfg(doc)]
use super::paper::build_paper_strategy_core_with;
use super::paper::{StrategyCorePaperConfig, paper_book};

/// One strategy on one series: the unit the MULTI-mount builders take N of (split-plane I10:
/// strategies sharing a venue account share a PROCESS). The single-mount builders' `strategy` +
/// [`MountPlacement`] pair, named so a call site says which slot is which.
///
/// ⚠ A distinct `spec.controller_id` per mount is the CALLER's job: `vike_core`'s `assemble_core`
/// PANICS on two mounts deriving one mount id (it would share a state sidecar and a journal key).
/// The daemon derives `{venue}__{symbol}__{interval}__{strategy-identity}` and refuses duplicates
/// at profile LOAD (`vike_tradehub::config::ProfileDaemon::validate`), naming both rows.
pub struct MountBuild {
    /// What the daemon's mount resolution returns for the profile's strategy file (or a boxed
    /// `strategies/market_making/spread_maker.rs`'s `SpreadMaker::from_params`).
    pub strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    /// The strategy-free half: venue / symbol / interval / seed + the paper fee scalars.
    pub spec: MountPlacement,
}

impl MountBuild {
    /// Lower into the core's [`Mount`] (the mapping [`crate::build_live_strategy_core`] and
    /// [`build_paper_strategy_core_with`] spell inline; the cross-exchange maker's live mount lowers
    /// through it, and so does a `.rs` mount the daemon hands the core once its build answers).
    pub fn into_mount(self) -> Mount {
        Mount {
            order_client_id_code: self.spec.order_client_id_code,
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
/// ([`MountMultiCore::fills`]' element; named for clippy's `type_complexity`).
pub type MountMultiFills = ((String, String), Arc<Mutex<Vec<Fill>>>);

/// The spawned multi-strategy PAPER mount: the [`CoreHandle`] plus each paper book's fill log, the
/// attribution a multi-mount test asserts on ("mount B's fill landed in mount B's book").
pub struct MountMultiCore {
    pub handle: CoreHandle,
    /// One per distinct `(venue, symbol)`: two strategies on one series share one book.
    pub fills: Vec<MountMultiFills>,
}

/// N strategies / venues / symbols on ONE paper core: the multi-mount twin of
/// [`build_paper_strategy_core_with`] (split-plane I10), the rehearsal path for a `[[mounts]]`
/// daemon profile. The layout answers the single-mount tripwire BY CONSTRUCTION:
///
/// * **one [`Engine`] per DISTINCT venue**: `spawn_core_multi` routes events and
///   `OrderIntent::Submit` by venue; each engine keeps its own `Account`/[`RiskGate`] (the
///   cross-venue firewall);
/// * **one [`ExecutionClientPaper`] per distinct `(venue, symbol)`**: a venue spanning several
///   symbols gets a [`vike_paper::ExecutionClientPaperMulti`] (routed by `request.symbol`, a miss
///   synthesizing the terminal `OrderRejected`) and the extra symbols land in
///   [`Engine::extra_symbols`] so their events fold. That is the switch [`MountPlacement::legs`]
///   demands: a single-symbol book stamps its OWN symbol on every fill and would hide misrouting.
///
/// A mount's declared `spec.legs` get a book each the same way, so a two-leg mount's fills land in
/// each leg's own book. A leg on ANOTHER venue (`vike_core::MountLeg::at`, the cross-exchange
/// maker's hedge) gets that venue's engine and book: the venue's own mounts' symbols first, then
/// the legs other mounts send there.
///
/// A venue engine's seed is the SUM of its mounts' `seed_cash`; a venue only a declared leg reaches
/// is seeded with the declaring mounts' `seed_cash` (each venue a mount trades is a paper account of
/// its own, at the mount's one seed). `CoreConfig::seed_cash` is the primary venue's sum, so the
/// drawdown denominator (`Σ seed + own PnL`) counts each row once. A set
/// `StrategyCorePaperConfig::account_seed` (the shipped daemon's one paper balance) replaces every one of those sums.
///
/// PANICS on an empty `mounts` (programmer error) and, via `assemble_core`, on two mounts sharing
/// a derived mount id (the daemon refuses that at LOAD: [`MountBuild`]).
pub fn build_paper_multi_strategy_core_with(
    mounts: Vec<MountBuild>,
    opts: StrategyCorePaperConfig,
) -> MountMultiCore {
    assert!(!mounts.is_empty(), "a multi-strategy paper mount needs at least one mount");
    let StrategyCorePaperConfig {
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
        order_owners,
        journal_restore,
        charge_paper_funding,
        funding_history,
        input_opener,
        session_rows,
        mounts_awaiting_strategy,
        account_seed,
        journal_forecasts,
        order_client_id_past_sessions,
        order_client_id_session_recorder,
    } = opts;
    // The venue a declared leg trades on: its own (`vike_core::MountLeg::at`), else the mount's.
    let leg_venue = |s: &MountPlacement, leg: &vike_core::MountLeg| {
        leg.venue.clone().unwrap_or_else(|| s.venue.clone())
    };
    // Distinct venues in declaration order, the mounts' own venues before a venue only a leg
    // reaches: engine order is observable (snapshot rows; the multi_mount reordering-stability
    // property).
    let mut venues: Vec<String> = Vec::new();
    let leg_venues = mounts.iter().flat_map(|m| m.spec.legs.iter().map(|l| leg_venue(&m.spec, l)));
    for venue in mounts.iter().map(|m| m.spec.venue.clone()).chain(leg_venues) {
        if !venues.contains(&venue) {
            venues.push(venue);
        }
    }
    let mut fills: Vec<MountMultiFills> = Vec::new();
    let mut engines: Vec<(f64, Engine<Box<dyn ExecutionClient + Send>>)> = Vec::new();
    for venue in &venues {
        let venue_specs: Vec<&MountPlacement> =
            mounts.iter().filter(|m| &m.spec.venue == venue).map(|m| &m.spec).collect();
        // The mounts sending a leg here from another venue.
        let leg_specs: Vec<&MountPlacement> = mounts
            .iter()
            .map(|m| &m.spec)
            .filter(|s| &s.venue != venue && s.legs.iter().any(|l| &leg_venue(s, l) == venue))
            .collect();
        // Distinct symbols in mount order, each mount's own before its declared legs, then the legs
        // other venues' mounts send here; the FIRST mount naming one supplies its book's fee/slippage
        // scalars.
        let mut symbol_specs: Vec<(&str, &MountPlacement)> = Vec::new();
        for s in venue_specs.iter().chain(leg_specs.iter()).copied() {
            let own = (&s.venue == venue).then_some(s.symbol.as_str());
            let legs = s.legs.iter().filter(|l| &leg_venue(s, l) == venue);
            for symbol in own.into_iter().chain(legs.map(|l| l.symbol.as_str())) {
                if !symbol_specs.iter().any(|(p, _)| *p == symbol) {
                    symbol_specs.push((symbol, s));
                }
            }
        }
        let seeded = if venue_specs.is_empty() { &leg_specs } else { &venue_specs };
        let seed: f64 = account_seed.unwrap_or_else(|| seeded.iter().map(|s| s.seed_cash).sum());
        let books: Vec<ExecutionClientPaper> =
            symbol_specs.iter().map(|(symbol, s)| paper_book(venue, symbol, s, &halt)).collect();
        for b in &books {
            fills.push(((venue.clone(), b.symbol.clone()), Arc::clone(&b.fills)));
        }
        let primary_symbol = symbol_specs[0].0.to_string();
        let extra_symbols: Vec<String> =
            symbol_specs[1..].iter().map(|(symbol, _)| symbol.to_string()).collect();
        let client: Box<dyn ExecutionClient + Send> = if books.len() == 1 {
            Box::new(books.into_iter().next().expect("exactly one book"))
        } else {
            let mut multi = vike_paper::ExecutionClientPaperMulti::new();
            for b in books {
                multi.add_book(b);
            }
            Box::new(multi)
        };
        let mut engine = Engine::new(
            Account::new(
                1.0,
                venue,
                crate::paper_contract_value_grid(
                    venue,
                    symbol_specs.iter().map(|(symbol, _)| *symbol),
                ),
                BalanceMode::Delta,
            ),
            RiskGate::new(risk_limits.clone()),
            client,
            venue,
            &primary_symbol,
        );
        engine.extra_symbols = extra_symbols;
        // A simulated book, and it says so: the journal's snapshot then names its tier, so the live
        // daemon's boot restore refuses a paper book instead of adopting it.
        engine.set_tier(vike_model::AccountTier::Paper);
        // Every spot pair it trades is `Cash`, as live books it (the spot rule).
        for (symbol, _) in &symbol_specs {
            crate::arming::admit_spot_cash(
                &mut engine.account,
                venue,
                symbol,
                vike_model::MarginMode::Cross,
            );
        }
        engines.push((seed, engine));
    }
    let (primary_seed, primary_engine) = engines.remove(0);
    let mut mount_strategy_files = std::collections::HashMap::new();
    let mut mount_fills_at_close = std::collections::HashSet::new();
    for m in &mounts {
        m.spec.name_strategy_file(&mut mount_strategy_files, &mut mount_fills_at_close);
    }
    let mut iter = mounts.into_iter();
    let first = iter.next().expect("asserted non-empty above");
    let mut config = CoreConfig {
        seed_cash: primary_seed,
        strategy: Some(first.into_mount()),
        extra_mounts: iter.map(MountBuild::into_mount).collect(),
        mount_strategy_files,
        mount_fills_at_close,
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        // Journal exactly as the single-mount builder does: the CALLER's resolved answer, with no
        // second resolution of its own (`StrategyCorePaperConfig::journal`).
        journal,
        journal_forecasts,
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        // `None` ⇒ runtime `Command::MountStrategy` refuses (the opts field).
        strategy_factory,
        // `None` ⇒ no order-ownership file (the opts field, decision 0113).
        order_owners,
        // Paper books do not survive a restart (the single-mount builder's note, decision 0127).
        restored_paper_orders_gone: true,
        // The paper engines pay funding from the venue's public rate (the opts field), and a restart
        // catches up the settlements they missed from the stored series.
        charge_paper_funding,
        funding_history,
        // A mount's inputs, read where the live arm reads them (the opts field).
        input_opener,
        // ...and the kept rows a restart's closed trades read a market's class from (the opts field).
        session_rows,
        // A mount whose strategy file is still building is parked by the core (the opts field).
        mounts_awaiting_strategy,
        // This install's past order client id sessions (the opts field).
        order_client_id_past_sessions,
        // ...and where the session it mints under is recorded before it mints (the opts field).
        order_client_id_session_recorder,
        ..CoreConfig::default()
    };
    if let Some(restore) = journal_restore {
        restore.apply_to(&mut config);
    }
    let handle = spawn_core_multi(primary_engine, engines, config);
    MountMultiCore { handle, fills }
}
