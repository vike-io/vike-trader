//! The tool-tab routing, run-gate, chat-key, results-strip and toolbar-chip scenarios, with the
//! roster tables they assert through. "The module doc" below is the root's, not this file's.

use std::assert_matches;
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use vike_data::HistoryStore;
use vike_studio::{ChatApiKeys, ResultsTab, RightTab};
use vike_ui_theme::icons;

use super::support::{
    all_text, button, buttons, harness_over, has_button, is_disabled, label_values, nodes,
    run_button, settle, shell,
};
use crate::common::{empty_store, qa_name, seeded_store, spawn_backtesthub_server, state};

// ============================ the roster tables ============================
// The store fixtures and the posed-state constructor ([`common::empty_store`]/
// [`common::seeded_store`]/[`common::qa_name`]/[`common::state`]) MOVED to
// `crates/vike-studio/tests/headless/common/mod.rs` when `crates/vike-studio/tests/headless/tessellation_goldens.rs`
// became their second consumer — that module's doc carries the move's argument. The tables below
// stay: they are assertion vocabulary, and only this file asserts with them.

/// The title each tool pane opens with through `crates/vike-studio/src/studio.rs`'s
/// `pane_header` — the one piece of text that says WHICH pane the shared right panel is
/// showing.
///
/// Every value was checked against the rest of a shell frame: none of the six is emitted as a
/// `Role::Label` anywhere else (the Indicators pane's category captions are
/// Trend/Momentum/Volatility/Volume/Statistics/Patterns/Price/Structure/User, and its indicator
/// rows are Buttons, not Labels). Assertions below compare for EQUALITY, not containment, so a
/// longer sentence that merely mentions one of these words cannot satisfy them.
fn pane_title(t: RightTab) -> &'static str {
    match t {
        RightTab::Sweep => "Param scan & Validate",
        RightTab::Strategy => "Strategy",
        RightTab::Data => "Stored data",
        RightTab::Indicators => "Indicators",
        RightTab::Saved => "Saved strategies",
        RightTab::Research => "Research",
        RightTab::Chat => "AI Copilot",
    }
}

/// The results strip in display order, as `crates/vike-studio/src/panes/results.rs`'s `results_ui`
/// emits it.
const RESULTS_TABS: [ResultsTab; 5] = [
    ResultsTab::Equity,
    ResultsTab::Trades,
    ResultsTab::Performance,
    ResultsTab::Distribution,
    ResultsTab::Validation,
];

/// The strip caption for each results tab.
///
/// `ResultsTab` carries no `Debug`, and this test has no business adding a derive to the library to
/// suit itself — the same reasoning as `crates/vike-studio/tests/saved_pane_a11y.rs`'s `describe`.
/// So every failure message below names a tab through this.
fn results_tab_name(t: ResultsTab) -> &'static str {
    match t {
        ResultsTab::Equity => "Equity",
        ResultsTab::Trades => "Trades",
        ResultsTab::Performance => "Performance",
        ResultsTab::Distribution => "Distribution",
        ResultsTab::Validation => "Validation",
    }
}

/// A string only THIS tab's BODY renders, or `None` when the body carries no distinctive text.
///
/// Each marker is an unconditional cell of its tab's table — `trades_tab`'s column header,
/// `perf_rows`' catalog id, `validation_rows`'s PSR label — so it is present even for a run that
/// never traded. The two `None`s are `equity_tab` and `distribution_tab`, both `egui_plot`
/// canvases with nothing to read; see the module doc for exactly how much those two poses still
/// assert and why that is stated rather than papered over.
fn results_body_marker(t: ResultsTab) -> Option<&'static str> {
    match t {
        ResultsTab::Equity => None,
        ResultsTab::Trades => Some("entry"),
        ResultsTab::Performance => Some("profit_factor"),
        ResultsTab::Distribution => None,
        ResultsTab::Validation => Some("Prob. Sharpe > 0 (PSR)"),
    }
}

// ============================ tests ============================

/// SAFETY, not tidiness: [`qa_name`] must name the tab it claims to, and the six names must be
/// distinct.
///
/// `new_with_qa` IGNORES a name it does not recognise, and a session that fell through that way is
/// the ONE configuration in which a frame rendered by this file could write the developer's real
/// `studio_workspace.json`. A typo in the table would also silently pose every harness on the
/// default tab, which would make the mutual-exclusion test below pass for the wrong reason.
#[test]
fn qa_name_names_the_tab_it_claims_to() {
    let mut seen: Vec<&str> = Vec::new();
    for tab in RightTab::ALL {
        let name = qa_name(tab);
        assert_eq!(
            RightTab::from_qa_str(name),
            Some(tab),
            "{name:?} must round-trip through from_qa_str"
        );
        assert!(!seen.contains(&name), "{name:?} names two tabs");
        seen.push(name);
    }
}

/// **The routing claim.** Each of the six tool tabs draws ITS pane and none of the other five.
///
/// Asserted as 49 `(pose, title)` pairs where presence must equal identity, which is what makes it
/// unpassable by a constant answer: an implementation that always renders one pane fails six of
/// seven poses, one that renders them all fails all seven. Reddens on swapping two arms of
/// `tool_pane_ui`'s `match self.right_tab` — an edit every pure test in the crate is blind to.
///
/// The floor runs FIRST so no assertion below it can pass vacuously over a frame that stopped
/// rendering the shell. No pointer event is ever sent here; the module doc says why that matters.
#[test]
fn each_tool_tab_renders_its_own_pane_and_only_its_own() {
    let (dir, store) = empty_store();
    for tab in RightTab::ALL {
        let mut h = shell(&store, &dir, tab);
        settle(&mut h);

        assert_eq!(
            buttons(&h, &icons::REFRESH.accessible_label("Refresh")).len(),
            1,
            "the {} pose lost the toolbar entirely",
            pane_title(tab)
        );
        for rail in RightTab::ALL {
            assert_eq!(
                buttons(&h, rail.label()).len(),
                1,
                "the {} pose must draw exactly one rail button for {}",
                pane_title(tab),
                pane_title(rail)
            );
        }

        let titles = label_values(&h);
        for other in RightTab::ALL {
            let want = other == tab;
            let got = titles.iter().any(|t| t == pane_title(other));
            assert_eq!(
                got,
                want,
                "with {} open, {:?} must{} be on screen; labels: {titles:?}",
                pane_title(tab),
                pane_title(other),
                if want { "" } else { " NOT" }
            );
        }
    }
}

/// **The run gates, both directions.** A store with nothing in it arms no dispatch affordance; a
/// store with a slice in it arms them — and the sweep needs a parsable grid on top of the slice.
///
/// Paired so a gate hard-wired either way fails one half. The third leg (an armed slice with an
/// empty grid keeping `▶ Run Sweep` disabled, then the same frame arming it once `grid` is seeded)
/// is what stops the seeded half being passable by "a slice enables everything".
///
/// ⚠ Nothing here CLICKS any of these: a click spawns a real backtest worker thread.
#[test]
fn an_empty_store_arms_no_run_affordance_and_a_seeded_one_arms_them() {
    let (empty_dir, empty) = empty_store();
    let mut h = shell(&empty, &empty_dir, RightTab::Sweep);
    settle(&mut h);

    assert!(is_disabled(&h, &run_button("Run")), "no slice, no Run");
    assert!(is_disabled(&h, &run_button("Run param scan")), "no slice, no Sweep");
    assert!(is_disabled(&h, "Walk-Forward"), "no slice, no Walk-Forward");
    assert!(
        !has_button(&h, &run_button("Run backtest")),
        "the empty-store arm of empty_state renders the backfill copy INSTEAD of the \
         getting-started button row — offering a dispatch here would act on nothing"
    );
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains("The data store is empty")),
        "an empty store keeps its own copy; rendered: {text:?}"
    );

    let (seeded_dir, seeded) = seeded_store();
    let mut h = shell(&seeded, &seeded_dir, RightTab::Sweep);
    settle(&mut h);

    assert!(!is_disabled(&h, &run_button("Run")), "a selected slice must arm Run");
    assert!(!is_disabled(&h, &run_button("Run backtest")), "...and the central panel's twin of it");
    assert!(!is_disabled(&h, "Walk-Forward"), "a selected slice is all Walk-Forward needs");
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains("Pick a data slice")),
        "a store with data gets the getting-started copy; rendered: {text:?}"
    );
    assert!(
        is_disabled(&h, &run_button("Run param scan")),
        "a slice alone is not enough: start_sweep returns early on an empty grid, and a styled \
         primary button that silently no-ops reads as broken"
    );

    h.state_mut().grid = vec![("fast".to_string(), "3, 5".to_string())];
    settle(&mut h);
    assert!(!is_disabled(&h, &run_button("Run param scan")), "a parsable grid must arm the sweep");
}

/// **The provider-key gate.** With no key the Send button is really disabled in the tree an
/// assistive client reads, and the pane says why; with a key it becomes pressable.
///
/// Both halves hold the slice and the input constant, so the only thing that moves is the key —
/// which is what makes this a test of `can_send`'s `has_key()` conjunct rather than of the other
/// three.
///
/// ⚠ It never clicks Send, in the voice of
/// `crates/vike-gateway-panel/tests/panel/a11y_form.rs`'s never-click-Save warning:
/// `start_chat_send` spawns a worker that builds a provider client and talks to it over the
/// network. Opening the pane and reading the tree is the whole interaction, and the planted key is
/// not a real one.
#[test]
fn the_chat_pane_offers_no_send_without_a_provider_key() {
    let (dir, store) = seeded_store();

    let mut keyless = state(&store, &dir, RightTab::Chat, ChatApiKeys::default());
    keyless.chat.input = "an rsi mean-reversion strategy".to_string();
    let mut h = harness_over(keyless);
    settle(&mut h);
    assert!(is_disabled(&h, "Send"), "no provider key, no Send");
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains("No provider key found")),
        "a disabled Send must SAY why; rendered: {text:?}"
    );

    let keys = ChatApiKeys { anthropic: Some("not-a-real-key".to_string()), cerebras: None };
    let mut keyed = state(&store, &dir, RightTab::Chat, keys);
    keyed.chat.input = "an rsi mean-reversion strategy".to_string();
    let mut h = harness_over(keyed);
    settle(&mut h);
    assert!(!is_disabled(&h, "Send"), "a provider key, a slice and a prompt must arm Send");
}

/// **The results surface, over a REAL backtest.** Every strip caption is CLICKED, and each pose
/// asserts three independent things: the state moved, the strip's own toggled flags moved, and the
/// BODY that renders only for that tab is the only text-bearing body on screen.
///
/// The run goes through `start_run` plus a bounded `poll` loop — the real dispatch path, and the
/// shape `crates/vike-studio/src/studio.rs`'s own `start_run_then_poll_reaches_a_result` uses —
/// rather than assigning a hand-built result: a fabricated equity curve is exactly where
/// `distribution_tab`'s fold produces a runaway coordinate, and reddening a geometry gate on a
/// fixture this file invented would prove nothing about the product.
///
/// Clicking rather than assigning `state_mut().tab` is the point: it proves `results_ui`'s
/// `(tab, name)` table and the `match tab` under it AGREE, which an assignment cannot.
#[test]
fn the_results_pane_renders_every_tab_of_a_real_backtest() {
    let (dir, store) = seeded_store();
    let mut st = state(&store, &dir, RightTab::Sweep, ChatApiKeys::default());
    // The run leaves this process now, so give it something hermetic to answer it.
    st.backend = vike_studio::Backend::Remote { addr: spawn_backtesthub_server(store.clone()) };
    st.start_run();
    assert!(st.running, "a seeded store auto-selects row 0, so Run must dispatch");
    for _ in 0..400 {
        st.poll();
        if !st.running {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!st.running, "400 bars of Rhai must finish inside the 4s budget");
    assert_matches!(st.last, Some(Ok(_)), "the run must produce a result for the pane to render");

    let mut h = harness_over(st);
    settle(&mut h);

    for tab in RESULTS_TABS {
        if h.state().tab != tab {
            button(&h, results_tab_name(tab)).click();
            settle(&mut h);
        }
        assert!(
            h.state().tab == tab,
            "clicking {} must select it, but the state reads {}",
            results_tab_name(tab),
            results_tab_name(h.state().tab)
        );
        let open: Vec<String> = nodes(&h, |n| n.accesskit_node().role() == Role::Label)
            .into_iter()
            .filter_map(|n| {
                let a = n.accesskit_node();
                a.label().or_else(|| a.value()).map(|s| s.to_string())
            })
            .collect();
        assert!(
            open.iter().any(|s| s == results_tab_name(tab)),
            "the open tab {} must be a label (spec §4.2)",
            results_tab_name(tab)
        );
        for other in RESULTS_TABS {
            assert_eq!(
                has_button(&h, results_tab_name(other)),
                other != tab,
                "with {} open, {} is a button exactly when it is not the open tab",
                results_tab_name(tab),
                results_tab_name(other)
            );
            if let Some(marker) = results_body_marker(other) {
                let present = all_text(&h).iter().any(|t| t == marker);
                assert_eq!(
                    present,
                    other == tab,
                    "with {} open, {marker:?} must{} be rendered",
                    results_tab_name(tab),
                    if other == tab { "" } else { " NOT" }
                );
            }
        }
    }
}

/// **A tab switch is WIRED and re-renders.** The toolbar's strategy chip exists so the operator can
/// see WHICH strategy `Run` will execute; clicking it must open the Strategy pane, and the pane
/// that was open must go away.
///
/// This is the one test that proves a pose CHANGE rather than a pose. Without it, an implementation
/// that renders the right pane for a constructor-supplied tab and ignores every later assignment
/// passes [`each_tool_tab_renders_its_own_pane_and_only_its_own`] completely.
#[test]
fn the_toolbar_strategy_chip_opens_the_strategy_pane() {
    let (dir, store) = seeded_store();
    let mut h = shell(&store, &dir, RightTab::Sweep);
    settle(&mut h);
    let titles = label_values(&h);
    assert!(
        titles.iter().any(|t| t == pane_title(RightTab::Sweep)),
        "the harness starts on the Sweep pane; labels: {titles:?}"
    );

    button(&h, &icons::STRATEGY.accessible_label("rhai")).click();
    settle(&mut h);

    assert_eq!(h.state().right_tab, RightTab::Strategy, "the chip must select the Strategy tab");
    let titles = label_values(&h);
    assert!(
        titles.iter().any(|t| t == pane_title(RightTab::Strategy)),
        "...and the panel must actually re-render as that pane; labels: {titles:?}"
    );
    assert!(
        !titles.iter().any(|t| t == pane_title(RightTab::Sweep)),
        "...with the previous pane gone, not stacked under it; labels: {titles:?}"
    );
}

/// **The Import CSV form fits the Data pane** (decision 0272): open over a posed preview whose file
/// has a long path, a long header cell and six columns of rows, every label, field, button and the
/// pane's own title stays inside the tools panel, and nothing of the central panel reaches into it.
/// A form laid out wider than the pane once cost the tools panel its width: the central panel painted
/// over the pane's left edge ("tored data", "ort CSV…"). The preview's six columns now scroll
/// sideways inside the pane instead.
#[test]
fn the_import_csv_form_fits_inside_the_data_pane() {
    let (dir, store) = empty_store();
    let mut h = harness_over(posed_import_form(&store, &dir));
    settle(&mut h);
    assert_the_import_form_fits(&h, 1440.0);
    // ...and in `crates/vike-studio/examples/studio_shot.rs`'s own window (1600×1000 pixels at 1.5
    // pixels per point) after its three frames, the capture a human looks at, and once settled.
    let shot = egui::vec2(1600.0 / 1.5, 1000.0 / 1.5);
    let mut h = egui_kittest::Harness::builder()
        .with_size(shot)
        .with_pixels_per_point(1.5)
        .with_max_steps(64)
        .build_ui_state(
            |ui, st: &mut vike_studio::StudioState| {
                if vike_ui_theme::harness::type_ready(ui.ctx()) {
                    st.ui(ui);
                }
            },
            posed_import_form(&store, &dir),
        );
    h.run_steps(3);
    assert_the_import_form_fits(&h, shot.x);
    settle(&mut h);
    assert_the_import_form_fits(&h, shot.x);
}

/// The Data pane with the Import CSV form open over a posed preview: a long path, a long header
/// cell, and six columns of rows.
fn posed_import_form(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    dir: &tempfile::TempDir,
) -> vike_studio::StudioState {
    use vike_datahub_client::archive::ImportPlanPreviewRow;
    use vike_datahub_client::upload::CsvUploadDone;

    let mut st = state(store, dir, RightTab::Data, ChatApiKeys::default());
    st.arm_csv_upload(Some("127.0.0.1:1".to_string()));
    let day = 16_436 * vike_model::MS_PER_DAY;
    let row = ImportPlanPreviewRow {
        timestamp: day,
        open: "1.0012".into(),
        high: "1.0019".into(),
        low: "0.9987".into(),
        close: "1.0004".into(),
        volume: "1234567.891".into(),
    };
    let form = &mut st.data_browser.import;
    form.open = true;
    form.path = r"C:\Users\someone\Downloads\a-rather-long-export-name-USDT-USD-daily.csv".into();
    form.label = "coinbase".into();
    form.symbol = "USDT-USD".into();
    form.finish(
        true,
        Ok(CsvUploadDone {
            header: vec![
                "timestamp_ms".into(),
                "a_long_column_name_from_some_spreadsheet_export".into(),
            ],
            preview: vec![row.clone(), row.clone(), row],
            bars: 3650,
            days: 3650,
            first_day: Some(day),
            last_day: Some(day + 3649 * vike_model::MS_PER_DAY),
            ..CsvUploadDone::default()
        }),
    );
    form.map_columns = true;
    form.picks = [1, 0, 0, 0, 2, 0];
    st
}

/// **The ONE strategy-file list fits the Strategy pane, every state in its words** (plan 2b): over
/// a posed served list — a built file, an unbuilt edited one, a long path, a multi-line build
/// failure, a file that cannot run here, a Rhai file and a broken one — every row's path and state
/// line lies inside the tools panel (a long one truncates, never widens the pane), the selected
/// file is the one Run executes, and nothing of the central panel reaches across the pane's edge.
#[test]
fn the_strategy_file_list_fits_the_strategy_pane_with_every_state() {
    use vike_backtesthub_client::proto::WireStrategyFile;
    use vike_ui_theme::value::studio;

    let (dir, store) = empty_store();
    let mut st = state(&store, &dir, RightTab::Strategy, ChatApiKeys::default());
    let file = |file: &str, build: &str, broken: &str, edited: bool| WireStrategyFile {
        file: file.to_string(),
        build: build.to_string(),
        broken: broken.to_string(),
        edited,
    };
    let long = "grid_dca/a_rather_long_strategy_file_name_that_must_truncate_in_the_pane.rs";
    st.strategy_source = vike_studio::StrategySource::Native;
    st.strategy_files = Some(Ok(vec![
        file("trend/buy_hold.rs", "built 3f9a1c0e", "", false),
        file("trend/momentum.rs", "not built", "", true),
        file(
            long,
            "build failed: error[E0425]: cannot find value `rungz` in this scope\n --> x",
            "",
            false,
        ),
        file("market_making/spread_maker.rs", "cannot run here: no sandbox on this box", "", false),
        file("trend/sma_cross.rhai", "", "", false),
        file("other/notes.rhai", "", "no header: a strategy file opens with comment lines", false),
    ]));
    st.native_file = "trend/buy_hold.rs".to_string();
    let mut h = harness_over(st);
    settle(&mut h);

    let right = 1440.0 - studio::RAIL_WIDTH;
    let left = right - studio::TOOLS_PANEL_W;
    let watched = [
        "Files on the server",
        "Refresh list",
        "Open in editor",
        "trend/buy_hold.rs",
        "built 3f9a1c0e",
        "not built · edited",
        long,
        "build failed: error[E0425]: cannot find value `rungz` in this scope",
        "cannot run here: no sandbox on this box",
        "Rhai",
        "broken: no header: a strategy file opens with comment lines",
    ];
    let mut seen = Vec::new();
    for node in h.root().children_recursive() {
        let a = node.accesskit_node();
        let Some(text) = a.label().or_else(|| a.value()) else { continue };
        if !watched.contains(&text.as_str()) {
            continue;
        }
        let r = node.rect();
        assert!(
            r.min.x >= left - 2.0 && r.max.x <= right + 2.0,
            "{text:?} at x {}..{} leaves the tools panel at x {left}..{right}",
            r.min.x,
            r.max.x
        );
        seen.push(text);
    }
    for want in watched {
        assert!(seen.iter().any(|s| s == want), "{want:?} is not on screen; saw {seen:?}");
    }
    for node in h.root().children_recursive() {
        let a = node.accesskit_node();
        if !matches!(a.role(), Role::Label | Role::Button) {
            continue;
        }
        let r = node.rect();
        assert!(
            !(r.min.x < left - 2.0 && r.max.x > left + 2.0),
            "{:?} at x {}..{} reaches across the tools panel's left edge at x {left}",
            a.label().or_else(|| a.value()),
            r.min.x,
            r.max.x
        );
    }
    assert!(!is_disabled(&h, "Open in editor"), "a picked file can be opened");

    // A click on another row selects the file Run executes.
    button(&h, "trend/momentum.rs").click();
    settle(&mut h);
    assert_eq!(h.state().native_file, "trend/momentum.rs");
    assert_matches!(
        h.state().current_spec(),
        vike_studio_core::StrategySpec::File { ref file, .. } if file == "trend/momentum.rs"
    );
}

/// **Save to server fits the Rust and Rhai panes** (the owner's ruling of 2026-11-13): the button,
/// its file field and a long answer line stay inside the tools panel, the button is armed over a
/// named `.rs` file, and disabled with its reason on a strategy-file pick.
#[test]
fn the_save_to_server_row_fits_the_strategy_pane() {
    use vike_ui_theme::value::studio;

    let (dir, store) = empty_store();
    let mut st = state(&store, &dir, RightTab::Strategy, ChatApiKeys::default());
    st.strategy_source = vike_studio::StrategySource::Rust;
    st.rust_file =
        "grid_dca/a_rather_long_strategy_file_name_that_must_truncate_in_the_pane.rs".into();
    st.save_file_last = Some(Ok(format!(
        "saved {} · sha256 3f9a1c0e · build failed: error[E0425]: cannot find value `rungz`",
        st.rust_file
    )));
    let mut h = harness_over(st);
    settle(&mut h);

    let save = icons::SAVE.accessible_label("Save to server");
    let right = 1440.0 - studio::RAIL_WIDTH;
    let left = right - studio::TOOLS_PANEL_W;
    let (mut button_seen, mut answer_seen) = (false, false);
    for node in h.root().children_recursive() {
        let a = node.accesskit_node();
        let Some(text) = a.label().or_else(|| a.value()) else { continue };
        if text != save && !text.starts_with("saved grid_dca/") {
            continue;
        }
        let r = node.rect();
        assert!(
            r.min.x >= left - 2.0 && r.max.x <= right + 2.0,
            "{text:?} at x {}..{} leaves the tools panel at x {left}..{right}",
            r.min.x,
            r.max.x
        );
        button_seen |= text == save;
        answer_seen |= text.starts_with("saved grid_dca/");
    }
    assert!(button_seen && answer_seen, "the Save button and the answer line are both on screen");
    assert!(!is_disabled(&h, &save), "a named .rs file can be saved");

    h.state_mut().strategy_source = vike_studio::StrategySource::Rhai;
    h.state_mut().rhai_file = "trend/sma_cross.rhai".to_string();
    settle(&mut h);
    assert!(!is_disabled(&h, &save), "...and a named .rhai file from the Rhai pane");
    h.state_mut().rhai_file = "trend/sma_cross.rs".to_string();
    settle(&mut h);
    assert!(is_disabled(&h, &save), "a Rhai buffer is not saved as a .rs file");
}

/// Every watched text of the form, and the pane's own title, lies inside the tools panel of a window
/// `window_w` points wide: the rail hugs its right edge and the tools panel sits left of it.
fn assert_the_import_form_fits(
    h: &egui_kittest::Harness<'static, vike_studio::StudioState>,
    window_w: f32,
) {
    use vike_ui_theme::value::studio;

    let right = window_w - studio::RAIL_WIDTH;
    let left = right - studio::TOOLS_PANEL_W;
    // The preview's first column header: its table scrolls sideways, so its START is in the pane.
    let watched =
        ["Stored data", "Import CSV…", "File", "Thousands", "Zone", "Preview", "time (UTC)"];
    let mut seen = Vec::new();
    for node in h.root().children_recursive() {
        let a = node.accesskit_node();
        let Some(text) = a.label().or_else(|| a.value()) else { continue };
        if !watched.contains(&text.as_str()) {
            continue;
        }
        let r = node.rect();
        assert!(
            r.min.x >= left - 2.0 && r.max.x <= right + 2.0,
            "{text:?} at x {}..{} leaves the tools panel at x {left}..{right}",
            r.min.x,
            r.max.x
        );
        seen.push(text);
    }
    for want in watched {
        assert!(seen.iter().any(|s| s == want), "{want:?} is not on screen; saw {seen:?}");
    }
    // ...and nothing of the panel to its left reaches into it. A form laid out wider than the pane
    // let the central panel take more width and paint over the tools panel's left edge ("tored
    // data"), while every widget of the form still sat where it was laid out: the straddling text is
    // the central panel's.
    for node in h.root().children_recursive() {
        let a = node.accesskit_node();
        if !matches!(a.role(), Role::Label | Role::Button) {
            continue;
        }
        let r = node.rect();
        assert!(
            !(r.min.x < left - 2.0 && r.max.x > left + 2.0),
            "{:?} at x {}..{} reaches across the tools panel's left edge at x {left}",
            a.label().or_else(|| a.value()),
            r.min.x,
            r.max.x
        );
    }
}
