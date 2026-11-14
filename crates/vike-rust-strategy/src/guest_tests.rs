use super::*;
use crate::abi::{GridOptionAbi, OptionAbi, StrAbi, TradeClosedRowAbi};
use std::cell::RefCell;

thread_local! {
    static PLACES: RefCell<Vec<Placed>> = const { RefCell::new(Vec::new()) };
    static BAR_AT_CALLS: RefCell<usize> = const { RefCell::new(0) };
    static SYMBOL_AT_CALLS: RefCell<usize> = const { RefCell::new(0) };
}

/// `(symbol, tag, side, qty, order, reduce_only)`, as recorded by `rec_place` — named to keep the
/// `thread_local!` declaration under clippy's `type_complexity` threshold.
type Placed = (String, String, i32, f64, Option<OrderPlacement>, bool);

const STUB_BAR_COUNT: usize = 4;

/// The universe [`stub_symbol_at`] answers.
const STUB_UNIVERSE: [&str; 2] = ["BTCUSDT", "ETHUSDT"];

fn read_symbol(s: *const u8, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    // SAFETY: the test's own stubs are called only through the vtable this test builds, with
    // `(s, n)` always the `(ptr, len)` of a live `&str` for the duration of this call — the
    // same contract `abi.rs`'s module doc states for every string crossing the boundary.
    let bytes = unsafe { std::slice::from_raw_parts(s, n) };
    std::str::from_utf8(bytes).expect("test harness only ever sends valid UTF-8").to_string()
}

/// One inert host slot past the erased context every slot opens with: its remaining parameter
/// types (each slot's own — that difference is the ABI) and the constant it answers.
macro_rules! host_stub {
    ($name:ident($($ty:ty),* $(,)?)) => {
        extern "C" fn $name(_c: *mut core::ffi::c_void, $(_: $ty),*) {}
    };
    ($name:ident($($ty:ty),*) -> $ret:ty => $answer:expr) => {
        extern "C" fn $name(_c: *mut core::ffi::c_void, $(_: $ty),*) -> $ret {
            $answer
        }
    };
}

host_stub!(stub_position(*const u8, usize) -> f64 => 3.5);
host_stub!(stub_price(*const u8, usize) -> f64 => 101.25);
host_stub!(stub_equity() -> f64 => 10_000.0);
host_stub!(stub_index() -> usize => 7);
host_stub!(stub_now() -> i64 => 1_700_000_000_000);
host_stub!(stub_bars_len(*const u8, usize) -> usize => STUB_BAR_COUNT);
host_stub!(stub_quote_vwap(*const u8, usize, i32, f64) -> crate::abi::OptionAbi<f64> => Some(42.0).into());
host_stub!(stub_depth_within_price(*const u8, usize, i32, f64) -> f64 => 99.0);
host_stub!(stub_contract_value(*const u8, usize) -> crate::abi::ContractValueOptionAbi =>
    Some(vike_model::ContractValue::inverse(100.0)).into());
extern "C" fn stub_bar_at(
    _c: *mut core::ffi::c_void,
    _s: *const u8,
    _n: usize,
    i: usize,
    out: *mut BarAbi,
) -> bool {
    BAR_AT_CALLS.with(|v| *v.borrow_mut() += 1);
    if i >= STUB_BAR_COUNT {
        return false;
    }
    let bar = BarAbi::from_bar(&Bar {
        timestamp: i as i64,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    });
    // SAFETY: `out` is the live, aligned `*mut BarAbi` `HostBroker::bars` passed for exactly
    // this call — the same contract `BrokerVTableAbi::bar_at`'s doc states.
    unsafe { *out = bar };
    true
}

fn bar_at_calls() -> usize {
    BAR_AT_CALLS.with(|v| *v.borrow())
}

// ---- OrderBroker slots: `place` records what crossed; the rest are inert here, their round
// trip over the real host vtable is
// `host_tests.rs`'s `every_order_verb_crosses_from_the_guest_to_the_host_as_itself` ----

extern "C" fn rec_place(
    _c: *mut core::ffi::c_void,
    ss: *const u8,
    sn: usize,
    ts: *const u8,
    tn: usize,
    side: i32,
    qty: f64,
    order: OrderPlacementAbi,
    reduce_only: bool,
) {
    let row =
        (read_symbol(ss, sn), read_symbol(ts, tn), side, qty, order.to_tagged_order(), reduce_only);
    PLACES.with(|v| v.borrow_mut().push(row));
}
host_stub!(stub_instrument_grid(*const u8, usize) -> GridOptionAbi => None.into());
host_stub!(
    stub_place_exit_leg(
        *const u8,
        usize,
        *const u8,
        usize,
        *const u8,
        usize,
        i32,
        f64,
        OrderPlacementAbi,
    )
);
host_stub!(stub_place_at_close(*const u8, usize, *const u8, usize, i32, f64, bool));
host_stub!(stub_place_close_from_fill(*const u8, usize, *const u8, usize, i32, f64, f64));
host_stub!(stub_rests_reduce_only_while_flat() -> bool => false);
host_stub!(stub_place_group(*const u8, usize, *const u8, usize));
host_stub!(stub_amend(*const u8, usize, OptionAbi<f64>, OptionAbi<f64>, OptionAbi<f64>));
host_stub!(stub_cancel(*const u8, usize));
host_stub!(stub_avg_price(*const u8, usize) -> f64 => 0.0);
host_stub!(stub_deny(*const u8, usize, *const u8, usize, f64, *const u8, usize));
extern "C" fn stub_symbol_at(_c: *mut core::ffi::c_void, i: usize) -> OptionAbi<StrAbi> {
    SYMBOL_AT_CALLS.with(|v| *v.borrow_mut() += 1);
    OptionAbi::<StrAbi>::of(STUB_UNIVERSE.get(i).copied())
}

// ---- ABI_VERSION 6 stubs: the input [`STUB_INPUT`] holds [`STUB_INPUT_BARS`] bars and every other
// name none; the ledger is what [`LEDGER`] says. Their bit-for-bit round trip over the real host
// vtable is `host_tests.rs`'s `an_inputs_bars_and_the_closed_trades_cross_from_the_host_bit_for_bit`.

const STUB_INPUT: &str = "bybit:ETHUSDT:1m";
const STUB_INPUT_BARS: usize = 3;

extern "C" fn stub_input_bars_len(_c: *mut core::ffi::c_void, s: *const u8, n: usize) -> usize {
    if read_symbol(s, n) == STUB_INPUT { STUB_INPUT_BARS } else { 0 }
}
extern "C" fn stub_input_bar_at(
    _c: *mut core::ffi::c_void,
    s: *const u8,
    n: usize,
    i: usize,
) -> OptionAbi<BarAbi> {
    INPUT_BAR_AT_CALLS.with(|v| *v.borrow_mut() += 1);
    let known = read_symbol(s, n) == STUB_INPUT && i < STUB_INPUT_BARS;
    let bar = vike_marketdata::test_support::flat_bar_zero_volume(i as i64, 10.0 + i as f64);
    OptionAbi::new(known.then(|| BarAbi::from_bar(&bar)), BarAbi::empty())
}
extern "C" fn stub_closed_trades_len(_c: *mut core::ffi::c_void) -> OptionAbi<usize> {
    OptionAbi::new(LEDGER.with(|l| *l.borrow()), 0)
}
extern "C" fn stub_closed_trade_at(
    _c: *mut core::ffi::c_void,
    i: usize,
) -> OptionAbi<TradeClosedRowAbi> {
    let n = LEDGER.with(|l| l.borrow().unwrap_or(0));
    // Its strings are `'static`, so the borrowed pair outlives this call as a host's row does.
    let row = TradeClosedRowAbi {
        symbol: StrAbi::of("BTCUSDT"),
        net: i as f64,
        ..TradeClosedRowAbi::empty()
    };
    OptionAbi::new((i < n).then_some(row), TradeClosedRowAbi::empty())
}

thread_local! {
    static INPUT_BAR_AT_CALLS: RefCell<usize> = const { RefCell::new(0) };
    /// What `stub_closed_trades_len` answers: `None` (no ledger) or a row count.
    static LEDGER: RefCell<Option<usize>> = const { RefCell::new(None) };
}

/// The host side every test here drives: the recording and stub slots above. A `static` because
/// [`HostBroker::new`] holds the vtable for `'static`, as a loaded strategy file's is.
static TEST_VTABLE: BrokerVTableAbi = BrokerVTableAbi {
    position: stub_position,
    price: stub_price,
    equity: stub_equity,
    index: stub_index,
    now: stub_now,
    bars_len: stub_bars_len,
    bar_at: stub_bar_at,
    quote_vwap: stub_quote_vwap,
    depth_within_price: stub_depth_within_price,
    contract_value: stub_contract_value,
    instrument_grid: stub_instrument_grid,
    input_bars_len: stub_input_bars_len,
    input_bar_at: stub_input_bar_at,
    closed_trades_len: stub_closed_trades_len,
    closed_trade_at: stub_closed_trade_at,
    place: rec_place,
    place_exit_leg: stub_place_exit_leg,
    place_at_close: stub_place_at_close,
    place_close_from_fill: stub_place_close_from_fill,
    rests_reduce_only_while_flat: stub_rests_reduce_only_while_flat,
    place_group: stub_place_group,
    amend: stub_amend,
    cancel: stub_cancel,
    deny: stub_deny,
    average_price: stub_avg_price,
    symbol_at: stub_symbol_at,
};

/// A guest-side broker over [`TEST_VTABLE`], with no host context behind it.
fn broker() -> HostBroker {
    HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &TEST_VTABLE })
}

#[test]
fn position_reads_through_the_vtable() {
    let b = broker();
    assert_eq!(b.position("BTCUSDT"), 3.5);
}

#[test]
fn price_equity_index_now_and_the_defaulted_reads_cross_the_vtable() {
    let b = broker();
    assert_eq!(b.price("BTCUSDT"), 101.25);
    assert_eq!(b.equity(), 10_000.0);
    assert_eq!(b.index(), 7);
    assert_eq!(b.now(), 1_700_000_000_000);
    assert_eq!(b.quote_vwap("BTCUSDT", 1, 1.0), Some(42.0));
    assert_eq!(b.depth_within_price("BTCUSDT", 1, 100.0), 99.0);
    assert_eq!(
        b.contract_value("BTCUSD"),
        Some(vike_model::ContractValue::inverse(100.0)),
        "the kind crosses with the unit, so an inverse contract is not read as linear"
    );
}

/// `bars()` returns a slice, so the guest must materialise its own. Refilling on every call
/// would be O(n) per access; this pins that it refills only when the host's `index` advanced.
#[test]
fn bars_are_cached_until_the_index_advances() {
    BAR_AT_CALLS.with(|v| *v.borrow_mut() = 0);
    let b = broker();
    let first = b.bars("BTCUSDT").len();
    let calls_after_first = bar_at_calls();
    let second = b.bars("BTCUSDT").len();
    assert_eq!(first, second);
    assert_eq!(first, STUB_BAR_COUNT);
    assert_eq!(calls_after_first, STUB_BAR_COUNT, "the first fill walks the whole cursor");
    assert_eq!(bar_at_calls(), calls_after_first, "a second bars() call must not re-fetch");
}

/// Two DIFFERENT symbols cache independently — refilling one must not disturb the other's
/// already-materialised mirror (the property that makes the `UnsafeCell` behind `cache_mut!`
/// sound: see the `// SAFETY:` note at its one call site).
#[test]
fn bars_cache_independently_per_symbol() {
    BAR_AT_CALLS.with(|v| *v.borrow_mut() = 0);
    let b = broker();
    let a = b.bars("AAA").len();
    let calls_after_a = bar_at_calls();
    let c = b.bars("CCC").len();
    assert_eq!(a, STUB_BAR_COUNT);
    assert_eq!(c, STUB_BAR_COUNT);
    assert_eq!(bar_at_calls(), calls_after_a * 2, "a new symbol must still walk its own cursor");
    // Re-reading the FIRST symbol must still be cached (not disturbed by filling the second).
    let a_again = b.bars("AAA").len();
    assert_eq!(a_again, STUB_BAR_COUNT);
    assert_eq!(bar_at_calls(), calls_after_a * 2, "the first symbol must not be re-fetched");
}

/// `OrderBroker::symbols` (`ABI_VERSION` 8) is the host's universe, never the trait's empty
/// default, read once per `HostBroker` through `symbol_at` until it answers `None`, and served from
/// the copy afterwards.
#[test]
fn the_universe_crosses_once_through_symbol_at() {
    SYMBOL_AT_CALLS.with(|v| *v.borrow_mut() = 0);
    let b = broker();
    assert_eq!(b.symbols(), STUB_UNIVERSE);
    let walked = SYMBOL_AT_CALLS.with(|v| *v.borrow());
    assert_eq!(walked, STUB_UNIVERSE.len() + 1, "every symbol, then the `None` past the end");
    assert_eq!(b.symbols(), STUB_UNIVERSE);
    assert_eq!(SYMBOL_AT_CALLS.with(|v| *v.borrow()), walked, "a second read must not re-fetch");
}

// ---- ABI_VERSION 6: the input history and the closed trades, each a vtable read, never the
// trait's default (no bars, and `None`) ----

/// The declared input's bars cross; `OrderBroker::input` is their last; an undeclared name reads nothing.
#[test]
fn input_bars_cross_the_vtable_and_input_is_their_last() {
    let b = broker();
    let bars = b.input_bars(STUB_INPUT);
    assert_eq!(bars.iter().map(|x| x.timestamp).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(b.input(STUB_INPUT).map(|x| x.close), Some(12.0));
    assert!(b.input_bars("bybit:SOLUSDT:1m").is_empty());
    assert_eq!(b.input("bybit:SOLUSDT:1m"), None);
}

/// A second read at the same host count is the cached copy, no cursor walk.
#[test]
fn input_bars_are_fetched_once_while_the_host_answers_the_same_count() {
    INPUT_BAR_AT_CALLS.with(|v| *v.borrow_mut() = 0);
    let b = broker();
    assert_eq!(b.input_bars(STUB_INPUT).len(), STUB_INPUT_BARS);
    assert_eq!(b.input(STUB_INPUT).map(|x| x.timestamp), Some(2));
    assert_eq!(INPUT_BAR_AT_CALLS.with(|v| *v.borrow()), STUB_INPUT_BARS);
}

/// `None` (the broker keeps no ledger) and an empty ledger cross as two answers, and rows cross.
#[test]
fn no_ledger_is_none_and_an_empty_ledger_is_some_empty() {
    for (answer, want) in [(None, None), (Some(0), Some(vec![])), (Some(2), Some(vec![0.0, 1.0]))] {
        LEDGER.with(|l| *l.borrow_mut() = answer);
        let b = broker();
        let nets = b.closed_trades().map(|rows| rows.iter().map(|r| r.net).collect::<Vec<_>>());
        assert_eq!(nets, want, "the host answered {answer:?}");
        assert!(b.closed_trades().into_iter().flatten().all(|r| r.symbol == "BTCUSDT"));
    }
}

/// The JSON the `ABI_VERSION` 8 exports lend and take round-trips: a params bag with its `None`s,
/// a state document (a `Value::Null` one included, which is not "saves nothing"), and text that is
/// not JSON is no state.
#[test]
fn the_lent_json_round_trips() {
    let bag =
        vike_model::StrategyParams::PositionController(vike_model::PositionControllerParams::new(
            5_000,
            1.5,
            vike_model::TripleBarrier::default(),
            0.75,
        ));
    assert_eq!(strategy_params_from_json(&params_json(&bag)), Some(bag));
    for state in [serde_json::json!({"fills": 3, "pct_scale": 0.97}), serde_json::Value::Null] {
        assert_eq!(state_from_json(&state_json(&state)), Some(state));
    }
    assert_eq!(state_from_json("not json"), None);
    let names = vec!["ETHUSDT".to_string(), "a \"b\"".to_string()];
    assert_eq!(serde_json::from_str::<Vec<String>>(&names_json(&names)).ok(), Some(names));
}

/// One forwarded event: its level code, its target and its text.
type Forwarded = (u32, String, String);

thread_local! {
    static FORWARDED: RefCell<Vec<Forwarded>> = const { RefCell::new(Vec::new()) };
}

extern "C" fn capture_log(level: u32, target: StrAbi, message: StrAbi) {
    FORWARDED.with(|f| f.borrow_mut().push((level, target.read(), message.read())));
}

/// A file's `tracing` event reaches the log slot as its level, its target and ONE line of text (the
/// message, then each field as `name=value`, a text field unquoted); an event above the host's
/// level is not formatted or sent at all.
#[test]
fn a_files_tracing_event_reaches_the_log_slot_as_one_line() {
    vike_log::capture::scoped(TracingForward::new(capture_log, 2), || {
        tracing::warn!(rungs = 4, side = "bid", "quote pulled");
        tracing::info!("below the host's level");
    });
    let forwarded = FORWARDED.with(|f| f.borrow().clone());
    assert_eq!(
        forwarded,
        vec![(2, module_path!().to_string(), "quote pulled rungs=4 side=bid".to_string())]
    );
    assert_eq!(
        crate::abi::log_level_filter(crate::abi::log_filter_code(
            tracing::level_filters::LevelFilter::WARN
        )),
        tracing::level_filters::LevelFilter::WARN
    );
    assert_eq!(crate::abi::log_filter_code(tracing::level_filters::LevelFilter::OFF), 0);
}
