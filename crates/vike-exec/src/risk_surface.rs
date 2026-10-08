//! **`[risk]`'s source, compiled in as DATA** — so a schema exporter one layer up can publish this
//! table's keys without reaching across a package boundary.
//!
//! [`crate::risk_profile::ProfileRisk`] is the `[risk]` table of a run profile, and
//! `vike_backtest::profile_surface` publishes the profile's schema by PARSING the sources that
//! declare it. An `include_str!` reaching `../../vike-exec/src/risk_profile.rs` would be a path
//! outside the including package, one that compiles today and breaks whenever either crate moves.
//! So the crate that OWNS the type exposes its own source and the exporter reads it through the
//! dependency edge that already exists (vike-exec is layer 20, vike-backtest 30) — the shape to
//! copy for any other foreign struct a profile reaches.
//!
//! A consumer may assume only that these bytes are the declaring file, verbatim. The SHAPE it is
//! parsed for (`pub name: Type,` fields carrying `///` docs and serde attributes) is
//! `vike_backtest::profile_surface`'s business; its parser panics rather than dropping a field it
//! cannot read, so a refactor here fails that gate instead of silently shortening a published
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
