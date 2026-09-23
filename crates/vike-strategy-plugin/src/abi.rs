//! The C-ABI vocabulary. Every type here is `#[repr(C)]` and every field is plain data.
//!
//! ⚠ The rule this file exists to hold: NO Rust type crosses by value. Not `Box`, not
//! `String`, not `Option<T>`, not `&[T]`. Strings cross as `(ptr, len)` and are never
//! NUL-terminated, because Rust strings are not. `Option<f64>` flattens to [`COptF64`].
//!
//! ⚠ The type declarations below are pure data and contain no `unsafe`. [`CBar::to_bar`] is
//! the one exception in this file — it reads back through a borrowed `(ptr, len)` pair, which
//! is the one place a `CBar` genuinely needs the raw pointer to be valid. That is expected in
//! this crate (`crates/vike-ops/tests/unsafe_and_toolchain_gate.rs`'s `UNSAFE_EXEMPT` names
//! it, per the design at
//! `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`), and the
//! `// SAFETY:` comment at the site names who owns the bytes and for how long.

/// Bumped whenever a vtable's shape or a signature changes. The loader refuses a
/// mismatch outright — it is the cheap guard that catches a DELIBERATE change, while
/// the toolchain fingerprint catches an accidental one.
///
/// ⚠ **1 -> 2: `BrokerVTable` grew four slots.** The real user-strategy entry contract is
/// `pub fn build<B: vike_model::HftBroker + 'static>(..)`, not `Broker` alone —
/// `HftBroker: Broker` adds `position(&self) -> f64` (no symbol; the mount pins one
/// `(venue, symbol)` series), `submit_limit_tagged`, `modify_tagged` and `cancel_tagged`. A
/// `HostBroker` implementing only `Broker` cannot satisfy that bound, so the cdylib template
/// (Track B) could never instantiate a real user strategy against it. This bump — and the four
/// new `BrokerVTable` fields below — is what makes `HostBroker` (`guest.rs`) actually implement
/// `HftBroker` rather than merely `Broker`.
///
/// ⚠ **2 -> 3: `PluginVTable` grew THIRTEEN dispatch slots** — every remaining
/// `vike_model::Strategy` seam method except the three whose payload is a `serde_json::Value` or
/// a RETURNED owned value (`params`, `save_state`, `load_state`; see
/// [`crate::host::UNWIRED_HOOKS`] for why those three are still refused at build time). ONE bump
/// for the whole batch rather than one per hook, deliberately: this version's job is to refuse a
/// plugin whose vtable SHAPE this host does not speak, and a plugin built against the two-slot
/// vtable is equally unloadable whichever of the thirteen it is missing — thirteen bumps would
/// have produced thirteen identical refusals of the same artifact.
pub const ABI_VERSION: u32 = 3;

/// A zero version cannot be distinguished from an unset field, so it must never be reachable.
/// Checked at COMPILE TIME — stronger than a `#[test]` asserting on a `const` (which
/// `clippy::assertions_on_constants`, a `-D warnings` merge-gate lint, correctly flags: the
/// condition is knowable at compile time, so a runtime `assert!` on it tests nothing an ordinary
/// build does not already prove) — and enforced on EVERY build, not only under `cargo test`.
const _: () =
    assert!(ABI_VERSION > 0, "a zero version cannot be distinguished from an unset field");

/// `Option<f64>` as plain data. `present == false` means the value is meaningless.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct COptF64 {
    pub value: f64,
    pub present: bool,
}

impl From<Option<f64>> for COptF64 {
    fn from(v: Option<f64>) -> Self {
        match v {
            Some(value) => Self { value, present: true },
            None => Self { value: f64::NAN, present: false },
        }
    }
}

impl From<COptF64> for Option<f64> {
    fn from(v: COptF64) -> Self {
        if v.present { Some(v.value) } else { None }
    }
}

/// Copy a borrowed `(ptr, len)` pair into an owned `String`, CHECKED rather than lossy.
///
/// ⚠ **The ONE `unsafe` site in this file, and deliberately so.** Every mirror below that carries
/// a string reads it back through here instead of spelling its own `from_raw_parts`, for the same
/// reason `host.rs`'s `read_str` and `loader.rs`'s `bind!` are single textual sites: the unsafe
/// act is identical at every one of them, and thirteen copies of it would be thirteen places for
/// the contract to be restated slightly differently. `crates/vike-ops/tests/
/// unsafe_and_toolchain_gate.rs`'s `UNSAFE_SITES` row for this file therefore stays at 1 across
/// the whole `ABI_VERSION` 3 widening.
///
/// The bytes crossing this ABI are supposed to be an exact copy of a live Rust `&str`'s bytes
/// (this module's header states the contract), so a decode failure is a CONTRACT VIOLATION worth
/// a loud panic rather than a quietly-corrupted symbol a strategy might then trade on — the
/// argument [`CBar::to_bar`]'s own doc makes at length, unchanged by the move.
///
/// ⚠ **Caller contract: only ever call this from inside a `catch_unwind`.** It panics, and every
/// call site is reached from an `extern "C"` frame. The template's dispatch exports and
/// `host.rs`'s `guarded` thunks both satisfy it; nothing here can enforce it.
fn read_borrowed_str(ptr: *const u8, len: usize) -> String {
    if len == 0 {
        return String::new();
    }
    // SAFETY: `(ptr, len)` is the pair this ABI's borrowed-string contract requires (this
    // module's header): produced from a live `&str`'s bytes by the peer across the boundary, and
    // kept alive and unmoved by that peer for at least the duration of the call this read is
    // inside. A null pointer is never passed with a non-zero length — every producer below either
    // writes a real `(ptr, len)` or a null pointer WITH a zero length, which the early return
    // above has already taken.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8(bytes.to_vec())
        .expect("plugin ABI contract violated: borrowed string bytes are not valid UTF-8")
}

/// A borrowed `&str` as plain data: `(ptr, len)`, never NUL-terminated.
///
/// ⚠ [`CBar`] predates this type and inlines its own `symbol_ptr`/`symbol_len` pair instead.
/// That is left alone on purpose — `CBar`'s layout is pinned by a test two other crates' fixtures
/// were written against, and re-spelling it would be churn with no behaviour attached. Every
/// mirror added at `ABI_VERSION` 3 uses this type.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CStrRef {
    pub ptr: *const u8,
    pub len: usize,
}

impl CStrRef {
    /// Borrows `s`'s bytes. Valid only for as long as `s` is alive and unmoved.
    pub fn of(s: &str) -> Self {
        CStrRef { ptr: s.as_ptr(), len: s.len() }
    }

    /// The empty string, as a null pointer and a zero length — what every mirror writes for a
    /// field a variant does not carry.
    pub fn empty() -> Self {
        CStrRef { ptr: std::ptr::null(), len: 0 }
    }

    /// Copy the borrowed bytes into an owned `String`. See [`read_borrowed_str`] for the contract.
    pub fn read(&self) -> String {
        read_borrowed_str(self.ptr, self.len)
    }
}

/// A borrowed `Option<&str>`. `present == false` means `None`, and then the pair is meaningless.
///
/// ⚠ The explicit flag is load-bearing rather than defensive: `Some("")`'s `as_ptr()` is a
/// non-null dangling pointer with length 0, and `None` is naturally spelled as a null pointer with
/// length 0 — so "is the pointer null" is a property of the ALLOCATOR, not of the option, and
/// reading the two apart from the pair alone would be reading a guarantee Rust does not make.
/// `OrderLifecycle::tag` is exactly this case: `None` means "placed through the untagged `Broker`
/// verbs", which is a different fact from "tagged with the empty string".
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct COptStrRef {
    pub s: CStrRef,
    pub present: bool,
}

impl COptStrRef {
    pub fn of(v: Option<&str>) -> Self {
        match v {
            Some(s) => COptStrRef { s: CStrRef::of(s), present: true },
            None => COptStrRef { s: CStrRef::empty(), present: false },
        }
    }

    pub fn read(&self) -> Option<String> {
        if self.present { Some(self.s.read()) } else { None }
    }
}

/// The wire mirror of `vike_marketdata::Bar`.
///
/// ⚠ `symbol` is a borrowed `(ptr, len)` owned by the CALLER for the duration of the
/// call only. A plugin that wants to keep it must copy it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CBar {
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub funding: COptF64,
    pub bid: COptF64,
    pub ask: COptF64,
    pub symbol_ptr: *const u8,
    pub symbol_len: usize,
}

impl CBar {
    /// An all-zero `CBar` with every optional absent and no symbol. A plugin (or a host cursor
    /// site like `BrokerVTable::bar_at`) uses this to build the out-param it writes into, so a
    /// caller who never checks the returned `bool` still reads a well-defined, empty bar rather
    /// than uninitialized stack memory.
    pub fn empty() -> Self {
        CBar {
            ts: 0,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close: 0.0,
            volume: 0.0,
            funding: COptF64 { value: 0.0, present: false },
            bid: COptF64 { value: 0.0, present: false },
            ask: COptF64 { value: 0.0, present: false },
            symbol_ptr: std::ptr::null(),
            symbol_len: 0,
        }
    }

    /// Borrows `b`'s symbol bytes. The returned `CBar`'s `symbol_ptr`/`symbol_len` are valid only
    /// for as long as `b` (and its `symbol` `String`, unmoved) is alive — the same contract this
    /// module's doc states for every `CBar` that crosses the boundary.
    pub fn from_bar(b: &vike_marketdata::Bar) -> Self {
        let (symbol_ptr, symbol_len) = match &b.symbol {
            Some(s) => (s.as_ptr(), s.len()),
            None => (std::ptr::null(), 0),
        };
        CBar {
            ts: b.ts,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume: b.volume,
            funding: b.funding.into(),
            bid: b.bid.into(),
            ask: b.ask.into(),
            symbol_ptr,
            symbol_len,
        }
    }

    /// Copies `self` into an owned `vike_marketdata::Bar`, including the symbol bytes.
    ///
    /// ⚠ **Track A review finding, addressed here rather than left lossy.** This used to read the
    /// symbol bytes with `String::from_utf8_lossy`, so an FFI peer that violated the byte contract
    /// (a stale pointer, a wrong length, a hand-built `CBar` that never went through
    /// [`CBar::from_bar`]) would silently get replacement characters instead of a signal anything
    /// was wrong — a strategy could then route orders/positions against a corrupted symbol with no
    /// evidence it happened. The bytes here are supposed to be an exact copy of a Rust `String`'s
    /// bytes (see this module's header): under the wire contract they can only fail to be valid
    /// UTF-8 if the peer corrupted them, which is a CONTRACT VIOLATION, not user data to tolerate
    /// gracefully. So this is now a CHECKED conversion that panics loudly instead.
    ///
    /// ⚠ **Caller contract, stated rather than enforced: `to_bar` must only be called from inside
    /// a `catch_unwind`.** This function is `pub` on a `pub` crate, so nothing here can make that
    /// true by construction — it is a REQUIREMENT on the caller, not a guarantee this file
    /// provides. The only production call site on THIS branch, `guest.rs`'s `HostBroker::bars`,
    /// satisfies it only once Track B's cdylib template exists: per the ABI contract ("a panic
    /// across a C-ABI boundary is UB" — the design doc), every plugin's generated `extern "C"`
    /// entry point wraps the ENTIRE call into user strategy code — which is what calls
    /// `HostBroker::bars`, which is what calls this — in `catch_unwind`, turning a caught panic
    /// into [`PluginStatus::Panicked`]. That wrapper is Track B's, is not on this branch, and this
    /// file cannot see or verify it exists. Anything that calls `to_bar` OUTSIDE such a wrapper —
    /// directly, in a test, or from a future call site — takes on the "a panic across a C-ABI
    /// boundary is UB" risk itself; this doc is the place that risk is written down, since the
    /// type system cannot carry it.
    pub fn to_bar(&self) -> vike_marketdata::Bar {
        let symbol = if self.symbol_ptr.is_null() {
            None
        } else {
            Some(read_borrowed_str(self.symbol_ptr, self.symbol_len))
        };
        vike_marketdata::Bar {
            ts: self.ts,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            funding: self.funding.into(),
            bid: self.bid.into(),
            ask: self.ask.into(),
            symbol,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The TIER-2 flat mirrors (`ABI_VERSION` 3): scalars plus, where present, a borrowed string.
//
// Each follows `CBar`'s shape exactly — a `from_*` the HOST builds by borrowing the real payload
// it already holds, and a `to_*` the GUEST copies into an owned Rust value before handing it to
// user code. Neither side ever sees the other's Rust type: `vike-model` is statically linked into
// both halves, so a `&QuoteTick` crossing the boundary would be two independently-compiled
// `repr(Rust)` layouts sharing a pointer, which is the exact thing this file exists to refuse.
// ---------------------------------------------------------------------------------------------

/// The wire mirror of `vike_marketdata::QuoteTick`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CQuoteTick {
    pub ts: i64,
    pub local_ts: i64,
    pub bid: f64,
    pub ask: f64,
    pub bid_size: f64,
    pub ask_size: f64,
    pub symbol: CStrRef,
}

impl CQuoteTick {
    pub fn of(q: &vike_marketdata::QuoteTick) -> Self {
        CQuoteTick {
            ts: q.ts,
            local_ts: q.local_ts,
            bid: q.bid,
            ask: q.ask,
            bid_size: q.bid_size,
            ask_size: q.ask_size,
            symbol: CStrRef::of(&q.symbol),
        }
    }

    pub fn to_quote_tick(&self) -> vike_marketdata::QuoteTick {
        vike_marketdata::QuoteTick {
            ts: self.ts,
            local_ts: self.local_ts,
            bid: self.bid,
            ask: self.ask,
            bid_size: self.bid_size,
            ask_size: self.ask_size,
            symbol: self.symbol.read(),
        }
    }
}

/// The wire mirror of `vike_marketdata::TradeTick`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CTradeTick {
    pub ts: i64,
    pub local_ts: i64,
    pub price: f64,
    pub size: f64,
    pub is_buyer_maker: bool,
    pub symbol: CStrRef,
}

impl CTradeTick {
    pub fn of(t: &vike_marketdata::TradeTick) -> Self {
        CTradeTick {
            ts: t.ts,
            local_ts: t.local_ts,
            price: t.price,
            size: t.size,
            is_buyer_maker: t.is_buyer_maker,
            symbol: CStrRef::of(&t.symbol),
        }
    }

    pub fn to_trade_tick(&self) -> vike_marketdata::TradeTick {
        vike_marketdata::TradeTick {
            ts: self.ts,
            local_ts: self.local_ts,
            price: self.price,
            size: self.size,
            is_buyer_maker: self.is_buyer_maker,
            symbol: self.symbol.read(),
        }
    }
}

/// The wire mirror of `vike_model::MarkTick`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CMarkTick {
    pub ts: i64,
    pub price: f64,
    /// The UNDERLYING series' own symbol — not the mount's. See `vike_model::MarkTick`.
    pub symbol: CStrRef,
}

impl CMarkTick {
    pub fn of(m: &vike_model::MarkTick) -> Self {
        CMarkTick { ts: m.ts, price: m.price, symbol: CStrRef::of(&m.symbol) }
    }

    pub fn to_mark_tick(&self) -> vike_model::MarkTick {
        vike_model::MarkTick { symbol: self.symbol.read(), price: self.price, ts: self.ts }
    }
}

/// The wire mirror of `vike_model::Fill`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CFill {
    pub ts: i64,
    /// +1 buy / -1 sell.
    pub side: i32,
    pub is_maker: bool,
    pub size: f64,
    pub price: f64,
    pub fee: f64,
    pub symbol: CStrRef,
}

impl CFill {
    pub fn of(f: &vike_model::Fill) -> Self {
        CFill {
            ts: f.ts,
            side: f.side,
            is_maker: f.is_maker,
            size: f.size,
            price: f.price,
            fee: f.fee,
            symbol: CStrRef::of(&f.symbol),
        }
    }

    pub fn to_fill(&self) -> vike_model::Fill {
        vike_model::Fill {
            side: self.side,
            size: self.size,
            price: self.price,
            fee: self.fee,
            ts: self.ts,
            is_maker: self.is_maker,
            symbol: self.symbol.read(),
        }
    }
}

/// The wire mirror of `vike_model::FlowToxicity` (three POD scalars, no string).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CFlowToxicity {
    pub bid: f64,
    pub ask: f64,
    pub ts: i64,
}

impl CFlowToxicity {
    pub fn of(f: vike_model::FlowToxicity) -> Self {
        CFlowToxicity { bid: f.bid, ask: f.ask, ts: f.ts }
    }

    pub fn to_flow(&self) -> vike_model::FlowToxicity {
        vike_model::FlowToxicity { bid: self.bid, ask: self.ask, ts: self.ts }
    }
}

// ---------------------------------------------------------------------------------------------
// `FeedStatus` as an integer, with an EXHAUSTIVE mapping in both directions.
// ---------------------------------------------------------------------------------------------

pub const FEED_STATUS_DISCONNECTED: u32 = 0;
pub const FEED_STATUS_STALE: u32 = 1;
pub const FEED_STATUS_LIVE: u32 = 2;

/// `FeedStatus` -> its wire code.
///
/// ⚠ The `match` has NO `_` arm, and that is the whole point: a variant added to
/// `vike_model::FeedStatus` is a COMPILE ERROR here rather than a silent fall-through to
/// `Disconnected`, which a plugin would then act on by pulling quotes that were never in danger.
pub fn feed_status_code(s: vike_model::FeedStatus) -> u32 {
    match s {
        vike_model::FeedStatus::Disconnected => FEED_STATUS_DISCONNECTED,
        vike_model::FeedStatus::Stale => FEED_STATUS_STALE,
        vike_model::FeedStatus::Live => FEED_STATUS_LIVE,
    }
}

/// The inverse. `None` for a code this host does not know — the guest turns that into
/// [`PluginStatus::BadParams`] rather than guessing, because every guess here is a market-health
/// claim the strategy will act on.
pub fn feed_status_from_code(code: u32) -> Option<vike_model::FeedStatus> {
    match code {
        FEED_STATUS_DISCONNECTED => Some(vike_model::FeedStatus::Disconnected),
        FEED_STATUS_STALE => Some(vike_model::FeedStatus::Stale),
        FEED_STATUS_LIVE => Some(vike_model::FeedStatus::Live),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// `OrderLifecycle` — a TAG plus a flat payload (`ABI_VERSION` 3, tier 3).
// ---------------------------------------------------------------------------------------------

pub const ORDER_EVENT_ACCEPTED: u32 = 0;
pub const ORDER_EVENT_REJECTED: u32 = 1;
pub const ORDER_EVENT_DENIED: u32 = 2;
pub const ORDER_EVENT_CANCELED: u32 = 3;
pub const ORDER_EVENT_EXPIRED: u32 = 4;
pub const ORDER_EVENT_FILLED: u32 = 5;

/// `OrderEventKind` -> its wire code. EXHAUSTIVE, no `_` arm — see [`feed_status_code`] for the
/// argument; here it is sharper still, because three of the six variants carry a `reason` and a
/// seventh added without a code would arrive as whatever the fall-through picked, with its reason
/// text silently dropped.
pub fn order_event_code(k: &vike_model::OrderEventKind) -> u32 {
    match k {
        vike_model::OrderEventKind::Accepted => ORDER_EVENT_ACCEPTED,
        vike_model::OrderEventKind::Rejected { .. } => ORDER_EVENT_REJECTED,
        vike_model::OrderEventKind::Denied { .. } => ORDER_EVENT_DENIED,
        vike_model::OrderEventKind::Canceled { .. } => ORDER_EVENT_CANCELED,
        vike_model::OrderEventKind::Expired => ORDER_EVENT_EXPIRED,
        vike_model::OrderEventKind::Filled => ORDER_EVENT_FILLED,
    }
}

/// The reason text a variant carries, BORROWED — `""` for the three that carry none. Exhaustive
/// for the same reason as [`order_event_code`], and separate from it so the two cannot disagree
/// about which variants have a payload.
pub fn order_event_reason(k: &vike_model::OrderEventKind) -> &str {
    match k {
        vike_model::OrderEventKind::Rejected { reason }
        | vike_model::OrderEventKind::Denied { reason }
        | vike_model::OrderEventKind::Canceled { reason } => reason.as_str(),
        vike_model::OrderEventKind::Accepted
        | vike_model::OrderEventKind::Expired
        | vike_model::OrderEventKind::Filled => "",
    }
}

/// Rebuild the kind from `(code, reason)`. `None` for an unknown code.
///
/// ⚠ The reason is DROPPED for the three payload-less variants rather than carried into a
/// fabricated field, and an unknown code is refused rather than defaulted: a strategy's
/// retry/refresh/abort machine keys off exactly this tag, so an invented `Canceled` would make a
/// live order look dead and free a slot that is still occupied.
pub fn order_event_from_code(code: u32, reason: String) -> Option<vike_model::OrderEventKind> {
    match code {
        ORDER_EVENT_ACCEPTED => Some(vike_model::OrderEventKind::Accepted),
        ORDER_EVENT_REJECTED => Some(vike_model::OrderEventKind::Rejected { reason }),
        ORDER_EVENT_DENIED => Some(vike_model::OrderEventKind::Denied { reason }),
        ORDER_EVENT_CANCELED => Some(vike_model::OrderEventKind::Canceled { reason }),
        ORDER_EVENT_EXPIRED => Some(vike_model::OrderEventKind::Expired),
        ORDER_EVENT_FILLED => Some(vike_model::OrderEventKind::Filled),
        _ => None,
    }
}

/// The wire mirror of `vike_model::OrderLifecycle`: a `kind` TAG plus the flat union of every
/// variant's payload, which across the six variants is exactly one optional string.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct COrderLifecycle {
    pub client_order_id: CStrRef,
    /// `None` for an order placed through the untagged `Broker` verbs — see [`COptStrRef`] for
    /// why a null pointer alone would not carry that distinction.
    pub tag: COptStrRef,
    /// One of the `ORDER_EVENT_*` codes.
    pub kind: u32,
    /// The reason text for `Rejected`/`Denied`/`Canceled`; empty for the other three.
    pub reason: CStrRef,
}

impl COrderLifecycle {
    pub fn of(ev: &vike_model::OrderLifecycle) -> Self {
        COrderLifecycle {
            client_order_id: CStrRef::of(&ev.client_order_id),
            tag: COptStrRef::of(ev.tag.as_deref()),
            kind: order_event_code(&ev.kind),
            reason: CStrRef::of(order_event_reason(&ev.kind)),
        }
    }

    /// `None` when `kind` is a code this host does not know.
    pub fn to_order_lifecycle(&self) -> Option<vike_model::OrderLifecycle> {
        let kind = order_event_from_code(self.kind, self.reason.read())?;
        Some(vike_model::OrderLifecycle {
            client_order_id: self.client_order_id.read(),
            tag: self.tag.read(),
            kind,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// The L2 BOOK CURSOR (`ABI_VERSION` 3, tier 3).
//
// ⚠ `L2Book`'s `bids`/`asks` are PRIVATE `BTreeMap<i64, f64>`s. There is no flat mirror of it to
// build — not "it would be large", but "the type does not expose its levels as data at all" — so
// the book crosses as a CURSOR, the same shape `BrokerVTable::bars_len`/`bar_at` already uses for
// the slice `Broker::bars` returns.
//
// WHAT ONE `on_order_book` DISPATCH COSTS, stated here because a cursor can look cheaper than it
// is. The host flattens its book ONCE per dispatch into two `Vec<CBookLevel>`s
// (`bid_levels + ask_levels` `BTreeMap` steps, two allocations) and lends THAT through `ctx`;
// then the guest makes `2 + 2 + bid_levels + ask_levels` C-ABI crossings and rebuilds an
// `L2Book`, which is `bid_levels + ask_levels` `BTreeMap` inserts. So it is O(levels) end to end,
// twice.
//
// It is NOT the cursor that makes the copy unavoidable: `Strategy::on_order_book` hands user code
// a concrete `&L2Book`, so SOMETHING must materialise one on the plugin's side of the boundary
// whatever shape the payload takes. What the cursor buys over the alternatives is that the HOST
// never clones its own book, no second serialisation format exists to drift, and a level is
// plain data at every step.
//
// ⚠ The host-side FLATTEN is what keeps `level_at` O(1) and is the reason `ctx` is not simply the
// `&L2Book`: a `BTreeMap` has no index, so a literal per-index seek would be `nth(i)` — O(i) —
// and a full walk would be O(levels²) node steps. A 200-level book would pay 40,000 of them per
// event to deliver 400.
// ---------------------------------------------------------------------------------------------

/// One price level as plain data — the wire mirror of `vike_marketdata::BookLevel`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CBookLevel {
    pub price: f64,
    pub qty: f64,
}

impl CBookLevel {
    /// An all-zero level, for the out-param a cursor read writes into — so a caller that ignores
    /// the returned `bool` reads a well-defined empty level rather than uninitialised stack
    /// memory. The same contract [`CBar::empty`] states for `bar_at`.
    pub fn empty() -> Self {
        CBookLevel { price: 0.0, qty: 0.0 }
    }
}

/// `side` values for the cursor below. Same convention as `Broker::submit_market`'s `side`.
pub const SIDE_BID: i32 = 1;
pub const SIDE_ASK: i32 = -1;

/// The host's book, erased — `ctx` is host-owned and valid for the duration of ONE
/// `vike_plugin_on_order_book` call, `vtable` is host-owned and `'static`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BookRef {
    pub ctx: *mut core::ffi::c_void,
    pub vtable: *const BookVTable,
}

/// What a plugin may ask the host about the book it was just handed.
#[repr(C)]
pub struct BookVTable {
    pub tick_size: extern "C" fn(*mut core::ffi::c_void) -> f64,
    pub last_seq: extern "C" fn(*mut core::ffi::c_void) -> u64,
    /// How many levels `side` ([`SIDE_BID`] / [`SIDE_ASK`]) has.
    pub side_len: extern "C" fn(*mut core::ffi::c_void, i32) -> usize,
    /// Writes level `i` of `side` into `out`, BEST FIRST (bids high->low, asks low->high).
    /// Returns false when `i` is out of range, leaving `out` untouched.
    pub level_at: extern "C" fn(*mut core::ffi::c_void, i32, usize, *mut CBookLevel) -> bool,
}

/// What every `extern "C"` entry point returns. A plugin never unwinds.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    Ok = 0,
    Panicked = 1,
    BadHandle = 2,
    BadParams = 3,
}

/// The host's broker, erased. `ctx` is an erased `&mut SimBroker`; `vtable` is
/// host-owned and `'static`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BrokerRef {
    pub ctx: *mut core::ffi::c_void,
    pub vtable: *const BrokerVTable,
}

/// The host's side of the boundary: what a plugin may call back into.
///
/// ⚠ `bars_len`/`bar_at` are a CURSOR, deliberately, because `Broker::bars` returns
/// `&[Bar]` and a slice of a non-`repr(C)` type cannot cross. Converting the whole
/// history on every call would be O(n) per access; the cursor is O(1) and lets the
/// guest cache by `index`.
#[repr(C)]
pub struct BrokerVTable {
    pub submit_market: extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64),
    pub submit_limit: extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64, f64),
    pub position: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> f64,
    pub price: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> f64,
    pub equity: extern "C" fn(*mut core::ffi::c_void) -> f64,
    pub index: extern "C" fn(*mut core::ffi::c_void) -> usize,
    pub now: extern "C" fn(*mut core::ffi::c_void) -> i64,
    pub bars_len: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> usize,
    /// Writes bar `i` of `symbol` into `out`. Returns false when `i` is out of range.
    pub bar_at: extern "C" fn(*mut core::ffi::c_void, *const u8, usize, usize, *mut CBar) -> bool,
    pub quote_vwap: extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64) -> COptF64,
    pub depth_within_price:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64) -> f64,

    // ---- HftBroker (vike_model::HftBroker: Broker) — added at ABI_VERSION 2 ----
    //
    // The real user-strategy entry contract is `build<B: HftBroker + 'static>`, not `Broker`
    // alone, so a `HostBroker` wrapping only the eleven slots above cannot satisfy it. These four
    // mirror `HftBroker`'s four REQUIRED methods exactly (no default bodies on that trait to fall
    // back on, unlike `Broker::quote_vwap`/`depth_within_price` above).
    /// `HftBroker::position(&self) -> f64` — deliberately NO symbol, unlike [`Self::position`]
    /// above: the HFT mount pins one `(venue, symbol)` series, so there is nothing to name.
    pub hft_position: extern "C" fn(*mut core::ffi::c_void) -> f64,
    /// `HftBroker::submit_limit_tagged(&mut self, tag, side, qty, price)` — `tag` crosses as the
    /// same borrowed `(ptr, len)` shape every other string in this ABI uses.
    pub submit_limit_tagged: extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64, f64),
    /// `HftBroker::modify_tagged(&mut self, tag, new_qty, new_price)` — the two `Option<f64>`
    /// fields flatten to [`COptF64`] exactly as [`Self::quote_vwap`]'s return does.
    pub modify_tagged: extern "C" fn(*mut core::ffi::c_void, *const u8, usize, COptF64, COptF64),
    /// `HftBroker::cancel_tagged(&mut self, tag)`.
    pub cancel_tagged: extern "C" fn(*mut core::ffi::c_void, *const u8, usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `CBar` must be plain data: no padding surprises, no Rust types.
    /// Pinned because a field added without `#[repr(C)]` discipline silently
    /// changes the layout both sides agreed on.
    #[test]
    fn cbar_layout_is_pinned() {
        assert_eq!(std::mem::size_of::<COptF64>(), 16);
        assert_eq!(std::mem::align_of::<CBar>(), 8);
        // ts + 5 f64 + 3 COptF64 + symbol ptr/len
        assert_eq!(std::mem::size_of::<CBar>(), 8 + 5 * 8 + 3 * 16 + 16);
    }

    // `abi_version_is_nonzero` used to live here as a `#[test]` asserting `ABI_VERSION > 0`.
    // `clippy::assertions_on_constants` (a `-D warnings` merge-gate lint) correctly flags an
    // `assert!` whose condition is knowable at compile time — the same claim now lives as a
    // `const _: () = assert!(..)` beside `ABI_VERSION`'s own declaration, which is CHECKED ON
    // EVERY BUILD rather than only under `cargo test`, so nothing was lost by deleting the test.

    /// `from_bar` -> `to_bar` must be lossless: every optional survives, including a `None` one
    /// (which must NOT come back as `Some(NaN)` — `COptF64`'s `present` flag, not the NaN sentinel
    /// in its `value`, is what `Into<Option<f64>>` reads).
    #[test]
    fn cbar_round_trip_preserves_optionals_and_symbol() {
        let original = vike_marketdata::Bar {
            ts: 1_700_000_000_000,
            open: 100.0,
            high: 101.5,
            low: 99.5,
            close: 100.25,
            volume: 42.0,
            funding: Some(0.01),
            bid: None,
            ask: Some(1.6),
            symbol: Some("BTCUSDT".to_string()),
        };

        let c = CBar::from_bar(&original);
        let round_tripped = c.to_bar();

        assert_eq!(round_tripped, original);
        assert_eq!(round_tripped.funding, Some(0.01));
        assert_eq!(round_tripped.bid, None, "an absent optional must not come back as Some(NaN)");
        assert_eq!(round_tripped.ask, Some(1.6));
        assert_eq!(round_tripped.symbol.as_deref(), Some("BTCUSDT"));
    }

    /// Track A review finding #1: the null-`symbol_ptr` branch (a `Bar` whose `symbol` is `None`)
    /// was covered by no test — only the `Some("BTCUSDT")` path ran. `from_bar` must produce a
    /// null `symbol_ptr`/zero `symbol_len` for a `None` symbol, and `to_bar` must read that back
    /// as `None` rather than dereferencing a null pointer or fabricating an empty string.
    #[test]
    fn cbar_round_trip_preserves_a_missing_symbol() {
        let original = vike_marketdata::Bar {
            ts: 1,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        };

        let c = CBar::from_bar(&original);
        assert!(c.symbol_ptr.is_null(), "a None symbol must cross as a null pointer");
        assert_eq!(c.symbol_len, 0);

        let round_tripped = c.to_bar();
        assert_eq!(round_tripped, original);
        assert_eq!(round_tripped.symbol, None);
    }

    // ---- ABI_VERSION 3: the tier-2 mirrors ----

    /// Every field of every tier-2 mirror must survive the round trip. One test over all five
    /// rather than five near-identical ones: the failure they guard against is a field FORGOTTEN
    /// in `of` or in `to_*`, and a forgotten field is equally invisible in whichever mirror it
    /// sits in. Every value below is distinct and non-default, so a field left at `Default` or
    /// copied from its neighbour fails rather than passing on a coincidence.
    #[test]
    fn the_tier_two_mirrors_round_trip_every_field() {
        let q = vike_marketdata::QuoteTick {
            ts: 1_700_000_000_001,
            local_ts: 1_700_000_000_002,
            bid: 99.5,
            ask: 100.5,
            bid_size: 3.25,
            ask_size: 4.75,
            symbol: "BTCUSDT".to_string(),
        };
        assert_eq!(CQuoteTick::of(&q).to_quote_tick(), q);

        let t = vike_marketdata::TradeTick {
            ts: 1_700_000_000_003,
            local_ts: 1_700_000_000_004,
            price: 100.25,
            size: 0.5,
            is_buyer_maker: true,
            symbol: "ETHUSDT".to_string(),
        };
        assert_eq!(CTradeTick::of(&t).to_trade_tick(), t);

        let m = vike_model::MarkTick {
            symbol: "btcusdt".to_string(),
            price: 64_321.5,
            ts: 1_700_000_000_005,
        };
        assert_eq!(CMarkTick::of(&m).to_mark_tick(), m);

        let f = vike_model::Fill {
            side: -1,
            size: 2.5,
            price: 101.75,
            fee: 0.0625,
            ts: 1_700_000_000_006,
            is_maker: true,
            symbol: "SOLUSDT".to_string(),
        };
        assert_eq!(CFill::of(&f).to_fill(), f);

        let fl = vike_model::FlowToxicity { bid: 0.25, ask: 0.75, ts: 1_700_000_000_007 };
        assert_eq!(CFlowToxicity::of(fl).to_flow(), fl);
    }

    /// An EMPTY symbol must come back empty, not as a panic and not as a fabricated value. The
    /// tick types carry `symbol: String` (not `Option<String>` like `Bar`), and "" is the
    /// documented single-symbol-path value — so this is the ordinary case on those paths, not an
    /// edge one. It exercises [`read_borrowed_str`]'s zero-length early return, which is the
    /// branch that keeps a null pointer from ever reaching `from_raw_parts`.
    #[test]
    fn an_empty_symbol_crosses_as_empty_rather_than_panicking() {
        let q = vike_marketdata::QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: String::new(),
        };
        assert_eq!(CQuoteTick::of(&q).to_quote_tick().symbol, "");
    }

    // ---- ABI_VERSION 3: the integer mappings ----

    /// Every `FeedStatus` variant must survive its code, and an unknown code must be REFUSED
    /// rather than defaulted — a fabricated `Disconnected` would make a strategy pull quotes that
    /// were never in danger.
    #[test]
    fn every_feed_status_round_trips_and_an_unknown_code_is_refused() {
        for s in [
            vike_model::FeedStatus::Disconnected,
            vike_model::FeedStatus::Stale,
            vike_model::FeedStatus::Live,
        ] {
            assert_eq!(feed_status_from_code(feed_status_code(s)), Some(s));
        }
        assert_eq!(feed_status_from_code(3), None);
        assert_eq!(feed_status_from_code(u32::MAX), None);
    }

    /// Every `OrderEventKind` variant must survive `(code, reason)`, INCLUDING the reason text of
    /// the three that carry one — and an unknown code must be refused. A `Canceled` invented from
    /// a code this host does not know would make a live order look dead and free a slot that is
    /// still occupied.
    #[test]
    fn every_order_event_kind_round_trips_with_its_reason() {
        let kinds = [
            vike_model::OrderEventKind::Accepted,
            vike_model::OrderEventKind::Rejected { reason: "min notional".to_string() },
            vike_model::OrderEventKind::Denied { reason: "risk gate veto".to_string() },
            vike_model::OrderEventKind::Canceled { reason: "operator pull".to_string() },
            vike_model::OrderEventKind::Expired,
            vike_model::OrderEventKind::Filled,
        ];
        for k in &kinds {
            let code = order_event_code(k);
            let reason = order_event_reason(k).to_string();
            assert_eq!(order_event_from_code(code, reason).as_ref(), Some(k));
        }
        assert_eq!(order_event_from_code(6, String::new()), None);
    }

    /// ...and the whole `OrderLifecycle`, through the mirror, with the `tag` present and absent.
    /// The absent case is the one that would silently pass on a null-pointer heuristic: `Some("")`
    /// and `None` are different facts about an order, and only `COptStrRef::present` separates
    /// them.
    #[test]
    fn an_order_lifecycle_round_trips_including_an_absent_tag() {
        let tagged = vike_model::OrderLifecycle {
            client_order_id: "vike-1".to_string(),
            tag: Some("bid-1".to_string()),
            kind: vike_model::OrderEventKind::Canceled { reason: "replaced".to_string() },
        };
        assert_eq!(COrderLifecycle::of(&tagged).to_order_lifecycle(), Some(tagged.clone()));

        let untagged = vike_model::OrderLifecycle {
            client_order_id: "vike-2".to_string(),
            tag: None,
            kind: vike_model::OrderEventKind::Accepted,
        };
        let back = COrderLifecycle::of(&untagged).to_order_lifecycle();
        assert_eq!(back, Some(untagged));

        let empty_tag = vike_model::OrderLifecycle {
            client_order_id: "vike-3".to_string(),
            tag: Some(String::new()),
            kind: vike_model::OrderEventKind::Filled,
        };
        assert_eq!(
            COrderLifecycle::of(&empty_tag).to_order_lifecycle(),
            Some(empty_tag),
            "an EMPTY tag is not an ABSENT tag — see COptStrRef's own doc"
        );
    }

    /// A `COrderLifecycle` carrying a kind code this host does not know is refused whole, rather
    /// than delivered with a guessed kind.
    #[test]
    fn an_unknown_lifecycle_kind_refuses_the_whole_event() {
        let coid = "vike-9";
        let ev = COrderLifecycle {
            client_order_id: CStrRef::of(coid),
            tag: COptStrRef::of(None),
            kind: 99,
            reason: CStrRef::empty(),
        };
        assert!(ev.to_order_lifecycle().is_none());
    }
}
