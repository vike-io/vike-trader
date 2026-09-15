//! Strong-types spike — head-to-head of the candidate field types.
//!
//! `cargo bench -p vike-model --bench strtypes` (release). Custom harness (repo style); stdout is
//! a raw result table. NOT a merge artifact — it measures the four operations the type choice
//! actually turns on: construct, size, map-lookup-by-coid, and the per-event symbol-filter compare.
//!
//! Values are the real distribution: a 7-char crypto symbol, the 77-char Polymarket token_id, a
//! 66-char hash trade_id, plus venue/side labels and a live-shape client_order_id.

use std::hint::black_box;
use std::time::Instant;

use arrayvec::ArrayString;
use compact_str::CompactString;
use indexmap::IndexMap;
use ustr::{Ustr, ustr};
use vike_model::events::{Event, FillEvent, OrderExpired, OrderFilled};

// ---- representative values (measured distribution) ----------------------------------------
const SHORT_SYM: &str = "BTCUSDT"; // 7 — crypto
const LONG_SYM: &str =
    "93005850938352995663334573245996733794924636935158112548608169054144721737755"; // 77 — Polymarket token_id
const VENUE: &str = "binance";
const SIDE: &str = "LONG";
const COID: &str = "deadbeef12345"; // 13 — live-shape client_order_id
const TRADE_HASH: &str = "0xabcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab"; // 66

/// The real `ClientOrderId` shape (session:u32 + seq:u64) — 16 bytes, Copy.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct Coid {
    session: u32,
    seq: u64,
}

/// Closed-set strong type for position_side — 1 byte.
#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
enum PositionSide {
    Both,
    Long,
    Short,
}
fn parse_side(s: &str) -> PositionSide {
    match s.as_bytes() {
        b"LONG" => PositionSide::Long,
        b"SHORT" => PositionSide::Short,
        _ => PositionSide::Both,
    }
}

/// ns/op over `iters` iterations, after a warm-up.
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
    println!("  {label:<34} {ns:>8.2} ns/op");
}

fn main() {
    // Pre-intern so Ustr construction measures the steady-state probe, not the first alloc.
    let _ = (ustr(SHORT_SYM), ustr(LONG_SYM), ustr(VENUE), ustr(SIDE));
    let n: u64 = 3_000_000;

    println!("\n=== size_of (bytes) — the struct footprint moved through the channel ===");
    println!("  String          {:>3}", std::mem::size_of::<String>());
    println!("  CompactString   {:>3}", std::mem::size_of::<CompactString>());
    println!("  Ustr            {:>3}", std::mem::size_of::<Ustr>());
    println!("  ArrayString<72> {:>3}", std::mem::size_of::<ArrayString<72>>());
    println!("  PositionSide    {:>3}  (enum)", std::mem::size_of::<PositionSide>());
    println!("  Coid{{u32,u64}}   {:>3}  (u64 key)", std::mem::size_of::<Coid>());

    println!("\n=== size_of the Event god-enum vs its variants (the channel-message size) ===");
    println!(
        "  Event (whole enum)     {:>4}  <- every message on the ingest lane is THIS big",
        std::mem::size_of::<Event>()
    );
    println!(
        "  FillEvent              {:>4}  (embedded in the fill-carrying variants)",
        std::mem::size_of::<FillEvent>()
    );
    println!(
        "  OrderFilled variant    {:>4}  (the largest — sets the enum size)",
        std::mem::size_of::<OrderFilled>()
    );
    println!(
        "  OrderExpired variant   {:>4}  (a small variant — pays the big-enum tax anyway)",
        std::mem::size_of::<OrderExpired>()
    );
    println!("  Event with Box<FillEvent> would shrink to ~= the next-largest non-fill variant");
    {
        use vike_model::events::*;
        println!("\n  -- non-fill variants (the boxing floor = the largest of these + 8 tag) --");
        println!("     OrderAccepted   {:>3}", std::mem::size_of::<OrderAccepted>());
        println!("     OrderCanceled   {:>3}", std::mem::size_of::<OrderCanceled>());
        println!("     OrderModified   {:>3}", std::mem::size_of::<OrderModified>());
        println!("     PositionChanged {:>3}", std::mem::size_of::<PositionChanged>());
        println!("     PositionLiquidated {:>3}", std::mem::size_of::<PositionLiquidated>());
        println!("     AccountState    {:>3}", std::mem::size_of::<AccountState>());
        println!("     FundingEvent    {:>3}", std::mem::size_of::<FundingEvent>());
        println!("\n  -- field-type sizes (for the shrink-the-fields option) --");
        println!(
            "     String {:>2} · Ustr {:>2} · Option<String> {:>2} · Option<f64> {:>2} · Coid {:>2} · enum {:>2}",
            std::mem::size_of::<String>(),
            std::mem::size_of::<Ustr>(),
            std::mem::size_of::<Option<String>>(),
            std::mem::size_of::<Option<f64>>(),
            std::mem::size_of::<Coid>(),
            std::mem::size_of::<vike_model::events::PositionSide>()
        );
    }

    println!("\n=== CONSTRUCT: short symbol \"{SHORT_SYM}\" (7 ch — fits inline) ===");
    row(
        "String::from",
        ns_per(n, || {
            black_box(String::from(black_box(SHORT_SYM)));
        }),
    );
    row(
        "CompactString::from",
        ns_per(n, || {
            black_box(CompactString::from(black_box(SHORT_SYM)));
        }),
    );
    row(
        "ustr (intern probe)",
        ns_per(n, || {
            black_box(ustr(black_box(SHORT_SYM)));
        }),
    );

    println!("\n=== CONSTRUCT: Polymarket token_id (77 ch — OVERFLOWS 24-byte inline) ===");
    row(
        "String::from",
        ns_per(n, || {
            black_box(String::from(black_box(LONG_SYM)));
        }),
    );
    row(
        "CompactString::from (heaps)",
        ns_per(n, || {
            black_box(CompactString::from(black_box(LONG_SYM)));
        }),
    );
    row(
        "ustr (intern probe)",
        ns_per(n, || {
            black_box(ustr(black_box(LONG_SYM)));
        }),
    );

    println!("\n=== CONSTRUCT: position_side \"{SIDE}\" (closed set) ===");
    row(
        "String::from",
        ns_per(n, || {
            black_box(String::from(black_box(SIDE)));
        }),
    );
    row(
        "CompactString::from",
        ns_per(n, || {
            black_box(CompactString::from(black_box(SIDE)));
        }),
    );
    row(
        "ustr (intern probe)",
        ns_per(n, || {
            black_box(ustr(black_box(SIDE)));
        }),
    );
    row(
        "enum parse (1 byte)",
        ns_per(n, || {
            black_box(parse_side(black_box(SIDE)));
        }),
    );

    println!("\n=== CONSTRUCT: trade_id 66-char hash (high-cardinality, unbounded len) ===");
    row(
        "String::from",
        ns_per(n, || {
            black_box(String::from(black_box(TRADE_HASH)));
        }),
    );
    row(
        "CompactString::from (heaps)",
        ns_per(n, || {
            black_box(CompactString::from(black_box(TRADE_HASH)));
        }),
    );
    row(
        "ArrayString<72>::from (inline)",
        ns_per(n, || {
            black_box(ArrayString::<72>::from(black_box(TRADE_HASH)).unwrap());
        }),
    );

    println!("\n=== CONSTRUCT/MINT: client_order_id \"{COID}\" (13 ch) ===");
    row(
        "String::from",
        ns_per(n, || {
            black_box(String::from(black_box(COID)));
        }),
    );
    row(
        "CompactString::from (inline)",
        ns_per(n, || {
            black_box(CompactString::from(black_box(COID)));
        }),
    );
    row(
        "Coid mint (session,seq → u64)",
        ns_per(n, {
            let mut seq = 0u64;
            move || {
                let c = Coid { session: 0xdead_beef, seq: black_box(seq) };
                seq += 1;
                black_box(c);
            }
        }),
    );

    // ---- MAP LOOKUP by client_order_id: the u64-key win ----
    println!("\n=== MAP: find order by client_order_id — 10k keys, look up every key ===");
    const K: usize = 10_000;
    let sweeps: u64 = 300;

    let mut m_str: IndexMap<String, u64> = IndexMap::with_capacity(K);
    let mut m_u64: IndexMap<Coid, u64> = IndexMap::with_capacity(K);
    let mut m_ust: IndexMap<Ustr, u64> = IndexMap::with_capacity(K);
    let mut keys_str = Vec::with_capacity(K);
    let mut keys_u64 = Vec::with_capacity(K);
    let mut keys_ust = Vec::with_capacity(K);
    for i in 0..K as u64 {
        let s = format!("deadbeef{i}");
        m_str.insert(s.clone(), i);
        keys_str.push(s.clone());
        let c = Coid { session: 0xdead_beef, seq: i };
        m_u64.insert(c, i);
        keys_u64.push(c);
        let u = ustr(&s);
        m_ust.insert(u, i);
        keys_ust.push(u);
    }

    let per = (K as u64) * sweeps;
    row(
        "IndexMap<String> lookup",
        ns_per(per, {
            let mut it = 0usize;
            move || {
                let k = &keys_str[it % K];
                it += 1;
                black_box(m_str.get(black_box(k)));
            }
        }),
    );
    row(
        "IndexMap<Ustr> lookup",
        ns_per(per, {
            let mut it = 0usize;
            move || {
                let k = keys_ust[it % K];
                it += 1;
                black_box(m_ust.get(black_box(&k)));
            }
        }),
    );
    row(
        "IndexMap<Coid u64> lookup",
        ns_per(per, {
            let mut it = 0usize;
            move || {
                let k = keys_u64[it % K];
                it += 1;
                black_box(m_u64.get(black_box(&k)));
            }
        }),
    );

    // ---- SYMBOL FILTER: per-event accepts_symbol compare ----
    println!("\n=== FILTER: per-event accepts_symbol — compare event.symbol to a target ===");
    // ~1/8 of events match the subscribed symbol (realistic).
    let syms_str: Vec<String> = (0..8u32)
        .map(|i| if i == 3 { LONG_SYM.to_string() } else { format!("TOKEN{i}") })
        .collect();
    let target_str = LONG_SYM.to_string();
    let syms_ust: Vec<Ustr> = syms_str.iter().map(|s| ustr(s)).collect();
    let target_ust = ustr(LONG_SYM);
    let fi: u64 = 3_000_000;
    row(
        "String ==  (memcmp)",
        ns_per(fi, {
            let mut it = 0usize;
            move || {
                let hit = black_box(&syms_str[it % 8]) == black_box(&target_str);
                it += 1;
                black_box(hit);
            }
        }),
    );
    row(
        "Ustr ==  (ptr eq)",
        ns_per(fi, {
            let mut it = 0usize;
            move || {
                let hit = black_box(syms_ust[it % 8]) == black_box(target_ust);
                it += 1;
                black_box(hit);
            }
        }),
    );

    // ---- ORDER LIFECYCLE: per-event registry lookup+mutate by client_order_id ----
    // Every ack/fill/cancel/modify finds its order by coid, then mutates status. THIS is where the
    // u64 coid key pays off end-to-end — the core-hop bench sends fills with an EMPTY coid, so it
    // never appears there. Models the exec engine's `IndexMap<coid, ManagedOrder>` registry.
    println!("\n=== ORDER LIFECYCLE: registry lookup+mutate by coid — {K} live orders ===");
    #[derive(Clone, Copy)]
    struct Ord {
        status: u8,
        filled: f64,
    }
    let mut reg_str: IndexMap<String, Ord> = IndexMap::with_capacity(K);
    let mut reg_u64: IndexMap<Coid, Ord> = IndexMap::with_capacity(K);
    let lk_str: Vec<String> = (0..K as u64).map(|i| format!("deadbeef{i}")).collect();
    let lk_u64: Vec<Coid> = (0..K as u64).map(|i| Coid { session: 0xdead_beef, seq: i }).collect();
    for (s, c) in lk_str.iter().zip(&lk_u64) {
        reg_str.insert(s.clone(), Ord { status: 0, filled: 0.0 });
        reg_u64.insert(*c, Ord { status: 0, filled: 0.0 });
    }
    let evper = (K as u64) * sweeps;
    row(
        "String registry: lookup+mutate",
        ns_per(evper, {
            let mut it = 0usize;
            move || {
                let k = &lk_str[it % K];
                it += 1;
                if let Some(o) = reg_str.get_mut(k) {
                    o.status = o.status.wrapping_add(1);
                    o.filled += 1.0;
                }
            }
        }),
    );
    row(
        "Coid u64 registry: lookup+mutate",
        ns_per(evper, {
            let mut it = 0usize;
            move || {
                let k = lk_u64[it % K];
                it += 1;
                if let Some(o) = reg_u64.get_mut(&k) {
                    o.status = o.status.wrapping_add(1);
                    o.filled += 1.0;
                }
            }
        }),
    );

    println!();
}
