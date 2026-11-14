//! The builder service's own tests: authentication and retention. Task 6 (Track B).
//!
//! None of these invoke `cargo` — they test the wire-authorization decision and the filesystem
//! retention pass directly, both pure functions in `vike_strategy_builder::builder` — so, unlike
//! `tests/build_errors.rs`, they run anywhere `cargo test -p vike-strategy-builder` runs, with no
//! `--ignored` flag needed:
//!
//! ```text
//! LANE=lane2 MSYS_NO_PATHCONV=1 just the latency box <branch> cargo test -p vike-strategy-builder --test builder_auth
//! ```

use std::path::Path;

use vike_hub_protocol::auth::{Domain, HubKeys, Scope, sign};
use vike_strategy_builder::builder::{
    DOMAIN, PROTO_VERSION, keys_from_credentials, prune, verify_auth,
};

/// No key configured at all — [`keys_from_credentials`] returns `None` before any socket ever opens, and
/// even a `HubKeys` with an empty `Write` slot refuses every mac: [`verify_auth`] checks
/// `keys.has(scope)` before it ever calls `verify`, so there is no key bytes for a forged mac to
/// coincidentally match.
#[test]
fn an_unauthenticated_build_request_is_refused() {
    let vars = std::collections::HashMap::new();
    assert!(keys_from_credentials(&vars).is_none(), "no stored builder key must mean no keys");

    let keys = HubKeys::new(Vec::new(), Vec::new());
    let nonce = [7u8; 32];
    // Even a mac "signed" with the empty key must not verify: `has(Write)` is false, so
    // `verify_auth` never even reaches the byte comparison.
    let forged = sign(DOMAIN, b"", &nonce, PROTO_VERSION, Scope::Write);
    assert!(!verify_auth(&keys, &nonce, PROTO_VERSION, Scope::Write, &forged));
}

/// A mac signed under a SIBLING service's domain (the same key bytes, the same scope, the same
/// nonce and protocol version — everything but the domain separator) must not authenticate here.
/// This is Decision 1 made concrete: a key minted for the tradehub control channel or the
/// datahub/compute domain must not be able to drive this compiler.
#[test]
fn a_key_for_another_domain_is_refused() {
    let key = b"a-real-builder-key";
    let keys = HubKeys::new(Vec::new(), key.to_vec());
    let nonce = [3u8; 32];

    // The tradehub's own separator, spelled as a literal exactly as
    // `vike_hub_protocol::auth`'s own `domain_separators_are_disjoint` test does. This
    // crate does not depend on `vike-tradehub-client` — a literal avoids pulling in a whole
    // sibling service's auth crate for one negative-test constant, and nothing in the layer
    // graph forbids the edge itself either way (`vike-tradehub-client` is layer 25, below this
    // crate's own 65) — so a literal is the honest, minimal spelling of "some other service's
    // domain" here too.
    let tradehub_domain = Domain::new(b"vike-tradehub-auth\0");
    let foreign_mac = sign(tradehub_domain, key, &nonce, PROTO_VERSION, Scope::Write);
    assert!(
        !verify_auth(&keys, &nonce, PROTO_VERSION, Scope::Write, &foreign_mac),
        "a mac signed under a sibling service's domain must not authenticate this one"
    );

    // The datahub/compute domain too, so this is a property of THIS domain being disjoint from
    // every sibling, not a one-off pairing.
    let datahub_domain = Domain::new(b"vike-datahub-auth\0");
    let datahub_mac = sign(datahub_domain, key, &nonce, PROTO_VERSION, Scope::Write);
    assert!(!verify_auth(&keys, &nonce, PROTO_VERSION, Scope::Write, &datahub_mac));

    // ...and the control: the SAME key, signed under THIS service's own domain, verifies — proof
    // the two failures above are about the DOMAIN and not about the scenario or the key.
    let real_mac = sign(DOMAIN, key, &nonce, PROTO_VERSION, Scope::Write);
    assert!(verify_auth(&keys, &nonce, PROTO_VERSION, Scope::Write, &real_mac));
}

/// **The retention rule.** A previous release's build folder goes whole; inside the current one the
/// newest `keep` artifacts and, apart, the newest `keep` recorded failures stay; a directory that is
/// not a build folder, and a file that is neither, are left alone.
#[test]
fn retention_drops_old_folders_and_keeps_the_newest_n_of_each_kind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (current, old) = ("0123abcd", "89abcdef");
    for i in 0..5 {
        plant(dir.path(), current, &format!("{i:064}.so"));
        plant(dir.path(), current, &format!("{i:064}.err"));
    }
    plant(dir.path(), current, "notes.txt");
    plant(dir.path(), old, &format!("{:064}.so", 9));
    plant(dir.path(), "not-a-folder", "kept.so");

    prune(dir.path(), current, 3).expect("prune");

    assert!(!dir.path().join(old).exists(), "a previous build folder goes whole");
    assert!(dir.path().join("not-a-folder/kept.so").exists(), "a foreign directory is left alone");
    assert!(dir.path().join(current).join("notes.txt").exists(), "a foreign file is left alone");
    for ext in ["so", "err"] {
        // Tied clocks (planted in one loop): the FILE NAME breaks the tie, descending, so "the
        // newest three" is indices 2, 3 and 4 of each kind.
        for i in 0..5 {
            let kept = dir.path().join(current).join(format!("{i:064}.{ext}")).exists();
            assert_eq!(kept, i >= 2, "{ext} {i}");
        }
    }
    prune(&dir.path().join("absent"), current, 3).expect("a missing builds directory is no error");
}

/// A file planted at `<dir>/<folder>/<name>`.
fn plant(dir: &Path, folder: &str, name: &str) {
    std::fs::create_dir_all(dir.join(folder)).expect("plant the folder");
    std::fs::write(dir.join(folder).join(name), b"fake").expect("plant the file");
}
