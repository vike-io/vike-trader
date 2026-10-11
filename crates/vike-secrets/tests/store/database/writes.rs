//! Proof 10 - a write reaches the store that answers, and no store means no write.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 10 — a WRITE reaches the store that answers
// ---------------------------------------------------------------------------------------------

/// **A write through the upsert lands where the READER looks.**
///
/// The sharpest writer is cTrader's OAuth persister, where a write that landed anywhere but the
/// store the readers open means the grant the VENUE rotated is lost at restart and that session
/// cannot re-authenticate.
///
/// Read back through `resolve_project` — the function every composition root reaches — rather than
/// through `read_table`, because the claim is about what a DAEMON would see and not about what the
/// row store happens to hold.
#[test]
fn a_write_is_read_back_by_the_resolver() {
    let fx = Fixture::live_shaped();

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
    // Every OTHER key is untouched — the upsert rule.
    assert_eq!(key_names(&resolved.secrets).len(), 67, "a write must not add or drop names");
    assert_eq!(
        value(&resolved.secrets, "BYBIT_DEMO_API_KEY").as_deref(),
        Some(fake_value("BYBIT_DEMO_API_KEY").as_str()),
        "an unrelated row moved"
    );
}

/// **With NO store the same call is REFUSED — and no database is created.**
///
/// A write with no database has nowhere to go and must say so rather than create a store that then
/// answers for every credential on the box. Both node keys and credentials take the same refusal.
#[test]
fn a_write_with_no_store_is_refused_and_creates_none() {
    let fx = Fixture::empty();
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Absent);

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
        assert!(said.contains("secrets init"), "{said}");
        assert!(!said.contains("rotated-nowhere"), "a refusal must never echo the value: {said}");
    }
    assert!(!fx.db().exists(), "a WRITE must never create a database");
}

/// **The node-key half of the same routing**, because a node-key write that landed elsewhere is
/// how a box silently keeps presenting the key it has just been told to rotate away from.
#[test]
fn a_node_key_write_reaches_the_table_when_the_database_answers() {
    let fx = Fixture::live_shaped();
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
/// enforced on the WRITE path.
///
/// Without it the invariant the two tables exist to buy could be broken by a writer while every
/// reader stayed green — and which value a reader then got would depend on which table it asked.
#[test]
fn a_write_may_not_give_a_name_a_second_namespace() {
    let fx = Fixture::live_shaped();

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

/// **A multi-line value is refused with a store and without one**, and it is refused FIRST — before
/// any store is asked.
///
/// SQLite would take it happily, which is exactly why it is refused: such a value cannot be typed on
/// the one line `vike-cli secrets set` reads, so the store must not grow a shape only this door can
/// make.
#[test]
fn a_multiline_value_is_refused_whichever_store_answers() {
    let updates = [("BINANCE_DEMO_API_KEY".to_string(), "one\ntwo".to_string())];

    let bare = Fixture::empty();
    let no_store_err = vike_secrets::save_credentials_to_store(
        bare.dir(),
        Table::Credential,
        &updates,
        Some(&classify),
    )
    .expect_err("a box with no store must refuse it");
    let fx = Fixture::live_shaped();
    let db_err = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &updates,
        Some(&classify),
    )
    .expect_err("the database must refuse the same thing");

    for e in [&no_store_err, &db_err] {
        assert!(e.to_string().contains("ONE line"), "{e}");
    }
    assert!(!bare.db().exists(), "the refusal created no store");
    assert_eq!(
        value(
            &vike_secrets::read_table(&fx.db(), Table::Credential).unwrap(),
            "BINANCE_DEMO_API_KEY"
        ),
        Some(fake_value("BINANCE_DEMO_API_KEY")),
        "the refused write must have changed nothing"
    );
}
