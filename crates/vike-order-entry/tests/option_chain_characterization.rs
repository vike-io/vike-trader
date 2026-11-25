//! Characterization test for [`vike_order_entry::option_chain::draw`] — the options-chain grid engine
//! extracted verbatim out of vike-app's `Options =>` tool arm. Mirrors vike-chart's
//! `tests/headless/draw_characterization.rs` headless-`egui::Context` harness: drives the REAL `draw`
//! through its public surface (fabricated `RawInput`, a warm-up frame so immediate-mode state
//! settles) and asserts the returned [`vike_order_entry::option_chain::OptionChainActions`], so a
//! later refactor of this engine is gated on "the same action still fires for the same input", not
//! merely "it compiles". It moved here from vike-chart's `headless` binary with the widget.
//!
//! Font setup: `draw` paints the CALLS/Strike/PUTS super-header in the custom `"bold"` family and
//! the column headers in `"semibold"` — both bound by `vike_ui_theme::appearance::install_type`,
//! which this harness calls at the small text size (the binding vike-chart's chart harness makes). A
//! bare headless `Context` has neither bound, and epaint PANICS on first use of an unbound
//! `FontFamily::Name`.

use vike_marketdata::{
    OptionChain, OptionChainExpiry, OptionChainStrike, OptionKind, OptionQuote,
    OptionUnderlyingKind,
};
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::market::{ThemeMarketColors, ThemeMarketColorsId};
use vike_ui_theme::theme::{ThemePalette, ThemePaletteId};

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
    let expiry =
        OptionChainExpiry { date: "2026-08-01".to_string(), dte: 5, label: "01 Aug".to_string() };
    let rows = vec![
        OptionChainStrike {
            strike: 58_000.0,
            call: Some(quote(58_000.0, OptionKind::Call, 3_100.0, 3_150.0, 0.52, 40.0)),
            put: Some(quote(58_000.0, OptionKind::Put, 120.0, 140.0, 0.58, 15.0)),
        },
        OptionChainStrike {
            strike: 60_000.0,
            call: Some(quote(60_000.0, OptionKind::Call, 1_800.0, 1_850.0, 0.55, 120.0)),
            put: Some(quote(60_000.0, OptionKind::Put, 700.0, 740.0, 0.56, 80.0)),
        },
        OptionChainStrike {
            strike: 62_000.0,
            call: Some(quote(62_000.0, OptionKind::Call, 900.0, 940.0, 0.57, 200.0)),
            put: Some(quote(62_000.0, OptionKind::Put, 1_700.0, 1_760.0, 0.59, 30.0)),
        },
    ];
    OptionChain {
        underlying: "BTC".to_string(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: Some(61_000.0),
        expiry,
        asof_ms: 1_700_000_000_000,
        source: "deribit".to_string(),
        rows,
    }
}

// ============================ headless harness ============================

fn screen_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(960.0, 620.0))
}

/// Run `option_chain::draw` for `frames` headless frames over one persistent `Context` (so
/// immediate-mode widget state — the plot-free version of the chart harness's warm-up — carries
/// over), feeding the same inputs every frame. Returns the LAST frame's actions and its
/// `FullOutput` (accesskit enabled; an `appearance` is `apply`d once, right after the font
/// binding, so `ThemeTokens::of`/`appearance::current` inside `draw` read it for every frame).
fn run_full(
    chains: &std::collections::BTreeMap<String, OptionChain>,
    default_expiry: &str,
    expiries: &[OptionChainExpiry],
    expiry_selection: &mut Option<String>,
    frames: usize,
    appearance: Option<&ThemeAppearance>,
) -> (vike_order_entry::option_chain::OptionChainActions, egui::FullOutput) {
    // No user orders/positions in the characterization runs → markers paint nothing (grid stays
    // byte-identical to before the chain-orders feature).
    let none = std::collections::BTreeMap::new();
    let flat = std::collections::BTreeMap::new();
    run_owned(chains, default_expiry, expiries, expiry_selection, frames, appearance, &none, &flat)
}

/// [`run_full`] with the user's own working orders and net positions keyed by instrument name.
fn run_owned(
    chains: &std::collections::BTreeMap<String, OptionChain>,
    default_expiry: &str,
    expiries: &[OptionChainExpiry],
    expiry_selection: &mut Option<String>,
    frames: usize,
    appearance: Option<&ThemeAppearance>,
    working: &std::collections::BTreeMap<String, Vec<String>>,
    positions: &std::collections::BTreeMap<String, vike_model::Position>,
) -> (vike_order_entry::option_chain::OptionChainActions, egui::FullOutput) {
    let context = egui::Context::default();
    vike_ui_theme::appearance::install_type(
        &context,
        vike_ui_theme::type_scale::ThemeTextSize::Small,
    );
    context.enable_accesskit();
    if let Some(a) = appearance {
        vike_ui_theme::appearance::apply(&context, a);
    }

    let underlyings = ["BTC".to_string(), "ETH".to_string(), "SOL".to_string()];
    let mut underlying_selection: Option<String> = None;
    let mut strike_window: usize = 12;
    let mut result = vike_order_entry::option_chain::OptionChainActions::default();
    let mut last_out: Option<egui::FullOutput> = None;
    for f in 0..frames {
        let raw = egui::RawInput {
            screen_rect: Some(screen_rect()),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = context.run_ui(raw, |ui| {
            result = vike_order_entry::option_chain::draw(
                ui,
                vike_order_entry::option_chain::OptionChainInputs {
                    chains,
                    default_expiry,
                    expiries,
                    expiry_selection,
                    underlyings: &underlyings,
                    underlying_selection: &mut underlying_selection,
                    strike_window: &mut strike_window,
                    working,
                    positions,
                    // A test fixture spells its own data; production resolves the label from the
                    // settings database (`vike_app_core::ui::windows::venue_label`).
                    venue_label: "Deribit",
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
        last_out = Some(out);
    }
    (result, last_out.expect("frames must be at least 1"))
}

/// [`run_full`] with no appearance posed (today's look), discarding the frame — what every test
/// that only cares about [`vike_order_entry::option_chain::OptionChainActions`] wants.
fn run(
    chains: &std::collections::BTreeMap<String, OptionChain>,
    default_expiry: &str,
    expiries: &[OptionChainExpiry],
    expiry_selection: &mut Option<String>,
    frames: usize,
) -> vike_order_entry::option_chain::OptionChainActions {
    run_full(chains, default_expiry, expiries, expiry_selection, frames, None).0
}

/// A one-chain, one-expiry fixture for the appearance-facing tests: enough rows that the grid
/// paints bid/ask/strike text, without needing the full 3-strike spread [`sample_chain`] builds.
fn sample() -> (std::collections::BTreeMap<String, OptionChain>, Vec<OptionChainExpiry>) {
    let mut chains = std::collections::BTreeMap::new();
    chains.insert("2026-08-01".to_string(), sample_chain());
    (
        chains,
        vec![OptionChainExpiry {
            date: "2026-08-01".to_string(),
            dte: 5,
            label: "01 Aug".to_string(),
        }],
    )
}

// ============================ tests ============================

#[test]
fn no_interaction_frame_reports_no_click_and_leaves_selection_untouched() {
    let mut chains = std::collections::BTreeMap::new();
    chains.insert("2026-08-01".to_string(), sample_chain());
    let expiries = vec![OptionChainExpiry {
        date: "2026-08-01".to_string(),
        dte: 5,
        label: "01 Aug".to_string(),
    }];
    let mut expiry_selection: Option<String> = None;

    let actions = run(&chains, "2026-08-01", &expiries, &mut expiry_selection, 2);

    assert!(
        !actions.refresh_clicked,
        "no pointer interaction this frame ⇒ the Refresh pill must not read as clicked"
    );
    assert!(
        expiry_selection.is_none(),
        "no click on the expiry strip ⇒ the cross-frame selection stays untouched"
    );
}

#[test]
fn empty_chain_renders_without_panicking() {
    // No chains at all, and `default_expiry` doesn't resolve to anything: exercises the
    // Fetching empty-rows fallback branch (spec §4.2 — no chain yet reads as loading, never
    // empty) and the `spot = None` / `atm_index = None` paths — the other structural edge from the
    // populated-chain case above.
    let chains: std::collections::BTreeMap<String, OptionChain> = std::collections::BTreeMap::new();
    let expiries: Vec<OptionChainExpiry> = Vec::new();
    let mut expiry_selection: Option<String> = None;

    let actions = run(&chains, "2026-08-01", &expiries, &mut expiry_selection, 2);

    assert!(!actions.refresh_clicked);
    assert!(expiry_selection.is_none());
}

/// Review Focus 5: bid and ask NUMBERS use the market set's text colours, never its graphic ones.
#[test]
fn bid_and_ask_numbers_are_written_in_the_market_text_colours() {
    let (chains, expiries) = sample();
    let a =
        ThemeAppearance { market: ThemeMarketColorsId::TradingView, ..ThemeAppearance::default() };
    let (_acts, out) = run_full(&chains, "2026-08-01", &expiries, &mut None, 2, Some(&a));
    let colours: Vec<egui::Color32> =
        text_sections(&out).into_iter().map(|(_, _, _, c)| c).collect();
    let tv = ThemeMarketColors::of(ThemeMarketColorsId::TradingView);
    assert!(colours.contains(&tv.up_text) && colours.contains(&tv.down_text), "{colours:?}");
    assert!(
        !colours.contains(&tv.up) && !colours.contains(&tv.down),
        "a graphic colour wrote a number"
    );
}

/// Spec §2: the accent is a shape, never the colour of a number — the spot price and the ATM
/// strike were. In every theme, no text is painted in the accent.
#[test]
fn no_number_is_painted_in_the_accent() {
    let (chains, expiries) = sample();
    for id in ThemePaletteId::ALL {
        let a = ThemeAppearance { theme: id, ..ThemeAppearance::default() };
        let (_acts, out) = run_full(&chains, "2026-08-01", &expiries, &mut None, 2, Some(&a));
        let accent = ThemePalette::of(id).accent;
        for (_, s, _, c) in text_sections(&out) {
            assert_ne!(c, accent, "{id:?}: {s:?} is painted in the accent");
        }
    }
}

/// Spec §4.2: loading and empty are different renderings. No chain yet is "fetching"; a fetched
/// expiry with no strikes is the empty tray.
#[test]
fn fetching_and_an_empty_expiry_render_differently() {
    let none = std::collections::BTreeMap::new();
    let (_a, fetching) = run_full(&none, "2026-08-01", &[], &mut None, 2, None);
    let mut empty_chain = sample_chain();
    empty_chain.rows.clear();
    let mut one = std::collections::BTreeMap::new();
    one.insert("2026-08-01".to_string(), empty_chain);
    let (_b, empty) = run_full(&one, "2026-08-01", &[], &mut None, 2, None);
    let words = |o: &egui::FullOutput| {
        text_sections(o).into_iter().map(|(_, s, _, _)| s).collect::<Vec<_>>()
    };
    let tray = vike_ui_theme::icons::EMPTY.accessible_label("");
    assert!(words(&fetching).iter().any(|s| s.contains("Fetching")), "{:?}", words(&fetching));
    assert!(!words(&fetching).contains(&tray), "fetching must not look empty");
    assert!(words(&empty).contains(&tray), "an expiry with no strikes shows the empty tray");
}

/// Spec §4.2: nothing behind it, no button. "Deribit" and "Next 30d" were live-looking pills.
#[test]
fn the_provider_and_scope_labels_are_not_buttons() {
    let (chains, expiries) = sample();
    let (_acts, out) = run_full(&chains, "2026-08-01", &expiries, &mut None, 2, None);
    let update = out.platform_output.accesskit_update.expect("accesskit is enabled");
    let buttons: Vec<String> = update
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == egui::accesskit::Role::Button)
        .filter_map(|(_, n)| n.label().map(str::to_string))
        .collect();
    for word in ["Deribit", "Next 30d"] {
        assert!(!buttons.iter().any(|b| b == word), "{word:?} is a button: {buttons:?}");
    }
    assert!(buttons.iter().any(|b| b.ends_with("Refresh")), "Refresh stays a button: {buttons:?}");
}

/// The user's own markers: a position badge per held instrument (signed, `+2` / `-0.50`) and a
/// `○N` working-order count, painted only where the snapshot holds something and never for a flat,
/// order-less instrument.
#[test]
fn own_positions_and_working_orders_paint_their_markers() {
    let mut chain = sample_chain();
    // ATM on the middle strike; the ±N window shows all three rows.
    chain.underlying_price = Some(60_000.0);
    for (row, (call, put)) in
        chain.rows.iter_mut().zip([("C58", "P58"), ("C60", "P60"), ("C62", "P62")])
    {
        row.call.as_mut().expect("fixture call").instrument_name = Some(call.to_string());
        row.put.as_mut().expect("fixture put").instrument_name = Some(put.to_string());
    }
    let mut chains = std::collections::BTreeMap::new();
    chains.insert("2026-08-01".to_string(), chain);
    let working = std::collections::BTreeMap::from([(
        "P60".to_string(),
        vec!["a".to_string(), "b".to_string()],
    )]);
    let position = |size: f64| vike_model::Position { size, average_price: 0.0 };
    let positions = std::collections::BTreeMap::from([
        ("C60".to_string(), position(2.0)),
        ("P60".to_string(), position(-0.5)),
        ("C62".to_string(), position(0.0)),
    ]);
    let expiries =
        [OptionChainExpiry { date: "2026-08-01".to_string(), dte: 5, label: "01 Aug".to_string() }];
    let words = |o: &egui::FullOutput| {
        text_sections(o).into_iter().map(|(_, s, _, _)| s).collect::<Vec<_>>()
    };
    let (_a, owned) =
        run_owned(&chains, "2026-08-01", &expiries, &mut None, 2, None, &working, &positions);
    let (_b, bare) = run_full(&chains, "2026-08-01", &expiries, &mut None, 2, None);
    let (owned, bare) = (words(&owned), words(&bare));
    let count = |ws: &[String], w: &str| ws.iter().filter(|x| *x == w).count();
    for want in ["+2", "-0.50", "○2"] {
        assert_eq!(count(&owned, want), count(&bare, want) + 1, "{want:?} once more: {owned:?}");
    }
    assert_eq!(owned.len(), bare.len() + 3, "the three markers and nothing else: {owned:?}");
}

// ============================ the owner's window (2026-11-16) ============================

/// A realistic BTC chain, the shape of the owner's report: 45 five-digit strikes around a spot of
/// 64,269.66, and five-digit bids, asks and volumes and three-digit IVs on both sides of each.
fn wide_chain(expiry: OptionChainExpiry) -> OptionChain {
    let rows = (0..45_u32)
        .map(|i| {
            let strike = 10_000.0 + 2_000.0 * f64::from(i);
            let iv = 1.2345 + 0.01 * f64::from(i);
            let side =
                |kind, bid: f64| Some(quote(strike, kind, bid, bid + 1_234.56, iv, 98_765.0));
            OptionChainStrike {
                strike,
                call: side(OptionKind::Call, 12_345.67),
                put: side(OptionKind::Put, 23_456.78),
            }
        })
        .collect();
    OptionChain {
        underlying: "BTC".to_string(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: Some(64_269.66),
        expiry,
        asof_ms: 1_700_000_000_000,
        source: "deribit".to_string(),
        rows,
    }
}

/// One settled frame of the chain drawn into a `size` body at the default appearance:
/// [`wide_chain`], the seven underlyings and eight expiries of the owner's toolbar, and the widest
/// strike window (±30, which takes every one of [`wide_chain`]'s 45 rows: 28 below the money, 17 at
/// or above).
fn render_wide_chain(size: egui::Vec2) -> egui::FullOutput {
    let context = egui::Context::default();
    vike_ui_theme::appearance::install_type(
        &context,
        vike_ui_theme::type_scale::ThemeTextSize::Small,
    );
    context.enable_accesskit();
    vike_ui_theme::appearance::apply(&context, &ThemeAppearance::default());
    let labels = ["17 Nov", "18 Nov", "19 Nov", "20 Nov", "27 Nov", "04 Dec", "25 Dec", "29 Jan"];
    let expiries: Vec<OptionChainExpiry> = labels
        .iter()
        .zip(1..)
        .map(|(label, dte)| OptionChainExpiry {
            date: format!("2026-12-{dte:02}"),
            dte,
            label: label.to_string(),
        })
        .collect();
    let chains = std::collections::BTreeMap::from([(
        expiries[0].date.clone(),
        wide_chain(expiries[0].clone()),
    )]);
    let underlyings: Vec<String> =
        ["BTC", "ETH", "SOL", "HYPE", "XRP", "AVAX", "TRX"].into_iter().map(String::from).collect();
    let (mut expiry_selection, mut underlying_selection, mut strike_window) = (None, None, 30);
    let (working, positions) =
        (std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
    let mut last = None;
    for frame in 0..2_u32 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(f64::from(frame) / 60.0),
            ..Default::default()
        };
        let mut out = context.run_ui(raw, |ui| {
            vike_order_entry::option_chain::draw(
                ui,
                vike_order_entry::option_chain::OptionChainInputs {
                    chains: &chains,
                    default_expiry: &expiries[0].date,
                    expiries: &expiries,
                    expiry_selection: &mut expiry_selection,
                    underlyings: &underlyings,
                    underlying_selection: &mut underlying_selection,
                    strike_window: &mut strike_window,
                    working: &working,
                    positions: &positions,
                    venue_label: "Deribit",
                },
            );
        });
        out.textures_delta.clear(); // see `run_owned`: nothing renders here
        vike_ui_theme::frame_sanity::assert_frame_sane(&out);
        last = Some(out);
    }
    last.expect("two frames ran")
}

/// Every painted text with its laid-out rect (the galley's own rect where it was painted).
fn text_rects(out: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    fn walk(shape: &egui::Shape, found: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(t) => {
                found.push((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2())));
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    out.shapes.iter().for_each(|c| walk(&c.shape, &mut found));
    found
}

/// Every widget that names itself to the accessibility tree (the toolbar's badges, selector
/// segments, pills and words, and the expiry strip), with the rect egui laid it out at. A label's
/// `TextRun` children (egui's selectable-text rows, on the label's own rect) are not widgets.
fn labelled_widgets(out: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    let update = out.platform_output.accesskit_update.as_ref().expect("accesskit is enabled");
    update
        .nodes
        .iter()
        .filter(|(_, n)| n.role() != egui::accesskit::Role::TextRun)
        .filter_map(|(_, n)| {
            let words = n.label().or(n.value())?.to_string();
            let b = n.bounds()?;
            let at = |x: f64, y: f64| egui::pos2(x as f32, y as f32);
            Some((words, egui::Rect::from_min_max(at(b.x0, b.y0), at(b.x1, b.y1))))
        })
        .collect()
}

/// The owner's report (2026-11-16, a ~950 × 600 Options window): the calls and puts columns were
/// painted over each other, and the spot · venue · expiry words sat on the toolbar's pills. Every
/// column is now as wide as its widest text and the toolbar wraps, so at the OLD default size (the
/// plain tool window), the reported size and the NEW default (twice the plain tool window,
/// `crates/vike-app-core/src/ui/window/spawn.rs`'s `plan_window_spawn`) no two texts on one line meet and
/// no two toolbar widgets meet; a body narrower than the table gets a table wider than itself (the
/// window's body scrolls it sideways), never squeezed columns.
#[test]
fn no_cell_or_toolbar_widget_overlaps_at_the_old_or_the_new_default_size() {
    let plain = vike_ui_theme::value::workspace::TOOL_WINDOW_SIZE;
    for size in [plain, egui::vec2(950.0, 600.0), plain * 2.0] {
        let out = render_wide_chain(size);
        let texts = text_rects(&out);
        assert!(texts.len() >= 45 * 20, "{size:?}: 45 rows of 20 cells, got {} texts", texts.len());
        for (i, (a_words, a)) in texts.iter().enumerate() {
            for (b_words, b) in &texts[i + 1..] {
                let one_line = (a.center().y - b.center().y).abs() < 0.5;
                assert!(
                    !(one_line && a.shrink(0.5).intersects(b.shrink(0.5))),
                    "{size:?}: {a_words:?} at {a:?} is painted over {b_words:?} at {b:?}"
                );
            }
        }
        let widgets = labelled_widgets(&out);
        assert!(widgets.iter().any(|(w, _)| w.ends_with("Refresh")), "{size:?}: {widgets:?}");
        for (i, (a_words, a)) in widgets.iter().enumerate() {
            for (b_words, b) in &widgets[i + 1..] {
                assert!(
                    !a.shrink(0.5).intersects(b.shrink(0.5)),
                    "{size:?}: widget {a_words:?} at {a:?} overlaps {b_words:?} at {b:?}"
                );
            }
        }
        if size == plain {
            let right = texts.iter().map(|(_, r)| r.right()).fold(0.0_f32, f32::max);
            assert!(
                right > size.x,
                "the table must run past a {size:?} body, not squeeze: {right}"
            );
        }
    }
}

// ============================ painted text ============================

/// One painted text section: `(galley, text, font, colour)`. `galley` numbers the text shapes in
/// paint order, so the sections of one galley group.
type Section = (usize, String, egui::FontId, egui::Color32);

/// Every text section the frame painted. The colour is the section's own, else the shape's
/// fallback, with the shape's override over both — what the painter resolves. The reading of
/// vike-chart's `tests/headless/common`'s `text_sections`, which this suite used before it moved here
/// with the widget: per SECTION, because an icon's label (`vike_ui_theme::icons`' `before`) is one
/// galley of two sections and the empty tray is asserted by its words' section alone.
fn text_sections(out: &egui::FullOutput) -> Vec<Section> {
    fn walk(shape: &egui::Shape, galleys: &mut usize, out: &mut Vec<Section>) {
        match shape {
            egui::Shape::Text(t) => {
                let job = &t.galley.job;
                for s in &job.sections {
                    let own = if s.format.color == egui::Color32::PLACEHOLDER {
                        t.fallback_color
                    } else {
                        s.format.color
                    };
                    // epaint 0.36's section range is `Range<ByteIndex>`; `ByteIndex` wraps a byte
                    // offset into `job.text`.
                    let text = job.text[s.byte_range.start.0..s.byte_range.end.0].to_string();
                    let colour = t.override_text_color.unwrap_or(own);
                    out.push((*galleys, text, s.format.font_id.clone(), colour));
                }
                *galleys += 1;
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, galleys, out)),
            _ => {}
        }
    }
    let (mut galleys, mut v) = (0, Vec::new());
    for cs in &out.shapes {
        walk(&cs.shape, &mut galleys, &mut v);
    }
    v
}
