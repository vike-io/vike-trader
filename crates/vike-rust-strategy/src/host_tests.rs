use super::*;
use vike_model::{OrderBroker, OrderPlacement};

/// The `OrderBroker` reads no test in this module observes, answered with a neutral constant. Each
/// of the three stubs below (`FakeBroker`, `PanickingBroker`, `OtherBroker`) spells only the
/// methods it exists to differ on — `place` and `position` — and takes the rest from here.
macro_rules! inert_broker_reads {
    () => {
        fn price(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _symbol: &str) -> &[Bar] {
            &[]
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    };
}

/// The `OrderBroker` verbs but `place` (`broker_vtable` binds the trait), inert in the two stubs
/// that exist for something else.
macro_rules! inert_order_verbs {
    () => {
        fn place_group(&mut self, _: &str, _legs: Vec<(String, vike_model::OrderListLeg)>) {}
        fn amend(&mut self, _: &str, _: Option<f64>, _: Option<f64>, _: Option<f64>) {}
        fn cancel(&mut self, _tag: &str) {}
        fn deny(&mut self, _: &str, _tag: &str, _quantity: f64, _reason: &str) {}
        fn average_price(&self, _: &str) -> f64 {
            0.0
        }
    };
}

#[derive(Default)]
struct FakeBroker {
    pos: f64,
    /// Every untagged market order, as `(symbol, side, qty)`.
    submits: Vec<(String, i32, f64)>,
    /// Every `OrderBroker` verb the host received, spelled `verb(args)` with `{:?}` floats, in order.
    orders: Vec<String>,
    /// The closed bars of the input [`FAKE_INPUT`]; every other name has none.
    input: Vec<Bar>,
    /// The whole series of [`FAKE_INPUT`] (`OrderBroker::input_whole`), every other name an empty one;
    /// `None`: no whole series served.
    whole: Option<Vec<Bar>>,
    /// What `OrderBroker::closed_trades` answers.
    ledger: Option<Vec<vike_model::TradeClosedRow>>,
    /// What `OrderBroker::symbols` answers, the run's universe.
    universe: Vec<String>,
}

/// The one input [`FakeBroker`] answers for.
const FAKE_INPUT: &str = "bybit:ETHUSDT:1m";

/// Records every verb, the four with a trait default included: a guest that fell back to the
/// default would arrive here as `place`, never as its own name. The symbol is recorded as received
/// (`{:?}`), so a guest's named symbol is seen to cross as itself (`ABI_VERSION` 8).
impl vike_model::OrderBroker for FakeBroker {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        o: vike_model::OrderPlacement,
        ro: bool,
    ) {
        if tag.is_empty() && o == OrderPlacement::Market {
            self.submits.push((symbol.to_string(), side, qty));
        }
        self.orders.push(format!("place({symbol:?}, {tag}, {side}, {qty:?}, {o:?}, {ro})"));
    }
    fn place_exit_leg(
        &mut self,
        symbol: &str,
        exit: &str,
        tag: &str,
        side: i32,
        qty: f64,
        o: vike_model::OrderPlacement,
    ) {
        let row = format!("place_exit_leg({symbol:?}, {exit}, {tag}, {side}, {qty:?}, {o:?})");
        self.orders.push(row);
    }
    fn place_at_close(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, ro: bool) {
        self.orders.push(format!("place_at_close({symbol:?}, {tag}, {side}, {qty:?}, {ro})"));
    }
    fn place_close_from_fill(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, level: f64) {
        let row = format!("place_close_from_fill({symbol:?}, {tag}, {side}, {qty:?}, {level:?})");
        self.orders.push(row);
    }
    fn rests_reduce_only_while_flat(&self) -> bool {
        true
    }
    fn place_group(&mut self, symbol: &str, legs: Vec<(String, vike_model::OrderListLeg)>) {
        self.orders.push(format!("place_group({symbol:?}, {legs:?})"));
    }
    fn amend(&mut self, tag: &str, q: Option<f64>, p: Option<f64>, t: Option<f64>) {
        self.orders.push(format!("amend({tag}, {q:?}, {p:?}, {t:?})"));
    }
    fn cancel(&mut self, tag: &str) {
        self.orders.push(format!("cancel({tag})"));
    }
    fn deny(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str) {
        self.orders.push(format!("deny({symbol:?}, {tag}, {qty:?}, {reason})"));
    }
    fn publish_forecast(&mut self, symbol: &str, f: vike_model::Forecast) {
        let (v, h, c) = (f.value(), f.horizon_ms(), f.confidence());
        self.orders.push(format!("publish_forecast({symbol:?}, {v:?}, {h}, {c:?})"));
    }
    /// The symbol's own figure, so the symbol is seen to cross.
    fn average_price(&self, symbol: &str) -> f64 {
        if symbol == "ETHUSDT" { 2_501.5 } else { 101.5 }
    }
    fn symbols(&self) -> &[String] {
        &self.universe
    }
    fn position(&self, _symbol: &str) -> f64 {
        self.pos
    }
    /// A grid for `BTCUSDT` alone, so both `Some` and `None` cross.
    fn instrument_grid(&self, symbol: &str) -> Option<vike_model::Grid> {
        (symbol == "BTCUSDT").then_some(FAKE_GRID)
    }
    fn input_bars(&self, name: &str) -> &[Bar] {
        if name == FAKE_INPUT { &self.input } else { &[] }
    }
    fn input_whole(&self, name: &str) -> Option<&[Bar]> {
        let whole = self.whole.as_deref()?;
        Some(if name == FAKE_INPUT { whole } else { &[] })
    }
    fn closed_trades(&self) -> Option<&[vike_model::TradeClosedRow]> {
        self.ledger.as_deref()
    }
    inert_broker_reads!();
}

const FAKE_GRID: vike_model::Grid = vike_model::Grid {
    tick_size: Some(0.05),
    lot_size: None,
    min_notional: Some(5.0),
    min_qty: Some(0.001),
};

/// A non-null, never-dereferenced sentinel handle for fakes that carry no real state — using
/// a genuine null here (as an earlier version of this test module did) would now trip the
/// `StrategyFileStatusAbi::BadHandle` guard `StrategyFileHost` checks before every dispatch, since that
/// guard cannot distinguish "no state needed" from "creation failed" any more than the real
/// ABI can (see `StrategyFileHost::new`'s doc).
fn dummy_handle() -> *mut c_void {
    std::ptr::without_provenance_mut(1)
}

// A hand-built fake strategy file: no `.so`, no dlopen. `fake_on_bar` wraps its received `BrokerRef`
// as a `guest::HostBroker` and submits a market order sized off the bar's close — proving
// guest.rs and host.rs interoperate correctly with each other, not just in isolation.
extern "C" fn fake_create(
    _params_ptr: *const u8,
    _params_len: usize,
    _facts_ptr: *const u8,
    _facts_len: usize,
) -> *mut c_void {
    dummy_handle()
}
extern "C" fn fake_destroy(_handle: *mut c_void) {}
extern "C" fn fake_warmup(_handle: *mut c_void) -> usize {
    2
}
extern "C" fn fake_on_bar(
    _handle: *mut c_void,
    r: BrokerRef,
    bar: *const LentBarAbi,
) -> StrategyFileStatusAbi {
    // SAFETY: `bar` is the host's live, aligned `*const LentBarAbi` `StrategyFileHost::on_bar` lent for
    // exactly this call.
    let close = unsafe { (*bar).close };
    let mut broker = crate::guest::HostBroker::new(r);
    OrderBroker::place(&mut broker, "BTCUSDT", "", 1, close, OrderPlacement::Market, false);
    StrategyFileStatusAbi::Ok
}
/// One `extern "C"` dispatch slot past the `(handle, BrokerRef)` pair every slot opens with: its
/// remaining parameter types (each slot's own — that difference is the ABI) and what it answers.
macro_rules! slot {
    ($name:ident($($ty:ty),*) => $answer:expr) => {
        extern "C" fn $name(_h: *mut c_void, _r: BrokerRef, $(_: $ty),*) -> StrategyFileStatusAbi {
            $answer
        }
    };
}

slot!(fake_on_bar_panicking(*const LentBarAbi) => StrategyFileStatusAbi::Panicked);

/// A strategy file declaring no input: the empty JSON list, lent from a static. It serves `symbols` too.
extern "C" fn stub_inputs(_h: *mut c_void) -> OptionAbi<StrAbi> {
    OptionAbi::new(Some(StrAbi::of("[]")), StrAbi::empty())
}

/// The inner `None` a strategy publishing no params, or saving no state, answers.
fn answered_none() -> OptionAbi<OptionAbi<StrAbi>> {
    OptionAbi::new(
        Some(OptionAbi::new(None, StrAbi::empty())),
        OptionAbi::new(None, StrAbi::empty()),
    )
}
extern "C" fn stub_answer(_h: *mut c_void) -> OptionAbi<OptionAbi<StrAbi>> {
    answered_none()
}
/// A strategy saving no state (`ABI_VERSION` 15: a state is lent as its bytes).
extern "C" fn stub_no_state(_h: *mut c_void) -> OptionAbi<OptionAbi<crate::abi::SliceAbi<u8>>> {
    let none = OptionAbi::new(None, crate::abi::SliceAbi::empty());
    OptionAbi::new(Some(none), OptionAbi::new(None, crate::abi::SliceAbi::empty()))
}
/// A file with no `refuse` export: answered, and no refusal.
extern "C" fn stub_refuse(_p: *const u8, _n: usize) -> OptionAbi<OptionAbi<StrAbi>> {
    answered_none()
}
extern "C" fn stub_load_state(_h: *mut c_void, _p: *const u8, _n: usize) -> StrategyFileStatusAbi {
    StrategyFileStatusAbi::Ok
}

// ---- the thirteen ABI_VERSION 3 dispatch slots, as inert stubs -----------------------------
//
// Only the slots a given test drives are overridden on the returned vtable; everything else
// answers `Ok` and does nothing. A stub per slot rather than one shared function because the
// signatures genuinely differ — that difference is the ABI.

slot!(stub_on_start() => StrategyFileStatusAbi::Ok);
slot!(stub_on_stop() => StrategyFileStatusAbi::Ok);
slot!(stub_on_quote_tick(*const LentTickQuoteAbi) => StrategyFileStatusAbi::Ok);
slot!(stub_on_trade_tick(*const LentTickTradeAbi) => StrategyFileStatusAbi::Ok);
slot!(stub_on_order_book(BookRef) => StrategyFileStatusAbi::Ok);
// Both `(bytes, len)` hooks of the vtable: `on_schedule` (its tag) and `on_params_updated` (its JSON).
slot!(stub_on_bytes(*const u8, usize) => StrategyFileStatusAbi::Ok);
slot!(stub_on_fill(*const FillAbi) => StrategyFileStatusAbi::Ok);
slot!(stub_on_feed_status(u32) => StrategyFileStatusAbi::Ok);
slot!(stub_on_mark(*const TickMarkAbi) => StrategyFileStatusAbi::Ok);
slot!(stub_on_reference_quote(*const u8, usize, *const LentTickQuoteAbi) => StrategyFileStatusAbi::Ok);
slot!(stub_on_flow(*const FlowToxicityAbi) => StrategyFileStatusAbi::Ok);
slot!(stub_on_order_event(*const OrderEventAbi) => StrategyFileStatusAbi::Ok);

fn fake_vtable() -> StrategyVTableAbi {
    StrategyVTableAbi {
        create: fake_create,
        destroy: fake_destroy,
        warmup: fake_warmup,
        inputs: stub_inputs,
        symbols: stub_inputs,
        stated_fee: stub_answer,
        stated_slippage: stub_answer,
        on_start: stub_on_start,
        on_bar: fake_on_bar,
        on_quote_tick: stub_on_quote_tick,
        on_trade_tick: stub_on_trade_tick,
        on_order_book: stub_on_order_book,
        on_schedule: stub_on_bytes,
        on_fill: stub_on_fill,
        on_feed_status: stub_on_feed_status,
        on_mark: stub_on_mark,
        on_reference_quote: stub_on_reference_quote,
        on_flow: stub_on_flow,
        on_order_event: stub_on_order_event,
        on_params_updated: stub_on_bytes,
        params: stub_answer,
        on_stop: stub_on_stop,
        save_state: stub_no_state,
        load_state: stub_load_state,
        refuse: stub_refuse,
        echo: stub_answer,
    }
}

fn bar(close: f64) -> Bar {
    vike_marketdata::test_support::flat_bar_zero_volume(1, close)
}

/// `ABI_VERSION` 14: `on_bar`'s payload is the HOST's bar itself, lent behind a pointer for the call
/// (`Bar` is `repr(C)` under `abi::layout`), so the file reads every field, the optionals and the
/// symbol included, where the host holds them: nothing is copied.
#[test]
fn the_on_bar_payload_is_the_hosts_bar() {
    thread_local! {
        static SEEN: std::cell::Cell<*const LentBarAbi> = const { std::cell::Cell::new(std::ptr::null()) };
    }
    extern "C" fn records(
        _h: *mut c_void,
        _r: BrokerRef,
        bar: *const LentBarAbi,
    ) -> StrategyFileStatusAbi {
        SEEN.with(|seen| seen.set(bar));
        StrategyFileStatusAbi::Ok
    }
    let mut vt = fake_vtable();
    vt.on_bar = records;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(vt, "", "{}"));
    let b = Bar { funding: Some(0.01), ask: Some(1.6), symbol: Some("BTCUSDT".into()), ..bar(1.5) };
    strategy.on_bar(&mut FakeBroker::default(), &b);
    assert_eq!(SEEN.with(std::cell::Cell::get), LentBarAbi::lend(&b), "the host's bar, not a copy");
}

#[test]
fn warmup_crosses_the_vtable() {
    let strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(fake_vtable(), "", "{}"));
    assert_eq!(strategy.warmup(), 2);
}

#[test]
fn on_bar_reaches_the_real_broker_through_both_wrappers() {
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(fake_vtable(), "", "{}"));
    let mut broker = FakeBroker::default();
    strategy.on_bar(&mut broker, &bar(42.0));
    assert_eq!(broker.submits, vec![("BTCUSDT".to_string(), 1, 42.0)]);
}

/// A strategy file that reports `Panicked` must not abort the process or unwind out of `on_bar` — it
/// is a reported, contained failure. `tests/load_refusals.rs` (Task 4) proves the stronger
/// claim (a REAL panic inside a compiled `.so`, caught by ITS OWN catch_unwind); this proves
/// the host's side of the contract: receiving `Panicked` never panics the caller.
#[test]
fn a_panicked_status_is_reported_and_does_not_panic_the_caller() {
    let mut vt = fake_vtable();
    vt.on_bar = fake_on_bar_panicking;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(vt, "", "{}"));
    let mut broker = FakeBroker::default();
    strategy.on_bar(&mut broker, &bar(1.0));
    assert!(broker.submits.is_empty(), "a panicked dispatch must not have reached the broker");
}

/// What the host counted is the strategy's `failures`, read through the `Box<dyn Strategy>` a run holds:
/// every dispatch that came back `Panicked`, the broker's clock at the first, and one message for one
/// hook and status. A file whose hooks answer `Ok` answers none.
#[test]
fn a_panicking_on_bar_is_counted_as_the_strategys_failures() {
    let mut vt = fake_vtable();
    vt.on_bar = fake_on_bar_panicking;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(vt, "", "{}"));
    let mut broker = FakeBroker::default();
    for close in [1.0, 2.0, 3.0] {
        strategy.on_bar(&mut broker, &bar(close));
    }
    let expected = StrategyFailures {
        count: 3,
        first_at: Some(0),
        messages: vec!["on_bar panicked (the file's own message is on stderr)".to_string()],
        disabled: false,
    };
    assert_eq!(strategy.failures(), expected);

    let mut clean: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(fake_vtable(), "", "{}"));
    clean.on_bar(&mut broker, &bar(1.0));
    assert!(clean.failures().is_empty(), "{:?}", clean.failures());
}

/// Important-4: a null `create()` handle must be reported as `StrategyFileStatusAbi::BadHandle` and
/// must NEVER be handed to `warmup`/`on_bar` — both strategy file functions below panic if called at
/// all, so this test fails LOUDLY if the guard regresses. ⚠ Not "a real panic, not a swallowed
/// one" (an earlier version of this comment said that, and a round-2 review caught it): both
/// vtable slots are `extern "C" fn` pointers, and the abort-on-unwind-across-`extern "C"`
/// behavior is baked into a function's OWN compiled body by its declared ABI, regardless of
/// whether the call crosses a real `dlopen` boundary or stays in-process — so a regression
/// here would ABORT the test process at the `must_not_be_called_*` call site, not raise a
/// catchable Rust panic `#[test]`'s own harness could report as a normal failure. Either way
/// the guard's absence is unmistakable; it just would not look like an ordinary red test.
#[test]
fn a_null_handle_is_never_handed_to_the_strategy_file() {
    extern "C" fn create_returns_null(
        _p: *const u8,
        _n: usize,
        _f: *const u8,
        _m: usize,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }
    extern "C" fn must_not_be_called_warmup(_h: *mut c_void) -> usize {
        panic!("warmup must never be called with a null handle");
    }
    slot!(must_not_be_called_on_bar(*const LentBarAbi) => {
        panic!("on_bar must never be called with a null handle")
    });
    let mut vt = fake_vtable();
    vt.create = create_returns_null;
    vt.warmup = must_not_be_called_warmup;
    vt.on_bar = must_not_be_called_on_bar;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(vt, "", "{}"));
    let mut broker = FakeBroker::default();

    assert_eq!(strategy.warmup(), 0, "a null handle must default warmup to 0");
    strategy.on_bar(&mut broker, &bar(1.0)); // must not panic and must not reach the broker
    assert!(broker.submits.is_empty());
    // ⚠ This test proves the `warmup`/`on_bar` guards, and ONLY those — `Drop`'s OWN
    // null-handle skip (`if !self.handle.is_null() { (self.vt.destroy)(self.handle); }`) is
    // NOT exercised here: `fake_destroy` is a no-op, so this test would pass identically
    // whether or not that skip existed. Proving it would need the same `must_not_be_called_*`
    // trap on `destroy`, which this test does not set up.
}

/// Critical-1: a panic inside the HOST's own `OrderBroker` impl — reached from a `t_*` thunk the
/// STRATEGY FILE calls into during its `on_bar` — must not abort the process, and must surface as a
/// genuine (catchable, on THIS side) panic from `StrategyFileHost::on_bar` rather than silently
/// producing a wrong number. `catch_unwind` here is what proves "did not abort": an escaped
/// unwind across the `extern "C"` thunk beneath this call would already have aborted the
/// process before this line could ever run.
#[test]
fn a_host_side_panic_inside_a_thunk_poisons_the_dispatch_and_is_re_raised() {
    struct PanickingBroker;
    impl vike_model::OrderBroker for PanickingBroker {
        fn place(&mut self, _: &str, _: &str, _: i32, _: f64, _: OrderPlacement, _: bool) {}
        inert_order_verbs!();
        fn position(&self, _s: &str) -> f64 {
            panic!("deliberate host-side panic for the Critical-1 mutation proof")
        }
        inert_broker_reads!();
    }

    // A fake strategy file whose on_bar calls the ONE thing that panics: OrderBroker::position via the
    // guest's own HostBroker wrapper (crossing the same real BrokerVTableAbi path production
    // code uses, not calling the thunk directly).
    extern "C" fn on_bar_calls_position(
        _h: *mut c_void,
        r: BrokerRef,
        _bar: *const LentBarAbi,
    ) -> StrategyFileStatusAbi {
        let broker = crate::guest::HostBroker::new(r);
        let _ = OrderBroker::position(&broker, "BTCUSDT"); // reaches PanickingBroker::position
        StrategyFileStatusAbi::Ok
    }
    let mut vt = fake_vtable();
    vt.on_bar = on_bar_calls_position;
    let mut strategy: Box<dyn Strategy<PanickingBroker>> =
        Box::new(StrategyFileHost::<PanickingBroker>::new(vt, "", "{}"));
    let mut broker = PanickingBroker;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        strategy.on_bar(&mut broker, &bar(1.0));
    }));
    assert!(result.is_err(), "the host-side panic must be re-raised from on_bar, not swallowed");
}

/// Two different concrete brokers used in the SAME process must each get thunks that
/// downcast `ctx` to THEIR OWN type — pinning the exact hazard `broker_vtable`'s doc warns
/// about (a naive function-local `static` would hand every `B` the first caller's thunks).
#[test]
fn broker_vtable_is_correct_per_distinct_broker_type() {
    #[derive(Default)]
    struct OtherBroker {
        submits: Vec<(String, i32, f64)>,
    }
    impl vike_model::OrderBroker for OtherBroker {
        fn place(
            &mut self,
            symbol: &str,
            _: &str,
            side: i32,
            qty: f64,
            _: OrderPlacement,
            _: bool,
        ) {
            self.submits.push((symbol.to_string(), side, qty));
        }
        inert_order_verbs!();
        fn position(&self, _s: &str) -> f64 {
            0.0
        }
        inert_broker_reads!();
    }

    // Force both instantiations to exist in the same binary, in either order.
    let vt_fake = broker_vtable::<FakeBroker>();
    let vt_other = broker_vtable::<OtherBroker>();
    let market = crate::abi::OrderPlacementAbi::of(OrderPlacement::Market);

    let mut a = FakeBroker::default();
    let ctx = (&mut a as *mut FakeBroker).cast();
    (vt_fake.place)(ctx, b"X".as_ptr(), 1, b"".as_ptr(), 0, 1, 1.0, market, false);
    assert_eq!(a.submits, vec![("X".to_string(), 1, 1.0)]);

    let mut o = OtherBroker::default();
    let ctx = (&mut o as *mut OtherBroker).cast();
    (vt_other.place)(ctx, b"Y".as_ptr(), 1, b"".as_ptr(), 0, -1, 2.0, market, false);
    assert_eq!(o.submits, vec![("Y".to_string(), -1, 2.0)]);
}

// ---- ABI_VERSION 3: every wired hook actually reaches the strategy file --------------------------

thread_local! {
    /// Which vtable slot each recording stub below was entered through, in order.
    static REACHED: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn note(hook: &'static str) -> StrategyFileStatusAbi {
    REACHED.with(|v| v.borrow_mut().push(hook));
    StrategyFileStatusAbi::Ok
}

/// **The structural witness for this whole widening.** `StrategyFileHost` implements
/// `Strategy<B>`, and every hook it does NOT override falls through to the trait's own no-op
/// default — which compiles, runs, and silently delivers nothing. That is exactly the failure
/// the unwired-hook refusal was invented for, and adding a `StrategyVTableAbi` FIELD does not
/// prevent it: a field can be declared, bound by the loader, exported by the template, and
/// still never called, because the one line that calls it lives in the trait impl.
///
/// So this drives all fifteen through the `Strategy` trait — the surface the backtest engine
/// uses — and asserts each one arrived at its own slot, by name. Deleting any single
/// `fn on_*` from the impl above makes it fail naming that hook.
#[test]
fn every_wired_hook_reaches_the_strategy_file_through_the_strategy_trait() {
    slot!(r_on_start() => note("on_start"));
    slot!(r_on_stop() => note("on_stop"));
    slot!(r_on_bar(*const LentBarAbi) => note("on_bar"));
    slot!(r_on_quote_tick(*const LentTickQuoteAbi) => note("on_quote_tick"));
    slot!(r_on_trade_tick(*const LentTickTradeAbi) => note("on_trade_tick"));
    slot!(r_on_order_book(BookRef) => note("on_order_book"));
    slot!(r_on_schedule(*const u8, usize) => note("on_schedule"));
    slot!(r_on_fill(*const FillAbi) => note("on_fill"));
    slot!(r_on_feed_status(u32) => note("on_feed_status"));
    slot!(r_on_mark(*const TickMarkAbi) => note("on_mark"));
    slot!(r_on_reference_quote(*const u8, usize, *const LentTickQuoteAbi) => note("on_reference_quote"));
    slot!(r_on_flow(*const FlowToxicityAbi) => note("on_flow"));
    slot!(r_on_order_event(*const OrderEventAbi) => note("on_order_event"));
    slot!(r_on_params_updated(*const u8, usize) => note("on_params_updated"));
    extern "C" fn r_warmup(_h: *mut c_void) -> usize {
        REACHED.with(|v| v.borrow_mut().push("warmup"));
        7
    }
    extern "C" fn r_inputs(_h: *mut c_void) -> OptionAbi<StrAbi> {
        REACHED.with(|v| v.borrow_mut().push("inputs"));
        OptionAbi::new(Some(StrAbi::of(r#"["1d"]"#)), StrAbi::empty())
    }
    extern "C" fn r_symbols(_h: *mut c_void) -> OptionAbi<StrAbi> {
        REACHED.with(|v| v.borrow_mut().push("symbols"));
        OptionAbi::new(Some(StrAbi::of(r#"["ETHUSDT"]"#)), StrAbi::empty())
    }
    extern "C" fn r_stated_fee(_h: *mut c_void) -> OptionAbi<OptionAbi<StrAbi>> {
        REACHED.with(|v| v.borrow_mut().push("stated_fee"));
        let fee = r#"{"PerShareWithFloor":{"per_share":0.05,"min":0.0,"max_percent":0.0}}"#;
        let stated = OptionAbi::new(Some(StrAbi::of(fee)), StrAbi::empty());
        OptionAbi::new(Some(stated), OptionAbi::new(None, StrAbi::empty()))
    }
    extern "C" fn r_stated_slippage(_h: *mut c_void) -> OptionAbi<OptionAbi<StrAbi>> {
        REACHED.with(|v| v.borrow_mut().push("stated_slippage"));
        let table = r#"{"model":"ticks","ticks":2.0,"tick":0.25}"#;
        let stated = OptionAbi::new(Some(StrAbi::of(table)), StrAbi::empty());
        OptionAbi::new(Some(stated), OptionAbi::new(None, StrAbi::empty()))
    }
    extern "C" fn r_params(_h: *mut c_void) -> OptionAbi<OptionAbi<StrAbi>> {
        REACHED.with(|v| v.borrow_mut().push("params"));
        answered_none()
    }
    extern "C" fn r_save_state(h: *mut c_void) -> OptionAbi<OptionAbi<crate::abi::SliceAbi<u8>>> {
        REACHED.with(|v| v.borrow_mut().push("save_state"));
        stub_no_state(h)
    }
    extern "C" fn r_load_state(_h: *mut c_void, _p: *const u8, _n: usize) -> StrategyFileStatusAbi {
        note("load_state")
    }

    REACHED.with(|v| v.borrow_mut().clear());
    let vt = StrategyVTableAbi {
        create: fake_create,
        destroy: fake_destroy,
        warmup: r_warmup,
        inputs: r_inputs,
        symbols: r_symbols,
        stated_fee: r_stated_fee,
        stated_slippage: r_stated_slippage,
        params: r_params,
        save_state: r_save_state,
        load_state: r_load_state,
        on_start: r_on_start,
        on_bar: r_on_bar,
        on_quote_tick: r_on_quote_tick,
        on_trade_tick: r_on_trade_tick,
        on_order_book: r_on_order_book,
        on_schedule: r_on_schedule,
        on_fill: r_on_fill,
        on_feed_status: r_on_feed_status,
        on_mark: r_on_mark,
        on_reference_quote: r_on_reference_quote,
        on_flow: r_on_flow,
        on_order_event: r_on_order_event,
        on_params_updated: r_on_params_updated,
        on_stop: r_on_stop,
        refuse: stub_refuse,
        echo: stub_answer,
    };
    let mut s: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(vt, "", "{}"));
    let mut b = FakeBroker::default();

    assert_eq!(s.warmup(), 7);
    assert_eq!(s.inputs(), ["1d"]);
    assert_eq!(s.symbols(), ["ETHUSDT"]);
    let per_contract =
        vike_model::FeeSchedule::PerShareWithFloor { per_share: 0.05, min: 0.0, max_percent: 0.0 };
    assert_eq!(s.stated_fee(), Some(per_contract), "the stated fee crosses as its JSON");
    let slippage = s.stated_slippage().expect("the stated slippage crosses as its JSON");
    assert_eq!(
        (slippage.model.as_str(), slippage.ticks, slippage.tick),
        ("ticks", Some(2.0), Some(0.25))
    );
    assert_eq!(s.params(), None);
    assert_eq!(s.save_state(), None);
    s.load_state(&vike_model::StrategyState::from_json_value(&serde_json::json!({"fills": 1})));
    s.on_start(&mut b);
    s.on_bar(&mut b, &bar(1.0));
    s.on_quote_tick(&mut b, &quote_tick());
    s.on_trade_tick(&mut b, &trade_tick());
    s.on_order_book(&mut b, &seeded_book());
    s.on_schedule(&mut b, "rebalance");
    s.on_fill(&mut b, &a_fill());
    s.on_feed_status(&mut b, vike_model::FeedStatus::Stale);
    s.on_mark(&mut b, &a_mark());
    s.on_reference_quote(&mut b, "binance", &quote_tick());
    s.on_flow(&mut b, vike_model::FlowToxicity { bid: 0.1, ask: 0.9, timestamp: 5 });
    s.on_order_event(&mut b, &a_lifecycle());
    s.on_params_updated(&mut b, &some_params());
    s.on_stop(&mut b);

    let reached = REACHED.with(|v| v.borrow().clone());
    let mut missing: Vec<&&str> =
        WIRED_HOOKS.iter().filter(|h| !reached.contains(&(**h))).collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "these WIRED hooks never reached the strategy file — `StrategyFileHost`'s `Strategy` impl is \
             still falling through to the trait's own no-op default for them, which is the silent \
             divergence the whole vtable exists to end: {missing:?}\nreached: {reached:?}"
    );
    assert_eq!(
        reached.len(),
        WIRED_HOOKS.len(),
        "one dispatch must reach exactly one slot: {reached:?}"
    );
}

// ---- shared payloads for the two tests above/below ----

fn quote_tick() -> vike_marketdata::TickQuote {
    vike_marketdata::TickQuote {
        timestamp: 10,
        local_timestamp: 11,
        bid: 99.0,
        ask: 101.0,
        bid_size: 1.5,
        ask_size: 2.5,
        symbol: "BTCUSDT".into(),
    }
}
fn trade_tick() -> vike_marketdata::TickTrade {
    vike_marketdata::TickTrade {
        timestamp: 12,
        local_timestamp: 13,
        price: 100.0,
        size: 0.25,
        is_buyer_maker: false,
        symbol: "BTCUSDT".into(),
    }
}
fn a_fill() -> vike_model::Fill {
    vike_model::Fill {
        side: 1,
        size: 2.0,
        price: 100.5,
        fee: 0.01,
        timestamp: 14,
        is_maker: false,
        symbol: "BTCUSDT".to_string(),
        tag: None,
    }
}
fn a_mark() -> vike_model::TickMark {
    vike_model::TickMark { symbol: "btcusdt".to_string(), price: 64_000.0, timestamp: 15 }
}
fn a_lifecycle() -> vike_model::OrderEvent {
    vike_model::OrderEvent {
        order_client_id: "order-client-id-1".to_string(),
        tag: Some("bid-1".to_string()),
        kind: vike_model::OrderEventKind::Rejected { reason: "min notional".to_string() },
    }
}
fn some_params() -> vike_model::StrategyParams {
    // The CONTROLLER variant, chosen because its `TripleBarrier` carries four `Option`
    // fields left at `None` — the exact shape a TOML hop could not serialise at all. A test
    // that used a fully-populated bag would pass under either encoding and prove nothing
    // about the choice `guest::strategy_params_from_json` argues for.
    vike_model::StrategyParams::PositionController(vike_model::PositionControllerParams::new(
        5_000,
        1.5,
        // Every leg left UNARMED — four `None`s, which is `TripleBarrier`'s documented
        // default and the ordinary state of a controller nobody has set a stop on.
        vike_model::TripleBarrier::default(),
        0.75,
    ))
}
fn seeded_book() -> vike_marketdata::L2Book {
    let mut book = vike_marketdata::L2Book::new(0.5);
    book.apply_snapshot(
        42,
        &[
            vike_marketdata::L2BookLevel::new(99.5, 3.0),
            vike_marketdata::L2BookLevel::new(99.0, 5.0),
            vike_marketdata::L2BookLevel::new(98.5, 7.0),
        ],
        &[
            vike_marketdata::L2BookLevel::new(100.0, 2.0),
            vike_marketdata::L2BookLevel::new(100.5, 4.0),
        ],
    );
    book
}

/// The book CURSOR, end to end through both wrappers: the host flattens its real `L2Book`, the
/// strategy file rebuilds one from the cursor, and every public read must agree.
///
/// ⚠ It compares the READS rather than the struct, and that is the stronger claim available:
/// `L2Book` has no `PartialEq` and its level maps are private, so what a user strategy can
/// actually observe IS this list of accessors. A reconstruction that happened to hold the same
/// bytes but answered `best_bid` differently would be useless; one that answers every accessor
/// identically is indistinguishable from the original to any strategy.
#[test]
fn the_book_cursor_rebuilds_a_book_the_strategy_file_cannot_tell_from_the_original() {
    thread_local! {
        static SEEN: std::cell::RefCell<Option<Vec<(String, String)>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Every public read of an `L2Book`, as `(name, debug-formatted value)` — a string so one
    /// vector can hold `Option<L2BookLevel>`, `Option<f64>`, `usize` and `u64` together, and so
    /// a failure prints what differed rather than a bare `false`.
    fn probe(b: &vike_marketdata::L2Book) -> Vec<(String, String)> {
        let mut out = vec![
            ("tick_size".to_string(), format!("{:?}", b.tick_size)),
            ("last_sequence".to_string(), format!("{:?}", b.last_sequence)),
            ("bid_levels".to_string(), format!("{:?}", b.bid_levels())),
            ("ask_levels".to_string(), format!("{:?}", b.ask_levels())),
            ("best_bid".to_string(), format!("{:?}", b.best_bid())),
            ("best_ask".to_string(), format!("{:?}", b.best_ask())),
            ("mid".to_string(), format!("{:?}", b.mid())),
            ("spread".to_string(), format!("{:?}", b.spread())),
            ("imbalance".to_string(), format!("{:?}", b.imbalance())),
            ("top_n(8)".to_string(), format!("{:?}", b.top_n(8))),
            ("vwap_buy_6".to_string(), format!("{:?}", b.average_price_for_quantity(1, 6.0))),
            ("vwap_sell_6".to_string(), format!("{:?}", b.average_price_for_quantity(-1, 6.0))),
            ("depth_buy_100.5".to_string(), format!("{:?}", b.quantity_for_price(1, 100.5))),
            ("depth_sell_99.0".to_string(), format!("{:?}", b.quantity_for_price(-1, 99.0))),
            ("sim_buy_3".to_string(), format!("{:?}", b.simulate_fill(1, 3.0))),
        ];
        for px in [98.5, 99.0, 99.5, 100.0, 100.5] {
            out.push((format!("bid_qty_at({px})"), format!("{:?}", b.bid_quantity_at(px))));
            out.push((format!("ask_qty_at({px})"), format!("{:?}", b.ask_quantity_at(px))));
        }
        out
    }

    extern "C" fn rebuild_and_record(
        _h: *mut c_void,
        _r: BrokerRef,
        b: BookRef,
    ) -> StrategyFileStatusAbi {
        let rebuilt = crate::guest::book_from_ref(b);
        SEEN.with(|s| *s.borrow_mut() = Some(probe(&rebuilt)));
        StrategyFileStatusAbi::Ok
    }

    SEEN.with(|s| *s.borrow_mut() = None);
    let mut vt = fake_vtable();
    vt.on_order_book = rebuild_and_record;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(vt, "", "{}"));
    let mut broker = FakeBroker::default();

    let original = seeded_book();
    strategy.on_order_book(&mut broker, &original);

    let rebuilt = SEEN
        .with(|s| s.borrow().clone())
        .expect("the strategy file's on_order_book must have been reached");
    let expected = probe(&original);
    let diffs: Vec<String> = expected
        .iter()
        .zip(&rebuilt)
        .filter(|((_, a), (_, b))| a != b)
        .map(|((name, a), (_, b))| format!("{name}: host {a} vs strategy file {b}"))
        .collect();
    assert!(
        diffs.is_empty(),
        "the rebuilt book answers differently from the host's own:\n  {}",
        diffs.join("\n  ")
    );
    assert_eq!(expected.len(), rebuilt.len(), "the probe lists must be the same shape");
}

/// An EMPTY book must cross as an empty book rather than as a panic or a fabricated level —
/// the boundary case `side_len == 0` on both sides, which is what a `GapStart` leaves behind
/// and therefore an ordinary state rather than an exotic one.
#[test]
fn an_empty_book_crosses_as_an_empty_book() {
    thread_local! {
        static LEVELS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((9, 9)) };
    }
    extern "C" fn count(_h: *mut c_void, _r: BrokerRef, b: BookRef) -> StrategyFileStatusAbi {
        let rebuilt = crate::guest::book_from_ref(b);
        LEVELS.with(|c| c.set((rebuilt.bid_levels(), rebuilt.ask_levels())));
        StrategyFileStatusAbi::Ok
    }
    let mut vt = fake_vtable();
    vt.on_order_book = count;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(vt, "", "{}"));
    let mut broker = FakeBroker::default();
    strategy.on_order_book(&mut broker, &vike_marketdata::L2Book::new(0.5));
    assert_eq!(LEVELS.with(std::cell::Cell::get), (0, 0));
}

/// The params bag must arrive DECODED, with its `None`s intact — the property that decides
/// JSON over TOML (`guest::strategy_params_from_json`'s own doc carries the measurement).
#[test]
fn a_params_bag_crosses_as_json_and_decodes_to_the_same_value() {
    thread_local! {
        static DECODED: std::cell::RefCell<Option<vike_model::StrategyParams>> =
            const { std::cell::RefCell::new(None) };
    }
    extern "C" fn decode(
        _h: *mut c_void,
        _r: BrokerRef,
        p: *const u8,
        n: usize,
    ) -> StrategyFileStatusAbi {
        // Read back through `StrAbi`, which is exactly what the cdylib template does — so
        // this stub exercises the production decode path rather than a second spelling of
        // it, and adds no `unsafe` site of its own (the one that matters lives in `abi.rs`,
        // once, for every borrowed string in this ABI).
        let text = crate::abi::StrAbi { ptr: p, len: n }.read();
        match crate::guest::strategy_params_from_json(&text) {
            Some(v) => {
                DECODED.with(|d| *d.borrow_mut() = Some(v));
                StrategyFileStatusAbi::Ok
            }
            None => StrategyFileStatusAbi::BadParams,
        }
    }
    let mut vt = fake_vtable();
    vt.on_params_updated = decode;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(StrategyFileHost::<FakeBroker>::new(vt, "", "{}"));
    let mut broker = FakeBroker::default();
    let sent = some_params();
    strategy.on_params_updated(&mut broker, &sent);
    assert_eq!(
        DECODED.with(|d| d.borrow().clone()),
        Some(sent),
        "the params bag must arrive byte-equal after the JSON hop, `None` fields included"
    );
}

// ---- ABI_VERSION 5: the order seam, guest to host ----------------------------------------------

/// Every `OrderBroker` verb and `OrderBroker::instrument_grid`, called on the GUEST's `HostBroker` over
/// the HOST's real [`broker_vtable`], reaches the host's broker as the SAME verb with the same
/// values: every `OrderPlacement` kind, each `Option` both ways, an order list through its lent rows, and
/// the four verbs with a trait default under their own names (a guest left on the default would
/// arrive here as `place`, and a strategy file backtest would fill differently from native).
#[test]
fn every_order_verb_crosses_from_the_guest_to_the_host_as_itself() {
    use vike_model::{
        ContingencyType, OrderBroker, OrderListLeg, OrderPlacement as T, OrderType, TimeInForce,
    };
    let kinds = [
        T::Market,
        T::Limit { price: 100.25 },
        T::Stop { trigger: 99.5 },
        T::StopLimit { trigger: 99.5, limit: 99.25 },
        T::TakeProfit { trigger: 101.0, limit: None },
        T::TakeProfit { trigger: 101.0, limit: Some(101.5) },
        T::Trailing { trail: 0.5, activation: None },
        T::Trailing { trail: 0.5, activation: Some(102.0) },
        T::Pegged {
            peg: vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Best, offset: 0.0 },
        },
        T::Iceberg { price: 100.25, display: 0.5 },
    ];
    let leg = |side, order_type, price, parent: Option<usize>, linked: Vec<usize>| OrderListLeg {
        side,
        qty: 2.5,
        order_type,
        price,
        trigger_price: None,
        trail: None,
        reduce_only: parent.is_some(),
        time_in_force: TimeInForce::Gtc,
        parent,
        contingency: (!linked.is_empty()).then_some(ContingencyType::Oco),
        linked,
        order_client_id: None,
    };
    let legs = vec![
        ("long#1".to_string(), leg(1, OrderType::Market, None, None, vec![])),
        ("long#2".to_string(), leg(-1, OrderType::Limit, Some(103.125), Some(0), vec![2])),
        ("long#3".to_string(), leg(-1, OrderType::Stop, None, Some(0), vec![1])),
        // a trailing leg (W3c's `OrderListLeg::trail`) crosses in the same rows
        (
            "long#4".to_string(),
            OrderListLeg {
                trigger_price: Some(104.0),
                trail: Some(0.75),
                ..leg(-1, OrderType::Trailing, None, Some(0), vec![])
            },
        ),
    ];

    let mut fake = FakeBroker {
        universe: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        ..FakeBroker::default()
    };
    let r = BrokerRef {
        ctx: (&mut fake as *mut FakeBroker).cast(),
        vtable: broker_vtable::<FakeBroker>(),
    };
    let mut g = crate::guest::HostBroker::new(r);
    let mut want = Vec::new();
    // the guest names a symbol, and the host hears that symbol (`ABI_VERSION` 8)
    let symbol = "BTCUSDT";
    for (i, k) in kinds.into_iter().enumerate() {
        let (tag, side, ro) = (format!("t#{i}"), if i % 2 == 0 { 1 } else { -1 }, i % 3 == 0);
        g.place(symbol, &tag, side, 1.5, k, ro);
        want.push(format!("place({symbol:?}, {tag}, {side}, 1.5, {k:?}, {ro})"));
    }
    let guard = T::Stop { trigger: 98.0 };
    g.place_exit_leg(symbol, "guard", "guard#1", -1, 2.0, guard);
    want.push(format!("place_exit_leg({symbol:?}, guard, guard#1, -1, 2.0, {guard:?})"));
    g.place_at_close(symbol, "drop#1", -1, 2.0, true);
    want.push(format!("place_at_close({symbol:?}, drop#1, -1, 2.0, true)"));
    g.place_close_from_fill(symbol, "cap#1", 1, 3.0, 97.75);
    want.push(format!("place_close_from_fill({symbol:?}, cap#1, 1, 3.0, 97.75)"));
    g.place_group(symbol, legs.clone());
    want.push(format!("place_group({symbol:?}, {legs:?})"));
    // a NaN or an infinity crosses as itself, never as `None`
    let odd = vec![(
        "odd#1".to_string(),
        OrderListLeg {
            qty: f64::NAN,
            price: Some(f64::NAN),
            trigger_price: Some(f64::INFINITY),
            trail: Some(f64::NEG_INFINITY),
            ..leg(1, OrderType::Trailing, None, None, vec![])
        },
    )];
    g.place_group(symbol, odd.clone());
    want.push(format!("place_group({symbol:?}, {odd:?})"));
    g.amend("t#1", Some(2.0), None, Some(99.0));
    want.push("amend(t#1, Some(2.0), None, Some(99.0))".to_string());
    g.amend("t#2", None, Some(100.5), None);
    want.push("amend(t#2, None, Some(100.5), None)".to_string());
    g.cancel("t#3");
    want.push("cancel(t#3)".to_string());
    g.deny(symbol, "bad#9", 2.5, "iceberg: no limit");
    want.push(format!("deny({symbol:?}, bad#9, 2.5, iceberg: no limit)"));
    // a forecast crosses with its symbol and its three terms, with or without a confidence (ABI 11)
    g.publish_forecast(symbol, vike_model::Forecast::new(-0.0025, 3_600_000, Some(0.8)).unwrap());
    want.push(format!("publish_forecast({symbol:?}, -0.0025, 3600000, Some(0.8))"));
    g.publish_forecast("", vike_model::Forecast::new(1.5, 60_000, None).unwrap());
    want.push("publish_forecast(\"\", 1.5, 60000, None)".to_string());
    assert!(g.rests_reduce_only_while_flat(), "the host's answer, not the trait's `false`");
    assert_eq!(g.average_price(symbol), 101.5);
    assert_eq!(g.average_price("ETHUSDT"), 2_501.5, "the symbol crosses to the host's read");
    assert_eq!(g.symbols(), ["BTCUSDT", "ETHUSDT"], "the host's universe, not the trait's none");
    assert_eq!(OrderBroker::instrument_grid(&g, "BTCUSDT"), Some(FAKE_GRID));
    assert_eq!(OrderBroker::instrument_grid(&g, "ETHUSDT"), None);
    drop(g);
    assert_eq!(fake.orders, want);
}

/// An order the order language sends on ANOTHER symbol than its run's (`Tactics::on`, tagged
/// `<symbol>/<name>#<n>`) reaches the host's broker on THAT symbol, each leg of its order list too
/// (`ABI_VERSION` 8): the host places it there or refuses an undeclared symbol in its own words, as
/// for the same source compiled in. Until 8 the guest refused it, because the slots carried no
/// symbol and sent it would have traded the mount's own.
#[test]
fn an_order_on_another_symbol_reaches_the_host_on_that_symbol() {
    use vike_model::{OrderBroker, OrderListLeg, OrderPlacement as T, OrderType, TimeInForce};
    let leg = |side| OrderListLeg {
        side,
        qty: 2.0,
        order_type: OrderType::Market,
        price: None,
        trigger_price: None,
        trail: None,
        reduce_only: false,
        time_in_force: TimeInForce::Gtc,
        parent: None,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    };
    let mut fake = FakeBroker::default();
    let r = BrokerRef {
        ctx: (&mut fake as *mut FakeBroker).cast(),
        vtable: broker_vtable::<FakeBroker>(),
    };
    let mut g = crate::guest::HostBroker::new(r);
    g.place("BTCUSDT", "L#1", 1, 1.0, T::Market, false);
    g.place("ETHUSDT", "ETHUSDT/L#1", 1, 1.0, T::Market, false);
    let stop = T::Stop { trigger: 90.0 };
    g.place_exit_leg("ETHUSDT", "L", "ETHUSDT/L#2", -1, 1.0, stop);
    g.place_at_close("ETHUSDT", "ETHUSDT/drop L#3", -1, 1.0, true);
    g.place_close_from_fill("ETHUSDT", "ETHUSDT/cap close#4", -1, 1.0, 95.0);
    let legs = vec![("ETHUSDT/E#5".to_string(), leg(1)), ("ETHUSDT/E#6".to_string(), leg(-1))];
    g.place_group("ETHUSDT", legs.clone());
    drop(g);
    assert_eq!(
        fake.orders,
        [
            format!("place(\"BTCUSDT\", L#1, 1, 1.0, {:?}, false)", T::Market),
            format!("place(\"ETHUSDT\", ETHUSDT/L#1, 1, 1.0, {:?}, false)", T::Market),
            format!("place_exit_leg(\"ETHUSDT\", L, ETHUSDT/L#2, -1, 1.0, {stop:?})"),
            "place_at_close(\"ETHUSDT\", ETHUSDT/drop L#3, -1, 1.0, true)".to_string(),
            "place_close_from_fill(\"ETHUSDT\", ETHUSDT/cap close#4, -1, 1.0, 95.0)".to_string(),
            format!("place_group(\"ETHUSDT\", {legs:?})"),
        ]
    );
}

/// An order the host cannot decode is REFUSED loudly, never guessed: an unknown kind code, a
/// stop-limit with no limit and an order list leg with a lifetime code this host does not know each
/// panic inside `guarded`, which latches [`POISONED`] (the dispatch re-raises it), and the broker
/// receives nothing.
#[test]
fn an_order_the_host_cannot_decode_poisons_the_dispatch_and_reaches_no_broker() {
    use crate::abi::{OrderListLegAbi, OrderPlacementAbi, SliceAbi};
    use vike_model::OrderPlacement;
    let leg = vike_model::OrderListLeg {
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        price: None,
        trigger_price: None,
        trail: None,
        reduce_only: false,
        time_in_force: vike_model::TimeInForce::Gtc,
        parent: None,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    };
    let bad_legs = [OrderListLegAbi { time_in_force: 99, ..OrderListLegAbi::of("g#1", &leg) }];
    let vt = broker_vtable::<FakeBroker>();
    let mut fake = FakeBroker::default();
    let ctx: *mut c_void = (&mut fake as *mut FakeBroker).cast();
    let (s, sn) = ("BTCUSDT".as_ptr(), 7);
    let (t, n) = ("t#1".as_ptr(), 3);
    let mut unknown = OrderPlacementAbi::of(OrderPlacement::Market);
    unknown.kind = 99;
    let mut no_limit =
        OrderPlacementAbi::of(OrderPlacement::StopLimit { trigger: 99.0, limit: 98.5 });
    no_limit.limit = None.into();
    // each poisons the dispatch under the seam verb's own name, which both diagnostics print
    let calls: [(&dyn Fn(), &str); 3] = [
        (&|| (vt.place)(ctx, s, sn, t, n, 1, 1.0, unknown, false), "OrderBroker::place"),
        (
            &|| (vt.place_exit_leg)(ctx, s, sn, t, n, t, n, 1, 1.0, no_limit),
            "OrderBroker::place_exit_leg",
        ),
        (&|| (vt.place_group)(ctx, s, sn, SliceAbi::of(&bad_legs)), "OrderBroker::place_group"),
    ];
    for (call, verb) in calls {
        POISONED.with(|p| p.set(None));
        call();
        assert_eq!(POISONED.with(Cell::get), Some(verb), "the call must poison the dispatch");
    }
    POISONED.with(|p| p.set(None));
    assert!(fake.orders.is_empty(), "nothing may reach the broker: {:?}", fake.orders);
}

// ---- ABI_VERSION 6: the inputs and the closed trades ---------------------------------------------

/// The names a strategy file declares arrive as themselves, through the JSON text `guest::names_json`
/// writes: a name with a quote, a colon and a non-ASCII letter included. `symbols` (`ABI_VERSION` 8)
/// crosses the same way.
#[test]
fn inputs_cross_from_the_strategy_file_as_the_names_it_declares() {
    fn names() -> Vec<String> {
        ["bybit:ETHUSDT:1m", "user-my \"csv\":OANDA:EUR_USD:1h", "1d", "ü"]
            .into_iter()
            .map(String::from)
            .collect()
    }
    extern "C" fn declares(_h: *mut c_void) -> OptionAbi<StrAbi> {
        static JSON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let json = JSON.get_or_init(|| crate::guest::names_json(&names()));
        OptionAbi::new(Some(StrAbi::of(json)), StrAbi::empty())
    }
    let mut vt = fake_vtable();
    vt.inputs = declares;
    vt.symbols = declares;
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    assert_eq!(Strategy::<FakeBroker>::inputs(&s), names());
    assert_eq!(Strategy::<FakeBroker>::symbols(&s), names());
}

/// `params` and `save_state` (`ABI_VERSION` 8) arrive as the strategy file answered: a bag with its
/// `None`s, a state's bytes (`ABI_VERSION` 15: msgpack, an empty one included, which is not "saves
/// nothing"), or `None`. A panicked answer (the outer `None`) stops the run rather than reading as
/// `None`, a null handle is never asked, and a `load_state` state crosses as its bytes, whole.
#[test]
fn params_and_the_state_cross_as_the_strategy_file_answers() {
    use crate::abi::SliceAbi;
    thread_local! {
        static LOADED: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
    }
    fn lend(text: &'static std::sync::OnceLock<String>) -> OptionAbi<OptionAbi<StrAbi>> {
        let inner =
            OptionAbi::new(Some(StrAbi::of(text.get().expect("set first"))), StrAbi::empty());
        OptionAbi::new(Some(inner), OptionAbi::new(None, StrAbi::empty()))
    }
    static PARAMS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    /// A NaN, so the bytes are seen to cross untouched (no JSON could carry it).
    static STATE: std::sync::OnceLock<vike_model::StrategyState> = std::sync::OnceLock::new();
    extern "C" fn params(_h: *mut c_void) -> OptionAbi<OptionAbi<StrAbi>> {
        lend(&PARAMS)
    }
    extern "C" fn state(_h: *mut c_void) -> OptionAbi<OptionAbi<SliceAbi<u8>>> {
        let bytes = SliceAbi::of(STATE.get().expect("set first").bytes());
        let inner = OptionAbi::new(Some(bytes), SliceAbi::empty());
        OptionAbi::new(Some(inner), OptionAbi::new(None, SliceAbi::empty()))
    }
    extern "C" fn panicked(_h: *mut c_void) -> OptionAbi<OptionAbi<StrAbi>> {
        OptionAbi::new(None, OptionAbi::new(None, StrAbi::empty()))
    }
    extern "C" fn no_state(_h: *mut c_void) -> OptionAbi<OptionAbi<SliceAbi<u8>>> {
        OptionAbi::new(None, OptionAbi::new(None, SliceAbi::empty()))
    }
    extern "C" fn load(_h: *mut c_void, p: *const u8, n: usize) -> StrategyFileStatusAbi {
        let bytes = crate::abi::read_borrowed_bytes(p, n).to_vec();
        LOADED.with(|l| *l.borrow_mut() = Some(bytes));
        StrategyFileStatusAbi::Ok
    }
    let bag = some_params();
    PARAMS.get_or_init(|| crate::guest::params_json(&bag));
    let saved = STATE.get_or_init(|| {
        vike_model::StrategyState::of(&(f64::NAN, -0.0_f64, "kept")).expect("encodes")
    });

    let mut vt = fake_vtable();
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    assert_eq!(
        (Strategy::<FakeBroker>::params(&s), Strategy::<FakeBroker>::save_state(&s)),
        (None, None)
    );

    (vt.params, vt.save_state, vt.load_state) = (params, state, load);
    let mut s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    assert_eq!(Strategy::<FakeBroker>::params(&s), Some(bag));
    assert_eq!(Strategy::<FakeBroker>::save_state(&s).as_ref(), Some(saved), "its very bytes");
    let doc =
        vike_model::StrategyState::from_json_value(&serde_json::json!({"fills": 3, "pct": 0.97}));
    Strategy::<FakeBroker>::load_state(&mut s, &doc);
    assert_eq!(LOADED.with(|l| l.borrow().clone()), Some(doc.into_bytes()));

    (vt.params, vt.save_state) = (panicked, no_state);
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    for read in [
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Strategy::<FakeBroker>::params(&s).is_none()
        })),
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Strategy::<FakeBroker>::save_state(&s).is_none()
        })),
    ] {
        assert!(read.is_err(), "a panicked answer must never read as `None`");
    }
}

/// `refuse` and `echo` (`ABI_VERSION` 9) arrive as the strategy file answered: no export (the inner
/// `None`) reads `None`, a refusal its own words (asked over the params document, with no strategy
/// created), an echo its `(key, value)` pairs in order; a panicked answer stops the run rather than
/// reading as "no refusal", and a null handle (a `create` that refused) reads `created() == false`
/// and is never asked for its echo.
#[test]
fn the_refusal_and_the_echo_cross_as_the_strategy_file_answers() {
    static REFUSAL: &str = "rungs must be at least 1";
    static ECHO: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    thread_local! {
        static ASKED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }
    fn lent(text: &'static str) -> OptionAbi<OptionAbi<StrAbi>> {
        let inner = OptionAbi::new(Some(StrAbi::of(text)), StrAbi::empty());
        OptionAbi::new(Some(inner), OptionAbi::new(None, StrAbi::empty()))
    }
    extern "C" fn refuses(p: *const u8, n: usize) -> OptionAbi<OptionAbi<StrAbi>> {
        ASKED.with(|a| *a.borrow_mut() = Some(crate::abi::StrAbi { ptr: p, len: n }.read()));
        lent(REFUSAL)
    }
    extern "C" fn echoes(_h: *mut c_void) -> OptionAbi<OptionAbi<StrAbi>> {
        lent(ECHO.get_or_init(|| {
            crate::guest::echo_json(&[("rungs", "4".to_string()), ("anchor", "first".to_string())])
        }))
    }
    extern "C" fn refuse_panicked(_p: *const u8, _n: usize) -> OptionAbi<OptionAbi<StrAbi>> {
        OptionAbi::new(None, OptionAbi::new(None, StrAbi::empty()))
    }
    extern "C" fn echo_panicked(_h: *mut c_void) -> OptionAbi<OptionAbi<StrAbi>> {
        OptionAbi::new(None, OptionAbi::new(None, StrAbi::empty()))
    }
    extern "C" fn create_returns_null(
        _p: *const u8,
        _n: usize,
        _f: *const u8,
        _m: usize,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    let vt = fake_vtable();
    assert_eq!(refusal(&vt, "rungs = 0\n"), None, "no refuse export refuses nothing");
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    assert!(s.created());
    assert_eq!(s.echo(), None, "no echo export answers none");

    let mut vt = fake_vtable();
    (vt.refuse, vt.echo) = (refuses, echoes);
    assert_eq!(refusal(&vt, "rungs = 0\n").as_deref(), Some(REFUSAL));
    assert_eq!(ASKED.with(|a| a.borrow().clone()).as_deref(), Some("rungs = 0\n"));
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    assert_eq!(
        s.echo(),
        Some(vec![("rungs".into(), "4".into()), ("anchor".into(), "first".into())])
    );

    (vt.refuse, vt.echo) = (refuse_panicked, echo_panicked);
    assert!(std::panic::catch_unwind(|| refusal(&vt, "")).is_err());
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.echo())).is_err());

    vt.create = create_returns_null;
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    assert!(!s.created());
    assert_eq!(s.echo(), None, "a null handle is never asked");
}

/// A strategy file whose `inputs` panicked (`None`) stops the run on the host's own stack rather than
/// declaring none; a null handle is never asked and declares none, as `warmup` answers 0.
#[test]
fn a_panicked_inputs_stops_the_run_and_a_null_handle_declares_none() {
    extern "C" fn panicked(_h: *mut c_void) -> OptionAbi<StrAbi> {
        OptionAbi::new(None, StrAbi::empty())
    }
    extern "C" fn create_returns_null(
        _p: *const u8,
        _n: usize,
        _f: *const u8,
        _m: usize,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }
    extern "C" fn must_not_be_called(_h: *mut c_void) -> OptionAbi<StrAbi> {
        panic!("inputs must never be called with a null handle");
    }
    let mut vt = fake_vtable();
    vt.inputs = panicked;
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Strategy::<FakeBroker>::inputs(&s)
    }));
    assert!(read.is_err(), "a panicked `inputs` must never read as \"declares none\"");

    let mut vt = fake_vtable();
    vt.create = create_returns_null;
    vt.inputs = must_not_be_called;
    let s = StrategyFileHost::<FakeBroker>::new(vt, "", "{}");
    assert!(Strategy::<FakeBroker>::inputs(&s).is_empty());
}

/// An input's bars and the closed-trade ledger, read on the GUEST's `HostBroker` over the HOST's
/// real [`broker_vtable`], are the host's own memory (`ABI_VERSION` 14: the same pointers), so every
/// field reads as the host's broker holds it, bit for bit (the `{:?}` of an `f64` is its shortest
/// round-tripping text, so `-0.0` and `0.0` differ): an undeclared name reads nothing,
/// `OrderBroker::input` is the last bar, and `None` (no ledger) stays apart from an empty ledger.
#[test]
fn the_guest_reads_the_hosts_own_memory() {
    let bar = |ts: i64, close: f64, extras: bool| Bar {
        timestamp: ts,
        open: 100.25,
        high: 101.0 + f64::EPSILON * 64.0,
        low: -0.0,
        close,
        volume: 1e-300,
        funding: extras.then_some(-0.0001),
        bid: extras.then_some(99.95),
        ask: None,
        symbol: extras.then(|| "ETHUSDT".into()),
    };
    let row = |tag: &str, side: i32, net: f64| vike_model::TradeClosedRow {
        symbol: "BTCUSDT".to_string(),
        tag: tag.to_string(),
        side,
        entry_timestamp: 1_700_000_000_000,
        entry_bar: 3,
        entry_price: 100.1 + f64::EPSILON * 128.0,
        exit_timestamp: 1_700_000_060_000,
        exit_bar: usize::MAX,
        exit_price: 0.1 + 0.2,
        qty: f64::MIN_POSITIVE,
        pnl: -0.0,
        commission: 1e-12,
        net,
    };
    let mut fake = FakeBroker {
        input: vec![bar(0, 10.5, true), bar(60_000, 11.0 / 3.0, false)],
        ledger: Some(vec![row("long#1", 1, 2.5), row("", -1, -1.0 / 3.0)]),
        ..FakeBroker::default()
    };
    let (want_bars, want_rows) = (format!("{:?}", fake.input), format!("{:?}", fake.ledger));
    let at = (fake.input.as_ptr(), fake.ledger.as_deref().map(<[_]>::as_ptr));
    let ctx: *mut c_void = (&mut fake as *mut FakeBroker).cast();
    let g = crate::guest::HostBroker::new(BrokerRef { ctx, vtable: broker_vtable::<FakeBroker>() });
    assert_eq!((g.input_bars(FAKE_INPUT).as_ptr(), g.closed_trades().map(<[_]>::as_ptr)), at);
    assert_eq!(format!("{:?}", g.input_bars(FAKE_INPUT)), want_bars);
    assert_eq!(
        format!("{:?}", g.input(FAKE_INPUT)),
        format!("{:?}", g.input_bars(FAKE_INPUT).last())
    );
    assert!(g.input_bars("bybit:SOLUSDT:1m").is_empty(), "an undeclared name reads nothing");
    assert_eq!(g.input("bybit:SOLUSDT:1m"), None);
    assert_eq!(format!("{:?}", g.closed_trades().map(<[_]>::to_vec)), want_rows);
    drop(g);

    for (ledger, want) in [(Some(Vec::new()), Some(0)), (None, None)] {
        fake.ledger = ledger;
        let ctx: *mut c_void = (&mut fake as *mut FakeBroker).cast();
        let g =
            crate::guest::HostBroker::new(BrokerRef { ctx, vtable: broker_vtable::<FakeBroker>() });
        assert_eq!(g.closed_trades().map(<[_]>::len), want, "no ledger is not an empty one");
    }
}

/// `ABI_VERSION` 15: a read by INDEX on the guest's `HostBroker`, over the host's real [`broker_vtable`],
/// answers what the same read by the name at that index answers, for every symbol of the host's universe
/// (the host broker's own `_at` defaults read by the name); a host that lists no inputs gives an input no
/// index, so it reads by name.
#[test]
fn a_read_by_index_answers_as_the_name_at_that_index() {
    let mut fake = FakeBroker {
        universe: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        pos: -2.5,
        input: vec![bar(1.0)],
        ..FakeBroker::default()
    };
    let ctx: *mut c_void = (&mut fake as *mut FakeBroker).cast();
    let g = crate::guest::HostBroker::new(BrokerRef { ctx, vtable: broker_vtable::<FakeBroker>() });
    for (index, name) in ["BTCUSDT", "ETHUSDT"].into_iter().enumerate() {
        assert_eq!(g.symbol_index(name), Some(index));
        assert_eq!(g.position_at(index).to_bits(), g.position(name).to_bits());
        assert_eq!(g.average_price_at(index).to_bits(), g.average_price(name).to_bits());
        assert_eq!(g.instrument_grid_at(index), g.instrument_grid(name), "{name}");
        assert_eq!(g.contract_value_at(index), g.contract_value(name), "{name}");
    }
    assert_ne!(g.average_price_at(0), g.average_price_at(1), "each index reads its own symbol");
    assert_eq!(g.input_index(FAKE_INPUT), None, "a host listing no inputs reads them by name");
    assert_eq!(g.input_bars(FAKE_INPUT).len(), 1);
}

/// `ABI_VERSION` 10: an input's whole series crosses from the host bit for bit (the bars still to come
/// and every extra field), an unknown name as an empty series, and `None` (no whole series served) as
/// itself, never as an empty one.
#[test]
fn an_inputs_whole_series_crosses_from_the_host_bit_for_bit() {
    let bar = |ts: i64, close: f64| Bar {
        timestamp: ts,
        open: 100.25,
        high: 101.0 + f64::EPSILON * 64.0,
        low: -0.0,
        close,
        volume: 1e-300,
        funding: Some(-0.0001),
        bid: None,
        ask: Some(100.5),
        symbol: Some("ETHUSDT".into()),
    };
    let whole = vec![bar(0, 10.5), bar(60_000, 11.0 / 3.0), bar(120_000, -0.0)];
    let mut fake =
        FakeBroker { input: whole[..1].to_vec(), whole: Some(whole), ..FakeBroker::default() };
    let (want, at) =
        (format!("{:?}", fake.whole.as_deref()), fake.whole.as_deref().map(<[_]>::as_ptr));
    let ctx: *mut c_void = (&mut fake as *mut FakeBroker).cast();
    let g = crate::guest::HostBroker::new(BrokerRef { ctx, vtable: broker_vtable::<FakeBroker>() });
    assert_eq!(g.input_whole(FAKE_INPUT).map(<[_]>::as_ptr), at, "the host's own series");
    assert_eq!(format!("{:?}", g.input_whole(FAKE_INPUT)), want);
    assert_eq!(g.input_bars(FAKE_INPUT).len(), 1, "the as-of read stays its own");
    assert_eq!(g.input_whole("bybit:SOLUSDT:1m").map(<[Bar]>::len), Some(0));
    drop(g);

    fake.whole = None;
    let ctx: *mut c_void = (&mut fake as *mut FakeBroker).cast();
    let g = crate::guest::HostBroker::new(BrokerRef { ctx, vtable: broker_vtable::<FakeBroker>() });
    assert_eq!(g.input_whole(FAKE_INPUT), None, "none served is not an empty series");
}

/// A host whose history changes between dispatches: its own bars, which are also its input
/// [`FAKE_INPUT`], the bars of [`OTHER`], and its closed rows. Every read of them is counted.
#[derive(Default)]
struct GrowingBroker {
    bars: Vec<Bar>,
    /// The bars of [`OTHER`], a second symbol.
    other: Vec<Bar>,
    rows: Vec<vike_model::TradeClosedRow>,
    reads: std::cell::Cell<usize>,
}

/// The second symbol [`GrowingBroker`] answers bars for.
const OTHER: &str = "ETHUSDT";

/// A flat bar `i` minutes in, at `close`.
fn minute_bar(i: usize, close: f64) -> Bar {
    vike_marketdata::test_support::flat_bar_zero_volume(i as i64 * 60_000, close)
}

/// A closed row of one contract booked at minute `i`, closed at `close`.
fn closed_row(i: usize, close: f64) -> vike_model::TradeClosedRow {
    vike_model::TradeClosedRow {
        symbol: "BTCUSDT".to_string(),
        tag: format!("long#{i}"),
        side: 1,
        entry_timestamp: i as i64 * 60_000,
        entry_bar: i,
        entry_price: close,
        exit_timestamp: i as i64 * 60_000,
        exit_bar: i,
        exit_price: close + 0.25,
        qty: 1.0,
        pnl: 0.25,
        commission: 0.1,
        net: 0.15,
    }
}

/// What one dispatch over `fake` reads: where its own bars are, then its own bars, [`OTHER`]'s, its
/// input and its closed rows.
type SeenHistory = (*const Bar, Vec<Bar>, Vec<Bar>, Vec<Bar>, Vec<vike_model::TradeClosedRow>);

fn dispatch_reading(fake: &mut GrowingBroker) -> SeenHistory {
    let ctx: *mut c_void = (fake as *mut GrowingBroker).cast();
    let g =
        crate::guest::HostBroker::new(BrokerRef { ctx, vtable: broker_vtable::<GrowingBroker>() });
    let own = g.bars("");
    let rows = g.closed_trades().expect("the host keeps a ledger").to_vec();
    (own.as_ptr(), own.to_vec(), g.bars(OTHER).to_vec(), g.input_bars(FAKE_INPUT).to_vec(), rows)
}

impl GrowingBroker {
    fn read<'a>(&self, history: &'a [Bar]) -> &'a [Bar] {
        self.reads.set(self.reads.get() + 1);
        history
    }
}

impl vike_model::OrderBroker for GrowingBroker {
    fn place(&mut self, _: &str, _: &str, _: i32, _: f64, _: OrderPlacement, _: bool) {}
    inert_order_verbs!();
    fn position(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn now(&self) -> i64 {
        0
    }
    fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }
    fn bars(&self, symbol: &str) -> &[Bar] {
        self.read(if symbol == OTHER { &self.other } else { &self.bars })
    }
    fn input_bars(&self, name: &str) -> &[Bar] {
        self.read(if name == FAKE_INPUT { &self.bars } else { &[] })
    }
    fn closed_trades(&self) -> Option<&[vike_model::TradeClosedRow]> {
        self.reads.set(self.reads.get() + 1);
        Some(&self.rows)
    }
}

/// `ABI_VERSION` 14: every dispatch reads the host's history as it is NOW, grown (its `Vec` moving as it
/// does), on two symbols, or rewritten in place (paper, live): each read is ONE call to the host that
/// lends its own rows, and nothing is kept from one dispatch to the next, so a run's history costs no
/// copy at all (13 copied each bar once where it only grew, and the whole history on every dispatch where
/// it did not).
#[test]
fn every_dispatch_reads_the_hosts_history_as_it_is_now() {
    let mut fake = GrowingBroker::default();
    for i in 0..50 {
        fake.bars.push(minute_bar(i, 10.0 + i as f64));
        fake.other.push(minute_bar(i, 500.0 - i as f64));
        if i % 7 == 0 {
            fake.rows.push(closed_row(i, 10.0 + i as f64));
        }
        fake.reads.set(0);
        let (at, own, other, input, rows) = dispatch_reading(&mut fake);
        assert_eq!(at, fake.bars.as_ptr(), "dispatch {i}: the host's bars, where they are now");
        let host = (fake.bars.clone(), fake.other.clone(), fake.bars.clone(), fake.rows.clone());
        assert_eq!((own, other, input, rows), host, "dispatch {i}");
        assert_eq!(fake.reads.get(), 4, "dispatch {i}: one call a read, nothing kept");
    }
    fake.bars[1].close = 12.5;
    fake.other[0].close = 21.0;
    fake.rows[0].net = -3.0;
    let (_, own, other, _, rows) = dispatch_reading(&mut fake);
    let host = (fake.bars.clone(), fake.other.clone(), fake.rows.clone());
    assert_eq!((own, other, rows), host, "the rewritten history");
}
