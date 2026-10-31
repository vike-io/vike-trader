//! **A base whose engines an order's venue cannot tell apart is REFOLDED, not read from its latest `Snap`
//! alone** (decision 0127's reopener, fired): a labelled account (`sim#ALT`) or two engines of one venue.
//!
//! The crash tail of such a base used to be unreplayable: a strategy's `StrategySubmit`, a conditional's
//! `ConditionalFire` and a margin call's release replayed as `Command::Order`, which lowers by the
//! payload's VENUE, while the live core lowered them onto the engine they belonged to. Each now carries the
//! route key of that engine and replays as `Command::OrderOn`; a gateway event replays with its ring, which
//! names the account of a fill no coid can. These tests gate what only a journal on disk shows:
//!
//! * each submit lands on the engine its record names (`sim#ALT`, and the default account beside it, which
//!   the payload route would have refused as ambiguous);
//! * a record with no key is read as the venue's default account only where the base has exactly that
//!   engine, and otherwise the whole refold is declined (the journal predates the key): snap-only, as before;
//! * a fill with no coid follows its gateway ring on a symbol both accounts trade;
//! * an order the base already holds keeps its engine for the tail's lifecycle events;
//! * and the live writer and the refold agree, end to end.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use vike_core::replay::{BootRestore, RestoreOutcome, restore_for_boot};
use vike_core::{CoreConfig, JournalConfig, LiveBroker, spawn_core_multi};
use vike_exec::{GatewayCursor, GatewayEvent, Ingest, OrderIntent};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord, SnapBooks};
use vike_marketdata::QuoteTick;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{Clock, OrderRequest, OrderStatus, Strategy};

use crate::kit::engines::{engine_on, sim_engine};
use crate::kit::events::{sim_bare_fill, sim_quote};
use crate::kit::requests::sim_limit;

const ALT: &str = "sim#ALT";

fn open(dir: &Path) -> CommandJournal {
    CommandJournal::open(dir, JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 }).unwrap()
}

fn journal_at(dir: &Path) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1_000,
    }
}

/// The default account `sim` and the labelled account `sim#ALT`, both mounted on BTCUSDT: the spread shape,
/// two accounts on one instrument.
fn two_accounts() -> Vec<vike_exec::EngineSnapshot> {
    let default = sim_engine().snapshot_state();
    let mut alt = sim_engine().snapshot_state();
    alt.route_key = Some(ALT.into());
    vec![default, alt]
}

fn write_base(j: &mut CommandJournal, engines: &[vike_exec::EngineSnapshot]) {
    let hash = vike_exec::state_hash(engines);
    j.append_snap(10, engines, "cafef00d", 0, 0, SnapBooks::default(), hash).unwrap();
}

fn restore(dir: &Path, tmp: &Path) -> BootRestore {
    restore_for_boot(Some(&journal_at(dir)), tmp, 42).expect("readable")
}

fn position(e: &vike_exec::EngineSnapshot) -> f64 {
    e.account.positions.iter().map(|(_, p)| p.size).sum()
}

fn ring_fill(tid: &str, qty: f64, ring: &str, seq: u64) -> Ingest {
    Ingest::GatewayEvent(Box::new(GatewayEvent {
        event: sim_bare_fill(tid, qty, 100.0),
        route_key: ring.into(),
        cursor: GatewayCursor { gw_epoch: 7, seq, ts_ms: seq as i64 },
    }))
}

/// **EACH SUBMIT LANDS ON THE ENGINE ITS RECORD NAMES.** Two mounts' submits sit in the tail: one on the
/// labelled `sim#ALT`, one on the default account `sim`. The payload route reaches the default account
/// or is refused as ambiguous, so before the key the base could not be refolded at all (a labelled
/// engine in it was the refusal itself).
#[test]
fn each_submit_lands_on_the_engine_its_record_names() {
    let dir = Scratch::reserved("labelled-refold-submits");
    let tmp = Scratch::created("labelled-refold-submits-tmp");
    {
        let mut j = open(&dir);
        write_base(&mut j, &two_accounts());
        for (i, key) in [ALT, "sim"].into_iter().enumerate() {
            let t = 11 + i as i64;
            let intent = OrderIntent::Submit(Box::new(sim_limit("", 1, 1.0, 100.0)));
            j.append_strategy_submit(t, "mount", &intent, Some(key)).unwrap();
            // `MintedSubmit` names the default account by ABSENCE (`CoreThread::journal_route_key`).
            let minted = sim_limit(&format!("cafef00d{i}"), 1, 1.0, 100.0);
            j.append_minted_submit(t, &minted, (key != "sim").then_some(key)).unwrap();
        }
        j.flush().unwrap();
    }

    let restore = restore(&dir, &tmp);
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let engines = restore.restored_engines();
    assert_eq!(engines.len(), 2);
    assert_eq!(engines[0].route_key, None, "the default account is first, as in the base");
    assert_eq!(engines[0].registry.len(), 1, "the default account's own submit");
    assert_eq!(engines[1].route_key.as_deref(), Some(ALT));
    assert_eq!(engines[1].registry.len(), 1, "the labelled account's own submit");
    assert_eq!(restore.coid_session().map(|s| s.1), Some(2), "both mints are counted");
}

/// **A RECORD WITH NO KEY IS READ AS THE DEFAULT ACCOUNT ONLY WHERE THE BASE HAS EXACTLY THAT ENGINE.** A
/// journal written before the key holds `StrategySubmit`s that name no engine, and on a venue with two
/// accounts the venue cannot say which: the refold is declined (the restore reads the latest `Snap` alone,
/// as it did for every such base), and the coid counter still covers the mint the tail made.
#[test]
fn a_record_with_no_key_on_a_venue_of_two_accounts_declines_the_refold() {
    let dir = Scratch::reserved("labelled-refold-legacy");
    let tmp = Scratch::created("labelled-refold-legacy-tmp");
    {
        let mut j = open(&dir);
        write_base(&mut j, &two_accounts());
        let intent = OrderIntent::Submit(Box::new(sim_limit("", 1, 1.0, 100.0)));
        j.append_strategy_submit(11, "mount", &intent, None).unwrap();
        j.append_minted_submit(11, &sim_limit("cafef00d0", 1, 1.0, 100.0), None).unwrap();
        j.flush().unwrap();
    }

    let restore = restore(&dir, &tmp);
    let RestoreOutcome::SnapOnly { why } = restore.outcome() else {
        panic!("a record that names no engine cannot be placed, got {:?}", restore.outcome());
    };
    assert!(why.contains("carries no route key") && why.contains("`sim`"), "{why}");
    assert_eq!(restore.coid_session().map(|s| s.1), Some(1), "the tail's mint is not re-minted");
}

/// …whereas a venue the labelled account does not share keeps the payload route: `other` has ONE engine,
/// its default account, which the venue names exactly. This is the shape of a node with a labelled account
/// on one venue and ordinary single-account venues beside it, whose tail used to be snap-only too.
#[test]
fn a_record_with_no_key_on_a_venue_of_one_default_account_still_lands() {
    let dir = Scratch::reserved("labelled-refold-solo");
    let tmp = Scratch::created("labelled-refold-solo-tmp");
    let other = engine_on("other", "XYZ", vike_exec::testing::RecordingClient::default());
    let mut labelled = sim_engine().snapshot_state();
    labelled.route_key = Some(ALT.into());
    {
        let mut j = open(&dir);
        write_base(&mut j, &[labelled, other.snapshot_state()]);
        let req = OrderRequest {
            venue: "other".into(),
            symbol: "XYZ".into(),
            ..sim_limit("", 1, 1.0, 100.0)
        };
        j.append_strategy_submit(11, "mount", &OrderIntent::Submit(Box::new(req.clone())), None)
            .unwrap();
        let minted = OrderRequest { client_order_id: "cafef00d0".into(), ..req };
        j.append_minted_submit(11, &minted, None).unwrap();
        j.flush().unwrap();
    }

    let restore = restore(&dir, &tmp);
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let engines = restore.restored_engines();
    assert_eq!(engines[0].registry.len(), 0, "the labelled account was not touched");
    assert_eq!(engines[1].registry.len(), 1, "`other`'s own submit");
}

/// **A FILL WITH NO COID FOLLOWS ITS GATEWAY RING** on a symbol both accounts trade: the one handle that
/// remains (`route_event_via`), and the reason the refold now replays a gateway event as itself on such a
/// base. Un-ringed it would be dropped as a foreign fill (two accounts, one symbol, no coid).
#[test]
fn a_fill_with_no_coid_follows_its_gateway_ring() {
    let dir = Scratch::reserved("labelled-refold-ring");
    let tmp = Scratch::created("labelled-refold-ring-tmp");
    {
        let mut j = open(&dir);
        write_base(&mut j, &two_accounts());
        j.append_cmd(11, &ring_fill("g1", 1.0, "sim", 1)).unwrap();
        j.append_cmd(12, &ring_fill("g2", 2.0, ALT, 1)).unwrap();
        j.flush().unwrap();
    }

    let restore = restore(&dir, &tmp);
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let engines = restore.restored_engines();
    assert_eq!(position(&engines[0]), 1.0, "the default account's ring");
    assert_eq!(position(&engines[1]), 2.0, "the labelled account's ring");
    assert_eq!(restore.gateway_cursors().len(), 2, "both rings advanced with their fold");
}

/// **AN ORDER THE BASE ALREADY HOLDS KEEPS ITS ENGINE.** The labelled account rests `pre-1` at the `Snap`;
/// the tail's lifecycle events name only the coid, and the replay core never saw the submit. Its row comes
/// from the base's registry (`CoreConfig::coid_routes`); without it the event routes to engine 0 and the
/// labelled account's order stays `Initialized`.
#[test]
fn an_order_the_base_already_holds_keeps_its_engine() {
    let dir = Scratch::reserved("labelled-refold-pre-base");
    let tmp = Scratch::created("labelled-refold-pre-base-tmp");
    let mut alt = sim_engine();
    alt.route_key = ALT.to_string();
    let mut outbox = vike_exec::Outbox::default();
    alt.submit_order(&sim_limit("pre-1", 1, 1.0, 100.0), 1, &mut outbox);
    let mut alt_snap = alt.snapshot_state();
    alt_snap.route_key = Some(ALT.into());
    {
        let mut j = open(&dir);
        write_base(&mut j, &[sim_engine().snapshot_state(), alt_snap]);
        for ev in [
            Event::OrderSubmitted(OrderSubmitted { client_order_id: "pre-1".into(), ts: 11 }),
            Event::OrderAccepted(OrderAccepted {
                client_order_id: "pre-1".into(),
                venue_order_id: Some("v-1".into()),
                ts: 12,
            }),
        ] {
            j.append_cmd(11, &Ingest::Event(ev)).unwrap();
        }
        j.flush().unwrap();
    }

    let restore = restore(&dir, &tmp);
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let status = restore.restored_engines()[1]
        .registry
        .iter()
        .find(|(coid, _)| coid == "pre-1")
        .map(|(_, o)| o.status);
    assert_eq!(
        status,
        Some(OrderStatus::Accepted),
        "the venue's answers reached the labelled account"
    );
}

/// Rests ONE tagged limit (a 1.0 buy at 100.0) on its first quote tick, then never trades again.
struct RestOnce {
    done: Arc<AtomicBool>,
}

impl Strategy<LiveBroker> for RestOnce {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if !self.done.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
        }
    }
}

/// **THE LIVE WRITER AND THE REFOLD AGREE, END TO END.** A real core runs the two accounts with a mount on
/// the LABELLED one: it rests a bid, and a ringed fill books into `sim#ALT`. Its journal's write-ahead
/// records name that engine; and when the exit `Snap` is taken away (a crash leaves the same shape) and
/// replaced by the base the core started from, the restore refolds the tail into exactly the book the
/// live core ended with: the same resting order under the same re-minted coid, the same position, and
/// nothing on the default account.
#[test]
fn the_live_writer_names_the_engine_and_the_refold_reproduces_the_book() {
    let dir = Scratch::reserved("labelled-refold-e2e");
    let crashed = Scratch::reserved("labelled-refold-e2e-crashed");
    let tmp = Scratch::created("labelled-refold-e2e-tmp");
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let engines = || {
        let mut alt = sim_engine();
        alt.route_key = ALT.to_string();
        (sim_engine(), vec![(500.0, alt)])
    };
    let config = CoreConfig {
        seed_cash: 1_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        strategy: Some(vike_core::StrategyMount {
            account: Some(AccountLabel::parse("ALT").unwrap()),
            symbols: Vec::new(),
            controller_id: Some("alt-bidder".into()),
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(RestOnce { done: Arc::default() }),
        }),
        journal: Some(journal_at(&dir)),
        ..CoreConfig::default()
    };
    let (primary, extras) = engines();
    let handle = spawn_core_multi(primary, extras, config);
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while handle.snapshot().orders.len() != 1 {
        assert!(Instant::now() < deadline, "the mount's bid never rested");
        std::thread::sleep(Duration::from_millis(5));
    }
    let Ingest::GatewayEvent(fill) = ring_fill("g1", 1.0, ALT, 1) else { unreachable!() };
    handle.event_sender().blocking_send_gateway(*fill).unwrap();
    handle.shutdown_and_join();

    // The write-ahead records name the labelled engine.
    let records = CommandJournal::read_all(&dir).unwrap();
    let submit_keys: Vec<Option<&str>> = records
        .iter()
        .filter_map(|r| match r {
            JournalRecord::StrategySubmit { route_key, .. } => Some(route_key.as_deref()),
            _ => None,
        })
        .collect();
    assert_eq!(submit_keys, [Some(ALT)], "the mount's submit names the engine it lowers onto");

    // The live core's own end state (the exit `Snap`), and the crash: that journal minus its exit `Snap`,
    // behind the base the core started from.
    let live = records
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, .. } => Some(engines.clone()),
            _ => None,
        })
        .expect("a clean stop writes an exit Snap");
    assert_eq!(position(&live[1]), 1.0, "precondition: the ringed fill booked into `sim#ALT`");
    {
        let mut j = open(&crashed);
        write_base(&mut j, &two_accounts());
        for r in records {
            match r {
                JournalRecord::Cmd { now_ms, msg, .. } => {
                    j.append_cmd(now_ms, &msg).unwrap();
                }
                JournalRecord::StrategySubmit { now_ms, mount_id, intent, route_key, .. } => {
                    j.append_strategy_submit(now_ms, &mount_id, &intent, route_key.as_deref())
                        .unwrap();
                }
                JournalRecord::MintedSubmit { now_ms, req, route_key, .. } => {
                    j.append_minted_submit(now_ms, &req, route_key.as_deref()).unwrap();
                }
                JournalRecord::Snap { .. } => {}
                other => panic!("this scenario writes no {other:?}"),
            }
        }
        j.flush().unwrap();
    }

    let restore = restore(&crashed, &tmp);
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let refolded = restore.restored_engines();
    assert_eq!(position(&refolded[1]), position(&live[1]));
    assert_eq!(position(&refolded[0]), 0.0, "nothing reached the default account");
    let coids = |e: &vike_exec::EngineSnapshot| -> Vec<String> {
        e.registry.iter().map(|(coid, _)| coid.clone()).collect()
    };
    assert_eq!(coids(&refolded[1]), coids(&live[1]), "the same bid under the same re-minted coid");
    assert_eq!(coids(&refolded[1]).len(), 1);
    assert!(coids(&refolded[0]).is_empty());
    assert_eq!(restore.coid_session().map(|s| s.1), Some(1));
}
