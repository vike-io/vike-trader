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
//!    `ui()`'s `tool_pane_ui` phase that routes seven tabs to seven renderers. Swapping two arms
//!    there keeps every pure test
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
//! `crates/vike-studio/src/studio/workspace.rs`'s `workspace_write_path` — the state directory a
//! root declared, else `<project>/settings/state` found by walking UP from the working directory,
//! and a test declares nothing. Under
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
//! - **P1 routing** — swap the `RightTab::Saved` and `RightTab::Chat` arms of `tool_pane_ui`'s
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

// The scenarios are split by theme into child modules of THIS one test binary (`--test
// studio_shell_render` still runs all of them): `support` is the shared harness plumbing and
// accessibility-tree queries; `panes` the tool-tab, run-gate, chat, results and toolbar scenarios;
// `disclosure` the refused-scan and depth-only placeholders; `editor_verdict` the editor header's
// verdict per strategy source; `research` the Research pane and the study surface. Every test keeps
// its name; its libtest path gains the module prefix, so a substring filter still finds it.
//
// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod panes;` would resolve
// against `tests/` (the root's own directory), not `tests/studio_shell_render/`. `common` is the
// opposite case and stays bare: it is the shared fixture module the sibling test binaries also use.
mod common;
#[path = "studio_shell_render/disclosure.rs"]
mod disclosure;
#[path = "studio_shell_render/editor_verdict.rs"]
mod editor_verdict;
#[path = "studio_shell_render/panes.rs"]
mod panes;
#[path = "studio_shell_render/research.rs"]
mod research;
#[path = "studio_shell_render/support.rs"]
mod support;
