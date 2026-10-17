//! The panel's store WRITE surfaces: the masked credential form and the account-row acts.
//!
//! Every store write this panel performs is in this file (the credential-writer gate pins it as
//! one row): a tier form's Save (`vike_secrets::save_owned_credentials`, against the account ROW the
//! form was opened for; the venue application group's against the venue), the LIVE form's
//! venue-wide attribution tail (`vike_secrets::save_credentials_to_store`, by its deployment NAME),
//! the field Remove (`vike_secrets::retire_credential_in_store`, ONE row by its stored NAME) and the
//! account-row acts (`vike_secrets::edit_account_in`). A credential write never creates an account
//! and never chooses a tier (decision 0156 item 6).

use std::time::Duration;
use vike_ui_theme::components::role_px;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::connections;

use vike_change_journal::{Actor, TIER_UNTIERED};

use super::state::{EditState, FormTarget, Verdict};
use super::{CONFIGURED_COLOR, ERROR_COLOR, MASKED_FIELD_HINT, note};
use crate::env_write::CredentialWrite;
use crate::keys::{FieldTarget, FormField, FormPlace, Sensitivity, form_fields, form_note};
use crate::status::{AccountGrids, HeldFields};

/// The form's place in the operator's words — the tier's own word (`AccountTier::as_str`), or
/// `application` for the venue's application group.
pub(super) fn place_word(place: FormPlace) -> &'static str {
    match place {
        FormPlace::Tier(tier) => tier.as_str(),
        FormPlace::VenueApp => "application",
    }
}

/// The tier word a change-journal record and a log line carry for a write to `target` from
/// `place`'s form: the account's tier, or `vike_change_journal::TIER_UNTIERED` for a row no
/// account owns (a venue application row, a deployment row).
fn tier_word(place: FormPlace, target: &FieldTarget) -> &'static str {
    match (place, target) {
        (FormPlace::Tier(tier), FieldTarget::Account(_)) => tier.as_str(),
        _ => TIER_UNTIERED,
    }
}

/// **What may be read back into `field`** — the PUBLIC value the store holds for it, or `None`.
///
/// `held` is the owner's [`HeldFields`], whose `readable` map was FILTERED through the schema's
/// `secret` when it was built; a deployment row (the attribution tail) is public by construction.
/// So a secret's plaintext is not merely skipped here, it was never handed to this function.
fn readable_value<'g>(
    grids: &'g AccountGrids,
    held: Option<&'g HeldFields>,
    field: &FormField,
) -> Option<&'g str> {
    match &field.target {
        FieldTarget::Account(k) | FieldTarget::Venue(k) => held.and_then(|h| h.readable(k.field)),
        FieldTarget::Deployment(name) => grids.deployment_value(name),
    }
}

/// **The stored NAME a Remove would retire for `field`**, when the store holds it: the account
/// key listing's own name for an account field ([`HeldFields::stored_name`] — never composed), the
/// deployment row's name for the attribution tail, and NONE for a venue application field (no
/// listing names those rows, so no Remove is offered rather than a guessed name retired).
fn removable_name<'a>(
    grids: &AccountGrids,
    held: Option<&'a HeldFields>,
    field: &'a FormField,
) -> Option<&'a str> {
    match &field.target {
        FieldTarget::Account(k) => held.filter(|h| h.holds(k.field))?.stored_name(k.field),
        FieldTarget::Venue(_) => None,
        FieldTarget::Deployment(name) => grids.deployment_value(name).map(|_| name.as_str()),
    }
}

/// Render the masked edit form for `state.target`, if any. No-op when nothing is open.
///
/// `creds` carries the resolved settings directory and the durable ledger — see
/// [`CredentialWrite`] for why BOTH are parameters rather than walks this function performs for
/// itself.
pub(super) fn render_edit_form(
    ui: &mut egui::Ui,
    state: &mut EditState,
    creds: CredentialWrite<'_>,
    grids: &AccountGrids,
) {
    let Some(FormTarget { venue, place, account: account_id }) = state.target.clone() else {
        return;
    };
    let label = state.account.clone();
    let fields = form_fields(&venue, place);
    // The owner's fields: the account row's for a tier's form, the venue's for its application
    // group. Presence and PUBLIC values only.
    let held = match place {
        FormPlace::Tier(_) => account_id.and_then(|id| grids.account_fields(id)),
        FormPlace::VenueApp => grids.venue_fields(&venue),
    };

    // ⚠ **THE READ-BACK, and the whole of it.** Exactly once per opened form, and only for the
    // fields the schema calls PUBLIC. [`readable_value`] reads maps the grid FILTERED when it was
    // built, so a secret's plaintext is never handed to this function. That is what keeps security
    // rule 1 a structural property rather than a careful one.
    if !state.prefilled {
        for (i, field) in fields.iter().enumerate() {
            if field.sensitivity() == Sensitivity::Public
                && let Some(current) = readable_value(grids, held, field)
                && let Some(buf) = state.buffers.get_mut(i)
            {
                current.clone_into(buf);
            }
        }
        state.prefilled = true;
    }

    ui.separator();
    // The title names the account only when there IS one to name — `AccountLabel::Default` has no
    // text BY DESIGN, so rendering one would invent a spelling that appears nowhere — and only on a
    // tier's form: the application group is the venue's, every account's.
    let heading = match (place, label.text()) {
        (FormPlace::Tier(_), Some(l)) => {
            format!("Edit {venue} / {} · account {l}", place_word(place))
        }
        _ => format!("Edit {venue} / {}", place_word(place)),
    };
    ui.label(
        egui::RichText::new(heading).monospace().strong().size(role_px(ui.ctx(), TextRole::Title)),
    );
    ui.label(
        egui::RichText::new(MASKED_FIELD_HINT).size(role_px(ui.ctx(), TextRole::Caption)).weak(),
    );
    if let Some(note) = form_note(&venue, place) {
        ui.label(egui::RichText::new(note).size(role_px(ui.ctx(), TextRole::Caption)).weak());
    }

    // ⚠ THE TWO-CLICK REMOVE's clock. An armed control disarms after `RETIRE_ARM`: expired HERE,
    // before any button is drawn, so a stale "Confirm remove" is never on screen, and a repaint is
    // requested for the deadline so the control returns to "Remove" without the pointer moving.
    let now = ui.input(|i| i.time);
    if let Some(left) = state.tick_retire_arm(now) {
        ui.ctx().request_repaint_after(Duration::from_secs_f64(left));
    }
    // What the Remove controls did this frame: the field whose confirm was clicked, and whether any
    // control (re)armed (the arming click is itself a click, and must not disarm what it armed).
    let mut retire_now: Option<usize> = None;
    let mut armed_this_frame = false;

    // ⚠ ONE label cell for the whole form, as wide as its widest label (never narrower than
    // `FIELD_LABEL_CELL`): a fixed cell let "Passphrase (optional)" and "Broker Code (venue-wide,
    // optional)" overrun it, so those input boxes started further right than API Key's. Labels are
    // LEFT-aligned in it, and every row is as tall as the Remove button, so the pitch is equal
    // with or without one.
    let label_font = egui::FontId::monospace(role_px(ui.ctx(), TextRole::Strong));
    let label_w = fields
        .iter()
        .map(|field| {
            ui.painter()
                .layout_no_wrap(
                    field.label.to_string(),
                    label_font.clone(),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
        })
        .fold(connections::FIELD_LABEL_CELL.x, f32::max)
        + space::MD;
    let row_h = {
        let button_text = egui::FontId::proportional(role_px(ui.ctx(), TextRole::Body));
        let text_h = ui
            .painter()
            .layout_no_wrap("Remove".to_string(), button_text, egui::Color32::PLACEHOLDER)
            .size()
            .y;
        (text_h + 2.0 * ui.spacing().button_padding.y)
            .max(ui.spacing().interact_size.y)
            .max(connections::FIELD_LABEL_CELL.y)
    };

    for (i, field) in fields.iter().enumerate() {
        let hint = field.hint();
        ui.horizontal(|ui| {
            ui.set_min_height(row_h);
            ui.allocate_ui_with_layout(
                egui::vec2(label_w, row_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(label_w); // the cell keeps its width, whatever the text measures
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(field.label)
                                .monospace()
                                .size(role_px(ui.ctx(), TextRole::Strong)),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend),
                    );
                },
            );
            let buf = state.buffers.get_mut(i).expect("buffers sized to fields in open()");
            // ⚠ MASKED IFF SECRET. `password(true)` is what files the widget as
            // `Role::PasswordInput`, which is the state assistive tech and any tree-reading
            // automation are entitled to speak aloud — right for a key, wrong for a login the
            // operator is here to READ. The schema's `secret` is the rule, and it fails closed.
            // A field removed since this form opened reads "not set" (and its buffer was emptied);
            // every other field keeps the FIELD it writes as its grey text.
            let removed = state.retired.iter().any(|k| k == hint);
            ui.add(
                egui::TextEdit::singleline(buf)
                    .password(field.sensitivity() == Sensitivity::Secret)
                    .hint_text(if removed { "not set" } else { hint })
                    .desired_width(connections::FIELD_INPUT_W),
            );
            // A Remove sits only beside a field the store HOLDS under a name it LISTS: there is
            // nothing to take out of an empty one, and offering it would claim there is.
            if removable_name(grids, held, field).is_some() {
                match retire_control(ui, &mut state.retire_armed, hint, now) {
                    RetireClick::Nothing => {}
                    RetireClick::Armed => armed_this_frame = true,
                    RetireClick::Confirmed => retire_now = Some(i),
                }
            }
        });
    }

    // ⚠ ANY OTHER CLICK DISARMS an armed Remove (the owner's two-click design, 0144's ADDED note):
    // a click on the Confirm control sends, a click on a Remove control re-arms THAT one, and a
    // click anywhere else — a field, Save, Cancel, the rail — drops the arm, so a confirm the
    // operator walked away from is not still live under the next click. The arming click itself is
    // a click, hence `armed_this_frame`.
    if state.retire_armed.is_some()
        && retire_now.is_none()
        && !armed_this_frame
        && ui.input(|i| i.pointer.any_click())
    {
        state.retire_armed = None;
        ui.ctx().request_repaint();
    }
    if let Some(i) = retire_now
        && let Some(field) = fields.get(i)
        && let Some(name) = removable_name(grids, held, field)
    {
        state.retire_armed = None;
        let hint = field.hint().to_string();
        let tier = tier_word(place, &field.target);
        // The DELETE and its durable record, together — `vike_secrets::retire_credential_in_store`
        // (decision 0144): the ONE row the readers answer for `name` is RETIRED, never erased, and
        // the record carries the NAME and no value. The name is the store's own (its account key
        // listing, or the deployment row's), never one composed here.
        match vike_secrets::retire_credential_in_store(
            creds.settings_dir,
            vike_secrets::Table::Credential,
            name,
            vike_secrets::CredentialJournal {
                actor: Actor::Gui,
                venue: &venue,
                tier,
                proc: creds.proc.clone(),
                now_ms: creds.now_ms,
            },
        ) {
            Ok((_backend, journal_error)) => {
                if let Some(err) = &journal_error {
                    tracing::error!(
                        error = %err,
                        dir = %err.dir.display(),
                        venue = %venue,
                        tier = %tier,
                        "credential retire NOT recorded to the change journal (the key IS removed)"
                    );
                }
                // NEVER the value: the name and the cells only.
                tracing::info!(
                    kind = "credential_retire",
                    venue = %venue,
                    env = %tier,
                    key = %name,
                    "removed credential"
                );
                // The field shows empty: its buffer (a public field's prefill included) is cleared
                // and `prefilled` stays latched, so nothing re-seeds it from a store that no longer
                // answers the field.
                if let Some(buf) = state.buffers.get_mut(i) {
                    buf.clear();
                }
                if !state.retired.contains(&hint) {
                    state.retired.push(hint.clone());
                }
                state.message = Some(Verdict {
                    is_error: false,
                    text: format!("Removed {hint} — kept at rest, Save a value to add it back"),
                    venue: venue.clone(),
                    place,
                });
            }
            // The store's own refusal (a field another spelling still answers for, a store that
            // will not open): verbatim, naming NAMES and a path, never a value.
            Err(e) => {
                state.message = Some(Verdict {
                    is_error: true,
                    text: format!("remove failed: {e}"),
                    venue: venue.clone(),
                    place,
                });
            }
        }
        ui.ctx().request_repaint();
    }

    // A gap before the buttons, so they do not sit on the last input.
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            // The fields an operator TYPED, split by where they land: the owner's fields (the
            // account row's, or the venue's application rows) and the deployment rows.
            let mut owned: Vec<(String, String)> = Vec::new();
            let mut deployment: Vec<(String, String)> = Vec::new();
            for (field, value) in fields.iter().zip(state.buffers.iter()) {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // ⚠ A PREFILLED public field the operator did not touch is not an edit. Without
                // this, every Save of any form would rewrite every readable field it showed and
                // journal it as a change, which would make the ledger's own record of *what was
                // rotated* useless exactly when somebody is reading it to find out.
                if readable_value(grids, held, field).map(str::trim) == Some(trimmed) {
                    continue;
                }
                match &field.target {
                    FieldTarget::Account(k) | FieldTarget::Venue(k) => {
                        owned.push((k.field.to_string(), trimmed.to_string()));
                    }
                    FieldTarget::Deployment(name) => {
                        deployment.push((name.clone(), trimmed.to_string()));
                    }
                }
            }

            if owned.is_empty() && deployment.is_empty() {
                state.message = Some(Verdict {
                    is_error: true,
                    text: "nothing entered — no changes saved".to_string(),
                    venue: venue.clone(),
                    place,
                });
            } else {
                match write_form(creds, &venue, place, account_id, &owned, &deployment) {
                    Ok(()) => {
                        // The console/journald copy STAYS. It is not a duplicate of the ledger — it
                        // is the line an operator tailing the app sees now, and the ledger is what
                        // survives a rotation they will read months later. NEVER a value.
                        //
                        // ⚠ TWO arms rather than one `account = %account` field: the DEFAULT
                        // account's line carries no account cell, exactly as it never did.
                        // `AccountLabel`'s `Display` renders the default account as the reserved
                        // spelling, so a single always-present field would add a cell to every
                        // single-account box's log line to say nothing.
                        let env = match place {
                            FormPlace::Tier(tier) => tier.as_str(),
                            FormPlace::VenueApp => TIER_UNTIERED,
                        };
                        let shown = match place {
                            FormPlace::Tier(_) => label.text(),
                            FormPlace::VenueApp => None,
                        };
                        match shown {
                            None => tracing::info!(
                                kind = "credential_write",
                                venue = %venue,
                                env = %env,
                                "saved credentials"
                            ),
                            Some(l) => tracing::info!(
                                kind = "credential_write",
                                venue = %venue,
                                env = %env,
                                account = %l,
                                "saved credentials"
                            ),
                        }
                        let msg = match shown {
                            None => format!("saved credentials for {venue}/{}", place_word(place)),
                            Some(l) => format!(
                                "saved credentials for {venue}/{} (account {l})",
                                place_word(place)
                            ),
                        };
                        state.close();
                        state.message = Some(Verdict {
                            is_error: false,
                            text: msg,
                            venue: venue.clone(),
                            place,
                        });
                        ui.ctx().request_repaint();
                    }
                    Err(text) => {
                        state.message =
                            Some(Verdict { is_error: true, text, venue: venue.clone(), place });
                    }
                }
            }
        }
        if ui.button("Cancel").clicked() {
            state.close();
        }
    });
    // ...and one after them, so the verdict line (drawn by the detail pane) is not jammed under.
    ui.add_space(space::MD);
}

/// **The store writes ONE Save performs**, in order, each with its durable record:
///
/// 1. the owner's FIELDS — `vike_secrets::save_owned_credentials` against the account row the form
///    was opened for (`CredentialOwner::Account`), or the venue's application rows
///    (`CredentialOwner::Venue`). The store REFUSES an account id no row carries; nothing here
///    creates an account or chooses a tier. The record carries the names written and the account's
///    tier word, never a value;
/// 2. the LIVE form's venue-wide attribution tail — `vike_secrets::save_credentials_to_store`, by
///    its DEPLOYMENT name, recorded under `vike_change_journal::TIER_UNTIERED` (no account owns it).
///
/// A failed write returns its sentence (an OS or store reason, a path, a name — never a value),
/// and the second is not attempted after a failed first. A journal failure does not fail the Save:
/// the rows ARE written, and it is logged here, the one place that can.
fn write_form(
    creds: CredentialWrite<'_>,
    venue: &str,
    place: FormPlace,
    account: Option<i64>,
    owned: &[(String, String)],
    deployment: &[(String, String)],
) -> Result<(), String> {
    if !owned.is_empty() {
        let owner = match (place, account) {
            (FormPlace::Tier(_), Some(id)) => vike_secrets::CredentialOwner::Account(id),
            (FormPlace::Tier(_), None) => {
                let why = "save failed: this form names no account row — NOTHING WAS WRITTEN";
                return Err(why.to_string());
            }
            (FormPlace::VenueApp, _) => vike_secrets::CredentialOwner::Venue(venue.to_string()),
        };
        let (_done, journal_error) = vike_secrets::save_owned_credentials(
            creds.settings_dir,
            &owner,
            owned,
            vike_secrets::UpsertIntent::Add,
            vike_secrets::AccountJournal {
                actor: Actor::Gui,
                proc: creds.proc.clone(),
                now_ms: creds.now_ms,
            },
        )
        // io::Error's Display is an OS or store reason about a path, an id or a field, never a
        // value.
        .map_err(|e| format!("save failed: {e}"))?;
        if let Some(err) = &journal_error {
            tracing::error!(
                error = %err,
                dir = %err.dir.display(),
                venue = %venue,
                tier = %place_word(place),
                "credential write NOT recorded to the change journal (the keys ARE saved)"
            );
        }
    }
    if !deployment.is_empty() {
        let (_backend, journal_error) = vike_secrets::save_credentials_to_store(
            creds.settings_dir,
            vike_secrets::Table::Credential,
            deployment,
            vike_secrets::UpsertIntent::Add,
            vike_secrets::CredentialJournal {
                actor: Actor::Gui,
                venue,
                tier: TIER_UNTIERED,
                proc: creds.proc.clone(),
                now_ms: creds.now_ms,
            },
        )
        .map_err(|e| {
            if owned.is_empty() {
                format!("save failed: {e}")
            } else {
                format!("saved the account's fields, but the venue-wide fields failed: {e}")
            }
        })?;
        if let Some(err) = &journal_error {
            tracing::error!(
                error = %err,
                dir = %err.dir.display(),
                venue = %venue,
                tier = %TIER_UNTIERED,
                "credential write NOT recorded to the change journal (the keys ARE saved)"
            );
        }
    }
    Ok(())
}

/// What one field's Remove control did this frame.
enum RetireClick {
    /// Not clicked.
    Nothing,
    /// The first click: the control now reads "Confirm remove FIELD".
    Armed,
    /// The second click, on the armed control: send.
    Confirmed,
}

/// **One held field's REMOVE control, and its two clicks.** Unarmed it reads "Remove"; clicked, it
/// ARMS (`armed` records the field and the egui time) and reads "Confirm remove FIELD" — the exact
/// field, in the error colour — until the second click sends, [`super::state::RETIRE_ARM`] passes,
/// or any other click lands. Nothing is typed (the owner's design, `docs/decisions/0144`'s ADDED
/// note); the wire's `confirm == key` is filled by the GUI that sends it, and THIS panel's store
/// call needs none.
fn retire_control(
    ui: &mut egui::Ui,
    armed: &mut Option<(String, f64)>,
    field: &str,
    now: f64,
) -> RetireClick {
    let is_armed = matches!(armed, Some((k, _)) if k == field);
    let size = role_px(ui.ctx(), TextRole::Body);
    if is_armed {
        let confirm = ui.button(
            egui::RichText::new(format!("Confirm remove {field}")).size(size).color(ERROR_COLOR),
        );
        return if confirm.clicked() { RetireClick::Confirmed } else { RetireClick::Nothing };
    }
    let remove = ui
        .button(egui::RichText::new("Remove").size(size))
        .on_hover_text(format!(
            "Remove {field} from the store (kept at rest; Save adds it back). The tier above reads \"configured\" only while every field it requires is stored."
        ));
    if remove.clicked() {
        *armed = Some((field.to_string(), now));
        RetireClick::Armed
    } else {
        RetireClick::Nothing
    }
}

/// **The `account` ROWS the settings database holds for the selected label, and the two acts a UI
/// may perform on one** — DEACTIVATE (reversible, the one it leads with) and REMOVE.
///
/// # ⚠ The grid and this block read the same object at two moments
///
/// Both are account ROWS now (decision 0156 item 6): the tier cells are the rows of
/// `(venue, label, tier)` out of the snapshot the panel's caller read this frame
/// ([`AccountGrids`]), and this block lists every row carrying the selected label, keyed by `id`,
/// out of its own held read (`EditState::account_rows`) through the same
/// `vike_secrets::CredentialStore::accounts` a mount reads — with the INACTIVE ones, and with the
/// two acts a row takes.
///
/// # Why DEACTIVATE and REMOVE and not CREATE
///
/// A row needs a `(venue, tier)`, and this strip's selection is venue-INDEPENDENT — one label spans
/// every venue's grid. A Create button here would have to GUESS which venue and which tier the
/// operator meant, and a guessed pair is a row filed against the wrong book. The two acts below
/// need neither: they address a row that EXISTS, by its own id. Creating one is
/// `vike-cli secrets account add --venue V --tier T --label L`, and the note below names it (a
/// tier cell with no row names it for its own venue and tier).
///
/// # Removal is refused by the store, not by a missing button
///
/// `vike_secrets::edit_account` REFUSES to delete a row that still owns credentials, naming them by
/// NAME, and the schema's foreign key refuses it again behind that.
/// **Nothing in THIS block erases a credential**, and nothing here rewrites a store. (The form above
/// retires ONE field's row on a deliberate two-click; that is the field's Remove, not an account
/// act.)
pub(crate) fn account_rows_block(
    ui: &mut egui::Ui,
    state: &mut EditState,
    creds: CredentialWrite<'_>,
) {
    let Some(label) = state.account.text().map(str::to_string) else { return };
    let settings_dir = creds.settings_dir;

    // ⚠ **THE STANDING RULE, rendered BEFORE the store is read and on every path below** — the
    // `Unanswerable` arm returns early, the empty arm returns early, and a reader who lands on
    // either still has to be told what this panel will and will not do. It is a property of the
    // PANEL rather than of whether this box happens to have rows, which is why it is not inside
    // any of the arms.
    let rule = icons::WARNING.before(
        ui.style(),
        egui::RichText::new(
            "nothing here erases a credential — the store is your only copy of live venue keys. A \
             Save is an in-place upsert of the fields it was asked to save, and Remove beside a \
             field only RETIRES that one field (two clicks): the row is kept at rest and the field \
             reads as not set until a Save adds it back. An account ROW can be removed too, and \
             that is a different object: the store REFUSES to delete one while it still owns \
             credentials, naming them.",
        )
        .monospace()
        .size(role_px(ui.ctx(), TextRole::Caption))
        .weak(),
    );
    note(ui, rule);

    // The rows, from the store that ANSWERS — the same `backend_in` probe the daemon's own
    // credential read asks, so this panel and that mount cannot disagree about which store they are
    // describing. A box with no database answers `Unanswerable` and gets the note below rather than
    // an empty list, which would read as *this label has no rows* about a store that cannot be
    // asked at all.
    //
    // ⚠ **Resolved ONCE and held in [`EditState::account_rows`], not once per frame.** This is an
    // immediate-mode body; the unconditional call that used to sit here opened the SQLite store
    // 60–120 times a second on the GUI thread, and held a SHARED lock a concurrent writer had to
    // wait out. That field's doc carries the measurement and the exact set of acts that invalidate
    // it.
    //
    // ⚠ The cache holds the RESULT, not the `Accounts`. Folding the `Err` arm into `Unanswerable`
    // would be one line shorter and would collapse *a store that exists and will not open* into
    // *a store with nothing to say* — the exact failure `vike_secrets::CredentialStore::accounts`'s own
    // doc exists to prevent, and the two arms below answer differently on purpose. The error is
    // stringified only because it is held across frames.
    if state.account_rows.is_none() {
        // The `account` table holds no credential, so the handle's scope withholds nothing here.
        let store = vike_secrets::CredentialStore::open(
            Some(vike_secrets::StoreLocation::new(settings_dir)),
            vike_secrets::CredentialScope::Full,
        );
        state.account_rows = Some(store.accounts().map_err(|e| e.to_string()));
    }
    let rows = match state.account_rows.clone().expect("just resolved above") {
        Ok(vike_secrets::Accounts::Known(rows)) => rows,
        Ok(vike_secrets::Accounts::Unanswerable(why)) => {
            note(
                ui,
                egui::RichText::new(format!(
                    "{why} — so there are no account ROWS to show for {label}, and a credential \
                     belongs to an account row: none can be attached until the store has one."
                ))
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Caption))
                .weak(),
            );
            return;
        }
        Err(e) => {
            // LOUD, never blank: a store that EXISTS and will not open is a different answer from
            // one with nothing to say, and collapsing the two is the failure
            // `vike_secrets::CredentialStore::accounts`'s own doc exists to prevent.
            note(
                ui,
                egui::RichText::new(format!(
                    "the settings database could not be read, so this panel cannot say which \
                     account rows exist: {e}"
                ))
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Caption))
                .color(ERROR_COLOR),
            );
            return;
        }
    };
    let mine: Vec<vike_secrets::Account> =
        rows.into_iter().filter(|a| a.label.as_deref() == Some(label.as_str())).collect();
    if mine.is_empty() {
        note(
            ui,
            egui::RichText::new(format!(
                "no account ROW in the settings database carries the label {label} yet. One is \
                 created by `vike-cli secrets account add --venue <venue> --tier <tier> --label \
                 {label}` — not from here, because a row belongs to ONE venue and ONE tier and this \
                 strip's selection names neither."
            ))
            .monospace()
            .size(role_px(ui.ctx(), TextRole::Caption))
            .weak(),
        );
        return;
    }

    // The act this frame, if any: `(id, Remove?)`. Collected rather than performed inside the row
    // loop so the store is written once, after the layout, with `&mut state` free.
    let mut act: Option<(i64, AccountRowAct)> = None;
    for row in &mine {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "row {}  {}/{}  {}",
                    row.id,
                    row.venue,
                    row.tier,
                    if row.active { "active" } else { "INACTIVE" }
                ))
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Body)),
            );
            // DEACTIVATE leads, and REMOVE sits beside it rather than instead of it: every consumer
            // of the arming reader already treats `active = 0` exactly as it would treat a deleted
            // row, and the row survives as evidence. It is the REVERSIBLE act, so it performs on
            // one click; the irreversible one below does not.
            let word = if row.active { "Deactivate" } else { "Activate" };
            if ui
                .button(egui::RichText::new(word).size(role_px(ui.ctx(), TextRole::Body)))
                .clicked()
            {
                act = Some((row.id, AccountRowAct::SetActive(!row.active)));
                state.remove_confirm = None;
            }
            // ⚠ **THE TYPED CONFIRM, and the button does not remove.** A DELETE is the one act on
            // this panel the store cannot put back, and the CLI (`--confirm N`) and the node wire
            // (`AccountRequest::confirm`) both require the operator to type the row id for it. A
            // GUI that deleted on one click would be the surface where the ceremony is cheapest to
            // skip, and it is the surface where a misclick is likeliest.
            //
            // The shape is the one `crates/vike-app-core/src/ui/tool_views/backend_settings.rs`'s
            // `can_save` gave a policy settings write until `docs/decisions/0086` point 7 deleted
            // it there: the box is NEVER pre-filled, because pre-filling reduces the ceremony to a
            // click, *"which is precisely what the contract exists to prevent"*.
            match &mut state.remove_confirm {
                Some((armed, typed)) if *armed == row.id => {
                    ui.label(
                        egui::RichText::new(format!("type {} to remove:", row.id))
                            .monospace()
                            .size(role_px(ui.ctx(), TextRole::Caption))
                            .color(ERROR_COLOR),
                    );
                    ui.add(
                        egui::TextEdit::singleline(typed)
                            .desired_width(connections::REMOVE_CONFIRM_W)
                            .font(egui::TextStyle::Monospace),
                    );
                    let matches = typed.trim() == row.id.to_string();
                    if ui
                        .add_enabled(
                            matches,
                            egui::Button::new(
                                egui::RichText::new("Confirm remove")
                                    .size(role_px(ui.ctx(), TextRole::Body)),
                            ),
                        )
                        .clicked()
                    {
                        act = Some((row.id, AccountRowAct::Remove));
                    }
                    if ui
                        .button(
                            egui::RichText::new("Cancel").size(role_px(ui.ctx(), TextRole::Body)),
                        )
                        .clicked()
                    {
                        state.remove_confirm = None;
                    }
                }
                _ => {
                    if ui
                        .button(
                            egui::RichText::new("Remove").size(role_px(ui.ctx(), TextRole::Body)),
                        )
                        .clicked()
                    {
                        // ARMS the confirm; it does not remove. The buffer starts EMPTY.
                        state.remove_confirm = Some((row.id, String::new()));
                    }
                }
            }
        });
    }
    // One warning icon leads the whole note; the second warning inside it is carried by its words.
    let rule = icons::WARNING.before(
        ui.style(),
        egui::RichText::new(
            "Deactivate is reversible and is the act to reach for: the row and its credential \
             keys stay, and every reader of the arming table already treats an inactive row exactly \
             as it treats a deleted one. Remove DELETES the row, and is REFUSED while it still owns \
             credentials. Remove ARMS a typed confirm rather than performing: the row id has to be \
             typed, exactly as `vike-cli secrets account remove --confirm N` requires it. A \
             RUNNING backend notices neither until it restarts: its arming snapshot is read once, \
             at boot.",
        )
        .monospace()
        .size(role_px(ui.ctx(), TextRole::Caption))
        .weak(),
    );
    note(ui, rule);

    if let Some((id, what)) = act {
        let edit = match what {
            AccountRowAct::Remove => vike_secrets::AccountEdit::Remove { id },
            AccountRowAct::SetActive(active) => vike_secrets::AccountEdit::SetActive { id, active },
        };
        // ⚠ Through `vike_secrets::edit_account_in`, which journals the edit itself — so the write
        // and its ledger record cannot drift apart at the call site, the rule
        // `save_credentials_to_store` holds for the credential half.
        state.row_message = Some(
            match vike_secrets::edit_account_in(
                settings_dir,
                edit,
                vike_secrets::AccountJournal {
                    actor: Actor::Gui,
                    proc: creds.proc.clone(),
                    now_ms: creds.now_ms,
                },
            ) {
                Ok((done, journal_error)) => {
                    if let Some(err) = &journal_error {
                        tracing::error!(
                            error = %err,
                            dir = %err.dir.display(),
                            id,
                            verb = done.verb,
                            "account row edit NOT recorded to the change journal (the row IS \
                             written)"
                        );
                    }
                    if done.changed {
                        format!("account row {id}: {}", done.verb)
                    } else {
                        format!("account row {id}: unchanged — nothing was written")
                    }
                }
                // ⚠ The store's OWN refusal, verbatim and SAFE to render: `vike_secrets`' account
                // arms name ids, venues, tiers and credential NAMES, from statements with no
                // `value` column, and the one that could have echoed an operator-supplied token
                // deliberately does not.
                Err(e) => e.to_string(),
            },
        );
        // ⚠ The act that changes the answer is the act that drops the cache — including on the
        // REFUSED and unchanged branches. A refusal means the store said no, and the rows it said
        // no about are the rows to re-read: this panel must never render a list from before a write
        // it just attempted. Re-resolving costs one open on the NEXT frame, not on every frame.
        state.account_rows = None;
    }
    if let Some(msg) = &state.row_message {
        let color =
            if msg.contains("NOTHING WAS WRITTEN") { ERROR_COLOR } else { CONFIGURED_COLOR };
        note(
            ui,
            egui::RichText::new(msg.as_str())
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Caption))
                .color(color),
        );
    }
}

/// Which act a row's buttons requested this frame. Two variants rather than a `bool`, so a call
/// site cannot read *remove* as *deactivate* — the two differ by whether the row survives.
#[derive(Clone, Copy)]
enum AccountRowAct {
    /// `account.active` — reversible.
    SetActive(bool),
    /// DELETE. Refused by the store while the row still owns credentials.
    Remove,
}
