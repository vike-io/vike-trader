//! Proofs 10-11 - a write reaches the store that answers, and a mixed re-migration.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 10 — a WRITE reaches the store that answers
// ---------------------------------------------------------------------------------------------

/// **On a migrated project, a write through the upsert lands where the READER looks.**
///
/// The defect: every production credential write named a FILE, and on a migrated box that file is
/// shadowed. The write succeeded, the file genuinely changed, the change journal recorded it, the
/// caller reported success — and no reader ever opened that file again. The sharpest instance is
/// cTrader's OAuth persister, where a shadowed write means the grant the VENUE rotated is lost at
/// restart and that session cannot re-authenticate.
///
/// Read back through `resolve_project` — the function every composition root reaches — rather than
/// through `read_table`, because the claim is about what a DAEMON would see and not about what the
/// row store happens to hold.
#[test]
fn a_write_on_a_migrated_project_is_read_back_by_the_resolver() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let file_before = std::fs::read(fx.store()).expect("read the file");

    let landed = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated-by-the-writer".to_string())],
        Some(&classify),
    )
    .expect("the write must succeed");
    assert_eq!(landed, Backend::Database(fx.db()), "it must report where it landed");

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(
        value(&resolved.secrets, "BINANCE_DEMO_API_KEY").as_deref(),
        Some("rotated-by-the-writer"),
        "the write did not reach the store that answers"
    );
    // Every OTHER key is untouched — the upsert rule, on the database branch.
    assert_eq!(key_names(&resolved.secrets).len(), 67, "a write must not add or drop names");
    assert_eq!(
        value(&resolved.secrets, "BYBIT_DEMO_API_KEY").as_deref(),
        Some(fake_value("BYBIT_DEMO_API_KEY").as_str()),
        "an unrelated row moved"
    );

    // ⚠ THE RULE THAT OUTRANKS THE REST: the credential file is the operator's only copy of their
    // live venue keys, and a write that routed past it must not have touched it either.
    assert_eq!(
        std::fs::read(fx.store()).expect("read the file"),
        file_before,
        "the shadowed credential file was modified by a write that did not go to it"
    );
}

/// **On an UNMIGRATED project the same call is REFUSED — no file is written and no database
/// created.**
///
/// The other half of the routing claim. Until 2026-10-07 this landed in `secrets.env` through the
/// byte-preserving file writer; that writer and the file store are gone, so a write with no database
/// has nowhere to go and must say so rather than leave a key in a file nothing reads. Both node keys
/// and credentials take the same refusal.
#[test]
fn a_write_on_an_unmigrated_project_is_refused_and_touches_no_file() {
    let fx = Fixture::live_shaped();
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Absent);
    let before = (digest(&fx.store()), digest(&fx.node()));

    for (table, name) in
        [(Table::Credential, "BINANCE_DEMO_API_KEY"), (Table::NodeKey, "VIKE_TRADEHUB_OBSERVE_KEY")]
    {
        let refused = vike_secrets::save_credentials_to_store(
            fx.dir(),
            table,
            &[(name.to_string(), "rotated-nowhere".to_string())],
            Some(&classify),
        )
        .expect_err("with no database there is no store to write");
        let said = refused.to_string();
        assert!(said.contains("migrate --init"), "{said}");
        assert!(!said.contains("rotated-nowhere"), "a refusal must never echo the value: {said}");
    }
    assert!(!fx.db().exists(), "a WRITE must never create a database");
    assert_eq!((digest(&fx.store()), digest(&fx.node())), before, "no credential FILE may change");
}

/// **The node-key half of the same routing**, because a shadowed node-key write is how a box
/// silently keeps presenting the key it has just been told to rotate away from.
#[test]
fn a_node_key_write_reaches_the_table_when_the_database_answers() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let credentials_before = vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().len();

    vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::NodeKey,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "rotated".to_string())],
        None,
    )
    .expect("write");

    let resolved = vike_secrets::resolve_node_keys(fx.arg(), is_node_key).expect("node keys");
    assert_eq!(resolved.source, Source::Database(fx.db()));
    assert_eq!(value(&resolved.secrets, "VIKE_TRADEHUB_OBSERVE_KEY").as_deref(), Some("rotated"));
    // The credential table is untouched — the two namespaces stay disjoint across a write.
    assert_eq!(
        vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().len(),
        credentials_before,
        "a node-key write must not add a row to the credential table"
    );
}

/// **A writer may not give a name a second home**, which is `docs/decisions/0051`'s static predicate
/// enforced on the WRITE path rather than on the migration's alone.
///
/// Without it the invariant the two tables exist to buy could be broken by a writer while every
/// reader stayed green — and which value a reader then got would depend on which table it asked.
#[test]
fn a_write_may_not_give_a_name_a_second_namespace() {
    let fx = Fixture::live_shaped();
    fx.migrate();

    let err = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "wrong-namespace".to_string())],
        Some(&classify),
    )
    .expect_err("a name already in `node_key` may not be written into `credential`");
    let said = err.to_string();
    assert!(said.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{said}");
    assert!(said.contains("ONE home"), "{said}");
    assert!(!said.contains("wrong-namespace"), "a refusal printed a VALUE: {said}");

    assert!(
        value(
            &vike_secrets::read_table(&fx.db(), Table::Credential).unwrap(),
            "VIKE_TRADEHUB_OBSERVE_KEY"
        )
        .is_none(),
        "nothing was written"
    );
}

/// **A multi-line value is refused on BOTH backends**, so a key cannot round-trip on a migrated box
/// and be rejected on an unmigrated one.
///
/// SQLite would take it happily. The file's grammar cannot represent it, so accepting it in the
/// database would make the two stores answer differently about what a valid credential is — which
/// is exactly the divergence the per-RUN backend choice exists to prevent.
#[test]
fn a_multiline_value_is_refused_whichever_store_answers() {
    let fx = Fixture::live_shaped();
    let updates = [("BINANCE_DEMO_API_KEY".to_string(), "one\ntwo".to_string())];

    let file_err = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &updates,
        Some(&classify),
    )
    .expect_err("the file branch must refuse");
    fx.migrate();
    let db_err = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &updates,
        Some(&classify),
    )
    .expect_err("the database branch must refuse the same thing");

    for e in [&file_err, &db_err] {
        assert!(e.to_string().contains("ONE line"), "{e}");
    }
    assert_eq!(
        value(
            &vike_secrets::read_table(&fx.db(), Table::Credential).unwrap(),
            "BINANCE_DEMO_API_KEY"
        ),
        Some(fake_value("BINANCE_DEMO_API_KEY")),
        "the refused write must have changed nothing"
    );
}

// ---------------------------------------------------------------------------------------------
// PROOF 11 — a MIXED re-migration
// ---------------------------------------------------------------------------------------------

/// **A new key lands, a disagreeing key is refused and NAMED, and nothing is overwritten.**
///
/// The second door the same finding opened: re-running the migration to absorb an edit was refused
/// WHOLE-RUN on any `DisagreesWithDatabase`, so an unrelated brand-new key added in the same edit
/// did not land either. The operator's only way forward was to hand-edit the credential file — the
/// one file this workspace promises never to require editing away from.
///
/// The refusal itself is unchanged and is the point: the stored value for the disagreeing key is
/// still exactly what it was, and the key is still named.
#[test]
fn a_mixed_re_migration_lands_the_new_key_and_refuses_only_the_disagreeing_one() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "BINANCE_DEMO_API_KEY=first\nOKX_DEMO_API_KEY=okx-one\n").unwrap();
    let first = fx.migrate();
    assert_eq!(first.outcome, vike_secrets::MigrationOutcome::Created);
    assert_eq!(first.inserted(), 2);

    // The operator's edit: one value changed (rotated in the file, which is no longer read), one key
    // added that the database has never heard of, one key untouched.
    std::fs::write(
        fx.store(),
        "BINANCE_DEMO_API_KEY=second\nOKX_DEMO_API_KEY=okx-one\nBYBIT_DEMO_API_KEY=bybit-new\n",
    )
    .unwrap();

    let report = fx.migrate();
    assert_eq!(
        report.outcome,
        vike_secrets::MigrationOutcome::Updated,
        "the run must SUCCEED and land what it can: {report}"
    );

    let stored = vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().into_map();
    // LANDS: the brand-new key.
    assert_eq!(
        stored.get("BYBIT_DEMO_API_KEY").map(String::as_str),
        Some("bybit-new"),
        "the unambiguous new key did not land: {report}"
    );
    // REFUSED, and NOT overwritten: the disagreeing key keeps the value already stored.
    assert_eq!(
        stored.get("BINANCE_DEMO_API_KEY").map(String::as_str),
        Some("first"),
        "a disagreeing key was overwritten — nothing here can tell which value is newer"
    );
    // UNTOUCHED: the key that agreed.
    assert_eq!(stored.get("OKX_DEMO_API_KEY").map(String::as_str), Some("okx-one"));
    assert_eq!(stored.len(), 3);

    // …and the refusal is NAMED rather than swallowed by the success.
    assert_eq!(
        report.refused,
        vec![vike_secrets::Ambiguity::DisagreesWithDatabase {
            key: "BINANCE_DEMO_API_KEY".to_string(),
            table: Table::Credential
        }]
    );
    let said = report.to_string();
    assert!(said.contains("REFUSED"), "{said}");
    assert!(said.contains("BINANCE_DEMO_API_KEY"), "{said}");
    for planted in ["=first", "=second", "bybit-new"] {
        assert!(!said.contains(planted), "a report printed a credential VALUE: {said}");
    }
}

/// **The three WHOLE-RUN refusals still refuse the whole run**, so the split above did not quietly
/// widen what a migration will do.
#[test]
fn the_whole_run_refusals_are_still_whole_run() {
    let fx = Fixture::empty();
    // A brand-new, perfectly unambiguous key sits beside a name the node file carries that the
    // predicate does not claim. NOTHING may land.
    std::fs::write(fx.store(), "OKX_DEMO_API_KEY=okx\n").unwrap();
    std::fs::write(fx.node(), "BINANCE_DEMO_API_KEY=b\n").unwrap();

    let found = refusal(&fx);
    assert_eq!(found.len(), 1);
    assert!(!fx.db().exists(), "a whole-run refusal must write nothing at all");
}
