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
// because the file is listed in that gate's `SOURCE_TEST_MODULE_OVERRIDES`.
//
// Two things still bite, and neither is the gate being fussy:
//   - a direct `std::env::var(..)` here IS a read, gate or no gate, and still needs a row;
//   - a literal ABOVE this `#[cfg(test)]` attribute is library code and is swept normally.
// Names with no prefix at all (`ACME_*`) are invisible to the sweep either way, and remain the
// right choice for a fixture whose point is the NAME SHAPE rather than a particular variable.

/// A box with NO settings database — no store at all, so its credential map is always EMPTY.
const ABSENT: vike_secrets::StoreBackend = vike_secrets::StoreBackend::Absent;

/// The box with a store — the settings database. A path, never a probe: `resolve` is pure and the backend is its
/// parameter, so nothing here has to put a file on disk to test the word it prints.
fn database() -> vike_secrets::StoreBackend {
    vike_secrets::StoreBackend::Database(PathBuf::from("/p/settings/db/vike.db"))
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

/// [`crossing_line`] ends the two `profile create` reports that select (`profile create run` selects nothing
/// since decision 0297). Each expected sentence is the one that verb's own copy printed before they
/// became one, byte for byte: dry and wet, the profile already active, another one active, and none
/// selected.
#[test]
fn each_bootstrap_report_ends_on_the_crossing_sentence_it_always_printed() {
    let cases = [
        (
            "daemon",
            create::daemon::UNSELECTED,
            " Nothing selects a daemon profile in this store today, so this is the CROSSING that \
             lets `vike-tradehub` start with no `--config` at all.",
        ),
        (
            "recorder",
            create::recorder::UNSELECTED,
            " Nothing selects a recorder profile in this store today, so this is the CROSSING that \
             lets the data daemon record with no `--record` file at all.",
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

/// **The profile family's old words are retired with no alias** (the naming audit's G3): each refuses
/// by its id, naming the spelling that replaced it, and that spelling parses.
#[test]
fn every_retired_profile_word_refuses_naming_its_replacement() {
    let argv = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
    for (old, new, named, refusal) in [
        (
            "recorder",
            "profile ls recorder",
            "config profile ls recorder",
            RetiredConfigSpelling::Recorder,
        ),
        (
            "activate daemon a",
            "profile activate daemon a",
            "config profile activate",
            RetiredConfigSpelling::Activate,
        ),
        (
            "deactivate run",
            "profile deactivate run",
            "config profile deactivate",
            RetiredConfigSpelling::Deactivate,
        ),
        (
            "delete-profile run a --yes",
            "profile rm run a --yes",
            "config profile rm",
            RetiredConfigSpelling::DeleteProfile,
        ),
        (
            "bootstrap-daemon a --venue bybit --strategy-file f.rs",
            "profile create daemon a --venue bybit --strategy f.rs",
            "config profile create daemon",
            RetiredConfigSpelling::BootstrapDaemon,
        ),
        (
            "bootstrap-recorder a",
            "profile create recorder a",
            "config profile create recorder",
            RetiredConfigSpelling::BootstrapRecorder,
        ),
        (
            "bootstrap-run a --risk.max_notional_per_order 1",
            "profile create run a --risk.max_notional_per_order 1",
            "config profile create run",
            RetiredConfigSpelling::BootstrapRun,
        ),
        (
            "set-mount-param --mount a:0 --key k --value 1",
            "profile mount set-param --mount a:0 --key k --value 1",
            "config profile mount set-param",
            RetiredConfigSpelling::SetMountParam,
        ),
        (
            "set-mount-max-leverage --mount a:0 --max-leverage 2",
            "profile mount set-max-leverage --mount a:0 --max-leverage 2",
            "config profile mount set-max-leverage",
            RetiredConfigSpelling::SetMountMaxLeverage,
        ),
        (
            "delete-operator-instrument --venue ig X",
            "operator-instrument rm --venue ig X",
            "config operator-instrument rm",
            RetiredConfigSpelling::DeleteOperatorInstrument,
        ),
        (
            "rename-symbol --account 4 --from A --to B",
            "rename-symbol --id 4 --from A --to B",
            "config rename-symbol --id ID",
            RetiredConfigSpelling::RenameSymbolAccount,
        ),
        (
            "profile create daemon a --strategy-file=f.rs",
            "profile create daemon a --strategy=f.rs",
            "--strategy",
            RetiredConfigSpelling::StrategyFile,
        ),
    ] {
        let said = parse(argv(old)).expect_err(old);
        assert!(said.contains(&format!("error [{}]", refusal.id().word())), "{}", &*said);
        assert!(said.contains(&format!("is now `{named}")), "{}", &*said);
        assert_eq!(retired_spelling(&argv(old)), Some(refusal), "{old}");
        assert!(parse(argv(new)).is_ok(), "the new spelling parses: {new}");
        assert_eq!(retired_spelling(&argv(new)), None, "{new}");
    }
}

#[cfg(test)]
mod render;
#[cfg(test)]
mod show;
