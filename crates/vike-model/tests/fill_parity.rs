//! R0/R1 golden gate: `compute_fill` against the FROZEN `fixtures/r0/compute_fill.json` bytes.
//! Every f64 is compared BIT-FOR-BIT (the fixture carries IEEE-754 hex bit patterns).
//!
//! ⚠ This read "`compute_fill` vs the Python oracle" until 2026-08-28, and the test below was
//! named `compute_fill_bit_parity_vs_python` for the same reason. Both described an ongoing
//! cross-implementation check that no longer exists. The fixture's PROVENANCE is unchanged — it
//! was exported from the Python app and `manifest.source_sha` pins the SHA that exported it — but
//! every exporter was deleted by `751de662`, so the committed bytes ARE the oracle: what the case
//! sweep below asserts is that `compute_fill` has not changed its arithmetic unnoticed.
//! `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` is the verdict.
//!
//! Unlike a libm-era pin
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`), these bits
//! are portable by construction rather than by luck: `crates/vike-model/src/fill.rs`'s
//! `compute_fill` reaches no transcendental and no `powi` at any arity, only `+ - * /`, which
//! IEEE 754 requires to be correctly rounded. Adding one would make this gate platform-sensitive
//! and nothing here would say so.

use serde::Deserialize;
use std::path::PathBuf;
use vike_model::{compute_fill, f64_from_hex_bits, f64_to_hex_bits, FillKind};

#[derive(Deserialize)]
struct Fixture {
    manifest: serde_json::Value,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    #[serde(rename = "in")]
    inp: In,
    out: Out,
}

#[derive(Deserialize)]
struct In {
    prior_size: String,
    prior_avg_px: String,
    side: i32,
    qty: String,
    price: String,
    multiplier: String,
}

#[derive(Deserialize)]
struct Out {
    kind: String,
    new_size: String,
    new_avg_px: String,
    closing_qty: String,
    entry_avg_px: String,
    realized_pnl: String,
    portion: String,
    leftover: String,
}

fn kind_str(k: FillKind) -> &'static str {
    match k {
        FillKind::Open => "open",
        FillKind::Add => "add",
        FillKind::Reduce => "reduce",
        FillKind::Flip => "flip",
        FillKind::Close => "close",
    }
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r0/compute_fill.json")
}

/// ⚠ RENAMED 2026-08-28 from `compute_fill_bit_parity_vs_python`, which claimed a comparison
/// against a Python implementation that no longer exists anywhere in this tree. Nothing outside
/// this file named the old spelling (checked across `crates/`, `docs/`, `scripts/`, `.github/`
/// and the `justfile`), so no reference was left dangling; a `-- compute_fill` filter still
/// selects it.
#[test]
fn compute_fill_bits_match_the_frozen_r0_fixture() {
    let text = std::fs::read_to_string(fixture_path())
        // ⚠ Not "run the exporter": scripts/export_r0_fixtures.py went with `751de662` and these
        // bytes cannot be regenerated. The dead path stays NAMED as the evidence for where they
        // came from — `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS` has a row.
        .expect(
            "fixtures/r0/compute_fill.json missing — exported by scripts/export_r0_fixtures.py",
        );
    let fx: Fixture = serde_json::from_str(&text).unwrap();
    assert!(fx.manifest.get("source_sha").is_some(), "fixture manifest must record the oracle SHA");
    assert!(fx.cases.len() >= 2000, "expected the full case sweep");

    for (i, case) in fx.cases.iter().enumerate() {
        let out = compute_fill(
            f64_from_hex_bits(&case.inp.prior_size).unwrap(),
            f64_from_hex_bits(&case.inp.prior_avg_px).unwrap(),
            case.inp.side,
            f64_from_hex_bits(&case.inp.qty).unwrap(),
            f64_from_hex_bits(&case.inp.price).unwrap(),
            f64_from_hex_bits(&case.inp.multiplier).unwrap(),
        );
        assert_eq!(kind_str(out.kind), case.out.kind, "case {i}: kind");
        let checks: [(&str, f64, &String); 7] = [
            ("new_size", out.new_size, &case.out.new_size),
            ("new_avg_px", out.new_avg_px, &case.out.new_avg_px),
            ("closing_qty", out.closing_qty, &case.out.closing_qty),
            ("entry_avg_px", out.entry_avg_px, &case.out.entry_avg_px),
            ("realized_pnl", out.realized_pnl, &case.out.realized_pnl),
            ("portion", out.portion, &case.out.portion),
            ("leftover", out.leftover, &case.out.leftover),
        ];
        for (name, got, want_hex) in checks {
            assert_eq!(
                &f64_to_hex_bits(got),
                want_hex,
                "case {i}: {name} bits diverge (got {} = {:e}, want {} = {:e})",
                f64_to_hex_bits(got),
                got,
                want_hex,
                f64_from_hex_bits(want_hex).unwrap(),
            );
        }
    }
}
