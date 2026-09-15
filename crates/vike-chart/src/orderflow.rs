//! Chart-side derivations from the per-bar footprint substrate (SP2). CVD and the visible
//! volume profile are DERIVED here from `&[FootprintBar]` — the single orderflow input — so
//! the app feeds one structure and the renderer computes the three views.
use vike_orderflow::FootprintBar;

/// Cumulative volume delta per bar: running Σ over bars of (Σ cell.buy − Σ cell.sell).
/// Index-aligned to the footprints (== chart bars). Naive fold (no compensation).
pub fn cvd_from_footprints(fps: &[FootprintBar]) -> Vec<f64> {
    let mut out = Vec::with_capacity(fps.len());
    let mut acc = 0.0;
    for fp in fps {
        let mut d = 0.0;
        for c in &fp.cells {
            d += c.buy_vol - c.sell_vol;
        }
        acc += d;
        out.push(acc);
    }
    out
}

/// Footprint cell numbers are drawn only when the cell is tall enough to be legible;
/// below this the renderer falls back to a delta-colored bar.
pub fn cell_text_legible(cell_px_height: f32) -> bool {
    cell_px_height >= 11.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_orderflow::PriceBin;
    fn fp(idx: u64, cells: &[(f64, f64, f64)]) -> FootprintBar {
        FootprintBar {
            bar_index: idx,
            cells: cells
                .iter()
                .map(|&(p, b, s)| PriceBin { price: p, buy_vol: b, sell_vol: s })
                .collect(),
        }
    }
    #[test]
    fn cvd_is_running_delta() {
        let fps = vec![
            fp(0, &[(100.0, 5.0, 2.0)]),                    // delta +3 → cvd 3
            fp(1, &[(101.0, 1.0, 4.0)]),                    // delta -3 → cvd 0
            fp(2, &[(100.0, 2.0, 0.0), (101.0, 0.0, 1.0)]), // delta +1 → cvd 1
        ];
        assert_eq!(cvd_from_footprints(&fps), vec![3.0, 0.0, 1.0]);
        assert_eq!(cvd_from_footprints(&[]), Vec::<f64>::new());
    }
    #[test]
    fn text_legibility_threshold() {
        assert!(cell_text_legible(11.0));
        assert!(cell_text_legible(20.0));
        assert!(!cell_text_legible(10.9));
    }
}
