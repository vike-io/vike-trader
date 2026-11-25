//! The Help menu's three windows: About, Keyboard shortcuts and Console.
//!
//! The menu only asks (`MenuBarActions`'s `open_about`, `open_shortcuts` and `open_console`);
//! [`HelpWindows::open`] takes the ask and [`HelpWindows::show`] draws whichever window is open,
//! every frame. The shell keeps one `HelpWindows` and does the one thing a click here must not do
//! inside a test: it opens the log folder the Console asks for, through [`open_folder`]. Until 2026-11-16
//! the About and Keyboard shortcuts rows were `let _ = ui.button(..)`: the click was drawn and
//! dropped, and no window existed to open.
//!
//! - **About** reads the build's own facts from `vike_buildinfo`, the ones every binary's
//!   `--version` and first log line print.
//! - **Keyboard shortcuts** lists the bindings the handlers read, never a copy: the caption's
//!   `FOCUS_PALETTE` (`/`), [`TOGGLE_LOG_SCALE`] (the desktop's frame consumes it) and the Studio's
//!   `vike_studio::SHORTCUTS`, which the shell hands in because this crate does not link the Studio.
//! - **Console** shows this process's own log, newest line last: the `vike_log::recent::RecentLines`
//!   tail the binary handed `vike_log::init`, so it holds what the console prints, under the
//!   console's filter (INFO and up unless `RUST_LOG` or `preferences.log_level` say otherwise).
//!   Its source buttons are the sources the tail has seen (`RecentLine::source`, named as each line
//!   arrived), never a list kept here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use egui::{Key, KeyboardShortcut, Modifiers, RichText};
use vike_log::recent::{RecentLine, RecentLines};
use vike_ui_theme::brand::APP_NAME;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::input::{self, ThemeInputField};
use vike_ui_theme::components::segmented::{ThemeSegment, segmented};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::workspace;

use super::menu_bar::MenuBarActions;

/// Alt+L: switch the price scale of the chart under the pointer, else the front chart, between log
/// and linear. The desktop's frame consumes it outside text input.
pub const TOGGLE_LOG_SCALE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::ALT, Key::L);

/// This crate's shortcuts and what each does; the Studio's arrive through [`HelpWindows::new`].
const SHORTCUTS: [(KeyboardShortcut, &str); 2] = [
    (super::caption::FOCUS_PALETTE, "Go to the symbol and command box in the top bar"),
    (
        TOGGLE_LOG_SCALE,
        "Chart: switch the price scale between log and linear (the chart under the pointer, else \
         the front one)",
    ),
];

/// The About window's links: the project's source and its releases.
const SOURCE_URL: &str = "https://github.com/vike-io/vike-trader";
const RELEASES_URL: &str = "https://github.com/vike-io/vike-trader/releases";

/// Why Open log folder is disabled.
const NO_LOG_FILE: &str =
    "This run writes no log file: file logging is off, or its folder could not be opened";

/// The `VIKE_SHOT_WIN` values that pose a Help window over the default chart for a capture, in the
/// order About, Keyboard shortcuts, Console.
pub(crate) const SHOT_WINS: [&str; 3] = ["about", "shortcuts", "console"];

/// Which lines the Console shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsoleLevel {
    All,
    WarningsAndErrors,
    Errors,
}

impl ConsoleLevel {
    fn shows(self, level: tracing::Level) -> bool {
        // `tracing` orders the more verbose levels as greater: ERROR < WARN < INFO.
        match self {
            ConsoleLevel::All => true,
            ConsoleLevel::WarningsAndErrors => level <= tracing::Level::WARN,
            ConsoleLevel::Errors => level == tracing::Level::ERROR,
        }
    }
}

/// The three windows and the Console's filters. One per process, kept by the shell.
pub struct HelpWindows {
    about: bool,
    shortcuts: bool,
    console: bool,
    recent: RecentLines,
    more_shortcuts: &'static [(KeyboardShortcut, &'static str)],
    level: ConsoleLevel,
    /// The sources the Console shows; empty is every source.
    sources: BTreeSet<String>,
    filter: String,
}

impl HelpWindows {
    /// `recent` is the tail the binary handed `vike_log::init`; `more_shortcuts` are the bindings of
    /// crates this one does not link (the Studio's); `shot_win` is `VIKE_SHOT_WIN`, and one of
    /// `SHOT_WINS` opens its window.
    pub fn new(
        recent: RecentLines,
        more_shortcuts: &'static [(KeyboardShortcut, &'static str)],
        shot_win: Option<&str>,
    ) -> Self {
        HelpWindows {
            about: shot_win == Some(SHOT_WINS[0]),
            shortcuts: shot_win == Some(SHOT_WINS[1]),
            console: shot_win == Some(SHOT_WINS[2]),
            recent,
            more_shortcuts,
            level: ConsoleLevel::All,
            sources: BTreeSet::new(),
            filter: String::new(),
        }
    }

    /// Open the windows the menu asked for this frame.
    pub fn open(&mut self, menu: &MenuBarActions) {
        self.about |= menu.open_about;
        self.shortcuts |= menu.open_shortcuts;
        self.console |= menu.open_console;
    }

    /// Draw the open windows, each closable from its title bar. `Some(dir)` when the Console's Open
    /// log folder was clicked: the shell opens it.
    pub fn show(&mut self, context: &egui::Context) -> Option<PathBuf> {
        window(context, format!("About {APP_NAME}"), &mut self.about)
            .resizable(false)
            .show(context, about_body);
        let more = self.more_shortcuts;
        window(context, "Keyboard shortcuts", &mut self.shortcuts)
            .resizable(false)
            .show(context, |ui| shortcuts_body(ui, more));
        let (recent, level, sources, filter) =
            (&self.recent, &mut self.level, &mut self.sources, &mut self.filter);
        window(context, "Console", &mut self.console)
            .default_size(workspace::TOOL_WINDOW_SIZE)
            .show(context, |ui| console_body(ui, recent, level, sources, filter))
            .and_then(|r| r.inner.flatten())
    }
}

/// A Help window: opens centred on the app, movable, closed by its title bar's ✕ (`open`).
///
/// ⚠ `Order::Foreground`, as the chart's own dialogs: the arena's windows are `Middle` and the
/// startup re-tile moves the chart to the top, so a `Middle` window opened before it is drawn UNDER
/// the chart (the first capture showed the chart's empty-state words across About's rows). And its
/// own inner margin, the density's `padding`: the theme zeroes `window_margin` for the flush arena.
fn window<'open>(
    context: &egui::Context,
    title: impl Into<egui::WidgetText>,
    open: &'open mut bool,
) -> egui::Window<'open> {
    let pad = ThemeTokens::of(context).metrics.padding;
    egui::Window::new(title)
        .open(open)
        .order(egui::Order::Foreground)
        .frame(egui::Frame::window(&context.global_style()).inner_margin(pad))
        .collapsible(false)
        .default_pos(context.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
}

fn about_body(ui: &mut egui::Ui) {
    let t = ThemeTokens::of(ui.ctx());
    ui.label(RichText::new(APP_NAME).font(t.font(ThemeTextRole::Heading)));
    egui::Grid::new("help-about").num_columns(2).show(ui, |ui| {
        let commit = format!("{} ({})", vike_buildinfo::GIT_SHA, vike_buildinfo::git_state());
        let rows = [
            ("Version", env!("CARGO_PKG_VERSION").to_string()),
            ("Commit", commit),
            ("Built", vike_buildinfo::BUILD_TIMESTAMP.to_string()),
            ("Target", vike_buildinfo::TARGET.to_string()),
        ];
        for (name, value) in rows {
            ui.label(RichText::new(name).color(t.theme.text_secondary));
            ui.label(RichText::new(value).font(t.mono(ThemeTextRole::Body)));
            ui.end_row();
        }
    });
    ui.horizontal(|ui| {
        ui.hyperlink_to("Source code", SOURCE_URL);
        ui.hyperlink_to("Releases", RELEASES_URL);
    });
}

fn shortcuts_body(ui: &mut egui::Ui, more: &[(KeyboardShortcut, &str)]) {
    let t = ThemeTokens::of(ui.ctx());
    egui::Grid::new("help-shortcuts").num_columns(2).striped(true).show(ui, |ui| {
        for (keys, does) in SHORTCUTS.iter().chain(more) {
            let keys = ui.ctx().format_shortcut(keys);
            ui.label(RichText::new(keys).font(t.mono(ThemeTextRole::Body)));
            ui.label(*does);
            ui.end_row();
        }
    });
}

/// The Console: the level, source and text filters, Copy all and Open log folder, then the kept
/// lines, newest at the bottom. The scroll follows new lines while it sits at the bottom and stays
/// where the reader scrolled up to otherwise (`stick_to_bottom`).
fn console_body(
    ui: &mut egui::Ui,
    recent: &RecentLines,
    level: &mut ConsoleLevel,
    sources: &mut BTreeSet<String>,
    filter: &mut String,
) -> Option<PathBuf> {
    let t = ThemeTokens::of(ui.ctx());
    let segments = [
        ThemeSegment {
            value: ConsoleLevel::All,
            label: "All",
            why: "Every kept line: INFO and up",
        },
        ThemeSegment {
            value: ConsoleLevel::WarningsAndErrors,
            label: "Warnings and errors",
            why: "WARN and ERROR lines only",
        },
        ThemeSegment { value: ConsoleLevel::Errors, label: "Errors", why: "ERROR lines only" },
    ];
    segmented(ui, level, &segments);
    let lines = recent.lines();
    source_buttons(ui, &lines, sources);
    let shown = shown_lines(lines, *level, sources, filter);
    let log_dir = recent.log_dir();
    let mut open_dir = None;
    ui.horizontal(|ui| {
        input::text(ui, filter, ThemeInputField { hint: "Filter text", ..Default::default() });
        if ui.add(ThemeButtonAction::secondary("Copy all")).clicked() {
            let all: Vec<&str> = shown.iter().map(|l| &*l.text).collect();
            ui.ctx().copy_text(all.join("\n"));
        }
        let open = ThemeButtonAction::secondary("Open log folder");
        let open = if log_dir.is_some() { open } else { open.disabled_because(NO_LOG_FILE) };
        if ui.add(open).clicked() {
            open_dir = log_dir.clone();
        }
    });
    ui.separator();
    // One display row per text line, so the rows are uniform and only the visible ones are laid out.
    let rows: Vec<(tracing::Level, &str)> =
        shown.iter().flat_map(|l| l.text.lines().map(move |s| (l.level, s))).collect();
    let font = t.mono(ThemeTextRole::Body);
    let row_height = ui.ctx().fonts_mut(|f| f.row_height(&font));
    egui::ScrollArea::both().auto_shrink(false).stick_to_bottom(true).show_rows(
        ui,
        row_height,
        rows.len(),
        |ui, range| {
            for (level, text) in &rows[range] {
                let ink = match *level {
                    tracing::Level::ERROR => ThemeStatus::Error.color(),
                    tracing::Level::WARN => ThemeStatus::Warning.color(),
                    _ => t.theme.text,
                };
                let words = RichText::new(*text).font(font.clone()).color(ink);
                ui.add(egui::Label::new(words).extend());
            }
        },
    );
    open_dir
}

/// Open `dir` in the OS file manager: Explorer on Windows, Finder (`open`) on macOS, `xdg-open`
/// elsewhere. The shell calls it with what [`HelpWindows::show`] returned. Not `egui::OpenUrl`: that
/// goes through the `webbrowser` crate, which opens a `file://` folder in a BROWSER.
///
/// The child is reaped on its own thread, so a quick `open`/`xdg-open` leaves no zombie behind. Its
/// exit status is not read: Explorer exits 1 after opening a folder.
pub fn open_folder(dir: &Path) -> std::io::Result<()> {
    let (program, dir) = if cfg!(windows) {
        // Explorer reads `/` as a switch and opens Documents instead of a forward-slash path.
        ("explorer", PathBuf::from(dir.to_string_lossy().replace('/', "\\")))
    } else if cfg!(target_os = "macos") {
        ("open", dir.to_path_buf())
    } else {
        ("xdg-open", dir.to_path_buf())
    };
    let mut child = std::process::Command::new(program).arg(dir).spawn()?;
    std::thread::Builder::new().name("ui-open-folder".into()).spawn(move || child.wait())?;
    Ok(())
}

/// "All sources", then one toggle per source in `lines` with its line count; a chosen source whose
/// lines all left the tail keeps its toggle, so it can still be turned off.
fn source_buttons(ui: &mut egui::Ui, lines: &[RecentLine], sources: &mut BTreeSet<String>) {
    let mut counts: BTreeMap<&str, usize> = sources.iter().map(|s| (s.as_str(), 0)).collect();
    for l in lines {
        *counts.entry(&*l.source).or_default() += 1;
    }
    // Applied after the row: `counts` borrows the names in `sources`.
    let (mut all, mut flip) = (false, None);
    ui.horizontal_wrapped(|ui| {
        let every = ThemeButtonAction::secondary("All sources").selected(sources.is_empty());
        all = ui.add(every).on_hover_text("Lines from every part of the program").clicked();
        for (source, n) in &counts {
            let on = sources.contains(*source);
            if ui.add(ThemeButtonAction::secondary(format!("{source} {n}")).selected(on)).clicked()
            {
                flip = Some(source.to_string());
            }
        }
    });
    if all {
        sources.clear();
    } else if let Some(source) = flip
        && !sources.remove(&source)
    {
        sources.insert(source);
    }
}

/// The lines `level` keeps, from `sources` (empty is every source), that contain `filter`, ignoring
/// case, oldest first.
fn shown_lines(
    lines: Vec<RecentLine>,
    level: ConsoleLevel,
    sources: &BTreeSet<String>,
    filter: &str,
) -> Vec<RecentLine> {
    let needle = filter.trim().to_lowercase();
    lines
        .into_iter()
        .filter(|l| level.shows(l.level))
        .filter(|l| sources.is_empty() || sources.contains(&*l.source))
        .filter(|l| needle.is_empty() || l.text.to_lowercase().contains(&needle))
        .collect()
}

#[cfg(test)]
#[path = "help_tests.rs"]
mod help_tests;
