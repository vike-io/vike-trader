//! Text goldens over the TESSELLATED Studio shell — this crate's rung 4.
//! `crates/vike-studio/tests/studio_shell_render.rs` proves WHAT is on screen (the accessibility
//! tree) and that every frame is paintable
//! (`crates/vike-ui-theme/src/frame_sanity.rs`'s `assert_frame_sane`); this file is the only
//! thing that can see WHERE the shell drew it, in what ORDER, and under which CLIP — the three
//! properties `crates/vike-ui-theme/src/frame_record.rs`'s `record_frame` exists to capture, and
//! whose grammar and stability argument live in that module's docs. The vike-chart original is
//! `crates/vike-chart/tests/tessellation_goldens.rs`; this is its Studio twin, and where the two
//! differ the difference is argued below rather than inherited.
//!
//! Each scenario drives the REAL `StudioState::ui` through `egui::Context::run_ui` for a fixed
//! frame schedule — the `crates/vike-studio/examples/studio_shot.rs` pattern, not `egui_kittest`,
//! because tessellation must run on the `Context` that ran the frames (the font atlas lives
//! there) and `Harness` does not hand its context back — then tessellates the last frame on that
//! same context and compares the canonical text record against a committed golden in
//! `tests/goldens/`.
//!
//! # ⚠ PLACEHOLDER GOLDENS — the three committed `.txt` files are markers, not records
//!
//! This suite was authored on a box where cargo cannot run, so the goldens could not be
//! generated. Every `golden_*` test below is RED — deliberately, loudly, with the regeneration
//! command in the failure — until someone runs, on a lane:
//!
//! ```sh
//! VIKE_REGEN_FRAME_GOLDENS=1 cargo test -p vike-studio --test tessellation_goldens
//! ```
//!
//! …then READS the three records and commits them. One command, before this branch PRs. A
//! regeneration that changes an existing golden must be justified in the commit message — a
//! golden waved through is not a gate. (The same switch shape as vike-chart's suite, and declared
//! in `vike_ops::settings::SETTINGS` the same way — one more `(name, krate)` row, because rows
//! are keyed per crate.)
//!
//! # Why THESE three poses
//!
//! All three sit on `RightTab::Sweep`, and they triangulate
//! `crates/vike-studio/src/studio.rs`'s `empty_state` chain — the exact surface the render
//! suite's P3/P3b plants pin, seen from the geometry side: a refused catalog scan
//! (`SERIES_SCAN_ADVICE` plus the store's own reason, in the error colour), an empty store (the
//! backfill copy), and a seeded store (the pickable ELSE arm: the getting-started copy, the
//! picker auto-selected, `▶ Run` armed). A defect in that chain's ORDER reddens the a11y suite
//! by wording and THIS suite by pixels-side geometry — two independent witnesses of the same
//! defect family. The chain's fourth arm — the depth-only store — is deliberately not posed: its
//! disclosure is pinned by the a11y suite's P3b pair, it adds no render branch the other three
//! do not already walk (the same centered placeholder, one more copy string), and a fourth
//! scenario here is one `SCENARIOS` row plus one [`build`] arm the day that stops being true.
//!
//! # Why these records are byte-stable (argued hazard by hazard, not assumed)
//!
//! - **No wall clock, no timezone.** vike-studio links no chrono and calls no `Instant`/
//!   `SystemTime` anywhere in `src/` (grepped, not presumed); egui's animation clock is fed a
//!   fixed `time: f / 60.0` per frame. The one pane that renders store-coverage numbers is the
//!   Data pane, which is deliberately NOT posed.
//! - **No store path reaches a glyph.** Nothing rendered on the Sweep tab prints a path (the one
//!   `to_string_lossy` in the crate is `crates/vike-studio/src/chat.rs`'s connect-command, behind
//!   a click this suite never sends). [`every_scenario_is_stable_against_itself`] is the direct
//!   gate: it builds each pose TWICE over two fresh temp directories, so a path leaking into
//!   rendered text fails same-machine instead of flaking cross-machine.
//! - **No libm value reaches a glyph or a vertex.** The seeded bars are `sin`/`cos` fixtures, but
//!   on this tab they surface only as the picker's static label (`binance · BTCUSDT · 1m`) — no
//!   price is ever laid out. That is why the chart suite's one-ULP perturbation twin
//!   (`crates/vike-chart/tests/tessellation_goldens.rs`'s
//!   `a_one_ulp_price_perturbation_leaves_every_record_unchanged`) has no counterpart here: an
//!   ARGUED omission, not an oversight — there is no fixture float whose last bit can move the
//!   frame, because no fixture float is ever drawn.
//! - **Fonts are egui's embedded defaults.** No named `FontFamily` is reachable from a Studio
//!   frame — `studio_shell_render.rs`'s module doc carries the measured dependency-graph
//!   argument — and `egui_code_editor` lays out through `FontId::monospace`, which
//!   `FontDefinitions::default` always defines. A future violation is loud: epaint panics naming
//!   the unbound family on the first frame.
//! - **Fixed schedule, pinned scale.** [`FRAMES`] frames at `f / 60.0` on a fresh `Context`,
//!   `pixels_per_point` never set and pinned at 1 by [`pixels_per_point_is_pinned_to_one`].
//! - **No workspace bleed.** `common::state` scrubs the restorable workspace fields and arms the
//!   readonly qa constructor, so the frames neither read nor write the box's real
//!   `studio_workspace.json` — `tests/common/mod.rs`'s module doc carries both halves.
//!
//! # No snapshot crate
//!
//! Inherited from the vike-chart original verbatim, because the argument is workspace-wide, not
//! chart-specific: a committed `.txt` per scenario plus the regen switch adds nothing to the
//! lockfile or the `cargo deny --all-features` surface every order-signing binary here shares.
//!
//! # Kill proof — OWED TO THE LANE
//!
//! The comparator itself is born demonstrated: the placeholder goldens above mean every
//! `golden_*` test FAILS until the real records land, which is a live demonstration that
//! `assert_frame_golden` gates. Two provoked plants remain owed, to be run on the lane beside the
//! regen and recorded here the way `studio_shell_render.rs`'s table records its eight:
//!
//! - **G1 comparator reach** — flip one byte in a committed golden's `runs` column. Expect
//!   exactly that scenario's `golden_*` test to fail, printing the first differing line and the
//!   regen hint, and the other two to stay green.
//! - **G2 the shared defect family** — apply the render suite's P3 plant (hoist the bare
//!   `available().is_empty()` arm in `empty_state`). Expect [`golden_sweep_refused_scan`] to move
//!   (the central panel's geometry now carries the empty-store copy) while
//!   [`golden_sweep_seeded_store`] stays green — the goldens seeing the same defect the a11y
//!   suite sees, from the pixels' side.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{empty_store, seeded_store, state, RefusingCatalogStore};
use vike_studio::{ChatApiKeys, RightTab, StoreHandle, StudioState};
use vike_ui_theme::frame_record::{assert_frame_golden, record_frame};

/// Regeneration switch — see the module doc. Named so `crates/vike-ops/src/settings.rs`'s
/// `VIKE_REGEN_FRAME_GOLDENS` row for THIS crate (`Naming::Konst("REGEN_ENV")`) resolves through
/// this constant rather than a second copy of the literal — deliberately the SAME variable as
/// vike-chart's suite, so one idiom rewrites every frame-golden suite in the tree.
const REGEN_ENV: &str = "VIKE_REGEN_FRAME_GOLDENS";

/// Printed in every golden failure so the reader is not left guessing how to rewrite the file.
const REGEN_HINT: &str =
    "VIKE_REGEN_FRAME_GOLDENS=1 cargo test -p vike-studio --test tessellation_goldens";

const GOLDEN_DIR: &str = "tests/goldens";

/// Every scenario, in one list — the per-scenario golden tests, the stability check and the
/// no-orphan-golden gate all walk it, so they cannot drift into covering different sets. Three
/// rows do not need the chart suite's closure-registry machinery; [`build`]'s exhaustive match
/// panics on a name this list and it disagree about.
const SCENARIOS: [&str; 3] = ["sweep_empty_store", "sweep_seeded_store", "sweep_refused_scan"];

/// Fixed frame schedule. Three because egui needs a second frame for combo/panel layout to settle
/// (`crates/vike-studio/examples/studio_shot.rs` measured that for these same poses) and a third
/// costs nothing; determinism needs the COUNT fixed, not the UI settled — both runs of a scenario
/// walk the same schedule, which is what [`every_scenario_is_stable_against_itself`] holds.
const FRAMES: usize = 3;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_DIR).join(format!("{name}.txt"))
}

/// The EXACT string `"1"`, the workspace's idiom for an opt-in gate (`VIKE_RECONCILE`,
/// `VIKE_REGEN_WINDOW_PIN`) — never a fuzzy truthy parse, so a stray `UPDATE=true` in someone's
/// shell cannot silently overwrite every golden in the tree.
fn regen_requested() -> bool {
    std::env::var(REGEN_ENV).as_deref() == Ok("1")
}

/// Build one scenario: the store shape, and a posed, workspace-scrubbed `StudioState` over it.
///
/// The `TempDir` rides the tuple because the store keeps reading that directory on every frame;
/// the `StoreHandle` rides it only so the tuple owns everything the state borrows from — the
/// state holds its own clone.
fn build(name: &str) -> (tempfile::TempDir, StoreHandle, StudioState) {
    match name {
        // The empty-store arm of `empty_state`: the backfill copy, no dispatch affordance.
        "sweep_empty_store" => {
            let (dir, store) = empty_store();
            let st = state(&store, &dir, RightTab::Sweep, ChatApiKeys::default());
            (dir, store, st)
        }
        // The pickable arm: `SlicePicker::refresh` auto-selects row 0, so the toolbar shows
        // `binance · BTCUSDT · 1m`, the getting-started copy renders and `▶ Run` is armed. A
        // deliberate bake-in: a change to the auto-select or the default template legitimately
        // rebaselines this golden, and the diff will say so in one line.
        "sweep_seeded_store" => {
            let (dir, store) = seeded_store();
            let st = state(&store, &dir, RightTab::Sweep, ChatApiKeys::default());
            (dir, store, st)
        }
        // The refusal arm: `SERIES_SCAN_ADVICE` plus the store's own reason in the error colour —
        // the P3 surface, recorded as geometry.
        "sweep_refused_scan" => {
            let dir = tempfile::tempdir().expect("temp dir");
            let store: StoreHandle = Arc::new(RefusingCatalogStore);
            let st = state(&store, &dir, RightTab::Sweep, ChatApiKeys::default());
            (dir, store, st)
        }
        other => panic!("no scenario named {other} — SCENARIOS and build() disagree"),
    }
}

/// Drive `ui()` for [`FRAMES`] frames on a fresh `Context` and tessellate the LAST frame on that
/// same context — the shape of `crates/vike-chart/tests/common/mod.rs`'s `run_tessellated`.
///
/// 1440×960 for the same measured reason as `studio_shell_render.rs`'s `harness_over`: the shell
/// is four panels wide, and a cramped viewport clips controls — here that would mean recording a
/// clipped shell and calling it the layout. The `textures_delta.clear()` runs BEFORE the sanity
/// assertion, and the order is load-bearing — `clear` touches `textures_delta` and never
/// `shapes`, while asserting first leaves the deltas unapplied and egui 0.36 panics a SECOND time
/// in the unwind, aborting the process before the bad coordinate is printed (measured by the
/// chart harness on a planted NaN; its `run_full` carries the full account).
fn run_tessellated(st: &mut StudioState) -> (f32, Vec<egui::epaint::ClippedPrimitive>) {
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 960.0));
    let mut last: Option<egui::FullOutput> = None;
    for f in 0..FRAMES {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| st.ui(ui));
        out.textures_delta.clear();
        vike_ui_theme::frame_sanity::assert_frame_sane(&out);
        last = Some(out);
    }
    let out = last.expect("FRAMES is non-zero, so at least one frame ran");
    let ppp = out.pixels_per_point;
    (ppp, ctx.tessellate(out.shapes, ppp))
}

/// Build and render one scenario. The `_dir` binding is what keeps the store's directory alive
/// through the frames — `_` alone would drop it before the first one.
fn run_scenario(name: &str) -> (f32, Vec<egui::epaint::ClippedPrimitive>) {
    let (_dir, _store, mut st) = build(name);
    run_tessellated(&mut st)
}

/// Render one scenario, record it, and compare against (or rewrite) its golden.
fn check(name: &str) {
    let (ppp, primitives) = run_scenario(name);
    let record = record_frame(name, ppp, &primitives);
    assert_frame_golden(&record, &golden_path(name), regen_requested(), REGEN_HINT);
}

// ============================ one test per scenario ============================
// Separate tests rather than one loop: nextest runs them in parallel, and a failure names the
// scenario in the test name instead of only in the panic body — the chart suite's reasoning.

#[test]
fn golden_sweep_empty_store() {
    check("sweep_empty_store");
}

#[test]
fn golden_sweep_seeded_store() {
    check("sweep_seeded_store");
}

#[test]
fn golden_sweep_refused_scan() {
    check("sweep_refused_scan");
}

// ============================ the guards that keep the suite honest ============================

#[test]
fn every_scenario_is_stable_against_itself() {
    // A golden that is not reproducible is worse than no golden: it teaches everyone to
    // re-baseline on red. Each scenario is built TWICE from scratch — two fresh temp directories,
    // two fresh `Context`s — and the two records must be byte-equal. The fresh-tempdir half is
    // this suite's own upgrade over the chart twin (whose fixtures hold no directory): it is the
    // DIRECT gate on a store path leaking into rendered text, the hazard the pose selection
    // avoids by construction and this test refuses to merely assume away.
    //
    // ⚠ Both records still come from ONE process, so a per-process difference (a randomly seeded
    // hasher) would agree with itself here — egui hashes ids with fixed seeds, and the
    // cross-process half is what the lane regen's committed-then-rechecked cycle measures.
    for name in SCENARIOS {
        let (ppp_a, a) = run_scenario(name);
        let (ppp_b, b) = run_scenario(name);
        assert_eq!(ppp_a, ppp_b, "{name}: pixels_per_point differed between two builds");
        let (ra, rb) = (record_frame(name, ppp_a, &a), record_frame(name, ppp_b, &b));
        assert_eq!(ra, rb, "{name}: the frame record is not stable against itself");
    }
}

#[test]
fn pixels_per_point_is_pinned_to_one() {
    // Every recorded number is in logical points and epaint's feathering is sized in PHYSICAL
    // pixels, so a scale factor would move vertex counts and boxes for identical layout. The
    // driver never sets one, which means 1.0; pinning it makes that an assertion rather than an
    // assumption. ONE scenario, not three: the `RawInput` is identical for every row (the chart
    // suite's reasoning, at a third of its row count).
    let (ppp, _prims) = run_scenario(SCENARIOS[0]);
    assert_eq!(ppp, 1.0, "goldens are recorded at ppp 1.0");
}

#[test]
fn every_golden_file_belongs_to_a_scenario_and_vice_versa() {
    // A scenario deleted from SCENARIOS leaves its golden behind, where it looks like coverage
    // and is checked by nothing; a scenario with no golden already fails loudly, but is checked
    // here too so one test answers "is the suite complete".
    if regen_requested() {
        // Under a rewrite this test races the three writers, and on a first-ever generation it
        // would fail against a directory that does not exist yet — precisely the run where the
        // failure means nothing. Measured on the chart twin's very first documented regen.
        return;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_DIR);
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.expect("readable dir entry").file_name().to_string_lossy().into_owned())
        .filter_map(|f| f.strip_suffix(".txt").map(str::to_owned))
        .collect();
    on_disk.sort();
    let mut expected: Vec<String> = SCENARIOS.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    assert_eq!(
        expected, on_disk,
        "SCENARIOS and {GOLDEN_DIR} disagree — an orphan golden checks nothing, and a scenario \
         with no golden cannot pass. Regenerate with:  {REGEN_HINT}"
    );
}
