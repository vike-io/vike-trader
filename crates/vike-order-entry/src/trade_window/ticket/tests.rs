//! The ticket's unit tests: where the form is hidden below, and how a row packs and balances.

use super::fit::{TradeWindowTicketRowFill, balance, pack, row_widths};
use super::full::hidden_below;

/// Ruling B2's condition: the divider is drawn while the form's content goes on below what it
/// shows, and not where it fits, nor where the trader has scrolled it to its end.
#[test]
fn a_form_is_hidden_below_only_while_its_content_goes_on_past_the_view() {
    assert!(hidden_below(400.0, 0.0, 300.0), "100 pt below the view");
    assert!(hidden_below(400.0, 99.0, 300.0), "a point short of the end");
    assert!(!hidden_below(400.0, 100.0, 300.0), "scrolled to its end");
    assert!(!hidden_below(300.0, 0.0, 300.0), "it fits exactly");
    assert!(!hidden_below(300.4, 0.0, 300.0), "within egui's rounding");
    assert!(!hidden_below(200.0, 0.0, 300.0), "it fits with room to spare");
}

/// W4 fix round 1, M1: `pack` at its edges. A row that is filled EXACTLY holds its last item;
/// a point over starts the next row; an item wider than any row still gets a row of its own
/// (it is never dropped, and never shares one); nothing packs into no rows.
#[test]
fn pack_fills_a_row_exactly_and_never_drops_an_item() {
    // 40 + 6 + 40 + 6 + 40 = 132.
    assert_eq!(pack(&[40.0, 40.0, 40.0], 6.0, 132.0), [3], "an exact fit is one row");
    assert_eq!(pack(&[40.0, 40.0, 40.0], 6.0, 131.0), [2, 1], "a point over starts the next");
    assert_eq!(pack(&[300.0], 6.0, 132.0), [1], "an item wider than the row keeps its own row");
    assert_eq!(pack(&[40.0, 300.0, 40.0], 6.0, 132.0), [1, 1, 1], "and shares it with nothing");
    assert_eq!(pack(&[], 6.0, 132.0), Vec::<usize>::new(), "nothing packs into no rows");
}

/// The v3 design's rows of choices are EQUAL buttons: `Equal` gives every item of a row the same
/// width with the whole row between them, whatever each needs; `LastOfRow` and `Item` leave
/// every item its own width but one, which takes what is left (never less than it needs).
#[test]
fn a_row_shares_its_width_as_its_fill_says() {
    // 3 items, 2 gaps of 6: (132 - 12) / 3.
    let eq = row_widths(TradeWindowTicketRowFill::Equal, 0, &[30.0, 50.0, 40.0], 6.0, 132.0);
    assert_eq!(eq, [40.0, 40.0, 40.0]);
    let last = row_widths(TradeWindowTicketRowFill::LastOfRow, 0, &[30.0, 50.0, 40.0], 6.0, 132.0);
    assert_eq!(last, [30.0, 50.0, 132.0 - 12.0 - 80.0]);
    // The item that grows is counted over every row: item 4 is the second row's second item.
    let second = row_widths(TradeWindowTicketRowFill::Item(4), 3, &[30.0, 40.0], 6.0, 100.0);
    assert_eq!(second, [30.0, 64.0]);
    let elsewhere = row_widths(TradeWindowTicketRowFill::Item(0), 3, &[30.0, 40.0], 6.0, 100.0);
    assert_eq!(elsewhere, [30.0, 40.0], "a row without the growing item grows none");
    // An item that does not fit its row keeps the width it needs.
    let tight = row_widths(TradeWindowTicketRowFill::LastOfRow, 0, &[60.0, 60.0], 6.0, 100.0);
    assert_eq!(tight, [60.0, 60.0]);
}

/// Five quick sizes that wrap are three and two, never four and one; a row that fits is left
/// alone, and the items are never lost or moved between rows' order.
#[test]
fn rows_that_wrap_are_evened_out() {
    assert_eq!(balance(vec![4, 1]), [3, 2]);
    assert_eq!(balance(vec![3, 3, 1]), [3, 2, 2]);
    assert_eq!(balance(vec![5]), [5]);
    assert_eq!(balance(vec![2, 2]), [2, 2]);
    assert_eq!(balance(Vec::new()), Vec::<usize>::new());
}
