use super::*;

/// No list is compiled in: a desktop no server has answered holds NOTHING and is read-only, an empty
/// server answer is an empty store a save can reach, and a table that holds lists shows exactly them.
#[test]
fn no_server_and_an_empty_answer_both_show_no_list() {
    let none = SymbolSetStore::default();
    assert!(none.sets.is_empty(), "no list is compiled in");
    assert!(!none.writable, "no server answered, so nothing can be saved");

    let empty = SymbolSetStore::from_server(Vec::new());
    assert!(empty.sets.is_empty(), "an empty table is an empty store");
    assert!(empty.writable);

    let mine = SymbolSet {
        name: "Mine".into(),
        symbols: vec!["AAA".into(), "BBB".into()],
        provider: "some-venue".into(),
        interval: "1h".into(),
        benchmark: String::new(),
        user: true,
    };
    let saved = SymbolSetStore::from_server(vec![mine.clone()]);
    assert_eq!(saved.sets, vec![mine], "a table that holds lists shows exactly them");
}

/// **A leftover `storage/datasets.json` is read by nothing**: the lists are the server's (the owner's ruling of
/// 2026-11-12, no migration), and this module opens no file at all.
#[test]
fn this_module_reads_no_file() {
    let src = include_str!("datasets.rs");
    assert!(!src.contains(concat!("std::", "fs")), "datasets.rs must not touch the filesystem");
}
