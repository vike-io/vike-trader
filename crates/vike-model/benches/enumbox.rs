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

#[path = "common/ns_per.rs"]
mod ns_per;
use ns_per::ns_per;

use ustr::{Ustr, ustr};

// ---- mirror the real FillEvent layout (168 B), alloc-free to construct ----
#[derive(Clone)]
struct FillBig {
    trade_id: String,
    order_client_id: String,
    venue: Ustr,
    symbol: Ustr,
    side: i32,
    last_qty: f64,
    last_price: f64,
    commission: f64,
    commission_asset: String,
    liquidity_side: String,
    timestamp: i64,
    mark_price: Option<f64>,
    position_side: u8,
}
#[derive(Clone)]
struct Filled {
    order_client_id: String,
    fill: FillBig,
    timestamp: i64,
} // ~200
#[derive(Clone)]
struct Small {
    order_client_id: String,
    timestamp: i64,
} // ~32
#[derive(Clone)]
struct Modified {
    order_client_id: String,
    venue_order_id: Option<String>,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    timestamp: i64,
} // ~88 — the largest NON-fill variant (the boxing floor)

enum EventInline {
    Fill(Filled),
    Small(Small),
    Modified(Modified),
}
enum EventBoxed {
    Fill(Box<Filled>),
    Small(Small),
    Modified(Modified),
}

fn row(label: &str, ns: f64) {
    println!("  {label:<40} {ns:>7.2} ns/event");
}

fn main() {
    let symbol_name = ustr("BTCUSDT");
    let ven = ustr("sim");
    // alloc-free templates: empty Strings (no heap), pre-interned Ustr (no probe).
    let mk_fill = || Filled {
        order_client_id: String::new(),
        fill: FillBig {
            trade_id: String::new(),
            order_client_id: String::new(),
            venue: ven,
            symbol: symbol_name,
            side: 1,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: String::new(),
            liquidity_side: String::new(),
            timestamp: 0,
            mark_price: None,
            position_side: 0,
        },
        timestamp: 0,
    };
    let mk_small = || Small { order_client_id: String::new(), timestamp: 0 };

    println!("\n=== size_of the modelled enums ===");
    println!(
        "  EvInline (fill by value) {:>4} B   (mirrors today's Event = 208)",
        std::mem::size_of::<EventInline>()
    );
    println!(
        "  EvBoxed  (fill boxed)    {:>4} B   (mirrors boxed Event ~= 96)",
        std::mem::size_of::<EventBoxed>()
    );

    let n: u64 = 20_000_000;

    println!("\n=== FILL event: construct → lane(push/pop) → fold → drop ===");
    row(
        "inline  (208 B memcpy, 0 alloc)",
        ns_per(n, {
            let mut dq: VecDeque<EventInline> = VecDeque::with_capacity(2);
            let mk = mk_fill;
            move || {
                dq.push_back(EventInline::Fill(mk()));
                if let Some(EventInline::Fill(f)) = dq.pop_front() {
                    black_box(f.fill.last_price + f.timestamp as f64);
                }
            }
        }),
    );
    row(
        "boxed   (8 B memcpy, +1 alloc/free)",
        ns_per(n, {
            let mut dq: VecDeque<EventBoxed> = VecDeque::with_capacity(2);
            let mk = mk_fill;
            move || {
                dq.push_back(EventBoxed::Fill(Box::new(mk())));
                if let Some(EventBoxed::Fill(f)) = dq.pop_front() {
                    black_box(f.fill.last_price + f.timestamp as f64); // derefs the Box
                }
            }
        }),
    );

    println!("\n=== SMALL event (the majority in a busy book): construct → lane → fold → drop ===");
    row(
        "inline  (208 B memcpy, 0 alloc)",
        ns_per(n, {
            let mut dq: VecDeque<EventInline> = VecDeque::with_capacity(2);
            let mk = mk_small;
            move || {
                dq.push_back(EventInline::Small(mk()));
                if let Some(EventInline::Small(s)) = dq.pop_front() {
                    black_box(s.timestamp);
                }
            }
        }),
    );
    row(
        "boxed   (96 B memcpy, 0 alloc)",
        ns_per(n, {
            let mut dq: VecDeque<EventBoxed> = VecDeque::with_capacity(2);
            let mk = mk_small;
            move || {
                dq.push_back(EventBoxed::Small(mk()));
                if let Some(EventBoxed::Small(s)) = dq.pop_front() {
                    black_box(s.timestamp);
                }
            }
        }),
    );

    // ---- realistic mixed stream: 5 small : 1 fill (market-making-ish) ----
    println!("\n=== MIXED stream 5 small : 1 fill (per event, avg) ===");
    row(
        "inline",
        ns_per(n, {
            let mut dq: VecDeque<EventInline> = VecDeque::with_capacity(2);
            let (mkf, mks) = (mk_fill, mk_small);
            let mut i = 0u64;
            move || {
                i += 1;
                if i.is_multiple_of(6) {
                    dq.push_back(EventInline::Fill(mkf()));
                } else {
                    dq.push_back(EventInline::Small(mks()));
                }
                match dq.pop_front() {
                    Some(EventInline::Fill(f)) => {
                        black_box(f.fill.last_price);
                    }
                    Some(EventInline::Small(s)) => {
                        black_box(s.timestamp);
                    }
                    _ => {}
                }
            }
        }),
    );
    row(
        "boxed",
        ns_per(n, {
            let mut dq: VecDeque<EventBoxed> = VecDeque::with_capacity(2);
            let (mkf, mks) = (mk_fill, mk_small);
            let mut i = 0u64;
            move || {
                i += 1;
                if i.is_multiple_of(6) {
                    dq.push_back(EventBoxed::Fill(Box::new(mkf())));
                } else {
                    dq.push_back(EventBoxed::Small(mks()));
                }
                match dq.pop_front() {
                    Some(EventBoxed::Fill(f)) => {
                        black_box(f.fill.last_price);
                    }
                    Some(EventBoxed::Small(s)) => {
                        black_box(s.timestamp);
                    }
                    _ => {}
                }
            }
        }),
    );

    println!();
}
