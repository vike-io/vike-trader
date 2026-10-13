//! **The two `[data]` coverage vocabularies a run request spells: what an armed coverage gate DOES
//! about a finding ([`OnGap`], `data.on_gap`) and the point-in-time universe rule
//! ([`UniverseMode`], `data.universe`).**
//!
//! Pure enums, their ONE roster each (`NAMES`) and the pure parse/spelling code over it. What the
//! engine DOES with a value — the plan, the coverage verdict, `CoverageGate`, `enforce` — stays in
//! `crates/vike-backtest/src/data_plan.rs` and `crates/vike-backtest/src/data_plan/verdict.rs`,
//! which switch on these variants; the resolvers (`DataCfg::on_gap`, `DataCfg::universe_mode`) and
//! the refusal sentences stay in `crates/vike-backtest/src/harness/profile/data_cfg.rs`.
//!
//! # Why this lives below the engine
//!
//! `vike-cli` validates `--on-gap` and `--universe` on its own side (a typo is a local usage error,
//! not a wasted round trip to a compute daemon) and links no engine crate. Both rosters were a
//! HAND-WRITTEN COPY there until they moved here: `crates/vike-cli/src/surface.rs`'s
//! `gap_dispositions` and `universe_modes` rows and `crates/vike-cli/src/cmd/data.rs`'s validators
//! now read [`OnGap::NAMES`] and [`UniverseMode::NAMES`] themselves, so a fourth disposition added
//! to the enum is a compile error in the one place that must learn about it
//! ([`OnGap::name`], an exhaustive `match`) and reaches the CLI with no second edit.

/// What an armed coverage gate DOES about a finding.
///
/// Declared beside [`UniverseMode`], in the vocabulary crate, because the engine that switches on it
/// and the CLI that spells it must both name ONE enumeration: the accepted set a refusal names and
/// the dispositions the engine actually implements cannot come apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnGap {
    /// Nothing runs. The default, because a gate that only warns is invisible on every run whose
    /// output nobody reads — which is every scripted run.
    Refuse,
    /// The findings are logged and the run proceeds, unchanged.
    Warn,
    /// The gate is inert. It exists so a committed profile can keep its statement of what the run
    /// is SUPPOSED to require while an operator overrides today's disposition.
    Run,
}

impl OnGap {
    /// ⚠ **Every spelling this type accepts, in the order a refusal names them — THE roster.**
    ///
    /// It exists because a MUTATION PROOF found the hole it closes. `DataCfg::on_gap` (in
    /// `crates/vike-backtest/src/harness/profile/data_cfg.rs`) matched three string arms and
    /// refused with a hand-written sentence that listed the same three, and
    /// `crates/vike-cli/src/surface.rs`'s `gap_dispositions` row was a third copy. A test comparing
    /// the CLI's copy to the REFUSAL passed with a fourth arm planted in the resolver, because the
    /// message is a copy too — so what it held was copy-against-copy, and the arms were free to
    /// drift away from both.
    ///
    /// So this array is the only door. [`Self::parse`] is the resolver — a walk over these names,
    /// not a `match` — and [`Self::name`] is an exhaustive `match` on the VARIANT, which makes a
    /// new variant a compile error here and forces its row in. A fourth ACCEPTED spelling now
    /// needs a row, and every reader (the refusal, the CLI roster, the CLI validator, the docs
    /// asset) reads the row.
    pub const NAMES: [&'static str; 3] = ["refuse", "warn", "run"];

    /// The disposition a NAME means, case- and space-insensitively, or `None` for one this type
    /// does not accept. The ONE acceptance rule: a spelling absent from [`Self::NAMES`] cannot be
    /// accepted by any path.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let want = raw.trim().to_ascii_lowercase();
        Self::NAMES.iter().position(|n| *n == want).map(|i| match i {
            0 => OnGap::Refuse,
            1 => OnGap::Warn,
            _ => OnGap::Run,
        })
    }

    /// The name a variant carries. ⚠ An exhaustive `match` deliberately, NOT an index: this is
    /// what makes a new VARIANT fail to compile until [`Self::NAMES`] and [`Self::parse`] have
    /// been taught about it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            OnGap::Refuse => "refuse",
            OnGap::Warn => "warn",
            OnGap::Run => "run",
        }
    }

    /// The accepted set as a refusal renders it — so no message spells the roster by hand.
    #[must_use]
    pub fn roster() -> String {
        Self::NAMES.join(" | ")
    }
}

/// The point-in-time universe rule — `data.universe`.
///
/// ⚠ **None of the three ever DROPS a member.** `DataCfg::universe` (in
/// `crates/vike-backtest/src/harness/profile/data_cfg.rs`) carries the whole argument; the short
/// form is that two sites resolve the slice and the bar engine asserts one row per symbol per
/// step, so a filter applied in one of them desyncs the symbol slots — and a universe a run
/// narrowed silently would not be the one the committed profile names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniverseMode {
    /// The symbol list verbatim. The default, and byte-identical to before the key existed.
    Declared,
    /// A member whose tape does not span the window is NAMED; the run proceeds.
    Covered,
    /// That run is REFUSED.
    Strict,
}

impl UniverseMode {
    /// Every spelling this type accepts, in refusal order — THE roster, on [`OnGap::NAMES`]'
    /// terms exactly, including the mutation proof that motivated it.
    pub const NAMES: [&'static str; 3] = ["declared", "covered", "strict"];

    /// The mode a NAME means, or `None` for one this type does not accept.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let want = raw.trim().to_ascii_lowercase();
        Self::NAMES.iter().position(|n| *n == want).map(|i| match i {
            0 => UniverseMode::Declared,
            1 => UniverseMode::Covered,
            _ => UniverseMode::Strict,
        })
    }

    /// The name a variant carries — an exhaustive `match`, for [`OnGap::name`]'s reason.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            UniverseMode::Declared => "declared",
            UniverseMode::Covered => "covered",
            UniverseMode::Strict => "strict",
        }
    }

    /// The accepted set as a refusal renders it.
    #[must_use]
    pub fn roster() -> String {
        Self::NAMES.join(" | ")
    }

    /// `true` when this mode reads the store at all. [`UniverseMode::Declared`] does not, which is
    /// what makes an ordinary profile pay nothing.
    pub fn consults_the_store(self) -> bool {
        !matches!(self, UniverseMode::Declared)
    }
}

#[cfg(test)]
#[path = "gap_tests.rs"]
mod tests;
