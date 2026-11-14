//! **The section name a profile's parameter-search grid is written under, and the presence rule
//! that says whether a profile declares one.**
//!
//! `[paramscan]` is the name, and the only one: ruling R2 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` renamed the section, first as a
//! serde alias that kept `[sweep]` loading, and the owner's ruling 20 (phase 4) removed the alias
//! (`crates/vike-backtest/src/harness/profile.rs`'s `ProfileBacktest::paramscan`), so a `[sweep]`
//! profile is refused at load. What holds [`PARAMSCAN_SECTIONS`] and the engine together is
//! `crates/vike-backtest/src/harness/profile/tests/core_and_data.rs`'s
//! `the_paramscan_sections_are_exactly_the_spellings_the_engine_loads`, which drives every member
//! of the const through the real profile deserializer.
//!
//! The rule — "present AND a non-empty TABLE" — is the engine's
//! `ProfileBacktest::is_paramscan`, and `vike-cli` needs the same answer BEFORE it has a profile
//! (its route pre-parse reads the raw TOML to pick the wire verb and links no engine crate).
//! [`declares_a_grid`] is that pre-parse's predicate. It takes the lookup as a closure instead of a
//! `toml::Table` because this crate has no `toml` dependency and none may be added for it
//! (`crates/vike-model/src/params.rs`'s `Params`: serde is byte-identical to `toml::Value`'s, so
//! TOML stays at the text edges); the caller says what it found under a name and this module
//! applies the rule.

/// The profile section names a parameter-search grid is written under. One since the owner's ruling 20
/// (phase 4): the legacy `sweep` spelling is refused, on the wire and in a profile.
pub const PARAMSCAN_SECTIONS: [&str; 1] = ["paramscan"];

/// What a pre-parse found under one section name — the only facts [`declares_a_grid`] needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamscanSectionShape {
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
/// * `Some(true)` — a non-empty table under `paramscan`;
/// * `Some(false)` — no such section, or an EMPTY table (a present-and-empty grid is no search);
/// * `None` — the section is present and the wrong shape, so THIS side cannot tell and the engine's
///   own parser, which refuses it with a type error naming the field, must answer.
///
/// "Text that is not TOML at all" is the CALLER's `None` too, before it has any lookup to hand over.
pub fn declares_a_grid(
    mut section_shape: impl FnMut(&str) -> Option<ParamscanSectionShape>,
) -> Option<bool> {
    match PARAMSCAN_SECTIONS.iter().find_map(|name| section_shape(name)) {
        None => Some(false),
        Some(ParamscanSectionShape::Table { empty }) => Some(!empty),
        Some(ParamscanSectionShape::NotATable) => None,
    }
}

#[cfg(test)]
#[path = "paramscan_tests.rs"]
mod tests;
