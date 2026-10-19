//! The OUTPUT half: the platform pin as a gate, and the control arm for its non-vacuity guard.

use super::harness::{MIN_DISTINCT, SAMPLES, finish, lcg, or_nan};
use super::rows::{PLATFORM_INVARIANT, production_rows};

/// The cross-platform proof, as a GATE rather than a diff somebody has to remember to run.
///
/// NOT `#[ignore]`d: CI runs it on Linux every PR and `just t vike-options` runs it on the Windows
/// dev box, both against the same committed table.
#[test]
fn converted_functions_are_platform_invariant() {
    let got = production_rows();
    assert_eq!(
        got.len(),
        PLATFORM_INVARIANT.len(),
        "row count drifted from PLATFORM_INVARIANT — it must be re-recorded"
    );

    let mut bad = Vec::new();
    for (r, (want_label, want_hash)) in got.iter().zip(PLATFORM_INVARIANT.iter()) {
        assert_eq!(&r.label, want_label, "row ORDER drifted — the compare is positional");
        if r.hash != *want_hash {
            bad.push(format!("  {:<50} want={want_hash} got={}", r.label, r.hash));
        }
    }

    // Non-vacuity BEFORE the hashes: a thin row is a defect whether or not its hash matched, and an
    // author re-recording a `"RECORD"` row must be told about it before pasting.
    let thin: Vec<String> = got
        .iter()
        .filter(|r| r.distinct < MIN_DISTINCT)
        .map(|r| format!("  {:<50} {} distinct finite of {} samples", r.label, r.distinct, r.n))
        .collect();
    assert!(
        thin.is_empty(),
        "a pinned row's corpus is too thin to detect a last-bit divergence: it would hash \
         identically on every platform and read as a PASS. In THIS crate the usual cause is a \
         corpus that fell outside a function's domain — every public function here returns `None` \
         out of domain, and a wall of canonical NaNs is perfectly stable everywhere. WIDEN or \
         re-centre the inputs; never lower MIN_DISTINCT ({MIN_DISTINCT}):\n{}",
        thin.join("\n")
    );

    if !bad.is_empty() {
        let paste: String =
            got.iter().map(|r| format!("    (\"{}\", \"{}\"),\n", r.label, r.hash)).collect();
        panic!(
            "\nvike-options output moved away from the platform pin:\n{}\n\n\
             There is no tolerance here to widen, and hand-editing a digit defeats the gate.\n\
             ⚠ Do NOT reach for `oracle_parity.rs` to decide whether the move matters: its\n\
             erf/exp/log tier is 1e-12 RELATIVE, so a last-bit move passes it in silence. That is\n\
             why this file is a BIT comparison.\n\
             If the want= column reads RECORD this is an UNFINISHED RECORDING rather than a\n\
             regression: run this file on BOTH Windows and Linux, confirm they agree, paste.\n\
             Otherwise establish WHY first:\n\
               (a) an intended numeric change — read docs/decisions/0021 before re-recording, then\n\
                   re-record on BOTH boxes and pin the agreed hash; or\n\
               (b) a call site went back to `(…).exp()` / `(…).ln()`, in which case the two\n\
                   platforms will NOT agree and the fix is in the source, not here.\n\n\
             const PLATFORM_INVARIANT: [(&str, &str); {}] = [\n{paste}];\n",
            bad.join("\n"),
            got.len(),
        );
    }
}

/// The control arm for the non-vacuity threshold itself.
///
/// ⚠ Without this, `MIN_DISTINCT` is an assertion nobody has checked DISCRIMINATES. A `distinct`
/// computation that counted samples rather than distinct values — or one that counted NaNs — would
/// pass every row above while the guard did nothing. The NaN arm is not decoration here: it is the
/// exact shape a mis-centred corpus takes in this crate, because every public function returns
/// `None` out of domain and `or_nan` hashes that as a canonical NaN.
#[test]
fn the_non_vacuity_check_discriminates() {
    let constant: Vec<f64> = (0..SAMPLES).map(|_| 0.5f64).collect();
    assert!(
        finish("control::constant", &constant).distinct < MIN_DISTINCT,
        "the non-vacuity check does not discriminate: a constant series passed it"
    );
    let all_none: Vec<f64> = (0..SAMPLES).map(|_| or_nan(None)).collect();
    assert_eq!(
        finish("control::all-none", &all_none).distinct,
        0,
        "a corpus that fell entirely outside every domain must report ZERO distinct FINITE values"
    );
    let mut s = 0xdead_beef_cafe_babeu64;
    let rich: Vec<f64> = (0..SAMPLES).map(|_| lcg(&mut s)).collect();
    assert!(
        finish("control::rich", &rich).distinct >= MIN_DISTINCT,
        "the corpus generator itself is degenerate — every row above is void"
    );
}
