use super::*;

#[derive(Default)]
struct FakeBroker {
    pos: f64,
    submits: Vec<(String, i32, f64)>,
}

impl Broker for FakeBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.submits.push((symbol.to_string(), side, qty));
    }
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
    fn position(&self, _symbol: &str) -> f64 {
        self.pos
    }
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
}

impl HftBroker for FakeBroker {
    fn position(&self) -> f64 {
        self.pos
    }
    fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
    fn modify_tagged(&mut self, _tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {}
    fn cancel_tagged(&mut self, _tag: &str) {}
}

/// A non-null, never-dereferenced sentinel handle for fakes that carry no real state — using
/// a genuine null here (as an earlier version of this test module did) would now trip the
/// `PluginStatus::BadHandle` guard `PluginStrategy` checks before every dispatch, since that
/// guard cannot distinguish "no state needed" from "creation failed" any more than the real
/// ABI can (see `PluginStrategy::new`'s doc).
fn dummy_handle() -> *mut c_void {
    std::ptr::without_provenance_mut(1)
}

// A hand-built fake plugin: no `.so`, no dlopen. `fake_on_bar` wraps its received `BrokerRef`
// as a `guest::HostBroker` and submits a market order sized off the bar's close — proving
// guest.rs and host.rs interoperate correctly with each other, not just in isolation.
extern "C" fn fake_create(_params_ptr: *const u8, _params_len: usize) -> *mut c_void {
    dummy_handle()
}
extern "C" fn fake_destroy(_handle: *mut c_void) {}
extern "C" fn fake_warmup(_handle: *mut c_void) -> usize {
    2
}
extern "C" fn fake_on_bar(_handle: *mut c_void, r: BrokerRef, bar: *const CBar) -> PluginStatus {
    // SAFETY: `bar` is the live, aligned `*const CBar` `PluginStrategy::on_bar` passed for
    // exactly this call.
    let close = unsafe { (*bar).close };
    let mut broker = crate::guest::HostBroker::new(r);
    vike_model::Broker::submit_market(&mut broker, "BTCUSDT", 1, close);
    PluginStatus::Ok
}
extern "C" fn fake_on_bar_panicking(
    _handle: *mut c_void,
    _r: BrokerRef,
    _bar: *const CBar,
) -> PluginStatus {
    PluginStatus::Panicked
}

// ---- the thirteen ABI_VERSION 3 dispatch slots, as inert stubs -----------------------------
//
// Only the slots a given test drives are overridden on the returned vtable; everything else
// answers `Ok` and does nothing. A stub per slot rather than one shared function because the
// signatures genuinely differ — that difference is the ABI.

extern "C" fn stub_on_start(_h: *mut c_void, _r: BrokerRef) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_stop(_h: *mut c_void, _r: BrokerRef) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_quote_tick(
    _h: *mut c_void,
    _r: BrokerRef,
    _q: *const CQuoteTick,
) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_trade_tick(
    _h: *mut c_void,
    _r: BrokerRef,
    _t: *const CTradeTick,
) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_order_book(_h: *mut c_void, _r: BrokerRef, _b: BookRef) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_schedule(
    _h: *mut c_void,
    _r: BrokerRef,
    _t: *const u8,
    _n: usize,
) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_fill(_h: *mut c_void, _r: BrokerRef, _f: *const CFill) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_feed_status(_h: *mut c_void, _r: BrokerRef, _s: u32) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_mark(_h: *mut c_void, _r: BrokerRef, _m: *const CMarkTick) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_reference_quote(
    _h: *mut c_void,
    _r: BrokerRef,
    _v: *const u8,
    _n: usize,
    _q: *const CQuoteTick,
) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_flow(
    _h: *mut c_void,
    _r: BrokerRef,
    _f: *const CFlowToxicity,
) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_order_event(
    _h: *mut c_void,
    _r: BrokerRef,
    _e: *const COrderLifecycle,
) -> PluginStatus {
    PluginStatus::Ok
}
extern "C" fn stub_on_params_updated(
    _h: *mut c_void,
    _r: BrokerRef,
    _p: *const u8,
    _n: usize,
) -> PluginStatus {
    PluginStatus::Ok
}

fn fake_vtable() -> PluginVTable {
    PluginVTable {
        create: fake_create,
        destroy: fake_destroy,
        warmup: fake_warmup,
        on_start: stub_on_start,
        on_bar: fake_on_bar,
        on_quote_tick: stub_on_quote_tick,
        on_trade_tick: stub_on_trade_tick,
        on_order_book: stub_on_order_book,
        on_schedule: stub_on_schedule,
        on_fill: stub_on_fill,
        on_feed_status: stub_on_feed_status,
        on_mark: stub_on_mark,
        on_reference_quote: stub_on_reference_quote,
        on_flow: stub_on_flow,
        on_order_event: stub_on_order_event,
        on_params_updated: stub_on_params_updated,
        on_stop: stub_on_stop,
    }
}

fn bar(close: f64) -> Bar {
    Bar {
        ts: 1,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// `CBar`'s round trip is exhaustively covered in `abi.rs` (Task 1); this is a thin re-check
/// that host.rs's own re-export path still round-trips, including the optionals — kept per
/// Task 3's brief, but not duplicating that file's full coverage.
#[test]
fn a_bar_survives_the_round_trip_including_its_optionals() {
    let b = Bar {
        ts: 1,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 10.0,
        funding: Some(0.01),
        bid: None,
        ask: Some(1.6),
        symbol: Some("BTCUSDT".to_string()),
    };
    let back = CBar::from_bar(&b).to_bar();
    assert_eq!(back.ts, b.ts);
    assert_eq!(back.close, b.close);
    assert_eq!(back.funding, Some(0.01));
    assert_eq!(back.bid, None, "an absent optional must not arrive as Some(NaN)");
    assert_eq!(back.symbol.as_deref(), Some("BTCUSDT"));
}

#[test]
fn warmup_crosses_the_vtable() {
    let strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(PluginStrategy::<FakeBroker>::new(fake_vtable(), ""));
    assert_eq!(strategy.warmup(), 2);
}

#[test]
fn on_bar_reaches_the_real_broker_through_both_wrappers() {
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(PluginStrategy::<FakeBroker>::new(fake_vtable(), ""));
    let mut broker = FakeBroker::default();
    strategy.on_bar(&mut broker, &bar(42.0));
    assert_eq!(broker.submits, vec![("BTCUSDT".to_string(), 1, 42.0)]);
}

/// A plugin that reports `Panicked` must not abort the process or unwind out of `on_bar` — it
/// is a reported, contained failure. `tests/load_refusals.rs` (Task 4) proves the stronger
/// claim (a REAL panic inside a compiled `.so`, caught by ITS OWN catch_unwind); this proves
/// the host's side of the contract: receiving `Panicked` never panics the caller.
#[test]
fn a_panicked_status_is_reported_and_does_not_panic_the_caller() {
    let mut vt = fake_vtable();
    vt.on_bar = fake_on_bar_panicking;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
    let mut broker = FakeBroker::default();
    strategy.on_bar(&mut broker, &bar(1.0));
    assert!(broker.submits.is_empty(), "a panicked dispatch must not have reached the broker");
}

/// Important-4: a null `create()` handle must be reported as `PluginStatus::BadHandle` and
/// must NEVER be handed to `warmup`/`on_bar` — both plugin functions below panic if called at
/// all, so this test fails LOUDLY if the guard regresses. ⚠ Not "a real panic, not a swallowed
/// one" (an earlier version of this comment said that, and a round-2 review caught it): both
/// vtable slots are `extern "C" fn` pointers, and the abort-on-unwind-across-`extern "C"`
/// behavior is baked into a function's OWN compiled body by its declared ABI, regardless of
/// whether the call crosses a real `dlopen` boundary or stays in-process — so a regression
/// here would ABORT the test process at the `must_not_be_called_*` call site, not raise a
/// catchable Rust panic `#[test]`'s own harness could report as a normal failure. Either way
/// the guard's absence is unmistakable; it just would not look like an ordinary red test.
#[test]
fn a_null_handle_is_never_handed_to_the_plugin() {
    extern "C" fn create_returns_null(_p: *const u8, _n: usize) -> *mut c_void {
        std::ptr::null_mut()
    }
    extern "C" fn must_not_be_called_warmup(_h: *mut c_void) -> usize {
        panic!("warmup must never be called with a null handle");
    }
    extern "C" fn must_not_be_called_on_bar(
        _h: *mut c_void,
        _r: BrokerRef,
        _b: *const CBar,
    ) -> PluginStatus {
        panic!("on_bar must never be called with a null handle");
    }
    let mut vt = fake_vtable();
    vt.create = create_returns_null;
    vt.warmup = must_not_be_called_warmup;
    vt.on_bar = must_not_be_called_on_bar;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
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

/// Critical-1: a panic inside the HOST's own `Broker` impl — reached from a `t_*` thunk the
/// PLUGIN calls into during its `on_bar` — must not abort the process, and must surface as a
/// genuine (catchable, on THIS side) panic from `PluginStrategy::on_bar` rather than silently
/// producing a wrong number. `catch_unwind` here is what proves "did not abort": an escaped
/// unwind across the `extern "C"` thunk beneath this call would already have aborted the
/// process before this line could ever run.
#[test]
fn a_host_side_panic_inside_a_thunk_poisons_the_dispatch_and_is_re_raised() {
    struct PanickingBroker;
    impl Broker for PanickingBroker {
        fn submit_market(&mut self, _s: &str, _side: i32, _qty: f64) {}
        fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _price: f64) {}
        fn position(&self, _s: &str) -> f64 {
            panic!("deliberate host-side panic for the Critical-1 mutation proof")
        }
        fn price(&self, _s: &str) -> f64 {
            0.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _s: &str) -> &[Bar] {
            &[]
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    }
    impl HftBroker for PanickingBroker {
        fn position(&self) -> f64 {
            0.0
        }
        fn submit_limit_tagged(&mut self, _t: &str, _side: i32, _qty: f64, _price: f64) {}
        fn modify_tagged(&mut self, _t: &str, _q: Option<f64>, _p: Option<f64>) {}
        fn cancel_tagged(&mut self, _t: &str) {}
    }

    // A fake plugin whose on_bar calls the ONE thing that panics: Broker::position via the
    // guest's own HostBroker wrapper (crossing the same real BrokerVTable path production
    // code uses, not calling the thunk directly).
    extern "C" fn on_bar_calls_position(
        _h: *mut c_void,
        r: BrokerRef,
        _bar: *const CBar,
    ) -> PluginStatus {
        let broker = crate::guest::HostBroker::new(r);
        let _ = Broker::position(&broker, "BTCUSDT"); // reaches PanickingBroker::position
        PluginStatus::Ok
    }
    let mut vt = fake_vtable();
    vt.on_bar = on_bar_calls_position;
    let mut strategy: Box<dyn Strategy<PanickingBroker>> =
        Box::new(PluginStrategy::<PanickingBroker>::new(vt, ""));
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
    impl Broker for OtherBroker {
        fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
            self.submits.push((symbol.to_string(), side, qty));
        }
        fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _price: f64) {}
        fn position(&self, _s: &str) -> f64 {
            0.0
        }
        fn price(&self, _s: &str) -> f64 {
            0.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _s: &str) -> &[Bar] {
            &[]
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    }
    impl HftBroker for OtherBroker {
        fn position(&self) -> f64 {
            0.0
        }
        fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
        fn modify_tagged(&mut self, _tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {}
        fn cancel_tagged(&mut self, _tag: &str) {}
    }

    // Force both instantiations to exist in the same binary, in either order.
    let vt_fake = broker_vtable::<FakeBroker>();
    let vt_other = broker_vtable::<OtherBroker>();

    let mut a = FakeBroker::default();
    (vt_fake.submit_market)((&mut a as *mut FakeBroker).cast(), b"X".as_ptr(), 1, 1, 1.0);
    assert_eq!(a.submits, vec![("X".to_string(), 1, 1.0)]);

    let mut o = OtherBroker::default();
    (vt_other.submit_market)((&mut o as *mut OtherBroker).cast(), b"Y".as_ptr(), 1, -1, 2.0);
    assert_eq!(o.submits, vec![("Y".to_string(), -1, 2.0)]);
}

// ---- ABI_VERSION 3: every wired hook actually reaches the plugin --------------------------

thread_local! {
    /// Which vtable slot each recording stub below was entered through, in order.
    static REACHED: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn note(hook: &'static str) -> PluginStatus {
    REACHED.with(|v| v.borrow_mut().push(hook));
    PluginStatus::Ok
}

/// **The structural witness for this whole widening.** `PluginStrategy` implements
/// `Strategy<B>`, and every hook it does NOT override falls through to the trait's own no-op
/// default — which compiles, runs, and silently delivers nothing. That is exactly the failure
/// the unwired-hook refusal was invented for, and adding a `PluginVTable` FIELD does not
/// prevent it: a field can be declared, bound by the loader, exported by the template, and
/// still never called, because the one line that calls it lives in the trait impl.
///
/// So this drives all fifteen through the `Strategy` trait — the surface the backtest engine
/// uses — and asserts each one arrived at its own slot, by name. Deleting any single
/// `fn on_*` from the impl above makes it fail naming that hook.
#[test]
fn every_wired_hook_reaches_the_plugin_through_the_strategy_trait() {
    extern "C" fn r_on_start(_h: *mut c_void, _r: BrokerRef) -> PluginStatus {
        note("on_start")
    }
    extern "C" fn r_on_stop(_h: *mut c_void, _r: BrokerRef) -> PluginStatus {
        note("on_stop")
    }
    extern "C" fn r_on_bar(_h: *mut c_void, _r: BrokerRef, _b: *const CBar) -> PluginStatus {
        note("on_bar")
    }
    extern "C" fn r_on_quote_tick(
        _h: *mut c_void,
        _r: BrokerRef,
        _q: *const CQuoteTick,
    ) -> PluginStatus {
        note("on_quote_tick")
    }
    extern "C" fn r_on_trade_tick(
        _h: *mut c_void,
        _r: BrokerRef,
        _t: *const CTradeTick,
    ) -> PluginStatus {
        note("on_trade_tick")
    }
    extern "C" fn r_on_order_book(_h: *mut c_void, _r: BrokerRef, _b: BookRef) -> PluginStatus {
        note("on_order_book")
    }
    extern "C" fn r_on_schedule(
        _h: *mut c_void,
        _r: BrokerRef,
        _t: *const u8,
        _n: usize,
    ) -> PluginStatus {
        note("on_schedule")
    }
    extern "C" fn r_on_fill(_h: *mut c_void, _r: BrokerRef, _f: *const CFill) -> PluginStatus {
        note("on_fill")
    }
    extern "C" fn r_on_feed_status(_h: *mut c_void, _r: BrokerRef, _s: u32) -> PluginStatus {
        note("on_feed_status")
    }
    extern "C" fn r_on_mark(_h: *mut c_void, _r: BrokerRef, _m: *const CMarkTick) -> PluginStatus {
        note("on_mark")
    }
    extern "C" fn r_on_reference_quote(
        _h: *mut c_void,
        _r: BrokerRef,
        _v: *const u8,
        _n: usize,
        _q: *const CQuoteTick,
    ) -> PluginStatus {
        note("on_reference_quote")
    }
    extern "C" fn r_on_flow(
        _h: *mut c_void,
        _r: BrokerRef,
        _f: *const CFlowToxicity,
    ) -> PluginStatus {
        note("on_flow")
    }
    extern "C" fn r_on_order_event(
        _h: *mut c_void,
        _r: BrokerRef,
        _e: *const COrderLifecycle,
    ) -> PluginStatus {
        note("on_order_event")
    }
    extern "C" fn r_on_params_updated(
        _h: *mut c_void,
        _r: BrokerRef,
        _p: *const u8,
        _n: usize,
    ) -> PluginStatus {
        note("on_params_updated")
    }
    extern "C" fn r_warmup(_h: *mut c_void) -> usize {
        REACHED.with(|v| v.borrow_mut().push("warmup"));
        7
    }

    REACHED.with(|v| v.borrow_mut().clear());
    let vt = PluginVTable {
        create: fake_create,
        destroy: fake_destroy,
        warmup: r_warmup,
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
    };
    let mut s: Box<dyn Strategy<FakeBroker>> = Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
    let mut b = FakeBroker::default();

    assert_eq!(s.warmup(), 7);
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
    s.on_flow(&mut b, vike_model::FlowToxicity { bid: 0.1, ask: 0.9, ts: 5 });
    s.on_order_event(&mut b, &a_lifecycle());
    s.on_params_updated(&mut b, &some_params());
    s.on_stop(&mut b);

    let reached = REACHED.with(|v| v.borrow().clone());
    let mut missing: Vec<&&str> =
        WIRED_HOOKS.iter().filter(|h| !reached.contains(&(**h))).collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "these WIRED hooks never reached the plugin — `PluginStrategy`'s `Strategy` impl is \
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

fn quote_tick() -> vike_marketdata::QuoteTick {
    vike_marketdata::QuoteTick {
        ts: 10,
        local_ts: 11,
        bid: 99.0,
        ask: 101.0,
        bid_size: 1.5,
        ask_size: 2.5,
        symbol: "BTCUSDT".to_string(),
    }
}
fn trade_tick() -> vike_marketdata::TradeTick {
    vike_marketdata::TradeTick {
        ts: 12,
        local_ts: 13,
        price: 100.0,
        size: 0.25,
        is_buyer_maker: false,
        symbol: "BTCUSDT".to_string(),
    }
}
fn a_fill() -> vike_model::Fill {
    vike_model::Fill {
        side: 1,
        size: 2.0,
        price: 100.5,
        fee: 0.01,
        ts: 14,
        is_maker: false,
        symbol: "BTCUSDT".to_string(),
    }
}
fn a_mark() -> vike_model::MarkTick {
    vike_model::MarkTick { symbol: "btcusdt".to_string(), price: 64_000.0, ts: 15 }
}
fn a_lifecycle() -> vike_model::OrderLifecycle {
    vike_model::OrderLifecycle {
        client_order_id: "coid-1".to_string(),
        tag: Some("bid-1".to_string()),
        kind: vike_model::OrderEventKind::Rejected { reason: "min notional".to_string() },
    }
}
fn some_params() -> vike_model::StrategyParams {
    // The CONTROLLER variant, chosen because its `TripleBarrier` carries four `Option`
    // fields left at `None` — the exact shape a TOML hop could not serialise at all. A test
    // that used a fully-populated bag would pass under either encoding and prove nothing
    // about the choice `guest::strategy_params_from_json` argues for.
    vike_model::StrategyParams::PositionController(vike_model::ControllerParams::new(
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
            vike_marketdata::BookLevel::new(99.5, 3.0),
            vike_marketdata::BookLevel::new(99.0, 5.0),
            vike_marketdata::BookLevel::new(98.5, 7.0),
        ],
        &[vike_marketdata::BookLevel::new(100.0, 2.0), vike_marketdata::BookLevel::new(100.5, 4.0)],
    );
    book
}

/// The book CURSOR, end to end through both wrappers: the host flattens its real `L2Book`, the
/// plugin rebuilds one from the cursor, and every public read must agree.
///
/// ⚠ It compares the READS rather than the struct, and that is the stronger claim available:
/// `L2Book` has no `PartialEq` and its level maps are private, so what a user strategy can
/// actually observe IS this list of accessors. A reconstruction that happened to hold the same
/// bytes but answered `best_bid` differently would be useless; one that answers every accessor
/// identically is indistinguishable from the original to any strategy.
#[test]
fn the_book_cursor_rebuilds_a_book_the_plugin_cannot_tell_from_the_original() {
    thread_local! {
        static SEEN: std::cell::RefCell<Option<Vec<(String, String)>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Every public read of an `L2Book`, as `(name, debug-formatted value)` — a string so one
    /// vector can hold `Option<BookLevel>`, `Option<f64>`, `usize` and `u64` together, and so
    /// a failure prints what differed rather than a bare `false`.
    fn probe(b: &vike_marketdata::L2Book) -> Vec<(String, String)> {
        let mut out = vec![
            ("tick_size".to_string(), format!("{:?}", b.tick_size)),
            ("last_seq".to_string(), format!("{:?}", b.last_seq)),
            ("bid_levels".to_string(), format!("{:?}", b.bid_levels())),
            ("ask_levels".to_string(), format!("{:?}", b.ask_levels())),
            ("best_bid".to_string(), format!("{:?}", b.best_bid())),
            ("best_ask".to_string(), format!("{:?}", b.best_ask())),
            ("mid".to_string(), format!("{:?}", b.mid())),
            ("spread".to_string(), format!("{:?}", b.spread())),
            ("imbalance".to_string(), format!("{:?}", b.imbalance())),
            ("top_n(8)".to_string(), format!("{:?}", b.top_n(8))),
            ("vwap_buy_6".to_string(), format!("{:?}", b.avg_px_for_quantity(1, 6.0))),
            ("vwap_sell_6".to_string(), format!("{:?}", b.avg_px_for_quantity(-1, 6.0))),
            ("depth_buy_100.5".to_string(), format!("{:?}", b.quantity_for_price(1, 100.5))),
            ("depth_sell_99.0".to_string(), format!("{:?}", b.quantity_for_price(-1, 99.0))),
            ("sim_buy_3".to_string(), format!("{:?}", b.simulate_fill(1, 3.0))),
        ];
        for px in [98.5, 99.0, 99.5, 100.0, 100.5] {
            out.push((format!("bid_qty_at({px})"), format!("{:?}", b.bid_qty_at(px))));
            out.push((format!("ask_qty_at({px})"), format!("{:?}", b.ask_qty_at(px))));
        }
        out
    }

    extern "C" fn rebuild_and_record(_h: *mut c_void, _r: BrokerRef, b: BookRef) -> PluginStatus {
        let rebuilt = crate::guest::book_from_ref(b);
        SEEN.with(|s| *s.borrow_mut() = Some(probe(&rebuilt)));
        PluginStatus::Ok
    }

    SEEN.with(|s| *s.borrow_mut() = None);
    let mut vt = fake_vtable();
    vt.on_order_book = rebuild_and_record;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
    let mut broker = FakeBroker::default();

    let original = seeded_book();
    strategy.on_order_book(&mut broker, &original);

    let rebuilt = SEEN
        .with(|s| s.borrow().clone())
        .expect("the plugin's on_order_book must have been reached");
    let expected = probe(&original);
    let diffs: Vec<String> = expected
        .iter()
        .zip(&rebuilt)
        .filter(|((_, a), (_, b))| a != b)
        .map(|((name, a), (_, b))| format!("{name}: host {a} vs plugin {b}"))
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
    extern "C" fn count(_h: *mut c_void, _r: BrokerRef, b: BookRef) -> PluginStatus {
        let rebuilt = crate::guest::book_from_ref(b);
        LEVELS.with(|c| c.set((rebuilt.bid_levels(), rebuilt.ask_levels())));
        PluginStatus::Ok
    }
    let mut vt = fake_vtable();
    vt.on_order_book = count;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
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
    extern "C" fn decode(_h: *mut c_void, _r: BrokerRef, p: *const u8, n: usize) -> PluginStatus {
        // Read back through `CStrRef`, which is exactly what the cdylib template does — so
        // this stub exercises the production decode path rather than a second spelling of
        // it, and adds no `unsafe` site of its own (the one that matters lives in `abi.rs`,
        // once, for every borrowed string in this ABI).
        let text = crate::abi::CStrRef { ptr: p, len: n }.read();
        match crate::guest::strategy_params_from_json(&text) {
            Some(v) => {
                DECODED.with(|d| *d.borrow_mut() = Some(v));
                PluginStatus::Ok
            }
            None => PluginStatus::BadParams,
        }
    }
    let mut vt = fake_vtable();
    vt.on_params_updated = decode;
    let mut strategy: Box<dyn Strategy<FakeBroker>> =
        Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
    let mut broker = FakeBroker::default();
    let sent = some_params();
    strategy.on_params_updated(&mut broker, &sent);
    assert_eq!(
        DECODED.with(|d| d.borrow().clone()),
        Some(sent),
        "the params bag must arrive byte-equal after the JSON hop, `None` fields included"
    );
}
