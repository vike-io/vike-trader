//! The window-chain rail — a persistent horizontal strip of Polymarket's rolling up/down
//! windows. RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! Polymarket's short-horizon up/down markets are a deterministic CHAIN of fixed-length
//! `window_ts` buckets: each window opens on a boundary (`open = now - now % window`) and closes
//! `window` seconds later, then the next one takes over. A scalper trades the CURRENT window while
//! watching the next few line up, so the rail shows the current window plus the next N, each with
//! a live MM:SS countdown, its Up/Down odds, and its volume — the current window's countdown
//! rendered big, turning the status red in the final ~10 seconds.
//!
//! The window math is [`super::up_down_window`]'s; the egui [`draw_chain_rail`] paints over it,
//! data-in / actions-out like the Trade window's ladder — caller-owned [`CockpitChainRailState`] +
//! borrowed [`CockpitChainRailInputs`] in, a `Vec<CockpitChainRailAction>` out. Up and Down are the
//! market colours the Appearance setting picks (`ThemeTokens::market`); the accent marks only the
//! picked card.

use egui::{Align2, CornerRadius, Rect, Role, Sense, Stroke, StrokeKind, Vec2, WidgetInfo};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{RADIUS, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::cockpit;

use super::up_down_window::{
    CockpitUpDownWindowCard, format_countdown, format_volume, is_urgent, window_open_ms,
};

/// Everything the rail needs to render one frame. Borrowed — the app owns the storage.
pub struct CockpitChainRailInputs<'a> {
    /// the asset label shown on each card (e.g. `BTC` for BTC up/down)
    pub asset: &'a str,
    /// the window cards to draw, left→right (typically the
    /// [`chain_windows`](super::up_down_window::chain_windows) chain's cards)
    pub cards: &'a [CockpitUpDownWindowCard],
    /// window length in seconds — closes each card and picks out the CURRENT window from `now_ms`
    pub window_secs: i64,
    /// current wall-clock, epoch ms — drives every countdown and the current-window highlight
    pub now_ms: i64,
}

/// Cross-frame view state for one rail (owned by the app). Persisted so a manual selection sticks.
#[derive(Clone, Debug, Default)]
pub struct CockpitChainRailState {
    /// index (into `inputs.cards`) of the window the trader selected; `None` ⇒ none pinned yet
    pub selected: Option<usize>,
}

/// An intent leaving the rail. The app maps it to whatever a window selection means for it
/// (routing the ladder/ticket to that window, etc.).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CockpitChainRailAction {
    /// The trader clicked the window card at this index (into `inputs.cards`).
    SelectWindow(usize),
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The rect of card `i` within `region`, `gap` in from the rail's edges and from its neighbour, so
/// paint and hit-test share one layout.
fn card_rect(region: Rect, i: usize, gap: f32) -> Rect {
    let x = region.min.x + gap + i as f32 * (cockpit::CARD_WIDTH + gap);
    Rect::from_min_size(
        egui::pos2(x, region.min.y + gap),
        Vec2::new(cockpit::CARD_WIDTH, region.height() - 2.0 * gap),
    )
}

/// Draw one frame of the window-chain rail and return the actions produced this frame.
///
/// `state` carries the pinned selection across frames; `inputs` is the borrowed per-frame asset +
/// cards + clock. Each card is its own widget, named by what a trader reads on it — the asset and
/// the countdown. The picked card is a Label and every other card a Button (spec §4.2). A click on
/// a Button card selects it (latched into `state`) and emits
/// [`CockpitChainRailAction::SelectWindow`].
///
/// Clicking the card that is ALREADY picked emits nothing: it is a Label and senses no click. It
/// re-emitted `SelectWindow` while the rail was one interaction over the whole strip; the one
/// caller, `vike-app-core`'s cockpit view, drops the rail's actions, so no command changes.
pub fn draw_chain_rail(
    ui: &mut egui::Ui,
    state: &mut CockpitChainRailState,
    inputs: &CockpitChainRailInputs,
) -> Vec<CockpitChainRailAction> {
    let t = ThemeTokens::of(ui.ctx());
    let mut actions: Vec<CockpitChainRailAction> = Vec::new();

    let (region, _) = ui
        .allocate_exact_size(Vec2::new(ui.available_width(), cockpit::RAIL_HEIGHT), Sense::hover());
    ui.painter().rect_filled(region, 0.0, t.theme.background);
    ui.painter().line_segment(
        [region.left_bottom(), region.right_bottom()],
        Stroke::new(stroke::HAIRLINE, t.theme.border),
    );

    let gap = t.metrics.gap;
    let round = CornerRadius::same(RADIUS);
    // A window that is not a positive length is no window: the rail stays an empty strip.
    let Some(current_open) = window_open_ms(inputs.now_ms, inputs.window_secs) else {
        return actions;
    };
    let window_ms = inputs.window_secs * 1000;

    let painter = ui.painter().clone();
    for (i, card) in inputs.cards.iter().enumerate() {
        let cr = card_rect(region, i, gap);
        // clip a card that overflows the visible rail rather than painting outside it
        if cr.min.x >= region.max.x {
            break;
        }
        let is_current = card.open_ms == current_open;
        let is_future = card.open_ms > current_open;
        let selected = state.selected == Some(i);

        // remaining: the CURRENT window counts down to its close; a FUTURE window counts down to
        // its open; a past window (shouldn't normally appear) reads as closed.
        let remaining = if is_current {
            card.open_ms + window_ms - inputs.now_ms
        } else if is_future {
            card.open_ms - inputs.now_ms
        } else {
            0
        };

        // card background + border: the current window is raised onto the card step; the picked
        // card carries the accent's 2 px selection edge, every other card the border rule
        painter.rect_filled(cr, round, if is_current { t.theme.card } else { t.theme.surface });
        let (bw, bcol) = if selected {
            (stroke::EDGE, t.theme.accent)
        } else {
            (stroke::HAIRLINE, t.theme.border)
        };
        painter.rect_stroke(cr, round, Stroke::new(bw, bcol), StrokeKind::Inside);

        // top row: asset label (left) + volume (right)
        painter.text(
            egui::pos2(cr.min.x + gap, cr.min.y + gap),
            Align2::LEFT_TOP,
            inputs.asset,
            t.mono(ThemeTextRole::Caption),
            if is_current { t.theme.text } else { t.theme.text_secondary },
        );
        if let Some(volume) = card.volume {
            painter.text(
                egui::pos2(cr.max.x - gap, cr.min.y + gap),
                Align2::RIGHT_TOP,
                format!("vol {}", format_volume(volume)),
                t.mono(ThemeTextRole::Caption),
                t.theme.text_tertiary,
            );
        }

        // centre: the countdown — big for the current window (the status red in its final
        // stretch), smaller and secondary otherwise
        let cd_col = if is_current && is_urgent(remaining) {
            ThemeStatus::Error.color()
        } else if is_current {
            t.theme.text
        } else {
            t.theme.text_secondary
        };
        let cd_font = if is_current {
            t.mono_semibold(ThemeTextRole::Display)
        } else {
            t.mono(ThemeTextRole::Title)
        };
        let cd_text = if is_future {
            // future windows show time-to-OPEN with a leading + so it reads distinct from a
            // live time-to-close countdown
            format!("+{}", format_countdown(remaining))
        } else {
            format_countdown(remaining)
        };
        // the card's accessible name: what a trader reads on it
        let spoken = format!("{} {cd_text}", inputs.asset);
        painter.text(cr.center(), Align2::CENTER_CENTER, cd_text, cd_font, cd_col);

        // bottom row: Up odds (left) + Down odds (right), in the market set's text colours
        let up = match card.up_price {
            Some(v) => format!("U {v:.2}"),
            None => "U —".to_string(),
        };
        let down = match card.down_price {
            Some(v) => format!("D {v:.2}"),
            None => "D —".to_string(),
        };
        painter.text(
            egui::pos2(cr.min.x + gap, cr.max.y - gap),
            Align2::LEFT_BOTTOM,
            up,
            t.mono(ThemeTextRole::Body),
            maps::side::OUTCOME_UP.text.resolve(&t),
        );
        painter.text(
            egui::pos2(cr.max.x - gap, cr.max.y - gap),
            Align2::RIGHT_BOTTOM,
            down,
            t.mono(ThemeTextRole::Body),
            maps::side::OUTCOME_DOWN.text.resolve(&t),
        );

        // Each card is its own widget, named by what a trader reads on it. The picked card is a
        // LABEL — "which window" is answered by role, and it cannot be picked again — and every
        // other card is a Button (spec §4.2).
        let sense = if selected { Sense::hover() } else { Sense::click() };
        let response = ui.interact(cr, ui.scope_id().with(("chain_card", i)), sense);
        let kind = if selected { Role::Label } else { Role::Button };
        response.widget_info(|| WidgetInfo::labeled(kind, true, &spoken));
        if response.clicked() {
            state.selected = Some(i);
            actions.push(CockpitChainRailAction::SelectWindow(i));
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_defaults_to_no_selection() {
        assert_eq!(CockpitChainRailState::default().selected, None);
    }
}
