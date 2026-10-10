//! The venue rail: its bounded width, the two-column rows, the narrow chip row, the accent mark.

use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::connections;

use vike_model::accounts::account_keys::AccountLabel;

use super::marks::tier_dot;
use crate::status::VenueCredStatus;
use crate::summary::{StoreHealth, TIERS};

// ⚠ **The rail is a BOUNDED column, not a fixed one — and the bounds are the approved design's
// (`minmax(190px, 232px)`).**
//
// It was `const RAIL_W: f32 = 168.0`, and 168 is below the design's own floor: a venue name plus
// three dots with nothing left over, which is what the owner's "ui is disbalanced" read against a
// live capture reported as *cramped*. [`rail_w`] measures the widest venue name in THIS grid and
// clamps the column into this range, so the rail is as wide as its content needs and never wider
// than the design allows — the same derive-don't-pin rule [`name_col_w`] already obeyed for the
// narrow arm's chips, applied to the column that holds them.

/// The gutter between the rail column and the detail pane, and the band the hairline dividing them
/// is painted down the middle of.
///
/// ⚠ **The divider is PAINTED rather than `ui.separator()`, and that is a sizing fact rather than a
/// styling one.** `egui-0.36.1`'s `Separator::ui` takes `ui.available_size_before_wrap()` — for a
/// VERTICAL separator that is the whole remaining HEIGHT — so a separator between these two columns
/// claims every pixel the window has left, exactly as the rail's own
/// `egui::vec2(RAIL_W, ui.available_height())` used to. See [`connections_ui`] for what that cost.
pub(super) const RAIL_GUTTER: f32 = space::XL2;

// ⚠⚠ **TOMBSTONE — `DETAIL_MAX_W` (620.0) and `PANEL_MAX_W` lived here, and they were the WRONG
// HALF of the design.**
//
// The approved grid is `grid-template-columns: minmax(190px, 232px) minmax(0, 1fr)`. The RAIL is
// the bounded column — [`connections::RAIL_MIN_W`]/[`connections::RAIL_MAX_W`], which is right and stays. The DETAIL column
// is `1fr`: it FILLS what is left. Those two constants capped the WHOLE panel, and
// [`connections_ui`] applied the cap to both columns at once, so the pane the design leaves
// unbounded was bounded at 620pt.
//
// MEASURED on a live capture of the thin client against the the CI box daemon, window MAXIMIZED at
// 2560x1600: the content sat in an ~866pt column hugging the top-left, every separator stopping
// a third of the way across, with the foot strip — correctly pinned to the floor — a long way
// below it. That is the SAME "ui is disbalanced" picture the measure was added to fix, produced
// by the opposite mechanism: instead of content demanding a window it did not need, content
// refused to use the window it was given.
//
// ⚠ **The tension, and why filling is safe where a wrapping `Label` was not.** A tool window
// auto-sizes to its content (`egui-0.36.1`'s `Window::show_dyn` builds its `Resize` with
// `.with_stroke(false)` and then `resizable(false)`, so `Resize::end` reports
// `size[d] = last_content_size[d]`), and `Resize::begin` feeds that straight back as
// `desired_size = desired_size.max(last_content_size)`. A child that FILLS `available_width`
// makes `last_content_size.x == desired_size.x`, which is a FIXED POINT of that line — the max
// of a value with itself. A child whose min width EXCEEDS what it was given is the growing case,
// and it is what a wrapping prose `Label` does when its wrap width is the arena. So prose keeps
// its measure ([`connections::NOTE_W`]) and the pane does not need one.
//
// **VERIFIED, not assumed** — `crates/vike-connections/tests/panel/layout.rs`'s
// `the_windows_sizing_loop_converges_with_a_filling_detail_pane` drives the REAL `egui::Resize`
// with the flags `Window` gives it and runs the loop to a fixed point.
//
// The measure a PROSE paragraph in this panel is set in — the store-unreadable banner, the account
// strip's label rule, its empty-account note, its standing no-credential-deletion rule and its
// account-row lines. ⚠ It used to carry a removal INSTRUCTION here too — the widest thing in the
// panel, because it interpolated an ABSOLUTE STORE PATH into a wrapping label. That text is a
// tombstone (`account_rows_block`); the measure it forced is not, and every paragraph that
// replaced it is set in it.
//
// ⚠ **This is the one measure that survived the tombstone above, and the distinction is the whole
// rule: a measure belongs on WORDS, not on a PANE.** A sentence set across a 2300pt pane is the
// text wall this redesign removed; a pane narrowed to a paragraph's width is the column in the
// corner it replaced it with. Applied through [`note`], always as `min(available_width, connections::NOTE_W)`,
// so it can only ever make a line SHORTER than the room there is — it can never push text outside
// the window.

/// The accent's mark under a SELECTED label — a `stroke::EDGE` line one point below the word
/// (`word` is the label's rect), the settings window's own selected-segment idiom. The accent is a
/// SHAPE, never the colour of the word: the word keeps the text ink. Shared by [`rail_row`],
/// [`rail_chips`] and [`account_chip`], so a venue and an account are marked the same way.
pub(super) fn accent_underline(ui: &egui::Ui, t: &Tokens, word: egui::Rect) {
    ui.painter().hline(
        word.x_range(),
        word.bottom() + space::HAIR,
        egui::Stroke::new(stroke::EDGE, t.theme.accent),
    );
}

/// One rail ROW: the venue name plus its three dots in `S D L` order.
///
/// ⚠ **The SELECTED row is a `Label`, not a `Button`** — the same idiom (and the same argument)
/// as [`account_chip`]: "which venue am I looking at" stays answerable from the accessibility
/// tree by ROLE alone, and the current selection cannot be re-picked into a no-op frame. That is
/// this widget's honest spelling of the mockup's `aria-current`.
///
/// ⚠ `name_w` is a PARAMETER rather than the `const NAME_W: f32 = 86.0` it used to be, for
/// [`name_col_w`]'s reason: 86pt is `hyperliquid` at 12pt monospace with about a point to spare,
/// so the rail was one roster slug away from painting a name over its own first dot. The caller
/// measures it once and hands the SAME value to this row and to the `Venue` header above it —
/// which is the one-constant rule [`connections::DOT_W`] states, kept while the value stops being a constant.
pub(super) fn rail_row(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    name_w: f32,
    selected: bool,
    account: &AccountLabel,
    health: &StoreHealth,
    pick: &mut Option<String>,
) {
    let t = Tokens::of(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = RAIL_ROW_GAP;
        // ⚠ A LEFT-ALIGNED fixed cell, not `add_sized` — that helper lays its widget out
        // centred-and-justified, which would centre each venue name in its column and leave a
        // rail of ragged first letters nobody can scan down.
        ui.allocate_ui_with_layout(
            egui::vec2(name_w, connections::CHIP_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(name_w);
                let name =
                    egui::RichText::new(&row.venue).monospace().size(role_px(ui.ctx(), RAIL_TEXT));
                if selected {
                    let word = ui
                        .add(egui::Label::new(name.strong().color(t.theme.text)).selectable(false))
                        .rect;
                    accent_underline(ui, &t, word);
                } else if ui
                    .add(egui::Button::new(name).frame(false))
                    .on_hover_text("show this venue's credential tiers")
                    .clicked()
                {
                    *pick = Some(row.venue.clone());
                }
            },
        );
        for tier in TIERS {
            tier_dot(ui, row, tier, account, health);
        }
    });
}

/// The rail's row text size in the TWO-COLUMN shape. The gutter between a venue name and its first
/// dot is `space::LG`. Named for [`name_col_w`]'s reason: the column is MEASURED at this size and
/// drawn at it, and a column measured at one size and drawn at another is a column whose dots do
/// not line up.
const RAIL_TEXT: TextRole = TextRole::Strong;

/// The narrow rail's chip text size, and the gap between a chip's own four items. Named because
/// [`name_col_w`] lays the venue names out at this size to MEASURE the name column, and a column
/// measured at one size and drawn at another is a column whose dots do not line up.
const CHIP_TEXT: TextRole = TextRole::Body;
const CHIP_GAP: f32 = space::SM;

/// The name column of either rail: the widest venue name IN THIS GRID, laid out with the real
/// painter at the size that rail draws them at.
///
/// ⚠ **Measured rather than pinned, and derived from the ROWS rather than from
/// [`vike_model::VENUES`]**, for the two reasons this module keeps re-learning. A constant is a
/// per-venue fact written down: a roster gaining a longer slug would paint that name over its own
/// dots, silently, with every test still green. And reading the roster rather than the rows would
/// widen the column for a venue this grid is not showing.
///
/// ⚠ It takes `pt` because BOTH rails now come through it. It was `chip_name_w`, private to the
/// narrow arm, while the two-column arm carried a written-down `const NAME_W: f32 = 86.0` — and
/// that constant was the very fact this function exists to stop anybody writing down.
fn name_col_w(ui: &egui::Ui, rows: &[VenueCredStatus], pt: f32) -> f32 {
    rows.iter()
        .map(|r| {
            ui.painter()
                .layout_no_wrap(
                    r.venue.clone(),
                    egui::FontId::monospace(pt),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
}

/// The two-column rail's width: its measured name column plus its three dot cells, clamped into
/// the approved design's `minmax(190px, 232px)` — see [`connections::RAIL_MIN_W`]/[`connections::RAIL_MAX_W`].
///
/// Returns the COLUMN width and the NAME cell width, because the caller owes the same name width
/// to the header and to every row (the one-constant rule [`connections::DOT_W`] states) and the column width to
/// the gutter the divider is painted in.
pub(super) fn rail_w(ui: &egui::Ui, rows: &[VenueCredStatus]) -> (f32, f32) {
    let name = name_col_w(ui, rows, role_px(ui.ctx(), RAIL_TEXT)) + space::LG;
    let dots = 3.0 * (connections::DOT_W + RAIL_ROW_GAP);
    ((name + dots).clamp(connections::RAIL_MIN_W, connections::RAIL_MAX_W), name)
}

/// The item spacing inside one two-column rail row — set explicitly by [`rail_row`] and read by
/// [`rail_w`], so the width the column is bounded to is the width its rows actually occupy.
pub(super) const RAIL_ROW_GAP: f32 = space::MD;

/// The rail in the NARROW shape: one wrapped chip per venue, the name and its three dots on one
/// line. Same selection rule as [`rail_row`] (selected = a label), same dots, same hovers — only
/// the layout differs, so the accessibility tree a test reads is the same in both shapes.
///
/// ⚠⚠ **EACH CHIP IS A FIXED-SIZE CHILD WITH AN EXPLICIT NON-WRAPPING LAYOUT, and that spelling is
/// the whole of this function.** It used to be `ui.scope(…)`, which INHERITS the parent's layout —
/// and the parent here is `horizontal_wrapped`, whose layout carries `main_wrap: true`. So every
/// chip was itself a wrapping row, opened at the cursor with only the width left on that line
/// (`Ui::new_child`'s `max_rect.unwrap_or_else(|| self.available_rect_before_wrap())`), and once
/// little was left it wrapped its own four items onto four lines. The parent's wrapped row then
/// advanced by that height, leaving the next chip even less width — and it COMPOUNDS.
///
/// What that cost, measured in the shipped 560pt window: the rail grew to ~700pt, the detail pane
/// and every one of its `✏` buttons landed 450-530pt below the window's clip rect, and
/// `egui-0.36.1/src/hit_test.rs` drops a widget whose `interact_rect` (`clip_rect ∩ rect`) is
/// negative. The buttons were all present in the accessibility tree with their labels, so every
/// test stayed green while **clicking the edit control did literally nothing** — the owner-reported
/// defect this function's spelling caused, and the reason
/// `crates/vike-app-core/tests/data_manager_credentials_panel.rs` now clicks a `✏` inside the real
/// window frame rather than at a harness width that only ever reaches the two-column arm.
///
/// `allocate_ui_with_layout` is the cure and not merely a workaround: it hands
/// `Ui::new_child` an explicit `max_rect` AND an explicit layout, so the chip cannot wrap inside
/// itself, while `Layout::next_frame`'s `main_wrap` arm still wraps the PARENT between chips —
/// which is what a wrapped chip row was supposed to mean.
pub(super) fn rail_chips(
    ui: &mut egui::Ui,
    rows: &[VenueCredStatus],
    selected: &str,
    account: &AccountLabel,
    health: &StoreHealth,
    pick: &mut Option<String>,
) {
    let t = Tokens::of(ui.ctx());
    let name_w = name_col_w(ui, rows, role_px(ui.ctx(), CHIP_TEXT));
    let chip_w = name_w + 3.0 * (CHIP_GAP + connections::DOT_W);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::XL;
        for row in rows {
            ui.allocate_ui_with_layout(
                egui::vec2(chip_w, connections::CHIP_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = CHIP_GAP;
                    let name = egui::RichText::new(&row.venue)
                        .monospace()
                        .size(role_px(ui.ctx(), CHIP_TEXT));
                    // A LEFT-ALIGNED fixed cell, the same idiom (and the same argument) as
                    // [`rail_row`]'s: the dots sit in columns across the chips rather than
                    // drifting with each venue name's own advance.
                    ui.allocate_ui_with_layout(
                        egui::vec2(name_w, connections::CHIP_H),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(name_w);
                            if row.venue == selected {
                                let word = ui
                                    .add(
                                        egui::Label::new(name.strong().color(t.theme.text))
                                            .selectable(false)
                                            .wrap_mode(egui::TextWrapMode::Extend),
                                    )
                                    .rect;
                                accent_underline(ui, &t, word);
                            } else if ui
                                .add(
                                    egui::Button::new(name)
                                        .frame(false)
                                        .wrap_mode(egui::TextWrapMode::Extend),
                                )
                                .on_hover_text("show this venue's credential tiers")
                                .clicked()
                            {
                                *pick = Some(row.venue.clone());
                            }
                        },
                    );
                    for tier in TIERS {
                        tier_dot(ui, row, tier, account, health);
                    }
                },
            );
        }
    });
}
