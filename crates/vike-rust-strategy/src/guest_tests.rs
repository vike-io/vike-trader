use super::*;
use crate::abi::{
    GridOptionAbi, LentBarAbi, LentTradeClosedRowAbi, NameOrIndexAbi, OptionAbi, OrderPlacementAbi,
    StrAbi,
};
use std::cell::RefCell;
use vike_marketdata::Bar;
use vike_model::{OrderPlacement, TradeClosedRow};

thread_local! {
    static PLACES: RefCell<Vec<Placed>> = const { RefCell::new(Vec::new()) };
    static BARS_CALLS: RefCell<usize> = const { RefCell::new(0) };
    static SYMBOL_AT_CALLS: RefCell<usize> = const { RefCell::new(0) };
    /// The bars [`stub_bars`] lends, as a host keeps its own.
    static STUB_BARS: Vec<Bar> = (0..STUB_BAR_COUNT)
        .map(|i| vike_marketdata::test_support::flat_bar_zero_volume(i as i64, 1.0))
        .collect();
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

/// `ABI_VERSION` 15: a read by index answers `1000 + index`, one by text 3.5, so a test sees which crossed.
extern "C" fn stub_position(_c: *mut core::ffi::c_void, s: NameOrIndexAbi) -> f64 {
    s.get().map_or(3.5, |index| 1000.0 + index as f64)
}
host_stub!(stub_price(NameOrIndexAbi) -> f64 => 101.25);
host_stub!(stub_equity() -> f64 => 10_000.0);
host_stub!(stub_index() -> usize => 7);
host_stub!(stub_now() -> i64 => 1_700_000_000_000);
host_stub!(stub_quote_vwap(*const u8, usize, i32, f64) -> crate::abi::OptionAbi<f64> => Some(42.0).into());
host_stub!(stub_depth_within_price(*const u8, usize, i32, f64) -> f64 => 99.0);
host_stub!(stub_contract_value(NameOrIndexAbi) -> crate::abi::ContractValueOptionAbi =>
    Some(vike_model::ContractValue::inverse(100.0)).into());
extern "C" fn stub_bars(_c: *mut core::ffi::c_void, _s: NameOrIndexAbi) -> SliceAbi<LentBarAbi> {
    BARS_CALLS.with(|v| *v.borrow_mut() += 1);
    STUB_BARS.with(|bars| LentBarAbi::lend_all(bars))
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
host_stub!(stub_instrument_grid(NameOrIndexAbi) -> GridOptionAbi => None.into());
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
host_stub!(stub_history_only_grows() -> bool => false);
host_stub!(stub_place_group(*const u8, usize, SliceAbi<crate::abi::OrderListLegAbi>));
host_stub!(stub_amend(*const u8, usize, OptionAbi<f64>, OptionAbi<f64>, OptionAbi<f64>));
host_stub!(stub_cancel(*const u8, usize));
host_stub!(stub_avg_price(NameOrIndexAbi) -> f64 => 0.0);
host_stub!(stub_deny(*const u8, usize, *const u8, usize, f64, *const u8, usize));
host_stub!(stub_publish_forecast(*const u8, usize, f64, i64, OptionAbi<f64>));
extern "C" fn stub_symbol_at(_c: *mut core::ffi::c_void, i: usize) -> OptionAbi<StrAbi> {
    SYMBOL_AT_CALLS.with(|v| *v.borrow_mut() += 1);
    OptionAbi::<StrAbi>::of(STUB_UNIVERSE.get(i).copied())
}
/// `ABI_VERSION` 15: the host's input list is [`STUB_UNSERVED`] then [`STUB_INPUT`].
extern "C" fn stub_input_name_at(_c: *mut core::ffi::c_void, i: usize) -> OptionAbi<StrAbi> {
    OptionAbi::<StrAbi>::of([STUB_UNSERVED, STUB_INPUT].get(i).copied())
}

// ---- ABI_VERSION 6 stubs: the input [`STUB_INPUT`] holds [`STUB_INPUT_BARS`] bars and every other
// name none; the ledger is what [`LEDGER`] says. Their round trip over the real host vtable is
// `host_tests.rs`'s `the_guest_reads_the_hosts_own_memory`.

const STUB_INPUT: &str = "bybit:ETHUSDT:1m";
const STUB_INPUT_BARS: usize = 3;
/// `ABI_VERSION` 10: the input [`STUB_INPUT`]'s whole series holds [`STUB_WHOLE_BARS`] bars (more than
/// have closed), [`STUB_UNSERVED`] is served none, and every other name an empty series.
const STUB_WHOLE_BARS: usize = 5;
const STUB_UNSERVED: &str = "bybit:XRPUSDT:1m";

thread_local! {
    /// The input's whole series; its first [`STUB_INPUT_BARS`] have closed.
    static STUB_WHOLE: Vec<Bar> = (0..STUB_WHOLE_BARS)
        .map(|i| vike_marketdata::test_support::flat_bar_zero_volume(i as i64, 10.0 + i as f64))
        .collect();
    /// The rows [`stub_closed_trades`] lends the first [`LEDGER`] of.
    static STUB_ROWS: Vec<TradeClosedRow> = (0..2)
        .map(|i| TradeClosedRow {
            symbol: "BTCUSDT".to_string(),
            tag: String::new(),
            side: 1,
            entry_timestamp: 0,
            entry_bar: 0,
            entry_price: 0.0,
            exit_timestamp: 0,
            exit_bar: 0,
            exit_price: 0.0,
            qty: 0.0,
            pnl: 0.0,
            commission: 0.0,
            net: f64::from(i),
        })
        .collect();
    /// What `stub_closed_trades` answers: `None` (no ledger) or a row count.
    static LEDGER: RefCell<Option<usize>> = const { RefCell::new(None) };
}

/// The input a read names: by text, or by index into [`stub_input_name_at`]'s list.
fn input_named(s: NameOrIndexAbi) -> String {
    match s.get() {
        Some(index) => [STUB_UNSERVED, STUB_INPUT][index].to_string(),
        None => read_symbol(s.text.ptr, s.text.len),
    }
}
extern "C" fn stub_input_bars(
    _c: *mut core::ffi::c_void,
    s: NameOrIndexAbi,
) -> SliceAbi<LentBarAbi> {
    let closed = if input_named(s) == STUB_INPUT { STUB_INPUT_BARS } else { 0 };
    STUB_WHOLE.with(|whole| LentBarAbi::lend_all(&whole[..closed]))
}
extern "C" fn stub_input_whole(
    _c: *mut core::ffi::c_void,
    s: NameOrIndexAbi,
) -> OptionAbi<SliceAbi<LentBarAbi>> {
    let name = input_named(s);
    let len = if name == STUB_INPUT { STUB_WHOLE_BARS } else { 0 };
    let whole = STUB_WHOLE.with(|whole| LentBarAbi::lend_all(&whole[..len]));
    OptionAbi::new((name != STUB_UNSERVED).then_some(whole), SliceAbi::empty())
}
extern "C" fn stub_closed_trades(
    _c: *mut core::ffi::c_void,
) -> OptionAbi<SliceAbi<LentTradeClosedRowAbi>> {
    let lend = |n: usize| STUB_ROWS.with(|r| LentTradeClosedRowAbi::lend_all(&r[..n]));
    let rows = LEDGER.with(|l| *l.borrow()).map(lend);
    OptionAbi::new(rows, SliceAbi::empty())
}

/// The host side every test here drives: the recording and stub slots above. A `static` because
/// [`HostBroker::new`] holds the vtable for `'static`, as a loaded strategy file's is.
static TEST_VTABLE: BrokerVTableAbi = BrokerVTableAbi {
    position: stub_position,
    price: stub_price,
    equity: stub_equity,
    index: stub_index,
    now: stub_now,
    bars: stub_bars,
    quote_vwap: stub_quote_vwap,
    depth_within_price: stub_depth_within_price,
    contract_value: stub_contract_value,
    instrument_grid: stub_instrument_grid,
    input_bars: stub_input_bars,
    closed_trades: stub_closed_trades,
    input_whole: stub_input_whole,
    history_only_grows: stub_history_only_grows,
    place: rec_place,
    place_exit_leg: stub_place_exit_leg,
    place_at_close: stub_place_at_close,
    place_close_from_fill: stub_place_close_from_fill,
    rests_reduce_only_while_flat: stub_rests_reduce_only_while_flat,
    place_group: stub_place_group,
    amend: stub_amend,
    cancel: stub_cancel,
    deny: stub_deny,
    publish_forecast: stub_publish_forecast,
    average_price: stub_avg_price,
    symbol_at: stub_symbol_at,
    input_name_at: stub_input_name_at,
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

/// `ABI_VERSION` 15: a read by index crosses its index, never a text, and a read by name its text; the
/// input list is the host's, read once a dispatch through `input_name_at`, and an input read by its index
/// is the read of that name.
#[test]
fn a_read_by_index_crosses_the_index_and_the_input_list_is_the_hosts() {
    let b = broker();
    assert_eq!((b.position_at(1), b.position("BTCUSDT")), (1001.0, 3.5));
    assert_eq!(b.symbol_index("ETHUSDT"), Some(1), "the host's universe");
    assert_eq!(b.input_names(), [STUB_UNSERVED, STUB_INPUT]);
    assert_eq!(b.input_index(STUB_INPUT), Some(1));
    assert_eq!(
        b.input_index("bybit:SOLUSDT:1m"),
        None,
        "an input the host does not list reads by name"
    );
    let (by_index, by_name) = (b.input_bars_at(1), b.input_bars(STUB_INPUT));
    assert_eq!((by_index.as_ptr(), by_index.len()), (by_name.as_ptr(), STUB_INPUT_BARS));
    assert_eq!(b.input_whole_at(0), None, "the unserved input by its index");
    assert_eq!(b.input_whole_at(1).map(<[Bar]>::len), Some(STUB_WHOLE_BARS));
}

/// An index of `usize::MAX` (`symbols().len() - 1` on an empty universe, wrapped in release) is refused
/// before it crosses: crossed, it is the by-text marker with an empty name, which the host reads as the
/// mount's OWN symbol. It panics inside the dispatch's `catch_unwind` (the file answers `Panicked`).
#[test]
fn an_index_that_wrapped_is_refused_not_read_as_the_mounts_own_symbol() {
    let b = broker();
    let wrapped = 0_usize.wrapping_sub(1);
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| b.price_at(wrapped)));
    assert!(read.is_err(), "a wrapped index must be refused, not answered {read:?}");
    assert_eq!(b.price_at(1), 101.25, "an ordinary index still reads");
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

/// `ABI_VERSION` 14: every history read is the host's own slice, asked again on every call (one vtable
/// call each, nothing kept), so a host whose rows moved between two reads is read where they are now.
#[test]
fn every_read_is_the_hosts_slice_one_call_each() {
    BARS_CALLS.with(|v| *v.borrow_mut() = 0);
    let b = broker();
    let host = STUB_BARS.with(|bars| bars.as_ptr());
    assert_eq!(b.bars("BTCUSDT").as_ptr(), host, "the host's bars, not a copy");
    assert_eq!(b.bars("BTCUSDT").len(), STUB_BAR_COUNT);
    assert_eq!(b.bars("ETHUSDT").as_ptr(), host);
    assert_eq!(BARS_CALLS.with(|v| *v.borrow()), 3, "one call a read");
    let whole = STUB_WHOLE.with(|whole| whole.as_ptr());
    assert_eq!(b.input_bars(STUB_INPUT).as_ptr(), whole);
    assert_eq!(b.input_whole(STUB_INPUT).map(<[Bar]>::as_ptr), Some(whole));
    LEDGER.with(|l| *l.borrow_mut() = Some(2));
    let rows = STUB_ROWS.with(|rows| rows.as_ptr());
    assert_eq!(b.closed_trades().map(<[TradeClosedRow]>::as_ptr), Some(rows));
}

/// A lent slice with rows at a null or misaligned pointer is a contract violation, refused loudly (the
/// template's `catch_unwind` reports it), never read; no rows is `&[]` whatever the pointer.
#[test]
fn a_null_or_misaligned_lent_slice_is_a_contract_violation() {
    let b = broker();
    let null = SliceAbi::<Bar> { ptr: std::ptr::null(), len: 2 };
    let misaligned = SliceAbi::<Bar> { ptr: std::ptr::without_provenance(1), len: 1 };
    for refused in [null, misaligned] {
        let read =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lent(&b, refused).len()));
        assert!(read.is_err(), "a slice at {:p} must be refused", refused.ptr);
    }
    assert!(lent(&b, SliceAbi::<Bar> { ptr: std::ptr::null(), len: 0 }).is_empty());
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

/// `ABI_VERSION` 10: an input's whole series crosses the vtable, bars still to come included; `None` (no
/// whole series served) and an empty series cross as two answers, never the trait's `None` for both.
#[test]
fn the_whole_series_crosses_the_vtable_and_none_is_not_empty() {
    let b = broker();
    let whole = b.input_whole(STUB_INPUT).expect("served");
    assert_eq!(whole.iter().map(|x| x.timestamp).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
    assert_eq!(b.input_bars(STUB_INPUT).len(), STUB_INPUT_BARS, "the as-of read is unchanged");
    assert_eq!(b.input_whole(STUB_INPUT).map(<[Bar]>::len), Some(STUB_WHOLE_BARS));
    assert_eq!(b.input_whole("bybit:SOLUSDT:1m").map(<[Bar]>::len), Some(0), "served, empty");
    assert_eq!(b.input_whole(STUB_UNSERVED), None, "not served");
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
    // `ABI_VERSION` 15: a state crosses as its msgpack bytes, read back as they are; bytes that are not
    // one msgpack value are no state.
    for state in [serde_json::json!({"fills": 3, "pct_scale": 0.97}), serde_json::Value::Null] {
        let state = vike_model::StrategyState::from_json_value(&state);
        assert_eq!(state_from_bytes(SliceAbi::of(state.bytes())), Some(state));
    }
    assert_eq!(state_from_bytes(SliceAbi::of(b"not msgpack \xc1")), None);
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
