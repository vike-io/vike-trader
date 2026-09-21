//! **The profile store's own mechanics** — the half of 0057 Phase 3 that touches a real database.
//!
//! The PURE half (the arming-outcome preservation proof, the explicit primary, the #1866 refusal
//! vocabulary, the body round trip) lives in `crates/vike-tradehub/tests/daemon/profile_rows.rs`,
//! because it needs `DaemonProfile` and that type is one crate up. What is here is everything a
//! `vike.db` on disk has to be true of:
//!
//! * **absence, no-tables and empty all answer the same** — the arming-preservation property at the
//!   read layer, and the reason a box that never migrates cannot be changed by this landing;
//! * **storing a BODY does not select one**, and re-storing a body does not deselect the live
//!   profile;
//! * **a write REFUSES to create the store**, which is a live-gate refusal rather than tidiness;
//! * **the read works with `settings/db` READ-ONLY** — the shape `ProtectSystem=strict` with
//!   `ReadWritePaths=settings/state` puts the deployed daemon in, so 0057's *"the entire READ half
//!   of this migration is reachable today with no unit change whatsoever"* is checked and not
//!   assumed;
//! * **a write from inside that same shape does not succeed** — the runtime half of *who may write*.
//!   The source half is `crates/vike-ops/tests/profile_writer_gate.rs`.
//!
//! The store is built the way a real box builds one: `vike_secrets::migrate` over a fixture
//! credential file, because a profile write may not bring a database into existence and a test that
//! reached around that would be testing a path production does not have.

use std::collections::BTreeMap;
use std::path::PathBuf;

use vike_secrets::profile_store::{
    MountRow, OperatorWrite, Primary, ProfileError, ProfileKind, ProfileRow, RecorderBody,
    RecorderRow, StoredProfile, SubscriptionRow, clear_active, read_profiles, render_recorder_toml,
    set_active, store_profile, toml_string_array,
};

/// **The asset-class vocabulary a production caller passes** (`vike_model::AssetClass::
/// SQL_WORDS`), spelled here because this crate cannot link the crate that owns it — the same seam,
/// and the same reason, as `classify` below and as `tests/account_reader.rs`'s own copy.
///
/// ⚠ **This copy is not an authority and must never become one.** It is a FIXTURE: these tests
/// exercise the schema mechanism (`NOT NULL`, the `CHECK`, the round trip), not the vocabulary. The
/// cross-crate pin — that the words the DDL is rendered from really are the enum's — lives in
/// `crates/vike-tradehub/tests/daemon/profile_rows.rs`, the one place that can see both crates, and
/// that is where a drift between them fails.
const VOCABULARY: &[&str] = &[
    "Equity",
    "Etf",
    "CryptoSpot",
    "CryptoPerp",
    "CryptoFuture",
    "Option",
    "Fx",
    "Future",
    "Index",
    "PredictionMarket",
    "Cfd",
];

/// No node keys in this fixture — the `node_key` namespace is 0051's and nothing here touches it.
fn is_node_key(_key: &str) -> bool {
    false
}

/// The account classification a production caller passes
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it — the same seam, and the same reason, as
/// `tests/account_reader.rs`'s own copy. One venue, one tier: nothing below asserts anything about
/// credential classification.
fn classify(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification, Placement};
    if let Some(field) = name.strip_prefix("BYBIT_DEMO_") {
        return Classification {
            placement: Placement::Account(AccountKey {
                venue: "bybit".to_string(),
                tier: "demo".to_string(),
                label: None,
                discriminator: None,
            }),
            field: field.to_string(),
            secret: true,
            recognised: true,
            pending_move: None,
        };
    }
    Classification::unrecognised(name)
}

/// A settings directory carrying a MIGRATED store — the state both live boxes are in.
struct Fixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Fixture {
    fn migrated() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        std::fs::write(
            settings.join("secrets.env"),
            "BYBIT_DEMO_API_KEY=not-a-real-key\nBYBIT_DEMO_API_SECRET=not-a-real-secret\n",
        )
        .expect("write fixture store");
        let arg = settings.to_str().expect("utf-8 temp path");
        match vike_secrets::migrate(Some(arg), is_node_key, &classify) {
            Ok(_) => {}
            Err(e) => panic!("the fixture migration refused: {e}"),
        }
        Fixture { _dir: dir, settings }
    }

    /// A settings directory with NO database — the state a box that never migrated is in.
    fn files_only() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        Fixture { _dir: dir, settings }
    }

    fn db(&self) -> PathBuf {
        vike_secrets::db_path_in(&self.settings)
    }
}

fn body(name: &str, venue: &str) -> StoredProfile {
    StoredProfile {
        row: ProfileRow {
            name: name.to_string(),
            kind: ProfileKind::Daemon,
            active: false,
            note: None,
        },
        mounts: vec![{
            let mut m = MountRow::new(0, venue, "CryptoSpot");
            m.symbol = Some("BTCUSDT".to_string());
            m
        }],
        params: BTreeMap::new(),
        settings: BTreeMap::new(),
        recorder: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Absence, emptiness, and the property they share
// ---------------------------------------------------------------------------------------------

/// **A box with no database, a box whose database predates the profile tables, and a box whose
/// tables are empty all answer the same thing.** That is the arming-preservation property at the
/// read layer: nothing this landing adds can change a box that has not been migrated into it.
#[test]
fn an_absent_store_and_a_pre_phase3_store_both_answer_nothing() {
    let none = Fixture::files_only();
    let p = read_profiles(&none.db()).expect("an absent store is not an error, it is unconfigured");
    assert!(p.all().is_empty());
    assert_eq!(p.active(ProfileKind::Daemon), None);
    assert!(!p.tables_present());

    // A MIGRATED store with no profile tables — every box on 0054's credential migration is here.
    let old = Fixture::migrated();
    let p = read_profiles(&old.db()).expect("a credential store with no profile tables reads fine");
    assert!(p.all().is_empty(), "a pre-Phase-3 store holds no profiles");
    assert_eq!(p.active(ProfileKind::Daemon), None, "and selects nothing");
    assert!(
        !p.tables_present(),
        "…and says the tables are absent rather than pretending they are empty — the two are the \
         same ANSWER and a different FACT, and only the fact is worth reporting"
    );
}

/// **Storing a BODY is not selecting one.** The separation the whole phase rests on: a migration can
/// run on a live box and change nothing about what it trades.
#[test]
fn storing_a_body_does_not_select_it() {
    let fx = Fixture::migrated();
    let w = OperatorWrite::claim("test");
    store_profile(&fx.db(), &body("tradehub", "bybit"), &w, 1, VOCABULARY).expect("store the body");

    let p = read_profiles(&fx.db()).expect("read");
    assert!(p.tables_present(), "the tables exist now");
    assert!(p.by_name("tradehub").is_some(), "the body is there");
    assert_eq!(
        p.active(ProfileKind::Daemon),
        None,
        "STORING A BODY MUST NOT ARM ANYTHING — a migration that stored a body and selected it is \
         0057 Question 3's hazard performed by accident"
    );
}

/// Selecting is the separate act, and a body edit does not undo it.
#[test]
fn selecting_is_a_separate_act_and_a_re_store_preserves_it() {
    let fx = Fixture::migrated();
    let db = fx.db();
    let w = OperatorWrite::claim("test");
    store_profile(&db, &body("tradehub", "bybit"), &w, 1, VOCABULARY).expect("store");
    set_active(&db, ProfileKind::Daemon, "tradehub", &w, 2, VOCABULARY).expect("activate");
    assert_eq!(
        read_profiles(&db).expect("read").active(ProfileKind::Daemon).map(|p| p.row.name.clone()),
        Some("tradehub".to_string())
    );

    store_profile(&db, &body("tradehub", "okx"), &w, 3, VOCABULARY).expect("re-store the body");
    let after = read_profiles(&db).expect("read");
    assert_eq!(
        after.active(ProfileKind::Daemon).map(|p| p.row.name.clone()),
        Some("tradehub".to_string()),
        "editing WHAT a profile is must not change WHETHER it is the one running"
    );
    assert_eq!(after.by_name("tradehub").expect("body").mounts[0].venue, "okx", "the edit landed");

    clear_active(&db, ProfileKind::Daemon, &w, 4, VOCABULARY).expect("deselect");
    assert_eq!(
        read_profiles(&db).expect("read").active(ProfileKind::Daemon),
        None,
        "and an operator can put the box back in the state every box is in today"
    );
}

/// At most one active row per kind, and switching is one act rather than two.
#[test]
fn exactly_one_profile_of_a_kind_is_active() {
    let fx = Fixture::migrated();
    let db = fx.db();
    let w = OperatorWrite::claim("test");
    store_profile(&db, &body("a", "bybit"), &w, 1, VOCABULARY).expect("store a");
    store_profile(&db, &body("b", "okx"), &w, 1, VOCABULARY).expect("store b");
    set_active(&db, ProfileKind::Daemon, "a", &w, 2, VOCABULARY).expect("activate a");
    set_active(&db, ProfileKind::Daemon, "b", &w, 3, VOCABULARY).expect("activate b");
    let p = read_profiles(&db).expect("read");
    let active: Vec<&str> =
        p.all().iter().filter(|s| s.row.active).map(|s| s.row.name.as_str()).collect();
    assert_eq!(active, vec!["b"], "switching must not leave two rows claiming the box");
}

/// The explicit primary is stored, read back, and survives a stored reordering of the ordinals.
#[test]
fn the_declared_primary_round_trips_through_the_store() {
    let fx = Fixture::migrated();
    let db = fx.db();
    let w = OperatorWrite::claim("test");
    let mut b = body("two", "bybit");
    let mut second = MountRow::new(1, "okx", "CryptoSpot");
    second.symbol = Some("BTC-USDT".to_string());
    second.is_primary = true;
    b.mounts.push(second);
    store_profile(&db, &b, &w, 1, VOCABULARY).expect("store");

    let p = read_profiles(&db).expect("read");
    let stored = p.by_name("two").expect("the profile");
    assert_eq!(stored.primary(), Primary::Declared(1));
    assert_eq!(stored.primary_mount().expect("a primary").venue, "okx");

    // Store the SAME two mounts with their ordinals swapped. A table has no inherent order; the
    // declaration is what survives, and that is the whole reason the column exists.
    let mut swapped = body("two", "okx");
    swapped.mounts[0].symbol = Some("BTC-USDT".to_string());
    swapped.mounts[0].is_primary = true;
    let mut other = MountRow::new(1, "bybit", "CryptoSpot");
    other.symbol = Some("BTCUSDT".to_string());
    swapped.mounts.push(other);
    store_profile(&db, &swapped, &w, 2, VOCABULARY).expect("re-store swapped");

    let p = read_profiles(&db).expect("read");
    let stored = p.by_name("two").expect("the profile");
    assert_eq!(
        stored.primary_mount().expect("a primary").venue,
        "okx",
        "the primary is the mount that DECLARED it, whatever ordinal it now sits at"
    );
}

// ---------------------------------------------------------------------------------------------
// Who may write
// ---------------------------------------------------------------------------------------------

/// **A profile write REFUSES to create the store**, and the refusal names why.
///
/// `crate::db`'s `open_for_write` creates a database when the path is absent, and
/// `vike_secrets::Backend` decides which store answers credentials from ONE `is_file` on that path.
/// An empty database brought into existence by a profile write would become the credential store,
/// the `secrets.env` beside it would stop being read, and every venue on the box would drop to paper
/// looking perfectly configured.
#[test]
fn a_profile_write_will_not_bring_a_settings_database_into_existence() {
    let fx = Fixture::files_only();
    let db = fx.db();
    let w = OperatorWrite::claim("test");
    let e = store_profile(&db, &body("tradehub", "bybit"), &w, 1, VOCABULARY)
        .expect_err("a profile write must not create the store");
    assert!(matches!(e, ProfileError::NoStore { .. }), "{e}");
    let text = e.to_string();
    assert!(text.contains("secrets migrate"), "names the one command that may create one: {text}");
    assert!(text.contains("paper"), "says what creating one would cost: {text}");
    assert!(!db.exists(), "AND NOTHING WAS CREATED — this is the assertion that matters");
    assert!(
        !vike_secrets::database_present(&db),
        "the backend probe must still answer Files for this box"
    );
}

/// Make `dir` and `file` read-only, returning the directory's previous permissions, or `None` when
/// this box cannot host the fence (root, or a mode-blind filesystem).
///
/// ⚠ **The probe is not caution, it is the difference between a proof and a coincidence.** Running
/// as root makes a 0555 directory refuse nothing, and a test that asserted a refusal there would
/// pass or fail for a reason unrelated to the code under it.
#[cfg(unix)]
fn make_read_only(dir: &std::path::Path, file: &std::path::Path) -> Option<std::fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;
    let restore = std::fs::metadata(dir).expect("meta").permissions();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444)).expect("chmod file");
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).expect("chmod dir");
    let probe = dir.join(".write-probe");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        restore_read_write(dir, file, restore);
        return None;
    }
    Some(restore)
}

#[cfg(unix)]
fn restore_read_write(
    dir: &std::path::Path,
    file: &std::path::Path,
    restore: std::fs::Permissions,
) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, restore).expect("restore dir");
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).expect("restore file");
}

/// **The read half needs no unit change.** The deployed daemon runs under `ProtectSystem=strict`
/// with `ReadWritePaths=settings/state`, so `settings/db` is read-only inside its own namespace.
/// This reproduces that shape and proves the active row still answers.
#[cfg(unix)]
#[test]
fn the_active_row_is_readable_with_the_settings_db_read_only() {
    let fx = Fixture::migrated();
    let db = fx.db();
    let w = OperatorWrite::claim("test");
    store_profile(&db, &body("tradehub", "bybit"), &w, 1, VOCABULARY).expect("store");
    set_active(&db, ProfileKind::Daemon, "tradehub", &w, 2, VOCABULARY).expect("activate");

    let db_dir = db.parent().expect("db dir").to_path_buf();
    let Some(restore) = make_read_only(&db_dir, &db) else {
        eprintln!(
            "skipped: a 0555 directory refused nothing on this box (root, or a mode-blind fs)"
        );
        return;
    };
    let read = read_profiles(&db);
    restore_read_write(&db_dir, &db, restore);

    let read = read.expect("a read-only settings/db must still answer");
    assert_eq!(
        read.active(ProfileKind::Daemon).map(|p| p.row.name.clone()),
        Some("tradehub".to_string()),
        "0057: \"the entire READ half of this migration is reachable today with no unit change \
         whatsoever\" — if this fails, that sentence is wrong and the phase needs a unit edit"
    );
}

/// **A write from inside the daemon's own read-only shape does not succeed**, and the store is
/// unchanged afterwards.
#[cfg(unix)]
#[test]
fn a_write_from_inside_the_daemons_read_only_namespace_does_not_succeed() {
    let fx = Fixture::migrated();
    let db = fx.db();
    let w = OperatorWrite::claim("test");
    store_profile(&db, &body("tradehub", "bybit"), &w, 1, VOCABULARY).expect("store");

    let db_dir = db.parent().expect("db dir").to_path_buf();
    let Some(restore) = make_read_only(&db_dir, &db) else {
        eprintln!(
            "skipped: a 0555 directory refused nothing on this box (root, or a mode-blind fs)"
        );
        return;
    };
    let wrote = set_active(&db, ProfileKind::Daemon, "tradehub", &w, 2, VOCABULARY);
    restore_read_write(&db_dir, &db, restore);

    let err = wrote.expect_err(
        "a write into a read-only settings/db must FAIL — if it succeeds, the daemon's own \
         namespace is not the fence 0057 measures it to be",
    );
    assert!(!err.to_string().is_empty(), "the refusal must say something an operator can act on");
    assert_eq!(
        read_profiles(&db).expect("read").active(ProfileKind::Daemon),
        None,
        "and the selection is exactly what it was before the attempt"
    );
}

/// `recorder` PARSES since the owner overruled 0057's NO on 2026-09-16.
///
/// ⚠ This test used to be `a_recorder_profile_kind_is_refused_by_name` and asserted the opposite,
/// citing 0057. It is inverted rather than deleted: the word was already in the schema's `CHECK`
/// and only the Rust side refused it, so the property worth holding is that the two AGREE — a word
/// the `CHECK` admits and `parse` refuses is a row nothing can read, and a word `parse` admits and
/// the `CHECK` refuses is a write that fails at the database.
#[test]
fn a_recorder_profile_kind_is_readable_since_the_ruling_was_overturned() {
    assert_eq!(
        ProfileKind::parse("recorder").expect("the owner overruled 0057's NO on 2026-09-16"),
        ProfileKind::Recorder
    );
    assert_eq!(ProfileKind::Recorder.sql_word(), "recorder");
    // …and a word neither side knows is still a NAMED refusal rather than a silent drop.
    let e = ProfileKind::parse("sweeper").expect_err("an unknown kind must not read as anything");
    assert!(e.to_string().contains("sweeper"), "names the word it could not read: {e}");
}

/// A recorder BODY round-trips through the store, and the rendered document is the file's shape.
///
/// This is the property the whole migration rests on: `crates/vike-datahub/src/recorder.rs` feeds
/// [`render_recorder_toml`]'s output to `RecorderProfile::from_toml`, so if the rows do not
/// round-trip, a row-loaded daemon records something the operator never wrote.
#[test]
fn a_recorder_body_round_trips_and_renders_the_document_it_came_from() {
    let fx = Fixture::migrated();
    let path = fx.db();
    let write = OperatorWrite::claim("test");
    let body = RecorderBody {
        row: RecorderRow {
            store: "/srv/vike-<unit>/market_data/hist".to_string(),
            interval_secs: Some(300),
            min_parts: Some(4),
            target_mb: Some(384),
            max_merge_rows: Some(1_000_000),
            // ⚠ ABSENT, not zero: `retention_days = 0` is a refusal in the profile validator, and
            // an absent key means "keep forever". The column must be able to say the difference.
            retention_days: None,
            alert_webhooks: Some(toml_string_array(&["telegram".to_string()])),
            alert_repeat_secs: None,
            alert_series_prefix: None,
            note: Some("max_merge_rows: 202MB peak at 1e6 rows (measured)".to_string()),
        },
        subscriptions: vec![
            SubscriptionRow {
                ord: 0,
                venue: "polymarket".to_string(),
                family: Some("btc-updown-5m".to_string()),
                symbols: None,
                backfill: Some("off".to_string()),
                note: None,
            },
            SubscriptionRow {
                ord: 1,
                venue: "binance".to_string(),
                family: None,
                symbols: Some(toml_string_array(&["BTCUSDT.P".to_string()])),
                backfill: None,
                note: Some("the only venue whose L2 for this exists anywhere".to_string()),
            },
        ],
    };
    let stored = StoredProfile {
        row: ProfileRow {
            name: "default".to_string(),
            kind: ProfileKind::Recorder,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: BTreeMap::new(),
        settings: BTreeMap::new(),
        recorder: Some(body.clone()),
    };
    store_profile(&path, &stored, &write, 1, VOCABULARY).expect("store the recorder body");

    let back = read_profiles(&path).expect("read back");
    let got = back.by_name("default").expect("the profile is there");
    assert_eq!(got.row.kind, ProfileKind::Recorder);
    assert_eq!(got.recorder.as_ref(), Some(&body), "the body round-trips column for column");

    let doc = render_recorder_toml(got.recorder.as_ref().expect("body"));
    // The keys the rows CARRY, and nothing else: `retention_days` and `repeat_secs` were absent in
    // the profile and must stay absent in the rendering, or a re-rendered document would differ
    // from the file it was migrated from and the migration's fence could never pass.
    assert!(doc.contains("store = \"/srv/vike-<unit>/market_data/hist\""), "{doc}");
    assert!(doc.contains("family = \"btc-updown-5m\""), "{doc}");
    assert!(doc.contains("symbols = [\"BTCUSDT.P\"]"), "{doc}");
    assert!(doc.contains("backfill = \"off\""), "{doc}");
    assert!(doc.contains("webhooks = [\"telegram\"]"), "{doc}");
    assert!(!doc.contains("retention_days"), "an absent key must not be rendered: {doc}");
    assert!(!doc.contains("repeat_secs"), "an absent key must not be rendered: {doc}");
    // …and storing a BODY still selects nothing, which is the separation the whole phase rests on.
    assert!(!got.row.active, "storing a recorder body must not activate it");
}

/// A store written before the recorder tables existed still reads its daemon and run profiles.
///
/// ⚠ **This is the regression the presence probe nearly shipped.** `read_profiles` answers
/// `Profiles::none()` when the profile tables are absent; widening that probe to the two NEW tables
/// would have made every already-migrated box report as un-migrated — its daemon and run profiles
/// gone from the read path, with no error anywhere. Two boxes are in that state today.
#[test]
fn a_store_without_the_recorder_tables_still_reads_its_other_profiles() {
    let fx = Fixture::migrated();
    let path = fx.db();
    let write = OperatorWrite::claim("test");
    store_profile(&path, &body("live", "bybit"), &write, 1, VOCABULARY)
        .expect("store a daemon body");
    // Drop the recorder tables, reproducing a store written by the pre-2026-09-16 schema.
    {
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.execute_batch("DROP TABLE subscription; DROP TABLE recorder;").expect("drop");
    }
    let back = read_profiles(&path).expect("a pre-recorder store must still read");
    let got = back.by_name("live").expect("the daemon profile is still there");
    assert_eq!(got.mounts.len(), 1, "and its mounts came with it");
    assert!(got.recorder.is_none(), "with no recorder body, which is what absence means");
}

// ---------------------------------------------------------------------------------------------
// The mount's asset class — 0061 phase 5
// ---------------------------------------------------------------------------------------------

/// The class a row was STORED with is the class it READS BACK with — the round trip the migration
/// depends on, since a mount that silently changed product on a store/load cycle would be worse
/// than one that never carried the field.
#[test]
fn the_mount_asset_class_round_trips() {
    let fx = Fixture::migrated();
    let db = fx.db();
    let w = OperatorWrite::claim("test");
    let mut b = body("tradehub", "bybit");
    b.mounts[0].asset_class = "CryptoPerp".to_string();
    b.mounts.push({
        let mut m = MountRow::new(1, "deribit", "Option");
        m.symbol = Some("BTC-27JUN25-60000-C".to_string());
        m
    });
    store_profile(&db, &b, &w, 1, VOCABULARY).expect("store");

    let stored = read_profiles(&db).expect("read");
    let p = stored.by_name("tradehub").expect("the profile");
    assert_eq!(p.mounts[0].asset_class, "CryptoPerp");
    assert_eq!(p.mounts[1].asset_class, "Option");
}
