//! The Studio SHELL, actually rendered: `crates/vike-studio/src/studio/shell.rs`'s `ui` driven
//! headlessly through `egui_kittest`, once per tool tab and once per results tab, with every
//! emitted frame also handed to the shared geometry invariant
//! (`crates/vike-ui-theme/src/frame_sanity.rs`'s `assert_frame_sane`).
//!
//! # The claim this file retires
//!
//! `crates/vike-studio/tests/saved_pane_a11y.rs` opened by saying the shell was "genuinely
//! expensive to drive headlessly — `StudioState::ui` wants a `DataFusionHist`, a worker thread and
//! a run backend", and that is why the crate's only two render tests each drove ONE pane. One
//! third of that was true when it was written and is not any more; the other two thirds were never
//! true at all.
//!
//! - The store is a TRAIT HANDLE since split-plane B12 (`crates/vike-studio-core/src/run.rs`'s
//!   `StoreHandle`), so `crates/vike-studio/src/studio.rs`'s `new_with_qa` asks only for that
//!   handle, a `PathBuf`, a `Default`-able key struct and two QA flags — an empty temp-directory
//!   store and three literals satisfy the lot.
//! - The four worker receivers (`run_rx`/`sweep_rx`/`wf_rx`/`chat_rx`) are `Option<Receiver<..>>`
//!   and are `None` while nothing is running, so RENDERING never wanted a thread. `poll()` folds
//!   nothing and returns.
//! - A "run backend" is a `Backend` enum value read ONLY when a Run is DISPATCHED. It has TWO
//!   variants (`Remote` and `Named`), and BOTH dial the compute daemon: the in-process `Local`
//!   arm is deleted, so a dispatched run leaves this process entirely. Rendering is unaffected
//!   either way, which is this bullet's point - the shell draws without a backend answering.
//!
//! The decisive datum is that the crate had already refuted itself:
//! `crates/vike-studio/examples/studio_shot.rs` has been driving the real `ui()` through
//! `egui::Context::run_ui` since it landed. What was missing was not a mechanism, it was a test.
//!
//! # What this catches that the crate's pure tests cannot
//!
//! Every `#[test]` under `crates/vike-studio/src/` is over a helper — `next_tab`, `from_qa_str`,
//! `perf_rows`, `compile_status`, `params_from_rows` — or over `StudioState`'s state machine
//! driven by direct method calls. None of them renders a frame, and therefore none of them knows:
//!
//! 1. **WHICH pane a tool tab actually draws.** `RightTab::ALL`/`label`/`icon`/`next_tab`/
//!    `from_qa_str` are all unit-tested and not one of them can see the `match self.right_tab` in
//!    `ui()` that routes seven tabs to seven renderers. Swapping two arms there keeps every pure test
//!    green and shows the operator the wrong tool. The mutual-exclusion form below (each pane's
//!    title present for its own pose and absent from the other five, across all six poses) cannot
//!    be satisfied by an implementation that always renders the same pane, or all of them.
//! 2. **Whether a gate that must not act on nothing is really disarmed.** `▶ Run`, `▶ Run Sweep`,
//!    `Walk-Forward`, `▶ Run backtest`, `▶ Run study` and `Send` are each one `add_enabled` bool. Inverting one
//!    changes no pure test and hands a click to `start_run`/`start_sweep`/`start_chat_send`, whose
//!    own early returns then make a styled primary button silently do nothing — the exact defect
//!    `sweep_pane_ui`'s own comment calls "reads as broken", and an assistive client offered a
//!    dead end.
//! 3. **That a refused catalog scan is DISCLOSED by the central panel** rather than wearing the
//!    empty-store costume. `crates/vike-studio/src/studio/center.rs`'s `empty_state` asks
//!    `SlicePicker::error` BEFORE `available().is_empty()` and its own doc calls itself "the second
//!    surface that rendered that lie". `crates/vike-studio/tests/picker_disclosure.rs` gates the
//!    FIRST surface (the picker's toolbar) and cannot see this one: it renders `SlicePicker::ui`,
//!    not the shell. Swapping those two arms sends an operator whose datahub went away to a
//!    backfill they do not need, and leaves that whole file green. The placeholder has FOUR arms,
//!    not two, so the pair is pinned twice: the refusal against the empty store, and the
//!    depth-only store against the empty store — a swap of the MIDDLE two is a distinct edit that
//!    the first pair alone cannot see, and it tells somebody who recorded `kind=depth` for that
//!    instrument to go and re-fetch history they already hold.
//! 4. **That the results strip is WIRED to the results body.** `results_ui` holds a `(tab, name)`
//!    table and a `match tab` under it; nothing pins that the two agree. Clicking each caption and
//!    asserting the BODY moved is a different claim from reading `StudioState::tab` back.
//! 5. **That the shell emits paintable geometry.** Every frame goes through [`settle`], so a `NaN`
//!    or runaway coordinate anywhere in the shell reddens whichever scenario provoked it — the
//!    free upgrade `crates/vike-chart/tests/common/mod.rs`'s `run_full` gives that crate.
//!
//! # Decisions taken here, and why (each was measured, not assumed)
//!
//! ⚠ **`new_with_qa`, never `new` — and it is a SAFETY choice, not a stylistic one.** `ui()` ends
//! with `maybe_persist_workspace`, which writes `studio_workspace.json` to
//! `crates/vike-studio/src/workspace.rs`'s `workspace_write_path` — `$VIKE_STATE_ROOT`, else
//! `<project>/settings/state` found by walking UP from the working directory. Under
//! `cargo test -p vike-studio` that is the CHECKOUT's own settings root, i.e. the developer's real
//! Studio workspace, never the test's temp directory. Any post-construction pose counts as drift
//! and the first frame would persist it. `new_with_qa(.., Some(name), false)` sets
//! `qa_workspace_readonly`, which returns from `maybe_persist_workspace` before the comparison —
//! so it is both the honest entry point (it is what `vike-desktop`'s `main.rs` calls; `new` is a
//! two-line wrapper over it) and the only safe one. That is why
//! [`qa_name_names_the_tab_it_claims_to`] is a safety test: an unrecognised name is silently
//! IGNORED by that constructor, and the session then falls back to workspace-restoring,
//! non-read-only construction.
//!
//! ⚠ **...and the READ half of the same hazard is defused, not eliminated.** `new_with_qa` also
//! calls `load_workspace(workspace_read_path(state_dir))`, which prefers that same real project
//! file, so on a box that has ever opened the Studio these frames would start from somebody's
//! collapsed editor, their template index and their last script — and the suite would assert
//! different things in CI (fresh checkout, no file) than on a dev box. [`common::state`] resets
//! every restorable field after construction; `poll()` at the top of `ui()` then recompiles
//! and clears the compile status, and `saved_source` is re-baselined so no "unsaved" chip
//! appears. Nothing is written; only this process's copy moves. **Maintenance obligation: a new
//! field on `StudioWorkspace` must join [`common::state`]'s reset, or this file — and the goldens
//! twin that shares the fixture — becomes machine-dependent again.**
//!
//! ⚠ **The harness binds the app's type, and this paragraph used to argue the opposite.** It said
//! no Studio frame could reach a named `FontFamily`, because the only producers sat in crates the
//! Studio did not render — true until the icon registry (design system spec §9): every icon the
//! Studio draws is a `vike_ui_theme::icons` constant in the icon family, which only the bundled
//! set binds, and epaint PANICS on an unbound `FontFamily::Name`. So [`harness_over`] opens with
//! `vike_ui_theme::harness::type_ready`, which binds on the frame kittest runs at build time and
//! draws nothing then; every scenario is settled before it asserts, so the frame it reads is a
//! bound one, laid out with the app's faces and sizes.
//!
//! ⚠ **[`settle`] deliberately does NOT clear `textures_delta` first**, which reverses the order
//! `assert_frame_sane`'s own doc demands of a raw `run_ui` harness. It is safe here for a specific
//! reason rather than by luck: `egui_kittest`'s `LazyRenderer` takes the frame's deltas BEFORE the
//! harness stores the output, so `Harness::output()` is already an empty-delta `FullOutput` and no
//! unapplied delta can double-panic during the assertion's unwind.
//!
//! ⚠ **Two results poses are weaker than the other three, and are labelled so rather than dressed
//! up.** `equity_tab` and `distribution_tab` are `egui_plot` canvases that emit no distinctive
//! text, so [`results_body_marker`] returns `None` for them and those two poses assert only that
//! the strip moved, that `StudioState::tab` moved, that the three text-bearing bodies are GONE,
//! and that the frame the plot emitted is paintable. That last one is not filler: a plot over a
//! degenerate range is exactly where a runaway coordinate comes from.
//!
//! ⚠ **[`each_tool_tab_renders_its_own_pane_and_only_its_own`] sends no pointer event at all, and
//! that is load-bearing.** The rail's hover tooltip is `RightTab::label()`, and THREE of its seven
//! values — `"Strategy"`, `"Indicators"` and `"Research"` — are also pane titles, so a rendered
//! tooltip would put a `Role::Label` carrying exactly one of those strings into the tree and break
//! the mutual-exclusion assertion from the outside, on three of the seven poses rather than one.
//! With no pointer there is no hover and no tooltip. The two tests that DO click
//! ([`the_results_pane_renders_every_tab_of_a_real_backtest`] and
//! [`the_toolbar_strategy_chip_opens_the_strategy_pane`]) click controls whose tooltip text, if it
//! ever rendered, equals none of the strings they assert on.
//!
//! # Kill proof
//!
//! **Shipped, and self-proving.** Three properties here cannot be passed by a constant answer, so
//! they keep working without anybody remembering a plant: the seven-pose mutual exclusion (49
//! `(pose, title)` pairs where presence must equal identity — an implementation that always
//! renders one pane fails six of seven, one that renders all seven fails all seven); the paired
//! controls (empty/seeded, keyless/keyed, refusing/empty, depth-only/empty — each pair asserts the
//! SAME predicate in both directions, so a gate stuck either way fails one half, and the last two
//! share one control precisely so the two disclosure arms are pinned against the same baseline);
//! and the structural floor (exactly
//! one Refresh button and exactly one rail button per `RightTab::ALL` entry, checked BEFORE
//! any other assertion, so nothing can pass vacuously over a frame that stopped rendering — the
//! failure mode `crates/vike-chart/tests/frame_sanity_gate.rs` warns about). Four exhaustive
//! matches ([`common::qa_name`], [`pane_title`], [`results_tab_name`], [`results_body_marker`])
//! make an EIGHTH `RightTab` or a sixth `ResultsTab` a COMPILE error in this binary. (It said
//! "a seventh" until Research became the seventh, which is the mechanism working.)
//!
//! **⚠ TRACED BY HAND 2026-08-22 — the lane run is still owed.** This repo's standard is that a
//! gate not shown to fail is decoration, and no cargo runs on the branch that discharged this
//! debt — so each plant below was discharged the next-honest way available: its edit was made
//! against the real `crates/vike-studio/src/studio.rs` (and siblings) and walked through the
//! named test assertion by assertion, in execution order, recording WHICH assertion fires FIRST
//! and with what message. That is a claim about the code as READ, not as executed, and the table
//! says so: a lane run of a plant replaces its `traced` row one-for-one with a `measured` one
//! (the way `crates/vike-chart/tests/frame_sanity_gate.rs` records its 32-of-312), and the
//! recipes below stay intact so that run needs no re-derivation. The trace found no dead plant —
//! all eight fire — but three of the original recipe EXPECTATIONS were over-claims and two
//! plants have collateral redness the first draft missed; both kinds are corrected in place.
//!
//! | plant | reddening test(s) | first failing assertion (traced) | status |
//! |---|---|---|---|
//! | P1 | [`each_tool_tab_renders_its_own_pane_and_only_its_own`]; collateral: [`the_chat_pane_offers_no_send_without_a_provider_key`] | pose Saved (5th of six — Sweep/Strategy/Data/Indicators pass, Data is routed above the match), `other == Saved`: `with Saved strategies open, "Saved strategies" must be on screen` (the chat pane rendered instead). The chat test never reaches an assertion of its own: [`button`]'s exactly-once floor panics with `expected exactly one "Send" button, found 0` — its Chat pose renders the Saved pane, which has no Send. | traced 2026-08-22 |
//! | P2 | the routing test alone — nothing else counts rail buttons | pose Sweep's rail floor at `rail == Chat`: `the Sweep & Validate pose must draw exactly one rail button for AI Copilot` — expected 1, found 0 | traced 2026-08-22 |
//! | P3 | [`a_refused_series_scan_is_disclosed_by_the_shell_not_rendered_as_an_empty_store`]; collateral: [`a_depth_only_store_is_disclosed_by_the_shell_not_rendered_as_an_empty_store`] (the hoisted arm shadows the depth arm too); all five `picker_disclosure.rs` tests GREEN as claimed — they harness `SlicePicker::ui` and never render `empty_state` | its FIRST assertion: `the central panel must say where to look` — under the hoist the refused pose renders the empty-store copy, and `SERIES_SCAN_ADVICE`/[`REASON`] survive only in the toolbar chip's HOVER text and the CLOSED combo popup, neither of which is in an un-hovered frame's tree | traced 2026-08-22 |
//! | P3b | [`a_depth_only_store_is_disclosed_by_the_shell_not_rendered_as_an_empty_store`]; P3's own test stays GREEN (the error arm is still first), which is what shows the two pins are distinct edits | its FIRST assertion: `a recorded-but-unreplayable instrument must be named by the central panel, not silently counted as nothing` — the second (the empty-store copy's absence) is equally violated and unreached | traced 2026-08-22 |
//! | P4 | [`an_empty_store_arms_no_run_affordance_and_a_seeded_one_arms_them`], one half per direction; [`the_results_pane_renders_every_tab_of_a_real_backtest`] stays green (it dispatches through `start_run`, whose own guards are not the forced sites) | forced TRUE (four sites: `ui`'s toolbar `can_run`, `empty_state`'s `can_run`, `sweep_pane_ui`'s `can` and `grid_ok`): the empty half's `no slice, no Run`. Forced FALSE: the seeded half's `a selected slice must arm Run`. | traced 2026-08-22 |
//! | P5 | [`the_chat_pane_offers_no_send_without_a_provider_key`], keyless half | `no provider key, no Send` — the deleted conjunct was the only false one (the seeded store auto-selects row 0, the input is posed non-empty, nothing is running), so Send arms; the `No provider key found` copy still renders, keyed on `available_providers()`, untouched | traced 2026-08-22 |
//! | P6 | [`the_results_pane_renders_every_tab_of_a_real_backtest`] | pose Performance's marker leg: `with Performance open, "Profit factor" must be rendered` — AFTER that pose's role asserts passed for Equity/Trades/Performance, which is exactly the strip/body independence the recipe wanted proven | traced 2026-08-22 |
//! | P7 | the seven harness-building tests; [`qa_name_names_the_tab_it_claims_to`] builds no harness and stays GREEN | each dies in [`settle`]'s `assert_frame_sane`: `frame geometry is not paintable (…)`, the bad-coordinate list naming the Circle's NaN centre. Lane caveat: if `egui_kittest` ever validates shapes before [`settle`] runs, the message moves but the redness does not — record which. | traced 2026-08-22 |
//!
//! The plants, verbatim — run them in a verification lane and flip the rows above to `measured`:
//!
//! - **P1 routing** — swap the `RightTab::Saved` and `RightTab::Chat` arms of `ui()`'s
//!   `match self.right_tab`. Expect [`each_tool_tab_renders_its_own_pane_and_only_its_own`] to
//!   panic at the FIRST swapped pose — Saved; the Chat pose is equally violated and unreached,
//!   because one `#[test]` iterates the six poses — and every pure test in the crate to stay
//!   green. The one collateral is [`the_chat_pane_offers_no_send_without_a_provider_key`], whose
//!   Chat pose now renders the Saved pane and loses its Send button to [`button`]'s
//!   exactly-once floor.
//! - **P2 rail completeness** — narrow the rail loop to `RightTab::ALL.iter().take(5)`. Expect the
//!   same test to fail on the missing icon button, and nothing else to move.
//! - **P3 disclosure order** (the sharpest) — in `empty_state`, move the `available().is_empty()`
//!   arm ABOVE the `picker.error()` arm. Expect
//!   [`a_refused_series_scan_is_disclosed_by_the_shell_not_rendered_as_an_empty_store`] to fail AND
//!   all five tests in `crates/vike-studio/tests/picker_disclosure.rs` to stay GREEN — that second
//!   half is the evidence that the central panel is a genuinely uncovered second surface.
//!   ⚠ Traced collateral the first draft of this recipe missed: hoisted to the top of the chain,
//!   the bare arm also shadows the `depth_only` arm, so
//!   [`a_depth_only_store_is_disclosed_by_the_shell_not_rendered_as_an_empty_store`] reddens TOO —
//!   extra coverage of the same defect family, not a miss.
//! - **P3b the same order, one arm along** — in `empty_state`, move the bare
//!   `available().is_empty()` arm ABOVE the `depth_only()` one. Expect
//!   [`a_depth_only_store_is_disclosed_by_the_shell_not_rendered_as_an_empty_store`] to fail at
//!   the FIRST of its two disclosure assertions (the second — the empty-store copy's absence — is
//!   equally violated and unreached) and P3's own test to stay GREEN, which is what shows the two
//!   are pinning different edits rather than the same one twice.
//! - **P4 the run gates, both directions** — force `can_run` (and `grid_ok`) to `true`, then to
//!   `false`. Expect [`an_empty_store_arms_no_run_affordance_and_a_seeded_one_arms_them`] to fail
//!   on the empty half and the seeded half respectively.
//! - **P5 the key gate** — delete `&& self.chat.has_key()` from `chat_pane_ui`'s `can_send`.
//!   Expect [`the_chat_pane_offers_no_send_without_a_provider_key`]'s keyless half to fail. Honest
//!   scope: `start_chat_send` re-checks `has_key`, so this produces a DEAD button, not an
//!   unauthenticated request.
//! - **P6 body routing** — route `ResultsTab::Performance` to `distribution_tab`. Expect
//!   [`the_results_pane_renders_every_tab_of_a_real_backtest`] to fail on the body marker while
//!   the strip's `toggled` assertion still passes, proving the strip and the body are asserted
//!   independently and the test is not merely reading its own click back.
//! - **P7 geometry reach** — paint a `NaN`-centred circle at the top of `StudioState::ui`. Expect
//!   every harness-building test in this file — seven of the eight;
//!   [`qa_name_names_the_tab_it_claims_to`] renders nothing and stays green — to fail with
//!   "frame geometry is not paintable", which is what proves [`settle`] reaches the invariant on
//!   a real shell frame.
//!
//! # Declared gap
//!
//! Nothing here asserts that these frames wrote no `studio_workspace.json`. `workspace_write_path`
//! and `WORKSPACE_FILE` are not re-exported from this crate, so the path is unreachable from an
//! integration test — and probing the developer's real settings root is precisely what must not
//! happen. `qa_workspace_readonly` is therefore RELIED ON here rather than proven.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    REASON, RefusingCatalogStore, empty_store, qa_name, seeded_store, spawn_compute_server, state,
};
use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_data::{DataFusionHist, HistStore};
use vike_model::{BookLevel, BookUpdate, BookUpdateKind};
use vike_studio::{
    ChatApiKeys, DEPTH_NOT_REPLAYABLE, NO_HOST, PLUGIN_EDITOR_CHIP, ResultsTab, RightTab,
    SERIES_SCAN_ADVICE, STUDY_METRICS_NOTE, STUDY_RESULT_TITLE, STUDY_RUN_KIND, StoreHandle,
    StrategySource, StudioState, StudyHost,
};
use vike_ui_theme::icons;

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

/// The results strip in display order, as `crates/vike-studio/src/results.rs`'s `results_ui`
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

// ============================ harness plumbing ============================

/// Wrap a posed state in a harness over the REAL `StudioState::ui`.
///
/// 1440×960 because the shell is four panels wide (a 40px rail, a 340px tools panel, a 460px
/// editor) and a cramped harness clips controls out of the tree, turning a real assertion into a
/// "not found" panic. `max_steps` is raised from `egui_kittest`'s default of 4 because
/// `Harness::run` PANICS past it: the raise costs nothing when the UI settles (the loop breaks the
/// moment a frame requests no immediate repaint) and removes a flake vector. If the shell ever
/// repaints forever the failure is loud and self-diagnosing — the error names the repaint causes.
fn harness_over(st: StudioState) -> Harness<'static, StudioState> {
    Harness::builder().with_size(egui::vec2(1440.0, 960.0)).with_max_steps(64).build_ui_state(
        |ui, st: &mut StudioState| {
            // The shell draws icons; their family is bound only by the app's type (module doc).
            if vike_ui_theme::harness::type_ready(ui.ctx()) {
                st.ui(ui);
            }
        },
        st,
    )
}

/// The common case: a keyless shell over `store`, posed on `tab`.
fn shell(
    store: &StoreHandle,
    dir: &tempfile::TempDir,
    tab: RightTab,
) -> Harness<'static, StudioState> {
    harness_over(state(store, dir, tab, ChatApiKeys::default()))
}

/// Run until repaints settle, then assert the emitted frame is paintable.
///
/// EVERY interaction in this file goes through here, which is what makes each of these scenarios a
/// geometry test as well as an accessibility one — the free upgrade
/// `crates/vike-chart/tests/common/mod.rs`'s `run_full` gives that crate's scenarios. See the
/// module doc for why no `textures_delta` clear precedes the assertion here even though
/// `assert_frame_sane`'s own doc demands one of a raw `run_ui` harness.
fn settle(h: &mut Harness<'static, StudioState>) {
    h.run();
    vike_ui_theme::frame_sanity::assert_frame_sane(h.output());
}

// ============================ accessibility-tree queries ============================

/// Every accessibility node matching `pred`, in tree order.
fn nodes<'t>(
    h: &'t Harness<'static, StudioState>,
    pred: impl Fn(&Node<'t>) -> bool,
) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

/// The accessible name of a run button: `icons::RUN`, then its words.
fn run_button(words: &str) -> String {
    icons::RUN.accessible_label(words)
}

/// Every BUTTON carrying exactly `label`. Equality, never containment: `Run` must not match
/// `Run Sweep` or `Run backtest`, and a rail icon must not match the toolbar's strategy chip,
/// whose label is that same icon followed by the strategy name.
fn buttons<'t>(h: &'t Harness<'static, StudioState>, label: &str) -> Vec<Node<'t>> {
    nodes(h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(label)
    })
}

/// The one button labelled `label`. Panics if it is not on screen exactly once — "the control
/// vanished" must fail loudly, not read as "disabled".
fn button<'t>(h: &'t Harness<'static, StudioState>, label: &str) -> Node<'t> {
    let found = buttons(h, label);
    assert_eq!(found.len(), 1, "expected exactly one {label:?} button, found {}", found.len());
    found[0]
}

fn is_disabled(h: &Harness<'static, StudioState>, label: &str) -> bool {
    button(h, label).accesskit_node().is_disabled()
}

fn has_button(h: &Harness<'static, StudioState>, label: &str) -> bool {
    !buttons(h, label).is_empty()
}

/// The text of every `Role::Label` node, read off its accesskit VALUE.
///
/// ⚠ The `value()` half is load-bearing and is the trap
/// `crates/vike-studio/tests/picker_disclosure.rs` documents from having fallen into it: egui files
/// a `Label`'s (and a `ComboBox`'s) text under the node's VALUE, not its `label`, so a helper that
/// read `label()` here would return an empty list for every store and turn every negative
/// assertion green.
fn label_values(h: &Harness<'static, StudioState>) -> Vec<String> {
    nodes(h, |n| n.accesskit_node().role() == Role::Label)
        .into_iter()
        .filter_map(|n| n.accesskit_node().value())
        .collect()
}

/// Every piece of text in the tree, label and value alike — what a human (or a screen reader) has
/// in front of them. The `contains` assertions below run over this, because several of the strings
/// they look for are embedded in a longer sentence the shell composed.
fn all_text(h: &Harness<'static, StudioState>) -> Vec<String> {
    h.root()
        .children_recursive()
        .flat_map(|n| {
            let a = n.accesskit_node();
            [a.label(), a.value()].into_iter().flatten().map(|t| t.to_string()).collect::<Vec<_>>()
        })
        .collect()
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
/// seven poses, one that renders them all fails all seven. Reddens on swapping two arms of `ui()`'s
/// `match self.right_tab` — an edit every pure test in the crate is blind to.
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
/// `crates/vike-connections/tests/connections_a11y.rs`'s never-click-Save warning:
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

/// **The second surface of the disclosure defect.** A store that REFUSED to enumerate must not
/// render as an empty one in the CENTRAL PANEL either.
///
/// `crates/vike-studio/tests/picker_disclosure.rs` covers `SlicePicker::ui` — the toolbar — and
/// cannot reach `crates/vike-studio/src/studio/center.rs`'s `empty_state`, whose own doc calls itself
/// "the second surface that rendered that lie". Swapping its two arms sends an operator whose
/// datahub went away to a backfill they do not need while leaving that entire file green.
///
/// The store's OWN reason is asserted, not just the advice: the advice is a constant this file
/// could match against a hard-coded string, the reason can only have come through the error.
#[test]
fn a_refused_series_scan_is_disclosed_by_the_shell_not_rendered_as_an_empty_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store: StoreHandle = Arc::new(RefusingCatalogStore);
    let mut h = shell(&store, &dir, RightTab::Sweep);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains(SERIES_SCAN_ADVICE)),
        "the central panel must say where to look; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains(REASON)),
        "...and carry the STORE's own words, not a message the shell invented; rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains("The data store is empty")),
        "a refused scan must not wear the empty-store costume; rendered: {text:?}"
    );

    let (empty_dir, empty) = empty_store();
    let mut h = shell(&empty, &empty_dir, RightTab::Sweep);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains("The data store is empty")),
        "the control: a store that ANSWERED and holds nothing keeps its own copy; \
         rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains(SERIES_SCAN_ADVICE)),
        "nothing failed here — the disclosure must not fire; rendered: {text:?}"
    );
    assert!(!text.iter().any(|t| t.contains(REASON)), "no reason exists; rendered: {text:?}");
}

/// **The MIDDLE arm of the same placeholder — the one the pair above cannot reach.**
///
/// `empty_state` picks between FOUR copies in a fixed order (refused scan, depth-only store, empty
/// store, pickable slice) and the test above pins only the FIRST against the THIRD, so a swap of
/// the middle two leaves it green. That swap is the same lie in the same panel wearing the other
/// costume: an operator who DID record this instrument — as the conflating `kind=depth` lane, which
/// no slice can replay — is told their store is empty and sent to re-fetch history they already
/// hold. `crates/vike-studio/tests/picker_disclosure.rs`'s
/// `a_depth_only_instrument_is_named_on_screen_instead_of_vanishing` is the TOOLBAR half of exactly
/// this claim and, being a `SlicePicker::ui` harness, can no more see the central panel here than
/// it could see the refusal.
///
/// The isolation is the refusal's, for the same reason: the toolbar's own `⚠ depth-only` chip
/// carries `DEPTH_NOT_REPLAYABLE` in HOVER text and its dropdown rows live in a CLOSED popup, so
/// neither is in an un-hovered frame's tree — the string asserted here can only have come from
/// `empty_state`. The two negative legs are what stop it passing for the wrong reason: the
/// empty-store copy must be absent (that is the swapped arm), and no dispatch may be offered over
/// a store holding nothing runnable.
#[test]
fn a_depth_only_store_is_disclosed_by_the_shell_not_rendered_as_an_empty_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = DataFusionHist::open(dir.path()).expect("open hist store");
    store
        .append_depth(
            "binance",
            "BTCUSDT",
            &[BookUpdate {
                ts: 1,
                local_ts: 1,
                seq: 0,
                kind: BookUpdateKind::Snapshot,
                tick_size: 0.01,
                bids: vec![BookLevel::new(100.0, 1.0)],
                asks: vec![BookLevel::new(100.5, 1.0)],
                symbol: "BTCUSDT".to_string(),
            }],
            None,
        )
        .expect("append depth");
    let store: StoreHandle = Arc::new(store);

    let mut h = shell(&store, &dir, RightTab::Sweep);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains(DEPTH_NOT_REPLAYABLE)),
        "a recorded-but-unreplayable instrument must be named by the central panel, not silently \
         counted as nothing; rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains("The data store is empty")),
        "this store is NOT empty — that copy sends the operator to a backfill they already ran; \
         rendered: {text:?}"
    );
    assert!(
        !has_button(&h, &run_button("Run backtest")),
        "nothing here is replayable, so the getting-started dispatch must not be offered"
    );
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

// ======================= the editor header's verdict, per strategy source =======================

/// A REAL plugin author's source: the Rust strategy both live smokes build — the builder's own and
/// `crates/vike-studio/tests/studio_live_path_smoke.rs`, which includes these same bytes. A strategy
/// the builder service actually compiles, not a fragment invented to fail a Rhai parse, because
/// the defect was a VALID plugin wearing a red header chip.
const PLUGIN_SOURCE: &str =
    include_str!("../../vike-strategy-builder/tests/fixtures/live_smoke_strategy.rs.in");

/// The editor header's Rhai error chip (`● error · line N`, or a bare `● error` when rhai's
/// message names no line), found by PREFIX so the assertion does not depend on which line of
/// [`PLUGIN_SOURCE`] rhai trips on first.
fn rhai_error_chip(text: &[String]) -> bool {
    text.iter().any(|t| t.starts_with("● error"))
}

/// The inline banner under the header: `icons::FAILED`, a space, then
/// `crates/vike-studio/src/editor.rs`'s `format_compile_error`. Found by that PREFIX — the icon
/// followed by words — which no other label carries (the central panel's failure mark is the icon
/// alone). The header chip's twin: both said the same wrong thing about a plugin, so both are
/// asserted.
fn rhai_error_banner(text: &[String]) -> bool {
    let banner = icons::FAILED.accessible_label("");
    text.iter().any(|t| t.starts_with(&format!("{banner} ")))
}

/// **The measured defect: a Plugin's Rust buffer was judged as RHAI.** On the real v0.1.34 GUI a
/// valid plugin read `● error · line 4` (red) in the editor header beside the `● built <sha>` of
/// its own successful Build — `crates/vike-studio/src/studio/workers.rs`'s `poll` ran the Rhai
/// compile check on the buffer whatever `strategy_source` said.
///
/// ONE harness, three poses, and every switch is a CLICK on the Strategy pane's own mode buttons
/// rather than an assignment — so it also proves the verdict follows the button the operator
/// actually presses:
///
/// 1. **Rhai mode (the session default) over the same Rust source shows the red chip and the
///    banner.** That is the control, and it is correct: Rhai mode would RUN this buffer as Rhai.
///    Without it the Plugin pose's "no error chip" could pass for the wrong reason — a source rhai
///    happens to accept, or a header that stopped rendering a chip at all.
/// 2. **Plugin mode drops both, shows no green verdict either, and says what is known instead**
///    ([`PLUGIN_EDITOR_CHIP`]). A bare "no red" would be satisfied by `● compiles`, which is the
///    same lie in the other colour — nothing judged this buffer.
/// 3. **Back to Rhai re-checks**: the red chip returns with the source unchanged, so no "no
///    verdict" state is carried across the switch.
///
/// The state-machine twin (what `poll` HOLDS, frame after frame) is
/// `crates/vike-studio/src/studio_tests.rs`'s
/// `plugin_mode_holds_no_rhai_verdict_and_rhai_mode_rechecks_on_return`.
#[test]
fn a_plugin_buffer_is_not_judged_as_rhai_and_the_same_buffer_in_rhai_mode_is() {
    let (dir, store) = empty_store();
    let mut st = state(&store, &dir, RightTab::Strategy, ChatApiKeys::default());
    st.editor.source = PLUGIN_SOURCE.to_string();
    st.saved_source = st.editor.source.clone();
    let mut h = harness_over(st);
    settle(&mut h);

    // 1 — the control.
    assert_eq!(h.state().strategy_source, StrategySource::Rhai, "a session starts in Rhai mode");
    let text = all_text(&h);
    assert!(
        rhai_error_chip(&text),
        "the control: Rust source in RHAI mode must wear the Rhai error chip; rendered: {text:?}"
    );
    assert!(rhai_error_banner(&text), "...and its inline banner; rendered: {text:?}");
    assert!(
        !text.iter().any(|t| t == PLUGIN_EDITOR_CHIP),
        "Rhai mode is not Plugin mode; rendered: {text:?}"
    );

    // 2 — the fix.
    button(&h, "Plugin (Rust)").click();
    settle(&mut h);
    assert_eq!(h.state().strategy_source, StrategySource::Plugin, "the click must take");
    let text = all_text(&h);
    assert!(
        !rhai_error_chip(&text),
        "a Plugin's Rust buffer must not be judged as Rhai; rendered: {text:?}"
    );
    assert!(!rhai_error_banner(&text), "...nor bannered as a Rhai error; rendered: {text:?}");
    assert!(
        !text.iter().any(|t| t == "● compiles"),
        "...nor passed as Rhai either — nothing has judged this buffer; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t == PLUGIN_EDITOR_CHIP),
        "the header must say what IS known: the language, and that Build is the verdict; \
         rendered: {text:?}"
    );

    // 3 — the way back.
    button(&h, "Rhai script").click();
    settle(&mut h);
    assert_eq!(h.state().strategy_source, StrategySource::Rhai, "the click must take");
    let text = all_text(&h);
    assert!(
        rhai_error_chip(&text),
        "back in Rhai mode the same buffer must be re-checked and fail again; rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t == PLUGIN_EDITOR_CHIP),
        "...and the Plugin chip must not outlive the switch; rendered: {text:?}"
    );
}

/// **Every control that writes RHAI into the editor is refused over a Plugin's Rust buffer, and
/// armed over a Rhai one.** There are four: the Sweep pane's template `Load`, each `Browse
/// templates` card's `Load`, the empty results panel's `Load a template`, and the AI Copilot's
/// `Apply to editor`. Before this, all four wrote Rhai straight over the plugin's Rust source while
/// `Plugin (Rust)` stayed selected — and a Plugin save keeps no copy of the source, so that was the
/// only copy. `crates/vike-studio/src/studio/strategy.rs`'s `rhai_writer_blocked_reason` argues why
/// they are refused rather than made to switch the source; its unit twin
/// `plugin_mode_refuses_every_rhai_writer_and_the_rhai_modes_do_not` pins the check INSIDE the
/// write.
///
/// Each pose flips ONLY the strategy source over one harness, so the one thing that moves the
/// controls is the mode. Rhai first, as the control — every loader armed, so the Plugin pose's
/// "disabled" cannot be a control that is always off — then Plugin, where a click on a refused
/// control must leave the buffer and the mode exactly as they were, then Rhai again, where the same
/// click writes. The gallery is OPENED (its header is clicked), so its cards are really in the tree:
/// the count assertion is what stops a gallery that rendered no cards passing "all disabled"
/// vacuously.
///
/// ⚠ The clicks are safe: a template load and an Apply are pure buffer writes — no worker, no
/// network. `Send` is never pressed.
#[test]
fn every_rhai_writer_is_refused_over_a_plugin_buffer_and_armed_over_a_rhai_one() {
    // SEEDED, so a slice is pickable and the empty results panel offers `Load a template`.
    let (dir, store) = seeded_store();
    let mut st = state(&store, &dir, RightTab::Sweep, ChatApiKeys::default());
    st.editor.source = PLUGIN_SOURCE.to_string();
    st.saved_source = st.editor.source.clone();
    let mut h = harness_over(st);
    settle(&mut h);
    button(&h, "Browse templates").click();
    settle(&mut h);
    let loaders = 1 + vike_script::TEMPLATES.len();

    // Rhai — the control.
    let loads = buttons(&h, "Load");
    assert_eq!(loads.len(), loaders, "the pane's Load plus one per gallery card must be on screen");
    assert!(loads.iter().all(|n| !n.accesskit_node().is_disabled()), "Rhai mode arms every Load");
    assert!(!is_disabled(&h, "Load a template"), "...and the results panel's loader");

    // Plugin — refused, all of them, and a click writes nothing.
    h.state_mut().strategy_source = StrategySource::Plugin;
    settle(&mut h);
    let loads = buttons(&h, "Load");
    assert_eq!(loads.len(), loaders, "Plugin mode must still SHOW the loaders, disabled");
    assert!(
        loads.iter().all(|n| n.accesskit_node().is_disabled()),
        "no template Load may write Rhai over a Plugin's Rust buffer"
    );
    assert!(is_disabled(&h, "Load a template"), "...nor the results panel's loader");
    buttons(&h, "Load")[0].click();
    settle(&mut h);
    button(&h, "Load a template").click();
    settle(&mut h);
    assert_eq!(
        h.state().editor.source,
        PLUGIN_SOURCE,
        "a refused loader must not touch the buffer"
    );
    assert_eq!(h.state().strategy_source, StrategySource::Plugin, "...nor switch the mode");

    // Rhai again — the same click now writes, so the refusal above was the mode's doing.
    h.state_mut().strategy_source = StrategySource::Rhai;
    settle(&mut h);
    buttons(&h, "Load")[0].click();
    settle(&mut h);
    assert_eq!(
        h.state().editor.source,
        vike_script::TEMPLATES[h.state().template_idx].1,
        "in Rhai mode the pane's Load writes the selected template"
    );

    // The copilot's Apply, the fourth writer, on its own pane.
    let generated = "fn on_bar() {}";
    let mut st = state(&store, &dir, RightTab::Chat, ChatApiKeys::default());
    st.editor.source = PLUGIN_SOURCE.to_string();
    st.saved_source = st.editor.source.clone();
    st.chat.last = Some(vike_ai::AgentResult {
        code: generated.to_string(),
        accepted: true,
        ..Default::default()
    });
    let mut h = harness_over(st);
    settle(&mut h);
    assert!(!is_disabled(&h, "Apply to editor"), "the control: Rhai mode arms Apply");
    h.state_mut().strategy_source = StrategySource::Plugin;
    settle(&mut h);
    assert!(is_disabled(&h, "Apply to editor"), "Apply must not write Rhai over Rust source");
    button(&h, "Apply to editor").click();
    settle(&mut h);
    assert_eq!(h.state().editor.source, PLUGIN_SOURCE, "a refused Apply must not touch the buffer");
    h.state_mut().strategy_source = StrategySource::Rhai;
    settle(&mut h);
    button(&h, "Apply to editor").click();
    settle(&mut h);
    assert_eq!(h.state().editor.source, generated, "in Rhai mode the same click applies");
}

// ======================= the Research pane and the study surface =======================

/// One rhai study, written into a throwaway `user_data/` tree the way a user's own project holds
/// one: `research/studies/rhai/<name>/<name>.rhai`.
///
/// The tier is deliberately the INTERPRETED one and not by preference: the COMPILED tier resolves
/// through a registry `crates/vike-user-research`'s `build.rs` generates by scanning
/// `<workspace>/user_data/research/studies/rust/` at BUILD time, which in every CI checkout is
/// absent — so a compiled study cannot be planted by a test at all, and the interpreted tier is
/// the whole of what a runner can be proven on here. That is the same argument
/// `crates/vike-studio-core/tests/rhai_study_pipeline.rs` makes for its own fixture tree.
fn user_data_with_a_study(name: &str, src: &str) -> (tempfile::TempDir, StudyHost) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let user_data = tmp.path().join("user_data");
    let studies_root = user_data.join("research").join("studies");
    let dir = studies_root.join("rhai").join(name);
    std::fs::create_dir_all(&dir).expect("study folder");
    std::fs::write(dir.join(format!("{name}.rhai")), src).expect("entry file");
    let host = StudyHost {
        studies_root,
        runs_root: user_data.join("runs"),
        scratch: tmp.path().join("tmp"),
        produced_by: "vike-app".to_string(),
        git_sha: None,
    };
    (tmp, host)
}

/// A study that reads the seeded bars and records what it found — the ordinary shape, and enough
/// to give the result surface a metric to render.
const COUNTING_STUDY: &str = r#"
fn run(ctx, params) {
    let bars = ctx.bars("binance", "BTCUSDT", "1m");
    let out = outcome();
    out.metric("n_bars", bars.len());
    return out;
}
"#;

/// **The host gate, both directions.** A Studio with no project above it offers no study dispatch
/// and SAYS why; one with a project and a study in it arms the button.
///
/// Paired the way [`an_empty_store_arms_no_run_affordance_and_a_seeded_one_arms_them`] is, and for
/// the same reason: a gate hard-wired either way fails one half. The negative half asserts the
/// button is ABSENT rather than disabled, because with no host there is no runs directory to mint
/// into and no studies root to list — the pane renders its refusal instead of a dead control.
///
/// ⚠ Neither half CLICKS: a click spawns a real study on a worker thread.
#[test]
fn a_studio_with_no_project_offers_no_study_run_and_a_hosted_one_arms_it() {
    let (dir, store) = seeded_store();
    let mut h = shell(&store, &dir, RightTab::Research);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains(NO_HOST)),
        "a Studio with nowhere to put a run must SAY so; rendered: {text:?}"
    );
    assert!(
        !has_button(&h, &run_button("Run study")),
        "no host, no dispatch — and not a dead button either"
    );

    let (_tmp, host) = user_data_with_a_study("counting", COUNTING_STUDY);
    let mut st = state(&store, &dir, RightTab::Research, ChatApiKeys::default());
    st.research.host = Some(host);
    let mut h = harness_over(st);
    settle(&mut h);
    assert!(!is_disabled(&h, &run_button("Run study")), "a listed study must arm the dispatch");
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t == "counting"),
        "...and the study must be listed BY NAME; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t == "rhai"),
        "...with the tier badge that says which binaries can run it; rendered: {text:?}"
    );
}

/// **The study result surface, over a REAL study run** — the sibling of
/// [`the_results_pane_renders_every_tab_of_a_real_backtest`], and the R6 claim asserted where a
/// reader actually meets it.
///
/// Three independent things, and the third is the one this whole design exists for:
///
/// 1. the study's own numbers are on screen, under a heading that names them as a study's;
/// 2. `vike_studio_core::STUDY_METRICS_NOTE` — the sentence the library writes into every study
///    run's manifest — is rendered VERBATIM beside them, so the reader is told what they are not
///    comparable with;
/// 3. **not one cell of the backtest results surface is on screen at the same time.**
///    `crates/vike-studio/src/results.rs`'s `perf_cells` labels are the marker set, because those
///    are exactly the strings a shared column would have put a study's Sharpe next to.
///
/// The run goes through `start_study` plus a bounded `poll` loop — the real dispatch path, so what
/// is rendered came out of `vike_studio_core::run_study_plan` and landed in `user_data/runs`
/// rather than being a fixture this file invented.
#[test]
fn a_study_run_renders_its_own_surface_and_never_a_backtests_columns() {
    let (dir, store) = seeded_store();
    let (_tmp, host) = user_data_with_a_study("counting", COUNTING_STUDY);
    let runs_root = host.runs_root.clone();
    let mut st = state(&store, &dir, RightTab::Research, ChatApiKeys::default());
    st.research.host = Some(host);
    st.research.refresh();

    st.start_study();
    for _ in 0..400 {
        st.poll();
        if st.study_last.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Copied OUT of the state before the harness takes ownership of it — `harness_over` moves
    // `st`, so a borrow held across it would not compile.
    let (run_id, kind) = match &st.study_last {
        Some(Ok(run)) => (run.run_id.clone(), run.manifest.kind.clone()),
        other => panic!("the study must finish inside the 4s budget and succeed: {other:?}"),
    };
    assert_eq!(kind, STUDY_RUN_KIND, "one kind, whichever tier produced it");
    assert!(runs_root.join(&run_id).is_dir(), "the run is on disk under user_data/runs");

    let mut h = harness_over(st);
    settle(&mut h);
    let text = all_text(&h);

    assert!(
        text.iter().any(|t| t == STUDY_RESULT_TITLE),
        "the central panel must be the STUDY surface; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t == "n_bars"),
        "the study's own metric must be on screen; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains(STUDY_METRICS_NOTE)),
        "the not-comparable-with-a-backtest note must be rendered VERBATIM; rendered: {text:?}"
    );
    for backtest_cell in ["Profit factor", "Max drawdown", "Final equity", "Win rate"] {
        assert!(
            !text.iter().any(|t| t == backtest_cell),
            "{backtest_cell:?} is a backtest column and must not share the frame with a study's \
             numbers; rendered: {text:?}"
        );
    }

    // ...and the run it just minted joins the SHARED list, told apart by kind.
    let rows = h.state().research.runs();
    assert_eq!(rows.len(), 1, "poll() must re-scan the runs after a study lands");
    assert_eq!(rows[0].kind, STUDY_RUN_KIND);
    assert_eq!(rows[0].run_id, run_id);
}

/// **A study that REFUSES is disclosed, and disclosed as a study's failure.**
///
/// A script with no `fn run(ctx, params)` is `RhaiStudy::new`'s named refusal, and the whole point
/// of routing it through `Self::error_state` is that the title says WHICH action failed — the same
/// correction `crates/vike-studio/src/studio/shell.rs`'s central panel already made for sweeps and
/// walk-forwards. A refusal that rendered as an empty panel would be the silence this workspace
/// keeps paying for.
#[test]
fn a_study_that_refuses_is_shown_as_a_study_failure_and_mints_no_run() {
    let (dir, store) = seeded_store();
    let (_tmp, host) = user_data_with_a_study("broken", "fn nope() { 1 }\n");
    let runs_root = host.runs_root.clone();
    let mut st = state(&store, &dir, RightTab::Research, ChatApiKeys::default());
    st.research.host = Some(host);
    st.research.refresh();

    st.start_study();
    for _ in 0..400 {
        st.poll();
        if st.study_last.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(st.study_last, Some(Err(_))), "a script with no entry must refuse");
    assert!(!runs_root.exists(), "a refusal must not mint a run directory");

    let mut h = harness_over(st);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t == "Study failed"),
        "the panel must name WHICH action failed; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains("fn run(ctx, params)")),
        "...and carry the runner's own words, which name the entry the study is missing;          rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t == STUDY_RESULT_TITLE),
        "a refusal is not a result; rendered: {text:?}"
    );
}

/// Spec §4.2: loading, empty and unreachable are three renderings. A store that REFUSED its
/// catalog must not wear the empty store's picture — which it did for as long as `empty_state`
/// drew one Studio mark and one title above both, and only the sentence under them differed.
#[test]
fn a_refused_store_and_an_empty_store_render_differently() {
    let cloud = icons::UNREACHABLE.accessible_label("");
    let tray = icons::EMPTY.accessible_label("");

    let dir = tempfile::tempdir().expect("temp dir");
    let refusing: StoreHandle = Arc::new(RefusingCatalogStore);
    let mut h = shell(&refusing, &dir, RightTab::Sweep);
    settle(&mut h);
    let refused = all_text(&h);
    assert!(refused.contains(&cloud), "a refused store shows the unreachable mark: {refused:?}");
    assert!(!refused.contains(&tray), "...and not the empty one: {refused:?}");

    let (empty_dir, empty) = empty_store();
    let mut h = shell(&empty, &empty_dir, RightTab::Sweep);
    settle(&mut h);
    let blank = all_text(&h);
    assert!(blank.contains(&tray), "an empty store shows the empty mark: {blank:?}");
    assert!(!blank.contains(&cloud), "...and not the unreachable one: {blank:?}");
}
