//! Arrangement geometry: cascade / tile / grid / minimize-all over the visible
//! windows, plus per-window maximize/restore — ports vike's `chartwin` arrange
//! actions (`_TILE_GAP` et al.).
//!
//! Arranging never draws: it writes one-frame `pending` rects into `WinState`s;
//! [`super::state::show_window`] consumes them next frame.

use super::state::WinState;
use egui::{Rect, pos2, vec2};

// Debug/PartialEq/Eq so `initial_arrange`'s planned `ArrangeAction`/`remember` values can be
// asserted table-style (assert_eq over planned modes) — purely additive, no manual impls exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    Cascade,
    TileH,
    TileV,
    Grid,
    MinimizeAll,
    RestoreAll,
}

/// Write `pending` rects into the visible windows (`open && !minimized`).
/// Naming: **Tile Horizontal = full-width rows stacked vertically; Tile Vertical
/// = full-height columns side by side.**
pub fn apply_arrange(wins: &mut [WinState], area: Rect, what: Arrange) {
    use Arrange::*;
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

    let idx: Vec<usize> =
        wins.iter().enumerate().filter(|(_, w)| w.open && !w.minimized).map(|(i, _)| i).collect();
    let n = idx.len();
    if n == 0 {
        return;
    }
    let nf = n as f32;
    const TILE_GAP: f32 = 2.0; // vike chartwin._TILE_GAP — outer margin AND inter-tile gutter

    match what {
        Cascade => {
            const OFF: f32 = 30.0;
            let w_sz = (area.width() * 0.6).max(260.0);
            let h_sz = (area.height() * 0.6).max(180.0);
            let max_steps = ((area.width() - w_sz) / OFF).max(1.0).floor();
            for (k, &i) in idx.iter().enumerate() {
                let step = (k as f32) % max_steps;
                let min = pos2(area.min.x + step * OFF, area.min.y + step * OFF);
                wins[i].pending = Some(Rect::from_min_size(min, vec2(w_sz, h_sz)));
                wins[i].maximized = false;
            }
        }
        TileH => {
            let g = TILE_GAP;
            let rh = (area.height() - (nf + 1.0) * g) / nf;
            for (k, &i) in idx.iter().enumerate() {
                let min = pos2(area.min.x + g, area.min.y + g + k as f32 * (rh + g));
                wins[i].pending = Some(Rect::from_min_size(min, vec2(area.width() - 2.0 * g, rh)));
                wins[i].maximized = false;
            }
        }
        TileV => {
            let g = TILE_GAP;
            let cw = (area.width() - (nf + 1.0) * g) / nf;
            for (k, &i) in idx.iter().enumerate() {
                let min = pos2(area.min.x + g + k as f32 * (cw + g), area.min.y + g);
                wins[i].pending = Some(Rect::from_min_size(min, vec2(cw, area.height() - 2.0 * g)));
                wins[i].maximized = false;
            }
        }
        Grid => {
            let g = TILE_GAP;
            let cols = nf.sqrt().ceil();
            let rows = (nf / cols).ceil();
            let cw = (area.width() - (cols + 1.0) * g) / cols;
            let ch = (area.height() - (rows + 1.0) * g) / rows;
            for (k, &i) in idx.iter().enumerate() {
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

pub fn maximize(w: &mut WinState, area: Rect) {
    w.pre_max = Some(Rect::from_min_size(w.pos, w.size));
    w.pending = Some(area);
    w.maximized = true;
}

pub fn unmaximize(w: &mut WinState) {
    if let Some(r) = w.pre_max.take() {
        w.pending = Some(r);
    }
    w.maximized = false;
}

#[cfg(test)]
mod tests {
    use super::super::state::WinKind;
    use super::*;

    const G: f32 = 2.0; // _TILE_GAP twin — outer margin and gutter

    fn win() -> WinState {
        let r = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 200.0));
        WinState::new("t", "BTCUSDT", "1m", WinKind::Chart, r)
    }

    fn area() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))
    }

    #[test]
    fn grid_2x2() {
        let mut wins = vec![win(), win(), win(), win()];
        apply_arrange(&mut wins, area(), Arrange::Grid);
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
        apply_arrange(&mut wins, area(), Arrange::TileH);
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
        apply_arrange(&mut wins, area(), Arrange::TileV);
        let cw = (800.0 - 4.0 * G) / 3.0; // 264
        let r2 = wins[2].pending.unwrap();
        assert_eq!(r2.min, pos2(G + 2.0 * (cw + G), G));
        assert_eq!(r2.size(), vec2(cw, 600.0 - 2.0 * G));
    }

    #[test]
    fn cascade_offsets_and_size() {
        let mut wins = vec![win(), win()];
        apply_arrange(&mut wins, area(), Arrange::Cascade);
        let w_sz = (800.0_f32 * 0.6).max(260.0);
        let h_sz = (600.0_f32 * 0.6).max(180.0);
        let r0 = wins[0].pending.unwrap();
        assert_eq!(r0.min, pos2(0.0, 0.0));
        assert_eq!(r0.size(), vec2(w_sz, h_sz));
        let r1 = wins[1].pending.unwrap();
        assert_eq!(r1.min, pos2(30.0, 30.0)); // 30px stagger per window
    }

    #[test]
    fn hidden_and_minimized_windows_are_skipped() {
        let mut wins = vec![win(), win(), win()];
        wins[1].open = false;
        wins[2].minimized = true;
        apply_arrange(&mut wins, area(), Arrange::Grid);
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
        apply_arrange(&mut wins, area(), Arrange::MinimizeAll);
        assert!(wins.iter().all(|w| w.minimized));
        apply_arrange(&mut wins, area(), Arrange::RestoreAll);
        assert!(wins.iter().all(|w| !w.minimized && w.open)); // restore also reopens
    }

    #[test]
    fn maximize_restores_prior_rect() {
        let before = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 200.0));
        let mut w = win();
        maximize(&mut w, area());
        assert!(w.maximized);
        assert_eq!(w.pending, Some(area()));
        assert_eq!(w.pre_max, Some(before));
        w.pending = None; // frame consumed the forced geometry
        unmaximize(&mut w);
        assert!(!w.maximized);
        assert_eq!(w.pending, Some(before));
        assert_eq!(w.pre_max, None);
    }
}
