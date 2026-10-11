//! The pin: every `IS [NOT] NULL` test a partial index carries, its verdict, and the argument for it.

use std::collections::BTreeMap;

/// **How the schema says a column's nullness selects a uniqueness rule.** Two paths, because a
/// uniqueness rule can be declared in two places and only one of them is an index `WHERE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// Partial unique indexes on the same table test the column in BOTH directions — each kind of
    /// row has its own rule and the NULL picks which. Nothing in [`DDL`] takes it any more:
    /// `venue_setting.tier` did until §5.2 step 7, and `venue_arming.label` until decision 0119.
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
pub(super) enum Verdict {
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
pub(super) const NULL_PREDICATE_PIN: [(&str, &str, Verdict, &str); 3] = [
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
    // ⚠ TWO `KindDiscriminator` rows stood here — `venue_arming_one_per_account` and
    // `venue_arming_one_per_venue`, both on `label`, spec §2.2: `label IS NULL` meant both the
    // venue's ceiling and the unlabelled account's. They are the last debt this pin carried, and
    // decision 0119 (2026-10-10) paid it by removing `venue_arming` from `DDL` altogether — an
    // older store keeps the table, but nothing reads or writes it — so `the_pin_has_no_stale_rows`
    // demanded the lines go.
    //
    // ⚠ TWO `KindDiscriminator` rows stood here — `venue_setting_one_per_machine` and
    // `venue_setting_one_per_tier`, both on `tier`, spec §2.6 — and they are the first debt this
    // pin has retired. §5.2 step 7 (2026-09-26) made `tier` `NOT NULL` with the stored word `'any'`
    // and replaced both indexes with one total `UNIQUE (venue, tier, field)`, so `DDL` carries no
    // NULL test on that table and `the_pin_has_no_stale_rows` demanded the lines go. The first row
    // predicted *"this pair outlives the pair above"*, on the ground that the spec scheduled no
    // stage for it; §9 later did, and it went first.
];

pub(super) const GROWTH_GUIDANCE: &str = "\
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

pub(super) fn pinned() -> BTreeMap<(String, String), Verdict> {
    NULL_PREDICATE_PIN
        .iter()
        .map(|(index, column, verdict, _)| {
            (((*index).to_string(), (*column).to_string()), *verdict)
        })
        .collect()
}
