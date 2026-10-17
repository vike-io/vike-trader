//! Registry rows — structure (structure.py).
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- structure (structure.py) ----
        ind!(
            "pivot_points", "Pivot Points", Structure, Overlay, false,
            out! {"p":Line, "r1":Line, "r2":Line, "r3":Line, "s1":Line, "s2":Line, "s3":Line}, &[], &[], PivotPoints
        ),
        ind!(
            "volume_profile_poc", "Volume Profile POC", Structure, Overlay, false, out! {"poc":Line}, &[],
            params!(("window", 50.0, 5.0, 500.0, 1.0), ("bins", 24.0, 4.0, 200.0, 1.0)), VolumeProfilePoc
        ),
        ind!(
            "zigzag",
            "ZigZag",
            Structure,
            Overlay,
            true,
            // `batch_zigzag` emits the PIVOT PRICE at each confirmed pivot (NaN between), so
            // this is a real price series drawn as the connecting zigzag line — `Line` says so.
            // Pixel-identical to the previous `Marker`: both take the non-Band overlay width
            // (1.5) and the same `seg_line` path, which already breaks the line at the NaN gaps.
            out! {"zigzag":Line},
            &[],
            params!(("deviation", 5.0, 0.1, 50.0, 0.5)),
            Zigzag
        ),
        ind!(
            "williams_fractal",
            "Williams Fractal",
            Structure,
            Overlay,
            true,
            // `batch_williams_fractal` emits the fractal's own high/low PRICE at each fractal
            // bar (NaN between) — discrete points, never a path. As `Marker` they fell to
            // `seg_line` and got joined into a zigzagging line across unrelated bars; `Dots`
            // renders each one where it belongs, on its bar.
            out! {"fractal_up":Dots, "fractal_down":Dots},
            &[],
            params!(("n", 2.0, 1.0, 10.0, 1.0)),
            WilliamsFractal
        ),
    ]
}
