//! **A settings database the reading process can READ but not WRITE: what this crate's own reader
//! and writer do there.**
//!
//! The shape: the database file readable, and the file, its `db/` directory and the settings
//! directory all unwritable by the process. Two properties of THIS crate are pinned over it:
//!
//! 1. **the credential read path needs no write access at all.** `load_workspace_dotenv_from` reaches
//!    `crates/vike-secrets/src/db.rs`'s `open_for_read` — `SQLITE_OPEN_READ_ONLY`, creating nothing.
//!    If that ever started needing to write (a journal, a pragma that persists, a lazily-created
//!    file), the infallible reader would hand such a process an EMPTY map, which a caller cannot
//!    tell from "no credentials".
//! 2. **a write attempt is REFUSED and leaves every byte where it was.** A venue-rotated credential
//!    is persisted through `save_credentials_to_store` — cTrader's `token_store::persist` is the one
//!    production caller that no human initiates — so that is the writer driven here, against the
//!    same shape. It must fail loudly, change nothing in the database, leave no rollback journal
//!    behind, and NOT fall back to writing a credential file beside it.
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
use vike_secrets::{Backend, Table};

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

/// The rotation write: one existing credential replaced, through the production front door.
fn rotate(settings: &Path) -> std::io::Result<Backend> {
    vike_secrets::save_credentials_to_store(
        settings,
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated-by-a-smoke".to_string())],
        Some(&support::classify),
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
    let control = Fixture::migrated();
    assert_eq!(
        rotate(control.dir()).expect("on a WRITABLE store the same write must land"),
        Backend::Database(control.db()),
        "the control write did not land in the database, so a refusal below would prove nothing"
    );

    // The read-only shape: no credential file beside the database, the database file readable,
    // nothing writable — the file, `db/`, and the settings directory.
    let fx = Fixture::migrated();
    std::fs::remove_file(fx.store()).expect("drop the shadowed credential file");
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

    // 1. Both reads: the whole-table one and the demo-only scoped one.
    let vars = vike_secrets::load_workspace_dotenv_from(fx.arg());
    assert_eq!(
        vars.get("BINANCE_DEMO_API_KEY").map(String::as_str),
        Some(support::fake_value("BINANCE_DEMO_API_KEY").as_str()),
        "the credential read came back without the demo key on a store this process can READ but \
         not WRITE — so the read path needs write access, and such a process gets an empty map it \
         cannot tell from no credentials. Read keys: {:?}",
        vars.keys().collect::<Vec<_>>()
    );
    let (scoped, _withheld) =
        vike_secrets::resolve_store_demo_only_in(fx.dir()).expect("the demo-only read must open");
    assert!(
        scoped.secrets.keys().any(|k| k == "BINANCE_DEMO_API_KEY"),
        "the demo-only read lost the demo key on a read-only store: {:?}",
        scoped.secrets
    );

    // 2. The write a rotation would perform.
    let refused = rotate(fx.dir());
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
    assert!(
        !fx.store().exists(),
        "the refused write fell back to creating a credential FILE beside the database"
    );
    assert_eq!(
        vike_secrets::load_workspace_dotenv_from(fx.arg())
            .get("BINANCE_DEMO_API_KEY")
            .map(String::as_str),
        Some(support::fake_value("BINANCE_DEMO_API_KEY").as_str()),
        "after the refused write the reader no longer answers the ORIGINAL value"
    );
}
