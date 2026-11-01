//! A second core started on a journal directory a live one holds REFUSES TO START IN WORDS: it exits
//! 1 with the single-writer interlock's own sentence and does not panic with a backtrace over it
//! (`crates/vike-core/src/runtime/journaling.rs`'s `refuse_start_journal_open`, called by
//! `assemble_core`).
//!
//! The refusal ends the process, so the second launch is a CHILD: this very test binary, re-run on
//! the child half below with the journal directory as its WORKING DIRECTORY. No environment
//! variable carries it, on purpose: an `env::var` read, even in a test, needs a row in
//! `vike_config::settings::SETTINGS` (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`
//! walks `tests/` too); the idiom is `crates/vike-change-journal/tests/change_journal_concurrent.rs`'s.

use std::process::Command;

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_journal::{CommandJournal, JournalFileConfig};

use crate::kit::engines::sim_engine;

/// The prefix of the scratch root the journal directory sits in (`Scratch::root` writes
/// `vjl-<tag>-`): the child starts a core only when its working directory is INSIDE one, so a stray
/// `cargo test -- --ignored` in a checkout touches nothing.
const CHILD_ROOT_PREFIX: &str = "vjl-double-launch-";

/// The CHILD half: start a core on the journal directory that is the working directory. Never run by
/// an ordinary test invocation (`#[ignore]`) and a no-op outside the parent's scratch root. In the
/// child it never returns: `spawn_core` ends the process, and a return (a panic here) is the failure
/// the parent reads as exit code 101.
#[test]
#[ignore = "spawned by a_second_launch_on_a_held_journal_exits_in_words_and_does_not_panic"]
fn the_second_launch_child() {
    let cwd = std::env::current_dir().expect("a working directory");
    let spawned = cwd
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n.to_string_lossy().starts_with(CHILD_ROOT_PREFIX));
    if !spawned {
        eprintln!("not a spawned child (cwd {}): starting nothing", cwd.display());
        return;
    }
    let config = CoreConfig { journal: Some(JournalConfig::at(cwd)), ..CoreConfig::default() };
    let _core = spawn_core(sim_engine(), config);
    panic!("the journal is held by the first launch: spawn_core must have ended this process");
}

/// **A double launch is a sentence and an exit code, not a backtrace.** The first instance holds the
/// journal (its writer keeps the interlock); the second, started on the same directory, exits 1 and
/// says on stderr that another instance is writing it.
#[test]
fn a_second_launch_on_a_held_journal_exits_in_words_and_does_not_panic() {
    let dir = crate::scratch::Scratch::reserved("double-launch");
    let cfg = JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 };
    let _first = CommandJournal::open(&dir, cfg).expect("the first launch opens the journal");

    let exe = std::env::current_exe().expect("this test binary's own path");
    let out = Command::new(exe)
        .args(["--ignored", "--exact", "double_launch::the_second_launch_child", "--nocapture"])
        .current_dir(&*dir)
        .output()
        .expect("re-run this test binary");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(1),
        "the second launch must exit 1 (101 is a panic).\n--- child stderr ---\n{stderr}"
    );
    assert!(
        stderr.contains("refusing to start")
            && stderr.contains("another vike instance is already writing the journal directory"),
        "the refusal must carry the interlock's own sentence.\n--- child stderr ---\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "the refusal must not be a panic.\n--- child stderr ---\n{stderr}"
    );
}
