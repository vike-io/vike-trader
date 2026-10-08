//! Ruling 10's move: `move_pending_rows` and `MovedRows`.

use super::*;

/// **PERFORM ruling 10's move** — take every `credential` row the classifier marks
/// `PendingMove::VenueSetting` and re-file it as a `setting` row, in ONE transaction.
///
/// `plan` answers, for one credential NAME, the settings key it should become — `None` for a row
/// that does not move. `vike_bridge_core::credentials` owns that derivation
/// (`classify_credential_name`'s `pending_move` plus `venue_setting_key`); it arrives as a closure
/// because this crate declares no `vike-*` dependency **of that kind** — `vike-bridge-core` is
/// rank 25 where tier 15's rule is *nothing above rank 10*
/// (`crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s
/// `every_tier_15_crate_names_nothing_above_the_vocabulary`), and it declares `vike-secrets`
/// itself, so the reverse edge is a cycle as well as a band violation. ⚠ The qualifier is
/// load-bearing: the unqualified sentence was true
/// until `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
/// 2026-09-20) admitted the `vike-model` edge, and the bound this seam rests on is the RANK, not
/// the absence of any edge at all. The `// MOVING the rows` section comment below already carried
/// the corrected wording; this one had not been swept with it.
///
/// `collisions` is the check `vike_bridge_core::credentials::rendered_name_collisions` performs,
/// handed in already evaluated for the keys this move would write.
///
/// # ⚠ TWO refusals, and both write NOTHING
///
/// * **A COLLISION** — a rendered name a live credential row already holds. §6.2 puts this check on
///   the migration because SQLite cannot express it: the two tables are one namespace and
///   `credential_one_live_name` holds inside `credential` alone.
/// * **A DIVERGENCE** — two names collapsing onto one key while holding different values. The ten
///   moving names are nine rows only because dukascopy's pair was MEASURED equal in the live store;
///   a store where they differ cannot be expressed by one row, and picking either would hand one
///   account the other's JForex server. Nothing here guesses which.
///
/// Both are all-or-nothing rather than per-row, for the reason `crate::schema::reshape_into` gives
/// about its own shortfall: a half-applied move leaves a value in neither home for some reader, and
/// a venue silently drops to paper with nothing to say why.
///
/// # ⚠ `dry_run` writes nothing and answers the same report
///
/// So an operator can see the whole verdict — keys, names, and both refusals — before anything
/// moves. That is the shape `vike-cli config adopt --dry-run` already has, and for the same reason:
/// this touches the only copy of a box's venue keys.
///
/// **The columns a `venue_setting` row is keyed by** — `(venue, tier, field)`, where `tier` is
/// `None` for a machine-scoped value.
///
/// A named type rather than the tuple written out at each site, because `-D clippy::type_complexity`
/// refuses the latter inside a `dyn Fn` and because the shape is the TABLE's, not any one caller's.
pub type VenueSettingKey = (String, Option<String>, String);

/// Splits the dotted operator-facing key into the columns the table holds. See
/// [`move_pending_rows`]' `split` parameter for why this crate takes it rather than owning it.
type SplitVenueKey<'a> = &'a dyn Fn(&str) -> Option<VenueSettingKey>;

/// # Errors
/// The engine, or a store at a schema outside [`READABLE_SCHEMA_VERSIONS`] entirely — which
/// includes, TODAY, a genuine schema-1 store, though not for the reason a first look at this task
/// suggests. ⚠ **This used to say "a store at a schema that predates the `setting` table", and that
/// clause is gone rather than merely corrected — the accurate replacement is a pre-existing bug,
/// not a clean guarantee.** `tx.execute_batch(crate::schema::DDL)` does create `setting` (and every
/// other table) unconditionally, so a store lacking only that table never refuses on its account —
/// but the SAME statement also runs `CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_value ON
/// credential (account_id, field) …`, part of the ORIGINAL 2026-09-14 schema-2 rollout and unrelated
/// to `venue`, which fails to prepare against a genuine schema-1 `credential`
/// (`name TEXT PRIMARY KEY, value TEXT` — neither column exists). [`ensure_venue_id_columns`]'s own
/// `has_column(tx, table, "venue")` guard is real and correct for the ONE precondition it checks,
/// but this function's own `execute_batch(DDL)` call above already fails before that guard is ever
/// reached — see `crates/vike-secrets/tests/migration/database/venue_id.rs`'s
/// `write_settings_still_refuses_a_genuine_schema_1_store_for_an_unrelated_pre_existing_reason`
/// (the identical `execute_batch(DDL)` line, on the sibling writer) for the pinned, CURRENT proof.
/// Reported rather than fixed here: unrelated to `venue_id`, predates this whole branch, and no
/// live box has ever reached it (both have been at schema 2 since before `setting` existed at all).
pub fn move_pending_rows(
    path: &Path,
    plan: &dyn Fn(&str) -> Option<String>,
    // ⚠ The dotted key `plan` renders is the OPERATOR-FACING spelling and the one the collision
    // check compares; the TABLE wants it as columns. This splits it, and it is a parameter for the
    // same reason `plan` is: the grammar lives in `vike_bridge_core::credentials`, and this crate
    // declares exactly ONE external dependency. A second copy of that parser here is precisely the
    // drift `crates/vike-bridge-core/tests/settings_dir_spellings.rs` exists to police elsewhere.
    split: SplitVenueKey<'_>,
    collisions: BTreeMap<String, String>,
    dry_run: bool,
) -> Result<MovedRows, DbError> {
    let mut report = MovedRows { collisions, ..MovedRows::default() };
    let live = read_table(path, Table::Credential)?.into_map();

    // WHICH rows move, and what each becomes. A key several names share is the one-to-many case.
    let mut by_key: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, value) in &live {
        if let Some(key) = plan(name) {
            by_key.entry(key).or_default().push((name.clone(), value.clone()));
        }
    }

    // ⚠ THE DIVERGENCE CHECK, before anything is written and before the collision verdict is acted
    // on: a caller that saw only the collisions would learn about this one row at a time.
    for (key, rows) in &by_key {
        let mut values: Vec<&str> = rows.iter().map(|(_, v)| v.as_str()).collect();
        values.sort_unstable();
        values.dedup();
        if values.len() > 1 {
            let mut names: Vec<String> = rows.iter().map(|(n, _)| n.clone()).collect();
            names.sort();
            report.divergent.insert(key.clone(), names);
        }
    }
    // ⚠ …and the SPLIT, resolved HERE rather than inside the transaction. A key `plan` rendered
    // that `split` cannot read back is a disagreement between two closures the CALLER owns, and the
    // only safe answer is to write nothing: guessing at `(venue, tier, field)` would file a row no
    // reader finds while deleting the credential that worked. Resolving it up front is also what
    // makes the refusal all-or-nothing like the other two — a check inside the loop would already
    // have deleted the rows that came before it.
    let mut columns: BTreeMap<String, VenueSettingKey> = BTreeMap::new();
    for key in by_key.keys() {
        match split(key) {
            Some(triple) => {
                columns.insert(key.clone(), triple);
            }
            None => report.unsplittable.push(key.clone()),
        }
    }
    report.unsplittable.sort();
    if report.refused() {
        return Ok(report);
    }

    for (key, rows) in &by_key {
        report.keys.push(key.clone());
        report.names.extend(rows.iter().map(|(n, _)| n.clone()));
    }
    report.keys.sort();
    report.names.sort();
    if dry_run || report.keys.is_empty() {
        return Ok(report);
    }

    let (mut conn, _created, _version) = open_for_write(path)?;
    let tx = conn.transaction().map_err(|e| DbError::sql(path, e))?;
    // ⚠ Same shape as `edit_account`'s: this writer names `venue_id` in its `venue_setting` INSERT
    // below and never passes through `fill_into`, so a store that predates the column would
    // otherwise fail outright the first time an operator ran `secrets move-venue-config` — see
    // [`ensure_venue_id_columns`]'s own doc.
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(path, e))?;
    ensure_venue_id_columns(&tx).map_err(|e| DbError::sql(path, e))?;
    // `venue_setting`'s venue link (`crate::schema::VenueLink`), from the shape the funnel left.
    let link = crate::schema::VenueLink::of(&tx, "venue_setting", "t")
        .map_err(|e| DbError::sql(path, e))?;
    for (key, rows) in &by_key {
        // The value is the one every name in the group agreed on — the divergence check above is
        // what licenses taking the first.
        //
        // ⚠ **THE `venue_setting` TABLE, NOT A `config` ROW — and the first shape was measured
        // wrong twice.** Ruling 10 filed these as `setting` rows under `config.venue.<venue>…`.
        // That cannot work: `vike_config::Config` carries `#[serde(deny_unknown_fields)]` and has
        // no `venue` field, so the whole `config` section failed to deserialize. MEASURED on the CI box
        // 2026-09-21, twice — first as `expected value at line 1 column 1` (the value was not a
        // JSON scalar), then, with that fixed, as `unknown field 'venue'`. Both times the section
        // fell back to compiled-in defaults, which took the ARMING CEILING with it and read every
        // venue as `paper`.
        //
        // Columns remove both failures at once: `value` is a plain string with nothing to parse,
        // and the row is not inside a typed schema at all. What it buys on top is real validation —
        // the DDL's `CHECK (tier IN ('any','paper','demo','live'))` refuses a mistyped tier at write
        // time, where a map field inside `Config` would have accepted it in silence. (This quoted
        // `('paper','demo','live')` until §5.2 step 7 added `'any'`; the text the store held
        // before step 7 was `tier IS NULL OR tier IN (…)`, never the quoted form.)
        // Resolved above, before the transaction opened — see the SPLIT check.
        //
        // ⚠ `venue_id = excluded.venue_id` on the conflict path too, not only `value` — the IDENTICAL
        // DEFENSIVE clause `crate::settings::set_venue_setting_in`'s twin upsert carries, on the same
        // table and the SAME reasoning, restated here rather than linked because the next reader of
        // THIS statement should not have to jump crates to find out it cannot fire:
        //
        // It makes this statement correct on its own, rather than depending on
        // `ensure_venue_id_columns` above (in this same function) having already filled every NULL.
        // It CANNOT fire today: `crate::schema::DDL`'s `UNIQUE (venue, tier, field)` keys on `venue`
        // first, so a conflicting row always names the SAME `venue` text as the incoming one, and
        // `venue_id` — a pure function of that text — is therefore already whatever
        // `excluded.venue_id` would set it to. (⚠ This named the two partial indexes
        // `venue_setting_one_per_tier`/`venue_setting_one_per_machine` until §5.2 step 7 collapsed
        // them into that one total `UNIQUE`; both keyed on `venue` first too, so the argument is
        // unchanged and only its subject moved.) Since the venue-links flip the table carries a
        // second total `UNIQUE (venue_id, tier, field)`, and the argument holds for it too: a
        // conflict there is on the SAME `venue_id`, which the clause then sets to itself. What
        // WOULD make it fire is a writer that leaves a STALE non-NULL `venue_id` behind, which
        // nothing in this crate does, and which `ensure_venue_id_columns` (NULL-only) would not
        // correct if one ever did. **Not regression-guarded by any test, deliberately** — see
        // `set_venue_setting_in`'s own copy of this note for the measurement. Do not delete this line
        // for lack of a failing test; that absence is the documented point, not dead code.
        let (venue, tier, field) = &columns[key];
        // ⚠ The tier is bound through `crate::schema::stored_venue_setting_tier`, never as the
        // `Option` itself — §5.2 step 7's WRITE boundary. The polymarket proxy family splits to
        // `tier = None`, and binding that wrote SQL NULL, which the shipped `tier TEXT NOT NULL`
        // refuses; the rows would never have moved. It also refuses a `Some("any")`, which the
        // tier split cannot produce (it classifies by `crate::schema::account_tier_named`).
        let stored_tier = crate::schema::stored_venue_setting_tier(tier.as_deref())
            .map_err(|e| DbError::sql(path, e))?;
        tx.execute(
            &format!(
                "INSERT INTO venue_setting ({}, tier, field, value) VALUES ({}, ?2, ?3, ?4) \
                 ON CONFLICT DO UPDATE SET value = excluded.value, venue_id = excluded.venue_id",
                link.columns,
                link.values("?1")
            ),
            (&venue, stored_tier, &field, &rows[0].1),
        )
        .map_err(|e| DbError::sql(path, e))?;
        for (name, _) in rows {
            tx.execute("DELETE FROM credential WHERE name = ?1", [name])
                .map_err(|e| DbError::sql(path, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// MOVING the rows — §6.2's ordering (A), write side
//
// ⚠ The READ side left this module on 2026-09-22. `read_table_folded` and `FoldedSecrets` lived
// here and took the name renderer as a CLOSURE, because the renderer lived in
// `vike_bridge_core::credentials` (layer 30) and this crate declares no `vike-*` dependency of
// that kind. That argument expired when the renderer MOVED DOWN into `crate::venue_setting`: the
// fold now runs inside `crate::store::resolve_store_in` and `resolve_store_scoped_in`, which is
// the only place it reaches all three readers. `read_table_folded` had no production caller and
// would have been a second implementation of the same fold, so it is gone rather than rewritten.
// ---------------------------------------------------------------------------------------------

/// **What a move DID, or would do** — the report `crate::db::move_pending_rows` answers with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovedRows {
    /// The settings keys written, sorted. One key per `venue_setting`-bound credential row, and
    /// FEWER keys than rows wherever a renderer is one-to-many (dukascopy's pair).
    pub keys: Vec<String>,
    /// The credential names removed, sorted. Always at least as many as [`Self::keys`].
    pub names: Vec<String>,
    /// ⚠ **Rows that would have COLLAPSED onto one key while holding DIFFERENT values**, as
    /// `key -> the names that disagreed`. Non-empty means nothing was written.
    ///
    /// §11 counts the ten moving names as nine rows because `DUKASCOPY_DEMO1_SERVER` and
    /// `DUKASCOPY_DEMO2_SERVER` hold the SAME value — MEASURED in the live store on 2026-09-14,
    /// not assumed. If that ever stops being true, one row cannot express both and silently keeping
    /// either would hand one dukascopy account the other's JForex server.
    pub divergent: BTreeMap<String, Vec<String>>,
    /// ⚠ **Rendered names a LIVE credential row already holds**, from
    /// `vike_bridge_core::credentials::rendered_name_collisions`. Non-empty means nothing was
    /// written.
    pub collisions: BTreeMap<String, String>,
    /// ⚠ **Keys `plan` rendered that `split` could not read back** — a disagreement between two
    /// closures the CALLER owns, not a fact about the store.
    ///
    /// It cannot happen while both come from `vike_bridge_core::credentials` (one renders the
    /// grammar the other parses, and `a_field_is_never_mistaken_for_a_tier` holds them level), so
    /// this list is empty on every real run. It exists because the alternative to reporting is
    /// GUESSING at columns for a key whose shape is unknown, and a wrong `(venue, tier, field)`
    /// writes a row a reader will never find while deleting the credential that worked.
    pub unsplittable: Vec<String>,
}

impl MovedRows {
    /// `true` when something stopped the move. Every refusal is all-or-nothing.
    #[must_use]
    pub fn refused(&self) -> bool {
        !self.divergent.is_empty() || !self.collisions.is_empty() || !self.unsplittable.is_empty()
    }
}
