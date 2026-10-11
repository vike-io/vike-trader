//! ORDER OWNERSHIP across a RESTART (decision 0113): a core records which mount owns each order it
//! mints, and each mount's ledger, in `crate::order_owners`' file; a NEW core assembled over the same
//! file gives a pre-restart order's fill back to the mount that placed it.
//!
//! Every "restart" here is the real one minus the process: the first core is DROPPED (its log drains,
//! flushes and joins its writer), and a second core is assembled through `assemble_core` over a new
//! log on the same file. Mounts are matched by MOUNT ID, so a reordered profile, a mount that comes
//! back only as a runtime mount, and an explicit unmount each get their own test.
//!
//! The assertions are about OWNERSHIP RESTORED — the mount's `coid_mount` entry, its ledger, its
//! `on_fill` — which is what main lacks today: a pre-restart order's fill there falls back to the
//! FIRST mount on the pair and books into no ledger. Decision 0116 (the unattributed-fill PR) makes
//! the converse true as well: without the restored entry such a fill would reach no mount at all.

use super::*;
use crate::order_owners::{ORDER_OWNERS_FILE, OrderOwnerLog};
use crate::scratch::Scratch;
use std::sync::Mutex as TestMutex;
use vike_exec::testing::RecordingClient;

use super::runtime_mount_tests::{Step, coid_fill, mount_cmd, quoter_factory, spec, unmount_cmd};
use crate::runtime::test_support::core_with;

/// The core clock every core here runs on: records are stamped with it and the boot's
/// time-to-live is measured back from it, so nothing ages out between the two cores.
pub(super) const NOW: i64 = 1_700_000_000_000;

/// What every mount heard through `on_fill`: `(label, fill size)`.
type Heard = Arc<TestMutex<Vec<(&'static str, f64)>>>;

/// The side the mount quotes on the next `on_feed_status` (`1` buys at 100, `-1` sells at 110),
/// consumed by that quote.
pub(super) type NextQuote = Arc<TestMutex<Option<i32>>>;

/// A profile-mount strategy: quotes when told to and records each fill it hears.
struct Owner {
    label: &'static str,
    next: NextQuote,
    heard: Heard,
}

impl Strategy<LiveBroker> for Owner {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        if let Some(side) = self.next.lock().unwrap().take() {
            broker.submit_limit_tagged("q", side, 1.0, if side > 0 { 100.0 } else { 110.0 });
        }
    }

    fn on_fill(&mut self, _broker: &mut LiveBroker, fill: &Fill) {
        self.heard.lock().unwrap().push((self.label, fill.size));
    }
}

/// An [`Owner`] mount on `(sim, BTCUSDT, 1m)` under its own controller id.
fn owner(label: &'static str, cid: &str, next: &NextQuote, heard: &Heard) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: Some(cid.to_string()),
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(Owner { label, next: Arc::clone(next), heard: Arc::clone(heard) }),
    }
}

pub(super) fn cell() -> NextQuote {
    Arc::new(TestMutex::new(None))
}

fn heard() -> Heard {
    Arc::new(TestMutex::new(Vec::new()))
}

/// The config of one "process": `mounts` in order (the first is `CoreConfig::strategy`) and the
/// ownership file in `dir`.
pub(super) fn config_on(dir: &Scratch, mounts: Vec<StrategyMount>) -> CoreConfig {
    let mut it = mounts.into_iter();
    CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(|| NOW),
        strategy: it.next(),
        extra_mounts: it.collect(),
        order_owners: Some(OrderOwnerLog::in_state_dir(dir)),
        ..CoreConfig::default()
    }
}

/// Assemble `config` and arm the primary engine's fill capture, as `spawn_core_multi` does for a
/// core with mounts.
pub(super) fn assembled(config: CoreConfig) -> CoreThread<RecordingClient> {
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;
    core
}

/// Tell the mount behind `next` to quote `side`, and return the coid it was given.
pub(super) fn quote(core: &mut CoreThread<RecordingClient>, next: &NextQuote, side: i32) -> String {
    *next.lock().unwrap() = Some(side);
    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    core.engine.client.submissions.last().expect("the mount quoted").client_order_id.clone()
}

/// A venue fill for `coid`, folded and delivered.
fn fill(core: &mut CoreThread<RecordingClient>, coid: &str, side: i32, qty: f64, px: f64) {
    core.bus.publish(Event::Fill(coid_fill(coid, side, qty, px)), &mut core.engine);
    core.dispatch_applied_fills();
}

pub(super) fn noted(core: &CoreThread<RecordingClient>, needle: &str) -> bool {
    core.recent.iter().any(|l| l.contains(needle))
}

/// 1 — **The restart.** Mount B rests an order; the process restarts with the same profile; the
/// order's fill reaches B's `on_fill` and B's ledger, and A hears nothing.
#[test]
fn a_resting_orders_fill_after_a_restart_reaches_the_mount_that_placed_it() {
    let dir = Scratch::reserved("vco-owners-restart");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut first = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    let coid = quote(&mut first, &nb, 1);
    drop(first);

    let mut second = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    assert_eq!(second.coid_mount.get(&coid), Some(&1), "B owns its pre-restart order again");
    fill(&mut second, &coid, 1, 1.0, 100.0);

    assert_eq!(h.lock().unwrap().clone(), vec![("B", 1.0)], "only B hears its order's fill");
    assert_eq!(second.mount_attr[1].size, 1.0, "the fill folded into B's ledger");
    assert_eq!(second.mount_attr[0].size.to_bits(), 0.0_f64.to_bits(), "A's ledger is untouched");
}

/// 2 — **Slot indices are not identities.** The profile lists the mounts the other way round after
/// the restart; ownership follows the mount ID, so B (now slot 0) still gets its fill.
#[test]
fn ownership_follows_the_mount_id_when_the_profile_is_reordered() {
    let dir = Scratch::reserved("vco-owners-reorder");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut first = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    let coid = quote(&mut first, &nb, 1);
    drop(first);

    let mut second = assembled(config_on(
        &dir,
        vec![owner("B", "maker-b", &nb, &h), owner("A", "maker-a", &na, &h)],
    ));
    assert_eq!(second.mount_ids[0], "maker_b");
    assert_eq!(second.coid_mount.get(&coid), Some(&0), "B is slot 0 now, and owns its order");
    fill(&mut second, &coid, 1, 1.0, 100.0);

    assert_eq!(h.lock().unwrap().clone(), vec![("B", 1.0)]);
    assert_eq!(second.mount_attr[0].size, 1.0, "B's ledger, on B's new slot");
}

/// 3 — **A mount dropped from the profile keeps its ownership waiting.** After the restart no
/// mount of B's id exists: B's entries are held (a recent-events note says so), the file keeps
/// them, and a fill on the pair reaches nobody. A later RUNTIME mount of B's id takes them back —
/// the resting order AND the ledger — and the next fill reaches it.
#[test]
fn a_mount_missing_at_boot_keeps_its_entries_until_a_mount_of_its_id_arrives() {
    let dir = Scratch::reserved("vco-owners-missing-mount");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut first = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    let bought = quote(&mut first, &nb, 1);
    fill(&mut first, &bought, 1, 1.0, 100.0);
    let resting = quote(&mut first, &nb, -1);
    drop(first);
    h.lock().unwrap().clear();

    let step = Arc::new(TestMutex::new(Step::Idle));
    let rt_heard: Heard = heard();
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut config = config_on(&dir, Vec::new());
    config.strategy_factory = Some(quoter_factory("RT", &step, &rt_heard, None, &loaded));
    let mut second = assembled(config);

    let held = second.pending_owners.get("maker_b").expect("B's entries wait for B");
    assert!(held.coids.iter().any(|o| o.coid == resting), "the resting order is held");
    assert_eq!(held.ledger.map(|l| l.size), Some(1.0), "and B's ledger with it");
    assert!(noted(&second, "ORDER OWNERS HELD: mount `maker_b`"), "recent: {:?}", second.recent);
    let on_disk = std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap();
    assert!(on_disk.contains(&resting), "the file keeps the entry");

    fill(&mut second, &resting, -1, 0.4, 110.0);
    assert!(rt_heard.lock().unwrap().is_empty() && h.lock().unwrap().is_empty(), "nobody heard it");

    second.dispatch(mount_cmd(spec("maker-b")));
    assert!(noted(&second, "ORDER OWNERS BOUND: mount `maker_b`"), "recent: {:?}", second.recent);
    assert!(second.pending_owners.is_empty());
    assert_eq!(second.coid_mount.get(&resting), Some(&0), "the runtime mount owns it");
    assert_eq!(second.mount_attr[0].size, 1.0, "and holds B's restored ledger");

    fill(&mut second, &resting, -1, 0.6, 111.0);
    assert_eq!(rt_heard.lock().unwrap().clone(), vec![("RT", 0.6)], "the next fill reaches it");
    let left = second.mount_attr[0].size;
    assert!((left - 0.4).abs() < 1e-12, "1.0 restored − 0.6 sold = 0.4, got {left}");
}

/// 4 — **A resurrected runtime mount gets its ownership.** The composition root resurrects a
/// runtime mount by sending `Command::MountStrategy` after the core spawned
/// (`resurrect_runtime_mounts`); that mount is bound to the orders it rested before the restart.
#[test]
fn a_resurrected_runtime_mount_takes_back_its_resting_orders() {
    let dir = Scratch::reserved("vco-owners-resurrect");
    let step = Arc::new(TestMutex::new(Step::Quote));
    let rt_heard: Heard = heard();
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut config = config_on(&dir, Vec::new());
    config.strategy_factory = Some(quoter_factory("RT", &step, &rt_heard, None, &loaded));
    let mut first = assembled(config);
    first.dispatch(mount_cmd(spec("rt-b")));
    first.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid = first.engine.client.submissions.last().expect("quoted").client_order_id.clone();
    drop(first);

    *step.lock().unwrap() = Step::Idle;
    let mut config = config_on(&dir, Vec::new());
    config.strategy_factory = Some(quoter_factory("RT", &step, &rt_heard, None, &loaded));
    let mut second = assembled(config);
    assert!(noted(&second, "ORDER OWNERS HELD: mount `rt_b`"), "recent: {:?}", second.recent);
    second.dispatch(mount_cmd(spec("rt-b"))); // what `resurrect_runtime_mounts` sends
    assert_eq!(second.coid_mount.get(&coid), Some(&0));
    fill(&mut second, &coid, 1, 1.0, 100.0);

    assert_eq!(rt_heard.lock().unwrap().clone(), vec![("RT", 1.0)]);
    assert_eq!(second.mount_attr[0].size, 1.0);
}

/// 5 — **An explicit unmount forgets.** The file carries an `unmount`, and a reload hands back
/// nothing of that mount: a later mount of the same id must not inherit a removed strategy's orders.
#[test]
fn an_explicit_unmount_forgets_the_mounts_ownership() {
    let dir = Scratch::reserved("vco-owners-unmount");
    let step = Arc::new(TestMutex::new(Step::Quote));
    let rt_heard: Heard = heard();
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut config = config_on(&dir, Vec::new());
    config.strategy_factory = Some(quoter_factory("RT", &step, &rt_heard, None, &loaded));
    let mut first = assembled(config);
    first.dispatch(mount_cmd(spec("rt-b")));
    first.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    let coid = first.engine.client.submissions.last().expect("quoted").client_order_id.clone();
    fill(&mut first, &coid, 1, 0.5, 100.0);
    first.dispatch(unmount_cmd("rt-b"));
    drop(first);

    let on_disk = std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap();
    assert!(on_disk.contains("\"kind\":\"unmount\""), "the unmount is on disk: {on_disk}");
    let reloaded = OrderOwnerLog::in_state_dir(&dir).start(NOW);
    assert!(
        reloaded.coids.iter().all(|o| o.mount_id != "rt_b"),
        "no ownership of rt_b: {reloaded:?}"
    );
    assert!(reloaded.ledgers.iter().all(|(m, _)| m != "rt_b"), "no ledger of rt_b");
}

/// 6 — **Ledger continuity.** B is long 1 at 100 before the restart and rests a sell at 110. After
/// the restart the sell's fill closes B FLAT with a realized 10 — not short 1 from zero — and a
/// notional budget of 50 does NOT latch: an un-restored ledger would read short 1 at 110 (notional
/// 110) and latch B for a position it does not have.
#[test]
fn a_restored_ledger_closes_flat_and_trips_no_budget_latch() {
    let dir = Scratch::reserved("vco-owners-ledger");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut first = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    let bought = quote(&mut first, &nb, 1);
    fill(&mut first, &bought, 1, 1.0, 100.0);
    let selling = quote(&mut first, &nb, -1);
    drop(first);

    let mut config =
        config_on(&dir, vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)]);
    config.mount_budgets.insert(
        "maker_b".into(),
        MountBudget { max_notional: Some(50.0), ..MountBudget::default() },
    );
    let mut second = assembled(config);
    assert_eq!(second.mount_attr[1].size, 1.0, "B's long is restored");
    assert_eq!(second.mount_attr[1].avg_px, 100.0);

    fill(&mut second, &selling, -1, 1.0, 110.0);
    assert_eq!(second.mount_attr[1].size, 0.0, "the sell closed B flat");
    assert_eq!(second.mount_attr[1].realized_pnl, 10.0, "and realized (110 − 100) × 1");

    second.dispatch(Ingest::BarClose(Box::new(BarUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar: Bar {
            ts: 60_000,
            open: 110.0,
            high: 110.0,
            low: 110.0,
            close: 110.0,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        },
    })));
    assert!(!second.mount_latched[1], "no budget latch: recent {:?}", second.recent);
    assert!(!noted(&second, "MOUNT BUDGET LATCH"));
}

/// 7 — **A journal restore is the explicit answer and wins.** With `CoreConfig::coid_mounts` /
/// `mount_attr` supplied, the file's entries are not applied (and the file is still started, so
/// this session's orders are recorded).
#[test]
fn an_explicit_journal_restore_wins_over_the_file() {
    let dir = Scratch::reserved("vco-owners-explicit");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut first = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    let coid = quote(&mut first, &nb, 1);
    fill(&mut first, &coid, 1, 0.5, 100.0);
    drop(first);

    let mut config =
        config_on(&dir, vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)]);
    config.coid_mounts = vec![(coid.clone(), "maker_a".to_string())];
    config.mount_attr = vec![vike_journal::SnapMountAttr {
        mount_id: "maker_a".into(),
        size: 5.0,
        avg_px: 1.0,
        realized_pnl: 0.0,
        fees_paid: 0.0,
    }];
    let mut second = assembled(config);
    assert_eq!(second.coid_mount.get(&coid), Some(&0), "the journal's owner, not the file's");
    assert_eq!(second.mount_attr[0].size, 5.0, "the journal's ledger");
    assert_eq!(
        second.mount_attr[1].size.to_bits(),
        0.0_f64.to_bits(),
        "the file's B ledger unused"
    );
    assert!(second.pending_owners.is_empty());
    let next = quote(&mut second, &na, 1);
    drop(second);
    assert!(
        std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap().contains(&next),
        "the file still records this session's orders"
    );
}

/// 8 — **`order_owners: None` is today's core.** Nothing is created, read or written.
#[test]
fn no_ownership_file_means_nothing_is_written() {
    let dir = Scratch::reserved("vco-owners-none");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut config =
        config_on(&dir, vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)]);
    config.order_owners = None;
    config.state_dir = Some(dir.to_path_buf());
    let mut core = assembled(config);
    let coid = quote(&mut core, &nb, 1);
    fill(&mut core, &coid, 1, 1.0, 100.0);
    assert!(core.order_owners.is_none());
    drop(core);
    assert!(!dir.join(ORDER_OWNERS_FILE).exists(), "no ownership file without the log");
}

// ---------------------------------------------------------------------------------------------
// ORDER RESTORE: the facts an `own` carries, the strategy tags put back at bind, the restored
// collection, the off switch and the tag-overwrite counter.
// ---------------------------------------------------------------------------------------------

use crate::order_owners::OwnedOrder;
use vike_exec::{Account, RiskGate};
use vike_model::RiskLimits;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{OrderAccepted, OrderCanceled, OrderSubmitted};

/// The registry key an [`Owner`]'s `q` tag files under on slot `slot` of the `sim`/`BTCUSDT`
/// series (`submit_limit_tagged("q", ..)`).
pub(super) fn q_key(slot: usize) -> String {
    format!("{slot}|sim|BTCUSDT|q")
}

/// What a restored `q` order of mount `maker_b` on `sim`/`BTCUSDT` looks like.
pub(super) fn restored_q(coid: &str) -> OwnedOrder {
    OwnedOrder {
        coid: coid.to_string(),
        mount_id: "maker_b".into(),
        venue: Some("sim".into()),
        symbol: Some("BTCUSDT".into()),
        account: None,
        tag: Some("q".into()),
    }
}

/// 9 — **The facts and the tag come back on the mount's NEW slot.** B rests a `q` quote; the
/// profile lists B first after the restart. The order is owned by slot 0, filed under slot 0's tag
/// key (and under no other slot's), carried with every fact in the restored collection, and
/// nothing is counted as adopted: adoption is the reconcile's, not the boot's.
#[test]
fn a_restart_restores_the_tag_and_the_facts_on_the_mounts_new_slot() {
    let dir = Scratch::reserved("vco-owners-tag-boot");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut first = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    let coid = quote(&mut first, &nb, 1);
    assert_eq!(first.strategy_tags.get(&q_key(1)), Some(&coid), "precondition: filed live");
    drop(first);

    let second = assembled(config_on(
        &dir,
        vec![owner("B", "maker-b", &nb, &h), owner("A", "maker-a", &na, &h)],
    ));
    assert_eq!(second.coid_mount.get(&coid), Some(&0));
    assert_eq!(second.strategy_tags.get(&q_key(0)), Some(&coid), "the tag, on B's slot 0");
    assert_eq!(second.strategy_tags.len(), 1, "and nowhere else");
    assert_eq!(second.tag_for_coid(0, &coid).as_deref(), Some("q"));
    assert_eq!(second.tag_for_coid(1, &coid), None, "A has no claim on it");
    assert_eq!(second.restored_orders.get(&coid), Some(&restored_q(&coid)));
    assert_eq!(second.restored_orders.len(), 1);
    assert_eq!(second.restore_counters, RestoreCounters::default(), "nothing adopted at boot");
}

/// 10 — **A line from before the facts existed restores ownership only.** No tag is filed, the
/// entry sits in the collection with every fact `None`, and its fill still reaches the owner.
#[test]
fn an_old_line_without_facts_restores_ownership_only() {
    let dir = Scratch::created("vco-owners-old-own");
    let (na, nb, h) = (cell(), cell(), heard());
    std::fs::write(
        dir.join(ORDER_OWNERS_FILE),
        format!("{{\"kind\":\"own\",\"coid\":\"old-1\",\"mount_id\":\"maker_b\",\"ts\":{NOW}}}\n"),
    )
    .unwrap();
    let mut second = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    assert_eq!(second.coid_mount.get("old-1"), Some(&1), "B owns it");
    assert!(second.strategy_tags.is_empty(), "no tag without a recorded one");
    let held = second.restored_orders.get("old-1").expect("ownership alone is a restored entry");
    assert_eq!(
        *held,
        OwnedOrder { coid: "old-1".into(), mount_id: "maker_b".into(), ..OwnedOrder::default() }
    );
    fill(&mut second, "old-1", 1, 1.0, 100.0);
    assert_eq!(h.lock().unwrap().clone(), vec![("B", 1.0)], "its fill still reaches B");
}

/// 11 — **An order with facts but no tag restores no tag.** A submit that names no tag (a stop, a
/// flatten, a plain `Submit`) is recorded with its venue and symbol and no tag.
#[test]
fn an_untagged_order_is_restored_with_its_facts_and_no_tag() {
    let dir = Scratch::reserved("vco-owners-untagged");
    let (nb, h) = (cell(), heard());
    let mut first = assembled(config_on(&dir, vec![owner("B", "maker-b", &nb, &h)]));
    let req = OrderRequest {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    let coids = first.apply_strategy_intent(OrderIntent::Submit(Box::new(req)), 1, 0);
    drop(first);

    let second = assembled(config_on(&dir, vec![owner("B", "maker-b", &nb, &h)]));
    let mut want = restored_q(&coids[0]);
    want.tag = None;
    assert_eq!(second.restored_orders.get(&coids[0]), Some(&want));
    assert_eq!(second.coid_mount.get(&coids[0]), Some(&0));
    assert!(second.strategy_tags.is_empty());
}

/// 12 — **A pending entry brings its tag when its runtime mount lands; an unmount takes both
/// away.** B's mount is absent at boot: its order waits, with no tag and no collection row. The
/// runtime mount of B's id binds it onto the new slot, tag included; unmounting that mount drops
/// the tag-registry rows and the collection rows of the slot.
#[test]
fn a_pending_entry_binds_with_its_tag_and_an_unmount_drops_both() {
    let dir = Scratch::reserved("vco-owners-tag-pending");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut first = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    let resting = quote(&mut first, &nb, 1);
    drop(first);

    let step = Arc::new(TestMutex::new(Step::Idle));
    let rt_heard: Heard = heard();
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut config = config_on(&dir, Vec::new());
    config.strategy_factory = Some(quoter_factory("RT", &step, &rt_heard, None, &loaded));
    let mut second = assembled(config);
    assert!(second.strategy_tags.is_empty(), "no slot, no tag");
    assert!(second.restored_orders.is_empty(), "and nothing in the collection while it waits");

    second.dispatch(mount_cmd(spec("maker-b")));
    assert_eq!(second.strategy_tags.get(&q_key(0)), Some(&resting), "the tag, on the new slot");
    assert_eq!(second.restored_orders.get(&resting), Some(&restored_q(&resting)));

    second.dispatch(unmount_cmd("maker-b"));
    assert!(second.strategy_tags.is_empty(), "the unmount dropped the slot's tags");
    assert!(second.restored_orders.is_empty(), "and its restored rows");
}

/// 13 — **`restore_orders_off` restores ownership and fills as before and nothing else**: no tag,
/// no collection row, at boot or on a runtime mount; the fill still reaches its owner.
#[test]
fn restore_orders_off_restores_no_tag_and_fills_no_collection() {
    let dir = Scratch::reserved("vco-owners-restore-off");
    let (na, nb, h) = (cell(), cell(), heard());
    let mut first = assembled(config_on(
        &dir,
        vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)],
    ));
    let coid = quote(&mut first, &nb, 1);
    drop(first);

    let mut config =
        config_on(&dir, vec![owner("A", "maker-a", &na, &h), owner("B", "maker-b", &nb, &h)]);
    config.restore_orders_off = true;
    let mut second = assembled(config);
    assert_eq!(second.coid_mount.get(&coid), Some(&1), "ownership is restored as today");
    assert!(second.strategy_tags.is_empty(), "no tag");
    assert!(second.restored_orders.is_empty(), "no collection row");
    fill(&mut second, &coid, 1, 1.0, 100.0);
    assert_eq!(h.lock().unwrap().clone(), vec![("B", 1.0)], "the fill reaches B as before");
    assert_eq!(second.mount_attr[1].size, 1.0, "and B's ledger");
    drop(second);

    // The runtime-mount path is switched off the same way.
    let step = Arc::new(TestMutex::new(Step::Idle));
    let loaded = Arc::new(TestMutex::new(Vec::new()));
    let mut config = config_on(&dir, Vec::new());
    config.restore_orders_off = true;
    config.strategy_factory = Some(quoter_factory("RT", &step, &heard(), None, &loaded));
    let mut third = assembled(config);
    third.dispatch(mount_cmd(spec("maker-b")));
    assert_eq!(third.coid_mount.get(&coid), Some(&0), "the runtime mount owns it");
    assert!(third.strategy_tags.is_empty() && third.restored_orders.is_empty());
}

/// Publish `Submitted`, `Accepted` and `Canceled` for `coid` into the primary engine, so the
/// registry holds it TERMINAL without `dispatch_applied_fills` having run (the tag stays filed).
fn kill_in_registry(core: &mut CoreThread<RecordingClient>, coid: &str) {
    let id = || coid.to_string();
    core.bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: id(), ts: 1 }),
        &mut core.engine,
    );
    core.bus.publish(
        Event::OrderAccepted(OrderAccepted { client_order_id: id(), venue_order_id: None, ts: 2 }),
        &mut core.engine,
    );
    core.bus.publish(
        Event::OrderCanceled(OrderCanceled {
            client_order_id: id(),
            reason: String::new().into(),
            ts: 3,
        }),
        &mut core.engine,
    );
}

/// 14 — **`tag_overwrite_orphaned` fires exactly when the registry still holds the OLD order
/// live.** First quote: nothing to overwrite. Second quote while the first is live: counted, noted
/// once, and the tag follows the newer order as before. Third quote after the registry holds the
/// second terminal: the ordinary requote, counted nothing.
#[test]
fn the_tag_overwrite_counter_fires_only_while_the_old_order_is_live() {
    let dir = Scratch::reserved("vco-owners-overwrite");
    let (nb, h) = (cell(), heard());
    let mut core = assembled(config_on(&dir, vec![owner("B", "maker-b", &nb, &h)]));
    let noted_count = |c: &CoreThread<RecordingClient>| {
        c.recent.iter().filter(|l| l.contains("re-pointed from live order")).count()
    };

    let first = quote(&mut core, &nb, 1);
    assert_eq!(core.restore_counters.tag_overwrite_orphaned, 0, "an empty key orphans nothing");

    let second = quote(&mut core, &nb, 1);
    assert_ne!(first, second);
    assert!(core.engine.registry[&first].status.is_live(), "precondition: the old one is live");
    assert_eq!(core.restore_counters.tag_overwrite_orphaned, 1, "the old order is orphaned");
    assert_eq!(noted_count(&core), 1, "one note per occurrence: {:?}", core.recent);
    assert!(noted(&core, &first) && noted(&core, &second), "naming both orders");
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&second), "the tag semantics are unchanged");

    kill_in_registry(&mut core, &second);
    assert!(!core.engine.registry[&second].status.is_live(), "precondition: dead in the registry");
    let third = quote(&mut core, &nb, 1);
    assert_eq!(core.restore_counters.tag_overwrite_orphaned, 1, "a dead old order is no orphan");
    assert_eq!(noted_count(&core), 1);
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&third));
}

/// 15 — **After a restart the registry does not hold the restored order, so a fresh submit under
/// its tag counts nothing** — until the reconcile pass adopts the order into the registry
/// (`restore_tests`' `after_the_adoption_a_fresh_quote_over_the_restored_tag_is_counted` is the
/// counterpart that bites). This one pins the state BEFORE any pass has run.
#[test]
fn a_fresh_submit_over_a_restored_tag_counts_nothing_until_the_order_is_in_the_registry() {
    let dir = Scratch::reserved("vco-owners-overwrite-restored");
    let (nb, h) = (cell(), heard());
    let mut first = assembled(config_on(&dir, vec![owner("B", "maker-b", &nb, &h)]));
    let old = quote(&mut first, &nb, 1);
    drop(first);

    let mut second = assembled(config_on(&dir, vec![owner("B", "maker-b", &nb, &h)]));
    assert_eq!(second.strategy_tags.get(&q_key(0)), Some(&old), "precondition: restored");
    let fresh = quote(&mut second, &nb, 1);
    assert_eq!(second.restore_counters.tag_overwrite_orphaned, 0, "the registry has no old order");
    assert_eq!(second.strategy_tags.get(&q_key(0)), Some(&fresh));
}

/// What [`TagSpy`] heard: `(Debug of the event kind, tag)`.
pub(super) type Events = Arc<TestMutex<Vec<(String, Option<String>)>>>;

/// A mount that quotes under `q` and records every lifecycle event it hears.
pub(super) struct TagSpy {
    pub(super) next: NextQuote,
    pub(super) events: Events,
}

impl Strategy<LiveBroker> for TagSpy {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        if let Some(side) = self.next.lock().unwrap().take() {
            broker.submit_limit_tagged("q", side, 1.0, 100.0);
        }
    }

    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &vike_model::OrderLifecycle) {
        self.events.lock().unwrap().push((format!("{:?}", event.kind), event.tag.clone()));
    }
}

pub(super) fn spy_mount(next: &NextQuote, events: &Events) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: Some("maker-b".to_string()),
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(TagSpy { next: Arc::clone(next), events: Arc::clone(events) }),
    }
}

/// 16 — **A terminal event for a restored coid reaches the strategy WITH its tag**, which is what
/// `SpreadMaker::on_order_event` keys on (it ignores a tagless event). The registry is given the
/// order the way the adoption step will (the test stands in for it), then the venue's cancel is
/// folded and delivered: the mount hears `Canceled` tagged `q`, and the tag entry is retired.
#[test]
fn a_terminal_event_for_a_restored_coid_is_stamped_with_its_restored_tag() {
    let dir = Scratch::reserved("vco-owners-stamp");
    let (nb, events) = (cell(), Events::default());
    let mut first = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
    let coid = quote(&mut first, &nb, 1);
    drop(first);

    let mut second = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
    assert_eq!(second.strategy_tags.get(&q_key(0)), Some(&coid), "precondition: restored");
    // What the adoption step will do: the order is back in the registry, resting.
    let request = OrderRequest {
        client_order_id: coid.clone(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    let mut adopted = vike_exec::ManagedOrder::new(request);
    adopted.status = vike_exec::OrderStatus::Accepted;
    second.engine.registry.insert(coid.clone(), adopted);

    second.bus.publish(
        Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.clone(),
            reason: String::new().into(),
            ts: 5,
        }),
        &mut second.engine,
    );
    second.dispatch_applied_fills();

    let heard = events.lock().unwrap().clone();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(heard[0].0.contains("Canceled"), "{heard:?}");
    assert_eq!(heard[0].1.as_deref(), Some("q"), "stamped with the RESTORED tag: {heard:?}");
    assert!(second.strategy_tags.is_empty(), "and the terminal retired it");
}

/// An engine on `binance`/`BTCUSDT` whose routing key is `route_key` (the two-account shape
/// `mount_account`'s fixtures use).
pub(super) fn binance_engine(route_key: &str, seed: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(seed, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    e.route_key = route_key.to_string();
    e.collect_applied_fills = true;
    e
}

/// 17 — **A restored order on an EXTRA engine keeps its engine.** Two accounts of one venue; the
/// mount on account `ALT` rests a quote on engine 1. After the restart the order is owned, tagged,
/// carries its account label in the collection and has its `coid_venue` row back, so its lifecycle
/// events route to engine 1 and not to the primary.
#[test]
fn a_restored_order_on_an_extra_engine_keeps_its_engine_row() {
    let dir = Scratch::reserved("vco-owners-extra-engine");
    let (nd, na, h) = (cell(), cell(), heard());
    let mount = |label: &'static str, cid: &str, account: AccountLabel, next: &NextQuote| {
        let mut m = owner(label, cid, next, &h);
        m.venue = "binance".into();
        m.account = Some(account);
        m
    };
    let build = || {
        let mut config = config_on(
            &dir,
            vec![
                mount("D", "m-default", AccountLabel::Default, &nd),
                mount("ALT", "m-alt", AccountLabel::parse("ALT").unwrap(), &na),
            ],
        );
        config.seed_cash = 1_000.0;
        crate::runtime::test_support::core_of(
            binance_engine("binance", 1_000.0),
            vec![(2_000.0, binance_engine("binance#ALT", 2_000.0))],
            config,
        )
    };

    let mut first = build();
    *na.lock().unwrap() = Some(1);
    first.drive_strategy_feed_status("binance", "BTCUSDT", FeedStatus::Live);
    let coid = first.extra_engines[0]
        .1
        .client
        .submissions
        .last()
        .expect("ALT quoted")
        .client_order_id
        .clone();
    assert_eq!(first.coid_venue.get(&coid), Some(&1), "precondition: minted on engine 1");
    drop(first);

    let second = build();
    assert_eq!(second.coid_mount.get(&coid), Some(&1));
    assert_eq!(second.coid_venue.get(&coid), Some(&1), "the engine row is back");
    let held = second.restored_orders.get(&coid).expect("restored");
    assert_eq!(held.account.as_deref(), Some("ALT"));
    assert_eq!(held.venue.as_deref(), Some("binance"));
    assert_eq!(held.tag.as_deref(), Some("q"));
    assert_eq!(second.strategy_tags.get("1|binance|BTCUSDT|q"), Some(&coid));
}
