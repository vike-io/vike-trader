//! The desktop's five appearance preferences (design system spec §5): read at start, changed on
//! the Settings window's Appearance section, re-applied live and saved as `preferences.*` rows of
//! THIS computer's settings database.
//!
//! The five READS stay in `crates/vike-desktop/src/main.rs`, so `vike-cli config show` names
//! `desktop` as their reader (`vike_config::Consumer::binary`). Everything that decides is here,
//! where CI runs it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::market::MarketId;
use vike_ui_theme::metrics::Density;
use vike_ui_theme::theme::ThemeId;
use vike_ui_theme::type_scale::TextSize;

/// The appearance the five resolved preference values name. A word no option has — unreachable
/// through the loader, which refuses it, but this function does not assume its caller — answers
/// that setting's default: a look is never a reason not to open a window.
pub fn appearance_from(
    theme: &str,
    market_colors: &str,
    header_gradient: bool,
    density: &str,
    text_size: &str,
) -> Appearance {
    Appearance {
        theme: ThemeId::from_key(theme).unwrap_or_default(),
        market: MarketId::from_key(market_colors).unwrap_or_default(),
        header_gradient,
        density: Density::from_key(density).unwrap_or_default(),
        text_size: TextSize::from_key(text_size).unwrap_or_default(),
    }
}

/// After a change was saved.
pub const SAVED_NOTE: &str = "Saved. It applies now, and every time the app starts.";
/// The boot found no project, so there is nowhere to save.
pub const NO_SETTINGS_DIR_NOTE: &str = "Not saved: the app found no project settings directory, \
     so this look lasts until the app closes. Start it inside the project, or set VIKE_SETTINGS_DIR.";
/// The project has no settings database, and the app does not create one.
pub const NO_DATABASE_NOTE: &str = "Not saved: this computer has no settings database, and the \
     app never creates one (a new database would hide the credential files beside it). This look \
     lasts until the app closes.";
/// Another writer held the store; nothing was written. The screen offers "Save again"
/// ([`AppearanceSession::can_save_again`]), and the next change saves the refused row too.
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

/// The wait a save gives the settings database's write lock: none. The save runs on the render
/// thread, like the venue-arming switch's, and a frame cannot lend another process any of its
/// 16 ms — `crates/vike-app-core/src/ui/tool_views/venues.rs`'s `VENUE_ARM_LOCK_BUDGET` carries
/// the argument.
pub const APPEARANCE_LOCK_BUDGET: Duration = Duration::ZERO;

/// What the last change did about SAVING it — the line under the Appearance section's controls.
/// Applying never fails; saving can, and says why.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SaveLine {
    /// No change yet this session.
    #[default]
    Idle,
    /// The change is applied and saved.
    Saved,
    /// The change is applied for this session and NOT saved; the reason, as the screen shows it.
    NotSaved(String),
}

/// The appearance this process runs, the look the settings database holds, and the Settings
/// window's pending change.
#[derive(Debug)]
pub struct AppearanceSession {
    appearance: Appearance,
    /// The look the settings database holds, as far as this process knows: the installed look at
    /// start, then advanced by every row that LANDS. A change writes the rows that differ from
    /// THIS, not from the look on screen, so a row an earlier save could not write is written
    /// with the next one and a "Saved" line is true of every setting.
    saved: Appearance,
    pending: Option<Appearance>,
    settings_dir: Option<PathBuf>,
    /// Whether `settings_dir` holds a settings database — at start, and again after every save.
    database_present: bool,
    last: SaveLine,
}

impl AppearanceSession {
    /// The session of an app that started with `appearance` installed. `settings_dir` is the
    /// binary's ONE boot walk, never a resolver call from here.
    pub fn new(appearance: Appearance, settings_dir: Option<PathBuf>) -> Self {
        let database_present = database_in(settings_dir.as_deref());
        Self {
            appearance,
            saved: appearance,
            pending: None,
            settings_dir,
            database_present,
            last: SaveLine::Idle,
        }
    }

    /// The appearance in force.
    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    /// What the last change did about saving it.
    pub fn last(&self) -> &SaveLine {
        &self.last
    }

    /// Where a change is saved; `None` when the boot found no project.
    pub fn settings_dir(&self) -> Option<&Path> {
        self.settings_dir.as_deref()
    }

    /// Whether this computer has a settings database to save into (never created by the app).
    pub fn database_present(&self) -> bool {
        self.database_present
    }

    /// `true` when the look in force is not what the database holds and there is a database to
    /// write to — a save was refused, most often because another writer held the store. The
    /// screen then offers "Save again", which requests the look already in force.
    pub fn can_save_again(&self) -> bool {
        self.appearance != self.saved && self.database_present
    }

    /// A choice from the screen. [`Self::apply_pending`] applies it once the frame's windows
    /// have drawn, so no frame mixes two looks. Requesting the look already in force saves
    /// whatever of it is not yet saved.
    pub fn request(&mut self, next: Appearance) {
        self.pending = Some(next);
    }

    /// Apply the pending choice and save it, if it changes anything; `true` when it did. The
    /// shell calls this once per frame, after its window loop — never
    /// `vike_ui_theme::appearance::install`, which would compare the whole font set again.
    pub fn apply_pending(
        &mut self,
        ctx: &egui::Context,
        journal: Option<&ChangeJournal>,
        now_ms: i64,
    ) -> bool {
        let Some(next) = self.pending.take() else {
            return false;
        };
        if next == self.appearance && next == self.saved {
            return false;
        }
        if next != self.appearance {
            vike_ui_theme::appearance::apply(ctx, &next);
            self.appearance = next;
        }
        let rows = changed_rows(&self.saved, &next);
        let (landed, line) = save_rows(self.settings_dir.as_deref(), &rows, journal, now_ms);
        for (key, _) in &rows[..landed] {
            adopt(&mut self.saved, &next, key);
        }
        self.last = line;
        self.database_present = database_in(self.settings_dir.as_deref());
        true
    }
}

/// Whether `settings_dir` holds a settings database. `None` holds none.
fn database_in(settings_dir: Option<&Path>) -> bool {
    settings_dir.is_some_and(|d| vike_secrets::database_present(&vike_secrets::db_path_in(d)))
}

// The five dotted keys: `changed_rows` writes them, `adopt` reads a landed one back.
const THEME_KEY: &str = "preferences.theme";
const MARKET_KEY: &str = "preferences.market_colors";
const GRADIENT_KEY: &str = "preferences.header_gradient";
const DENSITY_KEY: &str = "preferences.density";
const TEXT_SIZE_KEY: &str = "preferences.text_size";

/// The rows a change writes: one `(dotted key, value)` per setting that differs, in the order the
/// spec's §5 table lists them.
pub fn changed_rows(before: &Appearance, after: &Appearance) -> Vec<(&'static str, &'static str)> {
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
fn adopt(saved: &mut Appearance, next: &Appearance, key: &str) {
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
/// `crates/vike-secrets/src/db/migrate/outcome.rs`'s `MigrationOutcome` says why creating one is the harmful act.
pub fn save_rows(
    settings_dir: Option<&Path>,
    rows: &[(&str, &str)],
    journal: Option<&ChangeJournal>,
    now_ms: i64,
) -> (usize, SaveLine) {
    let Some(dir) = settings_dir else {
        return (0, SaveLine::NotSaved(NO_SETTINGS_DIR_NOTE.to_string()));
    };
    for (landed, (key, value)) in rows.iter().enumerate() {
        match vike_config::write_setting_row(dir, key, value, APPEARANCE_LOCK_BUDGET) {
            Ok(report) => record(
                journal,
                now_ms,
                Change::set_setting(
                    Outcome::Applied,
                    Actor::Gui,
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
                        Outcome::Refused,
                        Actor::Gui,
                        "preferences",
                        key,
                        None,
                        value,
                    )
                    .with_reason(Some(&error)),
                );
                tracing::error!(key = %key, error = %error, "appearance setting not saved");
                let line = SaveLine::NotSaved(match e {
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
    (rows.len(), SaveLine::Saved)
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
pub fn where_saved(settings_dir: Option<&Path>, database_present: bool) -> String {
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
pub fn pair_note(a: &Appearance) -> Option<&'static str> {
    (a.theme == ThemeId::Carbon && a.market == MarketId::ColourBlind)
        .then_some(CARBON_COLOURBLIND_NOTE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use vike_ui_theme::theme::Theme;

    /// vike-config spells the words as data, because it cannot name the options (layer 20 under
    /// 70). This holds its lists equal to the options' own keys, in order.
    #[test]
    fn the_settings_words_are_the_options_keys() {
        use vike_config::preferences::{DENSITIES, MARKET_COLOR_SETS, TEXT_SIZES, THEMES};
        assert_eq!(THEMES, ThemeId::ALL.map(ThemeId::key));
        assert_eq!(MARKET_COLOR_SETS, MarketId::ALL.map(MarketId::key));
        assert_eq!(DENSITIES, Density::ALL.map(Density::key));
        assert_eq!(TEXT_SIZES, TextSize::ALL.map(TextSize::key));
    }

    #[test]
    fn the_settings_defaults_are_the_default_appearance() {
        let p = vike_config::Preferences::default();
        let a = appearance_from(
            &p.theme,
            &p.market_colors,
            p.header_gradient,
            &p.density,
            &p.text_size,
        );
        assert_eq!(a, Appearance::default());
    }

    #[test]
    fn every_word_reaches_its_option_and_a_foreign_word_the_default() {
        for id in ThemeId::ALL {
            assert_eq!(appearance_from(id.key(), "classic", false, "normal", "standard").theme, id);
        }
        for m in MarketId::ALL {
            assert_eq!(appearance_from("graphite", m.key(), false, "normal", "standard").market, m);
        }
        for d in Density::ALL {
            assert_eq!(
                appearance_from("graphite", "classic", false, d.key(), "standard").density,
                d
            );
        }
        for s in TextSize::ALL {
            assert_eq!(
                appearance_from("graphite", "classic", false, "normal", s.key()).text_size,
                s
            );
        }
        assert!(appearance_from("graphite", "classic", true, "normal", "standard").header_gradient);
        assert_eq!(
            appearance_from("solarized", "rainbow", false, "cosy", "huge"),
            Appearance::default()
        );
    }

    /// 2026-08-24T00:00:00Z.
    const T: i64 = 1_787_616_000_000;

    fn planted() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        vike_secrets::plant_settings_rows(
            d.path(),
            &vike_secrets::StoredSettings {
                settings: vec![vike_secrets::SettingRow {
                    section: "preferences".to_string(),
                    key: "log_level".to_string(),
                    value: "\"info\"".to_string(),
                }],
                ..Default::default()
            },
        )
        .expect("a fresh store plants");
        d
    }

    fn row(dir: &Path, key: &str) -> Option<String> {
        let read = vike_secrets::read_settings_in(dir).ok()?;
        read.rows()?
            .settings
            .iter()
            .find(|r| r.section == "preferences" && r.key == key)
            .map(|r| r.value.clone())
    }

    fn midnight() -> Appearance {
        Appearance { theme: ThemeId::Midnight, ..Appearance::default() }
    }

    #[test]
    fn nothing_pending_or_the_same_look_touches_nothing() {
        let ctx = egui::Context::default();
        let mut s = AppearanceSession::new(Appearance::default(), None);
        assert!(!s.apply_pending(&ctx, None, T), "nothing requested");
        // A marker an apply would overwrite with Graphite's background.
        ctx.set_visuals(egui::Visuals { panel_fill: egui::Color32::RED, ..egui::Visuals::dark() });
        s.request(Appearance::default());
        assert!(!s.apply_pending(&ctx, None, T), "the same look is not applied again");
        assert_eq!(ctx.global_style().visuals.panel_fill, egui::Color32::RED);
        assert_eq!(s.last(), &SaveLine::Idle);
    }

    #[test]
    fn a_change_is_applied_to_the_context_and_saved_as_one_row() {
        let dir = planted();
        let ctx = egui::Context::default();
        let mut s = AppearanceSession::new(Appearance::default(), Some(dir.path().to_path_buf()));
        s.request(midnight());
        assert!(s.apply_pending(&ctx, None, T));
        assert_eq!(s.appearance(), midnight());
        assert_eq!(vike_ui_theme::appearance::current(&ctx), midnight());
        assert_eq!(ctx.global_style().visuals.panel_fill, Theme::of(ThemeId::Midnight).bg);
        assert_eq!(s.last(), &SaveLine::Saved);
        assert_eq!(row(dir.path(), "theme").as_deref(), Some("\"midnight\""));
        assert_eq!(row(dir.path(), "density"), None, "only the setting that moved is written");
    }

    #[test]
    fn a_change_writes_only_the_settings_that_moved() {
        let before = Appearance::default();
        let after = Appearance {
            header_gradient: true,
            text_size: TextSize::Large,
            ..Appearance::default()
        };
        assert_eq!(
            changed_rows(&before, &after),
            [("preferences.header_gradient", "true"), ("preferences.text_size", "large")]
        );
        assert!(changed_rows(&after, &after).is_empty());
        assert_eq!(
            changed_rows(&after, &before),
            [("preferences.header_gradient", "false"), ("preferences.text_size", "standard")]
        );
    }

    #[test]
    fn with_no_settings_directory_the_change_applies_and_says_it_was_not_saved() {
        let ctx = egui::Context::default();
        let mut s = AppearanceSession::new(Appearance::default(), None);
        s.request(midnight());
        assert!(s.apply_pending(&ctx, None, T));
        assert_eq!(vike_ui_theme::appearance::current(&ctx), midnight(), "applied anyway");
        assert_eq!(s.last(), &SaveLine::NotSaved(NO_SETTINGS_DIR_NOTE.to_string()));
    }

    /// Review Focus 1: a computer with no settings database — the change applies, the screen says
    /// it was not saved, and the database is NOT created (creating one would hide the credential
    /// files beside it).
    #[test]
    fn with_no_database_the_change_applies_and_no_database_is_created() {
        let dir = tempfile::tempdir().expect("a project directory with no database");
        let ctx = egui::Context::default();
        let mut s = AppearanceSession::new(Appearance::default(), Some(dir.path().to_path_buf()));
        s.request(midnight());
        assert!(s.apply_pending(&ctx, None, T));
        assert_eq!(vike_ui_theme::appearance::current(&ctx), midnight());
        assert_eq!(s.last(), &SaveLine::NotSaved(NO_DATABASE_NOTE.to_string()));
        assert!(!vike_secrets::db_path_in(dir.path()).exists(), "the app never creates the store");
    }

    #[test]
    fn a_saved_change_is_recorded_in_the_change_journal() {
        use vike_model::change_journal::{
            CHANGES_SUBDIR, ChangeJournal, KIND_SET_SETTING, Proc, month_file_name,
        };
        let dir = planted();
        let state = dir.path().join("state");
        let journal = ChangeJournal::in_state_dir(&state, Proc::new("vike-test", 4711, "0.1.0"));
        let ctx = egui::Context::default();
        let mut s = AppearanceSession::new(Appearance::default(), Some(dir.path().to_path_buf()));
        s.request(midnight());
        s.apply_pending(&ctx, Some(&journal), T);
        let ledger = std::fs::read_to_string(state.join(CHANGES_SUBDIR).join(month_file_name(T)))
            .expect("the ledger file");
        let record: serde_json::Value = serde_json::from_str(ledger.trim()).expect("one line");
        assert_eq!(record["kind"], KIND_SET_SETTING);
        assert_eq!(record["target"]["key"], "preferences.theme");
        assert_eq!(record["target"]["new"], "\"midnight\"");
        assert_eq!(record["actor"]["origin"], "gui");
        // Applied, not applied-pending-restart: the GUI re-applied the change live.
        assert_eq!(record["outcome"], "applied");
    }

    #[test]
    fn the_carbon_note_is_for_carbon_with_colour_blind_only() {
        for theme in ThemeId::ALL {
            for market in MarketId::ALL {
                let a = Appearance { theme, market, ..Appearance::default() };
                let want = theme == ThemeId::Carbon && market == MarketId::ColourBlind;
                assert_eq!(pair_note(&a).is_some(), want, "{theme:?} + {market:?}");
            }
        }
    }

    /// Review Focus 3: whose settings these are, said on the screen.
    #[test]
    fn the_screen_line_names_this_computers_database_and_not_the_backend() {
        let dir = planted();
        let db = vike_secrets::db_path_in(dir.path()).display().to_string();
        let line = where_saved(Some(dir.path()), true);
        assert!(line.contains(&db), "{line}");
        assert!(line.contains("not on the backend"), "{line}");
        // Final review, Important 2: with no database the line claims no save it cannot make.
        let none = where_saved(Some(dir.path()), false);
        assert!(!none.contains("they are saved in"), "{none}");
        assert!(none.contains("not the backend") && none.contains(&db), "{none}");
        assert!(none.contains("lasts until the app closes"), "{none}");
        assert!(where_saved(None, false).contains("until it closes"));
        // The session knows which: a planted directory has a database, an empty one has none.
        assert!(
            AppearanceSession::new(Appearance::default(), Some(dir.path().to_path_buf()))
                .database_present()
        );
        let empty = tempfile::tempdir().expect("tempdir");
        assert!(
            !AppearanceSession::new(Appearance::default(), Some(empty.path().to_path_buf()))
                .database_present()
        );
        assert!(!AppearanceSession::new(Appearance::default(), None).database_present());
    }

    /// Final review, Important 1: a save refused because another writer held the store — the
    /// zero lock budget's ordinary failure — must not leave the store behind the screen. The next
    /// change writes the refused row TOO, so its "Saved" is true.
    #[test]
    fn a_refused_save_is_retried_with_the_next_change() {
        let dir = planted();
        let ctx = egui::Context::default();
        let mut s = AppearanceSession::new(Appearance::default(), Some(dir.path().to_path_buf()));
        let lock = vike_secrets::hold_write_lock(dir.path());
        s.request(midnight());
        assert!(s.apply_pending(&ctx, None, T));
        assert_eq!(s.last(), &SaveLine::NotSaved(BUSY_NOTE.to_string()));
        assert_eq!(row(dir.path(), "theme"), None, "the held lock refused the write");
        drop(lock);
        s.request(Appearance { density: Density::Compact, ..midnight() });
        assert!(s.apply_pending(&ctx, None, T));
        assert_eq!(row(dir.path(), "density").as_deref(), Some("\"compact\""));
        assert_eq!(
            row(dir.path(), "theme").as_deref(),
            Some("\"midnight\""),
            "the refused theme row is written with the next change, or 'Saved' would be false"
        );
        assert_eq!(s.last(), &SaveLine::Saved);
    }

    /// …and it can be saved again WITHOUT changing the look: the screen offers that, and asks
    /// for it by requesting the look already in force.
    #[test]
    fn a_refused_save_can_be_saved_again_without_changing_the_look() {
        let dir = planted();
        let ctx = egui::Context::default();
        let mut s = AppearanceSession::new(Appearance::default(), Some(dir.path().to_path_buf()));
        let lock = vike_secrets::hold_write_lock(dir.path());
        s.request(midnight());
        s.apply_pending(&ctx, None, T);
        drop(lock);
        s.request(s.appearance());
        assert!(s.apply_pending(&ctx, None, T), "a same-look request saves what is unsaved");
        assert_eq!(row(dir.path(), "theme").as_deref(), Some("\"midnight\""));
        assert_eq!(s.last(), &SaveLine::Saved);
        s.request(s.appearance());
        assert!(!s.apply_pending(&ctx, None, T), "nothing left to save: nothing is touched");
    }
}
