//! The carrier of one run profile's `[risk]` rows: [`StoredProfileRisk`], [`ProfileRiskRow`] and
//! [`ProfileRiskSource`]. `vike-cli config show` builds them from the active `run` row's
//! `profile_setting` rows (`crates/vike-cli/src/cmd/config/store.rs`'s `run_profile_rows`); nothing
//! in this crate reads or writes the `profile_risk` TABLE.

use std::path::PathBuf;

/// One `[risk]` key of ONE run profile, and **the TOML RENDERING
/// of one scalar**.
///
/// The value is a rendering rather than a typed column for the same reason [`SettingRow::value`](super::SettingRow::value) is
/// — it round-trips through the same parse an operator's file goes through, so no second validator
/// is written. The key vocabulary is `vike_model::ProfileRisk::keys()`, and each row is judged by
/// that type's own parser (`vike_config::check_risk_key`).
///
/// ⚠ **This column stayed TOML when [`SettingRow::value`](super::SettingRow::value) became JSON on 2026-09-18, and the
/// difference is a decision rather than an oversight.** `vike_config::profile_risk`'s module doc
/// states the rule this table is built on — *"the mirror must never be
/// STRICTER than the boot"* — and a JSON column would break it on exactly one shape: JSON has no
/// literal for `inf`, while `max_leverage = inf` is a run profile `vike_model::ProfileRisk` parses
/// and `vike_config::RunProfile::validate` does not refuse. The settings column has no such shape,
/// because `vike_config::load` runs FIRST there and every `f64` field is finiteness-checked or
/// bounded. Every OTHER value this column can hold is valid JSON carrying the same value — the
/// roster admits floats, integers and bools and nothing else — so the two encodings differ on the
/// one case that forced them apart, and on nothing an operator's profile can otherwise produce.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProfileRiskRow {
    /// The `[risk]` key — `max_notional_per_order`, `max_leverage`, …. No `risk.` prefix: the
    /// table IS the `[risk]` table.
    pub key: String,
    /// The TOML rendering of one scalar — `250000.0`, `10`, `true`.
    pub value: String,
}

/// One run profile's whole `[risk]` table, as rows.
///
/// [`Self::profile`] is the profile's NAME, not a path. A path is a fact about one box's disk and
/// would put a box-specific string in a database that `vike-cli secrets`/`config` prints; the file
/// NAME is what an operator recognised (the table was written from a FILE by the retired
/// `config mirror --profile`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct StoredProfileRisk {
    /// The profile's file name — `run-live.toml`.
    pub profile: String,
    /// Its `[risk]` rows, sorted by key.
    pub rows: Vec<ProfileRiskRow>,
}

/// Where a run profile's `[risk]` rows came from, and **why there are none when there are none** —
/// a three-armed shape, because an empty `Vec` would let a caller report an authoritative nothing
/// about a store it cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileRiskSource {
    /// The profile tables are there and these are its run profiles (possibly none), sorted by name.
    Rows(Vec<StoredProfileRisk>),
    /// [`crate::Backend::Absent`] — no settings database on this box at all.
    NoDatabase {
        /// Where a database would be.
        path: PathBuf,
    },
    /// A database this binary can read that carries no profile tables yet.
    TableAbsent {
        /// The database that was opened.
        path: PathBuf,
    },
}

impl ProfileRiskSource {
    /// The profiles, or `None` for every arm that has none.
    #[must_use]
    pub fn profiles(&self) -> Option<&[StoredProfileRisk]> {
        match self {
            ProfileRiskSource::Rows(p) => Some(p),
            ProfileRiskSource::NoDatabase { .. } | ProfileRiskSource::TableAbsent { .. } => None,
        }
    }
}

impl std::fmt::Display for ProfileRiskSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileRiskSource::Rows(p) => write!(
                f,
                "{} mirrored run profile(s) carrying {} `[risk]` row(s)",
                p.len(),
                p.iter().map(|x| x.rows.len()).sum::<usize>()
            ),
            ProfileRiskSource::NoDatabase { path } => {
                write!(
                    f,
                    "no settings database at {} — the profile file answers alone",
                    path.display()
                )
            }
            // ⚠ It named the RETIRED `profile_risk` table until the run profile's body moved onto
            // the profile plane. Its one constructor is now `vike-cli config show`'s projection of
            // `profile_store`, so the absence this reports is the PROFILE TABLES' — a store
            // written before they existed, which is a different fact from "this profile sets no
            // ceiling" and must not read as one.
            ProfileRiskSource::TableAbsent { path } => write!(
                f,
                "the settings database {} carries no profile tables yet — `vike-cli config \
                 bootstrap-run` is what creates them",
                path.display()
            ),
        }
    }
}
