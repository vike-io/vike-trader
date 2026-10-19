//! The budget's arithmetic, driven with SYNTHETIC instants: nothing here reads a clock or sleeps.

use std::time::{Duration, Instant};

use vike_venues::{History, Market, Meter, rate_limits_for};

use super::ceiling::{Axis, Ceiling, SINGLE_HOST, ceilings_for, host_label, market_of};
use super::class::Class;
use super::key::{BudgetKey, Scope};
use super::ledger::Budget;
use super::line::Line;
use super::mode::{Admission, MODE, Mode};
use super::reserve::{ORDER_RESERVE_PERCENT, split};
use super::window::{RING_CAP, SlidingWindow};

/// `base` plus `secs` seconds.
fn after(base: Instant, secs: u64) -> Instant {
    base + Duration::from_secs(secs)
}

/// A history-shaped ceiling: a cap of 100 per minute, so the 20% reserve is 20 and the pool 80.
fn hundred_a_minute() -> Ceiling {
    Ceiling {
        axis: Axis::History,
        published: Some(120),
        admitted: 100,
        window: Duration::from_secs(60),
    }
}

// -- the reserve calculator ---------------------------------------------------------------------

/// A split never creates or loses a unit of a venue's budget, for any cap and any share.
#[test]
fn a_split_never_creates_or_loses_budget() {
    for cap in 0..=400u64 {
        for percent in [0, 1, 19, 20, 21, 50, 99, 100, 101, 1_000, u64::MAX] {
            let s = split(cap, percent);
            assert_eq!(s.order_reserve + s.shared_pool, cap, "cap {cap} at {percent}%");
            assert!(s.order_reserve <= cap, "the reserve never exceeds the cap: {s:?}");
        }
    }
}

/// A non-zero share of a non-zero cap reserves at least one unit: rounding goes UP, so a small
/// cap is never left with an empty order reserve by integer division.
#[test]
fn a_non_zero_share_of_a_non_zero_cap_reserves_something() {
    for cap in 1..=50u64 {
        for percent in 1..=100u64 {
            assert!(split(cap, percent).order_reserve >= 1, "cap {cap} at {percent}%");
        }
    }
}

/// The edges and two worked figures.
#[test]
fn the_split_at_its_edges_and_on_worked_figures() {
    let s = split(90, 0);
    assert_eq!((s.order_reserve, s.shared_pool), (0, 90), "0% reserves nothing");
    let s = split(90, 100);
    assert_eq!((s.order_reserve, s.shared_pool), (90, 0), "100% leaves no pool: the order meter");
    let s = split(90, 200);
    assert_eq!((s.order_reserve, s.shared_pool), (90, 0), "a share above 100 is 100");
    let s = split(0, 20);
    assert_eq!((s.order_reserve, s.shared_pool), (0, 0), "nothing to split");
    let s = split(7, 20);
    assert_eq!((s.order_reserve, s.shared_pool), (2, 5), "1.4 rounds up to 2");
    let s = split(4800, ORDER_RESERVE_PERCENT);
    assert_eq!((s.order_reserve, s.shared_pool), (960, 3840), "binance spot's soft limit");
}

// -- the sliding window -------------------------------------------------------------------------

/// The window is half-open: an entry exactly one window old has left it.
#[test]
fn a_window_sums_what_is_inside_it_and_drops_what_left() {
    let t0 = Instant::now();
    let mut w = SlidingWindow::new(Duration::from_secs(10));
    w.add(t0, 3);
    w.add(after(t0, 5), 4);
    assert_eq!(w.used(after(t0, 9)), 7);
    assert_eq!(w.used(after(t0, 10)), 4, "the first entry is exactly a window old: it left");
    assert_eq!(w.used(after(t0, 14)), 4);
    assert_eq!(w.used(after(t0, 15)), 0, "an idle window reads zero");
    assert_eq!(w.entries(), 0, "and holds nothing");
}

/// A zero cost is not an event.
#[test]
fn a_zero_cost_adds_nothing() {
    let t0 = Instant::now();
    let mut w = SlidingWindow::new(Duration::from_secs(10));
    w.add(t0, 0);
    assert_eq!((w.used(t0), w.entries()), (0, 0));
}

/// Shadow mode refuses nothing, so a runaway caller must not grow the ring without bound — and the
/// merged entries must keep the SUM exact.
#[test]
fn the_ring_is_bounded_and_the_sum_exact_under_a_flood() {
    let t0 = Instant::now();
    let mut w = SlidingWindow::new(Duration::from_secs(10));
    let flood = RING_CAP + 1_000;
    for _ in 0..flood {
        w.add(t0, 1);
    }
    assert_eq!(w.entries(), RING_CAP, "the ring stays at its cap");
    assert_eq!(w.used(t0), u64::try_from(flood).expect("fits"), "and the sum is exact");
    assert_eq!(w.used(after(t0, 10)), 0, "a merged flood still leaves the window");
}

/// A thread may hand in a slightly stale instant; it must not reorder the ring, or front-expiry
/// would keep a newer entry's company too long (or drop it too soon).
#[test]
fn a_stale_instant_is_clamped_and_never_reorders_the_ring() {
    let t0 = Instant::now();
    let mut w = SlidingWindow::new(Duration::from_secs(10));
    w.add(after(t0, 5), 1);
    w.add(t0, 1); // older than the newest entry: counted at the newest instant
    assert_eq!(w.used(after(t0, 5)), 2);
    assert_eq!(w.used(after(t0, 14)), 2, "both are 9 s old");
    assert_eq!(w.used(after(t0, 15)), 0, "both left together");
}

// -- the ceilings, read from the venue table ----------------------------------------------------

/// `got` is the ceiling the meter must produce: none for an ungated meter, the row's own numbers
/// otherwise.
fn check_meter(label: &str, meter: &Meter, got: Option<&Ceiling>) {
    match (meter, got) {
        (Meter::Ungated, None) => {}
        (Meter::Ungated, Some(c)) => panic!("{label}: an ungated meter produced {c:?}"),
        (_, None) => panic!("{label}: a gated meter produced no ceiling"),
        (meter, Some(c)) => {
            assert_eq!(c.admitted, u64::try_from(meter.admitted()).expect("fits"), "{label}");
            assert_eq!(c.window, meter.window(), "{label}");
            assert_eq!(
                c.published,
                meter.published_cap().map(|p| u64::try_from(p).expect("fits")),
                "{label}: a venue-published cap is carried, an own brake is not passed off as one"
            );
        }
    }
}

/// For EVERY roster venue and market, each ceiling is exactly the row's number and an undeclared
/// meter yields none — and the walk is not vacuous: every axis is produced by some venue.
#[test]
fn every_ceiling_is_the_venue_tables_own_number() {
    let mut seen = [false; 4];
    for venue in vike_model::VENUES {
        for market in [Market::Spot, Market::Perp] {
            let limits = rate_limits_for(venue, market);
            let ceilings = ceilings_for(venue, market);
            let find = |axis| ceilings.iter().find(|c| c.axis == axis);
            let label = |what: &str| format!("{venue} {market:?} {what}");
            check_meter(&label("orders"), &limits.orders, find(Axis::Orders));
            check_meter(&label("ws_sends"), &limits.ws_sends, find(Axis::WsSends));
            check_meter(&label("rest_ip_weight"), &limits.rest_ip_weight, find(Axis::RestIpWeight));
            match (limits.history, find(Axis::History)) {
                (History::Weighted { ceiling_per_min, soft_limit, .. }, Some(c)) => {
                    assert_eq!(c.published, Some(ceiling_per_min), "{venue} {market:?} history");
                    assert_eq!(c.admitted, soft_limit, "{venue} {market:?} history");
                    assert_eq!(c.window, Duration::from_secs(60));
                }
                (History::Unweighted { .. } | History::NotPaged, None) => {}
                (history, got) => panic!("{venue} {market:?}: history {history:?} gave {got:?}"),
            }
            for c in &ceilings {
                assert!(c.admitted > 0 && !c.window.is_zero(), "{venue}: {c:?} is usable");
                assert!(c.published.is_none_or(|p| c.admitted <= p), "{venue}: {c:?} under cap");
                let i = Axis::ALL.iter().position(|a| *a == c.axis).expect("a known axis");
                seen[i] = true;
            }
        }
    }
    assert_eq!(seen, [true; 4], "some roster venue declares every axis: the walk saw all four");
}

/// A venue string the table does not know creates no counter, so the wire cannot grow the ledger.
#[test]
fn an_unknown_venue_declares_no_ceiling() {
    assert!(ceilings_for("nosuchvenue", Market::Spot).is_empty());
}

/// A host label is the market word exactly where the two rows differ — binance and aster — and one
/// label elsewhere.
#[test]
fn the_host_is_the_market_only_where_the_table_splits_it() {
    for venue in ["binance", "aster"] {
        assert_eq!(host_label(venue, Market::Spot), "spot", "{venue}");
        assert_eq!(host_label(venue, Market::Perp), "perp", "{venue}");
    }
    for venue in vike_model::VENUES.iter().filter(|v| !["binance", "aster"].contains(v)) {
        for market in [Market::Spot, Market::Perp] {
            assert_eq!(host_label(venue, market), SINGLE_HOST, "{venue} {market:?}");
        }
    }
}

/// The market of a symbol is its `.P` suffix at a suffix venue and spot everywhere else.
#[test]
fn the_market_of_a_symbol_is_its_perp_suffix_at_a_suffix_venue() {
    assert_eq!(market_of("binance", "BTCUSDT.P"), Market::Perp);
    assert_eq!(market_of("binance", "BTCUSDT"), Market::Spot);
    assert_eq!(market_of("aster", "BTCUSDT.P"), Market::Perp);
    assert_eq!(market_of("okx", "BTC-USDT-SWAP"), Market::Spot, "a suffix-free venue is one host");
    assert_eq!(market_of("hyperliquid", "BTC.P"), Market::Spot, "no suffix convention there");
}

// -- one line: classes, the split, the detector -------------------------------------------------

/// A read draws the shared pool alone: it may not touch the order reserve.
#[test]
fn reads_draw_the_shared_pool_and_cannot_touch_the_reserve() {
    let t0 = Instant::now();
    let mut line = Line::new(hundred_a_minute());
    let key = BudgetKey::new("binance", "perp", Scope::Ip);
    for class in [Class::Live, Class::Interactive, Class::Bulk] {
        let seen = line.spend(class, 20, t0, Mode::Shadow);
        assert!(!seen.over, "{class:?}: 20 of an 80 pool");
    }
    let snap = line.snapshot(&key, t0);
    assert_eq!((snap.order_reserve, snap.shared_pool), (20, 80));
    assert_eq!((snap.order_used, snap.shared_used), (0, 60));
    let seen = line.spend(Class::Bulk, 21, t0, Mode::Shadow);
    assert!(seen.over, "81 of an 80 pool is over although 20 units of reserve sit idle");
}

/// An order inside the reserve is never over, however busy the pool is.
#[test]
fn an_order_inside_the_reserve_never_waits_behind_reads() {
    let t0 = Instant::now();
    let mut line = Line::new(hundred_a_minute());
    let key = BudgetKey::new("binance", "perp", Scope::Ip);
    // Shadow mode COUNTS an over-budget read, so the pool can be driven far past its size.
    assert!(line.spend(Class::Bulk, 500, t0, Mode::Shadow).over);
    let seen = line.spend(Class::Order, 20, t0, Mode::Shadow);
    assert!(!seen.over, "20 units fit the reserve");
    assert_eq!(line.snapshot(&key, t0).order_used, 20);
    assert!(line.spend(Class::Order, 1, t0, Mode::Shadow).over, "the 21st spills into a full pool");
}

/// An order larger than the reserve spends the reserve and the rest of its cost from the pool.
#[test]
fn an_order_overflow_spills_into_the_pool() {
    let t0 = Instant::now();
    let mut line = Line::new(hundred_a_minute());
    let key = BudgetKey::new("binance", "perp", Scope::Ip);
    let seen = line.spend(Class::Order, 30, t0, Mode::Shadow);
    assert!(!seen.over);
    let snap = line.snapshot(&key, t0);
    assert_eq!((snap.order_used, snap.shared_used, snap.counted), (20, 10, 30));
}

/// The order meter is ALL reserve: an order up to the cap is fine, and a read spending it is the
/// caller's mistake, which the detector discloses rather than hides.
#[test]
fn the_order_meter_is_all_reserve() {
    let t0 = Instant::now();
    let ceiling = Ceiling {
        axis: Axis::Orders,
        published: Some(100),
        admitted: 90,
        window: Duration::from_secs(10),
    };
    let mut line = Line::new(ceiling);
    assert!(!line.spend(Class::Order, 90, t0, Mode::Shadow).over);
    assert!(line.spend(Class::Order, 1, t0, Mode::Shadow).over, "the 91st order in the window");
    let mut line = Line::new(ceiling);
    assert!(line.spend(Class::Bulk, 1, t0, Mode::Shadow).over, "a read has no pool on this meter");
}

/// ONE announcement per line per window, however fast the over-requests come; the next window
/// announces again.
#[test]
fn the_detector_warns_once_per_line_per_window() {
    let t0 = Instant::now();
    let mut line = Line::new(hundred_a_minute());
    assert!(!line.spend(Class::Bulk, 80, t0, Mode::Shadow).announce, "within the pool: silent");
    let first = line.spend(Class::Bulk, 1, t0, Mode::Shadow);
    assert!(first.over && first.announce, "the first over-request announces");
    for secs in [1, 2, 30, 59] {
        let seen = line.spend(Class::Bulk, 100, after(t0, secs), Mode::Shadow);
        assert!(seen.over, "still over at +{secs}s");
        assert!(!seen.announce, "…but already announced at +{secs}s");
    }
    let again = line.spend(Class::Bulk, 100, after(t0, 60), Mode::Shadow);
    assert!(again.over && again.announce, "a whole window later it announces again");
    assert_eq!(
        line.snapshot(&BudgetKey::new("v", "all", Scope::Ip), after(t0, 60)).would_throttle,
        6
    );
}

/// Shadow admits and counts everything; enforcement (never built in) refuses and does NOT count.
#[test]
fn shadow_counts_what_enforcement_would_refuse() {
    let t0 = Instant::now();
    let key = BudgetKey::new("binance", "perp", Scope::Ip);

    let mut shadow = Line::new(hundred_a_minute());
    shadow.spend(Class::Bulk, 80, t0, Mode::Shadow);
    let over = shadow.spend(Class::Bulk, 5, t0, Mode::Shadow);
    assert_eq!(over.admission, Admission::Admit, "shadow refuses nothing");
    let snap = shadow.snapshot(&key, t0);
    assert_eq!((snap.counted, snap.shared_used, snap.would_throttle), (85, 85, 1));

    let mut enforce = Line::new(hundred_a_minute());
    enforce.spend(Class::Bulk, 80, t0, Mode::Enforce);
    let over = enforce.spend(Class::Bulk, 5, t0, Mode::Enforce);
    assert_eq!(over.admission, Admission::Refuse);
    let snap = enforce.snapshot(&key, t0);
    assert_eq!((snap.counted, snap.shared_used, snap.would_throttle), (80, 80, 1));
}

// -- the ledger ---------------------------------------------------------------------------------

/// THE wave-1 pin: this build refuses nothing. Flipping [`MODE`] to enforcement fails here first,
/// so the change cannot arrive inside an unrelated diff.
#[test]
fn wave_one_refuses_nothing() {
    assert_eq!(MODE, Mode::Shadow);
    let t0 = Instant::now();
    let budget = Budget::new();
    // binance perp history: soft limit 2000 a minute. Spend five times that.
    for _ in 0..5 {
        let verdict = budget.spend("binance", Market::Perp, Axis::History, Class::Bulk, 2_000, t0);
        assert_eq!(verdict, Admission::Admit);
    }
    let snap = budget.snapshot(t0);
    assert_eq!(snap.mode, Mode::Shadow);
    let line = &snap.lines[0];
    assert!(line.would_throttle >= 1, "the over-spend was disclosed: {line:?}");
    assert_eq!(line.counted, 10_000, "…and counted in full");
}

/// An axis the venue's row declares no ceiling for is counted as unmetered and admitted; so is a
/// venue the table does not know. Neither creates a line.
#[test]
fn an_unmetered_axis_is_counted_and_admitted() {
    let t0 = Instant::now();
    let budget = Budget::new();
    // binance's REQUEST_WEIGHT is declared under `history`; `rest_ip_weight` is ungated.
    let a = budget.spend("binance", Market::Spot, Axis::RestIpWeight, Class::Bulk, 5, t0);
    let b = budget.spend("nosuchvenue", Market::Spot, Axis::Orders, Class::Order, 1, t0);
    assert_eq!((a, b), (Admission::Admit, Admission::Admit));
    let snap = budget.snapshot(t0);
    assert_eq!(snap.unmetered, 2);
    assert!(snap.lines.is_empty(), "no counter was invented: {snap:?}");
}

/// Lines are keyed by venue, host and scope, so the two binance hosts and the three scopes of one
/// venue are separate counters.
#[test]
fn lines_are_keyed_by_venue_host_and_scope() {
    let t0 = Instant::now();
    let budget = Budget::new();
    budget.spend("binance", Market::Spot, Axis::History, Class::Bulk, 10, t0);
    budget.spend("binance", Market::Perp, Axis::History, Class::Bulk, 10, t0);
    budget.spend("binance", Market::Perp, Axis::Orders, Class::Order, 1, t0);
    budget.spend("binance", Market::Perp, Axis::WsSends, Class::Live, 1, t0);
    let keys: Vec<String> = budget
        .snapshot(t0)
        .lines
        .iter()
        .map(|l| format!("{}:{}", l.key, l.axis.as_str()))
        .collect();
    // The ledger is sorted by (venue, host, scope, axis), and `Scope` orders Ip, Account, Key,
    // Connection.
    assert_eq!(
        keys,
        [
            "binance/perp/ip:history",
            "binance/perp/account:orders",
            "binance/perp/connection:ws_sends",
            "binance/spot/ip:history",
        ],
        "four lines, one per (venue, host, scope, axis)"
    );
}

/// The snapshot reads the window as of ITS instant: an idle line reads zero but keeps its total.
#[test]
fn the_snapshot_reads_the_window_as_of_its_instant() {
    let t0 = Instant::now();
    let budget = Budget::new();
    budget.spend("binance", Market::Perp, Axis::History, Class::Bulk, 100, t0);
    assert_eq!(budget.snapshot(t0).lines[0].shared_used, 100);
    let later = budget.snapshot(after(t0, 61));
    assert_eq!(later.lines[0].shared_used, 0, "the minute rolled over");
    assert_eq!(later.lines[0].counted, 100, "the total does not");
}

/// The snapshot is a plain value a status verb can serialise, and it carries no credential: only
/// venue words, counters and the mode.
#[test]
fn the_snapshot_serialises_as_a_status_value() {
    let t0 = Instant::now();
    let budget = Budget::new();
    budget.spend("binance", Market::Perp, Axis::History, Class::Bulk, 10, t0);
    let value = serde_json::to_value(budget.snapshot(t0)).expect("a snapshot serialises");
    assert_eq!(value["mode"], "shadow");
    assert_eq!(value["unmetered"], 0);
    let line = &value["lines"][0];
    assert_eq!(line["key"]["venue"], "binance");
    assert_eq!(line["key"]["host"], "perp");
    assert_eq!(line["key"]["scope"], "ip");
    assert_eq!(line["axis"], "history");
    assert_eq!(line["window_ms"], 60_000);
    assert_eq!(line["shared_used"], 10);
}

/// The ledger is shared across the daemon's threads.
#[test]
fn the_ledger_is_shareable_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Budget>();
}
