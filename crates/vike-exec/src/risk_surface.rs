//! **`[risk]`'s source, compiled in as DATA** — so a schema exporter one layer up can publish this
//! table's keys without reaching across a package boundary.
//!
//! # Why this exists
//!
//! [`crate::risk_profile::ProfileRisk`] is the `[risk]` table of a backtest run profile, and
//! `vike_backtest::profile_surface` publishes that profile's schema by PARSING the sources that
//! declare it. It could not parse this one: an `include_str!` reaching
//! `../../vike-exec/src/risk_profile.rs` is a path outside the including package, which is exactly
//! the kind of edge that compiles today and breaks whenever either crate moves.
//!
//! ⚠ **So the table was published as a DECLARED HOLE — and the reason given for the hole was
//! WRONG.** It said the packaging gate refuses such a path; `crates/vike-ops/tests/packaging_gate.rs`
//! is about the `cargo binstall` URL template and mentions `include_str!` nowhere. The hole was
//! avoidable from the start, and eleven keys — including the per-order notional cap a live mount
//! refuses to start without — were missing from the reference because of a justification nobody
//! checked. This module is the fix, and it is the shape to copy for any other foreign struct a
//! profile reaches: **the crate that OWNS the type exposes its own source; the exporter reads it
//! through the dependency edge that already exists.**
//!
//! `vike-exec` declares layer 20 and `vike-backtest` layer 50, and the dependency is already there
//! for `ProfileRisk` itself — so nothing about the graph changes. This is one `pub const`.
//!
//! # What a consumer may assume
//!
//! Only that these bytes are the file that declares the type, verbatim. The SHAPE it is parsed for
//! — `pub name: Type,` fields carrying `///` docs and serde attributes — is
//! `vike_backtest::profile_surface`'s business, and its parser panics rather than dropping a field
//! it cannot read, so a refactor here fails that gate instead of silently shortening a published
//! table.

/// The source of [`crate::risk_profile`], verbatim.
pub const RISK_PROFILE_SRC: &str = include_str!("risk_profile.rs");

/// Repo-relative path of that source — the evidence a consumer cites for every key read out of it.
pub const RISK_PROFILE_SRC_PATH: &str = "crates/vike-exec/src/risk_profile.rs";

/// The struct inside it that IS the `[risk]` table.
pub const RISK_STRUCT: &str = "ProfileRisk";

#[cfg(test)]
mod tests {
    use super::*;

    /// The compiled-in source is the real file, and it still declares the struct a consumer parses
    /// for. A rename here would otherwise reach the far side as a table that silently lost its
    /// keys.
    #[test]
    fn the_compiled_in_source_declares_the_struct_it_advertises() {
        assert!(!RISK_PROFILE_SRC.is_empty(), "the source compiled in empty");
        assert!(
            RISK_PROFILE_SRC.contains(&format!("pub struct {RISK_STRUCT} {{")),
            "`pub struct {RISK_STRUCT}` is gone from {RISK_PROFILE_SRC_PATH} — a consumer parses \
             this source for that name"
        );
        assert!(
            RISK_PROFILE_SRC.contains("#[serde(deny_unknown_fields)]"),
            "{RISK_PROFILE_SRC_PATH} no longer refuses an undeclared key, and the published \
             reference tells readers that a typo in [risk] is a hard load error"
        );
    }

    /// The path this module publishes is the path this module was compiled from.
    #[test]
    fn the_advertised_path_is_this_file_pair() {
        assert!(RISK_PROFILE_SRC_PATH.starts_with("crates/"), "not repo-root-relative");
        assert!(RISK_PROFILE_SRC_PATH.ends_with("/risk_profile.rs"), "names another file");
    }
}
