//! The unit tests of `backtest_cli`: the router's own step before the log subscriber here, one file
//! per other surface under `backtest_cli/tests/`.
use super::*;
use std::assert_matches;

#[cfg(test)]
mod history_route;

#[cfg(test)]
mod persist;

#[cfg(test)]
mod rm_series;

/// The PURE half of the search-flag gate. `crates/vike-backtesthub/tests/optimizer_cli.rs` is the
/// shipped-binary half and is where the four defects are proven as an operator experiences them;
/// these are the table-driven pins a spawned-binary test cannot buy — a forgotten
/// [`METHOD_KNOBS`] row or a flag the struct lacks reddens HERE, by name, rather than turning into
/// a silently accepted knob or a flag value read as a profile path.
#[cfg(test)]
mod search_flags;

#[cfg(test)]
mod args;

/// **Only the `--addr` daemon loads its settings before the log subscriber** — and a blank
/// `--addr` is not the daemon there, so the router still refuses it by name later.
///
/// `VIKE_SETTINGS_DIR` names an EMPTY directory in the swept map (no process environment is
/// touched), so the daemon's load meets no database and every key is its compiled default.
#[test]
fn only_the_daemon_loads_its_settings_before_the_subscriber() {
    let empty = tempfile::TempDir::new().expect("create the empty settings dir");
    let vars = std::collections::HashMap::from([(
        "VIKE_SETTINGS_DIR".to_string(),
        empty.path().to_str().expect("utf-8 temp path").to_string(),
    )]);
    // The words are read by the REAL `clap` types first, exactly as `run` reads them.
    let addr_of = |words: &[&str]| {
        let line = words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        search_flags::parsed(&line)
            .unwrap_or_else(|e| panic!("{words:?} must parse: {e}"))
            .run
            .addr_flag()
    };

    for words in [&["--addr"][..], &["--addr", "127.0.0.1:0"], &["--addr=127.0.0.1:0"]] {
        let daemon = daemon_before_logging(&vars, addr_of(words))
            .unwrap_or_else(|_| panic!("{words:?}: an empty settings dir must load"));
        let (_, settings) = daemon.unwrap_or_else(|| panic!("{words:?} asks for the daemon"));
        assert_eq!(
            settings.preferences.log_file_level,
            vike_config::preferences::DEFAULT_LOG_FILE_LEVEL,
            "{words:?}: no row, so the compiled default"
        );
    }
    for words in [&["run.toml"][..], &["--list"], &["data", "export", "d:s:i"]] {
        assert_matches!(
            daemon_before_logging(&vars, addr_of(words)),
            Ok(None),
            "{words:?} is not the daemon and must load nothing before the subscriber"
        );
    }
    // A BLANK `--addr` is not a daemon with an odd address: it never reaches the daemon decision,
    // because `Program` refuses a blank value at the parse.
    for words in [&["--addr="][..], &["--addr", ""]] {
        let line = words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        let text = search_flags::parsed(&line).expect_err("a blank --addr is refused");
        assert!(text.contains("--addr") && text.contains("non-empty"), "{words:?}: {text}");
    }
}

/// `--addr` is `num_args(0..=1)`: a bare one is the configured address, the next token is its
/// value only when it is not a flag, and the inline form carries one — the three spellings the
/// deployed unit and an operator use, held at the parse.
#[test]
fn the_addr_flag_reads_its_optional_value() {
    use crate::backtest_cli::args::AddrFlag;
    let flag = |words: &[&str]| {
        let line = words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        search_flags::run_of(&line).unwrap_or_else(|e| panic!("{words:?}: {e}")).addr_flag()
    };
    assert_eq!(flag(&[]), AddrFlag::Absent);
    assert_eq!(flag(&["--addr"]), AddrFlag::Configured, "the deployed unit: nothing after it");
    assert_eq!(flag(&["--addr", "--named-run"]), AddrFlag::Configured);
    assert_eq!(flag(&["--addr", "--json"]), AddrFlag::Configured);
    assert_eq!(flag(&["--addr", "0.0.0.0:9"]), AddrFlag::Explicit("0.0.0.0:9".to_string()));
    assert_eq!(flag(&["--addr=0.0.0.0:9"]), AddrFlag::Explicit("0.0.0.0:9".to_string()));
    assert_eq!(
        flag(&["--named-run", "--addr", "0.0.0.0:9"]),
        AddrFlag::Explicit("0.0.0.0:9".to_string())
    );
    assert!(flag(&["--named-run", "--addr"]) == AddrFlag::Configured);
}

/// **The FILE level is the `preferences.log_file_level` row when one was read; with none, vike-log's
/// compiled default.** Which arm read it (the daemon's settings, a one-shot arm's single row) is
/// `run`'s business; this is the one place either becomes the `LogConfig`.
#[test]
fn the_file_level_is_the_row_and_without_one_the_compiled_default() {
    let read = log_config(Some("warn".to_string()), None);
    assert_eq!(read.file_level, "warn", "the row reaches the file layer");
    assert_eq!(read.file_prefix, "backtest");

    let none = log_config(None, Some(PathBuf::from("logs")));
    assert_eq!(none.file_level, vike_log::LogConfig::default().file_level);
    assert_eq!(none.file_prefix, "backtest", "one prefix for every arm");
    assert_eq!(none.project_dir, Some(PathBuf::from("logs")));
}

/// A one-shot arm whose `$VIKE_SETTINGS_DIR` names a project with the row takes it, before any
/// settings load: the test harness's `Scratch` relies on exactly this to keep a spawned engine's file
/// log quiet.
#[test]
fn a_one_shot_arm_reads_the_row_of_the_named_project() {
    let dir = tempfile::tempdir().expect("a settings directory");
    vike_secrets::plant_settings_rows(
        dir.path(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "preferences".to_string(),
                key: "log_file_level".to_string(),
                value: "\"off\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("plant the row");
    let vars = std::collections::HashMap::from([(
        "VIKE_SETTINGS_DIR".to_string(),
        dir.path().display().to_string(),
    )]);
    assert_eq!(one_shot_file_level(&vars).as_deref(), Some("off"));
}
