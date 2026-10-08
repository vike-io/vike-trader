//! The tool-tab routing, run-gate, chat-key, results-strip and toolbar-chip scenarios, with the
//! roster tables they assert through. "The module doc" below is the root's, not this file's.

use std::time::Duration;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use vike_studio::{ChatApiKeys, ResultsTab, RightTab};
use vike_ui_theme::icons;

use crate::common::{empty_store, qa_name, seeded_store, spawn_compute_server, state};
use crate::support::{
    all_text, button, buttons, harness_over, has_button, is_disabled, label_values, nodes,
    run_button, settle, shell,
};

// ============================ the roster tables ============================
// The store fixtures and the posed-state constructor ([`common::empty_store`]/
// [`common::seeded_store`]/[`common::qa_name`]/[`common::state`]) MOVED to
// `crates/vike-studio/tests/common/mod.rs` when `crates/vike-studio/tests/tessellation_goldens.rs`
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
        RightTab::Sweep => "Sweep & Validate",
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
/// `perf_cells`'s row label, `validation_rows`'s PSR label — so it is present even for a run that
/// never traded. The two `None`s are `equity_tab` and `distribution_tab`, both `egui_plot`
/// canvases with nothing to read; see the module doc for exactly how much those two poses still
/// assert and why that is stated rather than papered over.
fn results_body_marker(t: ResultsTab) -> Option<&'static str> {
    match t {
        ResultsTab::Equity => None,
        ResultsTab::Trades => Some("entry"),
        ResultsTab::Performance => Some("Profit factor"),
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
    assert!(is_disabled(&h, &run_button("Run Sweep")), "no slice, no Sweep");
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
        is_disabled(&h, &run_button("Run Sweep")),
        "a slice alone is not enough: start_sweep returns early on an empty grid, and a styled \
         primary button that silently no-ops reads as broken"
    );

    h.state_mut().grid = vec![("fast".to_string(), "3, 5".to_string())];
    settle(&mut h);
    assert!(!is_disabled(&h, &run_button("Run Sweep")), "a parsable grid must arm the sweep");
}

/// **The provider-key gate.** With no key the Send button is really disabled in the tree an
/// assistive client reads, and the pane says why; with a key it becomes pressable.
///
/// Both halves hold the slice and the input constant, so the only thing that moves is the key —
/// which is what makes this a test of `can_send`'s `has_key()` conjunct rather than of the other
/// three.
///
/// ⚠ It never clicks Send, in the voice of
/// `crates/vike-connections/tests/panel/a11y_form.rs`'s never-click-Save warning:
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
    st.backend = vike_studio::Backend::Remote { addr: spawn_compute_server(store.clone()) };
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
    assert!(matches!(st.last, Some(Ok(_))), "the run must produce a result for the pane to render");

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
