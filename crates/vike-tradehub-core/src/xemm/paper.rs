//! The PAPER cross-exchange mount: two engines over two single-symbol paper books, one per venue,
//! built by the generic multi-leg paper builder.

use std::sync::Arc;

use vike_core::{CoreHandle, LiveBroker};
use vike_model::{Fill, Strategy, XemmParams};

use super::{XemmConfigError, validate_xemm_mount};
use crate::run::{
    MountBuild, MountPlacement, StrategyCorePaperConfig, StrategyCorePaperHalt,
    build_paper_multi_strategy_core_with,
};

/// A spawned PAPER cross-exchange mount: the live core with TWO engines and TWO paper books.
pub struct XemmCorePaper {
    pub handle: CoreHandle,
    /// Fills booked on the MAKER venue's paper book.
    pub maker_fills: Arc<std::sync::Mutex<Vec<Fill>>>,
    /// Fills booked on the TAKER venue's paper book; a misrouted hedge shows as empty here.
    pub hedge_fills: Arc<std::sync::Mutex<Vec<Fill>>>,
}

/// Stand the cross-exchange maker `maker` builds up on the PRODUCTION live core over TWO paper
/// exchanges, one per venue with its own `Account`/`RiskGate`/`vike_model::FeeSchedule`, through
/// [`build_paper_multi_strategy_core_with`] (one engine per venue on `vike_core::spawn_core_multi`,
/// the real cross-venue firewall; the hedge leg's venue seeded at the mount's `seed_cash`). No money,
/// credentials or network: the caller drives both venues' ticks.
///
/// Per-leg fees come from the registry, each book's at its own lane; `OrderBrokerSimulator` collapses maker and
/// taker to one rate, so this is the only honest offline rehearsal of `maker_fee_A + taker_fee_B`
/// economics.
///
/// ⚠ **BOTH books arm the operator HALT sentinel, and this seam is why arming has a GATE.** It was
/// the unarmed THIRD paper mount seam while `crates/vike-paper/src/client.rs`'s `halt_path` doc claimed
/// per-seam assertions in `crates/vike-tradehub-core/src/paper_fallback.rs`'s `paper_client` and
/// `crates/vike-tradehub-core/src/run/paper.rs`'s `paper_client_for` made that impossible;
/// `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs` now fails on an unclassified site.
/// Both books are now `crates/vike-tradehub-core/src/run/paper.rs`'s `paper_book`, the seam that gate
/// classifies, so this file constructs none.
///
/// ONE resolved path (`vike_boot::halt::halt_path_from_env`, as every mount): one file to
/// `touch`. The hedge leg matters as much: a halt stopping quotes but not hedges would drift
/// inventory as the real run never would. A TEST expecting fills uses
/// [`build_paper_xemm_core_with`]; [`StrategyCorePaperHalt`]'s doc has the measurement.
///
/// The maker is the CALLER's (no library builds a strategy): `maker` is handed the mount, `params`
/// and the round trip [`validate_xemm_mount`] priced the mount at, and is called only once
/// the mount validates.
pub fn build_paper_xemm_core(
    spec: &MountPlacement,
    params: &XemmParams,
    maker: impl FnOnce(&MountPlacement, &XemmParams, f64) -> Box<dyn Strategy<LiveBroker> + Send>,
) -> Result<XemmCorePaper, XemmConfigError> {
    build_paper_xemm_core_with(spec, params, &StrategyCorePaperHalt::ProcessWide, maker)
}

/// [`build_paper_xemm_core`] with the HALT sentinel named by the caller. For TESTS:
/// `crates/vike-tradehub-core/tests/scripted/xemm_scripted.rs` fails on any box holding an operator HALT file —
/// MEASURED on the CI box (see [`StrategyCorePaperHalt`]). A test pins a path it never creates; a daemon gets
/// [`StrategyCorePaperHalt::ProcessWide`].
pub fn build_paper_xemm_core_with(
    spec: &MountPlacement,
    params: &XemmParams,
    halt: &StrategyCorePaperHalt,
    maker: impl FnOnce(&MountPlacement, &XemmParams, f64) -> Box<dyn Strategy<LiveBroker> + Send>,
) -> Result<XemmCorePaper, XemmConfigError> {
    let total_fee = validate_xemm_mount(spec, params, None)?;
    // The books fill with the SAME lanes `validate_xemm_mount` priced `total_fee` off (`paper_book`
    // resolves each `(venue, symbol)`'s lane), so realized costs and the break-even offset agree.
    let strategy = maker(spec, params, total_fee);
    let core = build_paper_multi_strategy_core_with(
        vec![MountBuild { strategy, spec: spec.clone() }],
        StrategyCorePaperConfig { halt: halt.clone(), ..StrategyCorePaperConfig::default() },
    );
    let hedge = &spec.legs[0];
    let book = |venue: &str, symbol: &str| {
        let fills = core.fills.iter().find(|((v, s), _)| v == venue && s == symbol);
        Arc::clone(&fills.expect("the multi-leg builder books every leg").1)
    };
    let maker_fills = book(&spec.venue, &spec.symbol);
    let hedge_fills = book(hedge.venue.as_deref().unwrap_or(&spec.venue), &hedge.symbol);
    Ok(XemmCorePaper { handle: core.handle, maker_fills, hedge_fills })
}
