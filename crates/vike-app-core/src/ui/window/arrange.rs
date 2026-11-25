//! Arrangement geometry: cascade / tile / grid / minimize-all over the visible
//! windows, plus per-window maximize/restore — ports vike's `chartwin` arrange
//! actions (`_TILE_GAP` et al.) — and which windows a maximized one hides
//! ([`hidden_by_maximized`]) and raising one over it ([`raise`]).
//!
//! Arranging never draws: it writes one-frame `pending` rects into `WindowState`s;
//! [`super::state::show_window`] consumes them next frame.

use super::state::WindowState;
use egui::{Rect, pos2, vec2};
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::workspace;

// Debug/PartialEq/Eq so `initial_arrange`'s planned `WindowInitialArrangeAction`/`remember` values can be
// asserted table-style (assert_eq over planned modes) — purely additive, no manual impls exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowArrange {
    Cascade,
    TileHorizontal,
    TileVertical,
    Grid,
    MinimizeAll,
    RestoreAll,
}

/// Write `pending` rects into the visible windows (`open && !minimized`).
/// Naming: **Tile Horizontal = full-width rows stacked vertically; Tile Vertical
/// = full-height columns side by side.**
pub fn apply_arrange(wins: &mut [WindowState], area: Rect, what: WindowArrange) {
    use WindowArrange::*;
    match what {
        MinimizeAll => {
            for w in wins.iter_mut() {
                w.minimized = true;
            }
            return;
        }
        RestoreAll => {
            for w in wins.iter_mut() {
                w.minimized = false;
                w.open = true;
            }
            return;
        }
        _ => {}
    }

    let index: Vec<usize> =
        wins.iter().enumerate().filter(|(_, w)| w.open && !w.minimized).map(|(i, _)| i).collect();
    let n = index.len();
    if n == 0 {
        return;
    }
    let nf = n as f32;
    // The tile gap, `space::XS` (the Python `chartwin._TILE_GAP`): outer margin AND the gutter
    // between tiles.

    match what {
        Cascade => {
            let off = workspace::ARRANGE_CASCADE_STEP;
            let w_size = (area.width() * workspace::ARRANGE_CASCADE_FRACTION)
                .max(workspace::ARRANGE_CASCADE_MIN_WIDTH);
            let h_size = (area.height() * workspace::ARRANGE_CASCADE_FRACTION)
                .max(workspace::ARRANGE_CASCADE_MIN_HEIGHT);
            let max_steps = ((area.width() - w_size) / off).max(1.0).floor();
            for (k, &i) in index.iter().enumerate() {
                let step = (k as f32) % max_steps;
                let min = pos2(area.min.x + step * off, area.min.y + step * off);
                wins[i].pending = Some(Rect::from_min_size(min, vec2(w_size, h_size)));
                wins[i].maximized = false;
            }
        }
        TileHorizontal => {
            let g = space::XS;
            let rh = (area.height() - (nf + 1.0) * g) / nf;
            for (k, &i) in index.iter().enumerate() {
                let min = pos2(area.min.x + g, area.min.y + g + k as f32 * (rh + g));
                wins[i].pending = Some(Rect::from_min_size(min, vec2(area.width() - 2.0 * g, rh)));
                wins[i].maximized = false;
            }
        }
        TileVertical => {
            let g = space::XS;
            let cw = (area.width() - (nf + 1.0) * g) / nf;
            for (k, &i) in index.iter().enumerate() {
                let min = pos2(area.min.x + g + k as f32 * (cw + g), area.min.y + g);
                wins[i].pending = Some(Rect::from_min_size(min, vec2(cw, area.height() - 2.0 * g)));
                wins[i].maximized = false;
            }
        }
        Grid => {
            let g = space::XS;
            let cols = nf.sqrt().ceil();
            let rows = (nf / cols).ceil();
            let cw = (area.width() - (cols + 1.0) * g) / cols;
            let ch = (area.height() - (rows + 1.0) * g) / rows;
            for (k, &i) in index.iter().enumerate() {
                let c = (k as f32) % cols;
                let r = (k as f32 / cols).floor();
                let min = pos2(area.min.x + g + c * (cw + g), area.min.y + g + r * (ch + g));
                wins[i].pending = Some(Rect::from_min_size(min, vec2(cw, ch)));
                wins[i].maximized = false;
            }
        }
        MinimizeAll | RestoreAll => unreachable!(),
    }
}

pub fn maximize(w: &mut WindowState, area: Rect) {
    w.pre_maximize = Some(Rect::from_min_size(w.screen_position, w.size));
    w.pending = Some(area);
    w.maximized = true;
}

pub fn unmaximize(w: &mut WindowState) {
    if let Some(r) = w.pre_maximize.take() {
        w.pending = Some(r);
    }
    w.maximized = false;
}

/// **Which of `wins` the maximized one hides this frame** (the desktop does not draw them), by index:
/// the windows behind it in egui's window order (`hidden_behind`). Nothing while no open window is
/// maximized (vike maximize fills the workspace by hiding what is behind it).
pub fn hidden_by_maximized(ctx: &egui::Context, wins: &[WindowState]) -> Vec<bool> {
    let Some(maximized) = wins.iter().find(|w| w.open && !w.minimized && w.maximized) else {
        return vec![false; wins.len()];
    };
    let order: Vec<egui::Id> = ctx.memory(|m| {
        m.layer_ids().filter(|l| l.order == egui::Order::Middle).map(|l| l.id).collect()
    });
    wins.iter().map(|w| w.id != maximized.id && hidden_behind(&order, maximized.id, w.id)).collect()
}

/// **Bring window `id` to the front**, over a maximized window too ([`hidden_by_maximized`]), which
/// stays maximized. egui orders a new layer on top at once and an old one at the frame's end, so one
/// more frame is asked for to draw it there.
pub fn raise(ctx: &egui::Context, id: egui::Id) {
    ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, id));
    ctx.request_repaint();
}

/// **Whether the maximized window `maximized` hides `window`**: it stands above it in egui's window
/// order (`order`, back to front: `egui::Memory::layer_ids`). A window raised over it (the Studio
/// brought back, a window just opened: each [`raise`]) is drawn over it, and the maximized window stays
/// maximized (the owner's pass of 2026-11-21: bringing the Studio back un-maximized his chart). A
/// window egui has not ordered yet (a restored layout's, never drawn) is behind it, as every other
/// window was.
fn hidden_behind(order: &[egui::Id], maximized: egui::Id, window: egui::Id) -> bool {
    let at = |id: egui::Id| order.iter().position(|o| *o == id);
    match (at(maximized), at(window)) {
        (Some(m), Some(w)) => m > w,
        (_, None) => true,
        (None, Some(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::WindowKind;
    use super::*;

    const G: f32 = 2.0; // _TILE_GAP twin — outer margin and gutter

    fn win() -> WindowState {
        let r = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 200.0));
        WindowState::new("t", "BTCUSDT", "1m", WindowKind::Chart, r)
    }

    fn area() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))
    }

    #[test]
    fn grid_2x2() {
        let mut wins = vec![win(), win(), win(), win()];
        apply_arrange(&mut wins, area(), WindowArrange::Grid);
        let cw = (800.0 - 3.0 * G) / 2.0; // 397
        let ch = (600.0 - 3.0 * G) / 2.0; // 297
        let r0 = wins[0].pending.unwrap();
        assert_eq!(r0.min, pos2(G, G));
        assert_eq!(r0.size(), vec2(cw, ch));
        // k=3 → col 1, row 1
        let r3 = wins[3].pending.unwrap();
        assert_eq!(r3.min, pos2(G + cw + G, G + ch + G));
        assert_eq!(r3.size(), vec2(cw, ch));
        assert!(wins.iter().all(|w| !w.maximized));
    }

    #[test]
    fn tile_h_full_width_rows() {
        let mut wins = vec![win(), win()];
        apply_arrange(&mut wins, area(), WindowArrange::TileHorizontal);
        let rh = (600.0 - 3.0 * G) / 2.0; // 297
        let r0 = wins[0].pending.unwrap();
        assert_eq!(r0.min, pos2(G, G));
        assert_eq!(r0.size(), vec2(800.0 - 2.0 * G, rh));
        let r1 = wins[1].pending.unwrap();
        assert_eq!(r1.min, pos2(G, G + rh + G));
    }

    #[test]
    fn tile_v_full_height_columns() {
        let mut wins = vec![win(), win(), win()];
        apply_arrange(&mut wins, area(), WindowArrange::TileVertical);
        let cw = (800.0 - 4.0 * G) / 3.0; // 264
        let r2 = wins[2].pending.unwrap();
        assert_eq!(r2.min, pos2(G + 2.0 * (cw + G), G));
        assert_eq!(r2.size(), vec2(cw, 600.0 - 2.0 * G));
    }

    #[test]
    fn cascade_offsets_and_size() {
        let mut wins = vec![win(), win()];
        apply_arrange(&mut wins, area(), WindowArrange::Cascade);
        let w_size = (800.0_f32 * 0.6).max(260.0);
        let h_size = (600.0_f32 * 0.6).max(180.0);
        let r0 = wins[0].pending.unwrap();
        assert_eq!(r0.min, pos2(0.0, 0.0));
        assert_eq!(r0.size(), vec2(w_size, h_size));
        let r1 = wins[1].pending.unwrap();
        assert_eq!(r1.min, pos2(30.0, 30.0)); // 30px stagger per window
    }

    #[test]
    fn hidden_and_minimized_windows_are_skipped() {
        let mut wins = vec![win(), win(), win()];
        wins[1].open = false;
        wins[2].minimized = true;
        apply_arrange(&mut wins, area(), WindowArrange::Grid);
        assert!(wins[1].pending.is_none());
        assert!(wins[2].pending.is_none());
        // the ONE visible window gets a 1×1 grid = the whole area minus margins
        let r = wins[0].pending.unwrap();
        assert_eq!(r.size(), vec2(800.0 - 2.0 * G, 600.0 - 2.0 * G));
    }

    #[test]
    fn minimize_and_restore_all() {
        let mut wins = vec![win(), win()];
        wins[1].open = false;
        apply_arrange(&mut wins, area(), WindowArrange::MinimizeAll);
        assert!(wins.iter().all(|w| w.minimized));
        apply_arrange(&mut wins, area(), WindowArrange::RestoreAll);
        assert!(wins.iter().all(|w| !w.minimized && w.open)); // restore also reopens
    }

    #[test]
    fn maximize_restores_prior_rect() {
        let before = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 200.0));
        let mut w = win();
        maximize(&mut w, area());
        assert!(w.maximized);
        assert_eq!(w.pending, Some(area()));
        assert_eq!(w.pre_maximize, Some(before));
        w.pending = None; // frame consumed the forced geometry
        unmaximize(&mut w);
        assert!(!w.maximized);
        assert_eq!(w.pending, Some(before));
        assert_eq!(w.pre_maximize, None);
    }

    /// A maximized window hides what stands behind it in egui's order, and nothing raised over it.
    #[test]
    fn a_maximized_window_hides_only_the_windows_behind_it() {
        let (chart, studio, news) =
            (egui::Id::unique("chart"), egui::Id::unique("studio"), egui::Id::unique("news"));
        let order = [studio, chart, news];
        assert!(hidden_behind(&order, chart, studio), "behind it: hidden");
        assert!(!hidden_behind(&order, chart, news), "raised over it: drawn over it");
        assert!(
            hidden_behind(&[chart], chart, studio),
            "never drawn (a restored layout's): hidden"
        );
        assert!(!hidden_behind(&[studio], chart, studio), "the maximized one not ordered yet");
    }

    /// Over a real egui order: the Studio raised over a maximized chart is drawn and the chart stays
    /// maximized; a window never ordered (a restored layout's) stays hidden; and with no window
    /// maximized every window is drawn.
    #[test]
    fn a_window_raised_over_a_maximized_one_is_drawn() {
        let mut chart = win();
        maximize(&mut chart, area());
        let studio = WindowState::new("studio", "", "", WindowKind::Studio, area());
        let restored = WindowState::new("restored", "", "", WindowKind::Studio, area());
        let mut wins = vec![chart, studio, restored];
        let ctx = egui::Context::default();
        raise(&ctx, wins[0].id);
        assert_eq!(hidden_by_maximized(&ctx, &wins), [false, true, true], "only the chart");
        raise(&ctx, wins[1].id);
        assert_eq!(hidden_by_maximized(&ctx, &wins), [false, false, true], "the Studio over it");
        assert!(wins[0].maximized, "and the chart stays maximized");
        wins[0].maximized = false;
        assert_eq!(hidden_by_maximized(&ctx, &wins), [false; 3], "nothing maximized: all drawn");
    }
}
