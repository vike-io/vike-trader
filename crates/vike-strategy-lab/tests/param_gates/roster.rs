//! The roster both param gates check (`param_gates.rs` and `param_keys_gate.rs`, which compiles this
//! same file), and every other shipped file outside the strategy gallery with the reason it is left
//! out: written once, so the two gates cannot disagree about which files they cover.
//!
//! The roster is the eight shipped taker files the deleted registry had a `ParamKeys::Declared` row
//! for (its `PARAM_KEYS`, over `PORTABLE_STRATEGIES`); the declaration is each file's header
//! `[params]` now. The makers were `ParamKeys::NotEnumerated` and stay out, as before; so do the
//! three takers the registry kept `SIMULATOR_ONLY`. [`unclassified`] holds both lists to the
//! shipped folder, so a file that joins the shipped set is on one of them by a written decision,
//! never silently checked by neither gate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_strategy::file::StrategyFileHeader;
use vike_strategy::file::scan::scan;
use vike_strategy_lab::gallery;

pub(crate) const BUY_HOLD: &str = "trend/buy_hold.rs";
pub(crate) const GRID: &str = "grid_dca/grid.rs";
pub(crate) const DCA_ACCUMULATE: &str = "grid_dca/dca_accumulate.rs";
pub(crate) const TRAILING_SCALPER: &str = "trend/trailing_scalper.rs";
pub(crate) const MOMENTUM: &str = "trend/momentum.rs";
pub(crate) const FUNDING_CARRY: &str = "funding/funding_carry.rs";
pub(crate) const FUNDING_CAPTURE: &str = "funding/funding_capture.rs";
pub(crate) const PAIRS_ZSCORE: &str = "pairs/pairs_zscore.rs";

/// The files both gates check, each by its path under `strategies/` (the shipped file's identity,
/// as the folder scan spells it).
pub(crate) const ROSTER: &[&str] = &[
    BUY_HOLD,
    GRID,
    DCA_ACCUMULATE,
    TRAILING_SCALPER,
    MOMENTUM,
    FUNDING_CARRY,
    FUNDING_CAPTURE,
    PAIRS_ZSCORE,
];

/// Every other shipped file outside the gallery, with why neither gate checks it.
pub(crate) const OUT: &[(&str, &str)] = &[
    (
        "market_making/spread_maker.rs",
        "the registry's `spread_maker` and `gueant_maker` rows (one file now, `spread_model = \
         \"gueant\"`) were `ParamKeys::NotEnumerated`: a roster file is driven key by key against \
         the scripted market, which the maker's conditional knobs (kappa fits, settlement, \
         underlying marks) cannot all be. Its header is held to its reader by \
         `crates/vike-strategy-lab/tests/makers/header.rs`'s \
         `the_spread_maker_declares_exactly_the_keys_its_reader_reads`",
    ),
    (
        "market_making/cross_exchange_maker.rs",
        "a maker, on no registry roster (it was mounted by the tradehub core, never by name), out \
         for the spread maker's reason. Its header is held to its reader by \
         `crates/vike-strategy-lab/tests/makers/header.rs`'s \
         `the_cross_exchange_maker_declares_exactly_the_keys_its_reader_reads`",
    ),
    (
        "prediction_markets/cheap_catch_updown_fair_value.rs",
        "the registry kept it `SIMULATOR_ONLY`: it submits only on a trade print, which the \
         scripted bar-and-quote market never plays, so `the_scripted_market_moves_every_strategy` \
         cannot hold for it (giving the script a trade feed is a trading-surface change: \
         `docs/decisions/0075-the-param-gate-feed-is-a-trading-surface-change.md`)",
    ),
    (
        "prediction_markets/sport_copy_follower.rs",
        "the registry kept it `SIMULATOR_ONLY`, for the cheap-catch file's reason: it submits only \
         from `on_trade_tick`, on a signal series the replay harness synthesizes",
    ),
    (
        "portfolio/rotation_top_k.rs",
        "the registry kept it `SIMULATOR_ONLY`: it ranks the run's universe, and its header declares \
         no `[params]` (it reads no key), so there is nothing for either gate to check",
    ),
    (
        "options/covered_call.rs",
        "an options file: it trades only the legs a backtest profile's `[data.options]` table reads \
         from a stored chain, on the event lane, and the scripted bar-and-quote market plays no chain; \
         its header is held to its reader by `crates/vike-strategy-lab/tests/takers/headers.rs`'s \
         `the_declared_defaults_are_what_an_empty_table_resolves_to`",
    ),
    (
        "other/example.rs",
        "the smallest complete file, the one a user copies, never a registry name: \
         `crates/vike-strategy-lab/tests/example.rs` drives it",
    ),
];

/// The shipped strategies folder, from `CARGO_MANIFEST_DIR` (never CWD).
fn strategies() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../strategies")
}

/// The text of the shipped file `file` (its path under `strategies/`).
pub(crate) fn source(file: &str) -> String {
    let path = strategies().join(file);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read `{}`: {e}", path.display()))
}

/// The header of the shipped file `file`, parsed and checked.
pub(crate) fn header(file: &str) -> StrategyFileHeader {
    StrategyFileHeader::parse(&source(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// Where [`ROSTER`], [`OUT`] and the shipped folder disagree: a shipped file outside the gallery on
/// neither list, a listed file that is no shipped file (moved or deleted), a file on both lists.
/// EMPTY when the two lists partition the shipped files outside the gallery.
pub(crate) fn unclassified() -> Vec<String> {
    let listed = scan(&strategies(), None, None);
    assert!(listed.unreadable.is_empty(), "unreadable folders: {:?}", listed.unreadable);
    let shipped: BTreeSet<String> = listed
        .files
        .into_iter()
        .map(|f| f.path)
        .filter(|path| !gallery::FILES.iter().any(|g| g.path == path.as_str()))
        .collect();
    let named: BTreeSet<String> =
        ROSTER.iter().chain(OUT.iter().map(|(file, _)| file)).map(|f| f.to_string()).collect();
    let mut out: Vec<String> =
        shipped.difference(&named).map(|f| format!("{f}: shipped, on neither list")).collect();
    out.extend(
        named.difference(&shipped).map(|f| format!("{f}: listed, but no such shipped file")),
    );
    out.extend(
        ROSTER
            .iter()
            .filter(|file| OUT.iter().any(|(left_out, _)| left_out == *file))
            .map(|file| format!("{file}: on both lists")),
    );
    out
}
