//! The BOOT RESTORE (`vike_core::replay::restore_for_boot`, decision 0127), end to end: a real core
//! journals, stops (or "crashes"), and the next boot restores from that directory — refolded across
//! several engines, from a torn tail, from a broken journal (moved aside), from a tail of waker
//! records — and says how deep it went.
//!
//! The assembly half (precedence, engines by route key, paper books, lost held exits, the price at
//! adoption) is white-box in `src/runtime/tests/boot_restore.rs`, `order_owners.rs` and
//! `restore.rs`; these gate what only a journal on disk can show.

use crate::scratch::Scratch;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_core::replay::{RestoreOutcome, restore_for_boot};
use vike_core::strategy_state::mount_id_with;
use vike_core::{CoreConfig, JournalConfig, spawn_core_multi};
use vike_exec::recon::{BalanceTol, ReconMode, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, CoreSnapshot, ExecutionClient, Ingest, OrderIntent, ReconcileReports};
use vike_journal::{
    CommandJournal, CorruptCause, JournalFileConfig, SnapBooks, end_of_frames, fnv1a32,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderSubmitted, TradeId};
use vike_model::test_support::file::plant;
use vike_model::{AccountTier, OrderRequest, OrderStatus, OrderStatusReport, OrderType};

use crate::kit::doubles::RestOnce;
use crate::kit::engines::sim_engine;
use crate::kit::events::{sim_bare_fill, sim_quote};
use crate::kit::handle::wait_for;
use crate::kit::journal::{build_source_journal, counter_clock, journal_at};
use crate::kit::requests::sim_limit;
use vike_core::testkit::mount_of;
use vike_exec::testing::engine_on;

/// A segment's header length (`vike_journal`'s private `format::HEADER`: magic, version, first
/// seq). The byte surgery below starts after it.
const HEADER: usize = 16;

/// A 1 MiB segment config for a journal a test opens by hand (the default pre-allocates 64 MiB).
fn small_file() -> JournalFileConfig {
    JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 }
}

/// A bare external fill on `venue`/`symbol` (no coid): it folds into that engine's position and
/// fill dedup, exactly as `sim_bare_fill` does on `sim`.
fn bare_fill_on(venue: &str, symbol: &str, tid: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(tid).expect("a non-empty trade id"),
        client_order_id: String::new(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// Every engine's positions. ⚠ NOT `CoreSnapshot::positions`, which mirrors the PRIMARY engine only
/// (`crates/vike-core/src/snapshot/build.rs`'s `build`): an extra engine's positions are on its own
/// `portfolio.venues` block.
fn every_position(s: &CoreSnapshot) -> Vec<&vike_exec::PositionView> {
    s.portfolio.venues.iter().flat_map(|v| v.positions.iter()).collect()
}

/// The highest-indexed segment file of `dir`.
fn last_segment(dir: &Path) -> PathBuf {
    let mut segs: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            let name = p.file_name()?.to_str()?.to_string();
            (name.starts_with("journal-") && name.ends_with(".vjl")).then_some(p)
        })
        .collect();
    segs.sort();
    segs.pop().expect("the journal wrote a segment")
}

/// THE MULTI-ENGINE RESTART: two engines (the live node always runs one per wired market), a mount
/// resting one quote and a position on each engine, a clean stop. The refold runs across both
/// engines (Full), every engine snapshot comes back, the resting quote is an ownership candidate of
/// the mount that placed it (with its price), and the next core assembled over the restore holds
/// both positions on the right engines.
#[test]
fn a_restart_with_resting_orders_across_two_engines_restores_each_engine_and_its_candidates() {
    let dir = Scratch::reserved("boot-restore-two-engines");
    let tmp = Scratch::created("boot-restore-two-engines-tmp");
    let config = |done: &Arc<AtomicBool>, journal: Option<JournalConfig>| CoreConfig {
        seed_cash: 1_000.0,
        clock: counter_clock(),
        coid_session: Some(("cafef00d".into(), 0)),
        strategy: Some(mount_of(
            "sim",
            "BTCUSDT",
            "1m",
            Box::new(RestOnce { done: Arc::clone(done) }),
        )),
        journal,
        ..CoreConfig::default()
    };
    let engines =
        || (sim_engine(), vec![(500.0, engine_on("sim2", "ETHUSDT", RecordingClient::default()))]);

    // ---- run 1: a resting quote on `sim`, a position on each engine, a clean stop.
    let done = Arc::<AtomicBool>::default();
    let (primary, extras) = engines();
    let handle =
        spawn_core_multi(primary, extras, config(&done, Some(journal_at(&dir, 1 << 20, 1_000))));
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    let events = handle.event_sender();
    events.blocking_send(sim_bare_fill("t-sim", 1.0, 100.0)).unwrap();
    events.blocking_send(bare_fill_on("sim2", "ETHUSDT", "t-sim2", 2.0, 50.0)).unwrap();
    let snap = wait_for(&handle, "the quote and both positions", |s| {
        s.orders.len() == 1 && every_position(s).len() == 2
    });
    let coid = snap.orders[0].client_order_id.clone();
    handle.shutdown_and_join();

    // ---- the boot restore.
    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    assert_eq!(restore.restored_engines().len(), 2, "both engines' snapshots");
    assert_eq!(restore.coid_session().map(|s| s.0.as_str()), Some("cafef00d"));
    let candidates = restore.journal_orders();
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    assert_eq!(candidates[0].coid, coid);
    assert_eq!(candidates[0].mount_id, mount_id_with(None, "sim", "BTCUSDT", "1m"));
    assert_eq!(candidates[0].price, Some(100.0));
    let line = restore.startup_line().expect("a journal is on");
    assert!(line.starts_with("journal restore: full from "), "{line}");
    assert!(line.contains("engines 2") && line.contains("resting orders 1"), "{line}");

    // ---- run 2: a fresh core over the restore (no journal of its own; the restore is the subject).
    let done2 = Arc::new(AtomicBool::new(true));
    let mut config2 = config(&done2, None);
    restore.apply_to(&mut config2);
    let (primary, extras) = engines();
    let handle = spawn_core_multi(primary, extras, config2);
    handle.tick_sender().quote(sim_quote(100.0, 2)).unwrap();
    let snap = wait_for(&handle, "the restored positions", |s| every_position(s).len() == 2);
    let size_of = |venue: &str| {
        every_position(&snap)
            .into_iter()
            .find(|p| p.venue == venue)
            .map(|p| p.size)
            .unwrap_or_default()
    };
    assert_eq!(size_of("sim"), 1.0, "sim's position came back onto sim");
    assert_eq!(size_of("sim2"), 2.0, "sim2's onto sim2");
    assert!(
        snap.recent_events.iter().any(|l| l.contains("journal restore: full")),
        "the restore says so on the ring: {:?}",
        snap.recent_events
    );
    handle.shutdown_and_join();
}

/// The venue's open-order row for `req`, acknowledged as `v-<coid>` and not filled.
fn venue_row(req: &OrderRequest) -> OrderStatusReport {
    OrderStatusReport {
        venue: req.venue.clone(),
        symbol: req.symbol.clone(),
        venue_order_id: format!("v-{}", req.client_order_id).into(),
        client_order_id: Some(req.client_order_id.clone()),
        side: req.side,
        order_type: OrderType::Limit,
        qty: req.qty,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: OrderStatus::Accepted,
        ts: 5,
    }
}

/// One successful reconcile pass of `venue`'s account `route_key` (`None`: its default account) that
/// lists `orders`, under the default `quarantine`, from a client that claims no trigger listing.
fn pass_listing(venue: &str, route_key: Option<&str>, orders: Vec<OrderStatusReport>) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        admitted_symbols: None,
        lists_trigger_orders: false,
        venue: venue.into(),
        since: 0,
        orders,
        fills: Vec::new(),
        positions: Vec::new(),
        policy: ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: route_key.map(str::to_string),
    }))
}

/// **A CONTROL-DOOR ORDER RESTING ON TWO ENGINES IS BACK IN THE BOOK AFTER A CLEAN RESTART** (the CI box
/// v0.1.99, 2026-11-01: four limits placed through the door on okx and `bybit#DEMO` came back as held
/// `UnknownOrder` rows, and the startup line read `resting orders 0`). Run 1: two `demo` accounts, the
/// default `sim` and the labelled `sim2#DEMO`, one limit placed through the door on each and
/// acknowledged by the venue, a clean stop (nothing cancelled). The restore names both; run 2's first
/// pass per account lists each order, and the book holds it again as ours (its coid, venue id, price,
/// size and filled size) with nothing held for it.
#[test]
fn a_control_door_order_resting_on_two_engines_is_back_in_the_book_after_a_clean_restart() {
    door_orders_come_back_as_ours(AccountTier::Demo, "DEMO");
}

/// **…and on two `live` books** (decision 0192 verdict 6, amended by the owner's "yes" of 2026-11-01
/// to "the same for live accounts"): the default `sim` and the labelled `sim2#LIVE`, both at `live`.
#[test]
fn a_control_door_order_resting_on_two_live_engines_is_back_in_the_book_after_a_clean_restart() {
    door_orders_come_back_as_ours(AccountTier::Live, "LIVE");
}

/// The scenario of the two tests above, with both engines at `tier` and the labelled one `sim2#<label>`.
fn door_orders_come_back_as_ours(tier: AccountTier, label: &str) {
    let dir = Scratch::reserved(&format!("boot-restore-door-orders-{label}"));
    let tmp = Scratch::created(&format!("boot-restore-door-orders-{label}-tmp"));
    let labelled_key = format!("sim2#{label}");
    let config = |journal: Option<JournalConfig>| CoreConfig {
        seed_cash: 10_000.0,
        clock: counter_clock(),
        coid_session: Some(("cafef00d".into(), 0)),
        restore_venue_gate: |_| true,
        journal,
        ..CoreConfig::default()
    };
    let engines = || {
        let mut primary = sim_engine();
        primary.set_tier(tier);
        let mut labelled = engine_on("sim2", "ETHUSDT", RecordingClient::default());
        labelled.route_key = labelled_key.clone();
        labelled.set_tier(tier);
        (primary, vec![(10_000.0, labelled)])
    };
    let door_orders = [
        sim_limit("door-sim", 1, 1.0, 100.0),
        OrderRequest {
            venue: "sim2".into(),
            symbol: "ETHUSDT".into(),
            account: Some(AccountLabel::parse(label).unwrap()),
            ..sim_limit("door-sim2", -1, 2.0, 50.0)
        },
    ];

    // ---- run 1: each order through the door, acknowledged, then a clean stop.
    let (primary, extras) = engines();
    let handle = spawn_core_multi(primary, extras, config(Some(journal_at(&dir, 1 << 20, 1_000))));
    let events = handle.event_sender();
    for req in &door_orders {
        let coid = req.client_order_id.clone();
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req.clone()))));
        events
            .blocking_send(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: coid.clone(),
                ts: 1,
            }))
            .unwrap();
        events
            .blocking_send(Event::OrderAccepted(OrderAccepted {
                client_order_id: coid.clone(),
                venue_order_id: Some(format!("v-{coid}").into()),
                ts: 1,
            }))
            .unwrap();
    }
    let resting = |s: &CoreSnapshot, coid: &str| {
        s.orders.iter().any(|o| o.client_order_id == coid && o.status == OrderStatus::Accepted)
    };
    wait_for(&handle, "both door orders resting", |s| {
        door_orders.iter().all(|r| resting(s, &r.client_order_id))
    });
    handle.shutdown_and_join();

    // ---- the boot restore: both are candidates.
    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let mut candidates: Vec<&str> =
        restore.journal_orders().iter().map(|o| o.coid.as_str()).collect();
    candidates.sort_unstable();
    assert_eq!(candidates, ["door-sim", "door-sim2"], "{:?}", restore.journal_orders());
    let line = restore.startup_line().expect("a journal is on");
    assert!(line.contains("resting orders 2"), "{line}");

    // ---- run 2: each account's first pass lists its order.
    let mut config2 = config(None);
    restore.apply_to(&mut config2);
    let (primary, extras) = engines();
    let handle = spawn_core_multi(primary, extras, config2);
    let cell = handle.snapshot_cell();
    handle.send_command(pass_listing("sim", None, vec![venue_row(&door_orders[0])]));
    handle.send_command(pass_listing(
        "sim2",
        Some(&labelled_key),
        vec![venue_row(&door_orders[1])],
    ));
    handle.shutdown_and_join();

    let snap = cell.load_full();
    for req in &door_orders {
        let coid = req.client_order_id.as_str();
        let o = snap.orders.iter().find(|o| o.client_order_id == coid);
        let o = o.unwrap_or_else(|| panic!("{coid} is back in the book: {:?}", snap.orders));
        assert_eq!(o.status, OrderStatus::Accepted, "{coid}");
        assert_eq!((o.venue.as_str(), o.side, o.qty), (req.venue.as_str(), req.side, req.qty));
        assert_eq!(o.price, req.price, "{coid}: the journal's price, which no report carries");
        assert_eq!(o.venue_order_id.as_deref(), Some(format!("v-{coid}").as_str()));
        assert_eq!(o.filled_qty, 0.0, "{coid}");
    }
    assert!(
        snap.recon.alerts.is_empty(),
        "nothing held for our own orders: {:?}",
        snap.recon.alerts
    );
}

/// The whole resting order a venue client was handed for each modify: where a venue whose amend sends the
/// order's lifetime again reads it from.
#[derive(Clone, Default)]
struct ModifySpy(Arc<std::sync::Mutex<Vec<OrderRequest>>>);

impl ExecutionClient for ModifySpy {
    fn submit(&mut self, _: &OrderRequest) {}
    fn cancel(&mut self, _: &str) {}
    fn modify(&mut self, order: &OrderRequest, _: Option<f64>, _: Option<f64>) {
        self.0.lock().unwrap().push(order.clone());
    }
}

/// **A MODIFY AFTER A RESTART KEEPS THE LIFETIME THE ORDER RESTED WITH, ON EVERY VENUE THE RESTORE IS OPEN FOR.**
/// A venue report carries no lifetime, so an adopted order used to come back `gtc` with no expiry, and OANDA's
/// replace, IBKR's re-placing, Hyperliquid's modify and Binance spot's cancel-replace send what the registry says:
/// a good-for-day order became good-till-cancelled and a post-only one could take. Run 1 rests one order per
/// lifetime the venue maps through the door and stops; run 2 (the real table's gate, the journal's restore) adopts
/// each at the first pass and modifies it. The client must be handed the lifetime it rested with.
#[test]
fn a_modify_after_a_restart_keeps_the_lifetime_the_order_rested_with() {
    use vike_model::TimeInForce::{Day, Gtd, PostOnly};
    use vike_venues::venue_restore::restores_orders;
    use vike_venues::venue_tif::{TifOutcome, venue_tif};

    let far = 4_102_444_800_000; // 2100-01-01
    for venue in vike_model::VENUES.iter().copied().filter(|v| restores_orders(v)) {
        let dir = Scratch::reserved(&format!("boot-restore-lifetime-{venue}"));
        let tmp = Scratch::created(&format!("boot-restore-lifetime-{venue}-tmp"));
        let config = |journal: Option<JournalConfig>| CoreConfig {
            seed_cash: 10_000.0,
            clock: counter_clock(),
            coid_session: Some(("cafef00d".into(), 0)),
            journal,
            ..CoreConfig::default()
        };
        let engine = |spy: ModifySpy| {
            let mut e = engine_on(venue, "BTCUSDT", spy);
            e.set_tier(AccountTier::Demo);
            e
        };
        let orders: Vec<OrderRequest> = [(Day, None), (Gtd, Some(far)), (PostOnly, None)]
            .into_iter()
            .filter(|(tif, _)| matches!(venue_tif(venue, *tif), TifOutcome::Mapped(_)))
            .map(|(tif, expiry)| OrderRequest {
                venue: venue.into(),
                time_in_force: tif,
                gtd_expiry: expiry,
                ..sim_limit(&format!("door-{tif:?}"), 1, 1.0, 100.0)
            })
            .collect();

        let run1 = ModifySpy::default();
        let journal = Some(journal_at(&dir, 1 << 20, 1_000));
        let handle = spawn_core_multi(engine(run1), Vec::new(), config(journal));
        let events = handle.event_sender();
        for req in &orders {
            let coid = req.client_order_id.clone();
            handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req.clone()))));
            events
                .blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: coid.clone(),
                    ts: 1,
                }))
                .unwrap();
            events
                .blocking_send(Event::OrderAccepted(OrderAccepted {
                    client_order_id: coid.clone(),
                    venue_order_id: Some(format!("v-{coid}").into()),
                    ts: 1,
                }))
                .unwrap();
        }
        wait_for(&handle, "every order resting", |s| {
            orders.iter().all(|r| {
                s.orders.iter().any(|o| {
                    o.client_order_id == r.client_order_id && o.status == OrderStatus::Accepted
                })
            })
        });
        handle.shutdown_and_join();

        let restore =
            restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42).expect("readable");
        let mut config2 = config(None);
        restore.apply_to(&mut config2);
        let spy = ModifySpy::default();
        let handle = spawn_core_multi(engine(spy.clone()), Vec::new(), config2);
        let rows = orders.iter().map(venue_row).collect();
        handle.send_command(pass_listing(venue, None, rows));
        for req in &orders {
            handle.send_command(Command::Order(OrderIntent::Modify {
                client_order_id: req.client_order_id.clone(),
                new_qty: None,
                new_price: Some(99.0),
            }));
        }
        handle.shutdown_and_join();

        let sent = spy.0.lock().unwrap();
        assert_eq!(sent.len(), orders.len(), "{venue}: each modify reached the client: {sent:?}");
        for (req, modified) in orders.iter().zip(sent.iter()) {
            assert_eq!(modified.client_order_id, req.client_order_id, "{venue}");
            assert_eq!(modified.time_in_force, req.time_in_force, "{venue}: {modified:?}");
            assert_eq!(modified.gtd_expiry, req.gtd_expiry, "{venue}: {modified:?}");
        }
    }
}

/// **AN ORDER THE PREVIOUS RELEASE ADOPTED IS NOT A KNOWN GTC.** That release rebuilt a resting order from a venue
/// report, journaled the type's default lifetime and had no flag to say it did not know, so a maker order came back
/// reading as one that may take. A real journal of that shape: the adopted order states no lifetime and its modify is
/// refused, while the order the same journal shows the node placing keeps its own and is sent.
#[test]
fn an_order_the_previous_release_adopted_is_not_read_as_a_known_gtc() {
    use vike_model::TimeInForce::PostOnly;

    let venue = "hyperliquid";
    let dir = Scratch::reserved("boot-restore-old-adopted");
    let tmp = Scratch::created("boot-restore-old-adopted-tmp");
    let placed = OrderRequest {
        venue: venue.into(),
        time_in_force: PostOnly,
        ..sim_limit("placed", 1, 1.0, 100.0)
    };
    let adopted = OrderRequest { venue: venue.into(), ..sim_limit("adopted", 1, 1.0, 100.0) };

    let mut previous = engine_on(venue, "BTCUSDT", RecordingClient::default());
    previous.set_tier(AccountTier::Demo);
    previous.reregister_orders(&[venue_row(&adopted)]);
    previous.registry.get_mut("adopted").unwrap().lifetime_unknown = false;
    let mut minted = vike_model::ManagedOrder::new(placed.clone());
    minted.status = OrderStatus::Accepted;
    minted.created_ms = Some(1);
    previous.registry.insert("placed".into(), minted);
    let engines = vec![previous.snapshot_state()];
    let bytes = serde_json::to_string(&engines).unwrap();
    assert!(!bytes.contains("lifetime_unknown"), "the shape v0.1.100 wrote: {bytes}");
    let mut journal = CommandJournal::open(&dir, small_file()).unwrap();
    let hash = vike_exec::state_hash(&engines);
    journal.append_snap(1_000, &engines, "cafef00d", 0, 0, SnapBooks::default(), hash).unwrap();
    drop(journal);

    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42).expect("readable");
    let of = |coid: &str| {
        let found = restore.journal_orders().iter().find(|o| o.coid == coid);
        found.unwrap_or_else(|| panic!("{coid}: {:?}", restore.journal_orders())).time_in_force
    };
    assert_eq!(of("adopted"), None, "the journal's GTC is the type's default");
    assert_eq!(of("placed"), Some(PostOnly));

    let spy = ModifySpy::default();
    let mut engine = engine_on(venue, "BTCUSDT", spy.clone());
    engine.set_tier(AccountTier::Demo);
    let mut config =
        CoreConfig { seed_cash: 10_000.0, clock: counter_clock(), ..Default::default() };
    restore.apply_to(&mut config);
    let handle = spawn_core_multi(engine, Vec::new(), config);
    handle.send_command(pass_listing(venue, None, vec![venue_row(&placed), venue_row(&adopted)]));
    for coid in ["adopted", "placed"] {
        handle.send_command(Command::Order(OrderIntent::Modify {
            client_order_id: coid.into(),
            new_qty: None,
            new_price: Some(99.0),
        }));
    }
    handle.shutdown_and_join();

    let sent = spy.0.lock().unwrap();
    assert_eq!(sent.len(), 1, "the adopted order's modify is refused: {sent:?}");
    assert_eq!(sent[0].client_order_id, "placed");
    assert_eq!(sent[0].time_in_force, PostOnly, "{sent:?}");
}

/// Decision 0169: an account STOPPED and STARTED again in one process leaves two engines under one route key
/// (the tombstone and the new one). The next boot must still restore the account: the journal's `Snap`
/// carries ONE engine for the key (the newest), or the restore sees "two carriers" and applies neither.
#[test]
fn a_stopped_then_restarted_account_is_restored_by_the_next_boot() {
    let dir = Scratch::reserved("boot-restore-stop-start");
    let tmp = Scratch::created("boot-restore-stop-start-tmp");
    let config = |journal: Option<JournalConfig>| CoreConfig {
        seed_cash: 1_000.0,
        clock: counter_clock(),
        coid_session: Some(("cafef00d".into(), 0)),
        journal,
        ..CoreConfig::default()
    };
    let second = || engine_on("sim2", "ETHUSDT", RecordingClient::default());

    // ---- run 1: sim2 joins, is stopped, starts again as a NEW engine, and takes a position.
    let handle =
        spawn_core_multi(sim_engine(), Vec::new(), config(Some(journal_at(&dir, 1 << 20, 1_000))));
    let events = handle.event_sender();
    events.add_account_engine(500.0, second()).unwrap();
    wait_for(&handle, "the first sim2 engine", |s| s.portfolio.venues.len() == 2);
    events
        .stop_account_engine(vike_exec::NotRunningAccount {
            venue: "sim2".to_string(),
            account: None,
            route_key: "sim2".to_string(),
            tier: None,
            reason: "account inactive".to_string(),
        })
        .unwrap();
    wait_for(&handle, "the stop", |s| {
        s.recent_events.iter().any(|l| l.contains("ACCOUNT STOPPED"))
    });
    events.detach_account_engine("sim2").unwrap();
    events.add_account_engine(500.0, second()).unwrap();
    wait_for(&handle, "the second sim2 engine", |s| s.portfolio.venues.len() == 3);
    events
        .routed("sim2")
        .blocking_send(bare_fill_on("sim2", "ETHUSDT", "t-again", 2.0, 50.0))
        .unwrap();
    wait_for(&handle, "the new engine's position", |s| every_position(s).len() == 1);
    handle.shutdown_and_join();

    // ---- the boot restore: ONE snapshot for sim2.
    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42).expect("readable");
    let for_sim2 = restore.restored_engines().iter().filter(|e| e.venue == "sim2").count();
    assert_eq!(for_sim2, 1, "the tombstone is not journaled beside the engine that replaced it");

    // ---- run 2: the account's position comes back onto it.
    let mut config2 = config(None);
    restore.apply_to(&mut config2);
    let handle = spawn_core_multi(sim_engine(), vec![(500.0, second())], config2);
    handle.tick_sender().quote(sim_quote(100.0, 2)).unwrap(); // a publish
    let snap = wait_for(&handle, "the restored position", |s| every_position(s).len() == 1);
    assert_eq!(every_position(&snap)[0].size, 2.0, "sim2's position was restored onto sim2");
    handle.shutdown_and_join();
}

/// A TORN length prefix at the end of the last segment (an ordinary crash mid-append): the readable
/// prefix restores, the segment is COPIED to `torn/` first (the live open overwrites those bytes),
/// the note says so, and the journal then opens and continues.
#[test]
fn a_torn_tail_restores_the_readable_prefix_keeps_the_evidence_and_says_so() {
    let dir = Scratch::reserved("boot-restore-torn");
    let tmp = Scratch::created("boot-restore-torn-tmp");
    build_source_journal(&dir);
    let intact = CommandJournal::read_all(&dir).unwrap().len();
    let seg = last_segment(&dir);
    plant(&seg, end_of_frames(&seg), &u32::MAX.to_le_bytes());

    let restore =
        restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &tmp, 7).expect("readable");
    let RestoreOutcome::TornTail { cause, kept_evidence, refolded } = restore.outcome() else {
        panic!("a torn tail, got {:?}", restore.outcome());
    };
    assert_eq!(*cause, CorruptCause::OobLen);
    assert!(*refolded, "the readable prefix ends in the exit Snap: nothing to fold but it refolds");
    let evidence = kept_evidence.as_ref().expect("the copy was kept");
    assert!(evidence.starts_with(dir.join(vike_journal::TORN_DIR)), "{}", evidence.display());
    assert_eq!(std::fs::read(evidence).unwrap(), std::fs::read(&seg).unwrap(), "byte for byte");
    assert!(restore.notes()[0].starts_with("JOURNAL TAIL TORN"), "{:?}", restore.notes());
    assert!(!restore.restored_engines().is_empty(), "the prefix restored");

    // The live core's open resumes over the torn frame, and the journal goes on.
    let mut j = CommandJournal::open(&dir, small_file()).unwrap();
    j.append_cmd(9_999, &Ingest::Event(sim_bare_fill("after", 1.0, 100.0))).unwrap();
    drop(j);
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), intact + 1);
}

/// A frame whose length was written whole and whose payload was not (`CheckMismatch`) at the end
/// of the last segment is a torn tail too.
#[test]
fn a_check_mismatch_tail_is_a_torn_tail_too() {
    let dir = Scratch::reserved("boot-restore-check-mismatch");
    let tmp = Scratch::created("boot-restore-check-mismatch-tmp");
    build_source_journal(&dir);
    let seg = last_segment(&dir);
    let mut frame = 8u32.to_le_bytes().to_vec();
    frame.extend_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
    frame.extend_from_slice(b"\x01\x02\x03\x04\x05\x06\x07\x08");
    plant(&seg, end_of_frames(&seg), &frame);

    let restore =
        restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &tmp, 7).expect("readable");
    assert!(
        matches!(
            restore.outcome(),
            RestoreOutcome::TornTail { cause: CorruptCause::CheckMismatch, .. }
        ),
        "{:?}",
        restore.outcome()
    );
}

/// Corruption in an EARLIER segment, with records written after it, is BROKEN: every later read
/// would stop at the same frame, so nothing is restored from it, the directory is moved aside
/// (not deleted), the line and the notes say so, and a fresh journal opens in its place.
#[test]
fn a_corrupt_frame_before_the_last_segment_moves_the_journal_aside_and_restores_nothing() {
    let root = Scratch::created("boot-restore-broken");
    let tmp = Scratch::created("boot-restore-broken-tmp");
    let dir = root.join("journal");
    let handle = spawn_core_multi(
        sim_engine(),
        Vec::new(),
        CoreConfig {
            seed_cash: 1_000.0,
            clock: counter_clock(),
            journal: Some(journal_at(&dir, 4 * 1024, 8)),
            ..CoreConfig::default()
        },
    );
    let events = handle.event_sender();
    for i in 0..60 {
        events.blocking_send(sim_bare_fill(&format!("f{i}"), 1.0, 100.0)).unwrap();
    }
    handle.shutdown_and_join();
    let seg0 = dir.join("journal-00000000.vjl");
    assert!(last_segment(&dir) != seg0, "the scenario must roll past segment 0");
    let byte = std::fs::read(&seg0).unwrap()[HEADER + 8];
    plant(&seg0, HEADER + 8, &[byte ^ 0xFF]);

    let restore =
        restore_for_boot(Some(&journal_at(&dir, 4 * 1024, 8)), &tmp, 42).expect("readable");
    let RestoreOutcome::Broken { cause, moved_to } = restore.outcome() else {
        panic!("broken, got {:?}", restore.outcome());
    };
    assert_eq!(*cause, CorruptCause::CheckMismatch);
    let moved = moved_to.as_ref().expect("moved aside");
    assert_eq!(moved, &root.join("journal.broken-0000000000042"));
    assert!(moved.join("journal-00000000.vjl").exists(), "nothing was deleted");
    assert!(!dir.exists(), "the journal directory is free for a fresh journal");
    assert!(restore.restored_engines().is_empty() && restore.coid_session().is_none());
    let line = restore.startup_line().unwrap();
    assert!(line.contains("BROKEN (check_mismatch)"), "{line}");
    assert!(
        restore.notes().iter().any(|n| n.starts_with("JOURNAL BROKEN")),
        "{:?}",
        restore.notes()
    );
    assert!(
        restore.notes().iter().any(|n| n.starts_with("PROTECTION LOST")),
        "no checkpoint was readable, so the loss is named rather than counted: {:?}",
        restore.notes()
    );
    let fresh = CommandJournal::open(&dir, small_file()).unwrap();
    drop(fresh);
    assert!(CommandJournal::read_all(&dir).unwrap().is_empty(), "a fresh journal");
}

/// A frame that passes its checksum and is not a record (`ParseFailed`: another build wrote it) is
/// BROKEN even at the very end of the journal — it is not the shape a crash leaves.
#[test]
fn a_parse_failed_frame_is_broken_not_torn() {
    let root = Scratch::created("boot-restore-parse-failed");
    let tmp = Scratch::created("boot-restore-parse-failed-tmp");
    let dir = root.join("journal");
    build_source_journal(&dir);
    let seg = last_segment(&dir);
    let payload = br#"{"NotARecordOfThisBuild":{}}"#;
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&fnv1a32(payload).to_le_bytes());
    frame.extend_from_slice(payload);
    plant(&seg, end_of_frames(&seg), &frame);

    let restore =
        restore_for_boot(Some(&JournalConfig::at(dir.clone())), &tmp, 5).expect("readable");
    assert!(
        matches!(
            restore.outcome(),
            RestoreOutcome::Broken { cause: CorruptCause::ParseFailed, moved_to: Some(_) }
        ),
        "{:?}",
        restore.outcome()
    );
    assert!(!dir.exists());
}

/// A crash tail of WAKER records (the live node arms the stuck-order watchdog, so its journal
/// always carries them): the refold skips them instead of refusing, counts them on the line, and
/// still folds the fill behind them.
#[test]
fn a_watchdog_tail_skips_the_waker_records_and_reports_the_count() {
    let dir = Scratch::reserved("boot-restore-wakers");
    let tmp = Scratch::created("boot-restore-wakers-tmp");
    {
        let mut j = CommandJournal::open(&dir, small_file()).unwrap();
        let engines = vec![sim_engine().snapshot_state()];
        let hash = vike_exec::state_hash(&engines);
        j.append_snap(10, &engines, "cafef00d", 0, 0, SnapBooks::default(), hash).unwrap();
        for t in 11..14 {
            j.append_cmd(t, &Ingest::Watchdog).unwrap();
        }
        j.append_cmd(14, &Ingest::Event(sim_bare_fill("w1", 1.0, 100.0))).unwrap();
    }

    let restore =
        restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &tmp, 7).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let line = restore.startup_line().unwrap();
    assert!(line.contains("(3 waker records skipped)"), "{line}");
    let engine = &restore.restored_engines()[0];
    assert!(engine.seen_trade_ids.iter().any(|t| t == "w1"), "the fill behind them folded");
    assert!(engine.account.positions.iter().any(|(_, p)| p.size == 1.0));
}

/// The replay core's scratch goes under the root the caller hands in, never the system temp: a root
/// that cannot hold a directory (a FILE) makes the refold fail, naming that root, and the restore
/// falls back to the latest `Snap`.
#[test]
fn restore_for_boot_writes_its_scratch_under_the_given_root_not_the_system_temp() {
    let dir = Scratch::reserved("boot-restore-scratch");
    let tmp = Scratch::created("boot-restore-scratch-tmp");
    build_source_journal(&dir);
    let not_a_dir = tmp.join("not-a-dir");
    std::fs::write(&not_a_dir, b"a file").unwrap();

    let restore =
        restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &not_a_dir, 7).unwrap();
    let RestoreOutcome::SnapOnly { why } = restore.outcome() else {
        panic!("snap-only, got {:?}", restore.outcome());
    };
    assert!(why.contains("not-a-dir"), "the refold named the root it was given: {why}");
    assert!(!restore.restored_engines().is_empty(), "the latest Snap still restores");
}

/// No journal configured: nothing is read, nothing is said. A configured journal whose directory
/// does not exist yet is a FRESH restore, which is said.
#[test]
fn no_journal_means_no_restore_and_no_line_and_a_missing_directory_is_fresh() {
    let tmp = Scratch::created("boot-restore-off-tmp");
    let off = restore_for_boot(None, &tmp, 1).unwrap();
    assert_eq!(off.outcome(), &RestoreOutcome::Off);
    assert!(off.startup_line().is_none());

    let dir = Scratch::reserved("boot-restore-fresh");
    let fresh = restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &tmp, 1).unwrap();
    assert!(matches!(fresh.outcome(), RestoreOutcome::Fresh { .. }), "{:?}", fresh.outcome());
    let line = fresh.startup_line().unwrap();
    assert!(line.contains("fresh (the journal directory does not exist yet)"), "{line}");
    assert!(!dir.exists(), "a restore creates no journal directory");
}

/// A segment a NEWER build stamped (a rollback without moving the journal aside) is a refused start
/// that names the cure — not the core's `expect` panicking at the mount a moment later.
#[test]
fn a_journal_a_newer_build_wrote_is_a_refused_start() {
    let dir = Scratch::reserved("boot-restore-newer");
    let tmp = Scratch::created("boot-restore-newer-tmp");
    build_source_journal(&dir);
    let seg = last_segment(&dir);
    plant(&seg, 4, &99u32.to_le_bytes());

    let err = restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &tmp, 1)
        .expect_err("refused");
    assert!(err.contains("format version") && err.contains("moved aside"), "{err}");
}

/// A journal ANOTHER instance is writing (its single-writer lock held) is a refused start that
/// reads and changes nothing — even a corrupt one is not moved aside under a live writer.
#[test]
fn a_journal_another_instance_is_writing_is_refused_and_left_untouched() {
    let dir = Scratch::reserved("boot-restore-locked");
    let tmp = Scratch::created("boot-restore-locked-tmp");
    build_source_journal(&dir);
    let before = CommandJournal::read_all(&dir).unwrap().len();
    let live = CommandJournal::open(&dir, small_file()).unwrap();

    let err = restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &tmp, 1)
        .expect_err("refused while another writer holds the lock");
    assert!(err.contains("refused, nothing read or changed"), "{err}");
    assert!(!dir.join(vike_journal::TORN_DIR).exists(), "no copy was taken");
    drop(live);
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), before, "the journal is untouched");
}

/// The same, for the PRIMARY (engine 0): its snapshot is always journaled (the replay takes the first one
/// as the primary), so the journal holds two carriers of its key, both with an explicit key. The next boot
/// applies the newest.
#[test]
fn a_stopped_then_restarted_primary_is_restored_by_the_next_boot() {
    let dir = Scratch::reserved("boot-restore-primary-stop-start");
    let tmp = Scratch::created("boot-restore-primary-stop-start-tmp");
    let config = |journal: Option<JournalConfig>| CoreConfig {
        seed_cash: 1_000.0,
        clock: counter_clock(),
        coid_session: Some(("cafef00d".into(), 0)),
        journal,
        ..CoreConfig::default()
    };
    let handle =
        spawn_core_multi(sim_engine(), Vec::new(), config(Some(journal_at(&dir, 1 << 20, 1_000))));
    let events = handle.event_sender();
    events
        .stop_account_engine(vike_exec::NotRunningAccount {
            venue: "sim".to_string(),
            account: None,
            route_key: "sim".to_string(),
            tier: None,
            reason: "account inactive".to_string(),
        })
        .unwrap();
    wait_for(&handle, "the stop", |s| {
        s.recent_events.iter().any(|l| l.contains("ACCOUNT STOPPED"))
    });
    events.detach_account_engine("sim").unwrap();
    events.add_account_engine(500.0, sim_engine()).unwrap();
    wait_for(&handle, "the new engine", |s| s.portfolio.venues.len() == 2);
    events.routed("sim").blocking_send(sim_bare_fill("t-again", 2.0, 100.0)).unwrap();
    wait_for(&handle, "the new engine's position", |s| every_position(s).len() == 1);
    handle.shutdown_and_join();

    let restore =
        restore_for_boot(Some(&journal_at(&dir, 1 << 20, 1_000)), &tmp, 42).expect("readable");
    let mut config2 = config(None);
    restore.apply_to(&mut config2);
    let handle = spawn_core_multi(sim_engine(), Vec::new(), config2);
    handle.tick_sender().quote(sim_quote(100.0, 2)).unwrap(); // a publish
    let snap = wait_for(&handle, "the restored position", |s| every_position(s).len() == 1);
    assert_eq!(every_position(&snap)[0].size, 2.0, "the primary's position was restored");
    handle.shutdown_and_join();
}
