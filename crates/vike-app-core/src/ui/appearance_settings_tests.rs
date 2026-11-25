use super::*;
use crate::test_support::{planted, preference};
use vike_ui_theme::theme::ThemePalette;

#[test]
fn the_settings_defaults_are_the_default_appearance() {
    let p = vike_config::Preferences::default();
    let a =
        appearance_from(&p.theme, &p.market_colors, p.header_gradient, &p.density, &p.text_size);
    assert_eq!(a, ThemeAppearance::default());
}

#[test]
fn every_word_reaches_its_option_and_a_foreign_word_the_default() {
    for id in ThemePaletteId::ALL {
        assert_eq!(appearance_from(id.key(), "classic", false, "normal", "standard").theme, id);
    }
    for m in ThemeMarketColorsId::ALL {
        assert_eq!(appearance_from("graphite", m.key(), false, "normal", "standard").market, m);
    }
    for d in ThemeDensity::ALL {
        assert_eq!(appearance_from("graphite", "classic", false, d.key(), "standard").density, d);
    }
    for s in ThemeTextSize::ALL {
        assert_eq!(appearance_from("graphite", "classic", false, "normal", s.key()).text_size, s);
    }
    assert!(appearance_from("graphite", "classic", true, "normal", "standard").header_gradient);
    assert_eq!(
        appearance_from("solarized", "rainbow", false, "cosy", "huge"),
        ThemeAppearance::default()
    );
}

/// 2026-08-24T00:00:00Z.
const T: i64 = 1_787_616_000_000;

fn midnight() -> ThemeAppearance {
    ThemeAppearance { theme: ThemePaletteId::Midnight, ..ThemeAppearance::default() }
}

#[test]
fn nothing_pending_or_the_same_look_touches_nothing() {
    let context = egui::Context::default();
    let mut s = ThemeAppearanceSession::new(ThemeAppearance::default(), None);
    assert!(!s.apply_pending(&context, None, T), "nothing requested");
    // A marker an apply would overwrite with Graphite's background.
    context.set_visuals(egui::Visuals { panel_fill: egui::Color32::RED, ..egui::Visuals::dark() });
    s.request(ThemeAppearance::default());
    assert!(!s.apply_pending(&context, None, T), "the same look is not applied again");
    assert_eq!(context.global_style().visuals.panel_fill, egui::Color32::RED);
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Idle);
}

#[test]
fn a_change_is_applied_to_the_context_and_saved_as_one_row() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()));
    s.request(midnight());
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(s.appearance(), midnight());
    assert_eq!(vike_ui_theme::appearance::current(&context), midnight());
    assert_eq!(
        context.global_style().visuals.panel_fill,
        ThemePalette::of(ThemePaletteId::Midnight).background
    );
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Saved);
    assert_eq!(preference(dir.path(), "theme").as_deref(), Some("\"midnight\""));
    assert_eq!(preference(dir.path(), "density"), None, "only the setting that moved is written");
}

#[test]
fn a_change_writes_only_the_settings_that_moved() {
    let before = ThemeAppearance::default();
    let after = ThemeAppearance {
        header_gradient: true,
        text_size: ThemeTextSize::Large,
        ..ThemeAppearance::default()
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
    let context = egui::Context::default();
    let mut s = ThemeAppearanceSession::new(ThemeAppearance::default(), None);
    s.request(midnight());
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(vike_ui_theme::appearance::current(&context), midnight(), "applied anyway");
    assert_eq!(
        s.last(),
        &ThemeAppearanceSessionSaveLine::NotSaved(NO_SETTINGS_DIR_NOTE.to_string())
    );
}

/// Review Focus 1: a computer with no settings database — the change applies, the screen says
/// it was not saved, and the database is NOT created (creating one would hide the credential
/// files beside it).
#[test]
fn with_no_database_the_change_applies_and_no_database_is_created() {
    let dir = tempfile::tempdir().expect("a project directory with no database");
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()));
    s.request(midnight());
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(vike_ui_theme::appearance::current(&context), midnight());
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::NotSaved(NO_DATABASE_NOTE.to_string()));
    assert!(!vike_secrets::db_path_in(dir.path()).exists(), "the app never creates the store");
}

#[test]
fn a_saved_change_is_recorded_in_the_change_journal() {
    use vike_change_journal::{
        CHANGES_SUBDIR, ChangeJournal, ChangeProcess, KIND_SET_SETTING, month_file_name,
    };
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let state = dir.path().join("state");
    let journal =
        ChangeJournal::in_state_dir(&state, ChangeProcess::new("vike-test", 4711, "0.1.0"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()));
    s.request(midnight());
    s.apply_pending(&context, Some(&journal), T);
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
    for theme in ThemePaletteId::ALL {
        for market in ThemeMarketColorsId::ALL {
            let a = ThemeAppearance { theme, market, ..ThemeAppearance::default() };
            let want =
                theme == ThemePaletteId::Carbon && market == ThemeMarketColorsId::ColourBlind;
            assert_eq!(pair_note(&a).is_some(), want, "{theme:?} + {market:?}");
        }
    }
}

/// Review Focus 3: whose settings these are, said on the screen.
#[test]
fn the_screen_line_names_this_computers_database_and_not_the_backend() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
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
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()))
            .database_present()
    );
    let empty = tempfile::tempdir().expect("tempdir");
    assert!(
        !ThemeAppearanceSession::new(ThemeAppearance::default(), Some(empty.path().to_path_buf()))
            .database_present()
    );
    assert!(!ThemeAppearanceSession::new(ThemeAppearance::default(), None).database_present());
}

/// Final review, Important 1: a save refused because another writer held the store — the
/// zero lock budget's ordinary failure — must not leave the store behind the screen. The next
/// change writes the refused row TOO, so its "Saved" is true.
#[test]
fn a_refused_save_is_retried_with_the_next_change() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()));
    let lock = vike_secrets::hold_write_lock(dir.path());
    s.request(midnight());
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::NotSaved(BUSY_NOTE.to_string()));
    assert_eq!(preference(dir.path(), "theme"), None, "the held lock refused the write");
    drop(lock);
    s.request(ThemeAppearance { density: ThemeDensity::Compact, ..midnight() });
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(preference(dir.path(), "density").as_deref(), Some("\"compact\""));
    assert_eq!(
        preference(dir.path(), "theme").as_deref(),
        Some("\"midnight\""),
        "the refused theme row is written with the next change, or 'Saved' would be false"
    );
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Saved);
}

/// …and it can be saved again WITHOUT changing the look: the screen offers that, and asks
/// for it by requesting the look already in force.
#[test]
fn a_refused_save_can_be_saved_again_without_changing_the_look() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()));
    let lock = vike_secrets::hold_write_lock(dir.path());
    s.request(midnight());
    s.apply_pending(&context, None, T);
    drop(lock);
    s.request(s.appearance());
    assert!(s.apply_pending(&context, None, T), "a same-look request saves what is unsaved");
    assert_eq!(preference(dir.path(), "theme").as_deref(), Some("\"midnight\""));
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Saved);
    s.request(s.appearance());
    assert!(!s.apply_pending(&context, None, T), "nothing left to save: nothing is touched");
}

// ── preferences.vsync: saved, never applied ─────────────────────────────────────────────

#[test]
fn a_new_session_runs_the_librarys_vsync_and_owes_no_restart() {
    let s = ThemeAppearanceSession::new(ThemeAppearance::default(), None);
    assert!(s.vsync() && s.vsync_in_force());
    assert!(!s.vsync_needs_restart());
    assert_eq!(s.vsync_note(), None);
    let off = ThemeAppearanceSession::new(ThemeAppearance::default(), None).with_vsync(false);
    assert!(!off.vsync() && !off.vsync_in_force());
    assert!(!off.vsync_needs_restart(), "a window opened with vsync off already runs it");
}

/// The row lands, the choice shows, the running window's value does not move, and the screen
/// says a restart is owed until the choice is put back.
#[test]
fn a_vsync_choice_is_saved_and_never_applied() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()))
            .with_vsync(true);
    s.request_vsync(false);
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(preference(dir.path(), "vsync").as_deref(), Some("false"));
    assert!(!s.vsync(), "the choice on screen");
    assert!(s.vsync_in_force(), "the running window keeps the present mode it opened with");
    assert!(s.vsync_needs_restart());
    let note = s.vsync_note().expect("a restart is owed");
    assert!(note.contains("next time the app starts"), "{note}");
    assert!(note.contains("this run keeps vertical sync on"), "{note}");
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Saved);
    assert_eq!(s.appearance(), ThemeAppearance::default(), "no look changed");
    assert_eq!(preference(dir.path(), "theme"), None, "and no appearance row was written");

    s.request_vsync(true);
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(preference(dir.path(), "vsync").as_deref(), Some("true"));
    assert!(!s.vsync_needs_restart(), "put back to what is running");
    assert_eq!(s.vsync_note(), None);
}

#[test]
fn requesting_the_vsync_already_saved_touches_nothing() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()))
            .with_vsync(false);
    s.request_vsync(false);
    assert!(!s.apply_pending(&context, None, T));
    assert_eq!(preference(dir.path(), "vsync"), None);
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Idle);
}

#[test]
fn a_look_and_a_vsync_choice_together_write_two_rows_and_both_are_adopted() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()));
    s.request(midnight());
    s.request_vsync(false);
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(preference(dir.path(), "theme").as_deref(), Some("\"midnight\""));
    assert_eq!(preference(dir.path(), "vsync").as_deref(), Some("false"));
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Saved);
    assert!(!s.can_save_again(), "both rows landed");
}

/// The vsync row is retried like any other: a refused save leaves it unsaved, "Save again" is
/// offered for it ALONE (the look is already saved), and the same-look request that button makes
/// writes it.
#[test]
fn a_refused_vsync_save_is_offered_again_and_written_by_the_same_look_request() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()));
    let lock = vike_secrets::hold_write_lock(dir.path());
    s.request_vsync(false);
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::NotSaved(BUSY_NOTE.to_string()));
    assert_eq!(preference(dir.path(), "vsync"), None, "the held lock refused the write");
    assert!(!s.vsync(), "the choice stays on screen");
    assert!(s.can_save_again(), "only the vsync row is unsaved, and that is enough");
    drop(lock);
    s.request(s.appearance());
    assert!(s.apply_pending(&context, None, T), "a same-look request saves what is unsaved");
    assert_eq!(preference(dir.path(), "vsync").as_deref(), Some("false"));
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Saved);
    assert!(!s.can_save_again());
}

#[test]
fn with_no_settings_directory_the_vsync_choice_is_kept_and_not_saved() {
    let context = egui::Context::default();
    let mut s = ThemeAppearanceSession::new(ThemeAppearance::default(), None);
    s.request_vsync(false);
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(
        s.last(),
        &ThemeAppearanceSessionSaveLine::NotSaved(NO_SETTINGS_DIR_NOTE.to_string())
    );
    assert!(!s.vsync());
}

#[test]
fn the_vsync_hint_names_the_cap_and_the_label_says_a_restart_is_needed() {
    assert!(VSYNC_LABEL.contains("requires restart"));
    let hint = vsync_hint();
    assert!(hint.contains("144 frames a second"), "{hint}");
    assert!(hint.contains("tear"), "{hint}");
}

// ── preferences.gpu_candles and preferences.display_timezone: in force at once, saved ──

/// vike-config spells the zones as data (layer 20 cannot name `ThemeDisplayTimeZone`): this holds its list
/// equal to the zones' own names, in menu order, and the session's defaults to its own.
#[test]
fn the_zone_words_are_the_zones_names_and_a_new_session_starts_on_the_defaults() {
    use vike_config::preferences::{DEFAULT_DISPLAY_TIMEZONE, DISPLAY_TIMEZONES};
    assert_eq!(DISPLAY_TIMEZONES, ThemeDisplayTimeZone::CURATED.map(ThemeDisplayTimeZone::name));
    assert_eq!(DEFAULT_DISPLAY_TIMEZONE, ThemeDisplayTimeZone::default().name());
    let s = ThemeAppearanceSession::new(ThemeAppearance::default(), None);
    assert!(s.gpu_candles(), "on by default");
    assert!(!s.draws_gpu_candles(), "…and drawn on the GPU only where a GPU path exists");
    assert_eq!(s.display_time_zone(), ThemeDisplayTimeZone::Local);
    let started = s.with_display((true, "Asia/Tokyo"), true);
    assert!(started.draws_gpu_candles());
    assert_eq!(started.display_time_zone(), ThemeDisplayTimeZone::parse("Asia/Tokyo"));
}

#[test]
fn a_display_choice_is_in_force_at_once_and_saved_as_its_row() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()))
            .with_display((true, "Local"), true);
    s.request_gpu_candles(false);
    s.request_display_time_zone(ThemeDisplayTimeZone::Utc);
    assert!(s.apply_pending(&context, None, T));
    assert!(!s.gpu_candles() && !s.draws_gpu_candles());
    assert_eq!(s.display_time_zone(), ThemeDisplayTimeZone::Utc);
    assert_eq!(preference(dir.path(), "gpu_candles").as_deref(), Some("false"));
    assert_eq!(preference(dir.path(), "display_timezone").as_deref(), Some("\"UTC\""));
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::Saved);
    assert_eq!(preference(dir.path(), "theme"), None, "no appearance row was written");
    s.request_display_time_zone(ThemeDisplayTimeZone::Utc);
    assert!(!s.apply_pending(&context, None, T), "the zone in force and saved touches nothing");
}

/// A refused save leaves the choice IN FORCE (it is live) and offers "Save again" for it.
#[test]
fn a_refused_display_save_stays_in_force_and_is_offered_again() {
    let dir = planted(tempfile::tempdir().expect("tempdir"));
    let context = egui::Context::default();
    let mut s =
        ThemeAppearanceSession::new(ThemeAppearance::default(), Some(dir.path().to_path_buf()));
    let lock = vike_secrets::hold_write_lock(dir.path());
    let tokyo = ThemeDisplayTimeZone::parse("Asia/Tokyo");
    s.request_display_time_zone(tokyo);
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(s.last(), &ThemeAppearanceSessionSaveLine::NotSaved(BUSY_NOTE.to_string()));
    assert_eq!(s.display_time_zone(), tokyo, "in force although not saved");
    assert!(s.can_save_again());
    drop(lock);
    s.request(s.appearance());
    assert!(s.apply_pending(&context, None, T));
    assert_eq!(preference(dir.path(), "display_timezone").as_deref(), Some("\"Asia/Tokyo\""));
    assert!(!s.can_save_again());
}

#[test]
fn retime_puts_every_chart_in_the_zone_in_force() {
    let s = ThemeAppearanceSession::new(ThemeAppearance::default(), None)
        .with_display((true, "Asia/Kolkata"), false);
    let mut charts = [ChartState::default(), ChartState::default()];
    s.retime(charts.iter_mut());
    assert!(charts.iter().all(|c| c.time_zone() == ThemeDisplayTimeZone::parse("Asia/Kolkata")));
}
