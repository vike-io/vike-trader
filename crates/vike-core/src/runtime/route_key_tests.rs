//! Routing by `ExecutionEngine::route_key` — the gate over the split that made two accounts of one
//! venue expressible in one process.
//!
//! # What was wrong
//!
//! [`CoreThread::engine_idx_for_route_key`] used to compare against `ExecutionEngine::venue`, the
//! same field every per-venue capability table is keyed on (`vike_model::caps_for`,
//! `amend_semantics`, `fee_schedule_for`, …). One field, two incompatible jobs — so a second
//! account of an exchange had no spelling:
//!
//! * label both engines `"binance"` and this function returns the FIRST match forever. The second
//!   engine is unreachable for fills, both accounts fold into ONE book, and reconcile compounds it:
//!   under `hybrid` `PositionDrift` auto-applies, so each pass rewrites the local position onto
//!   whichever account answered last.
//! * label the second `"binance#2"` and routing works while every capability lookup misses — the
//!   trap `crates/vike-exec/tests/engine/route_key.rs` gates from the other side.
//!
//! # …and the two ROUND TRIPS the split left open
//!
//! Splitting the engine's field did not split the strings that reach it. Two chains carried a
//! CANONICAL venue out of the core and handed it back as a ROUTING key, and both are gated below:
//!
//! * **The reconcile round trip.** `ReconcileReports` carried one venue string;
//!   `CoreThread::reconcile_reports` routed on it, then STORED it on `HeldReconAlert` and routed on
//!   it AGAIN in `confirm_recon` when an operator approved the held events. The payload now carries
//!   `route_key` beside `venue`, the held record keeps both, and the sections below drive each leg
//!   with the two facts different.
//! * **The order-payload double load.** `OrderRequest::venue` is what `engine_idx_for_route_key`
//!   routes on AND what `vike_model::preflight_order` selected the capability row by — and a
//!   non-roster string does not fail closed there, it returns `Ok(())`. `CoreThread::caps_venue`
//!   now asks the ROUTED ENGINE for its canonical venue instead of asking the payload.
//!
//! # White-box, and why
//!
//! `engine_idx_for_route_key`, `caps_venue`, `route_event`, `publish_to`, `reconcile_reports`,
//! `confirm_recon`, `apply_intent` and the `recon_alerts` store are all private to this module, and
//! they are precisely what is under test — composing `route_event` with `publish_to` is exactly
//! what the `Ingest::Event` dispatch arm does inline, and `reconcile_reports`/`confirm_recon` are
//! what the `Command::ReconcileReports`/`ConfirmRecon` arms call. The `use super::*`
//! sibling-test-module idiom of `safe_state_tests.rs`/`multi_mount_tests.rs`.

use super::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate, RiskLimits};
use vike_model::events::{FillEvent, TradeId};

/// The canonical venue BOTH engines below declare. A roster id, so every capability table resolves
/// its real row for either engine.
const CANON: &str = "binance";
/// The primary account's routing key — equal to [`CANON`], i.e. the default `ExecutionEngine::new`
/// produces and the shape every mount in this workspace builds.
const ACCOUNT_A: &str = "binance";
/// The SECOND account's routing key. Not a roster id; that is the point.
const ACCOUNT_B: &str = "binance-sub2";

const SYMBOL: &str = "BTCUSDT";

/// One engine whose canonical venue is `venue` and whose routing key is `route_key`.
///
/// ⚠ The `Account` is seeded with `route_key`, not `venue`, and that is not a shortcut — it is a
/// second seam this split does NOT close, recorded here because the test would otherwise hide it.
/// `Account::apply_fill` opens with a hard `assert_eq!(fill.venue, self.venue)`, so a fill can only
/// fold into an account whose venue string it carries. Routing a fill to the right one of two
/// same-venue engines therefore needs the fill to be DISTINGUISHABLE, and today the only field on
/// the payload that can distinguish it is the one that assert compares. Wiring a real second
/// account has to give `FillEvent` a route key of its own (or route it by coid, the way
/// `coid_venue` already routes order-lifecycle replies, which carry no venue at all); until then
/// this is how the two-account shape is expressible at all, and pinning it here is what makes the
/// gap visible instead of theoretical.
fn engine(venue: &str, route_key: &str, symbol: &str) -> ExecutionEngine<RecordingClient> {
    engine_with_account_venue(venue, route_key, route_key, symbol)
}

/// [`engine`] with the `Account`'s venue label chosen SEPARATELY from the routing key.
///
/// The fill-routing tests need it to be the route key (see [`engine`]'s note). The RECONCILE tests
/// need the opposite: a reconcile pass's synthesized fills carry the venue off the venue's own
/// FILL REPORT, which an adapter mints canonically, so they only fold into an account labelled
/// with the canonical venue. Both engines can carry that label at once precisely because reconcile
/// does NOT route by the fill — `reconcile_reports` resolves the engine ONCE from the pass's route
/// key and then publishes straight to it, which is the whole hop under test.
fn engine_with_account_venue(
    venue: &str,
    route_key: &str,
    account_venue: &str,
    symbol: &str,
) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, account_venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        symbol,
    );
    e.route_key = route_key.to_string();
    e
}

fn core_of(
    primary: ExecutionEngine<RecordingClient>,
    extras: Vec<(f64, ExecutionEngine<RecordingClient>)>,
) -> CoreThread<RecordingClient> {
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&primary.venue, &primary.symbol)));
    assemble_core(
        primary,
        extras,
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    )
}

/// The two-account core: one canonical venue, two routing keys.
fn two_accounts_of_one_venue() -> CoreThread<RecordingClient> {
    core_of(engine(CANON, ACCOUNT_A, SYMBOL), vec![(0.0, engine(CANON, ACCOUNT_B, SYMBOL))])
}

fn fill(route_key: &str, trade_id: &'static str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        client_order_id: String::new(),
        venue: route_key.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: "BOTH".into(),
    })
}

/// Signed `BOTH` size held by the engine at routing index `idx`, under `key`.
fn held(core: &CoreThread<RecordingClient>, idx: usize, key: &str) -> f64 {
    core.eng(idx)
        .account
        .positions
        .get(&(
            ustr::Ustr::from(key),
            ustr::Ustr::from(SYMBOL),
            vike_model::events::PositionSide::Both,
        ))
        .map(|p| p.size)
        .unwrap_or(0.0)
}

// -------------------------------------------------------------------------------------------
// INERTNESS — the single-account shape is untouched.
// -------------------------------------------------------------------------------------------

/// With one account per venue — every configuration this workspace actually builds — the routing
/// key IS the canonical venue, so every venue-tagged payload resolves exactly the engine it always
/// did. Driven over a multi-venue core shaped like `vike_run::build_node`'s, and exhaustive over
/// `vike_run::WIRED_MARKETS`' venue column would be a layer inversion here, so the roster is spelled
/// as the canonical `vike_model::VENUES` set this core is built from.
#[test]
fn one_account_per_venue_routes_by_the_canonical_venue_exactly_as_before() {
    let venues = ["binance", "bybit", "okx", "deribit", "hyperliquid"];
    let (first, rest) = venues.split_first().expect("non-empty");
    let core = core_of(
        engine(first, first, SYMBOL),
        rest.iter().map(|v| (0.0, engine(v, v, SYMBOL))).collect(),
    );

    for (i, v) in venues.iter().enumerate() {
        assert_eq!(core.eng(i).route_key, core.eng(i).venue, "{v}: single-account shape");
        assert_eq!(
            core.engine_idx_for_route_key(RouteKey::sole_account_of(v)),
            Some(i),
            "{v} must resolve its own engine at index {i}"
        );
        // …and the payload every lane actually sends still lands there.
        assert_eq!(core.route_event(&fill(v, "t", 1.0)), i, "{v}: a fill routes to its own engine");
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
/// never constructs this — `vike_run::WIRED_MARKETS` is unique per venue and its own test enforces
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

    let a = fill(ACCOUNT_A, "a-1", 3.0);
    let b = fill(ACCOUNT_B, "b-1", 7.0);
    assert_eq!(core.route_event(&a), 0);
    assert_eq!(core.route_event(&b), 1);

    // …and fold each through the runtime's own publish path, exactly as the `Ingest::Event` arm
    // does. A book, not an index, is what the bug actually corrupted.
    let (ia, ib) = (core.route_event(&a), core.route_event(&b));
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
    let mut core =
        core_of(engine(CANON, ACCOUNT_A, SYMBOL), vec![(0.0, engine(CANON, ACCOUNT_A, SYMBOL))]);

    assert_eq!(
        core.engine_idx_for_route_key(RouteKey::sole_account_of(ACCOUNT_A)),
        Some(0),
        "the primary answers first and the extra can never be reached"
    );

    let a = fill(ACCOUNT_A, "a-1", 3.0);
    let b = fill(ACCOUNT_A, "b-1", 7.0);
    let (ia, ib) = (core.route_event(&a), core.route_event(&b));
    assert_eq!((ia, ib), (0, 0), "both accounts' fills route to the SAME engine");
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(held(&core, 0, ACCOUNT_A), 10.0, "…and both fold into one book: 3 + 7");
    assert_eq!(held(&core, 1, ACCOUNT_A), 0.0, "the second engine folded nothing");
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
    policy: vike_exec::ReconPolicy,
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
        vike_exec::ReconPolicy::default(), // Synthesize everything — the fill folds immediately
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
    let quarantine =
        vike_exec::ReconPolicy { default: vike_exec::ReconMode::Quarantine, ..Default::default() };
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
    // `route_key: None` means for a single-account mount, which is why `None` is inert.
    core.reconcile_reports(reports_for(
        Some(CANON),
        vike_exec::ReconPolicy::default(),
        vec![missing_fill_report("t-b", 7.0)],
    ));
    assert_eq!(held(&core, 0, CANON), 7.0, "account A folded a pass that was about account B");
    assert_eq!(held(&core, 1, CANON), 0.0, "account B folded nothing and is unreachable");
}

/// INERTNESS. `route_key: None` — what every producer in this tree sends, and what every journal
/// written before the field existed decodes to — routes to the venue's sole account, i.e. exactly
/// where the one-field payload routed. Driven over the SINGLE-account shape this workspace actually
/// mounts, so it is the byte-identical claim and not a two-account convenience.
#[test]
fn an_absent_route_key_routes_exactly_where_the_single_field_payload_did() {
    let mut core = core_of(engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL), Vec::new());
    core.reconcile_reports(reports_for(
        None,
        vike_exec::ReconPolicy::default(),
        vec![missing_fill_report("t-a", 3.0)],
    ));
    assert_eq!(held(&core, 0, CANON), 3.0, "the sole account folded its own pass");
}

// -------------------------------------------------------------------------------------------
// RESIDUAL 2 — the ORDER payload's venue is doubly loaded, and the CAPABILITY half now asks the
// routed ENGINE instead of the payload.
//
// `OrderRequest::venue` is what `engine_idx_for_route_key` routes on AND what
// `vike_model::preflight_order` selected the caps row by. Those are the two jobs the split
// separated on `ExecutionEngine`; the payload has one field, so a per-ACCOUNT routing key on it
// used to reach `preflight_order` — where a string outside `vike_model::VENUES` does NOT fail
// closed to `VenueCaps::UNSUPPORTED` but returns `Ok(())`, i.e. every check skipped (order kind,
// TIF, margin mode) for one account of one exchange, silently.
//
// `CoreThread::caps_venue` resolves it from the engine the request routed to instead. Inert while
// every engine's route key IS its venue, because the routed engine's `venue` is then the same
// string the request already carried.
// -------------------------------------------------------------------------------------------

/// A decorated per-account routing key for `venue` — the second-account spelling, applied to every
/// roster venue in turn below.
fn sub_account_key(venue: &str) -> String {
    format!("{venue}-sub2")
}

/// EXHAUSTIVE over `vike_model::VENUES`: a ticket addressed by a second account's ROUTING KEY
/// reaches that account's engine, and the capability lookup for it still resolves the REAL venue
/// row. Both halves asserted for every roster venue, with no skip arm — a venue joining the roster
/// is covered the day it joins.
///
/// The last assertion is what makes this about money rather than tidiness: the routing key is NOT a
/// roster id, and `preflight_order`'s unknown-venue affordance answers `Ok(())` for a non-roster
/// string. Reading the routing string here is therefore not a conservative miss — it is the
/// preflight not running at all.
#[test]
fn a_ticket_addresses_the_right_engine_while_caps_resolve_the_real_venue_row() {
    for v in vike_model::VENUES {
        let key = sub_account_key(v);
        let core = core_of(engine(v, v, SYMBOL), vec![(0.0, engine(v, &key, SYMBOL))]);

        let routed = core.engine_idx_for_route_key(RouteKey::declared(&key));
        assert_eq!(routed, Some(1), "{v}: the ticket must address the SECOND account");

        let caps_venue = core.caps_venue(routed, &key);
        assert_eq!(caps_venue, *v, "{v}: the caps row must be the routed ENGINE's canonical venue");
        assert_eq!(vike_model::caps_for(caps_venue), vike_model::caps_for(v), "{v}");
        assert_ne!(vike_model::caps_for(caps_venue), vike_model::VenueCaps::UNSUPPORTED, "{v}");

        // The precondition that makes the whole thing bite — a fact about the roster and the
        // chosen string, independent of anything this test exercises.
        assert!(
            !vike_model::VENUES.contains(&key.as_str()),
            "{v}: a routing key is not a roster id, so keying caps on it SKIPS preflight entirely"
        );
    }
}

/// …and an UNROUTED payload keeps its own string, which preserves the unknown-venue affordance the
/// paper/sim engines behind non-roster ids depend on. Attributing it to the primary engine's row
/// instead would start refusing traffic that flows today — the one way this change could have moved
/// behaviour, pinned so it cannot.
#[test]
fn an_unrouted_payload_keeps_its_own_venue_for_the_caps_lookup() {
    let core = two_accounts_of_one_venue();
    let routed = core.engine_idx_for_route_key(RouteKey::sole_account_of("no-such-venue"));
    assert_eq!(routed, None, "precondition: nothing answers for this venue");
    assert_eq!(core.caps_venue(routed, "no-such-venue"), "no-such-venue");
}

/// END TO END through the real submit path: an order kind the venue's declared row does NOT
/// support, addressed by a second account's routing key, is REFUSED — it never reaches the client.
///
/// The (venue, kind) pair is DERIVED from the live caps table rather than written down, and the
/// derivation `expect`s rather than skipping, so a table that stopped declining anything fails here
/// instead of quietly passing.
#[test]
fn an_unsupported_order_kind_is_still_refused_for_a_second_account() {
    let (venue, kind) = vike_model::VENUES
        .iter()
        .find_map(|v| {
            vike_model::venue_caps::ORDER_KINDS
                .iter()
                .find(|k| !vike_model::caps_for(v).supported_order_kinds.contains(k))
                .map(|k| (*v, *k))
        })
        .expect("some roster venue declines some declared order kind");

    let key = sub_account_key(venue);
    let mut core = core_of(engine(venue, venue, SYMBOL), vec![(0.0, engine(venue, &key, SYMBOL))]);
    let req = vike_model::OrderRequest {
        client_order_id: "c-1".into(),
        venue: key.clone(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: kind.into(),
        price: Some(100.0),
        trigger_price: Some(100.0),
        ..Default::default()
    };
    core.apply_intent(OrderIntent::Submit(Box::new(req)), 0);

    assert_eq!(
        core.eng(1).client.submissions.len(),
        0,
        "{venue}/{kind}: the venue's real row declines this kind, so it must never reach the client"
    );
    assert_eq!(core.eng(0).client.submissions.len(), 0, "{venue}/{kind}: …nor the other account's");
}

/// The CONTROL for the test above, and what makes it non-vacuous: the same order, same engine, same
/// routing key, but a kind the venue DOES support — which must reach the client. A preflight that
/// refused everything (or a routing that reached no engine at all) would pass the test above and
/// fail this one.
#[test]
fn a_supported_order_kind_still_reaches_the_second_accounts_client() {
    let (venue, kind) = vike_model::VENUES
        .iter()
        .find_map(|v| {
            vike_model::caps_for(v)
                .supported_order_kinds
                .iter()
                .find(|k| k.eq_ignore_ascii_case("market") || k.eq_ignore_ascii_case("limit"))
                .map(|k| (*v, *k))
        })
        .expect("some roster venue supports market or limit");

    let key = sub_account_key(venue);
    let mut core = core_of(engine(venue, venue, SYMBOL), vec![(0.0, engine(venue, &key, SYMBOL))]);
    let req = vike_model::OrderRequest {
        client_order_id: "c-2".into(),
        venue: key.clone(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: kind.into(),
        price: Some(100.0),
        ..Default::default()
    };
    core.apply_intent(OrderIntent::Submit(Box::new(req)), 0);

    assert_eq!(
        core.eng(1).client.submissions.len(),
        1,
        "{venue}/{kind}: a supported kind must still reach the addressed account's client"
    );
}

// -------------------------------------------------------------------------------------------
// THE REAL WIRE: a venue-tagged payload carries the CANONICAL venue, never a route key
// -------------------------------------------------------------------------------------------
//
// Every test above drives `route_event` with a payload whose `venue` field IS a route key, which is
// what the reconcile lane can now do (its payload carries both) and what a venue's own WS pump can
// NOT: a Binance fill says `"binance"`, and nothing on that wire says which binance account it
// belongs to. So with two accounts mounted, the venue lookup resolves the FIRST engine forever and
// the second account's fills fold into the first account's book — the exact defect
// `ExecutionEngine::route_key` exists to make impossible, reintroduced one layer up.
//
// `CoreThread::route_event`'s `multi_account` branch closes it with the two handles a payload CAN
// carry, in order of exactness: the client-order-id (resolved through the submit-time `coid_venue`
// map — exact for every order this process placed, and needing nothing on any wire) and then the
// SYMBOL, which answers only while exactly ONE engine of the venue claims it.
//
// ⚠ That last qualifier is a correction. This comment used to say the symbol was exact "because
// `vike_config::symbol_conflicts` refuses to ARM two active accounts of one venue on one symbol" —
// and that refusal is GONE: two accounts on one instrument is an ordinary spread. So the coid moved
// ahead of the symbol, and `engine_idx_for_venue_symbol` answers `None` on ambiguity rather than
// picking the first match.

/// The second account's own symbol — distinct from [`SYMBOL`], so the SYMBOL lane below has an
/// unambiguous answer to give. Two accounts sharing one symbol is now a legal mount; it simply
/// routes by coid instead, which the fill-lane test below covers.
const SYMBOL_B: &str = "ETHUSDT";

/// A fill as a VENUE actually emits one: tagged with the canonical venue, and with the symbol that
/// says which book it belongs to.
fn wire_fill(symbol: &str, trade_id: &'static str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        client_order_id: String::new(),
        venue: CANON.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: "BOTH".into(),
    })
}

/// Signed `BOTH` size held by the engine at `idx`, for an arbitrary (venue-key, symbol) pair.
fn held_at(core: &CoreThread<RecordingClient>, idx: usize, key: &str, symbol: &str) -> f64 {
    core.eng(idx)
        .account
        .positions
        .get(&(
            ustr::Ustr::from(key),
            ustr::Ustr::from(symbol),
            vike_model::events::PositionSide::Both,
        ))
        .map(|p| p.size)
        .unwrap_or(0.0)
}

/// Two accounts of one exchange, each on its OWN symbol — the configuration in which the SYMBOL
/// lane has an unambiguous answer. (`vike_mount::make_engine_accounts` will mount two accounts on
/// ONE symbol too, now that the collision rule is gone; that configuration routes by coid, and
/// `a_shared_symbol_routes_by_coid_not_by_first_match` below is its gate.)
fn two_accounts_on_two_symbols() -> CoreThread<RecordingClient> {
    core_of(
        // ⚠ Both `Account`s are seeded with the CANONICAL venue, which is what
        // `vike_mount::make_engine_for_account` does: it passes `venue` to `Account::new` and
        // decorates only `route_key`. `Account::apply_fill` asserts the fill's venue equals its
        // own, so seeding a route key here would make the harness reject the very wire payload it
        // exists to route. The two books stay distinct because their SYMBOLS differ.
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL_B))],
    )
}

/// **THE GATE: a wire fill reaches the account that trades its symbol**, even though the payload
/// names only the exchange.
#[test]
fn a_venue_tagged_fill_reaches_the_account_that_trades_its_symbol() {
    let mut core = two_accounts_on_two_symbols();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");

    let a = wire_fill(SYMBOL, "w-a", 3.0);
    let b = wire_fill(SYMBOL_B, "w-b", 7.0);
    assert_eq!(core.route_event(&a), 0, "the primary account's symbol");
    assert_eq!(core.route_event(&b), 1, "the second account's symbol");

    let (ia, ib) = (core.route_event(&a), core.route_event(&b));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 3.0);
    assert_eq!(held_at(&core, 1, CANON, SYMBOL_B), 7.0);
    assert_eq!(
        held_at(&core, 0, CANON, SYMBOL_B),
        0.0,
        "the second account's fill must not fold into the first account's book"
    );
}

/// THE NEGATIVE CONTROL — collapse the two accounts onto ONE symbol and a COID-LESS wire fill can
/// no longer be told apart, so both land on the primary.
///
/// ⚠ **This configuration is now LEGAL** (two accounts on one instrument is an ordinary spread —
/// `vike_config::venue_accounts`), so read this test for what it is: the residual, pinned. What is
/// unroutable here is a fill naming NO order of ours — a foreign fill, or one whose coid this
/// process never submitted — and reconcile is what covers that, not this lane. Every fill belonging
/// to an order this process placed carries a coid and routes exactly;
/// [`a_shared_symbol_routes_by_coid_not_by_first_match`] is that half.
///
/// It also still does the job it was written for: the harness demonstrably SEES the ambiguity when
/// the property the symbol branch rests on is removed.
#[test]
fn a_coid_less_wire_fill_on_a_shared_symbol_lands_on_the_default_account() {
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
    );
    // The SYMBOL lookup declines rather than guessing — two engines claim the pair.
    assert_eq!(core.engine_idx_for_venue_symbol(CANON, SYMBOL), None);

    let a = wire_fill(SYMBOL, "c-a", 3.0);
    let b = wire_fill(SYMBOL, "c-b", 7.0);
    assert_eq!(core.route_event(&a), 0);
    assert_eq!(core.route_event(&b), 0, "no coid, one symbol: the default account is the fallback");

    let (ia, ib) = (core.route_event(&a), core.route_event(&b));
    core.publish_to(ia, a);
    core.publish_to(ib, b);
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 10.0, "both fills in one book");
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 0.0);
}

/// **THE FIX for the case above: a fill naming one of OUR orders routes by its COID**, exactly,
/// with nothing added to any wire.
///
/// `coid_venue` records the routing index at SUBMIT time — when the engine was unambiguous, because
/// the submitting caller knew which account it was trading. So a venue-tagged fill on a symbol two
/// accounts share still finds its own book, and the symbol is never consulted. This is what lets the
/// symbol-collision refusal be deleted without reopening the one-book bug for real orders.
#[test]
fn a_shared_symbol_routes_by_coid_not_by_first_match() {
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
    );
    // …as `apply_intent_routed` records it for every order lowered onto a non-primary engine.
    core.coid_venue.insert("ours-on-b".to_string(), 1);

    let mut b = wire_fill(SYMBOL, "coid-b", 7.0);
    if let Event::Fill(f) = &mut b {
        f.client_order_id = "ours-on-b".to_string();
    }
    assert_eq!(core.route_event(&b), 1, "the SECOND account's own order's fill");

    let idx = core.route_event(&b);
    core.publish_to(idx, b);
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 7.0);
    assert_eq!(
        held_at(&core, 0, CANON, SYMBOL),
        0.0,
        "…and nothing of it reached the default account's book"
    );

    // A coid this process never submitted is NOT invented into an answer: it falls through to the
    // venue, which is where an unattributed fill has always gone.
    let mut foreign = wire_fill(SYMBOL, "coid-f", 1.0);
    if let Event::Fill(f) = &mut foreign {
        f.client_order_id = "somebody-elses".to_string();
    }
    assert_eq!(core.route_event(&foreign), 0);
}

/// **The INERTNESS half: a MULTI-VENUE, single-account process never takes the new branch at all.**
///
/// `multi_account` is what gates it, and this is the shape `vike_run::build_node` actually builds —
/// one engine per venue, no two sharing an exchange. The flag is false, so every venue-tagged
/// payload takes the identical `return` it always took, and the VENUE decides even where the symbol
/// would point elsewhere.
#[test]
fn one_account_per_venue_never_consults_the_symbol() {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue("bybit", "bybit", "bybit", SYMBOL_B))],
    );
    assert!(!core.multi_account, "two venues, one account each — not a multi-account process");

    // A BINANCE fill carrying the symbol only the BYBIT engine mounts still routes to binance: the
    // venue decides, and the symbol is not consulted.
    assert_eq!(core.route_event(&wire_fill(SYMBOL_B, "i-1", 2.0)), 0);

    // …and bybit's own fills still reach bybit.
    let mut b = wire_fill(SYMBOL_B, "i-2", 2.0);
    if let Event::Fill(f) = &mut b {
        f.venue = "bybit".into();
    }
    assert_eq!(core.route_event(&b), 1);
}

// -------------------------------------------------------------------------------------------
// THE PAYLOAD WITH NO SYMBOL: `Event::AccountState`
// -------------------------------------------------------------------------------------------
//
// The section above closes the two-account routing question with the SYMBOL, and that answer is
// exact for every venue-tagged payload that HAS one. `Event::AccountState` does not: it is an
// account-wide balance snapshot, so it fell through to the venue lookup and a second account's
// balances folded into the FIRST account's book — while that same account's fills and positions
// routed correctly, which is what made it easy to miss.
//
// The cure is a route key on the payload, stamped by the MOUNT
// (`vike_mount::account_event_sender` -> `vike_exec::EventSender::routed`) rather than by a bridge,
// because a venue adapter holds one credential set and cannot name an account. These tests drive
// `route_event` with exactly what that lane produces.

/// A balance snapshot as a VENUE emits one: canonical venue, no route key. What every bridge in
/// this tree pushes, and what a DEFAULT-account lane leaves untouched.
fn wire_account_state(balance: f64) -> Event {
    Event::AccountState(vike_model::events::AccountState {
        venue: CANON.into(),
        balances: vec![("USDT".to_string(), balance)],
        ts: 1,
        route_key: None,
    })
}

/// …and the same snapshot after a LABELLED account's lane has stamped it.
fn stamped_account_state(route_key: &str, balance: f64) -> Event {
    let mut ev = wire_account_state(balance);
    if let Event::AccountState(a) = &mut ev {
        a.route_key = Some(route_key.into());
    }
    ev
}

fn balance_at(core: &CoreThread<RecordingClient>, idx: usize) -> f64 {
    core.eng(idx).account.balance
}

/// **THE GATE: a second account's balance snapshot reaches ITS OWN engine.**
///
/// The payload names no symbol, so the disambiguator the fill tests rest on cannot fire — the key
/// it carries is the whole of the answer.
#[test]
fn a_stamped_account_state_reaches_its_own_account() {
    let mut core = two_accounts_on_two_symbols();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");

    let a = wire_account_state(1_000.0);
    let b = stamped_account_state(ACCOUNT_B, 7_000.0);
    assert_eq!(core.route_event(&a), 0, "the default account's snapshot: unstamped, venue-routed");
    assert_eq!(core.route_event(&b), 1, "the second account's snapshot: routed by its stamped key");

    let (ia, ib) = (core.route_event(&a), core.route_event(&b));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(balance_at(&core, 0), 1_000.0);
    assert_eq!(balance_at(&core, 1), 7_000.0);
}

/// THE NEGATIVE CONTROL — strip the stamp and the bug is back, in this exact harness.
///
/// It is what makes the gate above a verdict rather than a coincidence: with both snapshots
/// unstamped the venue lookup answers `0` for both, the second account's engine never sees its own
/// balance, and the first account's book ends up holding the SECOND account's money.
#[test]
fn removing_the_stamp_reproduces_the_wrong_book_bug() {
    let mut core = two_accounts_on_two_symbols();
    let a = wire_account_state(1_000.0);
    let b = wire_account_state(7_000.0);
    assert_eq!(core.route_event(&a), 0);
    assert_eq!(core.route_event(&b), 0, "no symbol, no key: the second account is unreachable");

    let (ia, ib) = (core.route_event(&a), core.route_event(&b));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(balance_at(&core, 0), 7_000.0, "the second account's money in the first book");
    assert_eq!(balance_at(&core, 1), 0.0, "…and its own book never saw a balance at all");
}

/// **The INERTNESS half: a single-account process never sees a stamped payload at all**, because
/// nothing stamps a key equal to its own venue (`vike_exec::EventSender::routed`). Driving the
/// unstamped payload every such box produces takes the identical branch it always took.
#[test]
fn a_single_account_process_routes_account_state_exactly_as_before() {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue("bybit", "bybit", "bybit", SYMBOL_B))],
    );
    assert!(!core.multi_account, "two venues, one account each — not a multi-account process");

    assert_eq!(core.route_event(&wire_account_state(500.0)), 0);
    let mut bybit = wire_account_state(500.0);
    if let Event::AccountState(a) = &mut bybit {
        a.venue = "bybit".into();
    }
    assert_eq!(core.route_event(&bybit), 1);
}

/// A key naming NO mounted engine falls through to the venue lookup rather than being dropped —
/// the same unknown-key affordance `engine_idx_for_route_key` has everywhere else. The ENGINE then
/// refuses it (`ExecutionEngine::on_event`'s route-key filter), so the stray snapshot changes no
/// book; this pins the ROUTER half of that pair.
#[test]
fn an_unmountable_route_key_falls_through_to_the_venue_lookup() {
    let core = two_accounts_on_two_symbols();
    assert_eq!(core.route_event(&stamped_account_state("binance#never-mounted", 5.0)), 0);
}

// -------------------------------------------------------------------------------------------
// THE OTHER TWO PAYLOADS WITH NO CLIENT-ORDER-ID: `Funding` and `PositionLiquidated`
// -------------------------------------------------------------------------------------------
//
// The `AccountState` section above was written on the claim that it is "the one payload with
// NEITHER a coid nor a symbol to fall back on". The first half of that is right and the second half
// is a trap: `Event::Funding` and `Event::PositionLiquidated` carry a SYMBOL and no coid, and the
// symbol stopped being an account key the moment two accounts of one venue were allowed onto one
// instrument — which is the whole point of the mount `account` field, so it is not a corner case
// but the supported configuration.
//
// So on a SHARED symbol both of them fell through `engine_idx_for_venue_symbol`'s ambiguity `None`
// to the venue lookup, and landed on the venue's DEFAULT engine whichever account they belonged to:
// a labelled account's funding debit on the default account's `balance`, and a labelled account's
// liquidation CLOSING the default account's position at the venue's liq price while the account
// that was actually liquidated went on reporting the position open. Both books wrong, silently, and
// nothing about a strategy's own order flow is involved — the coid lane above cannot help.
//
// They are stamped now, by the same mount lane that stamps `AccountState`
// (`vike_exec::EventSender::routed`). These tests drive the SHARED-SYMBOL harness deliberately: on
// two symbols the old code would have routed correctly by accident.

/// Two accounts of one exchange on ONE symbol — the ordinary spread the deleted collision rule
/// refused, and the configuration in which the symbol answers nothing.
fn two_accounts_on_one_symbol() -> CoreThread<RecordingClient> {
    core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
    )
}

/// A funding payment as a VENUE emits one: canonical venue, a symbol, no coid, no route key.
fn wire_funding(amount: f64) -> Event {
    Event::Funding(vike_model::events::FundingEvent {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        position_side: vike_model::events::PositionSide::Both,
        funding_rate: 0.0001,
        amount,
        mark_price: None,
        ts: 1,
        route_key: None,
    })
}

fn stamped_funding(route_key: &str, amount: f64) -> Event {
    let mut ev = wire_funding(amount);
    if let Event::Funding(f) = &mut ev {
        f.route_key = Some(route_key.into());
    }
    ev
}

/// A liquidation as a VENUE emits one. `trade_id` is the per-engine dedup key and is deliberately
/// NOT a routing handle — see `PositionLiquidated::route_key`.
fn wire_liquidation(qty: f64, trade_id: &str) -> Event {
    Event::PositionLiquidated(vike_model::events::PositionLiquidated {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        position_side: vike_model::events::PositionSide::Both,
        qty,
        liq_price: 100.0,
        fee: 0.0,
        ts: 1,
        trade_id: trade_id.into(),
        route_key: None,
    })
}

fn stamped_liquidation(route_key: &str, qty: f64, trade_id: &str) -> Event {
    let mut ev = wire_liquidation(qty, trade_id);
    if let Event::PositionLiquidated(p) = &mut ev {
        p.route_key = Some(route_key.into());
    }
    ev
}

fn funding_paid_at(core: &CoreThread<RecordingClient>, idx: usize) -> f64 {
    core.eng(idx).account.funding_paid
}

/// **THE GATE: a second account's funding payment reaches ITS OWN engine, on a shared symbol.**
#[test]
fn a_stamped_funding_payment_reaches_the_account_that_paid_it() {
    let mut core = two_accounts_on_one_symbol();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");
    // The premise: the symbol answers NOTHING here, so the stamp is the whole of the answer.
    assert_eq!(core.engine_idx_for_venue_symbol(CANON, SYMBOL), None);

    let a = wire_funding(-1.0);
    let b = stamped_funding(ACCOUNT_B, -8.0);
    assert_eq!(core.route_event(&a), 0, "the default account's payment: unstamped, venue-routed");
    assert_eq!(core.route_event(&b), 1, "the second account's payment: routed by its stamped key");

    let (ia, ib) = (core.route_event(&a), core.route_event(&b));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(funding_paid_at(&core, 0), -1.0);
    assert_eq!(funding_paid_at(&core, 1), -8.0, "the labelled account's own book");
}

/// THE NEGATIVE CONTROL for it — strip the stamp and both payments land in one book, which is
/// exactly what shipped before this: the default account absorbs a debit it never incurred.
#[test]
fn an_unstamped_funding_payment_on_a_shared_symbol_lands_on_the_default_account() {
    let mut core = two_accounts_on_one_symbol();
    let a = wire_funding(-1.0);
    let b = wire_funding(-8.0);
    assert_eq!(core.route_event(&a), 0);
    assert_eq!(
        core.route_event(&b),
        0,
        "no key, no coid, an ambiguous symbol: the default account"
    );

    let (ia, ib) = (core.route_event(&a), core.route_event(&b));
    core.publish_to(ia, a);
    core.publish_to(ib, b);
    assert_eq!(funding_paid_at(&core, 0), -9.0, "both debits on one balance");
    assert_eq!(funding_paid_at(&core, 1), 0.0, "…and the account that paid one saw nothing");
}

/// **The liquidation twin, and the one that moves a POSITION.** The labelled account's engine holds
/// the position; the liquidation must close ITS book, not the default account's.
#[test]
fn a_stamped_liquidation_closes_the_account_that_was_liquidated() {
    let mut core = two_accounts_on_one_symbol();
    // Both accounts are long the same instrument — the spread's own shape, and the state in which a
    // misrouted liquidation is indistinguishable from a real one.
    let a_fill = wire_fill(SYMBOL, "liq-seed-a", 5.0);
    let mut b_fill = wire_fill(SYMBOL, "liq-seed-b", 5.0);
    if let Event::Fill(f) = &mut b_fill {
        f.client_order_id = "ours-on-b".to_string();
    }
    core.coid_venue.insert("ours-on-b".to_string(), 1);
    let (ia, ib) = (core.route_event(&a_fill), core.route_event(&b_fill));
    assert_eq!((ia, ib), (0, 1), "the seed fills must land one per book");
    core.publish_to(ia, a_fill);
    core.publish_to(ib, b_fill);
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 5.0);
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 5.0);

    let liq = stamped_liquidation(ACCOUNT_B, 5.0, "liq-b");
    assert_eq!(core.route_event(&liq), 1);
    let idx = core.route_event(&liq);
    core.publish_to(idx, liq);

    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 0.0, "the liquidated account is flat");
    assert_eq!(
        held_at(&core, 0, CANON, SYMBOL),
        5.0,
        "…and the account that was NOT liquidated still holds its position"
    );
}

/// THE NEGATIVE CONTROL — unstamped, the same liquidation flattens the WRONG book and leaves the
/// liquidated account reporting a position the venue has already closed. This is the shipped
/// behaviour this section exists to remove.
#[test]
fn an_unstamped_liquidation_on_a_shared_symbol_flattens_the_default_account() {
    let mut core = two_accounts_on_one_symbol();
    let a_fill = wire_fill(SYMBOL, "u-seed-a", 5.0);
    let mut b_fill = wire_fill(SYMBOL, "u-seed-b", 5.0);
    if let Event::Fill(f) = &mut b_fill {
        f.client_order_id = "ours-on-b".to_string();
    }
    core.coid_venue.insert("ours-on-b".to_string(), 1);
    let (ia, ib) = (core.route_event(&a_fill), core.route_event(&b_fill));
    core.publish_to(ia, a_fill);
    core.publish_to(ib, b_fill);

    let liq = wire_liquidation(5.0, "u-liq");
    assert_eq!(core.route_event(&liq), 0, "unstamped: the venue's default engine");
    let idx = core.route_event(&liq);
    core.publish_to(idx, liq);
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 0.0, "the wrong book was flattened");
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 5.0, "…and the liquidated one still looks open");
}

/// **The ENGINE-side backstop**, the twin of the `AccountState` filter: even handed the payload
/// directly, an engine refuses one stamped for a different account. This is what makes the router
/// the only thing that has to be right, rather than the only thing that CAN be right — and it is
/// the half `engine_idx_for_route_key`'s unknown-key affordance needs, since a key naming no
/// mounted engine falls through to the venue lookup and would otherwise be folded there.
#[test]
fn an_engine_refuses_a_funding_or_liquidation_stamped_for_another_account() {
    let mut core = two_accounts_on_one_symbol();
    // Deliberately published to engine 0 — the answer the OLD router gave.
    core.publish_to(0, stamped_funding(ACCOUNT_B, -8.0));
    assert_eq!(funding_paid_at(&core, 0), 0.0, "the default account refused a labelled debit");

    let seed = wire_fill(SYMBOL, "bs-seed", 5.0);
    core.publish_to(0, seed);
    core.publish_to(0, stamped_liquidation(ACCOUNT_B, 5.0, "bs-liq"));
    assert_eq!(
        held_at(&core, 0, CANON, SYMBOL),
        5.0,
        "the default account refused a labelled liquidation and kept its position"
    );
    // …and an UNSTAMPED payload still folds exactly as it always did, which is what keeps every
    // single-account box byte-identical.
    core.publish_to(0, wire_funding(-1.0));
    assert_eq!(funding_paid_at(&core, 0), -1.0);
}

/// **The INERTNESS half**: nothing stamps a key equal to its own venue, so a single-account process
/// drives the identical unstamped payloads it always drove and takes the identical branch.
#[test]
fn a_single_account_process_routes_funding_and_liquidation_exactly_as_before() {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue("bybit", "bybit", "bybit", SYMBOL_B))],
    );
    assert!(!core.multi_account, "two venues, one account each — not a multi-account process");

    assert_eq!(core.route_event(&wire_funding(-1.0)), 0);
    assert_eq!(core.route_event(&wire_liquidation(1.0, "s-1")), 0);
    let mut bybit = wire_funding(-1.0);
    if let Event::Funding(f) = &mut bybit {
        f.venue = "bybit".into();
        f.symbol = SYMBOL_B.into();
    }
    assert_eq!(core.route_event(&bybit), 1);
}
