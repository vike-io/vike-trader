//! **A credentialed lane's test kit**: a REAL settings database planted in a test's own temp directory,
//! and the log lines a read logs, for the credentialed history lanes' suites on both sides of the
//! gatewayhub's crate boundary — each venue's bridge (`crate::credentialed_klines`' specs: their reads are
//! `pub(crate)`, they return plaintext keys) and the gatewayhub (`src/data/accounts_tests.rs` and the
//! `server` group's `tests/lanes/{ig,oanda}_credential.rs`, the same lanes through the real server).
//! Moved here from the gatewayhub's testkit with the lanes' venue code. Test-support only: never in a
//! normal build.
//!
//! Every fixture plants ACCOUNT ROWS and writes their FIELDS (decision 0156 item 6): no credential NAME is
//! composed or spelled, and every VALUE is an obviously fake, distinctive string, so "no value reached a
//! log line" is an assertion that could actually fail. The store is created with `vike_secrets::create_store`
//! (the one creator) and seeded through the writers' record-less `test-support` bodies, against a
//! directory the CALLER made (a `tempfile` dir): nothing here names or reaches a real settings directory.

use vike_model::AccountTier;

/// One planted account: its venue, tier and label, and the `(field, value)` pairs filed against it.
pub type Planted = (&'static str, AccountTier, Option<&'static str>, Vec<(String, String)>);

/// Bytes that are not a database where the database belongs: a store that EXISTS and will not open
/// (`chmod 000` proves nothing as root, which CI is).
pub fn plant_unreadable_store(settings: &std::path::Path) {
    std::fs::create_dir_all(settings.join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(settings), b"not a sqlite database at all")
        .expect("plant the garbage");
}

/// Make the settings DATABASE — the only credential store — hold exactly `accounts`, 0600 on unix so the
/// permission finding is only what a test asks for. The store is created the one way a store comes into
/// being (`vike-cli secrets init`'s library half); each account is created with its tier FIRST and its
/// fields are then written against it by owner and field — the one way an account's credential reaches a
/// store. A store being REPLACED (a rotation, a removal) is rebuilt in the test's own temp directory: the
/// writer adds and replaces fields and never removes one.
pub fn write_store(settings: &std::path::Path, accounts: &[Planted]) {
    let db_dir = settings.join("db");
    if db_dir.exists() {
        std::fs::remove_dir_all(&db_dir).expect("drop this test's own previous store");
    }
    vike_secrets::create_store(Some(settings.to_str().expect("a utf-8 temp path")))
        .expect("create the empty store");
    for &(venue, tier, label, ref fields) in accounts {
        let created = vike_secrets::edit_account_in_unjournalled(
            settings,
            vike_secrets::AccountEdit::Create { venue, tier, label },
        );
        let id = match created {
            Ok(created) => created.after.expect("a create returns its row").id,
            // A neighbour at another tier under the SAME address (an unlabelled demo and live row of one
            // venue): the writer refuses a second ACTIVE row there (decision 0274), and an older store may
            // still hold one, so the fixture plants it the way such a store holds it.
            Err(vike_secrets::StoreError {
                kind: vike_secrets::StoreErrorKind::AccountLabelInUse { .. },
                ..
            }) => vike_secrets::plant_account_for_test(settings, venue, tier, label),
            Err(e) => panic!("create the account: {e}"),
        };
        if !fields.is_empty() {
            vike_secrets::save_owned_credentials_unjournalled(
                settings,
                &vike_secrets::CredentialOwner::Account(id),
                fields,
                vike_secrets::CredentialUpsertIntent::Add,
            )
            .expect("write the account's fields");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            vike_secrets::db_path_in(settings),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("chmod");
    }
}

/// Every event's level, then every field — `message` first — as ` name=value`, one line per event: the
/// shape the credential suites' "no value reached a log line" assertions read (a value in an
/// `error = %e` field is as much a leak as one in the message). Projected from
/// `vike_log::capture::captured`, the workspace's one scoped capture.
pub fn under_capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let (out, events) = vike_log::capture::captured(f);
    let mut lines = Vec::with_capacity(events.len());
    for e in &events {
        let mut line = format!("{} message={}", e.level, e.message);
        for (k, v) in &e.fields {
            line.push_str(&format!(" {k}={v}"));
        }
        lines.push(line);
    }
    (out, lines)
}
