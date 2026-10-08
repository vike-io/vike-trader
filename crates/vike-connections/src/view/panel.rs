//! The body `connections_ui` lays out in its container: banner, account strip, rail, detail pane.

use std::collections::HashMap;
use vike_ui_theme::components::role_px;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::connections;

use vike_model::feed_status::ConnectionState;

use super::detail::venue_detail;
use super::marks::rail_footnote;
use super::rail::{RAIL_GUTTER, RAIL_ROW_GAP, rail_chips, rail_row, rail_w};
use super::state::EditState;
use super::strip::account_strip;
use super::{ABSENT_COLOR, ERROR_COLOR, note};
use crate::env_write::CredentialWrite;
use crate::status::{AccountGrids, VenueCredStatus};
use crate::summary::{FeedFact, StoreHealth};

/// Everything [`connections_ui`] draws, inside the container it draws it in. Returns the venue the
/// operator picked this frame, if any — applied by the caller, after the panel has been laid out,
/// for the reason [`EditState::select_venue`] carries.
pub(super) fn panel_body(
    ui: &mut egui::Ui,
    grids: &AccountGrids,
    live: &HashMap<String, ConnectionState>,
    health: &StoreHealth,
    creds: CredentialWrite<'_>,
    state: &mut EditState,
) -> Option<String> {
    // ⚠ **THE BANNER THAT MUST COME FIRST.** `grids` was folded from the map the loader returned,
    // and that loader is INFALLIBLE: a store that exists and cannot be opened logs an error and
    // returns an EMPTY map, byte-identical to an absent one. Every bool below is then `false` for
    // a reason that is not "no credentials". The dots say `unknown` for themselves
    // (`crate::summary::TierState::Unknown`) — this says WHY, once, at the top, with the fault
    // verbatim, because the per-cell mark cannot carry a path and an OS reason.
    //
    // `SecretsError`'s `Display` is a path and an errno and never file contents (its own doc is
    // the authority), so rendering it here cannot render a credential.
    if let StoreHealth::Unreadable(why) = health {
        // ⚠ [`note`], not `ui.label`: this one interpolates a PATH and an OS reason, so it is the
        // panel's longest sentence in the one state an operator most needs to read it in.
        let unreadable = icons::WARNING.before(
            ui.style(),
            egui::RichText::new(format!(
                "the credential store could not be opened — nothing below was measured: {why}"
            ))
            .monospace()
            .size(role_px(ui.ctx(), TextRole::Body))
            .color(ERROR_COLOR),
        );
        note(ui, unreadable);
        ui.add_space(space::SM);
    }

    // ⚠ The mark legend rides the account strip's own row now (`account_strip` → the
    // `mark_legend_items` call at the end of it), right-aligned opposite the chips. It used to be
    // a `mark_legend(ui)` call HERE — a full-width run plus a full-width sentence, two stacked
    // bands across the top of the panel. `mark_legend_items` carries what that cost.
    account_strip(ui, grids, state, creds);

    // The rows the grid renders are THIS ACCOUNT's. A label with nothing in the store yet — one an
    // operator has just named — renders all-absent rather than the default account's dots, which is
    // `credential_status_for_account`'s own no-borrowing rule carried up into the view.
    let absent;
    let statuses: &[VenueCredStatus] = match grids.grid_for(&state.account) {
        Some(rows) => rows,
        None => {
            absent = grids.absent_grid();
            &absent
        }
    };

    // The selection is stored by NAME, so it survives a reorder and can be repaired when it names
    // a venue this grid does not carry (the default empty string on frame one, or a roster that
    // shrank under a persisted selection). Assigned DIRECTLY rather than through `select_venue`:
    // this is the resolution of an unset selection, not an operator switching venues, and running
    // the switch path would close a form on the frame it was opened.
    if !statuses.iter().any(|s| s.venue == state.venue)
        && let Some(first) = statuses.first()
    {
        state.venue = first.venue.clone();
        state.close();
    }

    let mut pick: Option<String> = None;
    let selected = state.venue.clone();
    let row = statuses.iter().find(|s| s.venue == selected).cloned();

    // ⚠ The two shapes differ in LAYOUT only. Both render the same rail dots with the same hovers
    // and the same selected-row-is-a-Label rule, so an accessibility-tree assertion holds in
    // either — which is what lets the headless tests drive one width and mean both.
    let two_column = ui.available_width() >= connections::RAIL_DETAIL_MIN_W;
    if two_column {
        let (rail_col, name_w) = rail_w(ui, statuses);
        // ⚠⚠ **NOTHING IN THIS ROW MAY CLAIM `available_height()`, and two things used to.**
        //
        // A tool window AUTO-SIZES TO ITS CONTENT — `egui-0.36.1`'s `Window::show_dyn` builds its
        // `Resize` with `.with_stroke(false)` and then `resizable(false)`, so `Resize::end` reports
        // `size[d] = last_content_size[d]` on BOTH axes. A body that claims every pixel it is
        // offered therefore does not merely look greedy: it TELLS THE WINDOW to be that tall, the
        // window grows, the body claims the new height, and the only thing that stops the loop is
        // `show_window`'s `constrain_to` clamping at the arena edge.
        //
        // MEASURED on the owner's capture: a ~1250pt-tall window whose rail ended at y≈600 and
        // whose detail pane ended at y≈400, with the foot strip — correctly pinned to the window's
        // floor by `connections_body`, which is what a status bar is — stranded ~600pt below the
        // last thing it described. The emptiness was never the STRIP's: it was this rail asking
        // for a window it had nothing to put in.
        //
        // So the rail is allocated at its NATURAL height (`0.0` is a desired size, not a bound —
        // egui's `max_rect` clips nothing, and `scope_dyn` advances the parent by the child's own
        // `min_rect`), and the divider between the columns is PAINTED rather than drawn with
        // `ui.separator()`, which takes `available_size_before_wrap()` and would claim the same
        // height right back. See [`RAIL_GUTTER`].
        let row_resp = ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(rail_col, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = RAIL_ROW_GAP;
                        ui.allocate_ui_with_layout(
                            egui::vec2(name_w, connections::RAIL_HEADER_H),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(name_w);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("Venue")
                                            .monospace()
                                            .strong()
                                            .size(role_px(ui.ctx(), TextRole::Caption))
                                            .color(ABSENT_COLOR),
                                    )
                                    .selectable(false),
                                );
                            },
                        );
                        for t in ["S", "D", "L"] {
                            ui.add_sized(
                                [connections::DOT_W, connections::RAIL_HEADER_H],
                                egui::Label::new(
                                    egui::RichText::new(t)
                                        .monospace()
                                        .strong()
                                        .size(role_px(ui.ctx(), TextRole::Caption))
                                        .color(ABSENT_COLOR),
                                )
                                .selectable(false),
                            );
                        }
                    });
                    ui.separator();
                    for s in statuses {
                        rail_row(
                            ui,
                            s,
                            name_w,
                            s.venue == selected,
                            &state.account,
                            health,
                            &mut pick,
                        );
                    }
                    // The rail's own FOOTER — the design's "About these dots", inside the rail
                    // column where it wraps at ~190pt instead of across the whole panel.
                    ui.add_space(space::LG);
                    ui.separator();
                    rail_footnote(ui);
                },
            );
            ui.add_space(RAIL_GUTTER);
            ui.vertical(|ui| {
                if let Some(s) = &row {
                    venue_detail(
                        ui,
                        s,
                        FeedFact::of(&s.venue, live),
                        health,
                        state,
                        creds,
                        grids.readable_values(),
                    );
                }
            });
        });
        // The divider, painted down the middle of the gutter across exactly what the row occupied.
        let rect = row_resp.response.rect;
        if rect.height() > 0.0 {
            let x = rect.left() + rail_col + RAIL_GUTTER * 0.5;
            ui.painter().vline(x, rect.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
        }
    } else {
        rail_chips(ui, statuses, &selected, &state.account, health, &mut pick);
        ui.separator();
        if let Some(s) = &row {
            venue_detail(
                ui,
                s,
                FeedFact::of(&s.venue, live),
                health,
                state,
                creds,
                grids.readable_values(),
            );
        }
    }

    if statuses.is_empty() {
        ui.label(
            egui::RichText::new("no venue rows to show")
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Body))
                .color(ABSENT_COLOR),
        );
    }

    // ⚠ The narrow arm has no rail COLUMN to foot, so the same sentence is a FOOTNOTE here, set in
    // [`connections::NOTE_W`]. It is at the foot rather than under the chips deliberately: a paragraph between
    // the marks and the detail pane is the band this redesign removed, and the foot is reachable
    // now for the same reason the window no longer grows — nothing above it claims the height.
    if !two_column {
        ui.add_space(space::LG);
        ui.separator();
        let w = ui.available_width().min(connections::NOTE_W);
        ui.allocate_ui_with_layout(
            egui::vec2(w, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            rail_footnote,
        );
    }

    pick
}
