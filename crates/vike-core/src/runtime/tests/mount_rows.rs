//! The published mount rows are REUSED between publishes while nothing in them moved, and never
//! stale — the regression gates for `crates/vike-core/src/runtime/mount_rows.rs`.
//!
//! - one test per `MountView` field that moves only that field's source and asserts the NEXT publish
//!   carries it (position, realized PnL, unrealized PnL and notional, the residual row, `latched`,
//!   `budget`, `ready`, `params`), plus the cases the numbers cannot show (a mount swapped for another
//!   with identical numbers, a slot lost without a recompute);
//! - a seeded property test holding every published list bit-equal to a fresh `mount_views()` build
//!   over random event sequences, and requiring reuse whenever no row moved.
//!
//! White-box, in-crate like `runtime_mount_tests` (`use super::*` re-exports the runtime module);
//! the mount commands and the quoter factory are that suite's own helpers.

use super::runtime_mount_tests::{Step, coid_fill, mount_cmd, quoter_factory, spec, unmount_cmd};
use super::*;
use crate::runtime::test_support::{coid_of, core_with, mount_of};
use std::sync::Mutex as TestMutex;
use vike_exec::MountSpec;
use vike_exec::testing::RecordingClient;
use vike_model::StrategyParams;
use vike_model::events::FillEvent;

// ---- the published mount rows are REUSED between publishes while nothing in them moved ----------
//
// `CoreThread::publish` hands the snapshot the same mount-row list again when no row's NUMBERS
// changed. The risk of that is exactly one thing: a STALE row, a field that moved without the cache
// noticing. So every `MountView` field has a test that moves only its own source and asserts the
// NEXT publish carries it, and the property test at the end holds the published list equal to a
// fresh `mount_views()` build after every step of a random event sequence. Every comparison is on
// bits (`to_bits`) for the `f64`s.

/// A mount whose live params sit in a cell the test owns, so a re-tune is visible to `params()` the
/// way `Command::UpdateParams` makes it. Quotes once per feed status so a test can mint a coid.
struct Tunable {
    params: Arc<TestMutex<Option<StrategyParams>>>,
}

impl Strategy<LiveBroker> for Tunable {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
    }

    fn on_params_updated(&mut self, _broker: &mut LiveBroker, params: &StrategyParams) {
        *self.params.lock().unwrap() = Some(params.clone());
    }

    fn params(&self) -> Option<StrategyParams> {
        self.params.lock().unwrap().clone()
    }
}

/// An `Xemm` params bag whose only distinguishing knob is `qty`.
fn tuned(qty: f64) -> StrategyParams {
    StrategyParams::Xemm(vike_model::XemmParams { qty, ..vike_model::XemmParams::default() })
}

/// Re-tune the ONE mount on `(sim, symbol, 1m)` the way an unaddressed `Command::UpdateParams`
/// does (`mount_id: None`; refused, and nothing retuned, when two mounts share the triple).
fn retune(core: &mut CoreThread<RecordingClient>, symbol: &str, params: StrategyParams) {
    core.drive_strategy_params(&ParamsUpdate {
        venue: "sim".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        mount_id: None,
        params,
    });
}

/// A core with TWO live mounts (`sim` BTCUSDT slot 0, ETHUSDT slot 1), so the published list is
/// `[mount 0, mount 1, residual]`. `readiness_gate` seeds both Pending (and a Pending mount's
/// quote is discarded, so only a gate-off core has minted a coid for [`coid_of`]). The feed has been
/// told live.
fn two_tunable_mounts(readiness_gate: bool) -> CoreThread<RecordingClient> {
    let mount = |symbol: &str| {
        mount_of("sim", symbol, "1m", Box::new(Tunable { params: Arc::new(TestMutex::new(None)) }))
    };
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        readiness_gate,
        strategy: Some(mount("BTCUSDT")),
        extra_mounts: vec![mount("ETHUSDT")],
        strategy_factory: Some(tunable_factory()),
        ..CoreConfig::default()
    });
    core.engine.extra_symbols = vec!["ETHUSDT".into()];
    core.engine.collect_applied_fills = true;
    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    core
}

/// Publish through the real guarded path and return the snapshot that landed in the cell.
fn publish_now(core: &mut CoreThread<RecordingClient>) -> Arc<CoreSnapshot> {
    core.publish_guarded();
    core.snapshot.load_full()
}

/// Whether two snapshots share ONE mount-row list (the same allocation), not merely equal rows.
fn share_rows(a: &CoreSnapshot, b: &CoreSnapshot) -> bool {
    std::ptr::eq(a.mounts.as_ptr(), b.mounts.as_ptr())
}

/// The first field in which two rows differ, with the floats compared on BITS (`-0.0` differs from
/// `0.0`) and `budget`/`params` on their `Debug` text (which prints every float exactly).
///
/// Both rows are destructured EXHAUSTIVELY: a field added to `MountView` stops this compiling until
/// somebody decides how it is compared, so a new field cannot be skipped silently.
fn row_diff(got: &MountView, want: &MountView) -> Option<&'static str> {
    let MountView {
        kind: g_kind,
        mount_id: g_mount_id,
        venue: g_venue,
        symbol: g_symbol,
        interval: g_interval,
        ready: g_ready,
        position: g_position,
        realized_pnl: g_realized,
        unrealized_pnl: g_unrealized,
        notional: g_notional,
        budget: g_budget,
        latched: g_latched,
        params: g_params,
    } = got;
    let MountView {
        kind: w_kind,
        mount_id: w_mount_id,
        venue: w_venue,
        symbol: w_symbol,
        interval: w_interval,
        ready: w_ready,
        position: w_position,
        realized_pnl: w_realized,
        unrealized_pnl: w_unrealized,
        notional: w_notional,
        budget: w_budget,
        latched: w_latched,
        params: w_params,
    } = want;
    if g_kind != w_kind {
        return Some("kind");
    }
    if g_mount_id != w_mount_id {
        return Some("mount_id");
    }
    if g_venue != w_venue {
        return Some("venue");
    }
    if g_symbol != w_symbol {
        return Some("symbol");
    }
    if g_interval != w_interval {
        return Some("interval");
    }
    if g_ready != w_ready {
        return Some("ready");
    }
    for (name, g, w) in [
        ("position", g_position, w_position),
        ("realized_pnl", g_realized, w_realized),
        ("unrealized_pnl", g_unrealized, w_unrealized),
        ("notional", g_notional, w_notional),
    ] {
        if g.to_bits() != w.to_bits() {
            return Some(name);
        }
    }
    if format!("{g_budget:?}") != format!("{w_budget:?}") {
        return Some("budget");
    }
    if g_latched != w_latched {
        return Some("latched");
    }
    if format!("{g_params:?}") != format!("{w_params:?}") {
        return Some("params");
    }
    None
}

/// Every field of every row equal (see [`row_diff`]); `what` names the step in a failure.
fn assert_rows_equal(what: &str, got: &[MountView], want: &[MountView]) {
    assert_eq!(got.len(), want.len(), "{what}: row count: {got:?} vs {want:?}");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if let Some(field) = row_diff(g, w) {
            panic!("{what}: row {i} differs in `{field}`: {g:?} vs {w:?}");
        }
    }
}

/// Whether two lists are the same rows in every field (see [`row_diff`]).
fn rows_identical(a: &[MountView], b: &[MountView]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| row_diff(x, y).is_none())
}

/// The published list is what a FRESH build produces right now.
fn assert_published_is_fresh(what: &str, core: &CoreThread<RecordingClient>, snap: &CoreSnapshot) {
    assert_rows_equal(what, &snap.mounts, &core.mount_views());
}

/// Fold a fill for `coid` on mount `slot` (minting its attribution) and dispatch it.
fn fill_for_mount(
    core: &mut CoreThread<RecordingClient>,
    slot: usize,
    coid: &str,
    side: i32,
    qty: f64,
    px: f64,
) {
    core.coid_mount.insert(coid.to_string(), slot);
    core.bus.publish(Event::Fill(coid_fill(coid, side, qty, px)), &mut core.engine);
    core.dispatch_applied_fills();
}

#[test]
fn a_publish_with_nothing_changed_reuses_the_mount_rows() {
    let mut core = two_tunable_mounts(false);
    let first = publish_now(&mut core);
    let second = publish_now(&mut core);
    assert_eq!(first.mounts.len(), 3, "two mounts and the residual row");
    assert!(share_rows(&first, &second), "nothing moved, so the same row list is published again");
    assert!(Arc::ptr_eq(&first.mounts, &second.mounts), "...and it is one `Arc`, not a copy");
    assert_published_is_fresh("reused", &core, &second);
}

/// A core with no mount publishes ONE shared empty list: the N=0 publish allocates nothing for it.
#[test]
fn a_mountless_core_publishes_one_shared_empty_row_list() {
    let mut core = core_with(CoreConfig::default());
    let first = publish_now(&mut core);
    let second = publish_now(&mut core);
    assert!(first.mounts.is_empty() && second.mounts.is_empty(), "no mount, no residual row");
    assert!(Arc::ptr_eq(&first.mounts, &second.mounts), "the empty list is shared, not rebuilt");
}

#[test]
fn a_fill_moves_the_position_row_on_the_next_publish() {
    let mut core = two_tunable_mounts(false);
    let before = publish_now(&mut core);
    let coid = coid_of(&core, 0);
    fill_for_mount(&mut core, 0, &coid, 1, 3.0, 100.0);
    let after = publish_now(&mut core);
    assert!(!share_rows(&before, &after), "a fill must not be served the cached rows");
    assert_eq!(after.mounts[0].position, 3.0, "the fill's position reached the row");
    assert_eq!(after.mounts[1].position, 0.0, "the other mount's row is untouched");
    assert_published_is_fresh("after a fill", &core, &after);
}

#[test]
fn a_mark_move_moves_unrealized_pnl_and_notional_only() {
    let mut core = two_tunable_mounts(false);
    let coid = coid_of(&core, 0);
    fill_for_mount(&mut core, 0, &coid, 1, 3.0, 100.0);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 110.0, 1);
    let before = publish_now(&mut core);
    assert_eq!(before.mounts[0].unrealized_pnl, 30.0, "3 long, 100 -> 110");
    assert_eq!(before.mounts[0].notional, 330.0);

    core.engine.price_board.set_mark("sim", "BTCUSDT", 120.0, 2);
    let after = publish_now(&mut core);
    assert!(!share_rows(&before, &after), "a mark move must not be served the cached rows");
    assert_eq!(after.mounts[0].position, 3.0, "the position did not move");
    assert_eq!(after.mounts[0].unrealized_pnl, 60.0, "3 long, 100 -> 120");
    assert_eq!(after.mounts[0].notional, 360.0);
    assert_published_is_fresh("after a mark move", &core, &after);
}

#[test]
fn a_realized_close_moves_the_realized_pnl_row() {
    let mut core = two_tunable_mounts(false);
    let coid = coid_of(&core, 0);
    fill_for_mount(&mut core, 0, &coid, 1, 3.0, 100.0);
    let before = publish_now(&mut core);
    fill_for_mount(&mut core, 0, "close-0", -1, 3.0, 110.0);
    let after = publish_now(&mut core);
    assert!(!share_rows(&before, &after), "a realized close must not be served the cached rows");
    assert_eq!(after.mounts[0].realized_pnl, 30.0, "3 x (110 - 100), no fees");
    assert_eq!(after.mounts[0].position, 0.0);
    assert_published_is_fresh("after a close", &core, &after);
}

/// An UNATTRIBUTED round trip (an operator ticket: its coids were never minted by a mount) moves the
/// RESIDUAL row alone, which the cache must notice although no mount row changed.
#[test]
fn an_unattributed_fill_moves_only_the_residual_row() {
    let mut core = two_tunable_mounts(false);
    let before = publish_now(&mut core);
    for (coid, side, px) in [("manual-1", 1, 100.0), ("manual-2", -1, 120.0)] {
        core.bus.publish(Event::Fill(coid_fill(coid, side, 1.0, px)), &mut core.engine);
    }
    core.dispatch_applied_fills();
    let after = publish_now(&mut core);
    assert!(!share_rows(&before, &after), "the residual moved, so the list is rebuilt");
    let residual = after.mounts.last().expect("residual row");
    assert_eq!(residual.kind, vike_exec::MountRowKind::Residual);
    assert_eq!(residual.realized_pnl, 20.0, "the manual ticket's +20 belongs to no mount");
    assert_eq!(after.mounts[0].realized_pnl, 0.0);
    assert_published_is_fresh("after an unattributed fill", &core, &after);
}

#[test]
fn a_budget_latch_moves_the_latched_row() {
    let mut core = two_tunable_mounts(false);
    let before = publish_now(&mut core);
    core.mount_latched[1] = true;
    let after = publish_now(&mut core);
    assert!(!share_rows(&before, &after), "a latch must not be served the cached rows");
    assert!(!after.mounts[0].latched && after.mounts[1].latched);
    assert_published_is_fresh("after a latch", &core, &after);
}

#[test]
fn a_budget_change_moves_the_budget_row() {
    let mut core = two_tunable_mounts(false);
    let before = publish_now(&mut core);
    let budget = MountBudget { max_loss: Some(5.0), max_notional: None, flatten_on_breach: false };
    core.mount_budget[0] = Some(budget);
    let after = publish_now(&mut core);
    assert!(!share_rows(&before, &after), "a budget change must not be served the cached rows");
    assert_eq!(after.mounts[0].budget, Some(budget));
    assert_eq!(after.mounts[1].budget, None);
    assert_published_is_fresh("after a budget", &core, &after);

    let wider = MountBudget { max_loss: Some(5.0), max_notional: Some(1.0), ..budget };
    core.mount_budget[0] = Some(wider);
    let wider_pub = publish_now(&mut core);
    assert_eq!(wider_pub.mounts[0].budget, Some(wider), "ONE arm of the budget moved");
    assert_published_is_fresh("after the second budget", &core, &wider_pub);
}

#[test]
fn readiness_moves_the_ready_row() {
    let mut core = two_tunable_mounts(true);
    let before = publish_now(&mut core);
    assert!(!before.mounts[0].ready && !before.mounts[1].ready, "both Pending");
    core.engine.price_board.set_quote("sim", "BTCUSDT", 99.0, 101.0, 0);
    core.maintain_mount_readiness(0);
    let after = publish_now(&mut core);
    assert!(!share_rows(&before, &after), "a readiness flip must not be served the cached rows");
    assert!(after.mounts[0].ready, "slot 0's symbol priced, so it is Ready");
    assert!(!after.mounts[1].ready, "slot 1's symbol is still unpriced");
    assert_published_is_fresh("after readiness", &core, &after);
}

#[test]
fn a_live_retune_moves_the_params_row() {
    let mut core = two_tunable_mounts(false);
    let before = publish_now(&mut core);
    assert_eq!(before.mounts[0].params, None);
    retune(&mut core, "BTCUSDT", tuned(1.0));
    let first = publish_now(&mut core);
    assert!(!share_rows(&before, &first), "a re-tune must not be served the cached rows");
    assert_eq!(first.mounts[0].params, Some(tuned(1.0)));
    assert_eq!(first.mounts[1].params, None, "the other mount was not addressed");
    retune(&mut core, "BTCUSDT", tuned(2.0));
    let second = publish_now(&mut core);
    assert_eq!(second.mounts[0].params, Some(tuned(2.0)), "a second re-tune, same variant");
    assert_published_is_fresh("after a re-tune", &core, &second);
}

/// A mount and an unmount change the LIST (rows appear, vanish, move against the residual), which
/// no row's numbers can show: `recompute_mount_gates` bumps the counter the cache keys on.
#[test]
fn a_runtime_mount_and_unmount_change_the_published_list() {
    let step = Arc::new(TestMutex::new(Step::Idle));
    let fills = Arc::new(TestMutex::new(Vec::new()));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy_factory: Some(quoter_factory("RT", &step, &fills, None, &loaded)),
        ..CoreConfig::default()
    });
    let empty = publish_now(&mut core);
    assert!(empty.mounts.is_empty(), "no mount, no residual row");

    core.dispatch(mount_cmd(spec("rt-a")));
    let one = publish_now(&mut core);
    assert_eq!(one.mounts.len(), 2, "the mount and the residual");
    assert_published_is_fresh("after a mount", &core, &one);

    core.dispatch(mount_cmd(MountSpec { symbol: "ETHUSDT".into(), ..spec("rt-b") }));
    let two = publish_now(&mut core);
    assert_eq!(two.mounts.len(), 3);
    assert_eq!(two.mounts[1].symbol, "ETHUSDT", "the new mount sits before the residual");
    assert_published_is_fresh("after a second mount", &core, &two);

    core.dispatch(unmount_cmd("rt-a"));
    let after_unmount = publish_now(&mut core);
    assert_eq!(after_unmount.mounts.len(), 2, "the tombstoned mount's row is gone");
    assert_eq!(after_unmount.mounts[0].symbol, "ETHUSDT");
    assert_published_is_fresh("after an unmount", &core, &after_unmount);

    core.dispatch(unmount_cmd("rt-b"));
    let none = publish_now(&mut core);
    assert!(none.mounts.is_empty(), "the last mount went, and the residual row with it");
}

/// A slot a strategy-hook panic leaves `None` takes NO recompute with it, so the cache cannot rely
/// on the counter: the live-slot count alone must give the list up.
#[test]
fn a_slot_lost_without_a_recompute_drops_its_row() {
    let mut core = two_tunable_mounts(false);
    let before = publish_now(&mut core);
    core.mounts[1] = None;
    let after = publish_now(&mut core);
    assert!(!share_rows(&before, &after));
    assert_eq!(after.mounts.len(), 2, "one mount and the residual");
    assert_published_is_fresh("after a lost slot", &core, &after);
}

/// A mount factory that builds a [`Tunable`] for every spec, so a test can mount at runtime.
fn tunable_factory() -> StrategyFactory {
    Box::new(|_spec: &MountSpec| Ok(Box::new(Tunable { params: Arc::new(TestMutex::new(None)) })))
}

/// REALIZED PnL alone: a position opened AND closed on one mount between two publishes (one drained
/// burst of fills). Afterwards position, unrealized PnL and notional are back at `0`, and the
/// residual row is unchanged (the account and the mount moved by the same amount), so the mount's
/// `realized_pnl` is the ONLY number that moved. A reuse check that skipped it would serve the old
/// `0.0`.
#[test]
fn a_position_opened_and_closed_between_publishes_moves_only_realized_pnl() {
    let mut core = two_tunable_mounts(false);
    let before = publish_now(&mut core);
    core.coid_mount.insert("open-0".to_string(), 0);
    core.coid_mount.insert("close-0".to_string(), 0);
    core.bus.publish(Event::Fill(coid_fill("open-0", 1, 1.0, 100.0)), &mut core.engine);
    core.bus.publish(Event::Fill(coid_fill("close-0", -1, 1.0, 110.0)), &mut core.engine);
    core.dispatch_applied_fills();
    let after = publish_now(&mut core);

    let (b, a) = (&before.mounts[0], &after.mounts[0]);
    // the premise: nothing else moved
    assert_eq!(a.position.to_bits(), b.position.to_bits(), "flat again");
    assert_eq!(a.unrealized_pnl.to_bits(), b.unrealized_pnl.to_bits());
    assert_eq!(a.notional.to_bits(), b.notional.to_bits());
    let (b_res, a_res) = (before.mounts.last().unwrap(), after.mounts.last().unwrap());
    assert_eq!(a_res.realized_pnl.to_bits(), b_res.realized_pnl.to_bits(), "residual unchanged");
    assert_eq!(after.mounts[1].realized_pnl.to_bits(), before.mounts[1].realized_pnl.to_bits());
    // the claim
    assert_eq!(a.realized_pnl, 10.0, "1 x (110 - 100), no fees");
    assert!(!share_rows(&before, &after), "a realized gain must not be served the cached rows");
    assert_published_is_fresh("after open + close", &core, &after);
}

/// The LIST changes while every number stays put: mount A is unmounted and a flat mount B on another
/// symbol is mounted between two publishes. The live count is the same and every number is
/// identical (all zero, ready, no params), so only the epoch `recompute_mount_gates` bumps can tell
/// the cache that the row now describes a different mount.
#[test]
fn swapping_a_mount_for_one_with_identical_numbers_changes_the_published_row() {
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy_factory: Some(tunable_factory()),
        ..CoreConfig::default()
    });
    core.dispatch(mount_cmd(spec("rt-a")));
    let before = publish_now(&mut core);
    assert_eq!(before.mounts.len(), 2, "the mount and the residual");
    assert_eq!(before.mounts[0].symbol, "BTCUSDT");

    core.dispatch(unmount_cmd("rt-a"));
    core.dispatch(mount_cmd(MountSpec { symbol: "ETHUSDT".into(), ..spec("rt-b") }));
    let after = publish_now(&mut core);

    // the premise: same live count, every NUMBER of the row identical
    assert_eq!(after.mounts.len(), before.mounts.len());
    let (b, a) = (&before.mounts[0], &after.mounts[0]);
    assert_eq!(
        [a.position, a.realized_pnl, a.unrealized_pnl, a.notional].map(f64::to_bits),
        [b.position, b.realized_pnl, b.unrealized_pnl, b.notional].map(f64::to_bits),
    );
    assert!(a.ready == b.ready && a.latched == b.latched && a.budget == b.budget);
    assert!(a.params == b.params);
    // the claim
    assert_eq!(a.symbol, "ETHUSDT", "the published row is the NEW mount");
    assert!(!share_rows(&before, &after), "a swapped mount must not be served the cached rows");
    assert_published_is_fresh("after a swap", &core, &after);
}

/// A row's `mount_id` is compared ON ITS OWN, not only through the epoch: the slot keeps its
/// numbers, its triple and the epoch, and only the id the core holds for it changes, so nothing but
/// the id compare in `mount_rows_unchanged` can see that the cached row names another mount. (No
/// production path renames a live slot; this pins the row-level identity check, which a slot swap
/// that skipped `recompute_mount_gates` would need.)
#[test]
fn a_changed_mount_id_with_identical_numbers_changes_the_published_row() {
    let mut core = two_tunable_mounts(false);
    let before = publish_now(&mut core);
    assert_eq!(
        before.mounts[0].mount_id,
        crate::strategy_state::mount_id_of("sim", "BTCUSDT", "1m")
    );
    let epoch = core.mount_epoch;

    core.mount_ids[0] = "renamed".to_string();
    let after = publish_now(&mut core);

    // the premise: the epoch did not move, and every other field of the row is the same
    assert_eq!(core.mount_epoch, epoch, "no recompute ran");
    let (b, a) = (&before.mounts[0], &after.mounts[0]);
    assert_eq!(row_diff(a, &MountView { mount_id: a.mount_id.clone(), ..b.clone() }), None);
    // the claim
    assert_eq!(a.mount_id, "renamed", "the published row carries the id the core holds now");
    assert!(!share_rows(&before, &after), "a changed mount id must not be served the cached rows");
    assert_published_is_fresh("after a changed id", &core, &after);
}

/// A tiny deterministic generator (an LCG), so a failing seed replays from the step number.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }

    fn side(&mut self) -> i32 {
        if self.below(2) == 0 { 1 } else { -1 }
    }
}

/// The symbols the property test's mounts and marks draw from.
const SYMBOLS: [&str; 3] = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];

/// What the property test has done so far: ids to keep fills and mounts unique, and which mounts a
/// later unmount can name.
struct World {
    minted: u64,
    mounted: u64,
    live: Vec<String>,
}

/// ONE random event on the core: a fill on any slot (a dead slot's too), a mark move, a latch, a
/// budget, a re-tune, an unattributed fill, a readiness flip, a runtime mount or an unmount.
fn random_event(
    core: &mut CoreThread<RecordingClient>,
    rng: &mut Lcg,
    world: &mut World,
    step: i64,
) {
    let symbol = SYMBOLS[rng.below(3) as usize];
    match rng.below(12) {
        0..=2 => {
            let slot = rng.below(core.mounts.len() as u64) as usize;
            world.minted += 1;
            let (side, px) = (rng.side(), 90.0 + rng.below(40) as f64);
            fill_for_mount(core, slot, &format!("p{}", world.minted), side, 1.0, px);
        }
        3 | 4 => {
            let px = 90.0 + rng.below(40) as f64;
            core.engine.price_board.set_mark("sim", symbol, px, step);
        }
        5 => {
            let slot = rng.below(core.mounts.len() as u64) as usize;
            core.mount_latched[slot] = !core.mount_latched[slot];
        }
        6 => {
            let slot = rng.below(core.mounts.len() as u64) as usize;
            core.mount_budget[slot] = match rng.below(3) {
                0 => None,
                1 => Some(MountBudget {
                    max_loss: Some(rng.below(9) as f64),
                    ..MountBudget::default()
                }),
                _ => Some(MountBudget {
                    max_notional: Some(rng.below(9) as f64),
                    flatten_on_breach: rng.below(2) == 0,
                    ..MountBudget::default()
                }),
            };
        }
        7 => {
            let qty = 1.0 + rng.below(4) as f64;
            retune(core, symbol, tuned(qty));
        }
        8 => {
            world.minted += 1;
            let side = rng.side();
            let fill = FillEvent {
                commission: rng.below(3) as f64,
                ..coid_fill(&format!("m{}", world.minted), side, 1.0, 100.0)
            };
            core.bus.publish(Event::Fill(fill), &mut core.engine);
            core.dispatch_applied_fills();
        }
        9 => {
            core.engine.price_board.set_quote("sim", symbol, 99.0, 101.0, step);
            core.maintain_mount_readiness(step);
        }
        10 => {
            world.mounted += 1;
            let id = format!("rt-{}", world.mounted);
            core.dispatch(mount_cmd(MountSpec { symbol: symbol.into(), ..spec(&id) }));
            world.live.push(id);
        }
        _ => {
            if !world.live.is_empty() {
                let id = world.live.swap_remove(rng.below(world.live.len() as u64) as usize);
                core.dispatch(unmount_cmd(&id));
            }
        }
    }
}

/// PROPERTY: whatever happens, the published list equals a fresh build, and a publish after which
/// nothing a row shows has moved (and no mount came or went) publishes the SAME list again. Between
/// two publishes the test applies 0 to 3 random events (so a single-event bias cannot hide a case
/// where two changes cancel or compound), over an alphabet that includes events on both slots,
/// readiness flips and runtime mounts and unmounts. A seeded generator, not `proptest`: the steps
/// need the core's own state, and a failing seed replays from the step number in the message.
#[test]
fn the_published_mount_rows_always_equal_a_fresh_build() {
    for seed in [1_u64, 2, 3, 0x9E37_79B9_7F4A_7C15] {
        let mut rng = Lcg(seed);
        // the readiness gate ON, so a readiness flip is something that can happen
        let mut core = two_tunable_mounts(true);
        let mut world = World {
            minted: 0,
            mounted: 0,
            live: vec![
                crate::strategy_state::mount_id_of("sim", "BTCUSDT", "1m"),
                crate::strategy_state::mount_id_of("sim", "ETHUSDT", "1m"),
            ],
        };
        let mut previous = publish_now(&mut core);
        let mut previous_epoch = core.mount_epoch;
        let (mut reused, mut rebuilt) = (0, 0);
        for step in 0..400_i64 {
            let what = format!("seed {seed:#x} step {step}");
            let events = rng.below(4);
            for _ in 0..events {
                random_event(&mut core, &mut rng, &mut world, step);
            }
            let now = publish_now(&mut core);
            assert_published_is_fresh(&what, &core, &now);
            let unchanged =
                core.mount_epoch == previous_epoch && rows_identical(&previous.mounts, &now.mounts);
            if events == 0 {
                assert!(unchanged, "{what}: no event, so no row can have moved");
            }
            if unchanged {
                assert!(
                    share_rows(&previous, &now),
                    "{what}: nothing moved, so the list is reused"
                );
                reused += 1;
            } else {
                rebuilt += 1;
            }
            previous = now;
            previous_epoch = core.mount_epoch;
        }
        // the generator must exercise both outcomes, or the test proves little
        assert!(reused > 20 && rebuilt > 20, "seed {seed:#x}: reused {reused}, rebuilt {rebuilt}");
    }
}
