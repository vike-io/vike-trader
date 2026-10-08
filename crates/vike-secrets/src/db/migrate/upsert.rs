//! `upsert_rows`: the database half of the credential UPSERT.

use super::*;

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// **The UPSERT, database half** — replace exactly the named rows, insert the ones that were absent,
/// leave every other row untouched.
///
/// The same contract the retired file writer (`env_write.rs`'s `save_credentials`) held over the file, expressed in the
/// only two statements SQLite needs for it. `crate::store::save_credentials_to_store` is the door;
/// nothing outside this crate calls this directly, and nothing here decides WHICH store is written.
///
/// # What it refuses
///
/// * a name already sitting in the OTHER table ([`DbErrorKind::WrongTable`]) — the static predicate,
///   enforced on the write path as well as on the migration's;
/// * a database that is not [`SCHEMA_VERSION`] — through [`open_for_write`]'s existing check;
/// * a database that VANISHED between the backend choice and this open
///   ([`DbErrorKind::VanishedDatabase`]) — see the invariant below.
///
/// # ⚠ The invariant is *the database holds what the file holds*, and ONLY [`migrate`] may create one
///
/// This function used to state a weaker one — *a database that exists has a finished schema and a
/// stamped version* — and satisfied it on the race path by CREATING and STAMPING. That is the wrong
/// invariant, and the difference is the whole of this section. A write reaches here holding one or
/// two keys; the store it was routed to holds sixty-odd. If the file is removed between
/// `crate::store::backend_in`'s probe and this open, minting a fresh database here leaves one that
/// is schema-complete, version-stamped, accepted by [`check_schema_version`] — and holding ONLY this
/// write's keys. From that moment [`crate::store::backend_at`] answers `Database` for every process
/// on the box, the credential file beside it is never read again, and every other key is gone. The
/// old invariant is satisfied the whole way down; the load-bearing one is violated at the first
/// statement.
///
/// So the rule is now structural rather than repaired after the fact: **[`migrate`] is the only
/// function in this module that may bring a database into existence**, because it is the only one
/// that reads the whole of both files first and can therefore satisfy *the database holds what the
/// file holds*. Every other writer refuses.
///
/// **The rollback deletes only a file this very call created**, on the branch where
/// [`open_for_write`] reports `created` — a path that did not exist moments earlier and has never
/// held a row belonging to anyone. It is not a credential store and it is not the operator's data;
/// leaving it would be the defect. Nothing here touches `secrets.env` or `node.env`, then or ever.
///
/// ONE transaction for the whole batch, which is what makes a rotating pair (cTrader's access +
/// refresh grant, a node key pair) impossible to half-write.
/// # ⚠ The schema-2 shape, and the three things that changed under it
///
/// 1. **`ON CONFLICT(name)` no longer names a unique constraint.** Schema 2's `name` uniqueness is
///    the PARTIAL index `credential_one_live_name … WHERE superseded_at IS NULL` (§4.1 argues why it
///    must be partial: a superseded row carries the same name as the value that replaced it), and
///    SQLite requires an upsert's conflict target to repeat a partial index's `WHERE`. The
///    statement below is an explicit UPDATE of the live row instead, which says the same thing and
///    needs no conflict target at all.
/// 2. **`field` is `NOT NULL`, so a row that is not already there needs a CLASSIFICATION.** That is
///    what `classify` is, and it is `Option` because it is meaningless for [`Table::NodeKey`] —
///    0051's pair belongs to no account and its table is `(name, value)` in every schema. A
///    credential name this store has never held, with no classifier, is
///    [`DbErrorKind::Unclassified`]: refused, never filed as infrastructure. Filing it that way
///    would detach a venue credential from its account in silence, which is the failure this
///    refusal exists to prevent.
/// 3. **Replacing a KNOWN key needs no classifier at all**, and that is what keeps the sharpest
///    writer in the workspace working unchanged: `crates/bridges/ctrader/src/token_store.rs`'s
///    `persist` re-writes a grant the VENUE rotated, under a name the store already holds.
///
/// ⚠ **`superseded_at` is NOT written here, and that is §12's explicit debt rather than an
/// oversight**: *"the sanctioned upsert replaces in place today; marking the old row superseded is
/// a behaviour change to that writer and to `credential_writer_gate.rs`'s vocabulary."* This
/// function still replaces in place, so a rotation loses the old value exactly as it did before
/// schema 2 — no better, no worse.
pub fn upsert_rows(
    path: &Path,
    table: Table,
    updates: &[(String, String)],
    classify: Option<&dyn Fn(&str) -> crate::schema::Classification>,
) -> Result<(), DbError> {
    if updates.is_empty() {
        return Ok(());
    }
    let (mut conn, created, version) = open_for_write(path)?;

    // ⚠ `created` is FALSE on every ordinary call — `crate::store::backend_in` has already seen the
    // file, which is why this function's doc says it creates no database. It can be true only if the
    // file was REMOVED between that probe and this open, and on that path the only safe act is to
    // put back what we found: no database. A write that fails loudly costs the caller a retry; the
    // alternative silently retires every key this write was not about. See the invariant above.
    if created {
        // Close the engine BEFORE unlinking — an open handle keeps the file alive on Windows, and a
        // rollback that quietly did not roll back is exactly the shape this branch exists to refuse.
        drop(conn);
        // Best-effort by necessity, and never a silent success: the error below is returned
        // whatever the unlink does, so a file we somehow could not remove is still reported as a
        // refused write rather than as a store that answered.
        let _ = std::fs::remove_file(path);
        return Err(DbError { path: path.to_path_buf(), kind: DbErrorKind::VanishedDatabase });
    }

    // The two-namespace check, BEFORE anything is written, and over the whole batch — so a refusal
    // names the offending key and leaves the store exactly as it found it.
    {
        let other = table.other();
        let sql = format!("SELECT 1 FROM {} WHERE name = ?1", other.sql_name());
        let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
        for (name, _) in updates {
            let clash = stmt.exists([name.as_str()]).map_err(|e| DbError::sql(path, e))?;
            if clash {
                return Err(DbError {
                    path: path.to_path_buf(),
                    kind: DbErrorKind::WrongTable {
                        key: name.clone(),
                        found_in: other,
                        wanted: table,
                    },
                });
            }
        }
    }

    let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
    // ⚠ **THE REPAIR FUNNEL, and this is the SIXTH writer to reach it — it was the one that did
    // not, and the one an OPERATOR meets.** [`ensure_venue_id_columns`]'s own doc names the other
    // five and what each would answer without this call; this function opened its transaction and
    // went straight to the INSERT until 2026-09-23. `vike-cli secrets set` arrives here, and
    // [`crate::schema::write_rows`] below mints an `account` row at the tier the production
    // classifier produces — so on a store that predates §4.4's rename the shipped word `paper`
    // meets the store's own `CHECK (tier IN ('sim', …))` and the operator sees a bare `CHECK
    // constraint failed` naming nothing, on a box that worked the day before. A store predating
    // `venue_id` dies one statement earlier still, `table account has no column named venue_id`.
    // `crates/vike-secrets/tests/migration/paper_tier.rs`'s
    // `a_credential_write_carries_a_pre_rename_store_onto_paper` is the regression, driven through
    // this door rather than through a writer that already reached the funnel.
    //
    // ⚠ **GUARDED on the version where the other five are not, and the guard is this function's
    // OWN schema-1 accommodation rather than a second spelling of the repair.**
    // [`ensure_venue_id_columns`] runs [`crate::schema::DDL`] (through [`ensure_venue_rows`]),
    // whose `credential_one_live_value ON credential (account_id, field)` index cannot PREPARE
    // against a genuine schema-1 `credential` — `(name TEXT PRIMARY KEY, value TEXT)` has neither
    // column, and `crates/vike-secrets/tests/migration/database/venue_id.rs`'s
    // `write_settings_still_refuses_a_genuine_schema_1_store_for_an_unrelated_pre_existing_reason`
    // pins exactly that crash for a writer that runs the batch unguarded. Calling it here
    // unconditionally would therefore turn a schema-1 REPLACEMENT into a hard failure — the case
    // `db_tests`' `a_new_key_written_to_a_schema_1_store_is_refused_and_names_the_way_out` asserts
    // still works, and the case this function's own `version == 1` branch below exists for, whose
    // sharpest instance is cTrader's venue-side grant rotation and whose loss is a lockout at the
    // next restart. Nothing is skipped by the guard: a store below [`SCHEMA_VERSION`] cannot reach
    // `write_rows` at all, because every absent name returns [`DbErrorKind::WriteToOlderSchema`]
    // first — the same predicate, so there is one meaning of *this store is the current shape*.
    if version == SCHEMA_VERSION {
        ensure_venue_id_columns(&tx).map_err(|e| DbError::sql(path, e))?;
    }
    match table {
        // 0051's pair, in every schema: `(name, value)`, no account, no classification.
        Table::NodeKey => {
            let sql = "INSERT INTO node_key (name, value) VALUES (?1, ?2) \
                       ON CONFLICT(name) DO UPDATE SET value = excluded.value";
            let mut stmt = tx.prepare(sql).map_err(|e| DbError::sql(path, e))?;
            for (name, value) in updates {
                stmt.execute((name, value)).map_err(|e| DbError::sql(path, e))?;
            }
        }
        Table::Credential => {
            let mut fresh: BTreeMap<String, String> = BTreeMap::new();
            {
                // ⚠ The `WHERE` is chosen by the SCHEMA and not spelled once: naming
                // `superseded_at` against a schema-1 table is a PREPARE-time error (*no such
                // column*), which would have turned every credential write on an unmigrated box
                // into a hard failure — including the venue's own cTrader grant rotation, whose
                // loss is a lockout at the next restart. A schema-1 table has one row per name by
                // its PRIMARY KEY, so the clause is unnecessary there as well as illegal.
                let sql = if version == 1 {
                    "UPDATE credential SET value = ?2 WHERE name = ?1"
                } else {
                    "UPDATE credential SET value = ?2 WHERE name = ?1 AND superseded_at IS NULL"
                };
                let mut update = tx.prepare(sql).map_err(|e| DbError::sql(path, e))?;
                for (name, value) in updates {
                    let touched =
                        update.execute((name, value)).map_err(|e| DbError::sql(path, e))?;
                    if touched == 0 {
                        // A name this store has never held. It needs a home, and the two ways of
                        // not having one are different refusals with different fixes.
                        if version != SCHEMA_VERSION {
                            return Err(DbError {
                                path: path.to_path_buf(),
                                kind: DbErrorKind::WriteToOlderSchema {
                                    key: name.clone(),
                                    found: version,
                                },
                            });
                        }
                        if classify.is_none() {
                            return Err(DbError {
                                path: path.to_path_buf(),
                                kind: DbErrorKind::Unclassified { key: name.clone() },
                            });
                        }
                        fresh.insert(name.clone(), value.clone());
                    }
                }
            }
            if !fresh.is_empty() {
                let classify = classify.expect("checked above, once per absent name");
                let report = crate::schema::write_rows(
                    &tx,
                    &fresh,
                    &crate::schema::FileComments::default(),
                    classify,
                )
                .map_err(|e| DbError::sql(path, e))?;
                // ⚠ A per-key refusal is fine for a MIGRATION (it carries the rest and names the
                // key) and is NOT fine here: this call has one or two keys in it and its caller
                // believes a successful return means the key landed. A rotating cTrader grant that
                // silently did not land is a lockout at the next restart.
                //
                // ⚠ **The refusal is CARRIED, not discarded.** This returned
                // `DbErrorKind::Unclassified` for every refusal the fill could raise, i.e. it told
                // the operator *the caller supplied no account classification* while holding, in
                // `report.refused`, the real reason — a tier the `CHECK` will not take, an account
                // with two answers, or two spellings of one credential disagreeing about its
                // value. Every one of those names a key and a repair; `Unclassified` names
                // neither and points at the wrong party.
                if let Some(first) = report.refused.first() {
                    return Err(DbError {
                        path: path.to_path_buf(),
                        kind: DbErrorKind::Refused { refusal: first.clone() },
                    });
                }
            }
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    // No stamp here, and no branch that could need one: the only call that reaches this line opened
    // a database that already existed, so `check_schema_version` has already accepted its version
    // and `migrate` is what wrote it. The `created` arm returned above.
    Ok(())
}
