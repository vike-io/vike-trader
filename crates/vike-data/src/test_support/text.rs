//! Reading the store tables' citations as TEXT: the whole-token match and the backticked-span reader the
//! three gates that pin this crate's tables against other files' source each carried a copy of until
//! 2026-10-31 (`tests/series_cadence_gate.rs` here, vike-store's `tests/store/live_rec_marker.rs`, and
//! vike-ops' `tests/venues/store_kind_gate/shape.rs`). Here rather than in `vike_model::test_support`
//! because every user already reaches this crate's `test-support`, and a new vike-model `test-support`
//! dev edge widens what a vike-model test edit plans (xtask's libm-walk planner gate). The source
//! walk ([`rs_files`]) is here for the same reason: vike-cli's message-literal gate and
//! vike-strategy-plugin's source fingerprint each carried it (vike-ops and xtask use `repo.rs`'s
//! `all_rs_files`, which a shipped test may not include).

use std::path::{Path, PathBuf};

/// `needle` occurs in `hay` as a WHOLE token, not as a fragment of a longer identifier.
pub fn contains_token(hay: &str, needle: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back().is_some_and(ident);
        let after = hay[i + needle.len()..].chars().next().is_some_and(ident);
        !before && !after
    })
}

/// The backtick-delimited spans of `text`, each paired with the text immediately AFTER its closing
/// backtick — which is how a `` `path`'s `SYMBOL` `` citation in a table row's prose is recognised.
/// Unbalanced backticks fail: a citation that cannot be read is not silently skipped.
pub fn backticked(text: &str) -> Vec<(&str, &str)> {
    let parts: Vec<&str> = text.split('`').collect();
    assert!(
        parts.len() % 2 == 1,
        "unbalanced backticks in row prose — a citation cannot be read:\n{text}"
    );
    parts
        .iter()
        .skip(1)
        .step_by(2)
        .zip(parts.iter().skip(2).step_by(2))
        .map(|(t, a)| (*t, *a))
        .collect()
}

/// Every `.rs` file under `dir`, recursively, pushed onto `out` in walk order (a caller that needs a
/// stable order sorts). An unreadable directory is skipped, not an error.
pub fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
