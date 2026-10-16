//! `node`'s unit tests: account refusal by name, engine rank order, and which legs reach a mount.
//!
//! The keep half of the crate is bridge-free (wave 2, W9a), so the arming table these tests judge a
//! mount against comes from the backend that dials nothing, `PaperBackend`: every `demo`/`live`
//! account is NOT RUNNING on it, and a `paper` account runs the simulator — which is all the refusal
//! reads. The same refusal over a table with armed rows is the venue half's
//! (`crates/vike-gatewayhub/src/trade/adapters/contract_tests/arming.rs`).

/// The arming snapshot a config's backend answers.
fn table_of(cfg: &super::RuntimeConfig) -> crate::ArmingTable {
    cfg.backend.arming_table().expect("the paper backend answers")
}

/// **THE LOUD REFUSAL fires, and it fires on the arming table the fan-out selects with.**
///
/// [`super::refuse_unarmed_mount_accounts`] is this crate's one refusal that does not degrade. A
/// mutation replacing its body with `Ok(())` (`account = "ALT"` accepted, no engine for it,
/// `vike_core` resolving the mount onto the DEFAULT account) was measured GREEN across
/// `vike-run` and `vike-tradehub`: cited in four doc comments, covered by nothing.
///
/// Three cases, each a different wrong answer:
///
/// * an account NO account row names is refused, naming venue, account, symbol and the commands
///   that would arm it (a refusal an operator cannot act on is a crash);
/// * an account named but resolving PAPER (no `__ALT` credentials) is refused too — the case
///   that matters most: `venue_account_arming` returns a row for it, so an "is there a row" check
///   would pass it to the default engine;
/// * an account-LESS mount (every shipped mount) names the venue's DEFAULT account, which since
///   decision 0156 runs only at its row's tier: with no row it is refused like any other, never
///   handed a paper engine in its place; a `paper` row runs the simulator and passes.
#[test]
fn a_mount_naming_an_unarmed_account_is_refused_by_name() {
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let cfg = |vars: std::collections::HashMap<String, String>, policy: crate::AdapterPolicy| {
        super::RuntimeConfig {
            backend: std::sync::Arc::new(crate::PaperBackend::new(&policy.accounts)),
            vars,
            properties_rec: None,
            seed_cash: 10_000.0,
            recon_enabled: false,
            core_config: vike_core::CoreConfig::default(),
            risk_profile: None,
            policy,
        }
    };
    let mount = |account: Option<vike_model::accounts::account_keys::AccountLabel>| {
        vec![super::RuntimeAccount {
            venue: "hyperliquid".to_string(),
            symbol: "BTC".to_string(),
            account,
        }]
    };

    // (1) an account no account row names.
    let bare = cfg(std::collections::HashMap::new(), crate::AdapterPolicy::default());
    let err =
        super::refuse_unarmed_mount_accounts(&bare, &table_of(&bare), &mount(Some(alt.clone())))
            .expect_err("an unarmed account must REFUSE, never fall through to the default one");
    let said = err.to_string();
    for needle in ["hyperliquid", "ALT", "BTC", "vike-cli secrets account activate"] {
        assert!(said.contains(needle), "the refusal must name `{needle}`: {said}");
    }

    // (2) …and one an ACTIVE row DOES name, resolved paper (no credentials): the check is on ARMED.
    let default = vike_model::accounts::account_keys::AccountLabel::Default;
    let named = cfg(
        std::collections::HashMap::new(),
        crate::AdapterPolicy::default()
            .with_account("hyperliquid", &default, vike_model::AccountTier::Demo)
            .with_account("hyperliquid", &alt, vike_model::AccountTier::Demo),
    );
    assert!(
        super::refuse_unarmed_mount_accounts(&named, &table_of(&named), &mount(Some(alt.clone())))
            .is_err(),
        "an account row arms nothing on its own — with no `__ALT` credentials the account \
             resolves PAPER, has no engine, and the mount must still be refused"
    );

    // (3) an account-LESS mount names the DEFAULT account: no row, so it does not run, and the
    // mount is refused naming why — never resolved onto a paper engine standing in for it.
    let err = super::refuse_unarmed_mount_accounts(&bare, &table_of(&bare), &mount(None))
        .expect_err("a default account no row names does not run");
    let said = err.to_string();
    for needle in ["hyperliquid", "BTC", vike_config::ArmingBlock::NoAccountRow.as_str()] {
        assert!(said.contains(needle), "the refusal must name `{needle}`: {said}");
    }
    // …and with a `paper` row it runs the simulator, so the mount passes.
    let paper_default = cfg(
        std::collections::HashMap::new(),
        crate::AdapterPolicy::default().with_account(
            "hyperliquid",
            &default,
            vike_model::AccountTier::Paper,
        ),
    );
    assert!(
        super::refuse_unarmed_mount_accounts(
            &paper_default,
            &table_of(&paper_default),
            &mount(None)
        )
        .is_ok(),
        "a `paper` default account runs, and its mount resolves onto its engine"
    );
}

/// [`super::in_engine_order`]'s rule: default engines held by ascending `engine_rank`. A tie
/// keeps MOUNT order (stable sort), though a debug `assemble_runtime` panics on one first (its
/// `debug_assert!`; the table's ranks are pinned distinct in
/// `crates/vike-venues/src/venue_wiring_tests.rs`'s
/// `the_core_holds_the_default_engines_in_rank_order`).
#[test]
fn the_default_engines_are_held_in_rank_order() {
    let row = |venue, engine_rank| super::WiredMarket { venue, symbol: "S", engine_rank };
    let mounted =
        vec![(row("a", 2), 'a'), (row("b", 0), 'b'), (row("c", 1), 'c'), (row("d", 1), 'd')];
    let got: Vec<char> = super::in_engine_order(mounted).into_iter().map(|(_, t)| t).collect();
    assert_eq!(got, ['b', 'c', 'd', 'a']);
}

/// [`ArmingTable::wired_markets`] keeps the wiring table's engine rows whose venue the table says
/// has an adapter, in table order, and drops a venue whose adapter is absent and a venue the table
/// does not name; the registry-less [`super::wired_markets`] is the whole wiring table.
#[test]
fn the_table_wires_the_markets_whose_adapter_it_carries() {
    let present = crate::VenueFacts { adapter_present: true, ..crate::VenueFacts::absent() };
    let facts = std::collections::BTreeMap::from([
        ("hyperliquid".to_string(), present),
        ("ibkr".to_string(), crate::VenueFacts::absent()),
    ]);
    let table = crate::ArmingTable::new(facts, Vec::new());
    let got: Vec<&str> = table.wired_markets().iter().map(|m| m.venue).collect();
    assert_eq!(got, ["hyperliquid"]);
    assert!(
        crate::ArmingTable::default().wired_markets().is_empty(),
        "no table row, nothing mounted"
    );
    assert!(
        super::wired_markets().iter().any(|m| m.venue == "hyperliquid"),
        "the registry-less list is the whole wiring table"
    );
}

// ---- the declared-leg derivation feeding `crate::make_engine_with_legs` ----

use vike_core::MountLeg;

/// THE BYTE-IDENTICAL PROPERTY at the derivation end: a leg-free mount (every mount today)
/// contributes nothing to ANY venue's engine, so `make_engine_with_legs` gets an empty list.
///
/// ⚠ NON-VACUOUS only because of the SECOND mount: alone, it holds under any implementation,
/// even one ignoring `venue` and `wired_symbol`. Beside a mount that DOES declare legs, a leak
/// onto a leg-free venue fails the first assertion; the second pins it is not merely misfiled.
#[test]
fn a_leg_free_mount_gets_nothing_even_beside_a_mount_that_declares_legs() {
    let none: Vec<MountLeg> = Vec::new();
    let some = vec![MountLeg::same_venue("ETHUSDT")];
    let mounts = || [("binance", none.as_slice()), ("bybit", some.as_slice())].into_iter();
    assert!(
        super::legs_for_venue(mounts(), "binance", "BTCUSDT").is_empty(),
        "binance declares no legs, so bybit's must not reach it"
    );
    assert_eq!(
        super::legs_for_venue(mounts(), "bybit", "SOLUSDT"),
        vec!["ETHUSDT".to_string()],
        "...while the mount that DOES declare one still gets it"
    );
}

/// A SAME-VENUE leg (`MountLeg::same_venue`) belongs to the MOUNT's venue only.
///
/// NON-VACUOUS: the OTHER venue is asserted empty, so handing every leg to every engine
/// (silently gridding a bybit engine with a binance symbol's tick) fails the second assertion.
#[test]
fn a_same_venue_leg_reaches_only_its_own_mounts_venue() {
    let legs = vec![MountLeg::same_venue("ETHUSDT")];
    let mounts = || [("binance", legs.as_slice())].into_iter();
    assert_eq!(super::legs_for_venue(mounts(), "binance", "BTCUSDT"), vec!["ETHUSDT".to_string()]);
    assert!(super::legs_for_venue(mounts(), "bybit", "BTCUSDT").is_empty());
}

/// A CROSS-VENUE leg (`MountLeg::at`, the xEMM hedge) is gridded on the venue it is SENT to —
/// the resolution `resolve_intent_venue` applies when routing its orders.
///
/// NON-VACUOUS: the mount's own venue is asserted EMPTY, so using `mount.venue` unconditionally
/// fails (the dangerous direction: gridding the maker for a symbol only the taker sees).
#[test]
fn a_cross_venue_leg_is_gridded_on_the_venue_it_routes_to() {
    let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
    let mounts = || [("hyperliquid", legs.as_slice())].into_iter();
    assert_eq!(
        super::legs_for_venue(mounts(), "okx", "ETH-USDT-SWAP"),
        vec!["BTC-USDT-SWAP".to_string()]
    );
    assert!(super::legs_for_venue(mounts(), "hyperliquid", "BTC").is_empty());
}

/// The venue's OWN wired symbol never becomes a leg (the engine's scalars are its grid; a second
/// row could only drift). The live xEMM shape: `XemmConfig::validate` refuses a hedge
/// symbol the taker is not wired for, so its one leg lands here and is dropped.
#[test]
fn the_venues_own_wired_symbol_is_never_a_leg() {
    let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
    let mounts = [("hyperliquid", legs.as_slice())];
    assert!(super::legs_for_venue(mounts.into_iter(), "okx", "BTC-USDT-SWAP").is_empty());
}

/// The SAME symbol from two mounts on one venue is ONE entry (a duplicate grid lookup can be a
/// wasted round trip), in declaration order (`grid_by_symbol` is an `IndexMap` whose order
/// reaches the serialized limits); a blank symbol is none.
#[test]
fn repeats_collapse_and_blanks_are_dropped_in_declaration_order() {
    let a = vec![MountLeg::same_venue("ETHUSDT"), MountLeg::same_venue("   ")];
    let b = vec![MountLeg::same_venue("SOLUSDT"), MountLeg::same_venue("ETHUSDT")];
    let mounts = [("binance", a.as_slice()), ("binance", b.as_slice())];
    assert_eq!(
        super::legs_for_venue(mounts.into_iter(), "binance", "BTCUSDT"),
        vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]
    );
}

// ── the `live-event-forward` relay (journal v18) ──────────────────────────────────────────────

mod forwarder {
    use super::super::build::forward_ingest;
    use vike_exec::{
        Command, FlowUpdate, GatewayCursor, GatewayEvent, Ingest, StreamStatusUpdate, TickSender,
        event_channel,
    };
    use vike_model::FeedStatus;
    use vike_model::events::{Event, OrderSubmitted};

    fn submitted(coid: &str) -> Event {
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 7 })
    }

    /// The core's ingest as the relay sees it: one queue, an event door and a tick door onto it.
    fn core() -> (vike_exec::EventSender, TickSender, tokio::sync::mpsc::Receiver<Ingest>) {
        let (events, rx) = event_channel(8);
        let ticks = TickSender { ingest: events.ingest.clone() };
        (events, ticks, rx)
    }

    /// A plain venue event reaches the core as a plain event: the relay's old, only behaviour.
    #[test]
    fn a_plain_event_reaches_the_core_as_an_event() {
        let (events, ticks, mut rx) = core();
        forward_ingest(Ingest::Event(submitted("c1")), &events, &ticks).unwrap();
        assert!(matches!(rx.try_recv(), Ok(Ingest::Event(Event::OrderSubmitted(_)))));
    }

    /// **The silent drop this relay used to be.** An event that came over the trade link carries
    /// its ring cursor, and the core journals the pair as ONE record. A relay that unwrapped only
    /// `Ingest::Event` (the `if let` it was) would deliver nothing here, and the cursor would die
    /// between the remote client and the journal. The planted-variant test: this fails when the
    /// arm is a `_ =>`.
    #[test]
    fn a_gateway_event_reaches_the_core_with_its_cursor() {
        let (events, ticks, mut rx) = core();
        let cursor = GatewayCursor { gw_epoch: 77, seq: 12, ts_ms: 99 };
        let gw = GatewayEvent { event: submitted("c2"), route_key: "binance#alt".into(), cursor };
        forward_ingest(Ingest::GatewayEvent(Box::new(gw)), &events, &ticks).unwrap();
        match rx.try_recv() {
            Ok(Ingest::GatewayEvent(g)) => {
                assert_eq!(g.cursor, cursor, "the cursor is not dropped or rewritten");
                assert_eq!(g.route_key.as_str(), "binance#alt", "and the ring it belongs to");
                assert!(matches!(g.event, Event::OrderSubmitted(_)));
            }
            other => panic!("the cursor never reached the core: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "one record, not an event and a cursor");
    }

    /// The link's own disclosure reaches the core as a status (the dead-man's and the strategies'
    /// input), not as an event and not nowhere.
    #[test]
    fn a_link_status_reaches_the_core_as_a_status() {
        let (events, ticks, mut rx) = core();
        let status = StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "gateway-trade".into(),
            status: FeedStatus::Disconnected,
        };
        forward_ingest(Ingest::StreamStatus(Box::new(status)), &events, &ticks).unwrap();
        match rx.try_recv() {
            Ok(Ingest::StreamStatus(s)) => {
                assert_eq!((s.venue.as_str(), s.status), ("binance", FeedStatus::Disconnected));
            }
            other => panic!("the link status never reached the core: {other:?}"),
        }
    }

    /// A trade-link transition reaches the core on its own variant: the stuck-order watchdog's
    /// input. Dropped here (`_ => Ok(())`), the watchdog would reject orders of an account whose
    /// link is merely down. The planted-variant test: this fails when the arm is a `_ =>`.
    #[test]
    fn an_exec_link_transition_reaches_the_core() {
        let (events, ticks, mut rx) = core();
        let update = vike_exec::ExecLinkUpdate {
            route_key: "binance#alt".into(),
            venue: "binance".into(),
            up: false,
            at_ts_ms: 31,
        };
        forward_ingest(Ingest::ExecLink(Box::new(update.clone())), &events, &ticks).unwrap();
        match rx.try_recv() {
            Ok(Ingest::ExecLink(u)) => assert_eq!(*u, update, "the transition arrives unchanged"),
            other => panic!("the link transition never reached the core: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    /// Traffic of other lanes was never relayed and still is not; the relay does not stop for it.
    #[test]
    fn other_lanes_traffic_is_dropped_as_before() {
        let (events, ticks, mut rx) = core();
        for ing in [
            Ingest::Watchdog,
            Ingest::Market,
            Ingest::Command(Command::Shutdown),
            Ingest::Flow(Box::new(FlowUpdate {
                venue: "polymarket".into(),
                symbol: "TOK".into(),
                flow: vike_model::FlowToxicity { bid: 0.1, ask: 0.2, ts: 1 },
            })),
        ] {
            forward_ingest(ing, &events, &ticks).unwrap();
        }
        assert!(rx.try_recv().is_err(), "nothing of another lane reaches the core's ingest");
    }

    /// **W13b finding 2: the boot-time replay must not stall the boot.** A lease resumed from the
    /// journal's cursor is replayed a whole ring (20 000 events) while the boot thread is still
    /// assembling, and the boot thread waits for reply frames that same lease reader completes. The
    /// stand-in below is that reader: it delivers the replay into the live lane (4096 slots, as in
    /// production), THEN completes the boot thread's request. Nothing may be waiting for the core
    /// before it exists: the request completes with no core, and when the core appears every event
    /// arrives exactly once, in order.
    #[test]
    fn a_replay_larger_than_the_lane_is_drained_before_the_core_exists_and_relayed_once_in_order() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;
        const REPLAY: u64 = 20_000;

        let (live, live_rx) = event_channel(4096);
        let (door, door_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let relay = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                super::super::build::run_live_forwarder(live_rx, &door_rx, &stop);
            })
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for seq in 1..=REPLAY {
                let cursor = GatewayCursor { gw_epoch: 9, seq, ts_ms: 0 };
                let gw = GatewayEvent {
                    event: submitted("replayed"),
                    route_key: "binance".into(),
                    cursor,
                };
                live.blocking_send_gateway(gw).expect("the lane stays open");
            }
            // The reply the boot thread is waiting for sits behind the replay in the stream.
            reply_tx.send(()).expect("the boot thread waits");
        });
        reply_rx.recv_timeout(Duration::from_secs(20)).expect(
            "the reader blocked on an undrained lane: the boot thread would never get its reply",
        );

        // The core appears (a small ingest, so the relay also has to wait for the core's drain).
        let (core_events, mut core_rx) = event_channel(64);
        let core_ticks = TickSender { ingest: core_events.ingest.clone() };
        door.send((core_events, core_ticks)).expect("the relay listens");
        for want in 1..=REPLAY {
            match core_rx.blocking_recv() {
                Some(Ingest::GatewayEvent(g)) => {
                    assert_eq!(g.cursor.seq, want, "in order, none lost, none twice");
                }
                other => panic!("event {want} never reached the core: {other:?}"),
            }
        }
        reader.join().expect("the reader ends");
        relay.join().expect("the relay ends when the lane closes");
        assert!(core_rx.try_recv().is_err(), "and nothing more: no event is folded twice");
    }

    /// **W13c: at the cap the forwarder sheds, it does not stall the reply path.** Before: a backlog at the
    /// bound stopped draining, the 4096-slot lane filled, the lease reader (which is also the thread that
    /// completes the boot thread's `AdapterReply` / `WireIdsReply`) parked inside its delivery, and the boot
    /// thread timed out into a restart loop. Now the lane stays drained, the overflow is dropped loudly, an
    /// account that lost an event relays everything after it WITHOUT its cursor (the journal's resume cursor
    /// never jumps the gap), another account keeps its cursors, and a link's status is never shed.
    #[test]
    fn a_backlog_at_its_cap_sheds_the_overflow_keeps_the_reply_path_open_and_unpins_the_gapped_account()
     {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;
        const CAP: usize = 1_000;
        const SENT: u64 = 5_000;

        let (live, live_rx) = event_channel(64);
        let (door, door_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let relay = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                super::super::build::relay_with_cap(live_rx, &door_rx, &stop, CAP);
            })
        };
        let gateway = |route: &str, seq: u64| GatewayEvent {
            event: submitted("replayed"),
            route_key: route.into(),
            cursor: GatewayCursor { gw_epoch: 9, seq, ts_ms: 0 },
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let reader = {
            let live = live.clone();
            std::thread::spawn(move || {
                for seq in 1..=SENT {
                    live.blocking_send_gateway(gateway("binance", seq))
                        .expect("the lane stays open");
                }
                // A link status past the cap: tiny, and the dead-man needs it.
                live.exec_link(vike_exec::ExecLinkUpdate {
                    route_key: "binance".into(),
                    venue: "binance".into(),
                    up: false,
                    at_ts_ms: 5,
                })
                .expect("the lane stays open");
                // The reply the boot thread waits for sits behind the whole replay.
                reply_tx.send(()).expect("the boot thread waits");
            })
        };
        reply_rx.recv_timeout(Duration::from_secs(20)).expect(
            "the reader parked behind a full backlog: the boot thread would never get its reply",
        );
        reader.join().expect("the reader ends");
        // Everything the reader sent has been taken off the lane (the last of it may still be in flight).
        let drained = std::time::Instant::now() + Duration::from_secs(20);
        while live.ingest.capacity() < live.ingest.max_capacity() {
            assert!(std::time::Instant::now() < drained, "the forwarder keeps the lane drained");
            std::thread::sleep(Duration::from_millis(2));
        }

        let (core_events, mut core_rx) = event_channel(64);
        let core_ticks = TickSender { ingest: core_events.ingest.clone() };
        door.send((core_events, core_ticks)).expect("the relay listens");
        for want in 1..=CAP as u64 {
            match core_rx.blocking_recv() {
                Some(Ingest::GatewayEvent(g)) => {
                    assert_eq!(g.cursor.seq, want, "the kept prefix, in order, with its cursors");
                }
                other => panic!("event {want} of the kept prefix never arrived: {other:?}"),
            }
        }
        assert!(
            matches!(core_rx.blocking_recv(), Some(Ingest::ExecLink(u)) if !u.up),
            "the link status past the cap is kept"
        );
        // From here on, live traffic: the gapped account loses its cursor, another keeps it.
        live.blocking_send_gateway(gateway("binance", SENT + 1)).expect("open");
        live.blocking_send_gateway(gateway("bybit", 1)).expect("open");
        match core_rx.blocking_recv() {
            Some(Ingest::Event(Event::OrderSubmitted(_))) => {}
            other => panic!("the gapped account's next event must carry no cursor: {other:?}"),
        }
        match core_rx.blocking_recv() {
            Some(Ingest::GatewayEvent(g)) => assert_eq!(g.route_key.as_str(), "bybit"),
            other => panic!("another account keeps its cursor: {other:?}"),
        }
        drop(live);
        relay.join().expect("the relay ends when the lane closes");
        assert!(core_rx.try_recv().is_err(), "and nothing more");
    }

    /// The memory the cap allows, from the REAL sizes (`vike_exec`'s own guard holds `Ingest` at 168
    /// bytes): an `Ingest` inline in the queue plus the boxed `GatewayEvent` it points at, times the cap.
    /// The strings an event owns and the queue's last doubling come on top (documented at the constant:
    /// 120 to 170 MB at the cap). A field that grows either type moves this number and fails here, where
    /// the cap is.
    #[test]
    fn the_boot_backlog_cap_is_bounded_by_the_real_size_of_what_it_holds() {
        use super::super::build::PRECORE_BACKLOG_CAP;
        let per_event = std::mem::size_of::<Ingest>() + std::mem::size_of::<GatewayEvent>();
        let at_cap = PRECORE_BACKLOG_CAP * per_event;
        assert!(per_event <= 400, "an Ingest and its boxed GatewayEvent grew to {per_event} bytes");
        assert!(
            at_cap <= 100 * 1024 * 1024,
            "the backlog at its cap is {at_cap} bytes before any string: raise the documented \
             estimate at PRECORE_BACKLOG_CAP or lower the cap"
        );
    }

    /// An assembly that fails before the core exists closes the door: the relay ends instead of
    /// waiting for a core that will never come.
    #[test]
    fn a_door_closed_without_a_core_ends_the_relay() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let (live, live_rx) = event_channel(8);
        let (door, door_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let relay = std::thread::spawn(move || {
            super::super::build::run_live_forwarder(live_rx, &door_rx, &stop);
        });
        drop(door);
        relay.join().expect("the relay ends with its door");
        assert!(live.blocking_send(submitted("late")).is_err(), "the lane is closed with it");
    }

    /// A core that is gone ends the relay (the thread `break`s on the error).
    #[test]
    fn a_gone_core_is_an_error() {
        let (events, ticks, rx) = core();
        drop(rx);
        assert!(forward_ingest(Ingest::Event(submitted("c3")), &events, &ticks).is_err());
    }
}
