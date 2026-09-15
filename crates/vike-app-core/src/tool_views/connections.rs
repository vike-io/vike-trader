//! The Connections tool body — **two tabs seated in the window's OWN title bar**, one status line
//! that follows the selection, and one ambient connection strip pinned to the foot.
//!
//! ⚠ **Three things in this file moved on the frame that was captured and judged against the
//! drawn design, and each was a structural divergence rather than a polish item.**
//!
//! 1. The segments were drawn in a ROW UNDER the bar, with the window's name repeated at its
//!    left — so the frame read `Connections | Credentials 0 | Backend` and a judge counted THREE
//!    tabs. There was no third tab: that first item was an inline title label. Both defects have
//!    one cure, because the real title bar already carries the window's name — the segments moved
//!    INTO it ([`title_bar_tabs`], painting into the rect
//!    `vike_app_core::workspace::tool_title_bar` reserved) and the inline label was deleted. The
//!    row of vertical space the design recovers is recovered, and the control is two segments.
//! 2. The hairline the selected chip BREAKS is now the one under the title bar, which is the only
//!    place the break means anything. It is painted here rather than by the bar, because vike's
//!    title bar deliberately draws no divider of its own and no other kind wants one.
//! 3. The ambient strip was in the flow but **never reached the screen**. The mechanism is worth
//!    writing down, and its first sentence is READ OFF THE CODE rather than derived:
//!    `vike_connections::connections_ui`'s two-column arm allocates its rail
//!    `egui::vec2(RAIL_W, ui.available_height())` — ALL of it — so the shipped
//!    `add_space(available_height - STRIP_H)` filler had nothing left to pad with and the strip was
//!    appended one strip-height BELOW the window's own bottom. What follows from there is DERIVED
//!    (from `egui::Resize` sizing an unlatched window to its content, so the rail reads a taller
//!    `available_height` next frame) rather than observed: the window grows, the rail claims the
//!    new height too, and the only thing that stops it is `show_window`'s `constrain_to` clamping
//!    at the desktop edge. What WAS observed is the outcome — no strip on the captured frame.
//!    The body is now laid out inside a rect short by [`strip_reservation`] ([`connections_body`]),
//!    so the row cannot be eaten, and the strip lands in the flow — which is what makes the window
//!    auto-size to INCLUDE it. ⚠ That reservation was itself a CONSTANT on the first cut and was
//!    short by about a third of what [`ambient_strip`] draws, so the row was reserved and the strip
//!    still crossed the window floor; [`strip_reservation`] is where the second half of this defect
//!    is argued.
//!
//! # What this window is, after the redesign
//!
//! It used to be FOUR stacked sections on one scroll: the credential grid, the Backends picker,
//! the Add/Edit form, and the Backend-settings table. Nothing said which of them was the one you
//! came for, the address of the live connection was rendered twice (in the picker row and again
//! in the settings section's header, where a reconnect could leave the two disagreeing), and the
//! settings table's one honest hazard — that a file write is inert while the environment sets the
//! key — was rendered nowhere at all.
//!
//! It is now:
//!
//! * **A segmented control seated IN the title bar** ([`title_bar_tabs`] over [`tab_bar`]) —
//!   `Credentials` and `Backend`, each with a live count and, for Backend, an amber dot when the
//!   node reports a key that is set and read by nothing. The SELECTED segment is filled with the
//!   panel's own background (the same `palette::BG` the title bar stands on, so the fill is not
//!   what marks it) and carries only THREE edges; its missing fourth edge is the gap it makes in
//!   the hairline under the bar, so the chip reads as continuous with the panel below it. There
//!   are exactly TWO segments and the window's name is the title bar's, not a third chip.
//! * **ONE status line** ([`status_line`]) that follows the selection: the ACTIVE tab's state
//!   spelled out and unlabelled, the INACTIVE tab's digested beside it, dimmer and second.
//!   Switching swaps which is which, so neither tab's state is ever hidden behind the other.
//! * **Tab 1 — Credentials**: `vike_connections::connections_ui`, a venue rail plus a per-venue
//!   detail pane that spells every `.env` key name out. That crate owns the credential plane
//!   entirely, including the ONE sanctioned write.
//! * **Tab 2 — Backend**: the registry rows and their editor, then
//!   [`super::backend_settings::backend_settings_section`] — the node's effective settings with an
//!   honest inline editor.
//! * **One ambient strip at the foot** ([`ambient_strip`]), IDENTICAL on both tabs: the live
//!   connection, its address, whether control is armed, and Disconnect / Add backend. It is
//!   process-level state you glance at, like a status bar, and belongs to neither tab — which is
//!   what removes the duplicated address.
//!
//! # ⚠ Where every number comes from
//!
//! Nothing here counts anything it cannot see.
//!
//! * The **Credentials** badge and its status line are
//!   `vike_connections::credential_summary` over the grid the panel is ACTUALLY showing — asked
//!   for through `vike_connections::shown_account` against the same `Ui`, so the count is this
//!   frame's rather than last frame's. `venues` is that grid's row count; `set` counts (venue,
//!   tier) cells whose every key is present in the store. Neither is a claim about arming, and
//!   the status line says `this machine` because the credential store is local — the backend's
//!   own store is not readable from here at all.
//!   ⚠ **And `set` is an `Option`, for the reason the Backend badge's already was.** The
//!   credential loader is infallible: a store that EXISTS and cannot be opened logs
//!   `tracing::error!` and returns an EMPTY map, which is byte-identical to an absent store. Folded
//!   blind, this badge would read a confident `0 set` about a file nothing ever read — a
//!   permissions bug wearing the not-configured answer, which the root `CLAUDE.md`'s
//!   "Credentials & the live gate" forbids by name. `vike_connections::StoreHealth` is threaded in
//!   from the binary's one credential read, the badge then renders no number and carries the amber
//!   dot, and the status line says the store was not opened.
//! * The **Backend** badge and digest are [`super::backend_settings::BackendDigest`], a fold over
//!   the rows the node actually sent. In every state where the node has NOT answered — no
//!   backend, a fetch in flight, a node predating the capability, a fault — the badge renders NO
//!   number and the digest says which of those it is.
//!
//! ⚠ [`tab_bar`], [`title_bar_tabs`], [`status_line`], [`ambient_strip`], [`credentials_line`] and
//! [`connections_body`] are `pub` so `crates/vike-app-core/tests/connections_tabs.rs` can drive
//! each one on its own `Harness`. [`connections_tool_content`] takes a whole [`ToolCtx`], which a
//! headless suite cannot reasonably build — and a chrome whose only entry point needs one is a
//! chrome nothing gates, which is the state `vike-desktop`'s CI exclusion makes permanent rather
//! than temporary. [`connections_body`] is the seam that answer produced: it is the WHOLE window
//! frame (segments, status line, the tab's body through a closure, the foot strip) with the one
//! parameter a harness cannot build replaced by that closure, so the composition — not just each
//! piece — is what the tests run.

use super::ToolCtx;
use super::backend_settings::{
    BackendDigest, BackendSettingsState, SettingsEditState, SettingsFilter, SettingsWriteRequest,
    backend_settings_section, should_fetch_settings,
};
use crate::backend_conn::{BackendAction, click_action, picker_rows};
use crate::backend_editor::{self, BackendForm, EditorState, FormError, KeyPresence};
use crate::backend_registry::BackendsFile;
use std::collections::HashMap;
use vike_ui_theme::palette;

/// Which of the window's two tabs is showing. Lives on `crate::tools::ToolView` (per window, like
/// `data_subtab`) rather than in egui temp memory, because the ambient strip's **Add backend**
/// switches it — a cross-widget effect needs state the shell can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionsTab {
    /// The venue credential rail + detail pane.
    #[default]
    Credentials,
    /// The backend registry and the connected node's effective settings.
    Backend,
}

/// The backend picker's read-only inputs (split-plane B1), grouped the way [`ToolCtx`] groups the
/// grid's: the loaded registry, the ACTIVE connection's record (registry entry or the `--observe`
/// synthetic one), and whether switching is available at all
/// (`crate::backend_conn::switching_available` — `false` while a local core runs; B2).
pub struct BackendPicker<'a> {
    /// The loaded `backends.json` registry.
    pub backends: &'a BackendsFile,
    /// The live connection's record, if any.
    pub active: Option<&'a crate::backend_registry::BackendRecord>,
    /// `false` ⇒ the rows render but the buttons are inert (a local core runs — B2 territory).
    pub available: bool,
}

/// The band [`ambient_strip`]'s leading rule costs. ⚠ It is PASSED to `egui::Separator::spacing`
/// rather than inherited: egui's own default lives inside `Style::separator_style` (a hard-coded
/// `6.0` in `egui-0.36.1/src/widget_style.rs`, reachable only through a `Classes`/`WidgetState`
/// pair), and a reservation computed from a number the widget might not be using is the defect
/// this constant exists to close.
const STRIP_SEPARATOR_H: f32 = 6.0;

/// The point size of [`ambient_strip`]'s BUTTONS — the tallest thing in its row, and therefore the
/// one [`strip_reservation`] has to lay out to know how tall the row is.
const STRIP_TEXT_PT: f32 = 11.0;

/// The tab chips' text size. Named because [`segment`] lays it out TWICE — once to measure the row
/// the chip has to fit its margins around, once to render it — and a chip measured at one size and
/// drawn at another is exactly the disagreement that put a chip below the hairline.
const CHIP_TEXT_PT: f32 = 12.0;

/// The longest label in [`ambient_strip`]'s row, laid out for real by [`strip_reservation`]. Its
/// text is irrelevant to the HEIGHT (every glyph shares the font's row height); it is named rather
/// than invented so the measurement reads as "the button this strip actually draws".
const STRIP_TALLEST_LABEL: &str = "Add backend";

/// **What [`connections_body`] must hold back for the foot strip — MEASURED, not pinned.**
///
/// ⚠ This replaced a `const STRIP_H: f32 = 22.0` that under-stated what [`ambient_strip`] draws by
/// about a third, and nothing checked it. The strip is a `Separator` plus one `item_spacing.y` gap
/// plus a wrapped row whose tallest child is a `Button`; against the shipped app's own style
/// (`vike-desktop`'s `main.rs` sets `button_padding = (8, 4)` and `item_spacing = (6, 4)`, not
/// egui's `(4, 1)` / `(8, 3)`) that row alone is taller than the whole reservation was. The strip
/// therefore crossed the window floor and was clipped — the SECOND way this window lost its foot
/// strip, after the one the module doc describes.
///
/// So the number is computed from the live `Ui` on every frame:
///
/// * [`STRIP_SEPARATOR_H`], which this file hands the separator rather than guessing;
/// * `spacing.item_spacing.y`, the flow's own gap before the row;
/// * the row: a non-`small` `Button`'s height is its text's row height plus twice
///   `spacing.button_padding.y`, floored at `spacing.interact_size.y` — `egui-0.36.1`'s
///   `Button::atom_ui` (`min_size.y.at_least(interact_size.y)`, and a frame whose inner/outer
///   margins cancel the hover expansion, leaving `button_padding` as the whole vertical cost).
///
/// …and then raised to whatever the strip ACTUALLY measured last frame, which is the arm that
/// survives a strip that WRAPS (`horizontal_wrapped` on a narrow window puts the Disconnect/Add
/// buttons on a second line, and no single-row arithmetic can see that coming). The seed above is
/// exact for the unwrapped case, so the remembered value only ever raises the reservation and the
/// pair converges in one frame — [`connections_body`] asks for a repaint precisely when it does
/// not, so a wrapped strip is clipped for one frame and never for two.
///
/// `pub` so `crates/vike-app-core/tests/connections_tabs.rs` can compare it against a strip it
/// DREW, rather than against a second copy of a number.
#[must_use]
pub fn strip_reservation(ui: &egui::Ui) -> f32 {
    let (gap, pad_y, floor) = {
        let sp = ui.spacing();
        (sp.item_spacing.y, sp.button_padding.y, sp.interact_size.y)
    };
    let text = ui
        .painter()
        .layout_no_wrap(
            STRIP_TALLEST_LABEL.to_owned(),
            egui::FontId::proportional(STRIP_TEXT_PT),
            egui::Color32::PLACEHOLDER,
        )
        .size()
        .y;
    let row = (text + 2.0 * pad_y).max(floor);
    (STRIP_SEPARATOR_H + gap + row).max(remembered_strip_height(ui))
}

/// Where [`connections_body`] remembers the strip's real height between frames. Keyed on the
/// BODY's own `ui.id()` — the same id `vike_connections::shown_account` and `connections_ui` key
/// their per-window state on, so a second Connections window remembers its own strip.
fn strip_height_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("ambient_strip_height")
}

/// The last height [`ambient_strip`] actually occupied in this window, or `0.0` on the first frame
/// (when [`strip_reservation`]'s style-derived seed is the whole answer).
fn remembered_strip_height(ui: &egui::Ui) -> f32 {
    ui.ctx().data(|d| d.get_temp::<f32>(strip_height_id(ui))).unwrap_or(0.0)
}

/// **What [`connections_body`] laid out, as geometry.**
///
/// ⚠ Returned rather than merely drawn because the foot strip's failure mode is INVISIBLE to the
/// accessibility tree: a strip pushed past the window's floor still reports every one of its
/// labels, which is exactly how the shipped window passed every test while rendering no strip at
/// all. "Did it fit" is a question about rects, so the rects leave the function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodyLayout {
    /// The window's own content floor, read BEFORE the reservation rewrote `max_rect`.
    pub window_floor: f32,
    /// The row held back for the foot strip this frame ([`strip_reservation`]).
    pub reserved: f32,
    /// Where the strip actually landed, separator included.
    pub strip: egui::Rect,
}

/// The Connections tool body. See the module doc for the shape; the parameter list is unchanged
/// from the four-stacked-sections version because every one of them is still needed — the tabs
/// route them rather than replacing them.
///
/// `vars` is a FRESH credential-store read done by the caller every frame this tool is visible: a
/// per-frame parse of a small file is cheap, and it keeps the rail live after a save (or after the
/// operator edits the store externally and comes back).
///
/// ⚠ `tv.connections_account` is `take`n, so the QA capture arm's preselection fires on exactly
/// ONE frame; it is ALSO handed to `shown_account` first (by reference) so the chrome's count
/// describes the account the panel is about to show rather than the one it showed last frame.
/// `slot` is the rect `vike_app_core::workspace::tool_title_bar` reserved between the window's
/// title and its `─ □ ✕` controls, handed back down so the segments are painted INSIDE the bar —
/// see [`title_bar_tabs`], and the module doc for why the row they used to occupy is gone.
#[allow(clippy::too_many_arguments)] // the tool seam's shape: read-only inputs + one OUT slot each
pub fn connections_tool_content(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut crate::tools::ToolView,
    vars: &HashMap<String, String>,
    health: &vike_connections::StoreHealth,
    slot: &crate::workspace::TitleTabSlot,
    picker: &BackendPicker<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut EditorState,
    registry: &mut Option<backend_editor::RegistryUpdate>,
    settings: &BackendSettingsState,
    settings_refresh: &mut bool,
    settings_edit: &mut SettingsEditState,
    settings_write: &mut Option<SettingsWriteRequest>,
) {
    // ONE grid per account the store holds. On a single-account box `AccountGrids::from_vars`
    // enumerates none, so this is `credential_status(vars)` and the rail renders exactly what a
    // single-account box has always been shown.
    let grids = vike_connections::AccountGrids::from_vars(vars);

    let live: HashMap<String, vike_connections::ConnectionState> = ctx
        .feed_statuses
        .iter()
        .map(|(venue, handle)| {
            let s = handle.lock().unwrap().clone();
            (venue.clone(), vike_connections::parse_feed_status(&s))
        })
        .collect();

    // ⚠ `take` FIRST (the one-shot contract), then hand a REFERENCE to `shown_account` so the
    // chrome above the panel counts the account the panel is about to render, and the owned value
    // to the panel itself.
    let preselect = tv.connections_account.take();
    let account = vike_connections::shown_account(ui, preselect.as_ref());
    let absent;
    let rows: &[vike_connections::VenueCredStatus] = match grids.grid_for(&account) {
        Some(rows) => rows,
        None => {
            absent = grids.absent_grid();
            &absent
        }
    };
    let cred = vike_connections::credential_summary(rows, health);
    let digest = BackendDigest::of(picker.active.is_some(), settings);

    // ⚠ **TAB-INDEPENDENT, and that is the whole point.** This call used to live inside
    // `backend_settings_section`, which the tab branch below draws on ONE tab — while `digest`,
    // three lines up, is rendered on BOTH (the segment's badge and the status line's digested
    // half). A connected backend left on the default Credentials tab therefore stayed `Idle` for
    // ever, and the chrome described a fetch nothing had requested. `should_fetch_settings`'s own
    // doc carries the argument; the scope claim it used to make survives one level up — a fetch
    // still starts only while this tool is being drawn.
    //
    // It is BEFORE the branch rather than after, so a first sight of a backend spends at most the
    // one frame `BackendDigest::NotFetched` describes honestly.
    if should_fetch_settings(picker.active.is_some(), settings) {
        *settings_refresh = true;
    }

    let filter = &mut tv.connections_settings_filter;
    connections_body(
        ui,
        slot,
        &mut tv.connections_tab,
        &account,
        &cred,
        &digest,
        picker,
        action,
        editor,
        |ui, tab, action, editor| match tab {
            ConnectionsTab::Credentials => {
                // ⚠ The SAME `ui` `shown_account` was asked about — see that function's doc, and
                // note that [`connections_body`] reserves the foot strip's row by SHRINKING this
                // `Ui`'s max height rather than by opening a child container, precisely so this
                // call still lands on the id `shown_account` was asked about. Do not introduce a
                // container here.
                vike_connections::connections_ui(
                    ui,
                    &grids,
                    &live,
                    health,
                    ctx.credentials,
                    preselect,
                );
            }
            ConnectionsTab::Backend => backend_tab(
                ui,
                vars,
                picker,
                action,
                editor,
                registry,
                settings,
                settings_refresh,
                settings_edit,
                settings_write,
                filter,
            ),
        },
    );
}

/// **The window frame, with the one thing a headless harness cannot build taken as a closure.**
///
/// Draws, in order: the title-bar segments (into `slot`, which the bar reserved), the status line,
/// the selected tab's `body`, and the ambient strip pinned to the foot. [`connections_tool_content`]
/// is this function plus the credential/backend inputs it needs a [`ToolCtx`] to gather; the split
/// exists so `crates/vike-app-core/tests/connections_tabs.rs` runs the COMPOSITION — that the strip
/// is drawn at all, identically, on both tabs — rather than each piece in isolation, which is the
/// coverage hole the strip's disappearance sat in.
///
/// ⚠ **The body is laid out inside a rect short by [`strip_reservation`], and that is load-bearing
/// rather than tidy.** The credential rail allocates `ui.available_height()` outright, so with the
/// strip merely appended after it there was never any room left and it landed one strip-height
/// below the window's own bottom. The module doc carries the rest of that account and marks which
/// half of it is derived. Bounding the body first makes the row unavailable to the rail; drawing
/// the strip IN THE FLOW afterwards is what makes the window size to include it.
///
/// ⚠ **The reservation is MEASURED, and the first cut of it was a constant that under-stated the
/// strip by about a third** — so the row was reserved, the body was bounded, and the strip still
/// crossed the window floor. [`strip_reservation`] carries the arithmetic; what belongs here is
/// the loop that closes it: this function feeds the strip's REAL height back into that function's
/// memory every frame, and asks for a repaint when the strip did not fit, so a strip that WRAPS
/// (the one case no single-row arithmetic can predict) is short by one frame rather than for ever.
/// The returned [`BodyLayout`] is what lets a headless test compare the drawn strip against the
/// reserved row instead of against a second copy of a number.
///
/// ⚠ **The reservation is `Ui::set_max_height` on the CALLER'S OWN `Ui`, never a child
/// container**, and the difference is not stylistic. `vike_connections::shown_account` is asked on
/// this same `Ui` one frame-step earlier so the chips can carry THIS frame's count, and both it
/// and `connections_ui` key their slot on `ui.id()`. A child `Ui` moves that id (or, forced back
/// to the parent's, registers a second widget under one id) — so the chrome would count one
/// account's grid while the panel rendered another's, and every `TextEdit` cursor and
/// `CollapsingState` inside the body would be discarded once on upgrade. Shrinking the max height
/// changes no id at all.
#[allow(clippy::too_many_arguments)] // the chrome's inputs: what it renders + the OUT slots it writes
pub fn connections_body(
    ui: &mut egui::Ui,
    slot: &crate::workspace::TitleTabSlot,
    tab: &mut ConnectionsTab,
    account: &vike_model::account_keys::AccountLabel,
    cred: &vike_connections::CredentialSummary,
    digest: &BackendDigest,
    picker: &BackendPicker<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut EditorState,
    body: impl FnOnce(&mut egui::Ui, ConnectionsTab, &mut Option<BackendAction>, &mut EditorState),
) -> BodyLayout {
    title_bar_tabs(ui, slot, tab, cred, digest);
    let selected = *tab;

    // Reserve the strip's row BEFORE anything can allocate it — see this function's doc. `bottom`
    // is the window's real floor, remembered because the shrink below rewrites `max_rect`.
    let bottom = ui.max_rect().bottom();
    let spacing = ui.spacing().item_spacing.y;
    let top = ui.cursor().top();
    let reserved = strip_reservation(ui);
    ui.set_max_height((bottom - reserved - spacing - top).max(0.0));
    // ⚠⚠ **`set_max_height` MOVES THE CURSOR, and it moves it UP.** `Placer::set_max_height`
    // unions the new `max_rect` back with the `Ui`'s `min_rect` ("make sure we didn't shrink too
    // much") and then snaps `cursor.min.y` to `max_rect.min.y` — and `min_rect` starts at the
    // `Ui`'s own top, so the cursor lands ABOVE everything already drawn. Measured, not feared:
    // the first cut of this reservation put the status line and the whole tab body back over the
    // title bar, and `the_foot_strip_survives_a_body_that_eats_every_pixel` caught it by reading a
    // body offered MORE height than the window had. Putting the cursor back is what makes the
    // call mean "you have this much room from here".
    let back = top - ui.cursor().top();
    if back > 0.0 {
        ui.add_space(back);
    }
    status_line(ui, selected, account, cred, digest);
    body(ui, selected, action, editor);

    // Pad down to the reserved row, then draw the strip IN THE FLOW. ⚠ Computed against `bottom`
    // rather than against `available_height`, because the shrink above is what `available_height`
    // now reports: a body that OVERFLOWED its bound leaves a NEGATIVE pad here, the strip lands
    // immediately after it, and the window auto-sizes to include it — which is the whole
    // difference from the shipped spelling, where the strip was appended to a body that had
    // already claimed every pixel and so fell off the window's bottom edge.
    let left = bottom - reserved - ui.cursor().top();
    if left > 0.0 {
        ui.add_space(left);
    }
    let strip = ambient_strip(ui, picker, action, tab, editor);

    // ⚠ Close the loop: remember what the strip COST, so next frame's reservation is the strip's
    // own measurement rather than an estimate of it. The repaint is asked for only when the strip
    // OVERFLOWED the row — which is what makes this converge instead of oscillating: the raised
    // reservation does not change the strip's height (its width is the window's either way), so
    // the very next frame fits and asks for nothing. Without the request a wrapped strip would
    // stay clipped until something else happened to repaint the window.
    let measured = strip.height();
    ui.ctx().data_mut(|d| d.insert_temp(strip_height_id(ui), measured));
    if measured > reserved + 0.5 {
        ui.ctx().request_repaint();
    }
    BodyLayout { window_floor: bottom, reserved, strip }
}

// -------------------------------------------------------------------------------------------
// The title-bar segmented control
// -------------------------------------------------------------------------------------------

/// **The segments, painted INTO the window's own title bar, plus the hairline they break.**
///
/// `ui` is the BODY's `Ui` and is used for three things only: its `Context`, its `LayerId`, and
/// its id — nothing is allocated in it, so the body's cursor is exactly where it was. The chips
/// are drawn through a DETACHED `egui::Ui` placed at `slot.tabs`, which is up in the title bar and
/// outside the body's clip rect.
///
/// ⚠ **Why the chips are painted during the BODY pass rather than by the title bar itself.** Their
/// labels carry live counts — `cred` is a fold over the credential grid the panel is ABOUT to
/// render (`vike_connections::shown_account` against this same `Ui`), and `digest` a fold over the
/// rows the node actually sent. The title bar is drawn before any of that exists. Publishing last
/// frame's numbers up to it would put a stale count on the chrome, which is precisely the
/// "plausible number" this window is not allowed to render; reserving a rect and painting into it
/// one step later costs nothing and keeps the count this frame's.
///
/// ⚠ **Interaction still works, and it works for a documented reason.** `tool_title_bar` allocates
/// the whole bar with `Sense::click_and_drag()` for the window move; a widget registered LATER at
/// the same position wins the hit, which is the same rule that already lets `─ □ ✕` beat the drag.
///
/// ⚠ The hairline is painted HERE, not by the bar: vike's tool title bar deliberately draws no
/// bottom divider (`crates/vike-app-core/src/workspace/title_bar.rs`'s `tool_title_bar` says so at
/// itself) and no other window kind wants one. The bar the chip breaks is therefore this window's
/// own, at [`crate::workspace::TitleTabSlot::hairline_y`] — painted under
/// [`crate::workspace::TitleTabSlot::hairline_clip`], NEVER under `slot.bar`, because that clip's
/// scissor bound excludes the line's own pixel row and deletes the entire stroke. That method's
/// doc carries the renderer arithmetic and the gate.
///
/// Returns the [`TabRow`] it drew, so a headless test can ask where the chips actually landed: the
/// detached `Ui`'s clip rect IS `slot.tabs`, so a row wider than the slot is SILENTLY CLIPPED and
/// the accessibility tree — which reports a clipped label exactly as it reports a drawn one —
/// cannot tell the two apart.
pub fn title_bar_tabs(
    ui: &egui::Ui,
    slot: &crate::workspace::TitleTabSlot,
    tab: &mut ConnectionsTab,
    cred: &vike_connections::CredentialSummary,
    digest: &BackendDigest,
) -> TabRow {
    // The slot is only room if the BAR reserved it. `WinKind::carries_title_tabs` is the one
    // decision, made in `crates/vike-app-core/src/workspace/title_bar.rs`'s `tool_title_bar`; a
    // `false` here means this window's title ran the full width and these chips are about to be
    // drawn over it.
    debug_assert!(
        slot.carries_tabs,
        "the title bar reserved no tab slot for this window kind: {slot:?}"
    );
    let row = {
        // ⚠ `Align::Max` in the cross axis — the chips are seated on the slot's BOTTOM, which is
        // the bar's bottom, which is the hairline. A centred row would leave the chip's missing
        // fourth edge floating above the line it is supposed to break.
        let mut chips = egui::Ui::new(
            ui.ctx().clone(),
            ui.id().with("connections_title_tabs"),
            egui::UiBuilder::new()
                .layer_id(ui.layer_id())
                .max_rect(slot.tabs)
                .layout(egui::Layout::left_to_right(egui::Align::Max))
                .style(ui.style().clone()),
        );
        tab_bar(&mut chips, tab, cred, digest, slot.pad)
    };

    // The hairline, BROKEN under the selected chip. Painted on the window's layer, spanning the
    // full window width rather than stopping at the chips' slot — and clipped by
    // `hairline_clip()`, which is the bar plus the line's own pixel row. ⚠ `slot.bar` here would
    // paint nothing at all: see that method's doc.
    let y = slot.hairline_y();
    let stroke = egui::Stroke::new(1.0, palette::BORDER);
    let painter = ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(slot.hairline_clip());
    for (a, b) in hairline_segments(slot.bar.left(), slot.bar.right(), row.selected_span()) {
        painter.hline(a..=b, y, stroke);
    }
    row
}

/// **What [`tab_bar`] drew, as geometry.**
///
/// ⚠ Returned because the chips' two failure modes are both invisible to the accessibility tree.
/// A chip wider than the slot is CLIPPED (the detached `Ui` [`title_bar_tabs`] builds takes
/// `slot.tabs` as its clip rect, per `egui-0.36.1/src/ui.rs`'s `Ui::new`, which sets
/// `clip_rect = max_rect`) and still reports its label; and the selected chip's bottom edge
/// LIFTING off the hairline — the thing the whole control means — is a y coordinate nothing in the
/// tree carries. Both are rect questions, so the rects leave the function.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TabRow {
    /// Every chip's frame rect, in draw order. Two today.
    pub chips: Vec<egui::Rect>,
    /// The SELECTED chip's frame rect — the one whose footprint breaks the hairline.
    pub selected: Option<egui::Rect>,
}

impl TabRow {
    /// The selected chip's horizontal span, as [`hairline_segments`] wants it.
    #[must_use]
    pub fn selected_span(&self) -> Option<(f32, f32)> {
        self.selected.map(|r| (r.left(), r.right()))
    }

    /// The union of every chip — what the row actually occupies, and therefore what has to fit
    /// inside the slot the bar reserved. `None` when no chip was drawn: `Rect::NOTHING` is the
    /// INVERTED (`+inf`..`-inf`) sentinel, so `contains_rect` answers `false` for it and a caller
    /// folding it silently would read "the row does not fit" from a row that does not exist.
    #[must_use]
    pub fn bounds(&self) -> Option<egui::Rect> {
        self.chips.iter().copied().reduce(|acc, r| acc.union(r))
    }
}

/// **The hairline, as the spans that are actually PAINTED** — everything except the selected
/// chip's own footprint.
///
/// Pure, and `pub` for exactly one reason: the break is the entire design idea of this control
/// ("the selected segment becomes continuous with its panel, which is what a tab MEANS"), it is
/// painted rather than laid out, and a painted line reaches no accessibility tree — so a headless
/// suite can gate it only by gating this arithmetic. Reddens on a hairline drawn straight through
/// the chip, and on a "break" that also erases the run to one side of it.
///
/// Returns one span when nothing is selected, two when the chip is interior, and one when the chip
/// is flush against either end of the bar.
#[must_use]
pub fn hairline_segments(
    bar_left: f32,
    bar_right: f32,
    chip: Option<(f32, f32)>,
) -> Vec<(f32, f32)> {
    let Some((a, b)) = chip else { return vec![(bar_left, bar_right)] };
    let mut out = Vec::with_capacity(2);
    if a > bar_left {
        out.push((bar_left, a));
    }
    if b < bar_right {
        out.push((b, bar_right));
    }
    out
}

/// The two tabs as a segmented control, drawn into whatever `Ui` it is handed — in production the
/// detached one [`title_bar_tabs`] places inside the title bar. Returns the [`TabRow`] it drew:
/// every chip's rect, and which one is selected (the span the hairline must be broken at).
///
/// ⚠ **The SELECTED segment is a `Label`, the others are `Button`s** — the same tree-readable
/// selection idiom `vike_connections`' account chips and venue rail use. "Which tab am I on" is
/// answerable from the accessibility tree by ROLE alone, which is how the headless tests read it,
/// and the current tab cannot be re-picked into a no-op frame.
///
/// ⚠ **There are exactly TWO segments and neither is the window's name.** An inline `Connections`
/// label used to lead this row, and on the captured frame it read as a third tab. The window's
/// name is the title bar's job and always was; `crate::workspace::title_bar::title_bar_plan` is
/// what drops it below the breakpoint so two chips still fit at ~400pt.
pub fn tab_bar(
    ui: &mut egui::Ui,
    tab: &mut ConnectionsTab,
    cred: &vike_connections::CredentialSummary,
    digest: &BackendDigest,
    pad: i8,
) -> TabRow {
    let mut row = TabRow::default();
    // ⚠ Captured ONCE, before any segment can write to `tab`. Reading `*tab` per segment would
    // make a click on the first segment change what the SECOND one compares against, so on the
    // click frame neither would render as selected and the hairline would close over the gap —
    // one frame of the chip detaching from its own panel, every time the tab is switched.
    let current = *tab;

    // ⚠ `with_layout(.., Align::Max)`, NOT `ui.horizontal` — the latter hard-codes `Align::Center`
    // in the cross axis, which would centre the chips in the slot and lift their bottom edges off
    // the hairline they are supposed to break. Bottom-seated is the whole geometry.
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Max), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        // ⚠⚠ **`interact_size.y = 0` IS WHAT KEEPS THE CHIPS INSIDE THE SLOT**, and the default
        // put the INACTIVE one a point BELOW the hairline it is drawn against. A non-`small`
        // `egui::Button` floors its height at `spacing.interact_size.y` — 18pt in egui's own style
        // — which with [`segment`]'s margins makes a 25pt chip in a 24pt slot; egui then shoves
        // the over-tall frame DOWN rather than letting it overflow upward
        // (`egui-0.36.1/src/layout.rs`'s `next_frame_ignore_wrap`: "for horizontal layouts we
        // always want to expand down"), so the overflow lands exactly where the hairline is. The
        // SELECTED chip is a `Label`, which has no such floor — so the default also made the two
        // chips different heights. Zeroing the floor gives both the same galley height and lets
        // [`segment`]'s measured margins fill the slot.
        // MEASURED, not feared: `the_chips_fit_the_slot_the_bar_reserved_at_the_narrow_width`
        // reported `chip [[130.3 14.0] - [208.4 39.0]]` against `slot [… 38.0]`.
        ui.spacing_mut().interact_size.y = 0.0;
        // ⚠ `cred.configured` is ALREADY an `Option` and is `None` for a store that would not
        // open — so the Credentials segment obeys the same rule the Backend one does and renders
        // its label alone rather than a `0` nobody measured. The amber dot marks it: something is
        // wrong with the store, which is a finding and not merely a missing number.
        let credentials = segment(
            ui,
            "Credentials",
            cred.configured,
            (!cred.health.is_readable())
                .then_some("the credential store could not be opened — nothing was counted"),
            current,
            ConnectionsTab::Credentials,
            tab,
            pad,
        );
        let backend = segment(
            ui,
            "Backend",
            digest.badge_count(),
            digest.has_finding().then_some("at least one key is set and read by nothing"),
            current,
            ConnectionsTab::Backend,
            tab,
            pad,
        );
        row.chips.push(credentials);
        row.chips.push(backend);
        row.selected = match current {
            ConnectionsTab::Credentials => Some(credentials),
            ConnectionsTab::Backend => Some(backend),
        };
    });
    row
}

/// One segment. Returns its chip's FRAME rect — the fill and the three edges, margins included —
/// which is what [`tab_bar`] folds into a [`TabRow`]: the selected one's span is where the hairline
/// breaks, and every one of them has to fit the slot.
///
/// ⚠ **The vertical margins are MEASURED against the slot rather than pinned**, and the pinned
/// `top: 3, bottom: 4` they replaced is why the chips overflowed. An `egui::Frame` is as tall as
/// its content plus its margins, so a chip's height is a property of the FONT — and the slot's is
/// `TAB_H`, a constant. The two agreed only by luck at one font size; when they disagreed, egui's
/// "expand down" rule (`egui-0.36.1/src/layout.rs`'s `next_frame_ignore_wrap`) pushed the surplus
/// past the slot's BOTTOM, which is precisely where the hairline is — so an over-tall chip does
/// not look slightly wrong, it lands on the wrong side of the line it is supposed to break.
/// Splitting the slot's leftover height 3:4 (the design's own proportion) makes the chip exactly
/// as tall as the slot at any font, which is what puts its bottom edge ON the hairline and keeps
/// both chips the same height.
#[allow(clippy::too_many_arguments)] // a chip: its text, its badge, its identity, and the out-slot
fn segment(
    ui: &mut egui::Ui,
    text: &str,
    count: Option<usize>,
    // ⚠ The FINDING's own sentence, not a flag: both segments can carry this dot and they carry it
    // for different reasons — a backend key read by nothing, a credential store that would not
    // open. A `bool` here gave the Credentials segment the Backend segment's hover.
    amber: Option<&str>,
    current: ConnectionsTab,
    this: ConnectionsTab,
    out: &mut ConnectionsTab,
    pad: i8,
) -> egui::Rect {
    let selected = current == this;
    // ⚠ The count is `Option` and an absent one renders NOTHING — never a `0`. A backend that has
    // not answered has no count, and a zero there would be a number the tool does not have.
    let label = match count {
        Some(n) => format!("{text} {n}"),
        None => text.to_string(),
    };
    let fill = if selected { palette::BG } else { egui::Color32::TRANSPARENT };
    // The slot's leftover height, split the way the pinned margins used to split 7pt: 3 above, the
    // rest below. Both chips carry the same 12pt row, so both land on the same margins and the
    // same height — the fill and the three edges line up across the pair.
    let row_h = ui
        .painter()
        .layout_no_wrap(
            label.clone(),
            egui::FontId::proportional(CHIP_TEXT_PT),
            egui::Color32::PLACEHOLDER,
        )
        .size()
        .y;
    let slack = (ui.max_rect().height() - row_h).clamp(0.0, 64.0);
    let top = (slack * 3.0 / 7.0).round();
    let (top, bottom) = (top as i8, (slack - top).round() as i8);
    let frame = egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin { left: pad, right: pad, top, bottom })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let rich = egui::RichText::new(&label).size(CHIP_TEXT_PT);
            if selected {
                ui.label(rich.strong().color(palette::TEXT));
            } else if ui.add(egui::Button::new(rich.color(palette::TEXT3)).frame(false)).clicked() {
                *out = this;
            }
            if let Some(hover) = amber {
                ui.label(egui::RichText::new("\u{25CF}").size(9.0).color(palette::WARN))
                    .on_hover_text(hover);
            }
        });
    let rect = frame.response.rect;
    if selected {
        // The chip's own three edges — left, top, right — so it reads as a tab whose FOURTH edge
        // is the gap it makes in the hairline below.
        let stroke = egui::Stroke::new(1.0, palette::BORDER);
        let p = ui.painter();
        let y0 = rect.top();
        let y1 = (rect.bottom()).max(y0);
        p.vline(rect.left() + 0.5, y0..=y1, stroke);
        p.vline(rect.right() - 0.5, y0..=y1, stroke);
        p.hline(rect.left()..=rect.right(), y0 + 0.5, stroke);
    }
    rect
}

// -------------------------------------------------------------------------------------------
// The one status line
// -------------------------------------------------------------------------------------------

/// The ACTIVE tab's state spelled out and unlabelled, the INACTIVE one digested beside it, dimmer
/// and second. Switching swaps which is which, so neither tab's state is ever hidden.
pub fn status_line(
    ui: &mut egui::Ui,
    tab: ConnectionsTab,
    account: &vike_model::account_keys::AccountLabel,
    cred: &vike_connections::CredentialSummary,
    digest: &BackendDigest,
) {
    let creds_active = credentials_line(account, cred, true);
    let creds_digest = credentials_line(account, cred, false);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let (active, inactive) = match tab {
            ConnectionsTab::Credentials => (creds_active, digest.line()),
            ConnectionsTab::Backend => {
                (digest.line().replace("Backend settings  ", ""), creds_digest)
            }
        };
        ui.label(egui::RichText::new(active).monospace().size(11.0).color(palette::TEXT2));
        ui.label(egui::RichText::new(inactive).monospace().size(10.0).color(palette::TEXT3));
    });
    ui.add_space(4.0);
}

/// The credentials half of the status line.
///
/// ⚠ `this machine` is literal and load-bearing: the credential store this tool reads and writes
/// is the LOCAL `<project>/settings` one that the binary's own boot walk resolved. The connected
/// backend has its own store on its own box and nothing here can see it — a line that said
/// "backend" or gave no subject at all would invite exactly that misreading.
///
/// `labelled` picks the ACTIVE (spelled out) rendering over the digested one; the only difference
/// is that the digested half names the tab it belongs to.
///
/// ⚠ **`N set of M configurable` is printed only when there is an N.** The credential loader is
/// infallible — a store that exists and cannot be opened logs an error and hands back an EMPTY
/// map, indistinguishable here from an absent one — so folding that into `0 set` would state a
/// measurement this tool never made, which is the fabrication the Backend badge's `Option` already
/// refuses on the other half of this line. `M` still prints: the denominator comes from the WRITE
/// table (`vike_connections::view::edit_fields`) and no store failure can take it away.
pub fn credentials_line(
    account: &vike_model::account_keys::AccountLabel,
    cred: &vike_connections::CredentialSummary,
    active: bool,
) -> String {
    let who = match account.text() {
        None => "this machine".to_string(),
        Some(l) => format!("this machine · account {l}"),
    };
    let counted = match cred.configured {
        Some(n) => format!("{n} set of {} configurable", cred.configurable),
        None => format!(
            "⚠ store unreadable — none of {} configurable cells was measured",
            cred.configurable
        ),
    };
    let body = format!("{who} · {} venues · {counted}", cred.venues);
    if active { body } else { format!("Credentials  {body}") }
}

// -------------------------------------------------------------------------------------------
// Tab 2 — Backend
// -------------------------------------------------------------------------------------------

/// The Backend tab: the registry rows and their add/edit/delete flow, then the connected node's
/// effective settings.
///
/// ⚠ The rows here are the REGISTRY's — records in `backends.json`, one per backend an operator
/// can connect to. The LIVE connection is not rendered again: it is the ambient strip's, once, at
/// the foot of the window. That is the duplication this redesign removes, and it was a real
/// disagreement rather than a cosmetic one — the old settings section's header restated the
/// active backend's name beside a picker row that could have been reconnected since.
#[allow(clippy::too_many_arguments)] // one arm of the tool seam: read-only inputs + OUT slots
fn backend_tab(
    ui: &mut egui::Ui,
    vars: &HashMap<String, String>,
    picker: &BackendPicker<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut EditorState,
    registry: &mut Option<backend_editor::RegistryUpdate>,
    settings: &BackendSettingsState,
    settings_refresh: &mut bool,
    settings_edit: &mut SettingsEditState,
    settings_write: &mut Option<SettingsWriteRequest>,
    filter: &mut SettingsFilter,
) {
    let rows = picker_rows(picker.backends, picker.active);
    if rows.is_empty() {
        ui.label(
            egui::RichText::new(
                "no backends configured — Add backend (in the strip below) creates the first \
                 record, stored in backends.json beside workspace.json",
            )
            .monospace()
            .size(11.0)
            .weak(),
        );
    } else {
        if !picker.available {
            ui.label(
                egui::RichText::new(
                    "a local trading core is running — backend switching is observe-mode only",
                )
                .monospace()
                .size(10.0)
                .weak(),
            );
        }
        for row in rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    egui::RichText::new(if row.is_active { "\u{25CF}" } else { " " })
                        .monospace()
                        .size(11.0)
                        .color(if row.is_active { palette::ACCENT } else { palette::TEXT3 }),
                );
                let name = if row.record.name.is_empty() {
                    "(--observe)"
                } else {
                    row.record.name.as_str()
                };
                ui.label(egui::RichText::new(name).monospace().size(11.0).strong());
                ui.label(
                    egui::RichText::new(&row.record.addr)
                        .monospace()
                        .size(11.0)
                        .color(palette::TEXT2),
                );
                ui.label(
                    egui::RichText::new(if row.record.control {
                        "control armed"
                    } else {
                        "read-only"
                    })
                    .monospace()
                    .size(10.0)
                    .weak(),
                );
                if !row.listed {
                    ui.label(
                        egui::RichText::new("(not in registry)").monospace().size(10.0).weak(),
                    );
                }
                let label = if row.is_active { "Disconnect" } else { "Connect" };
                if ui.add_enabled(picker.available, egui::Button::new(label)).clicked() {
                    *action = Some(click_action(row.record, picker.active));
                }
                // Manage buttons (I2) — registry rows only: the synthetic `--observe` record
                // exists for one process's lifetime and is not the file's to edit or delete.
                // NOT gated on `picker.available`: these edit the FILE, not the connection.
                if row.listed {
                    if ui.button("Edit").clicked() {
                        editor.open_edit(row.record);
                    }
                    if ui.button("Delete").clicked() {
                        editor.request_delete(&row.record.name);
                    }
                }
            });
        }
    }
    backend_editor_ui(ui, editor, vars, picker, registry);

    ui.add_space(8.0);
    ui.separator();
    let active_name =
        picker.active.map(|r| if r.name.is_empty() { "(--observe)" } else { r.name.as_str() });
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

// -------------------------------------------------------------------------------------------
// The ambient connection strip
// -------------------------------------------------------------------------------------------

/// ONE strip, pinned to the foot of the window and IDENTICAL on both tabs: the live connection's
/// dot, its name, its address, whether control is armed, whether the registry lists it, and
/// Disconnect / Add backend.
///
/// ⚠ **It belongs to neither tab, deliberately.** It is process-level state you glance at, like a
/// status bar — and rendering it once removes a duplication that could DISAGREE: the address used
/// to appear both in the picker row and in the Backend settings header, two renderings of one
/// fact that a reconnect between two frames could separate.
///
/// **Add backend** switches to the Backend tab and opens the add form there. The form is the
/// Backend tab's — putting an expanding editor inside a status strip is what a status strip is
/// not — so the click has to take the operator to where it lands, rather than opening a form on a
/// tab they are not looking at.
///
/// ⚠ **Returns the rect it actually occupied**, separator included, and that is the whole of the
/// second foot-strip defect: the row [`connections_body`] holds back used to be a CONSTANT that
/// nothing compared against what this function draws, and it was short by about a third under the
/// shipped app's own style. A measurement that leaves the function is what lets
/// [`strip_reservation`] be right next frame and lets a headless test say "the drawn strip fits
/// the reserved row" instead of re-asserting a number.
///
/// ⚠ The separator's height is PASSED (`STRIP_SEPARATOR_H`) rather than inherited, so the
/// reservation and the widget cannot be reading two different numbers.
pub fn ambient_strip(
    ui: &mut egui::Ui,
    picker: &BackendPicker<'_>,
    action: &mut Option<BackendAction>,
    tab: &mut ConnectionsTab,
    editor: &mut EditorState,
) -> egui::Rect {
    let top = ui.cursor().top();
    let left = ui.max_rect().left();
    ui.add(egui::Separator::default().spacing(STRIP_SEPARATOR_H));
    let row = ui
        .horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(egui::RichText::new("Backend").monospace().size(10.0).color(palette::TEXT3));
            match picker.active {
                Some(record) => {
                    // ⚠ The dot marks the record this process is ATTACHED to, not the link's health.
                    // `BackendPicker` carries no liveness — the observe bridge is a self-healing
                    // reconnect loop, so "attached" survives a momentary drop — and a dot that read as
                    // "up" would be claiming something this side was never told. The hover says so
                    // rather than the dot changing colour on a fact nobody measured.
                    ui.label(
                        egui::RichText::new("\u{25CF}").monospace().size(11.0).color(palette::ACCENT),
                    )
                    .on_hover_text(
                        "the backend this process is attached to — not a liveness check: the observe \
                         bridge reconnects on its own and this side is not told when it does",
                    );
                    let name =
                        if record.name.is_empty() { "(--observe)" } else { record.name.as_str() };
                    ui.label(egui::RichText::new(name).monospace().size(11.0).strong());
                    ui.label(
                        egui::RichText::new(&record.addr).monospace().size(11.0).color(palette::TEXT2),
                    );
                    ui.label(
                        egui::RichText::new(if record.control { "control armed" } else { "read-only" })
                            .monospace()
                            .size(10.0)
                            .weak(),
                    );
                    if !picker.backends.backends.contains(record) {
                        ui.label(
                            egui::RichText::new("(not in registry)").monospace().size(10.0).weak(),
                        );
                    }
                    if ui
                        .add_enabled(
                            picker.available,
                            egui::Button::new(egui::RichText::new("Disconnect").size(11.0)),
                        )
                        .clicked()
                    {
                        *action = Some(BackendAction::Disconnect);
                    }
                }
                None => {
                    ui.label(
                        egui::RichText::new("\u{25CB}").monospace().size(11.0).color(palette::TEXT3),
                    );
                    ui.label(egui::RichText::new("not connected").monospace().size(11.0).weak());
                }
            }
            if ui.button(egui::RichText::new("Add backend").size(11.0)).clicked() {
                // The form lives on the Backend tab; take the operator there rather than opening it
                // where they cannot see it.
                *tab = ConnectionsTab::Backend;
                editor.open_add();
            }
        })
        .response
        .rect;
    // The strip's real extent: from the flow position the separator started at, down to the
    // wrapped row's own floor. ⚠ Read off the ROW rather than computed from the pieces, because
    // `horizontal_wrapped` is the one part of this that cannot be predicted — on a narrow window
    // the Disconnect/Add buttons go to a second line and the strip is a row taller than any
    // single-row arithmetic would say.
    egui::Rect::from_min_max(
        egui::pos2(left, top),
        egui::pos2(row.right().max(left), row.bottom().max(top)),
    )
}

/// The editor body under the Backends rows — RENDER ONLY: reads the [`EditorState`], draws the
/// are-you-sure line or the Add/Edit form, and forwards Save/Confirm through the pure
/// `backend_editor` decisions into the `registry` OUT slot. Click flags are collected first and
/// applied after the `match`, because the state transitions need `&mut EditorState` while the
/// arms hold borrows into it.
fn backend_editor_ui(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    vars: &HashMap<String, String>,
    picker: &BackendPicker<'_>,
    registry: &mut Option<backend_editor::RegistryUpdate>,
) {
    let mut save_clicked = false;
    let mut confirm_clicked = false;
    let mut cancel_clicked = false;
    match editor {
        EditorState::Closed => {}
        EditorState::ConfirmDelete { name } => {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(format!("Delete backend {name:?}?"));
                if backend_editor::delete_disconnects(name, picker.active) {
                    ui.weak("— the ACTIVE backend: deleting disconnects it first");
                }
                confirm_clicked = ui.button("Delete").clicked();
                cancel_clicked = ui.button("Cancel").clicked();
            });
        }
        EditorState::Add { form, errors } => {
            let clicks = backend_form_ui(ui, "Add backend", form, errors, vars, false);
            (save_clicked, cancel_clicked) = clicks;
        }
        EditorState::Edit { original, form, errors } => {
            let defers = backend_editor::edit_defers(original, picker.active);
            let title = format!("Edit backend {original:?}");
            let clicks = backend_form_ui(ui, &title, form, errors, vars, defers);
            (save_clicked, cancel_clicked) = clicks;
        }
    }
    if cancel_clicked {
        editor.cancel();
    } else if save_clicked {
        if let Some(upd) = backend_editor::submit(editor, picker.backends) {
            *registry = Some(upd);
        }
    } else if confirm_clicked
        && let Some(upd) = backend_editor::confirm_delete(editor, picker.backends, picker.active)
    {
        *registry = Some(upd);
    }
}

/// The Add/Edit form body: the five fields, the B7 names-not-values hint, the per-key-name
/// presence indicator, the edit-active deferral line, and the previous Save's refusals. Returns
/// `(save_clicked, cancel_clicked)`.
fn backend_form_ui(
    ui: &mut egui::Ui,
    title: &str,
    form: &mut BackendForm,
    errors: &[FormError],
    vars: &HashMap<String, String>,
    defers: bool,
) -> (bool, bool) {
    ui.add_space(6.0);
    ui.strong(title);
    // B7, restated where the operator is typing: NAMES, never key material.
    ui.weak(backend_editor::KEY_NAME_HINT);
    ui.horizontal(|ui| {
        ui.label("name");
        ui.text_edit_singleline(&mut form.name);
    });
    ui.horizontal(|ui| {
        ui.label("addr");
        ui.text_edit_singleline(&mut form.addr);
        ui.weak("host:port");
    });
    ui.horizontal(|ui| {
        ui.label("observe key name");
        ui.text_edit_singleline(&mut form.observe_key);
        key_presence_badge(ui, backend_editor::key_presence(&form.observe_key, vars));
    });
    ui.horizontal(|ui| {
        ui.label("control key name");
        ui.text_edit_singleline(&mut form.control_key);
        ui.weak("(optional)");
        key_presence_badge(ui, backend_editor::key_presence(&form.control_key, vars));
    });
    ui.checkbox(&mut form.control, "control armed (write channel)");
    if backend_editor::armed_without_key(form) {
        ui.weak("armed but no control key named — the write channel can never mount");
    }
    if defers {
        ui.weak(
            "editing the ACTIVE backend — changes take effect on next connect; \
             the live connection is not rewired",
        );
    }
    for e in errors {
        ui.colored_label(egui::Color32::from_rgb(0xe5, 0x6a, 0x6a), e.to_string());
    }
    let mut clicks = (false, false);
    ui.horizontal(|ui| {
        clicks = (ui.button("Save").clicked(), ui.button("Cancel").clicked());
    });
    clicks
}

/// The typo-catcher's badge — presence only, never a value (`backend_editor::key_presence`'s
/// return type has no value-carrying variant, so this CANNOT print one).
fn key_presence_badge(ui: &mut egui::Ui, presence: KeyPresence) {
    match presence {
        KeyPresence::Blank => {}
        KeyPresence::Present => {
            ui.weak("✓ in store");
        }
        KeyPresence::Absent => {
            ui.weak("⚠ not in store");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_connections::CredentialSummary;
    use vike_tradehub_client::wire::{WireSettingsRow, WireSettingsShow};

    fn summary() -> CredentialSummary {
        CredentialSummary {
            venues: 14,
            configured: Some(15),
            configurable: 34,
            health: vike_connections::StoreHealth::Readable,
        }
    }

    fn row(key: &str, origin: &str, read_by: &str) -> WireSettingsRow {
        WireSettingsRow {
            section: "config.toml".into(),
            key: key.into(),
            value: "v".into(),
            origin: origin.into(),
            read_by: read_by.into(),
        }
    }

    /// ⚠ The status line NAMES THE MACHINE, and the reason is that the credential store is the
    /// LOCAL one while everything else on this window is the backend's. A line with no subject
    /// reads as the backend's, which is a store this tool cannot see at all.
    #[test]
    fn the_credentials_line_says_which_store_it_counted() {
        let line =
            credentials_line(&vike_model::account_keys::AccountLabel::Default, &summary(), true);
        assert!(line.starts_with("this machine · "), "{line}");
        assert!(line.contains("14 venues"), "{line}");
        assert!(line.contains("15 set of 34 configurable"), "the denominator is on screen: {line}");

        let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
        let labelled = credentials_line(&alt, &summary(), true);
        assert!(labelled.contains("account ALT"), "a labelled account is named: {labelled}");

        // The digested rendering names the tab it belongs to; the active one does not.
        let digested =
            credentials_line(&vike_model::account_keys::AccountLabel::Default, &summary(), false);
        assert!(digested.starts_with("Credentials  "), "{digested}");
    }

    /// ⚠ **A store that could not be OPENED renders no count at all.** The loader hands back the
    /// same empty map an absent store gives, so a fold that ignored
    /// `vike_connections::StoreHealth` would print a measured `0 set of 34 configurable` about a
    /// file nothing read — a permissions bug wearing the not-configured answer, which the root
    /// `CLAUDE.md`'s "Credentials & the live gate" forbids by name, rendered as a number.
    ///
    /// Reddens on `credentials_line` reaching for a `usize` again, and on the line dropping the
    /// denominator with the numerator (the denominator is the WRITE table's and is still known).
    #[test]
    fn an_unreadable_store_renders_no_credential_count() {
        let unreadable = CredentialSummary {
            venues: 14,
            configured: None,
            configurable: 34,
            health: vike_connections::StoreHealth::Unreadable("permission denied".into()),
        };
        let line =
            credentials_line(&vike_model::account_keys::AccountLabel::Default, &unreadable, true);
        assert!(!line.contains("0 set"), "a zero is a number this tool does not have: {line}");
        assert!(line.contains("store unreadable"), "{line}");
        assert!(line.contains("34 configurable"), "the denominator is still known: {line}");
        assert!(line.contains("14 venues"), "…and so is the roster: {line}");

        // The MEASURED zero — an absent store — still prints, because it is a measurement.
        let empty = CredentialSummary { configured: Some(0), ..unreadable.clone() };
        let empty = CredentialSummary { health: vike_connections::StoreHealth::Readable, ..empty };
        let line = credentials_line(&vike_model::account_keys::AccountLabel::Default, &empty, true);
        assert!(line.contains("0 set of 34 configurable"), "{line}");
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
            let d = BackendDigest::of(true, &state);
            assert_eq!(d.badge_count(), None, "{state:?} is not a count");
            assert!(!d.has_finding(), "{state:?} cannot have a finding");
        }
        assert_eq!(BackendDigest::of(false, &BackendSettingsState::Idle), BackendDigest::NoBackend);
        assert_eq!(BackendDigest::of(false, &BackendSettingsState::Idle).badge_count(), None);

        let show = WireSettingsShow {
            settings_dir: Some("/srv/vike-<unit>/settings".into()),
            rows: vec![
                row("config.tradehub_addr", "config.toml", "tradehub"),
                row("config.log_dir", "config.toml", "NO"),
                row("flags.reconcile", "default", "tradehub"),
            ],
        };
        let d = BackendDigest::of(true, &BackendSettingsState::Loaded(show));
        assert_eq!(d.badge_count(), Some(2), "two rows a layer names");
        assert!(d.has_finding(), "config.log_dir is set and read by nothing");
        let line = d.line();
        assert!(line.contains("3 keys"), "{line}");
        assert!(line.contains("2 set"), "{line}");
        assert!(line.contains("config.log_dir"), "the finding is NAMED, not just counted: {line}");
    }
}
