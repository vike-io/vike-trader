//! The app's icons (design system spec §9) — one Phosphor glyph per MEANING, drawn from the bundled
//! Phosphor face in a font family of its own.
//!
//! **One meaning, one icon.** Every icon the GUI shows is a constant below, named for what it
//! MEANS, and no two constants share a glyph (`one_icon_per_meaning`). Before this module the GUI
//! printed Unicode symbols and emoji: one glyph could mean four things (✕ was close, remove, delete
//! and cancel) while one meaning had two glyphs (refresh was ↻ and ⟳).
//! `crates/vike-ops/tests/ui_glyph_coverage.rs` keeps it that way: a pictograph printed anywhere
//! but here is refused, and so is a private-use character.
//!
//! **Why a family of its own.** Phosphor maps its icons to Private Use Area codepoints, and so does
//! Inter 4.1 — 745 of them, 307 shared with Phosphor (measured 2026-09-29). egui draws a character
//! with the FIRST face in a family's chain that maps it, so an icon in a text family behind Inter
//! draws Inter's own glyph instead, and Phosphor cannot lead a text family either: it maps `a`–`z`
//! to the blank glyphs its ligatures are spelled with. So [`FAMILY`] is the Phosphor face alone
//! (then egui's Hack, whose replacement glyph makes a missing icon a visible box), and an icon
//! reaches the screen only through this type:
//! - [`Icon::rich`], and `From<Icon> for WidgetText` — which is what makes
//!   `ui.button((icons::REFRESH, "Refresh"))` an icon followed by a word;
//! - [`Icon::before`] for a label or a tooltip, which take text rather than atoms;
//! - [`Icon::paint`] for a painter.
//!
//! A widget that shows an icon and no words goes through [`named`], which gives it the words its
//! tooltip carries as what a screen reader reads.
//!
//! `Icon` has no `Display`, on purpose: formatting its character into ordinary text is exactly the
//! wrong-face bug.
//!
//! The face and its provenance: `assets/fonts/SOURCES.md`. Each constant carries its Phosphor
//! name, and `every_icon_is_the_phosphor_glyph_its_name_says` holds the name to the codepoint
//! against the release's own stylesheet.

use egui::text::LayoutJob;
use egui::{Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, RichText, TextFormat};

/// The font family every icon draws in.
pub const FAMILY: &str = "icons";

/// [`FAMILY`] as an egui font family.
pub fn family() -> FontFamily {
    FontFamily::Name(FAMILY.into())
}

/// One icon: a Phosphor glyph and the upstream name it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Icon {
    name: &'static str,
    codepoint: char,
}

impl Icon {
    /// The Phosphor name — how a reader finds this glyph at phosphoricons.com.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The icon as text in [`FAMILY`]. Size and colour come from whatever it is given to, or from
    /// `.size(…)` / `.color(…)` on the result.
    pub fn rich(self) -> RichText {
        RichText::new(self.codepoint.to_string()).family(family())
    }

    /// The icon, a space, then `words` — ONE paragraph that wraps as one, for the widgets that take
    /// text rather than atoms (`ui.label`, a tooltip). The icon takes the words' size and colour.
    pub fn before(self, style: &egui::Style, words: impl Into<RichText>) -> LayoutJob {
        let mut tail = LayoutJob::default();
        words.into().append_to(&mut tail, style, egui::FontSelection::Default, egui::Align::Center);
        let format = tail.sections.first().map(|s| s.format.clone()).unwrap_or_default();
        let icon_format =
            TextFormat { font_id: FontId::new(format.font_id.size, family()), ..format.clone() };
        let mut job = LayoutJob::default();
        job.append(&self.codepoint.to_string(), 0.0, icon_format);
        job.append(" ", 0.0, format.clone());
        job.append(&tail.text, 0.0, format);
        job
    }

    /// Paint the icon. `font` must be in [`family`]: spelling it
    /// `FontId::new(<px>, icons::family())` keeps the size visible to
    /// `crates/vike-ops/tests/ui_literal_ratchet.rs`, like any other.
    pub fn paint(
        self,
        painter: &Painter,
        pos: Pos2,
        anchor: Align2,
        font: FontId,
        color: Color32,
    ) -> Rect {
        debug_assert_eq!(
            font.family,
            family(),
            "an icon painted in a text family is the wrong glyph"
        );
        painter.text(pos, anchor, self.codepoint, font, color)
    }

    /// The accessible name egui gives a widget that shows this icon — followed by ` words` when the
    /// icon leads words, because egui joins a button's text atoms with a space. For a test that
    /// finds a widget by its name; never for display (the module doc says why).
    pub fn accessible_label(self, words: &str) -> String {
        if words.is_empty() {
            self.codepoint.to_string()
        } else {
            format!("{} {words}", self.codepoint)
        }
    }
}

impl From<Icon> for egui::WidgetText {
    fn from(icon: Icon) -> Self {
        icon.rich().into()
    }
}

/// Name a widget that shows only an icon by what it does, and show the same words on hover.
///
/// egui names a widget by its text, and an icon's text is a private-use codepoint no screen reader
/// can speak, while hover text reaches no accessibility node. So an icon-only widget hands its
/// response here with the words its tooltip carries: the node reads them where egui would have put
/// the text — a label's VALUE, any other widget's name — and the pointer gets them as the tip. One
/// string, so the two cannot disagree, and a test finds the widget by the words a person would use.
pub fn named(response: egui::Response, words: &str) -> egui::Response {
    response.ctx.accesskit_node_builder(response.id, |node| {
        if node.role() == egui::accesskit::Role::Label {
            node.set_value(words);
        } else {
            node.set_label(words);
        }
    });
    response.on_hover_text(words)
}

/// Declares every icon constant AND [`ALL`] from one list, so the list the tests and the gallery
/// walk cannot miss a constant.
macro_rules! registry {
    ($( $(#[doc = $doc:literal])* $konst:ident = $name:literal, $cp:literal; )*) => {
        $(
            $(#[doc = $doc])*
            pub const $konst: Icon = Icon { name: $name, codepoint: $cp };
        )*
        /// Every icon, by constant name, in declaration order.
        pub const ALL: &[(&str, Icon)] = &[ $( (stringify!($konst), $konst), )* ];
    };
}

registry! {
    // ── Windows ──
    /// Close a window, a popup or a pane.
    CLOSE = "x", '\u{E4F6}';
    /// Minimize a window: a tool window to the rail, the app to the taskbar.
    MINIMIZE = "minus", '\u{E32A}';
    /// Make a window, or the chart's price pane, fill its space.
    MAXIMIZE = "square", '\u{E45E}';
    /// Undo MAXIMIZE.
    RESTORE = "copy-simple", '\u{E1CC}';
    // ── Window kinds (`WinKind::icon`) ──
    /// The chart window.
    CHART = "chart-line", '\u{E154}';
    /// The Account window.
    ACCOUNT = "wallet", '\u{E68A}';
    /// The price ladder: the Trade window's glyph, and the ladder toggle in its view controls.
    DOM = "ladder-simple", '\u{EC26}';
    /// The options-chain window.
    OPTIONS = "target", '\u{E47C}';
    /// The Greeks window — the calculation PR 3 had to drop.
    GREEKS = "math-operations", '\u{E31E}';
    /// The News window.
    NEWS = "newspaper", '\u{E344}';
    /// The economic-calendar window.
    CALENDAR = "calendar-blank", '\u{E10A}';
    /// Stored market data: the Data Manager window, Studio's Data tab and its header.
    DATA = "database", '\u{E1DE}';
    /// The Studio: its window, its toolbar mark and its empty-results art — the test tube PR 3
    /// had to drop.
    STUDIO = "flask", '\u{E79E}';
    /// The Connections window.
    CONNECTIONS = "plugs-connected", '\u{EB5A}';
    /// The Tearsheet window.
    TEARSHEET = "chart-bar", '\u{E150}';
    /// The Polymarket cockpit window.
    POLYMARKET = "dice-five", '\u{E1EE}';
    // ── Data Manager destinations (`DataDest::icon`) ──
    /// The Data Manager's Overview.
    OVERVIEW = "squares-four", '\u{E464}';
    /// Every stored series.
    ALL_SERIES = "table", '\u{E476}';
    /// Series with missing days — the destination, its attention rows and the foot line's count.
    HAS_GAPS = "calendar-x", '\u{E10C}';
    /// Series that stopped updating.
    STALE = "hourglass", '\u{E2B2}';
    /// Stored series grouped by venue.
    BY_VENUE = "buildings", '\u{E102}';
    /// Live bar feeds held in memory.
    CACHED_FEEDS = "broadcast", '\u{E0F2}';
    /// Where stored data comes from.
    PROVIDERS = "cloud-arrow-down", '\u{E1AC}';
    /// What the Data Manager did, newest first.
    ACTIVITY_LOG = "scroll", '\u{EB7A}';
    /// Saved symbol universes.
    DATASETS = "stack", '\u{E466}';
    /// Which venue may trade live.
    VENUE_ARMING = "power", '\u{E3DA}';
    /// The instrument catalogue.
    INSTRUMENTS = "list-magnifying-glass", '\u{EBE0}';
    /// The mounted store itself.
    STORE = "hard-drives", '\u{E2A0}';
    // ── Actions ──
    /// Read again what is shown: refresh, rescan, the scan in progress.
    REFRESH = "arrows-clockwise", '\u{E094}';
    /// Stop a feed and start it again.
    RESTART = "arrow-clockwise", '\u{E036}';
    /// Drop a feed's connection and dial it again.
    RECONNECT = "plugs", '\u{EB56}';
    /// Take an item out of a list or a chart; it is not destroyed.
    REMOVE = "minus-circle", '\u{E32C}';
    /// Destroy something stored: a DataSet, a stored series, a saved strategy.
    DELETE = "trash", '\u{E4A6}';
    /// Empty a log or a cache — the broom PR 3 had to drop.
    CLEAR = "broom", '\u{EC54}';
    /// Cancel what is in flight: open orders, resting orders, running jobs.
    CANCEL = "x-square", '\u{E4FA}';
    /// Write what is shown to a file.
    EXPORT = "export", '\u{EAF0}';
    /// Fetch missing history.
    BACKFILL = "clock-counter-clockwise", '\u{E1A0}';
    /// Save.
    SAVE = "floppy-disk", '\u{E248}';
    /// Start: run a backtest, a study or a sweep; test a symbol; resume a feed.
    RUN = "play", '\u{E3D0}';
    /// Pause a feed.
    PAUSE = "pause", '\u{E39E}';
    /// A feed's rate limit.
    RATE_LIMIT = "gauge", '\u{E628}';
    /// Edit what this row holds.
    EDIT = "pencil-simple", '\u{E3B4}';
    /// Hand a setting you changed back to its default — a chart colour back to the theme's.
    REVERT = "arrow-counter-clockwise", '\u{E038}';
    /// Search.
    SEARCH = "magnifying-glass", '\u{E30C}';
    /// Settings for what this sits on.
    SETTINGS = "gear-six", '\u{E272}';
    /// More actions.
    MORE = "dots-three", '\u{E1FE}';
    /// Open outside the app.
    EXTERNAL = "arrow-square-out", '\u{E5DE}';
    // ── Navigation ──
    /// Move earlier in time: pan the chart left, the calendar back.
    EARLIER = "arrow-left", '\u{E058}';
    /// Move later in time.
    LATER = "arrow-right", '\u{E06C}';
    /// Move a pane up.
    MOVE_UP = "arrow-up", '\u{E08E}';
    /// Move a pane down.
    MOVE_DOWN = "arrow-down", '\u{E03E}';
    /// Zoom in.
    ZOOM_IN = "magnifying-glass-plus", '\u{E310}';
    /// Zoom out.
    ZOOM_OUT = "magnifying-glass-minus", '\u{E30E}';
    /// Step a setting up: a ladder's price grouping.
    INCREASE = "plus-square", '\u{ED4A}';
    /// Step a setting down.
    DECREASE = "minus-square", '\u{ED4C}';
    /// Put the chart back to its default view.
    RESET_VIEW = "house", '\u{E2C2}';
    /// Follow the chart's symbol.
    FOLLOW_CHART = "link", '\u{E2E2}';
    /// Recentre the ladder on the last price.
    RECENTER = "crosshair", '\u{E1D6}';
    /// There is more below: a dropdown, an open section.
    DISCLOSE_OPEN = "caret-down", '\u{E136}';
    /// There is more to the side: a submenu, a closed section.
    DISCLOSE_CLOSED = "caret-right", '\u{E13A}';
    /// Bring a collapsed side panel back.
    EXPAND_PANEL = "caret-double-right", '\u{E12A}';
    /// Fold a side panel away.
    COLLAPSE_PANEL = "caret-double-left", '\u{E128}';
    /// The column is sorted smallest first.
    SORT_ASCENDING = "sort-ascending", '\u{E444}';
    /// The column is sorted largest first.
    SORT_DESCENDING = "sort-descending", '\u{E446}';
    // ── States ──
    /// Look at this before you rely on it.
    WARNING = "warning", '\u{E4E0}';
    /// Present, done.
    CHECK = "check", '\u{E182}';
    /// It failed.
    FAILED = "x-circle", '\u{E4F8}';
    /// A favourite: the favourites header, and each row's toggle.
    FAVOURITE = "star", '\u{E46A}';
    /// The best-ranked row.
    BEST = "trophy", '\u{E67E}';
    // ── Studio ──
    /// A strategy: the tab, its header and the source chips — the puzzle PR 3 had to drop.
    STRATEGY = "puzzle-piece", '\u{E596}';
    /// Sweep a parameter grid and validate.
    SWEEP = "grid-nine", '\u{EC8C}';
    /// Research: studies and their runs.
    RESEARCH = "binoculars", '\u{EA64}';
    /// The AI chat.
    CHAT = "chat-text", '\u{E17A}';
    /// Saved strategies.
    SAVED = "bookmarks-simple", '\u{E5F0}';
    /// Indicators: the chart's picker, its title and Studio's tab.
    INDICATORS = "function", '\u{EBE4}';
    /// The indicators you wrote yourself.
    MY_INDICATORS = "user", '\u{E4C2}';
    // ── Panes with no rows (`components::state`) ──
    /// A pane with nothing to show.
    EMPTY = "tray", '\u{E4AA}';
    /// A pane that could not reach its source.
    UNREACHABLE = "cloud-slash", '\u{E1B6}';
    // ── The Trade window ──
    /// Put the order ticket beside the ladder.
    PANEL_BESIDE = "square-split-horizontal", '\u{E870}';
    /// Put the order ticket under the ladder.
    PANEL_UNDER = "square-split-vertical", '\u{E874}';
    /// One-click trading is on: a ladder click sends at once.
    ONE_CLICK_ON = "lock-simple-open", '\u{E30A}';
    /// One-click trading is off: every order waits for a confirm.
    ONE_CLICK_OFF = "lock-simple", '\u{E308}';
    /// Type the size in the other unit (base coin or quote currency).
    SWAP_UNITS = "arrows-down-up", '\u{E098}';
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::text::{Fonts, TextOptions};

    /// One meaning, one icon: no two constants share a glyph.
    #[test]
    fn one_icon_per_meaning() {
        let mut seen = std::collections::BTreeMap::new();
        for (konst, icon) in ALL {
            if let Some(other) = seen.insert(icon.codepoint, *konst) {
                panic!("{konst} and {other} are the same glyph (`{}`)", icon.name);
            }
        }
        assert!(ALL.len() >= 78, "the registry lost entries: {}", ALL.len());
    }

    /// Every icon is drawn by the Phosphor face: the FIRST face in the icon family's chain that
    /// maps it — which is the face egui draws with.
    #[test]
    fn every_icon_is_drawn_by_the_icon_face() {
        let mut fonts = Fonts::new(TextOptions::default(), crate::fonts::definitions());
        let chars = fonts.fonts.font(&family()).characters().clone();
        for (konst, icon) in ALL {
            let faces = chars
                .get(&icon.codepoint)
                .unwrap_or_else(|| panic!("{konst}: no face maps U+{:04X}", icon.codepoint as u32));
            assert_eq!(faces[0], crate::fonts::ICON_FACE, "{konst} is drawn by {}", faces[0]);
        }
    }

    /// Each constant IS the Phosphor icon its name says. Phosphor's TTF carries no glyph names,
    /// so the release's own stylesheet — pinned beside the TTF — is the record.
    #[test]
    fn every_icon_is_the_phosphor_glyph_its_name_says() {
        let css = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fonts/Phosphor-Regular.css"),
        )
        .expect("the stylesheet ships beside the face");
        for (konst, icon) in ALL {
            let rule = format!(
                ".ph.ph-{}:before {{\n  content: \"\\{:x}\";",
                icon.name, icon.codepoint as u32
            );
            assert!(
                css.contains(&rule),
                "{konst}: Phosphor 2.1.2 does not map `{}` to U+{:04X}",
                icon.name,
                icon.codepoint as u32
            );
        }
    }

    /// An icon before words keeps the words' size and colour and sits in its own family.
    #[test]
    fn before_puts_the_icon_in_its_family_at_the_words_size() {
        let style = egui::Style::default();
        let job = WARNING.before(&style, RichText::new("unread").size(13.0).color(Color32::RED));
        // Two sections, not three: epaint 0.36's `LayoutJob::append` merges the space into the
        // words' section, which has the same format.
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[0].format.font_id, FontId::new(13.0, family()));
        assert_eq!(job.sections[0].format.color, Color32::RED);
        assert_eq!(job.sections[1].format.font_id, FontId::new(13.0, FontFamily::Proportional));
        assert_eq!(job.sections[1].format.color, Color32::RED);
        assert_eq!(job.text, format!("{} unread", WARNING.codepoint));
    }

    /// An icon-only button is named by what it does. egui would name it by its text — the icon's
    /// private-use codepoint, which no screen reader can speak — and hover text reaches no
    /// accessibility node; `named` gives the node the words and shows them on hover.
    #[test]
    fn an_icon_only_button_is_named_by_what_it_does() {
        let ctx = egui::Context::default();
        crate::appearance::install_type(&ctx, crate::type_scale::TextSize::Small);
        ctx.enable_accesskit();
        let mut id = None;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            id = Some(named(ui.button(CLOSE), "Close").id);
        });
        // epaint 0.36 panics on dropping texture deltas nobody applied.
        out.textures_delta.clear();
        let id = id.expect("the frame drew the button").accesskit_id();
        let update = out.platform_output.accesskit_update.expect("AccessKit is on");
        let (_, node) =
            update.nodes.iter().find(|(n, _)| *n == id).expect("the button has an AccessKit node");
        assert_eq!(node.label(), Some("Close"));
    }

    /// An icon-only LABEL is read as its words too. egui keeps a label's text in the node's value
    /// rather than its name, so that is where `named` has to put the words.
    #[test]
    fn an_icon_only_label_is_read_as_its_words() {
        let ctx = egui::Context::default();
        crate::appearance::install_type(&ctx, crate::type_scale::TextSize::Small);
        ctx.enable_accesskit();
        let mut id = None;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            id = Some(named(ui.label(WARNING), "Unread").id);
        });
        out.textures_delta.clear();
        let id = id.expect("the frame drew the label").accesskit_id();
        let update = out.platform_output.accesskit_update.expect("AccessKit is on");
        let (_, node) =
            update.nodes.iter().find(|(n, _)| *n == id).expect("the label has an AccessKit node");
        assert_eq!(node.role(), egui::accesskit::Role::Label);
        assert_eq!(node.value(), Some("Unread"));
    }

    /// The accessible name is what egui reports for an icon button, with and without words.
    #[test]
    fn the_accessible_label_is_the_codepoint_then_the_words() {
        assert_eq!(CLOSE.accessible_label(""), "\u{E4F6}");
        assert_eq!(REFRESH.accessible_label("Refresh"), "\u{E094} Refresh");
    }
}
