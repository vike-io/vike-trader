//! Measure-first: does boxing the fill-carrying `Event` variants help or hurt?
//!
//! `cargo bench -p vike-model --bench enumbox`. Models the real layout — an inline enum sized to
//! its largest variant (like today's 208 B `Event`) vs one where the fill variant is boxed (~96 B).
//! Fields are sized to mirror the real `FillEvent` (168 B), but use EMPTY strings + a pre-interned
//! Ustr so there is ZERO per-event string alloc — the ONLY alloc that differs is the `Box` itself
//! (same isolation trick the runtime_latency harness uses). The "lane" is a VecDeque: push = memcpy
//! the enum in, pop = memcpy out + fold + drop. Cross-thread wakeup is boxing-independent, so
//! excluding it makes the boxing effect MORE visible, not less.

// The mirror structs below deliberately carry unread fields and an unconstructed `Modified`
// variant: they exist to make each type's `size_of` match the real `Event`/`FillEvent` layout
// (the whole point of the bench), not to be fully exercised. Dead code here is intentional.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::hint::black_box;
use std::time::Instant;

use ustr::{ustr, Ustr};

// ---- mirror the real FillEvent layout (168 B), alloc-free to construct ----
#[derive(Clone)]
struct FillBig {
    trade_id: String,
    coid: String,
    venue: Ustr,
    symbol: Ustr,
    side: i32,
    last_qty: f64,
    last_px: f64,
    commission: f64,
    commission_asset: String,
    liquidity_side: String,
    ts: i64,
    mark_price: Option<f64>,
    position_side: u8,
}
#[derive(Clone)]
struct Filled {
    coid: String,
    fill: FillBig,
    ts: i64,
} // ~200
#[derive(Clone)]
struct Small {
    coid: String,
    ts: i64,
} // ~32
#[derive(Clone)]
struct Modified {
    coid: String,
    venue_order_id: Option<String>,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    ts: i64,
} // ~88 — the largest NON-fill variant (the boxing floor)

enum EvInline {
    Fill(Filled),
    Small(Small),
    Modified(Modified),
}
enum EvBoxed {
    Fill(Box<Filled>),
    Small(Small),
    Modified(Modified),
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
    println!("  {label:<40} {ns:>7.2} ns/event");
}

fn main() {
    let sym = ustr("BTCUSDT");
    let ven = ustr("sim");
    // alloc-free templates: empty Strings (no heap), pre-interned Ustr (no probe).
    let mk_fill = || Filled {
        coid: String::new(),
        fill: FillBig {
            trade_id: String::new(),
            coid: String::new(),
            venue: ven,
            symbol: sym,
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: String::new(),
            liquidity_side: String::new(),
            ts: 0,
            mark_price: None,
            position_side: 0,
        },
        ts: 0,
    };
    let mk_small = || Small { coid: String::new(), ts: 0 };

    println!("\n=== size_of the modelled enums ===");
    println!(
        "  EvInline (fill by value) {:>4} B   (mirrors today's Event = 208)",
        std::mem::size_of::<EvInline>()
    );
    println!(
        "  EvBoxed  (fill boxed)    {:>4} B   (mirrors boxed Event ~= 96)",
        std::mem::size_of::<EvBoxed>()
    );

    let n: u64 = 20_000_000;

    println!("\n=== FILL event: construct → lane(push/pop) → fold → drop ===");
    row(
        "inline  (208 B memcpy, 0 alloc)",
        ns_per(n, {
            let mut dq: VecDeque<EvInline> = VecDeque::with_capacity(2);
            let mk = mk_fill;
            move || {
                dq.push_back(EvInline::Fill(mk()));
                if let Some(EvInline::Fill(f)) = dq.pop_front() {
                    black_box(f.fill.last_px + f.ts as f64);
                }
            }
        }),
    );
    row(
        "boxed   (8 B memcpy, +1 alloc/free)",
        ns_per(n, {
            let mut dq: VecDeque<EvBoxed> = VecDeque::with_capacity(2);
            let mk = mk_fill;
            move || {
                dq.push_back(EvBoxed::Fill(Box::new(mk())));
                if let Some(EvBoxed::Fill(f)) = dq.pop_front() {
                    black_box(f.fill.last_px + f.ts as f64); // derefs the Box
                }
            }
        }),
    );

    println!("\n=== SMALL event (the majority in a busy book): construct → lane → fold → drop ===");
    row(
        "inline  (208 B memcpy, 0 alloc)",
        ns_per(n, {
            let mut dq: VecDeque<EvInline> = VecDeque::with_capacity(2);
            let mk = mk_small;
            move || {
                dq.push_back(EvInline::Small(mk()));
                if let Some(EvInline::Small(s)) = dq.pop_front() {
                    black_box(s.ts);
                }
            }
        }),
    );
    row(
        "boxed   (96 B memcpy, 0 alloc)",
        ns_per(n, {
            let mut dq: VecDeque<EvBoxed> = VecDeque::with_capacity(2);
            let mk = mk_small;
            move || {
                dq.push_back(EvBoxed::Small(mk()));
                if let Some(EvBoxed::Small(s)) = dq.pop_front() {
                    black_box(s.ts);
                }
            }
        }),
    );

    // ---- realistic mixed stream: 5 small : 1 fill (market-making-ish) ----
    println!("\n=== MIXED stream 5 small : 1 fill (per event, avg) ===");
    row(
        "inline",
        ns_per(n, {
            let mut dq: VecDeque<EvInline> = VecDeque::with_capacity(2);
            let (mkf, mks) = (mk_fill, mk_small);
            let mut i = 0u64;
            move || {
                i += 1;
                if i.is_multiple_of(6) {
                    dq.push_back(EvInline::Fill(mkf()));
                } else {
                    dq.push_back(EvInline::Small(mks()));
                }
                match dq.pop_front() {
                    Some(EvInline::Fill(f)) => {
                        black_box(f.fill.last_px);
                    }
                    Some(EvInline::Small(s)) => {
                        black_box(s.ts);
                    }
                    _ => {}
                }
            }
        }),
    );
    row(
        "boxed",
        ns_per(n, {
            let mut dq: VecDeque<EvBoxed> = VecDeque::with_capacity(2);
            let (mkf, mks) = (mk_fill, mk_small);
            let mut i = 0u64;
            move || {
                i += 1;
                if i.is_multiple_of(6) {
                    dq.push_back(EvBoxed::Fill(Box::new(mkf())));
                } else {
                    dq.push_back(EvBoxed::Small(mks()));
                }
                match dq.pop_front() {
                    Some(EvBoxed::Fill(f)) => {
                        black_box(f.fill.last_px);
                    }
                    Some(EvBoxed::Small(s)) => {
                        black_box(s.ts);
                    }
                    _ => {}
                }
            }
        }),
    );

    println!();
}
