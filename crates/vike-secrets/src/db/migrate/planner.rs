//! `Plan` and `plan` (module `planner`): everything a migration decides before anything it writes.

use super::*;
use crate::db::read::read_table_on;

/// **Everything a migration DECIDES, before anything it WRITES.**
///
/// Crate-private, and deliberately not a public type: it is the shared body of [`migrate`] and
/// [`preview`], not a second answer either of them could drift from. See [`plan`].
pub(super) struct Plan {
    /// Where the database is, or would be.
    pub(super) db: PathBuf,
    /// Did one exist when the classification was made? The ONLY fact here a write can invalidate —
    /// see [`preview`]'s *what a dry run cannot predict*.
    pub(super) exists: bool,
    /// The schema an existing database carries. `None` when there is none.
    pub(super) version: Option<i64>,
    /// §4.2's and §4.3's evidence, which lives in the credential file's COMMENT lines and which
    /// `crate::db::parse_credential_file` discards. See [`crate::schema::FileComments`] for why a second
    /// read of that file does not make this a second opinion about what a line means.
    pub(super) comments: crate::schema::FileComments,
    /// The rows a write would INSERT, keyed `(table, name)`. Empty means the write arm opens no
    /// connection at all, which is what makes *twice is the same as once* structural.
    pub(super) pending: BTreeMap<(Table, String), String>,
    /// One row per (file, table) pair that could have contributed. Assembled HERE rather than after
    /// the write, because it never depended on the write: it is a function of what was read, what is
    /// pending and what was already stored.
    pub(super) sources: Vec<SourceReport>,
    /// See [`Migration::doubly_claimed`].
    pub(super) doubly_claimed: Vec<String>,
    /// See [`Migration::refused`] — the per-KEY refusals, which leave the run `Ok`.
    pub(super) refused: Vec<Ambiguity>,
    /// **Every `(table, name) -> value` the database already holds**, as the read-only probe found
    /// it. Empty when there is none.
    ///
    /// The comparison in step 3 is what it is FOR; [`preview_rows`] is what it is KEPT for. A dry
    /// run has to be able to build the store the apply will be classifying into, and the only way
    /// to do that without opening the real database for writing is to rebuild it somewhere else
    /// out of what was read.
    pub(super) stored: BTreeMap<(Table, String), String>,
}

/// **The classification, the comparison and the report — everything [`migrate`] does except write.**
///
/// This exists so that [`preview`] is the SAME code path rather than a second implementation of the
/// same rules. A preview that classified separately would be a second opinion about which table a
/// name belongs in, about which keys are ambiguous, and about what is already stored — and the
/// moment those two opinions disagreed, the dry run would be advertising a migration that is not
/// the one the apply performs. `db_tests::the_two_entry_points_share_one_classifier` pins that both
/// call it, and `crates/vike-secrets/tests/migration/database/dry_run.rs`'s
/// `the_dry_run_predicts_exactly_what_the_apply_does` pins the behaviour over real fixtures.
///
/// **It opens NOTHING for writing.** `open_for_read` carries no `SQLITE_OPEN_CREATE`, so the whole
/// of this function is reachable on a box with no database and leaves it with no database — which is
/// the property the dry run is built on, rather than a rollback that would have to be trusted.
///
/// The steps are [`migrate`]'s own 1, 2, 3 and 5; the numbering there is the authority for what each
/// one is for.
pub(super) fn plan(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
) -> Result<Plan, MigrateError> {
    let secrets_path = crate::dotenv::workspace_dotenv_path_from(settings_dir);
    let node_path = crate::dotenv::workspace_node_path_from(settings_dir);
    let db_path = crate::dotenv::workspace_db_path_from(settings_dir);

    // The read-only CARRY: the one reader of a credential file left in this workspace (`carry`'s
    // module doc). Absent is an empty contribution; unreadable is loud.
    let secrets = crate::db::read_credential_file(&secrets_path)?;
    let node = crate::db::read_credential_file(&node_path)?;

    let mut refusals: Vec<Ambiguity> = Vec::new();
    let mut doubly_claimed: Vec<String> = Vec::new();

    // 1. Classify. `wanted[(table, name)] = (value, the file it came from)`.
    let mut wanted: BTreeMap<(Table, String), (String, PathBuf)> = BTreeMap::new();
    for (name, value) in &node {
        if !is_node_key(name) {
            refusals.push(Ambiguity::UnexpectedNameInNodeFile { key: name.clone() });
            continue;
        }
        wanted.insert((Table::NodeKey, name.clone()), (value.clone(), node_path.clone()));
    }
    for (name, value) in &secrets {
        // The same name in both files with different values is a half-migrated box. Whichever side
        // we picked would produce a mismatched pair, so neither is picked.
        if let Some(other) = node.get(name) {
            if other != value {
                refusals.push(Ambiguity::DisagreeingFiles { key: name.clone() });
            } else {
                // An identical value already claimed from the node file is the SAME row, and the
                // node file is the home, so it keeps the attribution — but the credential file DOES
                // hold this line, and a report that never says so under-counts that file against a
                // `grep -c`. See `Migration::doubly_claimed`.
                doubly_claimed.push(name.clone());
            }
            continue;
        }
        let table = if is_node_key(name) { Table::NodeKey } else { Table::Credential };
        wanted.insert((table, name.clone()), (value.clone(), secrets_path.clone()));
    }

    // 2. What is already there. A READ-ONLY open, so a run that turns out to have nothing to do
    //    never opens the database for writing — see step 4 in `migrate`, and `preview`, which never
    //    reaches a step 4 at all.
    let exists = database_present(&db_path);
    let mut version = None;
    let mut stored: BTreeMap<(Table, String), String> = BTreeMap::new();
    if exists {
        let (conn, found) = open_for_read(&db_path)?;
        version = Some(found);
        for table in [Table::Credential, Table::NodeKey] {
            for (name, value) in read_table_on(&db_path, &conn, table, found)?.into_map() {
                stored.insert((table, name), value);
            }
        }
    }

    // 2b. The credential file's COMMENT lines — the one thing the parser above throws away, and
    //     §4.2's rollback values and §4.3's provenance live in nothing else. Read from the SAME
    //     path step 1 read, so there is no second walk; absent is an empty contribution.
    let comments = match std::fs::read_to_string(&secrets_path) {
        Ok(text) => crate::schema::scan_comments(&text),
        // Unreadable is not silently empty anywhere else in this crate either — but step 1 already
        // opened this exact path through `crate::db::read_credential_file` and would have returned its error,
        // so reaching here with an error means the file vanished between the two reads. An empty
        // contribution is then the honest answer: there are no comments to carry.
        Err(_) => crate::schema::FileComments::default(),
    };

    // 3. Compare. Every finding, not the first.
    //
    // ⚠ TWO CLASSES of finding, and they are disposed of differently. See `MigrateError::Ambiguous`
    // and `Migration::refused`.
    let mut pending: BTreeMap<(Table, String), String> = BTreeMap::new();
    let mut already: BTreeSet<(Table, String)> = BTreeSet::new();
    let mut refused: Vec<Ambiguity> = Vec::new();
    for ((table, name), (value, _)) in &wanted {
        if stored.contains_key(&(table.other(), name.clone())) {
            refusals.push(Ambiguity::WrongTable {
                key: name.clone(),
                found_in: table.other(),
                wanted: *table,
            });
            continue;
        }
        match stored.get(&(*table, name.clone())) {
            Some(have) if have == value => {
                already.insert((*table, name.clone()));
            }
            // ⚠ PER-KEY, not whole-run. This used to join `refusals` and abort the run, so an
            // operator who added ONE brand-new key in the same edit that left ONE stale line behind
            // got neither: the new key did not land, and the only way forward was to hand-edit a
            // file to get a DIFFERENT key migrated. The refusal itself is unchanged and is the
            // point — this key is never silently overwritten and it is still NAMED — but it now
            // refuses itself alone, and every unambiguous key beside it lands.
            //
            // ⚠ It is also, by construction, IMPOSSIBLE on the run that CREATES the database:
            // `stored` is populated only `if exists`, so a non-empty `refused` proves a database was
            // already there. The irreversible first run therefore always carries everything the
            // files hold, which is what lets a CLI report a partial run as a finding rather than as
            // a failure.
            Some(_) => {
                refused.push(Ambiguity::DisagreesWithDatabase { key: name.clone(), table: *table })
            }
            None => {
                pending.insert((*table, name.clone()), value.clone());
            }
        }
    }

    if !refusals.is_empty() {
        refusals.sort();
        refusals.dedup();
        return Err(MigrateError::Ambiguous(refusals));
    }
    refused.sort();
    refused.dedup();

    // 5. Report — one row per (file, table) pair that could have contributed. (Step 4 is the WRITE,
    //    and it is the one thing this function does not do.)
    let mut sources = Vec::new();
    for (file, table) in [
        (&secrets_path, Table::Credential),
        (&secrets_path, Table::NodeKey),
        (&node_path, Table::NodeKey),
    ] {
        let keys: Vec<String> = wanted
            .iter()
            .filter(|((t, _), (_, src))| *t == table && src == file)
            .map(|((_, n), _)| n.clone())
            .collect();
        if keys.is_empty() && file == &secrets_path && table == Table::NodeKey {
            // The ordinary post-0051 box: no node key left in the credential store. A zero row here
            // would be noise, and its ABSENCE is the thing worth noticing.
            continue;
        }
        sources.push(SourceReport {
            file: file.clone(),
            table,
            read: keys.len(),
            inserted: keys.iter().filter(|n| pending.contains_key(&(table, (*n).clone()))).count(),
            already_present: keys
                .iter()
                .filter(|n| already.contains(&(table, (*n).clone())))
                .count(),
        });
    }

    doubly_claimed.sort();
    doubly_claimed.dedup();
    Ok(Plan {
        db: db_path,
        exists,
        version,
        comments,
        pending,
        sources,
        doubly_claimed,
        refused,
        stored,
    })
}
