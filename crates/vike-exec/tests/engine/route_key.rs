//! `ExecutionEngine::route_key` vs `ExecutionEngine::venue`: the gate over the `"binance#2"` trap.
//!
//! The two fields answer two unrelated questions:
//!
//! * **which engine** (`route_key`): `vike_core`'s `CoreThread::engine_idx_for_route_key` matches a
//!   fill against it to pick the book it folds into;
//! * **what does this venue support** (`venue`): it keys every per-venue capability table
//!   (`vike_model::caps_for`, `vike_model::amend_semantics`, `vike_model::fee_schedule_for`,
//!   `vike_model::venues::venue_tif::venue_tif`, `vike_model::venues::venue_margin_support`).
//!
//! One field doing both cannot express two accounts of one venue. Label both engines `"binance"`
//! and routing returns the FIRST match forever: both accounts fold into one book, and under
//! `hybrid` `PositionDrift` rewrites the local position onto whichever account answered last.
//! Label the second `"binance#2"` and every capability lookup misses: `caps_for` answers
//! `VenueCaps::UNSUPPORTED` (preflight rejects every order: loud, survivable) and `amend_semantics`
//! answers `AmendSemantics::Unknown`, which silently changes whether a modify's quantity is the
//! order's new TOTAL or its new REMAINING size.
//!
//! [`the_route_key_defaults_to_the_canonical_venue`] is the inertness claim. The rest decorate the
//! route key and assert the capability plane does not notice:
//! [`a_decorated_route_key_never_reaches_a_capability_table`] is the exhaustive form over
//! `vike_model::VENUES`, and [`a_decorated_route_key_does_not_change_the_amend_arithmetic`] drives
//! the REAL gate through `ExecutionEngine::modify_order` and measures the admit/deny boundary, so
//! it fails if the amend fallback is ever keyed on the route key (no type-check can see that).

use vike_exec::testing::RecordingClient;
use vike_exec::{EventBus, ExecutionEngine, Fold, Outbox, RiskLimits};
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderPartiallyFilled, OrderSubmitted};
use vike_model::{AmendSemantics, OrderRequest, VENUES, VenueCaps};

use crate::support::EngineBuilder;

/// The suffix a second account of one venue would take. Its exact spelling does not matter; that it
/// is NOT a roster id is the whole point, and [`the_decoration_is_not_a_roster_id`] pins that.
const SUFFIX: &str = "#2";

/// A venue whose amend convention is `InPlaceTotal` — the only variant that nets executed lots out
/// of the gate's projection, so it is the one whose loss to `Unknown` is measurable.
const IN_PLACE: &str = "binance";

const SYMBOL: &str = "BTCUSDT";
const RESTING_QTY: f64 = 10.0;
const EXECUTED: f64 = 4.0;
const PX: f64 = 100.0;
/// Chosen so the resting order is comfortably admitted at submit, making a refused re-quote a
/// contradiction rather than a configuration (the `partial_fill_amend_accounting.rs` idiom).
const EXPOSURE_CAP: f64 = 1_200.0;

/// An engine labelled `venue`, with `route_key` left at the default (`== venue`).
fn engine(venue: &str, limits: RiskLimits) -> ExecutionEngine<RecordingClient> {
    EngineBuilder { venue: venue.into(), symbol: SYMBOL.into(), limits, ..Default::default() }
        .build()
}

/// The same engine as [`engine`], with the route key DECORATED — the second-account shape.
/// `Account` keeps the canonical venue, exactly as a real second account would: its position keys
/// are compared against this engine's own `venue` and against `OrderRequest::venue`, both canonical.
fn engine_with_decorated_route_key(
    venue: &str,
    limits: RiskLimits,
) -> ExecutionEngine<RecordingClient> {
    let mut e = engine(venue, limits);
    e.route_key = format!("{venue}{SUFFIX}");
    e
}

fn limit(venue: &str, coid: &str, qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: venue.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        ts: 1,
        ..Default::default()
    }
}

fn fill(venue: &str, coid: &str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: "t1".into(),
        client_order_id: coid.to_string(),
        venue: venue.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        ts: 0,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    }
}

/// Rest one order through the real emitter split, then deliver ONE partial execution as a venue
/// lane does: the bare `Event::Fill` the ACCOUNT folds, then the `OrderPartiallyFilled` WRAP the
/// FSM folds. They land on opposite sides of the gate's sum, the arithmetic the amend convention
/// decides.
fn rested_and_partly_filled(
    mut e: ExecutionEngine<RecordingClient>,
    venue: &str,
) -> ExecutionEngine<RecordingClient> {
    let req = limit(venue, "c1", RESTING_QTY, PX);
    let before = e.client.submissions.len();
    let mut outbox = Outbox::default();
    e.submit_order(&req, 0, &mut outbox);
    assert_eq!(
        e.client.submissions.len(),
        before + 1,
        "precondition: the order itself must be admitted, or the measurement below proves nothing"
    );
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".into(), ts: 0 }),
        &mut e,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        &mut e,
    );
    let f = fill(venue, "c1", EXECUTED, PX);
    bus.publish(Event::Fill(f.clone()), &mut e);
    bus.publish(
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: "c1".into(),
            fill: f,
            ts: 2,
        }),
        &mut e,
    );
    assert_eq!(
        e.position_size("BOTH"),
        EXECUTED,
        "precondition: the partial execution must have folded, or the boundary below is not about it"
    );
    e
}

/// The largest amend TOTAL this engine still lets reach the venue — PROBED through the real
/// `modify_order` + `RiskGate`, never asserted, so the number cannot be "corrected" to match a
/// broken implementation. `InPlaceTotal` nets `EXECUTED` out of the projection and so admits
/// exactly that much more than a convention that does not.
fn exposure_boundary(e: impl Fn() -> ExecutionEngine<RecordingClient>, venue: &str) -> f64 {
    let mut last_admitted = 0.0;
    for total in 1..=30 {
        let total = f64::from(total);
        let mut eng = rested_and_partly_filled(e(), venue);
        let before = eng.client.modifies.len();
        let mut outbox = Outbox::default();
        eng.modify_order("c1", Some(total), None, 5, &mut outbox);
        if eng.client.modifies.len() > before {
            last_admitted = total;
        }
    }
    last_admitted
}

fn capped() -> RiskLimits {
    RiskLimits { max_total_exposure: Some(EXPOSURE_CAP), ..RiskLimits::new() }
}

// --- The premises: each keys on something INDEPENDENT of the split, so none can go vacuous ---

/// The decoration must not accidentally BE a roster id, or every assertion below is comparing a
/// venue to itself and passing for the wrong reason.
#[test]
fn the_decoration_is_not_a_roster_id() {
    for v in VENUES {
        let decorated = format!("{v}{SUFFIX}");
        assert!(
            !VENUES.contains(&decorated.as_str()),
            "{decorated} is itself a roster venue — pick another suffix"
        );
        assert_eq!(
            vike_model::caps_for(&decorated),
            VenueCaps::UNSUPPORTED,
            "the decorated key must be a capability-table MISS, or the trap it stands for is not \
             reproduced here"
        );
    }
}

/// `IN_PLACE` must be what the real table says, or the boundary test below measures a venue class
/// it does not name.
#[test]
fn the_amend_venue_is_what_the_table_says() {
    assert_eq!(vike_model::amend_semantics(IN_PLACE), AmendSemantics::InPlaceTotal);
    assert_eq!(
        vike_model::amend_semantics(&format!("{IN_PLACE}{SUFFIX}")),
        AmendSemantics::Unknown,
        "…and the decorated key resolves to the SILENT wrong answer — the hazard being gated"
    );
}

// --- THE GATES ---

/// INERTNESS: `ExecutionEngine::new` seeds `route_key` from `venue`, so a fresh engine is the
/// single-account shape. Exhaustive over the roster so a venue cannot be onboarded into a different
/// default.
#[test]
fn the_route_key_defaults_to_the_canonical_venue() {
    for v in VENUES {
        let e = engine(v, RiskLimits::new());
        assert_eq!(
            e.route_key, e.venue,
            "a freshly built {v} engine must be the single-account shape"
        );
        assert_eq!(e.route_key, *v);
    }
}

/// For EVERY roster venue, an engine whose route key is decorated still resolves its real
/// capability row (the tables key on `venue`). The negative half is load-bearing: the two fields
/// are NOT interchangeable, so moving a capability lookup onto the routing key reddens this.
#[test]
fn a_decorated_route_key_never_reaches_a_capability_table() {
    for v in VENUES {
        let e = engine_with_decorated_route_key(v, RiskLimits::new());
        assert_ne!(e.route_key, e.venue, "precondition: this engine is the two-account shape");

        assert_eq!(
            vike_model::caps_for(&e.venue),
            vike_model::caps_for(v),
            "{v}: the canonical field must still resolve {v}'s real row"
        );
        assert_ne!(
            vike_model::caps_for(&e.venue),
            VenueCaps::UNSUPPORTED,
            "{v}: a roster venue's row is never UNSUPPORTED — preflight would reject every order"
        );
        assert_eq!(
            vike_model::amend_semantics(&e.venue),
            vike_model::amend_semantics(v),
            "{v}: the amend convention must still be {v}'s"
        );
        assert_eq!(
            vike_model::fee_schedule_for(&e.venue),
            vike_model::fee_schedule_for(v),
            "{v}: the fee schedule must still be {v}'s"
        );

        // …and the route key is NOT a capability key. Both tables must MISS on it, which is what
        // makes decorating `venue` instead of `route_key` the bug this split exists to prevent.
        assert_eq!(vike_model::caps_for(&e.route_key), VenueCaps::UNSUPPORTED, "{v}");
        assert_eq!(vike_model::amend_semantics(&e.route_key), AmendSemantics::Unknown, "{v}");
    }
}

/// THE SITE THAT LOOKS LIKE ROUTING AND IS NOT: `on_event`'s `Event::AccountState` arm drops a
/// frame whose `venue` is not this engine's, a filter applied AFTER `vike_core`'s `route_event`
/// chose the engine. It must read `venue`: `AccountState::venue` is minted by a venue adapter and
/// is always a canonical roster id, so comparing it against a route key would silently drop every
/// account frame of a decorated engine, authoritative balance included. Pinned from the decorated
/// shape, the only one where the two fields differ.
#[test]
fn a_decorated_route_key_still_folds_its_venues_account_state() {
    let mut e = engine_with_decorated_route_key(IN_PLACE, RiskLimits::new());
    assert_ne!(e.route_key, e.venue, "precondition: this engine is the two-account shape");
    let before = e.account.balance;

    let mut bus = EventBus::new();
    bus.publish(
        Event::AccountState(vike_model::events::AccountState {
            venue: IN_PLACE.into(),
            balances: vec![("USDT".to_string(), 4_242.0)],
            ts: 1,
            route_key: None,
        }),
        &mut e,
    );

    assert_ne!(e.account.balance, before, "the account frame must have folded, not been dropped");
    assert_eq!(
        e.account.balance, 4_242.0,
        "an AccountState labelled with the CANONICAL venue belongs to this engine, whatever its \
         route key — comparing it against route_key would drop it and strand the balance"
    );
}

/// …and the SECOND filter on that arm, which IS about routing: a balance snapshot stamped with
/// another account's route key (`vike_mount::account_event_sender` stamps it) is refused, though it
/// carries this engine's canonical venue. The backstop for the case `vike_core`'s router cannot
/// serve: a key naming no mounted engine falls through to the venue lookup and lands on the DEFAULT
/// account, whose authoritative balance a labelled account's money would otherwise overwrite.
#[test]
fn an_account_state_addressed_to_another_account_is_refused() {
    let mut e = engine_with_decorated_route_key(IN_PLACE, RiskLimits::new());
    let before = e.account.balance;

    let mut bus = EventBus::new();
    let fold = bus.publish(
        Event::AccountState(vike_model::events::AccountState {
            venue: IN_PLACE.into(),
            balances: vec![("USDT".to_string(), 9_999.0)],
            ts: 1,
            // a THIRD account of the same exchange — canonical venue, someone else's key
            route_key: Some(format!("{IN_PLACE}#other").into()),
        }),
        &mut e,
    );

    assert_eq!(fold, Fold::Dropped, "a stamped frame for another account must not fold here");
    assert_eq!(e.account.balance, before, "…and must leave this account's balance untouched");
}

/// The other half: a frame stamped with THIS engine's own key folds, so the filter above cannot
/// pass by dropping every stamped frame (which would strand a labelled account's balance forever).
#[test]
fn an_account_state_addressed_to_this_account_folds() {
    let mut e = engine_with_decorated_route_key(IN_PLACE, RiskLimits::new());
    let own_key = e.route_key.clone();

    let mut bus = EventBus::new();
    let fold = bus.publish(
        Event::AccountState(vike_model::events::AccountState {
            venue: IN_PLACE.into(),
            balances: vec![("USDT".to_string(), 4_242.0)],
            ts: 1,
            route_key: Some(own_key.as_str().into()),
        }),
        &mut e,
    );

    assert_eq!(fold, Fold::Applied);
    assert_eq!(e.account.balance, 4_242.0);
}

/// THE MONEY SITE, END TO END: `ExecutionEngine::modify_order` falls back to
/// `vike_model::amend_semantics(&self.venue)` when the client declines, deciding whether executed
/// lots are netted out of the gate's projection. Measured as a DIFFERENCE through the real gate:
///
/// * a decorated engine must admit EXACTLY what an undecorated one admits — the split changed
///   nothing; and
/// * that must be `EXECUTED` MORE than an engine whose venue genuinely resolves `Unknown`, which is
///   what proves the measurement can tell the two conventions apart at all.
///
/// Flip that fallback to `self.route_key` and the first assertion fails by exactly `EXECUTED`.
#[test]
fn a_decorated_route_key_does_not_change_the_amend_arithmetic() {
    let plain = exposure_boundary(|| engine(IN_PLACE, capped()), IN_PLACE);
    let decorated =
        exposure_boundary(|| engine_with_decorated_route_key(IN_PLACE, capped()), IN_PLACE);

    // The control: `""` is on no roster, so it resolves `Unknown`, and it is NOT derived from the
    // fields under test, so this stays a real measurement even if the split is mutated.
    let unknown = exposure_boundary(|| engine("", capped()), "");
    assert_eq!(
        vike_model::amend_semantics(""),
        AmendSemantics::Unknown,
        "precondition: the control venue must resolve Unknown"
    );

    assert!(unknown > 0.0, "the control must admit SOME amend, or nothing is measured");
    assert_eq!(
        plain - unknown,
        EXECUTED,
        "precondition: the probe must be able to SEE the convention — an InPlaceTotal venue admits \
         exactly the executed qty more than an Unknown one (plain {plain}, unknown {unknown})"
    );
    assert_eq!(
        decorated,
        plain,
        "a decorated route key must not move the amend boundary: the convention is keyed on the \
         CANONICAL venue. decorated {decorated} vs plain {plain} — a gap of {} says the fallback \
         is reading route_key",
        plain - decorated
    );
}

/// `new` builds a simulated book, so it says PAPER. Only the mount raises the mode (the Trade
/// window design, §4.3).
#[test]
fn a_new_engine_says_paper() {
    let e = vike_exec::ExecutionEngine::new(
        vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta),
        vike_exec::RiskGate::new(vike_exec::RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    assert_eq!(e.mode, vike_exec::EngineMode::Paper);
}
