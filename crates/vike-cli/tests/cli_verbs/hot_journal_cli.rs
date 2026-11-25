//! **A killed writer's journal, met by the shipped binary**: what a daemon's `ExecStartPre=` does when a hub died inside a commit.
//! The recovery leaves its trace on stderr, the unit's journal, because this binary installs no log subscriber, and the advice
//! never includes a restore, which would lose every commit since the backup.

use std::process::{Command, Stdio};

use crate::support::settings_project;

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

#[test]
fn config_check_replays_a_killed_writers_journal_and_says_so_on_stderr() {
    let (_root, dir) = settings_project("vike-cli-hot-journal-");
    vike_secrets::create_store(Some(dir.to_str().expect("utf-8"))).expect("a store");
    vike_secrets::plant_hot_journal(&dir);
    let journal = dir.join("db").join("vike.db-journal");
    assert!(journal.is_file(), "the state under test");

    let out = Command::new(BIN)
        .args(["config", "check"])
        .env_clear()
        .env("VIKE_SETTINGS_DIR", &dir)
        .stdin(Stdio::null())
        .output()
        .expect("the vike-cli binary must run");
    let (stdout, stderr) =
        (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    assert!(stderr.contains("it has been replayed"), "the unit's journal must say so: {stderr}");
    assert!(stderr.contains("killed mid-write"), "{stderr}");
    assert!(!journal.exists(), "the replay removes the journal");
    assert!(!stdout.contains("nightly backup"), "a restore would lose every commit: {stdout}");
    assert!(!stdout.contains("could not be opened"), "the read answered: {stdout}");
}

/// A read of a store with no journal says nothing of one.
#[test]
fn config_check_of_a_clean_store_says_nothing_of_a_replay() {
    let (_root, dir) = settings_project("vike-cli-no-journal-");
    vike_secrets::create_store(Some(dir.to_str().expect("utf-8"))).expect("a store");
    let out = Command::new(BIN)
        .args(["config", "check"])
        .env_clear()
        .env("VIKE_SETTINGS_DIR", &dir)
        .stdin(Stdio::null())
        .output()
        .expect("the vike-cli binary must run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("replayed"), "{stderr}");
}
