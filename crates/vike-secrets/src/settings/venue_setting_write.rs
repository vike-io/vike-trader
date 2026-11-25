//! The operator's `venue_setting` writer: `set_venue_setting_in` (the write plus its journal record)
//! and the record-less body it runs first.

use vike_venues::venue_fields::VenueFieldName;

use super::*;

/// **Upsert ONE `venue_setting` row, with no record**, returning the value it replaced — `None` where the row is
/// new. The operator's writer for the venue settings (decision 0095).
///
/// ⚠ **Without it those ten values are read-only, which is the defect this workspace spent a day
/// removing for credentials.** `secrets set` writes the `credential` table, so after the move it
/// would not update a venue setting — it would create a SHADOW row, which the fold then reports as
/// a collision and resolves in the credential's favour. The operator would see their new value
/// ignored and no error anywhere.
///
/// ⚠ It touches ONE row. It does not clear the table, and nothing in this module may —
/// [`write_settings`]'s own ⚠ carries what a `DELETE FROM venue_setting` would cost.
///
/// `tier` is `None` for a machine-scoped row; the table stores that as `'any'` since §5.2 step 7,
/// and this function is one of the two writers that translate
/// (`crate::schema::stored_venue_setting_tier` is the translation). The tier is a
/// `vike_model::AccountTier` (decision 0129), so the stored word `'any'` has no Rust spelling as a
/// tier: the `Some("any")` that once overwrote the machine-scoped row (and was then refused here
/// by hand) cannot be written down any more.
///
/// # Errors
/// The engine, a store at an unreadable schema, or a `CHECK` the DDL enforces. A tier outside
/// `paper`/`demo`/`live` cannot reach it: the caller parsed a `vike_model::AccountTier`.
///
/// ⚠ **It writes NO change-journal record**, and it is the one venue-setting write that CREATES a
/// database where none is: production reaches it through [`set_venue_setting_in`] alone, which
/// refuses a box with no database first and records both outcomes. It is public to a TEST through
/// the `test-support` feature.
///
/// A new row is stored in the store spelling ([`VenueFieldName::store_spelling`], `SOCKS_PROXY`),
/// the one every writer uses. The field is matched in ANY case: a row of it spelled otherwise (only
/// hand-typed SQL or a test plant makes one; the table is never migrated) is the row the reader may
/// answer, so the write updates it in place and reports its value as the previous one, rather than
/// adding a row beside it that the reader would not answer.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn set_venue_setting_in_unjournalled(
    settings_dir: &Path,
    venue: &str,
    tier: Option<AccountTier>,
    field: &VenueFieldName,
    value: &str,
) -> Result<Option<String>, StoreError> {
    upsert_venue_setting(
        settings_dir,
        venue,
        tier,
        &field.store_spelling(),
        value,
        FieldMatch::AnyCase,
    )
}

/// **Plant ONE `venue_setting` row whose field is spelled `stored_field` VERBATIM**, in any case: a
/// spelling no writer stores (every one stores [`VenueFieldName::store_spelling`]), which only
/// hand-typed SQL can put in a store. For a test of the readers' tolerance of other spellings only;
/// the body is [`set_venue_setting_in_unjournalled`]'s, matching the field exactly, so the row is
/// linked by the venue's number as every row is.
///
/// # Errors
/// As [`set_venue_setting_in_unjournalled`].
#[cfg(feature = "test-support")]
pub fn plant_venue_setting_for_test(
    settings_dir: &Path,
    venue: &str,
    tier: Option<AccountTier>,
    stored_field: &str,
    value: &str,
) -> Result<Option<String>, StoreError> {
    upsert_venue_setting(settings_dir, venue, tier, stored_field, value, FieldMatch::Exact)
}

/// How [`upsert_venue_setting`] finds the row it replaces.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldMatch {
    /// The writer's: `field` is the store spelling and every spelling of it is the same field.
    AnyCase,
    /// The test plant's: only a row spelled exactly `field`.
    #[cfg(feature = "test-support")]
    Exact,
}

/// The one upsert body: `field` is the column's spelling, verbatim.
fn upsert_venue_setting(
    settings_dir: &Path,
    venue: &str,
    tier: Option<AccountTier>,
    field: &str,
    value: &str,
    matching: FieldMatch,
) -> Result<Option<String>, StoreError> {
    let db = crate::store_locator::db_path_in(settings_dir);
    // ⚠ §5.2 step 7's WRITE boundary — the tier is bound as the STORED word, in the lookup and the
    // upsert below alike. The funnel below carries the table onto
    // `tier TEXT NOT NULL`, so the `Option` itself would bind SQL NULL: refused outright by the
    // INSERT, and — the quiet half — matched by NOTHING in the lookup, which is well-formed either
    // way and would report `None` for a machine-scoped row that plainly holds a value. (This lookup
    // read `tier IS ?3` over the raw `Option` until step 7.)
    let stored_tier = crate::schema::stored_venue_setting_tier(tier);
    let Some(mut conn) = crate::db::open_for_write(&db)? else {
        return Err(StoreError { path: db, kind: crate::db::StoreErrorKind::NoDatabase });
    };
    // `IMMEDIATE`: the funnel and the lookup read before the upsert writes, and only a lock taken at
    // `BEGIN` waits.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| StoreError::sql(&db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| StoreError::sql(&db, e))?;
    // ⚠ This writer names `venue_id` in its own `venue_setting` INSERT below — see
    // `crate::db::ensure_venue_rows`'s own doc for why it needs this call rather than the bare DDL
    // batch above.
    crate::db::ensure_venue_rows(&tx).map_err(|e| StoreError::sql(&db, e))?;

    // The row it replaces, read inside the same transaction so the report cannot describe a value
    // some other writer changed in between. Found by the venue's NUMBER: `venue_id` is `NOT NULL`,
    // so every row holds one (`crate::schema::venue_is`). Matched in any case, it is the row the
    // reader answers: `read_settings` sorts by field in byte order and `VenueSettings::from_rows`
    // keeps the LAST row of a field, so the greatest spelling.
    let venue_is = crate::schema::venue_is("venue_id", "?1");
    let field_is = match matching {
        FieldMatch::AnyCase => "upper(field) = ?2",
        #[cfg(feature = "test-support")]
        FieldMatch::Exact => "field = ?2",
    };
    let previous: Option<String> = tx
        .query_row(
            &format!(
                "SELECT value FROM venue_setting WHERE {venue_is} AND {field_is} AND tier = ?3 \
                 ORDER BY field DESC LIMIT 1"
            ),
            (venue, field, stored_tier),
            |r| r.get(0),
        )
        .ok();

    // Every row of the field, in whatever case it is spelled, takes the value, so whichever the
    // reader answers is the new one; only a field with no row at all inserts one below.
    let updated = match matching {
        FieldMatch::AnyCase => tx
            .execute(
                &format!(
                    "UPDATE venue_setting SET value = ?4 WHERE {venue_is} AND tier = ?3 AND {field_is}"
                ),
                (venue, field, stored_tier, value),
            )
            .map_err(|e| StoreError::sql(&db, e))?,
        #[cfg(feature = "test-support")]
        FieldMatch::Exact => 0,
    };
    if updated > 0 {
        tx.commit().map_err(|e| StoreError::sql(&db, e))?;
        return Ok(previous);
    }

    // ⚠ `venue_id = excluded.venue_id` on the conflict path too, not only `value` — DEFENSIVE, and a
    // DECLARED BLIND SPOT rather than a covered branch.
    //
    // It makes this statement correct ON ITS OWN.
    //
    // ⚠ **It cannot fire today, and here is why**: `crate::schema::DDL`'s `UNIQUE (venue_id, tier,
    // field)` is the table's only conflict key, and a conflict on it is a row holding the same
    // `venue_id` already, so the clause sets it to itself.
    //
    // ⚠ **This is therefore NOT regression-guarded by any test, deliberately, and a mutation of this
    // clause will NOT redden the suite** — MEASURED, when `every_writer_names_its_venue_by_number_on_every_row`
    // still reached this branch (a second write of the same key), green with this clause removed, for
    // precisely the reason above. Do not delete this line after finding no test fails for it; that
    // absence is the point being documented, not evidence the line is dead code. (The writer's second
    // write of a key now takes the `UPDATE` above; only the test plant's exact match reaches here.)
    let link = crate::schema::VenueLink::of(&tx, "venue_setting", "t")
        .map_err(|e| StoreError::sql(&db, e))?;
    tx.execute(
        &format!(
            "INSERT INTO venue_setting ({}, tier, field, value) VALUES ({}, ?2, ?3, ?4) \
             ON CONFLICT DO UPDATE SET value = excluded.value, venue_id = excluded.venue_id",
            link.columns,
            link.values("?1")
        ),
        (venue, stored_tier, field, value),
    )
    .map_err(|e| StoreError::sql(&db, e))?;
    tx.commit().map_err(|e| StoreError::sql(&db, e))?;
    Ok(previous)
}

/// What a SECRET venue field's value is written as in the change journal.
const REDACTED: &str = "<secret>";

/// **[`set_venue_setting_in`]'s `Err` — the write's own refusal, paired with whether
/// recording THAT refusal in the change journal also succeeded.** Boxed: clippy's
/// `result_large_err` correctly refuses a bare `(StoreError, Option<ChangeAppendError>)` inline in
/// every `Result` this function's callers propagate (`StoreError` alone stays unboxed everywhere
/// else in this crate; it is the PAIR that crosses the threshold), and `type_complexity` asks for
/// the tuple to be named rather than spelled at the return type. A type alias over a raw tuple
/// rather than a named struct: the two cells have no name of their own beyond "the write's error"
/// and "the journal's", which a struct's field names would only restate.
pub type VenueSettingRefusal = Box<(StoreError, Option<crate::ChangeAppendError>)>;

/// **THE venue-setting write: [`set_venue_setting_in_unjournalled`] plus its `set_setting` record in
/// the change journal** — the venue-setting twin of [`crate::save_credentials_to_store`], and the ONE
/// `venue_setting` writer (`vike-cli config set venue.*` and the desktop both reach it).
///
/// The journal is the EXISTING change journal every settings write uses
/// (`vike_change_journal`, beside the store: `<settings_dir>/state/changes`), with `file` =
/// `venue` and the operator's dotted key. BOTH outcomes are recorded, as `vike-cli`'s
/// `settings_write` records them: a refused write carries its reason. ⚠ A `secret` field's value —
/// old and new — is recorded as `<secret>`, never verbatim.
///
/// A journal failure cannot fail the CALL — the underlying write's own outcome (applied or
/// refused) is always reported unchanged — but it must never be swallowed either, on EITHER
/// outcome: it comes back as `Some(ChangeAppendError)`, paired with whichever half of the result
/// it belongs to, for the caller to report.
///
/// ⚠ **This is the shape `crates/vike-cli/src/cmd/settings_write.rs`'s
/// `set_setting_journalled_within` already uses for the identical problem, carried here as
/// RETURNED DATA rather than as an `eprintln!`** — this crate's writers log nothing and
/// hand findings to their caller instead (see the crate doc's Redaction section, and
/// [`crate::ChangeAppendError`]'s own doc). A prior version of this function paired the journal
/// outcome with the result via `result.map(|previous| (previous, journal_error))`, which is a
/// no-op on `Err` — so a REFUSED write whose OWN refusal record also failed to append reported
/// only the refusal, and the caller had no way to learn the ledger did not record it either.
/// Found in review before any downstream task depended on the signature; the `Err` arm now carries
/// the same pair the `Ok` arm always has, rather than silently dropping half of it.
///
/// ⚠ **It CREATES NO DATABASE**: a box with no settings database is refused with
/// `StoreErrorKind::NoDatabase`, exactly as [`crate::edit_account_in`] refuses one — the database's
/// mere existence is the credential store's per-run backend choice, so creating one from a
/// venue-setting write would hand the box an EMPTY credential store (`docs/decisions/0036`,
/// reason 1). That
/// refusal carries no journal outcome at all (`Err((e, None))`): nothing beside a nonexistent
/// database has a journal directory to open either, and no change was attempted worth recording.
///
/// # Errors
/// Every refusal [`set_venue_setting_in_unjournalled`] states, and `NoDatabase` — each paired with the outcome
/// of recording THAT refusal in the change journal (`None` for the `NoDatabase` case above).
pub fn set_venue_setting_in(
    settings_dir: &Path,
    venue: &str,
    tier: Option<AccountTier>,
    field: &VenueFieldName,
    value: &str,
    secret: bool,
    journal: crate::ChangeStamp,
) -> Result<(Option<String>, Option<crate::ChangeAppendError>), VenueSettingRefusal> {
    use vike_change_journal::{Change, ChangeOutcome};
    let db = crate::store_locator::db_path_in(settings_dir);
    if !database_present(&db).map_err(|e| Box::new((e, None)))? {
        return Err(Box::new((
            StoreError { path: db, kind: crate::db::StoreErrorKind::NoDatabase },
            None,
        )));
    }
    let key = crate::venue_setting::venue_setting_key(venue, tier, field);
    let shown = |v: &str| if secret { REDACTED.to_string() } else { v.to_string() };
    let result = set_venue_setting_in_unjournalled(settings_dir, venue, tier, field, value);
    let change = match &result {
        Ok(previous) => Change::set_setting(
            ChangeOutcome::AppliedPendingRestart,
            journal.actor,
            "venue",
            &key,
            previous.as_deref().map(shown).as_deref(),
            &shown(value),
        ),
        Err(e) => Change::set_setting(
            ChangeOutcome::Refused,
            journal.actor,
            "venue",
            &key,
            None,
            &shown(value),
        )
        .with_reason(Some(&e.to_string())),
    };
    let journal_error =
        crate::store::append_beside(settings_dir, journal.process, journal.now_ms, &change);
    match result {
        Ok(previous) => Ok((previous, journal_error)),
        Err(e) => Err(Box::new((e, journal_error))),
    }
}
