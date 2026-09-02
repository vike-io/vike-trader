//! The options-chain grid engine — the mirrored CALLS | Strike·IV | PUTS ladder
//! (9 fields/side), ATM highlight, expiry strip, extracted verbatim from vike-app's
//! `Options =>` tool arm (was crates/vike-app/src/main.rs). Pricing/greeks/column model
//! live in `vike-options` (pure); this crate owns only the egui paint + pixel layout.
//! Mirrors `dom::draw`. CI-gated (headless egui, no wgpu).

use egui::{Align2, Color32, FontId, RichText, Sense, StrokeKind};
use vike_options::columns as ocols;
use vike_ui_theme::palette as pal;

/// One of the USER's own resting (working) orders on an option instrument — the render-lite view
/// the chain needs to paint a cancel marker. Plain data, NO vike-core dep: the app fills it from
/// `snap.orders` (see `vike_app_core::build_books`). `coid` is the client-order-id the cancel routes
/// on.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkingOrderLite {
    pub coid: String,
    pub side: i32,
    pub qty: f64,
    pub price: Option<f64>,
    pub filled_qty: f64,
}

/// The USER's own state on ONE option instrument: resting working orders + net signed position.
/// Keyed by `instrument_name` in [`OptionChainInputs::books`]. Plain data (no vike-core dep) — the
/// app fills it from the `CoreSnapshot`. An instrument with neither working orders nor a position
/// never appears (so a strike with nothing paints byte-identically to before this feature).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstrumentBook {
    pub working: Vec<WorkingOrderLite>,
    /// Net signed position size (+long / −short); `0.0` means flat.
    pub position_qty: f64,
}

// ---- palette: the local names this grid paints with, aliased to the ONE canonical
// `vike_ui_theme::palette` (no more literal copies of `vike-app/src/theme.rs` — the values can no
// longer drift from it) ----
const BLUE: Color32 = pal::BLUE;
const GREEN: Color32 = pal::UP;
const RED: Color32 = pal::DOWN;
const TEXT: Color32 = pal::TEXT;
const TEXT2: Color32 = pal::TEXT2;
const TEXT3: Color32 = pal::TEXT3;
const SPINE: Color32 = pal::HOVER; // raised centre Strike band
const BORD: Color32 = pal::BORDER;
/// Accent green — ATM row + active expiry-pill border (canonical hsl(148,72,56); the drifted
/// `(62,224,137)` this crate's history warns against is now impossible — it's `pal::ACCENT`).
const ACCENT: Color32 = pal::ACCENT;

/// Everything the grid needs to render one frame. Borrowed — the app owns the storage.
/// `expiry_sel` is mutated in place on an expiry-strip click (cross-frame selection), the same
/// shape as `DomState`'s fields.
pub struct OptionChainInputs<'a> {
    pub chains: &'a std::collections::BTreeMap<String, vike_options::OptionChain>,
    pub default_expiry: &'a str,
    pub expiries: &'a [vike_options::Expiry],
    pub expiry_sel: &'a mut Option<String>,
    /// The fetched underlyings to offer in the toolbar selector (e.g. `["BTC","ETH","SOL"]`).
    pub underlyings: &'a [String],
    /// The active underlying — mutated in place on a selector-pill click (cross-frame selection,
    /// same shape as `expiry_sel`). `None` = follow the first entry of `underlyings`.
    pub underlying_sel: &'a mut Option<String>,
    /// The display half-window: how many strike rows to show above AND below ATM. Mutated in place
    /// when the `±N strikes` pill is clicked (cycles the presets). The chain is fetched wide (±30),
    /// so changing this never needs a re-fetch — the renderer just trims to ±N.
    pub strike_window: &'a mut usize,
    /// The USER's own working orders + positions, keyed by `instrument_name`. Borrowed (the app
    /// owns the storage; built from the `CoreSnapshot` via `vike_app_core::build_books`). Empty =
    /// nothing owned → the grid paints exactly as it did before this feature.
    pub books: &'a std::collections::BTreeMap<String, InstrumentBook>,
}

/// Outcomes leaving the widget this frame. `refresh_clicked` is set when the Refresh pill is
/// clicked — the app forces an immediate chain re-poll (wakes the options poll thread). `order` is
/// set when the user clicks a tradeable CALL/PUT bid/ask cell — the app turns it into a prefilled
/// CONFIRM ticket. The click ONLY opens the ticket; nothing is submitted here.
#[derive(Default)]
pub struct OptionChainActions {
    pub refresh_clicked: bool,
    pub order: Option<OptionOrderClick>,
    /// Set when the user clicks a working-order marker on the chain — the `client_order_id` to
    /// cancel. The app routes it to `OrderIntent::Cancel` (mirrors the DOM's `DomAction::Cancel`).
    pub cancel: Option<String>,
}

/// A chain bid/ask cell click, carrying everything the confirm ticket needs to prefill. `side` is
/// vike's `i32` order side (+1 Buy / −1 Sell) — the codebase has no `Side` enum; an ASK click buys
/// (+1), a BID click sells (−1). Emitted only for cells whose quote has BOTH an `instrument_name`
/// AND the relevant price.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionOrderClick {
    pub instrument: String,
    pub side: i32,
    pub price: f64,
    pub is_call: bool,
    pub strike: f64,
}

/// Decide the order a chain bid/ask cell click produces. `want_ask` = the ASK cell was clicked
/// (⇒ Buy at ask); otherwise the BID cell (⇒ Sell at bid). Returns `None` — the cell is NOT
/// tradeable — when the quote lacks an `instrument_name` or the relevant price is absent.
fn quote_order(
    quote: &vike_options::OptionQuote,
    want_ask: bool,
    is_call: bool,
    strike: f64,
) -> Option<OptionOrderClick> {
    let instrument = quote.instrument_name.clone()?;
    let price = if want_ask { quote.ask } else { quote.bid }?;
    let side = if want_ask { 1 } else { -1 }; // ask → Buy(+1), bid → Sell(−1)
    Some(OptionOrderClick { instrument, side, price, is_call, strike })
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested) — no egui, so the column-layout math is verifiable standalone.
// ---------------------------------------------------------------------------

/// The `±N strikes` display presets the toolbar pill cycles through (half-window sizes).
pub const STRIKE_WINDOW_PRESETS: [usize; 4] = [6, 12, 20, 30];

/// Next preset in the cycle after `cur` (wraps around); an off-preset value snaps back to 12.
fn next_strike_window(cur: usize) -> usize {
    match STRIKE_WINDOW_PRESETS.iter().position(|&p| p == cur) {
        Some(i) => STRIKE_WINDOW_PRESETS[(i + 1) % STRIKE_WINDOW_PRESETS.len()],
        None => 12,
    }
}

/// Index range `[start, end)` of a BALANCED ±`n` window around the ATM strike (the first strike
/// `>= spot`) — the display window over the fetched-wide chain. Ascending strikes assumed.
/// `spot == None` (no ATM anchor) returns the whole set; `n == 0` returns just the ATM row.
///
/// "Balanced" = the SAME number of strikes above and below the ATM, capped to whichever side has
/// fewer: `k = min(n, below, above)` → `2k + 1` rows. A near expiry lists fewer far-OTM (high)
/// strikes, so a plain ±`n`-with-edge-clamp rendered lopsided (e.g. 12 below + 8 above); balancing
/// makes "±N" show an equal count each side (8 + 8 there), matching the label.
fn strike_window_range(strikes: &[f64], spot: Option<f64>, n: usize) -> (usize, usize) {
    let len = strikes.len();
    if len == 0 {
        return (0, 0);
    }
    let Some(s) = spot else { return (0, len) };
    let atm = strikes.iter().position(|&k| k >= s).unwrap_or(len - 1);
    let below = atm; // strikes below the ATM row (indices 0..atm)
    let above = len - 1 - atm; // strikes above the ATM row
    let k = n.min(below).min(above); // equal count each side
    (atm - k, atm + k + 1)
}

/// Compact signed-qty label for a position badge: `+2` / `-1` for whole sizes, two decimals for
/// fractional deribit sizes (e.g. `+0.10`). Sign is always shown.
fn fmt_pos_qty(q: f64) -> String {
    if (q.fract()).abs() < 1e-9 {
        format!("{q:+.0}")
    } else {
        format!("{q:+.2}")
    }
}

/// One rendered column: a per-side field cell, or one of the two centre-spine columns.
#[derive(Clone, Copy)]
enum Col {
    Field { field: &'static str, call: bool },
    Strike,
    Iv,
}

/// Per-field pixel-weight (a GUI-layout concern only — deliberately NOT part of the
/// venue-agnostic `vike_options::columns` model). The four "wide" numeric fields
/// (theor/bid/ask/distance) get slightly more room than the default; volume gets less (it
/// carries a magnitude bar, not just digits); everything else splits evenly.
fn weight(field: &str) -> f32 {
    match field {
        "bidpct" | "askpct" | "reldist" => 0.95,
        "theor" | "bid" | "ask" | "distance" => 1.05,
        "volume" => 0.8,
        _ => 1.0, // spread
    }
}

/// Cumulative left-edge pixel offsets for a row of weighted columns spanning `full` px total.
/// `edges[i]` is column `i`'s left edge and `edges[i + 1]` its right edge, so column `i` spans
/// `edges[i]..edges[i + 1]`. Returns `weights.len() + 1` offsets starting at `0.0`.
fn compute_edges(weights: &[f32], full: f32) -> Vec<f32> {
    let total_w: f32 = weights.iter().sum();
    let unit = full / total_w;
    let mut edges = Vec::with_capacity(weights.len() + 1);
    let mut acc = 0.0;
    edges.push(0.0);
    for w in weights {
        acc += w * unit;
        edges.push(acc);
    }
    edges
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw one frame of the options-chain grid and return the actions produced this frame.
pub fn draw(ui: &mut egui::Ui, inp: OptionChainInputs<'_>) -> OptionChainActions {
    // active expiry (ISO date) = the user's pick (if still present) else the nearest
    let active = inp
        .expiry_sel
        .as_ref()
        .filter(|t| inp.chains.contains_key(*t))
        .cloned()
        .unwrap_or_else(|| inp.default_expiry.to_string());
    let empty_rows: Vec<vike_options::StrikeRow> = Vec::new();
    let (spot, dte, all_rows) = inp
        .chains
        .get(&active)
        .map(|c| (c.underlying_price, c.expiry.dte.max(0), c.rows.as_slice()))
        .unwrap_or((None, 0, empty_rows.as_slice()));
    // The chain is fetched wide (±30); trim the DISPLAYED rows to ±N around ATM so the pill switches
    // instantly with no re-fetch. `atm_idx`/`max_vol` below are (re)computed over this windowed slice.
    let (w0, w1) = {
        let strikes: Vec<f64> = all_rows.iter().map(|r| r.strike).collect();
        strike_window_range(&strikes, spot, *inp.strike_window)
    };
    let rows = &all_rows[w0..w1];
    let expiry_label = inp
        .expiries
        .iter()
        .find(|e| e.date == active)
        .map(|e| e.label.clone())
        .unwrap_or_else(|| active.clone());
    let mono = FontId::new(13.0, egui::FontFamily::Monospace);

    // active underlying = the user's pick (if still a fetched underlying) else the first offered
    // (canonical: BTC); "BTC" is the last-resort label when nothing has fetched yet.
    let active_underlying = inp
        .underlying_sel
        .as_ref()
        .filter(|u| inp.underlyings.iter().any(|x| x == *u))
        .cloned()
        .unwrap_or_else(|| inp.underlyings.first().cloned().unwrap_or_else(|| "BTC".into()));

    let mut actions = OptionChainActions::default();

    // ---- toolbar: provider · underlying selector · scope pills ···· spot · venue · date ----
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.spacing_mut().button_padding = egui::vec2(14.0, 8.0); // vike pills are roomier
                                                                 // A static (non-interactive-looking) pill — the provider/scope labels.
        let pill = |ui: &mut egui::Ui, t: &str| -> egui::Response {
            ui.add(
                egui::Button::new(RichText::new(t).color(TEXT).size(13.0))
                    .fill(SPINE)
                    .stroke(egui::Stroke::new(1.0, BORD))
                    .min_size(egui::vec2(0.0, 34.0)),
            )
        };
        let _ = pill(ui, "Deribit");
        // Interactive underlying selector — one pill per fetched underlying; the active one gets the
        // green-border/raised styling the expiry tabs below use. Click sets `underlying_sel`.
        for u in inp.underlyings {
            let on = *u == active_underlying;
            let resp = ui.add(
                egui::Button::new(RichText::new(u).color(if on { TEXT } else { TEXT2 }).size(13.0))
                    .fill(if on { SPINE } else { Color32::TRANSPARENT })
                    .stroke(egui::Stroke::new(1.0, if on { ACCENT } else { BORD }))
                    .min_size(egui::vec2(0.0, 34.0)),
            );
            if resp.clicked() {
                *inp.underlying_sel = Some(u.clone());
            }
        }
        let _ = pill(ui, "Next 30d"); // inert scope label
                                      // ±N strikes — clickable: cycles the display half-window through the presets. No re-fetch;
                                      // the chain is fetched wide (±30) and the renderer trims to ±N (see `strike_window_range`).
        let strike_label = format!("±{} strikes", *inp.strike_window);
        if pill(ui, &strike_label).clicked() {
            *inp.strike_window = next_strike_window(*inp.strike_window);
        }
        // Refresh pill — its click is the one scope-pill outcome the caller reads (still inert today).
        actions.refresh_clicked = pill(ui, "Refresh").clicked();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(&expiry_label).color(TEXT2));
            ui.label(RichText::new("·").color(TEXT3));
            ui.label(RichText::new("deribit").color(TEXT2));
            ui.label(RichText::new("·").color(TEXT3));
            if let Some(s) = spot {
                ui.label(
                    RichText::new(format!(
                        "{} {}",
                        active_underlying,
                        vike_ui_theme::fmt::fmt_thousands(s)
                    ))
                    .color(ACCENT),
                );
            }
        });
    });
    ui.add_space(6.0);

    // ---- expiry date strip (pills; active = green border; "0DTE"/"02 Jul" labels) ----
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for e in inp.expiries {
            let on = e.date == active;
            let resp = ui.add(
                egui::Button::new(
                    RichText::new(&e.label).color(if on { TEXT } else { TEXT2 }).size(13.0),
                )
                .fill(if on { SPINE } else { Color32::TRANSPARENT })
                .stroke(egui::Stroke::new(1.0, if on { ACCENT } else { BORD }))
                .min_size(egui::vec2(58.0, 34.0)),
            );
            if resp.clicked() {
                *inp.expiry_sel = Some(e.date.clone());
            }
        }
    });
    ui.add_space(8.0);

    // ---- column model: CALLS (outer→spine) | Strike IV | PUTS (spine→outer) ----
    // The field sets and their order come from vike-options (`CHAIN_FIELDS` reversed
    // for the calls side — the Python `_columns("chain")` mirror); only the layout
    // weights are GUI-local.
    let mut cols: Vec<(&'static str, f32, Col)> = Vec::with_capacity(20);
    for &f in ocols::CHAIN_FIELDS.iter().rev() {
        cols.push((ocols::header(f), weight(f), Col::Field { field: f, call: true }));
    }
    cols.push(("Strike", 1.1, Col::Strike));
    cols.push(("IV", 0.85, Col::Iv));
    for &f in ocols::CHAIN_FIELDS.iter() {
        cols.push((ocols::header(f), weight(f), Col::Field { field: f, call: false }));
    }
    let strike_col = ocols::CHAIN_FIELDS.len(); // 9 — left edge of the centre spine
    let vol_of =
        |q: &Option<vike_options::OptionQuote>| q.as_ref().and_then(|q| q.volume).unwrap_or(0.0);
    let max_vol = rows.iter().map(|r| vol_of(&r.call).max(vol_of(&r.put))).fold(1.0_f64, f64::max);
    let atm_idx = spot.and_then(|s| rows.iter().position(|r| r.strike >= s));

    // value + colour for one cell — value/format from the pure column model; the
    // bid family renders green and the ask family red (Deribit-style, `_GREEN`/`_RED`)
    let cell = |c: &Col, row: &vike_options::StrikeRow| -> (String, Color32) {
        match c {
            Col::Strike => (ocols::fmt_strike(row.strike), TEXT),
            Col::Iv => {
                // centre IV: the call's if a call quote exists, else the put's
                let iv = row
                    .call
                    .as_ref()
                    .and_then(|q| q.iv)
                    .or_else(|| row.put.as_ref().and_then(|q| q.iv));
                (ocols::fmt(iv, "iv"), if iv.is_some() { TEXT } else { TEXT3 })
            }
            Col::Field { field, call } => {
                let q = if *call { row.call.as_ref() } else { row.put.as_ref() };
                let raw = ocols::cell_value(field, q, spot, dte, 0.0);
                let color = match (*field, raw.is_some()) {
                    (_, false) => TEXT3,
                    ("bid" | "bidpct", true) => GREEN,
                    ("ask" | "askpct", true) => RED,
                    _ => TEXT,
                };
                (ocols::fmt(raw, field), color)
            }
        }
    };

    let full = ui.available_width();
    let weights: Vec<f32> = cols.iter().map(|c| c.1).collect();
    let edges = compute_edges(&weights, full);

    // ---- super-header band: CALLS | Strike | PUTS centred over each group ----
    {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(full, 22.0), Sense::hover());
        let p = ui.painter();
        let mid_l = rect.left() + edges[strike_col];
        let mid_r = rect.left() + edges[strike_col + 2];
        let bf = FontId::new(13.0, egui::FontFamily::Name("bold".into()));
        p.text(
            egui::pos2((rect.left() + mid_l) / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            "CALLS",
            bf.clone(),
            BLUE,
        );
        p.text(
            egui::pos2((mid_l + mid_r) / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            "Strike",
            bf.clone(),
            TEXT3,
        );
        p.text(
            egui::pos2((mid_r + rect.right()) / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            "PUTS",
            bf,
            RED,
        );
    }
    // ---- column header row (right-aligned, 8px inset) ----
    {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(full, 24.0), Sense::hover());
        let p = ui.painter();
        let hf = FontId::new(12.0, egui::FontFamily::Name("semibold".into()));
        for (i, c) in cols.iter().enumerate() {
            p.text(
                egui::pos2(rect.left() + edges[i + 1] - 8.0, rect.center().y),
                Align2::RIGHT_CENTER,
                c.0,
                hf.clone(),
                TEXT2,
            );
        }
    }
    // ---- data rows ----
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if rows.is_empty() {
            ui.label(RichText::new("loading…").color(TEXT3));
        }
        for (ri, o) in rows.iter().enumerate() {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(full, 26.0), Sense::hover());
            let p = ui.painter();
            // ITM diagonal-hatch (Qt.BDiagPattern emulation) on in-the-money cells with
            // a quote: calls ITM when strike<spot, puts ITM when strike>spot; skip the
            // centre spine (mirrors OptionsTab._fill_side's `q is not None` gate).
            let hatch = Color32::from_rgba_unmultiplied(47, 52, 60, 70); // _ITM=BORDER, dimmed
            for (i, c) in cols.iter().enumerate() {
                let Col::Field { call, .. } = c.2 else { continue };
                let side = if call { &o.call } else { &o.put };
                let itm = side.is_some()
                    && spot.is_some_and(|s| if call { o.strike < s } else { o.strike > s });
                if !itm {
                    continue;
                }
                let cr = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + edges[i], rect.top()),
                    egui::pos2(rect.left() + edges[i + 1], rect.bottom()),
                );
                let cp = p.with_clip_rect(cr); // else the diagonals overflow into neighbours
                let mut x = cr.left() - 26.0;
                while x < cr.right() {
                    cp.line_segment(
                        [egui::pos2(x, cr.bottom()), egui::pos2(x + 26.0, cr.top())],
                        egui::Stroke::new(1.0, hatch),
                    );
                    x += 6.0;
                }
            }
            let spine = egui::Rect::from_min_max(
                egui::pos2(rect.left() + edges[strike_col], rect.top()),
                egui::pos2(rect.left() + edges[strike_col + 2], rect.bottom()),
            );
            p.rect_filled(spine, 0.0, SPINE);
            if Some(ri) == atm_idx {
                p.line_segment(
                    [egui::pos2(rect.left(), rect.top()), egui::pos2(rect.right(), rect.top())],
                    egui::Stroke::new(2.0, ACCENT),
                );
            }
            // Clickable bid/ask cells (CALL-bid, CALL-ask, PUT-bid, PUT-ask): a click prefills a
            // CONFIRM ticket (nothing is submitted here). Only cells whose quote has BOTH an
            // instrument_name AND the relevant price are interactive (and get a faint hover fill so
            // they read as clickable). All other cells/columns stay inert. `ui.interact` takes
            // `&self`, so it coexists with the `p = ui.painter()` borrow held for this row.
            for (i, c) in cols.iter().enumerate() {
                let Col::Field { field: field @ ("bid" | "ask"), call } = c.2 else { continue };
                let want_ask = field == "ask";
                let side_quote = if call { &o.call } else { &o.put };
                let Some(order) =
                    side_quote.as_ref().and_then(|q| quote_order(q, want_ask, call, o.strike))
                else {
                    continue; // not tradeable → no highlight, no interaction
                };
                let cell_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + edges[i], rect.top()),
                    egui::pos2(rect.left() + edges[i + 1], rect.bottom()),
                );
                let resp = ui.interact(cell_rect, ui.id().with(("optcell", ri, i)), Sense::click());
                if resp.hovered() {
                    // faint side-tinted fill: green for a bid (sell), red for an ask (buy)
                    let tint = if want_ask { RED } else { GREEN };
                    p.rect_filled(
                        cell_rect,
                        0.0,
                        Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), 28),
                    );
                }
                if resp.clicked() {
                    actions.order = Some(order);
                }
            }
            for (i, c) in cols.iter().enumerate() {
                if let Col::Field { field: "volume", call } = c.2 {
                    let v = vol_of(if call { &o.call } else { &o.put });
                    if v > 0.0 {
                        let frac = (v / max_vol).min(1.0) as f32;
                        let cl = rect.left() + edges[i] + 4.0;
                        let cw = (edges[i + 1] - edges[i] - 8.0) * frac;
                        let bar = if call { BLUE } else { RED };
                        p.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(cl, rect.bottom() - 6.0),
                                egui::vec2(cw, 3.0),
                            ),
                            0.0,
                            Color32::from_rgba_unmultiplied(bar.r(), bar.g(), bar.b(), 120),
                        );
                    }
                }
                let (txt, mut col) = cell(&c.2, o);
                if matches!(c.2, Col::Strike) && Some(ri) == atm_idx {
                    col = ACCENT;
                }
                p.text(
                    egui::pos2(rect.left() + edges[i + 1] - 8.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    txt,
                    mono.clone(),
                    col,
                );
            }
            // ---- the USER's own working orders + position markers (options-chain-orders) ----
            // For each side whose quote carries an `instrument_name` we hold a book for, paint a
            // compact position badge (green long / red short) and a clickable working-order marker
            // (`○N`, cancels the first resting order on click). Calls anchor in the free far-LEFT
            // gutter; puts just RIGHT of the centre spine — both land in the empty left area of a
            // NON-bid/ask column, so they never collide with the bid/ask order-click. A side with
            // neither a position nor a working order paints nothing (byte-identical to before).
            let small = FontId::new(11.0, egui::FontFamily::Monospace);
            for is_call in [true, false] {
                let quote = if is_call { &o.call } else { &o.put };
                let Some(inst) = quote.as_ref().and_then(|q| q.instrument_name.as_deref()) else {
                    continue;
                };
                let Some(book) = inp.books.get(inst) else { continue };
                if book.position_qty == 0.0 && book.working.is_empty() {
                    continue;
                }
                let my = rect.center().y;
                // measure-free width for a monospace 11px pill (avoids a galley borrow)
                let pill_w = |txt: &str| 8.0 + txt.chars().count() as f32 * 7.0;
                let mut x = if is_call {
                    rect.left() + edges[0] + 3.0
                } else {
                    rect.left() + edges[strike_col + 2] + 3.0
                };
                // position badge — filled+outlined tag, e.g. "+2" / "-1"
                if book.position_qty != 0.0 {
                    let txt = fmt_pos_qty(book.position_qty);
                    let col = if book.position_qty > 0.0 { GREEN } else { RED };
                    let w = pill_w(&txt);
                    let r = egui::Rect::from_min_size(egui::pos2(x, my - 8.0), egui::vec2(w, 16.0));
                    p.rect_filled(
                        r,
                        3.0,
                        Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 55),
                    );
                    p.rect_stroke(r, 3.0, egui::Stroke::new(1.0, col), StrokeKind::Inside);
                    p.text(egui::pos2(x + 4.0, my), Align2::LEFT_CENTER, txt, small.clone(), col);
                    x += w + 3.0;
                }
                // working-order marker — clickable; a click cancels the FIRST resting order (repeat
                // to peel them off one at a time). Subtle brighten on hover so it reads as clickable.
                if let Some(first) = book.working.first() {
                    let n = book.working.len();
                    let txt = format!("○{n}");
                    let w = pill_w(&txt);
                    let r = egui::Rect::from_min_size(egui::pos2(x, my - 8.0), egui::vec2(w, 16.0));
                    let resp =
                        ui.interact(r, ui.id().with(("optcancel", ri, is_call)), Sense::click());
                    let hot = resp.hovered();
                    p.rect_filled(
                        r,
                        3.0,
                        Color32::from_rgba_unmultiplied(
                            BLUE.r(),
                            BLUE.g(),
                            BLUE.b(),
                            if hot { 95 } else { 45 },
                        ),
                    );
                    p.rect_stroke(r, 3.0, egui::Stroke::new(1.0, BLUE), StrokeKind::Inside);
                    p.text(
                        egui::pos2(x + 4.0, my),
                        Align2::LEFT_CENTER,
                        &txt,
                        small.clone(),
                        if hot { TEXT } else { BLUE },
                    );
                    if resp.clicked() {
                        actions.cancel = Some(first.coid.clone());
                    }
                    if hot {
                        resp.on_hover_text(format!(
                            "Cancel working order {} ({n} resting)",
                            first.coid
                        ));
                    }
                }
            }
        }
    });

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote_with(
        bid: Option<f64>,
        ask: Option<f64>,
        name: Option<&str>,
    ) -> vike_options::OptionQuote {
        let mut q = vike_options::OptionQuote::new(30000.0, vike_options::OptionKind::Call);
        q.bid = bid;
        q.ask = ask;
        q.instrument_name = name.map(str::to_string);
        q
    }

    #[test]
    fn quote_order_ask_buys_at_ask() {
        let q = quote_with(Some(0.01), Some(0.02), Some("BTC-30000-C"));
        let o = quote_order(&q, true, true, 30000.0).expect("ask cell is tradeable");
        assert_eq!(o.side, 1, "an ASK click is a BUY");
        assert_eq!(o.price, 0.02, "priced at the ask");
        assert_eq!(o.instrument, "BTC-30000-C");
        assert!(o.is_call);
        assert_eq!(o.strike, 30000.0);
    }

    #[test]
    fn quote_order_bid_sells_at_bid() {
        let q = quote_with(Some(0.01), Some(0.02), Some("BTC-30000-P"));
        let o = quote_order(&q, false, false, 30000.0).expect("bid cell is tradeable");
        assert_eq!(o.side, -1, "a BID click is a SELL");
        assert_eq!(o.price, 0.01, "priced at the bid");
        assert!(!o.is_call);
    }

    #[test]
    fn quote_order_none_without_instrument_or_price() {
        // no instrument_name → not tradeable
        assert!(quote_order(&quote_with(Some(0.01), Some(0.02), None), true, true, 1.0).is_none());
        // ask absent → ask cell not tradeable
        assert!(quote_order(&quote_with(Some(0.01), None, Some("X")), true, true, 1.0).is_none());
        // bid absent → bid cell not tradeable
        assert!(quote_order(&quote_with(None, Some(0.02), Some("X")), false, true, 1.0).is_none());
    }

    #[test]
    fn weight_known_fields() {
        for f in ["bidpct", "askpct", "reldist"] {
            assert_eq!(weight(f), 0.95, "{f} should be the narrow-pct weight");
        }
        for f in ["theor", "bid", "ask", "distance"] {
            assert_eq!(weight(f), 1.05, "{f} should be the wide-numeric weight");
        }
        assert_eq!(weight("volume"), 0.8, "volume carries a magnitude bar, gets less text room");
        assert_eq!(weight("spread"), 1.0, "an unmatched field falls back to the default weight");
    }

    #[test]
    fn strike_window_cycles_through_presets() {
        assert_eq!(next_strike_window(6), 12);
        assert_eq!(next_strike_window(12), 20);
        assert_eq!(next_strike_window(20), 30);
        assert_eq!(next_strike_window(30), 6, "wraps back to the first preset");
        assert_eq!(next_strike_window(999), 12, "an off-preset value snaps to 12");
    }

    #[test]
    fn strike_window_range_is_balanced_around_atm() {
        // strikes 100..=190 (10 rows, step 10); spot 145 ⇒ ATM = first strike ≥ 145 = 150 (index 5).
        // below = 5 rows (100..=140), above = 4 rows (160..=190).
        let strikes: Vec<f64> = (0..10).map(|i| 100.0 + i as f64 * 10.0).collect();
        // ±2: min(2, below=5, above=4) = 2 each side ⇒ [3, 8): 130..=170 (5 rows, symmetric).
        assert_eq!(strike_window_range(&strikes, Some(145.0), 2), (3, 8));
        // ±0 ⇒ just the ATM row.
        assert_eq!(strike_window_range(&strikes, Some(145.0), 0), (5, 6));
        // BALANCED, not just clamped: ±50 caps to the SHORTER side (above=4) ⇒ 4 each side ⇒ [1, 10)
        // (drops the extra low row so the window stays symmetric around the ATM).
        assert_eq!(strike_window_range(&strikes, Some(145.0), 50), (1, 10));
        // spot at the lowest strike ⇒ ATM index 0, below = 0 ⇒ k = 0 ⇒ just the ATM row.
        assert_eq!(strike_window_range(&strikes, Some(100.0), 3), (0, 1));
        // spot at the highest strike ⇒ ATM index 9, above = 0 ⇒ k = 0 ⇒ just the ATM row.
        assert_eq!(strike_window_range(&strikes, Some(190.0), 3), (9, 10));
        // spot above every strike ⇒ ATM clamps to the last row, above = 0 ⇒ just it.
        assert_eq!(strike_window_range(&strikes, Some(500.0), 1), (9, 10));
        // no ATM anchor / empty input.
        assert_eq!(strike_window_range(&strikes, None, 2), (0, 10));
        assert_eq!(strike_window_range(&[], Some(145.0), 2), (0, 0));
    }

    #[test]
    fn compute_edges_cumulative_offsets() {
        let weights = [1.0_f32, 1.0, 2.0];
        let edges = compute_edges(&weights, 400.0);
        assert_eq!(edges, vec![0.0, 100.0, 200.0, 400.0]);
    }

    #[test]
    fn compute_edges_returns_one_more_than_weights() {
        let weights = [1.0_f32; 5];
        let edges = compute_edges(&weights, 500.0);
        assert_eq!(edges.len(), weights.len() + 1);
        assert_eq!(edges[0], 0.0);
        assert_eq!(*edges.last().unwrap(), 500.0);
    }

    #[test]
    fn strike_spine_lands_at_chain_fields_len() {
        // Mirror `draw`'s column layout exactly: 9 reversed call fields, Strike, IV, then 9
        // forward put fields — `strike_col` must land at CHAIN_FIELDS.len() (today: 9), the left
        // edge of the 2-column centre spine (Strike | IV).
        let fields = ocols::CHAIN_FIELDS;
        let n = fields.len();
        let mut weights: Vec<f32> = Vec::with_capacity(2 * n + 2);
        for &f in fields.iter().rev() {
            weights.push(weight(f));
        }
        weights.push(1.1); // Strike
        weights.push(0.85); // IV
        for &f in fields.iter() {
            weights.push(weight(f));
        }
        let strike_col = n;
        let edges = compute_edges(&weights, 2000.0);

        assert_eq!(strike_col, 9, "CHAIN_FIELDS today has 9 fields/side");
        assert_eq!(edges.len(), weights.len() + 1);
        // The spine spans exactly 2 columns (Strike then IV), both non-degenerate.
        assert!(
            edges[strike_col] < edges[strike_col + 1],
            "Strike column must have positive width"
        );
        assert!(
            edges[strike_col + 1] < edges[strike_col + 2],
            "IV column must have positive width"
        );
        // Calls (0..strike_col) and puts (strike_col+2..end) use the SAME per-field weight()
        // lookup, just in reversed order, so their total pixel widths must match exactly.
        let calls_w = edges[strike_col] - edges[0];
        let puts_w = edges[weights.len()] - edges[strike_col + 2];
        assert!(
            (calls_w - puts_w).abs() < 1e-3,
            "mirrored calls/puts sides must be equal width: {calls_w} vs {puts_w}"
        );
    }
}
