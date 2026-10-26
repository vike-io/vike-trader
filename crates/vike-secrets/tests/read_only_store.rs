//! **A settings database the reading process can READ but not WRITE: what this crate's own reader
//! and writer do there.**
//!
//! The shape: the database file readable, and the file, its `db/` directory and the settings
//! directory all unwritable by the process. Two properties of THIS crate are pinned over it:
//!
//! 1. **the credential read path needs no write access at all.** The whole-map read and the
//!    account credential book both reach `crates/vike-secrets/src/db/open.rs`'s `open_for_read` —
//!    `SQLITE_OPEN_READ_ONLY`, creating nothing. If that ever started needing to write (a journal,
//!    a pragma that persists, a lazily-created file), the infallible reader would hand such a
//!    process an EMPTY map, which a caller cannot tell from "no credentials".
//! 2. **a write attempt is REFUSED and leaves every byte where it was.** A venue-rotated credential
//!    is persisted against its ACCOUNT through the owner-addressed writer
//!    (`save_owned_credentials*`, `UpsertIntent::Replace`) — cTrader's token store is the one
//!    production caller that no human initiates — so that is the writer driven here, against the
//!    same shape. It must fail loudly, change nothing in the database, and leave no rollback
//!    journal behind.
//!
//! The control is a writable twin built the same way, on which the identical call LANDS: without it
//! a refusal here could be the call's own validation rather than the permissions.
//!
//! Unix only, and it skips LOUDLY where the permission does not take (a run as root): mode bits are
//! the whole subject, and a Windows box has none.
#![cfg(unix)]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use support::Fixture;
use vike_secrets::{CredentialOwner, OwnedWrite, UpsertIntent};

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

/// The rotation write: account `id`'s existing `API_KEY` replaced, through the production front
/// door.
fn rotate(settings: &Path, id: i64) -> std::io::Result<OwnedWrite> {
    vike_secrets::save_owned_credentials_unjournalled(
        settings,
        &CredentialOwner::Account(id),
        &[("API_KEY".to_string(), "rotated-by-a-smoke".to_string())],
        UpsertIntent::Replace,
    )
}

/// Restores owner-write on drop, so the bound tempdir can delete what a case locked.
struct Locked<'a>(&'a Fixture);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        let dir = self.0.dir();
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(dir.join("db"), std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(self.0.db(), std::fs::Permissions::from_mode(0o600));
    }
}

#[test]
fn a_store_the_process_cannot_write_still_answers_reads_and_refuses_the_rotation_writer() {
    // The CONTROL: on a writable twin, the identical write lands where the reader looks.
    let control = Fixture::seeded();
    let id = control.id_of("binance", "demo");
    let landed = rotate(control.dir(), id).expect("on a WRITABLE store the same write must land");
    assert_eq!(
        landed.names,
        [control.key("binance", "demo", "API_KEY")],
        "the control write did not land on the account's row, so a refusal below would prove nothing"
    );

    // The read-only shape: the database file readable, nothing writable — the file, `db/`, and the
    // settings directory.
    let fx = Fixture::seeded();
    let id = fx.id_of("binance", "demo");
    let key = fx.key("binance", "demo", "API_KEY");
    let _restore = Locked(&fx);
    set_mode(&fx.db(), 0o444);
    set_mode(&fx.dir().join("db"), 0o555);
    set_mode(fx.dir(), 0o555);
    if std::fs::OpenOptions::new().append(true).open(fx.db()).is_ok() {
        eprintln!(
            "SKIP: a write open of a 0444 file in a 0555 directory SUCCEEDED, so this process \
             ignores mode bits (root). The read-only shape cannot be built here."
        );
        return;
    }
    let before = std::fs::read(fx.db()).expect("the database is readable");

    // 1. The reads: the whole-table one, the account credential book and the no-live-keys one.
    let vars = crate::support::store_from(fx.arg())
        .read_all()
        .map(|r| r.secrets.into_map())
        .unwrap_or_default();
    assert_eq!(
        vars.get(&key).map(String::as_str),
        Some(support::fake_value(&key).as_str()),
        "the credential read came back without the demo key on a store this process can READ but \
         not WRITE — so the read path needs write access, and such a process gets an empty map it \
         cannot tell from no credentials. Read keys: {:?}",
        vars.keys().collect::<Vec<_>>()
    );
    let book = support::store_in(fx.dir())
        .credential_book()
        .expect("the credential book must open on a store this process cannot write");
    assert_eq!(
        book.account(id).and_then(|f| f.get("API_KEY")),
        Some(support::fake_value(&key).as_str()),
        "the account's own field came back from a read-only store"
    );
    let scoped = support::scoped_store_in(fx.dir(), vike_secrets::CredentialScope::NoLiveKeys)
        .read_all()
        .expect("the no-live-keys read must open");
    assert!(
        scoped.secrets.keys().any(|k| k == key),
        "the no-live-keys read lost the demo key on a read-only store: {:?}",
        scoped.secrets
    );

    // 2. The write a rotation would perform.
    let refused = rotate(fx.dir(), id);
    assert!(
        refused.is_err(),
        "⚠ the rotation writer SUCCEEDED against a store this process cannot write ({refused:?})."
    );
    assert_eq!(
        std::fs::read(fx.db()).expect("still readable"),
        before,
        "the refused write changed the database's bytes"
    );
    assert!(!fx.dir().join("db").join("vike.db-journal").exists(), "a rollback journal was left");
    assert_eq!(
        crate::support::store_from(fx.arg())
            .read_all()
            .map(|r| r.secrets.into_map())
            .unwrap_or_default()
            .get(&key)
            .map(String::as_str),
        Some(support::fake_value(&key).as_str()),
        "after the refused write the reader no longer answers the ORIGINAL value"
    );
}
