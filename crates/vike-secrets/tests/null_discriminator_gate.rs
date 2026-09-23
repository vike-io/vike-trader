//! **No NULL in this store's schema may be the SOLE discriminator of a row's KIND** — owner
//! ruling 2 of `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`: *"A design
//! where a NULL in a column decides what KIND of row it is is refused on principle, even where it
//! is unambiguous."* §7 item 2 of that spec asks for this gate in as many words, and calls it *"the
//! gate whose absence let §2.1's drift land"*.
//!
//! The shape it is about is live in two places and the spec measures both. §2.2, quoting
//! `crates/vike-config/src/mirror.rs`'s own wart comment beside `rows_from_loaded`: on
//! `venue_arming`, *"`label IS NULL` already means both 'the venue's ceiling' and 'the unlabelled
//! account's'"*. §2.6: `venue_setting` *"carries the identical NULL-discriminator shape"* —
//! `tier IS NULL` is the machine-scoped row, `tier NOT NULL` the tier-scoped one.
//!
//! # What is DERIVED, and why the spec's literal sentence was narrowed
//!
//! §7 item 2 spells the rule as *"fails on a partial index whose `WHERE` clause tests a column for
//! NULL"*. Taken literally that condemns all seven partial indexes in [`DDL`], three of which are
//! not the defect at all: `credential`'s two live-row indexes test `superseded_at IS NULL` and
//! `account_one_account_per_book` tests `venue_account_id IS NOT NULL`. Those are FILTERS — they
//! select a SUBSET of rows to constrain and NOTHING constrains the complement, neither a second
//! index nor the table's own natural key, so no row's KIND is being decided. Pinning them as debt
//! would be a lie in the opposite direction from the one this file exists to prevent, and the pin
//! would never shrink.
//!
//! ⚠ **That narrowing is the SPEC's, not this file's.** §6 exempts one of the three BY NAME:
//! *"`superseded_at IS NULL` remains a NULL that means 'not yet'. It does not discriminate a KIND
//! of row, and the timestamp is genuinely useful, so ruling 2 does not reach it."* The third,
//! `account_one_account_per_book`'s `venue_account_id IS NOT NULL`, the spec does not rule on —
//! its `Filter` is this gate's own derivation, and [`NULL_PREDICATE_PIN`]'s row says so.
//!
//! So the KIND verdict is derived from the DDL rather than hand-assigned, by the property that
//! separates the two — **the nullness of this column selects which uniqueness rule applies** —
//! which the schema can say along either of [`Route`]'s two paths. Today they derive
//! `venue_arming.label` and `venue_setting.tier`, the two the spec names, from the shipped schema
//! rather than from the spec's prose.
//!
//! Every NULL-testing index is still OBSERVED and must still be PINNED with its verdict and its
//! reason, so the literal rule survives as the classification duty: a new one reddens
//! [`every_null_predicate_is_classified`] until its author writes down which of the two it is.
//! That is the repo's per-venue-table idiom — a named row proves the case was classified rather
//! than forgotten.
//!
//! # ⚠ This lands GREEN with the debt pinned, NOT red, and the instruction to the contrary is
//! self-defeating
//!
//! §9's stage 1 schedules this gate *"written against the CURRENT schema, failing … they document
//! the debt"*. A red gate cannot be landed at all — `crates/vike-secrets/tests/store_link_gate.rs`
//! says so in its own words for the sibling gate, and that one shipped as a ratchet for the same
//! reason. The debt is documented by [`NULL_PREDICATE_PIN`]'s `KindDiscriminator` rows, which name
//! it, and by [`the_pin_has_no_stale_rows`], which turns removing one into a required edit here.
//!
//! ⚠ **No count of what a later stage removes is written here, and one WAS — wrongly.** This
//! paragraph said the `venue_arming` drop "shrinks this array by four rows"; it is the rows naming
//! `venue_arming_one_per_venue` and `venue_arming_one_per_account`, and a `venue_setting` fix
//! takes the rows naming `venue_setting_one_per_tier` and `venue_setting_one_per_machine`. Nothing
//! gates a derived count stated in prose, which is why this repo turns counts into declared array
//! lengths — so the retirements are NAMED instead, and [`the_pin_has_no_stale_rows`] prints
//! exactly which lines to delete when the day comes.
//!
//! # A sibling gate pins the same four
//!
//! `crates/vike-ops/tests/settings_store_ddl_gate.rs` compares the shipped `DDL` against §3's
//! printed schema (§7 item 5), and its `SPEC_DRIFT_PIN` carries the same four indexes as DRIFT
//! rather than as ruling-2 debt — `venue_setting`'s two under rows of their own, `venue_arming`'s
//! two folded into its `table:venue_arming` row, which says so. The two gates ask different
//! questions of the same four lines and neither subsumes the other: that one goes green when the
//! DDL matches the SPEC, this one when the schema stops letting a NULL decide a row's kind.
//! **Whoever retires one of these indexes has to edit both files**, and the failure message here
//! will not mention the other — hence this paragraph.
//!
//! ⚠ That pin also shows §3's plan landing exactly the shape [`Route::NaturalKey`] exists for: its
//! `venue_setting.unique:venue_id,tier,field` row says *"§3 replaces the two partial indexes below
//! with ONE total `UNIQUE`"*. A TOTAL `UNIQUE` with no partial index left carries no NULL test and
//! this gate observes nothing, which is the win; a total `UNIQUE` with ONE partial index still
//! beside it is the defect, and only that Route can see it.
//!
//! # Declared residuals
//!
//! * **A SINGLE-direction NULL index whose column reaches NEITHER of [`Route`]'s two paths is
//!   classified `Filter`.** One route was added after review measured the gap the other left: a
//!   table-level `UNIQUE (…)` is a uniqueness rule that no index `WHERE` mentions, so §3's
//!   replacement shipping `UNIQUE (venue_id, account_id)` beside a lone
//!   `CREATE UNIQUE INDEX … WHERE account_id IS NULL` would have re-landed ruling 2's defect inside
//!   the redesign this gate exists to guard, deriving `Filter` while the author pinned `Filter` and
//!   [`every_pinned_verdict_matches_the_schema`] AGREED. [`Route::NaturalKey`] closes that. What
//!   remains uncovered is a partition whose OTHER half is enforced by nothing declarative at all —
//!   application code, or a uniqueness rule nobody wrote down. The author still has to write a pin
//!   row and argue it, so it is not invisible; the gate will not say the word.
//! * **[`DDL`] is the whole scope of the pin.** `crates/vike-secrets/src/profile_store.rs`'s
//!   `profile_ddl` renders a SECOND schema in this crate.
//!   [`the_profile_store_schema_carries_no_kind_discriminator`] runs the same derivation over it —
//!   measured, not assumed away — but does not pin its filters, so that schema is covered for
//!   ruling-2 offences only.
//! * **A `CHECK` constraint is not read.** `mount`'s `CHECK ((symbol IS NULL) != (token_id IS
//!   NULL))` is a mutual-exclusion rule rather than a uniqueness partition; this gate reads index
//!   `WHERE` clauses, which is the surface §7 item 2 names.

use std::collections::{BTreeMap, BTreeSet};

use vike_secrets::DDL;
use vike_secrets::profile_store::profile_ddl;

/// **How the schema says a column's nullness selects a uniqueness rule.** Two paths, because a
/// uniqueness rule can be declared in two places and only one of them is an index `WHERE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// Partial unique indexes on the same table test the column in BOTH directions — each kind of
    /// row has its own rule and the NULL picks which. `venue_arming.label`, `venue_setting.tier`.
    BothDirections,
    /// The column is NULL-tested by a partial index AND is part of its table's NATURAL KEY (a
    /// table-level `UNIQUE (…)` / `PRIMARY KEY (…)` list, or an inline `UNIQUE` column
    /// constraint). The table constraint is the rule for the rows the partial index excludes, so
    /// the partition is the same one — written in two different places.
    ///
    /// ⚠ **Nothing in [`DDL`] takes this path today, and it exists for a shape §3 is about to
    /// ship.** `UNIQUE (venue_id, account_id)` beside a lone `… WHERE account_id IS NULL` is
    /// ruling 2's defect wearing one index instead of two, and [`Route::BothDirections`] cannot
    /// see it. [`the_natural_key_route_derives_a_discriminator`] is its fixture, so this arm is
    /// not merely asserted to work.
    NaturalKey,
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Route::BothDirections => "tested in both directions",
            Route::NaturalKey => "also part of the table's natural key",
        })
    }
}

/// What a NULL test in a partial index's `WHERE` clause IS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// **The debt.** The column's nullness selects which uniqueness rule applies — i.e. which KIND
    /// of row this is — along one of [`Route`]'s two paths. Ruling 2 refuses this shape.
    KindDiscriminator,
    /// A subset selector. No index constrains the complement, so no row's kind is being decided.
    Filter,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Verdict::KindDiscriminator => "KindDiscriminator",
            Verdict::Filter => "Filter",
        })
    }
}

/// **Every `IS [NOT] NULL` test carried by a partial index in [`DDL`]**, as
/// `(index, column, verdict, why)`.
///
/// ⚠ **The declared length is the COUNT** — no prose anywhere, this file included, may restate it.
///
/// A `KindDiscriminator` row is DEBT and names the spec section that retires it. A `Filter` row is
/// ACCEPTED and its fourth field is the argument for why it is not a discriminator; it is written
/// down rather than left out so that a later change flipping it into one (adding the complementary
/// index) reddens [`every_pinned_verdict_matches_the_schema`] instead of passing unread.
const NULL_PREDICATE_PIN: [(&str, &str, Verdict, &str); 7] = [
    (
        "account_one_account_per_book",
        "venue_account_id",
        Verdict::Filter,
        "⚠ the one Filter the SPEC does not rule on — this verdict is the gate's own derivation. \
         An account whose book the venue has not answered for yet cannot collide with another \
         over a number it does not have: nothing constrains the complement, and \
         `venue_account_id` is no part of `account`'s `UNIQUE (venue, tier, label)`, so neither \
         Route reaches it. The index's other predicate (`active = 1`) is the one ruling 9 is about",
    ),
    (
        "credential_one_live_name",
        "superseded_at",
        Verdict::Filter,
        "EXEMPTED BY NAME in spec §6 — *\"a NULL that means 'not yet'. It does not discriminate a \
         KIND of row, and the timestamp is genuinely useful, so ruling 2 does not reach it\"*. \
         §4.2's rollback copies are the SAME kind of row as the live one and are deliberately \
         kept, so nothing constrains the superseded complement",
    ),
    (
        "credential_one_live_value",
        "superseded_at",
        Verdict::Filter,
        "the same §6-exempted lifecycle stamp as `credential_one_live_name`, over \
         `(account_id, field)` instead of `name`",
    ),
    (
        "venue_arming_one_per_account",
        "label",
        Verdict::KindDiscriminator,
        "spec §2.2 — the labelled half of `venue_arming`'s split. Retires with §3's \
         `venue_arming — DELETED`, whose remaining blocker §9's as-built note states: a home for \
         an arming decision naming no account",
    ),
    (
        "venue_arming_one_per_venue",
        "label",
        Verdict::KindDiscriminator,
        "spec §2.2, the defect this gate is named for — `label IS NULL` means both the venue's \
         ceiling and the unlabelled account's. Retires with its sibling row above",
    ),
    (
        "venue_setting_one_per_machine",
        "tier",
        Verdict::KindDiscriminator,
        "spec §2.6 — `tier IS NULL` is the machine-scoped row. The spec calls this shape identical \
         to `venue_arming`'s and schedules no stage for it, so this pair outlives the pair above",
    ),
    (
        "venue_setting_one_per_tier",
        "tier",
        Verdict::KindDiscriminator,
        "spec §2.6, the tier-scoped half of the same split",
    ),
];

const GROWTH_GUIDANCE: &str = "\
A partial index's `WHERE` now tests a column for NULL and no row here says which of the two shapes
it is. Decide, and write the row:

  * `Verdict::KindDiscriminator` — this column's nullness decides which uniqueness rule applies,
    either because the table tests it in BOTH directions or because it is also part of the table's
    natural key. Owner ruling 2 REFUSES this on principle, even where it is unambiguous: give the
    two kinds a column that says which they are, or two tables. Adding a row here is for debt that
    a named stage already owes, never for new work.
  * `Verdict::Filter` — the index constrains a SUBSET and nothing constrains the complement, so no
    row's kind is being decided. `credential`'s `superseded_at IS NULL` pair is the worked example,
    and spec §6 exempts it by name.

The verdict is DERIVED from the schema, not chosen: writing the wrong word here fails
`every_pinned_verdict_matches_the_schema` rather than passing.";

// -------------------------------------------------------------------------------------------
// The parser — statements, then indexes, then the NULL tests in their `WHERE` clauses
// -------------------------------------------------------------------------------------------

/// One `CREATE [UNIQUE] INDEX` statement, reduced to what this gate reads.
#[derive(Debug, Clone)]
struct Index {
    name: String,
    table: String,
    /// The tokens after `WHERE`; empty for a full (non-partial) index.
    predicate: Vec<String>,
}

/// One `<column> IS [NOT] NULL` test, located.
#[derive(Debug, Clone)]
struct NullTest {
    index: String,
    table: String,
    column: String,
    /// `true` for `IS NOT NULL`.
    negated: bool,
}

/// Strip the punctuation a token may carry from the SQL around it — `(venue,` -> `venue`.
fn bare(token: &str) -> &str {
    token.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'))
}

/// Every `CREATE … INDEX` statement in a schema.
///
/// ⚠ Driven by SPLITTING ON `;` and then classifying each statement, deliberately NOT by splitting
/// on a `"CREATE UNIQUE INDEX IF NOT EXISTS "` literal. A literal split makes the parser's own
/// count uncheckable — the only independent needle left would be a count of the same literal, which
/// agrees with the split by construction. The review of the sibling gate measured that shape;
/// [`the_parser_sees_every_index_in_the_ddl`] can check THIS one because its needle (the bare word
/// `INDEX`) and this split are different derivations.
fn indexes(ddl: &str) -> Vec<Index> {
    let mut out = Vec::new();
    for statement in ddl.split(';') {
        let tokens: Vec<&str> = statement.split_whitespace().collect();
        if !tokens.iter().any(|t| t.eq_ignore_ascii_case("INDEX")) {
            continue;
        }
        let on = tokens
            .iter()
            .position(|t| t.eq_ignore_ascii_case("ON"))
            .expect("a CREATE INDEX statement names the table it is ON");
        assert!(on >= 1, "a CREATE INDEX statement names the index before `ON`: {statement}");
        let name = bare(tokens[on - 1]).to_string();
        let table =
            bare(tokens.get(on + 1).expect("a CREATE INDEX statement names a table after `ON`"))
                .to_string();
        let predicate = match tokens.iter().position(|t| t.eq_ignore_ascii_case("WHERE")) {
            Some(w) => tokens[w + 1..].iter().map(|t| (*t).to_string()).collect(),
            None => Vec::new(),
        };
        out.push(Index { name, table, predicate });
    }
    out
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

/// `(table name, column/constraint body)` for every `CREATE TABLE` in a schema.
///
/// Driven by the same statement split [`indexes`] uses, for the same reason, and cross-checked the
/// same way by [`the_parser_sees_every_table_in_the_ddl`].
fn tables(sql: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for statement in sql.split(';') {
        let is_table = statement.split_whitespace().any(|t| t.eq_ignore_ascii_case("TABLE"));
        if !is_table {
            continue;
        }
        let Some(open) = statement.find('(') else { continue };
        let name = statement[..open]
            .split_whitespace()
            .next_back()
            .map(bare)
            .expect("a CREATE TABLE names the table before its body")
            .to_string();
        let Some(close) = matching_paren(statement, open) else { continue };
        out.push((name, statement[open + 1..close].to_string()));
    }
    out
}

/// The index of the `)` closing the `(` at `open`.
fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices().skip_while(|(i, _)| *i < open) {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// A table body's top-level comma-separated parts — parenthesis-aware, so `CHECK (x IN (0, 1))`
/// stays one part instead of becoming three.
fn top_level_parts(body: &str) -> Vec<String> {
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

fn pinned() -> BTreeMap<(String, String), Verdict> {
    NULL_PREDICATE_PIN
        .iter()
        .map(|(index, column, verdict, _)| {
            (((*index).to_string(), (*column).to_string()), *verdict)
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
/// [`the_pin_has_no_stale_rows`] — seven pinned rows would go stale at once — but the profile
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
    let seen = tables(DDL).len();
    let expected = DDL.split_whitespace().filter(|t| t.eq_ignore_ascii_case("TABLE")).count();
    assert_eq!(
        seen, expected,
        "the parser found {seen} table(s) but `DDL` uses the word TABLE {expected} time(s) — a \
         table it drops is one whose natural key `Route::NaturalKey` cannot see"
    );
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
