use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use chrono::{FixedOffset, NaiveDate};
use vike_datahub_client::tool_feeds::{
    CalEvent as WireEvent, DividendRow, EarningsRow, EconomicCalendarReply, EquityCalendarReply,
    IpoRow, NewsItem, NewsReply, ToolFeedOutcome, ToolFeedRefusal,
};

use super::super::calendar::Importance;
use super::super::tool_feeds_wire::{FeedAsk, FeedReply, FeedsPass};
use super::*;
use crate::data::catalog_wire::NO_BACKEND_NOTE;

const ADDR: &str = "<host>:7879";
const WEEK: &str = "2026-10-12";
const NOW_MS: i64 = 1_760_000_000_000;

fn clock(now: Instant) -> FeedsClock {
    FeedsClock { now, now_ms: NOW_MS, today: NaiveDate::from_ymd_opt(2026, 10, 14).unwrap() }
}

fn snap<T>(rows: Vec<T>, generation: u64, ok: u32, total: u32) -> ToolFeedOutcome<T> {
    // `cadence_secs: 0` is a server that says nothing: the floors decide, as before the field.
    snap_on(rows, generation, ok, total, 0)
}

/// A snapshot fetched a minute ago from a server that reports `cadence_secs`.
fn snap_on<T>(
    rows: Vec<T>,
    generation: u64,
    ok: u32,
    total: u32,
    cadence_secs: u32,
) -> ToolFeedOutcome<T> {
    ToolFeedOutcome::Snapshot {
        rows,
        fetched_at_ms: NOW_MS - 60_000,
        generation,
        sources_ok: ok,
        sources_total: total,
        cadence_secs,
    }
}

fn headlines(n: usize) -> Vec<NewsItem> {
    (0..n)
        .map(|i| NewsItem {
            source: "CoinDesk".into(),
            title: format!("headline {i}"),
            summary: String::new(),
            url: String::new(),
            market: "crypto".into(),
            tags: Vec::new(),
            ts_ms: NOW_MS - i as i64,
        })
        .collect()
}

fn earning(date: &str) -> EarningsRow {
    EarningsRow {
        date: date.into(),
        symbol: "AAPL".into(),
        hour: "Pre-mkt".into(),
        eps_est: "1.00".into(),
        eps_act: String::new(),
        surprise: String::new(),
    }
}

fn dividend(date: &str) -> DividendRow {
    DividendRow {
        date: date.into(),
        symbol: "KO".into(),
        pay_date: String::new(),
        amount: "0.50 $".into(),
        yield_pct: String::new(),
        freq: String::new(),
    }
}

fn ipo(date: &str) -> IpoRow {
    IpoRow {
        date: date.into(),
        symbol: "NEW".into(),
        company: "New Co".into(),
        exchange: "NASDAQ".into(),
        price: String::new(),
        shares: String::new(),
        status: "Upcoming".into(),
    }
}

fn event(ts_ms: i64) -> WireEvent {
    WireEvent {
        ts_ms,
        currency: "USD".into(),
        title: "CPI".into(),
        impact: "High".into(),
        forecast: "0.3%".into(),
        previous: "0.2%".into(),
        actual: String::new(),
    }
}

fn news_ask(g: Option<u64>) -> FeedAsk {
    FeedAsk::News { if_generation: g }
}

fn equity_ask(g: Option<u64>) -> FeedAsk {
    FeedAsk::Equity { week_start: WEEK.into(), if_generation: g }
}

fn asked(answers: Vec<(FeedAsk, Result<FeedReply, String>)>) -> FeedsPass {
    FeedsPass::Asked { addr: ADDR.into(), answers }
}

fn news_pass(g: Option<u64>, outcome: ToolFeedOutcome<NewsItem>) -> (Vec<FeedAsk>, FeedsPass) {
    let ask = news_ask(g);
    (vec![ask.clone()], asked(vec![(ask, Ok(FeedReply::News(NewsReply { outcome })))]))
}

fn equity_pass(
    earnings: ToolFeedOutcome<EarningsRow>,
    dividends: ToolFeedOutcome<DividendRow>,
    ipos: ToolFeedOutcome<IpoRow>,
) -> (Vec<FeedAsk>, FeedsPass) {
    let ask = equity_ask(None);
    let reply = FeedReply::Equity(EquityCalendarReply { earnings, dividends, ipos });
    (vec![ask.clone()], asked(vec![(ask, Ok(reply))]))
}

fn merge_news(
    st: &mut FeedsState,
    td: &mut ToolData,
    now: Instant,
    pass: (Vec<FeedAsk>, FeedsPass),
) {
    st.merge(td, &pass.0, pass.1, &clock(now));
}

// ---- the link --------------------------------------------------------------------------------

#[test]
fn a_fresh_interest_wakes_the_thread_and_a_live_one_does_not() {
    let (tx, rx) = channel::<()>();
    let link = ToolFeedsLink::new(tx);
    let t0 = Instant::now();
    link.want_at(FeedInterest::News, t0);
    assert!(rx.try_recv().is_ok(), "a new interest wakes the thread");
    link.want_at(FeedInterest::News, t0 + Duration::from_secs(10));
    assert!(rx.try_recv().is_err(), "a renewed live interest does not");
    assert_eq!(link.live(t0 + Duration::from_secs(10)), [FeedInterest::News]);
}

#[test]
fn an_interest_lapses_after_its_ttl_and_wakes_again_when_it_returns() {
    let (tx, rx) = channel::<()>();
    let link = ToolFeedsLink::new(tx);
    let t0 = Instant::now();
    link.want_at(FeedInterest::Calendar, t0);
    let _ = rx.try_recv();
    assert!(link.live(t0 + INTEREST_TTL).is_empty(), "a closed window is forgotten");
    link.want_at(FeedInterest::Calendar, t0 + INTEREST_TTL + Duration::from_secs(1));
    assert!(rx.try_recv().is_ok(), "a window reopened after the lapse wakes the thread");
}

#[test]
fn a_refresh_is_remembered_once_and_wakes_the_thread() {
    let (tx, rx) = channel::<()>();
    let link = ToolFeedsLink::new(tx);
    assert!(!link.take_refresh());
    link.refresh();
    assert!(rx.try_recv().is_ok());
    assert!(link.take_refresh(), "the press is seen by the next pass");
    assert!(!link.take_refresh(), "and only by that one");
    // a closed channel (shutdown) never panics the UI
    drop(rx);
    link.refresh();
}

// ---- the schedule ----------------------------------------------------------------------------

#[test]
fn nothing_is_planned_without_an_interest_and_each_interest_plans_its_feeds() {
    let st = FeedsState::default();
    let t0 = Instant::now();
    assert!(st.plan(&[], false, WEEK, t0).is_empty());
    assert_eq!(st.plan(&[FeedInterest::News], false, WEEK, t0), [news_ask(None)]);
    assert_eq!(
        st.plan(&[FeedInterest::Calendar], false, WEEK, t0),
        [FeedAsk::Economic { if_generation: None }, equity_ask(None)],
        "the day-strip counts need the economic AND the equity feeds"
    );
}

#[test]
fn an_asked_feed_is_not_asked_again_until_its_minute_is_up_unless_forced() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(2), 7, 16, 16)));
    assert!(st.plan(&[FeedInterest::News], false, WEEK, t0 + Duration::from_secs(30)).is_empty());
    assert_eq!(
        st.plan(&[FeedInterest::News], false, WEEK, t0 + POLL_EVERY),
        [news_ask(Some(7))],
        "due after a minute, and the held generation rides along"
    );
    assert_eq!(
        st.plan(&[FeedInterest::News], true, WEEK, t0 + Duration::from_secs(1)),
        [news_ask(Some(7))],
        "the Refresh pill does not wait"
    );
}

#[test]
fn a_feed_still_warming_on_the_server_is_asked_again_in_seconds() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, ToolFeedOutcome::Warming));
    assert!(st.plan(&[FeedInterest::News], false, WEEK, t0 + Duration::from_secs(1)).is_empty());
    assert_eq!(st.plan(&[FeedInterest::News], false, WEEK, t0 + WARMING_RETRY), [news_ask(None)]);
    assert!(td.news_status.contains("first poll"), "{}", td.news_status);
}

// ---- the merge -------------------------------------------------------------------------------

#[test]
fn a_snapshot_replaces_the_rows_and_says_live_with_its_age_and_sources() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    merge_news(&mut st, &mut td, Instant::now(), news_pass(None, snap(headlines(2), 7, 15, 16)));
    assert_eq!(td.news.len(), 2);
    assert_eq!(
        td.news_status,
        format!(
            "2 headlines · LIVE · 15 of 16 sources · fetched 1m ago · via the datahub at {ADDR}"
        )
    );
}

#[test]
fn a_snapshot_older_than_three_cycles_or_from_zero_sources_reads_stale() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let old = ToolFeedOutcome::Snapshot {
        rows: headlines(1),
        fetched_at_ms: NOW_MS - 20 * 60_000,
        generation: 1,
        sources_ok: 16,
        sources_total: 16,
        cadence_secs: 300,
    };
    merge_news(&mut st, &mut td, Instant::now(), news_pass(None, old));
    assert!(
        td.news_status.contains("STALE") && td.news_status.contains("20m ago"),
        "{}",
        td.news_status
    );

    merge_news(&mut st, &mut td, Instant::now(), news_pass(None, snap(headlines(1), 2, 0, 16)));
    assert!(td.news_status.contains("reached none of its sources"), "{}", td.news_status);
}

// ---- the stale limit follows the server's reported cadence --------------------------------------

/// A snapshot that is `age_min` minutes old, from a server that reports `cadence_secs`.
fn aged<T>(rows: Vec<T>, age_min: i64, cadence_secs: u32) -> ToolFeedOutcome<T> {
    ToolFeedOutcome::Snapshot {
        rows,
        fetched_at_ms: NOW_MS - age_min * 60_000,
        generation: 1,
        sources_ok: 1,
        sources_total: 1,
        cadence_secs,
    }
}

/// **The limit is three reported cycles, never below the floor it has always had.** At the default
/// cadences the three cycles ARE the floor, so a box that sets no row is flagged as it always was;
/// a faster server does not lower it, a server that says nothing leaves it, and a slower one raises
/// it. The crossover is one second above the default, and nothing overflows at the top of a `u32`.
#[test]
fn the_stale_limit_is_three_reported_cycles_and_never_below_the_floor() {
    let secs = Duration::from_secs;
    assert_eq!(stale_after(NEWS_STALE_FLOOR, 300), NEWS_STALE_FLOOR, "default news: 3 cycles");
    assert_eq!(stale_after(CALENDAR_STALE_FLOOR, 1_800), CALENDAR_STALE_FLOOR, "default calendar");
    assert_eq!(stale_after(NEWS_STALE_FLOOR, 0), NEWS_STALE_FLOOR, "an older server says nothing");
    assert_eq!(stale_after(NEWS_STALE_FLOOR, 60), NEWS_STALE_FLOOR, "faster never lowers it");
    assert_eq!(stale_after(CALENDAR_STALE_FLOOR, 60), CALENDAR_STALE_FLOOR);
    assert_eq!(stale_after(NEWS_STALE_FLOOR, 301), secs(903), "one second slower than default");
    assert_eq!(stale_after(NEWS_STALE_FLOOR, 3_600), secs(3 * 3_600));
    assert_eq!(stale_after(CALENDAR_STALE_FLOOR, 86_400), secs(3 * 86_400));
    assert_eq!(
        stale_after(NEWS_STALE_FLOOR, u32::MAX),
        secs(3 * u64::from(u32::MAX)),
        "the product is computed in u64"
    );
}

/// A news snapshot judged at several (cadence, age) pairs, boundaries included: exactly the limit
/// is still LIVE, a minute past it is STALE.
#[test]
fn a_news_snapshot_goes_stale_after_three_of_the_servers_own_cycles() {
    let judged = |cadence_secs: u32, age_min: i64| {
        let (mut st, mut td) = (FeedsState::default(), ToolData::default());
        merge_news(
            &mut st,
            &mut td,
            Instant::now(),
            news_pass(None, aged(headlines(1), age_min, cadence_secs)),
        );
        let stale = matches!(td.news_health, FeedHealth::Stale);
        assert_eq!(stale, td.news_status.contains("STALE"), "colour and sentence agree");
        stale
    };
    assert!(judged(300, 16), "the default cadence, 16 min: past 15");
    assert!(!judged(300, 15), "the default cadence, exactly 15 min: not past it");
    assert!(judged(0, 16), "a server that reports nothing keeps the old limit");
    assert!(judged(60, 16), "a faster server does not tighten it");
    assert!(!judged(3_600, 120), "an hourly server, 2 h old: inside its 3 h");
    assert!(!judged(3_600, 180), "…exactly 3 h: not past it");
    assert!(judged(3_600, 181), "…a minute past 3 h");
    assert!(!judged(86_400, 60 * 24), "a daily server, a day old: inside its three days");
}

/// The calendar and the three equity feeds share the calendar floor and follow their own cadence.
#[test]
fn the_equity_feeds_follow_the_reported_cadence_too() {
    let judged = |cadence_secs: u32, age_min: i64| {
        let (mut st, mut td) = (FeedsState::default(), ToolData::default());
        let (plan, pass) = equity_pass(
            aged(vec![earning("2026-10-13")], age_min, cadence_secs),
            ToolFeedOutcome::Warming,
            ToolFeedOutcome::Warming,
        );
        st.merge(&mut td, &plan, pass, &clock(Instant::now()));
        td.cal_earnings_status.contains("STALE")
    };
    assert!(judged(1_800, 91), "the default cadence, 91 min: past 90");
    assert!(!judged(1_800, 90), "…exactly 90 min: not past it");
    assert!(judged(0, 91), "a server that says nothing keeps the old limit");
    assert!(!judged(7_200, 360), "a two-hour server, 6 h old: inside its 6 h");
    assert!(judged(7_200, 361), "…a minute past it");
}

/// An `Unchanged` answer rebuilds the status from the LAST snapshot's stamp, so the cadence that
/// snapshot reported keeps deciding: an hourly server's two-hour-old rows stay LIVE across polls.
#[test]
fn an_unchanged_answer_keeps_judging_by_the_cadence_the_snapshot_reported() {
    let (mut st, mut td) = (FeedsState::default(), ToolData::default());
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, aged(headlines(1), 120, 3_600)));
    assert!(td.news_status.contains("LIVE"), "{}", td.news_status);
    merge_news(
        &mut st,
        &mut td,
        t0,
        news_pass(Some(1), ToolFeedOutcome::Unchanged { generation: 1 }),
    );
    assert!(td.news_status.contains("LIVE"), "{}", td.news_status);
    assert!(matches!(td.news_health, FeedHealth::Live));
}

#[test]
fn a_refusal_is_the_servers_sentence_and_keeps_the_last_good_rows() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(3), 7, 16, 16)));
    merge_news(&mut st, &mut td, t0, news_pass(Some(7), ToolFeedOutcome::NotArmed));
    assert_eq!(td.news.len(), 3, "a refusal never blanks the window");
    assert_eq!(td.news_status, ToolFeedOutcome::<NewsItem>::NotArmed.describe("news"));
    assert_eq!(
        st.plan(&[FeedInterest::News], true, WEEK, t0),
        [news_ask(None)],
        "a non-snapshot answer forgets the generation, so the next ask carries the rows"
    );
}

#[test]
fn unchanged_restores_the_live_line_after_a_failure_and_keeps_the_rows() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(2), 7, 16, 16)));
    st.merge(
        &mut td,
        &[news_ask(Some(7))],
        FeedsPass::ConnectFailed { addr: ADDR.into(), error: "refused".into() },
        &clock(t0),
    );
    assert!(td.news_status.contains("could not be reached"), "{}", td.news_status);
    assert_eq!(td.news.len(), 2);

    merge_news(
        &mut st,
        &mut td,
        t0,
        news_pass(Some(7), ToolFeedOutcome::Unchanged { generation: 7 }),
    );
    assert!(
        td.news_status.contains("LIVE") && td.news_status.contains("2 headlines"),
        "{}",
        td.news_status
    );
    assert_eq!(td.news.len(), 2, "no rows were sent, none were lost");
}

#[test]
fn a_request_that_failed_on_the_wire_names_the_feed_and_keeps_the_rows() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(2), 7, 16, 16)));
    let ask = news_ask(Some(7));
    st.merge(
        &mut td,
        std::slice::from_ref(&ask),
        asked(vec![(ask.clone(), Err("eof".into()))]),
        &clock(t0),
    );
    assert_eq!(td.news_status, failed_line(ADDR, "news", "eof"));
    assert_eq!(td.news.len(), 2);
}

#[test]
fn no_backend_and_an_old_server_say_so_for_every_feed_that_was_to_be_asked() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let t0 = Instant::now();
    let plan = [FeedAsk::Economic { if_generation: None }, equity_ask(None)];
    st.merge(&mut td, &plan, FeedsPass::NoBackend, &clock(t0));
    for status in
        [&td.cal_status, &td.cal_earnings_status, &td.cal_dividends_status, &td.cal_ipo_status]
    {
        assert_eq!(status, NO_BACKEND_NOTE);
    }
    st.merge(
        &mut td,
        &plan,
        FeedsPass::ServerUnsupported { addr: ADDR.into(), features: vec!["options_chain".into()] },
        &clock(t0),
    );
    assert!(td.cal_status.contains("tool_feeds") && td.cal_status.contains("nothing was sent"));
    assert!(td.news_status.is_empty(), "a feed nobody asked about is left alone");
}

#[test]
fn a_keyed_feed_without_its_key_names_it_while_the_others_land() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let (plan, pass) = equity_pass(
        ToolFeedOutcome::NotConfigured { key_name: "FINNHUB_API_KEY".into() },
        snap(vec![dividend("2026-10-13")], 4, 1, 1),
        snap(vec![ipo("2026-10-14"), ipo("")], 4, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(Instant::now()));
    assert!(td.cal_earnings_status.contains("vike-cli secrets set FINNHUB_API_KEY"));
    assert!(td.cal_earnings_status.contains("DATAHUB"), "{}", td.cal_earnings_status);
    assert!(td.cal_dividends_status.contains("LIVE"));
    assert!(td.cal_ipo_status.contains("LIVE"));
    assert!(td.cal_earnings.is_empty());
    assert_eq!(td.cal_equity.get("2026-10-13"), Some(&(0, 1, 0)));
    assert_eq!(td.cal_equity.get("2026-10-14"), Some(&(0, 0, 1)));
    assert_eq!(td.cal_equity.len(), 2, "an IPO with no date belongs to no day");
}

#[test]
fn the_day_counts_follow_the_rows_of_each_snapshot() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let t0 = Instant::now();
    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-13"), earning("2026-10-13"), earning("2026-10-15")], 2, 1, 1),
        snap(vec![dividend("2026-10-13")], 2, 1, 1),
        snap(Vec::<IpoRow>::new(), 2, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(t0));
    assert_eq!(td.cal_equity.get("2026-10-13"), Some(&(2, 1, 0)));
    assert_eq!(td.cal_equity.get("2026-10-15"), Some(&(1, 0, 0)));

    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-16")], 3, 1, 1),
        ToolFeedOutcome::Unchanged { generation: 2 },
        ToolFeedOutcome::Unchanged { generation: 2 },
    );
    st.merge(&mut td, &plan, pass, &clock(t0));
    assert_eq!(
        td.cal_equity.get("2026-10-13"),
        Some(&(0, 1, 0)),
        "earnings replaced, dividends kept"
    );
    assert_eq!(td.cal_equity.get("2026-10-16"), Some(&(1, 0, 0)));
}

#[test]
fn the_economic_calendar_is_shown_in_the_viewers_timezone() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let ask = FeedAsk::Economic { if_generation: None };
    let reply = EconomicCalendarReply { outcome: snap(vec![event(NOW_MS)], 1, 1, 1) };
    st.merge(
        &mut td,
        std::slice::from_ref(&ask),
        asked(vec![(ask.clone(), Ok(FeedReply::Economic(reply)))]),
        &clock(Instant::now()),
    );
    assert_eq!(td.calendar.len(), 1);
    assert_eq!(td.calendar[0].country, "USD");
    assert_eq!(td.calendar[0].iso2, "us");
    assert_eq!(td.calendar[0].importance, Importance::High);
    assert!(td.cal_status.contains("1 events · LIVE"), "{}", td.cal_status);
}

#[test]
fn an_event_instant_becomes_a_local_clock_time_and_day() {
    // 2026-10-14 12:30:00 UTC, viewed from UTC+3 and from UTC-5.
    let ts = NaiveDate::from_ymd_opt(2026, 10, 14)
        .unwrap()
        .and_hms_opt(12, 30, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let east = CalEvent::from_wire(&event(ts), &FixedOffset::east_opt(3 * 3600).unwrap());
    assert_eq!(east.time, "15:30");
    assert_eq!(east.date_iso, "2026-10-14");
    assert_eq!(east.day, "Wednesday, October 14");
    assert_eq!(east.day_short, "Wed 14");

    // the same instant is the PREVIOUS day for a viewer far enough west
    let west = CalEvent::from_wire(
        &event(ts - 13 * 3600 * 1000),
        &FixedOffset::west_opt(5 * 3600).unwrap(),
    );
    assert_eq!(west.time, "18:30");
    assert_eq!(west.date_iso, "2026-10-13");
    assert_eq!(west.day, "Tuesday, October 13");
    assert_eq!(east.ts_ms, ts);
}

#[test]
fn an_event_with_no_instant_reads_a_dash_and_belongs_to_no_day() {
    let e = CalEvent::from_wire(&event(0), &FixedOffset::east_opt(0).unwrap());
    assert_eq!(e.time, "—");
    assert!(e.date_iso.is_empty() && e.day.is_empty() && e.day_short.is_empty());
}

#[test]
fn the_range_label_names_the_monday_to_sunday_week() {
    let monday = NaiveDate::from_ymd_opt(2026, 6, 29).unwrap();
    assert_eq!(week_range_label(monday), "Jun 29 — Jul 5, 2026");
    assert_eq!(
        monday_of(NaiveDate::from_ymd_opt(2026, 7, 5).unwrap()),
        monday,
        "Sunday is the week's last day"
    );
    assert_eq!(monday_of(monday), monday);
}

// ---- the equity generation and week ------------------------------------------------------------

#[test]
fn one_equity_generation_is_sent_only_when_all_three_feeds_hold_it() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let t0 = Instant::now();
    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-13")], 3, 1, 1),
        snap(vec![dividend("2026-10-13")], 3, 1, 1),
        snap(vec![ipo("2026-10-13")], 3, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(t0));
    assert_eq!(
        st.plan(&[FeedInterest::Calendar], true, WEEK, t0)[1],
        equity_ask(Some(3)),
        "all three at generation 3"
    );

    // one feed goes Warming (a restarted server): the next ask is a full one
    let (plan, pass) = equity_pass(
        ToolFeedOutcome::Warming,
        ToolFeedOutcome::Unchanged { generation: 3 },
        ToolFeedOutcome::Unchanged { generation: 3 },
    );
    st.merge(&mut td, &plan, pass, &clock(t0));
    assert_eq!(st.plan(&[FeedInterest::Calendar], true, WEEK, t0)[1], equity_ask(None));
    assert_eq!(td.cal_earnings.len(), 1, "and the earnings held are still shown");
}

#[test]
fn a_new_local_week_forgets_the_old_weeks_rows_but_the_first_week_clears_nothing() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    assert!(!st.enter_week(WEEK), "the first week there is has nothing to clear");
    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-13")], 3, 1, 1),
        snap(vec![dividend("2026-10-13")], 3, 1, 1),
        snap(vec![ipo("2026-10-13")], 3, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(Instant::now()));
    assert!(!st.enter_week(WEEK), "the same week is not a move");
    assert!(st.enter_week("2026-10-19"), "the Monday moved");
    assert_eq!(st.equity_generation(), None, "nothing is held for the new week");
    assert!(st.equity_due.is_none(), "so it is asked at once");
    clear_equity(&mut td);
    assert!(td.cal_earnings.is_empty() && td.cal_equity.is_empty());
}

#[test]
fn a_week_the_server_refuses_is_its_sentence() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    let refused = |r: ToolFeedRefusal| (r.clone(), ToolFeedOutcome::<EarningsRow>::Refused(r));
    let (r, earnings) = refused(ToolFeedRefusal::WeekOutOfRange {
        allowed_from: "2026-10-05".into(),
        allowed_to: "2026-10-19".into(),
    });
    let (plan, pass) = equity_pass(
        earnings,
        ToolFeedOutcome::Refused(r.clone()),
        ToolFeedOutcome::Refused(r.clone()),
    );
    st.merge(&mut td, &plan, pass, &clock(Instant::now()));
    for status in [&td.cal_earnings_status, &td.cal_dividends_status, &td.cal_ipo_status] {
        assert_eq!(status, &r.describe());
    }
}

// ---- a switched datahub ------------------------------------------------------------------------

fn dial(addr: &str) -> CatalogDial {
    CatalogDial { addr: addr.into(), key_name: "VIKE_DATAHUB_OBSERVE_KEY".into(), keys: None }
}

#[test]
fn a_switched_datahub_clears_everything_the_old_one_answered() {
    let mut st = FeedsState::default();
    let mut td = ToolData::default();
    assert!(!st.observe_dial(&mut td, Some(&dial("<host>:1"))), "the first dial clears nothing");
    merge_news(&mut st, &mut td, Instant::now(), news_pass(None, snap(headlines(2), 7, 16, 16)));
    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-13")], 3, 1, 1),
        snap(vec![dividend("2026-10-13")], 3, 1, 1),
        snap(vec![ipo("2026-10-13")], 3, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(Instant::now()));
    td.cal_range = "Oct 12 — Oct 18, 2026".into();

    assert!(!st.observe_dial(&mut td, Some(&dial("<host>:1"))), "the same dial keeps its data");
    assert_eq!(td.news.len(), 2);
    assert!(st.observe_dial(&mut td, Some(&dial("<host>:1"))), "another address clears");
    assert!(td.news.is_empty() && td.news_status.is_empty());
    assert!(td.cal_earnings.is_empty() && td.cal_dividends.is_empty() && td.cal_ipos.is_empty());
    assert!(td.cal_equity.is_empty() && td.cal_range.is_empty());
    assert!(td.cal_earnings_status.is_empty() && td.cal_ipo_status.is_empty());
    assert_eq!(
        st.plan(&[FeedInterest::News], false, WEEK, Instant::now()),
        [news_ask(None)],
        "the new server is asked from scratch, with no generation of the old one's"
    );
    assert!(st.observe_dial(&mut td, None), "no datahub at all clears too");
}

#[test]
fn an_age_reads_in_the_coarsest_whole_unit() {
    assert_eq!(age_label(-5), "0s");
    assert_eq!(age_label(59_000), "59s");
    assert_eq!(age_label(60_000), "1m");
    assert_eq!(age_label(3_600_000), "1h");
    assert_eq!(age_label(2 * 86_400_000), "2d");
}
