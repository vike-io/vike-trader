//! Fixed-point vs f64 — the "Python parity retired, is fixed-point worth adopting?" question.
//!
//! `cargo bench -p vike-model --bench numerics` (release). Measures the arithmetic vike actually
//! does (compute_fill's avg-px update, notional = price×qty, equity += pnl, and the equity-curve
//! sum) across f64, i64 fixed-point (9 dp, Nautilus-style), i128 fixed-point (defi 18-dp tier),
//! and `rust_decimal`. The headline it answers: fixed-point is a CORRECTNESS/DETERMINISM choice,
//! not a speed win — this bench quantifies the speed COST.

use std::hint::black_box;
use std::time::Instant;

use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use vike_model::py_sum;

const S: i64 = 1_000_000_000; // 1e9 → 9 decimal places (Nautilus default price precision)
const S128: i128 = 1_000_000_000;

#[inline]
fn to_fx(x: f64) -> i64 {
    (x * S as f64).round() as i64
}
#[inline]
fn fx_mul(a: i64, b: i64) -> i64 {
    ((a as i128 * b as i128) / S128) as i64 // i128 intermediate then rescale
}
#[inline]
fn fx_div(a: i64, b: i64) -> i64 {
    ((a as i128 * S128) / b as i128) as i64
}
#[inline]
fn fx128_mul(a: i128, b: i128) -> i128 {
    (a * b) / S128
}

fn ns_per<F: FnMut()>(iters: u64, mut f: F) -> f64 {
    for _ in 0..(iters / 8).max(1) {
        f();
    }
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    t.elapsed().as_nanos() as f64 / iters as f64
}
fn row(label: &str, ns: f64) {
    println!("  {label:<38} {ns:>8.3} ns/op");
}

fn main() {
    let n: u64 = 5_000_000;

    // representative values
    let price = 62000.12345678_f64;
    let qty = 1.5_f64;
    let (ps, pa) = (3.0_f64, 61000.0_f64); // prior size, prior avg
    let (pfx, qfx) = (to_fx(price), to_fx(qty));
    let (psfx, pafx) = (to_fx(ps), to_fx(pa));
    let (pd, qd) = (Decimal::from_f64(price).unwrap(), Decimal::from_f64(qty).unwrap());
    let (psd, pad) = (Decimal::from_f64(ps).unwrap(), Decimal::from_f64(pa).unwrap());

    println!(
        "\n=== size_of ===  f64 {} · i64 {} · i128 {} · Decimal {}",
        std::mem::size_of::<f64>(),
        std::mem::size_of::<i64>(),
        std::mem::size_of::<i128>(),
        std::mem::size_of::<Decimal>()
    );

    println!("\n=== MULTIPLY: notional = price × qty ===");
    row(
        "f64",
        ns_per(n, || {
            black_box(black_box(price) * black_box(qty));
        }),
    );
    row(
        "i64 fixed-point (9dp)",
        ns_per(n, || {
            black_box(fx_mul(black_box(pfx), black_box(qfx)));
        }),
    );
    row(
        "i128 fixed-point",
        ns_per(n, || {
            black_box(fx128_mul(black_box(pfx as i128), black_box(qfx as i128)));
        }),
    );
    row(
        "rust_decimal",
        ns_per(n, || {
            black_box(black_box(pd) * black_box(qd));
        }),
    );

    println!("\n=== ADD: equity += pnl ===");
    let pnl = 12.34_f64;
    let pnlfx = to_fx(pnl);
    let pnld = Decimal::from_f64(pnl).unwrap();
    row(
        "f64",
        ns_per(n, || {
            black_box(black_box(price) + black_box(pnl));
        }),
    );
    row(
        "i64 fixed-point",
        ns_per(n, || {
            black_box(black_box(pfx) + black_box(pnlfx));
        }),
    );
    row(
        "rust_decimal",
        ns_per(n, || {
            black_box(black_box(pd) + black_box(pnld));
        }),
    );

    println!("\n=== compute_fill core: new_avg = (ps·pa + q·px)/(ps+q) ===");
    row(
        "f64",
        ns_per(n, || {
            let (ps, pa, q, px) = (black_box(ps), black_box(pa), black_box(qty), black_box(price));
            black_box((ps * pa + q * px) / (ps + q));
        }),
    );
    row(
        "i64 fixed-point",
        ns_per(n, || {
            let (ps, pa, q, px) =
                (black_box(psfx), black_box(pafx), black_box(qfx), black_box(pfx));
            black_box(fx_div(fx_mul(ps, pa) + fx_mul(q, px), ps + q));
        }),
    );
    row(
        "rust_decimal",
        ns_per(n, || {
            let (ps, pa, q, px) = (black_box(psd), black_box(pad), black_box(qd), black_box(pd));
            black_box((ps * pa + q * px) / (ps + q));
        }),
    );

    // ---- equity-curve SUM over 10k pnls ----
    println!("\n=== SUM: equity curve over 10k P&Ls (accumulation) ===");
    const K: usize = 10_000;
    let mut f: Vec<f64> = Vec::with_capacity(K);
    for i in 0..K {
        // deterministic mix of +/- fractional pnls
        let v = ((i as f64) * 0.37).sin() * 137.91;
        f.push(v);
    }
    let vi: Vec<i64> = f.iter().map(|&x| to_fx(x)).collect();
    let vi128: Vec<i128> = vi.iter().map(|&x| x as i128).collect();
    let vd: Vec<Decimal> = f.iter().map(|&x| Decimal::from_f64(x).unwrap()).collect();
    let sweeps: u64 = 2000;
    row(
        "f64 naive fold",
        ns_per(sweeps, || {
            black_box(black_box(&f).iter().sum::<f64>());
        }),
    );
    row(
        "f64 py_sum (Neumaier, order-dependent)",
        ns_per(sweeps, || {
            black_box(py_sum(black_box(&f).iter().copied()));
        }),
    );
    row(
        "i64 exact (order-independent)",
        ns_per(sweeps, || {
            black_box(black_box(&vi).iter().sum::<i64>());
        }),
    );
    row(
        "i128 exact",
        ns_per(sweeps, || {
            black_box(black_box(&vi128).iter().sum::<i128>());
        }),
    );
    row(
        "rust_decimal exact",
        ns_per(sweeps, || {
            black_box(black_box(&vd).iter().sum::<Decimal>());
        }),
    );

    println!(
        "\nNOTE: fixed-point/Decimal buy EXACTNESS + cross-platform determinism (integer sums are"
    );
    println!(
        "associative → no py_sum/Neumaier dance, identical on x86/ARM). The numbers above are the"
    );
    println!("SPEED COST of that guarantee; f64 has the FPU. Adopt fixed-point for correctness, not perf.\n");
}
