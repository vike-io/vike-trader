//! The account strip above the grid: one chip per account, the Add-account form, the legend.

use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::connections;

use vike_model::accounts::account_keys::{AccountLabel, MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL};

use super::editor::account_rows_block;
use super::marks::{legend_w, mark_legend_items};
use super::rail::accent_underline;
use super::state::EditState;
use super::{CONNECTING_COLOR, ERROR_COLOR, note};
use crate::env_write::CredentialWrite;
use crate::status::AccountGrids;

/// One account chip. The SELECTED one is a label, not a button — so "which account am I looking
/// at" is answerable from the accessibility tree by role alone, and so the current account cannot
/// be re-picked into a no-op frame.
fn account_chip(
    ui: &mut egui::Ui,
    text: &str,
    label: &AccountLabel,
    selected: &AccountLabel,
    pick: &mut Option<AccountLabel>,
) {
    let t = Tokens::of(ui.ctx());
    if label == selected {
        let chip = egui::RichText::new(format!("\u{25CF} {text}"))
            .monospace()
            .size(role_px(ui.ctx(), TextRole::Strong))
            .strong()
            .color(t.theme.text);
        let word = ui.label(chip).rect;
        accent_underline(ui, &t, word);
    } else if ui
        .button(egui::RichText::new(text).monospace().size(role_px(ui.ctx(), TextRole::Strong)))
        .clicked()
    {
        *pick = Some(label.clone());
    }
}

/// **The account dimension: a SELECTOR above the grid, not a third axis inside it.**
///
/// # Why the grid does not grow
///
/// The table below is already the whole bridge roster times three tiers, with a Status column and
/// an edit affordance in every cell. An account dimension has two other homes and both are worse:
///
/// * **More ROWS** (one row per venue per account, the shape the Data Manager's Venues tab uses)
///   turns a fifteen-row table into a fifteen-times-N one in which, on a real box, fourteen out of
///   every fifteen rows are the default account's — the second account is what the operator came
///   for and it is what gets lost. That tab can afford the shape because its rows are ARMING rows:
///   a handful, produced by `vike_mount::venue_arming` from the policy file, not the full roster.
/// * **More COLUMNS** (Sim/Demo/Live per account) is three-times-N columns in a pane that already
///   carries five, and the columns that fall off the right edge are silently unreachable.
///
/// A selector keeps the table's shape CONSTANT in N. That is not only a legibility argument: it is
/// what makes "a box with no labelled account is unchanged" a structural property. With no labelled
/// account there is one chip, it is the one already selected, and every path below composes key
/// names through [`AccountLabel::Default`], which returns them unchanged — so the grid, its
/// tooltips, its forms, the keys it writes and the record it journals are the ones that shipped.
///
/// Hummingbot's is the same shape reached from the other direction: a named account container with
/// credentials submitted against an (account, connector) pair, one account in view at a time
/// (`vike_model::accounts::account_keys`' module doc surveys it, along with the two competitors that answer
/// differently).
///
/// # Creating one
///
/// **Add account** takes a label and nothing else, and it WRITES NOTHING. An account comes into
/// existence when its first credential is saved, which is the same rule the read side already
/// obeys — [`AccountGrids::from_vars`] derives the label set from the key names in the store, so
/// there is no registry a Create could add a row to and nothing that could disagree with the
/// store. Until then the label is selected, its grid reads all-absent, and it is marked as such.
///
/// ⚠ **NOT `vike_model::accounts::account_keys::accounts_in_store`**, and the difference is load-bearing for
/// the venues [`edit_fields`] repaired. That enumerator classifies a key only when it parses as
/// `{VENUE}_{TIER}{SUFFIX}` over the canonical roster and tier lists, so it answers `None` for
/// alpaca's `SANDBOX` tier token and for polymarket's `POLY_` prefix — the exact names this
/// editor's alpaca `DEMO` and polymarket `LIVE` forms now write. Were the strip built on it, an
/// operator could save a labelled credential for either venue, watch its dot light, and find the
/// chip gone on the next load. [`AccountGrids::from_vars`] is built on `split_account_key` plus
/// "does this account's grid light anything" instead, precisely so that cannot happen; its own doc
/// carries the full argument and `crates/vike-connections/tests/key_shapes/account_grids.rs` measures the gap.
///
/// The label is validated by `AccountLabel::parse` BEFORE it is accepted, and the refusal rendered
/// is that validator's own `Display` — see [`EditState::select_typed_account`].
///
/// # Removing one
///
/// **Nothing here deletes a credential, and that is the design.** The root `CLAUDE.md` rule is that
/// nothing in this workspace deletes, moves, truncates or wholesale-rewrites the credential store:
/// it is the operator's only copy of live venue keys, and the one sanctioned write is
/// `vike_secrets::save_credentials_to_store`'s upsert of NAMED keys. An account ROW's Remove act
/// lives in `account_rows_block`, and the store refuses it while that row still owns credentials.
pub(super) fn account_strip(
    ui: &mut egui::Ui,
    grids: &AccountGrids,
    state: &mut EditState,
    // The whole bundle, not just the settings directory: an account-row edit journals through
    // `vike_secrets::edit_account_in_journalled` now, which needs the process identity
    // [`CredentialWrite::proc`] carries alongside the instant `now_ms` always did.
    creds: CredentialWrite<'_>,
) {
    let selected = state.account.clone();
    let mut pick: Option<AccountLabel> = None;
    let mut open_add = false;

    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("Account")
                .monospace()
                .strong()
                .size(role_px(ui.ctx(), TextRole::Strong)),
        );
        // The default account is ALWAYS offered and always first: it is the account a
        // single-account box has, not the absence of one.
        account_chip(ui, "default", &AccountLabel::Default, &selected, &mut pick);
        for label in grids.labels() {
            let text = label.text().unwrap_or_default().to_string();
            account_chip(ui, &text, label, &selected, &mut pick);
        }
        // A label the operator has just NAMED has no keys yet, so it is in no store enumeration —
        // it is rendered from the selection itself, marked, so the strip does not silently drop the
        // account they are in the middle of filling in.
        if grids.grid_for(&selected).is_none()
            && let Some(l) = selected.text()
        {
            let chip = egui::RichText::new(format!("\u{25CF} {l} (new)"))
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Strong))
                .strong()
                .color(CONNECTING_COLOR);
            ui.label(chip);
        }
        if ui
            .button(egui::RichText::new("Add account").size(role_px(ui.ctx(), TextRole::Strong)))
            .clicked()
        {
            open_add = true;
        }
        // ⚠⚠ **THE LEGEND RIDES THIS ROW, right-aligned opposite the chips** — that is the approved
        // design's placement and the cure for a full-width legend band of its own. It is padded
        // rather than flexed: there is no flex spacer in egui, so [`legend_w`] measures the run
        // with the real painter and this pads the row's REMAINING width down to it.
        //
        // ⚠ The `gap` read is what makes the measurement exact rather than approximate — the run
        // is spaced by this row's own `item_spacing.x`, so the width is computed from the number
        // the row is actually using rather than from a second copy of it.
        //
        // ⚠ And when the row does NOT have the room — the 400pt arm — nothing is padded and the
        // run wraps onto the next line, which is what `horizontal_wrapped` does with any item that
        // does not fit. The one thing that may not happen is padding a row too narrow for the run
        // and pushing its tail off the right edge, which is why this is a comparison and not an
        // `add_space` on every frame. `connections::LEGEND_FIT_SLACK` is the rounding margin: the
        // run is laid out glyph by glyph by the same painter that measured it, and a fraction of a
        // point of disagreement would wrap the LAST entry alone onto its own line.
        //
        // ⚠⚠ **`available_rect_before_wrap()`, NOT `ui.available_width()` — that one LIES inside a
        // wrapping row.** `egui-0.36.1`'s `Layout::available_size` has a `main_wrap` arm that
        // returns `vec2(region.max_rect.width(), region.cursor.height())` for a horizontal wrap:
        // the WHOLE row's width, not what is left of it. Padding by that overshoots by everything
        // already on the line, and MEASURED with it the run was pushed past the wrap point and
        // landed at x = 8 on a line of its own — the band this call exists to remove, arrived at
        // through the padding meant to prevent it. `available_rect_before_wrap` is cursor-derived
        // and is the remaining width.
        let gap = ui.spacing().item_spacing.x;
        let run = legend_w(ui, gap);
        let room = ui.available_rect_before_wrap().width();
        let slack = connections::LEGEND_FIT_SLACK;
        if room >= run + slack {
            ui.add_space(room - run - slack);
        }
        mark_legend_items(ui);
    });

    if let Some(label) = pick {
        state.select_account(label);
    }
    if open_add {
        // The two forms are mutually exclusive modes of one panel: leaving a half-typed credential
        // form up while the account it would be written INTO is being renamed is the one state in
        // which a Save could land somewhere the operator is not looking.
        state.close();
        state.add_label = Some(String::new());
        state.label_error = None;
    }

    let mut create = false;
    let mut cancel = false;
    if let Some(buf) = state.add_label.as_mut() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("new account label")
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body)),
            );
            // ⚠ NO `char_limit`: it would TRUNCATE an over-long paste to a legal label the
            // operator did not choose, and they would then write the name they meant into a
            // `policy.accounts.<venue>.<LABEL>` row and find it matching nothing. That is the
            // same repair `AccountLabel::parse` refuses when it declines to uppercase `alt` — a
            // spelling the program fixes on your behalf is a spelling nobody learns. The validator
            // REFUSES, naming the length it was given.
            ui.add(
                egui::TextEdit::singleline(buf)
                    .hint_text("ALT")
                    .desired_width(connections::ALT_LABEL_INPUT_W),
            );
            create = ui.button("Create").clicked();
            cancel = ui.button("Cancel").clicked();
        });
        let rule = format!(
            "A-Z and 0-9 only, at most {MAX_LABEL_LEN} characters, and not \
             `{RESERVED_DEFAULT_LABEL}` (that is the account an unlabelled key already addresses). \
             Creating one writes nothing — the account exists once you save its first credential \
             below."
        );
        note(
            ui,
            egui::RichText::new(rule).monospace().size(role_px(ui.ctx(), TextRole::Caption)).weak(),
        );
    }
    if cancel {
        state.add_label = None;
        state.label_error = None;
    } else if create {
        let text = state.add_label.clone().unwrap_or_default();
        state.select_typed_account(&text);
    }
    if let Some(err) = &state.label_error {
        ui.label(
            egui::RichText::new(err.as_str())
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Caption))
                .color(ERROR_COLOR),
        );
    }

    // The two things only a LABELLED account needs said: that it may hold nothing yet, and how it
    // is removed. A default-account box renders neither, so its panel is the panel that shipped
    // plus the one strip above.
    if let Some(l) = state.account.text() {
        if grids.grid_for(&state.account).is_none() {
            let empty = format!(
                "account {l} holds no credentials in this store yet — the dots below read `not \
                 set` for that reason, not because a key is missing from an account that exists."
            );
            note(
                ui,
                egui::RichText::new(empty)
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .weak(),
            );
        }
        // Removal is refused in the STORE rather than by having no button —
        // `vike_secrets::edit_account` refuses to delete a row that still owns credentials, naming
        // them by key NAME. [`account_rows_block`] carries the whole argument.
        //
        // ⚠ The SETTINGS DIRECTORY is the one the root already resolved (`CredentialHome::resolve`),
        // never a fresh walk: the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-blind, and a panel
        // that showed one project's rows while editing another's is the exact defect
        // `CredentialWrite` exists to close for the credential half.
        account_rows_block(ui, state, creds);
    }
    ui.separator();
}
