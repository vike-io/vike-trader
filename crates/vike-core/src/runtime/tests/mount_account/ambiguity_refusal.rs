//! STAGE 1: an external command naming a venue of several accounts is refused, not misrouted.

use super::*;

// ---------------------------------------------------------------------------------------------
// STAGE 1 — THE NODE REFUSES ON AMBIGUITY
// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md`)
// ---------------------------------------------------------------------------------------------
//
// §4.2's table, ROW 1: an EXTERNAL command (a DOM click, a tradehub ticket, a CLI verb — every one
// of them `EngineRoute::Payload`, none of them able to name an account) that names a venue this
// process runs SEVERAL accounts of is REFUSED rather than routed to that venue's default book.
//
// §4.5's law decides WHICH verbs:
//
//   > A risk-REDUCING venue verb that names no account fans out to EVERY account of that venue. A
//   > risk-INCREASING one refuses.
//
// Both halves are gated below, on one harness, because the halves are only meaningful against each
// other: a build that refused the reducing verbs would take out the panic button (decision 0041's
// verdict 4), and one that fanned out the increasing verbs would place N orders in N books the
// sender never named — strictly worse than the single misroute Stage 1 exists to close.

/// A two-account core with NO strategy mount at all — the shape an operator ticket arrives at. The
/// mounts in [`spread_core`] would let a test accidentally route by `EngineRoute::Mount` and prove
/// nothing about the external path.
pub(crate) fn ticket_core() -> CoreThread<RecordingClient> {
    core_of(
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0))],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    )
}

/// …and its SINGLE-account twin, identical in every way the assertions below can see.
pub(crate) fn single_account_core() -> CoreThread<RecordingClient> {
    core_of(
        engine(CANON, 1_000.0),
        Vec::new(),
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    )
}

pub(crate) fn open_order(venue: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: String::new(),
        venue: venue.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}

/// Every order that reached a venue client, across every engine of the core.
pub(crate) fn submitted_anywhere(core: &CoreThread<RecordingClient>) -> usize {
    (0..=core.extra_engines.len()).map(|i| core.eng(i).client.submissions.len()).sum()
}

pub(crate) fn refusal_note(core: &CoreThread<RecordingClient>) -> Option<String> {
    core.recent.iter().find(|l| l.contains("REFUSED")).map(|l| l.to_string())
}

/// ⚠ **THE HEADLINE REFUSAL.** An account-less submit naming a venue with two accounts reaches
/// NEITHER book, mints no coid, and says which two books it could have meant.
#[test]
fn an_account_less_submit_to_a_two_account_venue_is_refused_by_name() {
    let mut core = ticket_core();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 1);

    assert!(coids.is_empty(), "nothing was minted: {coids:?}");
    assert_eq!(submitted_anywhere(&core), 0, "and nothing reached EITHER venue client");

    let note = refusal_note(&core).expect("the recent-events ring must say so");
    assert!(note.contains("binance"), "names the venue: {note}");
    assert!(note.contains("binance#ALT"), "names the OTHER candidate: {note}");
    assert!(note.contains("2 accounts"), "…and how many there are: {note}");
    assert!(note.contains("Nothing was sent"), "{note}");
}

/// **A SINGLE-account node is unchanged** — the claim that makes Stage 1 safe to ship. The same
/// intent, the same clock, the same engine: a minted coid and one submission, and nothing refused.
#[test]
fn a_single_account_node_routes_the_same_submit_exactly_as_before() {
    let mut core = single_account_core();
    assert!(!core.multi_account, "one account: every Stage-1 branch stays inert");
    assert_eq!(core.ambiguous_accounts(EngineRoute::Payload, CANON), None);

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 1);
    assert_eq!(coids.len(), 1, "one coid minted, as always");
    assert_eq!(core.eng(0).client.submissions.len(), 1, "…and one order at the venue");
    assert_eq!(refusal_note(&core), None, "nothing was refused");
}

/// …and an UNKNOWN venue keeps `unwrap_or(0)`'s historical answer rather than becoming §4.2's
/// `N = 0` refusal. Declared rather than defaulted: refusing there would change SINGLE-account
/// behaviour (every paper/sim engine behind a non-roster id), which is the one thing this stage
/// may not do — see `CoreThread::ambiguous_accounts`' own doc.
#[test]
fn an_unknown_venue_still_routes_to_engine_zero() {
    let mut core = single_account_core();
    assert_eq!(core.ambiguous_accounts(EngineRoute::Payload, "no-such-venue"), None);
    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order("no-such-venue"))), 1);
    assert_eq!(coids.len(), 1, "minted, not refused");
    assert_eq!(core.eng(0).client.submissions.len(), 1);
}

/// **The ambiguity is per VENUE, never per NODE** — §4.2: *"a box running forty-nine binance
/// accounts and one okx account keeps serving every account-less okx command unchanged, and refuses
/// only the binance ones."*
#[test]
fn a_single_account_venue_on_a_multi_account_node_is_untouched() {
    let okx = {
        let mut e = ExecutionEngine::new(
            Account::new(3_000.0, "okx", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "okx",
            SYMBOL,
        );
        e.route_key = "okx".to_string();
        e
    };
    let mut core = core_of(
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0)), (3_000.0, okx)],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    );
    assert!(core.multi_account);
    assert!(core.ambiguous_accounts(EngineRoute::Payload, CANON).is_some(), "binance IS ambiguous");
    assert_eq!(core.ambiguous_accounts(EngineRoute::Payload, "okx"), None, "okx is NOT");

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order("okx"))), 1);
    assert_eq!(coids.len(), 1, "the okx ticket is served exactly as before");
    assert_eq!(core.eng(2).client.submissions.len(), 1);
    assert_eq!(core.eng(0).client.submissions.len(), 0, "…and neither binance book was touched");
    assert_eq!(core.eng(1).client.submissions.len(), 0);
}

/// **§4.2 row 1 reaches the BATCH verb, through a fast path that would otherwise have skipped it.**
///
/// `SubmitBatch`'s `all_primary` test asks whether every leg resolves to engine 0. An account-less
/// leg on a two-account venue SATISFIES that question — `route_of` answers with that venue's
/// DEFAULT engine, which IS engine 0 here — so without the ambiguity clause beside it the whole
/// batch took the single-call path onto engine 0 and never re-entered the arm where the refusal
/// lives. The guard is therefore load-bearing rather than defensive, and this is what pins it.
///
/// The batch is deliberately MIXED, because that is §4.2's per-VENUE scoping applied INSIDE one
/// command: the ambiguous leg is refused BY NAME while the okx leg is submitted exactly as before.
#[test]
fn an_ambiguous_batch_leg_is_refused_while_its_unambiguous_sibling_is_submitted() {
    let okx = {
        let mut e = ExecutionEngine::new(
            Account::new(3_000.0, "okx", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "okx",
            SYMBOL,
        );
        e.route_key = "okx".to_string();
        e
    };
    let mut core = core_of(
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0)), (3_000.0, okx)],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    );

    let coids =
        core.apply_intent(OrderIntent::SubmitBatch(vec![open_order(CANON), open_order("okx")]), 1);

    assert_eq!(coids.len(), 1, "only the okx leg minted a coid: {coids:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 0, "the default binance book is untouched");
    assert_eq!(core.eng(1).client.submissions.len(), 0, "…and so is `ALT`");
    assert_eq!(core.eng(2).client.submissions.len(), 1, "the okx leg is served exactly as before");
    let note = refusal_note(&core).expect("the ring must say the binance leg was refused");
    assert!(note.contains("binance#ALT"), "…and name both candidates: {note}");
}

/// A BRACKET and a CONDITIONAL ARM are the same risk-INCREASING side and refuse the same way —
/// asserted together because the failure mode is "somebody guarded one arm and not its neighbour".
#[test]
fn every_risk_increasing_arm_refuses_and_mints_nothing() {
    let bracket = OrderIntent::Bracket(Box::new(vike_model::BracketSpec {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
    }));
    let arm = OrderIntent::ArmConditional(ConditionalIntent {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: -1,
        qty: 1.0,
        price: Some(90.0),
        trail: None,
        trigger_by: None,
    });
    // ⚠ A COMBO's refusal is asserted with a spec that would otherwise be REJECTED anyway
    // (`binance` carries no `supports_combo` row), which is precisely what makes it worth
    // asserting: the ambiguity guard sits AHEAD of both `validate` and the capability arm, so a
    // combo that names no account refuses for the RIGHT reason and mints nothing rather than
    // reaching the synthesized-terminal path on a guessed engine.
    let combo = OrderIntent::Combo(Box::new(vike_model::ComboSpec {
        venue: CANON.into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            vike_model::ComboLeg { symbol: SYMBOL.into(), ratio: 1 },
            vike_model::ComboLeg { symbol: "ETHUSDT".into(), ratio: -1 },
        ],
        net_limit: Some(1.0),
        time_in_force: vike_model::TimeInForce::default(),
    }));
    for (label, intent) in [("bracket", bracket), ("ArmConditional", arm), ("combo", combo)] {
        let mut core = ticket_core();
        let coids = core.apply_intent(intent, 1);
        assert!(coids.is_empty(), "{label}: nothing minted, got {coids:?}");
        assert_eq!(submitted_anywhere(&core), 0, "{label}: nothing reached a venue client");
        let note = refusal_note(&core)
            .unwrap_or_else(|| panic!("{label}: the ring must say so: {:?}", core.recent));
        assert!(note.contains("binance#ALT"), "{label} names both candidates: {note}");
    }
}

/// ⚠ **THE REDUCING SIDE, and the one that would have made this a trading halt.** §4.5:
///
///   > A risk-REDUCING venue verb that names no account fans out to EVERY account of that venue.
///
/// A standalone `Flatten` names a venue and a symbol and mints a `reduce_only` MARKET — it can only
/// CLOSE. It used to resolve ONE engine (`route_of(..).unwrap_or(0)`, the venue's default account),
/// so on this core "flatten my binance BTC" closed one book and left the other open — while
/// `MarketExit` on the same venue, which is this verb plus a mass-cancel, already fanned out.
#[test]
fn an_account_less_flatten_reaches_every_account_of_the_venue() {
    let mut core = ticket_core();
    set_position(&mut core.engine, 2.0, 100.0);
    set_position(&mut core.extra_engines[0].1, -3.0, 100.0);

    let coids = core.apply_intent(
        OrderIntent::Flatten { venue: CANON.into(), symbol: SYMBOL.into(), account: None },
        2,
    );

    assert_eq!(coids.len(), 2, "one leg per account: {coids:?}");
    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)], "the default account is flattened once");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and `ALT` once, on ITS OWN side and size");
    assert_eq!(refusal_note(&core), None, "a reducing verb is never refused for ambiguity");
}

/// …and the same verb on a SINGLE-account core is what it always was: one leg, on the one engine,
/// closing exactly the position it read.
#[test]
fn a_single_account_flatten_is_the_one_leg_it_always_was() {
    let mut core = single_account_core();
    set_position(&mut core.engine, 2.0, 100.0);

    let coids = core.apply_intent(
        OrderIntent::Flatten { venue: CANON.into(), symbol: SYMBOL.into(), account: None },
        2,
    );

    assert_eq!(coids.len(), 1);
    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)]);
}

/// A venue-scoped MARKET-EXIT is the other reducing verb, and it must stay fanned out rather than
/// be swept into the refusal — the guard against a future "refuse whenever ambiguous" applied
/// uniformly, which is decision 0041's verdict 4 by name.
#[test]
fn the_panic_button_is_not_refused() {
    let mut core = ticket_core();
    set_position(&mut core.engine, 2.0, 100.0);
    set_position(&mut core.extra_engines[0].1, -3.0, 100.0);

    core.apply_intent(OrderIntent::MarketExit { venue: Some(CANON.into()), account: None }, 2);

    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)], "the default account is flattened");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and so is `ALT`");
    assert_eq!(refusal_note(&core), None, "the panic button may never be refused for ambiguity");
}

/// **§9 item 2 — the FOREIGN-VENUE FALLTHROUGH.** A labelled mount's cross-venue leg defers to the
/// payload (correctly: *"the mount's account is a fact about its OWN venue"*), and that deference
/// used to land on the foreign venue's DEFAULT account. The guard covers it by construction —
/// `routed_engine` answering `None` IS the fallthrough — so the mount's OWN venue stays routable
/// while the external path on the same venue refuses.
#[test]
fn a_mount_route_that_names_its_account_is_never_ambiguous() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let core = spread_core(&seen);
    assert_eq!(core.route_of(EngineRoute::Mount(1), "okx"), None, "the deference is unchanged");
    assert_eq!(
        core.ambiguous_accounts(EngineRoute::Mount(1), CANON),
        None,
        "the mount named its account, so its own venue is not ambiguous for it"
    );
    assert!(
        core.ambiguous_accounts(EngineRoute::Payload, CANON).is_some(),
        "…while the external path, which names nothing, IS"
    );
}

/// **The refusal names the accounts in ENGINE order**, so the first one is the account the command
/// used to reach — which is what an operator needs in order to tell whether they were relying on
/// the old behaviour.
#[test]
fn the_refusal_lists_the_default_account_first() {
    let core = ticket_core();
    let candidates =
        core.ambiguous_accounts(EngineRoute::Payload, CANON).expect("two accounts is ambiguous");
    assert_eq!(candidates, vec!["binance".to_string(), "binance#ALT".to_string()]);
}

/// `accounts_of_venue` is `engines_of_venue(..).len()`, pinned — the allocation-free twin exists
/// only because `route_event`'s last rung asks the question on the fold, and a second answer would
/// be a second truth.
#[test]
fn the_account_count_is_the_engine_set_it_claims_to_be() {
    let core = ticket_core();
    for venue in [CANON, "binance#ALT", "okx", "no-such-venue"] {
        assert_eq!(core.accounts_of_venue(venue), core.engines_of_venue(venue).len(), "{venue}");
    }
    let single = single_account_core();
    for venue in [CANON, "okx"] {
        assert_eq!(
            single.accounts_of_venue(venue),
            single.engines_of_venue(venue).len(),
            "{venue}"
        );
    }
}

/// **The journal records the RESOLVED account** (§9 item 12) — and records NOTHING for a default
/// account, so a single-account box's journal bytes do not move.
#[test]
fn the_write_ahead_route_key_is_absent_for_a_default_account() {
    let core = single_account_core();
    assert_eq!(core.journal_route_key(0, CANON), None, "the venue's sole account names itself");

    let two = ticket_core();
    assert_eq!(two.journal_route_key(0, CANON), None, "…and so does the DEFAULT account of two");
    assert_eq!(
        two.journal_route_key(1, CANON),
        Some("binance#ALT".to_string()),
        "a labelled account is what the record has to carry"
    );
}

/// **AN ACCOUNT-LESS MOUNT REFUSES A VENUE THIS CORE RUNS TWICE** — the hole `sole_account_of`
/// names but cannot close on its own.
///
/// That spelling ASSERTS *"this venue has one account in this process"*. On a box that arms a
/// LABELLED account row the assertion is false, and the lookup nonetheless SUCCEEDS — the default
/// account's route key IS the bare venue — so the mount lands on the default account and looks
/// correct. A strategy executing on an account its author did not choose is the account arm's
/// catastrophe wearing an ABSENCE instead of a label.
///
/// ⚠ **Mutation proof (production code, not the harness):** delete the `carriers > 1` block from
/// `mount_engine_resolution`'s `None` arm and this test goes green again while the mount silently
/// resolves engine `binance` — which is exactly the state before the change.
#[test]
fn an_account_less_mount_is_refused_when_the_venue_has_two_engines() {
    let mounted = ["binance", "binance#ALT", "okx"];
    let reason = mount_engine_resolution(&mounted, "binance", None)
        .expect_err("two engines of one venue cannot be addressed by the venue alone");
    assert!(reason.contains("2 engines"), "the refusal must COUNT them: {reason}");
    assert!(reason.contains("binance#ALT"), "…and name the spellings it answers to: {reason}");
    assert!(
        reason.contains("account row"),
        "…and say where an account is armed, which is what the operator does next: {reason}"
    );
}

/// ⚠ **The complement, and it is what keeps the refusal off every single-account box** — which is
/// every box that arms no labelled account row, i.e. nearly all of them. Without this the test
/// above is satisfied by a `None` arm that refuses unconditionally.
#[test]
fn an_account_less_mount_still_resolves_a_venue_with_one_engine() {
    let mounted = ["binance", "okx"];
    assert_eq!(
        mount_engine_resolution(&mounted, "binance", None).expect("one engine is unambiguous"),
        0
    );
    assert_eq!(
        mount_engine_resolution(&mounted, "okx", None).expect("one engine is unambiguous"),
        1
    );
}

/// Naming the ACCOUNT resolves what the venue alone could not — the refusal above tells the
/// operator to do this, so it has to work.
#[test]
fn naming_the_account_resolves_a_venue_with_two_engines() {
    let mounted = ["binance", "binance#ALT"];
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a valid label");
    assert_eq!(
        mount_engine_resolution(&mounted, "binance", Some(&alt)).expect("the label addresses it"),
        1
    );
}

/// ⚠ **NAMING `DEFAULT` IS NOT THE SAME AS NAMING NOTHING, AND ON A TWO-ENGINE VENUE IT ROUTES.**
///
/// The design's sender table keeps these on separate rows — an ABSENT account is the sender naming
/// none, ambiguous at `N ≥ 2`; `DEFAULT` is the sender naming the unlabelled account the venue
/// already had, which is ambiguous at no engine count at all. This test exists because collapsing
/// the two is the natural way to write the refusal above (`account.filter(|l| !l.is_default())` is
/// already how `DEFAULT` reaches that arm to share its resolution) and it was how this change was
/// FIRST written — at which point the default account had no spelling that mounted it here: absence
/// refused, and `DEFAULT` folded into the same refusal, so the message told the operator to name an
/// account while refusing the name for the one they wanted. The eleven two-engine fixtures above
/// went red together and that is what they were saying.
#[test]
fn naming_default_resolves_the_unlabelled_engine_of_a_two_engine_venue() {
    let mounted = ["binance", "binance#ALT"];
    let default = vike_model::accounts::account_keys::AccountLabel::Default;
    assert_eq!(
        mount_engine_resolution(&mounted, "binance", Some(&default))
            .expect("`DEFAULT` names the unlabelled account and is never ambiguous"),
        0,
        "the unlabelled account's route key IS the bare venue, so it is engine zero here"
    );
    // …and the refusal is still armed for the row that IS ambiguous, on the same mounted set — so
    // this test cannot be passed by a `None` arm that simply stopped refusing.
    assert!(
        mount_engine_resolution(&mounted, "binance", None).is_err(),
        "absence must still refuse: it is a different row, not a different spelling of this one"
    );
}
