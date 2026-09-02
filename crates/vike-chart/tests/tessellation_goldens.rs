//! Text goldens over the TESSELLATED chart frame — the rung above
//! [`vike_ui_theme::frame_sanity::assert_frame_sane`].
//!
//! `assert_frame_sane` (which every scenario below ALSO runs, inside `Case::run_full`) answers one
//! question: is anything `NaN`, infinite or absurd? It is deliberately silent about **where** a
//! thing was drawn, **in what order**, and **under what clip**. This file is the only thing in the
//! repo that can see those three, and it sees them without a GPU — so it gates in CI, where
//! `Context::run_ui` and `Context::tessellate` are pure CPU. Pixels stay on the dev box
//! (`just qa-shots`).
//!
//! Each scenario renders the REAL `chart::draw` headlessly, tessellates the last frame on the same
//! `Context`, and compares `vike_ui_theme::frame_record`'s canonical text record against a
//! committed golden in `tests/goldens/`. The record's grammar and the reasoning behind every field
//! it does and does not carry live in that module's docs.
//!
//! # Regenerating
//!
//! ```sh
//! VIKE_REGEN_FRAME_GOLDENS=1 cargo test -p vike-chart --test tessellation_goldens
//! ```
//!
//! …then READ the diff. A regeneration that changes an existing golden must be justified in the
//! commit message — a golden waved through is not a gate. (The same switch shape as
//! `vike-indicators`' `VIKE_REGEN_WINDOW_PIN`, and declared in `vike_ops::settings::SETTINGS` the
//! same way.)
//!
//! # No snapshot crate
//!
//! `insta` (or any snapshot dependency) was considered and NOT taken. A committed `.txt` per
//! scenario plus this file's regeneration switch is ~30 lines, adds nothing to the lockfile or the
//! `cargo deny --all-features` surface that every order-signing binary in this workspace shares,
//! and matches the repo's existing committed-fixture culture (`fixtures/r0..r6`,
//! `vike-indicators`' `window_pin.tsv`, the captured venue frames). The two things `insta` would
//! buy — an interactive review UI and inline snapshots — are worth less here than a flat dependency
//! tree, and its `.snap` headers would add churn to a file whose whole value is that a diff reads
//! cleanly.
//!
//! # Why these goldens are reproducible
//!
//! Three hazards, each handled at its source rather than absorbed by a tolerance:
//!
//! 1. **Timezone.** `ChartState`'s display tz defaults to `DisplayTz::Local`, and it moves both the
//!    x-axis label TEXT and the hour/day GRID-LINE POSITIONS. `common::make_state` pins it to UTC —
//!    see that function's doc for the measurement.
//! 2. **Fonts.** `common::bind_chart_font_families` binds egui's EMBEDDED default font data, not a
//!    system lookup; `vike-app`'s `install_fonts` (which does probe the OS, with a silent fallback)
//!    is never called from a test.
//! 3. **Float drift.** [`a_one_ulp_price_perturbation_leaves_every_record_unchanged`] attacks the
//!    hazard head-on: it nudges every fixture price by one `f64` ULP — the only input a platform's
//!    libm can actually change — and asserts every record is byte-identical.
//!    [`every_scenario_is_stable_against_itself`] is the coarser twin: a record that does not equal
//!    itself must never reach a golden file. A THIRD guard was written, measured, and demoted from
//!    a gate to a diagnostic — see [`rounding_margins_are_reported_but_do_not_gate`].

mod common;

use std::path::PathBuf;

use common::{make_state, price_pane_point, warm_vpin, wave_bars, wave_bars_one_ulp, Case};
use indexmap::IndexMap;
use vike_chart::{Active, Bar, ChartStyle, PaneKey, ScaleMode};
use vike_ui_theme::frame_record::{assert_frame_golden, record_frame, rounding_margin};

/// Regeneration switch — see the module doc. Named so `crates/vike-ops/src/settings.rs`'s
/// `VIKE_REGEN_FRAME_GOLDENS` row (`Naming::Konst("REGEN_ENV")`) resolves through this constant
/// rather than a second copy of the literal.
const REGEN_ENV: &str = "VIKE_REGEN_FRAME_GOLDENS";

/// Printed in every golden failure so the reader is not left guessing how to rewrite the file.
const REGEN_HINT: &str =
    "VIKE_REGEN_FRAME_GOLDENS=1 cargo test -p vike-chart --test tessellation_goldens";

const GOLDEN_DIR: &str = "tests/goldens";

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_DIR).join(format!("{name}.txt"))
}

/// The EXACT string `"1"`, the workspace's idiom for an opt-in gate (`VIKE_RECONCILE`,
/// `VIKE_REGEN_WINDOW_PIN`) — never a fuzzy truthy parse, so a stray `UPDATE=true` in someone's
/// shell cannot silently overwrite every golden in the tree.
fn regen_requested() -> bool {
    std::env::var(REGEN_ENV).as_deref() == Ok("1")
}

/// Render one scenario, record it, and compare against (or rewrite) its golden.
fn check(name: &str) {
    with_scenario(name, |case| {
        let (ppp, primitives) = case.run_tessellated();
        let record = record_frame(name, ppp, &primitives);
        assert_frame_golden(&record, &golden_path(name), regen_requested(), REGEN_HINT);
    });
}

// ============================ the scenario registry ============================

/// EVERY scenario, built once and handed to `f` in order. ONE list, deliberately: the per-scenario
/// golden tests, the self-stability check, the knife-edge measurement and the
/// no-orphan-golden-file gate all walk it, so they cannot drift into covering different sets — the
/// failure mode where a scenario is added, its golden committed, and the stability guards never
/// look at it.
///
/// The 16 rows cover what the frame record can actually discriminate: every PANE CONFIGURATION
/// (price-only / +volume / +volume+study / study-only), every price-render branch reachable without
/// live data (candles, line, the HeikinAshi two-tier transform, the Renko reindexing transform and
/// its volume suppression), all three SCALE modes, the on-chart OVERLAY layer (an overlay indicator
/// painting into the price pane, the last-price chip, the grid), CROSSHAIR on and off in both a
/// one-pane and a three-pane layout, both background branches, and a second screen size so a
/// layout-reflow regression has somewhere to show up.
fn for_each_scenario_with(bars: fn(usize) -> Vec<Bar>, f: &mut dyn FnMut(&str, &Case<'_>)) {
    // ---- pane configurations ----
    {
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.options.show_volume = false;
        f("candles_price_only", &c);
    }
    {
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.sub_panes = &[PaneKey::Volume];
        f("candles_with_volume", &c);
    }
    {
        let st = make_state(bars(60));
        let rsi = vike_chart::indicators::get("rsi").expect("rsi indicator registered");
        let indicators = [Active::new(1, rsi, &st.bars)];
        let sub_panes = [PaneKey::Volume, PaneKey::Study(1)];
        let mut study_pane_of: IndexMap<u64, PaneKey> = IndexMap::new();
        study_pane_of.insert(1, PaneKey::Study(1));
        let mut c = Case::new(&st);
        c.indicators = &indicators;
        c.sub_panes = &sub_panes;
        c.study_pane_of = &study_pane_of;
        f("candles_volume_and_study", &c);
    }
    {
        // A sub-pane held open by a microstructure study ALONE — no indicator authored into it.
        let st = make_state(bars(60));
        let studies = [warm_vpin(9, st.closed_len)];
        let sub_panes = [PaneKey::Volume, PaneKey::Study(9)];
        let mut c = Case::new(&st);
        c.studies = &studies;
        c.sub_panes = &sub_panes;
        f("study_only_pane", &c);
    }

    // ---- price-render branches ----
    {
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.style = ChartStyle::Line;
        c.options.show_volume = false;
        f("line_price_only", &c);
    }
    {
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.style = ChartStyle::HeikinAshi;
        c.sub_panes = &[PaneKey::Volume];
        f("heikin_ashi_with_volume", &c);
    }
    {
        // Renko reindexes, so `style_preserves_volume(Renko) == false` and the authored Volume pane
        // is suppressed downstream — a pane-count property the record shows directly.
        let st = make_state(bars(60));
        let mut c = Case::new(&st);
        c.style = ChartStyle::Renko;
        c.sub_panes = &[PaneKey::Volume];
        f("renko_volume_suppressed", &c);
    }

    // ---- scale modes ----
    {
        let st = make_state(bars(60));
        let mut c = Case::new(&st);
        c.scale = ScaleMode::Log;
        c.sub_panes = &[PaneKey::Volume];
        f("log_scale_with_volume", &c);
    }
    {
        let st = make_state(bars(60));
        let mut c = Case::new(&st);
        c.scale = ScaleMode::Percent;
        c.sub_panes = &[PaneKey::Volume];
        f("percent_scale_with_volume", &c);
    }

    // ---- the overlay layer ----
    {
        // An OVERLAY indicator paints into the price pane itself rather than opening a sub-pane —
        // the other half of the indicator render path from `candles_volume_and_study`.
        let st = make_state(bars(60));
        let sma = vike_chart::indicators::get("sma").expect("sma indicator registered");
        let indicators = [Active::new(2, sma, &st.bars)];
        assert!(indicators[0].is_overlay(), "sma must be an overlay for this scenario to mean it");
        let mut c = Case::new(&st);
        c.indicators = &indicators;
        c.sub_panes = &[PaneKey::Volume];
        f("sma_overlay_on_price_pane", &c);
    }
    {
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.options.show_grid = false;
        c.sub_panes = &[PaneKey::Volume];
        f("grid_off", &c);
    }
    {
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.options.show_last_price = false;
        c.sub_panes = &[PaneKey::Volume];
        f("last_price_chip_off", &c);
    }

    // ---- crosshair on/off ----
    // There is no crosshair FLAG: the crosshair and its axis tags are driven by the pointer, so
    // "on" is a pointer inside the price pane and "off" is no pointer at all. Both a one-pane and a
    // three-pane layout, because the crosshair's vertical line is drawn per pane.
    {
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.options.show_volume = false;
        c.frames = 3; // the plot's bounds must warm up before a pointer resolves to a bar
        c.pointer = Some(price_pane_point(c.screen));
        f("crosshair_price_only", &c);
    }
    {
        let st = make_state(bars(60));
        let rsi = vike_chart::indicators::get("rsi").expect("rsi indicator registered");
        let indicators = [Active::new(1, rsi, &st.bars)];
        let sub_panes = [PaneKey::Volume, PaneKey::Study(1)];
        let mut study_pane_of: IndexMap<u64, PaneKey> = IndexMap::new();
        study_pane_of.insert(1, PaneKey::Study(1));
        let mut c = Case::new(&st);
        c.indicators = &indicators;
        c.sub_panes = &sub_panes;
        c.study_pane_of = &study_pane_of;
        c.frames = 3;
        c.pointer = Some(price_pane_point(c.screen));
        f("crosshair_three_panes", &c);
    }

    // ---- background branch + a second viewport size ----
    {
        // `bg_gradient` defaults to TRUE, so every other scenario records the gradient mesh; this
        // is the solid-fill branch, which must paint a flat rect and never touch `bg_top`.
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.options.bg_gradient = false;
        c.sub_panes = &[PaneKey::Volume];
        f("solid_background", &c);
    }
    {
        // A materially smaller viewport: pane heights, tick density and label elision all reflow,
        // so a layout regression that happens to be invisible at 960×620 has a second chance here.
        let st = make_state(bars(40));
        let mut c = Case::new(&st);
        c.screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(480.0, 360.0));
        c.sub_panes = &[PaneKey::Volume];
        f("narrow_viewport_with_volume", &c);
    }
}

/// The registry over the ORDINARY fixture — what every golden is recorded from.
fn for_each_scenario(f: &mut dyn FnMut(&str, &Case<'_>)) {
    for_each_scenario_with(wave_bars, f);
}

/// Run `f` against the one scenario called `name`, built from `bars`.
fn with_scenario_from<R>(
    bars: fn(usize) -> Vec<Bar>,
    name: &str,
    f: impl FnOnce(&Case<'_>) -> R,
) -> R {
    let mut slot = Some(f);
    let mut out = None;
    for_each_scenario_with(bars, &mut |n, case| {
        if n == name {
            if let Some(g) = slot.take() {
                out = Some(g(case));
            }
        }
    });
    out.unwrap_or_else(|| panic!("no scenario named {name} in for_each_scenario_with"))
}

/// Run `f` against the one scenario called `name`.
fn with_scenario<R>(name: &str, f: impl FnOnce(&Case<'_>) -> R) -> R {
    with_scenario_from(wave_bars, name, f)
}

fn scenario_names() -> Vec<String> {
    let mut names = Vec::new();
    for_each_scenario(&mut |n, _| names.push(n.to_owned()));
    names
}

// ============================ one test per scenario ============================
// Separate tests rather than one loop: nextest runs them in parallel, and a failure names the
// scenario in the test name instead of only in the panic body.

#[test]
fn golden_candles_price_only() {
    check("candles_price_only");
}

#[test]
fn golden_candles_with_volume() {
    check("candles_with_volume");
}

#[test]
fn golden_candles_volume_and_study() {
    check("candles_volume_and_study");
}

#[test]
fn golden_study_only_pane() {
    check("study_only_pane");
}

#[test]
fn golden_line_price_only() {
    check("line_price_only");
}

#[test]
fn golden_heikin_ashi_with_volume() {
    check("heikin_ashi_with_volume");
}

#[test]
fn golden_renko_volume_suppressed() {
    check("renko_volume_suppressed");
}

#[test]
fn golden_log_scale_with_volume() {
    check("log_scale_with_volume");
}

#[test]
fn golden_percent_scale_with_volume() {
    check("percent_scale_with_volume");
}

#[test]
fn golden_sma_overlay_on_price_pane() {
    check("sma_overlay_on_price_pane");
}

#[test]
fn golden_grid_off() {
    check("grid_off");
}

#[test]
fn golden_last_price_chip_off() {
    check("last_price_chip_off");
}

#[test]
fn golden_crosshair_price_only() {
    check("crosshair_price_only");
}

#[test]
fn golden_crosshair_three_panes() {
    check("crosshair_three_panes");
}

#[test]
fn golden_solid_background() {
    check("solid_background");
}

#[test]
fn golden_narrow_viewport_with_volume() {
    check("narrow_viewport_with_volume");
}

#[test]
fn the_narrow_viewport_scenario_paints_no_horizontally_truncated_text() {
    // The opt-in truncation rung (`vike_ui_theme::frame_sanity`'s `clipped_text_shapes`; its kill
    // proofs are `frame_record_gate.rs`'s defect 5), wired where its one false-positive class is
    // structurally absent: `chart::draw` mounts no `ScrollArea` (the one in
    // `crates/vike-chart/src/options_chain.rs` is a different renderer, reached by none of these
    // scenarios). The NARROW pose is where labels have the least room — where a truncation
    // regression lands first — and the committed goldens show zero horizontal glyph-vs-clip
    // overhang across all sixteen scenarios today (re-measured over `tests/goldens/` when this was
    // wired), so the gate starts green by measurement, not by hope. ONE pose, deliberately:
    // extending to `for_each_scenario` would arm fifteen more assertions in a session that cannot
    // compile them — a follow-up for one that can.
    with_scenario("narrow_viewport_with_volume", |case| {
        let (_acts, out) = case.run_frames();
        vike_ui_theme::frame_sanity::assert_no_clipped_text(
            &out,
            vike_ui_theme::frame_sanity::CLIPPED_TEXT_TOL,
        );
    });
}

// ============================ the guards that keep the suite honest ============================

#[test]
fn every_scenario_is_stable_against_itself() {
    // A golden that is not reproducible is worse than no golden: it teaches everyone to
    // re-baseline on red, and the next real regression is re-baselined with it. Rendering each
    // scenario twice on two fresh `Context`s and comparing the records is the cheapest guard
    // against the whole class — a stray `Instant::now`, a HashMap iteration order, a cached atlas
    // state leaking between frames.
    //
    // ⚠ This runs both records in ONE process, so it cannot see a per-process difference (a
    // randomly seeded hasher would agree with itself here). egui hashes ids with
    // `ahash::RandomState::with_seeds(1, 2, 3, 4)` — fixed constants, not a random seed — and the
    // cross-process half was measured separately by generating the whole golden set twice from a
    // clean tree and diffing.
    for name in scenario_names() {
        with_scenario(&name, |case| {
            let (ppp_a, a) = case.run_tessellated();
            let (ppp_b, b) = case.run_tessellated();
            assert_eq!(ppp_a, ppp_b, "{name}: pixels_per_point differed between two runs");
            let (ra, rb) = (record_frame(&name, ppp_a, &a), record_frame(&name, ppp_b, &b));
            assert_eq!(ra, rb, "{name}: the frame record is not stable against itself");
        });
    }
}

#[test]
fn a_one_ulp_price_perturbation_leaves_every_record_unchanged() {
    // THE cross-platform reproducibility gate, aimed at the hazard itself rather than a proxy.
    //
    // Everything from `run_ui` through `tessellate` is `f32` arithmetic that Rust will not
    // reassociate and LLVM will not contract, so the same logic executes the same operations in the
    // same order on Linux and on Windows. The ONE input that can genuinely differ is libm: the
    // fixtures compute prices with `f64::sin`/`f64::cos`, and glibc and the MSVC runtime are each
    // entitled to their own last bit.
    //
    // So: perturb exactly that, by MORE than it can really move (`wave_bars_one_ulp` nudges each
    // price a full ULP, roughly 10x the worst libm disagreement — see its doc), and require every
    // one of the sixteen records to come out byte-identical. A pass is a direct statement that a
    // libm difference cannot rebaseline this suite; a failure would name the scenario whose layout
    // is genuinely balanced on a rounding edge, which is a real defect worth seeing.
    for name in scenario_names() {
        let base = with_scenario_from(wave_bars, &name, |case| {
            let (ppp, p) = case.run_tessellated();
            record_frame(&name, ppp, &p)
        });
        let nudged = with_scenario_from(wave_bars_one_ulp, &name, |case| {
            let (ppp, p) = case.run_tessellated();
            record_frame(&name, ppp, &p)
        });
        assert_eq!(
            base, nudged,
            "{name}: a one-ULP price change moved the frame record — this golden would not \
             survive a platform whose libm rounds `sin` differently"
        );
    }
}

#[test]
fn rounding_margins_are_reported_but_do_not_gate() {
    // ⚠ A DEMOTED INVARIANT, kept as a diagnostic and documented here so the next person reads the
    // measurement instead of re-running the experiment.
    //
    // This began as a gate: "no recorded coordinate may sit within EPS of a `.5` rounding
    // boundary", on the theory that such a coordinate is one a platform's float drift could flip.
    // It fired on healthy frames TWICE, at two different thresholds and on two different scenarios:
    //
    //   * EPS 1e-3 — `candles_with_volume` puts a coordinate at 63.499626 (3.7e-4 from a boundary).
    //   * EPS 1e-5 — `solid_background` puts one at 0.49999997, i.e. 3e-8, exactly one f32 ULP.
    //
    // Neither is fragile, and the second makes the reason plain: it is a pure LAYOUT coordinate
    // (screen size × pane fractions), computed with no libm anywhere in its history, and therefore
    // bit-identical on every platform. Proximity to a boundary does not distinguish a reproducible
    // coordinate from a fragile one, so no threshold over it can be both non-vacuous and correct.
    // The gate is `a_one_ulp_price_perturbation_leaves_every_record_unchanged` above.
    //
    // What is left is worth keeping, and worth ASSERTING at the level it can support: that the
    // measurement is actually looking at real frames. It costs nothing and it is the number to
    // reach for first when a golden does move unexpectedly.
    let mut tightest = f32::MAX;
    let mut tightest_at = String::new();
    for name in scenario_names() {
        with_scenario(&name, |case| {
            let (_ppp, prims) = case.run_tessellated();
            let m = rounding_margin(&prims);
            assert!(
                m.samples > 100,
                "{name}: only {} coordinates measured — the scenario rendered almost nothing",
                m.samples
            );
            assert!(
                m.exact_halves > 0,
                "{name}: no exact half-integer coordinate — epaint's 0.5 feathering means a real \
                 frame always has some, so the measurement is not reading the frame it thinks: {m}"
            );
            if m.min_margin < tightest {
                tightest = m.min_margin;
                tightest_at = format!("{name}: {m}");
            }
        });
    }
    println!("tightest rounding margin across all scenarios — {tightest_at}");
}

#[test]
fn pixels_per_point_is_pinned_to_one() {
    // Every recorded number is in logical points, and epaint's anti-aliasing feathering is sized in
    // PHYSICAL pixels — so a scenario rendered at a different `pixels_per_point` produces different
    // vertex counts and different bounding boxes for identical layout. The harness never sets one,
    // which means 1.0; pinning it here makes that an assertion rather than an assumption, so a
    // future `RawInput` change that introduces a scale factor fails here instead of silently
    // rebaselining sixteen goldens.
    //
    // ONE scenario, not sixteen: `pixels_per_point` comes from the `RawInput` the shared harness
    // builds, which is identical for every row, so rendering the other fifteen would cost half a
    // second of CI time to re-assert the same fact. (`every_scenario_is_stable_against_itself`
    // separately checks that a scenario's ppp does not move between two runs of ITSELF.)
    let name = scenario_names().first().cloned().expect("the registry is not empty");
    with_scenario(&name, |case| {
        let (ppp, _prims) = case.run_tessellated();
        assert_eq!(ppp, 1.0, "{name}: goldens are recorded at ppp 1.0");
    });
}

#[test]
fn every_golden_file_belongs_to_a_scenario_and_vice_versa() {
    // A scenario deleted from the registry leaves its golden behind, where it looks like coverage
    // and is checked by nothing. The mirror case — a scenario with no golden — already fails
    // loudly, but is checked here too so one test answers "is the suite complete".
    if regen_requested() {
        // Under a rewrite this test races the sixteen that are writing the directory, and on a
        // first-ever generation it would fail against a directory that does not exist yet — which
        // is precisely the run where the failure means nothing. Measured, not theorised: the
        // documented regeneration command failed on its first invocation for exactly this reason.
        return;
    }
    let names = scenario_names();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_DIR);
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.expect("readable dir entry").file_name().to_string_lossy().into_owned())
        .filter_map(|f| f.strip_suffix(".txt").map(str::to_owned))
        .collect();
    on_disk.sort();
    let mut expected = names.clone();
    expected.sort();
    assert_eq!(
        expected, on_disk,
        "the scenario registry and {GOLDEN_DIR} disagree — an orphan golden checks nothing, \
         and a scenario with no golden cannot pass. Regenerate with:  {REGEN_HINT}"
    );

    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "two scenarios share a name and one golden");
}
