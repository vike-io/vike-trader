//! **The tradehub's half of a restart, over a real socket** (wave 2, W13): what the boot restore hands a
//! lease, and what the tradehub does when the gatewayhub it leases from restarts under it. Driven
//! against the gatewayhub client's own fake gatewayhub
//! (`crates/vike-gatewayhub-client/tests/support/fake_trade_peer.rs`), so every claim is read off the
//! FRAMES the peer recorded.
//!
//! * the journal cursor rides `TradeOpen.resume` for the account whose route key it names, and only that
//!   one;
//! * the boot wire-id prefetch is ONE `WireIds` request for the restored coids, on the boot thread,
//!   after the adapter answered and before any core exists; a miss afterwards is `None`; and the reconcile
//!   client says NOT READY (never an empty answer) until the prefetch has been paid;
//! * **a gatewayhub restart** (a new epoch) is answered by: `AdapterStart` again, `AdoptRestored` for
//!   EVERY resting order of the account and no other, and an immediate reconcile trigger. The bridge's
//!   in-memory order-id map (polymarket's CLOB ids, ig's deal ids, ctrader's `OrderIdMap`, hyperliquid's
//!   cloid registry) is modelled by the test as "what the frames since the restart taught"; the cancel
//!   of a resting order resolves only when the restart's frames taught it.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use vike_config::ArmingBlock;
use vike_exec::{CoreSnapshot, ExecutionClient, GatewayCursor, OrderView};
use vike_gatewayhub_client::proto::{
    ArmingRow, ArmingTable, GridSource, Request, Response, TradeDown, TradeUp, VenueFacts,
};
use vike_gatewayhub_client::trade::is_recon_not_ready;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, OrderStatus, OrderType};
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub::remote_backend::restore::BootHints;
use vike_tradehub::server::symbol_admission::SymbolFacts;
use vike_tradehub_core::{AdapterBind, AdapterPolicy, AdapterStartQuery, VenueBackend};

#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_trade_peer;

use fake_trade_peer::{
    Seen, Step, TradePeer, TradePeerConfig, default_script, fast_trade_timings, link_to,
};

const ID: &str = "vike-tradehub-restart-test";
const WAIT: Duration = Duration::from_secs(8);

fn accounts(venue: &str) -> AccountDirectory {
    AdapterPolicy::default().with_account(venue, &AccountLabel::Default, AccountTier::Demo).accounts
}

fn config_for(venue: &str, script: Option<fake_trade_peer::Script>) -> TradePeerConfig {
    let venue = venue.to_string();
    let mut cfg = TradePeerConfig::new();
    cfg.route_key = venue.clone();
    cfg.features.push(vike_gatewayhub_client::proto::trade_venue_feature(&venue));
    let arming_venue = venue.clone();
    let mut cfg = cfg.with_rpc(move |req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(ArmingTable {
            facts: std::collections::BTreeMap::from([(
                arming_venue.clone(),
                VenueFacts {
                    adapter_present: true,
                    addresses_accounts: true,
                    takes_recon_trigger: false,
                    grid_source: GridSource::NoGrid,
                    process_exclusive: None,
                    clock_decl_word: "wired".to_string(),
                },
            )]),
            rows: vec![ArmingRow {
                venue: arming_venue.clone(),
                label: None,
                tier: Some(AccountTier::Demo),
                effective: Some(AccountTier::Demo),
                block_word: ArmingBlock::None.word().to_string(),
                account_ids: vec![1],
                book: None,
            }],
            key_changed: Vec::new(),
        }))],
        other => vec![Response::Error(format!("unscripted: {other:?}"))],
    });
    if let Some(script) = script {
        cfg.script = script;
    }
    cfg
}

fn connect(peer: &TradePeer, venue: &str, boot: BootHints) -> RemoteBackend {
    RemoteBackend::connect(link_to(&peer.addr_string()), accounts(venue), ID.to_string(), WAIT)
        .with_timings(fast_trade_timings())
        .with_boot(boot)
}

fn query<'a>(venue: &'a str, label: &'a AccountLabel) -> AdapterStartQuery<'a> {
    AdapterStartQuery {
        venue,
        account: label,
        tier: AccountTier::Demo,
        symbol: "BTCUSDT",
        declared_legs: &[],
        recon_enabled: true,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: venue,
        properties_rec: None,
    }
}

fn order_view(coid: &str, venue: &str, account: Option<&str>, status: OrderStatus) -> OrderView {
    OrderView {
        client_order_id: coid.to_string(),
        venue: venue.to_string(),
        account: account.map(|l| AccountLabel::parse(l).expect("a label")),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        data_delay: None,
    }
}

fn adapter_starts(peer: &TradePeer) -> usize {
    peer.ups().iter().filter(|f| matches!(f, TradeUp::AdapterStart(_))).count()
}

// ---- the cursor ------------------------------------------------------------------------------------

#[test]
fn the_journal_cursor_rides_trade_open_for_its_own_account_only() {
    let peer = TradePeer::start(config_for("binance", None));
    let cursor = GatewayCursor { gw_epoch: 0x77, seq: 41, ts_ms: 1_700 };
    let boot = BootHints::of(
        vec![("binance".to_string(), cursor), ("binance#ALT".to_string(), cursor)],
        &[],
    );
    let backend = connect(&peer, "binance", boot);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    backend.start_adapter(query("binance", &label), &events, None).expect("starts");
    let opened = peer.seen().into_iter().find_map(|s| match s {
        Seen::Open { account, resume, .. } => Some((account, resume)),
        _ => None,
    });
    let (account, resume) = opened.expect("a TradeOpen");
    assert_eq!(account, None);
    let resume = resume.expect("the default account's cursor is presented");
    assert_eq!((resume.gw_epoch, resume.seq, resume.ts_ms), (0x77, 41, 1_700));

    // No cursor for the account: it opens fresh (and reconciles).
    let fresh = TradePeer::start(config_for("binance", None));
    let backend = connect(&fresh, "binance", BootHints::none());
    backend.start_adapter(query("binance", &label), &events, None).expect("starts");
    assert!(
        fresh.opens().iter().all(|(_, _, resume, _)| resume.is_none()),
        "an account the restore holds no cursor for opens with none"
    );
}

// ---- the wire-id prefetch --------------------------------------------------------------------------

#[test]
fn the_restored_coids_are_prefetched_once_after_the_adapter_and_before_the_fold_asks() {
    let peer = TradePeer::start(config_for("hyperliquid", None));
    let boot = BootHints::of(
        vec![],
        &[
            ("hl-resting", "hyperliquid", None),
            ("hl-alt", "hyperliquid", Some("ALT")),
            ("bn-other", "binance", None),
        ],
    );
    let backend = connect(&peer, "hyperliquid", boot);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let bind = backend.start_adapter(query("hyperliquid", &label), &events, None).expect("starts");

    let ups = peer.ups();
    let start_at =
        ups.iter().position(|f| matches!(f, TradeUp::AdapterStart(_))).expect("AdapterStart");
    let ask_at = ups
        .iter()
        .position(|f| matches!(f, TradeUp::WireIds { .. }))
        .expect("the boot prefetch asked");
    assert!(start_at < ask_at, "the adapter answers before the prefetch: {ups:?}");
    let asks: Vec<_> = ups
        .iter()
        .filter_map(|f| match f {
            TradeUp::WireIds { coids, .. } => Some(coids.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(asks, vec![vec!["hl-resting".to_string()]], "ONE request, this account's coid only");

    // The fold's `wire_id_for` is a cache read from here on.
    assert_eq!(bind.client.wire_id_for("hl-resting"), Some("0xhl-resting".to_string()));
}

#[test]
fn a_restored_set_beyond_one_frame_is_prefetched_in_full_and_nothing_is_cut() {
    // W13b finding 4: the set was cut at 4096 by sort order; the last coid below sorts last and would have
    // been the one left out. It is asked about, and answered, like the rest.
    let peer = TradePeer::start(config_for("hyperliquid", None));
    let ids: Vec<String> = (0..5_000).map(|i| format!("hl-{i:05}")).collect();
    let orders: Vec<(&str, &str, Option<&str>)> =
        ids.iter().map(|c| (c.as_str(), "hyperliquid", None)).collect();
    let backend = connect(&peer, "hyperliquid", BootHints::of(vec![], &orders));
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let bind = backend.start_adapter(query("hyperliquid", &label), &events, None).expect("starts");

    let asked: Vec<usize> = peer
        .ups()
        .iter()
        .filter_map(|f| match f {
            TradeUp::WireIds { coids, .. } => Some(coids.len()),
            _ => None,
        })
        .collect();
    assert_eq!(asked.iter().sum::<usize>(), 5_000, "every restored coid is asked: {asked:?}");
    assert!(asked.len() >= 2, "in pages, not one oversized frame: {asked:?}");
    assert_eq!(
        bind.client.wire_id_for("hl-04999"),
        Some("0xhl-04999".to_string()),
        "the coid that sorts last is not a cache miss"
    );
}

#[test]
fn a_reconcile_is_not_ready_until_the_prefetch_has_been_paid_and_never_an_empty_answer() {
    let answer = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&answer);
    let base = default_script();
    let script: fake_trade_peer::Script = Arc::new(move |frame| match frame {
        TradeUp::WireIds { .. } if !gate.load(Ordering::SeqCst) => {
            vec![Step::Sleep(Duration::from_millis(1))]
        }
        other => base(other),
    });
    let peer = TradePeer::start(config_for("hyperliquid", Some(script)));
    let boot = BootHints::of(vec![], &[("hl-1", "hyperliquid", None)]);
    let backend = connect(&peer, "hyperliquid", boot);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    // The daemon never answers the prefetch within the (fast) request bound: the start still lands, the
    // prefetch stays owed and says so.
    let bind = backend.start_adapter(query("hyperliquid", &label), &events, None).expect("starts");
    let recon = bind.recon.expect("a reconcile handle");
    let err = recon.fetch_order_status_reports(0).expect_err("an owed prefetch is not an empty Ok");
    assert!(is_recon_not_ready(&err), "{err}");

    answer.store(true, Ordering::SeqCst);
    assert_eq!(recon.fetch_order_status_reports(0), Ok(Vec::new()), "paid: the pass runs");
    assert_eq!(bind.client.wire_id_for("hl-1"), Some("0xhl-1".to_string()));
}

// ---- a gatewayhub restart --------------------------------------------------------------------------

/// What the frames since the restart taught the (empty) bridge map.
fn taught_since(peer: &TradePeer, second_start: usize) -> BTreeSet<String> {
    let mut starts = 0;
    let mut taught = BTreeSet::new();
    for frame in peer.ups() {
        match frame {
            TradeUp::AdapterStart(_) => starts += 1,
            TradeUp::AdoptRestored { pairs, .. } if starts >= second_start => {
                taught.extend(pairs.into_iter().map(|(coid, _)| coid));
            }
            _ => {}
        }
    }
    taught
}

#[test]
fn a_gatewayhub_restart_restarts_the_adapter_reteaches_every_resting_order_and_reconciles_at_once()
{
    for venue in ["polymarket", "ig", "ctrader", "hyperliquid"] {
        let peer = TradePeer::start(config_for(venue, None));
        let cell = Arc::new(OnceLock::new());
        let backend =
            connect(&peer, venue, BootHints::none()).with_snapshot_cell(Arc::clone(&cell));
        let (events, _lane) = fake_trade_peer::lane();
        let (trigger, pokes) = channel::<()>();
        let label = AccountLabel::Default;
        let mut bind =
            backend.start_adapter(query(venue, &label), &events, Some(trigger)).expect("starts");
        assert_eq!(adapter_starts(&peer), 1);

        // The core's view: two orders rest on this account; a filled one, another account's and
        // another venue's do not count.
        let mut snapshot = CoreSnapshot::empty(venue, "BTCUSDT");
        snapshot.orders = vec![
            order_view("live-1", venue, None, OrderStatus::Accepted),
            order_view("live-2", venue, None, OrderStatus::PartiallyFilled),
            order_view("done", venue, None, OrderStatus::Filled),
            order_view("alt", venue, Some("ALT"), OrderStatus::Accepted),
            order_view("elsewhere", "somewhere", None, OrderStatus::Accepted),
        ];
        cell.set(Arc::new(ArcSwap::from_pointee(snapshot))).expect("set once");

        // THE RESTART: a new epoch, and the bridge's in-memory map is empty. A cancel of a resting
        // order resolves only when the frames since the restart taught it.
        peer.restart();
        assert!(
            TradePeer::wait_until(WAIT, || adapter_starts(&peer) >= 2),
            "{venue}: the adapter is started again: {:?}",
            peer.ups()
        );
        assert!(
            TradePeer::wait_until(WAIT, || !taught_since(&peer, 2).is_empty()),
            "{venue}: the resting orders are re-taught: {:?}",
            peer.ups()
        );
        let map = taught_since(&peer, 2);
        let resting: BTreeSet<String> = ["live-1", "live-2"].map(String::from).into();
        assert_eq!(map, resting, "{venue}: EVERY resting order of THIS account, and nothing else");
        for coid in &resting {
            assert!(map.contains(coid), "{venue}: a cancel of {coid} resolves after the restart");
        }
        assert!(
            pokes.recv_timeout(WAIT).is_ok(),
            "{venue}: an immediate reconcile pass is requested (whatever the venue's trigger fact)"
        );

        // The restart is announced once: no second AdapterStart without a second epoch change.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(adapter_starts(&peer), 2, "{venue}");
        bind.client.cancel("live-1");
        drop(bind);
    }
}

#[test]
fn a_held_risk_off_cancel_goes_out_after_the_teaching_of_an_order_minted_since_boot() {
    // W13b finding 1: the dead-man's cancel written (never acknowledged) when the gatewayhub died is held
    // by the client and sent again once the new adapter is up. On a venue whose order-id map the restart
    // emptied it resolves only if the order was taught FIRST: the order below was minted after boot, so
    // only the re-teaching worker knows it.
    // The four venues whose bridge map the restart empties, and a control that keeps no such map: the peer
    // models "a cancel of an order not taught since the restart fails with no venue order id" for the first
    // four only (W13c: the old peer was venue-agnostic, so the loop added no coverage).
    for venue in ["polymarket", "ig", "ctrader", "hyperliquid", "binance"] {
        let failed = Arc::new(Mutex::new(Vec::<String>::new()));
        let peer = TradePeer::start(
            config_that_forgets_on_restart(venue, Arc::clone(&failed)).never_ack(),
        );
        let cell = Arc::new(OnceLock::new());
        let backend =
            connect(&peer, venue, BootHints::none()).with_snapshot_cell(Arc::clone(&cell));
        let (events, _lane) = fake_trade_peer::lane();
        let label = AccountLabel::Default;
        let mut bind = backend.start_adapter(query(venue, &label), &events, None).expect("starts");
        let mut snapshot = CoreSnapshot::empty(venue, "BTCUSDT");
        snapshot.orders = vec![order_view("minted-1", venue, None, OrderStatus::Accepted)];
        cell.set(Arc::new(ArcSwap::from_pointee(snapshot))).expect("set once");

        bind.client.cancel_with_intent("minted-1", vike_exec::CancelIntent::RiskOff);
        assert!(
            TradePeer::wait_until(WAIT, || peer
                .ups()
                .iter()
                .any(|f| matches!(f, TradeUp::Cancel { .. }))),
            "{venue}: the cancel was written"
        );
        peer.restart();
        assert!(TradePeer::wait_until(WAIT, || adapter_starts(&peer) >= 2), "{venue}");
        let cancels_since = |peer: &TradePeer| {
            let ups = peer.ups();
            let second = ups
                .iter()
                .enumerate()
                .filter(|(_, f)| matches!(f, TradeUp::AdapterStart(_)))
                .nth(1)
                .map_or(ups.len(), |(i, _)| i);
            ups[second..].to_vec()
        };
        assert!(
            TradePeer::wait_until(WAIT, || cancels_since(&peer)
                .iter()
                .any(|f| matches!(f, TradeUp::Cancel { .. }))),
            "{venue}: the held risk-off cancel is sent again: {:?}",
            peer.ups()
        );
        let since = cancels_since(&peer);
        let cancel_at =
            since.iter().position(|f| matches!(f, TradeUp::Cancel { .. })).expect("a cancel");
        let taught_at = since.iter().position(|f| {
            matches!(f, TradeUp::AdoptRestored { pairs, .. } if pairs.iter().any(|p| p.0 == "minted-1"))
        });
        assert!(
            taught_at.is_some_and(|t| t < cancel_at),
            "{venue}: the order is taught BEFORE its cancel reaches the adapter: {since:?}"
        );
        assert!(
            failed.lock().unwrap().is_empty(),
            "{venue}: no cancel reached an adapter that had forgotten its order: {:?}",
            failed.lock().unwrap()
        );
        // The model itself is venue-sensitive: a cancel of an order NOBODY taught since the restart fails on
        // the four venues that forget and on no other.
        bind.client.cancel_with_intent("ghost", vike_exec::CancelIntent::RiskOff);
        let forgets = FORGETTING.contains(&venue);
        assert!(
            TradePeer::wait_until(WAIT, || {
                peer.ups()
                    .iter()
                    .any(|f| matches!(f, TradeUp::Cancel { coid, .. } if coid == "ghost"))
            }),
            "{venue}: the ghost cancel reached the peer"
        );
        assert_eq!(
            failed.lock().unwrap().as_slice() == ["ghost".to_string()],
            forgets,
            "{venue}: the peer fails an untaught cancel exactly on a venue whose map the restart empties"
        );
        drop(bind);
    }
}

/// The venues whose bridge map a gatewayhub restart empties: a cancel of an order not taught since fails with
/// "no venue order id" (polymarket's CLOB ids, ig's deal ids, ctrader's `OrderIdMap`, hyperliquid's cloid registry).
const FORGETTING: [&str; 4] = ["polymarket", "ig", "ctrader", "hyperliquid"];

/// The default peer, plus the memory of a bridge: from the SECOND `AdapterStart` on (the restart) the map is
/// empty and only an `AdoptRestored` fills it, and a `Cancel` of a coid the map does not hold is pushed on
/// `failed` when `venue` is one of [`FORGETTING`] (a venue with no such map never fails). The commands are
/// seen by the peer's command observer (they never reach the request script).
fn config_that_forgets_on_restart(venue: &str, failed: Arc<Mutex<Vec<String>>>) -> TradePeerConfig {
    let forgets = FORGETTING.contains(&venue);
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let taught = Arc::new(Mutex::new(BTreeSet::<String>::new()));
    let base = default_script();
    let script: fake_trade_peer::Script = {
        let (starts, taught) = (Arc::clone(&starts), Arc::clone(&taught));
        Arc::new(move |frame| {
            if matches!(frame, TradeUp::AdapterStart(_))
                && starts.fetch_add(1, Ordering::SeqCst) >= 1
            {
                taught.lock().unwrap().clear();
            }
            base(frame)
        })
    };
    config_for(venue, Some(script)).with_command_observer(move |frame| match frame {
        TradeUp::AdoptRestored { pairs, .. } => {
            taught.lock().unwrap().extend(pairs.iter().map(|(coid, _)| coid.clone()));
        }
        TradeUp::Cancel { coid, .. }
            if forgets
                && starts.load(Ordering::SeqCst) >= 2
                && !taught.lock().unwrap().contains(coid) =>
        {
            failed.lock().unwrap().push(coid.clone());
        }
        _ => {}
    })
}

/// A script that answers the FIRST `AdapterStart` as a healthy daemon does and every later one with
/// `later(rid)`: the restarted gatewayhub's answer.
fn script_whose_restart_answers(
    later: impl Fn(u64) -> TradeDown + Send + Sync + 'static,
) -> fake_trade_peer::Script {
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let base = default_script();
    Arc::new(move |frame| match frame {
        TradeUp::AdapterStart(start) if starts.fetch_add(1, Ordering::SeqCst) >= 1 => {
            vec![Step::Send(later(start.rid))]
        }
        other => base(other),
    })
}

fn live_at(tier: AccountTier, rid: u64) -> TradeDown {
    TradeDown::AdapterReply {
        rid,
        reply: Box::new(vike_gatewayhub_client::proto::AdapterReply::Live {
            bound_tier: tier,
            grid: None,
            contract_value: None,
            margin_mode: None,
            leg_grids: Vec::new(),
            recon: true,
            leverage_cap: None,
        }),
    }
}

fn cancels_sent(peer: &TradePeer) -> usize {
    peer.ups().iter().filter(|f| matches!(f, TradeUp::Cancel { .. })).count()
}

#[test]
fn a_restarted_gatewayhub_that_binds_another_tier_ends_the_lease_as_the_boot_path_does() {
    // W13b finding 8: the boot path detaches and refuses the account when the daemon bound a client at
    // a tier the row does not state ("a live order path the operator did not ask for"); the epoch-change
    // path only logged and left the lease open, orders still flowing to a wrongly bound adapter.
    let script = script_whose_restart_answers(|rid| live_at(AccountTier::Live, rid));
    let peer = TradePeer::start(config_for("hyperliquid", Some(script)).never_ack());
    let cell = Arc::new(OnceLock::new());
    let backend =
        connect(&peer, "hyperliquid", BootHints::none()).with_snapshot_cell(Arc::clone(&cell));
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let bind = backend.start_adapter(query("hyperliquid", &label), &events, None).expect("starts");
    let mut snapshot = CoreSnapshot::empty("hyperliquid", "BTCUSDT");
    snapshot.orders = vec![order_view("live-1", "hyperliquid", None, OrderStatus::Accepted)];
    cell.set(Arc::new(ArcSwap::from_pointee(snapshot))).expect("set once");

    peer.restart();
    assert!(
        TradePeer::wait_until(WAIT, || adapter_starts(&peer) >= 2),
        "the adapter is asked again"
    );
    let ended = |peer: &TradePeer| {
        peer.ups().iter().any(|f| {
            matches!(
                f,
                TradeUp::Detach { phase: vike_gatewayhub_client::proto::DetachPhase::Final }
            )
        })
    };
    assert!(
        TradePeer::wait_until(WAIT, || ended(&peer)),
        "the lease is detached: {:?}",
        peer.ups()
    );
    assert!(
        taught_since(&peer, 2).is_empty(),
        "and nothing is taught to the wrongly bound adapter: {:?}",
        peer.ups()
    );
    drop(bind);
}

#[test]
fn a_refusal_asking_again_cannot_change_is_not_retried_and_releases_the_held_cancels() {
    // W13b finding 8: every error of every kind was retried every <= 5 s for as long as the lease was
    // open, while the held risk-off cancels waited on the answer for ever. A daemon that answers the
    // restart's `AdapterStart` with a frame it cannot (a desync) is not going to answer it better.
    let script = script_whose_restart_answers(|rid| TradeDown::IdentityRecorded {
        rid,
        ok: true,
        note: String::new(),
    });
    let peer = TradePeer::start(config_for("hyperliquid", Some(script)).never_ack());
    let backend = connect(&peer, "hyperliquid", BootHints::none());
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let mut bind =
        backend.start_adapter(query("hyperliquid", &label), &events, None).expect("starts");
    bind.client.cancel_with_intent("resting-1", vike_exec::CancelIntent::RiskOff);
    assert!(
        TradePeer::wait_until(WAIT, || cancels_sent(&peer) == 1),
        "written, never acknowledged"
    );

    peer.restart();
    assert!(TradePeer::wait_until(WAIT, || adapter_starts(&peer) >= 2), "asked once more");
    assert!(
        TradePeer::wait_until(WAIT, || cancels_sent(&peer) == 2),
        "the held risk-off cancel is released, not left waiting for an adapter that will not come"
    );
    // The first retry would be 0.5 s after the refusal, the next 1 s after that.
    std::thread::sleep(Duration::from_millis(2_500));
    assert_eq!(adapter_starts(&peer), 2, "a fixed refusal is not asked again: {:?}", peer.ups());
    drop(bind);
}

#[test]
fn a_link_flap_inside_one_epoch_re_teaches_nothing() {
    let peer = TradePeer::start(config_for("ig", None));
    let cell = Arc::new(OnceLock::new());
    let backend = connect(&peer, "ig", BootHints::none()).with_snapshot_cell(Arc::clone(&cell));
    let (events, _lane) = fake_trade_peer::lane();
    let (trigger, pokes) = channel::<()>();
    let label = AccountLabel::Default;
    let _bind = backend.start_adapter(query("ig", &label), &events, Some(trigger)).expect("starts");
    let mut snapshot = CoreSnapshot::empty("ig", "BTCUSDT");
    snapshot.orders = vec![order_view("live-1", "ig", None, OrderStatus::Accepted)];
    cell.set(Arc::new(ArcSwap::from_pointee(snapshot))).expect("set once");

    peer.kill_lease();
    assert!(peer.wait_leases(2, WAIT), "the client redials");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        adapter_starts(&peer),
        1,
        "the gatewayhub never restarted: its adapter is still there"
    );
    assert!(pokes.try_recv().is_err(), "…and no reconcile is forced");
    assert!(taught_since(&peer, 1).is_empty());
}

#[test]
fn a_ring_the_gatewayhub_could_not_replay_pokes_the_manager_of_a_venue_that_takes_no_trigger() {
    // W13b finding 7: alpaca, aster, ctrader, deribit, dukascopy, fxcm and ig declare
    // `takes_recon_trigger: false` (config_for's fact below), which is about the BRIDGE's own resync
    // supervisor. The link's gap paths used to be wired only for the others, so a stale cursor, a ring
    // overflow or a lost frame waited for the next interval pass (never, with interval 0).
    let peer = TradePeer::start(config_for("alpaca", None));
    let backend = connect(&peer, "alpaca", BootHints::none());
    let (events, _lane) = fake_trade_peer::lane();
    let (trigger, pokes) = channel::<()>();
    let label = AccountLabel::Default;
    let _bind =
        backend.start_adapter(query("alpaca", &label), &events, Some(trigger)).expect("starts");
    assert!(pokes.try_recv().is_err(), "nothing yet: a quiet start asks for no pass");

    assert!(peer.send(&TradeDown::ReplayUnavailable { floor: 7 }));
    assert!(pokes.recv_timeout(WAIT).is_ok(), "ReplayUnavailable reaches the manager");
}

#[test]
fn the_resting_orders_of_an_account_are_read_off_the_snapshot_by_venue_label_and_liveness() {
    use vike_tradehub::remote_backend::reteach::resting_orders;
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    snapshot.orders = vec![
        order_view("a", "binance", None, OrderStatus::Accepted),
        order_view("b", "binance", Some("ALT"), OrderStatus::Accepted),
        order_view("c", "binance", None, OrderStatus::Canceled),
        order_view("d", "bybit", None, OrderStatus::Accepted),
    ];
    let default: HashSet<String> =
        resting_orders(Some(&snapshot), "binance", None).into_iter().map(|(c, _)| c).collect();
    assert_eq!(default, HashSet::from(["a".to_string()]));
    let alt: HashSet<String> = resting_orders(Some(&snapshot), "binance", Some("ALT"))
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    assert_eq!(alt, HashSet::from(["b".to_string()]));
    assert!(resting_orders(None, "binance", None).is_empty(), "before the core is up: none");
}

/// The default peer, except that the SECOND `AdapterStart` (the restart's) is answered `later` after `delay`.
fn script_whose_restart_answers_after(
    delay: Duration,
    later: impl Fn(u64) -> TradeDown + Send + Sync + 'static,
) -> fake_trade_peer::Script {
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let base = default_script();
    Arc::new(move |frame| match frame {
        TradeUp::AdapterStart(start) if starts.fetch_add(1, Ordering::SeqCst) >= 1 => {
            vec![Step::Sleep(delay), Step::Send(later(start.rid))]
        }
        other => base(other),
    })
}

fn cancels_of(peer: &TradePeer, coid: &str) -> usize {
    peer.ups().iter().filter(|f| matches!(f, TradeUp::Cancel { coid: c, .. } if c == coid)).count()
}

#[test]
fn a_stop_that_begins_after_the_restarted_adapter_answered_still_sends_the_held_risk_off_cancel() {
    // W13c finding 1: the shutdown sweep's risk-off cancel is held across a gatewayhub restart for the
    // re-teaching worker's teaching. A stop that began while the worker waited for the adapter's answer made
    // the worker return at `gone()` AFTER the answer without releasing: `close_for_good` then turned the
    // cancel into an advisory while the adapter was up and the order kept resting.
    //
    // The restarted daemon answers its `AdapterStart` 1.2 s late. In between: a routine cancel is written
    // (nothing acknowledges it, so the lease's ack drain waits for it, which keeps the link thread handling
    // messages) and the stop begins. The answer then lands inside the drain.
    let script = script_whose_restart_answers_after(Duration::from_millis(1_200), |rid| {
        live_at(AccountTier::Demo, rid)
    });
    let peer = TradePeer::start(config_for("hyperliquid", Some(script)).never_ack());
    let cell = Arc::new(OnceLock::new());
    let backend =
        connect(&peer, "hyperliquid", BootHints::none()).with_snapshot_cell(Arc::clone(&cell));
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let mut bind =
        backend.start_adapter(query("hyperliquid", &label), &events, None).expect("starts");
    let mut snapshot = CoreSnapshot::empty("hyperliquid", "BTCUSDT");
    snapshot.orders = vec![order_view("resting-1", "hyperliquid", None, OrderStatus::Accepted)];
    cell.set(Arc::new(ArcSwap::from_pointee(snapshot))).expect("set once");
    bind.client.cancel_with_intent("resting-1", vike_exec::CancelIntent::RiskOff);
    assert!(TradePeer::wait_until(WAIT, || cancels_of(&peer, "resting-1") == 1), "written");

    peer.restart();
    assert!(TradePeer::wait_until(WAIT, || adapter_starts(&peer) >= 2), "asked again");
    // The peer is inside its late answer and reads nothing meanwhile: the routine cancel is Written (the
    // socket takes it) and stays unacknowledged, which is what the ack drain waits for.
    bind.client.cancel_with_intent("routine-1", vike_exec::CancelIntent::Routine);
    bind.client.begin_detach();

    assert!(
        TradePeer::wait_until(WAIT, || cancels_of(&peer, "resting-1") == 2),
        "the held risk-off cancel reaches the adapter that answered, ahead of the final Detach: {:?}",
        peer.ups()
    );
    let is_final = |f: &TradeUp| {
        matches!(f, TradeUp::Detach { phase: vike_gatewayhub_client::proto::DetachPhase::Final })
    };
    // The final Detach follows the ack drain (up to 2 s).
    assert!(TradePeer::wait_until(WAIT, || peer.ups().iter().any(is_final)), "the lease ends");
    let ups = peer.ups();
    let sent_at = ups
        .iter()
        .rposition(|f| matches!(f, TradeUp::Cancel { coid, .. } if coid == "resting-1"))
        .expect("sent");
    let detach_at = ups.iter().position(is_final).expect("the final Detach follows");
    assert!(sent_at < detach_at, "ahead of the final Detach: {ups:?}");
    drop(bind);
}

#[test]
fn a_restart_the_daemon_answers_not_running_releases_the_held_risk_off_cancel() {
    // W13c finding 2, the `NotRunning` branch: the worker releases the held cancels (plain `release_held`)
    // and the daemon answers each with an advisory. They are SENT, not left held until the lease closes.
    let script = script_whose_restart_answers(|rid| TradeDown::AdapterReply {
        rid,
        reply: Box::new(vike_gatewayhub_client::proto::AdapterReply::NotRunning {
            block_word: ArmingBlock::None.word().to_string(),
            refusal_text: "the account no longer arms".to_string(),
        }),
    });
    let peer = TradePeer::start(config_for("hyperliquid", Some(script)).never_ack());
    let backend = connect(&peer, "hyperliquid", BootHints::none());
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let mut bind =
        backend.start_adapter(query("hyperliquid", &label), &events, None).expect("starts");
    bind.client.cancel_with_intent("resting-1", vike_exec::CancelIntent::RiskOff);
    assert!(TradePeer::wait_until(WAIT, || cancels_of(&peer, "resting-1") == 1), "written");

    peer.restart();
    assert!(TradePeer::wait_until(WAIT, || adapter_starts(&peer) >= 2), "asked again");
    assert!(
        TradePeer::wait_until(WAIT, || cancels_of(&peer, "resting-1") == 2),
        "the held risk-off cancel is released after the NotRunning answer: {:?}",
        peer.ups()
    );
    drop(bind);
}

// ---- the admitted symbols the daemon's reconcile client was told ------------------------------------

/// The default peer, except that `SymbolFacts` is granted for any `*.P` symbol while `refuse` is false
/// and refused otherwise: the gatewayhub's public instrument list, as far as the node can tell.
fn config_granting_perp_facts(refuse: Arc<AtomicBool>) -> TradePeerConfig {
    config_granting_perp_facts_counted(refuse, Arc::new(AtomicUsize::new(0)))
}

/// [`config_granting_perp_facts`] that also counts the commits whose verdict the script has DECIDED.
/// The peer records a request (`facts_asked`) BEFORE it runs the script, so a test that flips `refuse`
/// the moment it sees the request can flip it before the script reads it and be answered the other way;
/// `decided` is bumped AFTER the flag was read, which is what such a test must wait for.
fn config_granting_perp_facts_counted(
    refuse: Arc<AtomicBool>,
    decided: Arc<AtomicUsize>,
) -> TradePeerConfig {
    let base = default_script();
    let script: fake_trade_peer::Script = Arc::new(move |frame| match frame {
        TradeUp::SymbolFacts { rid, symbol, probe, .. } => {
            let granted = !refuse.load(Ordering::SeqCst) && symbol.ends_with(".P");
            if !*probe {
                decided.fetch_add(1, Ordering::SeqCst);
            }
            vec![Step::Send(TradeDown::SymbolFactsReply {
                rid: *rid,
                facts: if granted {
                    Ok(vike_gatewayhub_client::proto::WireSymbolFacts {
                        properties: vike_model::SymbolProperties {
                            tick_size: 0.01,
                            step_size: 1.0,
                            ..Default::default()
                        },
                        contract_value: None,
                    })
                } else {
                    Err(format!("binance: no instrument {symbol}"))
                },
            })]
        }
        other => base(other),
    });
    config_for("binance", Some(script))
}

fn facts_asked(peer: &TradePeer) -> usize {
    peer.ups().iter().filter(|f| matches!(f, TradeUp::SymbolFacts { probe: false, .. })).count()
}

fn told(bind: &AdapterBind) -> Option<Vec<String>> {
    bind.recon.as_ref().expect("a reconcile handle").admitted_symbols()
}

/// `told` once it reads `want`, or after `WAIT`. A facts request is SEEN by the peer before its reply
/// is read by the link (which is when the symbol is recorded as told), and the link's own reconnect
/// poke can end `poke_after_facts` inside that window; the symbol is owed at the poke, not told.
fn told_settled(bind: &AdapterBind, want: &Option<Vec<String>>) -> Option<Vec<String>> {
    TradePeer::wait_until(WAIT, || told(bind) == *want);
    told(bind)
}

const SOL: &str = "SOLUSDT.P";

/// **After a gatewayhub restart the NEW daemon is told every symbol the account admitted, before the
/// reconcile pass that follows the restart.** The node's one-hour facts cache would answer the next
/// admission without asking, so nothing but the re-teaching worker ever reaches the new daemon: its
/// reconcile client reads the mounted symbol until it does, and a restored order on an admitted
/// symbol would be read as absent.
#[test]
fn a_gatewayhub_restart_tells_the_new_daemon_every_admitted_symbol_before_the_reconcile_pass() {
    let peer = TradePeer::start(config_granting_perp_facts(Arc::new(AtomicBool::new(false))));
    let backend = connect(&peer, "binance", BootHints::none());
    let (events, _lane) = fake_trade_peer::lane();
    let (trigger, pokes) = channel::<()>();
    let label = AccountLabel::Default;
    let bind =
        backend.start_adapter(query("binance", &label), &events, Some(trigger)).expect("starts");
    let facts = backend.symbol_facts();
    for symbol in [SOL, "ETHUSDT.P"] {
        facts.admission("binance", "binance", symbol).expect("granted");
        facts.admitted("binance", "binance", symbol);
    }
    assert_eq!(told(&bind), Some(vec![SOL.to_string(), "ETHUSDT.P".to_string()]));
    assert_eq!(facts_asked(&peer), 2);

    peer.restart();

    assert!(
        poke_after_facts(&pokes, &peer, 4),
        "the reconcile pass after the restart is requested once both symbols were asked of the NEW \
         daemon: {:?}",
        peer.ups()
    );
    assert_eq!(facts_asked(&peer), 4, "one request per symbol, no more");
    let ups = peer.ups();
    let second_start =
        ups.iter().enumerate().filter(|(_, f)| matches!(f, TradeUp::AdapterStart(_))).nth(1);
    let first_retold = ups
        .iter()
        .enumerate()
        .filter(|(_, f)| matches!(f, TradeUp::SymbolFacts { probe: false, .. }))
        .nth(2)
        .map(|(i, _)| i);
    assert!(
        second_start.zip(first_retold).is_some_and(|((s, _), r)| s < r),
        "after the adapter was started again: {ups:?}"
    );
    let want = Some(vec![SOL.to_string(), "ETHUSDT.P".to_string()]);
    assert_eq!(told_settled(&bind, &want), want);
}

/// The re-teaching is bounded: at most `MAX_EXTRA_SYMBOLS` symbols, one request each.
#[test]
fn the_retelling_is_bounded_by_the_extra_symbol_cap() {
    let peer = TradePeer::start(config_granting_perp_facts(Arc::new(AtomicBool::new(false))));
    let backend = connect(&peer, "binance", BootHints::none());
    let (events, _lane) = fake_trade_peer::lane();
    let (trigger, pokes) = channel::<()>();
    let label = AccountLabel::Default;
    let bind =
        backend.start_adapter(query("binance", &label), &events, Some(trigger)).expect("starts");
    let facts = backend.symbol_facts();
    for i in 0..vike_exec::MAX_EXTRA_SYMBOLS + 3 {
        let symbol = format!("S{i}USDT.P");
        let _ = facts.admission("binance", "binance", &symbol);
        facts.admitted("binance", "binance", &symbol);
    }
    let cap = vike_exec::MAX_EXTRA_SYMBOLS;
    assert_eq!(told(&bind).map(|l| l.len()), Some(cap));
    let before = facts_asked(&peer);

    peer.restart();
    assert!(poke_after_facts(&pokes, &peer, before + cap), "{:?}", peer.ups());
    assert_eq!(facts_asked(&peer) - before, cap, "one request per claimed symbol, no more");
    TradePeer::wait_until(WAIT, || told(&bind).map(|l| l.len()) == Some(cap));
    assert_eq!(told(&bind).map(|l| l.len()), Some(cap));
}

/// A re-teaching the daemon refuses must not be hidden by the facts cache: the reconcile pass runs
/// without the symbol (its orders are HELD by the core, not retired), and the next admission of the
/// symbol asks the daemon again, even though the node holds a fresh cached answer.
#[test]
fn a_symbol_the_new_daemon_refused_is_asked_again_by_the_next_admission_not_served_from_the_cache()
{
    let refuse = Arc::new(AtomicBool::new(false));
    let decided = Arc::new(AtomicUsize::new(0));
    let peer = TradePeer::start(config_granting_perp_facts_counted(
        Arc::clone(&refuse),
        Arc::clone(&decided),
    ));
    let backend = connect(&peer, "binance", BootHints::none());
    let (events, _lane) = fake_trade_peer::lane();
    let (trigger, pokes) = channel::<()>();
    let label = AccountLabel::Default;
    let bind =
        backend.start_adapter(query("binance", &label), &events, Some(trigger)).expect("starts");
    let facts = backend.symbol_facts();
    facts.admission("binance", "binance", SOL).expect("granted");
    facts.admitted("binance", "binance", SOL);

    refuse.store(true, Ordering::SeqCst);
    peer.restart();
    assert!(pokes.recv_timeout(WAIT).is_ok(), "the pass is requested whatever the retelling did");
    // `decided`, not `facts_asked`: the peer records a request BEFORE its script reads `refuse`, so
    // flipping the flag on the record alone could turn the refusal into a grant.
    assert!(
        TradePeer::wait_until(WAIT, || decided.load(Ordering::SeqCst) >= 2),
        "the daemon was asked (and refused) before the cache is tested: {:?}",
        peer.ups()
    );
    assert_eq!(told(&bind), Some(Vec::new()), "the daemon refused: nothing is claimed");

    refuse.store(false, Ordering::SeqCst);
    let before = facts_asked(&peer);
    facts.admission("binance", "binance", SOL).expect("granted now");
    facts.admitted("binance", "binance", SOL);
    assert!(facts_asked(&peer) > before, "the cached answer did not stand in for the daemon's");
    assert_eq!(told(&bind), Some(vec![SOL.to_string()]));
}

/// Wait for a reconcile poke that arrives once `expected` facts requests have been asked. The link's
/// own reconnect fires a poke too (it can come first, before the re-teaching), so the poke that
/// matters is the one the re-teaching worker sends AFTER telling the daemon.
fn poke_after_facts(
    pokes: &std::sync::mpsc::Receiver<()>,
    peer: &TradePeer,
    expected: usize,
) -> bool {
    while pokes.recv_timeout(WAIT).is_ok() {
        if facts_asked(peer) >= expected {
            return true;
        }
    }
    false
}

/// **A look is never claimed as read.** A Preview (or a refused order) asks the node for a symbol's
/// facts without admitting it; the daemon's reconcile client is not told, so the claim the next pass
/// carries must not list it (the core would read the venue's silence about an order on it as gone),
/// and a gatewayhub restart does not owe it to the new daemon.
#[test]
fn a_previewed_symbol_is_not_claimed_as_read_before_or_after_a_restart() {
    let peer = TradePeer::start(config_granting_perp_facts(Arc::new(AtomicBool::new(false))));
    let backend = connect(&peer, "binance", BootHints::none());
    let (events, _lane) = fake_trade_peer::lane();
    let (trigger, pokes) = channel::<()>();
    let label = AccountLabel::Default;
    let bind =
        backend.start_adapter(query("binance", &label), &events, Some(trigger)).expect("starts");
    let facts = backend.symbol_facts();
    facts.admission("binance", "binance", "ETHUSDT.P").expect("a look is answered");
    facts.admission("binance", "binance", SOL).expect("granted");
    facts.admitted("binance", "binance", SOL);
    assert_eq!(told(&bind), Some(vec![SOL.to_string()]), "the look took no slot");

    peer.restart();

    assert!(poke_after_facts(&pokes, &peer, 2), "{:?}", peer.ups());
    let want = Some(vec![SOL.to_string()]);
    assert_eq!(told_settled(&bind, &want), want, "only the committed symbol is retold");
    assert_eq!(facts_asked(&peer), 2, "the look was not committed after the restart either");
}
