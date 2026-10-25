//! The credential UPSERT, database half: [`upsert_rows`] (a NAME-addressed write — a deployment
//! row or a hub key) and [`upsert_owned`] (an ACCOUNT's or a VENUE's credential, addressed by its
//! owner and FIELD), and [`UpsertIntent`], the one word a caller says about a row the table does not
//! hold yet.

use super::*;

// ---------------------------------------------------------------------------------------------
// The intent
// ---------------------------------------------------------------------------------------------

/// **What a credential write may do with a row the table does not hold live** — the one decision
/// a caller of `crate::store::save_credentials_to_store` / `crate::store::save_owned_credentials`
/// makes, said as a word rather than as a closure
/// (`docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md`).
///
/// A row the table ALREADY holds live is replaced in place under either word, and keeps its owner
/// and its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertIntent {
    /// **Only rows the table already holds live.** Any other is REFUSED, naming it
    /// ([`DbErrorKind::NotHeld`]), and the whole batch writes nothing. The shape for a writer that
    /// must never invent a row: a rotation of a key that is already there.
    Replace,
    /// **A new row may be created.** Under a NAME-addressed write ([`upsert_rows`]) it is a
    /// DEPLOYMENT row — no account, no venue, its `field` the whole name — or a hub key. Under an
    /// OWNER-addressed write ([`upsert_owned`]) it is the owner's row at that field. ⚠ Neither ever
    /// creates an ACCOUNT or chooses a tier (decision 0156 item 6): an account is the operator's to
    /// create, with its tier, before a key is attached to it.
    Add,
}

/// **Whose credential an owner-addressed write targets** — an EXISTING account row, or a venue's
/// application rows (cTrader's OAuth client, shared by every account of the venue).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialOwner {
    /// `account.id` — the identity. A row that does not exist is REFUSED
    /// ([`DbErrorKind::NoSuchAccount`]): a credential write never creates an account.
    Account(i64),
    /// A `vike_model::VENUES` id the store's `venue` roster carries
    /// ([`DbErrorKind::UnknownVenue`] otherwise).
    Venue(String),
}

/// What [`upsert_owned`] wrote — the facts its journal record needs, NAMES only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedWrite {
    /// The venue the rows belong to (the account's, or the venue written).
    pub venue: String,
    /// The account's tier; `None` for a venue's application rows.
    pub tier: Option<AccountTier>,
    /// The NAME of every row written, in the order the fields were given — an existing row keeps the
    /// name it was stored under, a new one takes the writer's ([`account_credential_name`],
    /// [`venue_credential_name`]).
    pub names: Vec<String>,
}

/// **The name a NEW account credential row is stored under**: `{venue}#{account id}.{FIELD}` —
/// `binance#7.API_KEY`.
///
/// The `credential.name` column is a LABEL since decision 0156 item 6, and no reader looks a row up
/// by it: an account's credential is its `(account_id, field)`. What the scheme must give is what
/// `credential_one_live_name` demands — unique per live row — and it does, because the account id
/// is the account's identity and `credential_one_live_value` admits one live row per
/// `(account_id, field)`. It carries NO tier and no label, so moving an account's tier or label
/// leaves every name true. The `#` and `.` are characters no env-shaped legacy name holds, so a new
/// name can never collide with an older row's.
#[must_use]
pub fn account_credential_name(venue: &str, account_id: i64, field: &str) -> String {
    format!("{venue}#{account_id}.{field}")
}

/// **The name a NEW venue application credential row is stored under**: `{venue}.{FIELD}` —
/// `ctrader.CLIENT_ID`. One live row per `(venue, field)`: the writer replaces the live one in place.
#[must_use]
pub fn venue_credential_name(venue: &str, field: &str) -> String {
    format!("{venue}.{field}")
}

/// One field of an owner-addressed write, as [`upsert_owned`] planned it before writing: the live row
/// already held at that field (`(id, name)`), if any, and the pair to write.
struct PlannedField<'a> {
    held: Option<(i64, String)>,
    field: &'a String,
    value: &'a String,
}

/// Whether `field` is a `credential.field` word: `[A-Z0-9_]`, non-empty, opening and closing with a
/// letter or digit — `API_KEY`, `LOGIN`.
fn field_is_well_formed(field: &str) -> bool {
    let alnum = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
    !field.is_empty()
        && field.chars().all(|c| alnum(c) || c == '_')
        && field.starts_with(alnum)
        && field.ends_with(alnum)
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// Open `path` for a write, refusing (and rolling back) a database this very call created — the
/// invariant [`upsert_rows`]' doc carries.
fn open_existing_for_write(path: &Path) -> Result<Connection, DbError> {
    let (conn, created) = open_for_write(path)?;
    // ⚠ `created` is FALSE on every ordinary call — `crate::store::backend_in` has already seen the
    // file. It can be true only if the file was REMOVED between that probe and this open, and on
    // that path the only safe act is to put back what we found: no database. A write that fails
    // loudly costs the caller a retry; the alternative silently retires every key this write was
    // not about.
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
    Ok(conn)
}

/// **The UPSERT, database half, by NAME** — replace exactly the named rows, insert the ones that
/// were absent when the intent allows it, leave every other row untouched.
///
/// `crate::store::save_credentials_to_store` is the door; nothing outside this crate calls this
/// directly, and nothing here decides WHICH store is written.
///
/// # What a new NAME becomes
///
/// A `hub_key` name is inserted (decision 0051: the pair belongs to no account). A `credential`
/// name is inserted as a DEPLOYMENT row — `account_id` and `venue_id` NULL, its `field` the whole
/// name — and NOTHING else: an account's credential is addressed by its account and field
/// ([`upsert_owned`]), never derived from a name, so no name-addressed write can create an account
/// or choose a tier (decision 0156 item 6).
///
/// # What it refuses
///
/// * a name already sitting in the OTHER table ([`DbErrorKind::WrongTable`]) — the static predicate,
///   enforced on every write;
/// * under [`UpsertIntent::Replace`], a name the table does not hold live
///   ([`DbErrorKind::NotHeld`]) — in EITHER table, before anything is written;
/// * a database that is not [`SCHEMA_VERSION`] — through [`open_for_write`]'s existing check;
/// * a database that VANISHED between the backend choice and this open
///   ([`DbErrorKind::VanishedDatabase`]) — see the invariant below.
///
/// # ⚠ ONLY [`create_store`] may create a database, and only when asked to
///
/// A write reaches here holding one or two keys; the store it was routed to holds sixty-odd. If the
/// file is removed between `crate::store::backend_in`'s probe and this open, minting a fresh
/// database here would leave one that is schema-complete, version-stamped, accepted by
/// [`check_schema_version`] — and holding ONLY this write's keys. So **[`create_store`] is the only
/// function in this module that may bring a database into existence**, and it does so only on the
/// operator's explicit `vike-cli secrets init`. Every other writer refuses, and the rollback deletes
/// only a file this very call created.
///
/// ONE transaction for the whole batch, which is what makes a hub key pair impossible to
/// half-write.
///
/// ⚠ **`superseded_at` is NOT written here**: this replaces in place, so a rotation loses the old
/// value. The one writer of the column is the DELETE ([`retire_row`]), which marks a row retired.
pub(crate) fn upsert_rows(
    path: &Path,
    table: Table,
    updates: &[(String, String)],
    intent: UpsertIntent,
) -> Result<(), DbError> {
    if updates.is_empty() {
        return Ok(());
    }
    let mut conn = open_existing_for_write(path)?;

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
    // ⚠ **THE WRITE FUNNEL** ([`ensure_venue_rows`]): the `DDL` batch and the `venue` roster top-up
    // every writer runs first.
    ensure_venue_rows(&tx).map_err(|e| DbError::sql(path, e))?;
    match table {
        // 0051's pair: `(name, value)`, no account.
        Table::HubKey => {
            // A REPLACE may create no row, in this table either: every name must already be here,
            // asked over the whole batch before the first statement so a refusal writes nothing.
            if intent == UpsertIntent::Replace {
                let mut held = tx
                    .prepare("SELECT 1 FROM hub_key WHERE name = ?1")
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
            let sql = "INSERT INTO hub_key (name, value) VALUES (?1, ?2) \
                       ON CONFLICT(name) DO UPDATE SET value = excluded.value";
            let mut stmt = tx.prepare(sql).map_err(|e| DbError::sql(path, e))?;
            for (name, value) in updates {
                stmt.execute((name, value)).map_err(|e| DbError::sql(path, e))?;
            }
        }
        Table::Credential => {
            let link = crate::schema::VenueLink::of(&tx, "credential", "c")
                .map_err(|e| DbError::sql(path, e))?;
            let update =
                "UPDATE credential SET value = ?2 WHERE name = ?1 AND superseded_at IS NULL";
            // A DEPLOYMENT row: no account, no venue (the link's venue values are bound NULL), its
            // field the whole name. `secret = 1`: a value wrongly marked non-secret is the worse
            // error, and nothing here can know better.
            let insert = format!(
                "INSERT INTO credential (account_id, {}, field, value, name, secret, notes) \
                 VALUES (NULL, {}, ?1, ?2, ?1, 1, NULL)",
                link.columns,
                link.values("?3")
            );
            for (name, value) in updates {
                let touched =
                    tx.execute(update, (name, value)).map_err(|e| DbError::sql(path, e))?;
                if touched > 0 {
                    continue;
                }
                if intent == UpsertIntent::Replace {
                    return Err(DbError {
                        path: path.to_path_buf(),
                        kind: DbErrorKind::NotHeld { key: name.clone(), table },
                    });
                }
                tx.execute(&insert, (name, value, None::<String>))
                    .map_err(|e| DbError::sql(path, e))?;
            }
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

/// **The UPSERT, database half, by OWNER and FIELD** — an account's credential
/// (`credential.account_id` + `field`) or a venue's application credential (`venue_id` + `field`,
/// no account).
///
/// The owner must EXIST: an account id no row carries is [`DbErrorKind::NoSuchAccount`] and a venue
/// the roster does not carry [`DbErrorKind::UnknownVenue`] — a credential write never creates an
/// account and never chooses a tier (decision 0156 item 6). A field outside the field grammar is
/// [`DbErrorKind::FieldMalformed`]. Every refusal is asked before anything is written, and the batch
/// is ONE transaction, so a rotating pair (cTrader's access + refresh grant) cannot half-land.
///
/// A row the owner already holds LIVE at a field is replaced in place and keeps its name (a legacy
/// name stays a legacy name: it is a label, read by nothing). A new one is inserted under
/// [`account_credential_name`] / [`venue_credential_name`], unless the intent is
/// [`UpsertIntent::Replace`], which refuses it ([`DbErrorKind::NotHeld`], naming the field).
///
/// # Errors
/// The refusals above, the database's own ([`upsert_rows`]' list), or the engine's.
pub(crate) fn upsert_owned(
    path: &Path,
    owner: &CredentialOwner,
    fields: &[(String, String)],
    intent: UpsertIntent,
) -> Result<OwnedWrite, DbError> {
    let refuse = |kind| DbError { path: path.to_path_buf(), kind };
    if let Some((field, _)) = fields.iter().find(|(f, _)| !field_is_well_formed(f)) {
        return Err(refuse(DbErrorKind::FieldMalformed { field: field.clone() }));
    }
    let mut conn = open_existing_for_write(path)?;
    let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
    ensure_venue_rows(&tx).map_err(|e| DbError::sql(path, e))?;
    let link =
        crate::schema::VenueLink::of(&tx, "credential", "c").map_err(|e| DbError::sql(path, e))?;

    // The owner, asked FIRST: who it is decides every statement below.
    let (venue, tier) = match owner {
        CredentialOwner::Account(id) => {
            let account_link = crate::schema::VenueLink::of(&tx, "account", "a")
                .map_err(|e| DbError::sql(path, e))?;
            let row = tx
                .query_row(
                    &format!(
                        "SELECT {}, a.tier FROM account a {} WHERE a.id = ?1",
                        account_link.name, account_link.join
                    ),
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            crate::schema::tier_from_column(1, &r.get::<_, String>(1)?)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| DbError::sql(path, e))?;
            let Some((venue, tier)) = row else {
                return Err(refuse(DbErrorKind::NoSuchAccount { id: *id }));
            };
            (venue, Some(tier))
        }
        CredentialOwner::Venue(venue) => {
            let known: bool = tx
                .query_row("SELECT EXISTS(SELECT 1 FROM venue WHERE name = ?1)", [venue], |r| {
                    r.get(0)
                })
                .map_err(|e| DbError::sql(path, e))?;
            if !known {
                return Err(refuse(DbErrorKind::UnknownVenue { venue: venue.clone() }));
            }
            (venue.clone(), None)
        }
    };

    // The live row at each field, asked over the whole batch BEFORE the first write, so a REPLACE
    // that would create a row refuses with the store untouched.
    let (held_sql, owner_param): (String, rusqlite::types::Value) = match owner {
        CredentialOwner::Account(id) => (
            "SELECT id, name FROM credential \
             WHERE account_id = ?1 AND field = ?2 AND superseded_at IS NULL"
                .to_string(),
            rusqlite::types::Value::Integer(*id),
        ),
        CredentialOwner::Venue(v) => (
            format!(
                "SELECT id, name FROM credential WHERE account_id IS NULL AND {} \
                 AND field = ?2 AND superseded_at IS NULL ORDER BY id",
                crate::schema::venue_is("venue_id", "?1")
            ),
            rusqlite::types::Value::Text(v.clone()),
        ),
    };
    let mut planned: Vec<PlannedField<'_>> = Vec::new();
    {
        let mut held = tx.prepare(&held_sql).map_err(|e| DbError::sql(path, e))?;
        for (field, value) in fields {
            let mut rows = held
                .query((owner_param.clone(), field.as_str()))
                .map_err(|e| DbError::sql(path, e))?;
            // The LAST live row answers, as the reader's `ORDER BY id` fold does.
            let mut found = None;
            while let Some(r) = rows.next().map_err(|e| DbError::sql(path, e))? {
                found = Some((
                    r.get::<_, i64>(0).map_err(|e| DbError::sql(path, e))?,
                    r.get::<_, String>(1).map_err(|e| DbError::sql(path, e))?,
                ));
            }
            if found.is_none() && intent == UpsertIntent::Replace {
                let key = match owner {
                    CredentialOwner::Account(id) => account_credential_name(&venue, *id, field),
                    CredentialOwner::Venue(v) => venue_credential_name(v, field),
                };
                return Err(refuse(DbErrorKind::NotHeld { key, table: Table::Credential }));
            }
            planned.push(PlannedField { held: found, field, value });
        }
    }

    let insert = format!(
        "INSERT INTO credential (account_id, {}, field, value, name, secret, notes) \
         VALUES (?1, {}, ?3, ?4, ?5, ?6, NULL)",
        link.columns,
        link.values("?2")
    );
    let mut names = Vec::with_capacity(planned.len());
    for PlannedField { held, field, value } in planned {
        match held {
            Some((row, name)) => {
                tx.execute("UPDATE credential SET value = ?2 WHERE id = ?1", (row, value))
                    .map_err(|e| DbError::sql(path, e))?;
                names.push(name);
            }
            None => {
                let secret = vike_venues::credential_schema::schema_for(&venue)
                    .and_then(|s| s.field(field))
                    .is_none_or(|f| f.secret);
                let (account, venue_param, name) = match owner {
                    CredentialOwner::Account(id) => {
                        (Some(*id), None, account_credential_name(&venue, *id, field))
                    }
                    CredentialOwner::Venue(v) => {
                        (None, Some(v.clone()), venue_credential_name(v, field))
                    }
                };
                tx.execute(&insert, (account, venue_param, field, value, &name, i64::from(secret)))
                    .map_err(|e| DbError::sql(path, e))?;
                names.push(name);
            }
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(OwnedWrite { venue, tier, names })
}
