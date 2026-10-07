//! Proofs 1-5 - the round trip, byte-identical sources, idempotence, no sidecar, the modes.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 1 — all 67 names survive the round trip
// ---------------------------------------------------------------------------------------------

/// **Every key NAME in a live-shaped store comes back out of the database, and the SETS are equal.**
///
/// The failure this is written against is the one the brief calls the worst outcome available: a
/// migration that carries the ten names the generated grid knows about and silently drops the
/// fifty-seven bespoke ones. Set equality both ways is what catches it — a subset assertion would
/// pass on a migration that dropped half the store, and a COUNT assertion would pass on a migration
/// that invented names.
#[test]
fn all_67_key_names_survive_the_round_trip() {
    let fx = Fixture::live_shaped();
    let report = fx.migrate();

    let creds = vike_secrets::read_table(&fx.db(), Table::Credential).expect("credential table");
    let nodes = vike_secrets::read_table(&fx.db(), Table::NodeKey).expect("node_key table");

    let want_creds: BTreeSet<String> =
        LIVE_CREDENTIAL_KEYS.iter().map(|k| (*k).to_string()).collect();
    let want_nodes: BTreeSet<String> = LIVE_NODE_KEYS.iter().map(|k| (*k).to_string()).collect();

    assert_eq!(want_creds.len(), 67, "the fixture is the live store's 67 names");
    assert_eq!(
        key_names(&creds),
        want_creds,
        "the credential table is not the credential file's name set\n{report}"
    );
    assert_eq!(key_names(&nodes), want_nodes, "the node_key table is not the node file's name set");
    assert_eq!(creds.len(), 67, "67 credential rows");
    assert_eq!(nodes.len(), 4, "4 node-key rows");
    assert_eq!(report.keys_read(), 71, "67 credentials + 4 node keys read");
    assert_eq!(report.inserted(), 71, "…and all 71 inserted on the first run");

    // And the VALUES round-tripped too, so this is a migration rather than a name census.
    let back = creds.into_map();
    for k in LIVE_CREDENTIAL_KEYS {
        assert_eq!(back.get(k), Some(&fake_value(k)), "{k} came back changed");
    }
}

/// The same 67, reached through the PRODUCTION read path rather than through `read_table`.
///
/// Separate from the test above on purpose: that one proves the writer, this one proves that
/// `resolve_project` — the function every composition root reaches through — answers with the same
/// set. A migration that filled a table nothing reads would pass the first and fail this.
#[test]
fn the_production_resolver_answers_with_all_67() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the database answered");
    assert_eq!(
        key_names(&resolved.secrets),
        LIVE_CREDENTIAL_KEYS.iter().map(|k| (*k).to_string()).collect::<BTreeSet<_>>()
    );
    assert_eq!(resolved.secrets.len(), 67);
}

// ---------------------------------------------------------------------------------------------
// PROOF 2 — the files are byte-identical afterwards
// ---------------------------------------------------------------------------------------------

/// **The operator's only copy of their live venue keys is not touched — proven by bytes.**
///
/// The rule this enforces outranks everything else in the landing: nothing in this workspace
/// deletes, moves, truncates or wholesale-rewrites a credential file, and a migration is the exact
/// place somebody reaches for "…and then tidy it up". Byte comparison rather than "the keys are
/// still there", because a re-render that preserved every key while dropping the operator's comments
/// and their line ORDER would pass the weaker check and would still have destroyed something they
/// wrote.
#[test]
fn the_source_files_are_byte_identical_afterwards() {
    let fx = Fixture::live_shaped();

    let (secrets_before, node_before) =
        (std::fs::read(fx.store()).unwrap(), std::fs::read(fx.node()).unwrap());
    let (ds_before, dn_before) = (digest(&fx.store()), digest(&fx.node()));

    fx.migrate();

    let (ds_after, dn_after) = (digest(&fx.store()), digest(&fx.node()));
    assert_eq!(ds_before, ds_after, "the credential file changed: {ds_before} -> {ds_after}");
    assert_eq!(dn_before, dn_after, "the node file changed: {dn_before} -> {dn_after}");
    assert_eq!(std::fs::read(fx.store()).unwrap(), secrets_before, "credential file bytes");
    assert_eq!(std::fs::read(fx.node()).unwrap(), node_before, "node file bytes");
    assert!(fx.store().exists() && fx.node().exists(), "and neither file was removed");
}

// ---------------------------------------------------------------------------------------------
// PROOF 3 — twice is the same as once
// ---------------------------------------------------------------------------------------------

/// **A second migration changes nothing, down to the database's bytes.**
///
/// Idempotence stated at the level of rows would be satisfied by a second run that rewrote every row
/// with the same value — which touches the file, could churn a page, and would make "did anything
/// happen here" unanswerable from a `stat`. `migrate` is written so that a run with nothing pending
/// opens NO write connection at all, and this is the assertion that holds it to that.
#[test]
fn twice_is_the_same_as_once() {
    let fx = Fixture::live_shaped();

    let first = fx.migrate();
    assert_eq!(
        first.outcome,
        vike_secrets::MigrationOutcome::Created,
        "the first run creates the database"
    );
    assert_eq!(first.inserted(), 71);
    let after_first = (digest(&fx.db()), std::fs::read(fx.db()).unwrap());

    let second = fx.migrate();
    assert_eq!(
        second.outcome,
        vike_secrets::MigrationOutcome::AlreadyComplete,
        "the second run finds it already there AND already complete"
    );
    assert_eq!(second.inserted(), 0, "…and inserts nothing:\n{second}");
    assert_eq!(second.keys_read(), 71, "it still READ all 71 — it just had nothing to do");

    let after_second = (digest(&fx.db()), std::fs::read(fx.db()).unwrap());
    assert_eq!(after_first.0, after_second.0, "database digest moved on a no-op run");
    assert_eq!(after_first.1, after_second.1, "database bytes moved on a no-op run");
}

// ---------------------------------------------------------------------------------------------
// PROOF 4 — no sidecar at rest
// ---------------------------------------------------------------------------------------------

/// **`settings/db/` holds exactly one file after a migration and a clean close.**
///
/// This is what `journal_mode = DELETE` was chosen for, and 0054's constraint 1 was amended to
/// require it, so it is proven rather than trusted. A `vike.db-wal` beside the store would be a
/// SECOND plaintext credential artifact, would make a daemon-down read impossible in the read-only
/// `settings/` the deployed unit mounts, and would turn one `chmod 600` into a check over a set —
/// and it would announce itself as nothing but two extra files nobody looks at.
#[test]
fn no_sidecar_survives_a_clean_close() {
    let fx = Fixture::live_shaped();
    fx.migrate();

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
/// checkpointed and closed, so the pragma is asked directly. `migrate` already refuses when the
/// engine answers anything but `delete`; this proves the refusal has something true to check.
#[test]
fn the_journal_mode_on_disk_is_delete() {
    let fx = Fixture::live_shaped();
    fx.migrate();
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
/// venue key in it. `vike_secrets::save_credentials` has set an explicit mode on a store it creates
/// for exactly this reason since the incident 0036 records, and this is the same posture on the new
/// artifact.
///
/// Unix only — Windows has no mode bits and the equivalent question is an ACL query, which needs a
/// Win32 crate this workspace does not carry.
#[cfg(unix)]
#[test]
fn the_modes_are_0600_in_a_0700_directory() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::live_shaped();
    fx.migrate();

    let file = std::fs::metadata(fx.db()).expect("stat db").permissions().mode() & 0o777;
    let dir = std::fs::metadata(fx.db().parent().unwrap()).expect("stat dir").permissions().mode()
        & 0o777;
    assert_eq!(file, 0o600, "the database is mode {file:04o}, not 0600");
    assert_eq!(dir, 0o700, "settings/db is mode {dir:04o}, not 0700");
}
