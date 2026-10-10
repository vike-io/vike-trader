//! **§5.2 step 7's `'any'` word, held at its two boundaries on the shipped shape.**
//!
//! `venue_setting.tier` is `NOT NULL`: a machine-scoped row ("applies to any tier") is STORED as the
//! literal `'any'`, and ONE total `UNIQUE (venue_id, tier, field)` constrains every row.
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §2.6 diagnoses the shape
//! it replaced — a NULL in `tier` and two PARTIAL unique indexes split on it — and owner ruling 2
//! refuses a NULL that decides what KIND of row this is.
//!
//! A stored `'any'` that reaches a reader UNMAPPED makes a machine-scoped row UNREACHABLE. Not
//! wrong: invisible. The reader is `vike_secrets::venue_setting::load_venue_settings`, whose
//! `VenueSettings::from_rows` SKIPS a tier word it does not know, so nothing errors, the reader
//! answers "not configured", and a bridge falls back to its built-in default — which is exactly how
//! the polymarket egress proxy was read by nothing once before. So every row assertion here is
//! paired with one made THROUGH THE PUBLIC READER.
//!
//! ⚠ Values are obviously fake. No real credential exists anywhere near this file.

use std::collections::BTreeMap;

use vike_secrets::venue_setting::{SettingTier, VenueSettings};

use crate::support::Fixture;

/// A store born on the shipped shape (`secrets init`) holding one machine-scoped polymarket row,
/// filed through the public venue-setting writer.
fn with_a_machine_scoped_row() -> Fixture {
    let fx = Fixture::initialised();
    let was = vike_secrets::set_venue_setting_in(
        fx.dir(),
        "polymarket",
        None,
        "PROXY_HOST",
        "proxy.example.invalid",
    )
    .expect("a machine-scoped write");
    assert_eq!(was, None, "the write created the row; it replaced nothing");
    fx
}

/// `(id, venue, tier, field, value)` for every `venue_setting` row, ordered by id, the venue read
/// by its number.
fn rows(fx: &Fixture) -> Vec<(i64, String, String, String, String)> {
    let conn = fx.conn();
    let mut stmt = conn
        .prepare(
            "SELECT t.id, v.name, t.tier, t.field, t.value FROM venue_setting t \
             JOIN venue v ON v.id = t.venue_id ORDER BY t.id",
        )
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect()
}

/// Try one raw `venue_setting` INSERT, returning the engine's verdict. The row names its venue by
/// number, so a refusal is about the `tier` or the `UNIQUE` this file asks about and never about a
/// missing `venue_id`.
fn try_insert(
    fx: &Fixture,
    venue: &str,
    tier: Option<&str>,
    field: &str,
) -> rusqlite::Result<usize> {
    fx.conn().execute(
        "INSERT INTO venue_setting (venue_id, tier, field, value) \
         VALUES ((SELECT id FROM venue WHERE name = ?1), ?2, ?3, 'x')",
        (venue, tier, field),
    )
}

/// The `venue_setting` read every composition root performs.
fn settings(fx: &Fixture) -> BTreeMap<String, VenueSettings> {
    vike_secrets::venue_setting::load_venue_settings(fx.dir()).expect("the rows read")
}

/// **The WRITE boundary** — a machine-scoped write lands on the `'any'` row, reports the value it
/// replaced, and duplicates nothing.
///
/// A writer that bound SQL NULL for "no tier" would be refused by the `NOT NULL`; one that looked
/// the previous value up with `tier IS NULL` would report `None` for a row that plainly had a value.
/// Both are the same boundary forgotten on the other side.
#[test]
fn a_machine_scoped_write_replaces_the_any_row() {
    let fx = with_a_machine_scoped_row();
    let first = rows(&fx);
    assert_eq!(first.len(), 1, "one row filed: {first:?}");
    assert_eq!(first[0].2, "any", "the machine scope is STORED as the word, never as NULL");

    let was = vike_secrets::set_venue_setting_in(
        fx.dir(),
        "polymarket",
        None,
        "PROXY_HOST",
        "new.invalid",
    )
    .expect("a second machine-scoped write must land");
    assert_eq!(
        was.as_deref(),
        Some("proxy.example.invalid"),
        "the write must report the value it REPLACED — the `'any'` row is the same row"
    );

    let hosts = rows(&fx);
    assert_eq!(hosts.len(), 1, "the upsert DUPLICATED the machine-scoped row: {hosts:?}");
    assert_eq!(
        (hosts[0].0, hosts[0].2.as_str(), hosts[0].4.as_str()),
        (first[0].0, "any", "new.invalid"),
        "same id, stored as `'any'`, new value"
    );
    assert_eq!(
        settings(&fx)["polymarket"].get(SettingTier::Any, "proxy_host"),
        Some("new.invalid"),
        "…and the reader answers the new value, at the MACHINE tier"
    );
}

/// **`Some("any")` is REFUSED by the write boundary — it is not a second spelling of `None`.**
///
/// Step 7 added `'any'` to the `CHECK`, and a write boundary that passed a `Some("any")` straight
/// through would file it as the MACHINE-SCOPED row, overwriting it and returning its old value as
/// `previous` with nothing erroring (a reviewer found the contract and the behaviour disagreeing on
/// the branch that made the change). The map between the word and "no tier" is only one place per
/// direction if it is a bijection: `'any'` is reached from `None` and from nothing else. No
/// production caller can produce `Some("any")` today (`parse_venue_setting_key` classifies by
/// `account_tier_named`, which does not know the word), so this is the contract a future caller
/// meets, held where it cannot drift.
#[test]
fn the_stored_word_is_refused_as_a_tier_by_the_venue_setting_writer() {
    let fx = with_a_machine_scoped_row();
    let before = rows(&fx);

    let err = vike_secrets::set_venue_setting_in(
        fx.dir(),
        "polymarket",
        Some("any"),
        "PROXY_HOST",
        "aliased.invalid",
    )
    .expect_err("`Some(\"any\")` names no tier and must be REFUSED, not filed as the machine row");
    let text = err.to_string();
    assert!(
        text.contains("pass `None`"),
        "the refusal must say what the caller meant to pass, not surface a bare engine error: \
         {text}"
    );
    assert_eq!(rows(&fx), before, "a refused write must change no row");
    assert_eq!(
        settings(&fx)["polymarket"].get(SettingTier::Any, "proxy_host"),
        Some("proxy.example.invalid"),
        "…and the machine-scoped row still answers its OWN value"
    );
}

/// **ONE total `UNIQUE` keeps the two scopes apart** — the guarantee a pair of partial indexes
/// once gave, given by one constraint with no NULL deciding which half applies.
#[test]
fn the_total_unique_keeps_one_row_per_key_and_the_scopes_apart() {
    let fx = with_a_machine_scoped_row();

    let dup = try_insert(&fx, "polymarket", Some("any"), "PROXY_HOST")
        .expect_err("a second `'any'` row for one key must be REFUSED");
    assert!(dup.to_string().contains("UNIQUE"), "…by the total UNIQUE: {dup}");
    assert_eq!(
        try_insert(&fx, "polymarket", Some("live"), "PROXY_HOST").expect("a tier-scoped neighbour"),
        1,
        "a tier-scoped row for the same field is a DIFFERENT row"
    );
    let bad = try_insert(&fx, "polymarket", None, "PROXY_PORT").expect_err("NULL must be refused");
    // ⚠ The message must NAME the tier column: the row resolves its venue, so the refused column
    // is `tier` and nothing else.
    let text = bad.to_string();
    assert!(
        text.contains("NOT NULL") && text.contains("venue_setting.tier"),
        "a NULL tier is refused by the tier column itself: {text}"
    );
}
