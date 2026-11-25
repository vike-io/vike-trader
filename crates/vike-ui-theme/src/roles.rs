//! Colour ROLES: the closed vocabulary `ui-theme.toml` names a colour by, and what each role is on the
//! installed appearance.
//!
//! A row of the TOML's `[[mode]]` table says that a DEMO chip is outlined in `info` and its label is
//! `info` — a ROLE, never a hex — and this module is where that sentence is read. The role is resolved
//! at run time against the live [`ThemeTokens`], so `accent` is the accent of the theme the trader chose
//! and `info` is the one status blue, and a theme change reaches the chip on the next frame. Changing
//! which colour a mode wears is therefore a one-line edit of the TOML, with nothing in Rust to touch.
//!
//! # One vocabulary, in one place
//!
//! [`ThemeColourRole`] is that place: the macro below writes the enum, [`ThemeColourRole::ALL`] and
//! [`ThemeColourRole::key`] from ONE list, and [`ThemeColourRole::resolve`] is an exhaustive `match` over it, so a
//! role that is added without a colour is a compile error. The generator that reads the TOML
//! (`crates/vike-ui-theme/src/tokens_gen/modes.rs`) links this crate and validates a row against
//! [`ThemeColourRole::from_key`] and prints [`ThemeColourRole::ALL`] in its refusals and on the brand book, so
//! it carries no copy of the list for a test to compare.
//!
//! # What is generated
//!
//! [`modes`] — one [`ThemeAccountTierRow`] constant per `[[mode]]` row and [`modes::ALL`], the registry the brand
//! book and the tests iterate — is GENERATED into `mode_tokens.rs`; `components::chip::tier_roles` reads its
//! row, so the chip, the account chip and the venue chip's mark cannot disagree with the TOML.

use egui::{Color32, Stroke};

use crate::components::{ON_FILL, ThemeStatus, ThemeTokens};

/// Writes [`ThemeColourRole`], [`ThemeColourRole::ALL`] and [`ThemeColourRole::key`] from one list, so the three
/// cannot drift apart.
macro_rules! colour_roles {
    ($($(#[$doc:meta])* $variant:ident = $key:literal;)*) => {
        /// A colour, by what it is FOR rather than by its value.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ThemeColourRole {
            $($(#[$doc])* $variant,)*
        }

        const ROLE_COUNT: usize = [$(stringify!($variant)),*].len();

        impl ThemeColourRole {
            /// Every role, in the order the TOML's vocabulary lists them.
            pub const ALL: [ThemeColourRole; ROLE_COUNT] = [$(ThemeColourRole::$variant),*];

            /// The word `ui-theme.toml` writes.
            pub const fn key(self) -> &'static str {
                match self {
                    $(ThemeColourRole::$variant => $key,)*
                }
            }
        }
    };
}

colour_roles! {
    /// The theme's accent: the colour of a SHAPE (a fill, a ring, an underline, an edge).
    Accent = "accent";
    /// The theme's border grey: the outline of a control or a card.
    Border = "border";
    /// The theme's primary text.
    Text = "text";
    /// The theme's secondary text.
    TextSecondary = "text_secondary";
    /// The theme's caption grey, the faintest text.
    TextTertiary = "text_tertiary";
    /// The theme's background.
    Background = "background";
    /// The theme's surface, one step above the background.
    Surface = "surface";
    /// The theme's card, one step above the surface.
    Card = "card";
    /// The theme's hover fill: what a card or a row turns into under the pointer.
    Hover = "hover";
    /// The theme's UI text, one step brighter than the secondary text (`.strong()`).
    TextUi = "text_ui";
    /// The theme's gradient top: the lighter end of a panel's fill.
    GradientTop = "gradient_top";
    /// The theme's analysis line: a chart's reference line (a grid of levels, a zero line).
    AnalysisLine = "analysis_line";
    /// The market set's "up" as a graphic: a candle, a marker, the BUY fill.
    Up = "up";
    /// The market set's "down" as a graphic.
    Down = "down";
    /// The market set's "up" as text: a P&L, a signed change, a side word.
    UpText = "up_text";
    /// The market set's "down" as text.
    DownText = "down_text";
    /// The status green: healthy, the same in every theme.
    Ok = "ok";
    /// The status amber: not settled yet, the same in every theme.
    Warning = "warning";
    /// The status red: a fault, the same in every theme.
    Error = "error";
    /// The status blue: information, the same in every theme.
    Info = "info";
    /// The status grey: nothing live to show, the same in every theme.
    Muted = "muted";
    /// The label colour on every FILLED control (`components::ON_FILL`).
    OnFill = "on_fill";
    /// No colour: nothing is drawn. Valid for a fill or a stroke, never for a label or a mark.
    None = "none";
}

impl ThemeColourRole {
    /// The role a TOML word names.
    pub fn from_key(key: &str) -> Option<ThemeColourRole> {
        ThemeColourRole::ALL.into_iter().find(|r| r.key() == key)
    }

    /// The colour this role is under the appearance `t` was read from. `None` is transparent.
    pub fn resolve(self, t: &ThemeTokens) -> Color32 {
        match self {
            ThemeColourRole::Accent => t.theme.accent,
            ThemeColourRole::Border => t.theme.border,
            ThemeColourRole::Text => t.theme.text,
            ThemeColourRole::TextSecondary => t.theme.text_secondary,
            ThemeColourRole::TextTertiary => t.theme.text_tertiary,
            ThemeColourRole::Background => t.theme.background,
            ThemeColourRole::Surface => t.theme.surface,
            ThemeColourRole::Card => t.theme.card,
            ThemeColourRole::Hover => t.theme.hover,
            ThemeColourRole::TextUi => t.theme.text_ui,
            ThemeColourRole::GradientTop => t.theme.gradient_top,
            ThemeColourRole::AnalysisLine => t.theme.analysis_line,
            ThemeColourRole::Up => t.market.up,
            ThemeColourRole::Down => t.market.down,
            ThemeColourRole::UpText => t.market.up_text,
            ThemeColourRole::DownText => t.market.down_text,
            ThemeColourRole::Ok => ThemeStatus::Ok.color(),
            ThemeColourRole::Warning => ThemeStatus::Warning.color(),
            ThemeColourRole::Error => ThemeStatus::Error.color(),
            ThemeColourRole::Info => ThemeStatus::Info.color(),
            ThemeColourRole::Muted => ThemeStatus::Muted.color(),
            ThemeColourRole::OnFill => ON_FILL,
            ThemeColourRole::None => Color32::TRANSPARENT,
        }
    }

    /// An outline of `width` in this role: `Stroke::NONE` for `None`, so a mode that has no outline
    /// draws none rather than a transparent one.
    pub fn stroke(self, width: f32, t: &ThemeTokens) -> Stroke {
        match self {
            ThemeColourRole::None => Stroke::NONE,
            role => Stroke::new(width, role.resolve(t)),
        }
    }
}

/// How the venue chip's mode mark is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMarkShape {
    /// A filled disc.
    Filled,
    /// An unfilled ring.
    Ring,
}

impl ThemeMarkShape {
    /// Every shape, in the order the TOML's vocabulary lists them.
    pub const ALL: [ThemeMarkShape; 2] = [ThemeMarkShape::Filled, ThemeMarkShape::Ring];

    /// The word `ui-theme.toml` writes.
    pub const fn key(self) -> &'static str {
        match self {
            ThemeMarkShape::Filled => "filled",
            ThemeMarkShape::Ring => "ring",
        }
    }

    /// The shape a TOML word names.
    pub fn from_key(key: &str) -> Option<ThemeMarkShape> {
        ThemeMarkShape::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// One `[[mode]]` row of `ui-theme.toml`: the roles an account mode's chip and mark are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeAccountTierRow {
    /// The mode's label as the chip prints it (`LIVE`).
    pub name: &'static str,
    /// The chip's fill; `None` for an outlined chip.
    pub fill: ThemeColourRole,
    /// The chip's outline; `None` for a filled chip.
    pub stroke: ThemeColourRole,
    /// The chip's label.
    pub text: ThemeColourRole,
    /// The venue chip's mark: a disc or a ring...
    pub mark: ThemeMarkShape,
    /// ...and its colour.
    pub mark_colour: ThemeColourRole,
    /// What the mode is and why it wears these colours, one line.
    pub doc: &'static str,
}

// The rows: GENERATED from `crates/vike-ui-theme/ui-theme.toml` (`[[mode]]`), one constant each, and
// `modes::ALL`, the registry.
include!("generated/mode_tokens.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::ThemeAppearance;
    use crate::theme::ThemePaletteId;

    fn tokens(id: ThemePaletteId) -> ThemeTokens {
        ThemeTokens::from_appearance(&ThemeAppearance { theme: id, ..ThemeAppearance::default() })
    }

    /// The word a role is written by names exactly that role, and no two roles share one: the
    /// generator resolves a TOML word through `from_key`.
    #[test]
    fn every_role_has_its_own_word_and_it_reads_back() {
        for r in ThemeColourRole::ALL {
            assert_eq!(ThemeColourRole::from_key(r.key()), Some(r), "{r:?}");
        }
        let mut words: Vec<&str> = ThemeColourRole::ALL.iter().map(|r| r.key()).collect();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), ThemeColourRole::ALL.len(), "two roles share a word");
        assert_eq!(ThemeColourRole::from_key("mauve"), None);
        for s in ThemeMarkShape::ALL {
            assert_eq!(ThemeMarkShape::from_key(s.key()), Some(s), "{s:?}");
        }
    }

    /// Only `none` is transparent, so a label or a mark in any other role is drawn; and a status
    /// role is the same colour on every theme — it never takes the accent's place (spec §2).
    #[test]
    fn only_none_is_transparent_and_a_status_role_never_moves() {
        for id in ThemePaletteId::ALL {
            let t = tokens(id);
            for r in ThemeColourRole::ALL {
                let c = r.resolve(&t);
                if r == ThemeColourRole::None {
                    assert_eq!(c, Color32::TRANSPARENT, "{id:?} none");
                } else {
                    assert_eq!(c.a(), 255, "{id:?} {r:?} is opaque: {c:?}");
                }
            }
            for (role, status) in [
                (ThemeColourRole::Ok, ThemeStatus::Ok),
                (ThemeColourRole::Warning, ThemeStatus::Warning),
                (ThemeColourRole::Error, ThemeStatus::Error),
                (ThemeColourRole::Info, ThemeStatus::Info),
                (ThemeColourRole::Muted, ThemeStatus::Muted),
            ] {
                assert_eq!(role.resolve(&t), status.color(), "{id:?} {role:?}");
            }
        }
    }

    /// Every generated row resolves on every theme: its label and its mark are drawn (neither is
    /// `none`), and its outline is a `Stroke::NONE` exactly when the row says `none`.
    #[test]
    fn every_mode_row_resolves_on_every_theme() {
        assert!(!modes::ALL.is_empty(), "no mode row");
        for id in ThemePaletteId::ALL {
            let t = tokens(id);
            for row in modes::ALL {
                let at = format!("{id:?} {}", row.name);
                assert_ne!(row.text, ThemeColourRole::None, "{at}: a label with no colour");
                assert_ne!(row.mark_colour, ThemeColourRole::None, "{at}: a mark with no colour");
                assert_ne!(row.text.resolve(&t), Color32::TRANSPARENT, "{at}: text");
                assert_ne!(row.mark_colour.resolve(&t), Color32::TRANSPARENT, "{at}: mark");
                assert_eq!(
                    row.stroke.stroke(1.0, &t) == Stroke::NONE,
                    row.stroke == ThemeColourRole::None
                );
                assert!(!row.doc.is_empty() && !row.doc.contains('\n'), "{at}: doc");
            }
        }
    }

    /// The registry names each mode once.
    #[test]
    fn no_mode_is_listed_twice() {
        let mut names: Vec<&str> = modes::ALL.iter().map(|r| r.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "a mode is listed twice");
    }
}
