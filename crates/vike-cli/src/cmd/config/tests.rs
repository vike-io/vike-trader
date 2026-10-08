use super::*;
use vike_ops::settings::{Layer, Scope};

// NOTE for anyone adding a fixture here: a REALISTIC name is fine, and preferred where the case
// under test is about a specific real variable. This block used to demand invented ones
// (`ACME_*`, `THING_*`): the settings-registry gate's literal sweep
// (`vike_model::scan::find_map_lookups`, driven by `crates/vike-ops/tests/settings_secrets/settings_registry.rs`)
// harvested EVERY env-shaped string literal carrying a known prefix and demanded a matching
// `SETTINGS` row from the containing crate, so realistic fixture data failed that gate for a
// variable this crate does not read. Its `read_evidence_literals` no longer counts a literal in
// a test region as a read, and THIS file's trailing `#[cfg(test)]` block is a test region
// because the file is listed in that gate's `SRC_TEST_MODULE_OVERRIDES`.
//
// Two things still bite, and neither is the gate being fussy:
//   - a direct `std::env::var(..)` here IS a read, gate or no gate, and still needs a row;
//   - a literal ABOVE this `#[cfg(test)]` attribute is library code and is swept normally.
// Names with no prefix at all (`ACME_*`) are invisible to the sweep either way, and remain the
// right choice for a fixture whose point is the NAME SHAPE rather than a particular variable.

/// A box with NO settings database — no store at all, so its credential map is always EMPTY. (The
/// UNMIGRATED box these tests used to resolve against, `Backend::Files`, went with the credential
/// FILE store on 2026-10-07; the precedence cases resolve against [`database`] now.)
const ABSENT: vike_secrets::Backend = vike_secrets::Backend::Absent;

/// The box with a store — the settings database. A path, never a probe: `resolve` is pure and the backend is its
/// parameter, so nothing here has to put a file on disk to test the word it prints.
fn database() -> vike_secrets::Backend {
    vike_secrets::Backend::Database(PathBuf::from("/p/settings/db/vike.db"))
}

fn row(name: &'static str, default: &'static str) -> Setting {
    Setting {
        name,
        krate: "vike-cli",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default,
    }
}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[cfg(test)]
mod render;
#[cfg(test)]
mod show;
