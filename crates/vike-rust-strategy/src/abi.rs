//! The C-ABI vocabulary. Every type here is `#[repr(C)]` and every field is plain data.
//!
//! ⚠ The rule this file exists to hold: NO Rust type crosses by value. Not `Box`, not
//! `String`, not `Option<T>`, not `&[T]`. Strings cross as `(ptr, len)` and are never
//! NUL-terminated, because Rust strings are not. An `Option` of a scalar or of a mirror flattens
//! to [`OptionAbi`] of it.
//!
//! ⚠ **Two Rust types are LENT by reference, since `ABI_VERSION` 14: `vike_marketdata::Bar` and
//! `vike_model::TradeClosedRow`** (and the two ticks since 15). The history reads lend the host's own rows
//! as a [`SliceAbi`] and `on_bar` lends the host's bar behind a pointer, so the file reads them where the
//! host keeps them and nothing is copied across. Both types are `#[repr(C)]` in their declaration order,
//! and [`layout()`] (their sizes, alignments and field offsets, and the sizes of the std types inside them)
//! is compared at load: a file built against another layout is refused, never misread. The std types
//! inside (`Option<f64>`, `String`) stay `repr(Rust)`; that both sides lay them out alike rests on the
//! toolchain fingerprint (one rustc, one target), which the loader checks first. The bars' and ticks'
//! symbol is a `vike_marketdata::Symbol` (since 2026-11-20; a `String` before): one pointer to an entry
//! (the text's seedless hash and the text) the host never frees, so the file may read it, copy it and keep
//! it. The file links its own interner, and a `Symbol` means its TEXT (equal by pointer, else by the stored
//! hash and the text; hashed by that stored, seedless hash), so the host's handle and the file's own for one
//! instrument are equal and hash alike. [`layout()`] hashes its size and `Symbol::REPRESENTATION`, so a file
//! built against `String` rows, or another representation of the handle, is refused at load.
//!
//! ⚠ **What crosses is the BOUNDARY's own row, not an SDK's type** (decision 0296's step 6, the adapter):
//! [`LentBarAbi`], [`LentTradeClosedRowAbi`], [`LentTickQuoteAbi`] and [`LentTickTradeAbi`], field for field
//! what those four are on this host. [`layout()`] is computed over them, so the layout a file states is this
//! boundary's whichever SDK its strategy compiles against, and an SDK's type is read in place only where
//! `lent_in_place!` proves it laid out as the row (a compile error otherwise). An older SDK whose row
//! moved is retired, never adapted (decision 0296's verdict 7; `crate::sdk::usable`).
//!
//! ⚠ The type declarations below are pure data and contain no `unsafe`. [`read_borrowed_str`] is
//! the one exception in this file — it reads back through a borrowed `(ptr, len)` pair. That is
//! expected in this crate (`crates/vike-ops/tests/hygiene/unsafe_and_toolchain_gate/pins.rs`'s
//! `UNSAFE_EXEMPT` names it, per the design at
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
/// count and its item at an index ([`OptionAbi`] of a `BarAbi` and of a `TradeClosedRowAbi`, both gone at 14), the
/// bars' cursor answered by value. A v5 artifact is refused for the 3 -> 4 reason: its
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
/// (the refusal test moved one version on, to the 9 -> 10 bump below).
///
/// ⚠ **9 -> 10 (decision 0271's amendment of 2026-11-14, the owner's option B for the files that look
/// into the future): a strategy file reads an input's WHOLE series (`OrderBroker::input_whole`, the order
/// language's `Tactics::input_whole`) exactly as a built-in does; a v9 strategy file is refused and must
/// be rebuilt from its unchanged source.** `BrokerVTableAbi` grew two slots, `input_whole_len` and
/// `input_whole_at`, the read's count ([`OptionAbi`]: `None` where no whole series is served, never an
/// empty one) and its bar at an index. A v9 artifact would hand this host a vtable two slots short, and
/// its `HostBroker` left the read on the trait's `None`, so a file that reads ahead would trade on no
/// higher bar where the same source compiled in reads them
/// (the refusal test moved one version on, to the 10 -> 11 bump below).
///
/// ⚠ **10 -> 11 (book W35): a strategy file PUBLISHES a forecast per symbol beside its orders
/// (`OrderBroker::publish_forecast`, the order language's `Tactics::forecast`) exactly as a built-in
/// does; a v10 strategy file is refused and must be rebuilt from its unchanged source.**
/// `BrokerVTableAbi` grew the `publish_forecast` slot (the symbol and the forecast's three terms). A
/// v10 artifact would hand this host a vtable one slot short, and its `HostBroker` left the verb on
/// the trait's default, so a file's forecasts would vanish where the same source compiled in records
/// them (the refusal test moved one version on, to the 11 -> 12 bump below).
///
/// ⚠ **11 -> 12: a strategy file keeps the input bars it copied from one dispatch to the next where
/// the host's inputs only grow (the simulator's), so a backtest copies each input bar across the
/// boundary once, not the whole history on every bar (the square of the run's length: 32 months of
/// 1-minute bars took about ten hours); a v11 strategy file is refused and must be rebuilt from its
/// unchanged source.** `BrokerVTableAbi` grew an `inputs_only_grow` slot after `input_whole_at`, so a
/// v11 artifact would read every slot past it one off (the refusal test moved one version on, to the
/// 12 -> 13 bump below).
///
/// ⚠ **12 -> 13: the same keep covers the strategy's own bars and its closed rows.** The slot became
/// `history_only_grows` (`OrderBroker::history_only_grows`: every `bars`, input and `closed_trades`
/// answer a prefix of a series that never changes, the simulator's), so a backtest copies each bar and
/// each closed row once; a v12 file would read the widened answer as the inputs' alone. **And the
/// strategy's stated commission crosses** (`Strategy::stated_fee`, decision 0269: the default of the
/// file's `commission` param, the owner's ruling of the same day, no venue table): `host::StrategyVTableAbi`
/// grew the `stated_fee` export (its `FeeSchedule` as JSON), and **its stated slippage**
/// (`Strategy::stated_slippage`, the default of its `slippage` param, none is 0) the `stated_slippage`
/// export (its `vike_model::ProfileEngineSlippage` table as JSON); a v12 file exports neither. A v12
/// strategy file is refused and must be rebuilt from its unchanged source (the refusal test moved one
/// version on, to the 13 -> 14 bump below).
///
/// ⚠ **13 -> 14 (the owner, 2026-11-19): a strategy file reads its bars, input bars, whole series and
/// closed rows WHERE THE HOST KEEPS THEM, and `on_bar`'s bar is the host's own; nothing is copied across
/// the boundary and no history is kept in the file.** The eight count-and-item slots became four that
/// LEND a [`SliceAbi`] (`bars`, `input_bars`, `closed_trades`, `input_whole`), valid for the dispatch they
/// answer; `on_bar` takes a `*const vike_marketdata::Bar`; `BarAbi` and `TradeClosedRowAbi` are gone, and
/// the file exports its [`layout()`], which the loader compares. A v13 artifact would read a lent slice as a count
/// (the refusal test moved one version on, to the 14 -> 15 bump below).
///
/// ⚠ **14 -> 15 (decision 0296's step 1, the owner's "a compatibility record inside the .so, checked
/// before loading"): every artifact carries its COMPATIBILITY RECORD in the ELF section `.vike_identity`
/// (`crate::identity`), and the loader reads it from the file and refuses a mismatch BEFORE `dlopen`
/// runs any of the file's code.** The record states this version, the toolchain fingerprint, [`layout()`]
/// and the strategy boundary's hash (`crate::sdk::BOUNDARY_HASH`); the templates gained the static and
/// the `vike_strategy_identity` export that keeps it linked. **And the per-symbol and per-input reads name
/// their symbol or input by INDEX** in the host's universe or input list where the file knows it
/// ([`NameOrIndexAbi`], the owner's "do 1-7" of 2026-11-19): eight read slots take one, and
/// `BrokerVTableAbi` grew `input_name_at`. **And the tick hooks take the host's own tick** (the owner's
/// "implement for ticks the same as for bars"): `vike_marketdata::TickQuote` and `TickTrade` are `repr(C)`
/// under [`layout()`], and their two mirrors are gone. **And no JSON on two more paths** (the owner's
/// "2 (б) + 3 yes"): the strategy's STATE crosses as the msgpack bytes of `vike_model::StrategyState`
/// (`save_state` lends a [`SliceAbi`] of them, `load_state` takes them), and `place_group`'s legs as a
/// lent slice of [`OrderListLegAbi`] under [`layout()`]. A v14 artifact has no record and is refused
/// unread; it is rebuilt from its unchanged source (the refusal test:
/// `crates/vike-rust-strategy/tests/load_refusals.rs`'s
/// `a_strategy_file_built_against_abi_14_is_refused_naming_both_versions`).
pub const ABI_VERSION: u32 = 15;

/// A zero version cannot be distinguished from an unset field, so it must never be reachable.
/// Checked at COMPILE TIME — stronger than a `#[test]` asserting on a `const` (which
/// `clippy::assertions_on_constants`, a `-D warnings` merge-gate lint, correctly flags: the
/// condition is knowable at compile time, so a runtime `assert!` on it tests nothing an ordinary
/// build does not already prove) — and enforced on EVERY build, not only under `cargo test`.
const _: () =
    assert!(ABI_VERSION > 0, "a zero version cannot be distinguished from an unset field");

/// **The layout of the types a strategy file reads in place** (`ABI_VERSION` 14, the module doc):
/// FNV-1a over `vike_marketdata::Bar`'s and `vike_model::TradeClosedRow`'s sizes, alignments and every
/// field's offset, since 15 `vike_marketdata::TickQuote`'s and `TickTrade`'s too (the tick hooks lend the
/// host's own tick, as `on_bar` its bar), and the sizes of the std types inside them; and the research
/// study file's three by-value boundary types (`crate::study`'s `BytesAbi`, `SinkAbi`, `StudyHostAbi`), so a
/// study artifact built against another shape of them is refused at load as a strategy file is (a study
/// exports its layout from the same function, and `crate::loader::load_study` runs the same handshake).
/// Each side computes it from the types it was compiled against; the file exports its own
/// (`vike_strategy_layout`) and the loader refuses one that differs from the host's
/// (`crate::loader::LoadError::LayoutMismatch`). The std types' own insides are the toolchain
/// fingerprint's to vouch for, not this hash's. `const` (`ABI_VERSION` 15): a file's compatibility record
/// (`crate::identity::RECORD`) bakes it in at compile time. Since 15 also [`OrderListLegAbi`], the rows a
/// file lends to `place_group`.
///
/// Computed over the boundary's own rows ([`LentBarAbi`] and its three siblings, this module's doc), field
/// for field the four SDK types: the same numbers as when it read those types, so no artifact's layout
/// moved with them, and an older SDK's file built through its adapter states this host's. Since
/// 2026-11-20 the bars' and ticks' symbol is a `vike_marketdata::Symbol` (decision 0296's amendment of
/// that day): its size and `Symbol::REPRESENTATION` are words here, so the rows' move from `String`
/// changed this value with `ABI_VERSION` still 15, and every older artifact is refused at load.
pub const fn layout() -> u64 {
    use crate::study::{BytesAbi, SinkAbi, StudyHostAbi};
    use std::mem::{align_of, offset_of, size_of};
    let words = [
        size_of::<OrderListLegAbi>(),
        align_of::<OrderListLegAbi>(),
        offset_of!(OrderListLegAbi, tag),
        offset_of!(OrderListLegAbi, side),
        offset_of!(OrderListLegAbi, qty),
        offset_of!(OrderListLegAbi, order_type),
        offset_of!(OrderListLegAbi, order_type_word),
        offset_of!(OrderListLegAbi, price),
        offset_of!(OrderListLegAbi, trigger_price),
        offset_of!(OrderListLegAbi, trail),
        offset_of!(OrderListLegAbi, reduce_only),
        offset_of!(OrderListLegAbi, time_in_force),
        offset_of!(OrderListLegAbi, parent),
        offset_of!(OrderListLegAbi, contingency),
        offset_of!(OrderListLegAbi, linked),
        offset_of!(OrderListLegAbi, order_client_id),
        size_of::<OptionAbi<f64>>(),
        size_of::<OptionAbi<usize>>(),
        size_of::<OptionAbi<u32>>(),
        size_of::<OptionAbi<StrAbi>>(),
        size_of::<SliceAbi<usize>>(),
        size_of::<StrAbi>(),
        size_of::<BytesAbi>(),
        align_of::<BytesAbi>(),
        offset_of!(BytesAbi, ptr),
        offset_of!(BytesAbi, len),
        size_of::<SinkAbi>(),
        align_of::<SinkAbi>(),
        offset_of!(SinkAbi, sink),
        offset_of!(SinkAbi, write),
        size_of::<StudyHostAbi>(),
        align_of::<StudyHostAbi>(),
        offset_of!(StudyHostAbi, host),
        offset_of!(StudyHostAbi, call),
        size_of::<LentTickQuoteAbi>(),
        align_of::<LentTickQuoteAbi>(),
        offset_of!(LentTickQuoteAbi, timestamp),
        offset_of!(LentTickQuoteAbi, local_timestamp),
        offset_of!(LentTickQuoteAbi, bid),
        offset_of!(LentTickQuoteAbi, ask),
        offset_of!(LentTickQuoteAbi, bid_size),
        offset_of!(LentTickQuoteAbi, ask_size),
        offset_of!(LentTickQuoteAbi, symbol),
        size_of::<LentTickTradeAbi>(),
        align_of::<LentTickTradeAbi>(),
        offset_of!(LentTickTradeAbi, timestamp),
        offset_of!(LentTickTradeAbi, local_timestamp),
        offset_of!(LentTickTradeAbi, price),
        offset_of!(LentTickTradeAbi, size),
        offset_of!(LentTickTradeAbi, is_buyer_maker),
        offset_of!(LentTickTradeAbi, symbol),
        size_of::<LentBarAbi>(),
        align_of::<LentBarAbi>(),
        offset_of!(LentBarAbi, timestamp),
        offset_of!(LentBarAbi, open),
        offset_of!(LentBarAbi, high),
        offset_of!(LentBarAbi, low),
        offset_of!(LentBarAbi, close),
        offset_of!(LentBarAbi, volume),
        offset_of!(LentBarAbi, funding),
        offset_of!(LentBarAbi, bid),
        offset_of!(LentBarAbi, ask),
        offset_of!(LentBarAbi, symbol),
        size_of::<LentTradeClosedRowAbi>(),
        align_of::<LentTradeClosedRowAbi>(),
        offset_of!(LentTradeClosedRowAbi, symbol),
        offset_of!(LentTradeClosedRowAbi, tag),
        offset_of!(LentTradeClosedRowAbi, side),
        offset_of!(LentTradeClosedRowAbi, entry_timestamp),
        offset_of!(LentTradeClosedRowAbi, entry_bar),
        offset_of!(LentTradeClosedRowAbi, entry_price),
        offset_of!(LentTradeClosedRowAbi, exit_timestamp),
        offset_of!(LentTradeClosedRowAbi, exit_bar),
        offset_of!(LentTradeClosedRowAbi, exit_price),
        offset_of!(LentTradeClosedRowAbi, qty),
        offset_of!(LentTradeClosedRowAbi, pnl),
        offset_of!(LentTradeClosedRowAbi, commission),
        offset_of!(LentTradeClosedRowAbi, net),
        size_of::<Option<f64>>(),
        align_of::<Option<f64>>(),
        size_of::<Option<vike_marketdata::Symbol>>(),
        size_of::<vike_marketdata::Symbol>(),
        vike_marketdata::Symbol::REPRESENTATION as usize,
        size_of::<String>(),
    ];
    let mut digest = vike_utility::fnv::Fnv1a64::new();
    let mut i = 0;
    while i < words.len() {
        digest.write(&(words[i] as u64).to_le_bytes());
        i += 1;
    }
    digest.finish()
}

// ---- the rows a strategy file reads where the host keeps them (decision 0296's step 6) -------------
//
// Field for field what this host's SDK types are, in their declaration order, under `repr(C)`: never built,
// only borrowed (a host's row read through a pointer). `tests/boundary/lent_fields.txt` holds each field's
// TYPE, which `layout()` cannot see; `lent_in_place!` holds the SDK type to the row.

/// The bar the host lends (`vike_marketdata::Bar`'s fields).
#[repr(C)]
pub struct LentBarAbi {
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub funding: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub symbol: Option<vike_marketdata::Symbol>,
}

/// The closed row the host lends (`vike_model::TradeClosedRow`'s fields).
#[repr(C)]
pub struct LentTradeClosedRowAbi {
    pub symbol: String,
    pub tag: String,
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

/// The quote the host lends (`vike_marketdata::TickQuote`'s fields).
#[repr(C)]
pub struct LentTickQuoteAbi {
    pub timestamp: i64,
    pub local_timestamp: i64,
    pub bid: f64,
    pub ask: f64,
    pub bid_size: f64,
    pub ask_size: f64,
    pub symbol: vike_marketdata::Symbol,
}

/// The trade the host lends (`vike_marketdata::TickTrade`'s fields).
#[repr(C)]
pub struct LentTickTradeAbi {
    pub timestamp: i64,
    pub local_timestamp: i64,
    pub price: f64,
    pub size: f64,
    pub is_buyer_maker: bool,
    pub symbol: vike_marketdata::Symbol,
}

/// **Proves at compile time that an SDK type is laid out as the row the boundary lends in its place**: the
/// same size and alignment, the same fields and no other, and each at the same offset with the same type. `bar`,
/// `trade_closed_row`, `tick_quote` or `tick_trade`, over the SDK type of that name THIS crate compiles
/// against (an older SDK's, in the glue its adapter builds). Expands to items, so it sits in a module or a
/// function body; every site that reads a row as an SDK type in place states it first (`crate::guest`'s
/// broker reads and `face`, the host's lending below), and against a type laid out otherwise it does not
/// compile: an adapter converts instead. Costs nothing at run time.
macro_rules! lent_in_place {
    (bar) => {
        $crate::abi::lent_in_place!(@ ::vike_marketdata::Bar, $crate::abi::LentBarAbi;
            timestamp, open, high, low, close, volume, funding, bid, ask, symbol);
    };
    (trade_closed_row) => {
        $crate::abi::lent_in_place!(@ ::vike_model::TradeClosedRow, $crate::abi::LentTradeClosedRowAbi;
            symbol, tag, side, entry_timestamp, entry_bar, entry_price, exit_timestamp, exit_bar,
            exit_price, qty, pnl, commission, net);
    };
    (tick_quote) => {
        $crate::abi::lent_in_place!(@ ::vike_marketdata::TickQuote, $crate::abi::LentTickQuoteAbi;
            timestamp, local_timestamp, bid, ask, bid_size, ask_size, symbol);
    };
    (tick_trade) => {
        $crate::abi::lent_in_place!(@ ::vike_marketdata::TickTrade, $crate::abi::LentTickTradeAbi;
            timestamp, local_timestamp, price, size, is_buyer_maker, symbol);
    };
    (@ $sdk:path, $lent:path; $($field:ident),+) => {
        const _: () = assert!(
            ::std::mem::size_of::<$sdk>() == ::std::mem::size_of::<$lent>()
                && ::std::mem::align_of::<$sdk>() == ::std::mem::align_of::<$lent>()
                $(&& ::std::mem::offset_of!($sdk, $field) == ::std::mem::offset_of!($lent, $field))+,
            concat!(
                "`", stringify!($sdk), "` is not laid out as the boundary's `", stringify!($lent),
                "`, so it cannot be read in place: such an SDK is retired, never adapted (decision 0296, verdict 7)"
            )
        );
        // Each field's TYPE (an array holds one type, so a field typed otherwise does not compile), and EVERY
        // field: the destructures name no `..`, so a field either type has beyond the list (one in the other's
        // padding, say, which the host never wrote) does not compile either. Never called.
        const _: () = {
            fn _same_fields(sdk: &$sdk, lent: &$lent) {
                let $sdk { $($field: _),+ } = sdk;
                let $lent { $($field: _),+ } = lent;
                $(let _ = [&sdk.$field, &lent.$field];)+
            }
        };
    };
}
pub(crate) use lent_in_place;

// The host lends its own SDK's rows as the boundary's, which `lent_in_place!` proves alike field for field.
// The host is always this crate's current SDK, so never in the glue an adapter builds.
#[cfg(not(feature = "adapted"))]
mod lend {
    use super::{LentBarAbi, LentTickQuoteAbi, LentTickTradeAbi, LentTradeClosedRowAbi, SliceAbi};

    crate::abi::lent_in_place!(bar);
    crate::abi::lent_in_place!(trade_closed_row);
    crate::abi::lent_in_place!(tick_quote);
    crate::abi::lent_in_place!(tick_trade);

    impl LentBarAbi {
        /// The host's bar, lent for one call.
        pub fn lend(bar: &vike_marketdata::Bar) -> *const LentBarAbi {
            std::ptr::from_ref(bar).cast()
        }

        /// The host's bars, lent for one dispatch.
        pub fn lend_all(bars: &[vike_marketdata::Bar]) -> SliceAbi<LentBarAbi> {
            SliceAbi::of(bars).cast()
        }
    }

    impl LentTradeClosedRowAbi {
        /// The host's closed rows, lent for one dispatch.
        pub fn lend_all(rows: &[vike_model::TradeClosedRow]) -> SliceAbi<LentTradeClosedRowAbi> {
            SliceAbi::of(rows).cast()
        }
    }

    impl LentTickQuoteAbi {
        /// The host's quote, lent for one call.
        pub fn lend(quote: &vike_marketdata::TickQuote) -> *const LentTickQuoteAbi {
            std::ptr::from_ref(quote).cast()
        }
    }

    impl LentTickTradeAbi {
        /// The host's trade, lent for one call.
        pub fn lend(trade: &vike_marketdata::TickTrade) -> *const LentTickTradeAbi {
            std::ptr::from_ref(trade).cast()
        }
    }
}

/// A slice the host LENDS (`ABI_VERSION` 14): its pointer and length, valid for the dispatch it answers
/// and never kept past it (the guest's `HostBroker` ties the slice it rebuilds to its own borrow). `T` is
/// a type [`layout`] covers or a primitive; since 15 the file lends too (a state's bytes, an order list's
/// legs). Copy by hand: a derive would ask `T: Copy`.
#[repr(C)]
pub struct SliceAbi<T> {
    pub ptr: *const T,
    pub len: usize,
}

impl<T> Clone for SliceAbi<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for SliceAbi<T> {}

impl<T> SliceAbi<T> {
    /// Lends `rows`: valid only while they are alive and unmoved.
    pub fn of(rows: &[T]) -> Self {
        SliceAbi { ptr: rows.as_ptr(), len: rows.len() }
    }

    /// No rows: a null pointer and a zero length, what a thunk answers after a caught panic.
    pub fn empty() -> Self {
        SliceAbi { ptr: std::ptr::null(), len: 0 }
    }

    /// The same rows named as `U`: the pointer and the length, unread. Sound to READ as `U` only where `U`
    /// is laid out as `T` (`lent_in_place!` proves it at the site).
    pub fn cast<U>(self) -> SliceAbi<U> {
        SliceAbi { ptr: self.ptr.cast(), len: self.len }
    }
}

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
/// a loud panic rather than a quietly-corrupted symbol a strategy might then trade on (a lossy read
/// would hand it replacement characters and no signal anything was wrong).
///
/// ⚠ **Caller contract: only ever call this from inside a `catch_unwind`.** It panics, and every
/// call site is reached from an `extern "C"` frame. The template's dispatch exports and
/// `host.rs`'s `guarded` thunks both satisfy it; nothing here can enforce it.
///
/// `what` names the bytes in the panic (`"borrowed string"` on this side, `"symbol"` in the host's
/// thunks); it is formatted only on the failure path, never per call.
///
/// It BORROWS: the `&'a str` is the peer's bytes, valid for the call the pair was lent for and no
/// longer (a caller that keeps it copies it, as [`StrAbi::read`] does), so a thunk's symbol costs no
/// allocation.
pub(crate) fn read_borrowed_str<'a>(ptr: *const u8, len: usize, what: &str) -> &'a str {
    // `expect`'s own `"{msg}: {err:?}"` shape, spelled out so `what` costs nothing on success.
    std::str::from_utf8(read_borrowed_bytes(ptr, len)).unwrap_or_else(|e| {
        panic!("strategy file ABI contract violated: {what} bytes are not valid UTF-8: {e:?}")
    })
}

/// The borrowed bytes `(ptr, len)`, unchecked: [`read_borrowed_str`]'s read before its UTF-8 check, and a
/// strategy state's (`ABI_VERSION` 15: msgpack bytes, no text). It borrows, under [`read_borrowed_str`]'s
/// contract.
pub(crate) fn read_borrowed_bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    read_borrowed_slice(ptr, len, "borrowed bytes")
}

/// The borrowed rows `(ptr, len)` (`ABI_VERSION` 15: bytes, an order list's legs and each leg's links),
/// so the file keeps its ONE `unsafe` site. It borrows, under [`read_borrowed_str`]'s contract; a null or
/// misaligned pointer with rows is a contract violation, refused loudly (the caller is inside a
/// `catch_unwind`), never read. `what` is formatted only on that failure.
pub(crate) fn read_borrowed_slice<'a, T>(ptr: *const T, len: usize, what: &str) -> &'a [T] {
    if len == 0 {
        return &[];
    }
    assert!(
        !ptr.is_null() && ptr.is_aligned(),
        "strategy file ABI contract violated: {what}: {len} rows at a null or misaligned pointer"
    );
    // SAFETY: `(ptr, len)` is the pair this ABI's borrowed-rows contract requires (this module's
    // header): produced from a live slice (a `&str`'s bytes, a byte buffer, a `Vec` of `repr(C)` rows) by
    // the peer across the boundary, and kept alive and unmoved by that peer for at least the duration of
    // the call this read is inside. The pointer is non-null and aligned (asserted above), and `T` is laid
    // out alike on both sides (`repr(C)` under [`layout()`], or a primitive).
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// A strategy state's bytes as a strategy file lends them (`vike_strategy_save_state`, `ABI_VERSION` 15),
/// borrowed for the call: [`read_borrowed_bytes`] of the slice.
pub(crate) fn lent_bytes<'a>(lent: SliceAbi<u8>) -> &'a [u8] {
    read_borrowed_bytes(lent.ptr, lent.len)
}

/// A borrowed `&str` as plain data: `(ptr, len)`, never NUL-terminated. Every mirror added at
/// `ABI_VERSION` 3 uses this type.
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
        read_borrowed_str(self.ptr, self.len, "borrowed string").to_owned()
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

// ---------------------------------------------------------------------------------------------
// The TIER-2 flat mirrors (`ABI_VERSION` 3): scalars plus, where present, a borrowed string.
//
// Each is a `from_*` the HOST builds by borrowing the real payload it already holds, and a `to_*` the
// GUEST copies into an owned Rust value before handing it to user code. Neither side ever sees the
// other's Rust type: `vike-model` is statically linked into both halves, so a `&TickMark` crossing
// the boundary would be two independently-compiled `repr(Rust)` layouts sharing a pointer, which is
// the exact thing this file exists to refuse. The types lent instead are `repr(C)` under [`layout`]:
// `Bar` and `TradeClosedRow` since `ABI_VERSION` 14, and since 15 `vike_marketdata::TickQuote` and
// `TickTrade`, whose hooks take the host's own tick (their two mirrors, and the guest's owned copy of the
// symbol on every tick, are gone).
// ---------------------------------------------------------------------------------------------

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
// `OrderBroker::place` and its siblings take. `OrderBroker::place_group`'s legs cross as a lent slice of
// their own mirror, [`OrderListLegAbi`] (`ABI_VERSION` 15; JSON text before).
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

// `OrderBroker::place_group`'s legs (`ABI_VERSION` 15): a lent slice of [`OrderListLegAbi`], no text.
// The kinds cross as codes this file spells, each mapped by an EXHAUSTIVE `match` both ways, so a variant
// added to `vike_model` is a compile error here (and a change here moves `crate::sdk::BOUNDARY_HASH`).

pub const ORDER_TYPE_MARKET: u32 = 0;
pub const ORDER_TYPE_LIMIT: u32 = 1;
pub const ORDER_TYPE_STOP: u32 = 2;
pub const ORDER_TYPE_STOP_LIMIT: u32 = 3;
pub const ORDER_TYPE_TAKE_PROFIT: u32 = 4;
pub const ORDER_TYPE_MARKET_TO_LIMIT: u32 = 5;
pub const ORDER_TYPE_TRAILING: u32 = 6;
pub const ORDER_TYPE_TRAILING_LIMIT: u32 = 7;
pub const ORDER_TYPE_PEGGED: u32 = 8;
pub const ORDER_TYPE_TRAILING_TAKE_PROFIT: u32 = 9;
/// `vike_model::OrderType::Unrecognized`: its word crosses verbatim in [`OrderListLegAbi::order_type_word`].
pub const ORDER_TYPE_UNRECOGNIZED: u32 = u32::MAX;

pub const TIME_IN_FORCE_GTC: u32 = 0;
pub const TIME_IN_FORCE_IOC: u32 = 1;
pub const TIME_IN_FORCE_FOK: u32 = 2;
pub const TIME_IN_FORCE_GTD: u32 = 3;
pub const TIME_IN_FORCE_DAY: u32 = 4;
pub const TIME_IN_FORCE_POST_ONLY: u32 = 5;
pub const TIME_IN_FORCE_AT_OPEN: u32 = 6;
pub const TIME_IN_FORCE_AT_CLOSE: u32 = 7;

pub const CONTINGENCY_OTO: u32 = 0;
pub const CONTINGENCY_OCO: u32 = 1;
pub const CONTINGENCY_OUO: u32 = 2;

/// One leg of `OrderBroker::place_group` and its tag: `vike_model::OrderListLeg` field for field, its
/// floats as their bits (a NaN or an infinity reaches the host's broker as itself and is refused in the
/// words a native order meets), its strings and `linked` BORROWED from the guest's legs for the call.
/// Under [`layout()`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OrderListLegAbi {
    /// The leg's tag (the pair's first half).
    pub tag: StrAbi,
    pub side: i32,
    pub qty: f64,
    /// One of the `ORDER_TYPE_*` codes.
    pub order_type: u32,
    /// The word of an [`ORDER_TYPE_UNRECOGNIZED`] kind; empty for every other code.
    pub order_type_word: StrAbi,
    pub price: OptionAbi<f64>,
    pub trigger_price: OptionAbi<f64>,
    pub trail: OptionAbi<f64>,
    pub reduce_only: bool,
    /// One of the `TIME_IN_FORCE_*` codes.
    pub time_in_force: u32,
    pub parent: OptionAbi<usize>,
    /// One of the `CONTINGENCY_*` codes.
    pub contingency: OptionAbi<u32>,
    pub linked: SliceAbi<usize>,
    pub order_client_id: OptionAbi<StrAbi>,
}

impl OrderListLegAbi {
    /// Borrows `tag` and `leg`: valid only while both are alive and unmoved. Destructures the leg whole,
    /// so a field added to `vike_model::OrderListLeg` is a compile error here, never a field that stops
    /// crossing.
    pub fn of(tag: &str, leg: &vike_model::OrderListLeg) -> Self {
        use vike_model::{ContingencyType as C, OrderType as K, TimeInForce as F};
        let vike_model::OrderListLeg {
            side,
            qty,
            order_type,
            price,
            trigger_price,
            trail,
            reduce_only,
            time_in_force,
            parent,
            contingency,
            linked,
            order_client_id,
        } = leg;
        let (order_type, order_type_word) = match order_type {
            K::Market => (ORDER_TYPE_MARKET, StrAbi::empty()),
            K::Limit => (ORDER_TYPE_LIMIT, StrAbi::empty()),
            K::Stop => (ORDER_TYPE_STOP, StrAbi::empty()),
            K::StopLimit => (ORDER_TYPE_STOP_LIMIT, StrAbi::empty()),
            K::TakeProfit => (ORDER_TYPE_TAKE_PROFIT, StrAbi::empty()),
            K::MarketToLimit => (ORDER_TYPE_MARKET_TO_LIMIT, StrAbi::empty()),
            K::Trailing => (ORDER_TYPE_TRAILING, StrAbi::empty()),
            K::TrailingLimit => (ORDER_TYPE_TRAILING_LIMIT, StrAbi::empty()),
            K::Pegged => (ORDER_TYPE_PEGGED, StrAbi::empty()),
            K::TrailingTakeProfit => (ORDER_TYPE_TRAILING_TAKE_PROFIT, StrAbi::empty()),
            K::Unrecognized(word) => (ORDER_TYPE_UNRECOGNIZED, StrAbi::of(word)),
        };
        OrderListLegAbi {
            tag: StrAbi::of(tag),
            side: *side,
            qty: *qty,
            order_type,
            order_type_word,
            price: (*price).into(),
            trigger_price: (*trigger_price).into(),
            trail: (*trail).into(),
            reduce_only: *reduce_only,
            time_in_force: match time_in_force {
                F::Gtc => TIME_IN_FORCE_GTC,
                F::Ioc => TIME_IN_FORCE_IOC,
                F::Fok => TIME_IN_FORCE_FOK,
                F::Gtd => TIME_IN_FORCE_GTD,
                F::Day => TIME_IN_FORCE_DAY,
                F::PostOnly => TIME_IN_FORCE_POST_ONLY,
                F::AtOpen => TIME_IN_FORCE_AT_OPEN,
                F::AtClose => TIME_IN_FORCE_AT_CLOSE,
            },
            parent: OptionAbi::new(*parent, 0),
            contingency: OptionAbi::new(
                contingency.map(|c| match c {
                    C::Oto => CONTINGENCY_OTO,
                    C::Oco => CONTINGENCY_OCO,
                    C::Ouo => CONTINGENCY_OUO,
                }),
                0,
            ),
            linked: SliceAbi::of(linked),
            order_client_id: OptionAbi::<StrAbi>::of(order_client_id.as_deref()),
        }
    }

    /// The leg it borrows, owned; `Err` naming the field for a code this side does not know (refused,
    /// never guessed: a guessed kind is an order the strategy never asked for).
    pub fn to_leg(&self) -> Result<(String, vike_model::OrderListLeg), String> {
        use vike_model::{ContingencyType as C, OrderType as K, TimeInForce as F};
        let order_type = match self.order_type {
            ORDER_TYPE_MARKET => K::Market,
            ORDER_TYPE_LIMIT => K::Limit,
            ORDER_TYPE_STOP => K::Stop,
            ORDER_TYPE_STOP_LIMIT => K::StopLimit,
            ORDER_TYPE_TAKE_PROFIT => K::TakeProfit,
            ORDER_TYPE_MARKET_TO_LIMIT => K::MarketToLimit,
            ORDER_TYPE_TRAILING => K::Trailing,
            ORDER_TYPE_TRAILING_LIMIT => K::TrailingLimit,
            ORDER_TYPE_PEGGED => K::Pegged,
            ORDER_TYPE_TRAILING_TAKE_PROFIT => K::TrailingTakeProfit,
            ORDER_TYPE_UNRECOGNIZED => K::Unrecognized(self.order_type_word.read()),
            code => return Err(format!("order type code {code} is not one this host knows")),
        };
        let time_in_force = match self.time_in_force {
            TIME_IN_FORCE_GTC => F::Gtc,
            TIME_IN_FORCE_IOC => F::Ioc,
            TIME_IN_FORCE_FOK => F::Fok,
            TIME_IN_FORCE_GTD => F::Gtd,
            TIME_IN_FORCE_DAY => F::Day,
            TIME_IN_FORCE_POST_ONLY => F::PostOnly,
            TIME_IN_FORCE_AT_OPEN => F::AtOpen,
            TIME_IN_FORCE_AT_CLOSE => F::AtClose,
            code => return Err(format!("time-in-force code {code} is not one this host knows")),
        };
        let contingency = match self.contingency.get() {
            None => None,
            Some(CONTINGENCY_OTO) => Some(C::Oto),
            Some(CONTINGENCY_OCO) => Some(C::Oco),
            Some(CONTINGENCY_OUO) => Some(C::Ouo),
            Some(code) => {
                return Err(format!("contingency code {code} is not one this host knows"));
            }
        };
        let leg = vike_model::OrderListLeg {
            side: self.side,
            qty: self.qty,
            order_type,
            price: self.price.into(),
            trigger_price: self.trigger_price.into(),
            trail: self.trail.into(),
            reduce_only: self.reduce_only,
            time_in_force,
            parent: self.parent.get(),
            contingency,
            linked: read_borrowed_slice(
                self.linked.ptr,
                self.linked.len,
                "an order list leg's links",
            )
            .to_vec(),
            order_client_id: self.order_client_id.read(),
        };
        Ok((self.tag.read(), leg))
    }
}

/// The legs a strategy file lent to `BrokerVTableAbi::place_group`, owned; `Err` naming the leg for one
/// [`OrderListLegAbi::to_leg`] refuses.
pub(crate) fn order_list_of(
    lent: SliceAbi<OrderListLegAbi>,
) -> Result<Vec<(String, vike_model::OrderListLeg)>, String> {
    read_borrowed_slice(lent.ptr, lent.len, "an order list")
        .iter()
        .enumerate()
        .map(|(i, leg)| leg.to_leg().map_err(|e| format!("leg {i}: {e}")))
        .collect()
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
// the book crosses as a CURSOR, the shape the bars crossed in until `ABI_VERSION` 14 lent them in place.
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
    /// memory.
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

/// The name a per-symbol or per-input READ crosses with (`ABI_VERSION` 15): its INDEX in the host
/// broker's own list when the strategy file knows it (`vike_model::OrderBroker::symbols` for a symbol,
/// `vike_model::OrderBroker::input_names` for an input, both read across this boundary once), else its
/// TEXT. The host answers an index through the trait's `_at` read and a text through the named one, and
/// the trait holds the two equal (each `_at` default reads `symbols()[index]` by its name; the simulator
/// answers both through one body), so a read answers alike either way, and a per-bar read by index costs
/// the host no UTF-8 check and no search of a name. Verbs always cross their symbol as text.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NameOrIndexAbi {
    /// The index in the host's list, or [`NameOrIndexAbi::BY_TEXT`].
    pub index: usize,
    /// The name, read only when `index` is [`NameOrIndexAbi::BY_TEXT`]; empty otherwise.
    pub text: StrAbi,
}

impl NameOrIndexAbi {
    /// The `index` that says "read [`NameOrIndexAbi::text`]".
    pub const BY_TEXT: usize = usize::MAX;

    /// The entry at `index` of the host's list. ⚠ Panics on [`NameOrIndexAbi::BY_TEXT`]: crossed, it would
    /// read as the name `""`, the mount's OWN symbol (an index that wrapped, `symbols().len() - 1` on an
    /// empty universe). The guest calls this inside its dispatch's `catch_unwind`, so a strategy file
    /// answers `Panicked`, never a read of another symbol.
    pub fn index(index: usize) -> Self {
        assert!(
            index != Self::BY_TEXT,
            "an index of usize::MAX names no entry (it is the by-text marker): an index that wrapped"
        );
        NameOrIndexAbi { index, text: StrAbi::empty() }
    }

    /// `name`, by its text. Valid only for as long as `name` is alive and unmoved.
    pub fn text(name: &str) -> Self {
        NameOrIndexAbi { index: Self::BY_TEXT, text: StrAbi::of(name) }
    }

    /// The index it carries, `None` for a name by text.
    pub fn get(&self) -> Option<usize> {
        (self.index != Self::BY_TEXT).then_some(self.index)
    }
}

/// The host's side of the boundary: what a strategy file may call back into.
///
/// ⚠ **The per-symbol and per-input reads name their symbol or input by INDEX where the file knows it
/// (`ABI_VERSION` 15, [`NameOrIndexAbi`])**: `position`, `price`, `average_price`, `bars`,
/// `contract_value`, `instrument_grid`, `input_bars` and `input_whole`. `input_name_at` serves the
/// host's input list as `symbol_at` serves its universe.
///
/// ⚠ **The history reads LEND the host's own rows (`ABI_VERSION` 14)**: `bars`, `input_bars`,
/// `closed_trades` and `input_whole` answer a [`SliceAbi`] of the `Bar`s or `TradeClosedRow`s the host
/// keeps, as the boundary's rows ([`LentBarAbi`], [`LentTradeClosedRowAbi`]; [`layout`] holds both sides to
/// one layout), valid for the dispatch they answer. The guest asks
/// again on every read and keeps nothing, so a host whose rows move between dispatches (a `Vec` that
/// grew, paper's and live's rewritten history) is read as it is now. Until 13 they crossed as a count and
/// one item at a time, copied into the file's own history.
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
    pub position: extern "C" fn(*mut core::ffi::c_void, NameOrIndexAbi) -> f64,
    pub price: extern "C" fn(*mut core::ffi::c_void, NameOrIndexAbi) -> f64,
    pub equity: extern "C" fn(*mut core::ffi::c_void) -> f64,
    pub index: extern "C" fn(*mut core::ffi::c_void) -> usize,
    pub now: extern "C" fn(*mut core::ffi::c_void) -> i64,
    /// `OrderBroker::bars(&self, symbol)`, lent.
    pub bars: extern "C" fn(*mut core::ffi::c_void, NameOrIndexAbi) -> SliceAbi<LentBarAbi>,
    pub quote_vwap:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64) -> OptionAbi<f64>,
    pub depth_within_price:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, i32, f64) -> f64,
    /// `OrderBroker::contract_value(&self, symbol)` (`ABI_VERSION` 4), flattened to
    /// [`ContractValueOptionAbi`].
    pub contract_value:
        extern "C" fn(*mut core::ffi::c_void, NameOrIndexAbi) -> ContractValueOptionAbi,
    /// `OrderBroker::instrument_grid(&self, symbol)` (`ABI_VERSION` 5), flattened to [`GridOptionAbi`].
    pub instrument_grid: extern "C" fn(*mut core::ffi::c_void, NameOrIndexAbi) -> GridOptionAbi,
    /// `OrderBroker::input_bars(&self, name)` (`ABI_VERSION` 6), lent.
    pub input_bars: extern "C" fn(*mut core::ffi::c_void, NameOrIndexAbi) -> SliceAbi<LentBarAbi>,
    /// `OrderBroker::closed_trades(&self)` (`ABI_VERSION` 6), lent; `None` is the broker keeping no ledger,
    /// never an empty one.
    pub closed_trades:
        extern "C" fn(*mut core::ffi::c_void) -> OptionAbi<SliceAbi<LentTradeClosedRowAbi>>,
    /// `OrderBroker::input_whole(&self, name)` (`ABI_VERSION` 10), lent; `None` is no whole series served
    /// (paper, live, a file not marked `runs_on = "backtest"`), never an empty one.
    pub input_whole:
        extern "C" fn(*mut core::ffi::c_void, NameOrIndexAbi) -> OptionAbi<SliceAbi<LentBarAbi>>,
    /// `OrderBroker::history_only_grows(&self)` (`ABI_VERSION` 13): whether every history read answers a
    /// prefix of a series that never changes (the simulator's). Nothing in the file keeps history on it
    /// since 14; the order language reads it (`vike_tactics::Tactics::closed_net`).
    pub history_only_grows: extern "C" fn(*mut core::ffi::c_void) -> bool,

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
    /// `OrderBroker::place_group(symbol, legs)`: the symbol, then the legs LENT as [`OrderListLegAbi`]
    /// rows for the call (`ABI_VERSION` 15; their JSON text before). The mirror destructures
    /// `vike_model::OrderListLeg` whole, so a field added there cannot silently stop crossing.
    pub place_group:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, SliceAbi<OrderListLegAbi>),
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
    /// `OrderBroker::publish_forecast(symbol, forecast)` (`ABI_VERSION` 11, book W35): `symbol`, then the
    /// forecast's three terms (`vike_model::Forecast`'s value, horizon in milliseconds and optional
    /// confidence), which the host rebuilds through `vike_model::Forecast::new`, the one rule.
    pub publish_forecast:
        extern "C" fn(*mut core::ffi::c_void, *const u8, usize, f64, i64, OptionAbi<f64>),
    /// `OrderBroker::average_price(&self, symbol)`.
    pub average_price: extern "C" fn(*mut core::ffi::c_void, NameOrIndexAbi) -> f64,
    /// Symbol `i` of `OrderBroker::symbols(&self)`, the run's universe (`ABI_VERSION` 8), borrowed
    /// from the host's list, which does not change while the strategy file is called; `None` past its end.
    pub symbol_at: extern "C" fn(*mut core::ffi::c_void, usize) -> OptionAbi<StrAbi>,
    /// Input `i` of `OrderBroker::input_names(&self)` (`ABI_VERSION` 15), borrowed as `symbol_at`'s is;
    /// `None` past its end.
    pub input_name_at: extern "C" fn(*mut core::ffi::c_void, usize) -> OptionAbi<StrAbi>,
}

#[path = "abi_tests.rs"]
#[cfg(test)]
mod abi_tests;
