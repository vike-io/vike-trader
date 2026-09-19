//! Characterization test for [`vike_chart::options_chain::draw`] — the options-chain grid engine
//! extracted verbatim out of vike-app's `Options =>` tool arm. Mirrors
//! `tests/draw_characterization.rs`'s headless-`egui::Context` harness: drives the REAL `draw`
//! through its public surface (fabricated `RawInput`, a warm-up frame so immediate-mode state
//! settles) and asserts the returned [`vike_chart::OptionChainActions`], so a later refactor of
//! this engine is gated on "the same action still fires for the same input", not merely "it
//! compiles".
//!
//! Font setup: `draw` paints the CALLS/Strike/PUTS super-header in the custom `"bold"` family and
//! the column headers in `"semibold"` — both registered by vike-app at startup. A bare headless
//! `Context` has neither bound, and epaint PANICS on first use of an unbound `FontFamily::Name`.
//! `draw_characterization.rs` only needed `"light"`/`"semibold"` for `chart::draw`; this harness
//! additionally binds `"bold"` (the one family the chart harness never registered).

use vike_options::{AssetClass, Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow};

// ============================ fixtures ============================

/// One quote with just enough fields populated to exercise the bid/ask/iv/volume cell coloring
/// and the volume magnitude bar, without needing every column populated (`cell_value` already
/// treats absent fields as `None` -> "—", unit-tested in `vike-options`; this fixture only needs
/// to prove the grid paints end-to-end).
fn quote(strike: f64, kind: OptionKind, bid: f64, ask: f64, iv: f64, volume: f64) -> OptionQuote {
    let mut q = OptionQuote::new(strike, kind);
    q.bid = Some(bid);
    q.ask = Some(ask);
    q.mark = Some((bid + ask) / 2.0);
    q.iv = Some(iv);
    q.volume = Some(volume);
    q
}

/// A small BTC chain: 3 strikes straddling a spot of 61,000 (58k/60k/62k), one expiry. Exercises
/// the ATM row (first strike >= spot -> the 62k row), the bid/ask green/red cell coloring, and
/// the volume bar branch.
fn sample_chain() -> OptionChain {
    let expiry = Expiry { date: "2026-08-01".to_string(), dte: 5, label: "01 Aug".to_string() };
    let rows = vec![
        StrikeRow {
            strike: 58_000.0,
            call: Some(quote(58_000.0, OptionKind::Call, 3_100.0, 3_150.0, 0.52, 40.0)),
            put: Some(quote(58_000.0, OptionKind::Put, 120.0, 140.0, 0.58, 15.0)),
        },
        StrikeRow {
            strike: 60_000.0,
            call: Some(quote(60_000.0, OptionKind::Call, 1_800.0, 1_850.0, 0.55, 120.0)),
            put: Some(quote(60_000.0, OptionKind::Put, 700.0, 740.0, 0.56, 80.0)),
        },
        StrikeRow {
            strike: 62_000.0,
            call: Some(quote(62_000.0, OptionKind::Call, 900.0, 940.0, 0.57, 200.0)),
            put: Some(quote(62_000.0, OptionKind::Put, 1_700.0, 1_760.0, 0.59, 30.0)),
        },
    ];
    OptionChain {
        underlying: "BTC".to_string(),
        asset_class: AssetClass::Crypto,
        underlying_price: Some(61_000.0),
        expiry,
        asof_ms: 1_700_000_000_000,
        source: "deribit".to_string(),
        rows,
    }
}

// ============================ headless harness ============================

/// `options_chain::draw` renders the CALLS/Strike/PUTS super-header in `"bold"` and the column
/// headers in `"semibold"`; a bare headless `Context` lacks both and epaint PANICS on first use.
/// Bind both names to the default proportional font so text layout succeeds — a rendering
/// prerequisite only, it changes no `OptionChainActions` value.
fn bind_options_chain_font_families(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default FontDefinitions always define the Proportional family");
    for name in ["semibold", "bold"] {
        fonts.families.insert(egui::FontFamily::Name(name.into()), proportional.clone());
    }
    ctx.set_fonts(fonts);
}

fn screen_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(960.0, 620.0))
}

/// Run `options_chain::draw` for `frames` headless frames over one persistent `Context` (so
/// immediate-mode widget state — the plot-free version of the chart harness's warm-up — carries
/// over), feeding the same inputs every frame. Returns the LAST frame's actions.
fn run(
    chains: &std::collections::BTreeMap<String, OptionChain>,
    default_expiry: &str,
    expiries: &[Expiry],
    expiry_sel: &mut Option<String>,
    frames: usize,
) -> vike_chart::OptionChainActions {
    let ctx = egui::Context::default();
    bind_options_chain_font_families(&ctx);

    let underlyings = ["BTC".to_string(), "ETH".to_string(), "SOL".to_string()];
    let mut underlying_sel: Option<String> = None;
    let mut strike_window: usize = 12;
    // No user orders/positions in the characterization runs → markers paint nothing (grid stays
    // byte-identical to before the chain-orders feature).
    let books: std::collections::BTreeMap<String, vike_chart::InstrumentBook> =
        std::collections::BTreeMap::new();
    let mut result = vike_chart::OptionChainActions::default();
    for f in 0..frames {
        let raw = egui::RawInput {
            screen_rect: Some(screen_rect()),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            result = vike_chart::options_chain::draw(
                ui,
                vike_chart::OptionChainInputs {
                    chains,
                    default_expiry,
                    expiries,
                    expiry_sel,
                    underlyings: &underlyings,
                    underlying_sel: &mut underlying_sel,
                    strike_window: &mut strike_window,
                    books: &books,
                },
            );
        });
        // egui 0.36 makes `TexturesDelta` panic on drop while it still holds unapplied deltas.
        // Nothing renders here — the harness reads the returned actions — so clearing is the
        // explicit "deliberately not uploaded" this pass means. It comes BEFORE the assertion
        // below because `clear` touches no shape, while an assertion that unwinds past unapplied
        // deltas panics a second time in the destructor and ABORTS the process.
        out.textures_delta.clear();
        // The one shared headless-frame geometry invariant (`vike-ui-theme`, `test-support`) —
        // what makes the two action assertions below geometry tests too.
        vike_ui_theme::frame_sanity::assert_frame_sane(&out);
    }
    result
}

// ============================ tests ============================

#[test]
fn no_interaction_frame_reports_no_click_and_leaves_selection_untouched() {
    let mut chains = std::collections::BTreeMap::new();
    chains.insert("2026-08-01".to_string(), sample_chain());
    let expiries =
        vec![Expiry { date: "2026-08-01".to_string(), dte: 5, label: "01 Aug".to_string() }];
    let mut expiry_sel: Option<String> = None;

    let actions = run(&chains, "2026-08-01", &expiries, &mut expiry_sel, 2);

    assert!(
        !actions.refresh_clicked,
        "no pointer interaction this frame ⇒ the Refresh pill must not read as clicked"
    );
    assert!(
        expiry_sel.is_none(),
        "no click on the expiry strip ⇒ the cross-frame selection stays untouched"
    );
}

#[test]
fn empty_chain_renders_without_panicking() {
    // No chains at all, and `default_expiry` doesn't resolve to anything: exercises the
    // "loading…" empty-rows fallback branch and the `spot = None` / `atm_idx = None` paths —
    // the other structural edge from the populated-chain case above.
    let chains: std::collections::BTreeMap<String, OptionChain> = std::collections::BTreeMap::new();
    let expiries: Vec<Expiry> = Vec::new();
    let mut expiry_sel: Option<String> = None;

    let actions = run(&chains, "2026-08-01", &expiries, &mut expiry_sel, 2);

    assert!(!actions.refresh_clicked);
    assert!(expiry_sel.is_none());
}
