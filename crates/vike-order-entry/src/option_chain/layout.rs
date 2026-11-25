//! The chain grid's column layout: which columns a row has and how wide each is drawn.

use vike_marketdata::OptionKind;

// ---------------------------------------------------------------------------
// Pure logic (unit-tested) — no egui, so the column-layout math is verifiable standalone.
// ---------------------------------------------------------------------------

/// One rendered column: a per-side field cell, or one of the two centre-spine columns.
#[derive(Clone, Copy)]
pub(super) enum OptionChainColumn {
    Field { field: &'static str, kind: OptionKind },
    Strike,
    Iv,
}

/// Per-field share of the room a wide window has LEFT OVER once every column holds its widest text
/// (a GUI-layout concern only — deliberately NOT part of the venue-agnostic
/// `vike_options::chain::columns` model). The four "wide" numeric fields (theor/bid/ask/distance)
/// get slightly more of it than the default; volume gets less (it carries a magnitude bar, not just
/// digits); everything else splits evenly.
pub(super) fn weight(field: &str) -> f32 {
    match field {
        "bidpct" | "askpct" | "reldist" => 0.95,
        "theor" | "bid" | "ask" | "distance" => 1.05,
        "volume" => 0.8,
        _ => 1.0, // spread
    }
}

/// Cumulative left-edge pixel offsets for a row of columns: `edges[i]` is column `i`'s left edge and
/// `edges[i + 1]` its right edge. Column `i` is never narrower than `floors[i]`, and the room `full`
/// has beyond the floors' sum is shared out by `weights`. A `full` SHORT of the floors is not
/// honoured: the row is then exactly the floors' sum wide, wider than `full` (the window's body
/// scrolls it), because a squeezed column paints its right-aligned text over its left neighbour's.
/// Returns `weights.len() + 1` offsets starting at `0.0`.
pub(super) fn compute_edges(weights: &[f32], floors: &[f32], full: f32) -> Vec<f32> {
    let total_w: f32 = weights.iter().sum();
    let unit = (full - floors.iter().sum::<f32>()).max(0.0) / total_w;
    let mut edges = Vec::with_capacity(weights.len() + 1);
    let mut accumulator = 0.0;
    edges.push(0.0);
    for (w, floor) in weights.iter().zip(floors) {
        accumulator += floor + w * unit;
        edges.push(accumulator);
    }
    edges
}
