//! **The connected node's effective-settings table, rendered for real** —
//! `tool_views::backend_settings`'s `backend_settings_section`, driven directly through a bare
//! `egui_kittest::Harness` with no window chrome around it at all.
//!
//! Ported from `connections_tabs.rs` (deleted this task, Connections-merges-into-Data-Manager):
//! these tests never depended on `ConnectionsTab`/`connections_body`/`ambient_strip`/the
//! title-bar-tab chrome that task deletes; they drive `backend_settings_section` the same way
//! `backend_tab` (which survives, called directly from Data Manager's `DataDest::Backend`) does.
//! ⚠ The reference redo of this exact task (an older checkout, reviewed clean) made and caught a
//! real mistake here — deleting these tests wholesale along with the chrome file, before noticing
//! they drove no chrome at all — which is why they are split into their own file in THIS pass
//! rather than deleted with the rest.

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use std::assert_matches;
use vike_app_core::ui::tool_views::{
    BackendSettingsState, SettingsEditState, SettingsFilter, SettingsWriteRequest,
    backend_settings_section,
};
use vike_tradehub_client::wire::{WireSettingsRow, WireSettingsShow};

// ------------------------------------------------------------------------------------------
// Tree helpers — the same shapes `connections_tabs.rs` used.
// ------------------------------------------------------------------------------------------

fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

fn button_labels(h: &Harness<'_, ()>) -> Vec<String> {
    nodes(h, |n| n.accesskit_node().role() == Role::Button)
        .iter()
        .map(|n| n.accesskit_node().label().unwrap_or_default().to_string())
        .collect()
}

fn node_text(n: &Node<'_>) -> String {
    let a = n.accesskit_node();
    match (a.label(), a.value()) {
        (Some(l), _) if !l.is_empty() => l.to_string(),
        (_, Some(v)) => v.to_string(),
        _ => String::new(),
    }
}

fn node_rect(n: &Node<'_>) -> Option<egui::Rect> {
    n.accesskit_node().bounding_box().map(|_| n.rect())
}

fn tree_text(h: &Harness<'_, ()>) -> String {
    h.root()
        .children_recursive()
        .map(|n| {
            let a = n.accesskit_node();
            let mut s = String::new();
            if let Some(l) = a.label() {
                s.push_str(&l);
                s.push(' ');
            }
            if let Some(v) = a.value() {
                s.push_str(&v);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn text_rects(h: &Harness<'_, ()>, want: &str) -> Vec<egui::Rect> {
    nodes(h, |_| true).iter().filter(|n| node_text(n) == want).filter_map(node_rect).collect()
}

/// Every accessibility node's VALUE, one entry each — egui files a `Role::Label`'s text there, so
/// this is how a cell's exact content is asserted rather than searched for as a substring.
fn values(h: &Harness<'_, ()>) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter_map(|n| n.accesskit_node().value().map(|v| v.to_string()))
        .collect()
}

fn click(h: &mut Harness<'_, ()>, want: &str) {
    let wanted = want.to_string();
    let found = nodes(h, move |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(wanted.as_str())
    });
    assert_eq!(found.len(), 1, "exactly one button labelled {want:?} must be on screen");
    found[0].click();
    h.run();
}

// ------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------

fn row(key: &str, section: &str, origin: &str, read_by: &str) -> WireSettingsRow {
    WireSettingsRow {
        section: section.into(),
        key: key.into(),
        value: "127.0.0.1:7879".into(),
        origin: origin.into(),
        read_by: read_by.into(),
    }
}

/// A node answer spanning every ORIGIN kind and every READ value, plus one amber finding.
fn show() -> WireSettingsShow {
    WireSettingsShow {
        settings_dir: Some("/srv/vike-<unit>/settings".into()),
        rows: vec![
            row("config.tradehub_addr", "config.toml", "config.toml", "tradehub"),
            row("config.node_addr", "config.toml", "env:VIKE_NODE_ADDR", "cli"),
            row("config.log_dir", "config.toml", "config.toml", "NO"),
            row("preferences.chart_style", "preferences.toml", "default", "yes"),
            row("policy.max_notional_per_order", "policy.toml", "policy.toml", "tradehub"),
        ],
    }
}

// ------------------------------------------------------------------------------------------
// The harness
// ------------------------------------------------------------------------------------------

struct Settings {
    harness: Harness<'static, ()>,
    state: std::rc::Rc<std::cell::RefCell<(SettingsEditState, SettingsFilter)>>,
    /// The last request a Save put in the section's `write` out-slot — what the binary would send.
    written: std::rc::Rc<std::cell::RefCell<Option<SettingsWriteRequest>>>,
}

fn settings_harness(state: BackendSettingsState, filter: SettingsFilter) -> Settings {
    settings_harness_armed(state, filter, true, 1000.0)
}

fn settings_harness_armed(
    state: BackendSettingsState,
    filter: SettingsFilter,
    control_armed: bool,
    width: f32,
) -> Settings {
    let shared: std::rc::Rc<std::cell::RefCell<(SettingsEditState, SettingsFilter)>> =
        std::rc::Rc::new(std::cell::RefCell::new((SettingsEditState::Idle, filter)));
    let inner = shared.clone();
    let written: std::rc::Rc<std::cell::RefCell<Option<SettingsWriteRequest>>> =
        std::rc::Rc::default();
    let sink = written.clone();
    let harness = Harness::builder().with_size(egui::vec2(width, 800.0)).build_ui(move |ui| {
        // The section draws icons (a finding's warning), whose family only the app's type binds.
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        let mut refresh = false;
        let mut write: Option<SettingsWriteRequest> = None;
        let mut s = inner.borrow_mut();
        let (edit, filt) = &mut *s;
        backend_settings_section(
            ui,
            Some("prod"),
            control_armed,
            &state,
            &mut refresh,
            edit,
            &mut write,
            filt,
        );
        if let Some(req) = write {
            *sink.borrow_mut() = Some(req);
        }
    });
    Settings { harness, state: shared, written }
}

/// Click the `edit` affordance on row `i` of the settings table, then settle a frame.
fn open_row(s: &mut Settings, i: usize) {
    {
        let edits = nodes(&s.harness, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("edit")
        });
        assert!(i < edits.len(), "row {i} has no edit affordance (found {})", edits.len());
        edits[i].click();
    }
    s.harness.run();
}

// ------------------------------------------------------------------------------------------
// The tests
// ------------------------------------------------------------------------------------------

/// The table's summary is ONE dense paragraph carrying every fact the mockup asks for, and the
/// numbers are folds over the rows the node actually sent.
#[test]
fn the_backend_summary_is_one_paragraph_of_measured_numbers() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(text.contains("5 keys"), "{text}");
    assert!(text.contains("4 set from env or file"), "{text}");
    assert!(text.contains("1 at their compiled-in default"), "{text}");
    let finding =
        vike_ui_theme::icons::WARNING.accessible_label("set and read by nothing: config.log_dir");
    assert!(text.contains(&finding), "{text}");
    assert!(text.contains("restart-to-apply"), "{text}");
    assert!(text.contains("/srv/vike-<unit>/settings"), "the settings dir is named: {text}");
}

/// ORIGIN keeps the wire's own distinctions — `env:VAR`, a file name, and `default` — and READ
/// keeps all THREE of its values. Reddens on a panel that reduces either to a boolean.
#[test]
fn origin_and_read_reach_the_screen_with_every_value_the_wire_carries() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    let cells = values(&s.harness);
    // ORIGIN: three DIFFERENT answers, each rendered as itself. Collapsing `env:VAR` and a file
    // name into one "set" word is what would hide the env-outranks-file hazard entirely.
    for origin in ["env:VIKE_NODE_ADDR", "config.toml", "policy.toml", "default"] {
        assert!(
            cells.iter().any(|c| c == origin),
            "the ORIGIN cell {origin:?} must reach the screen verbatim: {cells:?}"
        );
    }
    // READ: all THREE values — a binary's short name, `yes` (a library reads it), and `NO`.
    for read in ["tradehub", "cli", "yes", "NO"] {
        assert!(
            cells.iter().any(|c| c == read),
            "the READ cell {read:?} must reach the screen verbatim: {cells:?}"
        );
    }
}

/// The `set · N` / `all · N` filter counts what it says and hides what it says.
#[test]
fn the_filter_counts_and_hides_the_rows_it_names() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::Set);
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(text.contains("set · 4"), "{text}");
    assert!(text.contains("all · 5"), "{text}");
    assert!(
        !text.contains("preferences.chart_style"),
        "the `set` filter hides a defaulted row: {text}"
    );

    click(&mut s.harness, "all · 5");
    let text = tree_text(&s.harness);
    assert!(text.contains("preferences.chart_style"), "`all` shows it again: {text}");
}

/// ⚠ A save lands ONE ROW of the node's settings database and never a file
/// (`docs/decisions/0086-settings-live-only-in-the-database.md` point 1), so no button the editor
/// draws may say it writes one.
fn assert_no_button_names_a_file(buttons: &[String]) {
    assert!(
        !buttons.iter().any(|b| b.to_lowercase().contains("file")),
        "a save writes a settings-database row, never a file: {buttons:?}"
    );
}

/// ⚠⚠ **THE POINT OF THE WHOLE THING.** A row the ENVIRONMENT sets gets a prominent amber warning
/// naming the variable, and the Save button relabels itself `Save anyway` — because the write is
/// accepted, lands its row, answers `restart_required: true`, and changes nothing the daemon will
/// read while the variable stays set. A pure test of `env_shadow` stays green with this renderer
/// wired to nothing, which is exactly why the button's LABEL is read off the real tree.
///
/// ⚠ The label read `Save to file anyway` until 0086 was applied to this screen. The HAZARD it
/// disclosed is unchanged and still asserted here; only the FILE was false.
#[test]
fn an_env_shadowed_row_warns_and_its_save_button_says_anyway() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();

    // Every row carries an `edit` button; the env-shadowed one is the SECOND row.
    open_row(&mut s, 1);
    let text = tree_text(&s.harness);
    assert!(
        text.contains("the ENVIRONMENT sets this key"),
        "the hazard must be stated, not implied: {text}"
    );
    assert!(text.contains("VIKE_NODE_ADDR"), "the VARIABLE is named: {text}");
    assert!(
        text.contains("outranks the settings database"),
        "…and so is the layer it beats, which is a database row, not a file: {text}"
    );
    assert!(
        text.contains("unset on the daemon's box and it restarts"),
        "…and so is the remedy: {text}"
    );
    let buttons = button_labels(&s.harness);
    assert!(
        buttons.iter().any(|b| b == "Save anyway"),
        "the button must be honest about what it does: {buttons:?}"
    );
    assert!(!buttons.iter().any(|b| b == "Save"), "{buttons:?}");
    assert_no_button_names_a_file(&buttons);
}

/// A row the environment does NOT set gets no such warning, and its button is a plain `Save` — so
/// the amber is a signal rather than decoration.
#[test]
fn a_row_the_environment_does_not_set_gets_no_warning_and_a_plain_save_button() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    open_row(&mut s, 0);
    let text = tree_text(&s.harness);
    assert!(!text.contains("the ENVIRONMENT sets this key"), "{text}");
    let buttons = button_labels(&s.harness);
    assert!(buttons.iter().any(|b| b == "Save"), "{buttons:?}");
    assert!(!buttons.iter().any(|b| b == "Save anyway"), "{buttons:?}");
    assert_no_button_names_a_file(&buttons);
}

/// The editor states exactly what will be written and WHERE — one row of the node's own settings
/// database, by its path under the directory the node reported — and shows the effective value with
/// its origin.
///
/// ⚠ It named `<settings dir>/config.toml` here, and carried a note beside Save saying a deployed
/// daemon could not write `settings/*.toml` (EROFS). Both were file-era and both are false now: the
/// daemon writes one row through the `settings/db` grant its shipped unit carries
/// (`crates/vike-tradehub/src/server/settings.rs`'s `apply_set_setting`), and nothing reads or writes a
/// settings file. A refusal still reaches the operator verbatim, through the `Failed` state.
#[test]
fn the_editor_names_the_settings_database_row_and_the_effective_value() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    open_row(&mut s, 0);
    let text = tree_text(&s.harness);
    assert!(text.contains("Effective now"), "{text}");
    assert!(text.contains("from config.toml"), "the origin travels with the value: {text}");
    assert!(
        text.contains("/srv/vike-<unit>/settings/db/vike.db"),
        "the node's settings DATABASE is named before the click: {text}"
    );
    assert!(
        !text.contains("/srv/vike-<unit>/settings/config.toml"),
        "no settings FILE is a write target: {text}"
    );
    assert!(text.contains("takes effect on restart"), "{text}");
    assert!(
        !text.contains("EROFS") && !text.contains("settings/*.toml"),
        "the file-era sandbox note is gone — the daemon's unit grants settings/db: {text}"
    );
}

/// ⚠ **No typed confirm, for ANY row — the risk ceiling included**
/// (`docs/decisions/0086-settings-live-only-in-the-database.md` point 7: *"confirmation over
/// confirmation … a nightmare"*). Walked through the REAL renderer over every row the fixture
/// carries: no "type the key name" line is drawn, ONE click on the save button takes the flow to
/// `Saving`, and the out-slot the binary drains holds that row's key.
#[test]
fn every_row_saves_on_one_click_with_no_typed_confirm_the_policy_ceiling_included() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    let keys: Vec<String> = show().rows.iter().map(|r| r.key.clone()).collect();
    assert!(keys.iter().any(|k| k.starts_with("policy.")), "the walk must reach a ceiling");
    for (i, key) in keys.iter().enumerate() {
        open_row(&mut s, i);
        let text = tree_text(&s.harness);
        assert!(!text.contains("type the key name"), "row {key} demands no retype: {text}");
        let shadowed = button_labels(&s.harness).iter().any(|b| b == "Save anyway");
        click(&mut s.harness, if shadowed { "Save anyway" } else { "Save" });
        assert_matches!(
            &s.state.borrow().0, SettingsEditState::Saving { key: k } if k == key,
            "row {key}: one click must take the save: {:?}",
            s.state.borrow().0
        );
        let written = s.written.borrow().as_ref().map(|r| r.key.clone());
        assert_eq!(written.as_deref(), Some(key.as_str()), "row {key}: the out-slot holds it");
    }
}

/// ⚠ **An UNARMED backend's write channel is named as the blocker BEFORE the click**, and the
/// sentence also names the node-side gate this process cannot see. Reddens on an editor that
/// offers Save against an observe-only record and lets a transport refusal be the first word on
/// the subject.
#[test]
fn an_unarmed_backend_says_its_write_channel_cannot_sign_the_write() {
    let mut armed = settings_harness_armed(
        BackendSettingsState::Loaded(show()),
        SettingsFilter::All,
        true,
        1000.0,
    );
    armed.harness.run();
    open_row(&mut armed, 0);
    assert!(
        !tree_text(&armed.harness).contains("write channel is NOT armed"),
        "an armed record gets no such line"
    );

    let mut unarmed = settings_harness_armed(
        BackendSettingsState::Loaded(show()),
        SettingsFilter::All,
        false,
        1000.0,
    );
    unarmed.harness.run();
    open_row(&mut unarmed, 0);
    let text = tree_text(&unarmed.harness);
    assert!(text.contains("write channel is NOT armed"), "{text}");
    assert!(text.contains("observe key cannot sign it"), "{text}");
    assert!(
        text.contains("flags.tradehub_control"),
        "…and the gate this side cannot see is named rather than assumed away: {text}"
    );
}

/// ⚠ The table survives ~400pt: the sticky header and its rows shrink TOGETHER, so the labels
/// stay registered with their columns, and every key still reaches the tree. Reddens on a header
/// computed independently of the rows — the failure mode a sticky header drawn outside the scroll
/// area invites.
#[test]
fn the_settings_table_survives_a_four_hundred_point_window() {
    let mut s = settings_harness_armed(
        BackendSettingsState::Loaded(show()),
        SettingsFilter::All,
        true,
        400.0,
    );
    s.harness.run();
    let text = tree_text(&s.harness);
    for header in ["KEY", "VALUE", "ORIGIN", "READ"] {
        assert!(text.contains(header), "{header} must survive a narrow window: {text}");
    }
    let edits = nodes(&s.harness, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some("edit")
    });
    assert_eq!(edits.len(), 5, "every row is still editable at 400pt");
}

/// Every non-`Loaded` state renders its own sentence rather than an ellipsis that outlives the
/// fault. ⚠ Five states, not three: `Unsupported` and `Error` are reachable on day one.
#[test]
fn every_fetch_state_renders_its_own_sentence() {
    for (state, needle) in [
        // ⚠ Idle and Pending render DIFFERENT sentences — "not asked yet" is not "asking".
        // Collapsing them is what let a tab-conditional auto-fetch pass for a working one.
        (BackendSettingsState::Idle, "have not been read yet"),
        (BackendSettingsState::Pending, "reading the node's settings"),
        (BackendSettingsState::Unsupported, "server predates settings-show"),
        (BackendSettingsState::Error("bad mac".into()), "bad mac"),
    ] {
        let mut s = settings_harness(state.clone(), SettingsFilter::Set);
        s.harness.run();
        let text = tree_text(&s.harness);
        assert!(text.contains(needle), "{state:?} must render {needle:?}: {text}");
        // ⚠ And the FILTER is absent, because it would carry two counts of a row set that has not
        // arrived. `set · 0 / all · 0` is two invented numbers where two measured ones go.
        assert!(!text.contains("set · "), "{state:?} must render no filter counts: {text}");
        assert!(!text.contains("all · "), "{state:?} must render no filter counts: {text}");
    }
}

/// ⚠ **THE SECTION NO LONGER DRIVES THE FETCH, AND THE TOOL DOES.** `should_fetch_settings` is
/// called by `data_body`, before the per-destination dispatch (see
/// `crates/vike-app-core/src/ui/tool_views/data.rs`), never by this section body — a section that
/// set its own auto-fetch again would restore the coupling this window's design removed (staying
/// on the default Credentials destination left the settings slot `Idle` forever before the fix).
#[test]
fn the_settings_section_sets_no_auto_fetch_and_only_refresh_does() {
    assert!(
        vike_app_core::ui::tool_views::should_fetch_settings(true, &BackendSettingsState::Idle),
        "the predicate itself is unchanged — an idle ACTIVE backend is still a fetch"
    );

    let refreshed = std::rc::Rc::new(std::cell::Cell::new(false));
    let inner = refreshed.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        let mut refresh = false;
        let mut edit = SettingsEditState::Idle;
        let mut write: Option<SettingsWriteRequest> = None;
        let mut filter = SettingsFilter::Set;
        backend_settings_section(
            ui,
            Some("prod"),
            true,
            &BackendSettingsState::Idle,
            &mut refresh,
            &mut edit,
            &mut write,
            &mut filter,
        );
        if refresh {
            inner.set(true);
        }
    });
    h.run();
    assert!(
        !refreshed.get(),
        "drawing the section must NOT request a fetch — the tool does that, before the \
         destination dispatch, so every destination that renders it gets one too"
    );

    click(&mut h, "Refresh");
    assert!(refreshed.get(), "…and the explicit Refresh click still does, in any state");
}

/// ⚠⚠ **A triangle that does not disclose is a control lying about being one.** Three halves: the
/// title reaches the screen, the filter and Refresh sit on the SAME ROW as it (a geometry
/// question, so it is asked with rects), and toggling it actually hides the table.
#[test]
fn the_backend_settings_section_has_a_titled_collapsible_header() {
    let title_text = vike_app_core::ui::tool_views::SECTION_TITLE;
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();

    let title = text_rects(&s.harness, title_text);
    assert!(!title.is_empty(), "the section header reaches the screen: {}", tree_text(&s.harness));
    let row = title[0];
    for label in ["all · 5", "Refresh"] {
        let r = text_rects(&s.harness, label);
        assert!(!r.is_empty(), "{label:?} is on screen: {}", tree_text(&s.harness));
        assert!(
            (r[0].center().y - row.center().y).abs() < row.height(),
            "{label:?} sits on the header's OWN row — that is what the design asks for, and it is \
             what makes the header a header rather than a caption: {:?} vs {row:?}",
            r[0]
        );
    }

    // …and it really discloses. The TITLE is the toggle a test can name — egui's own triangle is an
    // `Ui::interact` with no `WidgetInfo` and reaches the tree unlabelled, which is why
    // `backend_settings_section` makes the title clickable as well.
    assert!(tree_text(&s.harness).contains("config.tradehub_addr"), "the table starts open");
    click(&mut s.harness, title_text);
    s.harness.run();
    assert!(
        !tree_text(&s.harness).contains("config.tradehub_addr"),
        "collapsing the section hides the table it names: {}",
        tree_text(&s.harness)
    );
    click(&mut s.harness, title_text);
    s.harness.run();
    assert!(
        tree_text(&s.harness).contains("config.tradehub_addr"),
        "…and opening it brings the table back: {}",
        tree_text(&s.harness)
    );
}

/// ⚠ The editor opens INSIDE the table's scroll area (`max_height(300.0)`), beneath its own row. On
/// a row near the bottom of a long table it used to open past the area's lower edge, so the
/// operator saw a panel cut off mid-line and NO Save button — measured on the live Backend tab
/// (2026-09-28), where the ceiling row sits eleventh of eleven. Opening an editor must bring the
/// whole panel into view.
///
/// The row opened is the LOWEST one whose `edit` is visible without scrolling — the live case: a
/// row further down cannot be clicked until the operator scrolls to it, so it is not the question.
#[test]
fn an_editor_opened_on_the_lowest_visible_row_of_a_long_table_is_fully_visible() {
    let mut long = show();
    long.rows =
        (0..20).map(|i| row(&format!("config.key_{i:02}"), "config", "db", "tradehub")).collect();
    let mut s = settings_harness(BackendSettingsState::Loaded(long), SettingsFilter::All);
    // The build frame binds the app's type and draws nothing (`vike_ui_theme::harness`).
    s.harness.run();

    // The table's column header sits directly above the scroll area and never scrolls, so the
    // visible band is [its bottom, its bottom + the area's 300pt] (plus the separator's few pt).
    // egui files a Label as two nodes carrying one rect (see `texts_within`), so the header is one
    // RECT, possibly twice.
    let header = text_rects(&s.harness, "KEY");
    assert!(
        !header.is_empty() && header.iter().all(|r| *r == header[0]),
        "one KEY column header: {header:?}"
    );
    let (top, bottom) = (header[0].bottom(), header[0].bottom() + 300.0 + 12.0);

    let edits: Vec<egui::Rect> = nodes(&s.harness, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some("edit")
    })
    .iter()
    .filter_map(node_rect)
    .collect();
    let lowest_visible = edits
        .iter()
        .rposition(|r| r.bottom() <= bottom)
        .expect("at least one row is visible without scrolling");
    assert!(lowest_visible + 1 < edits.len(), "the table must be longer than its viewport");
    open_row(&mut s, lowest_visible);
    s.harness.run();

    let save: Vec<egui::Rect> = nodes(&s.harness, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some("Save")
    })
    .iter()
    .filter_map(node_rect)
    .collect();
    assert_eq!(save.len(), 1, "the open editor offers one Save: {}", tree_text(&s.harness));
    assert!(
        save[0].top() >= top && save[0].bottom() <= bottom,
        "Save at {:?} is outside the visible band {top}..{bottom}: the editor opened cut off",
        save[0]
    );
}
