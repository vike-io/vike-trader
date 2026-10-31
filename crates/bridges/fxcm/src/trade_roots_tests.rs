use super::*;

#[test]
fn an_id_nobody_linked_is_its_own_root() {
    let roots = TradeRoots::default();
    assert_eq!(roots.root_of("186982939"), "186982939");
}

#[test]
fn a_remainder_reads_as_the_trade_it_was_split_from() {
    let mut roots = TradeRoots::default();
    roots.link("186982939", "186983022");
    assert_eq!(roots.root_of("186983022"), "186982939");
    assert_eq!(roots.root_of("186982939"), "186982939", "the root stays itself");
}

/// A second split of the remainder reads as the FIRST trade, not as the remainder it came from: the core folded only the
/// first id.
#[test]
fn a_chain_of_splits_reads_as_its_first_trade() {
    let mut roots = TradeRoots::default();
    roots.link("T0", "R1");
    roots.link("R1", "R2");
    roots.link("R2", "R3");
    for id in ["R1", "R2", "R3"] {
        assert_eq!(roots.root_of(id), "T0", "{id}");
    }
}

/// A split whose parent nobody linked (the node was down for the first one) is a chain of its own from there: the best
/// the table knows, and the same answer a restart gives.
#[test]
fn a_parent_the_table_never_saw_is_the_root() {
    let mut roots = TradeRoots::default();
    roots.link("R1", "R2");
    assert_eq!(roots.root_of("R2"), "R1");
}

#[test]
fn a_link_that_makes_no_sense_is_ignored() {
    let mut roots = TradeRoots::default();
    roots.link("", "R1");
    roots.link("T0", "");
    roots.link("T0", "T0");
    roots.link("T0", "R1");
    // a cycle: T0 is R1's root, so R1 cannot be T0's parent
    roots.link("R1", "T0");
    assert_eq!(roots.root_of("R1"), "T0");
    assert_eq!(roots.root_of("T0"), "T0");
    assert_eq!(roots.root.len(), 1, "only the one real link was kept");
}

/// Relinking the same remainder is idempotent (the exec drain and a reconcile pass both report the same split), and the
/// queue holds one row per remainder so the cap counts what it should.
#[test]
fn the_same_split_reported_twice_is_one_link() {
    let mut roots = TradeRoots::default();
    roots.link("T0", "R1");
    roots.link("T0", "R1");
    assert_eq!((roots.root.len(), roots.inserted.len()), (1, 1));
}

#[test]
fn the_oldest_links_go_first_past_the_cap() {
    let mut roots = TradeRoots::default();
    for i in 0..(MAX_LINKS + 10) {
        roots.link(&format!("p{i}"), &format!("c{i}"));
    }
    assert_eq!(roots.root.len(), MAX_LINKS);
    assert_eq!(roots.inserted.len(), MAX_LINKS);
    assert_eq!(roots.root_of("c0"), "c0", "the oldest was forgotten");
    let last = MAX_LINKS + 9;
    assert_eq!(roots.root_of(&format!("c{last}")), format!("p{last}"), "the newest stands");
}

/// The fill report reads the table it is given: a split the exec lane heard (`T0 -> R1`) lets a second split the reconcile
/// session sees for the first time (`R1 -> R2`) read as `T0`, and the report teaches the table the row's own link.
#[test]
fn the_fill_report_follows_a_split_the_exec_lane_heard() {
    let mut roots = TradeRoots::default();
    roots.link("T0", "R1");
    let body = r#"[{"trade_id":"R2","order_id":"O","instrument":"EUR/USD","buysell":"B","amount":1000,"open_rate":1.1,"commission":0.0,"trade_id_origin":"R1"}]"#;
    let reports =
        crate::recon_client::parse_fills_with(body, "EURUSD", &mut roots).expect("parses");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].trade_id.as_str(), "T0");
    assert_eq!(roots.root_of("R2"), "T0");
}

#[test]
fn the_process_table_is_one_table() {
    // unique ids: other tests in this binary read the same table
    process().link("proc-T0", "proc-R1");
    assert_eq!(process().root_of("proc-R1"), "proc-T0");
}
