//! Proofs 1-5 - the round trip, idempotence, no sidecar, the journal mode, the modes.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 1 — all 67 names survive the round trip
// ---------------------------------------------------------------------------------------------

/// **Every key NAME in a live-shaped store comes back out of the database, and the SETS are equal.**
///
/// The failure this is written against is the worst outcome available: a writer that files the ten
/// names the generated grid knows about and silently drops the fifty-seven bespoke ones. Set
/// equality both ways is what catches it — a subset assertion would pass on a writer that dropped
/// half the store, and a COUNT assertion would pass on one that invented names.
#[test]
fn all_67_key_names_survive_the_round_trip() {
    let fx = Fixture::live_shaped();

    let creds = vike_secrets::read_table(&fx.db(), Table::Credential).expect("credential table");
    let nodes = vike_secrets::read_table(&fx.db(), Table::NodeKey).expect("node_key table");

    let want_creds: BTreeSet<String> =
        LIVE_CREDENTIAL_KEYS.iter().map(|k| (*k).to_string()).collect();
    let want_nodes: BTreeSet<String> = LIVE_NODE_KEYS.iter().map(|k| (*k).to_string()).collect();

    assert_eq!(want_creds.len(), 67, "the fixture is the live store's 67 names");
    assert_eq!(key_names(&creds), want_creds, "the credential table is not the name set written");
    assert_eq!(key_names(&nodes), want_nodes, "the node_key table is not the node-key set written");
    assert_eq!(creds.len(), 67, "67 credential rows");
    assert_eq!(nodes.len(), 4, "4 node-key rows");

    // And the VALUES round-tripped too, so this is a store rather than a name census.
    let back = creds.into_map();
    for k in LIVE_CREDENTIAL_KEYS {
        assert_eq!(back.get(k), Some(&fake_value(k)), "{k} came back changed");
    }
}

/// The same 67, reached through the PRODUCTION read path rather than through `read_table`.
///
/// Separate from the test above on purpose: that one proves the writer, this one proves that
/// `resolve_project` — the function every composition root reaches through — answers with the same
/// set. A writer that filled a table nothing reads would pass the first and fail this.
#[test]
fn the_production_resolver_answers_with_all_67() {
    let fx = Fixture::live_shaped();

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the database answered");
    assert_eq!(
        key_names(&resolved.secrets),
        LIVE_CREDENTIAL_KEYS.iter().map(|k| (*k).to_string()).collect::<BTreeSet<_>>()
    );
    assert_eq!(resolved.secrets.len(), 67);
}

// ---------------------------------------------------------------------------------------------
// PROOF 2 — `secrets init` over an existing store is a no-op, down to the bytes
// ---------------------------------------------------------------------------------------------

/// **`create_store` over a store already at the current schema changes nothing, down to the
/// database's bytes.**
///
/// Idempotence stated at the level of rows would be satisfied by a run that rewrote every row with
/// the same value — which touches the file, could churn a page, and would make "did anything happen
/// here" unanswerable from a `stat`. `create_store` is written so that a run with nothing to do opens NO
/// write connection at all, and this is the assertion that holds it to that — twice.
#[test]
fn a_create_over_a_current_store_changes_no_byte() {
    let fx = Fixture::live_shaped();
    let before = (digest(&fx.db()), std::fs::read(fx.db()).unwrap());

    for run in 1..=2 {
        let done = fx.create();
        assert!(!done.created, "run {run} finds the store already there: {done}");

        let after = (digest(&fx.db()), std::fs::read(fx.db()).unwrap());
        assert_eq!(before.0, after.0, "database digest moved on no-op run {run}");
        assert_eq!(before.1, after.1, "database bytes moved on no-op run {run}");
    }
}

// ---------------------------------------------------------------------------------------------
// PROOF 4 — no sidecar at rest
// ---------------------------------------------------------------------------------------------

/// **`settings/db/` holds exactly one file after writes and a clean close.**
///
/// This is what `journal_mode = DELETE` was chosen for, and 0054's constraint 1 was amended to
/// require it, so it is proven rather than trusted. A `vike.db-wal` beside the store would be a
/// SECOND plaintext credential artifact, would make a daemon-down read impossible in the read-only
/// `settings/` the deployed unit mounts, and would turn one `chmod 600` into a check over a set —
/// and it would announce itself as nothing but two extra files nobody looks at.
#[test]
fn no_sidecar_survives_a_clean_close() {
    let fx = Fixture::live_shaped();

    let dir = fx.db().parent().expect("db dir").to_path_buf();
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .expect("read settings/db")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    assert_eq!(found, vec!["vike.db".to_string()], "settings/db must hold ONE file at rest");
}

/// …and the engine agrees, rather than us inferring it from an absent file.
///
/// The directory check above would also pass on a WAL database that happened to have been
/// checkpointed and closed, so the pragma is asked directly. Every writer already refuses when the
/// engine answers anything but `delete`; this proves the refusal has something true to check.
#[test]
fn the_journal_mode_on_disk_is_delete() {
    let fx = Fixture::live_shaped();
    let conn = fx.conn();
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).expect("pragma");
    assert_eq!(mode.to_ascii_lowercase(), "delete", "the persisted journal mode is not DELETE");
    let version = user_version(&conn);
    assert_eq!(version, vike_secrets::SCHEMA_VERSION);
}

// ---------------------------------------------------------------------------------------------
// PROOF 5 — the modes
// ---------------------------------------------------------------------------------------------

/// **0600 on the database, 0700 on `settings/db` — set explicitly, never inherited.**
///
/// MEASURED on the live box 2026-09-13 and recorded in 0054: the umask there produces **0664**. A
/// database created without an explicit mode therefore lands group- and world-readable with every
/// venue key in it, which is why the store is created with an explicit mode.
///
/// Unix only — Windows has no mode bits and the equivalent question is an ACL query, which needs a
/// Win32 crate this workspace does not carry.
#[cfg(unix)]
#[test]
fn the_modes_are_0600_in_a_0700_directory() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::live_shaped();

    let file = std::fs::metadata(fx.db()).expect("stat db").permissions().mode() & 0o777;
    let dir = std::fs::metadata(fx.db().parent().unwrap()).expect("stat dir").permissions().mode()
        & 0o777;
    assert_eq!(file, 0o600, "the database is mode {file:04o}, not 0600");
    assert_eq!(dir, 0o700, "settings/db is mode {dir:04o}, not 0700");
}
