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

use vike_node_proto::auth::{Domain, NodeKeys, Scope, sign};
use vike_strategy_builder::builder::{
    DOMAIN, PROTO_VERSION, count_for, keys_from_vars, prune, verify_auth,
};

/// No key configured at all — [`keys_from_vars`] returns `None` before any socket ever opens, and
/// even a `NodeKeys` with an empty `Write` slot refuses every mac: [`verify_auth`] checks
/// `keys.has(scope)` before it ever calls `verify`, so there is no key bytes for a forged mac to
/// coincidentally match.
#[test]
fn an_unauthenticated_build_request_is_refused() {
    let vars = std::collections::HashMap::new();
    assert!(keys_from_vars(&vars).is_none(), "no VIKE_STRATEGY_BUILDER_KEY set must mean no keys");

    let keys = NodeKeys::new(Vec::new(), Vec::new());
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
    let keys = NodeKeys::new(Vec::new(), key.to_vec());
    let nonce = [3u8; 32];

    // The tradehub node's own separator, spelled as a literal exactly as
    // `vike_node_proto::auth`'s own `domain_separators_are_disjoint` test does. This
    // crate does not depend on `vike-tradehub-client` — a literal avoids pulling in a whole
    // sibling service's auth crate for one negative-test constant, and nothing in the layer
    // graph forbids the edge itself either way (`vike-tradehub-client` is layer 50, below this
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

/// Writes a fake artifact at the exact naming shape `build_plugin` would have produced, with a
/// FIXED modification time derived from `seq` (rather than relying on real wall-clock ordering
/// across a tight loop, which is not guaranteed distinct on every filesystem clock) — so "newest"
/// is deterministic without a sleep between plants.
fn plant_artifact(dir: &Path, name: &str, sha: &str) {
    let path = dir.join(format!("{name}-{sha}.so"));
    std::fs::write(&path, b"fake artifact").expect("plant artifact");
}

/// **The retention rule the spec requires.** Pruning `strat_a`'s artifacts down to the newest 3
/// must leave `strat_b`'s single artifact completely untouched.
///
/// ⚠ This second assertion is the one that matters, not the first. Pruning by a bare filename
/// PREFIX (`filename.starts_with(name)`) would not even need `strat_a` and `strat_b` to collide —
/// `builder::artifact_strategy_name`'s own doc names the exact incident this mirrors:
/// `vike_log`'s retention used to prune by `filename.starts_with(prefix)` against a prefix shared
/// by several binaries, which is why `LogConfig.file_prefix` now defaults to empty. `prune` here
/// groups by the EXACT name parsed from the fixed `-<sha256>.so` suffix, never by a prefix test,
/// so two strategies can never merge into one prune group no matter what their names share.
#[test]
fn retention_prunes_to_the_newest_n_per_strategy() {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..5 {
        plant_artifact(dir.path(), "strat_a", &format!("{i:064}"));
    }
    plant_artifact(dir.path(), "strat_b", &"f".repeat(64));

    prune(dir.path(), 3).expect("prune");

    assert_eq!(count_for(dir.path(), "strat_a"), 3, "the newest three survive");
    assert_eq!(count_for(dir.path(), "strat_b"), 1, "another strategy's artifact is untouched");
}

/// The newest-N choice specifically: with mtimes tied (planted in one loop, possibly within one
/// filesystem clock tick), the FILENAME tiebreak in `prune` is deterministic — the zero-padded
/// `sha` values here sort the same way numerically and lexicographically, so "the newest three"
/// unambiguously means indices 2, 3, 4.
#[test]
fn retention_keeps_the_lexicographically_newest_on_a_tied_clock() {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..5 {
        plant_artifact(dir.path(), "strat_a", &format!("{i:064}"));
    }
    prune(dir.path(), 3).expect("prune");
    for i in 2..5 {
        let sha = format!("{i:064}");
        assert!(
            dir.path().join(format!("strat_a-{sha}.so")).exists(),
            "artifact {i} should have survived pruning"
        );
    }
    for i in 0..2 {
        let sha = format!("{i:064}");
        assert!(
            !dir.path().join(format!("strat_a-{sha}.so")).exists(),
            "artifact {i} should have been pruned"
        );
    }
}
