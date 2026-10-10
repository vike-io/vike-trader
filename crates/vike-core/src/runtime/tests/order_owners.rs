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
const NOW: i64 = 1_700_000_000_000;

/// What every mount heard through `on_fill`: `(label, fill size)`.
type Heard = Arc<TestMutex<Vec<(&'static str, f64)>>>;

/// The side the mount quotes on the next `on_feed_status` (`1` buys at 100, `-1` sells at 110),
/// consumed by that quote.
type NextQuote = Arc<TestMutex<Option<i32>>>;

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

fn cell() -> NextQuote {
    Arc::new(TestMutex::new(None))
}

fn heard() -> Heard {
    Arc::new(TestMutex::new(Vec::new()))
}

/// The config of one "process": `mounts` in order (the first is `CoreConfig::strategy`) and the
/// ownership file in `dir`.
fn config_on(dir: &Scratch, mounts: Vec<StrategyMount>) -> CoreConfig {
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
fn assembled(config: CoreConfig) -> CoreThread<RecordingClient> {
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;
    core
}

/// Tell the mount behind `next` to quote `side`, and return the coid it was given.
fn quote(core: &mut CoreThread<RecordingClient>, next: &NextQuote, side: i32) -> String {
    *next.lock().unwrap() = Some(side);
    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
    core.engine.client.submissions.last().expect("the mount quoted").client_order_id.clone()
}

/// A venue fill for `coid`, folded and delivered.
fn fill(core: &mut CoreThread<RecordingClient>, coid: &str, side: i32, qty: f64, px: f64) {
    core.bus.publish(Event::Fill(coid_fill(coid, side, qty, px)), &mut core.engine);
    core.dispatch_applied_fills();
}

fn noted(core: &CoreThread<RecordingClient>, needle: &str) -> bool {
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
    assert!(held.coids.contains(&resting), "the resting order is held");
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
    assert!(reloaded.coids.iter().all(|(_, m)| m != "rt_b"), "no ownership of rt_b: {reloaded:?}");
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
