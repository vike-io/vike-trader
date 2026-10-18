//! **The two section spellings a profile's parameter-search grid is written under, and the
//! presence rule that says whether a profile declares one.**
//!
//! `[paramscan]` is the name; `[sweep]` loads FOREVER. Ruling R2 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` renamed the section, and the
//! engine landed the rename as a serde ALIAS rather than a replacement
//! (`crates/vike-backtest/src/harness/profile.rs`'s `BacktestProfile::paramscan`:
//! `#[serde(default, alias = "sweep")]`), because `BacktestProfile` is `deny_unknown_fields` and
//! every profile already on an operator's disk must keep loading. A serde attribute takes a string
//! LITERAL, so the engine cannot spell the pair from [`PARAMSCAN_SECTIONS`]; what holds the two
//! together is `crates/vike-backtest/src/harness/profile/tests/core_and_data.rs`'s
//! `the_paramscan_sections_are_exactly_the_spellings_the_engine_loads`, which drives every member
//! of the const through the real profile deserializer.
//!
//! The rule — "present AND a non-empty TABLE" — is the engine's
//! `BacktestProfile::is_paramscan`, and `vike-cli` needs the same answer BEFORE it has a profile
//! (its route pre-parse reads the raw TOML to pick the wire verb and links no engine crate).
//! [`declares_a_grid`] is that pre-parse's predicate. It takes the lookup as a closure instead of a
//! `toml::Table` because this crate has no `toml` dependency and none may be added for it
//! (`crates/vike-model/src/params.rs`'s `Params`: serde is byte-identical to `toml::Value`'s, so
//! TOML stays at the text edges); the caller says what it found under a name and this module
//! applies the rule.

/// The profile section names a parameter-search grid is written under, in LOOKUP order: the
/// current name first, then the permanent legacy alias. Both are live forever.
pub const PARAMSCAN_SECTIONS: [&str; 2] = ["paramscan", "sweep"];

/// What a pre-parse found under one section name — the only facts [`declares_a_grid`] needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionShape {
    /// A TABLE. `empty` says whether it holds no key.
    Table {
        /// `true` for a bare `[paramscan]` header with nothing under it.
        empty: bool,
    },
    /// Present and not a table (a scalar, an array): the wrong SHAPE for a grid.
    NotATable,
}

/// Whether a profile declares a parameter-search grid, from what `section_shape` finds under each
/// of [`PARAMSCAN_SECTIONS`]:
///
/// * `Some(true)` — a non-empty table under `paramscan`, or (with none under `paramscan`) under
///   `sweep`;
/// * `Some(false)` — no such section, or an EMPTY table (a present-and-empty grid is no search);
/// * `None` — the section is present and the wrong shape, so THIS side cannot tell and the engine's
///   own parser, which refuses it with a type error naming the field, must answer.
///
/// ⚠ The names are tried IN ORDER and the first one present wins, exactly as the pre-parse always
/// did. A profile carrying BOTH is a serde duplicate-field error on the engine side, which is the
/// right answer and not this function's to produce. "Text that is not TOML at all" is the CALLER's
/// `None` too, before it has any lookup to hand over.
pub fn declares_a_grid(
    mut section_shape: impl FnMut(&str) -> Option<SectionShape>,
) -> Option<bool> {
    match PARAMSCAN_SECTIONS.iter().find_map(|name| section_shape(name)) {
        None => Some(false),
        Some(SectionShape::Table { empty }) => Some(!empty),
        Some(SectionShape::NotATable) => None,
    }
}

#[cfg(test)]
#[path = "paramscan_tests.rs"]
mod tests;
