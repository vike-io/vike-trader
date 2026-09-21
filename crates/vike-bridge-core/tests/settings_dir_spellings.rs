//! **The settings-directory duplication is GONE, and this file is what it left behind.**
//!
//! ⚠ Read the shape before the history, because ~20 places in this tree cite this file as the
//! PRECEDENT for a two-spellings pin — `crates/vike-app-core/tests/node_key_store_spellings.rs`
//! calls it "this repo's precedent" outright, `crates/vike-bridge-core/tests/account_label_spellings.rs`
//! cites "the reason it gives", and `crates/vike-model/CLAUDE.md` and
//! `docs/decisions/0026-containerisation-additive-backend-image.md` both lean on it. The argument
//! below is still theirs. What changed is that THIS instance of it no longer applies, and the test
//! that enforced it has been replaced by one that enforces the thing which replaced it.
//!
//! # What the pin was for
//!
//! `vike_model::state_path` and `vike_secrets::dotenv` each spelled the project-settings resolver
//! independently — the marker walk, the `[workspace]`-decides-alone rule, the unreadable-manifest
//! case — because `vike-secrets` declared ZERO `vike-*` dependencies by policy and could not import
//! vike-model's copy, while vike-model would not take a dependency to learn a directory name.
//!
//! This test lived in `vike-bridge-core` because it is the only crate that depended on BOTH, so
//! neither owner could host it. It checked the CONSTANTS and, the half that mattered, the WALKS:
//! the same real directory tree resolved through each copy across every shape that distinguishes
//! the two markers. Both copies' doc comments had promised such a pin since the duplication landed.
//! **There was no such test** until this one was written, and for that whole period the two were
//! free to drift into two different answers to "where do my credentials live".
//!
//! # Why it does not apply any more
//!
//! `vike-secrets` now declares `vike-model`
//! (`docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`), and `dotenv.rs`'s
//! two entry points are one-line
//! delegations into `vike_model::state_path`. There is ONE implementation, so a test comparing two
//! would compare a function with itself — an assertion that cannot fail, which is worse than no
//! test because it reads like coverage.
//!
//! ⚠ The merge was CERTIFIED by the pin it retires, not merely assumed safe: the four assertions
//! this file used to carry were run one last time on the pre-merge tree and all four passed,
//! `the_two_copies_walk_identically` included. Its final act was to prove that collapsing the two
//! changed no answer.
//!
//! # What is pinned here instead
//!
//! The property the merge BOUGHT: that there is still only one copy. A future edit that gives
//! `vike-secrets` its own walk again — because somebody restores the zero-`vike-*` policy, or
//! inlines the logic to avoid the edge — would silently recreate the drift this file was written
//! for, and nothing else in the tree would notice. So the assertion is now a SOURCE check, which
//! is the only shape that can see it: `dotenv.rs` reaches the resolver by delegation and declares
//! no walk of its own.

use std::path::Path;

/// The one file that used to hold the second copy.
const DOTENV: &str = "crates/vike-secrets/src/dotenv.rs";

/// Machinery the second copy had, and which must not come back. Each name is a thing
/// `vike_model::state_path` owns; a re-appearance here means the walk was re-spelled.
const RESPELLING_MARKERS: [&str; 4] = [
    "ManifestChain",
    "nearest_project_marker",
    "declares_a_workspace",
    "opens_the_workspace_table",
];

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap()
}

/// The delegation is still a delegation: `vike-secrets` names `vike_model::state_path` and carries
/// no walk of its own.
///
/// Deliberately a TEXT check. The behavioural question ("do the two agree?") cannot be asked any
/// more — there is one function — so the only failure left to catch is structural, and a compile
/// would not catch it: a re-spelled private walk compiles perfectly.
#[test]
fn the_settings_resolver_is_still_spelled_once() {
    let src = std::fs::read_to_string(repo_root().join(DOTENV))
        .unwrap_or_else(|e| panic!("{DOTENV}: {e}"));

    assert!(
        src.contains("vike_model::state_path::project_settings_dir"),
        "{DOTENV} no longer delegates to `vike_model::state_path`. If the delegation moved, \
         re-point this check; if it was REPLACED by a local walk, that is the duplication this \
         file exists to prevent coming back — see the module doc."
    );

    for marker in RESPELLING_MARKERS {
        assert!(
            !src.contains(marker),
            "{DOTENV} names `{marker}`, which `vike_model::state_path` owns. The settings-directory \
             resolver was spelled twice until it was merged; this is that second copy growing back. \
             Delegate instead, or read the module doc for what the two copies cost."
        );
    }
}
