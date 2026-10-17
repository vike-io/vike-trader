//! The `depth` lane counts stream-health MARKERS as rows: a text pin on
//! `crates/vike-store/src/live_rec.rs`, the code the marker-inflation paragraph of
//! `vike_data::store::series_cadence`'s module doc cites. Text-only, so it reads the source by repo
//! path, the way the cadence gate beside the table does.

use std::path::{Path, PathBuf};

/// The workspace root — `crates/vike-store/../..`. Every path below is repo-relative, so a file
/// MOVE between crates turns this pin red rather than silently unchecking a claim.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn read_rel(rel: &str) -> String {
    read(&repo_root().join(rel))
}

/// `needle` occurs in `hay` as a WHOLE token, not as a fragment of a longer identifier. Copied
/// deliberately from `crates/vike-ops/tests/venues/store_kind_gate.rs` rather than shared: an integration
/// test is its own crate, and a shared helper would need a `test-support` surface for two functions.
fn contains_token(hay: &str, needle: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back().is_some_and(ident);
        let after = hay[i + needle.len()..].chars().next().is_some_and(ident);
        !before && !after
    })
}

/// **PINNED:** the `depth` lane counts stream-health MARKERS as rows, so a reconnect loop inflates
/// the very number a rate check reads.
///
/// `stream_status` routes `GapStart`/`Stale`/`LiveResume` into `Msg::Depth`, which reaches `ingest`
/// and bumps `Liveness::rows`. Its own doc measures ~20,000 reconnect cycles a day on the broken
/// lane. Any consumer of this table must leave margin for that; a floor derived as a fraction of a
/// declared ceiling does, and an absolute floor sized to the observed broken rate does not.
#[test]
fn a_status_marker_still_counts_as_a_depth_row() {
    let src = read_rel("crates/vike-store/src/live_rec.rs");
    assert!(
        contains_token(&src, "stream_status"),
        "`crates/vike-store/src/live_rec.rs`'s `stream_status` is gone — the marker-inflation \
         paragraph in `vike_data::store::series_cadence`'s module doc cites it."
    );
    // The arm is keyed by the shared `vike_data::StreamLabel` (it was the literal `"depth"` before the
    // label vocabulary became one type), so the pin reads the VARIANT arm, not a word.
    assert!(
        src.contains("Some(StreamLabel::Depth) => self.send(Msg::Depth"),
        "`crates/vike-store/src/live_rec.rs`'s `stream_status` no longer routes depth markers into \
         the depth lane. If markers stopped counting toward `Liveness::rows`, the margin argument \
         in `vike_data::store::series_cadence`'s module doc is stale — update it."
    );
}
