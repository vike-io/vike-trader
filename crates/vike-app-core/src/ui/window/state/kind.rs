//! `WindowKind`: which tool a window is, and the logic its kind decides (slug, title tabs).

use vike_ui_theme::icons::ThemeIcon;
use vike_ui_theme::maps::{self, ThemeMapRow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    Chart,
    Trade,
    /// The account overview: equity, then the tabs Accounts, Positions, Orders, Strategies and History
    /// (the Execution-history window's body, whose own kind is gone) — the old Trade window without
    /// its ticket.
    Account,
    Options,
    News,
    Calendar,
    Data,
    Studio,
    /// The Polymarket scalp cockpit — mounts the `vike-order-entry` cockpit widgets (window-chain rail,
    /// Price-to-Beat header, probability ladder, one-click ticket) for the rolling up/down markets.
    Polymarket,
    /// The Settings window: the app's own preferences — today one section, Appearance (design
    /// system spec §5).
    Settings,
}

impl WindowKind {
    /// This kind's row of `ui-theme.toml`'s `window_kind` map (`vike_ui_theme::maps::window_kind`): its
    /// `word` is the window's title and its `icon` the glyph in the title bar, so which words and which
    /// glyph a kind wears is a line of the TOML and nothing here. Exhaustive: a kind without a row does not
    /// compile, and `every_kind_has_its_own_window_kind_row_and_every_row_its_kind` holds the other direction.
    /// (The slug and the title-tab seat below are logic, not looks, and stay here.)
    pub(super) fn row(self) -> &'static ThemeMapRow {
        match self {
            WindowKind::Chart => &maps::window_kind::CHART,
            WindowKind::Trade => &maps::window_kind::TRADE,
            WindowKind::Account => &maps::window_kind::ACCOUNT,
            WindowKind::Options => &maps::window_kind::OPTIONS,
            WindowKind::News => &maps::window_kind::NEWS,
            WindowKind::Calendar => &maps::window_kind::CALENDAR,
            WindowKind::Data => &maps::window_kind::DATA,
            WindowKind::Studio => &maps::window_kind::STUDIO,
            WindowKind::Polymarket => &maps::window_kind::POLYMARKET,
            WindowKind::Settings => &maps::window_kind::SETTINGS,
        }
    }
    /// The window's title: its row's `word`. A title built from data (a chart's `BTCUSDT · 1m`) is not this.
    pub fn label(self) -> &'static str {
        self.row().word.expect("a window_kind row carries the window's title")
    }
    /// The stable lowercase slug of each kind — the `VIKE_TOOL=<slug>` QA vocabulary and the
    /// inverse of `Self::from_slug`. NOT the display [`Self::label`] (`Data` ⇒ `"data"`, never
    /// `"data manager"`). Exhaustive match: a new variant fails to compile until it names a slug.
    pub fn slug(self) -> &'static str {
        match self {
            WindowKind::Chart => "chart",
            WindowKind::Trade => "trade",
            WindowKind::Account => "account",
            WindowKind::Options => "options",
            WindowKind::News => "news",
            WindowKind::Calendar => "calendar",
            WindowKind::Data => "data",
            WindowKind::Studio => "studio",
            WindowKind::Polymarket => "polymarket",
            WindowKind::Settings => "settings",
        }
    }
    /// Whether this kind's body is TABBED and seats its segmented control inside the title bar
    /// rather than in a row under it (`super::title_bar::WindowTitleBarTabSlot`).
    ///
    /// ⚠ Two consequences, and the second is the one that surprises: the bar RESERVES a rect
    /// between the title and the window controls for a kind that answers `true`, and below
    /// `title_bar::TITLE_DROP_WIDTH` that kind's TITLE DROPS so two segments still fit at
    /// ~400pt — unless the bar can measure its segments and both fit (the Trade window; see
    /// `super::title_bar`'s module doc). A kind that answers `false` keeps its title at every
    /// width, because nothing is competing with it for the row.
    ///
    /// Exhaustive match rather than `matches!`, so a new variant has to decide.
    pub fn carries_title_tabs(self) -> bool {
        match self {
            WindowKind::Trade => true,
            WindowKind::Chart
            | WindowKind::Account
            | WindowKind::Options
            | WindowKind::News
            | WindowKind::Calendar
            | WindowKind::Data
            | WindowKind::Studio
            | WindowKind::Polymarket
            | WindowKind::Settings => false,
        }
    }
    /// Parse a [`Self::slug`] back to its kind — the explicit inverse that replaced the silent
    /// `_ => Calendar` fallback match at `vike-app`'s `VIKE_TOOL` read (the CALLER decides what a
    /// `None` does: `initial_arrange::plan_initial_arrange` refuses it). `None` for anything that is
    /// not exactly a known slug (case-sensitive, like the match it replaced). Resolved over [`Self::ALL`] against
    /// [`Self::slug`], so the two directions can never disagree.
    pub(crate) fn from_slug(s: &str) -> Option<WindowKind> {
        Self::ALL.into_iter().find(|k| k.slug() == s)
    }
    /// The window's icon in its title bar — one registry icon per kind: its row's `icon`.
    pub fn icon(self) -> ThemeIcon {
        self.row().icon().expect("a window_kind row names an icon of the registry")
    }
    /// Every kind, in declaration order — the roster `Self::from_slug` resolves over and the
    /// exhaustiveness test iterates. (The launcher row is not this list: the desktop's own
    /// `LAUNCHERS` table in `crates/vike-desktop/src/main.rs` is the row it draws.) Grow it with
    /// the enum: the exhaustive [`Self::slug`] match already fails to compile on a new variant,
    /// which puts you in this impl block next to this list.
    pub const ALL: [WindowKind; 10] = [
        WindowKind::Chart,
        WindowKind::Trade,
        WindowKind::Account,
        WindowKind::Options,
        WindowKind::News,
        WindowKind::Calendar,
        WindowKind::Data,
        WindowKind::Studio,
        WindowKind::Polymarket,
        WindowKind::Settings,
    ];
}
