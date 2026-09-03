//! The window-chain rail — a persistent horizontal strip of Polymarket's rolling up/down
//! windows. RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! Polymarket's short-horizon up/down markets are a deterministic CHAIN of fixed-length
//! `window_ts` buckets: each window opens on a boundary (`open = now - now % window`) and closes
//! `window` seconds later, then the next one takes over. A scalper trades the CURRENT window while
//! watching the next few line up, so the rail shows the current window plus the next N, each with
//! a live MM:SS countdown, its Up/Down odds, and its volume — the current window's countdown
//! rendered big and escalating to red in the final ~10 seconds.
//!
//! The window math is a pure, unit-tested i64-millisecond core ([`window_open_ms`],
//! [`window_close_ms`], [`chain_windows`], [`fmt_countdown`]); the egui [`draw`] paints over it,
//! data-in / actions-out like the DOM ladder — caller-owned [`ChainRailState`] + borrowed
//! [`ChainRailInputs`] in, a `Vec<ChainRailAction>` out. Colour language matches Polymarket:
//! YES/Up green, NO/Down red.

use egui::{Align2, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind, Vec2};
// The shared dark trading-terminal palette (one pinned home; this file used to hand-copy the
// const block — GUI audit F3).
use vike_ui_theme::palette::trading::{
    ACCENT, DOWN, FAINT, MUTED, PANEL, PANEL2, RULE, TXT, UP, URGENT,
};

const RAIL_H: f32 = 92.0;
const CARD_W: f32 = 118.0;
const CARD_GAP: f32 = 6.0;
const PAD: f32 = 6.0;
/// The current window's countdown turns from amber to red inside this many ms of the close.
const URGENT_MS: i64 = 10_000;

// ---------------------------------------------------------------------------
// Pure window math (unit-tested) — no egui, so the chain law is verifiable.
// ---------------------------------------------------------------------------

/// One window in the chain: the open boundary and its close, both epoch-millisecond timestamps.
/// `close_ms` is `open_ms + window` and is the START of the next window (half-open `[open, close)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    pub open_ms: i64,
    pub close_ms: i64,
}

/// Floor `ts` (epoch ms) to its window-open boundary — Polymarket's law `open = now - now % window`,
/// expressed with `rem_euclid` so negative timestamps floor toward −∞ instead of toward zero.
/// `window_secs` is clamped to at least 1 so a nonsensical zero-length window can never panic the
/// modulo.
pub fn window_open_ms(ts: i64, window_secs: i64) -> i64 {
    let window_ms = window_secs.max(1) * 1000;
    ts - ts.rem_euclid(window_ms)
}

/// The close (= next open) of a window that opened at `open_ms`, epoch ms.
pub fn window_close_ms(open_ms: i64, window_secs: i64) -> i64 {
    open_ms + window_secs.max(1) * 1000
}

/// The chain a scalper watches: the window CURRENT at `now` plus the next `n`, in order
/// (`n + 1` entries total). Boundaries are deterministic, so this is pure arithmetic off `now`.
pub fn chain_windows(now: i64, window_secs: i64, n: usize) -> Vec<Window> {
    let window_ms = window_secs.max(1) * 1000;
    let first = window_open_ms(now, window_secs);
    (0..=n)
        .map(|i| {
            let open = first + i as i64 * window_ms;
            Window { open_ms: open, close_ms: open + window_ms }
        })
        .collect()
}

/// Format a remaining duration as `M:SS`, flooring to whole seconds and clamping any negative
/// (already-closed) value to `0:00`. Minutes are unpadded, seconds zero-padded to two digits.
pub fn fmt_countdown(ms_remaining: i64) -> String {
    let secs = ms_remaining.max(0) / 1000;
    let m = secs / 60;
    let s = secs % 60;
    format!("{m}:{s:02}")
}

/// Is a window with `ms_remaining` to close in its final stretch (the countdown turns red)?
/// True for the closing window over the last [`URGENT_MS`] down to and including the boundary;
/// an already-closed window (negative remaining) is not urgent.
fn is_urgent(ms_remaining: i64) -> bool {
    (0..=URGENT_MS).contains(&ms_remaining)
}

/// Compact volume label: `k`/`M` scaled, matching the terminal's dense style. KEPT local, not
/// `vike_ui_theme::fmt::fmt_compact`, which diverges: uppercase `K`, trims trailing zeros
/// ("2M" vs our "2.0M") and has a `B` tier ("7.1B" vs our "7100.0M") — pinned in the tests.
fn fmt_vol(v: f64) -> String {
    let a = v.abs();
    if a >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if a >= 1_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else {
        format!("{v:.0}")
    }
}

// ---------------------------------------------------------------------------
// Widget types
// ---------------------------------------------------------------------------

/// One window's live snapshot, handed to the rail per frame. Built by the app from its market
/// state (odds/volume are already resolved for the window opening at `open_ms`). Holds f64s, so
/// deliberately NOT `PartialEq`/`Eq`.
#[derive(Clone, Copy, Debug)]
pub struct WindowCard {
    /// window-open boundary, epoch ms (matches [`Window::open_ms`])
    pub open_ms: i64,
    /// YES/Up price (0..1), if known
    pub up_price: Option<f64>,
    /// NO/Down price (0..1), if known
    pub dn_price: Option<f64>,
    /// traded volume for this window, if known
    pub volume: Option<f64>,
}

/// Everything the rail needs to render one frame. Borrowed — the app owns the storage.
pub struct ChainRailInputs<'a> {
    /// the asset label shown on each card (e.g. `BTC` for BTC up/down)
    pub asset: &'a str,
    /// the window cards to draw, left→right (typically the [`chain_windows`] chain's cards)
    pub cards: &'a [WindowCard],
    /// window length in seconds — closes each card and picks out the CURRENT window from `now_ms`
    pub window_secs: i64,
    /// current wall-clock, epoch ms — drives every countdown and the current-window highlight
    pub now_ms: i64,
}

/// Cross-frame view state for one rail (owned by the app). Persisted so a manual selection sticks.
#[derive(Clone, Debug, Default)]
pub struct ChainRailState {
    /// index (into `inputs.cards`) of the window the trader selected; `None` ⇒ none pinned yet
    pub selected: Option<usize>,
}

/// An intent leaving the rail. The app maps it to whatever a window selection means for it
/// (routing the DOM/ticket to that window, etc.).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChainRailAction {
    /// The trader clicked the window card at this index (into `inputs.cards`).
    SelectWindow(usize),
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The left edge x + rect of card `i` within `region`, so paint and hit-test share one layout.
fn card_rect(region: Rect, i: usize) -> Rect {
    let x = region.min.x + PAD + i as f32 * (CARD_W + CARD_GAP);
    Rect::from_min_size(
        egui::pos2(x, region.min.y + PAD),
        Vec2::new(CARD_W, region.height() - 2.0 * PAD),
    )
}

/// Draw one frame of the window-chain rail and return the actions produced this frame.
///
/// `state` carries the pinned selection across frames; `inputs` is the borrowed per-frame asset +
/// cards + clock. A click on a card selects it (latched into `state`) and emits
/// [`ChainRailAction::SelectWindow`].
pub fn draw(
    ui: &mut egui::Ui,
    state: &mut ChainRailState,
    inputs: &ChainRailInputs,
) -> Vec<ChainRailAction> {
    let mut actions: Vec<ChainRailAction> = Vec::new();

    let (region, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), RAIL_H), Sense::hover());
    ui.painter().rect_filled(region, 0.0, PANEL2);
    ui.painter()
        .line_segment([region.left_bottom(), region.right_bottom()], Stroke::new(1.0, RULE));

    let window_ms = inputs.window_secs.max(1) * 1000;
    let current_open = window_open_ms(inputs.now_ms, inputs.window_secs);

    let painter = ui.painter().clone();
    for (i, card) in inputs.cards.iter().enumerate() {
        let cr = card_rect(region, i);
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

        // card background + border (selected → thick amber; current → amber; else the rule line)
        painter.rect_filled(cr, 4.0, if is_current { PANEL } else { PANEL2 });
        let (bw, bcol) = if selected {
            (2.0, ACCENT)
        } else if is_current {
            (1.5, ACCENT)
        } else {
            (1.0, RULE)
        };
        painter.rect_stroke(cr, 4.0, Stroke::new(bw, bcol), StrokeKind::Inside);

        // top row: asset label (left) + volume (right)
        painter.text(
            egui::pos2(cr.min.x + 6.0, cr.min.y + 5.0),
            Align2::LEFT_TOP,
            inputs.asset,
            FontId::new(10.0, FontFamily::Monospace),
            if is_current { TXT } else { MUTED },
        );
        if let Some(vol) = card.volume {
            painter.text(
                egui::pos2(cr.max.x - 6.0, cr.min.y + 5.0),
                Align2::RIGHT_TOP,
                format!("vol {}", fmt_vol(vol)),
                FontId::new(9.0, FontFamily::Monospace),
                FAINT,
            );
        }

        // centre: the countdown — big + escalating for the current window, small/muted otherwise
        let cd_col = if is_current && is_urgent(remaining) {
            URGENT
        } else if is_current {
            ACCENT
        } else {
            MUTED
        };
        let cd_size = if is_current { 24.0 } else { 15.0 };
        let cd_text = if is_future {
            // future windows show time-to-OPEN with a leading + so it reads distinct from a
            // live time-to-close countdown
            format!("+{}", fmt_countdown(remaining))
        } else {
            fmt_countdown(remaining)
        };
        painter.text(
            cr.center(),
            Align2::CENTER_CENTER,
            cd_text,
            FontId::new(cd_size, FontFamily::Monospace),
            cd_col,
        );

        // bottom row: Up odds (green, left) + Down odds (red, right)
        let up = match card.up_price {
            Some(v) => format!("U {v:.2}"),
            None => "U —".to_string(),
        };
        let dn = match card.dn_price {
            Some(v) => format!("D {v:.2}"),
            None => "D —".to_string(),
        };
        painter.text(
            egui::pos2(cr.min.x + 6.0, cr.max.y - 6.0),
            Align2::LEFT_BOTTOM,
            up,
            FontId::new(11.0, FontFamily::Monospace),
            UP,
        );
        painter.text(
            egui::pos2(cr.max.x - 6.0, cr.max.y - 6.0),
            Align2::RIGHT_BOTTOM,
            dn,
            FontId::new(11.0, FontFamily::Monospace),
            DOWN,
        );
    }

    // one interaction over the whole rail; a click hit-tests to the card under the pointer
    let resp = ui.interact(region, ui.id().with("chain_rail"), Sense::click());
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            for i in 0..inputs.cards.len() {
                if card_rect(region, i).contains(pos) {
                    state.selected = Some(i);
                    actions.push(ChainRailAction::SelectWindow(i));
                    break;
                }
            }
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    const W5M: i64 = 300; // a 5-minute up/down window, in seconds
    const W5M_MS: i64 = 300_000;

    #[test]
    fn window_open_floors_to_boundary() {
        // exactly on a boundary stays put
        assert_eq!(window_open_ms(W5M_MS, W5M), W5M_MS);
        assert_eq!(window_open_ms(0, W5M), 0);
        // 5s past the boundary floors back to it
        assert_eq!(window_open_ms(W5M_MS + 5_000, W5M), W5M_MS);
        // one ms short of the next boundary is still the previous window
        assert_eq!(window_open_ms(W5M_MS - 1, W5M), 0);
        // a large epoch-ms timestamp: the open is a multiple of the window and within one window
        let ts = 1_724_000_123_456;
        let open = window_open_ms(ts, W5M);
        assert_eq!(open % W5M_MS, 0);
        assert!(open <= ts && ts - open < W5M_MS);
    }

    #[test]
    fn window_open_negative_ts_floors_toward_neg_infinity() {
        // rem_euclid keeps flooring toward −∞: −1ms belongs to the window [−300000, 0)
        assert_eq!(window_open_ms(-1, W5M), -W5M_MS);
        assert_eq!(window_open_ms(-W5M_MS, W5M), -W5M_MS);
        let open = window_open_ms(-123_456, W5M);
        assert!(open <= -123_456 && -123_456 < window_close_ms(open, W5M));
    }

    #[test]
    fn window_close_is_open_plus_window() {
        assert_eq!(window_close_ms(0, W5M), W5M_MS);
        assert_eq!(window_close_ms(W5M_MS, W5M), 2 * W5M_MS);
        // close of window k == open of window k+1
        let open = window_open_ms(1_724_000_123_456, W5M);
        assert_eq!(window_close_ms(open, W5M), open + W5M_MS);
    }

    #[test]
    fn zero_window_secs_does_not_panic() {
        // clamped to 1s (1000ms) — the modulo can never divide by zero
        assert_eq!(window_open_ms(1_500, 0), 1_000);
        assert_eq!(window_close_ms(1_000, 0), 2_000);
    }

    #[test]
    fn chain_windows_produces_current_plus_n() {
        let now = W5M_MS + 42_000; // 42s into the second window
        let chain = chain_windows(now, W5M, 4);
        // current + 4 = 5 entries
        assert_eq!(chain.len(), 5);
        // first is the window current at `now`
        assert_eq!(chain[0].open_ms, W5M_MS);
        assert_eq!(chain[0].close_ms, 2 * W5M_MS);
        // strictly ascending, contiguous: each close is the next open, spaced one window apart
        for pair in chain.windows(2) {
            assert_eq!(pair[0].close_ms, pair[1].open_ms);
            assert_eq!(pair[1].open_ms - pair[0].open_ms, W5M_MS);
        }
        // last window opens 4 windows after the first
        assert_eq!(chain[4].open_ms, W5M_MS + 4 * W5M_MS);
    }

    #[test]
    fn chain_windows_n_zero_is_just_the_current_window() {
        let chain = chain_windows(W5M_MS + 1, W5M, 0);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0], Window { open_ms: W5M_MS, close_ms: 2 * W5M_MS });
    }

    #[test]
    fn fmt_countdown_minutes_seconds() {
        assert_eq!(fmt_countdown(300_000), "5:00");
        assert_eq!(fmt_countdown(125_400), "2:05"); // floors 125.4s → 2:05
        assert_eq!(fmt_countdown(65_000), "1:05");
        assert_eq!(fmt_countdown(60_000), "1:00");
        assert_eq!(fmt_countdown(59_000), "0:59");
        assert_eq!(fmt_countdown(5_000), "0:05");
        assert_eq!(fmt_countdown(900), "0:00"); // sub-second floors to zero
    }

    #[test]
    fn fmt_countdown_clamps_negatives_to_zero() {
        assert_eq!(fmt_countdown(0), "0:00");
        assert_eq!(fmt_countdown(-1), "0:00");
        assert_eq!(fmt_countdown(-50_000), "0:00");
    }

    #[test]
    fn urgent_only_in_final_stretch() {
        assert!(is_urgent(URGENT_MS)); // exactly 10s → urgent
        assert!(is_urgent(URGENT_MS - 1));
        assert!(is_urgent(0)); // at the boundary → still urgent (red)
        assert!(!is_urgent(URGENT_MS + 1)); // 10.001s → not yet
        assert!(!is_urgent(-1)); // already closed → not urgent
    }

    /// Pins the rendered strings, INCLUDING the divergences that keep this formatter local
    /// instead of swapping to `vike_ui_theme::fmt::fmt_compact` (GUI audit F7 — a swap would
    /// change rendered text).
    #[test]
    fn fmt_vol_scales_thousands_and_millions() {
        assert_eq!(fmt_vol(0.0), "0");
        assert_eq!(fmt_vol(950.0), "950");
        assert_eq!(fmt_vol(1_500.0), "1.5k"); // lowercase k — fmt_compact prints "1.5K"
        assert_eq!(fmt_vol(12_300.0), "12.3k");
        assert_eq!(fmt_vol(2_300_000.0), "2.3M");
        assert_eq!(fmt_vol(2_000_000.0), "2.0M"); // always one decimal — fmt_compact trims to "2M"
        assert_eq!(fmt_vol(7_100_000_000.0), "7100.0M"); // no B tier — fmt_compact prints "7.1B"
    }

    #[test]
    fn state_defaults_to_no_selection() {
        assert_eq!(ChainRailState::default().selected, None);
    }
}
