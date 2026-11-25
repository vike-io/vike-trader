use super::*;

/// The last of each run survives, a lone element is untouched, the order is kept, and the count
/// returned is what was dropped.
#[test]
fn the_last_of_each_run_survives_in_order() {
    let mut rows = vec![(1, 'a'), (1, 'b'), (2, 'c'), (3, 'd'), (3, 'e'), (3, 'f'), (4, 'g')];
    assert_eq!(keep_last_of_each_run(&mut rows, |r| r.0), 3);
    assert_eq!(rows, [(1, 'b'), (2, 'c'), (3, 'f'), (4, 'g')]);

    let mut empty: Vec<(i32, char)> = Vec::new();
    assert_eq!(keep_last_of_each_run(&mut empty, |r| r.0), 0);
}
