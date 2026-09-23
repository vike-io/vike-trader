//! **`sqlite_sequence` must survive a table rebuild** — §7 item 4 of
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`, in its own words:
//! *"Remove the top account, rebuild the table, assert the next id is not reused."*
//!
//! §4.1 is the hazard this exists for, and it is the half a reader will not otherwise meet:
//! *"The high-water mark lives in a row of `sqlite_sequence`, and `DROP TABLE` deletes it. Nothing
//! in `crates/vike-secrets` knows this table exists today. On THIS migration there is no prior mark
//! to lose (the constraint is being introduced), so copying rows with their ids sets the sequence
//! to `max(id)`, which is correct. **The hazard is every FUTURE rebuild of these tables**: one that
//! replays surviving rows after the top row was removed rewinds the mark and silently un-does the
//! guarantee."*
//!
//! That sentence is the whole of it: a rebuild that copies rows WITH THEIR IDS sets the mark to
//! `max(id)` of what it copied, which is correct only while the top row is among them. Remove the
//! top row first and the mark rewinds to the survivor's id, and the next insert is handed a number
//! an operator already wrote down against a different account.
//!
//! # ⚠ Where this gate lives — the ruling §7 item 4 leaves open, and it was got WRONG first
//!
//! The obstacle is real: `crate::schema::reshape_into` is PRIVATE and un-re-exported, so nothing
//! under `tests/` can call it. **That is not the same as being unable to run the real reshape.**
//! `vike_secrets::migrate` is the PRODUCTION entry point that performs it — `fill_into` calls
//! `reshape_into` whenever it finds a store below the current schema — and `vike_secrets::
//! plant_schema_1` (behind the crate's `test-support` feature) plants the schema-1 store it runs
//! on. So [`the_real_reshape_does_not_rewind_an_armed_tables_mark`] drives the crate's own rebuild
//! through the same call a binary makes, which is a STRONGER witness than reaching past the
//! module's front door.
//!
//! ⚠ **This shipped first as a `#[cfg(test)]` module in `src/db/`, and that home was MEASURED to be
//! wrong** — five gates in `crates/vike-ops/tests/` fire on it, all correctly, because a file under
//! `crates/vike-secrets/src/` is held to the credential store's rules whether or not `#[cfg(test)]`
//! will delete it:
//!
//! * `compile_time_path_gate`'s `compile_time_paths_do_not_grow` — [`source_files`] resolves this
//!   crate's own directory from `CARGO_MANIFEST_DIR`, i.e. the tree the compiler was run in. That
//!   is exactly the defect that gate exists for, and in `src/` it is indistinguishable from a
//!   binary baking in its build checkout.
//! * `credential_source_roster_gate`'s `every_file_name_the_store_crate_spells_is_pinned` —
//!   [`TABLE_DROP_PIN`] spells `db.rs`, `schema.rs` and `profile_store.rs`, and a file NAME inside
//!   the store crate is a place a credential value can come from until somebody says otherwise.
//! * the same gate's `every_ingress_site_in_the_store_is_pinned` — `read_dir`, `read_to_string`
//!   and `Connection::open` are byte-ingress sites, and in `src/` each needs a declared row.
//! * the same gate's `the_test_cut_hides_no_production_item` — its `#[cfg(test)]` cut looks for a
//!   `}` at column zero, and `#[cfg(test)] mod name;` (a DECLARATION, not a block) never gives it
//!   one, so the cut could no longer prove what it was hiding.
//! * `credential_writer_gate`'s `every_credential_writer_caller_is_pinned`.
//!
//! Each of those is a row somebody could have written in another crate's gate. None should have
//! been: the scan half of this file has no business in `src/` at all, and once it moves, the
//! behavioural half loses nothing by coming with it.
//!
//! # ⚠ The debt is DERIVED rather than pinned, and stage 4 PAID it on 2026-09-23
//!
//! This gate shipped GREEN against a schema where `account` was `id INTEGER PRIMARY KEY` with no
//! `AUTOINCREMENT` anywhere near it, so §7 item 4's assertion — *the next id is not reused* — was
//! FALSE and a red test cannot be landed at all (`crates/vike-secrets/tests/store_link_gate.rs`
//! says so in its own words, and `crates/vike-secrets/tests/null_discriminator_gate.rs` shipped as
//! a ratchet for the same reason).
//!
//! The resolution was stronger than a pinned expectation, because the expectation is DERIVED from
//! the shipped schema rather than written down:
//! [`section_7_item_4_remove_the_top_account_rebuild_and_create_again`] asks [`armed_tables`]
//! whether `account` declares `AUTOINCREMENT` and asserts REUSE when it does not, NON-REUSE when it
//! does. So:
//!
//! | what stage 4 lands | this gate |
//! |---|---|
//! | nothing (the state this file landed against) | green — reuse MEASURED, not assumed |
//! | `AUTOINCREMENT` + a rebuild that carries the mark — **where the tree is now** | green, and §7 item 4's assertion is the live one |
//! | `AUTOINCREMENT` + a writer that defeats it (an explicit `id` in `edit_account`'s `INSERT`) | **RED** |
//!
//! The third row is not hypothetical — it is one of this gate's kill proofs, run against the real
//! `edit_account`.
//!
//! [`SEQUENCE_PIN`] was the staleness half and did its job: `account` was pinned
//! [`Sequence::Owed`], the shipped `DDL` armed it, and
//! [`every_pinned_verdict_matches_the_schema`] went red naming stage 4 — which is how stage 4 was
//! stopped from landing silently. Every row there now reads [`Sequence::Armed`].
//!
//! # Declared scope — what a green here does NOT cover
//!
//! * ⚠ **This bullet read *"No production path rebuilds an ARMED table today"* and stage 4
//!   falsified it**: `crate::schema::rebuild_table_from_ddl` is the one rebuild procedure in this
//!   crate, and both migrations that call it —
//!   `crate::schema::migrate_sim_tier_to_paper` and
//!   `crate::schema::migrate_tables_onto_autoincrement` — now touch armed tables. The live
//!   behaviour that had no assertion now has one:
//!   [`the_paper_tier_rebuild_does_not_rewind_the_account_marks`], which drives the REAL repair
//!   path over a store whose mark is above its top surviving row. What still covers the rebuilds
//!   nobody has written yet is [`every_table_drop_in_this_crates_source_is_classified`]: a rebuild
//!   needs a table drop, this crate's `src/` carries exactly the ones pinned, and a new one
//!   reddens until its author writes down what happens to the mark. That is a classification duty,
//!   not a proof — and it has already caught one, see [`TABLE_DROP_PIN`].
//! * **[`a_rebuild_that_does_not_carry_the_mark_rewinds_it`] measures the ENGINE**, in the same
//!   spirit as `crates/vike-secrets/src/db.rs`'s `the_schema_stamp_and_the_ddl_are_both_transactional`:
//!   it is pinned rather than assumed because every claim above rests on it. It is not a claim
//!   about a caller.
//! * **The scan reads `src/` only.** The rebuild helper in THIS file performs a table drop of its
//!   own and is deliberately out of scope: a harness is not production.

mod support;

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension};
use support::Fixture;
use vike_secrets::{AccountEdit, DDL};

// -------------------------------------------------------------------------------------------
// The pin
// -------------------------------------------------------------------------------------------

/// Whether a table of this plane has an `AUTOINCREMENT` high-water mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sequence {
    /// The table's own `id` declares `AUTOINCREMENT`, so SQLite keeps a `sqlite_sequence` row for
    /// it and a rebuild can LOSE that row.
    Armed,
    /// **The debt.** It does not, so SQLite hands a new row `max(rowid) + 1` and a freed id comes
    /// straight back out. A row carrying this word names the stage that retires it.
    Owed,
}

impl std::fmt::Display for Sequence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Sequence::Armed => "Armed",
            Sequence::Owed => "Owed",
        })
    }
}

/// **Every table this gate has a verdict about**, as `(table, verdict, why)`.
///
/// ⚠ **The declared length is the COUNT** — no prose anywhere, this file included, may restate it.
///
/// This is deliberately NOT every table in [`DDL`]. ⚠ **It was one row short of every ARMED table
/// until stage 4 landed, and the sentence that stood here said why: §4.1's identity ruling names
/// `account` alone, so the plane's other id-bearing tables carried no row.** Ruling 6 then gave
/// every one of them the same shape — *"One uniform table shape, so no per-table judgment is
/// required"* — and [`every_armed_table_is_classified`] turned that into rows here, which is the
/// growth this gate was built to force rather than a widening somebody chose.
///
/// Two tables in [`DDL`] are still absent, and neither escapes by accident: `settings_adoption` is
/// ruling 6's one STATED exception (*"a singleton whose `id` is a seal rather than a surrogate"*)
/// and `venue_arming` is a table §3 spells DELETED — neither declares `AUTOINCREMENT`, so neither
/// has a mark to keep, and the day either one gains one this gate reddens naming it.
const SEQUENCE_PIN: [(&str, Sequence, &str); 7] = [
    (
        "account",
        Sequence::Armed,
        "spec §2.4 and §4.1 — the ruled identity, and the one table §7 item 4 names. ⚠ ARMED by \
         §9's stage 4 on 2026-09-23, which is what made every carry in this crate load-bearing: \
         `crate::schema::rebuild_table_from_ddl` is the one rebuild procedure and both migrations \
         that call it now touch an armed table. \
         `the_paper_tier_rebuild_does_not_rewind_the_account_marks` is the behaviour assertion \
         that fails if its mark carry is removed, and \
         `section_7_item_4_remove_the_top_account_rebuild_and_create_again` is §7 item 4 proper, \
         now asserting NON-reuse",
    ),
    (
        "venue",
        Sequence::Armed,
        "armed since stage 2 seeded it, and the first table a rebuild could rewind — \
         `account.venue_id`, `credential.venue_id`, `venue_arming.venue_id` and \
         `venue_setting.venue_id` all REFERENCE it, so a reused venue id re-points live rows at a \
         different venue rather than merely confusing an operator's note",
    ),
    (
        "credential",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4. Its rows hold the live venue keys and \
         `credential_one_live_name` is what tells one account's key set from another's, so an id \
         handed out twice names two different SECRETS across time; nothing in the tree stores a \
         credential id, which is why this is a shape guarantee rather than an addressing one",
    ),
    (
        "venue_setting",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4. ⚠ It is the SECOND table \
         `crate::schema::migrate_sim_tier_to_paper` rebuilds, so its mark carry is load-bearing \
         for exactly the same reason `account`'s is",
    ),
    (
        "setting",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4. Addressed by `UNIQUE (section, key)` and \
         never by id, so what the mark protects here is the uniform shape itself",
    ),
    (
        "profile_risk",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4. Addressed by `UNIQUE (profile, key)`, same \
         as `setting` above",
    ),
    (
        "node_key",
        Sequence::Armed,
        "ruling 6's uniform shape, armed by stage 4 — and the one table that had NO surrogate id \
         at all before it (`name TEXT PRIMARY KEY`). Its `name` keeps a `UNIQUE`, which is what \
         `crates/vike-secrets/src/db.rs`'s `ON CONFLICT(name) DO UPDATE` upsert resolves against, \
         so 0051's pair is still addressed by NAME and the id is the shape, not the address",
    ),
];

const GROWTH_GUIDANCE: &str = "\
A table in the shipped `DDL` now declares `AUTOINCREMENT` and nothing here says so. Add its row:

  * `Sequence::Armed` — and then answer the question this file exists to ask: every rebuild of that
    table must carry its `sqlite_sequence` mark across, because a rebuild that replays surviving
    rows after the top one was removed rewinds the mark. `rebuild_preserving_ids` in this file
    measures that ONE property — the mark carry — which every real rebuild in this crate also
    performs; its own doc says plainly where its rename-aside shape otherwise diverges from theirs.
  * `Sequence::Owed` — the table has no mark and its ids are reused. Name the stage that arms it.";

// -------------------------------------------------------------------------------------------
// The DDL derivation
// -------------------------------------------------------------------------------------------

/// `(table name, column body)` for every `CREATE TABLE` in a schema.
///
/// The same shape `crates/vike-secrets/tests/store_link_gate.rs`'s `tables` uses, and deliberately
/// a second copy rather than a shared one: the two answer DIFFERENT questions about the body, and
/// `tests/support/mod.rs` is shared TEST FIXTURE rather than a home for one binary's parser.
fn tables(ddl: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for chunk in ddl.split("CREATE TABLE IF NOT EXISTS ").skip(1) {
        let name: String = chunk.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let open = chunk.find('(').expect("a CREATE TABLE has a body");
        let close = chunk.find(") STRICT").expect("every table in this store is STRICT");
        out.push((name, chunk[open + 1..close].to_string()));
    }
    out
}

/// Every table in `ddl` whose body declares `AUTOINCREMENT` — the tables SQLite keeps a
/// `sqlite_sequence` row for, and therefore the tables a rebuild can rewind.
fn armed_tables(ddl: &str) -> BTreeSet<String> {
    tables(ddl)
        .into_iter()
        .filter(|(_, body)| body.to_uppercase().contains("AUTOINCREMENT"))
        .map(|(name, _)| name)
        .collect()
}

/// The single `CREATE TABLE` statement for `table`, lifted out of the shipped [`DDL`] rather than
/// re-spelled, so a fixture that plants one table plants the one the store actually ships.
fn create_statement(ddl: &str, table: &str) -> String {
    let needle = format!("CREATE TABLE IF NOT EXISTS {table} ");
    let at = ddl.find(&needle).unwrap_or_else(|| panic!("`DDL` declares no table `{table}`"));
    let rest = &ddl[at..];
    let end = rest.find(';').expect("a CREATE TABLE statement ends");
    format!("{};", &rest[..end])
}

fn pinned() -> BTreeMap<String, Sequence> {
    SEQUENCE_PIN.iter().map(|(table, verdict, _)| ((*table).to_string(), *verdict)).collect()
}

// -------------------------------------------------------------------------------------------
// The engine seam — the table this crate did not know existed
// -------------------------------------------------------------------------------------------

/// This table's `sqlite_sequence` high-water mark, or `None` when it has no row (or when no
/// `AUTOINCREMENT` table has ever existed in this database, in which case the table itself is
/// absent and a bare query would be a `no such table` error rather than an answer).
fn mark(conn: &Connection, table: &str) -> Option<i64> {
    let present: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence'",
            [],
            |r| r.get(0),
        )
        .expect("sqlite_master is always readable");
    if present == 0 {
        return None;
    }
    conn.query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| r.get(0))
        .optional()
        .expect("reading a mark")
}

/// Put a mark back — the half a rebuild has to perform for itself.
fn set_mark(conn: &Connection, table: &str, seq: i64) {
    conn.execute("DELETE FROM sqlite_sequence WHERE name = ?1", [table]).expect("clear the mark");
    conn.execute("INSERT INTO sqlite_sequence (name, seq) VALUES (?1, ?2)", (table, seq))
        .expect("restore the mark");
}

fn max_id(conn: &Connection, table: &str) -> Option<i64> {
    conn.query_row(&format!("SELECT max(id) FROM {table}"), [], |r| r.get::<_, Option<i64>>(0))
        .expect("max id")
}

/// **Every table [`DDL`] gives a `venue_id INTEGER REFERENCES venue(id)` column** — `account`,
/// `credential`, `venue_setting`, `venue_arming` — checked for a row naming `venue_id`. Used to
/// turn *"nothing in this store points at it"* from a comment into an assertion: a roster reorder
/// that moved a REFERENCED venue to the top would otherwise make the DELETE below refuse (a
/// foreign-key question) rather than silently measure the wrong thing, but only if something
/// actually checks for it first.
fn nothing_references_venue(conn: &Connection, venue_id: i64) -> bool {
    ["account", "credential", "venue_setting", "venue_arming"].iter().all(|table| {
        let n: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE venue_id = ?1"),
                [venue_id],
                |r| r.get(0),
            )
            .expect("counting venue_id references");
        n == 0
    })
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
        .expect("sqlite_master is always readable");
    n > 0
}

fn index_exists(conn: &Connection, index: &str) -> bool {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [index],
            |r| r.get(0),
        )
        .expect("sqlite_master is always readable");
    n > 0
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut out = Vec::new();
    conn.pragma(None, "table_info", table, |row| {
        out.push(row.get::<_, String>(1)?);
        Ok(())
    })
    .expect("table_info");
    out
}

/// Whether [`rebuild_preserving_ids`] carries the old mark across, which is the ONE difference
/// between a correct rebuild and §4.1's hazard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkCarry {
    /// The correct procedure.
    Carried,
    /// §4.1's hazard, spelled out: the rows are replayed with their ids and nothing else is done.
    Dropped,
}

/// **Rebuild `table` in place, preserving every surviving row's id** — the NAIVE shape: rename
/// the old table aside, re-run the shipped [`DDL`] to create the new one, copy the rows across,
/// drop the old one.
///
/// ⚠ **This is NOT the shape stage 4 uses, and this doc used to claim it was.**
/// `crate::schema`'s `rebuild_table_from_ddl` (`crates/vike-secrets/src/schema.rs`) builds the
/// NEW table BESIDE the old one under a scratch name instead, and renames `scratch -> {table}` at
/// the end — the OPPOSITE direction — because Task 7 measured the rename-aside shape corrupt one
/// production rebuild (see the pragma note below) and inverted the problem away rather than
/// patching it. This function keeps the naive, rename-aside shape deliberately: it is what makes
/// the pragma pair below load-bearing rather than tidy, and it is what this file's §4.1
/// measurement is about.
///
/// # ⚠ The pragma pair below — BOTH load-bearing, and MEASURED rather than argued
///
/// This attribution was written down three times in three different directions by careful reading,
/// each time confidently. It was settled on 2026-09-23 by running it: each pragma mutated out on a
/// throwaway branch against this file's own tests, plus the 2x2 matrix asked of the engine
/// directly. What follows is what the engine answered (SQLite 3.53.2, the `bundled`
/// `libsqlite3-sys` build this workspace links). Do not "correct" it from a doc page — re-measure.
///
/// **The starting state is not what it looks like.** [`open`] is a bare `Connection::open`, and a
/// bare connection HERE comes up with `foreign_keys = 1`, not `0`: the bundled build is compiled
/// with `SQLITE_DEFAULT_FOREIGN_KEYS` (it is listed in `PRAGMA compile_options`), which inverts
/// SQLite's own documented default. So neither statement below is a restatement of the default,
/// and an argument that starts "foreign keys are off anyway" starts from a false premise.
///
/// * `foreign_keys = OFF` — **required, and not because it suppresses the rewrite.** Two things
///   rest on it. (1) `DROP TABLE {scratch}` below performs an implicit `DELETE FROM`, which fires
///   every child row's foreign key: with this statement deleted, both of this function's callers
///   die at that line with `drop the old table: … FOREIGN KEY constraint failed` (extended code
///   787) — measured. (2) It is what makes the NEXT pragma effective at all; see the matrix.
/// * `legacy_alter_table = ON` — **this is the pragma that suppresses SQLite's rewrite** of every
///   OTHER table's `REFERENCES` clause to follow a rename, and it suppresses it only while
///   `foreign_keys` is OFF. With this statement deleted and `foreign_keys = OFF` kept, a
///   `REFERENCES account(id)` clause is rewritten to name the scratch table instead and survives
///   the scratch table's own DROP pointing at nothing: both callers redden on this function's
///   closing check, ``the rebuild of `venue` left dangling references`` with 3 of them and
///   ``the rebuild of `account` left dangling references`` with 5 — measured.
///
/// The matrix behind those two sentences, on the rename-ASIDE shape, `REFERENCES` rewritten?
///
/// | `foreign_keys` | `legacy_alter_table` | rewritten | `pragma_foreign_key_check` |
/// |---|---|---|---|
/// | OFF | OFF | **yes** | 1 |
/// | OFF | ON  | no      | 0 |
/// | ON  | OFF | **yes** | the DROP fails first |
/// | ON  | ON  | **yes** | the DROP fails first |
///
/// The last row is the one that makes `legacy_alter_table` a conditional cure rather than a cure:
/// with `foreign_keys` ON it does nothing at all.
///
/// # ⚠ A rename-ASIDE outside a transaction is a DIFFERENT HAZARD from a rename-INTO-PLACE inside
/// one — do not carry this pragma pair across the two
///
/// That is the whole reason this note is long. `crate::schema`'s `rebuild_table_from_ddl` renames
/// the SCRATCH into place, inside the caller's transaction, on a connection whose `foreign_keys`
/// was pinned ON at open. Nothing references the scratch name, so there is no clause to rewrite —
/// `legacy_alter_table` genuinely IS inert there, measured: the child's `REFERENCES` clause comes
/// out byte-identical under both settings. And that function cannot reach for `foreign_keys = OFF`
/// either, because `PRAGMA foreign_keys` is a documented NO-OP inside a transaction — measured too:
/// issued inside one it returns `Ok` and the engine still answers `1`. What carries that function is
/// `defer_foreign_keys` plus the beside-not-aside shape, and its own trap 1 is the authority.
///
/// **The transplant is not hypothetical, and the matrix above explains it.** Task 7's first
/// rebuild attempt copied THIS pair into THAT shape: inside a transaction `foreign_keys = OFF` did
/// nothing, so the rename ran at the matrix's last row, where `legacy_alter_table = ON` also does
/// nothing — and its own `pragma_foreign_key_check` refused it, naming two dangling `credential`
/// rows. The lesson recorded from that failure was "`foreign_keys` is the pragma that suppresses",
/// which is this doc's third wrong direction. What actually happened is that a pair correct for one
/// shape was moved into a shape neither pragma can serve.
fn rebuild_preserving_ids(conn: &Connection, table: &str, carry: MarkCarry) {
    let scratch = format!("{table}_rebuild_scratch");
    let before = mark(conn, table);

    conn.execute_batch("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON;")
        .expect("a rebuild suspends the constraints it is about to break");
    conn.execute_batch(&format!("ALTER TABLE {table} RENAME TO {scratch};"))
        .expect("rename the old table aside");
    conn.execute_batch(DDL).expect("re-create it from the shipped schema");

    // The columns the OLD table actually had, intersected with the new one's — a rebuild carries
    // what it can and never names a column one side does not have.
    let old: BTreeSet<String> = columns(conn, &scratch).into_iter().collect();
    let shared: Vec<String> =
        columns(conn, table).into_iter().filter(|c| old.contains(c)).collect();
    assert!(shared.iter().any(|c| c == "id"), "a rebuild of `{table}` must carry its ids");
    let list = shared.join(", ");
    conn.execute_batch(&format!("INSERT INTO {table} ({list}) SELECT {list} FROM {scratch};"))
        .expect("replay the surviving rows with their ids");

    // ⚠ THE STATEMENT §4.1 IS ABOUT. Dropping the scratch table deletes ITS `sqlite_sequence` row —
    // the one the rename moved the mark onto — so what the rebuilt table is left holding is the
    // mark the replay above created, i.e. `max(id)` of what survived.
    conn.execute_batch(&format!("DROP TABLE {scratch};")).expect("drop the old table");

    // ⚠ AND A SECOND `DDL` PASS, which is not belt-and-braces. A rename carries the old table's
    // NAMED INDEXES with it, so `account_one_account_per_book` was still attached to the scratch
    // table when the batch above ran — and `CREATE UNIQUE INDEX IF NOT EXISTS` saw that name
    // already taken and did nothing. The drop then took the index with the scratch table, leaving a
    // rebuilt table with no unique index on it and every statement still green.
    // ⚠ Stage 4's own `rebuild_table_from_ddl` does NOT meet this trap: it builds the new table
    // under the scratch name (the opposite direction from this function), so the indexes stay
    // attached to the table being DROPPED — their names come free again, and that function's own
    // closing `DDL` pass re-creates them on the table that now carries the real name. See its
    // trap 2.
    conn.execute_batch(DDL).expect("re-create the indexes the scratch table was holding");

    if carry == MarkCarry::Carried
        && let Some(seq) = before
    {
        set_mark(conn, table, seq);
    }

    let violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
        .expect("foreign key check");
    assert_eq!(violations, 0, "the rebuild of `{table}` left dangling references");
    conn.execute_batch("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON;")
        .expect("restore the constraints");
}

// -------------------------------------------------------------------------------------------
// Fixtures
// -------------------------------------------------------------------------------------------

/// Create an account through the REAL verb and hand back its id.
///
/// ⚠ **This is how *"remove the top account"* is spelled given `AccountEdit::Remove`'s refusal.**
/// `edit_account` refuses to delete a row that still owns live `credential` rows
/// (`DbErrorKind::AccountHasCredentials`, named by key), so every account this gate creates is
/// created by the LIFECYCLE verb and given no credential at all — `account_key_names` then answers
/// empty and the removal is permitted. The alternative (writing credentials and superseding them
/// first) would need whole credential key spellings this fixture has no other use for.
fn create(fx: &Fixture, venue: &str, tier: &str, label: &str) -> i64 {
    fx.edit(AccountEdit::Create { venue, tier, label: Some(label) })
        .expect("create")
        .after
        .expect("a create leaves a row")
        .id
}

fn open(fx: &Fixture) -> Connection {
    Connection::open(fx.db()).expect("open the store directly")
}

// -------------------------------------------------------------------------------------------
// The gate — the schema derivation
// -------------------------------------------------------------------------------------------

#[test]
fn the_table_parser_sees_every_table_in_the_ddl() {
    // The anti-vacuity guard for [`armed_tables`]: a parser that quietly found NOTHING would make
    // `every_armed_table_is_classified` pass against any schema at all, including one that armed
    // every table and rebuilt them all naively. Derived rather than hand-counted, so a table the
    // parser drops or double-counts reddens this regardless of its name.
    let seen = tables(DDL).len();
    let expected = DDL.matches("CREATE TABLE IF NOT EXISTS ").count();
    assert_eq!(
        seen, expected,
        "the parser found {seen} table(s) but `DDL` contains {expected} `CREATE TABLE IF NOT \
         EXISTS` statement(s) — the parser missed one (or double-counted), which would make every \
         OTHER test in this file blind to whatever it dropped"
    );
}

#[test]
fn the_lifted_create_statement_is_the_shipped_one() {
    // The anti-vacuity guard for [`create_statement`], which the reshape fixture plants its armed
    // table from. A lifter that returned a truncated statement, or somebody else's, would make
    // that test measure a table this store does not ship.
    let venue = create_statement(DDL, "venue");
    assert!(venue.starts_with("CREATE TABLE IF NOT EXISTS venue "), "{venue}");
    assert!(venue.trim_end().ends_with("STRICT;"), "the statement must be whole: {venue}");
    assert!(
        venue.to_uppercase().contains("AUTOINCREMENT"),
        "the lifted statement must be the ARMED one this fixture needs: {venue}"
    );
    assert_eq!(
        venue.matches("CREATE TABLE").count(),
        1,
        "the lifter ran past the end of its own statement: {venue}"
    );
}

#[test]
fn every_armed_table_is_classified() {
    let pinned = pinned();
    let unclassified: Vec<String> = armed_tables(DDL)
        .into_iter()
        .filter(|table| !pinned.contains_key(table))
        .map(|table| format!("  {table}"))
        .collect();
    assert!(
        unclassified.is_empty(),
        "\nTable(s) in the shipped `DDL` declare `AUTOINCREMENT` and nothing in this gate says \
         so:\n\n{}\n\n{}\n\npinned {}\n",
        unclassified.join("\n"),
        GROWTH_GUIDANCE,
        SEQUENCE_PIN.len(),
    );
}

#[test]
fn the_pin_has_no_stale_rows() {
    let live: BTreeSet<String> = tables(DDL).into_iter().map(|(name, _)| name).collect();
    let gone: Vec<String> = SEQUENCE_PIN
        .iter()
        .filter(|(table, _, _)| !live.contains(*table))
        .map(|(table, verdict, _)| format!("  {table} ({verdict})"))
        .collect();
    assert!(
        gone.is_empty(),
        "\n`SEQUENCE_PIN` names table(s) the shipped `DDL` no longer carries:\n\n{}\n\nDelete \
         those lines and decrement the declared array length.\n",
        gone.join("\n"),
    );
}

/// **The staleness assertion — this is what goes red the moment stage 4 pays the debt.**
#[test]
fn every_pinned_verdict_matches_the_schema() {
    let armed = armed_tables(DDL);
    let wrong: Vec<String> = SEQUENCE_PIN
        .iter()
        .filter_map(|(table, verdict, _)| {
            let derived = if armed.contains(*table) { Sequence::Armed } else { Sequence::Owed };
            (derived != *verdict)
                .then(|| format!("  {table} — pinned {verdict}, `DDL` derives {derived}"))
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "\nA pinned verdict disagrees with the schema it claims to describe:\n\n{}\n\nOwed -> \
         Armed is the WIN this gate was written to wait for: spec §9's stage 4 landed. Three \
         things are now owed here, in this order — (1) flip the row's word to `Sequence::Armed` \
         and replace its `why` with the argument for the rebuild that carries its mark; (2) \
         re-read `section_7_item_4_remove_the_top_account_rebuild_and_create_again`, whose \
         expectation is DERIVED and has just inverted, so it is now asserting §7 item 4 proper; \
         (3) re-read EVERY `TABLE_DROP_PIN` row that names a rebuild of this table, because a \
         no-op mark carry has just become load-bearing and nothing else will say so.\n\nArmed -> \
         Owed means an `AUTOINCREMENT` was REMOVED, which re-opens id reuse on a column spec §4.1 \
         calls the account's identity.\n",
        wrong.join("\n"),
    );
}

// -------------------------------------------------------------------------------------------
// The gate — the behaviour
// -------------------------------------------------------------------------------------------

/// **The fact §4.1 says nobody here knows**, measured against a store built entirely by this
/// crate's own code: `sqlite_sequence` exists, and it carries a row for every table the shipped
/// schema arms.
///
/// It is also the precondition for everything below it. A store with no `sqlite_sequence` table at
/// all would make every mark comparison in this file compare `None` with `None` and pass.
#[test]
fn a_real_store_carries_a_sqlite_sequence_mark_for_every_armed_table() {
    let fx = Fixture::migrated();
    let conn = open(&fx);

    // ⚠ An armed table this fixture left EMPTY is SKIPPED rather than failed. When stage 4 arms
    // `account`, a store may legitimately hold no account row, and an empty table's mark says
    // nothing about the engine — failing there would redden stage 4 for a fixture's reason rather
    // than a real one. The counter below is what keeps the skip from emptying the test out.
    let armed = armed_tables(DDL);
    let mut checked = 0usize;
    for table in &armed {
        let Some(rows) = max_id(&conn, table) else { continue };
        assert_eq!(
            mark(&conn, table),
            Some(rows),
            "`{table}` is armed and holds rows, so SQLite must be keeping a `sqlite_sequence` \
             high-water mark for it at `max(id)` — if this is None the engine kept no mark and \
             every rebuild assertion in this file is measuring nothing"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no armed table in this store holds a row, so nothing above was compared — the fixture \
         stopped reaching the engine and every mark assertion in this file is now vacuous"
    );
}

/// **§7 item 4, literally: remove the top account, rebuild the table, and see what the next id
/// is.**
///
/// The expectation is DERIVED from the shipped `DDL` rather than written down, which is what lets
/// this land green today and bind for real the day stage 4 arms the column — see this module's
/// table. The rebuild here CARRIES the mark (the correct procedure), so a red means the schema arms
/// `account` and the store handed the freed id back anyway: either the rebuild lost the mark or a
/// writer is assigning ids itself.
#[test]
fn section_7_item_4_remove_the_top_account_rebuild_and_create_again() {
    let fx = Fixture::migrated();

    let first = create(&fx, "binance", "live", "ONE");
    let second = create(&fx, "binance", "live", "TWO");
    let top = create(&fx, "binance", "live", "THREE");
    assert!(first < second && second < top, "the fixture needs the third row to hold the top id");

    // "Remove the top account" — permitted because this row owns no credential. See `create`.
    fx.edit(AccountEdit::Remove { id: top }).expect("a keyless row removes");

    let conn = open(&fx);
    assert_eq!(max_id(&conn, "account"), Some(second), "the top id is now free");
    rebuild_preserving_ids(&conn, "account", MarkCarry::Carried);

    // Anti-vacuity: the rebuild must actually have happened, and must actually have PRESERVED the
    // ids. A rebuild that silently did nothing, or that renumbered the survivors, would make the
    // assertion below a statement about something else entirely.
    assert!(
        !table_exists(&conn, "account_rebuild_scratch"),
        "the scratch table survived — the rebuild did not finish"
    );
    let survivors: BTreeSet<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM account").expect("prepare");
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).expect("query");
        rows.map(|r| r.expect("row")).collect()
    };
    assert!(
        survivors.contains(&first) && survivors.contains(&second) && !survivors.contains(&top),
        "the rebuild must replay the surviving rows with their ids and resurrect none: {survivors:?}"
    );
    assert!(
        index_exists(&conn, "account_one_account_per_book"),
        "the rebuilt table lost the unique index the rename carried off with the scratch table — \
         see `rebuild_preserving_ids`' second `DDL` pass, and the trap it is there for"
    );
    drop(conn);

    // A DIFFERENT account, at a different venue, with a different label — nothing about it says it
    // should inherit the removed row's number.
    let reborn = create(&fx, "okx", "demo", "FOUR");

    if armed_tables(DDL).contains("account") {
        assert_ne!(
            reborn, top,
            "`account` declares `AUTOINCREMENT`, so §7 item 4's assertion is live: the id freed \
             by the removal must NOT come back. It did. Either the rebuild replayed the surviving \
             rows without carrying `sqlite_sequence` across (spec §4.1's hazard — `MarkCarry::\
             Dropped` in this file is that mistake, spelled out), or a writer is computing the id \
             itself instead of letting the engine assign it."
        );
        assert!(reborn > top, "an armed table hands out ids strictly above its high-water mark");
    } else {
        assert_eq!(
            reborn, top,
            "THE DEBT, measured: `account` carries no `AUTOINCREMENT`, so SQLite hands a new row \
             `max(rowid) + 1` and the id the removed account held comes straight back out — spec \
             §2.4. If this assertion is the one that failed, the schema changed WITHOUT \
             `AUTOINCREMENT` appearing in `account`'s body, and \
             `every_pinned_verdict_matches_the_schema` is not going to tell you about it."
        );
    }
}

/// **§4.1's hazard, run on the one table the shipped schema arms today.**
///
/// A rebuild that replays the surviving rows with their ids sets the mark to `max(id)` of what it
/// replayed. Remove the top row first and that is BELOW the old mark, so the next insert is handed
/// a number that has already been used. Carrying the mark across is the whole cure, and it is one
/// statement.
///
/// ⚠ This measures the ENGINE, and it is pinned rather than assumed because every claim in this
/// file rests on a dropped table taking its `sqlite_sequence` row with it, and nothing else in this
/// workspace states that anywhere.
#[test]
fn a_rebuild_that_does_not_carry_the_mark_rewinds_it() {
    assert!(
        armed_tables(DDL).contains("venue"),
        "this test runs on `venue` because it is the table the shipped schema arms; if it no \
         longer is, `SEQUENCE_PIN` is where that is decided and this test needs a new subject"
    );

    for (carry, expectation) in [(MarkCarry::Dropped, "rewinds"), (MarkCarry::Carried, "survives")]
    {
        let fx = Fixture::migrated();
        let conn = open(&fx);

        let top = max_id(&conn, "venue").expect("the migration seeded the roster");
        assert_eq!(mark(&conn, "venue"), Some(top), "the precondition: the mark is at the top row");
        // ⚠ The venue at the TOP of the roster must have nothing pointing at it: a `venue_id`
        // referencing the row would make this a foreign-key question rather than a sequence one,
        // and the delete below would be refused instead of measured. This holds only because
        // `vike_model::VENUES`' tail is `hyperliquid` while `support::FIXTURE_KEYS` names
        // binance/dukascopy/cloudflare — asserted rather than left as a comment, so a roster
        // reorder fails HERE, loudly and by name, instead of quietly testing the FK refusal path
        // under this test's name.
        assert!(
            nothing_references_venue(&conn, top),
            "the venue at the top of the roster (id {top}) is referenced by a `venue_id` column — \
             this test no longer measures the sequence hazard it claims to; the roster changed \
             under it and this fixture needs a genuinely-unreferenced venue instead"
        );
        conn.execute("DELETE FROM venue WHERE id = ?1", [top]).expect("remove the top row");
        assert_eq!(mark(&conn, "venue"), Some(top), "a DELETE alone does not move the mark");

        rebuild_preserving_ids(&conn, "venue", carry);

        let after = mark(&conn, "venue");
        match carry {
            MarkCarry::Dropped => assert_eq!(
                after,
                Some(top - 1),
                "spec §4.1: a rebuild that replays the survivors and does nothing else must be \
                 seen to REWIND the mark to `max(id)`. If it did not, this engine no longer \
                 deletes a dropped table's `sqlite_sequence` row and the whole hazard this gate \
                 exists for has changed shape"
            ),
            MarkCarry::Carried => assert_eq!(
                after,
                Some(top),
                "carrying the mark across is the cure, and it did not take"
            ),
        }

        // …and what that costs, at the only place it is visible: the next row's id.
        conn.execute("INSERT INTO venue (name) VALUES ('a-venue-this-roster-does-not-name')", [])
            .expect("insert");
        let next = max_id(&conn, "venue").expect("a row");
        match carry {
            MarkCarry::Dropped => assert_eq!(
                next, top,
                "the freed id came back out — every `venue_id` written down against the old venue \
                 {top} now names a different one ({expectation})"
            ),
            MarkCarry::Carried => assert_eq!(
                next,
                top + 1,
                "the mark {expectation}, so the freed id must not be handed out again"
            ),
        }
    }
}

/// **The REAL reshape, driven through the call a binary makes, over a store that already carries an
/// armed table with a mark.**
///
/// `vike_secrets::migrate` is `crate::schema::reshape_into`'s production entry point: `fill_into`
/// runs the reshape for any store below the current schema, which is what
/// `vike_secrets::plant_schema_1` puts on disk here. That is the whole of this file's location
/// ruling — the private function is unreachable from `tests/`, the REBUILD is not.
///
/// Today it is GREEN because the reshape renames, re-creates and drops `credential` alone and
/// leaves `venue` untouched. It binds the day that stops being true — a reshape whose `DDL` batch
/// or scratch drop reaches an armed table, which is exactly what stage 4's rebuild of `account`
/// will be.
#[test]
fn the_real_reshape_does_not_rewind_an_armed_tables_mark() {
    assert!(
        armed_tables(DDL).contains("venue"),
        "this test's subject is the table the shipped schema arms; `SEQUENCE_PIN` is where that is \
         decided"
    );
    let fx = Fixture::file_store();
    let creds: Vec<(String, String)> =
        support::FIXTURE_KEYS.iter().map(|k| ((*k).to_string(), support::fake_value(k))).collect();
    std::fs::create_dir_all(fx.db().parent().expect("a db dir")).expect("db dir");
    vike_secrets::plant_schema_1(&fx.db(), &creds, &[])
        .expect("plant the shape both boxes were in");

    // The armed table, planted from the shipped statement, with its top row FREED — the only state
    // a rewind is visible in. A schema-1 store has no `venue` table of its own; this is the state a
    // store that has already been carried once is in, which is the state every FUTURE reshape runs
    // over.
    let top = {
        let conn = open(&fx);
        conn.execute_batch(&create_statement(DDL, "venue")).expect("plant the armed table");
        for name in ["a-venue", "b-venue", "c-venue"] {
            conn.execute("INSERT INTO venue (name) VALUES (?1)", [name]).expect("seed");
        }
        let top = max_id(&conn, "venue").expect("the fixture seeded rows");
        conn.execute("DELETE FROM venue WHERE id = ?1", [top]).expect("free the top id");
        assert_eq!(
            mark(&conn, "venue"),
            Some(top),
            "the precondition this test is worthless without: an armed table whose mark is ABOVE \
             its top surviving row"
        );
        top
    };

    vike_secrets::migrate(fx.arg(), support::is_node_key, &support::classify)
        .expect("the real reshape, through the entry point a binary calls");

    let conn = open(&fx);
    // Anti-vacuity: the reshape must actually have RUN. A migration that refused, or that decided
    // there was nothing to do, would leave the mark untouched for the wrong reason entirely.
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("version");
    assert_eq!(version, vike_secrets::SCHEMA_VERSION, "the store did not reach the new schema");
    assert!(
        !table_exists(&conn, "credential_schema1"),
        "the reshape's own scratch table survived, so the rebuild did not finish"
    );

    let after = mark(&conn, "venue").expect("an armed table with rows keeps a mark");
    assert!(
        after >= top,
        "a `sqlite_sequence` mark may only ever GROW, and the reshape moved `venue`'s DOWN (was \
         {top}, now {after}). Spec §4.1: a rebuild that replays surviving rows after the top one \
         was removed rewinds the mark and silently un-does the guarantee. Whatever the reshape now \
         does to `venue`, it must carry the mark across — `rebuild_preserving_ids` in this file is \
         the procedure."
    );
    let reused: i64 = conn
        .query_row("SELECT count(*) FROM venue WHERE id = ?1", [top], |r| r.get(0))
        .expect("count");
    assert_eq!(
        reused, 0,
        "the id freed before the migration was handed to one of the roster venues the migration \
         seeded — which is §7 item 4's failure, on a real production path"
    );
}

/// One table's `CREATE TABLE` text as the engine stored it — the only place the OLD tier
/// vocabulary can be observed, since `DDL` is what the new one is spelled in.
fn table_sql_of(conn: &Connection, table: &str) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_else(|e| panic!("reading `{table}`'s stored statement: {e}"))
}

/// **Re-create `table` from a MUTATED copy of its own shipped statement**, keeping every row, every
/// id and (where the mutation leaves the table armed) its `sqlite_sequence` mark.
///
/// This is how the fixtures below plant the shapes a REAL store is in before a repair has run. The
/// statement is the SHIPPED one put through `mutate`, so a plant is this store's own schema wearing
/// one difference rather than a second spelling of it — the same discipline
/// `crate::schema::create_statement_under` follows in production. `select_expr` rewrites one
/// column's value in the copy, for the plants whose mutated constraint would refuse the rows as
/// they stand.
///
/// The mark is restored only when the mutated statement still declares `AUTOINCREMENT`: an unarmed
/// table has no mark to hold, and writing one into `sqlite_sequence` for it would be a fact the
/// engine ignores and a reader would believe.
fn replant(
    conn: &Connection,
    table: &str,
    mutate: &dyn Fn(String) -> String,
    select_expr: &dyn Fn(&str) -> Option<String>,
) {
    let scratch = format!("{table}_replant_scratch");
    let head = format!("CREATE TABLE IF NOT EXISTS {table} ");
    let statement = mutate(create_statement(DDL, table).replacen(
        &head,
        &format!("CREATE TABLE {scratch} "),
        1,
    ));
    let stays_armed = statement.to_uppercase().contains("AUTOINCREMENT");

    let before = mark(conn, table);
    // ⚠ The SAME pair as [`rebuild_preserving_ids`], and its doc is the authority for which one
    // does what — but this function is the OTHER shape (it builds the scratch BESIDE and renames it
    // INTO place), so the two halves earn their place differently here. `foreign_keys = OFF` is
    // what lets `DROP TABLE {table}` below run at all: this is a bare `Connection`, which comes up
    // with foreign keys ON in this build, and the drop performs an implicit `DELETE FROM` that
    // fires every child row. `legacy_alter_table = ON` has nothing to suppress — the only rename
    // here is `{scratch} -> {table}` and no clause in this schema names the scratch — and it is
    // kept so that this file spells a rebuild's pragmas ONE way rather than two.
    conn.execute_batch("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON;")
        .expect("a rebuild suspends the constraints it is about to break");
    conn.execute_batch(&statement).expect("plant the mutated table beside the live one");
    let cols = columns(conn, table);
    let selected: Vec<String> =
        cols.iter().map(|c| select_expr(c).unwrap_or_else(|| c.clone())).collect();
    conn.execute_batch(&format!(
        "INSERT INTO {scratch} ({}) SELECT {} FROM {table};",
        cols.join(", "),
        selected.join(", ")
    ))
    .expect("replay the rows with their ids");
    conn.execute_batch(&format!("DROP TABLE {table};")).expect("drop the live table");
    conn.execute_batch(&format!("ALTER TABLE {scratch} RENAME TO {table};")).expect("rename it in");
    conn.execute_batch(DDL).expect("re-create the indexes the drop took");
    match (stays_armed, before) {
        (true, Some(seq)) => set_mark(conn, table, seq),
        _ => {
            conn.execute("DELETE FROM sqlite_sequence WHERE name = ?1", [table]).expect("no mark");
        }
    }
    conn.execute_batch("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON;")
        .expect("restore the constraints");
}

/// **Put `table` back onto the PRE-§4.4 tier vocabulary**, keeping its rows, its ids and its
/// `sqlite_sequence` mark — the state a store that has not been written since the rename is in,
/// and the ONE state `crate::schema::migrate_sim_tier_to_paper` fires on.
///
/// It keeps `AUTOINCREMENT`, which is the whole point: a fixture that planted an UNARMED table
/// would make the assertions that follow measure nothing.
fn plant_pre_paper_tier(conn: &Connection, table: &str) {
    replant(conn, table, &|sql| sql.replace("'paper'", "'sim'"), &|c| {
        (c == "tier").then(|| "CASE tier WHEN 'paper' THEN 'sim' ELSE tier END".to_string())
    });
    let sql = table_sql_of(conn, table);
    assert!(sql.contains("'sim'"), "the plant must produce the OLD vocabulary: {sql}");
    assert!(
        sql.to_uppercase().contains("AUTOINCREMENT"),
        "…and must keep the table ARMED — a mark is the whole subject here: {sql}"
    );
}

/// **Put `table` back into the PRE-§4.1 shape: no `AUTOINCREMENT`**, keeping its rows and ids.
///
/// This is the state BOTH LIVE BOXES are in, and the one a fresh store can never be in — every
/// store born from `DDL` is armed at birth, so a test that only ever sees a fresh store proves
/// nothing about the migration.
fn plant_unarmed(conn: &Connection, table: &str) {
    replant(
        conn,
        table,
        &|sql| sql.replace("INTEGER PRIMARY KEY AUTOINCREMENT", "INTEGER PRIMARY KEY"),
        &|_| None,
    );
    let sql = table_sql_of(conn, table);
    assert!(
        !sql.to_uppercase().contains("AUTOINCREMENT"),
        "the plant must actually have DISARMED the table, or the repair under test has nothing to \
         do and its assertions are vacuous: {sql}"
    );
    assert_eq!(mark(conn, table), None, "…and an unarmed table holds no mark");
}

/// **The behaviour assertion [`TABLE_DROP_PIN`]'s `schema.rs` row used to declare as OWED** — and
/// it is owed no longer, because stage 4 armed the very tables §4.4's rebuild touches.
///
/// `crate::schema::migrate_sim_tier_to_paper` reads `account`'s `sqlite_sequence` mark before the
/// rebuild and puts it back afterwards. While no table declared `AUTOINCREMENT` that read answered
/// `None` and the carry was a NO-OP: no test could fail for its absence, which the pin row said in
/// its own words. Now it can, and this is the test that does — driven through
/// `AccountEdit::Create`, the verb an operator runs, so what is measured is the production repair
/// path rather than a model of it.
///
/// Delete the two `sqlite_sequence` statements at the end of
/// `crate::schema::rebuild_table_from_ddl` and this goes red naming a REUSED id.
#[test]
fn the_paper_tier_rebuild_does_not_rewind_the_account_marks() {
    assert!(
        armed_tables(DDL).contains("account"),
        "this test's whole subject is an ARMED `account`; `SEQUENCE_PIN` is where that is decided, \
         and while the table was `Owed` there was no mark for a rebuild to rewind"
    );

    let fx = Fixture::migrated();
    let first = create(&fx, "binance", "live", "ONE");
    let second = create(&fx, "binance", "live", "TWO");
    let top = create(&fx, "binance", "live", "THREE");
    assert!(first < second && second < top, "the fixture needs the third row to hold the top id");
    fx.edit(AccountEdit::Remove { id: top }).expect("a keyless row removes");

    {
        let conn = open(&fx);
        assert_eq!(max_id(&conn, "account"), Some(second), "the top id is now free");
        assert_eq!(
            mark(&conn, "account"),
            Some(top),
            "…and the mark is still ABOVE the top surviving row, which is the ONLY state a rewind \
             is visible in"
        );

        plant_pre_paper_tier(&conn, "account");

        // The preconditions, asserted AFTER the plant rather than assumed to have survived it.
        let sql = table_sql_of(&conn, "account");
        assert!(
            sql.contains("'sim'"),
            "the plant must leave a table `migrate_sim_tier_to_paper` will FIRE on: {sql}"
        );
        assert_eq!(
            mark(&conn, "account"),
            Some(top),
            "the plant must keep the mark above the top surviving row"
        );
        assert_eq!(
            max_id(&conn, "account"),
            Some(second),
            "…and must not have resurrected the removed row"
        );
    }

    // THE REAL REPAIR PATH: `edit_account` runs `ensure_venue_id_columns`, which is where
    // `migrate_sim_tier_to_paper` lives, before it inserts anything.
    let reborn = create(&fx, "okx", "demo", "FOUR");

    let conn = open(&fx);
    let sql = table_sql_of(&conn, "account");
    assert!(
        sql.contains("'paper'") && !sql.contains("'sim'"),
        "anti-vacuity: the rebuild must actually have RUN. If it did not, nothing below is a \
         statement about a rebuild at all — it is a statement about an untouched table: {sql}"
    );
    assert_eq!(
        reborn,
        top + 1,
        "§4.1: the rebuild replayed the SURVIVING rows with their ids, which sets the mark to \
         `max(id)` = {second} — a REWIND, because the row holding {top} had been removed first. \
         Carrying the old mark across is the whole cure and it is two statements at the end of \
         `crate::schema::rebuild_table_from_ddl`. Without them this account is handed {top}, the \
         number the removed one held, and every note, runbook and wire client that remembered it \
         now names a different account"
    );
    assert_ne!(
        reborn, top,
        "the id freed before the rebuild came back out — see the assertion above for the cure"
    );
    assert_eq!(
        mark(&conn, "account"),
        Some(top + 1),
        "…and the mark moved UP by exactly the row that was inserted, never down"
    );
}

/// **§4.1 reaches an EXISTING store, which is the only claim that is about the live boxes.**
///
/// ⚠ `crate::schema::DDL` is `CREATE TABLE IF NOT EXISTS` throughout, so the batch applies NOTHING
/// to a table that is already there: every store born after stage 4 is armed at birth, and a test
/// that creates a fresh store proves nothing whatever about the CI box or the dev box. Both of those
/// hold an `account` table written before §4.1, and the REBUILD is what arms it.
///
/// So this plants that shape — the shipped table with `AUTOINCREMENT` taken back out, rows and ids
/// intact — and then does the two things an operator does, in order: remove an account, create
/// another. It asserts all three properties the migration owes:
///
/// * the repair RAN (the stored statement declares `AUTOINCREMENT` afterwards);
/// * every surviving id is UNCHANGED, because `credential.account_id` and an operator's own notes
///   both name rows by that number;
/// * and the id freed by the removal is not handed out again — §2.4's defect, closed.
#[test]
fn an_existing_unarmed_account_table_is_armed_by_the_next_write_and_keeps_its_ids() {
    let fx = Fixture::migrated();
    let first = create(&fx, "binance", "live", "ONE");
    let second = create(&fx, "binance", "live", "TWO");
    let top = create(&fx, "binance", "live", "THREE");
    assert!(first < second && second < top, "the fixture needs the third row to hold the top id");

    {
        let conn = open(&fx);
        plant_unarmed(&conn, "account");
        assert_eq!(
            max_id(&conn, "account"),
            Some(top),
            "the plant must keep the rows it found — it is a disarming, not a truncation"
        );
    }

    // The first write an operator performs on such a store. The repair runs inside it, BEFORE the
    // removal, which is why the freed id is already protected by the time the row goes.
    fx.edit(AccountEdit::Remove { id: top }).expect("a keyless row removes");

    let conn = open(&fx);
    let sql = table_sql_of(&conn, "account");
    assert!(
        sql.to_uppercase().contains("AUTOINCREMENT"),
        "the repair must have run on the EXISTING table. `DDL` alone cannot do this — \
         `CREATE TABLE IF NOT EXISTS` changes nothing about a table that is already there, which \
         is exactly the state both live boxes are in: {sql}"
    );
    let survivors: BTreeSet<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM account").expect("prepare");
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).expect("query");
        rows.map(|r| r.expect("row")).collect()
    };
    assert!(
        survivors.contains(&first) && survivors.contains(&second) && !survivors.contains(&top),
        "the arming rebuild must replay every surviving row with its OWN id — a renumbering here \
         would re-point `credential.account_id` and every book an operator wrote down: {survivors:?}"
    );
    assert_eq!(
        mark(&conn, "account"),
        Some(top),
        "…and the mark it was given is `max(id)` of what it copied, which on THIS migration is \
         correct precisely because no row had been removed when it ran (spec §4.1)"
    );
    drop(conn);

    let reborn = create(&fx, "okx", "demo", "FOUR");
    assert_eq!(
        reborn,
        top + 1,
        "§2.4, closed: the id the removed account held must not come back. Before stage 4 this \
         was {top} — `remove account 16, add an account, and the new one IS account 16`"
    );
}

/// Every armed table's id set, for the before/after comparison
/// [`every_armed_table_planted_unarmed_at_once_is_repaired_by_one_write`] rests on.
fn ids_by_table(conn: &Connection, tables: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<i64>> {
    tables
        .iter()
        .map(|table| {
            let mut stmt =
                conn.prepare(&format!("SELECT id FROM {table}")).expect("every armed table has id");
            let ids: BTreeSet<i64> = stmt
                .query_map([], |r| r.get::<_, i64>(0))
                .expect("query")
                .map(|r| r.expect("row"))
                .collect();
            (table.clone(), ids)
        })
        .collect()
}

/// **EVERY armed table planted unarmed AT ONCE — the shape both live boxes are actually in.**
///
/// The tests above disarm ONE table. A store written before §4.1 has none of them armed, and
/// `crate::schema::migrate_tables_onto_autoincrement` rebuilds them in a LOOP, inside ONE
/// transaction, over tables that reference each other (`account.venue_id`, `credential.account_id`,
/// `credential.venue_id`, `venue_setting.venue_id`). The failures that needs are the ones a
/// single-table plant cannot produce: a rebuild dropping a parent another rebuild is part-way
/// through, a `REFERENCES` clause left naming a scratch table, an id renumbered because the loop
/// reached the child first. So this asks the whole-database question at the end —
/// `pragma_foreign_key_check` over everything, not over the statements just run.
///
/// The roster is DERIVED from the shipped `DDL` ([`armed_tables`]), never listed here, so a table
/// armed next month joins this test by existing rather than by somebody remembering.
///
/// ⚠ The rows in the tables the fixture leaves empty are planted DELIBERATELY. Without them
/// "every id survived" is a claim about the three tables `Fixture::migrated` happens to fill and a
/// vacuous truth about the rest — so the anti-vacuity loop below refuses an empty one BY NAME.
#[test]
fn every_armed_table_planted_unarmed_at_once_is_repaired_by_one_write() {
    let fx = Fixture::migrated();
    let first = create(&fx, "binance", "live", "PLANTONE");
    let second = create(&fx, "okx", "demo", "PLANTTWO");
    assert!(first < second, "the fixture needs two accounts with distinct ids");

    let armed = armed_tables(DDL);
    assert!(
        armed.len() > 1,
        "this test is about SEVERAL tables being repaired in one pass; the shipped schema arms \
         {} — if that is ever one, `SEQUENCE_PIN` is where it was decided",
        armed.len()
    );

    {
        let conn = open(&fx);
        conn.execute_batch(
            "INSERT INTO node_key (name, value) VALUES ('vike_node_unarmed_probe', 'v');
             INSERT INTO setting (section, key, value) VALUES ('flags', 'unarmed_probe', '1');
             INSERT INTO profile_risk (profile, key, value) VALUES ('probe', 'max_qty', '2');
             INSERT INTO venue_setting (venue, venue_id, tier, field, value)
                 SELECT 'binance', id, 'demo', 'unarmed_probe', 'v'
                 FROM venue WHERE name = 'binance';",
        )
        .expect("seed the armed tables the migration leaves empty");
    }

    let before = ids_by_table(&open(&fx), &armed);
    for (table, ids) in &before {
        assert!(
            !ids.is_empty(),
            "`{table}` is armed and EMPTY, so the id-preservation assertion below says nothing \
             about it. Give this fixture a row in it rather than letting the table drop quietly \
             out of what this test covers"
        );
    }

    {
        let conn = open(&fx);
        for table in &armed {
            plant_unarmed(&conn, table);
        }
        // The premise, asserted rather than assumed: a plant that silently left a table armed
        // would make every assertion below a statement about a store that needed no repair.
        for table in &armed {
            let sql = table_sql_of(&conn, table);
            assert!(
                !sql.to_uppercase().contains("AUTOINCREMENT"),
                "`{table}` survived the plant still armed: {sql}"
            );
        }
    }

    // ONE production write, through the verb an operator runs. The repair rides inside it.
    let reborn = create(&fx, "bybit", "demo", "PLANTTHREE");

    let conn = open(&fx);
    for table in &armed {
        let sql = table_sql_of(&conn, table);
        assert!(
            sql.to_uppercase().contains("AUTOINCREMENT"),
            "`{table}` was left UNARMED by the repair. One write must arm every table the shipped \
             `DDL` arms, not the first one the loop reaches — a half-repaired store hands the next \
             operator a reused id in whichever table was missed: {sql}"
        );
    }

    let after = ids_by_table(&conn, &armed);
    for (table, ids) in &before {
        let now = after.get(table).expect("the same roster on both sides");
        let lost: Vec<i64> = ids.difference(now).copied().collect();
        assert!(
            lost.is_empty(),
            "the rebuild of `{table}` did not replay {lost:?} with their own ids. Every one of \
             these numbers is an ADDRESS — `credential.account_id`, `venue_setting.venue_id` and \
             an operator's own notes all name rows by it"
        );
    }
    assert!(after["account"].contains(&reborn), "the new account is in the rebuilt table");
    assert_eq!(
        after["account"].len(),
        before["account"].len() + 1,
        "…and it is the ONLY row the write added: a rebuild that resurrected a row would pass the \
         loop above, which only asks that nothing was lost"
    );

    let violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
        .expect("the whole-database foreign key check");
    assert_eq!(
        violations, 0,
        "repairing every armed table in one transaction left dangling references. This is the \
         question a single-table plant cannot ask: each rebuild renames a scratch table into \
         place, and a clause rewritten to follow one of those renames survives the scratch's own \
         DROP pointing at nothing"
    );
}

// -------------------------------------------------------------------------------------------
// The gate — the source ratchet
// -------------------------------------------------------------------------------------------

/// The statement that deletes a `sqlite_sequence` row, COMPOSED rather than spelled.
///
/// This file's scan reads `crates/vike-secrets/src/`, not itself, so composing the needle is not
/// strictly required here — it is kept because the needle is the ONE string in this file that must
/// never accidentally become a match, and because the scan's root is a variable somebody may widen.
const DROPS_TABLE: &str = concat!("DROP ", "TABLE ");

/// **Every place this crate's `src/` drops a table**, as `(path under `src/`, the enclosing `fn`,
/// the table token, why it cannot lose a mark)`.
///
/// ⚠ **The declared length is the COUNT** — no prose anywhere may restate it.
///
/// A table drop is the statement §4.1 names, and a rebuild cannot be written without one.
///
/// ⚠ **This used to add "no production path rebuilds an armed table today, so there is no
/// behaviour to assert", and stage 4a made that false** — the module doc was swept for it and this
/// pin was not, which is the worse of the two places to leave it: the gate's own failure message
/// sends the next author HERE. `crates/vike-secrets/src/schema.rs`'s `rebuild_table_from_ddl` IS
/// that production path, it rebuilds tables the shipped `DDL` arms, and
/// [`the_paper_tier_rebuild_does_not_rewind_the_account_marks`] is the behaviour assertion that
/// reddens when its mark carry is deleted — measured by deleting it. So the duty this pin records
/// is now BOTH: classify the next drop, and make the rebuild around it carry the mark.
///
/// ⚠ **The `fn` column is part of the KEY, not decoration.** The pin was keyed on `(file, token)`
/// alone, and `db.rs` grew a SECOND `DROP TABLE credential_old;` in a different test — two sites
/// collapsing onto one row, which is the exact hole the `IF EXISTS` and `;` handling in
/// [`drops_in`] exists to close, re-opened one column further out.
/// [`the_drop_scan_distinguishes_two_identical_drops_in_one_file`] holds the distinction on planted
/// source rather than on whatever `db.rs` happens to contain.
///
/// ⚠ **It has already caught one, on the day it landed.** The `sim` -> `paper` rebuild
/// (`crates/vike-secrets/src/schema.rs`'s `migrate_sim_tier_to_paper`) arrived on the same branch
/// hours later, reddened this test, and its author read the message and made that rebuild carry the
/// mark. This pin was deliberately NOT written in advance for it: pre-blessing a site whose final
/// form nobody had read is the one thing a ratchet must not do.
///
/// # ⚠ Declared blind spots
///
/// [`table_drops`] keys on the LITERAL statement, so a drop assembled out of pieces — a `const` for
/// the verb, a `format!` that splices it — escapes the scan entirely. And the scan reads `src/`
/// ONLY: [`rebuild_preserving_ids`] in this file drops a table and is deliberately out of scope,
/// because a harness is not production. Both are measured rather than assumed away: this ratchet is
/// a classification duty over ordinary source, not a proof that no drop exists.
///
/// A THIRD: the `fn` column is the innermost `fn` DECLARED above the line, so two drops inside one
/// function collapse onto one row **when their TOKENS match as well**. That is narrower than the
/// hole it closes — the hole was two drops in DIFFERENT functions sharing a row, which the `fn`
/// column now separates.
///
/// ⚠ **Read that against the rows below rather than against a summary of them.** This sentence
/// used to end *"and is the shape the rows below are actually in — one drop per function, every one
/// of them"*, which the pin's own contents contradict: `rebuild_table_from_ddl` holds TWO drops and
/// has TWO rows, and the `{scratch};` row says so in its own words. What actually keeps those two
/// apart is the TOKEN — the original key doing its job — and the blind spot is therefore the pair a
/// function holds whose tokens are identical. `db.rs`'s two `credential_old;` drops were exactly
/// that pair one column further out, and they are separated here by the `fn` column instead.
const TABLE_DROP_PIN: [(&str, &str, &str, &str); 7] = [
    (
        "db.rs",
        "ensure_venue_rows_creates_the_table_on_a_store_that_predates_it",
        "venue;",
        "simulating a store older than the table — a fixture, and the mark it destroys is the \
         fixture's own",
    ),
    (
        "db.rs",
        "ensure_venue_id_columns_skips_a_credential_table_with_no_venue_column",
        "credential_old;",
        "rebuilding `credential` by rename-copy-drop to plant a table with no `venue` column. ⚠ \
         This row used to argue *`credential` is not armed, so there is no mark for the drop to \
         take*, and `2f677beaa` made that false — the shipped `DDL` declares `AUTOINCREMENT` on \
         `credential`. What holds now is the fixture argument: the plant is built on a store this \
         test created moments earlier, the copy carries every id explicitly, and no row has been \
         removed — so the mark the drop takes is the fixture's own and the replay leaves it where \
         it was",
    ),
    (
        "db.rs",
        "the_autoincrement_rebuild_normalizes_a_credential_table_that_lost_its_venue_columns",
        "credential_old;",
        "THE SECOND SITE, and it had no row of its own until the `fn` column existed — it planted \
         itself behind the row above and this ratchet counted one where there were two. It plants \
         the same shape UNARMED (`INTEGER PRIMARY KEY`, no `AUTOINCREMENT`), so the table the drop \
         leaves behind holds no mark at all, and the store is this test's own",
    ),
    (
        "profile_store.rs",
        "an_unrenderable_vocabulary_is_refused",
        "mount;",
        "not a statement at all: a SQL-injection probe STRING in `profile_ddl`'s vocabulary test, \
         which the `CHECK` list quotes rather than executes",
    ),
    (
        "schema.rs",
        "rebuild_table_from_ddl",
        "{scratch};",
        "the `DROP TABLE IF EXISTS` that clears a LEFTOVER scratch table before building the new \
         shape under that name. It takes no mark that matters: a scratch table only ever exists \
         inside a rebuild that did not finish, so its `sqlite_sequence` row is a copy of the live \
         table's made moments earlier and the live table still holds its own. ⚠ This row is also \
         the second site in one file the `fn` key is NOT needed for — it and the `{table};` row \
         below share a function and are told apart by the token, which is the original key doing \
         its job",
    ),
    (
        "schema.rs",
        "rebuild_table_from_ddl",
        "{table};",
        "the drop of the LIVE table — the ONE rebuild procedure in this \
         crate, called by `migrate_sim_tier_to_paper` (§4.4) and by \
         `migrate_tables_onto_autoincrement` (§4.1). It builds the new table beside the old and \
         drops the original rather than renaming it aside, so this statement takes the real \
         table's `sqlite_sequence` row, and it CARRIES the mark: read before the rebuild, put back \
         after it — the `MarkCarry::Carried` shape spelled in production. ⚠ THE DECLARED RESIDUAL \
         THAT STOOD HERE IS PAID. It read: *neither table it touches is armed today, so that carry \
         is a no-op and NO test can fail without it*. Stage 4 armed five more tables including \
         both of §4.4's, and `the_paper_tier_rebuild_does_not_rewind_the_account_marks` is the \
         behaviour assertion that now fails when the carry is deleted — measured by deleting it",
    ),
    (
        "schema.rs",
        "reshape_into",
        "{RESHAPE_SCRATCH};",
        "the drop of the renamed SCHEMA-1 `credential`. ⚠ Read the qualifier: it is the schema-1 \
         shape (`name TEXT PRIMARY KEY NOT NULL`, `crates/vike-secrets/src/db.rs`'s `SCHEMA_1`) \
         that declares no `AUTOINCREMENT` and therefore hands the drop no mark — the SHIPPED \
         `credential` has been armed since `2f677beaa`, and a row that said only *`credential` \
         declares no `AUTOINCREMENT`* would now read as a claim about the wrong table. \
         `the_real_reshape_does_not_rewind_an_armed_tables_mark` holds it measured rather than \
         argued",
    ),
];

/// Every `.rs` file under this crate's `src/`, as a path relative to it.
///
/// ⚠ `CARGO_MANIFEST_DIR` is the tree the COMPILER ran in, which is the defect
/// `crates/vike-ops/tests/compile_time_path_gate.rs` exists to refuse — in `src/`. Here it is
/// correct and is the ordinary shape for a gate that reads the repository: a test binary is only
/// ever run from the tree it was built in.
fn source_files() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("this crate's own src/ is readable") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(&root)
                    .expect("under src/")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path).expect("readable")));
            }
        }
    }
    out.sort();
    assert!(!out.is_empty(), "the walk found no source at all, so the scan below is vacuous");
    out
}

/// The name of the `fn` a line DECLARES, if it declares one — the scan's third key column.
///
/// Deliberately a declaration scan rather than a brace-depth parser: this reads Rust source as
/// TEXT, like every other gate in this tree, and the innermost `fn` declared above a line is the
/// answer wanted at every site the pin carries. Visibility and the `const`/`async`/`unsafe`
/// qualifiers are stripped so `pub(crate) async fn foo` answers `foo`.
fn declared_fn(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    loop {
        let stripped = ["pub(crate)", "pub(super)", "pub", "default", "const", "async", "unsafe"]
            .iter()
            .find_map(|kw| rest.strip_prefix(kw)?.strip_prefix(' '));
        match stripped {
            Some(next) => rest = next.trim_start(),
            None => break,
        }
    }
    let name: String = rest
        .strip_prefix("fn ")?
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// `(enclosing fn, table token)` for every table drop in ONE source text.
///
/// Split out from [`table_drops`] so the distinction it draws can be measured on PLANTED source
/// rather than on whatever `db.rs` happens to hold this week — see
/// [`the_drop_scan_distinguishes_two_identical_drops_in_one_file`].
///
/// Lines whose first non-whitespace bytes are `//` are skipped: a comment drops nothing, and this
/// crate's prose names the statement repeatedly. That skip runs BEFORE the `fn` detection, so a
/// `fn` named inside a doc comment cannot re-key the sites below it.
fn drops_in(text: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    // ⚠ The sentinel for a drop above the first `fn`. It is a spelling no `fn` can have, so a row
    // carrying it is visibly "at file scope" rather than silently attributed to nothing.
    let mut current = "<file scope>".to_string();
    for line in text.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        if let Some(name) = declared_fn(line) {
            current = name;
        }
        for (idx, _) in line.match_indices(DROPS_TABLE) {
            // ⚠ `IF EXISTS` is stepped over rather than read as the table. Without this the
            // token recorded for `DROP TABLE IF EXISTS venue` is the word `IF`, so two such
            // sites in one file collapse onto ONE pin row and the second is invisible — the
            // exact shape of hole this ratchet exists to close. Measured while killing this
            // test with a planted drop.
            let rest = line[idx + DROPS_TABLE.len()..].trim_start();
            let rest = rest.strip_prefix("IF EXISTS ").unwrap_or(rest);
            // ⚠ The token ENDS AT ITS OWN `;`, and that is not cosmetic. A drop inside a
            // multi-statement `format!` has no whitespace after the semicolon — the next byte
            // is the `\n` ESCAPE, two characters of source — so a scan that stopped only at
            // whitespace recorded `{table};\nALTER` and the pin row for the same site changed
            // its spelling the moment its author put a second statement on the line. Measured
            // on `migrate_sim_tier_to_paper`, twice, while this gate was being written.
            let mut token = String::new();
            for c in rest.chars() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                token.push(c);
                if c == ';' {
                    break;
                }
            }
            if !token.is_empty() {
                out.insert((current.clone(), token));
            }
        }
    }
    out
}

/// `(file, enclosing fn, table token)` for every table drop this crate's `src/` performs.
fn table_drops() -> BTreeSet<(String, String, String)> {
    source_files()
        .into_iter()
        .flat_map(|(file, text)| {
            drops_in(&text).into_iter().map(move |(func, token)| (file.clone(), func, token))
        })
        .collect()
}

/// **The `fn` column is a real distinction, held on PLANTED source.**
///
/// The pin was keyed on `(file, token)` and `db.rs` then grew a second `DROP TABLE
/// credential_old;` in a different test, which collapsed onto the first row and was classified by
/// nobody. This asserts the scan now tells the two apart — and deliberately does NOT read `db.rs`
/// to do it, because a guard that rests on today's source stops guarding the moment somebody
/// rewrites that file.
#[test]
fn the_drop_scan_distinguishes_two_identical_drops_in_one_file() {
    // ⚠ The statement is COMPOSED from [`DROPS_TABLE`], not spelled — same reason that constant is
    // composed. This file must stay free of the literal needle so that widening the scan's root to
    // `tests/` some day cannot make this fixture answer as a real drop.
    let planted = format!(
        "fn first_test() {{\n    \
             conn.execute_batch(\"{DROPS_TABLE}credential_old;\").unwrap();\n\
         }}\n\
         \n\
         /// fn a_doc_comment_naming_a_fn_must_not_re_key_anything() {{\n\
         // conn.execute_batch(\"{DROPS_TABLE}a_commented_out_drop;\");\n\
         pub(crate) async fn second_test() {{\n    \
             conn.execute_batch(\"{DROPS_TABLE}IF EXISTS credential_old;\").unwrap();\n\
         }}\n"
    );
    let observed = drops_in(&planted);
    assert_eq!(
        observed,
        BTreeSet::from([
            ("first_test".to_string(), "credential_old;".to_string()),
            ("second_test".to_string(), "credential_old;".to_string()),
        ]),
        "two identical drops in two functions must be TWO rows — one row here is the hole this \
         column exists to close, and a row naming `a_doc_comment_naming_a_fn_must_not_re_key_\
         anything` or `a_commented_out_drop` means the comment skip stopped running first"
    );
}

#[test]
fn every_table_drop_in_this_crates_source_is_classified() {
    let pinned: BTreeSet<(String, String, String)> = TABLE_DROP_PIN
        .iter()
        .map(|(file, func, token, _)| {
            ((*file).to_string(), (*func).to_string(), (*token).to_string())
        })
        .collect();
    let observed = table_drops();
    let added: Vec<String> = observed
        .difference(&pinned)
        .map(|(f, func, t)| format!("  {f}: `{func}` drops `{t}`"))
        .collect();
    assert!(
        added.is_empty(),
        "\nNEW table drop(s) in this crate's source, and a table drop is what deletes a \
         `sqlite_sequence` high-water mark (spec §4.1):\n\n{}\n\nIf this is part of a REBUILD of a \
         table `SEQUENCE_PIN` calls `Armed`, the rebuild must carry that mark across or every id \
         above the top SURVIVING row is handed out a second time. The PRODUCTION procedure is \
         `crates/vike-secrets/src/schema.rs`'s `rebuild_table_from_ddl` — copy that one. \
         `rebuild_preserving_ids` in this file measures the mark carry and `MarkCarry` is the one \
         decision in it, but it is the rename-ASIDE shape and its pragma pair does NOT transfer: \
         read its doc before lifting anything out of it. Then add the row here with the argument \
         for why this drop cannot lose a mark.\n\npinned {}, observed {}\n",
        added.join("\n"),
        TABLE_DROP_PIN.len(),
        observed.len(),
    );
}

#[test]
fn the_table_drop_pin_has_no_stale_rows() {
    let observed = table_drops();
    let gone: Vec<String> = TABLE_DROP_PIN
        .iter()
        .filter(|(file, func, token, _)| {
            !observed.contains(&((*file).to_string(), (*func).to_string(), (*token).to_string()))
        })
        .map(|(file, func, token, _)| format!("  {file}: `{func}` drops `{token}`"))
        .collect();
    assert!(
        gone.is_empty(),
        "\n`TABLE_DROP_PIN` names drops this crate's source no longer performs:\n\n{}\n\nDelete \
         those lines and decrement the declared array length. ⚠ A file RENAME lands here too — \
         these rows are keyed by path, so re-key the row rather than deleting it. ⚠ So does a \
         function RENAME, for the same reason: the second key column is the enclosing `fn`, and a \
         row whose drop merely moved house is a re-key and not a deletion.\n",
        gone.join("\n"),
    );
}
