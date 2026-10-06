//! Proof 21 - venue_id is filled on every write path that reaches ensure_venue_id_columns.

use super::*;

// For `support::pre_stage_2_ddl` alone — this file keeps its own `Fixture`, and nothing else of the
// shared module is used here.
use crate::support;

// ---------------------------------------------------------------------------------------------
// PROOF 21 — `venue_id` is filled, and nothing errors, on EVERY write path that reaches
// `ensure_venue_id_columns` — not only the one path (`fill_into`) this file already exercised
// ---------------------------------------------------------------------------------------------

/// Build a store already at [`vike_secrets::SCHEMA_VERSION`] but shaped like every box that
/// reached it BEFORE the `venue` table and the four `venue_id` columns existed — the state both
/// live boxes were in per the root `CLAUDE.md`, until a writer topped them up. Derived from the
/// shipped `DDL` by `support::pre_stage_2_ddl` (each removal asserted to match once) rather than
/// hand-written, so its venue half cannot silently drift from the batch, and then given the one
/// account and credential a migration of `BINANCE_DEMO_API_KEY` files. ⚠ Only its venue half is
/// old: it keeps `AUTOINCREMENT`, `'paper'` and step 7 (that helper's doc says why), and the
/// missing `venue` table and `venue_id` columns are all these tests are about.
///
/// ⚠ **It was built by DROPPING what a normal create made until the venue-links flip** — the
/// technique `crates/vike-secrets/src/db.rs`'s own
/// `ensure_venue_rows_creates_the_table_on_a_store_that_predates_it` still uses for `venue` alone.
/// That stopped working for the columns: `account.venue_id` is named by `UNIQUE (venue_id, tier,
/// label)` and by the book index now, and SQLite refuses to drop a column an index names
/// (`error in table account after drop column: no such column: venue_id`, measured).
fn planted_schema_2_without_venue(fx: &Fixture) {
    std::fs::write(fx.store(), "BINANCE_DEMO_API_KEY=seed\n").expect("seed one key");
    let conn = support::sql::plant_ddl(&fx.db(), &support::pre_stage_2_ddl());
    conn.execute_batch(
        "DROP TABLE venue;
         INSERT INTO account (id, venue, tier) VALUES (1, 'binance', 'demo');
         INSERT INTO credential (id, account_id, field, value, name)
             VALUES (1, 1, 'API_KEY', 'seed', 'BINANCE_DEMO_API_KEY');",
    )
    .expect("simulate a pre-stage-2 schema-2 store");
    support::sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
}

/// **Regression for the final-review fix wave's Finding 1.** `ensure_venue_id_columns` had exactly
/// ONE caller (`fill_into`) that also called `ensure_venue_rows` first, by hand, immediately before
/// it. The other four callers — `edit_account`, `move_pending_rows`,
/// `crate::settings::write_settings`, `crate::settings::set_venue_setting_in` — each ran their own
/// bare `execute_batch(DDL)` (creating `venue` EMPTY, since `DDL` is `CREATE TABLE IF NOT EXISTS`)
/// and called `ensure_venue_id_columns` directly, whose backfill sub-select then found no roster
/// rows and silently set every `venue_id` to NULL. `write_settings_in` is the exact path
/// `vike-cli config mirror` takes on a real box.
#[test]
fn write_settings_fills_venue_id_even_when_venue_predates_it() {
    let fx = Fixture::empty();
    planted_schema_2_without_venue(&fx);

    vike_secrets::write_settings_in(
        fx.dir(),
        &vike_secrets::StoredSettings {
            arming: vec![vike_secrets::ArmingRow {
                venue: "binance".to_string(),
                label: None,
                mode: "demo".to_string(),
                max_exposure: None,
            }],
            ..Default::default()
        },
    )
    .expect("write a venue_arming row on a store that predates `venue`");

    let conn = fx.conn();
    let venue_id: Option<i64> = conn
        .query_row("SELECT venue_id FROM venue_arming WHERE venue = 'binance'", [], |r| r.get(0))
        .expect("the row exists");
    let expected: i64 = conn
        .query_row("SELECT id FROM venue WHERE name = 'binance'", [], |r| r.get(0))
        .expect("the roster top-up must have run");
    assert_eq!(
        venue_id,
        Some(expected),
        "venue_id must be filled from the FRESHLY TOPPED-UP roster, not left NULL because `venue` \
         was still empty when the sub-select ran"
    );
}

/// The identical regression through the SECOND non-`fill_into` writer that creates a row carrying
/// `venue`: `edit_account`'s `Create` arm.
#[test]
fn edit_account_fills_venue_id_even_when_venue_predates_it() {
    let fx = Fixture::empty();
    // The seed key in `planted_schema_2_without_venue` already creates an UNLABELLED
    // `(binance, demo)` account, so this test creates one at a DIFFERENT tier — `live` — to avoid
    // `guard_account_label`'s own, unrelated `AmbiguousUnlabelledAccount` refusal (two unlabelled
    // rows at one `(venue, tier)`), which is not what this test is about.
    planted_schema_2_without_venue(&fx);

    vike_secrets::edit_account(
        &fx.db(),
        vike_secrets::AccountEdit::Create { venue: "binance", tier: "live", label: None },
    )
    .expect("create an account on a store that predates `venue`");

    let conn = fx.conn();
    // By its tier alone: the write carried `account` onto the shipped shape, which has had no text
    // `venue` since the venue-links plan's second release, and the planted store holds one `live`
    // account — the one this write created.
    let venue_id: Option<i64> = conn
        .query_row("SELECT venue_id FROM account WHERE tier = 'live'", [], |r| r.get(0))
        .expect("the row exists");
    let expected: i64 = conn
        .query_row("SELECT id FROM venue WHERE name = 'binance'", [], |r| r.get(0))
        .expect("the roster top-up must have run");
    assert_eq!(venue_id, Some(expected), "same defect, the account-creation writer");
}

/// **What Finding 2, as described, is NOT: `write_settings` does not merely fail differently on a
/// genuine schema-1 store — it was ALREADY refusing one, before this branch, for a reason that has
/// nothing to do with `venue`.** `write_settings`'s own `tx.execute_batch(crate::schema::DDL)` — a
/// line that predates Task 1/2/3 entirely — runs `CREATE UNIQUE INDEX IF NOT EXISTS
/// credential_one_live_value ON credential (account_id, field) WHERE superseded_at IS NULL;` (part
/// of the ORIGINAL 2026-09-14 schema-2 rollout, `docs/superpowers/specs/
/// 2026-09-14-the-credential-schema.md`), and a genuine schema-1 `credential` —
/// `(name TEXT PRIMARY KEY, value TEXT)` — has neither `account_id` nor `field`. That statement
/// fails to PREPARE with `no such column: account_id`, at `write_settings`'s OWN first DDL
/// statement, before `ensure_arming_columns` or `ensure_venue_id_columns` (this fix wave's own
/// code) is ever reached. `edit_account`'s `version < ACCOUNT_TABLE_SCHEMA` guard is therefore the
/// ONLY one of the four non-`fill_into` writers that was ever actually protected against a
/// schema-1 store; `move_pending_rows` and `set_venue_setting_in` share this same exposure.
///
/// This is PINNED here — asserting the CURRENT failure, by its actual error text — as the honest
/// record of a real discovery this fix wave's own regression testing surfaced, deliberately NOT
/// fixed in this wave (it is unrelated to `venue`/`venue_id`, predates this whole branch, and no
/// live box has ever hit it: both have been at schema 2 since 2026-09-14, before `write_settings`
/// existed at all). Reported to the coordinator rather than silently patched around. If this test
/// ever starts PASSING (config mirror stops refusing schema 1), that fix landed — delete this test
/// and consider `ensure_venue_id_columns`'s own `has_column(tx, table, "venue")` guard for
/// `credential` finally reachable in practice; see
/// [`ensure_venue_id_columns_skips_a_credential_table_with_no_venue_column`]'s own doc (in
/// `crates/vike-secrets/src/db.rs`'s `db_tests`) for where that guard IS exercised today, in
/// isolation from this crash.
#[test]
fn write_settings_still_refuses_a_genuine_schema_1_store_for_an_unrelated_pre_existing_reason() {
    let fx = Fixture::empty();
    vike_secrets::plant_schema_1(&fx.db(), &[], &[]).expect("plant a schema-1 store");

    let err = vike_secrets::write_settings_in(fx.dir(), &vike_secrets::StoredSettings::default())
        .expect_err("must still refuse — this is a PRE-EXISTING gap, not one this fix wave closes");
    let message = err.to_string();
    assert!(
        message.contains("account_id"),
        "the refusal must be the KNOWN pre-existing `credential_one_live_value` index crash \
         (`no such column: account_id`), not `venue`/`venue_id` — a different failure text here \
         means either this bug was fixed (see this test's own doc) or a NEW one was introduced: {message}"
    );
}
