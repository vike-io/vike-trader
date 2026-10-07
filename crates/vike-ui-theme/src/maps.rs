//! The MAPS: every "which colour, word or icon does this state or kind use" mapping, as rows of
//! `ui-theme.toml`'s `[[map]]` table instead of `match` ladders in Rust.
//!
//! A mapping is a Rust enum the code already has (a feed's connection state, a button's kind, a calendar
//! event's importance). Its colours, its word and its icon are ROWS, one per variant, under a map of their own
//! (`maps::connection::DISCONNECTED`), and the enum binds each variant to its row with an exhaustive `match`:
//!
//! ```text
//! impl Message {
//!     fn row(self) -> &'static MapRow {
//!         match self {
//!             Message::Info => &strip::INFO,
//!             Message::Ok => &strip::OK,
//!         }
//!     }
//! }
//! ```
//!
//! so a variant without a row is a compile error, and a row without a variant is a test failure (the test
//! iterates the map's `ALL`). Changing which colour a state wears is then one line of the TOML, with nothing in
//! Rust to touch.
//!
//! # What a row holds
//!
//! Every field but the key and the doc is optional, and which one means what is the MAP's own (the comment above
//! its sentinel in the TOML says): `colour` is the row's main colour (a dot, a mark, a glyph), `fill` and `stroke`
//! a shape's, `text` a label's; each is a [`ColourRole`] resolved at run time (`row.colour.resolve(&tokens)`),
//! and an absent one is [`ColourRole::None`]. `word` is a label shown to the user, `icon` the KEY of an icon of
//! [`crate::icons`] (the constant's name in its `registry!`, as `CONNECTIONS`; [`MapRow::icon()`] reads it),
//! `count` a small number (the bars lit) and `flag` a yes/no (dashed).
//!
//! # What is generated
//!
//! The maps — one `pub mod <map>` per declared map holding one [`MapRow`] constant per row and `ALL`, its rows in
//! the order the TOML lists them, and [`MAPS`], every map alphabetically — are GENERATED into `map_tokens.rs`.
//! [`all_maps`] and [`find`] read that registry, which is also what the brand book and `tests/map_table.rs`
//! iterate. A map with no rows yet still has its module and its `MAPS` line.

use crate::icons::{self, Icon};
use crate::roles::ColourRole;

/// One `[[map]]` row of `ui-theme.toml`: what a state or a kind of a mapping looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapRow {
    /// The state or kind, UPPER_SNAKE, as the row's constant is named (`DISCONNECTED`).
    pub key: &'static str,
    /// The row's main colour: a dot, a mark, a glyph. [`ColourRole::None`] when the row has none.
    pub colour: ColourRole,
    /// A fill behind the label; [`ColourRole::None`] for none.
    pub fill: ColourRole,
    /// An outline; [`ColourRole::None`] for none.
    pub stroke: ColourRole,
    /// The label's colour; [`ColourRole::None`] for none.
    pub text: ColourRole,
    /// A label shown to the user.
    pub word: Option<&'static str>,
    /// The key of the row's icon in [`crate::icons`]'s registry (the constant's name).
    pub icon: Option<&'static str>,
    /// A small number the row carries (the bars lit).
    pub count: Option<u8>,
    /// A yes/no the row carries (dashed).
    pub flag: Option<bool>,
    /// What the row is on screen, one line.
    pub doc: &'static str,
}

impl MapRow {
    /// The icon the row's `icon` key names, which the generator checked is in the registry.
    pub fn icon(&self) -> Option<Icon> {
        let key = self.icon?;
        icons::ALL.iter().find(|(k, _)| *k == key).map(|(_, icon)| *icon)
    }
}

/// One map and its rows, in the order the TOML lists them.
#[derive(Clone, Copy, Debug)]
pub struct Map {
    /// The mapping's name, snake_case, as its module is named (`empty_pane`).
    pub name: &'static str,
    pub rows: &'static [&'static MapRow],
}

// The maps: GENERATED from `crates/vike-ui-theme/ui-theme.toml` (`[[map]]`), one `pub mod` per map and the
// registry `MAPS`.
include!("map_tokens.rs");

/// Every map, alphabetical: what the brand book and the tests iterate.
pub fn all_maps() -> &'static [Map] {
    MAPS
}

/// The row `key` of the map `map`.
pub fn find(map: &str, key: &str) -> Option<&'static MapRow> {
    all_maps().iter().find(|m| m.name == map)?.rows.iter().copied().find(|r| r.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::Tokens;
    use crate::theme::ThemeId;
    use egui::Color32;

    fn tokens(id: ThemeId) -> Tokens {
        Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() })
    }

    /// The registry holds each map once, alphabetically, and each key once within its map.
    #[test]
    fn the_maps_are_alphabetical_and_each_row_is_listed_once() {
        let names: Vec<&str> = all_maps().iter().map(|m| m.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "MAPS must be alphabetical, one entry per map");
        for m in all_maps() {
            let mut keys: Vec<&str> = m.rows.iter().map(|r| r.key).collect();
            let before = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), before, "{}: a row is listed twice", m.name);
        }
    }

    /// A key is the Rust constant's name: UPPER_SNAKE, and not the registry's own.
    #[test]
    fn a_key_is_an_upper_snake_constant_name() {
        for m in all_maps() {
            for r in m.rows {
                let ok = r.key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                    && r.key
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                assert!(ok && r.key != "ALL", "{}.{}: not a constant name", m.name, r.key);
            }
        }
    }

    /// Every row resolves on every theme: each of its four roles is a colour (opaque) or `none` (transparent),
    /// and only `none` is transparent.
    #[test]
    fn every_row_resolves_on_every_theme() {
        for id in ThemeId::ALL {
            let t = tokens(id);
            for m in all_maps() {
                for r in m.rows {
                    for (part, role) in [
                        ("colour", r.colour),
                        ("fill", r.fill),
                        ("stroke", r.stroke),
                        ("text", r.text),
                    ] {
                        let c = role.resolve(&t);
                        let at = format!("{id:?} {}.{} {part}", m.name, r.key);
                        if role == ColourRole::None {
                            assert_eq!(c, Color32::TRANSPARENT, "{at}");
                        } else {
                            assert_eq!(c.a(), 255, "{at} is opaque: {c:?}");
                        }
                    }
                }
            }
        }
    }

    /// Every icon a row names is in the registry (the generator checked it; this checks the compiled rows),
    /// and a row with no icon has none.
    #[test]
    fn every_icon_a_row_names_exists() {
        for m in all_maps() {
            for r in m.rows {
                assert_eq!(
                    r.icon().is_some(),
                    r.icon.is_some(),
                    "{}.{}: icon {:?} is not a key of the registry",
                    m.name,
                    r.key,
                    r.icon
                );
            }
        }
    }

    /// A word and a doc are one line of something; a row is read back by `find`.
    #[test]
    fn a_word_and_a_doc_are_one_line_and_find_reads_the_row_all_lists() {
        for m in all_maps() {
            for r in m.rows {
                let at = format!("{}.{}", m.name, r.key);
                assert!(!r.doc.is_empty() && !r.doc.contains('\n'), "{at}: doc");
                if let Some(w) = r.word {
                    assert!(
                        !w.is_empty() && w == w.trim() && !w.contains('\n'),
                        "{at}: word {w:?}"
                    );
                }
                assert_eq!(find(m.name, r.key), Some(*r), "{at}");
            }
        }
        assert_eq!(find("no_such_map", "NO_SUCH"), None);
    }
}
