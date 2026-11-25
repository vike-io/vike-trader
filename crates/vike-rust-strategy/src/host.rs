//! The HOST side: wraps a loaded strategy file handle as an ordinary [`vike_model::Strategy`], so
//! `vike-studio-core`/`vike-backend` drive it exactly like any built-in strategy.
//!
//! ⚠ **Layering forces this to be GENERIC over `OrderBroker`, not hard-wired to `OrderBrokerSimulator`.** The
//! design names this crate's job as producing "`Strategy<OrderBrokerSimulator>`", but `OrderBrokerSimulator` lived in
//! `vike-backtest` when this was written, declared `layer = 50` — ABOVE this crate's then
//! `layer = 41` (`crates/vike-ops/tests/architecture/layer_gate.rs`: every normal dependency must be STRICTLY
//! lower). So `vike-rust-strategy` could not name `OrderBrokerSimulator` at all without inverting the
//! dependency graph. ⚠ That reason briefly EXPIRED on 2026-09-27, when `OrderBrokerSimulator`
//! (`crates/vike-simulator/src/engine/sim_broker.rs`) moved to `vike-simulator` at layer 30 (docs/decisions/0087),
//! and came back the same day: this crate dropped to `layer = 15`, so the simulator is ABOVE it again
//! and naming `OrderBrokerSimulator` is illegal by rank once more. The generic shape was never only a rank
//! workaround anyway — it is the NARROWER one: this crate needs only `OrderBroker`, and a normal edge
//! on the simulator would put it into every consumer's graph for nothing.
//! Every thunk and [`StrategyFileHost`] below are instead generic over any
//! `B: vike_model::OrderBroker + 'static`; the module doc this crate opened with already
//! anticipated exactly this shape ("`vike-studio-core`, `vike-backend` and every consumer receive
//! an ordinary `Box<dyn Strategy<OrderBrokerSimulator>>`") — it is a HIGHER-layer crate that instantiates
//! `B = OrderBrokerSimulator` by naming `StrategyFileHost<OrderBrokerSimulator>`, never this one.
//!
//! ⚠ **`OrderBroker` alone since `ABI_VERSION` 8 (the one broker interface).** A user `build` bounds
//! `B: vike_model::OrderBroker + 'static`, so `broker_vtable` and [`StrategyFileHost`] take exactly
//! that; `OrderBrokerSimulator` and the live core's `LiveBroker` implement it, and every thunk bounds it (the
//! one broker plan's step 9 moved the reads of the deleted `Broker` trait into it). Until 8 they also
//! bound `HftBroker`, whose symbol-less `position` made every call to either `position` a UFCS call;
//! nothing here names it any more.
//!
//! ⚠ **Review Critical 1: every `t_*` thunk below wraps its body in `catch_unwind`.** These are
//! `extern "C"` frames CALLED BY STRATEGY FILE CODE — the OTHER direction from the ones the design's
//! global constraint named ("every extern "C" entry point in the strategy file wraps catch_unwind"),
//! which is why that rule alone did not cover them. A panic in a host-side `OrderBroker` impl (a
//! `OrderBrokerSimulator` unwrap, an out-of-range index, an allocation failure) or in `read_str` (this file,
//! deliberately panicking rather than lossy on a contract violation) would otherwise unwind out
//! of a C-ABI frame and hit rustc's abort shim — "it destroys the backtest server", the exact
//! outcome the design exists to prevent. See [`guarded`].

use std::any::TypeId;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::{Mutex, OnceLock};

use vike_marketdata::Bar;
use vike_model::{OrderBroker, Strategy, StrategyFailures};

use crate::abi::{
    BookRef, BrokerRef, BrokerVTableAbi, FillAbi, FlowToxicityAbi, LentBarAbi, LentTickQuoteAbi,
    LentTickTradeAbi, OptionAbi, OrderEventAbi, StrAbi, StrategyFileStatusAbi, TickMarkAbi,
};

use book::{BOOK_VTABLE, BookCursor};
use thunks::{
    t_amend, t_avg_price, t_bars, t_cancel, t_closed_trades, t_contract_value, t_deny,
    t_depth_within_price, t_equity, t_history_only_grows, t_index, t_input_bars, t_input_name_at,
    t_input_whole, t_instrument_grid, t_now, t_place, t_place_at_close, t_place_close_from_fill,
    t_place_exit_leg, t_place_group, t_position, t_price, t_publish_forecast, t_quote_vwap,
    t_rests_reduce_only_while_flat, t_symbol_at,
};

mod book;
mod thunks;

thread_local! {
    /// Set by [`guarded`] when a host-side thunk panics during the CURRENT dispatch, to the name of
    /// the call that panicked (`OrderBroker::place`), so the re-raise names it.
    /// `StrategyFileHost::on_bar` resets it before dispatching into the strategy file and reads it after —
    /// see `guarded`'s own doc for why swallowing the panic without this would be worse than not
    /// catching it at all.
    static POISONED: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Run `f`, catching any panic and returning `neutral` instead, so a panic inside a `t_*` thunk
/// (below) — an `extern "C"` frame CALLED BY STRATEGY FILE CODE — never unwinds across that C-ABI frame.
/// See this module's header (Critical 1) for why the design's own "the strategy file wraps catch_unwind"
/// rule does not already cover this direction.
///
/// `AssertUnwindSafe`: `&mut B` is not `UnwindSafe` by default (a panic mid-mutation could leave
/// the referent inconsistent), and this crate accepts that deliberately — a panicking `OrderBroker`
/// impl is a HOST-SIDE BUG, and the alternative (an uncaught unwind aborting the whole backtest
/// server) is strictly worse than a broker left in a possibly-inconsistent state whose CALLER is
/// about to be told, loudly, to stop trusting this dispatch (see [`POISONED`] below).
///
/// Latches [`POISONED`] rather than merely swallowing the panic: a thunk quietly returning `0.0`
/// for `equity` after a panic would read as a plausible, WRONG NUMBER with no signal anything was
/// skipped — silently wrong is worse than loudly failed. `StrategyFileHost::on_bar` reads the flag
/// once the strategy file's call returns and re-raises as a genuine Rust panic, which is safe to do
/// there: that frame is back on the host's own stack, no C-ABI boundary in the way.
///
/// ⚠ **Known limit of the poison design, recorded rather than fixed (round-2 review): the
/// re-panic in `on_bar` stops the CALLER trusting the REST of this dispatch, but it does not undo
/// side effects a strategy file already took on an EARLIER neutral value.** A strategy file whose `on_bar` reads
/// `equity()` (returns `0.0`, poisoned), sizes an order off that `0.0`, submits it, and THEN calls
/// `position()` (which also panics) will have already placed that order by the time `on_bar`
/// re-panics — the poison flag makes the DISPATCH untrustworthy in hindsight, it does not roll
/// back what the strategy file already did with the bad number. Track A's scope is "does not abort the
/// process"; "does not act on a bad number" would need the host to refuse `submit_*` calls once
/// poisoned, which is follow-up work, not built here.
///
/// ⚠ **The recovery path itself must not be able to escape this frame.** `eprintln!` panics if
/// stderr is closed — reporting the ORIGINAL panic must never itself become a SECOND, uncaught one
/// unwinding across the very `extern "C"` frame this function exists to protect. [`POISONED`] is
/// therefore set FIRST, unconditionally, and the message formatting + `eprintln!` run inside their
/// OWN nested `catch_unwind` — best-effort reporting, never load-bearing.
///
/// `call` names the seam verb the thunk forwards to (`OrderBroker::place`, `OrderBroker::equity`), so
/// both diagnostics say which call failed.
fn guarded<F: FnOnce() -> R, R>(call: &'static str, neutral: R, f: F) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(payload) => {
            POISONED.with(|p| p.set(Some(call)));
            // Best-effort: if even reporting the panic panics (a closed stderr), swallow that
            // too rather than let it unwind across this extern "C" frame in its place.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let msg = vike_utility::panic::payload_text(&*payload);
                eprintln!(
                    "vike-rust-strategy: the host-side `{call}` call panicked while a strategy file \
                     was calling back into the host: {msg}. Returning a neutral placeholder for \
                     this one call; the dispatch that triggered it is poisoned — see \
                     StrategyFileHost::on_bar."
                );
            }));
            neutral
        }
    }
}

/// The `BrokerVTableAbi` a strategy file calls back into, monomorphized for `B` and cached for the life of
/// the process — one leaked `BrokerVTableAbi` per DISTINCT `B` ever asked for (in the shipped
/// binaries, exactly one: `OrderBrokerSimulator`).
///
/// Generic rather than the design's literal "`broker_vtable() -> &'static BrokerVTableAbi`" —
/// necessarily so; see this module's header. The cache is keyed by [`TypeId`] rather than a plain
/// function-local `static`, because a `static` declared inside a generic function whose TYPE does
/// not itself mention the generic parameter is exactly ONE instance shared across every
/// monomorphization (a well-known Rust footgun): `BrokerVTableAbi`'s fields are already type-erased
/// `extern "C" fn(*mut c_void, ..)` pointers, so its type never mentions `B`, and a naive
/// `static VT: OnceLock<BrokerVTableAbi>` inside this function would hand every `B` the FIRST
/// caller's thunks. The registry sidesteps it with zero `unsafe`: `Box::leak` gives a genuinely
/// `'static` reference per key, and the map is read back by value (`&'static BrokerVTableAbi` is
/// `Copy`), so nothing here borrows from the `MutexGuard`.
///
/// ⚠ Called ONCE per [`StrategyFileHost`] (from `StrategyFileHost::new`), not once per dispatch — a
/// review flagged the earlier version's per-`on_bar` call as a `Mutex` lock + `TypeId` hash +
/// `HashMap` lookup paid on every bar in the backtest hot loop, for a value that never changes for
/// a given `B`.
pub fn broker_vtable<B: OrderBroker + 'static>() -> &'static BrokerVTableAbi {
    static REGISTRY: OnceLock<Mutex<HashMap<TypeId, &'static BrokerVTableAbi>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    map.entry(TypeId::of::<B>()).or_insert_with(|| {
        Box::leak(Box::new(BrokerVTableAbi {
            position: t_position::<B>,
            price: t_price::<B>,
            equity: t_equity::<B>,
            index: t_index::<B>,
            now: t_now::<B>,
            bars: t_bars::<B>,
            quote_vwap: t_quote_vwap::<B>,
            depth_within_price: t_depth_within_price::<B>,
            contract_value: t_contract_value::<B>,
            instrument_grid: t_instrument_grid::<B>,
            input_bars: t_input_bars::<B>,
            closed_trades: t_closed_trades::<B>,
            input_whole: t_input_whole::<B>,
            history_only_grows: t_history_only_grows::<B>,
            place: t_place::<B>,
            place_exit_leg: t_place_exit_leg::<B>,
            place_at_close: t_place_at_close::<B>,
            place_close_from_fill: t_place_close_from_fill::<B>,
            rests_reduce_only_while_flat: t_rests_reduce_only_while_flat::<B>,
            place_group: t_place_group::<B>,
            amend: t_amend::<B>,
            cancel: t_cancel::<B>,
            deny: t_deny::<B>,
            publish_forecast: t_publish_forecast::<B>,
            average_price: t_avg_price::<B>,
            symbol_at: t_symbol_at::<B>,
            input_name_at: t_input_name_at::<B>,
        }))
    })
}

/// A loaded strategy file's exported dispatch symbols this file gives meaning to. `loader.rs` (Task 4)
/// is what NAMES and RESOLVES these via `dlsym`; this struct is defined here because
/// [`StrategyFileHost`] is what the pointers mean, and the loader only produces one.
///
/// ⚠ **It used to be deliberately NARROW — `create`/`destroy` plus `warmup`/`on_bar` — and it is
/// not any more.** At `ABI_VERSION` 3 it carries the WHOLE `vike_model::Strategy` seam except the
/// members named in [`UNWIRED_HOOKS`], so a strategy file receives what a compiled-in strategy
/// receives. The narrow version was safe only because of the build-time refusal that reads
/// [`UNWIRED_HOOKS`]; every hook moved from that list to [`WIRED_HOOKS`] is a refusal lifted. At
/// `ABI_VERSION` 8 the last four moved, so it carries the whole seam.
///
/// Field order mirrors the declaration order of `vike_model::Strategy`'s own methods, so a reader
/// comparing the two reads them in one pass.
#[derive(Clone, Copy, Debug)]
pub struct StrategyVTableAbi {
    /// The params as a borrowed TOML document, then the mount facts as a borrowed JSON document
    /// (`vike_model::params::MountFacts`, `ABI_VERSION` 9). Null when either does not parse, the
    /// entry panicked, or a `rule` file was handed no symbol.
    pub create: extern "C" fn(*const u8, usize, *const u8, usize) -> *mut c_void,
    pub destroy: extern "C" fn(*mut c_void),
    pub warmup: extern "C" fn(*mut c_void) -> usize,
    /// `Strategy::inputs` (`ABI_VERSION` 6): the names as the JSON array text
    /// `guest::names_json` writes, lent by the strategy file until its next call or `destroy`;
    /// `None` when its own `catch_unwind` caught a panic, never read as "declares none".
    pub inputs: extern "C" fn(*mut c_void) -> OptionAbi<StrAbi>,
    /// `Strategy::symbols` (`ABI_VERSION` 8), lent as `inputs` is: the symbols the strategy file
    /// trades beside its own.
    pub symbols: extern "C" fn(*mut c_void) -> OptionAbi<StrAbi>,
    /// `Strategy::stated_fee` (`ABI_VERSION` 13), answered as `params` is: the inner `None` is the
    /// strategy stating none, the text the JSON of its `vike_model::FeeSchedule` (`guest::fee_json`).
    pub stated_fee: extern "C" fn(*mut c_void) -> OptionAbi<OptionAbi<StrAbi>>,
    /// `Strategy::stated_slippage` (`ABI_VERSION` 13), answered as `stated_fee` is, the text the JSON of
    /// its `vike_model::ProfileEngineSlippage` (`guest::slippage_json`).
    pub stated_slippage: extern "C" fn(*mut c_void) -> OptionAbi<OptionAbi<StrAbi>>,
    pub on_start: extern "C" fn(*mut c_void, BrokerRef) -> StrategyFileStatusAbi,
    /// The host's own bar, lent for the call as the boundary's row (`ABI_VERSION` 14: `Bar` is `repr(C)`,
    /// laid out as `abi::LentBarAbi` under `abi::layout`).
    pub on_bar: extern "C" fn(*mut c_void, BrokerRef, *const LentBarAbi) -> StrategyFileStatusAbi,
    /// The host's own quote, lent for the call as `on_bar`'s bar is (`ABI_VERSION` 15: `TickQuote` is
    /// `repr(C)` under `abi::layout`).
    pub on_quote_tick:
        extern "C" fn(*mut c_void, BrokerRef, *const LentTickQuoteAbi) -> StrategyFileStatusAbi,
    /// The host's own trade, lent for the call (`ABI_VERSION` 15, as `on_quote_tick`).
    pub on_trade_tick:
        extern "C" fn(*mut c_void, BrokerRef, *const LentTickTradeAbi) -> StrategyFileStatusAbi,
    pub on_order_book: extern "C" fn(*mut c_void, BrokerRef, BookRef) -> StrategyFileStatusAbi,
    /// `tag` crosses as the borrowed `(ptr, len)` every string in this ABI uses.
    pub on_schedule:
        extern "C" fn(*mut c_void, BrokerRef, *const u8, usize) -> StrategyFileStatusAbi,
    pub on_fill: extern "C" fn(*mut c_void, BrokerRef, *const FillAbi) -> StrategyFileStatusAbi,
    /// One of `abi`'s `FEED_STATUS_*` codes — see `abi::feed_status_code`.
    pub on_feed_status: extern "C" fn(*mut c_void, BrokerRef, u32) -> StrategyFileStatusAbi,
    pub on_mark: extern "C" fn(*mut c_void, BrokerRef, *const TickMarkAbi) -> StrategyFileStatusAbi,
    /// `venue` is a borrowed `(ptr, len)`, carried as an ARGUMENT because no tick type carries a
    /// venue — `vike_model::Strategy::on_reference_quote`'s own doc argues why. The quote is the host's
    /// own, lent as `on_quote_tick`'s is.
    pub on_reference_quote: extern "C" fn(
        *mut c_void,
        BrokerRef,
        *const u8,
        usize,
        *const LentTickQuoteAbi,
    ) -> StrategyFileStatusAbi,
    pub on_flow:
        extern "C" fn(*mut c_void, BrokerRef, *const FlowToxicityAbi) -> StrategyFileStatusAbi,
    pub on_order_event:
        extern "C" fn(*mut c_void, BrokerRef, *const OrderEventAbi) -> StrategyFileStatusAbi,
    /// The params bag as a JSON document, borrowed `(ptr, len)` — `guest::strategy_params_from_json`
    /// carries the argument for JSON over TOML and over a flat mirror.
    pub on_params_updated:
        extern "C" fn(*mut c_void, BrokerRef, *const u8, usize) -> StrategyFileStatusAbi,
    /// `Strategy::params` (`ABI_VERSION` 8): the outer `None` is a caught panic, the inner one the
    /// strategy publishing none, and the text the JSON of its `StrategyParams`, lent as `inputs` is.
    pub params: extern "C" fn(*mut c_void) -> OptionAbi<OptionAbi<StrAbi>>,
    pub on_stop: extern "C" fn(*mut c_void, BrokerRef) -> StrategyFileStatusAbi,
    /// `Strategy::save_state` (`ABI_VERSION` 8), answered as `params` is: the inner `None` is the
    /// strategy saving nothing, the bytes the state it saves (`vike_model::StrategyState`, msgpack with
    /// field names since `ABI_VERSION` 15), lent from the handle until its next call.
    pub save_state: extern "C" fn(*mut c_void) -> OptionAbi<OptionAbi<crate::abi::SliceAbi<u8>>>,
    /// `Strategy::load_state` (`ABI_VERSION` 8): the state's borrowed bytes (msgpack since 15); bytes that
    /// are not one msgpack value answer `BadParams` and restore nothing.
    pub load_state: extern "C" fn(*mut c_void, *const u8, usize) -> StrategyFileStatusAbi,
    /// The file's optional `refuse` export over a borrowed params document (`ABI_VERSION` 9), asked
    /// with no strategy created: the inner `None` is no refusal (or no `refuse` export), the text the
    /// refusal itself, lent from the file's per-thread buffer; the outer `None` a caught panic or a
    /// document that does not parse.
    pub refuse: extern "C" fn(*const u8, usize) -> OptionAbi<OptionAbi<StrAbi>>,
    /// The file's optional `echo` export over the params it was created with (`ABI_VERSION` 9): the
    /// inner `None` is a file with no `echo` export, the text the JSON `guest::echo_json` writes.
    pub echo: extern "C" fn(*mut c_void) -> OptionAbi<OptionAbi<StrAbi>>,
}

/// A strategy file instance wearing [`Strategy<B>`] for the SAME `B` it was constructed with.
///
/// Generic over `B` (rather than the earlier non-generic struct implementing `Strategy<B>` for
/// EVERY `B` via a blanket impl) for two reasons a review surfaced together: it lets `new` resolve
/// and cache [`broker_vtable`] ONCE at construction instead of once per dispatch, and it makes the
/// type system — not a runtime check — refuse a `StrategyFileHost<OrderBrokerSimulator>` ever being asked to
/// route a DIFFERENT broker's `ctx` through thunks built for `OrderBrokerSimulator`.
///
/// RAII over the strategy file's own lifecycle: [`StrategyFileHost::new`] calls `create` once, [`Drop`]
/// calls `destroy` once — the caller never needs an explicit teardown call.
pub struct StrategyFileHost<B> {
    vt: StrategyVTableAbi,
    handle: *mut c_void,
    /// Resolved once in `new`, not on every `on_bar` — see [`broker_vtable`]'s own doc for the
    /// cost this replaces.
    broker_vtable: &'static BrokerVTableAbi,
    /// Every dispatch that came back other than `Ok` (`StrategyFileHost::report`), the answer of
    /// `Strategy::failures`: counted HERE because the status is all that crosses (the file's own panic
    /// text goes to its stderr).
    failed: RefCell<StrategyFailures>,
    _broker: PhantomData<B>,
}

impl<B: OrderBroker + 'static> StrategyFileHost<B> {
    /// `params_toml` crosses as `(ptr, len)` TOML text, never a `vike_model::params::Params` — no
    /// Rust type crosses this ABI by value (this module's header). The strategy file parses it back into
    /// `Params` on its own side.
    ///
    /// Does NOT refuse a null `create` result with a `Result` return: `create`/`destroy` are the
    /// ONE lifecycle pair this ABI gives no separate status channel for (unlike `on_bar`, which
    /// returns a real [`StrategyFileStatusAbi`]) — null is the only signal a failed creation has, and a
    /// fallible constructor a caller can `.unwrap()` without reading is no safer than what this
    /// does instead: carry the null handle, and have EVERY dispatch method check it before
    /// calling into the strategy file at all (`warmup`/`on_bar` below), reporting
    /// `StrategyFileStatusAbi::BadHandle` — the enum variant this ABI already declares for exactly this.
    ///
    /// `facts_json` is the mount's [`vike_model::params::MountFacts`] as JSON text (`ABI_VERSION` 9),
    /// crossed the same way; `"{}"` states none.
    pub fn new(vt: StrategyVTableAbi, params_toml: &str, facts_json: &str) -> Self {
        let handle = (vt.create)(
            params_toml.as_ptr(),
            params_toml.len(),
            facts_json.as_ptr(),
            facts_json.len(),
        );
        Self {
            vt,
            handle,
            broker_vtable: broker_vtable::<B>(),
            failed: RefCell::default(),
            _broker: PhantomData,
        }
    }

    /// Whether `create` handed back a strategy: `false` when the params or the facts did not
    /// parse, the entry panicked, or a `rule` file was handed no symbol. Every hook of such a host
    /// is a reported no-op, so a caller that can refuse a mount asks this first.
    pub fn created(&self) -> bool {
        !self.handle.is_null()
    }

    /// The file's effective-params line (its optional `echo` export, `ABI_VERSION` 9), as
    /// `(key, value)` pairs in the file's order; `None` for a file with no `echo` export.
    pub fn echo(&self) -> Option<Vec<(String, String)>> {
        let text = self.lent("echo", self.vt.echo)?.get()?.read();
        Some(serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!(
                "strategy file ABI contract violated: the strategy file's `echo` is not a JSON \
                 list of [key, value] pairs: {e}"
            )
        }))
    }

    /// The ONE body every dispatch method below shares: refuse a null handle, arm the poison
    /// latch, hand the strategy file a `BrokerRef` built on THIS call's `&mut B`, then judge what came
    /// back.
    ///
    /// ⚠ **It is a shared helper because it was a shared BODY, and the widening from two hooks to
    /// fifteen is what made that matter.** Each of the four acts below is easy to leave out of one
    /// hook and impossible to notice afterwards: a missing `POISONED` reset makes a LATER hook
    /// inherit an earlier one's panic, a missing poison READ lets a neutral placeholder pass for
    /// an answer, and a missing status check drops a `Panicked` dispatch in silence. Fifteen
    /// hand-written copies would have been fifteen chances to omit one, and the omission reads as
    /// working code.
    ///
    /// `hook` is named in both diagnostics so a failure says WHICH seam method failed — with two
    /// hooks the message could hard-code `on_bar`; with fifteen a generic message would send a
    /// reader hunting.
    fn dispatch(
        &self,
        hook: &'static str,
        broker: &mut B,
        call: impl FnOnce(*mut c_void, BrokerRef) -> StrategyFileStatusAbi,
    ) {
        let status = if self.handle.is_null() {
            // Produce the REAL enum value rather than merely naming it in a message: a null
            // handle is exactly what `StrategyFileStatusAbi::BadHandle` models, so it is reported through
            // the same `status != StrategyFileStatusAbi::Ok` path below instead of a parallel one.
            StrategyFileStatusAbi::BadHandle
        } else {
            POISONED.with(|p| p.set(None));
            let r = BrokerRef { ctx: (broker as *mut B).cast(), vtable: self.broker_vtable };
            let s = call(self.handle, r);
            if let Some(failed) = POISONED.with(Cell::get) {
                // Safe to raise a genuine panic HERE: this frame is back on the host's own stack,
                // with no C-ABI boundary between it and the panic site — unlike the thunks
                // `guarded` protects, propagating here does not abort the process. Correct to
                // raise one: continuing would let the CALLER (this method's own caller) believe
                // whatever neutral placeholder the panicking thunk returned was a real answer.
                panic!(
                    "vike-rust-strategy: the host-side `{failed}` call panicked while this \
                     strategy file's {hook} was dispatching (see the panic message printed above for \
                     the real cause)."
                );
            }
            s
        };
        self.report(hook, status, || Some(broker.now()));
    }

    /// Report a dispatch whose status is not `Ok`, and count it (`Strategy::failures`) at the
    /// strategy clock `at` answers (read only on a failure). Every `Strategy` hook returns `()` — there
    /// is no Result to propagate through the trait, so a non-Ok status is reported rather than
    /// silently dropped.
    /// `eprintln!`, not `tracing` (which this crate takes only to carry a file's own events,
    /// [`host_log`]): a dispatch failure must reach an operator's eyes even in a root that installs
    /// no subscriber, and stderr is what the composition roots wrapping this call already capture.
    fn report(&self, hook: &str, status: StrategyFileStatusAbi, at: impl FnOnce() -> Option<i64>) {
        let why = match status {
            StrategyFileStatusAbi::Ok => return,
            StrategyFileStatusAbi::Panicked => "panicked (the file's own message is on stderr)",
            StrategyFileStatusAbi::BadHandle => "was not run: the file's create() made no strategy",
            StrategyFileStatusAbi::BadParams => "was not run: the file could not decode its input",
        };
        eprintln!(
            "vike-rust-strategy: strategy file {hook} returned {status:?} — this dispatch was \
             SKIPPED, so the strategy did not see the event. A Panicked status means the \
             strategy file's own catch_unwind caught a panic in user strategy code (\"a panic across \
             a C-ABI boundary is UB\" — the design doc); BadHandle means create() returned a \
             null handle; BadParams means the payload carried a code or a document this \
             strategy file could not decode."
        );
        self.failed.borrow_mut().record(at(), &format!("{hook} {why}"));
    }

    /// What a LENDING export answered (`inputs`, `symbols`, `params`, `save_state`): the strategy
    /// file keeps its text only until its next call, so the caller copies it at once. `None` for a
    /// null handle, which is never asked.
    ///
    /// ⚠ A caught panic PANICS here, on the host's own stack, and never reads as an empty answer:
    /// a run that loaded nothing for a strategy that reads an input, mounted none of the legs it
    /// trades or restarted with no state it saved would trade differently from the same source
    /// compiled in, with no signal, and a compiled-in hook that panics stops its run too.
    fn lent<T: Copy>(
        &self,
        hook: &str,
        export: extern "C" fn(*mut c_void) -> OptionAbi<T>,
    ) -> Option<T> {
        if self.handle.is_null() {
            eprintln!(
                "vike-rust-strategy: the strategy file's handle is null (StrategyFileStatusAbi::BadHandle \
                 — create() failed) — `{hook}` answers none rather than calling into it"
            );
            return None;
        }
        let Some(answer) = export(self.handle).get() else {
            panic!(
                "vike-rust-strategy: the strategy file's `{hook}` panicked (its own catch_unwind \
                 caught it), so what it answers is unknown and this run cannot go on without it"
            );
        };
        Some(answer)
    }
}

/// The host's log slot (`abi::LogFn`, `ABI_VERSION` 9): a strategy file's own `tracing` event,
/// re-emitted through THIS process's `tracing` under the target `strategy_file`, its module path in
/// the file as the `file` field. The loader hands it to every file it loads
/// (`loader::load`'s `vike_strategy_set_log`). Never unwinds into the file: a bad string is dropped.
pub extern "C" fn host_log(level: u32, target: StrAbi, message: StrAbi) {
    let _ = std::panic::catch_unwind(|| {
        let (file, message) = (target.read(), message.read());
        match level {
            1 => tracing::error!(target: "strategy_file", file = %file, "{message}"),
            2 => tracing::warn!(target: "strategy_file", file = %file, "{message}"),
            3 => tracing::info!(target: "strategy_file", file = %file, "{message}"),
            4 => tracing::debug!(target: "strategy_file", file = %file, "{message}"),
            _ => tracing::trace!(target: "strategy_file", file = %file, "{message}"),
        }
    });
}

/// The file's own refusal of `params_toml` (its optional `refuse` export, `ABI_VERSION` 9), asked
/// before any strategy is created: `None` when it has no `refuse` or refuses nothing. A caught panic,
/// or a document the file cannot parse, PANICS here, never reads as "no refusal" (a mount must not
/// open on params nobody checked).
pub fn refusal(vt: &StrategyVTableAbi, params_toml: &str) -> Option<String> {
    let Some(answer) = (vt.refuse)(params_toml.as_ptr(), params_toml.len()).get() else {
        panic!(
            "vike-rust-strategy: the strategy file's `refuse` panicked or could not parse the params \
             document, so whether it refuses them is unknown"
        );
    };
    answer.get().map(|text| text.read())
}

/// A lent JSON list of names (`inputs`, `symbols`), decoded; a null handle's is empty.
fn names_of(hook: &str, lent: Option<StrAbi>) -> Vec<String> {
    lent.map(|text| {
        serde_json::from_str(&text.read()).unwrap_or_else(|e| {
            panic!(
                "strategy file ABI contract violated: the strategy file's `{hook}` are not a JSON list of \
                 names: {e}"
            )
        })
    })
    .unwrap_or_default()
}

impl<B> Drop for StrategyFileHost<B> {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            (self.vt.destroy)(self.handle);
        }
    }
}

// `StrategyFileHost` holds a raw `*mut c_void` a single strategy file instance owns exclusively; it is
// neither `Send` nor `Sync` by default (a raw pointer field opts both out), which is correct for
// Track A's backtest-only scope — nothing here claims the strategy file's own code is thread-safe.

impl<B: OrderBroker + 'static> Strategy<B> for StrategyFileHost<B> {
    fn warmup(&self) -> usize {
        if self.handle.is_null() {
            eprintln!(
                "vike-rust-strategy: strategy file handle is null (StrategyFileStatusAbi::BadHandle — create() \
                 failed) — warmup defaults to 0 rather than calling into the strategy file"
            );
            return 0;
        }
        (self.vt.warmup)(self.handle)
    }

    /// Read once, before the run (the backtest loads only these). A failure panics (`StrategyFileHost::lent`).
    fn inputs(&self) -> Vec<String> {
        names_of("inputs", self.lent("inputs", self.vt.inputs))
    }

    /// Read once, before a live mount (it declares each as a leg). A failure panics
    /// (`StrategyFileHost::lent`).
    fn symbols(&self) -> Vec<String> {
        names_of("symbols", self.lent("symbols", self.vt.symbols))
    }

    /// Read once, before a backtest (charged where its profile states no fee), as `params` is read: a
    /// failure panics, and so does a document that is not a `FeeSchedule`, never read as "states none".
    fn stated_fee(&self) -> Option<vike_model::FeeSchedule> {
        let text = self.lent("stated_fee", self.vt.stated_fee)?.get()?.read();
        Some(serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!(
                "strategy file ABI contract violated: the strategy file's `stated_fee` is not a \
                 FeeSchedule document: {e}: {text}"
            )
        }))
    }

    /// Read once, before a backtest (charged where the run states no slippage), as `stated_fee` is.
    fn stated_slippage(&self) -> Option<vike_model::ProfileEngineSlippage> {
        let text = self.lent("stated_slippage", self.vt.stated_slippage)?.get()?.read();
        Some(serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!(
                "strategy file ABI contract violated: the strategy file's `stated_slippage` is not a \
                 slippage table: {e}: {text}"
            )
        }))
    }

    fn on_start(&mut self, broker: &mut B) {
        self.dispatch("on_start", broker, |h, r| (self.vt.on_start)(h, r));
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.dispatch("on_bar", broker, |h, r| (self.vt.on_bar)(h, r, LentBarAbi::lend(bar)));
    }

    /// The host's own quote, lent for the call (`ABI_VERSION` 15): nothing is copied, as for `on_bar`.
    fn on_quote_tick(&mut self, broker: &mut B, q: &vike_marketdata::TickQuote) {
        self.dispatch("on_quote_tick", broker, |h, r| {
            (self.vt.on_quote_tick)(h, r, LentTickQuoteAbi::lend(q))
        });
    }

    /// The host's own trade, lent for the call (`ABI_VERSION` 15).
    fn on_trade_tick(&mut self, broker: &mut B, t: &vike_marketdata::TickTrade) {
        self.dispatch("on_trade_tick", broker, |h, r| {
            (self.vt.on_trade_tick)(h, r, LentTickTradeAbi::lend(t))
        });
    }

    fn on_order_book(&mut self, broker: &mut B, book: &vike_marketdata::L2Book) {
        // Flattened ONCE per dispatch, here, and lent to the strategy file for exactly this call — see
        // `BookCursor`'s own doc for why the cursor's `ctx` is this and not the `&L2Book`.
        let cursor = BookCursor::of(book);
        let book_ref =
            BookRef { ctx: (&raw const cursor).cast::<c_void>().cast_mut(), vtable: &BOOK_VTABLE };
        self.dispatch("on_order_book", broker, |h, r| (self.vt.on_order_book)(h, r, book_ref));
    }

    fn on_schedule(&mut self, broker: &mut B, tag: &str) {
        self.dispatch("on_schedule", broker, |h, r| {
            (self.vt.on_schedule)(h, r, tag.as_ptr(), tag.len())
        });
    }

    fn on_fill(&mut self, broker: &mut B, fill: &vike_model::Fill) {
        let c = FillAbi::of(fill);
        self.dispatch("on_fill", broker, |h, r| (self.vt.on_fill)(h, r, &c as *const FillAbi));
    }

    fn on_feed_status(&mut self, broker: &mut B, status: vike_model::FeedStatus) {
        let code = crate::abi::feed_status_code(status);
        self.dispatch("on_feed_status", broker, |h, r| (self.vt.on_feed_status)(h, r, code));
    }

    fn on_mark(&mut self, broker: &mut B, mark: &vike_model::TickMark) {
        let c = TickMarkAbi::of(mark);
        self.dispatch("on_mark", broker, |h, r| (self.vt.on_mark)(h, r, &c as *const TickMarkAbi));
    }

    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &vike_marketdata::TickQuote) {
        self.dispatch("on_reference_quote", broker, |h, r| {
            (self.vt.on_reference_quote)(
                h,
                r,
                venue.as_ptr(),
                venue.len(),
                LentTickQuoteAbi::lend(q),
            )
        });
    }

    fn on_flow(&mut self, broker: &mut B, flow: vike_model::FlowToxicity) {
        let c = FlowToxicityAbi::of(flow);
        self.dispatch("on_flow", broker, |h, r| {
            (self.vt.on_flow)(h, r, &c as *const FlowToxicityAbi)
        });
    }

    fn on_order_event(&mut self, broker: &mut B, event: &vike_model::OrderEvent) {
        let c = OrderEventAbi::of(event);
        self.dispatch("on_order_event", broker, |h, r| {
            (self.vt.on_order_event)(h, r, &c as *const OrderEventAbi)
        });
    }

    fn on_params_updated(&mut self, broker: &mut B, params: &vike_model::StrategyParams) {
        // ⚠ A serialisation failure must NOT be silently skipped — that is exactly the
        // hook-arrived-empty shape this whole widening exists to end. `serde_json` cannot fail on
        // these types (no map with non-string keys, no NaN outside `f64`'s own allowance), but
        // "cannot fail" is a claim about today's `StrategyParams`, so the impossible branch
        // reports itself rather than returning.
        let json = match serde_json::to_string(params) {
            Ok(j) => j,
            Err(e) => {
                eprintln!(
                    "vike-rust-strategy: could not serialise StrategyParams for this strategy file's \
                     on_params_updated: {e}. The update was NOT delivered — the strategy is still \
                     running its previous tunables. This is a defect in the params type, not in \
                     the operator's input."
                );
                return;
            }
        };
        self.dispatch("on_params_updated", broker, |h, r| {
            (self.vt.on_params_updated)(h, r, json.as_ptr(), json.len())
        });
    }

    /// The strategy's live params, read off the cold path. A failure panics (`StrategyFileHost::lent`),
    /// and so does a document that is not a `StrategyParams`, never read as "publishes none".
    fn params(&self) -> Option<vike_model::StrategyParams> {
        let text = self.lent("params", self.vt.params)?.get()?.read();
        Some(crate::guest::strategy_params_from_json(&text).unwrap_or_else(|| {
            panic!(
                "strategy file ABI contract violated: the strategy file's `params` are not a \
                 StrategyParams document: {text}"
            )
        }))
    }

    fn on_stop(&mut self, broker: &mut B) {
        self.dispatch("on_stop", broker, |h, r| (self.vt.on_stop)(h, r));
    }

    /// The state the strategy saves, as `params` is read (a failure panics, never "saves nothing"): the
    /// bytes the strategy file encoded (`ABI_VERSION` 15), copied once and never decoded here, so a
    /// sandboxed live mount sends its tradehub, and the sidecar keeps, exactly what the strategy wrote.
    fn save_state(&self) -> Option<vike_model::StrategyState> {
        let lent = self.lent("save_state", self.vt.save_state)?.get()?;
        Some(vike_model::StrategyState::from_bytes(crate::abi::lent_bytes(lent).to_vec()))
    }

    /// The state crosses as its bytes. A null handle or a status other than `Ok` is reported as a
    /// skipped dispatch is (the strategy then runs from the state it was built with).
    fn load_state(&mut self, state: &vike_model::StrategyState) {
        let status = if self.handle.is_null() {
            StrategyFileStatusAbi::BadHandle
        } else {
            let bytes = state.bytes();
            (self.vt.load_state)(self.handle, bytes.as_ptr(), bytes.len())
        };
        // A restore runs before any event: there is no strategy clock to read.
        self.report("load_state", status, || None);
    }

    /// Counted on this side of the boundary, never asked of the file ([`UNWIRED_HOOKS`]): every
    /// dispatch that came back other than `Ok`.
    fn failures(&self) -> StrategyFailures {
        self.failed.borrow().clone()
    }
}

/// The `Strategy` seam methods [`StrategyVTableAbi`] actually carries, so a strategy file genuinely receives
/// them.
///
/// ⚠ **Adding a field to [`StrategyVTableAbi`] means adding its name here.** Forgetting leaves
/// `build_strategy_file` refusing a hook that now works — loud and immediate, which is the correct
/// direction for this list to rot in.
pub const WIRED_HOOKS: &[&str] = &[
    "warmup",
    "inputs",
    "symbols",
    "stated_fee",
    "stated_slippage",
    "on_start",
    "on_bar",
    "on_quote_tick",
    "on_trade_tick",
    "on_order_book",
    "on_schedule",
    "on_fill",
    "on_feed_status",
    "on_mark",
    "on_reference_quote",
    "on_flow",
    "on_order_event",
    "on_params_updated",
    "params",
    "on_stop",
    "save_state",
    "load_state",
];

/// Every OTHER `vike_model::Strategy` method: declared by the trait, overridable by a user
/// strategy, and NOT carried by [`StrategyVTableAbi`] — so an override of one is dead code under the
/// strategy file mechanism and live code when the file is compiled in-process.
///
/// `vike_strategy_builder::render::build_strategy_file` refuses a source that overrides one of these,
/// which is the only signal that exists.
///
/// ⚠ **Held complete against the real trait** by
/// `crates/vike-rust-strategy/tests/hook_roster.rs`, which reads
/// `crates/vike-model/src/strategy/mod.rs` and fails if the trait grows a method neither list
/// names — otherwise a new hook would join the silent set the day it was declared, which is the
/// failure this whole pair exists to end.
///
/// ⚠ **`failures` alone, and on purpose: it is the HOST's answer, never the file's.** The list was
/// sixteen, went to three at 3 (`params`, `save_state`, `load_state`: two RETURN owned data, which
/// seemed to need an ownership contract for text freed across the `.so` boundary, and the third is
/// their pair), gained `inputs` at decision 0271 and lost it at 6, and gained `symbols` with live
/// multi-symbol mounts (step 12), refused while the order slots could not name a leg. At 8 all four
/// were wired the way `inputs` was at 6, and the list stood empty, kept as the place a FUTURE hook
/// goes until its export exists. `failures` (what a strategy's hooks failed with, a backtest
/// report's) is counted by [`StrategyFileHost`] itself over the statuses its dispatches return, so no
/// export carries it and a file's own override would only lie: the builder refuses one.
pub const UNWIRED_HOOKS: &[&str] = &["failures"];

#[path = "host_tests.rs"]
#[cfg(test)]
mod host_tests;
