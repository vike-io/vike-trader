//! The backend-connection plane: the registry rows and their add/edit/delete flow
//! ([`backend_tab`], called directly from `windows::data`'s `data_body` on
//! `DataManagerDestination::Backend`), the ambient identity/Disconnect/Add-backend row that sits above it
//! ([`strip_row`], called on BOTH `DataManagerDestination::Gateway` and `DataManagerDestination::Backend`), and the
//! connected node's effective settings editor ([`settings::backend_settings_section`],
//! reached through [`backend_tab`]).
//!
//! ⚠ **This file used to own the WHOLE standalone Connections window** — two tabs seated in its
//! own title bar, one status line, this same row pinned to its foot — until
//! Connections-merges-into-Data-Manager retargeted every one of its call sites onto Data Manager's
//! `DataManagerDestination::Gateway`/`Backend` destinations and deleted everything that existed only to let
//! the standalone window compose them: `ConnectionsTab`, `connections_tool_content`,
//! `connections_body`, `BodyLayout`, the title-bar-tab chrome (`title_bar_tabs`/`tab_bar`/
//! `segment`/`TabRow`/`hairline_segments`), the one status line (`status_line`/`credentials_line`),
//! `ambient_strip` itself, and the whole foot-strip reservation feedback loop
//! (`strip_reservation`/`strip_height_id`/`settled_strip_height`/`remembered_strip_height`) —
//! because `strip_row` is now called directly from `data_body`, gated on
//! `matches!(tv.data_destination, DataManagerDestination::Gateway | DataManagerDestination::Backend)`, the exact shape
//! `should_fetch_settings`'s own call one line above it already uses:
//!
//! * The title-bar-tab chrome existed so TWO always-coexisting tabs of one window could be told
//!   apart; Data Manager tells its (thirteen, not two) destinations apart with its own rail
//!   (`data_manager::rail::rail`) instead, so there is no segmented control left for it to seat.
//! * The status line / `credentials_line` existed to show the INACTIVE tab's digest beside the
//!   active one's — a concept that needs exactly two mutually-exclusive panes. Data Manager has
//!   no such pairing (Gateway and Backend are two of thirteen independent destinations, never
//!   shown together), so there is nothing for it to digest.
//! * `connections_body`'s foot-strip reservation protected [`strip_row`] from the window's own
//!   floor (a fixed-size `egui::Window` that could not grow past the arena edge). `data_body`
//!   draws [`strip_row`] in the ORDINARY flow, like any other widget — nothing downstream measures
//!   its height back into a layout decision, so there is no floor to protect it from and no
//!   feedback loop to close.
//!
//! [`strip_row`]'s own signature changed by exactly one parameter: what used to flip
//! `ConnectionsTab` to `Backend` on an **Add backend** click now flips `tv.data_destination` to
//! [`super::DataManagerDestination::Backend`] instead — the two enums' roles in a caller's state were already
//! identical, so nothing else about the function's body, or the identity/Disconnect rendering
//! above the buttons, needed to change.
//!
//! # Where every number comes from
//!
//! ⚠ **There is no Backend badge on the rail, and this used to claim there was.**
//! `data_manager::rail::DataManagerRailCounts::count` returns `None` for both `DataManagerDestination::Gateway` and
//! `DataManagerDestination::Backend` outright — its own comment gives the reason ("neither has a natural single
//! count — a per-venue presence grid and a single box's connection state"). What
//! [`settings::BackendSettingsDigest`] actually feeds is the dense summary paragraph INSIDE
//! `backend_settings_section`'s own collapsible header (`settings/mod.rs`): the `Counts`
//! variant renders its fields directly ("N keys · N set from env or file · …"), every other
//! variant through `BackendSettingsDigest::line()`. That section is reached only through [`backend_tab`],
//! which this file still owns.
//!
//! ⚠ [`backend_tab`] rendered `crate::backend::connection::picker_rows` until the redesign that
//! put [`strip_row`] at the foot of the old window — whose unlisted head row restated the strip's
//! dot, name, ADDRESS, `control armed`, `(not in registry)` and `Disconnect`, on a tab the strip
//! was ALSO drawn on. It reads `registry_rows` instead, and
//! `crates/vike-app-core/tests/screens/data_manager_backend_strip.rs`'s
//! `the_live_connection_is_rendered_once_across_the_strip_and_the_backend_destination` counts the
//! DISTINCT POSITIONS the address is laid out at, over the real [`strip_row`] + [`backend_tab`]
//! pair rather than a stub.
//!
//! [`strip_row`] and [`backend_tab`] are `pub` because `data_body` calls them directly now — the
//! same reason every other function in this module's public surface is `pub`, now that there is no
//! `WindowToolContext`-consuming wrapper left to hide behind.

pub(super) mod provider_keys;
pub(super) mod settings;

use super::DataManagerDestination;
use crate::backend::connection::{BackendAction, active_is_unlisted, click_action, registry_rows};
use crate::backend::editor::{self, BackendEditorState, BackendForm, BackendFormError};
use crate::backend::identity;
use crate::backend::registry::BackendRegistry;
use settings::{
    BackendSettingsEditState, BackendSettingsFilter, BackendSettingsState,
    BackendSettingsWriteRequest, backend_settings_section,
};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, role_px};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::connections;

/// The backend picker's read-only inputs (split-plane B1), grouped the way [`super::WindowToolContext`]
/// groups the grid's: the loaded registry and the ACTIVE connection's record (registry entry or the
/// `--observe` synthetic one).
///
/// ⚠ It carried an `available` flag until 2026-10 — B2's fence, `false` while a local core ran,
/// which made the Connect/Disconnect buttons inert and painted "a local trading core is running".
/// The desktop has no local core, its one caller passed `true`, and the flag went with the GUI's
/// other always-constant leftovers: switching is always available.
pub struct BackendPickerSources<'a> {
    /// The loaded `backends.json` registry.
    pub backends: &'a BackendRegistry,
    /// The live connection's record, if any.
    pub active: Option<&'a crate::backend::registry::BackendRecord>,
    /// WHICH BOX the connected daemon says it is running on, when it says anything — the one
    /// identity fact this side cannot derive, since a tunnelled client's own socket address is
    /// always the tunnel mouth. `None` from a daemon that reported none. See
    /// [`crate::backend::identity::BackendIdentityReport`].
    pub reported: Option<identity::BackendIdentityReport<'a>>,
}

/// The point size of [`strip_row`]'s BUTTONS — the tallest thing in its row, and therefore the one
/// [`strip_actions_w`] has to lay out to know how much width they need.
const STRIP_TEXT: ThemeTextRole = ThemeTextRole::Body;

/// The width [`strip_row`]'s action buttons will occupy — `Add backend` always, `Disconnect` too
/// when a backend is attached — so the row can pad up to the right edge instead of drawing them
/// wherever the preceding text happened to stop.
///
/// Measured rather than pinned: a `Button`'s width is its galley plus `button_padding.x` on each
/// side, and the shipped app sets its OWN `button_padding` (`vike_ui_theme::appearance::install`
/// uses `(8, 4)` at the default density, not egui's `(4, 1)`), so a constant here would be wrong
/// in the one build that matters.
fn strip_actions_w(ui: &egui::Ui, with_disconnect: bool) -> f32 {
    let (gap, pad_x) = {
        let sp = ui.spacing();
        (sp.item_spacing.x, sp.button_padding.x)
    };
    let mut labels: Vec<&str> = Vec::with_capacity(2);
    if with_disconnect {
        labels.push("Disconnect");
    }
    labels.push("Add backend");
    let n = labels.len() as f32;
    let text: f32 = labels
        .into_iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap(
                    l.to_owned(),
                    egui::FontId::proportional(role_px(ui.ctx(), STRIP_TEXT)),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
        })
        .sum();
    // One inter-item gap BETWEEN the buttons, plus the gap that precedes the first one — the pad
    // is inserted as its own item, so the layout puts a gap on each side of it.
    text + n * 2.0 * pad_x + n * gap
}

// -------------------------------------------------------------------------------------------
// DataManagerDestination::Backend
// -------------------------------------------------------------------------------------------

/// Draw a backend's IDENTITY — headline first, address second — the same way on every surface.
///
/// [`crate::backend::identity`] decides WHAT each piece says and this decides how it
/// looks, once, so the picker rows and [`strip_row`] cannot drift apart. An unnamed backend's
/// headline is italic and dimmed: it is this module's stand-in for a name rather than a name, and
/// it must not read as one the operator chose.
fn identity_labels(ui: &mut egui::Ui, id: &identity::BackendIdentity<'_>) {
    let t = ThemeTokens::of(ui.ctx());
    let headline = egui::RichText::new(id.headline)
        .monospace()
        .size(role_px(ui.ctx(), ThemeTextRole::Body))
        .strong();
    let headline =
        if id.named { headline } else { headline.italics().color(t.theme.text_secondary) };
    let response = ui.label(headline);
    if let Some(hover) = id.headline_hover() {
        response.on_hover_text(hover);
    }
    // THE ADDRESS THAT NAMES THE BOX. A daemon's own report is worth reading, so it is NOT dimmed;
    // a dial address is the tunnel mouth, identical on every box, and stays secondary. Which of the
    // two this is — and where the other one went — is `backend::identity`'s decision, once, so the
    // strip, the rows and the status bar cannot disagree.
    let address = egui::RichText::new(id.box_address())
        .monospace()
        .size(role_px(ui.ctx(), ThemeTextRole::Body));
    let address =
        if id.box_address_is_reported() { address } else { address.color(t.theme.text_secondary) };
    ui.label(address).on_hover_text(id.box_address_hover());
}

/// The identity's NOTE, drawn last so a row reads name → address → control state → note.
///
/// ⚠ An INVITATION is not dimmed. `(not in registry)` used to be a `.weak()` footnote, which is
/// exactly how an operator reads past the one line telling them nothing here knows which box they
/// are attached to; the full text ink (against the footnote's dim) is the difference between a
/// fault report and an action. It is NOT the accent: the accent is a shape, never a word's colour.
fn identity_note(ui: &mut egui::Ui, id: &identity::BackendIdentity<'_>) {
    let t = ThemeTokens::of(ui.ctx());
    if let Some(note) = id.note {
        let text =
            egui::RichText::new(note).monospace().size(role_px(ui.ctx(), ThemeTextRole::Caption));
        ui.label(if id.invitation { text.color(t.theme.text) } else { text.weak() });
    }
}

/// The Backend destination's body: the registry rows and their add/edit/delete flow, then the
/// connected node's effective settings. Called directly by `windows::data`'s `data_body` on
/// `DataManagerDestination::Backend`, right after [`strip_row`] draws above it.
///
/// ⚠ The rows here are the REGISTRY's — records in `backends.json`, one per backend an operator
/// can connect to. The LIVE connection is not rendered again: it is [`strip_row`]'s, once, above
/// this destination's own content. That is a duplication this module must never reintroduce, and
/// it was a real disagreement rather than a cosmetic one when it first shipped — the old settings
/// section's header restated the active backend's name beside a picker row that could have been
/// reconnected since.
///
/// ⚠⚠ **THAT WAS A CLAIM THE CODE DID NOT KEEP, and this function is where it was broken.** It
/// used to call `crate::backend::connection::picker_rows`, whose FIRST arm pushes the live
/// connection itself as an unlisted row whenever the registry does not list it — so a
/// `--observe ADDR` session rendered the dot, `(--observe)`, the address, `control armed`,
/// `(not in registry)` and a `Disconnect` on this destination AND every one of them again in
/// [`strip_row`] above it. Two renderings of one fact is precisely what [`strip_row`] exists to
/// end.
///
/// Two edits close it and both are visible here:
///
/// * `crate::backend::connection::registry_rows` — the registry's records ALONE. The head row
///   was never editable anyway (`if row.listed` already suppressed its Edit/Delete), which is the
///   tell that it was not a record. When the live connection is unlisted this destination says so
///   in a SENTENCE that names [`strip_row`]'s row, which is information that row does not carry
///   and a registry row could not add.
/// * **The active row keeps no `Disconnect`.** Disconnecting is one act on process-level state and
///   [`strip_row`] owns it. `Connect` STAYS on every non-active row, because that is how switching
///   happens and [`strip_row`] offers no switch — so this destination keeps the verb the row
///   above it lacks and gives up the one it would otherwise duplicate.
///
/// ⚠ `pub` because `data_body` calls this directly now; the earlier reason it was `pub` — a
/// headless harness needing to draw it inside the standalone window's `connections_body` — went
/// with that function. `crates/vike-app-core/tests/screens/data_manager_backend_strip.rs`'s
/// `the_live_connection_is_rendered_once_across_the_strip_and_the_backend_destination` drives
/// [`strip_row`] immediately followed by this function, in the same frame, the way `data_body`
/// does — not a stub standing in for either.
#[expect(clippy::too_many_arguments)] // one arm of the tool seam: read-only inputs + OUT slots
pub fn backend_tab(
    ui: &mut egui::Ui,
    here: &vike_account_manager::AccountManagerCredentialWrite<'_>,
    picker: &BackendPickerSources<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut BackendEditorState,
    registry: &mut Option<editor::BackendRegistryUpdate>,
    settings: &BackendSettingsState,
    settings_refresh: &mut bool,
    settings_edit: &mut BackendSettingsEditState,
    settings_write: &mut Option<BackendSettingsWriteRequest>,
    filter: &mut BackendSettingsFilter,
) {
    let t = ThemeTokens::of(ui.ctx());
    server_section(ui, action, here);
    ui.add_space(space::LG);
    ui.separator();
    let rows = registry_rows(picker.backends, picker.active);
    // The live connection, when the registry does not list it. ONE sentence pointing at the one
    // place it IS rendered — never a second copy of it.
    if active_is_unlisted(picker.backends, picker.active) {
        ui.label(
            egui::RichText::new(
                "the live connection is not a saved backend, so it has no row here — it is shown \
                 in the backend-connection row above this screen, where Disconnect and the \
                 invitation to give this box a name both live.",
            )
            .monospace()
            .size(role_px(ui.ctx(), ThemeTextRole::Caption))
            .weak(),
        );
        ui.add_space(space::SM);
    }
    if let Some(hint) = &picker.backends.refused {
        // An older build's (or a corrupt) `backends.json`: nothing in it was read.
        ui.colored_label(ThemeStatus::Error.color(), hint);
    } else if rows.is_empty() {
        ui.label(
            egui::RichText::new(
                "no backends configured — Add backend (above) creates the first record, stored \
                 in backends.json beside workspace.json",
            )
            .monospace()
            .size(role_px(ui.ctx(), ThemeTextRole::Body))
            .weak(),
        );
    } else {
        for row in rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::MD;
                ui.label(
                    egui::RichText::new(if row.is_active { "\u{25CF}" } else { " " })
                        .monospace()
                        .size(role_px(ui.ctx(), ThemeTextRole::Body))
                        .color(if row.is_active { t.theme.accent } else { t.theme.text_tertiary }),
                );
                // NAME first, address second — `crate::backend::identity` is the one
                // authority for both, so this row and the foot strip cannot describe the same
                // backend two ways.
                let id = identity::identify_reported(
                    row.record,
                    row.listed,
                    // Only the row this process is ATTACHED to has a daemon speaking for it.
                    identity::report_for(row.record, picker.active, picker.reported),
                );
                identity_labels(ui, &id);
                ui.label(
                    egui::RichText::new(if row.record.control {
                        "control armed"
                    } else {
                        "read-only"
                    })
                    .monospace()
                    .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                    .weak(),
                );
                identity_note(ui, &id);
                // ⚠ `Connect` on the NON-ACTIVE rows only. The active row's counterpart was
                // `Disconnect`, which is the strip's — see this function's doc. Leaving it here as
                // a DISABLED button would be the same duplication wearing a grey coat, so the
                // active row simply carries no connect verb: it is the one you are on.
                if !row.is_active && ui.button("Connect").clicked() {
                    *action = Some(click_action(row.record, picker.active));
                }
                // Manage buttons (I2) — these edit the FILE, not the connection. Every row here is
                // a registry record now (`registry_rows`), so there is no longer a `row.listed` arm
                // to guard them with.
                if ui.button("Edit").clicked() {
                    editor.open_edit(row.record);
                }
                if ui.button("Delete").clicked() {
                    editor.request_delete(&row.record.name);
                }
            });
        }
    }
    backend_editor_ui(ui, editor, picker, registry);

    ui.add_space(space::LG);
    ui.separator();
    // The settings header names the backend whose settings these are — the NAME, through the one
    // authority, so it cannot say `(--observe)` while the strip two rows up says something else.
    let active_name = picker.active.map(identity::headline);
    // ⚠ The ACTIVE record's own write-channel arming (B9), handed down so the settings editor can
    // say before the click that an unarmed backend cannot sign a write — rather than answering
    // with a transport refusal afterwards.
    let control_armed = picker.active.is_some_and(|r| r.control);
    backend_settings_section(
        ui,
        active_name,
        control_armed,
        settings,
        settings_refresh,
        settings_edit,
        settings_write,
        filter,
    );
}

/// The **Server** group (decision 0288): connect this desktop to its server with no terminal. One
/// address line (`vikectl install` and `vikectl status` print it) registers this PC's own device key
/// there over ssh (its PUBLIC half; the private half never leaves this PC); "this computer" registers
/// it with the server's container on this PC; a guest pastes the ONE invitation the server's owner
/// sent (it stays in this process, never on a command line). Each runs IN THIS PROCESS
/// (`crate::backend::server_link`, no `vike-cli` needed), its words shown here, the device key and
/// the dial rows filed in `here`'s settings database; when such a run has connected, the desktop
/// reconnects with its own key to the address the run stored (`server_link::take_reconnect`). The
/// owner's [`access_section`] sits below.
fn server_section(
    ui: &mut egui::Ui,
    action: &mut Option<BackendAction>,
    here: &vike_account_manager::AccountManagerCredentialWrite<'_>,
) {
    use crate::backend::server_link;
    if let Some(dial) = server_link::take_reconnect() {
        *action =
            Some(BackendAction::Connect(crate::backend::connection::cli_observe_record(&dial)));
    }
    let run = server_link::current();
    let running = run.as_ref().is_some_and(|r| r.ok.is_none());
    let line_id = ui.make_persistent_id("server_address_line");
    let paste_id = ui.make_persistent_id("server_invitation");
    let mut line = ui.data_mut(|d| d.get_temp::<String>(line_id)).unwrap_or_default();
    let mut paste = ui.data_mut(|d| d.get_temp::<String>(paste_id)).unwrap_or_default();
    let mut started: Option<Result<(), String>> = None;
    ui.label(egui::RichText::new("Server").size(role_px(ui.ctx(), ThemeTextRole::Body)).strong());
    caption(
        ui,
        "paste the address line the server printed (vikectl install or vikectl status: \
         user@host:/folder). The desktop registers its own key there over ssh (nothing secret \
         leaves this PC), raises the tunnel and raises it again after a restart. ssh must log in \
         without a password prompt (a key in your agent).",
    );
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut line).hint_text("app@203.0.113.7:/home/app/vike"));
        let parsed = server_link::parse_server_line(&line);
        let clicked = ui
            .add_enabled(!running && parsed.is_ok(), egui::Button::new("Connect over ssh"))
            .on_disabled_hover_text(parsed.as_ref().err().map_or("", String::as_str))
            .clicked();
        if let (true, Ok(server)) = (clicked, parsed) {
            started = Some(link(
                here,
                &format!("connecting to {}", server.host),
                format!("ssh {}", server.host),
                server_link::BackendLink::Server(server),
            ));
        }
    });
    if ui
        .add_enabled(!running, egui::Button::new("Connect the server on this computer"))
        .on_hover_text(
            "registers this PC with the server's container (vikectl's `vike`) on this PC by docker \
             exec. A Docker Desktop server answers here only with host networking on: Docker \
             Desktop 4.34 or later, signed in, Settings > Resources > Network.",
        )
        .clicked()
    {
        started = Some(link(
            here,
            "connecting to the server on this computer",
            String::new(),
            server_link::BackendLink::ThisComputer,
        ));
    }
    caption(
        ui,
        "a guest, with no login on the server: paste the ONE invitation its owner sent you (it \
         works once, within 15 minutes). This PC then registers its own keys there and uses them.",
    );
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut paste).password(true).hint_text("vike-invite-1 …"));
        let clicked = ui
            .add_enabled(!running && server_link::is_invitation(&paste), egui::Button::new("Join"))
            .on_disabled_hover_text("paste the line that starts with vike-invite-1")
            .clicked();
        if clicked {
            started = Some(link(
                here,
                "joining with an invitation",
                String::new(),
                server_link::BackendLink::Invitation(paste.trim().to_string()),
            ));
            paste.clear();
        }
    });
    ui.add_space(space::LG);
    access_section(ui, &line, running, &mut started);
    ui.data_mut(|d| {
        d.insert_temp(line_id, line);
        d.insert_temp(paste_id, paste);
    });
    if let Some(Err(e)) = started {
        caption(ui, &e);
    }
    let Some(run) = run else { return };
    let verdict = match run.ok {
        None => "running",
        Some(true) => "done",
        Some(false) => "failed",
    };
    caption(ui, &format!("{}: {verdict}", run.title));
    let invitation = server_link::invitation_in(&run.lines);
    if let Some(invitation) = invitation {
        caption(
            ui,
            "the invitation is ready: give it to that person alone. It opens the tunnel once, \
             within 15 minutes, and it is kept here until you forget it.",
        );
        ui.horizontal(|ui| {
            if ui.button("Copy the invitation").clicked() {
                ui.ctx().copy_text(invitation.to_string());
            }
            if ui.button("Forget it").clicked() {
                server_link::forget_output();
            }
        });
    }
    for line in run.lines.iter().filter(|l| Some(l.as_str()) != invitation) {
        caption(ui, line);
    }
    if run.ok == Some(false) && !run.by_hand.is_empty() {
        caption(ui, &format!("to see ssh's own questions, run it in a terminal: {}", run.by_hand));
    }
    if running {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// Start one connect IN THIS PROCESS, filing this PC's device key and dial rows in `here`'s settings
/// database and recording the rows under `here`'s process.
fn link(
    here: &vike_account_manager::AccountManagerCredentialWrite<'_>,
    title: &str,
    by_hand: String,
    link: crate::backend::server_link::BackendLink,
) -> Result<(), String> {
    let process = Some(here.process.clone());
    crate::backend::server_link::start_link(title, by_hand, link, here.settings_dir, process, true)
}

/// A caption line of the Server and Access groups.
fn caption(ui: &mut egui::Ui, text: &str) {
    let size = role_px(ui.ctx(), ThemeTextRole::Caption);
    ui.label(egui::RichText::new(text).monospace().size(size).weak());
}

/// The **Access** group (decision 0288): who else may reach the server of `line`, run on that server
/// over the owner's own ssh (`crate::backend::server_link::access_args`): the list of people and
/// devices, a new role, a revoke, and an invitation for a guest, which [`server_section`] shows until
/// it is forgotten. Whoever holds a shell there is the owner, so nothing here needs a key of its own.
fn access_section(
    ui: &mut egui::Ui,
    line: &str,
    running: bool,
    started: &mut Option<Result<(), String>>,
) {
    use crate::backend::server_link::{self, BackendAccessVerb};
    let who_id = ui.make_persistent_id("access_who");
    let role_id = ui.make_persistent_id("access_role");
    let roles = vike_hub_protocol::access::ROLES.map(|scope| scope.name());
    let mut who = ui.data_mut(|d| d.get_temp::<String>(who_id)).unwrap_or_default();
    let mut role =
        ui.data_mut(|d| d.get_temp::<String>(role_id)).unwrap_or_else(|| roles[0].to_string());
    ui.label(egui::RichText::new("Access").size(role_px(ui.ctx(), ThemeTextRole::Body)).strong());
    caption(
        ui,
        "who may reach the server of the address above: each person's devices, with a role \
         (observe watches, control also trades, admin also manages accounts). It runs there over \
         your ssh.",
    );
    let server = server_link::parse_server_line(line);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut who).hint_text("person or person/device"));
        // No visible label sits beside the picker, so it is named in the tree alone.
        egui::ComboBox::from_id_salt(role_id)
            .selected_text(role.as_str())
            .show_ui(ui, |ui| {
                for name in roles {
                    ui.selectable_value(&mut role, name.to_string(), name);
                }
            })
            .response
            .accessible_name("Role");
    });
    let named = who.trim().to_string();
    let hint = server.as_ref().err().map_or("type a person", String::as_str);
    let mut verb = None;
    ui.horizontal(|ui| {
        let ready = !running && server.is_ok();
        let button = |ui: &mut egui::Ui, label: &str, enabled: bool| {
            ui.add_enabled(enabled, egui::Button::new(label)).on_disabled_hover_text(hint).clicked()
        };
        if button(ui, "List", ready) {
            verb = Some(BackendAccessVerb::List);
        }
        let ready = ready && !named.is_empty();
        if button(ui, "Invite", ready) {
            verb = Some(BackendAccessVerb::Invite { person: named.clone(), role: role.clone() });
        }
        if button(ui, "Set role", ready) {
            verb = Some(BackendAccessVerb::Role { who: named.clone(), role: role.clone() });
        }
        if button(ui, "Revoke", ready) {
            verb = Some(BackendAccessVerb::Revoke { who: named.clone() });
        }
    });
    if let (Some(verb), Ok(server)) = (verb, &server) {
        let title = format!("access on {}: {verb:?}", server.host);
        *started = Some(
            server_link::access_args(server, &verb)
                .and_then(|args| server_link::start_ssh(&title, args)),
        );
    }
    ui.data_mut(|d| {
        d.insert_temp(who_id, who);
        d.insert_temp(role_id, role);
    });
}

// -------------------------------------------------------------------------------------------
// The ambient connection row
// -------------------------------------------------------------------------------------------

/// **The live connection's identity, control state and Disconnect / Add-backend actions — drawn
/// by `data_body` directly above EITHER of Data Manager's `DataManagerDestination::Gateway`/`Backend`
/// destinations, identically.**
///
/// ⚠ **It belongs to neither destination, deliberately.** It is process-level state you glance
/// at, like a status bar — rendering it once, above whichever of the two is on screen, removes a
/// duplication that could DISAGREE: the address used to appear both in the picker row and in the
/// Backend settings header, two renderings of one fact that a reconnect between two frames could
/// separate.
///
/// **Add backend** switches `*destination` to [`DataManagerDestination::Backend`] and opens the add form there. The
/// form is [`backend_tab`]'s — putting an expanding editor inside this row is what this row is
/// not — so the click has to take the operator to where it lands, rather than opening a form on a
/// destination they are not looking at.
///
/// ⚠ **The layout is spelled out and never inherited**, for the reason
/// `crates/vike-account-manager/src/view/rail.rs`'s `rail_chips` documents at length: `Ui::scope` would
/// inherit the PARENT's layout, and a wrapping row nested inside a wrapping row compounds.
/// `with_main_wrap(true)` is what keeps the Disconnect/Add buttons wrapping onto a second line on
/// a narrow window rather than running off the right edge.
///
/// ⚠ **This used to sit inside `ambient_strip`'s zero-height-allocated child**, a wrapper that
/// existed so the standalone Connections window's `connections_body` could measure this row's
/// height and feed it back into a foot-strip reservation — a mechanism Data Manager has no
/// equivalent of (`data_body` draws this row in the ordinary flow, and nothing downstream measures
/// it back into a layout decision). `ambient_strip` is deleted with that mechanism
/// (2026-10-05, Connections-merges-into-Data-Manager); this function's OWN body is unchanged
/// except for the one parameter whose type changed, below.
pub fn strip_row(
    ui: &mut egui::Ui,
    picker: &BackendPickerSources<'_>,
    action: &mut Option<BackendAction>,
    destination: &mut DataManagerDestination,
    editor: &mut BackendEditorState,
) {
    let t = ThemeTokens::of(ui.ctx());
    ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 0.0),
            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
            |ui| {
                ui.spacing_mut().item_spacing.x = space::MD;
            ui.label(egui::RichText::new("Backend").monospace().size(role_px(ui.ctx(), ThemeTextRole::Caption)).color(t.theme.text_tertiary));
            match picker.active {
                Some(record) => {
                    // ⚠ The dot marks the record this process is ATTACHED to, not the link's health.
                    // `BackendPickerSources` carries no liveness — the observe bridge is a self-healing
                    // reconnect loop, so "attached" survives a momentary drop — and a dot that read as
                    // "up" would be claiming something this side was never told. The hover says so
                    // rather than the dot changing colour on a fact nobody measured.
                    ui.label(
                        egui::RichText::new("\u{25CF}").monospace().size(role_px(ui.ctx(), ThemeTextRole::Body)).color(t.theme.accent),
                    )
                    .on_hover_text(
                        "the backend this process is attached to — not a liveness check: the observe \
                         bridge reconnects on its own and this side is not told when it does",
                    );
                    // ⚠ THE NAME LEADS. The address used to, and it identified nothing: both the CI box
                    // listeners bind loopback, so every thin client reaches them down an SSH tunnel
                    // and every one of them showed `127.0.0.1` whatever box it was attached to.
                    // `crate::backend::identity` carries the measurement.
                    let id = identity::identify_reported(
                        record,
                        picker.backends.backends.contains(record),
                        picker.reported,
                    );
                    identity_labels(ui, &id);
                    ui.label(
                        egui::RichText::new(if record.control { "control armed" } else { "read-only" })
                            .monospace()
                            .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                            .weak(),
                    );
                    identity_note(ui, &id);
                }
                None => {
                    ui.label(
                        egui::RichText::new("\u{25CB}").monospace().size(role_px(ui.ctx(), ThemeTextRole::Body)).color(t.theme.text_tertiary),
                    );
                    ui.label(egui::RichText::new("not connected").monospace().size(role_px(ui.ctx(), ThemeTextRole::Body)).weak());
                }
            }
            // ⚠ THE BUTTONS SIT AT THE ROW'S RIGHT EDGE, not immediately after the text. The
            // approved design's strip is `space-between`: state on the left, actions on the right.
            // Drawn inline they landed mid-row wherever the note happened to end, so the actions
            // moved horizontally every time the note changed — `(not in the registry)` versus the
            // longer invitation is a ~200pt swing, and a control that moves is a control you have
            // to hunt for.
            //
            // egui has no flex spacer, so the run is MEASURED and padded, exactly as
            // `vike_account_manager::view`'s `account_strip` does for its legend. Both rules from that
            // site apply here and neither is optional:
            //
            //   * `available_rect_before_wrap()`, NEVER `available_width()` — `Layout::available_size`'s
            //     `main_wrap` arm returns the WHOLE row's width rather than what is left of it, so
            //     padding by it overshoots by everything already drawn and pushes the buttons past
            //     the wrap point onto a line of their own. That is the band this padding exists to
            //     prevent, reached through the padding meant to prevent it.
            //   * pad ONLY when the run still fits. On a narrow window the buttons are MEANT to
            //     wrap onto a second line, and padding a row too narrow for them would push the
            //     tail off the right edge instead.
            let show_disconnect = picker.active.is_some();
            let run = strip_actions_w(ui, show_disconnect);
            let room = ui.available_rect_before_wrap().width();
            // `connections::STRIP_ACTIONS_FIT_SLACK` is the rounding margin this leaves when it
            // right-aligns the action buttons. The run is measured by the same painter that then
            // lays it out, and a fraction of a point of disagreement would wrap the LAST button
            // alone onto its own line — the twin of `connections::LEGEND_FIT_SLACK` (the legend
            // strip's, in `vike_account_manager::view`), for the same reason.
            let slack = connections::STRIP_ACTIONS_FIT_SLACK;
            if room >= run + slack {
                ui.add_space(room - run - slack);
            }
            if show_disconnect
                && ui
                    .button(egui::RichText::new("Disconnect").size(role_px(ui.ctx(), ThemeTextRole::Body)))
                    .clicked()
            {
                *action = Some(BackendAction::Disconnect);
            }
            if ui.button(egui::RichText::new("Add backend").size(role_px(ui.ctx(), ThemeTextRole::Body))).clicked() {
                // The form lives on the Backend destination; take the operator there rather than
                // opening it where they cannot see it.
                *destination = DataManagerDestination::Backend;
                editor.open_add();
            }
            },
        );
}

/// The editor body under the Backends rows — RENDER ONLY: reads the [`BackendEditorState`], draws the
/// are-you-sure line or the Add/Edit form, and forwards Save/Confirm through the pure
/// `backend::editor` decisions into the `registry` OUT slot. Click flags are collected first and
/// applied after the `match`, because the state transitions need `&mut BackendEditorState` while the
/// arms hold borrows into it.
fn backend_editor_ui(
    ui: &mut egui::Ui,
    editor: &mut BackendEditorState,
    picker: &BackendPickerSources<'_>,
    registry: &mut Option<editor::BackendRegistryUpdate>,
) {
    let mut save_clicked = false;
    let mut confirm_clicked = false;
    let mut cancel_clicked = false;
    match editor {
        BackendEditorState::Closed => {}
        BackendEditorState::ConfirmDelete { name } => {
            ui.add_space(space::MD);
            ui.horizontal(|ui| {
                ui.label(format!("Delete backend {name:?}?"));
                if editor::delete_disconnects(name, picker.active) {
                    ui.weak("— the ACTIVE backend: deleting disconnects it first");
                }
                confirm_clicked = ui.button("Delete").clicked();
                cancel_clicked = ui.button("Cancel").clicked();
            });
        }
        BackendEditorState::Add { form, errors } => {
            let clicks = backend_form_ui(ui, "Add backend", form, errors, false);
            (save_clicked, cancel_clicked) = clicks;
        }
        BackendEditorState::Edit { original, form, errors } => {
            let defers = editor::edit_defers(original, picker.active);
            let title = format!("Edit backend {original:?}");
            let clicks = backend_form_ui(ui, &title, form, errors, defers);
            (save_clicked, cancel_clicked) = clicks;
        }
    }
    if cancel_clicked {
        editor.cancel();
    } else if save_clicked {
        if let Some(upd) = editor::submit(editor, picker.backends) {
            *registry = Some(upd);
        }
    } else if confirm_clicked
        && let Some(upd) = editor::confirm_delete(editor, picker.backends, picker.active)
    {
        *registry = Some(upd);
    }
}

/// The Add/Edit form body: the three fields, the edit-active deferral line, and the previous
/// Save's refusals. No key field: this desktop's device key signs every dial (decision 0288).
/// Returns `(save_clicked, cancel_clicked)`.
fn backend_form_ui(
    ui: &mut egui::Ui,
    title: &str,
    form: &mut BackendForm,
    errors: &[BackendFormError],
    defers: bool,
) -> (bool, bool) {
    ui.add_space(space::MD);
    ui.strong(title);
    ui.horizontal(|ui| {
        ui.label("name");
        ui.text_edit_singleline(&mut form.name);
    });
    ui.horizontal(|ui| {
        ui.label("address");
        ui.text_edit_singleline(&mut form.address);
        ui.weak("host:port");
    });
    ui.checkbox(&mut form.control, "control armed (write channel)");
    if form.control && crate::backend::registry::device_key().is_none() {
        ui.weak(format!("armed, but {}", crate::backend::registry::no_device_key(&form.address)));
    }
    if defers {
        ui.weak(
            "editing the ACTIVE backend — changes take effect on next connect; \
             the live connection is not rewired",
        );
    }
    for e in errors {
        ui.colored_label(ThemeStatus::Error.color(), e.to_string());
    }
    let mut clicks = (false, false);
    ui.horizontal(|ui| {
        clicks = (ui.button("Save").clicked(), ui.button("Cancel").clicked());
    });
    clicks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::windows::backend::settings::BackendSettingsDigest;
    use vike_tradehub_client::wire::{SettingsShowRowWire, SettingsShowWire};

    fn row(key: &str, origin: &str, read_by: &str) -> SettingsShowRowWire {
        SettingsShowRowWire {
            section: "config.toml".into(),
            key: key.into(),
            value: "v".into(),
            origin: origin.into(),
            read_by: read_by.into(),
        }
    }

    /// ⚠ The Backend badge carries NO NUMBER in every state where the node has not answered — the
    /// segment then renders its label alone. Reddens on a `0` standing in for "not known", which
    /// is the shape of invention this tool is not allowed to make.
    #[test]
    fn the_backend_badge_has_no_number_until_the_node_answers() {
        for state in [
            BackendSettingsState::Idle,
            BackendSettingsState::Pending,
            BackendSettingsState::Unsupported,
            BackendSettingsState::Error("bad mac".into()),
        ] {
            let d = BackendSettingsDigest::of(true, &state);
            assert_eq!(d.badge_count(), None, "{state:?} is not a count");
            assert!(!d.has_finding(), "{state:?} cannot have a finding");
        }
        assert_eq!(
            BackendSettingsDigest::of(false, &BackendSettingsState::Idle),
            BackendSettingsDigest::NoBackend
        );
        assert_eq!(
            BackendSettingsDigest::of(false, &BackendSettingsState::Idle).badge_count(),
            None
        );

        let show = SettingsShowWire {
            settings_dir: Some("/srv/vike-<unit>/settings".into()),
            rows: vec![
                row("config.tradehub_addr", "config.toml", "tradehub"),
                row("config.log_dir", "config.toml", "NO"),
                row("flags.reconcile", "default", "tradehub"),
            ],
            refusal: None,
        };
        let d = BackendSettingsDigest::of(true, &BackendSettingsState::Loaded(show));
        assert_eq!(d.badge_count(), Some(2), "two rows a layer names");
        assert!(d.has_finding(), "config.log_dir is set and read by nothing");
        let line = d.line();
        assert!(line.contains("3 keys"), "{line}");
        assert!(line.contains("2 set"), "{line}");
        assert!(line.contains("config.log_dir"), "the finding is NAMED, not just counted: {line}");
    }

    /// ⚠ **`NotFetched` is not `Loading`, and the digest must say so in its own words** — "not
    /// read yet" is a different fact from "reading…", and folding them together (as `BackendSettingsDigest`
    /// used to) is what let the Backend destination's badge/digest claim a fetch was in flight in
    /// the one state where nothing had asked for one. `BackendSettingsDigest::line()` carries the exact
    /// wording a reader sees, so this asserts the strings directly rather than through a harness —
    /// no chrome renders this digest any differently from `.line()`'s own output.
    #[test]
    fn the_backend_digest_tells_not_asked_yet_apart_from_asking() {
        let idle = BackendSettingsDigest::of(true, &BackendSettingsState::Idle);
        let pending = BackendSettingsDigest::of(true, &BackendSettingsState::Pending);
        assert_ne!(idle, pending, "two different facts");
        assert_eq!(idle, BackendSettingsDigest::NotFetched);
        assert_eq!(pending, BackendSettingsDigest::Loading);
        assert_eq!(idle.badge_count(), None, "neither invents a number");
        assert_eq!(pending.badge_count(), None);
        assert!(idle.line().contains("not read yet"), "{}", idle.line());
        assert!(pending.line().contains("reading…"), "{}", pending.line());
    }
}
