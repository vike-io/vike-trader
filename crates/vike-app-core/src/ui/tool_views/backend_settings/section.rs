//! The section body: the collapsible header, the summary paragraph and the fetch-state arms.

use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::TextRole;

use super::edit::{SettingsEditState, SettingsWriteRequest};
use super::rows::{BackendDigest, SettingsFilter, row_is_set};
use super::table::settings_table;
use super::{BackendSettingsState, PREDATES_SETTINGS_SHOW};

#[cfg(doc)]
use super::{edit::settings_write_state, edit::take_save, should_fetch_settings};

/// The section body — the Backends picker's sibling on Data Manager's `DataDest::Backend`
/// destination. Renders the ACTIVE backend's state under a `▸ BACKEND SETTINGS` disclosure header
/// that carries the filter segment and Refresh on its own row ([`SECTION_TITLE`]); `refresh` is
/// the fetch out-slot the binary drains after the frame (set ⇒ start a fetch). ⚠ **The AUTO-fetch
/// is no longer set here** — it moved up to `super::data`'s `data_body`.
///
/// ⚠ **This said the move was because "this body is drawn on only one of two destinations while
/// the digest it feeds is drawn on both" — true of the OLD standalone Connections window (whose
/// status line and Backend-tab badge, both deleted with it, digested this body's answer on either
/// tab), false of Data Manager today.** [`BackendDigest`] feeds only THIS section's own collapsing
/// header, and this section is reached only through `backend_tab`, which only `DataDest::Backend`
/// calls — `crates/vike-app-core/src/ui/tool_views/connections.rs`'s module doc says so outright.
/// The move up to `data_body` survives for a different, still-real reason: `data_body` gates the
/// fetch trigger on `Credentials | Backend` so the fetch is already warm by the time an operator
/// switches from the rail-adjacent Credentials destination over to Backend, never on
/// `DataDest::Backend` alone, and this section's OWN draw is conditional on that one destination
/// — so a trigger set only when this body runs would arm one frame too late for that pre-warming to
/// do anything. [`should_fetch_settings`]'s own doc carries the regression that made the move
/// necessary and is where the historical claim is argued in full. What this body still sets is the
/// EXPLICIT Refresh click, whatever the state, which belongs to the button that was clicked.
///
/// The WRITE half: `edit` is the per-row edit flow (UI-thread-owned — its text buffers are
/// live), `write` the out-slot an accepted Save fills ([`take_save`]); the binary drains it
/// after the frame, runs the wire write, and folds the outcome back with
/// [`settings_write_state`]. Every decision is a pure function above — this body only renders.
pub fn backend_settings_section(
    ui: &mut egui::Ui,
    active_backend: Option<&str>,
    control_armed: bool,
    state: &BackendSettingsState,
    refresh: &mut bool,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
    filter: &mut SettingsFilter,
) {
    let t = Tokens::of(ui.ctx());
    let Some(name) = active_backend else {
        ui.label(
            egui::RichText::new(
                "no backend connected — the strip at the foot of this window is where one is \
                 connected, and its settings can only be read over that connection",
            )
            .monospace()
            .size(role_px(ui.ctx(), TextRole::Body))
            .weak(),
        );
        return;
    };
    // ------------------------------------------------------------------------------------------
    // ⚠ THE SECTION HEADER — the design's `▸ BACKEND SETTINGS`, with the filter segment and
    // Refresh seated on the SAME row.
    //
    // It was ABSENT: this body went straight from the summary paragraph into a bare
    // `ui.horizontal` holding the two filter segments and Refresh, and nothing on screen said
    // where one section ended and the next began. On the Backend tab that matters more than it
    // sounds — the registry rows, their editor and this table are three stacked blocks, and the
    // only thing separating the last two was a `ui.separator()`.
    //
    // A REAL `CollapsingState`, not a triangle that does nothing: a disclosure glyph that does not
    // disclose is a control lying about being one, which is this window's whole theme. It is
    // id-keyed like every other `CollapsingState`, so the open/closed bit is discarded once on an
    // upgrade that changes the id — the module doc of `super::connections` records that trade;
    // `default_open = true` means the discard restores exactly what ships.
    // ------------------------------------------------------------------------------------------
    let section_id = ui.make_persistent_id(SECTION_ID_SALT);
    let collapsing = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        section_id,
        true,
    );
    let mut toggle = false;
    let header = collapsing.show_header(ui, |ui| {
        ui.spacing_mut().item_spacing.x = space::MD;
        // ⚠ The TITLE toggles too, not only the triangle egui draws to its left. Two reasons, and
        // the second is the one that made it a rule here: a section title that looks like a header
        // and is inert while the 12pt glyph beside it is not is a smaller version of this window's
        // whole defect; and egui's own disclosure control is an `Ui::interact` over a painted
        // triangle with no `WidgetInfo`, so it reaches the accessibility tree as an unlabelled node
        // that a headless test cannot name — a control nothing can gate is a control that will rot.
        // `crates/vike-app-core/tests/backend_settings_section_rendering.rs`'s
        // `the_backend_settings_section_has_a_titled_collapsible_header` clicks THIS.
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(SECTION_TITLE)
                        .monospace()
                        .strong()
                        .size(role_px(ui.ctx(), TextRole::Body))
                        .color(t.theme.text2),
                )
                .frame(false),
            )
            .on_hover_text("show or hide the node's effective settings")
            .clicked()
        {
            toggle = true;
        }
        // ⚠ The filter renders ONLY against rows the node actually sent. Before the answer
        // arrives there is no row to count, and a `set · 0 / all · 0` control would be two
        // invented numbers sitting where two measured ones go — the same fabrication the
        // Backend badge refuses.
        if let BackendSettingsState::Loaded(show) = state {
            ui.add_space(space::MD);
            let n_set = show.rows.iter().filter(|r| row_is_set(r)).count();
            filter_segment(ui, filter, SettingsFilter::Set, &format!("set · {n_set}"));
            filter_segment(ui, filter, SettingsFilter::All, &format!("all · {}", show.rows.len()));
        }
        ui.add_space(space::MD);
        if ui.small_button("Refresh").clicked() {
            *refresh = true;
        }
    });
    let mut header = header;
    if toggle {
        header.toggle();
    }
    let shown = *filter;
    header.body_unindented(|ui| {
        settings_section_body(ui, name, control_armed, state, edit, write, shown);
    });
}

/// The section header's title, and the id salt its open/closed bit is keyed on. Named because the
/// header is drawn in one place and asserted in another, and a title spelled twice is a title that
/// can disagree with itself.
pub const SECTION_TITLE: &str = "BACKEND SETTINGS";
const SECTION_ID_SALT: &str = "vike_backend_settings_section";

/// Everything UNDER the section header: the dense summary paragraph, then whichever of the fetch
/// states the node is in. Split out of [`backend_settings_section`] only so the collapsing body
/// closure stays readable — no behaviour of its own.
fn settings_section_body(
    ui: &mut egui::Ui,
    name: &str,
    control_armed: bool,
    state: &BackendSettingsState,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
    filter: SettingsFilter,
) {
    // ------------------------------------------------------------------------------------------
    // The dense summary readout — ONE wrapped paragraph, not a stack of lines.
    // ------------------------------------------------------------------------------------------
    let digest = BackendDigest::of(true, state);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        ui.label(
            egui::RichText::new("effective settings of")
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Body))
                .weak(),
        );
        ui.label(
            egui::RichText::new(name).monospace().strong().size(role_px(ui.ctx(), TextRole::Body)),
        );
        match &digest {
            BackendDigest::Counts { keys, set, findings, first_finding, settings_dir } => {
                let defaulted = keys.saturating_sub(*set);
                ui.label(
                    egui::RichText::new(format!(
                        "· {keys} keys · {set} set from env or file · {defaulted} at their \
                         compiled-in default ·"
                    ))
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .weak(),
                );
                if *findings > 0 {
                    let text = match first_finding {
                        Some(k) if *findings == 1 => format!("set and read by nothing: {k} ·"),
                        Some(k) => format!("{findings} set and read by nothing, incl. {k} ·"),
                        None => format!("{findings} set and read by nothing ·"),
                    };
                    ui.label(
                        icons::WARNING.before(
                            ui.style(),
                            egui::RichText::new(text)
                                .monospace()
                                .size(role_px(ui.ctx(), TextRole::Body))
                                .color(ui.visuals().warn_fg_color),
                        ),
                    );
                }
                ui.label(
                    egui::RichText::new(
                        "edits are restart-to-apply unless the node says otherwise ·",
                    )
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .weak(),
                );
                match settings_dir {
                    Some(dir) => {
                        ui.label(
                            egui::RichText::new("settings dir")
                                .monospace()
                                .size(role_px(ui.ctx(), TextRole::Body))
                                .weak(),
                        );
                        ui.label(
                            egui::RichText::new(dir.as_str())
                                .monospace()
                                .size(role_px(ui.ctx(), TextRole::Body))
                                .weak(),
                        );
                    }
                    None => {
                        ui.label(
                            egui::RichText::new(
                                "no settings directory — this node resolved none at boot, runs on \
                                 compiled-in defaults, and has no settings database any write \
                                 could reach",
                            )
                            .monospace()
                            .size(role_px(ui.ctx(), TextRole::Body))
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                }
            }
            other => {
                let color = match other {
                    BackendDigest::Error(_) => ui.visuals().warn_fg_color,
                    _ => ui.visuals().weak_text_color(),
                };
                // Never a finding here: only `Counts` has one, and it is the arm above. The line
                // is drawn as text, so a finding would need `icons::WARNING` before it.
                ui.label(
                    egui::RichText::new(other.line().replace("Backend settings  ", "· "))
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Body))
                        .color(color),
                );
            }
        }
    });

    ui.add_space(space::SM);

    // ⚠ The filter + Refresh row that used to sit HERE is now on the section header's own row —
    // see [`backend_settings_section`]. It moved rather than being duplicated: there is exactly
    // one `filter_segment` pair and one Refresh button in this file.
    match state {
        // ⚠ Two sentences, not one. `Idle` is *nothing has asked yet* and `Pending` is *a fetch is
        // in flight*, and the same collapse in the digest is what let the tab-conditional
        // auto-fetch hide for a whole design — see [`should_fetch_settings`]. In production this
        // arm is a single frame (`data_body` requests the fetch before the per-destination
        // dispatch); if it persists, the driver is not running and this says so instead of
        // animating an ellipsis for ever.
        BackendSettingsState::Idle => {
            ui.label(
                egui::RichText::new("the node's settings have not been read yet")
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .weak(),
            );
        }
        BackendSettingsState::Pending => {
            ui.label(
                egui::RichText::new("reading the node's settings…")
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .weak(),
            );
        }
        BackendSettingsState::Unsupported => {
            ui.label(
                egui::RichText::new(PREDATES_SETTINGS_SHOW)
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .weak(),
            );
        }
        BackendSettingsState::Error(e) => {
            // ⚠ WRAPPED, not one line. The daemon's own refusals are paragraphs — the
            // `SettingsWriteError::Lock` text alone is ~600 characters and leads with the
            // namespace question, because `ls -ld` from an ordinary shell answers it wrongly.
            // Rendering it unwrapped is rendering it unread.
            ui.label(
                egui::RichText::new(format!("settings unavailable — {e}"))
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .color(ui.visuals().warn_fg_color),
            );
        }
        BackendSettingsState::Loaded(show) => {
            settings_table(ui, show, filter, control_armed, edit, write);
        }
    }
}

/// One segment of the `set · N` / `all · N` control. The SELECTED one is a label, not a button —
/// the same tree-readable-selection idiom the venue rail and the account strip use.
fn filter_segment(
    ui: &mut egui::Ui,
    filter: &mut SettingsFilter,
    value: SettingsFilter,
    text: &str,
) {
    let t = Tokens::of(ui.ctx());
    if *filter == value {
        // The chosen segment is still a LABEL (it stays readable as the selection), in the text
        // ink; the accent is its OUTLINE — a shape, never the colour of a word. The frame's
        // margin is the sibling `small_button`'s own (the theme's horizontal button padding, no
        // vertical padding) less the stroke the frame adds, so the two segments keep one width.
        let pad_x = ui.spacing().button_padding.x - stroke::HAIRLINE;
        egui::Frame::new()
            .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.accent))
            .corner_radius(egui::CornerRadius::same(RADIUS))
            .inner_margin(egui::Margin::symmetric(pad_x.round() as i8, 0))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(text)
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Caption))
                        .strong()
                        .color(t.theme.text),
                );
            });
    } else if ui
        .small_button(
            egui::RichText::new(text).monospace().size(role_px(ui.ctx(), TextRole::Caption)),
        )
        .clicked()
    {
        *filter = value;
    }
}
