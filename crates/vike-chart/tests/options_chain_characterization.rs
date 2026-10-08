//! Characterization test for [`vike_chart::options_chain::draw`] — the options-chain grid engine
//! extracted verbatim out of vike-app's `Options =>` tool arm. Mirrors
//! `tests/draw_characterization.rs`'s headless-`egui::Context` harness: drives the REAL `draw`
//! through its public surface (fabricated `RawInput`, a warm-up frame so immediate-mode state
//! settles) and asserts the returned [`vike_chart::OptionChainActions`], so a later refactor of
//! this engine is gated on "the same action still fires for the same input", not merely "it
//! compiles".
//!
//! Font setup: `draw` paints the CALLS/Strike/PUTS super-header in the custom `"bold"` family and
//! the column headers in `"semibold"` — both bound by `vike_ui_theme::appearance::install_type`,
//! which this harness calls. A bare headless `Context` has neither bound, and epaint PANICS on
//! first use of an unbound `FontFamily::Name`.

mod common;

use common::text_sections;
use vike_options::{Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, UnderlyingKind};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::market::{MarketColors, MarketId};
use vike_ui_theme::theme::{Theme, ThemeId};

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
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(61_000.0),
        expiry,
        asof_ms: 1_700_000_000_000,
        source: "deribit".to_string(),
        rows,
    }
}

// ============================ headless harness ============================

/// Installs the app's type (faces AND sizes), so the widths this harness measures are the app's — and
/// `options_chain::draw`'s `"bold"`/`"semibold"` headers are bound (epaint PANICS on an unbound
/// family). A rendering prerequisite only: it changes no `OptionChainActions` value.
fn bind_options_chain_font_families(ctx: &egui::Context) {
    vike_ui_theme::appearance::install_type(ctx, vike_ui_theme::type_scale::TextSize::Small);
}

fn screen_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(960.0, 620.0))
}

/// Run `options_chain::draw` for `frames` headless frames over one persistent `Context` (so
/// immediate-mode widget state — the plot-free version of the chart harness's warm-up — carries
/// over), feeding the same inputs every frame. Returns the LAST frame's actions and its
/// `FullOutput` (accesskit enabled; an `appearance` is `apply`d once, right after the font
/// binding, so `Tokens::of`/`appearance::current` inside `draw` read it for every frame).
fn run_full(
    chains: &std::collections::BTreeMap<String, OptionChain>,
    default_expiry: &str,
    expiries: &[Expiry],
    expiry_sel: &mut Option<String>,
    frames: usize,
    appearance: Option<&Appearance>,
) -> (vike_chart::OptionChainActions, egui::FullOutput) {
    let ctx = egui::Context::default();
    bind_options_chain_font_families(&ctx);
    ctx.enable_accesskit();
    if let Some(a) = appearance {
        vike_ui_theme::appearance::apply(&ctx, a);
    }

    let underlyings = ["BTC".to_string(), "ETH".to_string(), "SOL".to_string()];
    let mut underlying_sel: Option<String> = None;
    let mut strike_window: usize = 12;
    // No user orders/positions in the characterization runs → markers paint nothing (grid stays
    // byte-identical to before the chain-orders feature).
    let books: std::collections::BTreeMap<String, vike_chart::InstrumentBook> =
        std::collections::BTreeMap::new();
    let mut result = vike_chart::OptionChainActions::default();
    let mut last_out: Option<egui::FullOutput> = None;
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
                    // A test fixture spells its own data; production resolves the label from the
                    // settings database (`vike_app_core::ui::tool_views::venue_label`).
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
/// that only cares about [`vike_chart::OptionChainActions`] wants.
fn run(
    chains: &std::collections::BTreeMap<String, OptionChain>,
    default_expiry: &str,
    expiries: &[Expiry],
    expiry_sel: &mut Option<String>,
    frames: usize,
) -> vike_chart::OptionChainActions {
    run_full(chains, default_expiry, expiries, expiry_sel, frames, None).0
}

/// A one-chain, one-expiry fixture for the appearance-facing tests: enough rows that the grid
/// paints bid/ask/strike text, without needing the full 3-strike spread [`sample_chain`] builds.
fn sample() -> (std::collections::BTreeMap<String, OptionChain>, Vec<Expiry>) {
    let mut chains = std::collections::BTreeMap::new();
    chains.insert("2026-08-01".to_string(), sample_chain());
    (chains, vec![Expiry { date: "2026-08-01".to_string(), dte: 5, label: "01 Aug".to_string() }])
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
    // Fetching empty-rows fallback branch (spec §4.2 — no chain yet reads as loading, never
    // empty) and the `spot = None` / `atm_idx = None` paths — the other structural edge from the
    // populated-chain case above.
    let chains: std::collections::BTreeMap<String, OptionChain> = std::collections::BTreeMap::new();
    let expiries: Vec<Expiry> = Vec::new();
    let mut expiry_sel: Option<String> = None;

    let actions = run(&chains, "2026-08-01", &expiries, &mut expiry_sel, 2);

    assert!(!actions.refresh_clicked);
    assert!(expiry_sel.is_none());
}

/// Review Focus 5: bid and ask NUMBERS use the market set's text colours, never its graphic ones.
#[test]
fn bid_and_ask_numbers_are_written_in_the_market_text_colours() {
    let (chains, expiries) = sample();
    let a = Appearance { market: MarketId::TradingView, ..Appearance::default() };
    let (_acts, out) = run_full(&chains, "2026-08-01", &expiries, &mut None, 2, Some(&a));
    let colours: Vec<egui::Color32> =
        text_sections(&out).into_iter().map(|(_, _, _, c)| c).collect();
    let tv = MarketColors::of(MarketId::TradingView);
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
    for id in ThemeId::ALL {
        let a = Appearance { theme: id, ..Appearance::default() };
        let (_acts, out) = run_full(&chains, "2026-08-01", &expiries, &mut None, 2, Some(&a));
        let accent = Theme::of(id).accent;
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
