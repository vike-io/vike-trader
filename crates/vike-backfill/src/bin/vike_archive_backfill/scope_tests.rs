//! The declared credential SCOPE, exercised through this binary's own `api_key` resolver.

use super::*;
use vike_secrets::{KeyScope, ScopedSecrets, Source};

/// ⚠ **The mutation target for "a migrated caller's declared set omits a name it needs".**
///
/// Dropping the name from `SCOPE` makes this red, naming the scope — instead of the binary
/// quietly reporting the credential missing on a box where it IS configured.
#[test]
fn the_declared_scope_covers_the_name_this_binary_reads() {
    let scoped = ScopedSecrets::empty(&KeyScope::of(SCOPE), Source::None);
    match api_key(&scoped) {
        Ok(_) => {}
        Err(undeclared) => {
            panic!("this binary reads a credential its own SCOPE does not declare: {undeclared}")
        }
    }
}

/// ...and the other direction, so the test above is not vacuous: an UNDECLARED name really is a
/// distinct answer from an absent one, at this call site and not merely inside the store crate.
#[test]
fn a_name_outside_the_scope_refuses_rather_than_reading_as_absent() {
    // ⚠ A name with no `vike_model::scan` prefix, deliberately: an env-SHAPED literal here would be
    // a sighting in a `src/bin` file, which that scanner scores `Layer::Binary` (a `#[cfg(test)]`
    // module under `src/bin` is NOT test-classified), and would demand a registry row asserting
    // a read this binary does not make. MEASURED — the first spelling used `VIKE_SETTINGS_DIR`
    // and reddened `declared_layer_matches_the_path` in five bins at once.
    let foreign =
        ScopedSecrets::empty(&KeyScope::of(["A_NAME_THIS_BINARY_DOES_NOT_READ"]), Source::None);
    assert!(
        api_key(&foreign).is_err(),
        "a scope that declares something else must REFUSE, never answer `no credential` — \
         absent credentials are the live gate and this is not that"
    );
}
