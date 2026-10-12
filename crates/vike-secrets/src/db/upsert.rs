//! `upsert_rows`: the database half of the credential UPSERT, and [`UpsertIntent`], the one word a
//! caller says about a NAME the table does not hold yet.

use super::*;

// ---------------------------------------------------------------------------------------------
// The intent
// ---------------------------------------------------------------------------------------------

/// **What a credential write may do with a NAME the table does not hold live** — the one decision
/// a caller of `crate::store::save_credentials_to_store` makes, said as a word rather than as a
/// closure (`docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md`).
///
/// A name the table ALREADY holds live is replaced in place under either word, and its row keeps
/// the account placement it already has: neither word re-files it.
///
/// It replaced a `classify: Option<&dyn Fn(&str) -> Classification>` parameter, which was plumbing
/// from the days the classifier lived above this crate (decision 0135 moved it down) and which
/// carried the intent only by accident: `None` meant [`UpsertIntent::Replace`],
/// `Some(&classify_credential_name)` meant [`UpsertIntent::Add`], and every caller in the tree
/// passed one of exactly those two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertIntent {
    /// **Only names the table already holds live.** A name it does not hold is REFUSED, naming it
    /// ([`DbErrorKind::NotHeld`]), and the whole batch writes nothing. The shape for a writer that
    /// must never invent a row: a rotation of a key that is already there.
    Replace,
    /// **A new name may be created.** A new `credential` row is filed under the account, venue
    /// and field `crate::credentials::classify_credential_name` derives from its NAME — the one
    /// classifier, the one every writer that adds a credential passes. A `node_key` row has no
    /// account and no field (decision 0051), so a new node-key name is simply inserted.
    Add,
}

impl UpsertIntent {
    /// The rule [`upsert_rows`] applies to a name the table does not hold live.
    pub(crate) fn new_names(self) -> NewNames<'static> {
        match self {
            UpsertIntent::Replace => NewNames::Refused,
            UpsertIntent::Add => NewNames::FiledBy(&crate::credentials::classify_credential_name),
        }
    }
}

/// [`UpsertIntent`] one level down: what [`upsert_rows`] does with a name the table does not hold
/// live. Crate-private, because the classifier is not a caller's choice in production — the only
/// way to reach [`NewNames::FiledBy`] with anything but the real classifier is the test-support
/// door `crate::store::save_credentials_to_store_filed_by`, which exists so this crate's own suites
/// can drive the schema-2 fill with SYNTHETIC classifications (the `Rule` tables of
/// `crates/vike-secrets/tests/support/classify.rs`) that no real name produces.
#[derive(Clone, Copy)]
pub(crate) enum NewNames<'a> {
    /// [`UpsertIntent::Replace`]: a name the table does not hold is refused.
    Refused,
    /// A name the table does not hold is inserted, a `credential` row filed under this
    /// classification.
    FiledBy(&'a dyn Fn(&str) -> crate::schema::Classification),
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// **The UPSERT, database half** — replace exactly the named rows, insert the ones that were absent
/// when the intent allows it, leave every other row untouched.
///
/// `crate::store::save_credentials_to_store` is the door; nothing outside this crate calls this
/// directly, and nothing here decides WHICH store is written.
///
/// # What it refuses
///
/// * a name already sitting in the OTHER table ([`DbErrorKind::WrongTable`]) — the static predicate,
///   enforced on every write;
/// * under [`NewNames::Refused`] (a caller's [`UpsertIntent::Replace`]), a name the table does not
///   hold live ([`DbErrorKind::NotHeld`]) — in EITHER table, before anything is written;
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
///    what [`NewNames::FiledBy`] carries, and it is meaningless for [`Table::NodeKey`] — 0051's
///    pair belongs to no account and its table is `(name, value)`. A credential name this store
///    does not hold, under [`NewNames::Refused`], is [`DbErrorKind::NotHeld`]: refused, never filed
///    as infrastructure. Filing it that way would detach a venue credential from its account in
///    silence, which is the failure this refusal exists to prevent.
/// 3. **Replacing a KNOWN key consults no classifier at all**, under either intent: the row
///    already carries its account, venue and field, and the replace leaves all three exactly where
///    they are.
///
/// ⚠ **`superseded_at` is NOT written here, and that is §12's explicit debt rather than an
/// oversight**: *"the sanctioned upsert replaces in place today; marking the old row superseded is
/// a behaviour change to that writer and to `credential_writer_gate.rs`'s vocabulary."* This
/// function replaces in place, so a rotation loses the old value. The one writer of the column
/// besides the schema-2 fill's alias demotion is the DELETE ([`retire_row`]), and it marks a row
/// retired rather than superseded by a newer value.
pub(crate) fn upsert_rows(
    path: &Path,
    table: Table,
    updates: &[(String, String)],
    new_names: NewNames<'_>,
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
            // A REPLACE may create no row, in this table either: every name must already be here,
            // asked over the whole batch before the first statement so a refusal writes nothing.
            if matches!(new_names, NewNames::Refused) {
                let mut held = tx
                    .prepare("SELECT 1 FROM node_key WHERE name = ?1")
                    .map_err(|e| DbError::sql(path, e))?;
                for (name, _) in updates {
                    if !held.exists([name.as_str()]).map_err(|e| DbError::sql(path, e))? {
                        return Err(DbError {
                            path: path.to_path_buf(),
                            kind: DbErrorKind::NotHeld { key: name.clone(), table },
                        });
                    }
                }
            }
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
                        // A name this store does not hold live. It needs a home, which needs a
                        // classification — and an intent that allows a new row at all.
                        if matches!(new_names, NewNames::Refused) {
                            return Err(DbError {
                                path: path.to_path_buf(),
                                kind: DbErrorKind::NotHeld { key: name.clone(), table },
                            });
                        }
                        fresh.insert(name.clone(), value.clone());
                    }
                }
            }
            if let NewNames::FiledBy(classify) = new_names
                && !fresh.is_empty()
            {
                let report = crate::schema::write_rows(&tx, &fresh, classify)
                    .map_err(|e| DbError::sql(path, e))?;
                // ⚠ `write_rows` reports a refusal per KEY and carries on, and that is NOT fine
                // here: this call has one or two keys in it and its caller
                // believes a successful return means the key landed. A rotating cTrader grant that
                // silently did not land is a lockout at the next restart.
                //
                // ⚠ **The refusal is CARRIED, not discarded.** This once returned a generic
                // *no classification supplied* refusal for every refusal the fill could raise,
                // while holding, in `report.refused`, the real reason — a tier the `CHECK` will not
                // take, an account with two answers, or two spellings of one credential disagreeing
                // about its value. Every one of those names a key and a repair; the generic one
                // named neither and pointed at the wrong party.
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
