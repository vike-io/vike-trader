//! **Every column shaped like an untyped inter-table link in the settings store is a declared
//! FOREIGN KEY or a PINNED exception** — as a RATCHET over the two shapes [`observed`] can key on:
//! a column named `venue` or ending `_venue`, and an INTEGER column ending `_id` with no
//! `REFERENCES`. That is a narrower claim than "every link" — see *Declared blind spot* below.
//!
//! A link held by TEXT equality is a link the storage engine cannot enforce: nothing refuses a
//! `venue` string naming no venue, and nothing refuses deleting the row a text column points at.
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §2.3 measures FIVE
//! untyped links this store carries today; this gate's two rules derive four of them.
//!
//! This file does NOT fix them. It FREEZES them: [`UNTYPED_LINK_PIN`] is the set, its declared
//! array length IS the count, and the two tests below hold it from both sides. A fifth column
//! matching either shape reddens CI; removing a pinned one reddens CI as a one-line cleanup.
//!
//! ⚠ A ratchet rather than a flat ban, because the four cannot be removed without the reshape that
//! §5 of the spec describes — and a red gate cannot be landed at all.
//!
//! # Declared blind spot
//!
//! [`observed`]'s two rules key on a column's NAME or TYPE, and one of the spec's five untyped
//! links carries neither shape: `venue_arming.label` is TEXT, named nothing like `venue`, and
//! names an ACCOUNT — resolved not by any rule here but at MOUNT, by
//! `crates/vike-mount/src/dukascopy.rs`'s `resolve_account`. This gate cannot see it and does not
//! pretend to. It is deliberately NOT added to [`UNTYPED_LINK_PIN`]: [`observed`] cannot derive it
//! from the DDL alone, so a hand-added row for it would immediately fail
//! [`the_pin_has_no_stale_rows`] — a pin row the parser can never produce is a lie in the opposite
//! direction from the one this file exists to prevent. It retires with the arming-collapse work a
//! later stage of the plan schedules, rather than being fixed here.

use std::collections::BTreeSet;

use vike_secrets::DDL;

/// The untyped links this store carries TODAY, as `(table, column)`.
///
/// ⚠ **The declared length is the COUNT** — no prose anywhere, this file included, may restate it.
const UNTYPED_LINK_PIN: [(&str, &str); 4] = [
    ("account", "venue"),
    ("credential", "venue"),
    ("venue_arming", "venue"),
    ("venue_setting", "venue"),
];

const GROWTH_GUIDANCE: &str = "\
A new column links one table to another by TEXT or by an unconstrained integer. Two ways out:

  * give it a `REFERENCES <table>(id)` clause, which is what the engine can enforce; or
  * if the target table does not exist yet, add the row here WITH the reason, and shrink the pin
    when it does.

A `venue` TEXT column is the shape this pin exists for: `vike_model::venues::VENUES` is the roster
authority and the `venue` table is its projection, so a venue reference has a target to point at.";

/// `(table name, column body)` for every `CREATE TABLE` in `DDL`.
fn tables() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for chunk in DDL.split("CREATE TABLE IF NOT EXISTS ").skip(1) {
        let name: String = chunk.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let open = chunk.find('(').expect("a CREATE TABLE has a body");
        let close = chunk.find(") STRICT").expect("every table in this store is STRICT");
        out.push((name, chunk[open + 1..close].to_string()));
    }
    out
}

/// The body's top-level comma-separated parts — parenthesis-aware, so `CHECK (x IN (0, 1))` stays
/// one part instead of becoming three.
fn parts(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for c in body.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Every `(table, column)` that names another table's row without a `REFERENCES` clause.
fn observed() -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for (table, body) in tables() {
        for part in parts(&body) {
            let upper = part.to_uppercase();
            // Table constraints are not columns.
            if upper.starts_with("CHECK")
                || upper.starts_with("UNIQUE")
                || upper.starts_with("PRIMARY KEY")
                || upper.starts_with("FOREIGN KEY")
            {
                continue;
            }
            let Some(column) = part.split_whitespace().next() else { continue };
            if upper.contains("REFERENCES") {
                continue;
            }
            // A `venue`-named column anywhere but the `venue` table names a venue by text —
            // whether the column IS `venue` or merely ENDS in `_venue` (`parent_venue`, were one
            // added, is exactly as untyped a link as `venue` itself).
            let names_a_venue =
                (column == "venue" || column.ends_with("_venue")) && table != "venue";
            // An `*_id` column that is not this table's own primary key names another row —
            // but only when it is the shape a real reference takes here: every surrogate key in
            // this store is a bare `INTEGER PRIMARY KEY`, so an enforceable link to one is
            // necessarily `INTEGER` too (`account_id INTEGER REFERENCES account(id)`), and that is
            // exactly the OTHER unenforced shape `GROWTH_GUIDANCE` names ("by TEXT or by an
            // unconstrained integer"). `account.venue_account_id` is deliberately NOT this shape:
            // it is `TEXT`, and per `vike_secrets::schema`'s own module doc it is "a place for the
            // venue's own answer" — an external system's identifier with no local row behind it,
            // never a candidate FK. ⚠ RESIDUAL: after this narrowing, `GROWTH_GUIDANCE`'s "TEXT"
            // arm is reachable only through `names_a_venue` above, never through this branch — a
            // future TEXT `*_id` column that is NOT venue-shaped (unlike `venue_account_id`, which
            // this narrowing exists to exclude) would pass this gate unseen. Measured, not fixed:
            // no such column exists in the DDL today.
            let names_a_row = column.ends_with("_id")
                && upper.contains("INTEGER")
                && !upper.contains("PRIMARY KEY");
            if names_a_venue || names_a_row {
                out.insert((table.clone(), column.to_string()));
            }
        }
    }
    out
}

fn pinned() -> BTreeSet<(String, String)> {
    UNTYPED_LINK_PIN.iter().map(|(t, c)| ((*t).to_string(), (*c).to_string())).collect()
}

#[test]
fn untyped_links_do_not_grow() {
    let added: Vec<String> =
        observed().difference(&pinned()).map(|(t, c)| format!("  {t}.{c}")).collect();
    assert!(
        added.is_empty(),
        "\nNEW untyped inter-table links — the storage engine cannot enforce these:\n\n{}\n\n{}\n\n\
         pinned {}, observed {}\n",
        added.join("\n"),
        GROWTH_GUIDANCE,
        UNTYPED_LINK_PIN.len(),
        observed().len(),
    );
}

#[test]
fn the_pin_has_no_stale_rows() {
    let removed: Vec<String> =
        pinned().difference(&observed()).map(|(t, c)| format!("  {t}.{c}")).collect();
    assert!(
        removed.is_empty(),
        "\n`UNTYPED_LINK_PIN` names links that are no longer untyped. This is a WIN; only the \
         bookkeeping is left:\n\n{}\n\nDelete those lines and decrement the declared array length.\n",
        removed.join("\n"),
    );
}

#[test]
fn the_parser_sees_every_table_in_the_ddl() {
    // ⚠ A HAND-COPIED roster here is exactly the defect this whole file exists to fight — this test
    // named 7 of the (then) 9 tables in `DDL` until a review measured the gap: it never named `venue`
    // (Task 2) or `settings_adoption`, and would have said nothing at all about a TENTH table added
    // tomorrow. Derived instead: the parser's own table COUNT against a count of the DDL's own
    // `CREATE TABLE IF NOT EXISTS` occurrences, so a table the parser silently drops or double-counts
    // reddens this test regardless of its name — the one property a named list can never have.
    let seen = tables().len();
    let expected = DDL.matches("CREATE TABLE IF NOT EXISTS ").count();
    assert_eq!(
        seen, expected,
        "the parser found {seen} table(s) but `DDL` contains {expected} `CREATE TABLE IF NOT \
         EXISTS` statement(s) — the parser missed one (or double-counted), which would make every \
         OTHER test in this file blind to whatever table it dropped"
    );
}
