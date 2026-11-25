use super::*;

/// Past the bound the OLDEST is forgotten, a repeated id keeps its age, and `take` hands back what is
/// held oldest first, leaving nothing.
#[test]
fn the_oldest_goes_first_and_take_empties_it() {
    let mut set = RecentSet::default();
    for id in ["a", "b", "c", "a"] {
        set.insert(id, 3);
    }
    set.insert("d", 3);
    assert!(!set.contains("a"), "the oldest goes once a fourth id arrives");
    assert!(set.contains("b") && set.contains("c") && set.contains("d"));
    assert_eq!(set.take(), ["b", "c", "d"]);
    assert!(set.take().is_empty() && !set.contains("b"));
}

/// A removed id is forgotten at once and is not handed back by `take`.
#[test]
fn a_removed_id_is_forgotten_at_once() {
    let mut set = RecentSet::default();
    set.insert("a", 8);
    set.insert("b", 8);
    set.remove("a");
    assert!(!set.contains("a"));
    assert_eq!(set.take(), ["b"]);
}
