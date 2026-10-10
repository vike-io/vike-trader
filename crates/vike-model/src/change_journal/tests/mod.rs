use super::*;
use crate::scratch::ScratchDir;

#[cfg(test)]
mod append_and_lock;
#[cfg(test)]
mod caps_and_refusal;
#[cfg(test)]
mod cleaning_and_paths;
#[cfg(test)]
mod records;
#[cfg(test)]
mod retention;

/// A throwaway journal directory for one test.
///
/// Built with [`ScratchDir`], the same dogfooding [`crate::scratch`]'s own tests use: unique per
/// process, self-deleting on the panic path, and no `tempfile` dev-dependency in the crate every
/// binary in this workspace links. The system temp directory is legitimate here —
/// `crates/vike-ops/tests/hygiene/system_temp_gate.rs` scopes itself to production code.
fn root() -> ScratchDir {
    ScratchDir::create_in(&std::env::temp_dir(), "vike-changejournal-selftest").expect("root")
}

fn journal(dir: &Path) -> ChangeJournal {
    ChangeJournal::new(dir.to_path_buf(), Proc::new("vike-test", 4711, "0.1.0"))
}

/// 2026-08-21T00:00:00Z, the anchor every timestamped test below uses.
const T: i64 = 1_787_356_800_000;

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("journal file")
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line parses as one JSON object"))
        .collect()
}
