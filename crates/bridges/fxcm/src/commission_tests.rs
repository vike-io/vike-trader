//! The close side of a commission, over the vendor's own worked examples (the SDK's "Trading Commissions" article: EUR/USD at
//! $0.00008 per unit "any deal", 10K units: open 0.40, close 0.40, total 0.80) and the shapes the rule must never get wrong.

use super::*;
use serde_json::json;

/// 10K EUR/USD opened, the open row read `0.40` (half of the schedule's `0.80`).
const OPEN: Option<(f64, f64)> = Some((10_000.0, 0.40));

fn envelope(kind: &str, trade_id: &str, amount: i64, commission: f64) -> Value {
    json!({"kind": kind, "order_id": "o", "trade_id": trade_id, "instrument": "EUR/USD", "side": "S",
           "amount": amount, "rate": 1.1, "commission": commission, "ts": 0})
}

fn commission_of(v: &Value) -> f64 {
    v["commission"].as_f64().expect("a number")
}

#[test]
fn a_schedule_that_pays_half_at_each_deal_books_the_other_half_at_the_close() {
    // The closed row reads the position's total (0.80): the open fill booked 0.40, the close owes the rest.
    assert_eq!(owed_at_close(0.80, OPEN, 10_000.0), 0.40);
}

#[test]
fn a_schedule_that_pays_at_the_close_books_the_whole_figure_there() {
    // Nothing was charged at the open (`getCommission` reads 0.0 "if the commission has not been charged yet").
    assert_eq!(owed_at_close(0.80, Some((10_000.0, 0.0)), 10_000.0), 0.80);
}

#[test]
fn a_schedule_that_pays_at_the_open_books_nothing_more_at_the_close() {
    assert_eq!(owed_at_close(0.80, Some((10_000.0, 0.80)), 10_000.0), 0.0);
}

#[test]
fn a_part_close_takes_only_its_share_of_the_open_fill() {
    // 2000 units opened (open half 0.08 at $0.00004 a unit), 1000 closed: the closed row reads the total of THOSE 1000 units
    // (0.08), of which the open share is half of 0.08.
    assert_eq!(owed_at_close(0.08, Some((2_000.0, 0.08)), 1_000.0), 0.04);
}

#[test]
fn a_figure_below_the_open_share_owes_nothing_and_never_a_rebate() {
    // If a closed row carried the CLOSE side alone, the rule must not turn the difference into a credit.
    assert_eq!(owed_at_close(0.20, OPEN, 10_000.0), 0.0);
    assert_eq!(
        owed_at_close(0.40, OPEN, 10_000.0),
        0.0,
        "an equal figure is the open share exactly"
    );
}

#[test]
fn no_charge_no_open_fill_and_nonsense_all_owe_nothing() {
    assert_eq!(owed_at_close(0.0, OPEN, 10_000.0), 0.0, "the demo: nothing is charged");
    assert_eq!(
        owed_at_close(-0.80, OPEN, 10_000.0),
        0.0,
        "a negative closed figure is not turned into a charge"
    );
    assert_eq!(owed_at_close(f64::NAN, OPEN, 10_000.0), 0.0);
    assert_eq!(
        owed_at_close(0.80, None, 10_000.0),
        0.0,
        "never the whole figure when the open side may be booked"
    );
    assert_eq!(
        owed_at_close(0.80, Some((0.0, 0.40)), 10_000.0),
        0.0,
        "an open fill of no size has no share"
    );
}

#[test]
fn a_close_of_more_than_the_open_fill_takes_the_whole_open_share_and_no_more() {
    assert_eq!(
        owed_at_close(0.80, OPEN, 20_000.0),
        0.40,
        "the share is capped at the open fill's own commission"
    );
}

#[test]
fn the_result_is_in_the_wires_precision() {
    // 3 units of $0.00008 a unit, any deal: the open half is 0.000120, the total 0.000240 (what the shim prints with %.6f).
    assert_eq!(owed_at_close(0.000240, Some((3.0, 0.000120)), 3.0), 0.00012);
    // A residue of binary arithmetic is rounding, not money.
    assert_eq!(owed_at_close(0.1 + 0.2, Some((10_000.0, 0.3)), 10_000.0), 0.0);
}

#[test]
fn settle_remembers_an_open_and_rewrites_the_close_of_that_trade() {
    let mut opens = OpenCommissions::default();
    let mut open = envelope("fill", "101", 10_000, 0.40);
    opens.settle(&mut open);
    assert_eq!(
        commission_of(&open),
        0.40,
        "an opening fill is left exactly as the venue stated it"
    );

    let mut close = envelope("fill", "101-close-o9", 10_000, 0.80);
    opens.settle(&mut close);
    assert_eq!(commission_of(&close), 0.40);
    assert_eq!(close["trade_id"], "101-close-o9", "nothing else in the envelope moves");

    // A reconnect re-surfaces the same closed row: the same answer, the core drops the repeated trade id.
    let mut again = envelope("fill", "101-close-o9", 10_000, 0.80);
    opens.settle(&mut again);
    assert_eq!(commission_of(&again), 0.40);
}

/// The sign is the vendor's word and was never measured: a charge written as a negative number must book as a charge, not
/// credit the account. Both halves of the SDK's "any deal" example, read with the opposite sign.
#[test]
fn a_negative_figure_books_as_a_charge_of_the_same_size_at_the_open_and_at_the_close() {
    let mut opens = OpenCommissions::default();
    let mut open = envelope("fill", "101", 10_000, -0.40);
    opens.settle(&mut open);
    assert_eq!(commission_of(&open), 0.40, "the open fill books the magnitude, a cost");

    let mut close = envelope("fill", "101-close-o9", 10_000, -0.80);
    opens.settle(&mut close);
    assert_eq!(
        commission_of(&close),
        0.40,
        "the close owes the other half, as with the opposite sign"
    );
}

#[test]
fn a_close_whose_open_fill_was_never_seen_books_nothing_not_the_whole_figure() {
    let mut opens = OpenCommissions::default();
    let mut close = envelope("fill", "777-close-o9", 10_000, 0.80);
    opens.settle(&mut close);
    assert_eq!(commission_of(&close), 0.0);
}

#[test]
fn a_shim_that_still_sends_zero_on_a_close_books_zero_as_before() {
    let mut opens = OpenCommissions::default();
    opens.settle(&mut envelope("fill", "101", 10_000, 0.40));
    let mut close = envelope("fill", "101-close-o9", 10_000, 0.0);
    opens.settle(&mut close);
    assert_eq!(commission_of(&close), 0.0);
}

#[test]
fn only_a_fill_is_touched() {
    let mut opens = OpenCommissions::default();
    let canceled = json!({"kind": "canceled", "order_id": "o", "ts": 0});
    let mut same = canceled.clone();
    opens.settle(&mut same);
    assert_eq!(same, canceled);
    // A fill with no trade id is the mapper's refusal to make (it drops it), not this layer's.
    let mut nameless = json!({"kind": "fill", "commission": 0.5, "amount": 1});
    opens.settle(&mut nameless);
    assert_eq!(commission_of(&nameless), 0.5);
}

#[test]
fn the_memory_is_bounded_and_forgets_the_oldest() {
    let mut opens = OpenCommissions::default();
    for i in 0..=MAX_OPENS {
        opens.settle(&mut envelope("fill", &format!("t{i}"), 1_000, 0.10));
    }
    assert_eq!(opens.opens.len(), MAX_OPENS);
    let mut oldest = envelope("fill", "t0-close-o", 1_000, 0.20);
    opens.settle(&mut oldest);
    assert_eq!(
        commission_of(&oldest),
        0.0,
        "the oldest open fill is gone: its close side is unknown"
    );
    let mut newest = envelope("fill", &format!("t{MAX_OPENS}-close-o"), 1_000, 0.20);
    opens.settle(&mut newest);
    assert_eq!(commission_of(&newest), 0.10);
}
