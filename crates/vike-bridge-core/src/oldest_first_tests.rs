use super::*;

/// `(id, side, opened, size)`: a position of the test venue.
type Pos = (u32, i8, i64, i64);

fn take(open: &[Pos], order_side: i8, want: i64) -> Vec<(u32, i64)> {
    take_oldest_first(
        open,
        |p| p.1 == -order_side,
        |a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)),
        |p| p.3,
        want,
    )
    .into_iter()
    .map(|(p, n)| (p.0, n))
    .collect()
}

/// Only the positions facing the order, oldest first (a tie on the open time goes to the lower id), each taken for
/// the smaller of what is left and its size.
#[test]
fn the_facing_positions_are_taken_oldest_first_until_the_order_is_covered() {
    let open = [(3, 1, 20, 50), (1, 1, 10, 40), (2, -1, 5, 99), (4, 1, 10, 30)];
    assert_eq!(
        take(&open, -1, 60),
        vec![(1, 40), (4, 20)],
        "a SELL takes the longs: 1 then 4 (tie at 10)"
    );
    assert_eq!(take(&open, 1, 60), vec![(2, 60)], "a BUY takes the short only");
}

/// The legs are capped at what faces the order; nothing facing, or nothing wanted, is no leg.
#[test]
fn the_legs_never_add_up_to_more_than_the_positions_hold() {
    let open = [(1, 1, 10, 40), (2, 1, 20, 30)];
    assert_eq!(take(&open, -1, 1_000), vec![(1, 40), (2, 30)]);
    assert_eq!(take(&open, -1, i64::MAX), vec![(1, 40), (2, 30)], "every facing position, whole");
    assert!(take(&open, 1, 10).is_empty(), "nothing faces a BUY");
    assert!(take(&open, -1, 0).is_empty(), "nothing wanted");
    assert!(take(&[], -1, 10).is_empty());
}
