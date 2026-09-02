//! order_side representation: `i32` (+1/-1) vs `enum OrderSide` vs a SIGNED quantity — the
//! arithmetic cost of the Tier-1b `side: i32 -> enum` decision.
//!
//! `cargo bench -p vike-model --bench orderside` (release). Custom harness, repo style; stdout is
//! a raw ns/op table. `side` is unlike the other strong-types fields: it is not a label carried
//! and compared, it is a NUMBER used as a multiplier everywhere — the Account position fold
//! (`net += side*qty`), the signed notional (`side*qty*px`), realized PnL. This bench answers: does
//! replacing the raw integer with an enum + a `.sign()`/`.signum()` accessor CALLED AT EACH MATH
//! SITE cost anything, and is carrying a signed quantity (Buy=+qty, Sell=-qty, NO side field at
//! all — the sign is implicit) cheaper still?
//!
//! Two side patterns are measured: ALTERNATING (branch-predictor-friendly) and RANDOM-ish
//! (data-dependent) — because the enum accessor is a `match`, and the question is whether LLVM
//! lowers it to a branchless select (no misprediction) or a real branch.

use std::hint::black_box;
use std::time::Instant;

/// Candidate strong type. `#[repr(i8)]` with Buy=+1/Sell=-1 so `as i8` IS the sign.
#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(i8)]
enum OrderSide {
    Buy = 1,
    Sell = -1,
}
impl OrderSide {
    /// The signed multiplier used at arithmetic sites (`side.sign() * qty`) — the MATCH form.
    #[inline]
    fn sign(self) -> f64 {
        match self {
            OrderSide::Buy => 1.0,
            OrderSide::Sell => -1.0,
        }
    }
    /// The CAST form — only valid because `#[repr(i8)] Buy=1/Sell=-1`, so the discriminant IS
    /// the sign. Branchless (no misprediction), bit-identical to today's `side as f64`.
    #[inline]
    fn sign_cast(self) -> f64 {
        self as i8 as f64
    }
    /// Integer signum (`i32` path — mirrors today's `side * qty` where side: i32).
    #[inline]
    fn signum(self) -> i32 {
        self as i32
    }
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
    println!("  {label:<44} {ns:>9.1} ns/op");
}

fn main() {
    println!(
        "\n=== size_of ===  i32 {} · OrderSide(enum) {} · f64 {}",
        std::mem::size_of::<i32>(),
        std::mem::size_of::<OrderSide>(),
        std::mem::size_of::<f64>()
    );

    const N: usize = 100_000;
    let qtys: Vec<f64> = (0..N).map(|i| 1.0 + (i % 7) as f64 * 0.3).collect();
    let pxs: Vec<f64> = (0..N).map(|i| 100.0 + (i % 11) as f64).collect();

    // ALTERNATING sides (predictable) and a scrambled/data-dependent pattern (stresses branch pred).
    let alt_i32: Vec<i32> = (0..N).map(|i| if i % 2 == 0 { 1 } else { -1 }).collect();
    let alt_en: Vec<OrderSide> =
        (0..N).map(|i| if i % 2 == 0 { OrderSide::Buy } else { OrderSide::Sell }).collect();
    // scramble: a cheap LCG-ish bit mix so the side sequence is not trivially predictable
    let scr_bit = |i: usize| ((i.wrapping_mul(2654435761) >> 13) & 1) == 0;
    let scr_i32: Vec<i32> = (0..N).map(|i| if scr_bit(i) { 1 } else { -1 }).collect();
    let scr_en: Vec<OrderSide> =
        (0..N).map(|i| if scr_bit(i) { OrderSide::Buy } else { OrderSide::Sell }).collect();

    // SIGNED-QTY representation: qty already carries the sign — no side field, no per-site multiply.
    let alt_sqty: Vec<f64> =
        (0..N).map(|i| qtys[i] * if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
    let scr_sqty: Vec<f64> =
        (0..N).map(|i| qtys[i] * if scr_bit(i) { 1.0 } else { -1.0 }).collect();

    let sweeps: u64 = 2000;

    // ---- FOLD: net signed size = Σ(side·qty). THE Account position fold, per fill. ----
    println!("\n=== FOLD: net signed size over {N} fills (Account position fold) — ALTERNATING sides ===");
    row(
        "i32:  net += side as f64 * qty",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for i in 0..N {
                net += black_box(alt_i32[i]) as f64 * black_box(qtys[i]);
            }
            black_box(net);
        }),
    );
    row(
        "enum: net += side.sign() * qty",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for i in 0..N {
                net += black_box(alt_en[i]).sign() * black_box(qtys[i]);
            }
            black_box(net);
        }),
    );
    row(
        "enum: net += side.signum() as f64 * qty",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for i in 0..N {
                net += black_box(alt_en[i]).signum() as f64 * black_box(qtys[i]);
            }
            black_box(net);
        }),
    );
    row(
        "signed-qty: net += sqty (no side field)",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for &v in &alt_sqty {
                net += black_box(v);
            }
            black_box(net);
        }),
    );

    println!("\n=== FOLD: same, SCRAMBLED sides (branch-prediction stress) ===");
    row(
        "i32:  net += side as f64 * qty",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for i in 0..N {
                net += black_box(scr_i32[i]) as f64 * black_box(qtys[i]);
            }
            black_box(net);
        }),
    );
    row(
        "enum: net += side.sign() * qty",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for i in 0..N {
                net += black_box(scr_en[i]).sign() * black_box(qtys[i]);
            }
            black_box(net);
        }),
    );
    row(
        "signed-qty: net += sqty (no side field)",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for &v in &scr_sqty {
                net += black_box(v);
            }
            black_box(net);
        }),
    );

    // ---- signed NOTIONAL = side·qty·px, accumulated (fold + a multiply). ----
    println!("\n=== NOTIONAL: Σ(side·qty·px) over {N} fills — SCRAMBLED sides ===");
    row(
        "i32:  acc += side as f64 * qty * px",
        ns_per(sweeps, || {
            let mut acc = 0.0;
            for i in 0..N {
                acc += black_box(scr_i32[i]) as f64 * black_box(qtys[i]) * black_box(pxs[i]);
            }
            black_box(acc);
        }),
    );
    row(
        "enum: acc += side.sign() * qty * px",
        ns_per(sweeps, || {
            let mut acc = 0.0;
            for i in 0..N {
                acc += black_box(scr_en[i]).sign() * black_box(qtys[i]) * black_box(pxs[i]);
            }
            black_box(acc);
        }),
    );
    row(
        "signed-qty: acc += sqty * px",
        ns_per(sweeps, || {
            let mut acc = 0.0;
            for i in 0..N {
                acc += black_box(scr_sqty[i]) * black_box(pxs[i]);
            }
            black_box(acc);
        }),
    );

    // ---- MATCH .sign() vs CAST .sign_cast() on the SCRAMBLED fold: does the repr(i8) cast form
    // avoid the branch-misprediction the match form shows? (This picks the sign() impl.) ----
    println!("\n=== SIGN IMPL: i32 vs match-sign vs cast-sign — SCRAMBLED fold ===");
    row(
        "i32:        net += side as f64 * qty",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for i in 0..N {
                net += black_box(scr_i32[i]) as f64 * black_box(qtys[i]);
            }
            black_box(net);
        }),
    );
    row(
        "enum match: net += side.sign() * qty",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for i in 0..N {
                net += black_box(scr_en[i]).sign() * black_box(qtys[i]);
            }
            black_box(net);
        }),
    );
    row(
        "enum cast:  net += side.sign_cast() * qty",
        ns_per(sweeps, || {
            let mut net = 0.0;
            for i in 0..N {
                net += black_box(scr_en[i]).sign_cast() * black_box(qtys[i]);
            }
            black_box(net);
        }),
    );

    // ---- single-op accessor latency (isolated: does .sign() compile to a branchless select?) ----
    println!("\n=== SINGLE-OP: the accessor itself, data-dependent (mispredict probe) ===");
    let ops: u64 = 20_000_000;
    row(
        "i32 -> f64 cast (side as f64)",
        ns_per(ops, {
            let mut i = 0usize;
            move || {
                let s = scr_i32[i % N];
                i += 1;
                black_box(black_box(s) as f64);
            }
        }),
    );
    row(
        "enum .sign() -> f64",
        ns_per(ops, {
            let mut i = 0usize;
            move || {
                let s = scr_en[i % N];
                i += 1;
                black_box(black_box(s).sign());
            }
        }),
    );

    println!(
        "\nNOTE: the FOLD/NOTIONAL loops are the real Account hot path; the single-op row shows"
    );
    println!("whether `.sign()` is a branchless select (≈ the cast) or a mispredicting branch.\n");
}
