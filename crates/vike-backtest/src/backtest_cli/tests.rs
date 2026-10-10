//! The unit tests of `backtest_cli`: the router's own step before the log subscriber here, one file
//! per other surface under `backtest_cli/tests/`.
use super::*;
use std::assert_matches;

#[cfg(test)]
mod data_subcommand;

#[cfg(test)]
mod history_route;

#[cfg(test)]
mod persist;

#[cfg(test)]
mod rm_series;

/// The PURE half of the search-flag gate. `crates/vike-backtest/tests/optimizer_cli.rs` is the
/// shipped-binary half and is where the four defects are proven as an operator experiences them;
/// these are the table-driven pins a spawned-binary test cannot buy — a forgotten
/// [`METHOD_KNOBS`] or [`PROFILE_PATH_VALUED`] row reddens HERE, by name, rather than turning into
/// a silently accepted knob or a flag value read as a profile path.
#[cfg(test)]
mod search_flags;

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
    let argv = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();

    for words in [&["--addr"][..], &["--addr", "127.0.0.1:0"], &["--addr=127.0.0.1:0"]] {
        let daemon = daemon_before_logging(&vars, &argv(words))
            .unwrap_or_else(|_| panic!("{words:?}: an empty settings dir must load"));
        let (_, settings) = daemon.unwrap_or_else(|| panic!("{words:?} asks for the daemon"));
        assert_eq!(
            settings.preferences.log_file_level,
            vike_config::preferences::DEFAULT_LOG_FILE_LEVEL,
            "{words:?}: no row, so the compiled default"
        );
    }
    for words in
        [&["run.toml"][..], &["--list"], &["data", "export"], &["--addr="], &["--addr", ""]]
    {
        assert_matches!(
            daemon_before_logging(&vars, &argv(words)),
            Ok(None),
            "{words:?} is not the daemon and must load nothing before the subscriber"
        );
    }
}

/// **The daemon's FILE level is its `preferences.log_file_level` row; every other arm keeps
/// vike-log's compiled default.** `VIKE_LOG_FILE_LEVEL` is not consulted here: it wins inside
/// `vike_log::init`, over whatever this hands it.
#[test]
fn the_daemon_file_level_is_its_row_and_every_other_arm_keeps_the_default() {
    let mut settings = vike_config::Settings::default();
    settings.preferences.log_file_level = "warn".to_string();

    let daemon = log_config(Some(&settings), None);
    assert_eq!(daemon.file_level, "warn", "the row reaches the daemon's file layer");
    assert_eq!(daemon.file_prefix, "backtest");

    let other = log_config(None, Some(PathBuf::from("logs")));
    assert_eq!(other.file_level, vike_log::LogConfig::default().file_level);
    assert_eq!(other.file_prefix, "backtest", "one prefix for every arm");
    assert_eq!(other.project_dir, Some(PathBuf::from("logs")));
}
