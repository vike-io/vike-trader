//! White-box tests of [`fold_conditionals`] — the pure membership fold behind
//! re-arm-on-restore. The end-to-end restore is gated in `tests/conditional_journal.rs`;
//! these pin the per-record semantics (add / remove / mass-cancel scoping) in isolation.

use super::fold::{fold_gateway_cursors, tail_facts};
use super::*;
use vike_exec::GatewayCursor;
use vike_journal::ConditionalRecord;

fn arm(arm_id: &str, venue: &str, symbol: &str, price: Option<f64>) -> SnapConditional {
    SnapConditional {
        arm_id: arm_id.into(),
        terms: ConditionalRecord {
            venue: venue.into(),
            symbol: symbol.into(),
            side: -1,
            qty: 1.0,
            price,
            trail: price.is_none().then_some(5.0),
            extreme: price.is_none().then_some(100.0),
            trigger_by: None,
            reduce_only: false,
        },
        route_key: None,
    }
}

fn armed_rec(seq: u64, c: &SnapConditional) -> JournalRecord {
    JournalRecord::ConditionalArmed {
        seq,
        now_ms: seq as i64,
        arm_id: c.arm_id.clone(),
        resolved: c.terms.clone(),
    }
}

fn fire_rec(seq: u64, arm_id: &str) -> JournalRecord {
    JournalRecord::ConditionalFire {
        seq,
        now_ms: seq as i64,
        arm_id: arm_id.into(),
        trigger_px: 0.0,
        req: OrderRequest::default(),
        route_key: None,
    }
}

fn disarm_rec(seq: u64, arm_id: &str) -> JournalRecord {
    JournalRecord::ConditionalDisarmed { seq, now_ms: seq as i64, arm_id: arm_id.into() }
}

fn cmd(seq: u64, intent: vike_exec::OrderIntent) -> JournalRecord {
    JournalRecord::Cmd {
        seq,
        now_ms: seq as i64,
        msg: Ingest::Command(vike_exec::Command::Order(intent)),
    }
}

fn ids(books: &[SnapConditional]) -> Vec<&str> {
    books.iter().map(|c| c.arm_id.as_str()).collect()
}

#[test]
fn armed_adds_fire_and_disarm_remove_in_record_order() {
    let base = vec![arm("a0", "sim", "BTC", None), arm("a1", "sim", "BTC", Some(95.0))];
    let a2 = arm("a2", "sim", "BTC", Some(90.0));
    let tail = vec![
        fire_rec(10, "a0"),   // the live session consumed a0 (the replay core never would)
        armed_rec(11, &a2),   // armed after the base Snap
        disarm_rec(12, "a1"), // disarmed after the base Snap
    ];
    let (books, armed_count) = fold_conditionals(base, &tail);
    assert_eq!(ids(&books), vec!["a2"], "fire and disarm removed; the tail arm added");
    assert_eq!(books[0].terms.price, Some(90.0), "the ARM record's resolved terms carried");
    assert_eq!(armed_count, 1);
}

#[test]
fn tail_arms_keep_record_order_which_is_fire_order() {
    let a2 = arm("a2", "sim", "BTC", Some(90.0));
    let a3 = arm("a3", "sim", "BTC", Some(85.0));
    let (books, _) = fold_conditionals(
        vec![arm("a0", "sim", "BTC", Some(95.0))],
        &[armed_rec(1, &a2), armed_rec(2, &a3)],
    );
    assert_eq!(ids(&books), vec!["a0", "a2", "a3"], "base first, tail in record order");
}

#[test]
fn mass_cancel_scoping_matches_the_live_arm() {
    let base = || {
        vec![
            arm("a0", "sim", "BTC", Some(95.0)),
            arm("a1", "sim", "ETH", Some(90.0)),
            arm("a2", "other", "BTC", Some(85.0)),
        ]
    };
    // global: everything clears
    let (books, _) = fold_conditionals(
        base(),
        &[cmd(1, vike_exec::OrderIntent::MassCancel { venue: None, symbol: None, account: None })],
    );
    assert!(books.is_empty(), "global mass-cancel clears every book");
    // venue-scoped: only that venue's arms clear
    let (books, _) = fold_conditionals(
        base(),
        &[cmd(
            1,
            vike_exec::OrderIntent::MassCancel {
                venue: Some("sim".into()),
                symbol: None,
                account: None,
            },
        )],
    );
    assert_eq!(ids(&books), vec!["a2"], "venue scope clears only sim's arms");
    // (venue, symbol)-scoped: only the one book clears
    let (books, _) = fold_conditionals(
        base(),
        &[cmd(
            1,
            vike_exec::OrderIntent::MassCancel {
                venue: Some("sim".into()),
                symbol: Some("BTC".into()),
                account: None,
            },
        )],
    );
    assert_eq!(ids(&books), vec!["a1", "a2"], "(venue, symbol) scope clears one book");
    // symbol without venue: the live no-op
    let (books, _) = fold_conditionals(
        base(),
        &[cmd(
            1,
            vike_exec::OrderIntent::MassCancel {
                venue: None,
                symbol: Some("BTC".into()),
                account: None,
            },
        )],
    );
    assert_eq!(ids(&books), vec!["a0", "a1", "a2"], "symbol-without-venue is ignored");
    // MarketExit: venue-scoped clear (its mass-cancel leg), None = all
    let (books, _) = fold_conditionals(
        base(),
        &[cmd(1, vike_exec::OrderIntent::MarketExit { venue: Some("sim".into()), account: None })],
    );
    assert_eq!(ids(&books), vec!["a2"], "MarketExit clears its venue scope");
    let (books, _) = fold_conditionals(
        base(),
        &[cmd(1, vike_exec::OrderIntent::MarketExit { venue: None, account: None })],
    );
    assert!(books.is_empty(), "venue-less MarketExit clears everything");
}

/// Three arms on two venues, and the label `ALT`, which both account-row tests below use.
fn account_row_base() -> Vec<SnapConditional> {
    vec![
        arm("a0", "sim", "BTC", Some(95.0)),
        arm("a1", "sim", "ETH", Some(90.0)),
        arm("a2", "other", "BTC", Some(85.0)),
    ]
}

fn alt_account() -> Option<vike_model::accounts::account_keys::AccountLabel> {
    Some(vike_model::accounts::account_keys::AccountLabel::Named("ALT".into()))
}

/// **An account named with NO venue folds as the live REFUSAL: nothing clears.** The live core
/// refuses the intent (`CoreThread::reduce_route_for_account`) instead of reading it as the
/// global clear. A fold that cleared everything would restore books the live session never
/// had, with every protective stop gone after a restart. The live side of this row is
/// `mount_account_tests`' `an_account_named_with_no_venue_is_refused_and_never_widened`, which
/// asserts the protection unchanged.
///
/// This is live-arm parity in both directions. The fold carries no route key, but it needs
/// none here: the live refusal does not depend on which accounts the core holds.
#[test]
fn a_reducing_verb_naming_an_account_and_no_venue_folds_as_the_live_refusal() {
    for venueless in [
        vike_exec::OrderIntent::MassCancel { venue: None, symbol: None, account: alt_account() },
        vike_exec::OrderIntent::MarketExit { venue: None, account: alt_account() },
    ] {
        let label = format!("{venueless:?}");
        let (books, _) = fold_conditionals(account_row_base(), &[cmd(1, venueless)]);
        assert_eq!(
            ids(&books),
            vec!["a0", "a1", "a2"],
            "{label}: an account with no venue is refused live, so it clears nothing here"
        );
    }
}

/// ⚠ **A venue-scoped labelled reduce folds as the VENUE clear. That matches the live arm for a
/// HELD account, and it is `fold_intent_scope`'s declared residual for an UNHELD one.** This
/// test pins both cases by name.
///
/// * **HELD account (parity).** This fold is single-engine by construction (`replay_from`
///   refuses a multi-engine base). On one engine, a held account's narrowed clear covers the
///   whole venue scope, because every arm resolves to the one engine there is.
/// * **UNHELD account (the residual, NOT parity).** The live core refuses the intent and clears
///   nothing, while this fold clears `sim`'s arms. The fold cannot tell the two cases apart:
///   `EngineSnapshot` carries no route key, so neither this fixture nor `fold_intent_scope` can
///   say which account the base engine holds. The assertion below pins that divergence as a
///   known residual, not as correct. When the base carries its route key, the unheld case must
///   FLIP to "clears nothing". That flip is the fix, not a regression.
///
/// **What this test does and does not witness.** It pins the fold's answer, including that a
/// labelled reduce never clears past its venue (`other`'s arm survives). It does NOT witness the
/// account change itself. It passed while the fold still ignored the field, because the answer
/// is the account-less venue clear. The venue-less test above is the one that witnesses the
/// change.
#[test]
fn a_venue_scoped_labelled_reduce_folds_as_the_venue_clear_parity_only_for_a_held_account() {
    for scoped in [
        vike_exec::OrderIntent::MassCancel {
            venue: Some("sim".into()),
            symbol: None,
            account: alt_account(),
        },
        vike_exec::OrderIntent::MarketExit { venue: Some("sim".into()), account: alt_account() },
    ] {
        let label = format!("{scoped:?}");
        let (books, _) = fold_conditionals(account_row_base(), &[cmd(1, scoped)]);
        assert_eq!(
            ids(&books),
            vec!["a2"],
            "{label}: the venue scope — the live answer for a HELD account on one engine, and the \
                 declared residual (the live core refuses and clears nothing) for an unheld one, \
                 which this fold cannot tell apart without the base engine's route key"
        );
    }
}

// ── the gateway door (journal v18) ────────────────────────────────────────────────────────────────

fn gateway_cmd(
    seq: u64,
    event: vike_model::events::Event,
    key: &str,
    c: GatewayCursor,
) -> JournalRecord {
    JournalRecord::Cmd {
        seq,
        now_ms: seq as i64,
        msg: Ingest::GatewayEvent(Box::new(vike_exec::GatewayEvent {
            event,
            route_key: key.into(),
            cursor: c,
        })),
    }
}

fn cursor(gw_epoch: u64, seq: u64) -> GatewayCursor {
    GatewayCursor { gw_epoch, seq, ts_ms: seq as i64 }
}

fn canceled(coid: &str) -> vike_model::events::Event {
    vike_model::events::Event::OrderCanceled(vike_model::events::OrderCanceled {
        client_order_id: coid.into(),
        reason: String::new().into(),
        ts: 1,
    })
}

/// **A terminal that came over a gateway ended its order**, for the crash tail's snap-only reading:
/// read as `Ingest::Event` only, the closed order would restore as resting until the first reconcile
/// pass found it gone. The plain door is the control, and a non-terminal over the gateway ends
/// nothing.
#[test]
fn a_terminal_over_the_gateway_ends_its_order_in_the_crash_tail() {
    let plain = JournalRecord::Cmd { seq: 1, now_ms: 1, msg: Ingest::Event(canceled("plain")) };
    let over_gateway = gateway_cmd(2, canceled("gw"), "sim", cursor(1, 1));
    let not_terminal = gateway_cmd(
        3,
        vike_model::events::Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: "open".into(),
            venue_order_id: None,
            ts: 1,
        }),
        "sim",
        cursor(1, 2),
    );
    let facts = tail_facts(&[plain, over_gateway, not_terminal]);
    assert!(facts.ended("plain"), "the control");
    assert!(facts.ended("gw"), "the gateway door ends an order exactly as the plain one does");
    assert!(!facts.ended("open"), "an acceptance ends nothing");
}

/// **The restore's cursor fold**: the `Snap`'s cursor per account, advanced by the tail in journal
/// order through the one rule -- the larger `seq` inside an epoch, the later arrival across
/// epochs -- and an account with an engine joins in first-seen order. A tail with no gateway event
/// returns the `Snap`'s cursors untouched, and a plain event is no cursor.
#[test]
fn the_cursor_fold_takes_the_snaps_cursors_and_advances_them_over_the_tail() {
    let base = vec![("sim".to_string(), cursor(7, 10)), ("quiet".to_string(), cursor(7, 3))];
    let engines = [snap_of("sim", None), snap_of("quiet", None), snap_of("fresh", None)];
    let fill = |tid: &str| {
        vike_model::events::Event::Fill(vike_model::events::FillEvent {
            trade_id: vike_model::events::TradeId::new(tid).unwrap(),
            client_order_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts: 1,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        })
    };
    assert_eq!(fold_gateway_cursors(&base, &engines, &[]), base, "no tail, no change");
    let plain = JournalRecord::Cmd { seq: 1, now_ms: 1, msg: Ingest::Event(fill("p")) };
    assert_eq!(fold_gateway_cursors(&base, &engines, &[plain]), base, "a plain event is no cursor");

    let tail = vec![
        gateway_cmd(1, fill("a"), "sim", cursor(7, 12)),
        gateway_cmd(2, fill("b"), "sim", cursor(7, 4)), // a straggler: never rewinds
        gateway_cmd(3, fill("c"), "fresh", cursor(2, 1)), // an engine the Snap holds, no cursor yet
        gateway_cmd(4, fill("d"), "quiet", cursor(9, 1)), // a new epoch wins
    ];
    assert_eq!(
        fold_gateway_cursors(&base, &engines, &tail),
        vec![
            ("sim".to_string(), cursor(7, 12)),
            ("quiet".to_string(), cursor(9, 1)),
            ("fresh".to_string(), cursor(2, 1)),
        ]
    );
}

/// An engine snapshot of `venue` answering to `route_key` (`None`: the venue's default account).
fn snap_of(venue: &str, route_key: Option<&str>) -> vike_exec::EngineSnapshot {
    let mut e = vike_exec::ExecutionEngine::new(
        vike_exec::Account::new(1.0, venue, None, vike_exec::BalanceMode::Delta),
        vike_exec::RiskGate::new(vike_model::RiskLimits::new()),
        vike_exec::testing::RecordingClient::default(),
        venue,
        "BTCUSDT",
    );
    if let Some(key) = route_key {
        e.route_key = key.to_string();
    }
    e.snapshot_state()
}

/// **W13b finding 3: a cursor may only advance over events an engine folded.** The refold builds its
/// replay core from the base `Snap`'s engines alone. An account armed AFTER that `Snap` (new account,
/// restart, trades, crash before the next checkpoint) has tail events no engine ever saw, and a cursor
/// past them tells the gateway they were applied: it would never send them again. Only the accounts the
/// `Snap` holds an engine for advance; the others keep the `Snap`'s cursor or have none.
#[test]
fn an_account_the_snap_holds_no_engine_for_gets_no_cursor_from_the_tail() {
    let fill = |tid: &str| {
        vike_model::events::Event::Fill(vike_model::events::FillEvent {
            trade_id: vike_model::events::TradeId::new(tid).unwrap(),
            client_order_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts: 1,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        })
    };
    // `sim` (default account) and `sim#ALT` (a labelled one) are in the Snap; `later` was armed after.
    let engines = [snap_of("sim", None), snap_of("sim", Some("sim#ALT"))];
    let base = vec![("sim".to_string(), cursor(7, 10))];
    let tail = vec![
        gateway_cmd(1, fill("a"), "sim", cursor(7, 11)),
        gateway_cmd(2, fill("b"), "later", cursor(3, 1)),
        gateway_cmd(3, fill("c"), "sim#ALT", cursor(5, 4)),
        gateway_cmd(4, fill("d"), "later", cursor(3, 2)),
    ];
    assert_eq!(
        fold_gateway_cursors(&base, &engines, &tail),
        vec![("sim".to_string(), cursor(7, 11)), ("sim#ALT".to_string(), cursor(5, 4))],
        "an engine in the Snap advances (its route key, the venue id for a default account); `later` \
         has no engine, so its events were folded nowhere and it keeps no cursor"
    );

    // An account the Snap already held a cursor for, but no engine: unadvanced, not dropped.
    let base = vec![("later".to_string(), cursor(3, 0))];
    assert_eq!(
        fold_gateway_cursors(&base, &engines, &tail[1..2]),
        base,
        "the checkpoint's own cursor is all the restored state is at"
    );
}
