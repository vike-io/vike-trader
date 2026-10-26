//! **The `setting.section` `CHECK` lists exactly [`SETTINGS_SECTIONS`].** The const is the owner of
//! the four section words (`crates/vike-secrets/src/settings/model.rs`'s `SETTINGS_SECTIONS`); the
//! `CHECK (section IN (…))` literal in `crates/vike-secrets/src/schema/ddl.rs`'s `DDL` restates the
//! same four words as SQL, and nothing tied the two together. The mirror in `vike-config`
//! (`crates/vike-config/tests/mirror.rs`) is pinned equal to the const, and the spec comparison in
//! `crates/vike-secrets/tests/settings_store_ddl_gate.rs` compares the DDL to a design document, so
//! neither reads the const against the DDL: a fifth section added to the const and not to the
//! `CHECK` would pass both and fail only at the first insert into the new section, on a live
//! store; the reverse would leave a word the store accepts and no writer can name.
//!
//! This reads the word list OUT of the shipped `DDL` string, with the shared parser
//! (`ddl_parser.rs`), and holds it equal to the const as a SET (order is not part of a `CHECK`'s
//! meaning). It changes no DDL: a store that already exists keeps the `CHECK` it was built with, so
//! adding a section word is a schema change with its own record, and this gate is what makes the
//! const and the literal move together when that record lands.
//!
//! Declared blind spot: an EXISTING store was built from an older batch and keeps its own `CHECK`;
//! this is the text of the batch that builds a FRESH store, like the sibling gates.

use std::collections::BTreeSet;

use crate::ddl_parser::{assert_sees_every_table, tables, top_level_parts};
use vike_secrets::{DDL, SETTINGS_SECTIONS};

/// The words of `CHECK (section IN ('a', 'b', …))` in `sql`'s `setting` table.
///
/// Panics, naming what is missing, when the table or its `CHECK` is not there: a gate that read
/// nothing must be red, not a vacuous equality of two empty sets.
fn setting_check_sections(sql: &str) -> BTreeSet<String> {
    let (_, body) = tables(sql)
        .into_iter()
        .find(|(name, _)| name == "setting")
        .expect("the DDL declares a `setting` table");
    let check = top_level_parts(&body)
        .into_iter()
        .find(|part| {
            let upper = part.to_ascii_uppercase();
            upper.starts_with("CHECK") && upper.contains("SECTION IN")
        })
        .expect("the `setting` table carries a `CHECK (section IN (…))`");
    let at = check.to_ascii_uppercase().find("SECTION IN").expect("found above");
    let open = check[at..].find('(').map(|i| at + i).expect("an `IN` list opens with `(`");
    let close = check[open..].find(')').map(|i| open + i).expect("an `IN` list closes with `)`");
    check[open + 1..close]
        .split(',')
        .map(|word| word.trim().trim_matches('\'').to_string())
        .collect()
}

/// THE gate: the `CHECK`'s words are the const's words, no more and no fewer.
#[test]
fn the_setting_section_check_lists_exactly_the_settings_sections() {
    let in_ddl = setting_check_sections(DDL);
    let in_const: BTreeSet<String> = SETTINGS_SECTIONS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        in_const.len(),
        SETTINGS_SECTIONS.len(),
        "`SETTINGS_SECTIONS` names a section twice: {SETTINGS_SECTIONS:?}"
    );
    assert_eq!(
        in_ddl, in_const,
        "the `setting.section` CHECK in `crates/vike-secrets/src/schema/ddl.rs`'s `DDL` lists \
         {in_ddl:?} but `SETTINGS_SECTIONS` is {in_const:?}. They are one vocabulary spelled twice: \
         a word in the const and not in the CHECK is refused by the store at the first insert, and \
         a word in the CHECK and not in the const is a section no writer can name. Adding a \
         section is a schema change (an existing store keeps its CHECK), so it needs its own \
         record before either side moves."
    );
}

/// The extractor reads what it claims to, proved on a PLANTED batch: a `CHECK` that differs from
/// the const comes back as the planted words, so a green above is not two empty sets agreeing. The
/// planted text is shaped like the shipped table (inline column list, table-level `UNIQUE` and
/// `CHECK`), with the words in a different order and an extra one.
#[test]
fn the_extractor_reads_a_planted_check() {
    let planted = "\
CREATE TABLE IF NOT EXISTS setting (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    section TEXT NOT NULL,
    key     TEXT NOT NULL,
    UNIQUE (section, key),
    CHECK (section IN ('flags', 'policy', 'extra'))
) STRICT;
CREATE TABLE IF NOT EXISTS venue_setting (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    tier TEXT NOT NULL,
    CHECK (tier IN ('any', 'paper', 'demo', 'live'))
) STRICT;";
    let got = setting_check_sections(planted);
    let want: BTreeSet<String> =
        ["extra", "flags", "policy"].iter().map(|s| (*s).to_string()).collect();
    assert_eq!(got, want, "the extractor must read the `setting` CHECK and no other table's");
}

/// The shipped batch was read in full by the shared parser (the anti-vacuity guard every gate in
/// this binary carries), so `setting` was not found by a parser that had silently dropped others.
#[test]
fn the_parser_sees_every_table_in_the_ddl() {
    assert_sees_every_table(DDL);
    assert!(!setting_check_sections(DDL).is_empty(), "the shipped `setting` CHECK read as empty");
}
