//! Trailing interaction controls painted OVER the price plot after it lays out
//! (chart refactor PR-4): the right (y) + bottom (x) drag-zoom gutters and the
//! bottom-center navigation / scale overlay rows. Each was inline in `draw()`; the bodies
//! are moved verbatim, reading the frame + follow/settings state and writing their
//! result through `&mut` out-params (`navigation_clicked`/`scale_change`), so the render is
//! byte-identical. See `chart/mod.rs`'s `draw`.

use crate::chart::ChartNavigation;
use crate::chart::interact::{ChartFollowLive, ChartFollowLiveAutoYEvent, next_auto_y};
use crate::chart::scale::ChartScaleMode;
use crate::chart::settings::{ChartSettings, ChartSettingsDialog};
use egui::{Align, CursorIcon, Layout, Rect, Sense, UiBuilder, Vec2};
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::button::{ThemeButtonAction, ThemeButtonIcon};
use vike_ui_theme::components::overlay::menu_item;
use vike_ui_theme::components::segmented::{ThemeSegment, segmented};
use vike_ui_theme::components::toggle::{checkbox, radio};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::chart;

/// Right gutter (chart-UX bundle T4): y-drag zoom + dbl-click "back to
/// auto" over the price plot's own vertical span. MUST come after `plot.show`
/// above (ordering invariant): `frame` (the plot-area rect) is only known
/// from `response.transform` once the price plot has actually laid out this
/// frame — native `.allow_axis_zoom_drag`/`.allow_double_click_reset` are
/// disabled on every pane (see the Plot builders above), so this
/// hand-rolled rect is now the ONLY thing that responds to a right-gutter
/// drag/dbl-click.
/// C2b Task 7b: with a secondary price axis active the right gutter is TWO
/// columns wide (primary inner + compare outer); span the y-drag / dbl-click
/// zone across BOTH so a drag anywhere in the price margin still zooms the
/// primary y. INACTIVE ⇒ `chart::Y_AXIS_GUTTER_WIDTH`, byte-identical to before.
///
/// TradingView parity: right-clicking the price axis opens a scale context
/// menu (Auto / Regular / Logarithmic / Percent / Indexed to 100 / Invert). It
/// writes into the SAME `scale_change`/`navigation_clicked` out-params `scale_row`'s hover
/// buttons do — the two paths are equivalent; the hover buttons stay.
/// `requested_scale` is the active (as-requested, pre-fallback) mode, used to
/// radio-check the current item; `requested_invert` is the persisted Invert flag
/// (an orthogonal modifier, not a mode), rendered as a checkbox that emits
/// `invert_change`.
pub(crate) fn y_gutter(
    ui: &egui::Ui,
    frame: egui::Rect,
    sec_axis_active: bool,
    follow: &mut ChartFollowLive,
    requested_scale: ChartScaleMode,
    requested_invert: bool,
    scale_change: &mut Option<ChartScaleMode>,
    invert_change: &mut Option<bool>,
    navigation_clicked: &mut Option<ChartNavigation>,
) {
    let right_gutter_w =
        if sec_axis_active { 2.0 * chart::Y_AXIS_GUTTER_WIDTH } else { chart::Y_AXIS_GUTTER_WIDTH };
    let y_gutter = Rect::from_min_max(
        egui::pos2(frame.right(), frame.top()),
        egui::pos2(frame.right() + right_gutter_w, frame.bottom()),
    );
    let y_response = ui.interact(y_gutter, ui.scope_id().with("y_gutter"), Sense::click_and_drag());
    if y_response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if y_response.dragged() {
        follow.auto_y = next_auto_y(follow.auto_y, ChartFollowLiveAutoYEvent::Drag); // manual y-scale: takes over from autofit
        follow.pending_y_drag += y_response.drag_delta().y;
    }
    if y_response.double_clicked() {
        follow.auto_y = next_auto_y(follow.auto_y, ChartFollowLiveAutoYEvent::DblClick); // restores autofit (native reset is disabled)
    }
    // TradingView-parity right-click scale menu on the price axis, on the kit's menu rows
    // (GUI design system step 7): `menu_item` for "Auto", `radio` for the four scale modes and
    // `checkbox` for "Invert scale" — the same rows `scale_row`'s hover buttons duplicate.
    y_response.context_menu(|ui| {
        // "Auto" = Y-only re-fit (same ChartNavigation::AutoY the Auto hover-button emits);
        // NOT ChartNavigation::Reset, which would also reset the x-range / re-pin follow.
        if menu_item(ui, None, "Auto (fits data to screen)", None).clicked() {
            *navigation_clicked = Some(ChartNavigation::AutoY);
            ui.close();
        }
        ui.separator();
        // Radio-checked against the REQUESTED mode (matches scale_row's active
        // highlight, not the possibly-downgraded effective mode).
        let mut mode = requested_scale;
        for (m, label) in [
            (ChartScaleMode::Linear, "Regular"),
            (ChartScaleMode::Logarithmic, "Logarithmic"),
            (ChartScaleMode::Percent, "Percent"),
            // "Indexed to 100": the Percent twin — rebase first-visible to 100.
            (ChartScaleMode::Indexed, "Indexed to 100"),
        ] {
            if radio(ui, &mut mode, m, label).clicked() {
                *scale_change = Some(m);
                ui.close();
            }
        }
        ui.separator();
        // "Invert scale" is an ORTHOGONAL modifier (flips the y-axis vertically),
        // usable on top of ANY mode — a checkbox, not a radio item. A local `bool`
        // fed to `checkbox` toggles and is compared to emit `invert_change`.
        let mut invert = requested_invert;
        if checkbox(ui, &mut invert, "Invert scale").changed() {
            *invert_change = Some(invert);
            ui.close();
        }
    });
}

/// Bottom gutter (chart-UX bundle T4): x-drag zoom + dbl-click reset,
/// belonging to whichever pane ended up bottom-most this frame (tracked via
/// `bottom_frame` above — price when there are no sub-panes, otherwise the
/// last sub-pane, since only IT renders the shared time axis). MUST come
/// after every `plot.show` above (ordering invariant, same reason as the
/// right gutter): `bottom_frame` is only final once the bottom-most pane
/// has actually laid out this frame.
pub(crate) fn x_gutter(
    ui: &egui::Ui,
    bottom_frame: egui::Rect,
    follow: &mut ChartFollowLive,
    navigation_clicked: &mut Option<ChartNavigation>,
) {
    let x_gutter = Rect::from_min_max(
        egui::pos2(bottom_frame.left(), bottom_frame.bottom()),
        egui::pos2(bottom_frame.right(), bottom_frame.bottom() + chart::AXIS_LABEL_HEIGHT),
    );
    let x_response = ui.interact(x_gutter, ui.scope_id().with("x_gutter"), Sense::click_and_drag());
    if x_response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    if x_response.dragged() {
        follow.pending_x_drag += x_response.drag_delta().x;
    }
    if x_response.double_clicked() {
        *navigation_clicked = Some(ChartNavigation::Reset); // bottom gutter dbl-click reuses the existing Reset path
    }
}

/// T6: a gear (`icons::SETTINGS`) trailing the navigation row opens the "Chart settings" dialog. It
/// is NOT a `ChartNavigation` (no bounds effect) — handled via `toggle_settings` below. Every button in the
/// row is an icon alone, so each is named by what it does (the kit's `ThemeButtonIcon::new(icon, tip)`
/// files `tip` as its accessible name; GUI design system step 7): the words are its accessible
/// name and its hover text.
pub(crate) fn navigation_row(
    ui: &mut egui::Ui,
    frame: egui::Rect,
    settings: &mut ChartSettingsDialog,
    options: &ChartSettings,
    navigation_clicked: &mut Option<ChartNavigation>,
    // Feature #1b (TradingView parity): the price-pane maximize flag
    // (`ChartFollowLive::price_maximized`); the maximize button below toggles it.
    maximized: &mut bool,
) {
    // TradingView parity: the navigation overlay appears only while the pointer is over the price
    // frame — the chart stays clean otherwise (TV has no permanent on-chart toolbar). Only the
    // BUTTONS are hover-gated (below); the child ui is created unconditionally so its id is
    // present in BOTH egui layout passes (see the `new_child` block). Not through a window above.
    let hovered = ui.rect_contains_pointer(frame);
    let mut toggle_settings = false;
    let labels = [
        (icons::ZOOM_OUT, ChartNavigation::ZoomOut, "Zoom out"),
        (icons::ZOOM_IN, ChartNavigation::ZoomIn, "Zoom in"),
        (icons::EARLIER, ChartNavigation::PanLeft, "Earlier"),
        (icons::LATER, ChartNavigation::PanRight, "Later"),
        (icons::RESET_VIEW, ChartNavigation::Reset, "Reset view"),
    ];
    // The row's size comes from the density (spec §4): each control is a `control_height` square,
    // `space::XS` apart — the kit's own icon-button metric, not a hand-picked pixel width.
    let t = ThemeTokens::of(ui.ctx());
    let side = t.metrics.control_height;
    let n = labels.len() + 2; // + the gear and maximize/restore
    let row_w = side * n as f32 + space::XS * (n - 1) as f32;
    let row = Rect::from_min_size(
        egui::pos2(
            frame.center().x - row_w / 2.0,
            frame.bottom() - chart::NAVIGATION_ROW_FROM_BOTTOM,
        ),
        Vec2::new(row_w, side),
    );
    // Drawn via `new_child` (NOT `scope_builder`), so the parent layout cursor is NOT advanced
    // by this overlay — the price plot already moved it, and the sub-panes below must land there
    // regardless. A cursor-advancing builder would shift the panes across egui's two layout
    // passes on a hover transition (the "panes jump on hover" bug). The child ui is created
    // UNCONDITIONALLY (stable id in both passes); only the BUTTONS are hover-gated — otherwise
    // the child's id appears in one pass and not the other and egui flashes a red
    // "changed id between passes" box.
    let mut navigation_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .id_salt("vike_navigation_row")
            .layout(Layout::left_to_right(Align::Center)),
    );
    if hovered {
        let ui = &mut navigation_ui;
        ui.spacing_mut().item_spacing.x = space::XS;
        for (icon, n, tip) in labels {
            if ui.add(ThemeButtonIcon::new(icon, tip)).clicked() {
                *navigation_clicked = Some(n);
            }
        }
        if ui.add(ThemeButtonIcon::new(icons::SETTINGS, "Chart settings")).clicked() {
            toggle_settings = true;
        }
        // Feature #1b: MAXIMIZE fills the chart with the price pane (hides the sub-panes);
        // when maximized it becomes a RESTORE control. Mirrors TradingView's per-pane
        // maximize/restore affordance.
        let (icon, hint) = if *maximized {
            (icons::RESTORE, "Restore panes")
        } else {
            (icons::MAXIMIZE, "Maximize price")
        };
        if ui.add(ThemeButtonIcon::new(icon, hint)).clicked() {
            *maximized = !*maximized;
        }
    }
    // Toggle the dialog (opens next frame; see the top-of-`draw` dialog block).
    // On OPEN, seed the working copy from the COMMITTED options so an edit
    // session always starts from what's on screen.
    if toggle_settings {
        settings.open = !settings.open;
        if settings.open {
            settings.working = options.clone();
        }
    }
}

/// The scale row's segments: the requested scale is the selected one (a Label, spec §4.2).
/// `Indexed to 100`, reachable from the price-axis menu, selects none of them.
const SCALES: [ThemeSegment<'static, ChartScaleMode>; 3] = [
    ThemeSegment { value: ChartScaleMode::Linear, label: "Lin", why: "Price on a linear scale" },
    ThemeSegment {
        value: ChartScaleMode::Logarithmic,
        label: "Log",
        why: "Price on a logarithmic scale",
    },
    ThemeSegment {
        value: ChartScaleMode::Percent,
        label: "%",
        why: "Change from the first visible bar, in percent",
    },
];

/// Log / % scale toggles + Auto (chart-UX bundle T3; on the kit since GUI design system step 7):
/// one segmented `Lin`/`Log`/`%` control plus a secondary `Auto` button, right-aligned at the
/// frame's bottom-right. The selected segment reports the REQUESTED mode (not the possibly-
/// downgraded effective mode), matching `y_gutter`'s price-axis menu radio-check; no market
/// colour marks the active one any more (decision 4).
pub(crate) fn scale_row(
    ui: &mut egui::Ui,
    frame: egui::Rect,
    requested_scale: ChartScaleMode,
    scale_change: &mut Option<ChartScaleMode>,
    navigation_clicked: &mut Option<ChartNavigation>,
) {
    // TradingView parity: the Log/%/Auto scale controls appear only on hover over the price
    // frame — TV keeps them off the chart (scale mode lives in the price-axis right-click menu /
    // Alt+L / Alt+P). Only the BUTTONS are hover-gated; the child ui is created unconditionally
    // (stable id in both layout passes — see the `new_child` block). Not through a window above.
    let hovered = ui.rect_contains_pointer(frame);
    let t = ThemeTokens::of(ui.ctx());
    let right = frame.right() - space::LG;
    let top = frame.bottom() - chart::SCALE_ROW_FROM_BOTTOM;
    let scale_row = Rect::from_min_max(
        egui::pos2(right - chart::SCALE_ROW_MAX_WIDTH, top),
        egui::pos2(right, top + t.metrics.control_height),
    );
    // `new_child` (not `scope_builder`): non-cursor-advancing overlay, so this row never shifts
    // the panes below across egui's layout passes (see `navigation_row`). Child created
    // unconditionally (stable id); only the BUTTONS are hover-gated.
    let mut scale_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(scale_row)
            .id_salt("vike_scale_row")
            .layout(Layout::right_to_left(Align::Center)),
    );
    if hovered {
        let ui = &mut scale_ui;
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        if ui.add(ThemeButtonAction::secondary("Auto")).clicked() {
            // Y-only re-fit (TradingView oracle) — do NOT reuse ChartNavigation::Reset, which also resets
            // cx0/cx1 and re-pins follow.on.
            *navigation_clicked = Some(ChartNavigation::AutoY);
        }
        let mut mode = requested_scale;
        if segmented(ui, &mut mode, &SCALES) {
            *scale_change = Some(mode);
        }
    }
}
