//! **A grid multiple must survive the round-then-format chain.** This is the COMPOSITIONAL law
//! `crates/vike-bridge-core/tests/format_props.rs` does not state: snap a value onto the venue's
//! grid, format it for the wire, and ask whether the multiple you asked for is the multiple that
//! went out.
//!
//! ⚠ **This file was committed RED and is now GREEN, and both halves of that are the point.** It
//! was written as a characterization test — it failed on a defect that was in `main`, and it
//! passes under the cure that landed in #1562: `crates/vike-bridge-core/src/format.rs`'s
//! `grid_multiple_image`, a bit-exact WITNESS rather than a tolerance. (An earlier version of this
//! header pointed at a `ULP-STEP-LOSS-FIX.md` design note at the repo root, which was scratch and
//! is not in the tree; the cure's own doc comment carries the argument now, and #1562's message
//! carries the four candidates and why three were rejected.)
//!
//! **Do not weaken this file to make it green** — it is green on its own terms. If it ever reddens
//! again, the quantizer moved, and the answer is in the quantizer.
//!
//! # The defect
//!
//! `crates/vike-model/src/scalar.rs`'s `round_to_step` ends in a MULTIPLICATION —
//! `(value / step).round_ties_even() * step` — and `n as f64 * step` is not always the f64
//! nearest the decimal grid point `n·step`. When the step's own f64 image sits below its decimal
//! value, the product can land ONE f64 ULP below the grid point's float. Its shortest-round-trip
//! decimal image is then strictly below `n·step`, and `crates/vike-bridge-core/src/format.rs`'s
//! `quantize_to_step` — which computes `(value / step).trunc() * step` in exact `Decimal` — reads
//! a ratio of `n - ε` and truncates it to `n - 1`.
//!
//! **The error is not one ULP. It is a WHOLE STEP.** On the wire that is a whole tick off a limit
//! price, or a whole lot off an order size, on a live order. The chain is the production one:
//! `crates/vike-exec/src/risk.rs`'s `check_inner` calls `round_to(request.qty, grid.lot_size)` and
//! writes the result into the outgoing request, and that value reaches `format_to_step_f` in
//! `crates/bridges/binance/src/family/order_map.rs`, `crates/bridges/bybit/src/perp.rs`,
//! `crates/bridges/aster/src/perp.rs`, `crates/bridges/okx/src/perp.rs` and
//! `crates/bridges/deribit/src/client.rs`.
//!
//! # MEASURED, 2026-08-28, Windows/MSVC — 2000 grid multiples per step
//!
//! Driven through the real chain (`round_to_step` then `format_to_step_f`, and separately the
//! string entry point `format_to_step`). Both entry points lost the SAME multiples:
//!
//! ```text
//! step     lost/2000  first n
//! 1e-8       0
//! 1e-7     578 (28.9%)   n=13
//! 1e-6     602 (30.1%)   n=5
//! 1e-5       0
//! 1e-4       0
//! 1e-3       0
//! 1e-2       0
//! 1e-1       0
//! 5e-1       0
//! 1e0        0
//! ```
//!
//! The worked example: `5 · fl(1e-6)` is `0.0000049999999999999996`, whose wire string is
//! `"0.000004"` — a lot short.
//!
//! # Why the ladder runs 1e-8 to 1e0, and what the ULP column is for
//!
//! "1e-8 is clean and 1e-7 is not" is the kind of claim that becomes folklore the moment it is
//! written down without its evidence, so this file does not assert it — it MEASURES it, every
//! run, for every rung. The last column of the report is a histogram of
//! `grid_float.to_bits() - value.to_bits()`: how many f64 ULPs BELOW the grid point's own float
//! the chain's output landed, over the whole sweep. A rung whose histogram is `{0: 2000}` cannot
//! lose a step (the value IS the grid point's float, so its shortest decimal image IS `n·step`); a
//! rung with a `1:` bucket is the rung where the multiplication drifted, and the loss count tracks
//! that bucket. **The boundary is therefore a recorded number rather than a story about which
//! powers of ten round which way.**
//!
//! The rungs are the ten in the table above and no others, deliberately: the printed table is
//! meant to be comparable line-for-line with the recording. The non-power-of-ten venue grids (0.5
//! is here; 0.25 / 0.05 / 2.5 / 0.0005 are not) ride `format_props.rs`, which is where a
//! randomized law belongs — this file is the pinned ladder.
//!
//! # What the comparison is
//!
//! Every assertion is an EXACT `Decimal` comparison with no tolerance, for the reason
//! `format_props.rs` gives: every step here is a terminating decimal, so the quantizer's internal
//! division is exact and a moved output is a genuine wire change rather than a rounding artifact.
//! Both sides are rescaled to one common scale first ([`CMP_SCALE`]) so that the two entry points'
//! different step SPELLINGS — `format_to_step_f(v, 1.0)` goes through `py_f64_str`, which yields
//! `"1.0"` (scale 1), while the string entry point here is spelled `"1"` (scale 0) — compare on
//! value rather than on trailing zeros.

use std::collections::BTreeMap;

use rust_decimal::prelude::*;
use vike_bridge_core::format::{format_to_step, format_to_step_f};
use vike_model::round_to_step;

/// Grid multiples swept per rung: `n` in `1..=MULTIPLES`.
///
/// 2000 is the recording's own sample count, kept so the printed table can be read straight
/// against the module doc's. It is also well past the first loss on every rung that has one (the
/// deepest recorded first-loss is `n=13`).
const MULTIPLES: i64 = 2000;

/// The common scale both sides are rescaled to before comparing.
///
/// Derived, not picked: the deepest step on the ladder is `1e-8` (scale 8), every emitted string
/// carries at most its step's scale (`format_props.rs`'s law 2), and `rust_decimal` caps scale at
/// 28. Anything in `8..=28` is exact for both sides — rescaling UP only appends zeros — and 12
/// leaves headroom for a deeper rung without approaching the cap.
const CMP_SCALE: u32 = 12;

/// The step ladder, in BOTH spellings the two entry points take.
///
/// The pairing is not redundant, and `crates/vike-bridge-core/tests/wire_quantizer_probe.rs`'s
/// `STEPS` carries the same one for the same reason: `format_to_step` takes the STRING and
/// `format_to_step_f` takes the f64 and routes it through `py_f64_str`, so the two can disagree
/// and covering one would leave the other's `str(float)` shim untested.
const LADDER: [(&str, f64); 10] = [
    ("0.00000001", 1e-8),
    ("0.0000001", 1e-7),
    ("0.000001", 1e-6),
    ("0.00001", 1e-5),
    ("0.0001", 1e-4),
    ("0.001", 1e-3),
    ("0.01", 1e-2),
    ("0.1", 1e-1),
    ("0.5", 5e-1),
    ("1", 1e0),
];

/// Both sides of every comparison go through here, so a scale difference can never be mistaken for
/// a value difference. See [`CMP_SCALE`].
fn at_cmp_scale(mut d: Decimal) -> Decimal {
    d.rescale(CMP_SCALE);
    d
}

/// THE LAW: for every rung of the ladder and every multiple `n`, the wire string of
/// `round_to_step(n · step, step)` is the exact decimal `n · step`.
///
/// Fails today. The failure message carries the whole measured table — loss counts for both entry
/// points, the first losing multiple, and the ULP histogram that locates the boundary — so one red
/// run is the evidence rather than a starting point for one.
#[test]
fn a_grid_multiple_survives_round_to_step_then_the_wire_format() {
    let mut report = String::new();
    report.push_str("\nA multiple asked for on the grid did not come back off the wire.\n");
    report.push_str("Each lost row is a WHOLE step short on a live order - a tick off a limit\n");
    report.push_str("price, a lot off a size - never one ULP. This file's module doc has the\n");
    report.push_str("mechanism; format.rs's grid_multiple_image is the cure that landed.\n\n");
    report.push_str("step           f64-step   str-step  first n  grid minus value (f64 ULPs)\n");

    let mut total_lost = 0usize;
    for (step_str, step_f) in LADDER {
        let step_d = Decimal::from_str(step_str).expect("every ladder step parses as a Decimal");
        let mut lost_f = 0usize;
        let mut lost_s = 0usize;
        let mut first_n: Option<i64> = None;
        let mut ulps: BTreeMap<i64, usize> = BTreeMap::new();

        for n in 1..=MULTIPLES {
            // The REAL chain: the snap `vike_mm::avellaneda`'s `snap_to_tick` and
            // `vike_exec::risk`'s `round_to` both delegate to, then the venue's wire format.
            let v = round_to_step(n as f64 * step_f, step_f);
            let want = at_cmp_scale(Decimal::from(n) * step_d);

            // Where the chain's output sits relative to the grid point's OWN nearest float. This
            // is the boundary evidence: a rung that only ever lands on it cannot lose a step.
            let grid_f = want.to_string().parse::<f64>().expect("a plain decimal parses as f64");
            let away = grid_f.to_bits() as i64 - v.to_bits() as i64;
            *ulps.entry(away).or_insert(0) += 1;

            let out_f = format_to_step_f(v, step_f);
            let out_s = format_to_step(v, step_str);
            let dec_f = Decimal::from_str(&out_f).expect("the f64-step output is a decimal");
            let dec_s = Decimal::from_str(&out_s).expect("the str-step output is a decimal");
            let got_f = at_cmp_scale(dec_f);
            let got_s = at_cmp_scale(dec_s);

            if got_f != want {
                lost_f += 1;
            }
            if got_s != want {
                lost_s += 1;
            }
            if first_n.is_none() && (got_f != want || got_s != want) {
                first_n = Some(n);
            }
        }

        total_lost += lost_f + lost_s;
        let first = first_n.map_or_else(|| "-".to_string(), |n| n.to_string());
        let row = format!("{step_str:<12}  {lost_f:>9}  {lost_s:>9}  {first:>7}  {ulps:?}\n");
        report.push_str(&row);
    }

    println!("{report}");
    assert_eq!(total_lost, 0, "{report}");
}

/// The module doc's worked example, pinned on its own so a red run names ONE number rather than a
/// table: `5 · fl(1e-6)` is `0.0000049999999999999996`, and `quantize_to_step`'s truncation reads
/// its ratio as `4.9999999999999996` and emits four micro-lots where five were ordered.
#[test]
fn the_worked_example_reaches_the_wire_whole() {
    let step = 1e-6;
    let v = round_to_step(5.0 * step, step);
    let by_f64 = format_to_step_f(v, step);
    let by_str = format_to_step(v, "0.000001");
    assert_eq!(by_f64, "0.000005", "f64-step entry: round_to_step gave {v}");
    assert_eq!(by_str, "0.000005", "string entry: round_to_step gave {v}");
}

/// THE CONTROL, and it is what stops the law above from being satisfiable by "round everything
/// up".
///
/// A value the caller genuinely meant to be truncated — a real fractional quantity sitting
/// mid-step, nowhere near a grid point — must STILL truncate toward zero. Every expectation here
/// is a row of the frozen `fixtures/r6/format.json`, so this test passes today and must go on
/// passing after any fix: a cure that moves these has overshot, which is exactly what
/// `crates/vike-bridge-core/src/format.rs`'s header ("Quantize DOWN — never overshoot a limit")
/// forbids.
#[test]
fn a_genuine_mid_step_value_still_truncates() {
    assert_eq!(format_to_step(0.015, "0.01"), "0.01");
    assert_eq!(format_to_step(99999.99999999, "0.1"), "99999.9");
    assert_eq!(format_to_step(99999.99999999, "1"), "99999");
    assert_eq!(format_to_step(123.456789, "0.01"), "123.45");
    assert_eq!(format_to_step(123.456789, "0.05"), "123.45");
}
