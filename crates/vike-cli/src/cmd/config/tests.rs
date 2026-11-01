use super::*;
use vike_config::settings::{Layer, Medium, Scope};

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

/// A box with NO settings database — no store at all, so its credential map is always EMPTY.
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
        medium: Medium::ProcessEnv,
        default,
    }
}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// [`crossing_line`] ends the three `bootstrap-*` reports. Each expected sentence is the one that
/// verb's own copy printed before the three became one, byte for byte: dry and wet, the profile
/// already active, another one active, and none selected.
#[test]
fn each_bootstrap_report_ends_on_the_crossing_sentence_it_always_printed() {
    let cases = [
        (
            "daemon",
            bootstrap_daemon::UNSELECTED,
            " Nothing selects a daemon profile in this store today, so this is the CROSSING that \
             lets `vike-tradehub` start with no `--config` at all.",
        ),
        (
            "recorder",
            bootstrap_recorder::UNSELECTED,
            " Nothing selects a recorder profile in this store today, so this is the CROSSING that \
             lets the data daemon record with no `--record` file at all.",
        ),
        (
            "run",
            bootstrap_run::UNSELECTED,
            " Nothing selected a run profile in this store, so this is the CROSSING: from the next \
             restart the daemon's pre-trade ceilings come from this row.",
        ),
    ];
    let new = Some("new".to_string());
    let old = Some("old".to_string());
    for (kind, unselected, none_line) in cases {
        let line = |already: &Option<String>, dry_run| {
            crossing_line(kind, unselected, already, "new", dry_run)
        };
        let same = |was: &str| {
            format!(" `new` {was} already the active {kind} profile; its body is this one now.")
        };
        let repoint =
            |verb: &str| format!(" This {verb} the box's {kind} profile from `old` to `new`.");
        assert_eq!(line(&new, true), same("is"), "{kind}");
        assert_eq!(line(&new, false), same("was"), "{kind}");
        assert_eq!(line(&old, true), repoint("would REPOINT"), "{kind}");
        assert_eq!(line(&old, false), repoint("REPOINTED"), "{kind}");
        assert_eq!(line(&None, true), none_line, "{kind}");
        assert_eq!(line(&None, false), none_line, "{kind}");
    }
}

#[cfg(test)]
mod render;
#[cfg(test)]
mod show;
