//! **A position opened FROM FLAT books the ASSET's ruling margin mode, not the type default.**
//!
//! `Account::fold` is the sole writer of `PositionEntry`, and its prior-entry lookup used to be a
//! bare `unwrap_or_default()` — so every brand-new position was born `MarginMode::Cross`, because
//! that is what `MarginMode::default()` is. Venue-blind and asset-blind.
//!
//! That is wrong on hyperliquid, whose `meta.universe` publishes `onlyIsolated` PER ASSET: on such
//! an asset there is no cross option, so the mode that rules is `Isolated`, and the venue itself
//! agrees (`vike_hyperliquid::recon_client`'s `parse_positions` maps `leverage.type == "isolated"`
//! → `Isolated`). #1087 derived that per-asset truth as
//! `vike_hyperliquid::symbology::InstrumentRef::effective_margin_mode`; this file gates the OTHER
//! half — that the fold consumes it.
//!
//! ## Why the fold and not reconcile
//! Nothing downstream repairs a mis-booked mode on this venue:
//! - `ExecutionEngine::apply_snapshot` DOES let venue truth win, via
//!   `ReconcileSnapshot::position_margin` (gated by `reconcile_margin_mode.rs`) — but hyperliquid
//!   builds no `ReconcileSnapshot` at all; only binance/bybit/okx perp populate that vec.
//! - Hyperliquid reconciles through the `recon::diff`/`resolve` lane instead, which never reads
//!   `PositionStatusReport::margin_mode` (no `Divergence` variant is margin-shaped) and heals a
//!   size gap by synthesizing `Fill`s — which re-enter this very fold and inherit the same prior.
//!
//! So the mode is wrong for the life of the session, and it is READ: `ExecutionEngine::submit_order`
//! excludes non-cross positions from the gate's shared `margin_used`, `check_margin_call_priced`
//! partitions the liquidation pools on it, and `vike_core::snapshot`'s liquidation-price badge
//! routes on it. A walled-off position mis-booked cross is charged against equity it does not
//! consume and shown a whole-account liquidation price.
//!
//! ## The shape being pinned
//! The grid is SPARSE — only symbols whose mode differs from `Cross` get a row — so the assertions
//! below deliberately cover the absent-row and empty-grid cases too: those are the byte-identical
//! paths that 13 of the 14 roster venues take, and a change that "fixed" the isolated case by
//! defaulting everything to something else would break them.

use indexmap::IndexMap;
use vike_exec::{Account, BalanceMode, PositionKey};
use vike_model::events::FillEvent;
use vike_model::MarginMode;

const VENUE: &str = "hyperliquid";
/// The one isolated-only hyperliquid asset still LIVE as of 2026-08-05 (the other eight —
/// HPOS/RLB/UNIBOT/OX/FRIEND/SHIA/NFTI/PANDORA — are delisted). Named so the case reads as
/// reachable rather than historical.
const ISOLATED_ONLY: &str = "CASHCAT";
const ORDINARY: &str = "BTC";

fn key(symbol: &str) -> PositionKey {
    (VENUE.into(), symbol.into(), "BOTH".into())
}

/// Per-test-process fill counter, so every `fill(..)` this file builds carries a DISTINCT
/// `trade_id`. Load-bearing since the dedup ledger moved onto `Account`: `apply_fill` is now
/// idempotent per `trade_id`, so a shared `"t1"` made the 2nd and 3rd fill of a sequence refusals
/// rather than folds — which is also what a REAL venue stream would have done, so the old hardcoded
/// id was the test lying about the fold it was measuring, not the guard being wrong.
static FILL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn fill(symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    let n = FILL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FillEvent {
        trade_id: vike_model::events::TradeId::prefixed("t", n),
        client_order_id: "c1".to_string(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

/// The grid `vike_mount::margin_mode_grid` builds for a mount on an isolated-only asset: one row,
/// the mounted symbol.
fn isolated_grid(symbol: &str) -> IndexMap<String, MarginMode> {
    let mut g = IndexMap::new();
    g.insert(symbol.to_string(), MarginMode::Isolated);
    g
}

fn account(grid: Option<IndexMap<String, MarginMode>>) -> Account {
    Account::new(1.0, VENUE, None, BalanceMode::Delta).with_default_margin_modes(grid)
}

fn position_mode(a: &Account, symbol: &str) -> MarginMode {
    a.positions.get(&key(symbol)).unwrap_or_else(|| panic!("no position for {symbol}")).margin_mode
}

/// **THE GATE.** The first fill on an isolated-only asset opens the position ISOLATED. Before the
/// fold consulted the grid this asserted `Cross`, and no later pass on this venue would have
/// changed it.
#[test]
fn a_position_opened_from_flat_takes_the_assets_ruling_margin_mode() {
    let mut a = account(Some(isolated_grid(ISOLATED_ONLY)));
    a.apply_fill(&fill(ISOLATED_ONLY, 1, 10.0, 0.02));

    assert_eq!(
        position_mode(&a, ISOLATED_ONLY),
        MarginMode::Isolated,
        "an isolated-only asset must be born Isolated, not MarginMode::default()"
    );
    assert_ne!(
        position_mode(&a, ISOLATED_ONLY),
        vike_model::caps_for(VENUE).default_margin_mode,
        "and it must DISAGREE with the per-VENUE default — that inequality IS the bug (#1087)"
    );
}

/// The mode survives subsequent fills into the same position: the open-from-flat read happens ONCE,
/// and every fill after it carries the prior forward (the pre-existing `Account::fold` law). Also
/// the cost claim, asserted as behavior: a fill into an existing position never consults the grid,
/// so a grid that disagrees with the standing position cannot rewrite it.
#[test]
fn the_mode_is_read_once_at_birth_and_carried_forward() {
    let mut a = account(Some(isolated_grid(ISOLATED_ONLY)));
    a.apply_fill(&fill(ISOLATED_ONLY, 1, 10.0, 0.02));
    a.apply_fill(&fill(ISOLATED_ONLY, 1, 5.0, 0.03));
    a.apply_fill(&fill(ISOLATED_ONLY, -1, 3.0, 0.04));

    let p = a.positions[&key(ISOLATED_ONLY)];
    assert_eq!(p.size, 12.0, "sanity: the size fold is untouched");
    assert_eq!(p.margin_mode, MarginMode::Isolated, "mode must survive every later fill");
}

/// A CLOSE to flat then a re-open re-reads the grid — the position is gone from the map, so the
/// next fill is an open-from-flat like any other. Pinned because `compute_fill` leaves a `size ==
/// 0.0` ENTRY behind rather than removing the key, which means the re-open actually takes the
/// carry-forward branch; either way the answer must be `Isolated`, and this asserts it rather than
/// leaving it to inspection.
#[test]
fn a_reopen_after_flat_is_still_isolated() {
    let mut a = account(Some(isolated_grid(ISOLATED_ONLY)));
    a.apply_fill(&fill(ISOLATED_ONLY, 1, 10.0, 0.02));
    a.apply_fill(&fill(ISOLATED_ONLY, -1, 10.0, 0.02)); // flat
    a.apply_fill(&fill(ISOLATED_ONLY, 1, 4.0, 0.05)); // re-open

    assert_eq!(position_mode(&a, ISOLATED_ONLY), MarginMode::Isolated);
}

/// A symbol with NO row in the grid is `Cross` — the sparse-grid contract. The grid carries only
/// what differs, so an ordinary hyperliquid asset mounted alongside an isolated-only one must not
/// inherit its neighbour's mode.
#[test]
fn a_symbol_absent_from_the_grid_is_cross() {
    let mut a = account(Some(isolated_grid(ISOLATED_ONLY)));
    a.apply_fill(&fill(ORDINARY, 1, 1.0, 60_000.0));

    assert_eq!(
        position_mode(&a, ORDINARY),
        MarginMode::Cross,
        "no row ⇒ the per-venue default, not the other symbol's mode"
    );
    assert_eq!(position_mode(&a, ORDINARY), vike_model::caps_for(VENUE).default_margin_mode);
}

/// **The byte-identical path**, which is every venue but hyperliquid and every mount that never
/// installs a grid: no grid ⇒ `Cross`, exactly as before this wiring existed. Asserted through BOTH
/// doors — never calling the builder, and calling it with `None` — because `make_engine` reaches
/// the second one on all 13 other venues.
#[test]
fn no_grid_is_byte_identical_cross() {
    for a in [
        &mut Account::new(1.0, VENUE, None, BalanceMode::Delta),
        &mut account(None),
        &mut account(Some(IndexMap::new())),
    ] {
        a.apply_fill(&fill(ISOLATED_ONLY, 1, 10.0, 0.02));
        assert_eq!(position_mode(a, ISOLATED_ONLY), MarginMode::Cross);
        assert_eq!(a.default_margin_mode_of(ISOLATED_ONLY), MarginMode::Cross);
    }
}

/// The accessor's own contract, independent of the fold: present row wins, absent row is `Cross`.
#[test]
fn default_margin_mode_of_reads_the_grid() {
    let a = account(Some(isolated_grid(ISOLATED_ONLY)));
    assert_eq!(a.default_margin_mode_of(ISOLATED_ONLY), MarginMode::Isolated);
    assert_eq!(a.default_margin_mode_of(ORDINARY), MarginMode::Cross);
    assert_eq!(a.default_margin_mode_of("NOT-A-SYMBOL"), MarginMode::Cross);
}

/// The consequence, gated where it is actually read: an isolated position is EXCLUDED from the
/// account-wide margin fold that the admitting gate and the margin-call watchdog both run
/// (`Account::margin_in_use_by`'s `is_cross()` filter). Mis-booking it cross is therefore not a
/// cosmetic label — it charges shared equity the position does not consume.
#[test]
fn the_mis_booked_mode_would_have_charged_shared_equity() {
    let mut iso = account(Some(isolated_grid(ISOLATED_ONLY)));
    iso.apply_fill(&fill(ISOLATED_ONLY, 1, 100.0, 1.0));
    iso.set_mark_from(VENUE, ISOLATED_ONLY, 1.0, vike_exec::MarkSource::VenueMark, 0);

    let mut cross = account(None); // the pre-fix booking of the SAME fills
    cross.apply_fill(&fill(ISOLATED_ONLY, 1, 100.0, 1.0));
    cross.set_mark_from(VENUE, ISOLATED_ONLY, 1.0, vike_exec::MarkSource::VenueMark, 0);

    let used = |a: &Account| a.margin_in_use_by(|_k, p| p.margin_mode.is_cross().then_some(0.1));
    assert_eq!(used(&iso), 0.0, "isolated ⇒ its own wallet backs it, not the shared pool");
    assert_eq!(used(&cross), 10.0, "cross ⇒ 100 · 1.0 · 1 · 0.1 charged against shared equity");
}
