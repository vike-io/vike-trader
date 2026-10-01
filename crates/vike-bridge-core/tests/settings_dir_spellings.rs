//! **The settings-directory duplication is all but GONE, and this file is what it left behind —
//! a source check over the merged WALK, and one surviving equality pin.**
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
//!
//! # And the CONSTANTS, which this file said nothing about until 2026-09-26
//!
//! ⚠ The rewrite above dropped the CONSTANT comparison along with the walk one, while both
//! constants' docs in `dotenv.rs` went on promising *"a test pins the two spellings equal"*. The
//! pin was gone and the promise was not, for five days — the same shape this file's own history
//! records as having gone unnoticed once before, which is why it is written out here rather than
//! merely fixed.
//!
//! Two of the three collapsed instead of being pinned, on the disposition
//! `crates/vike-bridge-core/tests/account_label_spellings.rs` reached for the account-label pair in
//! the same sweep: `vike_secrets::dotenv`'s `SETTINGS_DIR` and `STATE_DIR` were second spellings of
//! [`vike_model::state_path::PROJECT_SETTINGS_DIR`] and [`vike_model::state_path::STATE_SUBDIR`]
//! resting on the retired policy and on nothing else, so `dotenv.rs` imports the first and needs
//! the second nowhere. **A pin over ONE declaration cannot fail**, so none was written for them.
//!
//! [`vike_secrets::SETTINGS_DIR_ENV`] is the one that stayed, and it stayed for a reason the dead
//! policy was standing in front of: it names an ENVIRONMENT VARIABLE, `vike_ops::scan` resolves
//! constants **crate-wide**, and `vike_ops::settings::SETTINGS` is keyed on the pair
//! `(name, krate)` — so the store crate importing the name instead of declaring it would strand
//! that table's row for this variable under the `vike-secrets` key, and
//! `every_declared_variable_is_read` would refuse the PR. Two declarations survive, so an equality
//! assertion over them CAN fail, and
//! [`the_settings_dir_variable_is_spelled_the_same_in_both_crates`] is it.
//!
//! ⚠ **This file is no longer the only place that assertion COULD live**, and the module doc above
//! must not be read as saying otherwise: 0072 gave `vike-secrets` the `vike-model` edge, so its own
//! `#[cfg(test)]` module can compare the two constants directly. It lives here because this is
//! where the duplication's history is written and where ~20 places in the tree cite it — a reason
//! about documentation, not about visibility. The PREDICATE pin in `account_label_spellings.rs`
//! still has the harder constraint and genuinely cannot move.

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

/// **The `$VIKE_SETTINGS_DIR` NAME is still declared in both crates, deliberately — so it is
/// PINNED.** The module doc carries why this one did not collapse with its two neighbours; the
/// short version is that `vike_ops::scan` resolves constants crate-wide, so importing the name
/// would strand `vike_ops::settings::SETTINGS`'s row for it under the `vike-secrets` key.
///
/// ⚠ Unlike the retired walk comparison, this one CAN FAIL: these are two independent `const`
/// declarations, so editing either alone reddens it. That distinction is the whole reason the walk
/// assertion was replaced by a source check rather than kept — an assertion that cannot fail reads
/// like coverage and is worse than none.
///
/// What a failure MEANS, and why it is not cosmetic: the two constants are what a caller passes to
/// `project_settings_dir_from`. A box exporting the name one crate reads while a root looks up the
/// other would resolve credentials through the walk and settings through the override, or the
/// reverse — the split that `dotenv.rs`'s `project_settings_dir_for` exists to prevent one level
/// down, wearing the variable's name instead of the working directory's absence.
#[test]
fn the_settings_dir_variable_is_spelled_the_same_in_both_crates() {
    assert_eq!(
        vike_secrets::SETTINGS_DIR_ENV,
        vike_model::state_path::SETTINGS_DIR_ENV,
        "the settings-directory override is spelled `{}` by the store crate and `{}` by the model \
         — one was edited without the other, and an operator exporting the loser configures \
         nothing while the startup banner names the winner",
        vike_secrets::SETTINGS_DIR_ENV,
        vike_model::state_path::SETTINGS_DIR_ENV,
    );
}
