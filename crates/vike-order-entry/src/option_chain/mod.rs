//! The options-chain grid engine — the mirrored CALLS | Strike·IV | PUTS ladder
//! (9 fields/side), ATM highlight, expiry strip, extracted verbatim from vike-app's
//! `Options =>` tool arm (was crates/vike-desktop/src/main.rs). The chain snapshot it paints
//! (`OptionChain`/`OptionChainExpiry`/`OptionChainStrike`/`OptionQuote`) is `vike-marketdata` feed data; the
//! pricing/greeks/column model live in `vike-options` (pure); this crate owns only the egui
//! paint + pixel layout.
//! The same shape as the Trade window's widget (`vike_order_entry::trade_window::draw`, which replaced the
//! DOM's): borrowed inputs in, actions out. CI-gated (headless egui, no wgpu).

mod grid;
mod layout;
#[cfg(test)]
mod tests;
pub mod ticket;
mod toolbar;

use egui::FontId;
use vike_marketdata::OptionKind;
use vike_options::chain::columns;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;

use grid::{chain_cell, paint_chain_header, paint_chain_rows, volume_of};
use layout::{OptionChainColumn, compute_edges, weight};
use toolbar::{chain_toolbar, expiry_strip};

// Every colour this grid paints is a token: the theme's and the market set's through the kit's
// `ThemeTokens`, a call's and a put's from the `callput` map's rows, a side of the book's and a position's
// from the `side` map's. The calls' blue is a SERIES colour, not status (#2318 ruled it the deferred
// series palette's first rotation colour, spec §10, decision 5): the row says `info` because that
// role's value is exactly it today, and the working-order marker reads the same row.

/// Everything the grid needs to render one frame. Borrowed — the app owns the storage.
/// `expiry_selection` is mutated in place on an expiry-strip click (cross-frame selection), the same
/// shape as the Trade window's `TradeWindowState` fields.
pub struct OptionChainInputs<'a> {
    pub chains: &'a std::collections::BTreeMap<String, vike_marketdata::OptionChain>,
    pub default_expiry: &'a str,
    pub expiries: &'a [vike_marketdata::OptionChainExpiry],
    pub expiry_selection: &'a mut Option<String>,
    /// The fetched underlyings to offer in the toolbar selector (e.g. `["BTC","ETH","SOL"]`).
    pub underlyings: &'a [String],
    /// The active underlying — mutated in place on a selector-pill click (cross-frame selection,
    /// same shape as `expiry_selection`). `None` = follow the first entry of `underlyings`.
    pub underlying_selection: &'a mut Option<String>,
    /// The display half-window: how many strike rows to show above AND below ATM. Mutated in place
    /// when the `±N strikes` pill is clicked (cycles the presets). The chain is fetched wide (±30),
    /// so changing this never needs a re-fetch — the renderer just trims to ±N.
    pub strike_window: &'a mut usize,
    /// The USER's own resting (working) orders: their order client ids, in the snapshot's order, keyed by
    /// `instrument_name` (plain data the app builds from the `CoreSnapshot`'s order rows,
    /// `vike_app_core::orders::option_chain_orders::option_chain_orders`; the chart links no
    /// execution crate). The marker counts them and cancels the FIRST one. Empty = nothing owned →
    /// the grid paints exactly as it did before this feature.
    pub working: &'a std::collections::BTreeMap<String, Vec<String>>,
    /// The USER's NET position per `instrument_name`; only its signed `size` is painted (+long /
    /// −short, `0.0` flat).
    pub positions: &'a std::collections::BTreeMap<String, vike_model::Position>,
    /// The chain's venue as the settings database spells it; the caller resolves it. The widget
    /// spells no venue of its own (the owner's ruling of 2026-09-30).
    pub venue_label: &'a str,
}

/// Outcomes leaving the widget this frame. `refresh_clicked` is set when the Refresh pill is
/// clicked — the app forces an immediate chain re-poll (wakes the options poll thread). `order` is
/// set when the user clicks a tradeable CALL/PUT bid/ask cell — the app turns it into a prefilled
/// CONFIRM ticket. The click ONLY opens the ticket; nothing is submitted here.
#[derive(Default)]
pub struct OptionChainActions {
    pub refresh_clicked: bool,
    pub order: Option<OptionChainOrderClick>,
    /// Set when the user clicks a working-order marker on the chain — the `order_client_id` to
    /// cancel. The app routes it to `OrderIntent::Cancel` (mirrors the Trade window's `TradeWindowAction::Cancel`).
    pub cancel: Option<String>,
}

/// A chain bid/ask cell click, carrying everything the confirm ticket needs to prefill. An ASK
/// click buys (`OrderSide::Long`), a BID click sells (`OrderSide::Short`); the order seam's signed
/// `i32` is `i32::from(side)`. Emitted only for cells whose quote has BOTH an `instrument_name` AND
/// the relevant price.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionChainOrderClick {
    pub instrument: String,
    pub side: vike_model::OrderSide,
    pub price: f64,
    pub kind: OptionKind,
    pub strike: f64,
}

/// The displayed ±`n` rows over the fetched-wide chain: the ONE at-the-money window
/// ([`vike_options::atm_strike_window`]) the 100 ms ticker streams, so every row shown can carry a
/// live bid/ask. No spot (nothing to centre on) shows every row.
fn strike_window_rows(
    rows: &[vike_marketdata::OptionChainStrike],
    spot: Option<f64>,
    n: usize,
) -> &[vike_marketdata::OptionChainStrike] {
    match spot {
        Some(s) => &rows[vike_options::atm_strike_window(rows, s, n)],
        None => rows,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw one frame of the options-chain grid and return the actions produced this frame.
///
/// The body is the frame in its original order: the toolbar and the expiry strip
/// (`chain_toolbar`, `expiry_strip`), the column model, the two header rows
/// (`paint_chain_header`) and the data rows (`paint_chain_rows`, one `grid::paint_chain_row` each).
/// The geometry the last three share is `OptionChainGrid`.
pub fn draw(ui: &mut egui::Ui, mut inp: OptionChainInputs<'_>) -> OptionChainActions {
    let t = ThemeTokens::of(ui.ctx());
    // active expiry (ISO date) = the user's pick (if still present) else the nearest
    let active = inp
        .expiry_selection
        .as_ref()
        .filter(|t| inp.chains.contains_key(*t))
        .cloned()
        .unwrap_or_else(|| inp.default_expiry.to_string());
    let empty_rows: Vec<vike_marketdata::OptionChainStrike> = Vec::new();
    let (spot, dte, all_rows) = inp
        .chains
        .get(&active)
        .map(|c| (c.underlying_price, c.expiry.dte.max(0), c.rows.as_slice()))
        .unwrap_or((None, 0, empty_rows.as_slice()));
    // The chain is fetched wide (±30); trim the DISPLAYED rows to ±N around ATM so the pill switches
    // instantly with no re-fetch. `atm_index`/`max_volume` below are (re)computed over this windowed slice.
    let rows = strike_window_rows(all_rows, spot, *inp.strike_window);
    let expiry_label = inp
        .expiries
        .iter()
        .find(|e| e.date == active)
        .map(|e| e.label.clone())
        .unwrap_or_else(|| active.clone());
    let mono = t.mono(ThemeTextRole::Body);

    // active underlying = the user's pick (if still a fetched underlying) else the first offered
    // (the listing's first); no label at all while nothing has fetched — never a compiled symbol.
    let active_underlying = inp
        .underlying_selection
        .as_ref()
        .filter(|u| inp.underlyings.iter().any(|x| x == *u))
        .cloned()
        .unwrap_or_else(|| inp.underlyings.first().cloned().unwrap_or_default());

    let mut actions = OptionChainActions::default();

    chain_toolbar(ui, &t, &mut inp, &active_underlying, &expiry_label, spot, &mut actions);
    expiry_strip(ui, &t, &mut inp, &active);

    // ---- column model: CALLS (outer→spine) | Strike IV | PUTS (spine→outer) ----
    // The field sets and their order come from vike-options (`CHAIN_FIELDS` reversed
    // for the calls side — the Python `_columns("chain")` mirror); only the layout
    // weights are GUI-local.
    let mut cols: Vec<(&'static str, f32, OptionChainColumn)> = Vec::with_capacity(20);
    for &f in columns::CHAIN_FIELDS.iter().rev() {
        cols.push((
            columns::header(f),
            weight(f),
            OptionChainColumn::Field { field: f, kind: OptionKind::Call },
        ));
    }
    cols.push(("Strike", 1.1, OptionChainColumn::Strike));
    cols.push(("IV", 0.85, OptionChainColumn::Iv));
    for &f in columns::CHAIN_FIELDS.iter() {
        cols.push((
            columns::header(f),
            weight(f),
            OptionChainColumn::Field { field: f, kind: OptionKind::Put },
        ));
    }
    let strike_col = columns::CHAIN_FIELDS.len(); // 9 — left edge of the centre spine
    let max_volume =
        rows.iter().map(|r| volume_of(&r.call).max(volume_of(&r.put))).fold(1.0_f64, f64::max);
    let atm_index = spot.and_then(|s| rows.iter().position(|r| r.strike >= s));

    // Every column is at least as wide as its widest text this frame (its title or any shown cell)
    // plus the inset that text is drawn at (`space::LG`, right) and a gap on its left (`space::SM`).
    // Shares of the window's width alone gave a 20-column chain ~47pt a column in a ~950pt window,
    // and a right-aligned value wider than its column was painted over its neighbour.
    let header_font = t.font_semibold(ThemeTextRole::Caption);
    let width_of = |text: String, font: &FontId| {
        ui.painter().layout_no_wrap(text, font.clone(), t.theme.text).size().x
    };
    let floors: Vec<f32> = cols
        .iter()
        .map(|(title, _, col)| {
            let widest = rows
                .iter()
                .map(|r| width_of(chain_cell(&t, spot, dte, col, r).0, &mono))
                .fold(width_of(title.to_string(), &header_font), f32::max);
            widest + space::LG + space::SM
        })
        .collect();
    let weights: Vec<f32> = cols.iter().map(|c| c.1).collect();
    let edges = compute_edges(&weights, &floors, ui.available_width());
    let full = edges[cols.len()];

    let g = OptionChainGrid {
        t: &t,
        cols: &cols,
        edges: &edges,
        strike_col,
        full,
        spot,
        dte,
        atm_index,
        max_volume,
        mono: &mono,
        header_font: &header_font,
    };
    paint_chain_header(ui, &g);
    paint_chain_rows(ui, &g, rows, &inp, &active, &mut actions);

    actions
}

/// The frame's resolved grid geometry and per-frame constants, shared by the two header rows and
/// every data row. Borrowed from [`draw`]'s locals and `Copy`, so each phase destructures what it
/// uses.
#[derive(Clone, Copy)]
struct OptionChainGrid<'a> {
    t: &'a ThemeTokens,
    cols: &'a [(&'static str, f32, OptionChainColumn)],
    edges: &'a [f32],
    strike_col: usize,
    full: f32,
    spot: Option<f64>,
    dte: i64,
    atm_index: Option<usize>,
    max_volume: f64,
    mono: &'a FontId,
    /// The column titles' font, which [`draw`] measured them in.
    header_font: &'a FontId,
}
