//! `upsert_rows`: the database half of the credential UPSERT.

use super::*;

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// **The UPSERT, database half** — replace exactly the named rows, insert the ones that were absent,
/// leave every other row untouched.
///
/// `crate::store::save_credentials_to_store` is the door; nothing outside this crate calls this
/// directly, and nothing here decides WHICH store is written.
///
/// # What it refuses
///
/// * a name already sitting in the OTHER table ([`DbErrorKind::WrongTable`]) — the static predicate,
///   enforced on every write;
/// * a database that is not [`SCHEMA_VERSION`] — through [`open_for_write`]'s existing check;
/// * a database that VANISHED between the backend choice and this open
///   ([`DbErrorKind::VanishedDatabase`]) — see the invariant below.
///
/// # ⚠ ONLY [`create_store`] may create a database, and only when asked to
///
/// This function used to satisfy *a database that exists has a finished schema and a stamped
/// version* on the race path by CREATING and STAMPING. That is the wrong invariant, and the
/// difference is the whole of this section. A write reaches here holding one or two keys; the store
/// it was routed to holds sixty-odd. If the file is removed between `crate::store::backend_in`'s
/// probe and this open, minting a fresh database here leaves one that is schema-complete,
/// version-stamped, accepted by [`check_schema_version`] — and holding ONLY this write's keys. From
/// that moment [`crate::store::backend_at`] answers `Database` for every process on the box and
/// every other key is gone.
///
/// So the rule is structural rather than repaired after the fact: **[`create_store`] is the only
/// function in this module that may bring a database into existence**, and it does so only on the
/// operator's explicit `vike-cli secrets init`. Every other writer refuses.
///
/// **The rollback deletes only a file this very call created**, on the branch where
/// [`open_for_write`] reports `created` — a path that did not exist moments earlier and has never
/// held a row belonging to anyone. It is not a credential store and it is not the operator's data;
/// leaving it would be the defect.
///
/// ONE transaction for the whole batch, which is what makes a rotating pair (cTrader's access +
/// refresh grant, a node key pair) impossible to half-write.
///
/// # ⚠ The shape, and the three things it asks of this writer
///
/// 1. **`ON CONFLICT(name)` names no unique constraint.** `credential`'s `name` uniqueness is
///    the PARTIAL index `credential_one_live_name … WHERE superseded_at IS NULL` (§4.1 argues why it
///    must be partial: a superseded row carries the same name as the value that replaced it), and
///    SQLite requires an upsert's conflict target to repeat a partial index's `WHERE`. The
///    statement below is an explicit UPDATE of the live row instead, which says the same thing and
///    needs no conflict target at all.
/// 2. **`field` is `NOT NULL`, so a row that is not already there needs a CLASSIFICATION.** That is
///    what `classify` is, and it is `Option` because it is meaningless for [`Table::NodeKey`] —
///    0051's pair belongs to no account and its table is `(name, value)`. A credential name this
///    store has never held, with no classifier, is [`DbErrorKind::Unclassified`]: refused, never filed as infrastructure. Filing it that way
///    would detach a venue credential from its account in silence, which is the failure this
///    refusal exists to prevent.
/// 3. **Replacing a KNOWN key needs no classifier at all**, and that is what keeps the sharpest
///    writer in the workspace working unchanged: `crates/bridges/ctrader/src/token_store.rs`'s
///    `persist` re-writes a grant the VENUE rotated, under a name the store already holds.
///
/// ⚠ **`superseded_at` is NOT written here, and that is §12's explicit debt rather than an
/// oversight**: *"the sanctioned upsert replaces in place today; marking the old row superseded is
/// a behaviour change to that writer and to `credential_writer_gate.rs`'s vocabulary."* This
/// function replaces in place, so a rotation loses the old value.
pub(crate) fn upsert_rows(
    path: &Path,
    table: Table,
    updates: &[(String, String)],
    classify: Option<&dyn Fn(&str) -> crate::schema::Classification>,
) -> Result<(), DbError> {
    if updates.is_empty() {
        return Ok(());
    }
    let (mut conn, created) = open_for_write(path)?;

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
    // ⚠ **THE WRITE FUNNEL** ([`ensure_venue_rows`]): [`crate::schema::write_rows`] below mints an
    // `account` row whose `venue_id` is looked up in the `venue` roster in the same statement.
    ensure_venue_rows(&tx).map_err(|e| DbError::sql(path, e))?;
    match table {
        // 0051's pair: `(name, value)`, no account, no classification.
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
                let sql =
                    "UPDATE credential SET value = ?2 WHERE name = ?1 AND superseded_at IS NULL";
                let mut update = tx.prepare(sql).map_err(|e| DbError::sql(path, e))?;
                for (name, value) in updates {
                    let touched =
                        update.execute((name, value)).map_err(|e| DbError::sql(path, e))?;
                    if touched == 0 {
                        // A name this store has never held. It needs a home, which needs a
                        // classification.
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
                let report = crate::schema::write_rows(&tx, &fresh, classify)
                    .map_err(|e| DbError::sql(path, e))?;
                // ⚠ `write_rows` reports a refusal per KEY and carries on, and that is NOT fine
                // here: this call has one or two keys in it and its caller
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
    // and `create_store` is what wrote it. The `created` arm returned above.
    Ok(())
}
