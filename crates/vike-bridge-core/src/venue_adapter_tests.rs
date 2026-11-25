use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_execution::recon::ReconcileClientFake;
use vike_log::capture::captured;
use vike_model::events::Event;

use crate::exec_actor::run_loop;
use crate::venue_adapter_fixture::{AdapterFixture, PlantedAdapter, found_tier_events};

/// A stub implementation, used only to prove the shape compiles as a registry row.
static NOTHING: PlantedAdapter =
    PlantedAdapter::new("binance", ArmingResolution::NotRunning(ArmingNotRunningCause::NoLiveArm));

/// THE OBJECT-SAFETY PIN: a `'static` table of `&dyn VenueAdapter` rows is what `vike-tradehub`'s
/// registry is, so it must compile here first.
#[test]
fn the_contract_is_object_safe_and_a_static_table_of_rows_compiles() {
    static ROWS: &[&dyn VenueAdapter] = &[&NOTHING];
    assert_eq!(ROWS[0].venue(), "binance");
    let fx = AdapterFixture::new(&[]);
    assert_eq!(
        ROWS[0].resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoLiveArm)
    );
    assert!(
        ROWS[0].server_time_ms(&fx.inputs(AccountTier::Live), Duration::from_millis(1)).is_err()
    );
    assert!(ROWS[0].credential_probe(&fx.inputs(AccountTier::Live)).is_none());
}

#[test]
fn a_paper_outcome_carries_no_reconcile_and_no_identity() {
    let out = AdapterOutcome::paper();
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

fn counting_build(count: &AtomicUsize) -> Option<Box<dyn ReconcileClient>> {
    count.fetch_add(1, Ordering::SeqCst);
    None
}

/// `reconcile_if_enabled`'s whole property: the factory is never CALLED while reconciliation is off.
#[test]
fn reconcile_if_enabled_is_lazy_when_disabled() {
    let count = AtomicUsize::new(0);
    assert!(reconcile_if_enabled(false, || counting_build(&count)).is_none());
    assert_eq!(count.load(Ordering::SeqCst), 0, "a disabled gate must not call the factory");
    let _ = reconcile_if_enabled(true, || counting_build(&count));
    assert_eq!(count.load(Ordering::SeqCst), 1, "an enabled gate calls it exactly once");
}

#[test]
fn the_missing_field_message_names_the_field() {
    assert_eq!(missing_time_field("time"), "time missing from the server-time response");
}

/// The workspace's existing offline `ReconcileClient` double — never fetched from here; these tests
/// assert on WHETHER a client was constructed, never on what it returns.
fn stub() -> Box<dyn ReconcileClient> {
    Box::new(ReconcileClientFake::default())
}

/// GATE ON — the other direction, the one that keeps laziness from becoming a silent
/// reconciliation outage: the factory runs EXACTLY once and its result is passed through
/// untouched.
#[test]
fn reconcile_if_enabled_builds_exactly_once_when_enabled() {
    let calls = AtomicUsize::new(0);
    let out = reconcile_if_enabled(true, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Some(stub())
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "enabled ⇒ built once, not zero and not twice");
    assert!(out.is_some(), "enabled ⇒ the factory's client is returned verbatim");
}

/// …and an ENABLED gate does not paper over a factory that fails: a venue whose handshake
/// returns `None` (unreachable / bad creds) still resolves `None` — the gate short-circuits, it
/// never substitutes.
#[test]
fn reconcile_if_enabled_passes_through_a_failed_build() {
    let calls = AtomicUsize::new(0);
    let out = reconcile_if_enabled(true, || {
        calls.fetch_add(1, Ordering::SeqCst);
        None
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(out.is_none(), "a failed handshake stays reconcile-inert, exec unaffected");
}

/// The one-key-set probe the venues whose tier is the network answer through (their copies' whole
/// matrix): a set arms the tier the network names, nothing held below live; none is paper,
/// `LiveCredentialsAbsent` on mainnet. Red on a swapped cause, an inverted `bound_tier` or a
/// held-below-live reason.
#[test]
fn a_one_key_set_arms_the_tier_its_network_names() {
    use vike_model::VenueNetwork::{Mainnet, Testnet};
    assert_eq!(bound_tier(Mainnet), AccountTier::Live);
    assert_eq!(bound_tier(Testnet), AccountTier::Demo);
    let armed = |tier| ArmingResolution::Armed { tier, held_below_live: None };
    assert_eq!(resolve_one_key_set(true, Mainnet), armed(AccountTier::Live));
    assert_eq!(resolve_one_key_set(true, Testnet), armed(AccountTier::Demo));
    assert_eq!(
        resolve_one_key_set(false, Mainnet),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveCredentialsAbsent)
    );
    assert_eq!(
        resolve_one_key_set(false, Testnet),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
}

/// A demo-only arm (alpaca's sandbox, ig's demo gateway, ctrader's and dukascopy's tier gate) over
/// every tier: `Paper` reads as `Demo`, `Live` alone is refused, and with no config the tier is never
/// asked. Red on a `Paper => false`, a `Live => true`, or a resolution arm swapped.
#[test]
fn a_demo_only_arm_refuses_only_the_live_tier() {
    assert!(demo_arm(AccountTier::Demo) && demo_arm(AccountTier::Paper));
    assert!(!demo_arm(AccountTier::Live));
    let demo = ArmingResolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm),
    };
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert!(matches!(ArmingDemoOnly::of(tier, Some(7)), ArmingDemoOnly::Demo(7)), "{tier}");
        assert_eq!(ArmingDemoOnly::of(tier, Some(7)).resolution(), demo, "{tier}");
    }
    let live = ArmingDemoOnly::of(AccountTier::Live, Some(7));
    assert!(matches!(live, ArmingDemoOnly::LiveTierNotWired));
    assert_eq!(
        live.resolution(),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
    for tier in AccountTier::ALL {
        let none = ArmingDemoOnly::<u8>::of(tier, None);
        assert!(matches!(none, ArmingDemoOnly::Unconfigured), "{tier}");
        assert_eq!(
            none.resolution(),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
            "{tier}"
        );
    }
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a label")
}

/// The live-tier line is said for a `live` account's whole set alone, once, in the venue's own word
/// for its demo tier, carrying `found_tier` and never `tier`. Red on a line for another arm, or on a
/// literal word where the venue's table speaks.
#[test]
fn a_demo_only_arm_says_the_live_tier_in_the_venues_word_and_nothing_else() {
    let ((), events) = captured(|| {
        ArmingDemoOnly::<u8>::of(AccountTier::Live, Some(7)).report_live_tier("alpaca", &alt());
    });
    let [line] = events.as_slice() else { panic!("one line: {events:?}") };
    assert_eq!(line.field("found_tier"), Some("live"));
    assert_eq!(line.field("tier"), None, "{line:?}");
    assert_eq!((line.field("venue"), line.field("account")), (Some("alpaca"), Some("ALT")));
    assert!(line.message.contains("mounts only its sandbox tier"), "{}", line.message);
    assert!(line.message.contains("account ALT is a LIVE account"), "{}", line.message);
    for quiet in [
        ArmingDemoOnly::of(AccountTier::Demo, Some(7)),
        ArmingDemoOnly::of(AccountTier::Live, None),
    ] {
        let ((), events) = captured(|| quiet.report_live_tier("alpaca", &alt()));
        assert!(events.is_empty(), "{events:?}");
    }
}

/// The demo-only start's prefix: the demo config starts; a refused demo address and a `live`
/// account's whole set do not, each said once; an unconfigured account does not, silently.
#[test]
fn the_demo_only_start_prefix_starts_only_a_demo_config_and_says_why_not() {
    let start = |arm: ArmingDemoOnly<Result<u8, &str>>| {
        captured(|| arm.config_for_start("deribit", &AccountLabel::Default))
    };
    let (config, events) = start(ArmingDemoOnly::of(AccountTier::Demo, Some(Ok(7))));
    assert_eq!((config, events.len()), (Some(7), 0), "{events:?}");

    let (config, events) = start(ArmingDemoOnly::of(AccountTier::Demo, Some(Err("no row"))));
    let [line] = events.as_slice() else { panic!("one line: {events:?}") };
    assert_eq!(config, None);
    assert_eq!(line.level, tracing::Level::ERROR);
    assert_eq!(line.message, "deribit: REFUSED — no row; the account is NOT RUNNING");
    assert_eq!(line.field("account"), Some(AccountLabel::Default.to_string().as_str()));

    let (config, events) = start(ArmingDemoOnly::of(AccountTier::Live, Some(Ok(7))));
    assert_eq!(config, None);
    assert_eq!(found_tier_events(&events).len(), 1, "{events:?}");
    assert!(events[0].message.contains("mounts only its testnet tier"), "{events:?}");

    let (config, events) = start(ArmingDemoOnly::of(AccountTier::Demo, None));
    assert_eq!((config, events.len()), (None, 0), "{events:?}");
}

/// A one-key-set arm warns of a `live` account with no keys, and of nothing below live.
#[test]
fn only_a_live_account_with_no_keys_is_warned() {
    let ((), events) =
        captured(|| report_live_credentials_absent("aster", &alt(), AccountTier::Live));
    let [line] = events.as_slice() else { panic!("one line: {events:?}") };
    assert_eq!(line.level, tracing::Level::WARN);
    assert_eq!(line.message, format!("aster: {LIVE_CREDENTIALS_ABSENT}"));
    assert_eq!((line.field("venue"), line.field("account")), (Some("aster"), Some("ALT")));
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        let ((), events) = captured(|| report_live_credentials_absent("aster", &alt(), tier));
        assert!(events.is_empty(), "{tier}: {events:?}");
    }
}

/// A failing probe fails every time, in the words it was given, as a refusal.
#[test]
fn a_failing_probe_fails_in_its_words() {
    let CredentialProbe::ReadOnly(probe) = CredentialProbe::failing("no row".to_string()) else {
        panic!("a read-only probe")
    };
    let refused = vike_execution::recon::ReconcileFetchError::new(
        vike_utility::errors::FailureOutcome::Refused,
        "no row",
    );
    assert_eq!((probe(), probe()), (Err(refused.clone()), Err(refused)));
}

#[test]
fn an_account_is_named_as_the_default_or_by_its_label() {
    assert_eq!(account_phrase(&AccountLabel::Default), "the default account");
    assert_eq!(account_phrase(&alt()), "account ALT");
}

fn refused_order(order_client_id: &str) -> Box<vike_model::OrderRequest> {
    Box::new(crate::execution_drive::limit("bybit", order_client_id, "BTCUSDT", 0.01, 50000.0))
}

fn drain_events(rx: &mut tokio::sync::mpsc::Receiver<vike_execution::Ingest>) -> Vec<Event> {
    crate::execution_drive::drain_ready(|| rx.try_recv().ok())
}

/// THE ONE FORM OF AN ARMING REFUSAL: for the orders a refusing session made itself,
/// `refuse_command` answers every command exactly as `run_loop` over `VenueRestRefusing` does, so a loop
/// of its own and the shared loop cannot drift.
#[test]
fn refuse_command_answers_as_run_loop_over_refusing_rest() {
    let order = refused_order;
    let commands = || {
        let intent = vike_model::OrderCancelIntent::Unspecified;
        vec![
            ExecutionClientActorCommand::Submit(order("c1"), std::time::Instant::now()),
            ExecutionClientActorCommand::Submit(order("c2"), std::time::Instant::now()),
            ExecutionClientActorCommand::Submit(order("c3"), std::time::Instant::now()),
            ExecutionClientActorCommand::Cancel { order_client_id: "c1".into(), intent },
            ExecutionClientActorCommand::CancelBatch {
                order_client_ids: vec!["c2".into(), "c3".into()],
                intent,
            },
            ExecutionClientActorCommand::Modify {
                order: order("c4"),
                new_qty: Some(1.0),
                new_price: None,
                new_trigger_price: None,
                queued_at: std::time::Instant::now(),
            },
            ExecutionClientActorCommand::Modify {
                order: order("c5"),
                new_qty: None,
                new_price: None,
                new_trigger_price: Some(9.0),
                queued_at: std::time::Instant::now(),
            },
        ]
    };
    let drain = drain_events;

    let (sender, mut ingest) = vike_execution::event_channel(64);
    for cmd in commands() {
        assert!(refuse_command(&sender, cmd, "refused: why"));
    }
    let shutdown = refuse_command(&sender, ExecutionClientActorCommand::Shutdown, "refused: why");
    assert!(!shutdown, "Shutdown leaves");
    let ours = drain(&mut ingest);

    let (sender, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel();
    commands()
        .into_iter()
        .chain([ExecutionClientActorCommand::Shutdown])
        .for_each(|cmd| tx.send(cmd).unwrap());
    let rest = VenueRestRefusing { why: "refused: why".to_string() };
    run_loop(&rest, &sender, &rx, Vec::new, Duration::from_secs(3600));
    // An amend's refusal is stamped when it is made (`crate::rest::amend_answer_time`): the two runs
    // agree on everything but that moment.
    let unstamped = |events: Vec<Event>| -> Vec<Event> {
        events
            .into_iter()
            .map(|mut event| {
                if let Event::OrderModifyRejected(r) = &mut event {
                    assert!(r.timestamp > 0, "never 0");
                    r.timestamp = 0;
                }
                event
            })
            .collect()
    };
    assert_eq!(unstamped(ours.clone()), unstamped(drain(&mut ingest)));

    let kinds: Vec<&str> = ours
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderRejected(_) => "Rejected",
            Event::OrderCanceled(_) => "Canceled",
            Event::OrderModifyRejected(_) => "ModifyRejected",
            other => panic!("{other:?}"),
        })
        .collect();
    let refusals = ["Submitted", "Rejected"].repeat(3);
    let answers = ["Canceled", "Canceled", "Canceled", "ModifyRejected", "ModifyRejected"];
    assert_eq!(
        kinds,
        [refusals.as_slice(), answers.as_slice()].concat(),
        "every amend, of the price and of the trigger, is refused in the refusal's words"
    );
    for event in &ours {
        if let Event::OrderModifyRejected(r) = event {
            assert_eq!(
                r.reason.as_str(),
                "refused: why",
                "the operator reads the start's sentence"
            );
        }
    }
}

/// A cancel of an order this session did NOT make (restored from an earlier process, the venue may
/// still hold it) is the non-terminal `OrderCancelRejected` with the refusal's words, alone and in a
/// batch; an order this session refused is `OrderCanceled` each time, as `VenueRestRefusing` answers it.
/// Red on a terminal `OrderCanceled` for an order that may still rest.
#[test]
fn a_refusing_session_never_cancels_an_order_it_did_not_make() {
    let (sender, mut ingest) = vike_execution::event_channel(64);
    let intent = vike_model::OrderCancelIntent::Unspecified;
    let commands = [
        ExecutionClientActorCommand::Cancel { order_client_id: "restored".into(), intent },
        ExecutionClientActorCommand::Submit(refused_order("mine"), std::time::Instant::now()),
        ExecutionClientActorCommand::CancelBatch {
            order_client_ids: vec!["mine".into(), "restored".into()],
            intent,
        },
        ExecutionClientActorCommand::Cancel { order_client_id: "mine".into(), intent },
    ];
    for cmd in commands {
        assert!(refuse_command(&sender, cmd, "refused: why"));
    }
    // Nothing a refusing session answers was sent (decision 0303's NOT SENT).
    let not_sent = Some(vike_model::OrderRefusalId::NotSent.word());
    let answers: Vec<(String, &str)> = drain_events(&mut ingest)
        .into_iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(c) => Some((c.order_client_id, "Canceled")),
            Event::OrderCancelRejected(r) => {
                assert!(r.reason.as_str().starts_with("refused: why;"), "{}", r.reason.as_str());
                assert_eq!(r.refusal_id.as_deref(), not_sent);
                Some((r.order_client_id, "CancelRejected"))
            }
            Event::OrderRejected(r) => {
                assert_eq!(r.refusal_id.as_deref(), not_sent);
                None
            }
            Event::OrderSubmitted(_) => None,
            other => panic!("{other:?}"),
        })
        .collect();
    let answer = |order_client_id: &str, kind| (order_client_id.to_string(), kind);
    assert_eq!(
        answers,
        [
            answer("restored", "CancelRejected"),
            answer("mine", "Canceled"),
            answer("restored", "CancelRejected"),
            answer("mine", "Canceled"),
        ]
    );
}

/// A venue that takes every order, for the loop that runs once the properties loaded.
struct Accepting;

impl crate::rest::VenueRest for Accepting {
    fn submit_order(&self, request: &vike_model::OrderRequest) -> Vec<Event> {
        let order_client_id = request.order_client_id.clone();
        vec![
            Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                order_client_id: order_client_id.clone(),
                timestamp: 0,
            }),
            Event::OrderAccepted(vike_model::events::OrderAccepted {
                order_client_id,
                venue_order_id: Some("v-1".to_string().into()),
                timestamp: 0,
            }),
        ]
    }
    fn cancel_order(
        &self,
        _order_client_id: &str,
    ) -> Result<crate::rest::OrderCancelAck, crate::transport::VenueApiError> {
        Ok(crate::rest::OrderCancelAck::Canceled)
    }
}

/// The venue's command-loop client built with NO grid, as a bridge hands it to `await_properties`: it
/// records every cancel it SENDS (an order client id starting `gone` is one the venue no longer holds, one starting
/// `taken` one whose cancel the venue only took), and it must never be asked to place an order. Its read by order
/// client id finds a `taken-cancelled` order cancelled (behind the accept a found order carries), a `taken-resting`
/// one still resting, and a `taken-absent` one nowhere (the "absent" reject).
#[derive(Default)]
struct GridLess {
    sent: std::sync::Mutex<Vec<String>>,
}

impl crate::rest::VenueRest for GridLess {
    fn submit_order(&self, request: &vike_model::OrderRequest) -> Vec<Event> {
        panic!("a grid-less client never places an order: {}", request.order_client_id)
    }
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<crate::rest::OrderCancelAck, crate::transport::VenueApiError> {
        self.sent.lock().unwrap().push(order_client_id.to_string());
        Ok(if order_client_id.starts_with("gone") {
            crate::rest::OrderCancelAck::AlreadyGone
        } else if order_client_id.starts_with("taken") {
            crate::rest::OrderCancelAck::Requested
        } else {
            crate::rest::OrderCancelAck::Canceled
        })
    }
    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        let accepted = Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some("v1".into()),
            timestamp: ts,
        });
        match order_client_id {
            "taken-cancelled" => vec![
                accepted,
                cancel_event(order_client_id, ExecutionClientActorCancelOutcome::Canceled),
            ],
            "taken-resting" => vec![accepted],
            "taken-absent" => vec![crate::rest::venue_confirms_absent(order_client_id, ts)],
            other => panic!("no read of {other} is expected"),
        }
    }
}

/// NO FALLBACK GRID (owner, 2026-11-13): while the mounted symbol's properties cannot be fetched, its
/// order is refused with a terminal `OrderRejected` that names why (not loaded, and the fetch's own
/// error), the fetch is asked again after the pause, and once it answers the order flows: the next one
/// is served by the venue's loop. Red on an order that vanishes, or on a refusal that hides the error.
#[test]
fn an_order_is_refused_while_the_properties_are_not_loaded_and_admitted_once_they_are() {
    let (sender, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(ExecutionClientActorCommand::Submit(refused_order("early"), std::time::Instant::now()))
        .unwrap();
    let asked = AtomicUsize::new(0);
    let fetch = || match asked.fetch_add(1, Ordering::SeqCst) {
        0 => Err("exchangeInfo could not be read: HTTP 503".to_string()),
        _ => Ok(0.25),
    };
    let pause = Duration::from_millis(30);
    let found =
        await_properties("binance", "ETHUSDT", &sender, &rx, pause, GridLess::default, fetch);
    assert_eq!(found, Some(0.25), "the second fetch's answer");
    assert_eq!(asked.load(Ordering::SeqCst), 2, "asked again after the pause, and no more");
    let refused = drain_events(&mut ingest);
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(matches!(&refused[0], Event::OrderSubmitted(s) if s.order_client_id == "early"));
    let Event::OrderRejected(r) = &refused[1] else { panic!("{refused:?}") };
    assert_eq!(r.order_client_id, "early");
    assert_eq!(
        r.reason.as_str(),
        "binance: ETHUSDT: properties not loaded (exchangeInfo could not be read: HTTP 503); the \
         order was NOT sent, and the properties are fetched again"
    );

    tx.send(ExecutionClientActorCommand::Submit(refused_order("late"), std::time::Instant::now()))
        .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&Accepting, &sender, &rx, Vec::new, Duration::from_secs(3600));
    let served = drain_events(&mut ingest);
    assert!(
        served.iter().any(|e| matches!(e, Event::OrderAccepted(a) if a.order_client_id == "late")),
        "once the properties loaded the order reaches the venue: {served:?}"
    );
}

/// A `Shutdown` while the properties are not loaded ends the wait: nothing more is fetched and the
/// caller builds nothing.
#[test]
fn a_shutdown_ends_the_wait_for_the_properties() {
    let (sender, _ingest) = vike_execution::event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    let asked = AtomicUsize::new(0);
    let fetch = || -> Result<f64, String> {
        asked.fetch_add(1, Ordering::SeqCst);
        Err("unreachable".to_string())
    };
    let pause = Duration::from_secs(60);
    let found =
        await_properties("okx", "ETH-USDT-SWAP", &sender, &rx, pause, GridLess::default, fetch);
    assert_eq!(found, None);
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

/// **A CANCEL NEEDS NO GRID, SO IT IS SENT** (behaviour kept: the owner ruled only new orders refused):
/// with the grid fetch failing, a cancel of an order an EARLIER run placed reaches the venue through the
/// grid-less client and is answered as the trading loop answers it (`OrderCanceled`), while an amend,
/// which needs the grid, is refused in words. Red on a cancel that waits for the grid or is refused.
#[test]
fn a_cancel_of_an_earlier_runs_order_is_sent_while_the_grid_is_unknown() {
    let (sender, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel();
    let intent = vike_model::OrderCancelIntent::Unspecified;
    tx.send(ExecutionClientActorCommand::Cancel { order_client_id: "restored".into(), intent })
        .unwrap();
    tx.send(ExecutionClientActorCommand::CancelBatch {
        order_client_ids: vec!["restored-2".into(), "restored-3".into()],
        intent,
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Modify {
        order: refused_order("restored"),
        new_qty: None,
        new_price: Some(1.0),
        new_trigger_price: None,
        queued_at: std::time::Instant::now(),
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    let light = std::sync::Arc::new(GridLess::default());
    struct Shared(std::sync::Arc<GridLess>);
    impl crate::rest::VenueRest for Shared {
        fn submit_order(&self, request: &vike_model::OrderRequest) -> Vec<Event> {
            self.0.submit_order(request)
        }
        fn cancel_order(
            &self,
            order_client_id: &str,
        ) -> Result<crate::rest::OrderCancelAck, crate::transport::VenueApiError> {
            self.0.cancel_order(order_client_id)
        }
    }
    let handed = std::sync::Arc::clone(&light);
    let fetch = || -> Result<f64, String> { Err("exchangeInfo: HTTP 503".to_string()) };
    let pause = Duration::from_secs(60);
    let found =
        await_properties("binance", "ETHUSDT", &sender, &rx, pause, move || Shared(handed), fetch);
    assert_eq!(found, None, "shut down before the grid loaded");
    assert_eq!(
        *light.sent.lock().unwrap(),
        ["restored", "restored-2", "restored-3"],
        "every cancel was SENT, none waited for the grid"
    );
    let answers: Vec<(String, &str)> = drain_events(&mut ingest)
        .into_iter()
        .map(|e| match e {
            Event::OrderCanceled(c) => (c.order_client_id, "Canceled"),
            Event::OrderModifyRejected(r) => {
                assert!(
                    r.reason.as_str().contains("properties not loaded"),
                    "{}",
                    r.reason.as_str()
                );
                (r.order_client_id, "ModifyRejected")
            }
            other => panic!("{other:?}"),
        })
        .collect();
    let answer = |order_client_id: &str, kind| (order_client_id.to_string(), kind);
    assert_eq!(
        answers,
        [
            answer("restored", "Canceled"),
            answer("restored-2", "Canceled"),
            answer("restored-3", "Canceled"),
            answer("restored", "ModifyRejected"),
        ]
    );
}

/// A cancel the venue calls "already gone" during the wait gets the trading loop's non-terminal answer,
/// and the loop that runs once the grid loads takes it over: its end settles it `OrderCanceled`, as it
/// settles its own. Red on an "already gone" left unanswered for ever, or answered terminally at once.
#[test]
fn an_already_gone_cancel_during_the_wait_is_settled_by_the_loop_after_it() {
    let (sender, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel();
    let intent = vike_model::OrderCancelIntent::Unspecified;
    tx.send(ExecutionClientActorCommand::Cancel { order_client_id: "gone-1".into(), intent })
        .unwrap();
    let asked = AtomicUsize::new(0);
    let fetch = || match asked.fetch_add(1, Ordering::SeqCst) {
        0 => Err("exchangeInfo: HTTP 503".to_string()),
        _ => Ok(()),
    };
    let pause = Duration::from_millis(30);
    let found =
        await_properties("binance", "ETHUSDT", &sender, &rx, pause, GridLess::default, fetch);
    assert_eq!(found, Some(()));
    let during = drain_events(&mut ingest);
    let [Event::OrderCancelRejected(r)] = during.as_slice() else { panic!("{during:?}") };
    assert_eq!(
        (r.order_client_id.as_str(), r.reason.as_str()),
        ("gone-1", crate::exec_actor::ALREADY_GONE_REASON)
    );

    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&Accepting, &sender, &rx, Vec::new, Duration::from_secs(3600));
    let after = drain_events(&mut ingest);
    assert!(
        after.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "gone-1")),
        "the loop after the wait settles it: {after:?}"
    );
}

/// A cancel the venue only TOOK during the wait (`Requested`: bybit's and okx's answer) is read back by its order
/// client id at once, since no stream is open to end it and the loop after the wait drops the venue's row as older
/// than itself: the read's cancel is the answer, and nothing else it says is (no second accept, no "absent"
/// reject); a read that shows no cancel says nothing, and the order stays live. Red on a taken cancel left
/// unanswered while the read says cancelled, or on a guess.
#[test]
fn a_taken_cancel_during_the_wait_is_read_back_by_its_order_client_id() {
    let (sender, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel();
    let intent = vike_model::OrderCancelIntent::Unspecified;
    for order_client_id in ["taken-cancelled", "taken-resting", "taken-absent"] {
        tx.send(ExecutionClientActorCommand::Cancel {
            order_client_id: order_client_id.into(),
            intent,
        })
        .unwrap();
    }
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    let fetch = || -> Result<(), String> { Err("instruments-info: HTTP 503".to_string()) };
    let pause = Duration::from_secs(60);
    let found =
        await_properties("bybit", "BTCUSDT.P", &sender, &rx, pause, GridLess::default, fetch);
    assert_eq!(found, None);
    let answers: Vec<(String, &str)> = drain_events(&mut ingest)
        .into_iter()
        .map(|e| match e {
            Event::OrderCanceled(c) => (c.order_client_id, "Canceled"),
            other => panic!("only the read's cancel is forwarded: {other:?}"),
        })
        .collect();
    assert_eq!(answers, [("taken-cancelled".to_string(), "Canceled")]);
}

/// A bridge that implements no read answers "not offered by <venue>" for EVERY kind, in the one
/// sentence `vike_model::not_offered_words` spells, and asks nothing of the venue.
#[test]
fn a_bridge_that_writes_no_read_is_not_offered_in_words() {
    let fx = AdapterFixture::new(&[]);
    for kind in [
        vike_model::VenueReadKind::Balance,
        vike_model::VenueReadKind::OpenOrders,
        vike_model::VenueReadKind::Positions,
    ] {
        let why = NOTHING
            .venue_read(&fx.inputs(AccountTier::Live), kind)
            .expect_err("the default offers nothing");
        assert_eq!(why, vike_model::not_offered_words("binance", kind).into());
        assert!(why.text().starts_with("not offered by binance"), "{why}");
    }
}
