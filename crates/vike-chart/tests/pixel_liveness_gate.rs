//! The kill proofs for [`vike_ui_theme::pixel_liveness`] — the shared non-golden pixel-liveness
//! assertion the two `png-export` offscreen harnesses (`crates/vike-chart/examples/export_png.rs`'s
//! `main` and `crates/vike-studio/examples/studio_shot.rs`'s `main`) call on the RGBA buffer they
//! just rendered and saved.
//!
//! WHY IT LIVES IN A CONSUMER CRATE rather than beside the helper: the same argument
//! `frame_sanity_gate.rs` states in its own header — the helper is compiled only behind
//! `vike-ui-theme`'s `test-support` feature, `vike-ui-theme` has no CI feature lane of its own,
//! and `vike-chart`'s dev-dependency turns the feature on inside the DERIVED CI roster, so these
//! proofs run in the ordinary `just test` / CI fast lane with no new lane bought.
//!
//! WHY THEY EXIST AT ALL: this repo's standard is that a gate which has not been shown to fail on
//! the defect it exists for is decoration. Every check the spec ships is provoked here on a
//! synthetic buffer built to fail exactly it — a uniform "black pane" (the frame an un-uploaded
//! font atlas paints: egui-wgpu skips every mesh whose texture is missing, and even solid fills
//! sample the atlas), a two-colour skeleton, a one-colour wash with token variety, and a frame
//! with one dead quadrant — plus the acceptance half: a noisy buffer passes, and the dead-quadrant
//! frame that reddens a chart-shaped spec is ACCEPTED by a studio-shaped one, pinning that the
//! quadrant floor is per-scene opt-in rather than doctrine (the studio poses have a mostly-flat
//! central panel by design; its harness argues that beside its spec).
//!
//! The specs used here exercise the MECHANISM with small round numbers. The shipped scene floors
//! are deliberately NOT restated in this file — they live beside the scenes whose facts justify
//! them (`crates/vike-chart/examples/export_png.rs`'s `LIVENESS` and
//! `crates/vike-studio/examples/studio_shot.rs`'s `LIVENESS`), a second copy here would drift, and
//! no unit test can measure the real scenes anyway: the `png-export` lane runs them.
//!
//! A synthetic buffer proves the FUNCTION reddens; the harnesses REACHING it is compile-pinned by
//! the call sites themselves (the lane builds and runs both examples), so no by-hand planted-defect
//! measurement was needed here, unlike `frame_sanity_gate.rs`'s 32-test sweep.

use vike_ui_theme::pixel_liveness::{
    assert_pixels_live, pixel_report, LivenessSpec, QUADRANT_NAMES,
};

/// A chart-shaped spec: all three checks armed. Small round MECHANISM numbers (see module docs).
const QUADRANTED: LivenessSpec = LivenessSpec {
    min_distinct_colors: 8,
    max_dominant_share: 0.9,
    min_quadrant_distinct: Some(2),
};

/// A studio-shaped spec: the same global floors with the quadrant floor deliberately off.
const GLOBAL_ONLY: LivenessSpec =
    LivenessSpec { min_distinct_colors: 8, max_dominant_share: 0.9, min_quadrant_distinct: None };

/// One flat colour everywhere — the black-pane frame this whole module exists for.
fn uniform(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat((w * h) as usize)
}

/// Deterministic per-pixel colour noise (xorshift32, alpha pinned to 0xFF like a real readback).
/// No `rand` dep: the sequence only needs to be varied and reproducible, not random.
fn noisy(w: u32, h: u32) -> Vec<u8> {
    let mut s: u32 = 0x9E37_79B9;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        let [r, g, b, _] = s.to_be_bytes();
        out.extend_from_slice(&[r, g, b, 0xFF]);
    }
    out
}

fn set_px(buf: &mut [u8], w: u32, x: u32, y: u32, rgba: [u8; 4]) {
    let i = ((y * w + x) * 4) as usize;
    buf[i..i + 4].copy_from_slice(&rgba);
}

// ============================ the baseline ============================

#[test]
fn a_noisy_frame_passes_every_check() {
    // The acceptance half: a frame with per-pixel variety clears all three checks with the same
    // margins a real rendered scene does (a healthy harness frame has MORE structure than this,
    // not less — AA text alone gives hundreds of distinct colours).
    let buf = noisy(64, 64);
    let report = pixel_report(&buf, 64, 64);
    assert!(report.distinct_colors > 1000, "xorshift noise must be varied: {report}");
    assert!(report.dominant_share < 0.01, "no colour may dominate noise: {report}");
    assert_pixels_live(&report, &QUADRANTED);
    assert_pixels_live(&report, &GLOBAL_ONLY);
}

#[test]
fn the_measurement_itself_is_pinned_on_a_hand_computable_buffer() {
    // 4x2, quadrants split at x=2 / y=1: row 0 = A A B B, row 1 = C C D D. Guards the failure
    // mode that would make every proof below meaningless — a report that miscounts would pass
    // some spec for the wrong reason.
    let (a, b, c, d) = ([1, 0, 0, 255], [0, 2, 0, 255], [0, 0, 3, 255], [4, 4, 4, 255]);
    let mut buf = Vec::new();
    for px in [a, a, b, b, c, c, d, d] {
        buf.extend_from_slice(&px);
    }
    let report = pixel_report(&buf, 4, 2);
    assert_eq!((report.width, report.height), (4, 2));
    assert_eq!(report.distinct_colors, 4);
    assert_eq!(report.quadrant_distinct, [1, 1, 1, 1]);
    // Four-way tie at 2 pixels each: the deterministic tie-break picks the numerically largest
    // RGBA value, which is D (0x040404ff tops 0x010000ff / 0x000200ff / 0x000003ff).
    assert_eq!(report.dominant_color, d);
    assert!((report.dominant_share - 0.25).abs() < 1e-12, "2 of 8 pixels: {report}");
}

#[test]
fn the_quadrant_indexing_matches_the_names_the_diagnostic_prints() {
    // Plant a second colour ONLY in the top-right quadrant and require index 1 — the index
    // QUADRANT_NAMES calls "top-right" — to be the one that moves. A transposed quadrant index
    // would send a triager to the wrong quarter of the image.
    let mut buf = uniform(6, 4, [9, 9, 9, 255]);
    set_px(&mut buf, 6, 5, 0, [200, 0, 0, 255]);
    let report = pixel_report(&buf, 6, 4);
    assert_eq!(report.quadrant_distinct, [1, 2, 1, 1]);
    assert_eq!(QUADRANT_NAMES[1], "top-right");
}

// ============================ the kill proofs ============================

#[test]
fn a_uniform_frame_reddens_on_every_armed_check() {
    let buf = uniform(64, 64, [0, 0, 0, 255]);
    let report = pixel_report(&buf, 64, 64);
    let violations = QUADRANTED.violations(&report);
    assert_eq!(
        violations.len(),
        6,
        "distinct floor + dominant cap + all four quadrants must fire: {violations:#?}"
    );
    assert!(violations[0].contains("distinct-colour floor"), "{violations:#?}");
    assert!(violations[1].contains("dominant-share cap"), "{violations:#?}");
    for name in QUADRANT_NAMES {
        assert!(
            violations.iter().any(|v| v.contains("quadrant floor") && v.contains(name)),
            "the {name} quadrant must be named: {violations:#?}"
        );
    }
}

#[test]
#[should_panic(expected = "rendered pixels are not live")]
fn a_uniform_frame_panics_through_the_assert_entry_point() {
    let buf = uniform(64, 64, [0, 0, 0, 255]);
    assert_pixels_live(&pixel_report(&buf, 64, 64), &GLOBAL_ONLY);
}

#[test]
fn a_two_colour_frame_reddens_on_the_distinct_floor_even_though_nothing_dominates() {
    // The decision this test pins: a 50/50 two-colour frame has NO dominant colour, yet it is a
    // skeleton, not a live scene — the distinct floor is what catches it, so the floor must stay
    // above 2 in every shipped spec.
    let mut buf = uniform(64, 64, [10, 10, 10, 255]);
    for y in 0..64 {
        for x in 0..32 {
            set_px(&mut buf, 64, x, y, [240, 240, 240, 255]);
        }
    }
    let report = pixel_report(&buf, 64, 64);
    assert!((report.dominant_share - 0.5).abs() < 1e-12, "an exact split: {report}");
    let violations = GLOBAL_ONLY.violations(&report);
    assert_eq!(violations.len(), 1, "{violations:#?}");
    assert!(violations[0].contains("distinct-colour floor"), "{violations:#?}");
}

#[test]
fn a_one_colour_wash_reddens_on_the_dominant_cap_even_with_token_variety() {
    // Enough distinct colours to clear the floor, all crowded into one corner while one colour
    // covers ~99.6% of the frame — the check the distinct floor alone cannot make.
    let mut buf = uniform(64, 64, [5, 5, 5, 255]);
    for x in 0..16u32 {
        set_px(&mut buf, 64, x, 0, [x as u8 + 1, 0, 0, 255]);
    }
    let report = pixel_report(&buf, 64, 64);
    assert_eq!(report.distinct_colors, 17, "16 planted + the wash: {report}");
    let violations = GLOBAL_ONLY.violations(&report);
    assert_eq!(violations.len(), 1, "{violations:#?}");
    assert!(violations[0].contains("dominant-share cap"), "{violations:#?}");
}

#[test]
fn a_dead_quadrant_reddens_a_quadranted_spec_and_passes_a_global_only_one() {
    // Three quadrants of noise, a flat bottom-right — the render-stopped-partway frame. The
    // chart-shaped spec must redden NAMING the dead quadrant; the studio-shaped spec must accept
    // the very same buffer, because its scene legitimately keeps a flat region (the mostly-empty
    // central panel) and its harness disarms the quadrant floor for exactly that reason.
    let mut buf = noisy(64, 64);
    for y in 32..64 {
        for x in 32..64 {
            set_px(&mut buf, 64, x, y, [7, 7, 7, 255]);
        }
    }
    let report = pixel_report(&buf, 64, 64);
    let violations = QUADRANTED.violations(&report);
    assert_eq!(violations.len(), 1, "{violations:#?}");
    assert!(
        violations[0].contains("quadrant floor") && violations[0].contains("bottom-right"),
        "{violations:#?}"
    );
    assert_pixels_live(&report, &GLOBAL_ONLY);
}

// ============================ wiring bugs are loud ============================

#[test]
#[should_panic(expected = "harness wiring bug")]
fn a_buffer_that_disagrees_with_its_dimensions_is_a_wiring_bug_not_a_verdict() {
    let buf = uniform(4, 4, [0, 0, 0, 255]);
    let _ = pixel_report(&buf, 8, 8);
}

#[test]
#[should_panic(expected = "harness wiring bug")]
fn a_zero_sized_image_is_a_wiring_bug_not_a_verdict() {
    let _ = pixel_report(&[], 0, 0);
}
