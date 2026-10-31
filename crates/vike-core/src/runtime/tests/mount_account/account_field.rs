//! The payload names its account: `OrderRequest::account`, and the routing that reads it.

use super::*;

// ---------------------------------------------------------------------------------------------
// **THE PAYLOAD NAMES ITS ACCOUNT** — `OrderRequest::account`, and the routing that reads it.
//
// The four behaviours below are the whole of the field's contract at this seam. Two of them are
// NEW and are gated here; the other two are the no-regression anchors, and they are deliberately
// NOT re-written under new names — `an_account_less_submit_to_a_two_account_venue_is_refused_by_name`
// and `a_single_account_node_routes_the_same_submit_exactly_as_before` already pin them, ON THIS
// HARNESS, over an `open_order` whose `account` is `None`. A second copy under a second name is
// the "second spelling free to disagree" this tree keeps paying for, and it would be a copy that
// could be made green by editing it rather than the code.
//
// | behaviour | where it is gated |
// |---|---|
// | a labelled order reaches the named engine and ONLY it | `a_labelled_order_reaches_the_named_engine_and_only_it`, below |
// | an account-LESS order on an ambiguous venue still refuses | `an_account_less_submit_to_a_two_account_venue_is_refused_by_name` |
// | an order naming an account this node does not hold is NOT silently routed | `a_payload_naming_an_unheld_account_is_not_silently_routed_to_the_sole_account`, below |
// | `None` resolves exactly as it did | `a_single_account_node_routes_the_same_submit_exactly_as_before` |

/// [`open_order`] naming an ACCOUNT of its venue.
pub(crate) fn open_order_for(venue: &str, account: AccountLabel) -> vike_model::OrderRequest {
    vike_model::OrderRequest { account: Some(account), ..open_order(venue) }
}

/// ⚠ **THE HEADLINE.** A labelled order on a TWO-account venue reaches the account it names, and
/// only that one — where before this field was read it reached NEITHER, because
/// `ambiguous_accounts` refused it before routing was consulted at all: `EngineRoute::Payload`
/// answered `None` from `routed_engine` unconditionally, whatever the payload said.
///
/// The `ONLY it` half is not decoration. A fan-out would place the order in both books, which
/// §4.5 names as strictly worse than the single misroute this whole stage exists to close.
#[test]
fn a_labelled_order_reaches_the_named_engine_and_only_it() {
    let mut core = ticket_core();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 1);

    assert_eq!(coids.len(), 1, "a named account is not ambiguous, so a coid is minted: {coids:?}");
    assert_eq!(refusal_note(&core), None, "…and nothing was refused");
    assert_eq!(
        core.eng(1).client.submissions.len(),
        1,
        "the order reached `binance#ALT`, the account it named"
    );
    assert_eq!(
        core.eng(0).client.submissions.len(),
        0,
        "…and NOT the venue's default account, which is where it used to go before Stage 1 \
         refused it outright"
    );
    assert_eq!(submitted_anywhere(&core), 1, "exactly one book, never a fan-out");
}

/// **The wire's spelling of the unlabelled account routes to it** — `AccountLabel::Default` on a
/// venue this process runs TWICE resolves engine 0 rather than refusing.
///
/// A separate test from the one above because it is a separate arm: `route_key_of` renders the
/// default account as the BARE VENUE ID, so a resolver that reached for the label text and
/// suffixed it unconditionally would mint `binance#DEFAULT`, match no engine, and turn the one
/// spelling that addresses the original account into a refusal. That is exactly the failure
/// `naming_default_resolves_the_unlabelled_engine_of_a_two_engine_venue` records on the MOUNT
/// side; this is its payload-side twin.
#[test]
fn a_payload_naming_default_reaches_the_unlabelled_account_of_a_two_account_venue() {
    let mut core = ticket_core();
    let order = open_order_for(CANON, default_acct());
    let coids = core.apply_intent(OrderIntent::Submit(Box::new(order)), 1);

    assert_eq!(coids.len(), 1, "`DEFAULT` names a book, so it is never ambiguous: {coids:?}");
    assert_eq!(refusal_note(&core), None);
    assert_eq!(core.eng(0).client.submissions.len(), 1, "the unlabelled engine got it");
    assert_eq!(core.eng(1).client.submissions.len(), 0, "…and `ALT` did not");
}

/// ⚠ **RULED: an order naming an account this process does not hold is NOT routed to the venue's
/// sole account.** Asserted on a SINGLE-account core, where `multi_account` is false and the
/// ambiguity gate is silent by construction — so nothing but this rule stands between the payload
/// and `route_of`'s `sole_account_of` fallback, which would answer `Some(0)` and trade the one
/// book the node has on a client's typo. It is the configuration where the wrong answer is a
/// well-formed one and nobody would notice.
///
/// ⚠ **This is DEFENCE IN DEPTH, not the operator-facing refusal.** The node's EDGE is what tells
/// a client `refused: binance has no account NOSUCH`, before the Ack, and it renders the accounts
/// the node does hold; that belongs to the daemon's wire handler and is not this seam's job. What
/// is gated HERE is the narrower claim the core owes every caller it has — including the ones the
/// edge cannot cover: the core must not be the thing that MANUFACTURES a misroute out of an
/// account nobody mounted. So the assertion is about what does not happen.
#[test]
fn a_payload_naming_an_unheld_account_is_not_silently_routed_to_the_sole_account() {
    let mut core = single_account_core();
    assert!(!core.multi_account, "the single-account core: the ambiguity gate cannot fire here");
    assert_eq!(
        core.ambiguous_accounts(EngineRoute::Payload, CANON),
        None,
        "…which is what makes this test about the FALLBACK and not about that gate"
    );

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 1);

    assert_eq!(
        core.eng(0).client.submissions.len(),
        0,
        "the one book this node has is NOT where an order naming `ALT` goes"
    );
    assert_eq!(submitted_anywhere(&core), 0, "nothing reached any venue client");
    assert!(coids.is_empty(), "nothing was minted either — refused before the mint: {coids:?}");
}

/// …and the unheld-account refusal is SAID, on the surface an operator reads. The core's two
/// surfaces are `tracing::error!` and the recent-events ring; this checks the ring, which is what
/// the GUI, the Telegram channel and `vike-cli` render.
///
/// ⚠ It names the venue and the account and stops there. The list of accounts this node DOES hold
/// is the EDGE's rendering, deliberately not duplicated into the core — see
/// `vike_core::account_ambiguity::unheld_account_refusal`.
#[test]
fn the_unheld_account_refusal_names_the_venue_and_the_account() {
    let mut core = single_account_core();
    let _ = core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 1);

    let note = refusal_note(&core).expect("the recent-events ring must say so");
    assert!(note.contains("ALT"), "names the account that was asked for: {note}");
    assert!(note.contains(CANON), "names the venue: {note}");
    assert!(note.contains("Nothing was sent"), "{note}");
}

/// **A labelled order does not disturb the venue-scoped ambiguity gate.** The constraint is that
/// `ambiguous_accounts` neither widens nor narrows for the inputs it already sees: an account-LESS
/// order on the same two-account venue is still its business, and still refuses, on the very core
/// that just routed a labelled one.
///
/// Both halves on ONE core because "unchanged" is a claim about the gate, not about a fixture: a
/// resolver that quietly disarmed the gate would pass the headline test above and fail here.
#[test]
fn resolving_a_labelled_order_leaves_the_account_less_refusal_armed() {
    let mut core = ticket_core();

    let labelled =
        core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 1);
    assert_eq!(labelled.len(), 1, "the labelled order routed");

    let bare = core.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 2);
    assert!(bare.is_empty(), "…and the account-LESS one is still refused: {bare:?}");
    assert!(
        refusal_note(&core).is_some_and(|n| n.contains("2 accounts")),
        "by the EXISTING path, with the EXISTING message"
    );
    assert_eq!(submitted_anywhere(&core), 1, "only the labelled order ever reached a book");
    assert_eq!(
        core.ambiguous_accounts(EngineRoute::Payload, CANON),
        Some(vec!["binance".to_string(), "binance#ALT".to_string()]),
        "the gate's own answer for an account-less payload is byte-identical"
    );
}

/// ⚠ **A LABELLED LEG OF A `SubmitBatch` DOES NOT BYPASS THE SINGLE-SUBMIT ARM'S REFUSAL** — the
/// sibling-intent twin of
/// `a_payload_naming_an_unheld_account_is_not_silently_routed_to_the_sole_account`.
///
/// `all_primary` decides whether the batch takes the ONE-CALL fast path or re-enters the
/// single-submit arm leg by leg, and it used to ask only about `r.venue`. On a SINGLE-account core
/// every clause was satisfied by a leg naming an account that does not exist — the ambiguity gate
/// is silent (a one-account venue has nothing to be ambiguous about) and `route_of` answers
/// `Some(0)` through `sole_account_of` — so the leg was minted, journaled under engine 0's key and
/// submitted into engine 0's book, which is the exact outcome the `Submit` arm refuses.
///
/// That is the shape this repository keeps paying for: a rule stated in ONE of two sibling paths.
/// The arm's first clause already argues it for an AMBIGUOUS leg in its own words; this is the
/// same argument one row down the sender table, and it is gated rather than argued.
///
/// ⚠ **The refusal note is also the proof that the leg left the fast path**, and is the only such
/// proof available: `RecordingClient` inherits `submit_batch`'s fan-out default, so the two paths
/// are indistinguishable at the client. It does not need a counter — `refuse_unheld_account` is
/// reachable from the single-submit arm and from nowhere else, so a note naming `ALT` cannot have
/// been written by the batched path.
#[test]
fn a_labelled_batch_leg_does_not_bypass_the_unheld_account_refusal() {
    let mut core = single_account_core();
    assert!(!core.multi_account, "the dangerous core: the ambiguity gate cannot fire here");

    let coids = core.apply_intent(
        OrderIntent::SubmitBatch(vec![open_order_for(CANON, alt()), open_order(CANON)]),
        1,
    );

    assert_eq!(
        core.eng(0).client.submissions.len(),
        1,
        "the account-LESS leg is submitted exactly as it always was, and the leg naming `ALT` is \
         not — a batch does not get a second, quieter door onto the one book this node has"
    );
    assert_eq!(
        core.eng(0).client.submissions[0].account,
        None,
        "…and the one that landed is the account-less leg, not the labelled one"
    );
    assert_eq!(coids.len(), 1, "one leg minted, one refused before its mint: {coids:?}");
    assert!(
        refusal_note(&core).is_some_and(|n| n.contains("ALT")),
        "refused BY NAME, through the single-submit arm rather than around it — and that note is \
         itself the evidence the leg re-entered that arm"
    );
}

/// **An account-LESS `SubmitBatch` is byte-identical**, which is what makes the clause above safe
/// to add: `r.account` is `None` for every leg any caller builds today, so the new test always
/// passes and `all_primary` is the predicate it was.
///
/// Asserted as the whole observable contract rather than as "it took the fast path" — the two
/// paths are indistinguishable at `RecordingClient` (see the test above), so what is pinned is
/// what a caller can see: every leg minted, every leg at the venue, nothing refused.
#[test]
fn an_account_less_batch_is_unchanged() {
    let mut core = single_account_core();
    let coids =
        core.apply_intent(OrderIntent::SubmitBatch(vec![open_order(CANON), open_order(CANON)]), 1);

    assert_eq!(coids.len(), 2, "both legs minted: {coids:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 2, "…and both reached the venue");
    assert_eq!(refusal_note(&core), None, "nothing refused");
}

/// **A labelled leg naming an account this core DOES hold is routed by the batch path too**, so
/// the clause above is a re-route and not a blanket batch refusal. On the two-account core the
/// `ALT` leg reaches `binance#ALT` and the account-less leg is refused as ambiguous — each leg
/// answered on its own terms, in one intent.
#[test]
fn a_batch_routes_each_leg_on_its_own_account() {
    let mut core = ticket_core();
    let coids = core.apply_intent(
        OrderIntent::SubmitBatch(vec![open_order_for(CANON, alt()), open_order(CANON)]),
        1,
    );

    assert_eq!(coids.len(), 1, "the labelled leg minted; the account-less one refused: {coids:?}");
    assert_eq!(core.eng(1).client.submissions.len(), 1, "the `ALT` leg reached `ALT`'s book");
    assert_eq!(core.eng(0).client.submissions.len(), 0, "and NOT the default account's");
    assert!(
        refusal_note(&core).is_some_and(|n| n.contains("2 accounts")),
        "the account-LESS leg is still refused by the EXISTING ambiguity path"
    );
}

/// ⚠ **RECORDED BEHAVIOUR CHANGE, and it is intended.** A payload naming `DEFAULT` on a venue this
/// core runs NO engine for is REFUSED, while its account-LESS twin
/// (`an_unknown_venue_still_routes_to_engine_zero`) still lands on engine 0 through `route_of`'s
/// `unwrap_or(0)`.
///
/// The two are a different ROW of the sender table, not two spellings of one. An ABSENT account
/// declines to name a book, and §4.2's `N = 0` cell deliberately keeps the historical answer for
/// that — `CoreThread::ambiguous_accounts`' own doc records why, and paper/sim engines behind
/// non-roster ids depend on it. `DEFAULT` NAMES a specific book, and this core has no engine of
/// that venue at all, so there is nothing for it to have named.
///
/// It is pinned here because it is the one place this change makes a caller's life harder rather
/// than easier: it bites a paper/sim engine behind a non-roster id the moment a client starts
/// sending `"account":"DEFAULT"` on every ticket. An unpinned behaviour change is how the next
/// person "fixes" it back without ever learning it was a decision — so if this test goes red, read
/// the paragraph above before editing it.
#[test]
fn a_payload_naming_default_on_an_unmounted_venue_refuses_while_its_account_less_twin_does_not() {
    let mut core = single_account_core();

    // The NAMED default account of a venue this core runs nothing for: refused.
    let named = core.apply_intent(
        OrderIntent::Submit(Box::new(open_order_for("no-such-venue", default_acct()))),
        1,
    );
    assert!(named.is_empty(), "nothing minted: {named:?}");
    assert_eq!(submitted_anywhere(&core), 0, "and nothing reached engine 0's book");
    assert!(
        refusal_note(&core).is_some_and(|n| n.contains("no-such-venue")),
        "refused by name, naming the venue it could not find"
    );

    // …and the ACCOUNT-LESS twin on the very same venue is untouched: engine 0, as always.
    let mut twin = single_account_core();
    let bare = twin.apply_intent(OrderIntent::Submit(Box::new(open_order("no-such-venue"))), 1);
    assert_eq!(bare.len(), 1, "minted, not refused — §4.2's `N = 0` cell is unchanged: {bare:?}");
    assert_eq!(twin.eng(0).client.submissions.len(), 1, "…and it still lands on engine 0");
    assert_eq!(refusal_note(&twin), None);
}

/// The node's list of accounts it does NOT run (decision 0156): okx's DEFAULT account and binance's
/// `ALT`, each with the reason word the mount gave it.
fn not_running_list() -> Vec<vike_exec::NotRunningAccount> {
    vec![
        vike_exec::NotRunningAccount {
            venue: "okx".to_string(),
            account: None,
            route_key: "okx".to_string(),
            tier: Some(vike_model::AccountTier::Live),
            reason: "no credentials".to_string(),
        },
        vike_exec::NotRunningAccount {
            venue: CANON.to_string(),
            account: Some(alt()),
            route_key: "binance#ALT".to_string(),
            tier: Some(vike_model::AccountTier::Demo),
            reason: "flags.tradehub_live off".to_string(),
        },
    ]
}

/// ⚠ **DECISION 0156 ITEM 2: AN ORDER TO AN ACCOUNT THAT DOES NOT RUN NEITHER VANISHES NOR REACHES
/// A SIMULATOR.** The node runs ONE engine (binance's default account, a recording client standing
/// in for whatever book it is); okx's default account and binance's `ALT` do not run. Before the
/// decision each of them had a PAPER engine standing in under its own name, and an order to one
/// was filled by that simulator. Now:
///
/// * an order NAMING `ALT` is refused before anything is minted — no coid exists to vanish — and
///   the refusal says the account does not run and WHY (`flags.tradehub_live off`), on the surface
///   an operator reads;
/// * an ACCOUNT-LESS order for okx — whose default account does not run — is refused the same way,
///   naming its reason (`no credentials`). This is the arm the decision made reachable: a venue the
///   core holds no engine for used to route to engine 0 (`an_unknown_venue_still_routes_to_engine_zero`
///   keeps that for a venue nobody NAMED), which here would be BINANCE's book;
/// * nothing reaches ANY engine's client — not the one engine this node runs, and there is no
///   other.
///
/// CONTROL: the account that runs takes its order, so the refusals are the list's doing.
#[test]
fn an_order_to_an_account_that_does_not_run_neither_vanishes_nor_reaches_a_simulator() {
    let mut core = core_of(
        engine(CANON, 1_000.0),
        Vec::new(),
        CoreConfig { seed_cash: 1_000.0, not_running: not_running_list(), ..CoreConfig::default() },
    );

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 1);
    assert!(coids.is_empty(), "refused before the mint, so no coid can vanish: {coids:?}");
    let note = refusal_note(&core).expect("the refusal is SAID");
    assert!(note.contains("ALT") && note.contains(CANON), "names the account: {note}");
    assert!(note.contains("does NOT RUN"), "says it does not run: {note}");
    assert!(note.contains("flags.tradehub_live off"), "names WHY: {note}");
    assert!(note.contains("not filled by a paper book"), "{note}");
    assert_eq!(submitted_anywhere(&core), 0, "nothing reached any venue client");

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order("okx"))), 2);
    assert!(coids.is_empty(), "an account-less order for okx is refused too: {coids:?}");
    let note = core
        .recent
        .iter()
        .rev()
        .find(|l| l.contains("REFUSED"))
        .map(|l| l.to_string())
        .expect("the okx refusal is SAID");
    assert!(note.contains("okx") && note.contains("no credentials"), "{note}");
    assert_eq!(
        submitted_anywhere(&core),
        0,
        "…and it did NOT fall through to binance's book, the engine-0 fallback"
    );

    // CONTROL: the account that RUNS takes its order.
    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 3);
    assert_eq!(coids.len(), 1, "binance's default account runs: {coids:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 1, "…and its order reached its own book");
}

// ---------------------------------------------------------------------------------------------
// **A BRACKET NAMES ITS ACCOUNT** — `BracketSpec::account`, and `lower_bracket` reading it the way
// `lower_submit` reads `OrderRequest::account`. Before the field a bracket on a venue that runs two
// engines (a lane twin beside the spot engine, or a labelled account) could only be refused as
// ambiguous, whatever the sender meant.
// ---------------------------------------------------------------------------------------------

/// A bracket on [`CANON`]/[`SYMBOL`] naming `account` (`None` = it names nothing).
fn bracket_for(account: Option<AccountLabel>) -> vike_model::BracketSpec {
    vike_model::BracketSpec {
        venue: CANON.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
        account,
    }
}

/// ⚠ **THE HEADLINE.** A bracket naming `ALT` on the two-account venue reaches `binance#ALT` and only
/// it: the OTO entry is live there, both exits are HELD, and every leg is recorded against ALT's
/// engine so the release and any cancel find it again.
#[test]
fn a_bracket_naming_an_account_reaches_the_named_engine_and_only_it() {
    let mut core = ticket_core();
    let coids = core.apply_intent(OrderIntent::Bracket(Box::new(bracket_for(Some(alt())))), 1);

    assert_eq!(coids.len(), 3, "entry + stop-loss + take-profit minted: {coids:?}");
    assert_eq!(refusal_note(&core), None, "a named account is not ambiguous");
    assert_eq!(core.eng(1).client.submissions.len(), 1, "only the OTO entry is live, on `ALT`");
    assert_eq!(core.eng(1).client.submissions[0].client_order_id, coids[0]);
    assert_eq!(core.eng(1).client.submissions[0].account, Some(alt()), "the leg carries it");
    assert_eq!(submitted_anywhere(&core), 1, "exactly one book, never a fan-out");
    assert_eq!(core.eng(0).client.submissions.len(), 0, "the default account saw nothing");
    assert_eq!(core.held_orders.len(), 2, "both protective exits are held, off the venue");
    for coid in &coids {
        assert_eq!(
            core.coid_venue.get(coid),
            Some(&1),
            "{coid} is recorded against `ALT`'s engine"
        );
    }
}

/// `DEFAULT` names the unlabelled account: it reaches engine 0 of the two-account venue rather than
/// minting `binance#DEFAULT` and refusing (the payload-side rule, one verb over).
#[test]
fn a_bracket_naming_default_reaches_the_unlabelled_account_of_a_two_account_venue() {
    let mut core = ticket_core();
    let coids =
        core.apply_intent(OrderIntent::Bracket(Box::new(bracket_for(Some(default_acct())))), 1);

    assert_eq!(coids.len(), 3, "`DEFAULT` names a book, so it is never ambiguous: {coids:?}");
    assert_eq!(refusal_note(&core), None);
    assert_eq!(core.eng(0).client.submissions.len(), 1, "the unlabelled engine got the entry");
    assert_eq!(core.eng(1).client.submissions.len(), 0, "…and `ALT` did not");
}

/// A bracket naming NOTHING keeps today's refusal, on the very core that just routed a named one:
/// the resolution must not disarm the ambiguity gate for the account-less spec.
#[test]
fn a_bracket_naming_nothing_still_refuses_beside_a_routed_one() {
    let mut core = ticket_core();
    let named = core.apply_intent(OrderIntent::Bracket(Box::new(bracket_for(Some(alt())))), 1);
    assert_eq!(named.len(), 3, "the named bracket routed");

    let bare = core.apply_intent(OrderIntent::Bracket(Box::new(bracket_for(None))), 2);
    assert!(bare.is_empty(), "…and the account-LESS one is refused before the mint: {bare:?}");
    let note = refusal_note(&core).expect("the ring must say so");
    assert!(note.contains("2 accounts") && note.contains("binance#ALT"), "existing words: {note}");
    assert_eq!(submitted_anywhere(&core), 1, "only the named bracket's entry ever reached a book");
}

/// ⚠ **An unheld account is refused BY NAME and mints nothing**, on both cores: the two-account one
/// (where the ambiguity gate would have said something else) and the SINGLE-account one, where the
/// gate is silent and `route_of`'s `sole_account_of` fallback would answer engine 0 — the one book
/// the node has, traded on a typo.
#[test]
fn a_bracket_naming_an_unheld_account_is_refused_in_words_and_mints_nothing() {
    let nosuch = AccountLabel::parse("NOSUCH").expect("a legal label");
    for (what, mut core) in
        [("two-account", ticket_core()), ("single-account", single_account_core())]
    {
        let spec = bracket_for(Some(nosuch.clone()));
        let coids = core.apply_intent(OrderIntent::Bracket(Box::new(spec)), 1);

        assert!(coids.is_empty(), "{what}: refused before the mint: {coids:?}");
        assert_eq!(submitted_anywhere(&core), 0, "{what}: nothing reached any venue client");
        assert!(core.held_orders.is_empty(), "{what}: no exit was held either");
        let note = refusal_note(&core).unwrap_or_else(|| panic!("{what}: the refusal is SAID"));
        assert!(note.contains("bracket"), "{what}: names the verb: {note}");
        assert!(note.contains("NOSUCH") && note.contains(CANON), "{what}: names the book: {note}");
        assert!(note.contains("Nothing was sent"), "{what}: {note}");
    }
}

/// Feed `core` the venue's full fill of the bracket entry `coid` (`OrderSubmitted`, `OrderAccepted`, `Fill`,
/// `OrderFilled`, as a real adapter delivers it). The events find their engine by the `coid_venue` row
/// `lower_bracket` recorded, and the `OrderFilled` releases the held exits.
fn fill_entry(core: &mut CoreThread<RecordingClient>, coid: &str) {
    use vike_model::events::{
        Event, FillEvent, OrderAccepted, OrderFilled, OrderSubmitted, PositionSide, TradeId,
    };
    let fill = FillEvent {
        trade_id: TradeId::prefixed("t-", coid),
        client_order_id: coid.to_string(),
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    };
    for ev in [
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts: 0 }),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(format!("v-{coid}").into()),
            ts: 0,
        }),
        Event::Fill(fill.clone()),
        Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts: 0 }),
    ] {
        core.dispatch(Ingest::Event(ev));
    }
}

/// The client-order-ids engine `idx`'s client was handed.
fn sent_to(core: &CoreThread<RecordingClient>, idx: usize) -> Vec<String> {
    core.eng(idx).client.submissions.iter().map(|r| r.client_order_id.clone()).collect()
}

/// ⚠ **The release is the moment the caller that knew the account is long gone.** A bracket on `ALT`
/// whose entry fills releases BOTH exits onto `ALT`'s client and not the default account's — the
/// `coid_venue` row `lower_bracket` recorded is what `submit_resolved` reads — and the released
/// legs still carry the account they were built with.
#[test]
fn a_labelled_brackets_entry_fill_releases_both_exits_onto_the_labelled_engine() {
    let mut core = ticket_core();
    let coids = core.apply_intent(OrderIntent::Bracket(Box::new(bracket_for(Some(alt())))), 1);
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
    assert_eq!(core.eng(1).client.submissions.len(), 1, "only the entry is live pre-fill");

    fill_entry(&mut core, &entry);

    let alt_sent = sent_to(&core, 1);
    assert!(alt_sent.contains(&sl) && alt_sent.contains(&tp), "both exits on `ALT`: {alt_sent:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 0, "NOT one on the default account's book");
    assert!(core.held_orders.is_empty(), "nothing left held once the parent filled");
    assert!(
        core.eng(1).client.submissions.iter().all(|r| r.account == Some(alt())),
        "every leg that reached `ALT`'s client carries the account"
    );
}

/// ⚠ **A named bracket on ENGINE ZERO is recorded too, because engine 0 is not always the default account.**
/// `lower_bracket` used to record its coids in `coid_venue` only for a non-primary engine, and the release's
/// fallback (`held_release_engine`) re-derives the engine from the VENUE when nothing is recorded: the venue's
/// DEFAULT engine. On a node booted with `binance#ALT` as engine 0 (the default account inactive) that later
/// starts the default account at index 1, a bracket naming `ALT` filled its entry on ALT and sent BOTH
/// reduce-only exits to the default engine, which holds no position, leaving ALT naked. Here ALT is engine 0,
/// the default account is engine 1, and everything stays on ALT.
#[test]
fn a_named_bracket_on_engine_zero_releases_its_exits_onto_engine_zero_not_the_default_account() {
    let mut core = core_of(
        engine("binance#ALT", 1_000.0),
        vec![(2_000.0, engine(CANON, 2_000.0))],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    );
    assert!(core.multi_account, "two engines of one exchange");
    let coids = core.apply_intent(OrderIntent::Bracket(Box::new(bracket_for(Some(alt())))), 1);
    assert_eq!(coids.len(), 3, "the named bracket routed to engine 0: {coids:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 1, "the entry is live on `ALT`, engine 0");
    for coid in &coids {
        assert_eq!(core.coid_venue.get(coid), Some(&0), "{coid} is recorded against engine 0");
    }
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());

    fill_entry(&mut core, &entry);

    let alt_sent = sent_to(&core, 0);
    assert!(alt_sent.contains(&sl) && alt_sent.contains(&tp), "both exits on ALT: {alt_sent:?}");
    assert_eq!(core.eng(1).client.submissions.len(), 0, "NOT one on the default account's book");
    assert!(core.held_orders.is_empty(), "nothing left held once the parent filled");
}

/// …and the same for the UNLABELLED account named `DEFAULT` while it is engine 0 (the shape every box has
/// today): the exits are released onto it and the labelled engine beside it sees nothing.
#[test]
fn a_bracket_naming_default_on_engine_zero_releases_its_exits_onto_engine_zero() {
    let mut core = ticket_core();
    let coids =
        core.apply_intent(OrderIntent::Bracket(Box::new(bracket_for(Some(default_acct())))), 1);
    assert_eq!(coids.len(), 3, "{coids:?}");
    let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());

    fill_entry(&mut core, &entry);

    let sent = sent_to(&core, 0);
    assert!(
        sent.contains(&sl) && sent.contains(&tp),
        "both exits on the default account: {sent:?}"
    );
    assert_eq!(core.eng(1).client.submissions.len(), 0, "`ALT` saw nothing");
}

// ---------------------------------------------------------------------------------------------
// **A STOP LEG THE VENUE REJECTS AT RELEASE** — the venue risk is Binance's perp STOP leg (unverified);
// whatever the cause, the core's side must be fail-safe and LOUD. The exits are held until the entry
// fills, so a stop the venue refuses is found out only then, with the position already open.
// ---------------------------------------------------------------------------------------------

/// A venue that REJECTS every stop order it is sent and accepts the rest, the way an adapter does: the
/// `[OrderSubmitted, OrderRejected | OrderAccepted]` pair comes back through `poll_events` right after the
/// submit (a cancel is answered with `OrderCanceled`). `rest_stops` makes it TAKE stops, and
/// `reject_exit_limits` makes it refuse a reduce-only limit (a bracket's take-profit) instead.
#[derive(Default)]
struct StopRejectingClient {
    submissions: Vec<vike_model::OrderRequest>,
    cancels: Vec<String>,
    pending: std::collections::VecDeque<vike_model::events::Event>,
    rest_stops: bool,
    /// Reject the reduce-only LIMIT (a bracket's take-profit) instead of the stop.
    reject_exit_limits: bool,
}

impl ExecutionClient for StopRejectingClient {
    fn submit(&mut self, request: &vike_model::OrderRequest) {
        use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};
        let coid = request.client_order_id.clone();
        self.submissions.push(request.clone());
        self.pending.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: coid.clone(),
            ts: 0,
        }));
        // `reject_exit_limits` refuses the reduce-only limit INSTEAD of the stop: that mode takes stops.
        let refused = match request.order_type {
            vike_model::OrderType::Stop => !self.rest_stops && !self.reject_exit_limits,
            vike_model::OrderType::Limit => self.reject_exit_limits && request.reduce_only,
            _ => false,
        };
        self.pending.push_back(if refused {
            Event::OrderRejected(OrderRejected {
                cause: None,
                client_order_id: coid,
                reason: "-2021 Order would immediately trigger".to_string().into(),
                ts: 0,
            })
        } else {
            Event::OrderAccepted(OrderAccepted {
                venue_order_id: Some(format!("v-{coid}").into()),
                client_order_id: coid,
                ts: 0,
            })
        });
    }
    fn cancel(&mut self, client_order_id: &str) {
        use vike_model::events::{Event, OrderCanceled};
        self.cancels.push(client_order_id.to_string());
        self.pending.push_back(Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: "user".to_string().into(),
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        self.pending.pop_front()
    }
}

/// The two-account venue of [`ticket_core`] over [`StopRejectingClient`]s.
fn stop_rejecting_core(config: CoreConfig) -> CoreThread<StopRejectingClient> {
    rejecting_core(config, false)
}

/// [`stop_rejecting_core`], or (`reject_exit_limits`) the venue that rejects the take-profit and takes the stop.
fn rejecting_core(config: CoreConfig, reject_exit_limits: bool) -> CoreThread<StopRejectingClient> {
    let engine = |route_key: &str, seed: f64| {
        let mut e = ExecutionEngine::new(
            Account::new(seed, CANON, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            StopRejectingClient { reject_exit_limits, ..StopRejectingClient::default() },
            CANON,
            SYMBOL,
        );
        e.route_key = route_key.to_string();
        e.collect_applied_fills = true;
        e
    };
    core_of(engine(CANON, 1_000.0), vec![(2_000.0, engine("binance#ALT", 2_000.0))], config)
}

/// The client-order-ids engine `idx`'s [`StopRejectingClient`] was handed.
fn sent_to_stop_rejecting(core: &CoreThread<StopRejectingClient>, idx: usize) -> Vec<String> {
    core.eng(idx).client.submissions.iter().map(|r| r.client_order_id.clone()).collect()
}

/// Place a bracket naming `ALT` on `core`, fill its entry, and return the `(stop-loss, take-profit)` coids.
/// The double has accepted the entry by then (its `poll_events` pair is drained by the submit's pump), so
/// only the fill is fed.
fn fill_the_alt_entry(core: &mut CoreThread<StopRejectingClient>) -> (String, String) {
    use vike_model::events::{Event, FillEvent, OrderFilled, PositionSide, TradeId};
    let before = core.eng(1).client.submissions.len();
    let coids = core.apply_intent(OrderIntent::Bracket(Box::new(bracket_for(Some(alt())))), 1);
    assert_eq!(coids.len(), 3, "{coids:?}");
    assert_eq!(core.eng(1).client.submissions.len(), before + 1, "only the entry is live pre-fill");
    let entry = coids[0].clone();
    let fill = FillEvent {
        trade_id: TradeId::prefixed("t-", &entry),
        client_order_id: entry.clone(),
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    };
    core.dispatch(Ingest::Event(Event::Fill(fill.clone())));
    core.dispatch(Ingest::Event(Event::OrderFilled(OrderFilled {
        client_order_id: entry,
        fill,
        ts: 0,
    })));
    (coids[1].clone(), coids[2].clone())
}

/// ⚠ **A stop-loss the venue REJECTS when it is released is surfaced, and the take-profit is KEPT by default.**
/// The trace: the entry's fill releases both exits (`drive_contingency_on_fill` → `submit_resolved`); the venue
/// answers the stop with `OrderRejected`; that terminal is a dead PROTECTIVE exit
/// (`drive_contingency_on_terminal`): its own book entry is cleaned, a `warn!` and a recent-events note say
/// "protective exit … died unfilled … surviving OCO sibling KEPT" (the rejection itself is folded into the
/// registry, not printed on the ring). The position keeps the take-profit (a naked position is the worse default, see
/// `CoreConfig::oco_cancel_sibling_on_dead_exit`) and the operator is told its downside is unprotected. Nothing
/// is cancelled, flattened or retried on its own.
#[test]
fn a_stop_leg_the_venue_rejects_at_release_is_said_loudly_and_the_take_profit_is_kept() {
    let mut core = stop_rejecting_core(CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() });
    let (sl, tp) = fill_the_alt_entry(&mut core);

    let sent = sent_to_stop_rejecting(&core, 1);
    assert!(sent.contains(&sl) && sent.contains(&tp), "both exits were released to ALT: {sent:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 0, "and nothing to the default account");
    assert!(
        core.eng(1).registry.get(&sl).is_some_and(|o| o.status.is_terminal()),
        "the stop leg is REJECTED, a terminal state in the registry (not a vanished order)"
    );
    assert!(
        core.recent.iter().any(|l| {
            l.contains("protective exit")
                && l.contains(&sl)
                && l.contains("died unfilled")
                && l.contains("KEPT")
        }),
        "the dead protective leg is SAID: {:?}",
        core.recent
    );
    // …and it does not claim a protection the position does not have: the survivor is a TAKE-PROFIT.
    assert!(
        core.recent.iter().any(|l| {
            l.contains(&sl) && l.contains(&tp) && l.contains("not a stop") && l.contains("NO stop")
        }),
        "the line names the take-profit as no protection: {:?}",
        core.recent
    );
    assert!(
        !core.recent.iter().any(|l| l.contains("surviving OCO sibling KEPT")),
        "the keep-protection wording is for a surviving STOP only: {:?}",
        core.recent
    );
    assert!(!core.contingency.contains(&sl), "the dead exit's own book entry is cleaned");
    assert!(core.contingency.contains(&tp), "the surviving take-profit stays in the book");
    assert!(core.eng(1).client.cancels.is_empty(), "the take-profit is NOT auto-cancelled");
    assert!(
        core.eng(1).registry.get(&tp).is_some_and(|o| !o.status.is_terminal()),
        "and it is still working at the venue"
    );
}

/// The mirror: the venue rejects the TAKE-PROFIT and takes the stop, so the survivor IS protection and the line keeps
/// saying the position keeps its remaining one (a stop that is the only exit left is exactly that).
#[test]
fn a_take_profit_the_venue_rejects_leaves_the_stop_and_the_line_says_it_is_kept() {
    let mut core = rejecting_core(CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() }, true);
    let (sl, tp) = fill_the_alt_entry(&mut core);

    assert!(
        core.eng(1).registry.get(&tp).is_some_and(|o| o.status.is_terminal()),
        "the take-profit is the rejected leg"
    );
    assert!(
        core.recent.iter().any(|l| {
            l.contains("protective exit")
                && l.contains(&tp)
                && l.contains("died unfilled")
                && l.contains("surviving OCO sibling KEPT")
        }),
        "{:?}",
        core.recent
    );
    assert!(
        !core.recent.iter().any(|l| l.contains("not a stop") || l.contains("NO stop")),
        "a surviving stop is protection: {:?}",
        core.recent
    );
    assert!(core.contingency.contains(&sl), "the stop stays in the book");
}

/// …and with the opt-in knob ON the surviving take-profit is CANCELLED and the note says so (the fully-flat
/// book choice): the position is then unprotected on both sides, which the line states.
#[test]
fn a_stop_leg_the_venue_rejects_cancels_the_take_profit_when_the_knob_is_on_and_says_so() {
    let mut core = stop_rejecting_core(CoreConfig {
        seed_cash: 1_000.0,
        oco_cancel_sibling_on_dead_exit: true,
        ..CoreConfig::default()
    });
    let (sl, tp) = fill_the_alt_entry(&mut core);

    assert_eq!(core.eng(1).client.cancels, vec![tp.clone()], "the take-profit is cancelled at ALT");
    assert!(
        core.recent
            .iter()
            .any(|l| l.contains(&sl) && l.contains("died unfilled") && l.contains("CANCELED")),
        "{:?}",
        core.recent
    );
    assert!(!core.contingency.contains(&sl) && !core.contingency.contains(&tp));
}

/// The `POSITION WITHOUT STOP` line of a bracket's dead stop, if the ring carries one.
fn naked_line(core: &CoreThread<StopRejectingClient>) -> Option<String> {
    core.recent.iter().find(|l| l.contains("POSITION WITHOUT STOP")).map(|l| l.to_string())
}

/// ⚠ **A bracket whose STOP dies after its entry filled says `POSITION WITHOUT STOP`, at once, and does
/// nothing else** (known-limitations, "every venue, for a BRACKET"). The venue refuses the released stop,
/// the position is long 1 on `ALT` and the take-profit that is left is not cover. The line names the dead
/// stop, its entry, the account, the symbol and the size, carries the venue's reason and the flatten
/// command, and says the node does not flatten: the double saw the three bracket orders and no cancel.
#[test]
fn a_bracket_stop_that_dies_after_the_entry_filled_raises_position_without_stop() {
    let mut core = stop_rejecting_core(CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() });
    let (sl, tp) = fill_the_alt_entry(&mut core);

    let line = naked_line(&core).unwrap_or_else(|| panic!("no alert: {:?}", core.recent));
    assert!(line.contains(&format!("the stop {sl} of bracket entry")), "{line}");
    assert!(line.contains("ended unfilled (-2021 Order would immediately trigger)"), "{line}");
    assert!(
        line.contains(&format!("binance#ALT binance: {SYMBOL} long 1 has no full stop")),
        "{line}"
    );
    assert!(line.contains("vike-cli trade position flatten binance/ALT BTCUSDT"), "{line}");
    assert!(line.contains("This node does not flatten it"), "{line}");
    assert_eq!(
        core.eng(1).client.submissions.len(),
        3,
        "entry, stop, take-profit and nothing else"
    );
    assert!(core.eng(1).client.cancels.is_empty(), "nothing is cancelled for the operator");
    assert!(core.eng(1).registry.get(&tp).is_some_and(|o| !o.status.is_terminal()));
    assert_eq!(
        core.recent.iter().filter(|l| l.contains("POSITION WITHOUT STOP")).count(),
        1,
        "one line per death: {:?}",
        core.recent
    );
}

/// …and with the opt-in sibling-cancel knob ON the position is naked all the same, so the line is said too.
#[test]
fn the_sibling_cancel_knob_does_not_silence_the_naked_bracket_line() {
    let mut core = stop_rejecting_core(CoreConfig {
        seed_cash: 1_000.0,
        oco_cancel_sibling_on_dead_exit: true,
        ..CoreConfig::default()
    });
    let (sl, _tp) = fill_the_alt_entry(&mut core);
    let line = naked_line(&core).unwrap_or_else(|| panic!("no alert: {:?}", core.recent));
    assert!(line.contains(&sl), "{line}");
}

/// A dying TAKE-PROFIT names nothing: the stop still rests and the position is covered.
#[test]
fn a_dying_take_profit_does_not_raise_position_without_stop() {
    let mut core = stop_rejecting_core(CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() });
    core.eng_mut(1).client.rest_stops = true;
    core.eng_mut(1).client.reject_exit_limits = true;
    let (sl, tp) = fill_the_alt_entry(&mut core);

    assert!(
        core.recent.iter().any(|l| l.contains(&tp) && l.contains("died unfilled")),
        "the take-profit's death is still said as before: {:?}",
        core.recent
    );
    assert!(core.eng(1).registry.get(&sl).is_some_and(|o| !o.status.is_terminal()));
    assert!(naked_line(&core).is_none(), "the stop rests: {:?}", core.recent);
}

/// A bracket stop that dies while ANOTHER stop covers the whole position is not a naked position.
#[test]
fn a_bracket_stop_that_dies_beside_another_stop_raises_nothing() {
    let mut core = stop_rejecting_core(CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() });
    // An unrelated resting stop on `ALT` for the whole size: this venue takes it…
    core.eng_mut(1).client.rest_stops = true;
    let other = vike_model::OrderRequest {
        order_type: vike_model::OrderType::Stop,
        side: -1,
        price: None,
        trigger_price: Some(95.0),
        ..open_order_for(CANON, alt())
    };
    let coids = core.apply_intent(OrderIntent::Submit(Box::new(other)), 1);
    assert_eq!(coids.len(), 1, "{coids:?}");
    // …and refuses the bracket's own.
    core.eng_mut(1).client.rest_stops = false;
    let (sl, _tp) = fill_the_alt_entry(&mut core);

    assert!(core.eng(1).registry.get(&sl).is_some_and(|o| o.status.is_terminal()));
    assert!(core.recent.iter().any(|l| l.contains(&sl) && l.contains("died unfilled")));
    assert!(naked_line(&core).is_none(), "the other stop covers it: {:?}", core.recent);
}
