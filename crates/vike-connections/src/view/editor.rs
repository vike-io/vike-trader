//! The panel's two store WRITE surfaces: the masked credential form and the account-row acts.

use std::collections::HashMap;
use vike_ui_theme::components::role_px;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::connections;

use vike_model::change_journal::Actor;

use super::state::{EditState, Verdict};
use super::{CONFIGURED_COLOR, ERROR_COLOR, MASKED_FIELD_HINT, note};
use crate::env_write::CredentialWrite;
use crate::keys::{Sensitivity, account_fields, form_note, key_sensitivity};

/// Render the masked edit form for `state.target`, if any. No-op when nothing is open.
///
/// `creds` carries the resolved settings directory and the durable ledger — see [`CredentialWrite`] for why
/// BOTH are parameters rather than walks this function performs for itself.
pub(super) fn render_edit_form(
    ui: &mut egui::Ui,
    state: &mut EditState,
    creds: CredentialWrite<'_>,
    readable: &HashMap<String, String>,
) {
    let Some((venue, env_label)) = state.target.clone() else { return };
    let account = state.account.clone();
    // ⚠ `account_fields`, not `edit_fields`: the KEY NAMES this form writes carry the account, and
    // for the DEFAULT account they are the identical `String`s the venue-wide form always wrote.
    let fields = account_fields(&venue, &env_label, &account);

    // ⚠ **THE READ-BACK, and the whole of it.** Exactly once per opened form, and only for the
    // fields [`key_sensitivity`] classifies [`Sensitivity::Public`]. `readable` is
    // `crate::status::AccountGrids::readable_values` — a map the grid FILTERED through that same
    // classifier when it was built, so a secret's plaintext is not merely skipped here, it was
    // never handed to this function. That is what keeps security rule 1 a structural property
    // rather than a careful one, and it is why the filter lives at the grid rather than here.
    if !state.prefilled {
        for (i, (_, key)) in fields.iter().enumerate() {
            if key_sensitivity(key) == Sensitivity::Public
                && let Some(current) = readable.get(key)
                && let Some(buf) = state.buffers.get_mut(i)
            {
                buf.clone_from(current);
            }
        }
        state.prefilled = true;
    }

    ui.separator();
    // The title names the account only when there IS one to name — `AccountLabel::Default` has no
    // text BY DESIGN (its keys carry no label), so rendering one would invent a spelling that
    // appears in no file, and the default account's heading is byte-identical to before.
    let heading = match account.text() {
        None => format!("Edit {venue} / {}", env_label.to_lowercase()),
        Some(l) => format!("Edit {venue} / {} · account {l}", env_label.to_lowercase()),
    };
    ui.label(
        egui::RichText::new(heading).monospace().strong().size(role_px(ui.ctx(), TextRole::Title)),
    );
    ui.label(
        egui::RichText::new(MASKED_FIELD_HINT).size(role_px(ui.ctx(), TextRole::Caption)).weak(),
    );
    if let Some(note) = form_note(&venue, &env_label) {
        ui.label(egui::RichText::new(note).size(role_px(ui.ctx(), TextRole::Caption)).weak());
    }

    // Two venues pack a field list that is really TWO groups, and a thin divider between them
    // makes that visually obvious. Dukascopy's DEMO form carries two independent demo accounts
    // (DEMO1, DEMO2); cTrader's carries this tier's OAuth grant and then the tier-less Spotware
    // app registration, which is shared by all three of its cells and is the one thing about that
    // form an operator has to notice. Both groups otherwise render through the exact same generic
    // loop below (no special-cased widgets), and every other venue gets no divider at all.
    //
    // ⚠ The index is the LENGTH OF THE FIRST GROUP, so it moves when that group does: dukascopy's
    // DEMO1 group is the login pair (it carried a third field, `_SERVER`, until decision 0095's
    // Task 7 made the server a venue setting), or the break lands inside a group and the panel
    // claims one account's field belongs to the other.
    let group_break = match (venue.as_str(), env_label.as_str()) {
        ("dukascopy", "DEMO") => Some(2),
        ("ctrader", _) => Some(2),
        _ => None,
    };

    for (i, (label, key)) in fields.iter().enumerate() {
        if group_break == Some(i) {
            ui.add_space(space::SM);
            ui.separator();
        }
        ui.horizontal(|ui| {
            ui.add_sized(
                connections::FIELD_LABEL_CELL,
                egui::Label::new(
                    egui::RichText::new(*label)
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Strong)),
                ),
            );
            let buf = state.buffers.get_mut(i).expect("buffers sized to fields in open()");
            // ⚠ MASKED IFF SECRET. `password(true)` is what files the widget as
            // `Role::PasswordInput`, which is the state assistive tech and any tree-reading
            // automation are entitled to speak aloud — right for a key, wrong for a server name
            // the operator is here to READ. [`key_sensitivity`] is the rule, and it fails closed.
            ui.add(
                egui::TextEdit::singleline(buf)
                    .password(key_sensitivity(key) == Sensitivity::Secret)
                    .hint_text(key.as_str())
                    .desired_width(connections::FIELD_INPUT_W),
            );
        });
    }

    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            let updates: Vec<(String, String)> = fields
                .iter()
                .zip(state.buffers.iter())
                .filter_map(|((_, key), value)| {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return None;
                    }
                    // ⚠ A PREFILLED public field the operator did not touch is not an edit. Without
                    // this, every Save of any form would rewrite every readable key it showed and
                    // journal it as a change, which would make the ledger's own record of *what
                    // was rotated* useless exactly when somebody is reading it to find out.
                    if readable.get(key).map(|v| v.trim()) == Some(trimmed) {
                        return None;
                    }
                    Some((key.clone(), trimmed.to_string()))
                })
                .collect();

            if updates.is_empty() {
                state.message = Some(Verdict {
                    is_error: true,
                    text: "nothing entered — no changes saved".to_string(),
                    venue: venue.clone(),
                    tier: env_label.clone(),
                });
            } else {
                // The store write AND its durable record, together — see
                // [`vike_secrets::save_credentials_to_store_journalled`]. `venue` is the grid row
                // and `env_label` the
                // COLUMN the form was opened from, which is what an operator clicked; ⚠ the KEYS are
                // the authority on what was actually written, and for several venues the two
                // genuinely differ: aster's and alpaca's DEMO columns write the bridges' own
                // `ASTER_TESTNET_*` / `ALPACA_SANDBOX_*` vars, dukascopy's DEMO column writes
                // `DUKASCOPY_DEMO1_*`/`DEMO2_*`, and polymarket's LIVE column writes `POLY_*`
                // rather than `POLYMARKET_*`. `edit_fields` is where every one of those mappings
                // lives — deliberately NOT counted here, because a count is exactly the claim this
                // module has already watched rot. The record carries both cells, so neither reading
                // is lost; nothing here re-derives a tier from the key names.
                match vike_secrets::save_credentials_to_store_journalled(
                    creds.settings_dir,
                    vike_secrets::Table::Credential,
                    &updates,
                    Some(&vike_bridge_core::credentials::classify_credential_name),
                    vike_secrets::CredentialJournal {
                        actor: Actor::Gui,
                        venue: &venue,
                        tier: &env_label,
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
                                tier = %env_label,
                                "credential write NOT recorded to the change journal (the keys ARE \
                                 saved)"
                            );
                        }
                        // The console/journald copy STAYS. It is not a duplicate of the ledger — it
                        // is the line an operator tailing the app sees now, and the ledger is what
                        // survives a rotation they will read months later. Both are built from the
                        // same two cells, so they cannot disagree; the ledger additionally carries
                        // the KEY NAMES, which this line has never had and which is exactly what a
                        // `kind`-less "saved credentials" cannot answer.
                        // NEVER log the key/secret/passphrase value — venue/env tier only.
                        //
                        // ⚠ TWO arms rather than one `account = %account` field, and the reason is
                        // the contract this change is held to: the DEFAULT account's line must be
                        // byte-identical to the one that shipped, field for field. `AccountLabel`'s
                        // `Display` renders the default account as the reserved spelling, so a
                        // single always-present field would have added a cell to every
                        // single-account box's log line to say nothing. The label itself is
                        // `[A-Z0-9]` by construction and names an ACCOUNT, never a value.
                        match account.text() {
                            None => tracing::info!(
                                kind = "credential_write",
                                venue = %venue,
                                env = %env_label.to_lowercase(),
                                "saved credentials"
                            ),
                            Some(l) => tracing::info!(
                                kind = "credential_write",
                                venue = %venue,
                                env = %env_label.to_lowercase(),
                                account = %l,
                                "saved credentials"
                            ),
                        }
                        let msg = match account.text() {
                            None => {
                                format!(
                                    "saved credentials for {venue}/{}",
                                    env_label.to_lowercase()
                                )
                            }
                            Some(l) => format!(
                                "saved credentials for {venue}/{} (account {l})",
                                env_label.to_lowercase()
                            ),
                        };
                        state.close();
                        state.message = Some(Verdict {
                            is_error: false,
                            text: msg,
                            venue: venue.clone(),
                            tier: env_label.clone(),
                        });
                        ui.ctx().request_repaint();
                    }
                    Err(e) => {
                        // io::Error's Display is an OS message about the path, never file content.
                        state.message = Some(Verdict {
                            is_error: true,
                            text: format!("save failed: {e}"),
                            venue: venue.clone(),
                            tier: env_label.clone(),
                        });
                    }
                }
            }
        }
        if ui.button("Cancel").clicked() {
            state.close();
        }
    });
}

/// **The `account` ROWS the settings database holds for the selected label, and the two acts a UI
/// may perform on one** — DEACTIVATE (reversible, the one it leads with) and REMOVE.
///
/// # ⚠ The two "accounts" on this screen are different objects, and this block is the seam
///
/// Everything above it reads the credential key-NAME grammar: [`AccountGrids::from_vars`] derives
/// an account from `{BASE}__{LABEL}` and knows nothing about the settings database. The STORE's
/// account is a ROW, with an `id`, a venue, a tier and a book. On a migrated box the two answer
/// differently — the grid shows `default` plus any `__LABEL` accounts, and is blind to dukascopy's
/// two unlabelled rows entirely — and pretending otherwise is how a panel comes to disagree with
/// the process it is attached to. So this block says which object it is about: it renders ROWS,
/// keyed by `id`, read through the same `vike_secrets::resolve_accounts_in` a mount reads.
///
/// # Why DEACTIVATE and REMOVE and not CREATE
///
/// A row needs a `(venue, tier)`, and this strip's selection is venue-INDEPENDENT — one label spans
/// every venue's grid. A Create button here would have to GUESS which venue and which tier the
/// operator meant, and a guessed pair is a row filed against the wrong book. The two acts below
/// need neither: they address a row that EXISTS, by its own id. Creating one is
/// `vike-cli secrets account add --venue V --tier T --label L`, and the note below names it.
///
/// # Removal is refused by the store, not by a missing button
///
/// `vike_secrets::edit_account` REFUSES to delete a row that still owns credentials, naming them by
/// KEY NAME, and the schema's foreign key refuses it again behind that.
/// **Nothing here deletes a credential**, and nothing here rewrites a store.
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
            "nothing here deletes a credential — the store is your only copy of live venue keys, \
             and the one write this app performs is an in-place upsert of the named keys it was \
             asked to save. An account ROW can be removed, and that is a different object: the \
             store REFUSES to delete one while it still owns credentials, naming them by key name.",
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
    // *a store with nothing to say* — the exact failure `vike_secrets::resolve_accounts_in`'s own
    // doc exists to prevent, and the two arms below answer differently on purpose. The error is
    // stringified only because it is held across frames.
    if state.account_rows.is_none() {
        state.account_rows =
            Some(vike_secrets::resolve_accounts_in(settings_dir).map_err(|e| e.to_string()));
    }
    let rows = match state.account_rows.clone().expect("just resolved above") {
        Ok(vike_secrets::Accounts::Known(rows)) => rows,
        Ok(vike_secrets::Accounts::Unanswerable(why)) => {
            note(
                ui,
                egui::RichText::new(format!(
                    "{why} — so there are no account ROWS to show for {label}. On this box an \
                     account IS its credential key names, which the grid below already renders."
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
            // `vike_secrets::resolve_accounts_in`'s own doc exists to prevent.
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
        // ⚠ Through `vike_secrets::edit_account_in_journalled`, never `edit_account_in` directly —
        // so the write and its ledger record cannot drift apart at the call site, the rule
        // `save_credentials_to_store_journalled` already holds for the credential half.
        state.row_message = Some(
            match vike_secrets::edit_account_in_journalled(
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
                // arms name ids, venues, tiers and credential key NAMES, from statements with no
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
