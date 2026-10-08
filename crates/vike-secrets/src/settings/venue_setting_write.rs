//! The operator's `venue_setting` writer: `set_venue_setting_in` and its journalled twin.

use super::*;

/// **Upsert ONE `venue_setting` row**, returning the value it replaced — `None` where the row is
/// new. The operator's writer for the values `secrets move-venue-config` relocated.
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
/// (`crate::schema::stored_venue_setting_tier` is the translation). ⚠ `Some("any")` is NOT a Rust
/// tier and no production caller can produce one — `crate::venue_setting::parse_venue_setting_key`
/// classifies
/// by `crate::schema::account_tier_named`, which does not know the word — and it is REFUSED here,
/// before the store is opened. ⚠ This paragraph used to end *"Were one passed, it would land on the
/// machine-scoped row, which is what the stored word means, never on a second row"*, which was
/// what the code did and contradicted the `# Errors` section below: step 7 added `'any'` to the
/// `CHECK`, so the database no longer refused the word, and the write overwrote the machine-scoped
/// row and reported its old value with nothing erroring. Review found the two disagreeing; the
/// refusal is `stored_venue_setting_tier`'s, so the `# Errors` promise is true again.
///
/// # Errors
/// The engine, a store at an unreadable schema, or a `CHECK` the DDL enforces — a `tier` outside
/// `paper`/`demo`/`live` is refused HERE, by the database, which is the validation a `config` map
/// field could never have given. `Some("any")` — the one word the `CHECK` admits that is not a
/// tier — is refused by the write boundary instead, and nothing is opened or migrated for it.
pub fn set_venue_setting_in(
    settings_dir: &Path,
    venue: &str,
    tier: Option<&str>,
    field: &str,
    value: &str,
) -> Result<Option<String>, DbError> {
    let db = crate::dotenv::db_path_in(settings_dir);
    // ⚠ §5.2 step 7's WRITE boundary — the tier is bound as the STORED word, in the lookup and the
    // upsert below alike, and it is resolved BEFORE the store is opened so a refused `Some("any")`
    // opens nothing and migrates nothing. The funnel below carries the table onto
    // `tier TEXT NOT NULL`, so the `Option` itself would bind SQL NULL: refused outright by the
    // INSERT, and — the quiet half — matched by NOTHING in the lookup, which is well-formed either
    // way and would report `None` for a machine-scoped row that plainly holds a value. (This lookup
    // read `tier IS ?3` over the raw `Option` until step 7.)
    let stored_tier =
        crate::schema::stored_venue_setting_tier(tier).map_err(|e| DbError::sql(&db, e))?;
    let (mut conn, _created, _version) = crate::db::open_for_write(&db)?;
    let tx = conn.transaction().map_err(|e| DbError::sql(&db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(&db, e))?;
    // ⚠ This writer names `venue_id` in its own `venue_setting` INSERT below — see
    // `crate::db::ensure_venue_id_columns`'s own doc for why a store that predates the column needs
    // this call rather than the bare DDL batch above.
    crate::db::ensure_venue_id_columns(&tx).map_err(|e| DbError::sql(&db, e))?;

    // The row it replaces, read inside the same transaction so the report cannot describe a value
    // some other writer changed in between. Found by the venue's NUMBER: the funnel above has
    // carried the store, so every row holds one (`crate::schema::venue_is`).
    let previous: Option<String> = tx
        .query_row(
            &format!(
                "SELECT value FROM venue_setting WHERE {} AND field = ?2 AND tier = ?3",
                crate::schema::venue_is("venue_id", "?1")
            ),
            (venue, field, stored_tier),
            |r| r.get(0),
        )
        .ok();

    // ⚠ `venue_id = excluded.venue_id` on the conflict path too, not only `value` — DEFENSIVE, and a
    // DECLARED BLIND SPOT rather than a covered branch, in the same spirit
    // `crates/vike-secrets/tests/gates/store_link.rs` declares `venue_arming.label` as a link its own
    // rules cannot see rather than pretending otherwise.
    //
    // It makes this statement correct ON ITS OWN, rather than by depending on
    // `ensure_venue_id_columns` above (in this same function) having already filled every NULL —
    // i.e. on a call in ANOTHER function rather than on this one.
    //
    // ⚠ **It cannot fire today, and here is why**: `crate::schema::DDL`'s `UNIQUE (venue, tier,
    // field)` keys on `venue` FIRST, so `venue` sits INSIDE this table's conflict key. (⚠ This named
    // the two partial indexes `venue_setting_one_per_tier` and `venue_setting_one_per_machine` until
    // §5.2 step 7 collapsed them into that one total `UNIQUE`; both keyed on `venue` first as well,
    // so the argument survives the change unaltered.) A row can only ever conflict
    // with an incoming row naming the SAME `venue` text, and `venue_id` is a pure function of that
    // text (the sub-select above) — so a conflicting row's existing `venue_id`, if it was ever
    // correctly set, is BY CONSTRUCTION the same value `excluded.venue_id` would carry. There is no
    // sequence of calls to this function that can make an existing row's `venue_id` need to CHANGE
    // on a conflict. Since the venue-links flip `venue_id` sits inside a conflict key too — the
    // second total `UNIQUE (venue_id, tier, field)` — and a conflict on THAT key is a row holding
    // the same `venue_id` already, so the clause sets it to itself and the argument holds for both.
    // (The untargeted `ON CONFLICT` is what lets one `DO UPDATE` answer whichever of the two
    // fires.) ⚠ Since the plan's second release the shipped table has no text `venue` and no
    // text-keyed `UNIQUE`, so on a carried store the number's `UNIQUE` is the only conflict key and
    // that last argument is the whole of it; the text half above still describes a store no writer
    // of that release has carried yet.
    //
    // What WOULD make it fire: a writer that leaves a STALE, non-NULL `venue_id` on a row — nothing
    // in this crate does. `ensure_venue_id_columns` heals only `venue_id IS NULL`; it would NOT
    // correct a stale-but-non-NULL value, so if such a writer ever existed, this clause is what would
    // catch up behind it.
    //
    // ⚠ **This is therefore NOT regression-guarded by any test, deliberately, and a mutation of this
    // clause will NOT redden the suite** — MEASURED: `every_writer_names_its_venue_by_number_on_every_row`
    // (`venue_id_agrees_with_the_text_column_on_every_row` until the plan's second release)
    // exercises this exact `ON CONFLICT` branch (a second `set_venue_setting_in` call for the same
    // key) and stays green with this clause removed, for precisely the reason above. Do not delete
    // this line after finding no test fails for it; that absence is the point being documented, not
    // evidence the line is dead code.
    let link = crate::schema::VenueLink::of(&tx, "venue_setting", "t")
        .map_err(|e| DbError::sql(&db, e))?;
    tx.execute(
        &format!(
            "INSERT INTO venue_setting ({}, tier, field, value) VALUES ({}, ?2, ?3, ?4) \
             ON CONFLICT DO UPDATE SET value = excluded.value, venue_id = excluded.venue_id",
            link.columns,
            link.values("?1")
        ),
        (venue, stored_tier, field, value),
    )
    .map_err(|e| DbError::sql(&db, e))?;
    tx.commit().map_err(|e| DbError::sql(&db, e))?;
    Ok(previous)
}

/// What a SECRET venue field's value is written as in the change journal.
const REDACTED: &str = "<secret>";

/// **[`set_venue_setting_in_journalled`]'s `Err` — the write's own refusal, paired with whether
/// recording THAT refusal in the change journal also succeeded.** Boxed: clippy's
/// `result_large_err` correctly refuses a bare `(DbError, Option<JournalAppendError>)` inline in
/// every `Result` this function's callers propagate (`DbError` alone stays unboxed everywhere
/// else in this crate; it is the PAIR that crosses the threshold), and `type_complexity` asks for
/// the tuple to be named rather than spelled at the return type. A type alias over a raw tuple
/// rather than a named struct: the two cells have no name of their own beyond "the write's error"
/// and "the journal's", which a struct's field names would only restate.
pub type VenueSettingRefusal = Box<(DbError, Option<crate::JournalAppendError>)>;

/// **[`set_venue_setting_in`], plus its `set_setting` record in the change journal** — the
/// venue-setting twin of [`crate::save_credentials_to_store_journalled`], and the ONE journalled
/// `venue_setting` writer (`vike-cli config set venue.*` and the desktop both reach it).
///
/// The journal is the EXISTING change journal every settings write uses
/// (`vike_model::change_journal`, beside the store: `<settings_dir>/state/changes`), with `file` =
/// `venue` and the operator's dotted key. BOTH outcomes are recorded, as `vike-cli`'s
/// `settings_write` records them: a refused write carries its reason. ⚠ A `secret` field's value —
/// old and new — is recorded as `<secret>`, never verbatim.
///
/// A journal failure cannot fail the CALL — the underlying write's own outcome (applied or
/// refused) is always reported unchanged — but it must never be swallowed either, on EITHER
/// outcome: it comes back as `Some(JournalAppendError)`, paired with whichever half of the result
/// it belongs to, for the caller to report.
///
/// ⚠ **This is the shape `crates/vike-cli/src/cmd/settings_write.rs`'s
/// `set_setting_journalled_within` already uses for the identical problem, carried here as
/// RETURNED DATA rather than as an `eprintln!`** — this crate carries no logging dependency and
/// hands findings to its caller instead (see the crate doc's Redaction section, and
/// [`crate::JournalAppendError`]'s own doc). A prior version of this function paired the journal
/// outcome with the result via `result.map(|previous| (previous, journal_error))`, which is a
/// no-op on `Err` — so a REFUSED write whose OWN refusal record also failed to append reported
/// only the refusal, and the caller had no way to learn the ledger did not record it either.
/// Found in review before any downstream task depended on the signature; the `Err` arm now carries
/// the same pair the `Ok` arm always has, rather than silently dropping half of it.
///
/// ⚠ **It CREATES NO DATABASE**: a box with no settings database is refused with
/// `DbErrorKind::NoDatabase`, exactly as [`crate::edit_account_in`] refuses one — the database's
/// mere existence is the credential store's per-run backend choice, so creating one from a
/// venue-setting write would stop `secrets.env` being read (`docs/decisions/0036`, reason 1). That
/// refusal carries no journal outcome at all (`Err((e, None))`): nothing beside a nonexistent
/// database has a journal directory to open either, and no change was attempted worth recording.
///
/// # Errors
/// Every refusal [`set_venue_setting_in`] states, and `NoDatabase` — each paired with the outcome
/// of recording THAT refusal in the change journal (`None` for the `NoDatabase` case above).
pub fn set_venue_setting_in_journalled(
    settings_dir: &Path,
    venue: &str,
    tier: Option<&str>,
    field: &str,
    value: &str,
    secret: bool,
    journal: crate::AccountJournal,
) -> Result<(Option<String>, Option<crate::JournalAppendError>), VenueSettingRefusal> {
    use vike_model::change_journal::{Change, Outcome};
    let db = crate::dotenv::db_path_in(settings_dir);
    if !database_present(&db) {
        return Err(Box::new((
            DbError { path: db, kind: crate::db::DbErrorKind::NoDatabase },
            None,
        )));
    }
    let key = crate::venue_setting::venue_setting_key(venue, tier, field);
    let shown = |v: &str| if secret { REDACTED.to_string() } else { v.to_string() };
    let result = set_venue_setting_in(settings_dir, venue, tier, field, value);
    let change = match &result {
        Ok(previous) => Change::set_setting(
            Outcome::AppliedPendingRestart,
            journal.actor,
            "venue",
            &key,
            previous.as_deref().map(shown).as_deref(),
            &shown(value),
        ),
        Err(e) => {
            Change::set_setting(Outcome::Refused, journal.actor, "venue", &key, None, &shown(value))
                .with_reason(Some(&e.to_string()))
        }
    };
    let cj = crate::store::journal_beside(settings_dir, journal.proc);
    let journal_error = cj
        .append(journal.now_ms, &change)
        .err()
        .map(|source| crate::JournalAppendError { dir: cj.dir().to_path_buf(), source });
    match result {
        Ok(previous) => Ok((previous, journal_error)),
        Err(e) => Err(Box::new((e, journal_error))),
    }
}
