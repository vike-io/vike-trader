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
