//! End-to-end tests for `vike-cli config migrate-store`, driving the SHIPPED binary
//! (`CARGO_BIN_EXE_vike-cli`) against a settings directory in a temp dir.
//!
//! The verb is the operator's one DELIBERATE door onto the settings store's pending migrations.
//! What these cover is the venue links' move onto `venue_id` (`vike_secrets::venue_links`): it
//! rides the write funnel, so without this verb it happens at whichever write a store meets first —
//! days later, perhaps, and at a moment nobody chose — and on the CI box the daemon cannot write
//! `settings/` at all. So the verb carries a store on purpose, says it did, says when there was
//! nothing to carry without opening the store for writing, and refuses a store only a human can
//! repair the way every writer does.
//!
//! ⚠ Every invocation sets `VIKE_SETTINGS_DIR` to the case's own temp directory on the CHILD, with
//! an otherwise EMPTY environment — this verb WRITES, so the redirect is what keeps a test out of a
//! real settings database. The store is planted with
//! `vike_secrets::venue_links::plant_pre_venue_link_store`: the shape the release before the move
//! wrote, decision 0095's marker present, as on the CI box.
//!
//! ⚠ **…or with `vike_secrets::venue_links::plant_release_1_store`**, the shape the move's FIRST
//! release carried a store onto — which is what the CI box and the dev box hold — because since the
//! plan's second release the verb also CONTRACTS a store: it drops the text `venue` column the first
//! release left beside the number, on purpose, and refuses a store the drop refuses.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use vike_secrets::live_means_mainnet::{live_means_mainnet_pending, unmark_live_means_mainnet};
use vike_secrets::venue_links::{
    LinkSnapshot, link_snapshot, plant_pre_venue_link_store, plant_release_1_store,
};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// Rows in all three linked tables, each with its number, on the release-before shape — plus an
/// `account` row planted and then DELETED, so `account`'s `AUTOINCREMENT` mark sits ABOVE every
/// surviving id: the state in which a rebuild that replayed the survivors without carrying the mark
/// would REWIND it, and hand the removed account's id to the next one.
const ROWS: &str = "\
INSERT INTO account (id, venue, venue_id, tier)
    VALUES (1, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'demo');
INSERT INTO account (id, venue, venue_id, tier)
    VALUES (2, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'live');
INSERT INTO account (id, venue, venue_id, tier)
    VALUES (3, 'okx', (SELECT id FROM venue WHERE name = 'okx'), 'demo');
DELETE FROM account WHERE id = 3;
INSERT INTO venue_setting (venue, venue_id, tier, field, value)
    VALUES ('polymarket', (SELECT id FROM venue WHERE name = 'polymarket'), 'any',
            'PROXY_ENABLED', 'false');
INSERT INTO venue_arming (venue, venue_id, label, mode)
    VALUES ('binance', (SELECT id FROM venue WHERE name = 'binance'), NULL, 'demo');
";

/// Two `credential` rows for the first-release shape, which a contraction REBUILDS: one filed
/// against account 1, one venue-scoped with its venue by text AND number. The names are not
/// env-shaped on purpose (this file is in a crate the settings registry sweeps for env names).
const CREDENTIAL_ROWS: &str = "\
INSERT INTO credential (id, account_id, venue, venue_id, field, value, name)
    VALUES (1, 1, NULL, NULL, 'API_KEY', 'an-account-value', 'binance-demo-key');
INSERT INTO credential (id, account_id, venue, venue_id, field, value, name)
    VALUES (2, NULL, 'ctrader', (SELECT id FROM venue WHERE name = 'ctrader'), 'CLIENT_ID',
            'a-venue-value', 'ctrader-client-id');
";

/// A venue-level `live` ceiling on okx, one of decision 0095's four switched venues: on a store with
/// 0095's marker removed it makes that migration PENDING, so its transaction runs the store's whole
/// write path.
const LIVE_CEILING: &str = "\
INSERT INTO venue_arming (venue, venue_id, label, mode)
    VALUES ('okx', (SELECT id FROM venue WHERE name = 'okx'), NULL, 'live');
";

/// A throwaway project and its `settings/` child, holding a planted store.
struct Case {
    /// Held only for its `Drop`.
    _root: tempfile::TempDir,
    dir: PathBuf,
}

impl Case {
    fn planted(rows: &str) -> Self {
        Case::planted_with(rows, plant_pre_venue_link_store)
    }

    /// The same, on the shape the move's FIRST release carried a store onto.
    fn planted_on_release_1(rows: &str) -> Self {
        Case::planted_with(rows, plant_release_1_store)
    }

    fn planted_with(rows: &str, plant: fn(&std::path::Path, &str)) -> Self {
        let root =
            tempfile::Builder::new().prefix("vike-cli-migrate-store-").tempdir().expect("tempdir");
        let dir = root.path().join("settings");
        std::fs::create_dir_all(&dir).expect("settings dir");
        plant(&dir, rows);
        Case { _root: root, dir }
    }

    fn db(&self) -> PathBuf {
        vike_secrets::db_path_in(&self.dir)
    }

    /// `vike-cli config migrate-store`, with the case's directory as THE settings directory.
    fn run(&self) -> Output {
        Command::new(BIN)
            .args(["config", "migrate-store"])
            .env_clear()
            .env("VIKE_SETTINGS_DIR", &self.dir)
            .stdin(Stdio::null())
            .output()
            .expect("the vike-cli binary must run")
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// `(table, id)` of every row a snapshot holds — the identity a carry may not move.
fn ids(s: &LinkSnapshot) -> Vec<(String, i64)> {
    s.rows.iter().map(|(table, id, _)| (table.clone(), *id)).collect()
}

/// Every mark but `venue`'s, which the write funnel's roster top-up advances on EVERY write by
/// design (`crate::db`'s `ensure_venue_rows` names the climb) and which no rebuild touches.
///
/// ⚠ **A mark of `0` is dropped as well, because it is the same state as no mark at all.** It is an
/// `AUTOINCREMENT` table that has never handed out an id: SQLite gives the next row
/// `max(mark, largest rowid) + 1` either way, so nothing can be reused and nothing moved. A carry
/// writes one since the venue-links plan's second release: the drop pass rebuilds this fixture's
/// EMPTY `credential` (the release-before shape still carries its text `venue`), and replaying
/// zero rows into the new table leaves its `sqlite_sequence` row at `0` where there was none.
/// MEASURED: `[("account", 3), ("credential", 0), ("venue_setting", 1)]` after, against
/// `[("account", 3), ("venue_setting", 1)]` before. A rebuild that REWOUND a mark is still caught,
/// to `0` included: the mark it had before is in the before list and missing from the after one.
fn marks_but_the_roster(s: &LinkSnapshot) -> Vec<(String, i64)> {
    s.marks.iter().filter(|(table, seq)| table != "venue" && *seq != 0).cloned().collect()
}

/// **The verb CARRIES a store on purpose** — the move the next write would otherwise make at a
/// moment nobody chose — and moves no id and no mark while it does.
#[test]
fn a_store_on_the_old_shape_is_carried_on_purpose_and_keeps_every_id_and_mark() {
    let c = Case::planted(ROWS);
    let before = link_snapshot(&c.dir);
    assert_eq!(
        before.nullable,
        ["account", "venue_setting", "venue_arming"],
        "premise: the planted store is on the release-before shape"
    );
    assert!(
        before.marks.iter().any(|(table, seq)| table == "account" && *seq == 3),
        "premise: `account`'s mark sits above its largest surviving id: {:?}",
        before.marks
    );

    let out = c.run();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains("venue links: carried onto venue_id"), "it says what it did: {said}");
    // …and, in the same run, the second release's contraction: a store this old is carried AND
    // contracted by one transaction, so it says both.
    assert!(
        said.contains(
            "venue links: text venue column dropped from account, credential, venue_setting"
        ),
        "…both steps it took: {said}"
    );
    assert!(
        said.contains("decision 0095's ceiling migration: not pending"),
        "…and says only what is true of 0095, which had nothing to do: {said}"
    );
    assert!(
        !said.contains("current"),
        "…and does not call a store it just changed current: {said}"
    );

    let after = link_snapshot(&c.dir);
    assert!(after.nullable.is_empty(), "every linked table refuses NULL now: {after:?}");
    assert_eq!(after.text_venue, ["venue_arming"], "only `venue_arming` keeps its text column");
    assert!(
        after.rows.iter().all(|(_, _, venue_id)| venue_id.is_some()),
        "every row carries its number: {after:?}"
    );
    assert_eq!(ids(&after), ids(&before), "no row moved, and none was lost");
    assert_eq!(
        marks_but_the_roster(&after),
        marks_but_the_roster(&before),
        "no `AUTOINCREMENT` mark moved — `account`'s still sits above its surviving ids"
    );
}

/// **A second run says there is nothing to carry, and opens nothing for writing to find that out.**
/// The verb's probe is read-only, so a carried store's file is not touched — asserted on its mtime,
/// which any committed write moves.
#[test]
fn a_second_run_reports_already_carried_and_writes_nothing() {
    let c = Case::planted(ROWS);
    let first = c.run();
    assert_eq!(code(&first), 0, "{}", stderr(&first));
    let stamp = std::fs::metadata(c.db()).expect("the database").modified().expect("its mtime");
    let carried = link_snapshot(&c.dir);
    // A file's mtime can be as coarse as a second on some filesystems, so a write by the second run
    // could otherwise land on the first run's own stamp and be invisible.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let second = c.run();
    assert_eq!(code(&second), 0, "{}", stderr(&second));
    let said = stdout(&second);
    assert!(said.contains("venue links: already carried and contracted"), "{said}");
    assert_eq!(
        std::fs::metadata(c.db()).expect("the database").modified().expect("its mtime"),
        stamp,
        "the second run opened nothing for writing"
    );
    assert_eq!(link_snapshot(&c.dir), carried, "…and nothing about the store moved");
}

/// **A store only a human can repair is refused the way every writer refuses it** — non-zero, the
/// row named, and NOTHING committed. Not with *run this as the user that owns the store*: the
/// refusal is not an ownership matter, and its repair is the SQLite-client one it names.
#[test]
fn a_row_naming_no_roster_venue_is_refused_by_name_and_nothing_is_committed() {
    let c = Case::planted(&format!(
        "{ROWS}
INSERT INTO account (id, venue, venue_id, tier) VALUES (7, 'no-such-venue', NULL, 'demo');
INSERT INTO account (id, venue, venue_id, tier) VALUES (8, 'okx', NULL, 'live');"
    ));
    let before = link_snapshot(&c.dir);
    assert!(
        before.rows.contains(&("account".to_string(), 8, None)),
        "premise: row 8 names a roster venue and has no number, so the carry's backfill would \
         number it — which makes it the row that proves nothing was committed"
    );

    let out = c.run();
    let said = stderr(&out);
    assert_eq!(code(&out), 1, "refused on the run-failure rung, as the 0095 refusal is: {said}");
    assert!(
        said.contains("`account` id 7 (venue 'no-such-venue')"),
        "the refusal names the table, the row and the venue string together: {said}"
    );
    assert!(
        !said.contains("run this as the user that owns"),
        "a repair refusal is not an ownership matter: {said}"
    );
    assert!(!said.contains("could not be read"), "…nor a store that could not be read: {said}");
    let after = link_snapshot(&c.dir);
    assert_eq!(after, before, "nothing was committed: every number, id and mark is as planted");
    assert!(after.rows.contains(&("account".to_string(), 7, None)), "…the refused row among them");
}

// ---------------------------------------------------------------------------------------------
// The second release: the verb CONTRACTS a store the first release carried
// ---------------------------------------------------------------------------------------------

/// **A store the first release carried is CONTRACTED on purpose** — the text `venue` column goes
/// from `account`, `credential` and `venue_setting` by the operator's run rather than by whatever
/// write comes next — and no id and no mark moves. There is nothing to carry onto the number, so the
/// verb says only what it did. (It answered "already carried" here and wrote nothing until the
/// second release's first fix round.)
#[test]
fn a_store_the_first_release_carried_is_contracted_on_purpose_and_keeps_every_id_and_mark() {
    let c = Case::planted_on_release_1(&format!("{ROWS}{CREDENTIAL_ROWS}"));
    let before = link_snapshot(&c.dir);
    assert!(before.nullable.is_empty(), "premise: the first release carried it: {before:?}");
    assert_eq!(
        before.text_venue,
        ["account", "credential", "venue_setting", "venue_arming"],
        "premise: every linked table still carries its text column"
    );
    assert_eq!(
        before
            .credentials
            .iter()
            .map(|(id, account, venue)| (*id, *account, venue.is_some()))
            .collect::<Vec<_>>(),
        [(1, Some(1), false), (2, None, true)],
        "premise: an account-scoped row, and a venue-scoped row carrying its venue's number"
    );

    let out = c.run();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let said = stdout(&out);
    assert!(
        said.contains(
            "venue links: text venue column dropped from account, credential, venue_setting"
        ),
        "it says what it did: {said}"
    );
    assert!(!said.contains("carried onto venue_id"), "…and nothing it did not: {said}");

    let after = link_snapshot(&c.dir);
    assert_eq!(after.text_venue, ["venue_arming"], "only `venue_arming` keeps its text column");
    assert_eq!(after.rows, before.rows, "every row kept its id and its number");
    assert_eq!(
        after.credentials, before.credentials,
        "every credential row kept its id, its account and its venue's number: the contraction \
         REBUILDS `credential`, and a row it dropped or re-filed would change which key signs what"
    );
    assert_eq!(
        marks_but_the_roster(&after),
        marks_but_the_roster(&before),
        "no `AUTOINCREMENT` mark moved — `account`'s still sits above its surviving ids"
    );

    // …and a second run finds nothing owed and opens nothing for writing.
    let second = c.run();
    assert_eq!(code(&second), 0, "{}", stderr(&second));
    assert!(stdout(&second).contains("venue links: already carried and contracted"));
    assert_eq!(link_snapshot(&c.dir), after, "the second run moved nothing");
}

/// **A first-release store the DROP refuses is refused by the verb** — exit 1, the row named, and
/// nothing committed — rather than reported finished. A venue-scoped `credential` row filed for a
/// venue the roster lacks keeps its text and a NULL number on that shape, and the drop would erase
/// the only venue it names. (Until the second release's first fix round this store answered
/// "already carried" with exit 0, and the refusal met the next write instead, whatever it was.)
#[test]
fn a_first_release_store_the_drop_refuses_is_refused_by_name_and_nothing_is_committed() {
    let c = Case::planted_on_release_1(&format!(
        "{ROWS}{CREDENTIAL_ROWS}
INSERT INTO credential (id, account_id, venue, venue_id, field, value, name)
    VALUES (4, NULL, 'no-such-venue', NULL, 'API_KEY', 'a-stray-value', 'no-such-venue-key');"
    ));
    let before = link_snapshot(&c.dir);

    let out = c.run();
    let said = stderr(&out);
    assert_eq!(code(&out), 1, "refused on the run-failure rung: {said}");
    assert!(
        said.contains("`credential` id 4 (venue 'no-such-venue')"),
        "the refusal names the table, the row and the venue string together: {said}"
    );
    assert!(!said.contains("a-stray-value"), "…and never a value: {said}");
    assert!(
        !said.contains("run this as the user that owns"),
        "a repair refusal is not an ownership matter: {said}"
    );
    assert_eq!(link_snapshot(&c.dir), before, "nothing was committed: every text column is there");
}

// ---------------------------------------------------------------------------------------------
// Decision 0095 pending: its transaction runs the store's whole write path
// ---------------------------------------------------------------------------------------------

/// **Decision 0095 pending on a store its write path REFUSES: nothing is reported applied, because
/// nothing was committed.** 0095's rewrite and the write path share ONE transaction, so the refusal
/// rolls the rewrite back with everything else, and the verb exits 1 with the refusal's own text and
/// no 0095 line. (The binary meets the refusal twice: `vike-cli`'s own boot applies a pending 0095
/// first, under `vike_boot::Ceilings::InterpretOrMark`, which marks the refusal and carries on; the
/// verb's own 0095 step then meets it again and exits.)
///
/// ⚠ This is the reachable half of "0095 and a refused venue-links step". The other half — 0095
/// COMMITTED and the venue links' own step failing after it, the case `execute` prints 0095's lines
/// first for — needs that step to fail on a store 0095's transaction had just run the SAME write
/// path over and committed. No planted store can do that; only another process acting between the
/// two calls (a held write lock, a concurrent writer) or an engine fault reaches it, and a test
/// driving the binary cannot interleave with either. In the shipped binary the verb's own 0095 step
/// is `Applied` only when the boot's attempt failed first, which narrows it further.
#[test]
fn with_decision_0095_pending_a_refused_store_reports_nothing_applied_and_commits_nothing() {
    let c = Case::planted(&format!(
        "{ROWS}{LIVE_CEILING}
INSERT INTO account (id, venue, venue_id, tier) VALUES (7, 'no-such-venue', NULL, 'demo');"
    ));
    unmark_live_means_mainnet(&c.dir);
    assert!(live_means_mainnet_pending(&c.dir).expect("the probe"), "premise: 0095 is pending");
    let before = link_snapshot(&c.dir);

    let out = c.run();
    let said = stderr(&out);
    assert_eq!(code(&out), 1, "refused on the run-failure rung: {said}");
    assert!(said.contains("`account` id 7 (venue 'no-such-venue')"), "the row is named: {said}");
    assert!(
        !stdout(&out).contains("decision 0095 applied"),
        "no rewrite is reported, because none was committed: {}",
        stdout(&out)
    );
    assert!(
        live_means_mainnet_pending(&c.dir).expect("the probe"),
        "…and decision 0095 is still pending: its rewrite rolled back with the refusal"
    );
    assert_eq!(link_snapshot(&c.dir), before, "nothing was committed");
}
