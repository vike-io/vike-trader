//! `vike_exec::RiskLimits::grid_by_symbol` had NO producer in production — every mount left it
//! empty, so the per-symbol PRICE/SIZE GRID the `RiskGate` consults on every `check` could only
//! ever be populated by a unit test. `vike_mount::make_engine_with_legs` is that producer; this
//! file pins its two ends through the REAL public entry points.
//!
//! Both scenarios run over an EMPTY credentials map (`HashMap::new()`), so every venue mounts its
//! PAPER fallback — no network, no creds, CI-safe — which is also the condition under which the
//! acceptance property is most interesting: the wiring must be inert.
//!
//! What is deliberately NOT here: the two arms that actually POPULATE the map (hyperliquid and
//! cTrader — the `vike_mount::DeclaredGridSource::InHand` pair) need live credentials and a live
//! handshake, so their fill is unreachable from network-free CI. The composition they perform is
//! pinned instead against the pure `symbol_grid::declared_symbol_grids` in
//! `crates/vike-mount/src/symbol_grid.rs`'s own `#[cfg(test)]` module — including
//! `a_two_symbol_mount_judges_each_leg_on_its_own_lot`, the end-to-end
//! grid → `RiskGate` → verdict proof — exactly the same network-free-CI split
//! `risk_profile_wiring.rs` documents for `merge_operator_budget`.

use std::collections::{HashMap, HashSet};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// A machine policy whose ARMING CEILING permits [`VENUE`] — with an EMPTY credential map, so the
/// mount is still paper by the older gate.
///
/// ⚠ Load-bearing rather than boilerplate. A `paper` ceiling (the default, and what `None` means)
/// returns the paper engine from a SEPARATE assembly at the top of `make_engine_with_legs`, above
/// the venue arms entirely — so without this every scenario in this file would exercise that early
/// return instead of the leg machinery it exists to pin, and the delegation equality below would
/// compare one function to itself.
fn armed() -> vike_mount::MountPolicy {
    vike_mount::MountPolicy {
        venues: vike_config::VenuePolicy::default().declare(VENUE, vike_config::VenueMode::Demo),
        ..vike_mount::MountPolicy::default()
    }
}

/// A DELEGATION-DRIFT guard, and nothing more — said plainly, because an earlier draft of this
/// comment claimed otherwise.
///
/// ⚠ **This test cannot fail today, and that is a property of the code rather than of the test.**
/// `vike_mount::make_engine` is a one-line delegation to `make_engine_with_legs(venue, symbol, &[],
/// …)`, so both halves of the equality below run the IDENTICAL body: there is no second
/// implementation for them to disagree about. The earlier doc claimed it would catch "a delegation
/// that reordered the fold against `merge_operator_budget`, or that armed a default only on one
/// path", which describes a two-implementation world this crate deliberately does not have.
///
/// It is kept because that world is one refactor away: the moment somebody re-implements
/// `make_engine` independently — to skip the leg machinery on the hot single-symbol path, say —
/// this becomes the assertion that the two entry points still agree, over the WHOLE `RiskLimits`
/// rather than just `grid_by_symbol`. The real single-symbol acceptance condition is proved by
/// `an_empty_declaration_never_touches_the_venue`, which panics on any venue lookup, and by
/// `the_mounted_symbol_is_never_re_gridded`.
#[test]
fn the_single_symbol_entry_point_is_the_no_legs_case_of_the_multi_symbol_one() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars: HashMap<String, String> = HashMap::new();

    let policy = armed();

    let mut live_a = HashSet::new();
    let (plain, _) = vike_mount::make_engine(
        VENUE,
        SYMBOL,
        &vars,
        &tx,
        &mut live_a,
        false,
        None,
        None,
        None,
        Some(&policy),
    )
    .expect("a paper mount must never refuse to start");

    let mut live_b = HashSet::new();
    let (with_legs, _) = vike_mount::make_engine_with_legs(
        VENUE,
        SYMBOL,
        &[],
        &vars,
        &tx,
        &mut live_b,
        false,
        None,
        None,
        None,
        Some(&policy),
    )
    .expect("a paper mount must never refuse to start");

    assert!(plain.gate.limits.grid_by_symbol.is_empty(), "no legs ⇒ no per-symbol grid");
    assert_eq!(
        plain.gate.limits, with_legs.gate.limits,
        "make_engine must be exactly make_engine_with_legs over an empty leg list"
    );
    assert_eq!(live_a, live_b, "…and must reach the same live/paper verdict");
}

/// A mount that DECLARES a leg on a venue whose arm cannot grid one still starts, still mounts, and
/// carries an EMPTY map — the degrade is inert, never a refusal and never a fabricated grid.
///
/// binance is a `vike_mount::DeclaredGridSource::PerSymbolFetch` venue (its only per-symbol source
/// is a symbol-scoped blocking round trip, which a mount deliberately does not add per leg — see
/// `vike_mount::symbol_grid`'s module doc), and with no credentials it mounts paper regardless. The
/// ⚠ The leg is therefore NOT warned about and there are no mount scalars to fall back to — an
/// earlier draft of this comment claimed both, and neither happens in THIS scenario. A paper mount
/// fetched nothing, so `limits` is `RiskLimits::new()`'s all-`None` grid, `carries_a_venue_grid`
/// returns false, and `warn_ungridded_legs` returns before emitting anything — exactly as its own
/// doc says it must ("a warning would be pure noise on the most common mount in the tree"). The
/// warning path is worth pinning where `carries_a_venue_grid` is TRUE, which is a unit test beside
/// that function, not here. What this test proves is narrower and still worth proving: a mount that
/// declares a leg its arm cannot grid STARTS, and files no row.
///
/// NON-VACUOUS in the direction that can actually regress: it asserts the map is EMPTY rather than
/// merely "not panicking", so an implementation that fabricated a row from
/// `SymbolProperties::default()` — an all-`None` `SymbolGrid`, invisible to `grid_for` but a lie to
/// the operator and to `warn_ungridded_legs` — fails. It also asserts the mount SUCCEEDS, so a
/// future "refuse a leg we cannot grid" would have to be a deliberate, reviewed change here.
#[test]
fn a_declared_leg_an_arm_cannot_grid_leaves_the_mount_inert_and_started() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars: HashMap<String, String> = HashMap::new();
    let mut live = HashSet::new();
    let legs = vec!["ETHUSDT".to_string()];

    let (engine, recon) = vike_mount::make_engine_with_legs(
        VENUE,
        SYMBOL,
        &legs,
        &vars,
        &tx,
        &mut live,
        false,
        None,
        None,
        None,
        Some(&armed()),
    )
    .expect("a declared leg must never stop a mount from starting");

    assert!(recon.is_none(), "absent creds ⇒ paper, no reconcile handle");
    assert!(live.is_empty(), "absent creds ⇒ venue not marked live");
    assert!(
        engine.gate.limits.grid_by_symbol.is_empty(),
        "an arm that cannot resolve a leg's grid must declare NOTHING for it, not a zeroed row"
    );
    assert_eq!(
        vike_mount::declared_grid_source(VENUE),
        vike_mount::DeclaredGridSource::PerSymbolFetch,
        "…and this is WHY: binance's only per-symbol grid source is a blocking round trip"
    );
}
