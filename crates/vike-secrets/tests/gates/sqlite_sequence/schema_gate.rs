//! The gate over the shipped schema: every armed table is classified, no pin row is stale, and
//! every pinned verdict matches what `DDL` derives.

use std::collections::BTreeSet;

use super::armed_tables;
use super::pin::{GROWTH_GUIDANCE, SEQUENCE_PIN, Sequence, pinned};
use crate::ddl_parser::{assert_sees_every_table, tables};
use vike_secrets::DDL;

// -------------------------------------------------------------------------------------------
// The gate — the schema derivation
// -------------------------------------------------------------------------------------------

#[test]
fn the_table_parser_sees_every_table_in_the_ddl() {
    // The anti-vacuity guard for [`armed_tables`]: a parser that quietly found NOTHING would make
    // `every_armed_table_is_classified` pass against any schema at all, including one that armed
    // every table and rebuilt them all naively. Derived rather than hand-counted, so a table the
    // parser drops or double-counts reddens this regardless of its name.
    assert_sees_every_table(DDL);
}

#[test]
fn every_armed_table_is_classified() {
    let pinned = pinned();
    let unclassified: Vec<String> = armed_tables(DDL)
        .into_iter()
        .filter(|table| !pinned.contains_key(table))
        .map(|table| format!("  {table}"))
        .collect();
    assert!(
        unclassified.is_empty(),
        "\nTable(s) in the shipped `DDL` declare `AUTOINCREMENT` and nothing in this gate says \
         so:\n\n{}\n\n{}\n\npinned {}\n",
        unclassified.join("\n"),
        GROWTH_GUIDANCE,
        SEQUENCE_PIN.len(),
    );
}

#[test]
fn the_pin_has_no_stale_rows() {
    let live: BTreeSet<String> = tables(DDL).into_iter().map(|(name, _)| name).collect();
    let gone: Vec<String> = SEQUENCE_PIN
        .iter()
        .filter(|(table, _, _)| !live.contains(*table))
        .map(|(table, verdict, _)| format!("  {table} ({verdict})"))
        .collect();
    assert!(
        gone.is_empty(),
        "\n`SEQUENCE_PIN` names table(s) the shipped `DDL` no longer carries:\n\n{}\n\nDelete \
         those lines and decrement the declared array length.\n",
        gone.join("\n"),
    );
}

/// **The staleness assertion — this is what goes red the moment stage 4 pays the debt.**
#[test]
fn every_pinned_verdict_matches_the_schema() {
    let armed = armed_tables(DDL);
    let wrong: Vec<String> = SEQUENCE_PIN
        .iter()
        .filter_map(|(table, verdict, _)| {
            let derived = if armed.contains(*table) { Sequence::Armed } else { Sequence::Owed };
            (derived != *verdict)
                .then(|| format!("  {table} — pinned {verdict}, `DDL` derives {derived}"))
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "\nA pinned verdict disagrees with the schema it claims to describe:\n\n{}\n\nOwed -> \
         Armed is the WIN this gate was written to wait for: spec §9's stage 4 landed. Three \
         things are now owed here, in this order — (1) flip the row's word to `Sequence::Armed` \
         and replace its `why` with the argument for the rebuild that carries its mark; (2) \
         re-read `section_7_item_4_remove_the_top_account_rebuild_and_create_again`, whose \
         expectation is DERIVED and has just inverted, so it is now asserting §7 item 4 proper; \
         (3) re-read EVERY `TABLE_DROP_PIN` row that names a rebuild of this table, because a \
         no-op mark carry has just become load-bearing and nothing else will say so.\n\nArmed -> \
         Owed means an `AUTOINCREMENT` was REMOVED, which re-opens id reuse on a column spec §4.1 \
         calls the account's identity.\n",
        wrong.join("\n"),
    );
}
