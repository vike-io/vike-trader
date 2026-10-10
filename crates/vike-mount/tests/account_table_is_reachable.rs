//! **The arming side can ASK the `account` table** — and on a box that has none it is told so,
//! rather than told there are no accounts.
//!
//! `vike-mount` links no `vike-secrets`; the store surfaces through
//! `vike_bridge_core::credentials`, as the credential MAP does, so "the mount can reach an
//! account-table reader" is proven HERE by compiling and answering.
//!
//! ⚠ **Nothing in THIS FILE arms, mounts or signs anything** — it proves the seam, not a mount.
//!
//! ⚠ **The reader has a consumer**: dukascopy's `make_engine_for_account` arm (#1845) keys on the
//! `account` rows it returns to pick the LEGAL ENTITY an order reaches, and the bridge declares
//! `addresses_accounts` itself (`crates/bridges/dukascopy/src/mount.rs`'s `DukascopyVenueMount`).
//! ⚠ A composition root does the READ and hands it down as `vike_mount::MountPolicy::accounts`: a
//! library opening the store from a process-global path is the class
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down.
//!
//! The property: every CI runner and fresh checkout has NO settings database. Answering
//! `Known(vec![])` there would read "no accounts" for a store holding every credential —
//! downstream, the LIVE GATE: every venue silently paper with `secrets.env` on disk. The third
//! state prevents that, and must survive the re-export.

use std::collections::HashMap;

use vike_bridge_core::credentials::{Accounts, NoAccountTable, load_workspace_accounts_from_env};

/// The one fact the loader takes from a process-environment sweep, spelled as every root spells it.
fn env_pointing_at(dir: &std::path::Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("VIKE_SETTINGS_DIR".to_string(), dir.to_str().expect("utf-8 temp path").to_string());
    env
}

/// **A box with no settings database says it CANNOT ANSWER, from the mount's side.** Both halves
/// matter: the call COMPILES in the crate that owns `make_engine`, and the answer is the third
/// state, not an empty list.
#[test]
fn the_mount_side_can_ask_and_an_unmigrated_box_says_it_cannot_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    // A credential FILE: a configured box, where "no accounts" is the WRONG answer.
    std::fs::write(settings.join("secrets.env"), "BINANCE_DEMO_API_KEY=not-a-real-key\n")
        .expect("write a file store");

    let answer =
        load_workspace_accounts_from_env(&env_pointing_at(&settings)).expect("a file store opens");

    assert_eq!(
        answer.known(),
        None,
        "a box with no store answered the mount with a row list: {answer:?}"
    );
    let NoAccountTable::NoStore { db } = answer.unanswerable().expect("a reason");
    assert!(db.ends_with("vike.db"), "{db:?}");

    // …and the per-venue shape an arming site reaches for: `None`, never an empty list
    // (`unwrap_or_default()` there is the bug this type makes visible).
    assert_eq!(answer.active_for_venue("dukascopy"), None);
    assert_eq!(answer.active_for_venue("binance"), None);
}

/// **The vocabulary crosses the re-export intact**: `Accounts`, `Account` and `NoAccountTable` are
/// nameable here. Re-exporting only the FUNCTION leaves no caller able to declare its return type,
/// which ends in a `let _ =` and a lost answer.
#[test]
fn the_account_vocabulary_is_nameable_from_the_mount() {
    let empty: Accounts = Accounts::Known(Vec::new());
    assert!(empty.known().is_some_and(<[_]>::is_empty), "{empty:?}");
    // An EMPTY answer is still an ANSWER: `Some(empty)`, not `None`.
    assert!(
        empty.active_for_venue("binance").is_some_and(|v| v.is_empty()),
        "an empty answer collapsed into `cannot ask`"
    );

    let row = vike_bridge_core::credentials::Account {
        id: 7,
        venue: "dukascopy".to_string(),
        tier: "demo".to_string(),
        // ⚠ NULL, and a reader may not synthesise one (owner refused DEMO1/DEMO2): labels are
        // optional, `id` is the identity.
        label: None,
        // ⚠ NOT YET KNOWN, never "this account has no book": until `vike-cli secrets set-book` or
        // `confirm` writes it (`vike_secrets::set_venue_account_id`), the column is NULL.
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        // DERIVED from `venue_arming` rows (`vike_secrets::Account::armed`); read by nothing here.
        armed: false,
    };
    let one = Accounts::Known(vec![row]);
    let duka = one.active_for_venue("dukascopy").expect("answered");
    assert_eq!(duka.len(), 1);
    assert_eq!(duka[0].id, 7);
}
