//! The edit flow's panel, rendered beneath the row it edits.

use vike_tradehub_client::wire::WireSettingsRow;
use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::backend_settings;

use super::edit::{SAVED_RESTART_NOTE, SettingsEditState, SettingsWriteRequest, take_save};
use super::rows::{env_shadow, write_target};

/// The edit flow's own panel, rendered beneath the row it edits, inside the table's scroll area,
/// whenever that row is being edited (or a write is in flight / just finished). Renders
/// [`SettingsEditState`]; every decision (may this save? what does the outcome mean?) is a pure
/// function above. Returns the panel's response so the section can scroll it into view.
pub(super) fn settings_edit_panel(
    ui: &mut egui::Ui,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
    row: &WireSettingsRow,
    settings_dir: Option<&str>,
    control_armed: bool,
) -> egui::Response {
    let t = Tokens::of(ui.ctx());
    // Clicks are staged and applied AFTER the match: inside an arm the state is mutably borrowed
    // for its live text buffers, so the transitions ([`take_save`], the reset to `Idle`) run once
    // that borrow ends — the same deferred-mutation shape the section's own out-slots use.
    let mut do_save = false;
    let mut dismiss = false;
    let shadow = env_shadow(row).map(str::to_string);
    let target = write_target(settings_dir);
    let panel = egui::Frame::new()
        .fill(t.theme.card)
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .inner_margin(egui::Margin::symmetric(space::LG as i8, space::MD as i8))
        .show(ui, |ui| match edit {
            SettingsEditState::Idle => {}
            SettingsEditState::Editing { file: _, key, value } => {
                // EFFECTIVE NOW, with its origin — the value this row actually resolves to today
                // and the layer that set it. Rendered before the control, because "what am I
                // changing from" is the first thing the operator needs and the table row it
                // expanded under scrolls.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = space::SM;
                    ui.label(
                        egui::RichText::new("Effective now")
                            .monospace()
                            .size(role_px(ui.ctx(), TextRole::Caption))
                            .color(t.theme.text3),
                    );
                    ui.label(
                        egui::RichText::new(if row.value.is_empty() {
                            "-"
                        } else {
                            row.value.as_str()
                        })
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Body))
                        .strong(),
                    );
                    ui.label(
                        egui::RichText::new(format!("from {}", row.origin))
                            .monospace()
                            .size(role_px(ui.ctx(), TextRole::Caption))
                            .weak(),
                    );
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::SM;
                    ui.label(
                        egui::RichText::new("New value")
                            .monospace()
                            .size(role_px(ui.ctx(), TextRole::Caption)),
                    );
                    ui.add(
                        egui::TextEdit::singleline(value)
                            .desired_width(backend_settings::EDIT_FIELD_W),
                    );
                });
                // ⚠ No confirm line, for any key — the policy ceilings included (0086 point 7; the
                // tombstone above `SettingsWriteRequest` names what stood here).

                // ⚠⚠ THE WARNING THIS WHOLE PANEL EXISTS FOR. When the ENVIRONMENT sets this key,
                // writing the row changes nothing the running daemon will ever read: the write is
                // ACCEPTED, `restart_required: true` comes back, and the refetch shows the row
                // still reading `env:VAR` with the old value — which looks exactly like a failure.
                // Nothing on the wire, in `apply_set_setting`, or in this panel said so before.
                if let Some(var) = &shadow {
                    ui.add_space(space::SM);
                    let words = egui::RichText::new(format!(
                        "the ENVIRONMENT sets this key, and env outranks the settings \
                             database. Saving this row changes nothing until {var} is unset on \
                             the daemon's box and it restarts — this row will still read \
                             env:{var} with the old value after the write. Unset the variable \
                             there first."
                    ))
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .color(ui.visuals().warn_fg_color);
                    ui.label(icons::WARNING.before(ui.style(), words));
                }

                // ⚠ A write is a CONTROL action and the observe key cannot sign it. This backend
                // record's own arming gate (`BackendRecord::control` — the B9 per-backend gate
                // `backend_registry::resolve_keys` enforces) is knowable BEFORE the click, so an
                // unarmed record says so here rather than answering with a transport refusal.
                // ⚠ It is not the only gate: `flags.tradehub_control` must also be true ON THE
                // NODE, or every `Request::Command` answers "control not enabled on this node" —
                // and this side cannot see that flag, so the sentence names it rather than
                // claiming an armed record is sufficient.
                if !control_armed {
                    ui.add_space(space::SM);
                    let words = egui::RichText::new(
                        "this backend's write channel is NOT armed — a settings write is a \
                             CONTROL action and the observe key cannot sign it. Arm the record \
                             (Edit it above: a named control key plus `control armed`). The node \
                             must ALSO have flags.tradehub_control = true, which this side cannot \
                             see from here.",
                    )
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .color(ui.visuals().warn_fg_color);
                    ui.label(icons::WARNING.before(ui.style(), words));
                }

                // What will be written, and where — exactly, before the click.
                ui.add_space(space::SM);
                match &target {
                    Some(path) => ui.label(
                        egui::RichText::new(format!(
                            "writes {} = {} as one row of {path} on the backend · takes effect \
                             on restart",
                            key,
                            if value.is_empty() { "\"\"" } else { value.as_str() }
                        ))
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Caption))
                        .weak(),
                    ),
                    // The read half reported no settings directory, which is the same condition
                    // the write half refuses with "this node resolved no settings directory at
                    // boot". Say it before the click rather than after it.
                    None => ui.label(
                        egui::RichText::new(
                            "this node resolved NO settings directory at boot — there is no \
                             settings database to write and the daemon will refuse this write \
                             outright",
                        )
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Caption))
                        .color(ui.visuals().warn_fg_color),
                    ),
                };

                ui.add_space(space::SM);
                ui.horizontal(|ui| {
                    // ⚠ The LABEL is honest about the shadow: "Save anyway" is what the button
                    // does when the environment outranks the row, and a plain "Save" there would
                    // be the panel claiming an effect it cannot deliver. It read "Save to file" /
                    // "Save to file anyway" until 0086 was applied here: the hazard was right, the
                    // FILE never was. Always ENABLED — no key demands a typed confirm.
                    let label = if shadow.is_some() { "Save anyway" } else { "Save" };
                    if ui.button(label).clicked() {
                        do_save = true;
                    }
                    if ui.small_button("Cancel").clicked() {
                        dismiss = true;
                    }
                });
            }
            SettingsEditState::Saving { key } => {
                ui.label(
                    egui::RichText::new(format!("saving {key}…"))
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Caption))
                        .weak(),
                );
            }
            SettingsEditState::Saved { key, restart_required } => {
                let note = if *restart_required {
                    format!("{key}: {SAVED_RESTART_NOTE}")
                } else {
                    format!("{key}: saved and applied — no restart needed")
                };
                ui.label(
                    egui::RichText::new(note)
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Caption))
                        .color(ui.visuals().strong_text_color()),
                );
                // ⚠ An ACCEPTED write to an env-shadowed key still changes nothing the running
                // process reads. The acceptance is about the ROW; say what it is not about.
                if let Some(var) = &shadow {
                    ui.label(
                        egui::RichText::new(format!(
                            "the row was written — the EFFECTIVE value is still {var}'s until \
                             that variable is unset on the daemon's box",
                        ))
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Caption))
                        .color(ui.visuals().warn_fg_color),
                    );
                }
                if ui.small_button("OK").clicked() {
                    dismiss = true;
                }
            }
            SettingsEditState::Failed { key, error } => {
                // ⚠ WRAPPED. The daemon's refusals arrive as its own sentences — the loader's
                // message, verbatim — not as codes. One unwrapped line is one unread line.
                ui.label(
                    egui::RichText::new(format!("{key}: write refused — {error}"))
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Caption))
                        .color(ui.visuals().warn_fg_color),
                );
                if ui.small_button("Dismiss").clicked() {
                    dismiss = true;
                }
            }
        });
    if do_save {
        // `take_save` re-checks the gate and transitions to `Saving`; the binary drains the
        // request after the frame.
        if let Some(req) = take_save(edit) {
            *write = Some(req);
        }
    } else if dismiss {
        *edit = SettingsEditState::Idle;
    }
    panel.response
}
