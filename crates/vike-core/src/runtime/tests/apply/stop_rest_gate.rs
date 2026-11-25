//! The part of a fired stop past its position OPENS: it faces every ceiling an opening order faces, on the position it
//! really produces, and the flagged close in front of it is never held up. The gate at the send cannot judge it, because
//! it still reads the whole position (`CoreThread::submit_fired`).

use super::*;

/// A core whose engine holds a mark of 100 for its symbol.
fn priced() -> CoreThread<RecordingClient> {
    let mut c = test_core();
    c.engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 0);
    c
}

/// `c` long `position` at 100, with a sell stop of `stop_quantity` armed and fired, and the stop spent whatever was sent.
fn fired(
    mut c: CoreThread<RecordingClient>,
    position: f64,
    stop_quantity: f64,
) -> CoreThread<RecordingClient> {
    c.engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
        vike_execution::PositionEntry {
            size: position,
            average_price: 100.0,
            ..Default::default()
        },
    );
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: stop_quantity,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
            kind_terms: None,
            account: None,
        }),
        0,
    );
    c.fire_conditionals_at_price("sim", "BTCUSDT", 90.0, 1);
    assert!(
        c.emulated.cond_engine.is_empty()
            && c.emulated.order_emulation_arms.values().all(|b| b.is_empty())
    );
    c
}

fn sent(c: &CoreThread<RecordingClient>) -> Vec<(f64, bool)> {
    c.engine.client.submissions.iter().map(|r| (r.qty, r.reduce_only)).collect()
}

/// The close alone went, the rest was minted nothing, and the ring says what refused it.
fn assert_rest_refused(c: &CoreThread<RecordingClient>, close: f64, word: &str) {
    assert_eq!(sent(c), vec![(close, true)], "{word}: {:?}", c.recent);
    assert_eq!(c.engine.registry.len(), 1, "{word}: the refused rest has no order");
    assert!(
        c.recent.iter().any(|n| n.contains("NOT sent") && n.contains(word)),
        "{word}: {:?}",
        c.recent
    );
}

/// **Long 5, stop 10, the rest 5.** The gate at the send reads `is_covered_reduce(-1, 5, 5)` and skips each of these,
/// and `over-max-exposure` projects 0, while the order opens short 5.
#[test]
fn a_rest_the_size_of_the_position_faces_every_ceiling_of_an_opening_order() {
    type Set = fn(&mut Engine<RecordingClient>);
    let rows: [(&str, Set); 7] = [
        ("over-max-notional", |e| e.gate.limits.max_notional_per_order = Some(400.0)),
        ("below-min-qty", |e| e.gate.limits.min_qty = Some(6.0)),
        ("below-min-notional", |e| e.gate.limits.min_notional = Some(600.0)),
        ("over-max-exposure", |e| e.gate.limits.max_total_exposure = Some(400.0)),
        ("over-account-exposure", |e| e.gate.limits.max_account_exposure = Some(400.0)),
        // The close frees the position's 50, so the rest's 50 meets an equity of 40 and no more: read with the reversal's
        // whole credit of 100 it would pass.
        ("insufficient-margin", |e| {
            e.gate.limits.initial_margin_requirement = Some(0.1);
            e.equity_seed = 40.0;
        }),
        ("quote-rate-stale", |e| {
            e.set_account_currency("EUR").unwrap();
            e.declare_mount_quote_currency("USD").unwrap();
        }),
    ];
    for (word, set) in rows {
        let mut c = priced();
        set(&mut c.engine);
        assert_rest_refused(&fired(c, 5.0, 10.0), 5.0, word);
    }
}

/// The mark floor's row: an engine nobody priced values the rest at nothing, so only the floor can stop it.
#[test]
fn a_rest_on_an_unpriced_mount_meets_the_mark_floor() {
    let mut c = test_core();
    c.engine.gate.require_mark = true;
    assert_rest_refused(&fired(c, 5.0, 10.0), 5.0, "no-mark-for-mounted-symbol");
}

/// **Long 1, stop 3, the rest 2.** Not covered, so the gate at the send runs its lanes, but it projects long 1 less 2, a short
/// of 1, while the order ends short 2: a cap of 1.5 passed it.
#[test]
fn a_rest_larger_than_the_position_is_judged_on_the_short_it_makes() {
    let capped = |cap: f64| {
        let mut c = priced();
        c.engine.gate.limits.max_total_exposure = Some(cap);
        fired(c, 1.0, 3.0)
    };
    assert_rest_refused(&capped(150.0), 1.0, "over-max-exposure");
    assert_eq!(sent(&capped(200.0)), vec![(1.0, true), (2.0, false)]);
}

/// **The other side of each ceiling above: the rest is judged on its own size, from flat.** Every number is exactly at its
/// limit, so the whole order of 10 (a notional of 1000) or the position still counted as held would be refused.
#[test]
fn a_rest_inside_every_ceiling_goes_behind_its_close() {
    let mut c = priced();
    c.engine.gate.require_mark = true;
    c.engine.equity_seed = 50.0;
    let limits = &mut c.engine.gate.limits;
    limits.max_notional_per_order = Some(500.0);
    limits.min_qty = Some(5.0);
    limits.min_notional = Some(500.0);
    limits.max_total_exposure = Some(500.0);
    limits.max_account_exposure = Some(500.0);
    limits.initial_margin_requirement = Some(0.1);
    let c = fired(c, 5.0, 10.0);
    assert_eq!(sent(&c), vec![(5.0, true), (5.0, false)], "{:?}", c.recent);
}
