use std::collections::HashMap;
use std::path::Path;

use super::*;

/// The environment a root hands in when `$VIKE_SETTINGS_DIR` names `settings`.
fn named(settings: &Path) -> HashMap<String, String> {
    HashMap::from([("VIKE_SETTINGS_DIR".to_string(), settings.display().to_string())])
}

/// Plant ONE `preferences` row, `value` as the JSON scalar the store holds.
fn plant(settings: &Path, key: &str, value: &str) {
    vike_secrets::plant_settings_rows(
        settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "preferences".to_string(),
                key: key.to_string(),
                value: value.to_string(),
            }],
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("plant preferences.{key} under {}: {e}", settings.display()));
}

/// No project and no override: nothing resolves, nothing is said, and the root keeps its default.
#[test]
fn with_no_project_nothing_is_read_and_the_compiled_default_stands() {
    let read = log_file_level(&HashMap::new(), None);
    assert_eq!(read, LogFileLevel::default());
    assert_eq!(read.level_or("warn"), "warn");
    assert_eq!(read.unread_line("warn"), None);
}

/// A named directory with no database is the ordinary state of a fresh box: the compiled default,
/// in silence, and the log home still hangs off the named directory.
#[test]
fn a_project_with_no_database_keeps_the_compiled_default_in_silence() {
    let dir = tempfile::tempdir().expect("a settings directory");
    let read = log_file_level(&named(dir.path()), None);
    assert_eq!(read.level, None);
    assert_eq!(read.unread, None, "no database is not a failure");
    let logs = dir
        .path()
        .join(vike_model::paths::state_path::STATE_SUBDIR)
        .join(vike_model::paths::state_path::LOGS_SUBDIR);
    assert_eq!(read.log_home.as_deref(), Some(logs.as_path()));
}

/// The row is the level, and it beats whatever the root compiled in.
#[test]
fn the_row_is_the_level() {
    let dir = tempfile::tempdir().expect("a settings directory");
    plant(dir.path(), "log_file_level", "\"warn\"");
    let read = log_file_level(&named(dir.path()), None);
    assert_eq!(read.level.as_deref(), Some("warn"));
    assert_eq!(read.level_or("trace"), "warn");
    assert_eq!(read.unread, None);
}

/// A store that holds rows but not this one answers `None`: the root's compiled default stands, not
/// the loader's `trace` (a batch tool compiles in `warn`, and a missing row must not raise it).
#[test]
fn a_store_without_the_row_keeps_the_compiled_default() {
    let dir = tempfile::tempdir().expect("a settings directory");
    plant(dir.path(), "log_level", "\"info\"");
    let read = log_file_level(&named(dir.path()), None);
    assert_eq!(read.level, None);
    assert_eq!(read.level_or("warn"), "warn");
    assert_eq!(read.unread, None);
}

/// A row the loader refuses (a blank level) is not trusted: the compiled default, and a line saying
/// why that names the default it fell back to.
#[test]
fn an_illegal_row_falls_back_to_the_compiled_default_and_says_so() {
    let dir = tempfile::tempdir().expect("a settings directory");
    plant(dir.path(), "log_file_level", "\"\"");
    let read = log_file_level(&named(dir.path()), None);
    assert_eq!(read.level, None);
    let line = read.unread_line("warn").expect("an unreadable row is reported");
    assert!(line.contains(KEY) && line.contains("`warn`"), "{line}");
}

/// The override decides over the working directory: the level comes from the NAMED project, and so
/// does the log home, so the two cannot describe different projects.
#[test]
fn the_named_project_beats_the_working_directory() {
    let named_dir = tempfile::tempdir().expect("the named settings directory");
    plant(named_dir.path(), "log_file_level", "\"error\"");
    let elsewhere = tempfile::tempdir().expect("a working directory elsewhere");
    let read = log_file_level(&named(named_dir.path()), Some(elsewhere.path()));
    assert_eq!(read.level.as_deref(), Some("error"));
    assert!(read.log_home.as_deref().is_some_and(|home| home.starts_with(named_dir.path())));
}
