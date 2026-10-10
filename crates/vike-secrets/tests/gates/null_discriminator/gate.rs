//! The derivation and the gate: parse the schema, derive which NULL tests are kind discriminators,
//! and hold the pin against it from both sides.

use std::collections::{BTreeMap, BTreeSet};

use super::pin::{GROWTH_GUIDANCE, NULL_PREDICATE_PIN, Route, Verdict, pinned};
use crate::ddl_parser::{assert_sees_every_table, bare, indexes, tables, top_level_parts};
use vike_secrets::DDL;
use vike_secrets::profile_store::profile_ddl;

// -------------------------------------------------------------------------------------------
// The NULL tests in the shared parser's index predicates, and the discriminators they derive
// -------------------------------------------------------------------------------------------

/// One `<column> IS [NOT] NULL` test, located.
#[derive(Debug, Clone)]
struct NullTest {
    index: String,
    table: String,
    column: String,
    /// `true` for `IS NOT NULL`.
    negated: bool,
}

/// Every `<column> IS [NOT] NULL` test carried by a partial index in `ddl`.
///
/// A `Vec` rather than a set: [`the_scanner_finds_every_null_test_in_the_ddl`] compares this
/// LENGTH against an independent count, and a set would silently absorb a duplicate into an
/// off-by-one nobody could read.
fn null_tests(ddl: &str) -> Vec<NullTest> {
    let mut out = Vec::new();
    for index in indexes(ddl) {
        let p = &index.predicate;
        for i in 0..p.len() {
            let Some((column, negated)) = null_test_at(p, i) else { continue };
            out.push(NullTest {
                index: index.name.clone(),
                table: index.table.clone(),
                column,
                negated,
            });
        }
    }
    out
}

/// `(column, negated)` when `tokens[i]` opens an `IS [NOT] NULL` test with a column before it.
fn null_test_at(tokens: &[String], i: usize) -> Option<(String, bool)> {
    if !tokens[i].eq_ignore_ascii_case("IS") {
        return None;
    }
    let negated = tokens.get(i + 1).is_some_and(|t| t.eq_ignore_ascii_case("NOT"));
    let null_at = if negated { i + 2 } else { i + 1 };
    if !tokens.get(null_at).is_some_and(|t| bare(t).eq_ignore_ascii_case("NULL")) {
        return None;
    }
    let column = bare(tokens.get(i.checked_sub(1)?)?);
    (!column.is_empty()).then(|| (column.to_string(), negated))
}

/// **Every `(table, column)` whose nullness selects a uniqueness rule, and by which [`Route`]** —
/// the whole derivation behind [`Verdict::KindDiscriminator`].
///
/// `BothDirections` wins a tie, because it is the shape the spec's two named defects wear and the
/// one a reader will recognise.
fn discriminators(sql: &str, tests: &[NullTest]) -> BTreeMap<(String, String), Route> {
    let key = |t: &NullTest| (t.table.clone(), t.column.clone());
    let nulls: BTreeSet<_> = tests.iter().filter(|t| !t.negated).map(key).collect();
    let not_nulls: BTreeSet<_> = tests.iter().filter(|t| t.negated).map(key).collect();
    let keys = natural_key_columns(sql);
    let mut out = BTreeMap::new();
    for pair in tests.iter().map(key) {
        if keys.get(&pair.0).is_some_and(|cols| cols.contains(&pair.1)) {
            out.insert(pair.clone(), Route::NaturalKey);
        }
        if nulls.contains(&pair) && not_nulls.contains(&pair) {
            out.insert(pair, Route::BothDirections);
        }
    }
    out
}

/// **Every table's NATURAL-KEY columns** — the second place a uniqueness rule is declared, which
/// no index `WHERE` clause mentions.
///
/// Three shapes, all of them uniqueness rules the engine enforces: a table-level `UNIQUE (…)`, a
/// table-level `PRIMARY KEY (…)` (whose columns in a SQLite rowid table are NOT implicitly
/// `NOT NULL` — the quirk that makes this reachable), and a bare `UNIQUE` keyword in a column
/// definition.
fn natural_key_columns(sql: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (table, body) in tables(sql) {
        let cols = out.entry(table).or_default();
        for part in top_level_parts(&body) {
            let upper = part.to_ascii_uppercase();
            if upper.starts_with("CHECK") || upper.starts_with("FOREIGN KEY") {
                continue;
            }
            if upper.starts_with("UNIQUE") || upper.starts_with("PRIMARY KEY") {
                // A table-level key constraint: every identifier inside its parentheses.
                let Some(open) = part.find('(') else { continue };
                let Some(close) = part.rfind(')') else { continue };
                for column in part[open + 1..close].split(',') {
                    let column = bare(column.trim());
                    if !column.is_empty() {
                        cols.insert(column.to_string());
                    }
                }
                continue;
            }
            // A column definition. `name TEXT NOT NULL UNIQUE` is the same rule written inline.
            let mut tokens = part.split_whitespace();
            let Some(column) = tokens.next().map(bare) else { continue };
            if !column.is_empty() && tokens.any(|t| bare(t).eq_ignore_ascii_case("UNIQUE")) {
                cols.insert(column.to_string());
            }
        }
    }
    out
}

/// `(index, column) -> verdict` for every NULL test [`DDL`] carries.
fn observed() -> BTreeMap<(String, String), Verdict> {
    let tests = null_tests(DDL);
    let discriminators = discriminators(DDL, &tests);
    tests
        .iter()
        .map(|t| {
            let verdict = if discriminators.contains_key(&(t.table.clone(), t.column.clone())) {
                Verdict::KindDiscriminator
            } else {
                Verdict::Filter
            };
            ((t.index.clone(), t.column.clone()), verdict)
        })
        .collect()
}

/// An independent count of a schema's index statements: the bare word `INDEX`, which every
/// `CREATE … INDEX` carries exactly once and no identifier in either schema uses.
fn index_word_count(ddl: &str) -> usize {
    ddl.split_whitespace().filter(|t| t.eq_ignore_ascii_case("INDEX")).count()
}

/// An independent count of a schema's NULL tests: a scan of the WHOLE text's token stream, which
/// reaches [`indexes`] and its predicate extraction not at all. That is the independence the
/// counting tests rest on — the two answers agree only if the index parser lost nothing.
fn null_phrase_count(sql: &str) -> usize {
    let tokens: Vec<String> = sql.split_whitespace().map(String::from).collect();
    (0..tokens.len()).filter(|i| null_test_at(&tokens, *i).is_some()).count()
}

/// The same count restricted to the statements [`indexes`] does NOT read — the `CHECK`
/// constraints inside `CREATE TABLE`. Subtracted so the comparison is over index predicates alone.
fn null_phrases_outside_indexes(sql: &str) -> usize {
    sql.split(';')
        .filter(|s| !s.split_whitespace().any(|t| t.eq_ignore_ascii_case("INDEX")))
        .map(null_phrase_count)
        .sum()
}

/// A THIRD count, and the only one that shares no code with [`null_test_at`]: the two phrases as
/// SUBSTRINGS of the ASCII-uppercased text. (`IS NULL` is not a substring of `IS NOT NULL`, so a
/// plain sum double-counts nothing.)
///
/// ⚠ **Its whole job is the failure the other two cannot see.** [`null_phrase_count`] and
/// [`null_tests`] both recognise a NULL test through `null_test_at`, so a recogniser that went
/// blind would report `0 == 0` and every count test would pass. On [`DDL`] that is backstopped by
/// [`the_pin_has_no_stale_rows`] — every pinned row would go stale at once (it said *"seven"*,
/// a count §5.2 step 7 made five; the declared array length is the count) — but the profile
/// schema has no pin behind it and would have passed silently.
///
/// RESIDUAL: being a substring scan it is whitespace-literal, so reformatting `IS\n    NULL` in
/// either schema reddens [`the_two_null_recognisers_agree`] rather than anything real. That is the
/// cheap direction to be wrong in.
fn null_phrase_substrings(sql: &str) -> usize {
    let upper = sql.to_ascii_uppercase();
    upper.matches("IS NOT NULL").count() + upper.matches("IS NULL").count()
}

// -------------------------------------------------------------------------------------------
// The gate
// -------------------------------------------------------------------------------------------

#[test]
fn every_null_predicate_is_classified() {
    let observed = observed();
    let pinned = pinned();
    let added: Vec<String> = observed
        .iter()
        .filter(|(key, _)| !pinned.contains_key(*key))
        .map(|((index, column), verdict)| format!("  {index}: {column} -> {verdict}"))
        .collect();
    assert!(
        added.is_empty(),
        "\nUNCLASSIFIED NULL predicate(s) in a partial index — owner ruling 2 says a NULL may not \
         decide what KIND of row this is:\n\n{}\n\n{}\n\npinned {}, observed {}\n",
        added.join("\n"),
        GROWTH_GUIDANCE,
        NULL_PREDICATE_PIN.len(),
        observed.len(),
    );
}

#[test]
fn the_pin_has_no_stale_rows() {
    let observed = observed();
    let removed: Vec<String> = NULL_PREDICATE_PIN
        .iter()
        .filter(|(index, column, _, _)| {
            !observed.contains_key(&((*index).to_string(), (*column).to_string()))
        })
        .map(|(index, column, verdict, _)| format!("  {index}: {column} ({verdict})"))
        .collect();
    assert!(
        removed.is_empty(),
        "\n`NULL_PREDICATE_PIN` names NULL predicates `DDL` no longer carries. For a \
         `KindDiscriminator` row this is the WIN this gate exists to wait for; only the \
         bookkeeping is left:\n\n{}\n\nDelete those lines and decrement the declared array \
         length.\n",
        removed.join("\n"),
    );
}

#[test]
fn every_pinned_verdict_matches_the_schema() {
    let observed = observed();
    let wrong: Vec<String> = NULL_PREDICATE_PIN
        .iter()
        .filter_map(|(index, column, pinned_verdict, _)| {
            let derived = observed.get(&((*index).to_string(), (*column).to_string()))?;
            (derived != pinned_verdict).then(|| {
                format!("  {index}: {column} — pinned {pinned_verdict}, DDL derives {derived}")
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "\nA pinned verdict disagrees with the schema it claims to describe:\n\n{}\n\nA row \
         flipping Filter -> KindDiscriminator means a complementary partial index was added and \
         that column's nullness now decides a row's KIND, which is what ruling 2 refuses. A row \
         flipping the other way means the complement was removed — correct the word.\n",
        wrong.join("\n"),
    );
}

#[test]
fn the_parser_sees_every_index_in_the_ddl() {
    // ⚠ NOT a hand-copied roster, and not a count of the literal this parser splits on either —
    // see [`indexes`]. The needle is the bare word `INDEX`; the parser splits on `;` and
    // classifies. A statement the parser drops, or double-counts, reddens this regardless of its
    // name. RESIDUAL: a column or table named `index` would inflate the needle. None exists, and
    // this test is where that would announce itself.
    let seen = indexes(DDL).len();
    let expected = index_word_count(DDL);
    assert_eq!(
        seen, expected,
        "the parser found {seen} index statement(s) but `DDL` uses the word INDEX {expected} \
         time(s) — the parser missed one (or double-counted), which would make every OTHER test in \
         this file blind to whatever it dropped"
    );
}

#[test]
fn the_scanner_finds_every_null_test_in_the_ddl() {
    // The anti-vacuity guard for [`null_tests`]: a scanner that quietly found NOTHING would make
    // `every_null_predicate_is_classified` pass on any schema at all. Derived from the same text by
    // a different route — a substring count of the two phrases — minus the ones a `CHECK`
    // constraint carries, which live in `CREATE TABLE` statements this gate deliberately does not
    // read.
    let expected = null_phrase_count(DDL) - null_phrases_outside_indexes(DDL);
    let seen = null_tests(DDL).len();
    assert_eq!(
        seen, expected,
        "the scanner found {seen} NULL test(s) in `DDL`'s index predicates but the text carries \
         {expected} outside its CHECK constraints — a NULL test this scanner cannot see is one \
         nothing in this file classifies"
    );
}

#[test]
fn the_parser_sees_every_table_in_the_ddl() {
    // The twin of `the_parser_sees_every_index_in_the_ddl`, for the parser `Route::NaturalKey`
    // rests on. Same independence: the needle is the bare word `TABLE`, the parser splits on `;`.
    // The count itself is the shared parser's own guard now.
    assert_sees_every_table(DDL);
}

#[test]
fn the_two_null_recognisers_agree() {
    // ⚠ The guard the two counting tests above CANNOT be: they both reach a NULL test through
    // `null_test_at`, so a blind recogniser makes each of them pass on `0 == 0`. This one compares
    // that recogniser against a substring scan sharing none of its code, over BOTH schemas —
    // and the profile schema is the half that has no pin standing behind it.
    let profile = profile_ddl(&["fx"]).expect("a bare-ASCII vocabulary renders");
    for (what, sql) in [("DDL", DDL), ("profile_ddl", profile.as_str())] {
        assert_eq!(
            null_phrase_count(sql),
            null_phrase_substrings(sql),
            "{what}: the token recogniser and the substring scan disagree about how many NULL \
             tests this schema carries — if the token side is the one at zero, every count test in \
             this file is passing on nothing"
        );
    }
}

#[test]
fn the_natural_key_route_derives_a_discriminator() {
    // ⚠ The FIXTURE for `Route::NaturalKey`, which no statement in either live schema takes today
    // — so without this the arm would be dead code asserted to work. The SQL below is §3's
    // replacement shape as the review described it: one table-level `UNIQUE` plus ONE partial
    // index. `Route::BothDirections` is blind to it, and that is the whole point.
    let planted = "\
CREATE TABLE IF NOT EXISTS arming (
    id         INTEGER PRIMARY KEY,
    venue_id   INTEGER NOT NULL,
    account_id INTEGER,
    mode       TEXT NOT NULL,
    UNIQUE (venue_id, account_id)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS arming_one_per_venue
    ON arming (venue_id) WHERE account_id IS NULL;
";
    let tests = null_tests(planted);
    assert_eq!(tests.len(), 1, "the fixture carries exactly one NULL test");
    let found = discriminators(planted, &tests);
    assert_eq!(
        found.get(&("arming".to_string(), "account_id".to_string())),
        Some(&Route::NaturalKey),
        "a NULL-tested column that is also part of its table's natural key is a KIND \
         discriminator, and only this Route can say so — the column is tested in ONE direction, so \
         `Route::BothDirections` sees nothing. Derived: {found:?}"
    );
    // ...and the sibling route, proven on the shape the live schema actually wears, so neither arm
    // can rot into the other's coverage.
    let pair = "\
CREATE TABLE IF NOT EXISTS arming (venue TEXT NOT NULL, label TEXT, mode TEXT NOT NULL) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS a ON arming (venue) WHERE label IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS b ON arming (venue, label) WHERE label IS NOT NULL;
";
    assert_eq!(
        discriminators(pair, &null_tests(pair)).get(&("arming".to_string(), "label".to_string())),
        Some(&Route::BothDirections),
        "a column tested in both directions is a KIND discriminator, and `label` is no part of \
         this fixture's natural key — it has none"
    );
}

#[test]
fn the_profile_store_schema_carries_no_kind_discriminator() {
    // ⚠ The SECOND schema this crate ships, and it is not in the pin — see this file's declared
    // residuals. The same derivation, BOTH Routes, is run over it so ruling 2 is checked there
    // rather than assumed: its one NULL predicate (`subscription`'s `family IS NOT NULL`) has no
    // complementary sibling and `family` is no part of `PRIMARY KEY (profile, ord)`, so it is a
    // filter. The vocabulary argument only fills a `CHECK` list and reaches nothing here.
    let ddl = profile_ddl(&["fx"]).expect("a bare-ASCII vocabulary renders");
    let tests = null_tests(&ddl);
    // Anti-vacuity, derived rather than hand-counted: an empty scan here would make the assertion
    // below pass on any schema. ⚠ This alone is NOT enough — it shares `null_test_at` with what it
    // guards, so `the_two_null_recognisers_agree` carries the other half for this schema.
    assert_eq!(
        tests.len(),
        null_phrase_count(&ddl) - null_phrases_outside_indexes(&ddl),
        "the scanner missed a NULL test in the profile schema's index predicates, so the \
         assertion below would be checking nothing"
    );
    let found: Vec<String> = discriminators(&ddl, &tests)
        .iter()
        .map(|((table, column), route)| format!("  {table}.{column} — {route}"))
        .collect();
    assert!(
        found.is_empty(),
        "\nThe profile schema now lets a column's NULLNESS decide which uniqueness rule applies, \
         i.e. what KIND of row it is:\n\n{}\n\nOwner ruling 2 refuses that shape. This schema \
         carries no such debt today and is deliberately absent from `NULL_PREDICATE_PIN`; do not \
         add a row for it there.\n",
        found.join("\n"),
    );
}
