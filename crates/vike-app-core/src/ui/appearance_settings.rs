//! The desktop's five appearance preferences (design system spec §5): read at start, changed on
//! the Settings window's Appearance section, re-applied live and saved as `preferences.*` rows of
//! THIS computer's settings database.
//!
//! The five READS stay in `crates/vike-desktop/src/main.rs`, so `vike-cli config show` names
//! `desktop` as their reader (`vike_config::Consumer::binary`). Everything that decides is here,
//! where CI runs it.
//!
//! The same session also carries `preferences.vsync`, which is not an appearance setting but rides
//! the same machinery (one save path, one "Save again", one change-journal record per row) on the
//! Settings window's Display section. It differs in ONE way: it is STARTUP-ONLY. The present mode
//! is fixed when the window opens, so a change is saved and NOT applied; the session keeps what the
//! running window was opened with (`ThemeAppearanceSession::vsync_in_force`) apart from what was chosen
//! ([`ThemeAppearanceSession::vsync`]), and the screen says "requires restart" until the two agree.
//!
//! Two more rows ride it on the same section, and they are LIVE: `preferences.gpu_candles` (the
//! charts' GPU candle layer) and `preferences.display_timezone` (the zone of every time the app
//! shows). They used to be the workspace file's `gpu_render` and `display_time_zone`, which are no longer
//! read. The shell reads [`ThemeAppearanceSession::draws_gpu_candles`] and
//! [`ThemeAppearanceSession::display_time_zone`] every frame, and calls [`ThemeAppearanceSession::retime`] on the
//! frame a change lands so an open chart's axis moves at once.

use std::path::{Path, PathBuf};
use std::time::Duration;

use vike_change_journal::{Change, ChangeActor, ChangeJournal, ChangeOutcome};
use vike_chart::chart::state::ChartState;
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::market::ThemeMarketColorsId;
use vike_ui_theme::metrics::ThemeDensity;
use vike_ui_theme::theme::ThemePaletteId;
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;
use vike_ui_theme::type_scale::ThemeTextSize;

use crate::ui::gpu_startup::LIBRARY_DEFAULT_VSYNC;

/// The appearance the five resolved preference values name. A word no option has — unreachable
/// through the loader, which refuses it, but this function does not assume its caller — answers
/// that setting's default: a look is never a reason not to open a window.
pub fn appearance_from(
    theme: &str,
    market_colors: &str,
    header_gradient: bool,
    density: &str,
    text_size: &str,
) -> ThemeAppearance {
    ThemeAppearance {
        theme: ThemePaletteId::from_key(theme).unwrap_or_default(),
        market: ThemeMarketColorsId::from_key(market_colors).unwrap_or_default(),
        header_gradient,
        density: ThemeDensity::from_key(density).unwrap_or_default(),
        text_size: ThemeTextSize::from_key(text_size).unwrap_or_default(),
    }
}

/// After a change was saved.
pub const SAVED_NOTE: &str = "Saved. It applies now, and every time the app starts.";
/// The boot found no project, so there is nowhere to save.
pub(crate) const NO_SETTINGS_DIR_NOTE: &str = "Not saved: the app found no project settings directory, \
     so this look lasts until the app closes. Start it inside the project, or set VIKE_SETTINGS_DIR.";
/// The project has no settings database, and the app does not create one.
pub const NO_DATABASE_NOTE: &str = "Not saved: this computer has no settings database, and the \
     app never creates one (a new database would hide the credential files beside it). This look \
     lasts until the app closes.";
/// Another writer held the store; nothing was written. The screen offers "Save again"
/// (`ThemeAppearanceSession::can_save_again`), and the next change saves the refused row too.
pub const BUSY_NOTE: &str = "Not saved: another program was writing the settings database. This \
     look is applied for now; press Save again to save it, or it is saved with your next change \
     here.";
/// The one extra line spec §5 asks for: Carbon's amber accent next to the colour-blind set's
/// amber-orange "down" (§3.1). A note, never a refusal: the pair stays selectable.
pub const CARBON_COLOURBLIND_NOTE: &str = "With Carbon and Colour-blind, the accent and \"down\" \
     are both amber-orange. The accent only marks shapes (an underline, a ring, a fill), never a \
     number, so a falling price still reads as down.";
/// Owner decision 3 of this PR's plan: until step 7 of the design system ends, screens that paint
/// their own colours and sizes do not follow these settings yet. Delete this line in step 8
/// ("Gates to zero"), when the last of them has moved.
pub const MIGRATION_NOTE: &str = "Some screens still paint their own colours and sizes. Each \
     follows these settings as the design system reaches it.";

/// The vertical-sync checkbox's label. "requires restart" is part of it, so the control says so
/// before it is touched, not only after.
pub const VSYNC_LABEL: &str = "Vertical sync (requires restart)";

/// The line under the vertical-sync checkbox: what turning it off does, and the cap that goes with it.
pub(crate) fn vsync_hint() -> String {
    format!(
        "Off shows frames the moment they are drawn, which can tear, and the app then limits \
         itself to {} frames a second.",
        crate::ui::frame_cap::max_frames_per_second()
    )
}

/// The GPU candle checkbox's label, and the line under it.
pub const GPU_CANDLES_LABEL: &str = "Draw chart candles on the GPU";
pub(crate) const GPU_CANDLES_HINT: &str = "On: the graphics card draws the chart candles as their own \
     fast layer, which helps with tens of thousands of candles on screen. Off: the candles are \
     drawn the ordinary way, with the rest of the window.";
/// Why the GPU candle checkbox is disabled (spec §4.2: a disabled control says why).
pub const GPU_UNAVAILABLE: &str =
    "GPU unavailable: no wgpu backend, or the candle pipeline failed to build at startup";
/// The time-zone row's label.
pub(crate) const TIME_ZONE_LABEL: &str =
    "Time zone of the chart axes, the Calendar, the Console and the status bar clock";

/// The wait a save gives the settings database's write lock: none. The save runs on the render
/// thread, and a frame cannot lend another process any of its 16 ms: the shared writer's default
/// (~3 s) spent HERE is a frozen window with no repaint and no cancel, which an operator reads as
/// "the app hung" rather than "another writer holds the database". So ONE attempt and no wait; a
/// refusal costs the operator one click, and lands in the [`ThemeAppearanceSessionSaveLine`] they already read. Retrying
/// on the next frame was refused: it would spin a render loop against a lock a daemon may hold for
/// as long as it likes, with nothing on screen saying so.
pub(crate) const APPEARANCE_LOCK_BUDGET: Duration = Duration::ZERO;

/// What the last change did about SAVING it — the line under the Appearance section's controls.
/// Applying never fails; saving can, and says why.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum ThemeAppearanceSessionSaveLine {
    /// No change yet this session.
    #[default]
    Idle,
    /// The change is applied and saved.
    Saved,
    /// The change is applied for this session and NOT saved; the reason, as the screen shows it.
    NotSaved(String),
}

/// `preferences.vsync` as the Settings window holds it. See the module doc for why it is apart from
/// [`ThemeAppearance`]: it is startup-only.
#[derive(Debug, Clone, Copy)]
struct ThemeAppearanceSessionVsyncRow {
    /// What the running window's present mode was chosen with. Fixed for the life of the process.
    in_force: bool,
    /// What the operator has chosen: `in_force` until a click.
    chosen: bool,
    /// What the settings database holds, as far as this process knows (advanced by every row that
    /// LANDS, exactly as [`ThemeAppearanceSession`]'s `saved` is).
    saved: bool,
    pending: Option<bool>,
}

impl ThemeAppearanceSessionVsyncRow {
    /// A row whose running window, choice and database all say `on`.
    const fn settled(on: bool) -> Self {
        Self { in_force: on, chosen: on, saved: on, pending: None }
    }
}

/// A row applied the moment it is chosen (`preferences.gpu_candles`, `preferences.display_timezone`):
/// what is in force, what the database holds (advanced as [`ThemeAppearanceSessionVsyncRow`]'s `saved` is), and the
/// screen's pending choice.
#[derive(Debug, Clone, Copy)]
struct ThemeAppearanceSessionLiveRow<T> {
    chosen: T,
    saved: T,
    pending: Option<T>,
}

impl<T: Copy> ThemeAppearanceSessionLiveRow<T> {
    const fn settled(v: T) -> Self {
        Self { chosen: v, saved: v, pending: None }
    }
}

/// The appearance this process runs, the look the settings database holds, and the Settings
/// window's pending change.
#[derive(Debug)]
pub struct ThemeAppearanceSession {
    appearance: ThemeAppearance,
    /// The look the settings database holds, as far as this process knows: the installed look at
    /// start, then advanced by every row that LANDS. A change writes the rows that differ from
    /// THIS, not from the look on screen, so a row an earlier save could not write is written
    /// with the next one and a "Saved" line is true of every setting.
    saved: ThemeAppearance,
    pending: Option<ThemeAppearance>,
    settings_dir: Option<PathBuf>,
    /// Whether `settings_dir` holds a settings database — at start, and again after every save.
    database_present: bool,
    last: ThemeAppearanceSessionSaveLine,
    vsync: ThemeAppearanceSessionVsyncRow,
    gpu_candles: ThemeAppearanceSessionLiveRow<bool>,
    /// Whether a GPU candle path exists at all (the shell's pipeline build at startup).
    gpu_available: bool,
    display_time_zone: ThemeAppearanceSessionLiveRow<ThemeDisplayTimeZone>,
}

impl ThemeAppearanceSession {
    /// The session of an app that started with `appearance` installed. `settings_dir` is the
    /// binary's ONE boot walk, never a resolver call from here. The vsync row starts at the
    /// library's own default; [`Self::with_vsync`] says what the window was really opened with.
    /// The two display rows start at their defaults with no GPU path; [`Self::with_display`] says
    /// what the binary resolved.
    pub fn new(appearance: ThemeAppearance, settings_dir: Option<PathBuf>) -> Self {
        let database_present = database_in(settings_dir.as_deref());
        Self {
            appearance,
            saved: appearance,
            pending: None,
            settings_dir,
            database_present,
            last: ThemeAppearanceSessionSaveLine::Idle,
            vsync: ThemeAppearanceSessionVsyncRow::settled(LIBRARY_DEFAULT_VSYNC),
            gpu_candles: ThemeAppearanceSessionLiveRow::settled(
                vike_config::preferences::DEFAULT_GPU_CANDLES,
            ),
            gpu_available: false,
            display_time_zone: ThemeAppearanceSessionLiveRow::settled(ThemeDisplayTimeZone::parse(
                vike_config::preferences::DEFAULT_DISPLAY_TIMEZONE,
            )),
        }
    }

    /// The session of an app started with the two display rows the binary resolved,
    /// `preferences.gpu_candles` and `preferences.display_timezone`, and with `gpu_available`:
    /// whether a GPU candle path exists at all. The database is taken to hold the same.
    #[must_use]
    pub fn with_display(
        mut self,
        (gpu_candles, display_timezone): (bool, &str),
        gpu_available: bool,
    ) -> Self {
        self.gpu_candles = ThemeAppearanceSessionLiveRow::settled(gpu_candles);
        self.display_time_zone =
            ThemeAppearanceSessionLiveRow::settled(ThemeDisplayTimeZone::parse(display_timezone));
        self.gpu_available = gpu_available;
        self
    }

    /// The GPU candle choice on screen (and in force where a GPU path exists).
    pub fn gpu_candles(&self) -> bool {
        self.gpu_candles.chosen
    }

    /// Whether a GPU candle path exists at all; without one the Settings window disables the box.
    pub(crate) fn gpu_available(&self) -> bool {
        self.gpu_available
    }

    /// Whether the charts draw their candles on the GPU this frame: the choice, where a GPU path
    /// exists. What the shell wires into each chart's GPU candle seam.
    pub fn draws_gpu_candles(&self) -> bool {
        self.gpu_candles.chosen && self.gpu_available
    }

    /// A GPU candle choice from the screen; [`Self::apply_pending`] applies and saves it.
    pub(crate) fn request_gpu_candles(&mut self, on: bool) {
        self.gpu_candles.pending = Some(on);
    }

    /// The time zone every time the app shows is in.
    pub fn display_time_zone(&self) -> ThemeDisplayTimeZone {
        self.display_time_zone.chosen
    }

    /// A time-zone choice from the screen; [`Self::apply_pending`] applies and saves it.
    pub(crate) fn request_display_time_zone(&mut self, time_zone: ThemeDisplayTimeZone) {
        self.display_time_zone.pending = Some(time_zone);
    }

    /// Put every chart in [`Self::display_time_zone`] now, marks included. The shell calls it on the frame
    /// [`Self::apply_pending`] applied a change: `core_sync`'s own re-time runs only when the core's
    /// frame changes, which can be a while for a quiet symbol.
    pub fn retime<'a>(&self, charts: impl IntoIterator<Item = &'a mut ChartState>) {
        for cs in charts {
            cs.set_time_zone(self.display_time_zone.chosen);
            cs.refresh_caches();
        }
    }

    /// The session of an app whose window was opened with `vsync` (the resolved
    /// `preferences.vsync`, read by the binary). The database is taken to hold the same.
    #[must_use]
    pub fn with_vsync(mut self, vsync: bool) -> Self {
        self.vsync = ThemeAppearanceSessionVsyncRow::settled(vsync);
        self
    }

    /// The vertical-sync choice on screen: the running window's until the operator clicks.
    pub fn vsync(&self) -> bool {
        self.vsync.chosen
    }

    /// What the running window's present mode was opened with.
    #[cfg(any(test, feature = "test-support"))]
    pub fn vsync_in_force(&self) -> bool {
        self.vsync.in_force
    }

    /// Whether the saved choice differs from the running window's, i.e. a restart is owed.
    pub fn vsync_needs_restart(&self) -> bool {
        self.vsync.chosen != self.vsync.in_force
    }

    /// The line that says a vertical-sync choice is waiting for a restart, or `None` when the
    /// choice is what the window is running.
    pub(crate) fn vsync_note(&self) -> Option<String> {
        self.vsync_needs_restart().then(|| {
            format!(
                "Applies the next time the app starts; this run keeps vertical sync {}.",
                if self.vsync.in_force { "on" } else { "off" }
            )
        })
    }

    /// A vertical-sync choice from the screen; [`Self::apply_pending`] saves it. It is never
    /// applied to the running window.
    pub(crate) fn request_vsync(&mut self, on: bool) {
        self.vsync.pending = Some(on);
    }

    /// The appearance in force.
    pub fn appearance(&self) -> ThemeAppearance {
        self.appearance
    }

    /// What the last change did about saving it.
    pub(crate) fn last(&self) -> &ThemeAppearanceSessionSaveLine {
        &self.last
    }

    /// Where a change is saved; `None` when the boot found no project.
    pub fn settings_dir(&self) -> Option<&Path> {
        self.settings_dir.as_deref()
    }

    /// Whether this computer has a settings database to save into (never created by the app).
    pub(crate) fn database_present(&self) -> bool {
        self.database_present
    }

    /// `true` when the look in force is not what the database holds and there is a database to
    /// write to — a save was refused, most often because another writer held the store. The
    /// screen then offers "Save again", which requests the look already in force.
    pub(crate) fn can_save_again(&self) -> bool {
        (self.appearance != self.saved
            || self.vsync.chosen != self.vsync.saved
            || self.gpu_candles.chosen != self.gpu_candles.saved
            || self.display_time_zone.chosen != self.display_time_zone.saved)
            && self.database_present
    }

    /// A choice from the screen. [`Self::apply_pending`] applies it once the frame's windows
    /// have drawn, so no frame mixes two looks. Requesting the look already in force saves
    /// whatever of it is not yet saved.
    pub fn request(&mut self, next: ThemeAppearance) {
        self.pending = Some(next);
    }

    /// Apply the pending choice and save it, if it changes anything; `true` when it did. The
    /// shell calls this once per frame, after its window loop — never
    /// `vike_ui_theme::appearance::install`, which would compare the whole font set again.
    pub fn apply_pending(
        &mut self,
        context: &egui::Context,
        journal: Option<&ChangeJournal>,
        now_ms: i64,
    ) -> bool {
        let (look, vsync) = (self.pending.take(), self.vsync.pending.take());
        let (gpu, time_zone) =
            (self.gpu_candles.pending.take(), self.display_time_zone.pending.take());
        if look.is_none() && vsync.is_none() && gpu.is_none() && time_zone.is_none() {
            return false;
        }
        let next = look.unwrap_or(self.appearance);
        let next_vsync = vsync.unwrap_or(self.vsync.chosen);
        let next_gpu = gpu.unwrap_or(self.gpu_candles.chosen);
        let next_time_zone = time_zone.unwrap_or(self.display_time_zone.chosen);
        let mut rows = changed_rows(&self.saved, &next);
        if next_vsync != self.vsync.saved {
            rows.push((VSYNC_KEY, if next_vsync { "true" } else { "false" }));
        }
        if next_gpu != self.gpu_candles.saved {
            rows.push((GPU_CANDLES_KEY, if next_gpu { "true" } else { "false" }));
        }
        if next_time_zone != self.display_time_zone.saved {
            rows.push((DISPLAY_TIMEZONE_KEY, next_time_zone.name()));
        }
        let in_force = next == self.appearance
            && next_vsync == self.vsync.chosen
            && next_gpu == self.gpu_candles.chosen
            && next_time_zone == self.display_time_zone.chosen;
        if in_force && rows.is_empty() {
            return false;
        }
        if next != self.appearance {
            vike_ui_theme::appearance::apply(context, &next);
            self.appearance = next;
        }
        // The vsync choice is recorded, never applied: the running window keeps its present mode.
        // The two display rows are in force from here; the shell reads them every frame.
        self.vsync.chosen = next_vsync;
        self.gpu_candles.chosen = next_gpu;
        self.display_time_zone.chosen = next_time_zone;
        let (landed, line) = save_rows(self.settings_dir.as_deref(), &rows, journal, now_ms);
        for (key, _) in &rows[..landed] {
            match *key {
                VSYNC_KEY => self.vsync.saved = next_vsync,
                GPU_CANDLES_KEY => self.gpu_candles.saved = next_gpu,
                DISPLAY_TIMEZONE_KEY => self.display_time_zone.saved = next_time_zone,
                _ => adopt(&mut self.saved, &next, key),
            }
        }
        self.last = line;
        self.database_present = database_in(self.settings_dir.as_deref());
        true
    }
}

/// Whether `settings_dir` holds a settings database. `None` holds none; one the filesystem will not
/// stat counts as there, so a save tries it and shows the store's own error rather than "no store".
fn database_in(settings_dir: Option<&Path>) -> bool {
    settings_dir.is_some_and(|d| {
        vike_secrets::database_present(&vike_secrets::db_path_in(d)).unwrap_or(true)
    })
}

// The five dotted keys: `changed_rows` writes them, `adopt` reads a landed one back.
const THEME_KEY: &str = "preferences.theme";
const MARKET_KEY: &str = "preferences.market_colors";
const GRADIENT_KEY: &str = "preferences.header_gradient";
const DENSITY_KEY: &str = "preferences.density";
const TEXT_SIZE_KEY: &str = "preferences.text_size";
/// …and the three rows this session writes that are not appearance settings (module doc).
const VSYNC_KEY: &str = "preferences.vsync";
const GPU_CANDLES_KEY: &str = "preferences.gpu_candles";
const DISPLAY_TIMEZONE_KEY: &str = "preferences.display_timezone";

/// The rows a change writes: one `(dotted key, value)` per setting that differs, in the order the
/// spec's §5 table lists them.
pub(crate) fn changed_rows(
    before: &ThemeAppearance,
    after: &ThemeAppearance,
) -> Vec<(&'static str, &'static str)> {
    let mut rows = Vec::new();
    if before.theme != after.theme {
        rows.push((THEME_KEY, after.theme.key()));
    }
    if before.market != after.market {
        rows.push((MARKET_KEY, after.market.key()));
    }
    if before.header_gradient != after.header_gradient {
        rows.push((GRADIENT_KEY, if after.header_gradient { "true" } else { "false" }));
    }
    if before.density != after.density {
        rows.push((DENSITY_KEY, after.density.key()));
    }
    if before.text_size != after.text_size {
        rows.push((TEXT_SIZE_KEY, after.text_size.key()));
    }
    rows
}

/// Take the setting `key` names from `next` into `saved` — a row that landed.
fn adopt(saved: &mut ThemeAppearance, next: &ThemeAppearance, key: &str) {
    match key {
        THEME_KEY => saved.theme = next.theme,
        MARKET_KEY => saved.market = next.market,
        GRADIENT_KEY => saved.header_gradient = next.header_gradient,
        DENSITY_KEY => saved.density = next.density,
        TEXT_SIZE_KEY => saved.text_size = next.text_size,
        _ => {}
    }
}

/// Write `rows` through the one settings writer (`vike_config::write_setting_row`, decision 0086)
/// and record each in the change journal under the GUI actor. It stops at the first refusal:
/// every row before it is saved and recorded, and the line says why the rest are not. Returns how
/// many of `rows`, in order, landed, and the line.
///
/// ⚠ It NEVER creates the database. The writer refuses a project without one
/// ([`vike_secrets::RowWriteError::NoDatabase`]), and that is the right answer here: a new
/// `db/vike.db` switches the credential store from the files beside it to an empty database.
/// `crates/vike-secrets/src/db/create.rs`'s `create_store` says why creating one is the
/// operator's act.
pub(crate) fn save_rows(
    settings_dir: Option<&Path>,
    rows: &[(&str, &str)],
    journal: Option<&ChangeJournal>,
    now_ms: i64,
) -> (usize, ThemeAppearanceSessionSaveLine) {
    let Some(dir) = settings_dir else {
        return (0, ThemeAppearanceSessionSaveLine::NotSaved(NO_SETTINGS_DIR_NOTE.to_string()));
    };
    for (landed, (key, value)) in rows.iter().enumerate() {
        match vike_config::write_setting_row(dir, key, value, APPEARANCE_LOCK_BUDGET) {
            Ok(report) => record(
                journal,
                now_ms,
                Change::set_setting(
                    ChangeOutcome::Applied,
                    ChangeActor::Gui,
                    "preferences",
                    &report.key,
                    report.old_value.as_deref(),
                    &report.new_value,
                ),
            ),
            Err(e) => {
                let error = e.to_string();
                record(
                    journal,
                    now_ms,
                    Change::set_setting(
                        ChangeOutcome::Refused,
                        ChangeActor::Gui,
                        "preferences",
                        key,
                        None,
                        value,
                    )
                    .with_reason(Some(&error)),
                );
                tracing::error!(key = %key, error = %error, "appearance setting not saved");
                let line = ThemeAppearanceSessionSaveLine::NotSaved(match e {
                    vike_config::RowPlanError::Refused(
                        vike_secrets::RowWriteError::NoDatabase { .. },
                    ) => NO_DATABASE_NOTE.to_string(),
                    vike_config::RowPlanError::Refused(vike_secrets::RowWriteError::Busy) => {
                        BUSY_NOTE.to_string()
                    }
                    _ => format!("Not saved: {error}"),
                });
                return (landed, line);
            }
        }
    }
    (rows.len(), ThemeAppearanceSessionSaveLine::Saved)
}

/// Append one record; a ledger failure is logged and changes nothing about the save, which has
/// already happened or already not.
fn record(journal: Option<&ChangeJournal>, now_ms: i64, change: Change) {
    if let Some(j) = journal
        && let Err(e) = j.append(now_ms, &change)
    {
        tracing::warn!(error = %e, "the appearance setting is saved; its ledger record is not");
    }
}

/// Whose settings these are — the line at the top of the Appearance section. This computer's,
/// in its own settings database; never the backend's, whose `preferences.*` rows are what
/// Connections → Backend settings shows and what a desktop on THAT box would read.
///
/// ⚠ It names the database as where they are saved ONLY when `database_present`: on a computer
/// with a project but no database (a fresh install, the Docker thin client) the line says a change
/// lasts until the app closes, because the app never creates one.
pub(crate) fn where_saved(settings_dir: Option<&Path>, database_present: bool) -> String {
    match settings_dir {
        Some(dir) if database_present => format!(
            "These settings belong to this computer: they are saved in {}, not on the backend.",
            vike_secrets::db_path_in(dir).display()
        ),
        Some(dir) => format!(
            "These settings belong to this computer, not the backend, but it has no settings \
             database ({}): a change here lasts until the app closes, and the app never creates \
             one.",
            vike_secrets::db_path_in(dir).display()
        ),
        None => "This app found no project settings directory, so a change here lasts until it \
                 closes."
            .to_string(),
    }
}

/// [`CARBON_COLOURBLIND_NOTE`] exactly when the theme is Carbon and the market set Colour-blind.
pub(crate) fn pair_note(a: &ThemeAppearance) -> Option<&'static str> {
    (a.theme == ThemePaletteId::Carbon && a.market == ThemeMarketColorsId::ColourBlind)
        .then_some(CARBON_COLOURBLIND_NOTE)
}

#[path = "appearance_settings_tests.rs"]
#[cfg(test)]
mod appearance_settings_tests;
