//! Proof 6 and the second reader - the read path is per-RUN, never per-KEY.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 6 — the read path: per-RUN, never per-KEY
// ---------------------------------------------------------------------------------------------

/// **RULE 1 — a box with no database has NO credentials, whatever files sit beside it.**
///
/// Until 2026-10-07 this fixture — 67 venue keys in `secrets.env`, four node keys in `node.env`,
/// no database — answered all 71 out of the files. The credential FILE store is removed: the probe
/// says [`Backend::Absent`], every reader answers EMPTY (the live gate: every venue paper), and the
/// files are REPORTED as unread — never parsed into a map, never touched. A regression that put any
/// file reader back behind the absent arm turns every assertion here red.
#[test]
fn a_box_with_no_database_has_no_credentials_and_says_why() {
    let fx = Fixture::live_shaped();
    let (store_before, node_before) = (digest(&fx.store()), digest(&fx.node()));
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Absent,
        "no database, so: no store at all"
    );

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("absent is an answer");
    assert_eq!(resolved.source, Source::None, "NO file may answer");
    assert!(resolved.secrets.is_empty(), "no credentials ⇒ every venue stays paper");
    assert!(resolved.shadowed.is_none(), "nothing is shadowing anything without a database");
    let unread = resolved.unread.expect("a keyed secrets.env with no database must be REPORTED");
    assert_eq!(unread.file, fx.store());
    assert_eq!(unread.keyed, Ok(67), "the finding counts the keyed names it is not reading");
    let said = unread.to_string();
    assert!(said.contains("NOT READ") && said.contains("vike-cli secrets migrate"), "{said}");

    let nodes = vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("node keys");
    assert_eq!(nodes.source, Source::None, "node.env must not answer either");
    assert!(nodes.secrets.is_empty());
    assert_eq!(nodes.unread.expect("node.env must be reported").keyed, Ok(4));

    // ⚠ THE SECOND READER, on the same project: infallible, so it cannot report — it can only
    // answer, and the answer is the same empty map.
    assert!(
        vike_secrets::load_workspace_dotenv_from(fx.arg()).is_empty(),
        "the infallible reader must not read the file either"
    );

    // …and none of it touched either file, or created a database.
    assert_eq!(digest(&fx.store()), store_before, "secrets.env must be byte-identical");
    assert_eq!(digest(&fx.node()), node_before, "node.env must be byte-identical");
    assert!(!fx.db().exists(), "a READ must never bring a database into existence");

    // An empty project is the same live gate, with nothing to report.
    let bare = Fixture::empty();
    let empty = vike_secrets::resolve_project(bare.arg()).expect("absent is an answer");
    assert_eq!(empty.source, Source::None);
    assert!(empty.secrets.is_empty() && empty.unread.is_none());
}

/// **When the database exists it answers WHOLLY, and the file is not consulted for anything.**
///
/// The mid-migration hazard made executable: the file is left holding a DIFFERENT value for a key
/// the database also has, plus a key the database has never heard of. A per-KEY fallback — the
/// ladder `docs/decisions/0051` forbids — would return the file's value for the second key and read
/// half from each. The per-RUN choice cannot: the file is never opened.
#[test]
fn the_database_answers_wholly_and_the_file_does_not() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    // Rewrite the file AFTER the migration: one changed value, one key the database does not hold.
    let mut text = std::fs::read_to_string(fx.store()).unwrap();
    text = text.replace(
        &format!("BINANCE_DEMO_API_KEY={}", fake_value("BINANCE_DEMO_API_KEY")),
        "BINANCE_DEMO_API_KEY=edited-after-the-migration",
    );
    text.push_str("A_KEY_ONLY_THE_FILE_HAS=never-migrated\n");
    std::fs::write(fx.store(), text).unwrap();

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()));
    let map = resolved.secrets.clone().into_map();
    assert_eq!(
        map.get("BINANCE_DEMO_API_KEY"),
        Some(&fake_value("BINANCE_DEMO_API_KEY")),
        "the DATABASE's value must win — a per-key merge would have taken the file's"
    );
    assert!(
        !map.contains_key("A_KEY_ONLY_THE_FILE_HAS"),
        "a key only the file has must NOT resolve — that would be the ladder"
    );
    assert_eq!(resolved.secrets.len(), 67, "exactly the table, nothing merged in");

    // …and the operator is told, rather than left to discover it by an edit that does nothing.
    let shadow = resolved.shadowed.expect("a file the database now shadows must be reported");
    assert_eq!(shadow.file, fx.store());
    assert_eq!(shadow.db, fx.db());
    let said = shadow.to_string();
    assert!(said.contains("NO LONGER READ"), "{said}");
}

/// **The node keys come from the table, and no file is opened.**
///
/// `resolve_node_keys` used to run 0051's one deliberate fallback (`node.env`, then the credential
/// store, warned) on a box with no database; that fallback went with the credential file plane on
/// 2026-10-07. The fixture plants a disagreeing value in `node.env` so that a file read anywhere
/// in the path would be visible rather than merely unproven.
#[test]
fn the_node_key_table_answers_and_no_file_does() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    std::fs::write(
        fx.node(),
        "VIKE_TRADEHUB_OBSERVE_KEY=edited-after-the-migration\n\
         VIKE_TRADEHUB_CONTROL_KEY=edited-after-the-migration\n",
    )
    .unwrap();

    let resolved = vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("nodes");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the TABLE answered, not either file");
    let map = resolved.secrets.into_map();
    assert_eq!(
        map.get("VIKE_TRADEHUB_OBSERVE_KEY"),
        Some(&fake_value("VIKE_TRADEHUB_OBSERVE_KEY")),
        "the database's value, not the file's"
    );
    assert_eq!(map.len(), 4, "the whole node_key table and nothing else");
}

/// A node key still sitting in the CREDENTIAL file — 0051's legacy home — is migrated into the
/// `node_key` TABLE, which is what discharges that fallback rather than deferring it.
#[test]
fn a_node_key_in_the_legacy_home_lands_in_the_node_table() {
    let fx = Fixture::empty();
    std::fs::write(
        fx.store(),
        "BINANCE_DEMO_API_KEY=b\nVIKE_TRADEHUB_OBSERVE_KEY=o\nVIKE_TRADEHUB_CONTROL_KEY=c\n",
    )
    .unwrap();

    let report = fx.migrate();
    let creds = vike_secrets::read_table(&fx.db(), Table::Credential).expect("credentials");
    let nodes = vike_secrets::read_table(&fx.db(), Table::NodeKey).expect("node keys");

    assert_eq!(key_names(&creds), BTreeSet::from(["BINANCE_DEMO_API_KEY".to_string()]));
    assert_eq!(
        key_names(&nodes),
        BTreeSet::from([
            "VIKE_TRADEHUB_OBSERVE_KEY".to_string(),
            "VIKE_TRADEHUB_CONTROL_KEY".to_string()
        ]),
        "the legacy home is DRAINED into the right namespace\n{report}"
    );
    // …and the report says where they came from, because an operator reading this needs to know the
    // node keys moved namespace without their asking.
    let said = report.to_string();
    assert!(said.contains("node_key"), "{said}");
    assert!(said.contains("READ ONLY"), "{said}");
}

// ---------------------------------------------------------------------------------------------
// PROOF — THE SECOND READER IS ON THE SAME LADDER
// ---------------------------------------------------------------------------------------------

/// **`load_workspace_dotenv_from` answers from the DATABASE on a migrated project.**
///
/// The defect this is written against was live in shipped code: that function was a
/// `read_to_string` of the credential FILE and was never routed through `Backend`, so it was a
/// SECOND store choice sitting beside the one `resolve_project` makes — the ladder
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids, reached through the other
/// artifact. On a migrated box it opened the retired file while every other reader used the
/// database, and it did so on live paths: `crates/vike-run/src/bin/ibkr_mount.rs`'s `main` and
/// `crates/vike-backfill/src/bin/ibkr_backfill.rs`'s `main` (both bins are DELETED now, measured
/// unused — the second by docs/decisions/0094 — this describes what they did while they existed)
/// took IBKR's account, host, port and client id through it, and
/// `crates/bridges/polymarket/src/egress.rs`'s now-deleted `dotenv_proxy_vars` took
/// the egress settings that way too. Nothing errored — an absent key IS the live gate — so an IBKR
/// mount lost its account in silence. (Decision 0095 later deleted that Polymarket reader outright:
/// the bridge takes its egress from a root's declaration now, never a store read of its own.)
///
/// The assertions are the SAME three the fallible reader's
/// [`the_database_answers_wholly_and_the_file_does_not`] makes, deliberately: same fixture, same
/// mid-migration hazard, and the two readers must not be able to disagree. **Reverting
/// `load_workspace_dotenv_from`'s body to its `read_to_string` makes every one of them fail.**
#[test]
fn the_second_reader_answers_from_the_database_on_a_migrated_project() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    // The mid-migration hazard, planted exactly as the fallible reader's twin plants it: the file is
    // left holding a DIFFERENT value for a key the database also has, plus a key the database has
    // never heard of.
    let mut text = std::fs::read_to_string(fx.store()).unwrap();
    text = text.replace(
        &format!("IBKR_DEMO_ACCOUNT={}", fake_value("IBKR_DEMO_ACCOUNT")),
        "IBKR_DEMO_ACCOUNT=edited-after-the-migration",
    );
    text.push_str("A_KEY_ONLY_THE_FILE_HAS=never-migrated\n");
    std::fs::write(fx.store(), text).unwrap();

    let map = vike_secrets::load_workspace_dotenv_from(fx.arg());

    assert_eq!(
        map.get("IBKR_DEMO_ACCOUNT"),
        Some(&fake_value("IBKR_DEMO_ACCOUNT")),
        "THE FILE ANSWERED. This reader is still opening the retired store: an IBKR config read \
         on a migrated box takes its account from a file nothing else reads."
    );
    assert!(
        !map.contains_key("A_KEY_ONLY_THE_FILE_HAS"),
        "a key only the file has must NOT resolve — that is the ladder, per key"
    );
    assert_eq!(map.len(), 67, "exactly the table, nothing merged in from the file");

    // …and it is the same answer the fallible reader gives, which is the property that makes the
    // two structurally incapable of disagreeing rather than merely agreeing today.
    let fallible = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(fallible.source, Source::Database(fx.db()));
    assert_eq!(map, fallible.secrets.into_map(), "one store choice, or it was never closed");

    // The no-override twin routes through the same body, so it cannot be the file reader either.
    // Asserted as an EQUALITY of the two spellings rather than against the fixture: the no-override
    // call walks from the real working directory, which is this test binary's, not the fixture's.
    assert_eq!(
        vike_secrets::load_workspace_dotenv_from(None),
        vike_secrets::load_workspace_dotenv(),
        "the two spellings must remain one function"
    );
}

/// **A database that EXISTS and cannot be READ is an empty map to the infallible reader and a LOUD
/// error to the fallible one — and the asymmetry is pinned rather than assumed.**
///
/// `load_workspace_dotenv_from` is infallible by signature and this crate carries no logging
/// dependency, so routing it through the backend could not give it a channel for the error. That is
/// deliberate — these are STARTUP paths (two `main`s and a proxy resolver) that must not gain a new
/// hard failure — but "empty map" and "loud error" must not quietly become the same answer, so both
/// halves are measured here on ONE store in ONE state.
///
/// It is also not a NEW silence: a present-but-unreadable FILE has always returned an empty map from
/// this function. What the database changes is how REACHABLE that state is, because
/// `check_schema_version` refuses an unstamped or wrong-version database that a file reader would
/// never have rejected. The cure for a caller that must tell the two apart is unchanged and is
/// named in the function's own doc: use `resolve_project`.
#[test]
fn an_unreadable_database_is_empty_to_the_infallible_reader_and_loud_to_the_fallible_one() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    // The state a crash leaves and the state a future schema leaves, reached the same way the
    // interrupted-migration test reaches it: move `user_version` off `SCHEMA_VERSION`.
    let conn = fx.conn();
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION + 1);
    drop(conn);

    // The database is still THERE, so the backend still chooses it. This is the precondition: the
    // file is not consulted on either path below.
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));
    assert!(fx.store().exists(), "…and the file is sitting right there, holding all 67");

    let loud = vike_secrets::resolve_project(fx.arg())
        .expect_err("a store that exists and cannot be read is an ERROR");
    let said = loud.to_string();
    assert!(said.contains("schema version"), "the error must say what is wrong: {said}");

    let quiet = vike_secrets::load_workspace_dotenv_from(fx.arg());
    assert!(
        quiet.is_empty(),
        "the infallible reader must NOT fall back to the file here — that would be the per-key \
         ladder reappearing on the failure path, which is the worst place for it"
    );
}

/// **The WRITE path on a project whose database was removed is REFUSED — it lands nowhere, and
/// it is not the race either.**
///
/// The companion to `crates/vike-secrets/src/db/migrate_tests.rs`'s
/// `a_write_whose_database_vanished_creates_nothing_and_fails_loudly`, which reconstructs the state
/// INSIDE the window between `backend_in`'s probe and `upsert_rows`' open. This one covers the
/// ordinary case that looks similar from outside: the database is gone BEFORE the probe, so the
/// probe says `Absent`. Until 2026-10-07 the write then landed in `secrets.env`; with the file
/// store removed that would be a key written to a file nothing reads, so it is REFUSED by name,
/// no database comes back, and the file is not touched.
#[test]
fn a_write_after_the_database_is_gone_is_refused_and_touches_nothing() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let before = digest(&fx.store());

    // The operator removes the database — their own act, which nothing in this workspace performs.
    std::fs::remove_file(fx.db()).expect("remove the database");
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Absent,
        "the probe sees no database"
    );

    let refused = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated".to_string())],
        Some(&classify),
    )
    .expect_err("with no database there is no store to write");
    let said = refused.to_string();
    assert!(said.contains("vike-cli secrets migrate"), "the refusal must name the way in: {said}");

    assert!(!fx.db().exists(), "a WRITE must never bring a database back into existence");
    assert_eq!(before, digest(&fx.store()), "the credential FILE must not be written");
    assert!(
        vike_secrets::load_workspace_dotenv_from(fx.arg()).is_empty(),
        "and with no database the box has no credentials"
    );
}
