//! **The settings-directory resolver is spelled ONCE, and the one name still spelled twice is
//! pinned equal.**
//!
//! This file is the tree's PRECEDENT for a two-spellings pin (cited by
//! `crates/vike-app-core/tests/node_key_store_spellings.rs`,
//! `crates/vike-bridge-core/tests/account_label_spellings.rs` and
//! `docs/decisions/0026-containerisation-additive-backend-image.md`). The rule: **a fact spelled in
//! two crates owes a pin that can FAIL**, hosted where both are visible; a fact spelled ONCE owes
//! none, because an assertion comparing a declaration with itself cannot fail and reads like
//! coverage.
//!
//! # The walk: one copy, held by a source check
//!
//! `crates/vike-secrets/src/store_locator.rs` reaches the project-settings resolver by delegation into
//! `vike_model::paths::state_path` (`vike-secrets` declares `vike-model`, decision 0072). There is
//! no second walk whose answer could disagree, so the only failure left is structural — a re-spelled
//! private walk, which compiles perfectly — and a TEXT check is the shape that sees it.
//!
//! # The constants
//!
//! `store_locator.rs` imports [`vike_model::paths::state_path::PROJECT_SETTINGS_DIR`] and needs no
//! `STATE_SUBDIR`, so neither is pinned. [`vike_secrets::SETTINGS_DIR_ENV`] stays declared in BOTH
//! crates on purpose: it names an ENVIRONMENT VARIABLE, `vike_model::scan` resolves constants
//! **crate-wide**, and `vike_ops::settings::SETTINGS` is keyed `(name, krate)` — importing the name
//! would strand that table's row under the `vike-secrets` key and `every_declared_variable_is_read`
//! would refuse the PR. Two declarations, so the equality CAN fail:
//! [`the_settings_dir_variable_is_spelled_the_same_in_both_crates`] holds that they agree.

use std::path::Path;

/// The one file that used to hold the second copy.
const STORE_LOCATOR: &str = "crates/vike-secrets/src/store_locator.rs";

/// Machinery the second copy had, and which must not come back. Each name is a thing
/// `vike_model::paths::state_path` owns; a re-appearance here means the walk was re-spelled.
const RESPELLING_MARKERS: [&str; 4] = [
    "ManifestChain",
    "nearest_project_marker",
    "declares_a_workspace",
    "opens_the_workspace_table",
];

// `parent()` twice leaves no `..` in the path, unlike the `join("..")` twins such as
// `crates/vike-ops/tests/common/repo.rs`'s `workspace_root`: not the same `PathBuf`.
fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap()
}

/// The delegation is still a delegation: `vike-secrets` names `vike_model::paths::state_path` and
/// carries no walk of its own. Deliberately a TEXT check (see the module doc).
#[test]
fn the_settings_resolver_is_still_spelled_once() {
    let src = std::fs::read_to_string(repo_root().join(STORE_LOCATOR))
        .unwrap_or_else(|e| panic!("{STORE_LOCATOR}: {e}"));

    assert!(
        src.contains("vike_model::paths::state_path::project_settings_dir"),
        "{STORE_LOCATOR} no longer delegates to `vike_model::paths::state_path`. If the delegation moved, \
         re-point this check; if it was REPLACED by a local walk, that is the duplication this \
         file exists to prevent coming back — see the module doc."
    );

    for marker in RESPELLING_MARKERS {
        assert!(
            !src.contains(marker),
            "{STORE_LOCATOR} names `{marker}`, which `vike_model::paths::state_path` owns. The settings-directory \
             resolver was spelled twice until it was merged; this is that second copy growing back. \
             Delegate instead, or read the module doc for what the two copies cost."
        );
    }
}

/// **The `$VIKE_SETTINGS_DIR` NAME is declared in both crates, deliberately — so it is PINNED**
/// (the module doc carries why). Two independent `const` declarations, so editing either alone
/// reddens this.
///
/// What a failure MEANS, and why it is not cosmetic: the two constants are what a caller passes to
/// `project_settings_dir_from`. A box exporting the name one crate reads while a root looks up the
/// other would resolve credentials through the walk and settings through the override, or the
/// reverse — the split that `store_locator.rs`'s `project_settings_dir_for` exists to prevent one level
/// down, wearing the variable's name instead of the working directory's absence.
#[test]
fn the_settings_dir_variable_is_spelled_the_same_in_both_crates() {
    assert_eq!(
        vike_secrets::SETTINGS_DIR_ENV,
        vike_model::paths::state_path::SETTINGS_DIR_ENV,
        "the settings-directory override is spelled `{}` by the store crate and `{}` by the model \
         — one was edited without the other, and an operator exporting the loser configures \
         nothing while the startup banner names the winner",
        vike_secrets::SETTINGS_DIR_ENV,
        vike_model::paths::state_path::SETTINGS_DIR_ENV,
    );
}
