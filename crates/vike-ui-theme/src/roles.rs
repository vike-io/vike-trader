//! Colour ROLES: the closed vocabulary `ui-theme.toml` names a colour by, and what each role is on the
//! installed appearance.
//!
//! A row of the TOML's `[[mode]]` table says that a DEMO chip is outlined in `info` and its label is
//! `info` — a ROLE, never a hex — and this module is where that sentence is read. The role is resolved
//! at run time against the live [`Tokens`], so `accent` is the accent of the theme the trader chose
//! and `info` is the one status blue, and a theme change reaches the chip on the next frame. Changing
//! which colour a mode wears is therefore a one-line edit of the TOML, with nothing in Rust to touch.
//!
//! # One vocabulary, in one place
//!
//! [`ColourRole`] is that place: the macro below writes the enum, [`ColourRole::ALL`] and
//! [`ColourRole::key`] from ONE list, and [`ColourRole::resolve`] is an exhaustive `match` over it, so a
//! role that is added without a colour is a compile error. The generator that reads the TOML
//! (`crates/vike-ui-theme/tests/tokens_gen/modes.rs`) links this crate and validates a row against
//! [`ColourRole::from_key`] and prints [`ColourRole::ALL`] in its refusals and on the brand book, so
//! it carries no copy of the list for a test to compare.
//!
//! # What is generated
//!
//! [`modes`] — one [`ModeRow`] constant per `[[mode]]` row and [`modes::ALL`], the registry the brand
//! book and the tests iterate — is GENERATED into `mode_tokens.rs`; `components::chip::Mode` reads its
//! row, so the chip, the account chip and the venue chip's mark cannot disagree with the TOML.

use egui::{Color32, Stroke};

use crate::components::{ON_FILL, Status, Tokens};

/// Writes [`ColourRole`], [`ColourRole::ALL`] and [`ColourRole::key`] from one list, so the three
/// cannot drift apart.
macro_rules! colour_roles {
    ($($(#[$doc:meta])* $variant:ident = $key:literal;)*) => {
        /// A colour, by what it is FOR rather than by its value.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ColourRole {
            $($(#[$doc])* $variant,)*
        }

        const ROLE_COUNT: usize = [$(stringify!($variant)),*].len();

        impl ColourRole {
            /// Every role, in the order the TOML's vocabulary lists them.
            pub const ALL: [ColourRole; ROLE_COUNT] = [$(ColourRole::$variant),*];

            /// The word `ui-theme.toml` writes.
            pub const fn key(self) -> &'static str {
                match self {
                    $(ColourRole::$variant => $key,)*
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
    Text2 = "text2";
    /// The theme's caption grey, the faintest text.
    Text3 = "text3";
    /// The theme's background.
    Bg = "bg";
    /// The theme's surface, one step above the background.
    Surface = "surface";
    /// The theme's card, one step above the surface.
    Card = "card";
    /// The theme's hover fill: what a card or a row turns into under the pointer.
    Hover = "hover";
    /// The theme's UI text, one step brighter than the secondary text (`.strong()`).
    TextUi = "text_ui";
    /// The theme's gradient top: the lighter end of a panel's fill.
    GradTop = "grad_top";
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

impl ColourRole {
    /// The role a TOML word names.
    pub fn from_key(key: &str) -> Option<ColourRole> {
        ColourRole::ALL.into_iter().find(|r| r.key() == key)
    }

    /// The colour this role is under the appearance `t` was read from. `None` is transparent.
    pub fn resolve(self, t: &Tokens) -> Color32 {
        match self {
            ColourRole::Accent => t.theme.accent,
            ColourRole::Border => t.theme.border,
            ColourRole::Text => t.theme.text,
            ColourRole::Text2 => t.theme.text2,
            ColourRole::Text3 => t.theme.text3,
            ColourRole::Bg => t.theme.bg,
            ColourRole::Surface => t.theme.surface,
            ColourRole::Card => t.theme.card,
            ColourRole::Hover => t.theme.hover,
            ColourRole::TextUi => t.theme.text_ui,
            ColourRole::GradTop => t.theme.grad_top,
            ColourRole::AnalysisLine => t.theme.analysis_line,
            ColourRole::Up => t.market.up,
            ColourRole::Down => t.market.down,
            ColourRole::UpText => t.market.up_text,
            ColourRole::DownText => t.market.down_text,
            ColourRole::Ok => Status::Ok.color(),
            ColourRole::Warning => Status::Warning.color(),
            ColourRole::Error => Status::Error.color(),
            ColourRole::Info => Status::Info.color(),
            ColourRole::Muted => Status::Muted.color(),
            ColourRole::OnFill => ON_FILL,
            ColourRole::None => Color32::TRANSPARENT,
        }
    }

    /// An outline of `width` in this role: `Stroke::NONE` for `None`, so a mode that has no outline
    /// draws none rather than a transparent one.
    pub fn stroke(self, width: f32, t: &Tokens) -> Stroke {
        match self {
            ColourRole::None => Stroke::NONE,
            role => Stroke::new(width, role.resolve(t)),
        }
    }
}

/// How the venue chip's mode mark is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkShape {
    /// A filled disc.
    Filled,
    /// An unfilled ring.
    Ring,
}

impl MarkShape {
    /// Every shape, in the order the TOML's vocabulary lists them.
    pub const ALL: [MarkShape; 2] = [MarkShape::Filled, MarkShape::Ring];

    /// The word `ui-theme.toml` writes.
    pub const fn key(self) -> &'static str {
        match self {
            MarkShape::Filled => "filled",
            MarkShape::Ring => "ring",
        }
    }

    /// The shape a TOML word names.
    pub fn from_key(key: &str) -> Option<MarkShape> {
        MarkShape::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// One `[[mode]]` row of `ui-theme.toml`: the roles an account mode's chip and mark are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeRow {
    /// The mode's label as the chip prints it (`LIVE`).
    pub name: &'static str,
    /// The chip's fill; `None` for an outlined chip.
    pub fill: ColourRole,
    /// The chip's outline; `None` for a filled chip.
    pub stroke: ColourRole,
    /// The chip's label.
    pub text: ColourRole,
    /// The venue chip's mark: a disc or a ring...
    pub mark: MarkShape,
    /// ...and its colour.
    pub mark_colour: ColourRole,
    /// What the mode is and why it wears these colours, one line.
    pub doc: &'static str,
}

// The rows: GENERATED from `crates/vike-ui-theme/ui-theme.toml` (`[[mode]]`), one constant each, and
// `modes::ALL`, the registry.
include!("mode_tokens.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::theme::ThemeId;

    fn tokens(id: ThemeId) -> Tokens {
        Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() })
    }

    /// The word a role is written by names exactly that role, and no two roles share one: the
    /// generator resolves a TOML word through `from_key`.
    #[test]
    fn every_role_has_its_own_word_and_it_reads_back() {
        for r in ColourRole::ALL {
            assert_eq!(ColourRole::from_key(r.key()), Some(r), "{r:?}");
        }
        let mut words: Vec<&str> = ColourRole::ALL.iter().map(|r| r.key()).collect();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), ColourRole::ALL.len(), "two roles share a word");
        assert_eq!(ColourRole::from_key("mauve"), None);
        for s in MarkShape::ALL {
            assert_eq!(MarkShape::from_key(s.key()), Some(s), "{s:?}");
        }
    }

    /// Only `none` is transparent, so a label or a mark in any other role is drawn; and a status
    /// role is the same colour on every theme — it never takes the accent's place (spec §2).
    #[test]
    fn only_none_is_transparent_and_a_status_role_never_moves() {
        for id in ThemeId::ALL {
            let t = tokens(id);
            for r in ColourRole::ALL {
                let c = r.resolve(&t);
                if r == ColourRole::None {
                    assert_eq!(c, Color32::TRANSPARENT, "{id:?} none");
                } else {
                    assert_eq!(c.a(), 255, "{id:?} {r:?} is opaque: {c:?}");
                }
            }
            for (role, status) in [
                (ColourRole::Ok, Status::Ok),
                (ColourRole::Warning, Status::Warning),
                (ColourRole::Error, Status::Error),
                (ColourRole::Info, Status::Info),
                (ColourRole::Muted, Status::Muted),
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
        for id in ThemeId::ALL {
            let t = tokens(id);
            for row in modes::ALL {
                let at = format!("{id:?} {}", row.name);
                assert_ne!(row.text, ColourRole::None, "{at}: a label with no colour");
                assert_ne!(row.mark_colour, ColourRole::None, "{at}: a mark with no colour");
                assert_ne!(row.text.resolve(&t), Color32::TRANSPARENT, "{at}: text");
                assert_ne!(row.mark_colour.resolve(&t), Color32::TRANSPARENT, "{at}: mark");
                assert_eq!(
                    row.stroke.stroke(1.0, &t) == Stroke::NONE,
                    row.stroke == ColourRole::None
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
