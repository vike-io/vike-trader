//! The builder service's own tests: authentication and retention. Task 6 (Track B).
//!
//! None of these invoke `cargo` — they test the wire-authorization decision and the filesystem
//! retention pass directly, both pure functions in `vike_strategy_builder::builder` — so, unlike
//! `tests/build_errors.rs`, they run anywhere `cargo test -p vike-strategy-builder` runs, with no
//! `--ignored` flag needed:
//!
//! ```text
//! LANE=the build runner MSYS_NO_PATHCONV=1 just the latency box <branch> cargo test -p vike-strategy-builder --test integration -- builder_auth::
//! ```

use std::path::Path;

use vike_hub_protocol::auth::{Domain, HubKeys, Scope, sign};
use vike_rust_strategy::sdk::Sdk;
use vike_strategy_builder::builder::{DOMAIN, PROTO_VERSION, prune, verify_auth};

/// No key configured at all — a `HubKeys` with an empty `Write` slot refuses every mac: [`verify_auth`]
/// checks `keys.has(scope)` before it ever calls `verify`, so there is no key bytes for a forged mac to
/// coincidentally match. (A box whose store holds no key never serves at all: `builder::run` refuses to
/// start, `builder_tests.rs` holds that.)
#[test]
fn an_unauthenticated_build_request_is_refused() {
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

/// **The retention rule, per SDK** (decision 0296). Inside the SDK's folder a build folder of other
/// sources goes whole, and inside each current one the `keep` artifacts and, apart, the `keep` recorded
/// failures used most recently stay; a work directory, another SDK's folders, a directory that is not a
/// build folder and a file that is neither are left alone; a folder of the layout before SDKs goes.
#[test]
fn retention_drops_old_folders_and_keeps_the_newest_n_of_each_kind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (current, other_kind, old) = ("sdk-3/0123abcd", "sdk-3/01234567", "sdk-3/89abcdef");
    for i in 0..5 {
        plant(dir.path(), current, &format!("{i:064}.so"));
        plant(dir.path(), current, &format!("{i:064}.err"));
    }
    plant(dir.path(), current, "notes.txt");
    plant(dir.path(), other_kind, &format!("{:064}.so", 8));
    plant(dir.path(), old, &format!("{:064}.so", 9));
    plant(dir.path(), "sdk-3/work-0123abcd", "Cargo.toml");
    plant(dir.path(), "sdk-2/89abcdef", &format!("{:064}.so", 9));
    plant(dir.path(), "fedcba98", &format!("{:064}.so", 9));
    plant(dir.path(), "not-a-folder", "kept.so");

    let folders = ["0123abcd".to_string(), "01234567".to_string()];
    prune(dir.path(), Sdk::Number(3), &folders, 3).expect("prune");

    assert!(!dir.path().join(old).exists(), "a previous build folder goes whole");
    assert!(dir.path().join(other_kind).exists(), "every current folder (one a kind) stays");
    assert!(dir.path().join("sdk-3/work-0123abcd").exists(), "a work directory is the sweep's");
    assert!(dir.path().join("sdk-2/89abcdef").exists(), "another SDK's folders are left alone");
    assert!(!dir.path().join("fedcba98").exists(), "a folder of the layout before SDKs goes");
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
    prune(&dir.path().join("absent"), Sdk::Dev, &folders, 3)
        .expect("a missing builds directory is no error");
}

/// **Retention evicts by LAST USE, not by build time.** An artifact built long ago and used a moment ago
/// (a cache hit stamps the access time) outlives one built later and never used since; with no use
/// stamp, the build time is the last use. The same for a recorded failure.
#[test]
fn retention_keeps_what_was_used_last_not_what_was_built_last() {
    use std::time::{Duration, SystemTime};
    let dir = tempfile::tempdir().expect("tempdir");
    let current = "sdk-dev/0123abcd";
    let day = |n: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + n * 86_400);
    for ext in ["so", "err"] {
        // (name, built, last used)
        for (name, built, used) in
            [("old_in_use", 1, 9), ("newer_unused", 5, 5), ("newest_unused", 6, 6)]
        {
            let file = format!("{name}.{ext}");
            plant(dir.path(), current, &file);
            let times = std::fs::FileTimes::new().set_modified(day(built)).set_accessed(day(used));
            std::fs::File::options()
                .write(true)
                .open(dir.path().join(current).join(&file))
                .and_then(|f| f.set_times(times))
                .expect("set the planted file's times");
        }
    }

    prune(dir.path(), Sdk::Dev, &["0123abcd".to_string()], 2).expect("prune");

    for ext in ["so", "err"] {
        let kept = |name: &str| dir.path().join(current).join(format!("{name}.{ext}")).exists();
        assert!(kept("old_in_use"), "{ext}: built first, used last: kept");
        assert!(kept("newest_unused"), "{ext}: built last, never used since: kept by its build");
        assert!(!kept("newer_unused"), "{ext}: the least recently used goes");
    }
}

/// A file planted at `<dir>/<folder>/<name>`.
fn plant(dir: &Path, folder: &str, name: &str) {
    std::fs::create_dir_all(dir.join(folder)).expect("plant the folder");
    std::fs::write(dir.join(folder).join(name), b"fake").expect("plant the file");
}
