//! Inertness, the two-accounts-one-venue gate, and the reconcile round trip (residual 1).

use super::*;

// -------------------------------------------------------------------------------------------
// INERTNESS — the single-account shape is untouched.
// -------------------------------------------------------------------------------------------

/// With one account per venue — every configuration this workspace actually builds — the routing
/// key IS the canonical venue, so every venue-tagged payload resolves exactly the engine it always
/// did. Driven over a multi-venue core shaped like `vike_mount::build_node`'s, and exhaustive over
/// `vike_tradehub::wired_markets::WIRED_MARKETS`' venue column would be a layer inversion here, so the roster is spelled
/// as the canonical `vike_model::VENUES` set this core is built from.
#[test]
fn one_account_per_venue_routes_by_the_canonical_venue_exactly_as_before() {
    let venues = ["binance", "bybit", "okx", "deribit", "hyperliquid"];
    let (first, rest) = venues.split_first().expect("non-empty");
    let core = core_of(
        engine(first, first, SYMBOL),
        rest.iter().map(|v| (0.0, engine(v, v, SYMBOL))).collect(),
        CoreConfig::default(),
    );

    for (i, v) in venues.iter().enumerate() {
        assert_eq!(core.eng(i).route_key, core.eng(i).venue, "{v}: single-account shape");
        assert_eq!(
            core.engine_idx_for_route_key(RouteKey::sole_account_of(v)),
            Some(i),
            "{v} must resolve its own engine at index {i}"
        );
        // …and the payload every lane actually sends still lands there.
        assert_eq!(
            core.route_event(&fill(v, "t", 1.0)),
            Some(i),
            "{v}: a fill routes to its own engine"
        );
    }
    assert_eq!(
        core.engine_idx_for_route_key(RouteKey::sole_account_of("no-such-venue")),
        None,
        "an unknown key is still a miss, not a silent primary"
    );
}

// -------------------------------------------------------------------------------------------
// THE GATE — two accounts, one canonical venue.
// -------------------------------------------------------------------------------------------

/// THE PREMISE, stated on the engines themselves: both declare the SAME canonical venue and
/// DIFFERENT routing keys. Keyed on the construction, not on the routing result, so it cannot go
/// vacuous if routing regresses.
#[test]
fn the_two_engines_share_a_canonical_venue_and_differ_only_in_route_key() {
    let core = two_accounts_of_one_venue();
    assert_eq!(core.eng(0).venue, core.eng(1).venue, "one exchange");
    assert_eq!(core.eng(0).venue, CANON);
    assert_ne!(core.eng(0).route_key, core.eng(1).route_key, "two accounts");
    // The old single-field spelling is exactly this configuration collapsed, which is why it could
    // not work: on `venue` alone the two engines are indistinguishable.
    assert!(
        !vike_model::VENUES.contains(&ACCOUNT_B),
        "the second account's key is deliberately NOT a roster id — putting it on `venue` is the \
         trap this split exists to make impossible"
    );
}

/// THE GATE. Each account's fill routes to ITS OWN engine, and lands in ITS OWN book. Production
/// never constructs this — `vike_tradehub::wired_markets::WIRED_MARKETS` is unique per venue and its own test enforces
/// that — which is exactly why it has to be constructed here: it is the whole point of the change,
/// and nothing else in the tree can demonstrate it.
#[test]
fn two_accounts_of_one_venue_each_route_to_their_own_engine() {
    let mut core = two_accounts_of_one_venue();

    assert_eq!(
        core.engine_idx_for_route_key(RouteKey::sole_account_of(ACCOUNT_A)),
        Some(0),
        "account A is the primary"
    );
    assert_eq!(
        core.engine_idx_for_route_key(RouteKey::sole_account_of(ACCOUNT_B)),
        Some(1),
        "account B is the extra"
    );

    // ⚠ ACCOUNT_A's key IS the canonical venue (that is what a DEFAULT account's route key is), so
    // a coid-less payload labelled with it is indistinguishable from a bare venue-tagged one — and
    // since §5.4 that is UNATTRIBUTED on a venue with two accounts rather than folded into the
    // default book. Naming the order is what makes it routable, which is exactly what production
    // does: `apply_intent_routed` records the index in `coid_venue` at every submit.
    let mut a = fill(ACCOUNT_A, "a-1", 3.0);
    if let Event::Fill(f) = &mut a {
        f.client_order_id = "ours-on-a".to_string();
    }
    core.coid_venue.insert("ours-on-a".to_string(), 0);
    let b = fill(ACCOUNT_B, "b-1", 7.0);
    assert_eq!(core.route_event(&a), Some(0));
    assert_eq!(core.route_event(&b), Some(1), "…and ACCOUNT_B's key names ONE engine outright");

    // …and fold each through the runtime's own publish path, exactly as the `Ingest::Event` arm
    // does. A book, not an index, is what the bug actually corrupted.
    let (ia, ib) =
        (core.route_event(&a).expect("attributed"), core.route_event(&b).expect("attributed"));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(held(&core, 0, ACCOUNT_A), 3.0, "account A holds its own fill");
    assert_eq!(held(&core, 1, ACCOUNT_B), 7.0, "account B holds its own fill");
    assert_eq!(
        held(&core, 0, ACCOUNT_B),
        0.0,
        "account A must not hold account B's fill — this is the two-books-in-one bug"
    );
    assert_eq!(held(&core, 1, ACCOUNT_A), 0.0, "…and the converse");
}

/// THE NEGATIVE CONTROL — what the OLD behaviour was, reproduced by collapsing the two route keys
/// back together. The second engine becomes unreachable and BOTH accounts' fills fold into the
/// primary's book, which is the failure the split removes.
///
/// This is what makes the gate above non-vacuous: it proves the harness can SEE the bug, so a pass
/// there is a real verdict rather than a routing that happens to answer 0 and 1 for other reasons.
#[test]
fn collapsing_the_route_keys_reproduces_the_unreachable_second_engine() {
    // Both engines keyed ACCOUNT_A — i.e. the single-field world, where `venue` was the router.
    let mut core = core_of(
        engine(CANON, ACCOUNT_A, SYMBOL),
        vec![(0.0, engine(CANON, ACCOUNT_A, SYMBOL))],
        CoreConfig::default(),
    );

    assert_eq!(
        core.engine_idx_for_route_key(RouteKey::sole_account_of(ACCOUNT_A)),
        Some(0),
        "the primary answers first and the extra can never be reached"
    );

    // Each account's own order still finds its own book — that is what `coid_venue` is for, and it
    // is the half the collapse cannot take away.
    let mut a = fill(ACCOUNT_A, "a-1", 3.0);
    if let Event::Fill(f) = &mut a {
        f.client_order_id = "ours-on-a".to_string();
    }
    core.coid_venue.insert("ours-on-a".to_string(), 0);
    let mut b = fill(ACCOUNT_A, "b-1", 7.0);
    if let Event::Fill(f) = &mut b {
        f.client_order_id = "ours-on-b".to_string();
    }
    core.coid_venue.insert("ours-on-b".to_string(), 1);
    let (ia, ib) =
        (core.route_event(&a).expect("attributed"), core.route_event(&b).expect("attributed"));
    assert_eq!((ia, ib), (0, 1), "a NAMED order routes by its coid whatever the keys say");
    core.publish_to(ia, a);
    core.publish_to(ib, b);
    assert_eq!(held(&core, 0, ACCOUNT_A), 3.0);
    assert_eq!(held(&core, 1, ACCOUNT_A), 7.0);

    // …and the collapse's real cost, which §5.4 turned from a silent misroute into a refusal: an
    // UNNAMED fill can no longer be attributed to either engine, so neither book moves. Before
    // §5.4 both of these folded into the primary's book (3 + 7 in one ledger) — that is the
    // behaviour this negative control used to pin, and closing it is the point of Stage 1.
    let c = fill(ACCOUNT_A, "c-1", 11.0);
    assert_eq!(core.route_event(&c), None, "two engines, one key, no order named: unattributed");
    assert_eq!(held(&core, 0, ACCOUNT_A), 3.0, "…and nothing was folded anywhere");
    assert_eq!(held(&core, 1, ACCOUNT_A), 7.0);
}

/// The capability plane is untouched by any of this: both engines resolve the REAL row, because
/// every table is keyed on `venue` and the account distinction went on `route_key`. The exhaustive
/// form over `vike_model::VENUES` lives in `crates/vike-exec/tests/engine/route_key.rs`; this is
/// the tie proving it still holds for an engine assembled into a running core.
#[test]
fn both_accounts_still_resolve_the_real_venue_row() {
    let core = two_accounts_of_one_venue();
    for idx in [0, 1] {
        assert_eq!(vike_model::caps_for(&core.eng(idx).venue), vike_model::caps_for(CANON));
        assert_ne!(vike_model::caps_for(&core.eng(idx).venue), vike_model::VenueCaps::UNSUPPORTED);
        assert_eq!(
            vike_model::amend_semantics(&core.eng(idx).venue),
            vike_model::amend_semantics(CANON),
        );
    }
    assert_eq!(
        vike_model::caps_for(ACCOUNT_B),
        vike_model::VenueCaps::UNSUPPORTED,
        "…while the routing key itself is a table MISS, which is why it may never key one"
    );
}

// -------------------------------------------------------------------------------------------
// RESIDUAL 1 — the RECONCILE round trip: canonical goes out, routing comes back in.
//
// `ReconcileReports` used to carry ONE venue string, and `CoreThread::reconcile_reports` answered
// two questions from it: which engine's `local_view` this pass diffs against and whose book its
// synthesized events fold into (ROUTING), and what to call the venue in every note, log line and
// held-alert identity (CANONICAL). It then STORED that one string on `HeldReconAlert` and routed
// on it again in `confirm_recon`, minutes later, when an operator approved the held events — so
// the crossing outlived the pass that made it.
//
// The tests below drive both legs with the two facts DIFFERENT, which is the only configuration
// that can tell them apart.
// -------------------------------------------------------------------------------------------

/// A venue FILL REPORT for [`CANON`] that local state has never folded — a `MissingFill`.
///
/// ⚠ Its `venue` is the CANONICAL id, never a route key, because that is what a venue adapter
/// mints. That asymmetry is the residual in one line: the REPORTS are canonical, the ROUTING is
/// per-account, and one field cannot be both.
fn missing_fill_report(trade_id: &'static str, qty: f64) -> vike_model::FillReport {
    vike_model::FillReport {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        trade_id: trade_id.into(),
        venue_order_id: "v9".into(),
        client_order_id: None, // external order — no local coid
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Taker,
        ts: 5,
    }
}

fn reports_for(
    route_key: Option<&str>,
    policy: vike_exec::recon::ReconPolicy,
    fills: Vec<vike_model::FillReport>,
) -> ReconcileReports {
    ReconcileReports {
        venue: CANON.into(),
        since: 0,
        orders: Vec::new(),
        fills,
        positions: Vec::new(),
        policy,
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: route_key.map(str::to_string),
    }
}

/// Two accounts of one venue, both `Account`s labelled with the CANONICAL venue — see
/// [`engine_with_account_venue`] for why that is the right seeding for the reconcile path and the
/// wrong one for the fill-routing path.
fn two_accounts_for_reconcile() -> CoreThread<RecordingClient> {
    core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
        CoreConfig::default(),
    )
}

/// THE PREMISE for both legs, keyed on the CONSTRUCTION rather than on any routing result, so it
/// cannot go vacuous if routing regresses: the two engines are one exchange and two accounts, both
/// books are addressable under the same canonical position key, and the second account's routing
/// key is deliberately not a roster id.
#[test]
fn the_reconcile_harness_is_two_accounts_of_one_exchange() {
    let core = two_accounts_for_reconcile();
    assert_eq!(core.eng(0).venue, core.eng(1).venue, "one exchange");
    assert_eq!(core.eng(0).venue, CANON);
    assert_ne!(core.eng(0).route_key, core.eng(1).route_key, "two accounts");
    assert_eq!(core.eng(0).account.venue, core.eng(1).account.venue, "one position-key namespace");
    assert!(!vike_model::VENUES.contains(&ACCOUNT_B), "the routing key is not a roster id");
}

/// LEG 1 — the pass. A reconcile pass whose ROUTE KEY names account B folds its synthesized fill
/// into B's book and leaves A flat, while the pass's `venue` (the canonical id every report row
/// carries, and the key the manager's health probe is looked up under) stays `"binance"`.
///
/// Fold this pass by `venue` instead — the pre-fix spelling, and the only spelling the payload used
/// to allow — and it resolves account A: A's local view is what B's venue reports get diffed
/// against, and B's venue truth lands in A's book. Under `hybrid` that is not a
/// display bug, it is `PositionDrift` rewriting one account's size onto the other's number and
/// booking realized PnL at the other's average price.
#[test]
fn a_reconcile_pass_folds_into_the_engine_its_route_key_names() {
    let mut core = two_accounts_for_reconcile();
    core.reconcile_reports(reports_for(
        Some(ACCOUNT_B),
        vike_exec::recon::ReconPolicy::default(), // Synthesize everything — the fill folds immediately
        vec![missing_fill_report("t-b", 7.0)],
    ));

    assert_eq!(held(&core, 1, CANON), 7.0, "account B folded its own venue's fill");
    assert_eq!(
        held(&core, 0, CANON),
        0.0,
        "account A must not have folded account B's fill — this is the reconcile round trip"
    );
}

/// LEG 2 — THE DECISIVE ONE. The confirm. A QUARANTINED divergence is held across time and folded
/// only when an operator approves it, and `confirm_recon` re-resolves the engine from the held
/// record. So the pass's routing decision has to be STORED, and stored as a route key: a held row
/// keyed by canonical venue alone would send operator-approved fills into the first account of the
/// exchange, whichever account raised them.
#[test]
fn an_operator_confirm_folds_into_the_engine_the_raising_pass_routed_to() {
    let mut core = two_accounts_for_reconcile();
    let quarantine = vike_exec::recon::ReconPolicy {
        default: vike_exec::recon::ReconMode::Quarantine,
        ..Default::default()
    };
    core.reconcile_reports(reports_for(
        Some(ACCOUNT_B),
        quarantine,
        vec![missing_fill_report("t-b", 7.0)],
    ));

    // Held, not folded — the precondition that makes the confirm the thing under test.
    assert_eq!(core.recon_alerts.len(), 1, "the divergence must be HELD for an operator");
    assert_eq!(held(&core, 0, CANON), 0.0, "nothing folds before the confirm");
    assert_eq!(held(&core, 1, CANON), 0.0, "…in either book");
    let (&id, alert) = core.recon_alerts.first().expect("one held alert");
    assert_eq!(alert.venue, CANON, "the operator-facing label is the exchange");
    assert_eq!(alert.route_key, ACCOUNT_B, "…and the stored routing decision is the account");

    core.confirm_recon(id);

    assert_eq!(held(&core, 1, CANON), 7.0, "the confirmed events fold into account B");
    assert_eq!(
        held(&core, 0, CANON),
        0.0,
        "…and NOT into account A, which is the engine `held.venue` would have resolved"
    );
}

/// THE NEGATIVE CONTROL for both legs — what the pre-fix behaviour was, reproduced by spelling the
/// pass the only way the old payload could: routed by the canonical venue. Both accounts' passes
/// then resolve account A, so account B is unreachable for reconcile entirely and its venue truth
/// folds into A's book.
///
/// This is what makes the two gates above non-vacuous: it proves the harness can SEE the failure,
/// so a pass there is a verdict rather than two books that happen to differ for other reasons.
#[test]
fn routing_a_reconcile_pass_by_its_canonical_venue_reaches_only_the_first_account() {
    let mut core = two_accounts_for_reconcile();
    // `Some(CANON)` is what a payload carrying ONE venue string could say — and what
    // `route_key: None` means for a single-account mount, which is why `None` is inert THERE.
    //
    // ⚠ It has to be spelled `Some` here, and that is not a stylistic choice: on this two-account
    // core a `None` is now REFUSED outright (`CoreThread::reconcile_reports`' Class E arm — a pass
    // that cannot say which account it read is not folded against the venue's default), so it
    // would fold nothing and this negative control would stop demonstrating the misroute it
    // exists to demonstrate. `Some(CANON)` reproduces the pre-fix BEHAVIOUR — account A's engine
    // resolved for a pass about account B — without asking the refusal about it.
    core.reconcile_reports(reports_for(
        Some(CANON),
        vike_exec::recon::ReconPolicy::default(),
        vec![missing_fill_report("t-b", 7.0)],
    ));
    assert_eq!(held(&core, 0, CANON), 7.0, "account A folded a pass that was about account B");
    assert_eq!(held(&core, 1, CANON), 0.0, "account B folded nothing and is unreachable");
}

/// INERTNESS. `route_key: None` — what a venue with ONE account still sends, and what every
/// journal written before the field existed decodes to — routes to that sole account, i.e. exactly
/// where the one-field payload routed. Driven over the SINGLE-account shape, so it is the
/// byte-identical claim and not a two-account convenience.
///
/// ⚠ "what every producer in this tree sends" is what this said, and it stopped being true when
/// the reconcile manager learned to stamp a labelled account's route key. What survives — and is
/// the whole of what a running deployment depends on — is that a venue with one account is
/// unchanged. On a venue with SEVERAL, a `None` is refused rather than routed; see
/// `crates/vike-core/tests/recon/recon_per_account.rs`'s
/// `a_pass_that_names_no_account_is_refused_where_the_venue_has_several` for that half.
#[test]
fn an_absent_route_key_routes_exactly_where_the_single_field_payload_did() {
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        Vec::new(),
        CoreConfig::default(),
    );
    core.reconcile_reports(reports_for(
        None,
        vike_exec::recon::ReconPolicy::default(),
        vec![missing_fill_report("t-a", 3.0)],
    ));
    assert_eq!(held(&core, 0, CANON), 3.0, "the sole account folded its own pass");
}
