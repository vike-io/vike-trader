//! [`assert_pixels_live`] — the shared NON-GOLDEN pixel-liveness assertion for the two
//! `png-export` offscreen harnesses (`crates/vike-chart/examples/export_png.rs`'s `rasterize` and
//! `crates/vike-studio/examples/studio_shot.rs`'s `rasterize`, whose readbacks are the only RGBA
//! buffers this repo produces headlessly).
//!
//! WHY THIS EXISTS: `scripts/ci_feature_suite.sh`'s `png-export` arm RUNS both harnesses on the
//! CI runners' Mesa lavapipe software rasterizer, and before this module the only thing anything
//! asserted about the rendered image was that a non-empty file landed on disk. That gates "did
//! not crash" and nothing more: a harness that painted a fully-black frame exits 0, saves a
//! perfectly valid PNG, and the lane stays green. The gap is not hypothetical — it is the SILENT
//! half of the 2026-08-16 texture-delta incident. The half that panicked (dropping a
//! `TexturesDelta` that still held entries) is what turned the lane from check-only into run;
//! the half that panics nothing sits one lost loop away in the same harness code: fail to apply
//! the accumulated deltas and egui-wgpu 0.36's `Renderer::render` finds no texture for the font
//! atlas, logs `Missing texture` at `warn`, and draws NOTHING for that mesh — and since even
//! solid fills sample `WHITE_UV` inside the atlas, the whole frame collapses to the render
//! pass's clear colour. Exit 0, valid PNG, one colour, green lane. This module is what reddens.
//!
//! It lives HERE for the reason [`crate::frame_sanity`] does: `vike-ui-theme` is layer 70, both
//! harness crates already sit above it (vike-chart as a normal dep, vike-studio as the
//! `test-support` dev-dep its shell-render tests take), and a law spelled twice is the defect
//! class this crate exists to prevent. Behind the same `test-support` feature — examples are dev
//! targets, so the dev-dependency's feature enable reaches them — and it needs no dependency at
//! all: the input is a raw RGBA byte buffer, deliberately, so the `image` crate stays where it
//! is (in the harnesses) and this crate's dependency surface stays `egui` alone.
//!
//! # The three measurements, and why they are REBASELINE-FREE
//!
//! Everything here is a floor or a cap with an ORDER-OF-MAGNITUDE gap between a healthy frame
//! and the dead frame it exists to catch — no golden image, no per-pixel expectation, nothing a
//! legitimate UI change could move past a threshold. That gap is also what makes the checks hold
//! on BOTH rasterizers that run the harnesses (lavapipe in CI, DX12/WARP or a real driver on the
//! dev box): the two genuinely differ in AA coverage arithmetic, which shifts WHICH blend
//! colours appear and moves the distinct-colour count by small amounts — noise that a margin of
//! 10x absorbs and a tuned-to-one-box threshold would not.
//!
//! 1. **Distinct-colour floor** ([`LivenessSpec::min_distinct_colors`]). A dead frame measures 1;
//!    a panels-only skeleton measures tens; any real scene with anti-aliased text measures
//!    hundreds to thousands (each glyph edge blends foreground over background through dozens of
//!    coverage levels). The per-scene floor sits far under healthy and far over dead.
//! 2. **Dominant-share cap** ([`LivenessSpec::max_dominant_share`]). The complement, for frames
//!    that keep a sliver of variety while one colour eats everything: a dead frame's dominant
//!    colour covers 100.00% by definition, a healthy UI's biggest flat fill stays near 90%.
//! 3. **Per-quadrant distinct floor** ([`LivenessSpec::min_quadrant_distinct`]), OPT-IN per
//!    scene. "Something was drawn in every quarter of the image" — which is TRUE of the chart
//!    scene (the plot spans the frame; every quadrant carries gridlines, candles or volume bars,
//!    and axis text) and FALSE-FIRE-PRONE for the Studio poses (a deliberately mostly-flat
//!    central panel, panel fills that may legitimately share one colour, and panel seams whose
//!    quadrant is a layout accident). `Some` where the scene supports it, `None` where it does
//!    not; each harness argues its own choice beside its own spec.
//!
//! The thresholds themselves live AT THE CALL SITES, not here: a floor is a fact about a scene
//! (how many candles, how much text), and this crate knows nothing about scenes. What lives here
//! is the measurement and the verdict machinery, exactly once. The kill proofs live in
//! `crates/vike-chart/tests/pixel_liveness_gate.rs`, in a consumer crate for the reason
//! `crates/vike-chart/tests/frame_sanity_gate.rs` states in its own header.
//!
//! # What was considered and deliberately NOT shipped
//!
//! - **Golden-image comparison, at any tolerance.** The rebaseline treadmill is the thing this
//!   module exists to avoid; the golden rung already exists as TEXT over tessellation
//!   ([`crate::frame_record`], GPU-free, byte-stable) and as a HUMAN over real pixels
//!   (`just qa-shots` on the dev box). Between those two rungs the only honest machine question
//!   about rasterized pixels is "is anything alive in here" — so that is the only question asked.
//! - **Per-colour expectations** ("the chart background must be this exact value"). Palette
//!   facts are pinned once, in [`crate::palette`]'s tests and the frame-record colour runs;
//!   restating one here as a pixel predicate would be the two-homes drift this crate exists to
//!   end — and sRGB round-tripping through a framebuffer is exactly where two rasterizers may
//!   disagree by one ULP per channel.
//! - **Mean-luminance / entropy floors.** Both collapse to the distinct-colour and
//!   dominant-share pair for every failure mode anyone could name, while being harder to argue
//!   margins for across rasterizers. Two simple numbers with wide gaps beat one clever number
//!   with a narrow one.

use std::collections::{HashMap, HashSet};
use std::fmt;

/// Quadrant display names, indexed exactly as [`PixelReport::quadrant_distinct`]:
/// `[top-left, top-right, bottom-left, bottom-right]`.
pub const QUADRANT_NAMES: [&str; 4] = ["top-left", "top-right", "bottom-left", "bottom-right"];

/// The per-scene liveness floors one harness asserts. Constructed `const` at each call site,
/// beside the scene whose facts justify the numbers — see the module docs for why the values do
/// not live here.
#[derive(Debug, Clone)]
pub struct LivenessSpec {
    /// The frame must carry at least this many distinct RGBA values. A dead frame carries 1.
    pub min_distinct_colors: usize,
    /// No single RGBA value may cover more than this fraction of the pixels. A dead frame's
    /// dominant colour covers 1.0.
    pub max_dominant_share: f64,
    /// If `Some(n)`, each of the four image quadrants must carry at least `n` distinct RGBA
    /// values. Opt-in: only for scenes whose REAL output puts content in every quadrant.
    pub min_quadrant_distinct: Option<usize>,
}

impl LivenessSpec {
    /// Every way `report` fails this spec, one human-readable finding per check — empty means
    /// live. Exposed separately from [`assert_pixels_live`] so a test (or a curious harness) can
    /// inspect the verdict without panicking.
    #[must_use]
    pub fn violations(&self, report: &PixelReport) -> Vec<String> {
        let mut v = Vec::new();
        if report.distinct_colors < self.min_distinct_colors {
            v.push(format!(
                "distinct-colour floor: {} distinct colours < {} — a (near-)blank frame",
                report.distinct_colors, self.min_distinct_colors
            ));
        }
        if report.dominant_share > self.max_dominant_share {
            v.push(format!(
                "dominant-share cap: {} covers {:.2}% > cap {:.2}% — one colour ate the frame",
                hex(report.dominant_color),
                report.dominant_share * 100.0,
                self.max_dominant_share * 100.0
            ));
        }
        if let Some(floor) = self.min_quadrant_distinct {
            for (name, &distinct) in QUADRANT_NAMES.iter().zip(&report.quadrant_distinct) {
                if distinct < floor {
                    v.push(format!(
                        "quadrant floor: {name} has {distinct} distinct colours < {floor}"
                    ));
                }
            }
        }
        v
    }
}

/// What one readback's pixels measured — the facts [`LivenessSpec::violations`] judges.
#[derive(Debug, Clone)]
pub struct PixelReport {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Distinct RGBA values across the whole image.
    pub distinct_colors: usize,
    /// The most frequent RGBA value (ties broken toward the numerically larger value, so the
    /// report is deterministic — `HashMap` iteration order is not).
    pub dominant_color: [u8; 4],
    /// Fraction of all pixels carrying [`Self::dominant_color`], in `0.0..=1.0`.
    pub dominant_share: f64,
    /// Distinct RGBA values per quadrant, indexed as [`QUADRANT_NAMES`]. Pixels on the exact
    /// half-way column/row belong to the right/bottom side.
    pub quadrant_distinct: [usize; 4],
}

impl fmt::Display for PixelReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}x{} px | {} distinct colours | dominant {} at {:.2}% | quadrants TL/TR/BL/BR {:?}",
            self.width,
            self.height,
            self.distinct_colors,
            hex(self.dominant_color),
            self.dominant_share * 100.0,
            self.quadrant_distinct,
        )
    }
}

/// Measure one RGBA buffer. Pure and total over well-formed input; the two panics below are
/// harness WIRING bugs (a buffer that disagrees with its own dimensions), not verdicts.
///
/// # Panics
///
/// If `width * height == 0`, or `rgba.len() != width * height * 4`.
#[must_use]
pub fn pixel_report(rgba: &[u8], width: u32, height: u32) -> PixelReport {
    let px = width as usize * height as usize;
    assert!(px > 0, "pixel_report: zero-sized image ({width}x{height}) — harness wiring bug");
    assert_eq!(
        rgba.len(),
        px * 4,
        "pixel_report: buffer is {} bytes but {width}x{height} RGBA needs {} — harness wiring bug",
        rgba.len(),
        px * 4
    );

    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut quads: [HashSet<u32>; 4] =
        [HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new()];
    let (half_w, half_h) = (width / 2, height / 2);
    for y in 0..height {
        for x in 0..width {
            let i = (y as usize * width as usize + x as usize) * 4;
            let c = u32::from_be_bytes([rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]);
            *counts.entry(c).or_insert(0) += 1;
            quads[usize::from(y >= half_h) * 2 + usize::from(x >= half_w)].insert(c);
        }
    }
    let (dominant, n) = counts
        .iter()
        .map(|(&c, &n)| (c, n))
        .max_by_key(|&(c, n)| (n, c))
        .expect("px > 0, so at least one colour was counted");
    PixelReport {
        width,
        height,
        distinct_colors: counts.len(),
        dominant_color: dominant.to_be_bytes(),
        dominant_share: f64::from(n) / px as f64,
        quadrant_distinct: [quads[0].len(), quads[1].len(), quads[2].len(), quads[3].len()],
    }
}

/// Assert one measured readback is live under the calling scene's spec.
///
/// # Panics
///
/// If any check in `spec` fails, naming every failed check with its measured value — the message
/// is the diagnostic, because in the lane that runs the harnesses a panic IS the red.
#[track_caller]
pub fn assert_pixels_live(report: &PixelReport, spec: &LivenessSpec) {
    let violations = spec.violations(report);
    assert!(
        violations.is_empty(),
        "rendered pixels are not live ({report})\n  {}",
        violations.join("\n  ")
    );
}

fn hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
}
