//! The shared headless-`draw` harness: the OHLCV fixtures, the [`Case`] scenario builder, and the
//! font/timezone prerequisites every `chart::draw` render test needs.
//!
//! MOVED here, not copied. It grew inside `draw_characterization.rs`; `tessellation_goldens.rs`
//! then needed the same scenarios, and this repo's standing rule is that a law spelled twice is the
//! defect. An integration test cannot `use` another integration test — each `tests/*.rs` is its own
//! crate — so `tests/common/mod.rs`, included by both with `mod common;`, is the one place the
//! harness can live. (A `tests/` SUBDIRECTORY module is not itself a test target, so this file
//! builds no extra binary and runs no extra test.)
//!
//! Headless mechanics (egui 0.36 / egui_plot 0.36): egui computes hover/interaction from the
//! PREVIOUS frame's widget geometry, so a single frame can't register a pointer over a widget that
//! didn't exist yet — every scenario runs ≥2 frames on one persistent `Context` (the plot's stored
//! bounds + last-frame widget rects carry over), feeding the pointer each frame and the
//! zoom/interaction event on the last one. The pointer is placed in the upper-left-of-centre of the
//! price pane: above the bottom nav/scale overlay rows, left of the right price-axis gutter, so it
//! lands on the price plot itself (the only pane whose closure fills `ChartActions::hovered`).

// Both including binaries use a SUBSET of this module, and each compiles it separately — so an item
// only `tessellation_goldens.rs` calls is dead code in `draw_characterization.rs` and vice versa,
// and `-D warnings` would fail on a helper that is used, just not by the binary being compiled.
// This is the standard `tests/common` idiom; it suppresses nothing about the crate under test.
#![allow(dead_code)]

use indexmap::IndexMap;
use vike_chart::scale::ScaleAssign;
use vike_chart::{
    Active, ActiveStudy, Bar, ChartActions, ChartInputs, ChartOptions, ChartState, ChartStyle,
    DisplayTz, FollowLive, IndicatorDialog, PaneFractions, PaneKey, ScaleMode, SettingsDialog,
    draw,
};

// ============================ fixtures ============================

/// A gently-varying OHLCV series (deterministic, no RNG) — safe for EVERY chart style, including the
/// path-dependent transforms (Renko/Range/Kagi/PnF) whose `auto_box` needs a non-degenerate price
/// range. One-minute bars from a fixed epoch. All bars CLOSED (no forming bar), matching the app's
/// steady state and the `benches/render.rs` construction pattern.
pub fn wave_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let base = 100.0 + (i as f64 * 0.30).sin() * 6.0;
            Bar {
                t: i as f64,
                ot: 1_700_000_000_000 + i as i64 * 60_000,
                o: base,
                h: base + 2.5,
                l: base - 2.5,
                c: base + (i as f64 * 0.7).cos(),
                v: 10.0 + (i % 7) as f64,
            }
        })
        .collect()
}

/// A perfectly UNIFORM series: every bar has the same O/H/L/C. Used only by the exact-OHLC hover
/// test — because all bars are identical, the hovered readout is `[o, h, l, c]` regardless of which
/// bar the pointer's plot-x resolves to, making the assertion independent of the (fiddly, layout-
/// dependent) exact screen→plot-coordinate mapping.
pub fn flat_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            t: i as f64,
            ot: 1_700_000_000_000 + i as i64 * 60_000,
            o: 1.0,
            h: 2.0,
            l: 0.5,
            c: 1.5,
            v: 10.0,
        })
        .collect()
}

/// [`wave_bars`] with every PRICE nudged up by exactly one `f64` ULP.
///
/// The perturbation-robustness probe for `tessellation_goldens.rs`. `wave_bars` computes prices
/// with `f64::sin`/`f64::cos` — libm functions, and glibc and the MSVC runtime are each entitled to
/// their own last bit. That is the ONLY input to a headless chart frame that can differ between two
/// platforms running the same binary logic; everything downstream is `f32` arithmetic Rust will not
/// reassociate. So the perturbation is a whole ULP of the PRICE, which is exactly the right size.
///
/// ⚠ **It used to say "roughly 10× more than the real hazard", and that was MEASURED FALSE on
/// 2026-08-25 — the guard is sized 1×, not 10×.** The reasoning behind the old claim was that a
/// 1-ULP error in `sin(x)` (magnitude ≤ 1, so ≤ 2.2e-16) scaled by 6 moves the price by ≤ 1.3e-15,
/// under a tenth of an ULP at magnitude 100. The perturbation is indeed that small; the ROUNDED
/// RESULT is not bounded by it. When the exact sum falls within that distance of the midpoint
/// between two representable doubles, the two platforms round to different neighbours and the
/// stored price differs by a FULL ULP. Comparing this fixture's `o` and `c` columns computed on
/// Windows against the same columns computed on the CI box (n = 400): one index — 207 — differs, in
/// both columns, by exactly one ULP (`4057ff646a7269f9` vs `…f8`, `40583aaa5d571031` vs `…30`).
/// The guard therefore COVERS the real hazard exactly and has no headroom above it. Do not
/// "simplify" it to a fraction of an ULP on the strength of the magnitude argument.
///
/// Volume is deliberately NOT perturbed: it is `10.0 + (i % 7) as f64`, exact integer arithmetic
/// with no libm anywhere near it, so nudging it would test a divergence that cannot occur.
pub fn wave_bars_one_ulp(n: usize) -> Vec<Bar> {
    /// The next larger `f64`. Every fixture price is positive and finite, so incrementing the bit
    /// pattern is exactly one ULP up.
    fn up(v: f64) -> f64 {
        f64::from_bits(v.to_bits() + 1)
    }
    wave_bars(n)
        .into_iter()
        .map(|b| Bar { o: up(b.o), h: up(b.h), l: up(b.l), c: up(b.c), ..b })
        .collect()
}

/// A wave series scaled to a chosen price magnitude, on the SAME one-minute ot grid as
/// [`wave_bars`] so a compare reindexes cleanly onto the primary's index domain.
pub fn wave_bars_scaled(n: usize, scale: f64) -> Vec<Bar> {
    wave_bars(n)
        .into_iter()
        .map(|b| Bar { o: b.o * scale, h: b.h * scale, l: b.l * scale, c: b.c * scale, ..b })
        .collect()
}

/// [`wave_bars`] with every open-time shifted FORWARD by `ms`, prices untouched — a compare symbol
/// whose history begins only after the primary's ends. ⚠ The shift has to clear the primary's own
/// SPAN — `(n - 1) * 60_000` ms against a `wave_bars(n)` primary — and NOT its absolute last `ot`,
/// which is ~1.7e12 and would make every caller's constant look wrong: both fixtures start at the
/// same epoch, so only the width matters. `crates/vike-chart/src/render.rs`'s `reindex_by_ot` is a
/// strict floor/as-of join, so against an unshifted [`wave_bars`] primary every slot then resolves
/// `None` — the ALL-GAP compare window that TWO sites on this path each run an extremum fold
/// empty-handed for: `crates/vike-chart/src/chart/subpanes.rs`'s `draw_one_series_pane` (which
/// falls back to finite bounds) and `crates/vike-chart/src/chart/price_render.rs`'s
/// `compute_secondary_axis_lines` (which skips the overlay outright). The degeneracy is purely
/// temporal — any bar that DID align would be an ordinary wave price.
pub fn wave_bars_ot_shifted(n: usize, ms: i64) -> Vec<Bar> {
    wave_bars(n).into_iter().map(|b| Bar { ot: b.ot + ms, ..b }).collect()
}

/// Build a fully-closed [`ChartState`] from render bars (mirrors `benches/render.rs`): pin the
/// display timezone, set `bars` + `closed_len`, then `refresh_caches()` to seed the y-extent /
/// hour-mark / grid-step caches `draw` reads.
///
/// ⚠ **The `set_tz` call is load-bearing and is the reason this helper exists rather than a plain
/// struct literal.** `ChartState`'s tz defaults to [`DisplayTz::Local`] — `chrono::Local`, i.e. the
/// MACHINE's zone — and it feeds `vike_chart::hour_mark_indices` / `day_mark_indices` and every
/// x-axis tick label. Left at the default, a rendered frame differs between two machines in both
/// the label TEXT and the grid-line POSITIONS: not a subtle sub-pixel effect, a whole grid line
/// moving. That makes `tessellation_goldens.rs` unreproducible, and it makes every render test in
/// this crate quietly machine-dependent. Pinning to UTC here fixes both at the source. Measured:
/// running the golden suite under `TZ=Asia/Kolkata` against goldens generated under `TZ=UTC`
/// reddens without this line, and passes with it.
pub fn make_state(bars: Vec<Bar>) -> ChartState {
    let mut s = ChartState::default();
    s.set_tz(DisplayTz::Utc);
    let n = bars.len();
    s.bars = bars;
    s.closed_len = n;
    s.refresh_caches();
    s
}

/// A VPIN [`ActiveStudy`] warmed with one committed sample, self-keyed to `PaneKey::Study(uid)`.
/// Params are `bucket volume 10 / window 2 / aggressor flag`, so a single 10-unit trade fills a
/// bucket and commits a value — the study is neither gated nor all-NaN, i.e. it has something real
/// to draw.
pub fn warm_vpin(uid: u64, bars: usize) -> ActiveStudy {
    let spec = vike_chart::get_study("vpin").expect("vpin study registered");
    let mut s = ActiveStudy::with_params(uid, spec, vec![10.0, 2.0, 1.0]);
    s.on_trade(
        &vike_model::TradeTick {
            ts: 1,
            local_ts: 0,
            price: 100.0,
            size: 10.0,
            is_buyer_maker: false,
            symbol: String::new(),
        },
        None,
    );
    // Bar-index the series the way the app's per-frame call does: `closed_len` closed samples, no
    // forming tail (this fixture's `ChartState`s are fully closed).
    s.sync(bars, false);
    s
}

// ============================ headless harness ============================

/// One scripted `draw` invocation. Owns the immutable inputs; the per-frame mutable state
/// (`FollowLive`/`SettingsDialog`/`IndicatorDialog`/`PaneFractions`) is created fresh inside
/// [`Case::run`] so each scenario is independent, and persists across that scenario's frames.
pub struct Case<'a> {
    pub state: &'a ChartState,
    pub style: ChartStyle,
    pub options: ChartOptions,
    pub indicators: &'a [Active],
    /// Tick-driven microstructure studies (`ChartInputs::studies`). Empty in every
    /// pre-existing scenario, which is exactly the byte-identical default.
    pub studies: &'a [ActiveStudy],
    pub sub_panes: &'a [PaneKey],
    pub study_pane_of: &'a IndexMap<u64, PaneKey>,
    pub scale: ScaleMode,
    pub screen: egui::Rect,
    pub frames: usize,
    /// Pointer position (screen px), fed via `PointerMoved` on every frame. `None` = no pointer.
    pub pointer: Option<egui::Pos2>,
    /// Emit an `Event::Zoom` on the LAST frame (a zoom gesture) — used to script the
    /// interaction-detection path (`ChartActions::interacted`).
    pub zoom_last: bool,
}

impl<'a> Case<'a> {
    /// Minimal scenario: candles, default options, no indicators/studies, Linear scale, an
    /// 960×620 screen, two frames, no pointer, no zoom.
    pub fn new(state: &'a ChartState) -> Self {
        Case {
            state,
            style: ChartStyle::Candles,
            options: ChartOptions::default(),
            indicators: &[],
            studies: &[],
            sub_panes: &[],
            study_pane_of: EMPTY_STUDY_MAP.get_or_init(IndexMap::new),
            scale: ScaleMode::Linear,
            screen: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(960.0, 620.0)),
            frames: 2,
            pointer: None,
            zoom_last: false,
        }
    }

    /// Run the scenario and return the LAST frame's [`ChartActions`].
    pub fn run(&self) -> ChartActions {
        self.run_frames().0
    }

    /// Like [`Self::run`] but also returns the LAST frame's [`egui::FullOutput`], whose `shapes`
    /// hold every paint primitive `draw` emitted that frame — so a test can assert on the actual
    /// render output (e.g. the gradient-background mesh) rather than just [`ChartActions`].
    pub fn run_frames(&self) -> (ChartActions, egui::FullOutput) {
        let (acts, out, _ctx) = self.run_full();
        (acts, out)
    }

    /// The LAST frame, TESSELLATED — `Vec<ClippedPrimitive>`, the flat draw-call sequence a
    /// renderer would execute, which is what `vike_ui_theme::frame_record` records.
    ///
    /// Tessellation MUST happen on the same `Context` that ran the frames (its font atlas is only
    /// realized once a pass has run), which is why this cannot be a free function over a
    /// `FullOutput` and why [`Self::run_full`] hands the context back.
    pub fn run_tessellated(&self) -> (f32, Vec<egui::epaint::ClippedPrimitive>) {
        let (_acts, out, ctx) = self.run_full();
        let ppp = out.pixels_per_point;
        (ppp, ctx.tessellate(out.shapes, ppp))
    }

    /// One persistent `Context` across all frames (so the plot's stored bounds + prior-frame widget
    /// rects warm up so immediate-mode hover/interaction can register). Each frame is a
    /// `ctx.run_ui` pass — which does egui's begin/run/end internally and hands `draw` a
    /// full-viewport background `Ui` (the same shape as the window body `vike-app` renders it
    /// inside). The `Context` comes back so a caller can tessellate on it.
    pub fn run_full(&self) -> (ChartActions, egui::FullOutput, egui::Context) {
        let ctx = egui::Context::default();
        bind_chart_font_families(&ctx);
        let mut follow = FollowLive::default();
        let mut settings = SettingsDialog::default();
        let mut indicator_dialog = IndicatorDialog::default();
        let mut panes = PaneFractions::default();
        let empty_series_pane_of: IndexMap<String, PaneKey> = IndexMap::new();
        let empty_series_scale: IndexMap<String, ScaleAssign> = IndexMap::new();

        let mut result = ChartActions::default();
        let mut last_out: Option<egui::FullOutput> = None;
        for f in 0..self.frames {
            let mut raw = egui::RawInput {
                screen_rect: Some(self.screen),
                time: Some(f as f64 / 60.0),
                ..Default::default()
            };
            if let Some(p) = self.pointer {
                raw.events.push(egui::Event::PointerMoved(p));
            }
            if self.zoom_last && f + 1 == self.frames {
                // A zoom gesture over the (now-hovered) plot: `draw` reads `ctx.input(zoom_delta())`
                // and, when the plot response is hovered, sets `ChartActions::interacted`.
                raw.events.push(egui::Event::Zoom(1.1));
            }

            let mut out = ctx.run_ui(raw, |ui| {
                result = draw(
                    ui,
                    ChartInputs {
                        state: self.state,
                        style: self.style,
                        nav: None,
                        indicators: self.indicators,
                        studies: self.studies,
                        follow: &mut follow,
                        options: &self.options,
                        settings: &mut settings,
                        indicator_dialog: &mut indicator_dialog,
                        scale: self.scale,
                        invert: false,
                        panes: &mut panes,
                        sync: None,
                        footprint: None,
                        footprint_gen: 0,
                        cvd_on: false,
                        profile_on: false,
                        of_tick_size: 0.0,
                        sub_panes: self.sub_panes,
                        study_pane_of: self.study_pane_of,
                        overlays: &[],
                        series_panes: &[],
                        series_pane_of: &empty_series_pane_of,
                        series_scale: &empty_series_scale,
                        gpu_candles: None,
                    },
                );
            });
            // egui 0.36 made `TexturesDelta` PANIC on drop when it still holds unapplied deltas
            // ("Deltas need to be handled … call `clear` before dropping"). A real app hands them to
            // a renderer; this harness only inspects shapes and `ChartActions`. Clearing says
            // "deliberately not rendered" — which is what a headless characterization pass is.
            //
            // ⚠ It happens BEFORE the geometry assertion below, and the order is load-bearing.
            // `clear` touches `textures_delta` and never `shapes`, so it cannot hide anything the
            // assertion reads — while asserting FIRST leaves the deltas unapplied, so dropping the
            // frame during the assertion's unwind panics a SECOND time and the process ABORTS:
            // measured on a planted NaN, all 18 scenarios died with `SIGABRT` / "panic in a
            // destructor during cleanup" and never printed which coordinate was wrong.
            out.textures_delta.clear();
            // Every frame this harness renders is also a GEOMETRY test — the one shared invariant
            // (`vike-ui-theme`, `test-support`). This is what turns a scenario from "draw did not
            // panic" into "draw emitted paintable shapes". `tessellation_goldens.rs` is the rung
            // above it: WHERE, in what ORDER, under which CLIP.
            vike_ui_theme::frame_sanity::assert_frame_sane(&out);
            last_out = Some(out);
        }
        let last = last_out.expect("scenario runs at least one frame");
        (result, last, ctx)
    }
}

/// `chart::draw` renders some labels in the custom `"light"` / `"semibold"` font families that
/// `vike-app` registers at startup; a bare headless `Context` lacks them and epaint PANICS on first
/// use ("FontFamily::Name(..) is not bound to any fonts"). Bind both names to the default
/// proportional font so text layout succeeds — the app's real font environment, reproduced for the
/// test. Purely a rendering prerequisite: it changes no `ChartActions` value.
///
/// ⚠ For `tessellation_goldens.rs` this is also the whole reason TEXT GEOMETRY is reproducible.
/// `egui::FontDefinitions::default()` is EMBEDDED font data (Ubuntu-Light / Hack / emoji fonts
/// compiled into the binary), not a system-font lookup — unlike `vike-app`'s `install_fonts`, which
/// probes the OS and falls back silently, and which no test calls. Text layout is therefore the
/// same pure-Rust `ab_glyph` arithmetic on every machine.
pub fn bind_chart_font_families(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default FontDefinitions always define the Proportional family");
    for name in ["light", "semibold"] {
        fonts.families.insert(egui::FontFamily::Name(name.into()), proportional.clone());
    }
    ctx.set_fonts(fonts);
}

// A single shared empty `study_pane_of` map so `Case::new` can hand out a `&'static` reference
// without every caller declaring its own local. `OnceLock` because a plain `const`/`static`
// `IndexMap::new()` isn't const-constructible here.
static EMPTY_STUDY_MAP: std::sync::OnceLock<IndexMap<u64, PaneKey>> = std::sync::OnceLock::new();

/// A point inside the price pane's plot area: 45% across (left of the ~84px right price-axis
/// gutter), 28% down (above the bottom nav/scale overlay rows, and within the price pane even when
/// volume + a study pane split off the lower ~40%).
pub fn price_pane_point(screen: egui::Rect) -> egui::Pos2 {
    egui::pos2(screen.min.x + screen.width() * 0.45, screen.min.y + screen.height() * 0.28)
}
