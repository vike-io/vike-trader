//! The design values that are not a step of a scale: each window's own measures, strengths and fixed
//! colours — a column's width, a marker's inset, the strength of a hover wash, a series' colour — as
//! `vike_ui_theme::value::<group>::<NAME>`.
//!
//! They are GENERATED from `crates/vike-ui-theme/ui-theme.toml`'s `[[value]]` rows (`group`, `name`,
//! `kind`, `unit`, `value`, `doc`), so the number a window draws with, the CSS variable a mock-up reads
//! and the row on the brand book are one value (decision 0103). A group is a window or an area
//! (`trade`, `data_manager`); a constant's type is its row's `kind` — `f32`, `f64` (for a number the
//! code works with in `f64`, such as a width in egui_plot's bar-width x-units), `u8`, `i8`, `u16`,
//! `egui::Vec2` (a `[width, height]` pair) or `egui::Color32`.
//!
//! The scales that more than one window shares stay where they were: `crate::metrics::{space, stroke,
//! alpha}`. A value belongs here when ONE window (or a few siblings) owns it.
//!
//! [`GROUPS`] is the registry: every group, alphabetical, with its rows as [`Entry`] data. An entry's
//! value is built from the constant itself, so a test that compares a row of the TOML with its entry is
//! comparing what the app compiles. It is what the brand book and `tests/value_table.rs` iterate.

/// What a value is, in Rust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    F32,
    F64,
    U8,
    I8,
    U16,
    /// An `egui::Vec2`, the pair `[width, height]`.
    Vec2,
    /// An `egui::Color32`.
    Colour,
}

impl Kind {
    /// The word `ui-theme.toml` writes (`kind = "f32"`).
    pub const fn key(self) -> &'static str {
        match self {
            Kind::F32 => "f32",
            Kind::F64 => "f64",
            Kind::U8 => "u8",
            Kind::I8 => "i8",
            Kind::U16 => "u16",
            Kind::Vec2 => "vec2",
            Kind::Colour => "colour",
        }
    }
}

/// What a number means, which is how the brand book draws it. A colour has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A length in points.
    Px,
    /// A unitless factor.
    Ratio,
    /// An opacity on the 0..=255 byte scale.
    Alpha,
    /// A whole number that is neither: a count, a limit.
    Count,
}

impl Unit {
    /// The word `ui-theme.toml` writes (`unit = "px"`).
    pub const fn key(self) -> &'static str {
        match self {
            Unit::Px => "px",
            Unit::Ratio => "ratio",
            Unit::Alpha => "alpha",
            Unit::Count => "count",
        }
    }
}

/// A value, with its kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Data {
    F32(f32),
    F64(f64),
    U8(u8),
    I8(i8),
    U16(u16),
    Vec2(egui::Vec2),
    Colour(egui::Color32),
}

impl Data {
    pub const fn kind(&self) -> Kind {
        match self {
            Data::F32(_) => Kind::F32,
            Data::F64(_) => Kind::F64,
            Data::U8(_) => Kind::U8,
            Data::I8(_) => Kind::I8,
            Data::U16(_) => Kind::U16,
            Data::Vec2(_) => Kind::Vec2,
            Data::Colour(_) => Kind::Colour,
        }
    }
}

/// One row of the table, as data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub group: &'static str,
    /// The constant's name within its group's module.
    pub name: &'static str,
    /// What the number means; `None` for a colour.
    pub unit: Option<Unit>,
    pub value: Data,
    /// What it is on screen, one line.
    pub doc: &'static str,
}

impl Entry {
    pub const fn kind(&self) -> Kind {
        self.value.kind()
    }
}

/// One group (one window or area) and its rows, in the order the TOML lists them.
#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub name: &'static str,
    pub entries: &'static [Entry],
}

/// Every entry of every group, alphabetical by group.
pub fn all() -> impl Iterator<Item = &'static Entry> {
    GROUPS.iter().flat_map(|g| g.entries.iter())
}

/// The entry named `name` in `group`.
pub fn find(group: &str, name: &str) -> Option<&'static Entry> {
    all().find(|e| e.group == group && e.name == name)
}

// The groups, their constants and the registry: GENERATED from `crates/vike-ui-theme/ui-theme.toml`
// (`[[value]]`), one `pub mod` per group.
include!("value_tokens.rs");

#[cfg(test)]
mod tests {
    use super::*;

    // The constructors the generator writes for the kinds no row uses yet must be `const fn`s: a
    // colour row becomes `Color32::from_rgb` (opaque) or `from_rgba_unmultiplied_const`, a size
    // `egui::vec2`. If an egui bump makes one of them non-const, this stops compiling here, not
    // at the first window that adds such a row.
    const _: egui::Color32 = egui::Color32::from_rgb(1, 2, 3);
    const _: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(1, 2, 3, 4);
    const _: egui::Vec2 = egui::vec2(1.0, 2.0);

    #[test]
    fn the_groups_are_alphabetical_and_unique() {
        let names: Vec<&str> = GROUPS.iter().map(|g| g.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "GROUPS must be alphabetical, one entry per group");
    }

    #[test]
    fn every_entry_sits_in_its_own_group_and_is_named_once() {
        for g in GROUPS {
            let mut seen = Vec::new();
            for e in g.entries {
                assert_eq!(e.group, g.name, "{} is listed under {}", e.name, g.name);
                assert!(!seen.contains(&e.name), "{}.{} is listed twice", g.name, e.name);
                seen.push(e.name);
            }
        }
    }

    #[test]
    fn a_unit_belongs_to_its_kind() {
        for e in all() {
            let ok = matches!(
                (e.kind(), e.unit),
                (Kind::Colour, None)
                    | (Kind::F32 | Kind::F64, Some(Unit::Px | Unit::Ratio | Unit::Alpha))
                    | (Kind::U8, Some(Unit::Px | Unit::Alpha | Unit::Count))
                    | (Kind::U16, Some(Unit::Px | Unit::Count))
                    | (Kind::I8 | Kind::Vec2, Some(Unit::Px))
            );
            assert!(
                ok,
                "{}.{}: a {} cannot carry unit {:?}",
                e.group,
                e.name,
                e.kind().key(),
                e.unit
            );
        }
    }

    #[test]
    fn find_reads_the_entry_all_lists() {
        for e in all() {
            assert_eq!(find(e.group, e.name), Some(e), "{}.{}", e.group, e.name);
        }
        assert_eq!(find("no_such_group", "NO_SUCH"), None);
    }
}
