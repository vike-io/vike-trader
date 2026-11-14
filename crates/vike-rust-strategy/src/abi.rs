//! The C-ABI vocabulary. Every type here is `#[repr(C)]` and every field is plain data.
//!
//! ⚠ The rule this file exists to hold: NO Rust type crosses by value. Not `Box`, not
//! `String`, not `Option<T>`, not `&[T]`. Strings cross as `(ptr, len)` and are never
//! NUL-terminated, because Rust strings are not. An `Option` of a scalar or of a mirror flattens
//! to [`OptionAbi`] of it.
//!
//! ⚠ The type declarations below are pure data and contain no `unsafe`. [`BarAbi::to_bar`] is
//! the one exception in this file — it reads back through a borrowed `(ptr, len)` pair, which
//! is the one place a `BarAbi` genuinely needs the raw pointer to be valid. That is expected in
//! this crate (`crates/vike-ops/tests/hygiene/unsafe_and_toolchain_gate/pins.rs`'s `UNSAFE_EXEMPT` names
//! it, per the design at
//! `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`), and the
//! `// SAFETY:` comment at the site names who owns the bytes and for how long.

/// Bumped whenever a vtable's shape or a signature changes. The loader refuses a
/// mismatch outright — it is the cheap guard that catches a DELIBERATE change, while
/// the toolchain fingerprint catches an accidental one.
///
/// ⚠ **1 -> 2: `BrokerVTableAbi` grew four slots.** The real user-strategy entry contract is
/// `pub fn build<B: vike_model::HftBroker + 'static>(..)`, not `Broker` alone —
/// `HftBroker: Broker` adds `position(&self) -> f64` (no symbol; the mount pins one
/// `(venue, symbol)` series), `submit_limit_tagged`, `modify_tagged` and `cancel_tagged`. A
/// `HostBroker` implementing only `Broker` cannot satisfy that bound, so the cdylib template
/// (Track B) could never instantiate a real user strategy against it. This bump — and the four
/// new `BrokerVTableAbi` fields below — is what makes `HostBroker` (`guest.rs`) actually implement
/// `HftBroker` rather than merely `Broker`.
///
/// ⚠ **2 -> 3: `StrategyVTableAbi` grew THIRTEEN dispatch slots** — every remaining
/// `vike_model::Strategy` seam method except the three whose payload is a `serde_json::Value` or
/// a RETURNED owned value (`params`, `save_state`, `load_state`; see
/// [`crate::host::UNWIRED_HOOKS`] for why those three are still refused at build time). ONE bump
/// for the whole batch rather than one per hook, deliberately: this version's job is to refuse a
/// strategy file whose vtable SHAPE this host does not speak, and a strategy file built against the two-slot
/// vtable is equally unloadable whichever of the thirteen it is missing — thirteen bumps would
/// have produced thirteen identical refusals of the same artifact.
///
/// ⚠ **NOT bumped when the user entry contract moved from `&toml::Value` to
/// `&vike_model::params::Params`.** That changed the SOURCE a user file compiles against, not this
/// binary contract: `create` still takes `(*const u8, usize)` and the bytes are still the TOML
/// document the host renders, so an artifact compiled before the move parses the same text with the
/// same `toml` and runs unchanged. A bump would refuse every such working artifact for no shape it
/// cannot speak; a user source still naming `toml::Value` fails at its next BUILD instead, which is
/// where a source change belongs.
///
/// ⚠ **3 -> 4 (owner 2026-11-09): `OrderBroker::contract_value`, so a strategy file values PnL at the
/// contract's unit; a v3 strategy file is refused and must be rebuilt from its unchanged source.**
/// `BrokerVTableAbi` grew the one `contract_value` slot. A v3 artifact would run, but its strategy was
/// compiled against a `Broker` with no such read, so its position executor would fold every
/// contract at a multiplier of 1: silently wrong PnL on a future or an inverse contract, which is
/// why the bump refuses it rather than letting it load (the refusal test moved one version on, to
/// the 4 -> 5 bump below).
///
/// ⚠ **4 -> 5 (owner 2026-11-10, decision 0269's later wave: "all three ways write orders the same
/// way"): `vike_model::OrderBroker` and `OrderBroker::instrument_grid`, so a strategy file writes orders in the
/// order language (`vike_tactics::RuleRun`) exactly as a built-in does; a v4 strategy file is refused and must
/// be rebuilt from its unchanged source.** `BrokerVTableAbi` grew ten slots: the grid read and the nine
/// `OrderBroker` verbs, the four with a trait default among them, which the guest forwards rather than
/// leaving on that default (the default is the LIVE meaning, and `OrderBrokerSimulator` overrides all four, so
/// a strategy file falling back to it would backtest differently from the same source compiled in). `FillAbi`
/// grew `tag`, because `RuleRun` matches a fill to its order by it. A v4 artifact is refused for the 3 -> 4
/// reason: its `HostBroker` left `instrument_grid` on the trait default, so a strategy in it that reads
/// the grid reads `None` where the same source compiled in reads the engine's tick, and snaps or sizes
/// differently with no signal (the refusal test moved one version on, to the 5 -> 6 bump below).
///
/// ⚠ **5 -> 6 (decision 0271, the strategy side): a strategy file declares the stored series it reads
/// (`Strategy::inputs`) and reads them (`OrderBroker::input_bars`, and so `OrderBroker::input`) and its closed
/// trades (`OrderBroker::closed_trades`) exactly as a built-in does; a v5 strategy file is refused and must be
/// rebuilt from its unchanged source.** `StrategyVTableAbi` grew the `inputs` export (the names as JSON
/// text the strategy file lends, [`OptionAbi`] of a [`StrAbi`]); `BrokerVTableAbi` grew four slots, each read's
/// count and its item at an index ([`OptionAbi`] of a [`BarAbi`] and of a [`TradeClosedRowAbi`]), the
/// `bars_len`/`bar_at` cursor answered by value. A v5 artifact is refused for the 3 -> 4 reason: its
/// `HostBroker` left both reads on the trait defaults and the host never asked its `inputs`, so a
/// strategy in it reads no input and no ledger (`None`) where the same source compiled in reads the
/// engine's, and trades differently with no signal
/// (the refusal test moved one version on, to the 6 -> 7 bump below).
///
/// ⚠ **6 -> 7 (owner 2026-11-10: the chase and the iceberg are order kinds, and every Rust strategy
/// is rebuilt once for them): a strategy file places a pegged
/// order and an iceberg (`vike_model::OrderPlacement::Pegged` and `Iceberg`, the order language's
/// `.chase()` and `.iceberg(visible)`) exactly as a built-in does; a v6 strategy file is refused and must be
/// rebuilt from its unchanged source.** [`OrderPlacementAbi`] grew three fields (the peg's anchor and
/// offset, the visible size) and two kind codes, and `BrokerVTableAbi` the `deny` slot (the language's
/// refusal of a combination it cannot send, `OrderBroker::deny`). It crosses BY VALUE in `BrokerVTableAbi::place` and
/// `place_exit_leg`, so a v6 artifact would hand this host a struct of the old size (the refusal test
/// moved one version on, to the 7 -> 8 bump below).
///
/// ⚠ **7 -> 8 (the one broker interface, decision 0277's final seam, W-b step 8): a strategy file's
/// orders name their symbol, it reads the run's universe, declares the symbols it trades and keeps
/// its live params and its restart state exactly as a built-in does; a v7 strategy file is refused
/// and must be rebuilt from its source.** Every [`BrokerVTableAbi`] slot that places, refuses or reads an order's book
/// (`place`, `place_exit_leg`, `place_at_close`, `place_close_from_fill`, `place_group`, `deny`,
/// `average_price`) carries the symbol, the `symbol_at` slot answers `OrderBroker::symbols`, and the six
/// slots of the retired verbs (`submit_market`, `submit_limit`, `hft_position`, `submit_limit_tagged`,
/// `modify_tagged`, `cancel_tagged`) are gone: `guest::HostBroker` implements `OrderBroker` and no
/// longer `HftBroker`, so a source whose `build` still bounds `B: HftBroker` fails its next Build with
/// the one-line fix (`vike_model::host_build::moved_name::renamed_name_hint`). `host::StrategyVTableAbi`
/// grew four exports (`Strategy::symbols`, `params`, `save_state`, `load_state`), each JSON text, so no
/// `Strategy` hook is refused at build any more. The mirrors took their `...Abi` names. A v7 artifact
/// would hand this host a `place` with no symbol where the slot reads one (the refusal test moved one
/// version on, to the 8 -> 9 bump below).
///
/// ⚠ **8 -> 9 (plan 2b, every strategy is a file): `create` takes the MOUNT FACTS beside the params,
/// and a strategy file answers its own refusal and its effective-params line; a v8 strategy file is
/// refused and must be rebuilt from its source.** `host::StrategyVTableAbi::create` takes a second
/// borrowed document, the mount's `vike_model::params::MountFacts` as JSON (the symbol a `rule` file
/// trades, the venue, the interval, the tick size, the static and the account maker round trip), and
/// the vtable grew the `refuse` export (asked over a params document before any strategy is created)
/// and the `echo` export (lent from the handle), and the loader hands every file the host's log slot
/// (`LogFn`, the `vike_strategy_set_log` export), so a file's own `tracing` events are logged by the
/// host. A v8
/// artifact's `create` would read two arguments where the host passes four and would export neither
/// (`crates/vike-rust-strategy/tests/load_refusals.rs`'s
/// `a_strategy_file_built_against_abi_8_is_refused_naming_both_versions`).
pub const ABI_VERSION: u32 = 9;

/// A zero version cannot be distinguished from an unset field, so it must never be reachable.
/// Checked at COMPILE TIME — stronger than a `#[test]` asserting on a `const` (which
/// `clippy::assertions_on_constants`, a `-D warnings` merge-gate lint, correctly flags: the
/// condition is knowable at compile time, so a runtime `assert!` on it tests nothing an ordinary
/// build does not already prove) — and enforced on EVERY build, not only under `cargo test`.
const _: () =
    assert!(ABI_VERSION > 0, "a zero version cannot be distinguished from an unset field");

/// `Option<T>` of a `repr(C)` `T` as plain data: what a read answers BY VALUE, so no side writes
/// through the other's pointer. `present == false` means `None`, and then `value` is a blank the
/// caller chose and is never read. `OptionAbi<f64>` (an `Option<f64>`, its blank NaN) and
/// `OptionAbi<StrAbi>` (a borrowed `Option<&str>`) are the two this ABI uses most.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OptionAbi<T> {
    pub value: T,
    pub present: bool,
}

impl<T> OptionAbi<T> {
    /// `v`, with `blank` standing in for a `None`.
    pub fn new(v: Option<T>, blank: T) -> Self {
        match v {
            Some(value) => OptionAbi { value, present: true },
            None => OptionAbi { value: blank, present: false },
        }
    }

    pub fn get(self) -> Option<T> {
        if self.present { Some(self.value) } else { None }
    }
}

impl From<Option<f64>> for OptionAbi<f64> {
    fn from(v: Option<f64>) -> Self {
        OptionAbi::new(v, f64::NAN)
    }
}

impl From<OptionAbi<f64>> for Option<f64> {
    fn from(v: OptionAbi<f64>) -> Self {
        v.get()
    }
}

pub const NOTIONAL_LINEAR: u32 = 0;
pub const NOTIONAL_INVERSE: u32 = 1;

/// `Option<vike_model::ContractValue>` as plain data (`ABI_VERSION` 4). `kind` is one of the
/// `NOTIONAL_*` codes; `present == false` means `None`, and then the other two are meaningless.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ContractValueOptionAbi {
    pub kind: u32,
    pub per_unit: f64,
    pub present: bool,
}

impl From<Option<vike_model::ContractValue>> for ContractValueOptionAbi {
    fn from(v: Option<vike_model::ContractValue>) -> Self {
        match v {
            Some(v) => ContractValueOptionAbi {
                // EXHAUSTIVE, no `_` arm: a new kind is a compile error here, never a silent linear.
                kind: match v.kind {
                    vike_model::NotionalKind::Linear => NOTIONAL_LINEAR,
                    vike_model::NotionalKind::Inverse => NOTIONAL_INVERSE,
                },
                per_unit: v.per_unit,
                present: true,
            },
            None => {
                ContractValueOptionAbi { kind: NOTIONAL_LINEAR, per_unit: f64::NAN, present: false }
            }
        }
    }
}

impl From<ContractValueOptionAbi> for Option<vike_model::ContractValue> {
    /// `None` for a code this side does not know, never a guessed kind: an inverse contract read as
    /// linear is wrong by the price.
    fn from(v: ContractValueOptionAbi) -> Self {
        if !v.present {
            return None;
        }
        match v.kind {
            NOTIONAL_LINEAR => Some(vike_model::ContractValue::linear(v.per_unit)),
            NOTIONAL_INVERSE => Some(vike_model::ContractValue::inverse(v.per_unit)),
            _ => None,
        }
    }
}

/// Copy a borrowed `(ptr, len)` pair into an owned `String`, CHECKED rather than lossy.
///
/// ⚠ **The ONE `unsafe` site in this file, and deliberately so.** Every mirror below that carries
/// a string reads it back through here instead of spelling its own `from_raw_parts` (and so does
/// `host/thunks.rs`'s `read_str`, the host side, and its order-list read at `ABI_VERSION` 5), for
/// the same reason `loader.rs`'s `bind!` is a
/// single textual site: the unsafe act is identical at every one of them, and thirteen copies of it
/// would be thirteen places for the contract to be restated slightly differently. `crates/vike-ops/tests/
/// unsafe_and_toolchain_gate.rs`'s `UNSAFE_SITES` row for this file therefore stays at 1 across
/// the whole `ABI_VERSION` 3 widening.
///
/// The bytes crossing this ABI are supposed to be an exact copy of a live Rust `&str`'s bytes
/// (this module's header states the contract), so a decode failure is a CONTRACT VIOLATION worth
/// a loud panic rather than a quietly-corrupted symbol a strategy might then trade on — the
/// argument [`BarAbi::to_bar`]'s own doc makes at length, unchanged by the move.
///
/// ⚠ **Caller contract: only ever call this from inside a `catch_unwind`.** It panics, and every
/// call site is reached from an `extern "C"` frame. The template's dispatch exports and
/// `host.rs`'s `guarded` thunks both satisfy it; nothing here can enforce it.
///
/// `what` names the bytes in the panic (`"borrowed string"` on this side, `"symbol"` in the host's
/// thunks); it is formatted only on the failure path, never per call.
pub(crate) fn read_borrowed_str(ptr: *const u8, len: usize, what: &str) -> String {
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
    // `expect`'s own `"{msg}: {err:?}"` shape, spelled out so `what` costs nothing on success.
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|e| {
        panic!("strategy file ABI contract violated: {what} bytes are not valid UTF-8: {e:?}")
    })
}

/// A borrowed `&str` as plain data: `(ptr, len)`, never NUL-terminated.
///
/// ⚠ [`BarAbi`] predates this type and inlines its own `symbol_ptr`/`symbol_len` pair instead.
/// That is left alone on purpose — `BarAbi`'s layout is pinned by a test two other crates' fixtures
/// were written against, and re-spelling it would be churn with no behaviour attached. Every
/// mirror added at `ABI_VERSION` 3 uses this type.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StrAbi {
    pub ptr: *const u8,
    pub len: usize,
}

impl StrAbi {
    /// Borrows `s`'s bytes. Valid only for as long as `s` is alive and unmoved.
    pub fn of(s: &str) -> Self {
        StrAbi { ptr: s.as_ptr(), len: s.len() }
    }

    /// The empty string, as a null pointer and a zero length — what every mirror writes for a
    /// field a variant does not carry.
    pub fn empty() -> Self {
        StrAbi { ptr: std::ptr::null(), len: 0 }
    }

    /// Copy the borrowed bytes into an owned `String`. See [`read_borrowed_str`] for the contract.
    pub fn read(&self) -> String {
        read_borrowed_str(self.ptr, self.len, "borrowed string")
    }
}

/// The host's log slot (`ABI_VERSION` 9): what a strategy file's own `tracing` events are handed to.
/// A strategy file links its OWN `tracing`, whose dispatcher is a process-global of that copy, so
/// without this its events reach no subscriber. `level` is a [`log_level_code`]; `target` (the
/// event's module path inside the file) and `message` (its message and fields as text) are borrowed
/// for the call.
pub type LogFn = extern "C" fn(level: u32, target: StrAbi, message: StrAbi);

/// A `tracing` level as the number [`LogFn`] carries: `ERROR` 1 to `TRACE` 5 (0 is "off", a max
/// level only).
pub fn log_level_code(level: tracing::Level) -> u32 {
    match level {
        tracing::Level::ERROR => 1,
        tracing::Level::WARN => 2,
        tracing::Level::INFO => 3,
        tracing::Level::DEBUG => 4,
        tracing::Level::TRACE => 5,
    }
}

/// The filter a max-level code stands for: 0 admits nothing, 1-5 up to that [`log_level_code`], and
/// a code above 5 everything.
pub fn log_level_filter(code: u32) -> tracing::level_filters::LevelFilter {
    use tracing::level_filters::LevelFilter;
    match code {
        0 => LevelFilter::OFF,
        1 => LevelFilter::ERROR,
        2 => LevelFilter::WARN,
        3 => LevelFilter::INFO,
        4 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    }
}

/// [`log_level_filter`]'s inverse, for the most verbose level a host's subscriber admits.
pub fn log_filter_code(filter: tracing::level_filters::LevelFilter) -> u32 {
    filter.into_level().map_or(0, log_level_code)
}

/// A borrowed `Option<&str>`. `present == false` means `None`, and then the pair is meaningless.
///
/// ⚠ The explicit flag is load-bearing rather than defensive: `Some("")`'s `as_ptr()` is a
/// non-null dangling pointer with length 0, and `None` is naturally spelled as a null pointer with
/// length 0 — so "is the pointer null" is a property of the ALLOCATOR, not of the option, and
/// reading the two apart from the pair alone would be reading a guarantee Rust does not make.
/// `OrderEvent::tag` is exactly this case: `None` means "placed untagged", which is a different
/// fact from "tagged with the empty string".
impl OptionAbi<StrAbi> {
    pub fn of(v: Option<&str>) -> Self {
        OptionAbi::new(v.map(StrAbi::of), StrAbi::empty())
    }

    pub fn read(&self) -> Option<String> {
        if self.present { Some(self.value.read()) } else { None }
    }
}

/// The wire mirror of `vike_marketdata::Bar`.
///
/// ⚠ `symbol` is a borrowed `(ptr, len)` owned by the CALLER for the duration of the
/// call only. A strategy file that wants to keep it must copy it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BarAbi {
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub funding: OptionAbi<f64>,
    pub bid: OptionAbi<f64>,
    pub ask: OptionAbi<f64>,
    pub symbol_ptr: *const u8,
    pub symbol_len: usize,
}

impl BarAbi {
    /// An all-zero `BarAbi` with every optional absent and no symbol. A strategy file (or a host cursor
    /// site like `BrokerVTableAbi::bar_at`) uses this to build the out-param it writes into, so a
    /// caller who never checks the returned `bool` still reads a well-defined, empty bar rather
    /// than uninitialized stack memory.
    pub fn empty() -> Self {
        BarAbi {
            timestamp: 0,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close: 0.0,
            volume: 0.0,
            funding: OptionAbi { value: 0.0, present: false },
            bid: OptionAbi { value: 0.0, present: false },
            ask: OptionAbi { value: 0.0, present: false },
            symbol_ptr: std::ptr::null(),
            symbol_len: 0,
        }
    }

    /// Borrows `b`'s symbol bytes. The returned `BarAbi`'s `symbol_ptr`/`symbol_len` are valid only
    /// for as long as `b` (and its `symbol` `String`, unmoved) is alive — the same contract this
    /// module's doc states for every `BarAbi` that crosses the boundary.
    pub fn from_bar(b: &vike_marketdata::Bar) -> Self {
        let (symbol_ptr, symbol_len) = match &b.symbol {
            Some(s) => (s.as_ptr(), s.len()),
            None => (std::ptr::null(), 0),
        };
        BarAbi {
            timestamp: b.timestamp,
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
    /// (a stale pointer, a wrong length, a hand-built `BarAbi` that never went through
    /// [`BarAbi::from_bar`]) would silently get replacement characters instead of a signal anything
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
    /// across a C-ABI boundary is UB" — the design doc), every strategy file's generated `extern "C"`
    /// entry point wraps the ENTIRE call into user strategy code — which is what calls
    /// `HostBroker::bars`, which is what calls this — in `catch_unwind`, turning a caught panic
    /// into [`StrategyFileStatusAbi::Panicked`]. That wrapper is Track B's, is not on this branch, and this
    /// file cannot see or verify it exists. Anything that calls `to_bar` OUTSIDE such a wrapper —
    /// directly, in a test, or from a future call site — takes on the "a panic across a C-ABI
    /// boundary is UB" risk itself; this doc is the place that risk is written down, since the
    /// type system cannot carry it.
    pub fn to_bar(&self) -> vike_marketdata::Bar {
        let symbol = if self.symbol_ptr.is_null() {
            None
        } else {
            Some(read_borrowed_str(self.symbol_ptr, self.symbol_len, "borrowed string"))
        };
        vike_marketdata::Bar {
            timestamp: self.timestamp,
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
// Each follows `BarAbi`'s shape exactly — a `from_*` the HOST builds by borrowing the real payload
// it already holds, and a `to_*` the GUEST copies into an owned Rust value before handing it to
// user code. Neither side ever sees the other's Rust type: `vike-model` is statically linked into
// both halves, so a `&TickQuote` crossing the boundary would be two independently-compiled
// `repr(Rust)` layouts sharing a pointer, which is the exact thing this file exists to refuse.
// ---------------------------------------------------------------------------------------------

/// The wire mirror of `vike_marketdata::TickQuote`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TickQuoteAbi {
    pub timestamp: i64,
    pub local_timestamp: i64,
    pub bid: f64,
    pub ask: f64,
    pub bid_size: f64,
    pub ask_size: f64,
    pub symbol: StrAbi,
}

impl TickQuoteAbi {
    pub fn of(q: &vike_marketdata::TickQuote) -> Self {
        TickQuoteAbi {
            timestamp: q.timestamp,
            local_timestamp: q.local_timestamp,
            bid: q.bid,
            ask: q.ask,
            bid_size: q.bid_size,
            ask_size: q.ask_size,
            symbol: StrAbi::of(&q.symbol),
        }
    }

    pub fn to_quote_tick(&self) -> vike_marketdata::TickQuote {
        vike_marketdata::TickQuote {
            timestamp: self.timestamp,
            local_timestamp: self.local_timestamp,
            bid: self.bid,
            ask: self.ask,
            bid_size: self.bid_size,
            ask_size: self.ask_size,
            symbol: self.symbol.read(),
        }
    }
}

/// The wire mirror of `vike_marketdata::TickTrade`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TickTradeAbi {
    pub timestamp: i64,
    pub local_timestamp: i64,
    pub price: f64,
    pub size: f64,
    pub is_buyer_maker: bool,
    pub symbol: StrAbi,
}

impl TickTradeAbi {
    pub fn of(t: &vike_marketdata::TickTrade) -> Self {
        TickTradeAbi {
            timestamp: t.timestamp,
            local_timestamp: t.local_timestamp,
            price: t.price,
            size: t.size,
            is_buyer_maker: t.is_buyer_maker,
            symbol: StrAbi::of(&t.symbol),
        }
    }

    pub fn to_trade_tick(&self) -> vike_marketdata::TickTrade {
        vike_marketdata::TickTrade {
            timestamp: self.timestamp,
            local_timestamp: self.local_timestamp,
            price: self.price,
            size: self.size,
            is_buyer_maker: self.is_buyer_maker,
            symbol: self.symbol.read(),
        }
    }
}

/// The wire mirror of `vike_model::TickMark`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TickMarkAbi {
    pub timestamp: i64,
    pub price: f64,
    /// The UNDERLYING series' own symbol — not the mount's. See `vike_model::TickMark`.
    pub symbol: StrAbi,
}

impl TickMarkAbi {
    pub fn of(m: &vike_model::TickMark) -> Self {
        TickMarkAbi { timestamp: m.timestamp, price: m.price, symbol: StrAbi::of(&m.symbol) }
    }

    pub fn to_mark_tick(&self) -> vike_model::TickMark {
        vike_model::TickMark {
            symbol: self.symbol.read(),
            price: self.price,
            timestamp: self.timestamp,
        }
    }
}

/// The wire mirror of `vike_model::Fill`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FillAbi {
    pub timestamp: i64,
    /// +1 buy / -1 sell.
    pub side: i32,
    pub is_maker: bool,
    pub size: f64,
    pub price: f64,
    pub fee: f64,
    pub symbol: StrAbi,
    /// The tag of the order that filled (`ABI_VERSION` 5); `None` for an untagged one. The order
    /// language matches a fill to its book by it, so dropping it would leave every order open.
    pub tag: OptionAbi<StrAbi>,
}

impl FillAbi {
    pub fn of(f: &vike_model::Fill) -> Self {
        FillAbi {
            timestamp: f.timestamp,
            side: f.side,
            is_maker: f.is_maker,
            size: f.size,
            price: f.price,
            fee: f.fee,
            symbol: StrAbi::of(&f.symbol),
            tag: OptionAbi::<StrAbi>::of(f.tag.as_deref()),
        }
    }

    pub fn to_fill(&self) -> vike_model::Fill {
        vike_model::Fill {
            side: self.side,
            size: self.size,
            price: self.price,
            fee: self.fee,
            timestamp: self.timestamp,
            is_maker: self.is_maker,
            symbol: self.symbol.read(),
            tag: self.tag.read(),
        }
    }
}

/// The wire mirror of `vike_model::FlowToxicity` (three POD scalars, no string).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FlowToxicityAbi {
    pub bid: f64,
    pub ask: f64,
    pub timestamp: i64,
}

impl FlowToxicityAbi {
    pub fn of(f: vike_model::FlowToxicity) -> Self {
        FlowToxicityAbi { bid: f.bid, ask: f.ask, timestamp: f.timestamp }
    }

    pub fn to_flow(&self) -> vike_model::FlowToxicity {
        vike_model::FlowToxicity { bid: self.bid, ask: self.ask, timestamp: self.timestamp }
    }
}

// ---------------------------------------------------------------------------------------------
// The order seam's mirrors (`ABI_VERSION` 5): `OrderBroker::instrument_grid`'s answer and the order
// `OrderBroker::place` and its siblings take. `OrderBroker::place_group`'s legs cross as JSON text
// instead (`BrokerVTableAbi::place_group`).
// ---------------------------------------------------------------------------------------------

/// `Option<vike_model::Grid>` as plain data (`ABI_VERSION` 5). `present == false` means
/// `None`, and then the four fields are meaningless.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GridOptionAbi {
    pub tick_size: OptionAbi<f64>,
    pub lot_size: OptionAbi<f64>,
    pub min_notional: OptionAbi<f64>,
    pub min_qty: OptionAbi<f64>,
    pub present: bool,
}

impl From<Option<vike_model::Grid>> for GridOptionAbi {
    fn from(v: Option<vike_model::Grid>) -> Self {
        let g = v.unwrap_or(vike_model::Grid {
            tick_size: None,
            lot_size: None,
            min_notional: None,
            min_qty: None,
        });
        GridOptionAbi {
            tick_size: g.tick_size.into(),
            lot_size: g.lot_size.into(),
            min_notional: g.min_notional.into(),
            min_qty: g.min_qty.into(),
            present: v.is_some(),
        }
    }
}

impl From<GridOptionAbi> for Option<vike_model::Grid> {
    fn from(v: GridOptionAbi) -> Self {
        v.present.then(|| vike_model::Grid {
            tick_size: v.tick_size.into(),
            lot_size: v.lot_size.into(),
            min_notional: v.min_notional.into(),
            min_qty: v.min_qty.into(),
        })
    }
}

// `OrderPlacement` as a KIND code plus the flat union of every variant's prices.
pub const TAGGED_MARKET: u32 = 0;
pub const TAGGED_LIMIT: u32 = 1;
pub const TAGGED_STOP: u32 = 2;
pub const TAGGED_STOP_LIMIT: u32 = 3;
pub const TAGGED_TAKE_PROFIT: u32 = 4;
pub const TAGGED_TRAILING: u32 = 5;
pub const TAGGED_PEGGED: u32 = 6;
pub const TAGGED_ICEBERG: u32 = 7;

/// The wire mirror of `vike_model::OrderPlacement`: a `kind` code and every price any variant states.
/// A field the kind does not state is NaN or absent and never read.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OrderPlacementAbi {
    /// One of the `TAGGED_*` codes.
    pub kind: u32,
    /// `Limit`'s and `Iceberg`'s price.
    pub price: f64,
    /// `Stop`'s, `StopLimit`'s and `TakeProfit`'s trigger.
    pub trigger: f64,
    /// `StopLimit`'s limit (always present) and `TakeProfit`'s (optional).
    pub limit: OptionAbi<f64>,
    /// `Trailing`'s trail.
    pub trail: f64,
    /// `Trailing`'s activation.
    pub activation: OptionAbi<f64>,
    /// `Pegged`'s anchor: its place in `vike_model::OrderPegAnchor::ALL` (`ABI_VERSION` 7).
    pub anchor: u32,
    /// `Pegged`'s offset (`ABI_VERSION` 7).
    pub offset: f64,
    /// `Iceberg`'s visible size (`ABI_VERSION` 7).
    pub display: f64,
}

impl OrderPlacementAbi {
    /// EXHAUSTIVE, no `_` arm: a kind added to `vike_model::OrderPlacement` is a compile error here,
    /// never an order sent as some other kind.
    pub fn of(o: vike_model::OrderPlacement) -> Self {
        use vike_model::OrderPlacement as T;
        let none = OptionAbi::<f64>::from(None);
        let blank = OrderPlacementAbi {
            kind: TAGGED_MARKET,
            price: f64::NAN,
            trigger: f64::NAN,
            limit: none,
            trail: f64::NAN,
            activation: none,
            anchor: u32::MAX,
            offset: f64::NAN,
            display: f64::NAN,
        };
        match o {
            T::Market => blank,
            T::Limit { price } => OrderPlacementAbi { kind: TAGGED_LIMIT, price, ..blank },
            T::Stop { trigger } => OrderPlacementAbi { kind: TAGGED_STOP, trigger, ..blank },
            T::StopLimit { trigger, limit } => OrderPlacementAbi {
                kind: TAGGED_STOP_LIMIT,
                trigger,
                limit: Some(limit).into(),
                ..blank
            },
            T::TakeProfit { trigger, limit } => OrderPlacementAbi {
                kind: TAGGED_TAKE_PROFIT,
                trigger,
                limit: limit.into(),
                ..blank
            },
            T::Trailing { trail, activation } => OrderPlacementAbi {
                kind: TAGGED_TRAILING,
                trail,
                activation: activation.into(),
                ..blank
            },
            T::Pegged { peg } => OrderPlacementAbi {
                kind: TAGGED_PEGGED,
                // `OrderPegAnchor::ALL` is in declaration order, so the discriminant is its place there
                anchor: peg.anchor as u32,
                offset: peg.offset,
                ..blank
            },
            T::Iceberg { price, display } => {
                OrderPlacementAbi { kind: TAGGED_ICEBERG, price, display, ..blank }
            }
        }
    }

    /// The inverse. `None` for a code this side does not know, a stop-limit with no limit or an
    /// anchor outside `OrderPegAnchor::ALL`: refused, never guessed, because a guessed kind is an order
    /// the strategy never asked for.
    pub fn to_tagged_order(&self) -> Option<vike_model::OrderPlacement> {
        use vike_model::OrderPlacement as T;
        Some(match self.kind {
            TAGGED_MARKET => T::Market,
            TAGGED_LIMIT => T::Limit { price: self.price },
            TAGGED_STOP => T::Stop { trigger: self.trigger },
            TAGGED_STOP_LIMIT => {
                T::StopLimit { trigger: self.trigger, limit: Option::from(self.limit)? }
            }
            TAGGED_TAKE_PROFIT => T::TakeProfit { trigger: self.trigger, limit: self.limit.into() },
            TAGGED_TRAILING => {
                T::Trailing { trail: self.trail, activation: self.activation.into() }
            }
            TAGGED_PEGGED => {
                let anchor =
                    *vike_model::OrderPegAnchor::ALL.get(usize::try_from(self.anchor).ok()?)?;
                T::Pegged { peg: vike_model::OrderPeg { anchor, offset: self.offset } }
            }
            TAGGED_ICEBERG => T::Iceberg { price: self.price, display: self.display },
            _ => return None,
        })
    }
}

/// An order list's numbers, by their JSON keys. JSON has no NaN or infinity (`serde_json` writes one
/// as `null`, which a price reads back as `None`), so [`order_list_json`] carries a non-finite one
/// as its text (`NaN`, `inf`, `-inf`) and [`order_list_of_json`] reads it back: every leg reaches the
/// host's broker bit for bit, and is refused in the words a native order meets
/// (`vike_model::order_terms_refusal`'s "the price NaN is not a finite number"). A finite list's
/// text is the plain serde JSON. ⚠ A new `f64` field of `vike_model::OrderListLeg` joins this list.
const ORDER_LIST_NUMBERS: [&str; 4] = ["qty", "price", "trigger_price", "trail"];

/// `OrderBroker::place_group`'s legs as the JSON text `BrokerVTableAbi::place_group` carries.
pub(crate) fn order_list_json(legs: &[(String, vike_model::OrderListLeg)]) -> String {
    let mut doc = serde_json::to_value(legs)
        .expect("an order list's legs always encode as JSON (strings, numbers, enums)");
    for (i, (_, leg)) in legs.iter().enumerate() {
        let numbers = [Some(leg.qty), leg.price, leg.trigger_price, leg.trail];
        for (key, x) in ORDER_LIST_NUMBERS.into_iter().zip(numbers) {
            if let Some(x) = x.filter(|x| !x.is_finite()) {
                doc[i][1][key] = x.to_string().into();
            }
        }
    }
    doc.to_string()
}

/// The legs [`order_list_json`] wrote, a non-finite number restored; an error for text that is not
/// one.
pub(crate) fn order_list_of_json(
    json: &str,
) -> Result<Vec<(String, vike_model::OrderListLeg)>, String> {
    let mut doc: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut odd = Vec::new();
    for (i, pair) in doc.as_array_mut().into_iter().flatten().enumerate() {
        let Some(leg) = pair.get_mut(1).and_then(serde_json::Value::as_object_mut) else {
            continue;
        };
        for key in ORDER_LIST_NUMBERS {
            if let Some(text) = leg.get(key).and_then(serde_json::Value::as_str) {
                let x: f64 = text.parse().map_err(|_| format!("leg {i}'s {key} is {text:?}"))?;
                odd.push((i, key, x));
                leg.insert(key.to_string(), 0.0.into());
            }
        }
    }
    let mut legs: Vec<(String, vike_model::OrderListLeg)> =
        serde_json::from_value(doc).map_err(|e| e.to_string())?;
    for (i, key, x) in odd {
        let leg = &mut legs[i].1;
        match key {
            "qty" => leg.qty = x,
            "price" => leg.price = Some(x),
            "trigger_price" => leg.trigger_price = Some(x),
            _ => leg.trail = Some(x),
        }
    }
    Ok(legs)
}

// ---------------------------------------------------------------------------------------------
// The closed-trade ledger's row (`ABI_VERSION` 6). An input's bars cross as `BarAbi`s.
// ---------------------------------------------------------------------------------------------

/// The wire mirror of `vike_model::TradeClosedRow`. `symbol` and `tag` borrow the host's row, valid while
/// the host's ledger is unchanged (the guest copies them at once).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TradeClosedRowAbi {
    pub symbol: StrAbi,
    pub tag: StrAbi,
    pub side: i32,
    pub entry_timestamp: i64,
    pub entry_bar: usize,
    pub entry_price: f64,
    pub exit_timestamp: i64,
    pub exit_bar: usize,
    pub exit_price: f64,
    pub qty: f64,
    pub pnl: f64,
    pub commission: f64,
    pub net: f64,
}

impl TradeClosedRowAbi {
    /// An all-zero row with empty strings: the blank of a `OptionAbi` that answers `None`.
    pub fn empty() -> Self {
        TradeClosedRowAbi {
            symbol: StrAbi::empty(),
            tag: StrAbi::empty(),
            side: 0,
            entry_timestamp: 0,
            entry_bar: 0,
            entry_price: 0.0,
            exit_timestamp: 0,
            exit_bar: 0,
            exit_price: 0.0,
            qty: 0.0,
            pnl: 0.0,
            commission: 0.0,
            net: 0.0,
        }
    }

    pub fn of(r: &vike_model::TradeClosedRow) -> Self {
        TradeClosedRowAbi {
            symbol: StrAbi::of(&r.symbol),
            tag: StrAbi::of(&r.tag),
            side: r.side,
            entry_timestamp: r.entry_timestamp,
            entry_bar: r.entry_bar,
            entry_price: r.entry_price,
            exit_timestamp: r.exit_timestamp,
            exit_bar: r.exit_bar,
            exit_price: r.exit_price,
            qty: r.qty,
            pnl: r.pnl,
            commission: r.commission,
            net: r.net,
        }
    }

    /// Every field named, so a field added to `TradeClosedRow` is a compile error here, never a row that
    /// silently stops carrying it.
    pub fn to_closed_row(&self) -> vike_model::TradeClosedRow {
        vike_model::TradeClosedRow {
            symbol: self.symbol.read(),
            tag: self.tag.read(),
            side: self.side,
            entry_timestamp: self.entry_timestamp,
            entry_bar: self.entry_bar,
            entry_price: self.entry_price,
            exit_timestamp: self.exit_timestamp,
            exit_bar: self.exit_bar,
            exit_price: self.exit_price,
            qty: self.qty,
            pnl: self.pnl,
            commission: self.commission,
            net: self.net,
        }
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
/// `Disconnected`, which a strategy file would then act on by pulling quotes that were never in danger.
pub fn feed_status_code(s: vike_model::FeedStatus) -> u32 {
    match s {
        vike_model::FeedStatus::Disconnected => FEED_STATUS_DISCONNECTED,
        vike_model::FeedStatus::Stale => FEED_STATUS_STALE,
        vike_model::FeedStatus::Live => FEED_STATUS_LIVE,
    }
}

/// The inverse. `None` for a code this host does not know — the guest turns that into
/// [`StrategyFileStatusAbi::BadParams`] rather than guessing, because every guess here is a market-health
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
// `OrderEvent` — a TAG plus a flat payload (`ABI_VERSION` 3, tier 3).
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

/// The wire mirror of `vike_model::OrderEvent`: a `kind` TAG plus the flat union of every
/// variant's payload, which across the six variants is exactly one optional string.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OrderEventAbi {
    pub order_client_id: StrAbi,
    /// `None` for an order placed untagged (`""`) — see [`OptionAbi<StrAbi>`] for
    /// why a null pointer alone would not carry that distinction.
    pub tag: OptionAbi<StrAbi>,
    /// One of the `ORDER_EVENT_*` codes.
    pub kind: u32,
    /// The reason text for `Rejected`/`Denied`/`Canceled`; empty for the other three.
    pub reason: StrAbi,
}

impl OrderEventAbi {
    pub fn of(event: &vike_model::OrderEvent) -> Self {
        OrderEventAbi {
            order_client_id: StrAbi::of(&event.order_client_id),
            tag: OptionAbi::<StrAbi>::of(event.tag.as_deref()),
            kind: order_event_code(&event.kind),
            reason: StrAbi::of(order_event_reason(&event.kind)),
        }
    }

    /// `None` when `kind` is a code this host does not know.
    pub fn to_order_lifecycle(&self) -> Option<vike_model::OrderEvent> {
        let kind = order_event_from_code(self.kind, self.reason.read())?;
        Some(vike_model::OrderEvent {
            order_client_id: self.order_client_id.read(),
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
// the book crosses as a CURSOR, the same shape `BrokerVTableAbi::bars_len`/`bar_at` already uses for
// the slice `OrderBroker::bars` returns.
//
// WHAT ONE `on_order_book` DISPATCH COSTS, stated here because a cursor can look cheaper than it
// is. The host flattens its book ONCE per dispatch into two `Vec<L2BookLevelAbi>`s
// (`bid_levels + ask_levels` `BTreeMap` steps, two allocations) and lends THAT through `ctx`;
// then the guest makes `2 + 2 + bid_levels + ask_levels` C-ABI crossings and rebuilds an
// `L2Book`, which is `bid_levels + ask_levels` `BTreeMap` inserts. So it is O(levels) end to end,
// twice.
//
// It is NOT the cursor that makes the copy unavoidable: `Strategy::on_order_book` hands user code
// a concrete `&L2Book`, so SOMETHING must materialise one on the strategy file's side of the boundary
// whatever shape the payload takes. What the cursor buys over the alternatives is that the HOST
// never clones its own book, no second serialisation format exists to drift, and a level is
// plain data at every step.
//
// ⚠ The host-side FLATTEN is what keeps `level_at` O(1) and is the reason `ctx` is not simply the
// `&L2Book`: a `BTreeMap` has no index, so a literal per-index seek would be `nth(i)` — O(i) —
// and a full walk would be O(levels²) node steps. A 200-level book would pay 40,000 of them per
// event to deliver 400.
// ---------------------------------------------------------------------------------------------

/// One price level as plain data — the wire mirror of `vike_marketdata::L2BookLevel`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct L2BookLevelAbi {
    pub price: f64,
    pub qty: f64,
}

impl L2BookLevelAbi {
    /// An all-zero level, for the out-param a cursor read writes into — so a caller that ignores
    /// the returned `bool` reads a well-defined empty level rather than uninitialised stack
    /// memory. The same contract [`BarAbi::empty`] states for `bar_at`.
    pub fn empty() -> Self {
        L2BookLevelAbi { price: 0.0, qty: 0.0 }
    }
}

/// `side` values for the cursor below. Same convention as `OrderBroker::place`'s `side`.
pub const SIDE_BID: i32 = 1;
pub const SIDE_ASK: i32 = -1;

/// The host's book, erased — `ctx` is host-owned and valid for the duration of ONE
/// `vike_strategy_on_order_book` call, `vtable` is host-owned and `'static`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BookRef {
    pub ctx: *mut core::ffi::c_void,
    pub vtable: *const BookVTable,
}

/// What a strategy file may ask the host about the book it was just handed.
#[repr(C)]
pub struct BookVTable {
    pub tick_size: extern "C" fn(*mut core::ffi::c_void) -> f64,
    pub last_sequence: extern "C" fn(*mut core::ffi::c_void) -> u64,
    /// How many levels `side` ([`SIDE_BID`] / [`SIDE_ASK`]) has.
    pub side_len: extern "C" fn(*mut core::ffi::c_void, i32) -> usize,
    /// Writes level `i` of `side` into `out`, BEST FIRST (bids high->low, asks low->high).
    /// Returns false when `i` is out of range, leaving `out` untouched.
    pub level_at: extern "C" fn(*mut core::ffi::c_void, i32, usize, *mut L2BookLevelAbi) -> bool,
}

/// What every `extern "C"` entry point returns. A strategy file never unwinds.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrategyFileStatusAbi {
    Ok = 0,
    Panicked = 1,
    BadHandle = 2,
    BadParams = 3,
}

/// The host's broker, erased. `ctx` is an erased `&mut OrderBrokerSimulator`; `vtable` is
/// host-owned and `'static`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BrokerRef {
    pub ctx: *mut core::ffi::c_void,
    pub vtable: *const BrokerVTableAbi,
}

/// The host's side of the boundary: what a strategy file may call back into.
///
/// ⚠ `bars_len`/`bar_at` are a CURSOR, deliberately, because `OrderBroker::bars` returns
/// `&[Bar]` and a slice of a non-`repr(C)` type cannot cross. Converting the whole
/// history on every call would be O(n) per access; the cursor is O(1) and lets the
/// guest cache by `index`.
///
/// ⚠ **Its shape since `ABI_VERSION` 8 is the one broker interface's (decision 0277's final seam):
/// the `vike_model::OrderBroker` reads and verbs, nothing else.** The slots of the
/// retired verbs (`Broker::submit_market`/`submit_limit` and `HftBroker`'s four) are gone, and every
/// slot that places, refuses or reads an order's book carries the SYMBOL the strategy named as a
/// borrowed `(ptr, len)` ahead of its other arguments, `""` being the mount's own as in the trait.
/// Every tag and exit name is a borrowed `(ptr, len)` too; every `Option<f64>` an
/// [`OptionAbi`]`<f64>`; every order an [`OrderPlacementAbi`].
#[repr(C)]
pub struct BrokerVTableAbi {
    // ---- the `OrderBroker` reads ----
    pub position: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> f64,
    pub price: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> f64,
    pub equity: extern "C" fn(*mut core::ffi::c_void) -> f64,
    pub index: extern "C" fn(*mut core::ffi::c_void) -> usize,
    pub now: extern "C" fn(*mut core::ffi::c_void) -> i64,
    pub bars_len: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> usize,
    /// Writes bar `i` of `symbol` into `out`. Returns false when `i` is out of range.
    pub bar_at: extern "C" fn(*mut core::ffi::c_void, *const u8, usize, usize, *mut BarAbi) -> bool,
    pub quote_vwap:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64) -> OptionAbi<f64>,
    pub depth_within_price:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64) -> f64,
    /// `OrderBroker::contract_value(&self, symbol)` (`ABI_VERSION` 4), flattened to
    /// [`ContractValueOptionAbi`].
    pub contract_value:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> ContractValueOptionAbi,
    /// `OrderBroker::instrument_grid(&self, symbol)` (`ABI_VERSION` 5), flattened to [`GridOptionAbi`].
    pub instrument_grid: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> GridOptionAbi,
    // `OrderBroker::input_bars` and `OrderBroker::closed_trades` (`ABI_VERSION` 6), each as its count and its
    // item at an index: the `bars_len`/`bar_at` cursor, answered by value. An item past the count is
    // `None`.
    /// `OrderBroker::input_bars(&self, name).len()`.
    pub input_bars_len: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> usize,
    /// Bar `i` of `OrderBroker::input_bars(&self, name)`.
    pub input_bar_at:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, usize) -> OptionAbi<BarAbi>,
    /// `OrderBroker::closed_trades(&self)`'s length; `None` is the broker keeping no ledger, never an
    /// empty one.
    pub closed_trades_len: extern "C" fn(*mut core::ffi::c_void) -> OptionAbi<usize>,
    /// Row `i` of `OrderBroker::closed_trades(&self)`.
    pub closed_trade_at:
        extern "C" fn(*mut core::ffi::c_void, usize) -> OptionAbi<TradeClosedRowAbi>,

    // ---- the `OrderBroker` verbs, in that trait's order (`ABI_VERSION` 5; the symbol at 8) ----
    /// `OrderBroker::place(symbol, tag, side, qty, order, reduce_only)`.
    pub place: extern "C" fn(
        *mut core::ffi::c_void,
        *const u8,
        usize,
        *const u8,
        usize,
        i32,
        f64,
        OrderPlacementAbi,
        bool,
    ),
    /// `OrderBroker::place_exit_leg(symbol, exit, tag, side, qty, order)`: `symbol`, `exit`, then
    /// `tag`.
    pub place_exit_leg: extern "C" fn(
        *mut core::ffi::c_void,
        *const u8,
        usize,
        *const u8,
        usize,
        *const u8,
        usize,
        i32,
        f64,
        OrderPlacementAbi,
    ),
    /// `OrderBroker::place_at_close(symbol, tag, side, qty, reduce_only)`.
    pub place_at_close:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, *const u8, usize, i32, f64, bool),
    /// `OrderBroker::place_close_from_fill(symbol, tag, side, qty, level)`.
    pub place_close_from_fill:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, *const u8, usize, i32, f64, f64),
    /// `OrderBroker::rests_reduce_only_while_flat(&self)`.
    pub rests_reduce_only_while_flat: extern "C" fn(*mut core::ffi::c_void) -> bool,
    /// `OrderBroker::place_group(symbol, legs)`: the symbol, then the legs as the JSON text of their
    /// `Vec<(String, vike_model::OrderListLeg)>` (`OrderListLeg` is serde's already), the
    /// `on_params_updated` precedent: a flat mirror of a list of legs, each with its links, would be
    /// a hand-kept second copy of `OrderListLeg` that a new field silently stops crossing. A NaN or
    /// an infinity, which JSON cannot write, crosses as its text (`order_list_json`).
    pub place_group: extern "C" fn(*mut core::ffi::c_void, *const u8, usize, *const u8, usize),
    /// `OrderBroker::amend(tag, new_qty, new_price, new_trigger)`: a tag names one order of the
    /// mount whatever its symbol, so no symbol.
    pub amend: extern "C" fn(
        *mut core::ffi::c_void,
        *const u8,
        usize,
        OptionAbi<f64>,
        OptionAbi<f64>,
        OptionAbi<f64>,
    ),
    /// `OrderBroker::cancel(tag)`.
    pub cancel: extern "C" fn(*mut core::ffi::c_void, *const u8, usize),
    /// `OrderBroker::deny(symbol, tag, qty, reason)` (`ABI_VERSION` 7): `symbol`, `tag`, then
    /// `qty`, then `reason`, each string a borrowed `(ptr, len)`.
    pub deny: extern "C" fn(
        *mut core::ffi::c_void,
        *const u8,
        usize,
        *const u8,
        usize,
        f64,
        *const u8,
        usize,
    ),
    /// `OrderBroker::average_price(&self, symbol)`.
    pub average_price: extern "C" fn(*mut core::ffi::c_void, *const u8, usize) -> f64,
    /// Symbol `i` of `OrderBroker::symbols(&self)`, the run's universe (`ABI_VERSION` 8), borrowed
    /// from the host's list, which does not change while the strategy file is called; `None` past its end.
    pub symbol_at: extern "C" fn(*mut core::ffi::c_void, usize) -> OptionAbi<StrAbi>,
}

#[path = "abi_tests.rs"]
#[cfg(test)]
mod abi_tests;
