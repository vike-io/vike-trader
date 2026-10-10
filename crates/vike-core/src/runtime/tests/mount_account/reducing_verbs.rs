//! The risk-reducing verbs (mass-cancel, flatten, market-exit) name their account.

use super::*;

// ---------------------------------------------------------------------------------------------
// THE RISK-REDUCING VERBS NAME THEIR ACCOUNT
// (`docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md` §4.4, the
// half of it the submit plane left: owner ruling "B", 2026-09-26)
// ---------------------------------------------------------------------------------------------
//
// `OrderIntent::MassCancel`, `Flatten` and `MarketExit` carry an `account` now. Until they did, a
// `market-exit binance ALT` from the wire was lowered with the account dropped and fanned over
// EVERY account of the exchange — so it cancelled the DEFAULT account's orders and flattened its
// positions too, and the only guard was a client-side capability no node advertised.
//
// Four rows, each asserted on a real two-account core (`ticket_core`), because each is a different
// arm and "somebody guarded one and not its neighbour" is the failure this file keeps finding:
//
// * a HELD account narrows the verb to that account's book, and only it;
// * an UNHELD account is refused by name and touches NOTHING — on the single-account core too,
//   where `route_of`'s `sole_account_of` fallback would otherwise answer with the one book;
// * an account with NO venue is refused, never read as "every engine";
// * NO account keeps today's reach EXACTLY — the venue-wide fan-out and the unscoped panic button.

/// One resting limit on EACH account of [`ticket_core`], each routed by its own payload account —
/// so "each account's own order" is a fact about the core's registries rather than about the test.
/// Returns `(default's coid, ALT's coid)`.
fn rest_one_order_per_account(core: &mut CoreThread<RecordingClient>) -> (String, String) {
    let d =
        core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, default_acct()))), 1);
    let a = core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 1);
    assert_eq!((d.len(), a.len()), (1, 1), "one resting order per account: {d:?} {a:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 1, "the default order is on the default book");
    assert_eq!(core.eng(1).client.submissions.len(), 1, "…and `ALT`'s is on `ALT`'s");
    (d[0].clone(), a[0].clone())
}

/// [`ticket_core`] with one resting order AND one position per account — opposite sides and
/// different sizes, so a leg that lands on the wrong book is a position INCREASE of the wrong
/// quantity rather than a benign no-op.
fn loaded_ticket_core() -> (CoreThread<RecordingClient>, String, String) {
    let mut core = ticket_core();
    let (d, a) = rest_one_order_per_account(&mut core);
    set_position(&mut core.engine, 2.0, 100.0);
    set_position(&mut core.extra_engines[0].1, -3.0, 100.0);
    (core, d, a)
}

/// A sell stop at 90 on [`CANON`]/[`SYMBOL`] — one account's emulated protective stop.
fn protective_stop() -> OrderIntent {
    OrderIntent::ArmConditional(ConditionalIntent {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: -1,
        qty: 1.0,
        price: Some(90.0),
        trail: None,
        trigger_by: None,
    })
}

/// A resting-entry bracket on [`CANON`]/[`SYMBOL`] — its entry goes to the venue, and its two exits
/// are HELD by the core (`held_orders`) until that entry fills.
fn protective_bracket() -> OrderIntent {
    OrderIntent::Bracket(Box::new(vike_model::BracketSpec {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
    }))
}

/// Give EVERY engine of `core` its own core-held protection: one armed stop and one bracket, whose
/// two exits the core holds off the venue. Both are armed through `EngineRoute::Engine`, as a
/// labelled mount arms them, so each arm carries its `cond_engine` row and each held exit resolves to
/// its own engine. That is the attribution the account-scoped clear works from.
///
/// Returns each engine's two held exit coids, in engine order.
fn protect_each_account(core: &mut CoreThread<RecordingClient>) -> Vec<Vec<String>> {
    let engines = core.extra_engines.len() + 1;
    let mut exits = Vec::new();
    for eidx in 0..engines {
        let route = EngineRoute::Engine(eidx);
        core.apply_intent_routed(protective_stop(), 1, CancelIntent::Unspecified, route);
        let legs =
            core.apply_intent_routed(protective_bracket(), 1, CancelIntent::Unspecified, route);
        assert_eq!(legs.len(), 3, "the bracket arm returns [entry, sl, tp]: {legs:?}");
        exits.push(legs[1..].to_vec());
    }
    let p = protection(core);
    assert_eq!(p.armed, (0..engines).collect::<Vec<usize>>(), "one conditional stop per account");
    assert_eq!(p.book_arms.len(), engines, "…each one in the shared book");
    assert_eq!(p.held.len(), 2 * engines, "two held bracket exits per account");
    exits
}

/// The core-held protection on a core. It is compared as a value, so each field is sorted.
#[derive(Debug, PartialEq)]
struct Protection {
    /// The engine each `cond_engine` row says its arm fires onto.
    armed: Vec<usize>,
    /// Every arm id in the shared `conditional_books`. This includes a restored arm, which has no
    /// `cond_engine` row.
    book_arms: Vec<String>,
    /// Every held bracket exit's coid (`held_orders`).
    held: Vec<String>,
}

fn protection(core: &CoreThread<RecordingClient>) -> Protection {
    let mut armed: Vec<usize> = core.cond_engine.values().map(|a| a.engine).collect();
    armed.sort_unstable();
    let mut book_arms: Vec<String> = core
        .conditional_books
        .values()
        .flat_map(|b| b.iter().map(|(id, _)| id.to_string()))
        .collect();
    book_arms.sort();
    let mut held: Vec<String> = core.held_orders.keys().cloned().collect();
    held.sort();
    Protection { armed, book_arms, held }
}

/// Nothing was cancelled and nothing was closed on ANY engine of `core`, and the core-held
/// protection is exactly `before`. This is the "touches nothing" half of every refusal below,
/// written once.
///
/// The protection half is here because cancels and closes cannot see it. A refused reduce that
/// still ran `clear_held_scope` / `clear_conditional_scope` would strip every account's stops and
/// held exits while cancelling and closing nothing. The replay fold's refusal rows
/// (`crate::replay`'s `fold_intent_scope`) promise that clear never happens, and this is the live
/// side of that promise.
fn assert_untouched(core: &CoreThread<RecordingClient>, name: &str, before: &Protection) {
    for e in 0..=core.extra_engines.len() {
        assert!(core.eng(e).client.cancels.is_empty(), "{name}: engine {e} cancelled nothing");
        assert_eq!(
            closes(core.eng(e)),
            Vec::<(i32, f64)>::new(),
            "{name}: engine {e} closed nothing"
        );
    }
    assert_eq!(
        &protection(core),
        before,
        "{name}: every account's armed stops and held bracket exits are exactly what they were — a \
         refused reduce clears no protection"
    );
}

/// ⚠ **THE HEADLINE, per verb: a named account's book, and ONLY that book.** Before this, each of
/// these three frames reached BOTH accounts — the default account's order cancelled and its
/// position flattened by a command that named `ALT`.
#[test]
fn a_labelled_mass_cancel_cancels_only_the_named_account() {
    let (mut core, resting_default, resting_alt) = loaded_ticket_core();

    core.apply_intent(
        OrderIntent::MassCancel { venue: Some(CANON.into()), symbol: None, account: Some(alt()) },
        2,
    );

    assert_eq!(core.eng(1).client.cancels, vec![resting_alt], "`ALT` cancels its own order");
    assert!(
        core.eng(0).client.cancels.is_empty(),
        "the DEFAULT account's order ({resting_default}) is not the named account's to cancel — \
         it was, before this: the account was dropped and the cancel fanned over every account of \
         the exchange"
    );
    assert_eq!(refusal_note(&core), None, "a held account is not a refusal");
}

#[test]
fn a_labelled_flatten_closes_only_the_named_account() {
    let (mut core, _, _) = loaded_ticket_core();

    let coids = core.apply_intent(
        OrderIntent::Flatten { venue: CANON.into(), symbol: SYMBOL.into(), account: Some(alt()) },
        2,
    );

    assert_eq!(coids.len(), 1, "ONE leg, on the named account: {coids:?}");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "`ALT`'s short is bought back at its size");
    assert_eq!(
        closes(core.eng(0)),
        Vec::<(i32, f64)>::new(),
        "…and the default account's long is left alone"
    );
}

#[test]
fn a_labelled_market_exit_reaches_only_the_named_account() {
    let (mut core, _, resting_alt) = loaded_ticket_core();

    core.apply_intent(
        OrderIntent::MarketExit { venue: Some(CANON.into()), account: Some(alt()) },
        2,
    );

    assert_eq!(core.eng(1).client.cancels, vec![resting_alt], "`ALT`'s order is cancelled");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and its position flattened");
    assert!(core.eng(0).client.cancels.is_empty(), "the default account's order stays resting");
    assert_eq!(
        closes(core.eng(0)),
        Vec::<(i32, f64)>::new(),
        "…and its position stays open: `market-exit binance ALT` is a way out of ONE book"
    );
}

/// The wire's spelling of the UNLABELLED account narrows to it — `route_key_of` renders
/// `AccountLabel::Default` as the bare venue id, so a resolver that suffixed the label text would
/// mint `binance#DEFAULT`, match nothing, and refuse the one spelling that names the original book.
#[test]
fn a_reducing_verb_naming_default_reaches_only_the_unlabelled_account() {
    let (mut core, resting_default, _) = loaded_ticket_core();

    core.apply_intent(
        OrderIntent::MarketExit { venue: Some(CANON.into()), account: Some(default_acct()) },
        2,
    );

    assert_eq!(core.eng(0).client.cancels, vec![resting_default]);
    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)], "the default account is flattened");
    assert!(core.eng(1).client.cancels.is_empty(), "`ALT` is untouched");
    assert_eq!(closes(core.eng(1)), Vec::<(i32, f64)>::new());
    assert_eq!(refusal_note(&core), None);
}

/// The three reducing verbs, each naming `account` on [`CANON`].
fn reducers_naming(account: AccountLabel) -> Vec<(&'static str, OrderIntent)> {
    vec![
        (
            "mass_cancel",
            OrderIntent::MassCancel {
                venue: Some(CANON.into()),
                symbol: None,
                account: Some(account.clone()),
            },
        ),
        (
            "flatten",
            OrderIntent::Flatten {
                venue: CANON.into(),
                symbol: SYMBOL.into(),
                account: Some(account.clone()),
            },
        ),
        (
            "market_exit",
            OrderIntent::MarketExit { venue: Some(CANON.into()), account: Some(account) },
        ),
    ]
}

fn nosuch() -> AccountLabel {
    AccountLabel::parse("NOSUCH").expect("a legal label")
}

/// ⚠ **AN UNHELD ACCOUNT IS REFUSED, AND IT TOUCHES NOTHING** — on the two-account core, and on the
/// SINGLE-account core, which is the dangerous one: there the ambiguity gate is silent by
/// construction and `route_of`'s `sole_account_of` fallback answers with the one book the node has,
/// so a verb that merely "fell through" would cancel and flatten it on a client's typo.
///
/// The node's EDGE refuses this frame before the Ack (`vike_tradehub::server::refusal::account_refusal`);
/// what is gated here is the core's own promise to every caller the edge does not stand in front
/// of — it must never MANUFACTURE a reduction out of an account nobody mounted.
///
/// "Touches nothing" includes each account's core-held protection (`protect_each_account`), which
/// is asserted directly. No cancel or close would show it if that protection were stripped.
#[test]
fn a_reducing_verb_naming_an_unheld_account_is_refused_and_touches_nothing() {
    for (name, intent) in reducers_naming(nosuch()) {
        let (mut core, _, _) = loaded_ticket_core();
        protect_each_account(&mut core);
        let before = protection(&core);
        let coids = core.apply_intent(intent, 2);
        assert!(coids.is_empty(), "{name}: nothing minted: {coids:?}");
        assert_untouched(&core, name, &before);
        let note = refusal_note(&core).unwrap_or_else(|| panic!("{name}: the ring must say so"));
        assert!(note.contains("NOSUCH") && note.contains(CANON), "{name}: names both: {note}");
    }
    for (name, intent) in reducers_naming(nosuch()) {
        let mut core = single_account_core();
        let resting = core.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 1);
        assert_eq!(resting.len(), 1, "the one book holds one resting order");
        set_position(&mut core.engine, 2.0, 100.0);
        protect_each_account(&mut core);
        let before = protection(&core);
        let coids = core.apply_intent(intent, 2);
        assert!(coids.is_empty(), "{name}: nothing minted on the single-account core: {coids:?}");
        assert_untouched(&core, name, &before);
        assert!(refusal_note(&core).is_some(), "{name}: …and the ring says so");
    }
}

/// ⚠ **AN ACCOUNT WITH NO VENUE IS REFUSED, NEVER WIDENED.** An account label names one book OF a
/// venue, so `MarketExit { venue: None, account: Some(ALT) }` resolves to nothing — and the arm it
/// would otherwise fall into is the GLOBAL exit, the widest reach any command has. The narrowest
/// request an operator can make must not become the widest action the node can take.
///
/// The global clear matters most here, because it is where the venue-less arm would land.
/// `MassCancel { venue: None, .. }` empties EVERY account's stops and held exits, so each account's
/// protection is asserted unchanged. The replay fold's matching row
/// (`crate::replay`'s `fold_intent_scope`, an account with no venue clears nothing) promises the
/// same thing from the journal side.
#[test]
fn an_account_named_with_no_venue_is_refused_and_never_widened() {
    for (name, intent) in [
        (
            "mass_cancel",
            OrderIntent::MassCancel { venue: None, symbol: None, account: Some(alt()) },
        ),
        ("market_exit", OrderIntent::MarketExit { venue: None, account: Some(alt()) }),
    ] {
        let (mut core, _, _) = loaded_ticket_core();
        protect_each_account(&mut core);
        let before = protection(&core);
        let coids = core.apply_intent(intent, 2);
        assert!(coids.is_empty(), "{name}: nothing minted: {coids:?}");
        assert_untouched(&core, name, &before);
        let note = refusal_note(&core).unwrap_or_else(|| panic!("{name}: the ring must say so"));
        assert!(note.contains("ALT"), "{name}: names the account: {note}");
    }
}

/// ⚠ **NO ACCOUNT KEEPS TODAY'S REACH EXACTLY — asserted explicitly, on the core that just
/// narrowed.** Regressing the panic button is the worst outcome this change could have, so the two
/// account-less shapes are driven AFTER a labelled verb on the same core: the venue-wide exit and
/// the unscoped one both reach every account, and neither is refused.
#[test]
fn an_account_less_reduce_still_reaches_every_account_after_a_labelled_one() {
    let (mut core, _, _) = loaded_ticket_core();
    core.apply_intent(
        OrderIntent::MassCancel { venue: Some(CANON.into()), symbol: None, account: Some(alt()) },
        2,
    );
    assert!(core.eng(0).client.cancels.is_empty(), "the labelled cancel narrowed");

    // The venue-wide exit names no account: both books.
    core.apply_intent(OrderIntent::MarketExit { venue: Some(CANON.into()), account: None }, 3);
    assert_eq!(core.eng(0).client.cancels.len(), 1, "the default order is cancelled now");
    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)], "…and the default account flattened");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and `ALT` flattened");
    assert_eq!(refusal_note(&core), None, "a fan-out is never a refusal");

    // …and the UNSCOPED panic button, on a fresh loaded core: every engine, no argument needed.
    let (mut fresh, d, a) = loaded_ticket_core();
    fresh.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 2);
    assert_eq!(fresh.eng(0).client.cancels, vec![d], "the panic button reaches the default book");
    assert_eq!(fresh.eng(1).client.cancels, vec![a], "…and `ALT`'s");
    assert_eq!(closes(fresh.eng(0)), vec![(-1, 2.0)]);
    assert_eq!(closes(fresh.eng(1)), vec![(1, 3.0)]);
    assert_eq!(refusal_note(&fresh), None, "the panic button may never be refused");
}

/// ⚠ **A NAMED account's mass-cancel leaves the OTHER account's core-held protection alone.** The
/// engines are only half of what a mass-cancel empties: the core also holds each account's
/// emulated conditional stops (`conditional_books`) and bracket exits held off the venue until their
/// entry fills (`held_orders`). Both are keyed by the EXCHANGE, so a venue-wide clear empties both
/// accounts' — which is right for an account-less cancel, and would be a way for `mass-cancel
/// binance ALT` to strip the DEFAULT account's stop-loss while leaving its position open.
///
/// So the clear narrows with the cancel: each arm and each held exit is attributed to the engine it
/// will fire or release onto (`CoreThread::cond_engine` / `coid_venue`), and only the named
/// account's are dropped.
#[test]
fn a_labelled_mass_cancel_clears_only_the_named_accounts_held_exits_and_arms() {
    let mut core = ticket_core();
    let mut alt_exits = Vec::new();
    for eidx in [0, 1] {
        let route = EngineRoute::Engine(eidx);
        core.apply_intent_routed(protective_stop(), 1, CancelIntent::Unspecified, route);
        let legs =
            core.apply_intent_routed(protective_bracket(), 1, CancelIntent::Unspecified, route);
        assert_eq!(legs.len(), 3, "the bracket arm returns [entry, sl, tp]: {legs:?}");
        if eidx == 1 {
            alt_exits = legs[1..].to_vec();
        }
    }
    let mut armed: Vec<usize> = core.cond_engine.values().map(|a| a.engine).collect();
    armed.sort_unstable();
    assert_eq!(armed, vec![0, 1], "one conditional stop per account");
    assert_eq!(core.held_orders.len(), 4, "two held bracket exits per account");

    core.apply_intent(
        OrderIntent::MassCancel { venue: Some(CANON.into()), symbol: None, account: Some(alt()) },
        2,
    );

    assert_eq!(
        core.cond_engine.values().map(|a| a.engine).collect::<Vec<usize>>(),
        vec![0],
        "the DEFAULT account's stop is still armed — only `ALT`'s left the book"
    );
    let book_arms: usize = core.conditional_books.values().map(|b| b.iter().count()).sum();
    assert_eq!(book_arms, 1, "…and the shared book holds exactly that one arm");
    assert_eq!(core.held_orders.len(), 2, "the DEFAULT account's two held exits survive");
    for coid in &alt_exits {
        assert!(!core.held_orders.contains_key(coid), "`ALT`'s held exit {coid} is dropped");
    }
}

/// ⚠ **A labelled MARKET EXIT also leaves the other account's core-held protection alone.** The exit
/// narrows that protection only through its cancel leg. `CoreThread::market_exit_mass_cancel`
/// carries the exit's account onto that leg, and the `MassCancel` arm narrows its held-exit and
/// armed-stop clear from it.
///
/// If the leg were built with no account, `market-exit binance ALT` would still cancel only `ALT`'s
/// engine, because the exit's resolved route names that engine. Its clear would then run
/// venue-wide, stripping the DEFAULT account's stop-loss and held bracket exits while that
/// account's position and bracket entry stayed live. Cancels and closes look the same either way,
/// so this test asserts the protection itself.
#[test]
fn a_labelled_market_exit_leaves_the_other_accounts_held_exits_and_arms() {
    let (mut core, _, resting_alt) = loaded_ticket_core();
    let exits = protect_each_account(&mut core);

    core.apply_intent(
        OrderIntent::MarketExit { venue: Some(CANON.into()), account: Some(alt()) },
        2,
    );

    let after = protection(&core);
    assert_eq!(
        after.armed,
        vec![0],
        "the DEFAULT account's stop is still armed — only `ALT`'s left"
    );
    assert_eq!(after.book_arms.len(), 1, "…and the shared book holds exactly that one arm");
    let mut default_exits = exits[0].clone();
    default_exits.sort();
    assert_eq!(
        after.held, default_exits,
        "the DEFAULT account's two held exits survive, and `ALT`'s are dropped"
    );
    // …and the exit still did its whole job on `ALT`, and nothing on the default account.
    assert!(
        core.eng(1).client.cancels.contains(&resting_alt),
        "`ALT`'s resting order is cancelled"
    );
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and its position flattened");
    assert!(
        core.eng(0).client.cancels.is_empty(),
        "the default account's resting order and bracket entry stay live"
    );
    assert_eq!(closes(core.eng(0)), Vec::<(i32, f64)>::new(), "…and its position stays open");
    assert_eq!(refusal_note(&core), None, "a held account is not a refusal");
}

/// A stop armed before a restart, re-seeded the way `CoreConfig::conditionals` does it. It keeps
/// its `(venue, symbol)` terms and gets no account: `SnapConditional` carries no route key, which is
/// that field's declared residual.
fn restored_stop(arm_id: &str) -> vike_journal::SnapConditional {
    vike_journal::SnapConditional {
        arm_id: arm_id.to_string(),
        terms: vike_journal::ConditionalRecord {
            venue: CANON.into(),
            symbol: SYMBOL.into(),
            side: -1,
            qty: 1.0,
            price: Some(90.0),
            trail: None,
            extreme: None,
            trigger_by: None,
        },
    }
}

/// [`ticket_core`] after a restart, with one restored stop (`r0`) in the shared book.
fn restored_ticket_core() -> CoreThread<RecordingClient> {
    core_of(
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0))],
        CoreConfig {
            seed_cash: 1_000.0,
            conditionals: vec![restored_stop("r0")],
            ..CoreConfig::default()
        },
    )
}

/// ⚠ **A RESTORED arm on a two-account venue belongs to no account a labelled cancel can name, so a
/// labelled cancel leaves it, whichever account it names.**
///
/// The account-scoped clear decides each arm by the engine its FIRE would reach
/// (`CoreThread::armed_engine`). A restored arm has no `cond_engine` row, so its fire takes the
/// payload route with no account. On a venue with two accounts, the Submit arm's ambiguity gate
/// refuses that, so the fire reaches no engine at all. The first half of this test asserts that
/// premise rather than assuming it.
///
/// Before this, the clear answered with the venue's DEFAULT engine, which the fire never reaches.
/// `mass-cancel binance DEFAULT` disarmed an arm that may be `ALT`'s, and `mass-cancel binance ALT`
/// kept it for the same wrong reason. An arm no account can be shown to own is not in any named
/// account's scope, so only the account-less, venue-wide clear takes it. That clear is unchanged,
/// and the last block asserts it.
#[test]
fn a_labelled_mass_cancel_leaves_a_restored_arm_it_cannot_attribute_to_the_named_account() {
    let mut fired = restored_ticket_core();
    assert!(fired.cond_engine.is_empty(), "a restored arm carries no account row");
    assert_eq!(protection(&fired).book_arms, vec!["r0".to_string()], "…but it IS armed");
    fired.fire_conditionals_bar(CANON, SYMBOL, &crashing_bar());
    assert_eq!(submitted_anywhere(&fired), 0, "its fire reached no book");
    let note = refusal_note(&fired).expect("…and the ring says why");
    assert!(note.contains("submit REFUSED") && note.contains("binance#ALT"), "{note}");

    for account in [alt(), default_acct()] {
        let mut core = restored_ticket_core();
        core.apply_intent(
            OrderIntent::MassCancel {
                venue: Some(CANON.into()),
                symbol: None,
                account: Some(account.clone()),
            },
            2,
        );
        assert_eq!(
            protection(&core).book_arms,
            vec!["r0".to_string()],
            "`{account}`: a labelled cancel clears only what its fire would put in the named \
             account's book, and this arm's fire reaches no account — attributing it to the venue's \
             DEFAULT engine is a guess, and under `DEFAULT` it disarmed an arm that may be `ALT`'s"
        );
        assert_eq!(refusal_note(&core), None, "`{account}`: a held account is not a refusal");
    }

    // The account-less cancel still takes it: the venue-wide clear is unchanged.
    let mut core = restored_ticket_core();
    core.apply_intent(
        OrderIntent::MassCancel { venue: Some(CANON.into()), symbol: None, account: None },
        2,
    );
    assert!(
        protection(&core).book_arms.is_empty(),
        "an account-less cancel clears the venue's arms, a restored one included"
    );
}
