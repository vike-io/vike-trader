//! Proof 6 and the second reader - the read path is per-RUN, never per-KEY.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 6 — the read path: per-RUN, never per-KEY
// ---------------------------------------------------------------------------------------------

/// **A box with no database behaves exactly as it did before any of this existed.**
///
/// Stage 2's hard constraint. The probe is one `is_file` on one path, so a box that never migrates
/// reaches the same parser, the same findings and the same absent-arm live gate.
#[test]
fn a_box_with_no_database_is_unchanged() {
    let fx = Fixture::live_shaped();
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Files,
        "no database, so: files"
    );

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::File(fx.store()), "the FILE answered");
    assert_eq!(resolved.secrets.len(), 67);
    assert!(resolved.shadowed.is_none(), "nothing is shadowing anything on an unmigrated box");

    let (nodes, source) =
        vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("node keys");
    assert_eq!(source, NodeKeySource::NodeFile, "the node FILE answered");
    assert_eq!(nodes.secrets.len(), 4);

    // …and an absent store is still the live gate rather than an error.
    let bare = Fixture::empty();
    let empty = vike_secrets::resolve_project(bare.arg()).expect("absent is an answer");
    assert_eq!(empty.source, Source::None);
    assert!(empty.secrets.is_empty(), "no credentials ⇒ every venue stays paper");

    // ⚠ THE SECOND READER, on the same two projects. `load_workspace_dotenv_from` now asks
    // `backend_in` like everything else, so this is where "a box with no database is byte-identical
    // to today" stops being a claim about ONE function. Both arms it ever had are re-proved:
    // a present file parses to exactly the file's pairs, and an absent one is the empty map.
    let second = vike_secrets::load_workspace_dotenv_from(fx.arg());
    assert_eq!(
        second,
        resolved.secrets.clone().into_map(),
        "the infallible reader and the fallible one must answer identically on an unmigrated box"
    );
    assert_eq!(second.len(), 67);
    assert_eq!(
        second.get("BINANCE_DEMO_API_KEY"),
        Some(&fake_value("BINANCE_DEMO_API_KEY")),
        "…and it is the FILE's bytes, parsed the way they always were"
    );
    assert!(
        vike_secrets::load_workspace_dotenv_from(bare.arg()).is_empty(),
        "an absent store is still an empty map here — the live gate, not an error"
    );
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

/// **The node keys come from the table too, and no file is opened — so the depth never exceeds one.**
///
/// `resolve_node_keys` runs 0051's one deliberate fallback (`node.env`, then the credential store,
/// warned). 0054 fixes the order explicitly: the database read must REPLACE that chain rather than
/// stack on it, or a node key becomes resolvable from three places and 0051's retirement condition
/// becomes unsatisfiable. The fixture plants a disagreeing value in `node.env` so that a nested
/// implementation would be visible rather than merely unproven.
#[test]
fn the_node_key_table_replaces_the_file_chain_rather_than_stacking_on_it() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    std::fs::write(
        fx.node(),
        "VIKE_TRADEHUB_OBSERVE_KEY=edited-after-the-migration\n\
         VIKE_TRADEHUB_CONTROL_KEY=edited-after-the-migration\n",
    )
    .unwrap();

    let (resolved, source) = vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("nodes");
    assert_eq!(source, NodeKeySource::Database, "the TABLE answered, not either file");
    assert_ne!(
        source,
        NodeKeySource::LegacyCredentialStore,
        "0051's fallback is not reachable here"
    );
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

/// **The WRITE path on a project whose database was removed routes to the FILES branch — it is not
/// the race, and it must not be mistaken for it.**
///
/// The companion to `crates/vike-secrets/src/db/migrate_tests.rs`'s
/// `a_write_whose_database_vanished_creates_nothing_and_fails_loudly`, which reconstructs the state
/// INSIDE the window between `backend_in`'s probe and `upsert_rows`' open. This one covers the
/// ordinary case that looks similar from outside and is entirely different: the database is gone
/// BEFORE the probe, so the probe says `Files` and the write lands in the file exactly as it does on
/// a box that never migrated. Without this, a fix that refused too much would look correct.
#[test]
fn a_write_after_the_database_is_gone_lands_in_the_file_and_creates_no_database() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let before = digest(&fx.store());

    // The operator removes the database — their own act, which nothing in this workspace performs.
    std::fs::remove_file(fx.db()).expect("remove the database");
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Files,
        "the probe sees no database"
    );

    let landed = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated".to_string())],
        Some(&classify),
    )
    .expect("a write on a files-backed project must land");
    assert_eq!(landed, Backend::Files, "…in the FILE");

    assert!(!fx.db().exists(), "a WRITE must never bring a database back into existence");
    assert_ne!(before, digest(&fx.store()), "the file genuinely changed");

    // The upsert rule: exactly the named key, every other byte alone.
    let after = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(after.source, Source::File(fx.store()));
    assert_eq!(after.secrets.len(), 67, "no key gained or lost");
    assert_eq!(value(&after.secrets, "BINANCE_DEMO_API_KEY").as_deref(), Some("rotated"));
    assert_eq!(
        value(&after.secrets, "IBKR_DEMO_ACCOUNT"),
        Some(fake_value("IBKR_DEMO_ACCOUNT")),
        "a neighbouring key was rewritten"
    );

    // …and the second reader sees the same file, because it asks the same backend.
    assert_eq!(
        vike_secrets::load_workspace_dotenv_from(fx.arg()),
        after.secrets.into_map(),
        "one store choice on the write path and the read path alike"
    );
}
