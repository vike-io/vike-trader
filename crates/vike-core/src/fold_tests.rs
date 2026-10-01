//! White-box tests of [`fold_conditionals`] — the pure membership fold behind
//! re-arm-on-restore. The end-to-end restore is gated in `tests/conditional_journal.rs`;
//! these pin the per-record semantics (add / remove / mass-cancel scoping) in isolation.

use super::*;
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
        },
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

fn alt_account() -> Option<vike_model::account_keys::AccountLabel> {
    Some(vike_model::account_keys::AccountLabel::Named("ALT".into()))
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
