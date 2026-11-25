//! The chart style vocabulary: `ChartStyle` and its menu `STYLE_SECTIONS`, and beside it what a
//! style draws through: `series` (the frame's render series), `transforms` (the
//! Renko/Range/LineBreak/Kagi/PnF ports) and the painters `lines`, `synthetic`, `footprint` and
//! `style_icon`. Every painter reads the frame's RESOLVED colours
//! (`crate::chart::colors::ChartColors`), never a colour of its own.

pub(crate) mod footprint;
pub(crate) mod lines;
pub(crate) mod series;
pub(crate) mod style_icon;
pub(crate) mod synthetic;
pub mod transforms;

/// A chart's style. The variant name is the word `workspace.json` stores (`WindowSnapshot::style`).
#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ChartStyle {
    Candles,
    Hollow,
    HeikinAshi,
    VolumeCandles,
    Bars,
    HlcBars,
    HighLow,
    Line,
    LineMarkers,
    StepLine,
    Area,
    Baseline,
    HlcArea,
    Columns,
    Renko,
    Range,
    LineBreak,
    Kagi,
    PointFigure,
    /// Per-bar buy/sell price-cell grid (SP2, T5) — the orderflow "footprint" chart. Rendered
    /// from `ChartInputs::footprint`; falls back to plain candles when that's `None` (default-off
    /// — see `ChartInputs::footprint`'s doc).
    Footprint,
}

impl ChartStyle {
    pub fn label(self) -> &'static str {
        use ChartStyle::*;
        match self {
            Candles => "Candles",
            Hollow => "Hollow candles",
            HeikinAshi => "Heikin Ashi",
            VolumeCandles => "Volume candles",
            Bars => "Bars",
            HlcBars => "HLC bars",
            HighLow => "High-low",
            Line => "Line",
            LineMarkers => "Line with markers",
            StepLine => "Step line",
            Area => "Area",
            Baseline => "Baseline",
            HlcArea => "HLC area",
            Columns => "Columns",
            Renko => "Renko",
            Range => "Range",
            LineBreak => "Line break",
            Kagi => "Kagi",
            PointFigure => "Point & Figure",
            Footprint => "Footprint",
        }
    }
    /// All 20 styles in menu order (for QA capture via `VIKE_STYLE=<index>`, whose index is this
    /// order: a new style goes at the END so an existing capture command keeps its style).
    pub const ALL: [ChartStyle; 20] = [
        ChartStyle::Candles,
        ChartStyle::Hollow,
        ChartStyle::HeikinAshi,
        ChartStyle::VolumeCandles,
        ChartStyle::Bars,
        ChartStyle::HlcBars,
        ChartStyle::HighLow,
        ChartStyle::Line,
        ChartStyle::LineMarkers,
        ChartStyle::StepLine,
        ChartStyle::Area,
        ChartStyle::Baseline,
        ChartStyle::HlcArea,
        ChartStyle::Columns,
        ChartStyle::Renko,
        ChartStyle::Range,
        ChartStyle::LineBreak,
        ChartStyle::Kagi,
        ChartStyle::PointFigure,
        ChartStyle::Footprint,
    ];
}

pub const STYLE_SECTIONS: &[(&str, &[ChartStyle])] = &[
    (
        "Bars",
        &[
            ChartStyle::Candles,
            ChartStyle::Hollow,
            ChartStyle::HeikinAshi,
            ChartStyle::VolumeCandles,
            ChartStyle::Bars,
            ChartStyle::HlcBars,
            ChartStyle::HighLow,
        ],
    ),
    (
        "Lines",
        &[
            ChartStyle::Line,
            ChartStyle::LineMarkers,
            ChartStyle::StepLine,
            ChartStyle::Area,
            ChartStyle::Baseline,
            ChartStyle::HlcArea,
            ChartStyle::Columns,
        ],
    ),
    (
        "Special",
        &[
            ChartStyle::Renko,
            ChartStyle::Range,
            ChartStyle::LineBreak,
            ChartStyle::Kagi,
            ChartStyle::PointFigure,
        ],
    ),
    // SP2, T5: its own section rather than folded into "Special" — Footprint isn't a
    // reindexing transform (see `style_preserves_volume`/the `owned` transform gate in `draw`),
    // it's an alternate per-bar substrate (buy/sell price cells) for the SAME bar axis, so it
    // reads as a distinct category from Renko/Range/LineBreak/Kagi/PointFigure's box-reindexing.
    ("Orderflow", &[ChartStyle::Footprint]),
];

#[cfg(test)]
mod tests {
    use super::*;

    // --- SP2, T5: Footprint chart style --------------------------------------------------

    #[test]
    fn chart_style_all_appends_footprint_at_a_stable_end() {
        // `VIKE_STYLE=<index>` picks a style by POSITION in `ALL` — appending at the end keeps
        // every pre-existing style's index (0..=18) unchanged.
        assert_eq!(ChartStyle::ALL.len(), 20);
        assert_eq!(ChartStyle::ALL[19], ChartStyle::Footprint);
        assert_eq!(ChartStyle::ALL[0], ChartStyle::Candles);
        assert_eq!(ChartStyle::ALL[18], ChartStyle::PointFigure);
        assert_eq!(ChartStyle::Footprint.label(), "Footprint");
    }

    #[test]
    fn style_sections_cover_all_styles_exactly_once() {
        let total: usize = STYLE_SECTIONS.iter().map(|(_, styles)| styles.len()).sum();
        assert_eq!(
            total,
            ChartStyle::ALL.len(),
            "STYLE_SECTIONS entry count must match ChartStyle::ALL"
        );
        for s in ChartStyle::ALL {
            let hits = STYLE_SECTIONS.iter().filter(|(_, styles)| styles.contains(&s)).count();
            assert_eq!(hits, 1, "{} must appear in exactly one STYLE_SECTIONS group", s.label());
        }
        assert!(
            STYLE_SECTIONS
                .iter()
                .any(|(name, styles)| *name == "Orderflow"
                    && styles.contains(&ChartStyle::Footprint)),
            "Footprint must be listed under the Orderflow section"
        );
    }
}
