//! **The arming side can ASK the `account` table** — and on a box that has none it is told so,
//! rather than told there are no accounts.
//!
//! `vike-mount` links no `vike-secrets` of its own; the store surfaces for it through
//! `vike_bridge_core::credentials`, exactly as the credential MAP already does. So the claim *the
//! account table has a reader the mount can reach* is only true if it compiles and answers from
//! HERE, and this file is where that is proven rather than asserted in a doc comment.
//!
//! ⚠ **Nothing in THIS FILE arms, mounts or signs anything** — it proves the seam, not a mount.
//!
//! ⚠ **The step it was landed ahead of has landed.** This doc said
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §12 barred anything from joining
//! `crates/vike-mount/src/arming.rs`'s `arm_addresses_accounts` before that venue's
//! `make_engine_for_account` arm threaded an account into an account-aware loader. Dukascopy's arm
//! does exactly that now (#1845), keyed on the `account` rows this reader returns, and the venue
//! joined that list — so the reader below is no longer a capability waiting for a consumer: it
//! decides which LEGAL ENTITY a Dukascopy order reaches. ⚠ The READ itself has also moved: a
//! composition root performs it and hands the result down as `vike_mount::MountPolicy::accounts`,
//! because a library opening the credential store at a path taken from a process global is the class
//! `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down. What this
//! file proves is unchanged and is the reason that move was possible: the seam ANSWERS, from this
//! side of the re-export, in the third state.
//!
//! # The property being held
//!
//! Every CI runner and every fresh checkout is a box with NO settings database. If the reader
//! answered `Known(vec![])` there, the first caller to reach for it from the mount would read *this
//! venue has no accounts* about a store holding every credential it has ever held — and downstream
//! that is the LIVE GATE: every venue silently on paper with `secrets.env` sitting on disk looking
//! exactly right. The third state is what makes that impossible, and it has to survive the trip
//! through the re-export to be worth anything.

use std::collections::HashMap;

use vike_bridge_core::credentials::{Accounts, NoAccountTable, load_workspace_accounts_from_env};

/// The one fact the loader takes out of a process-environment sweep — spelled as the literal, the
/// way every root's own read of it is spelled.
fn env_pointing_at(dir: &std::path::Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("VIKE_SETTINGS_DIR".to_string(), dir.to_str().expect("utf-8 temp path").to_string());
    env
}

/// **A box with no settings database says it CANNOT ANSWER, from the mount's own side of the
/// re-export.**
///
/// Two things at once, and both are the point: the call COMPILES here (so the seam is genuinely
/// reachable from the crate that owns `make_engine`), and the answer is the third state rather than
/// an empty list.
#[test]
fn the_mount_side_can_ask_and_an_unmigrated_box_says_it_cannot_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    // A credential FILE, so this is a configured box rather than an empty one — which is what makes
    // "no accounts" the wrong answer rather than an unremarkable one.
    std::fs::write(settings.join("secrets.env"), "BINANCE_DEMO_API_KEY=not-a-real-key\n")
        .expect("write a file store");

    let answer =
        load_workspace_accounts_from_env(&env_pointing_at(&settings)).expect("a file store opens");

    assert_eq!(
        answer.known(),
        None,
        "an unmigrated box answered the mount with a row list: {answer:?}"
    );
    match answer.unanswerable().expect("a reason") {
        NoAccountTable::FileStore { file } => assert!(file.ends_with("secrets.env"), "{file:?}"),
        other => panic!("an unmigrated box reported {other:?}"),
    }

    // …and the per-venue shape, which is the one an arming site would actually reach for. `None`,
    // never an empty list — `unwrap_or_default()` at such a call site is the bug this type exists
    // to make visible.
    assert_eq!(answer.active_for_venue("dukascopy"), None);
    assert_eq!(answer.active_for_venue("binance"), None);
}

/// **The vocabulary crosses the re-export intact** — `Accounts`, `Account` and `NoAccountTable` are
/// nameable here, so a future arming site can hold one in a variable and match on it.
///
/// A re-export that landed only the FUNCTION would compile every call and leave no caller able to
/// declare the type it returns, which is the shape that ends in a `let _ =` and a lost answer.
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
        // ⚠ NULL, and a reader may not synthesise one — the owner refused the provisional
        // DEMO1/DEMO2 labels at the spec's signature: labels are informative and optional, `id` is
        // the identity.
        label: None,
        // ⚠ NOT YET KNOWN, never "this account has no book" — §11 steps 3-4 are unperformed, so
        // nothing in the tree writes this column yet.
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
    };
    let one = Accounts::Known(vec![row]);
    let duka = one.active_for_venue("dukascopy").expect("answered");
    assert_eq!(duka.len(), 1);
    assert_eq!(duka[0].id, 7);
}
