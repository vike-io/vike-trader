use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use chrono::NaiveDate;

use super::*;
use crate::dial::NO_BACKEND_NOTE;
use crate::link::DatahubLinkError;
use crate::tool_feeds::{
    CalendarEconomicReply, CalendarEquityReply, DatahubPollRefusal, NewsReply,
};

const ADDRESS: &str = "<host>:7879";
const WEEK: &str = "2026-10-12";
const NOW_MS: i64 = 1_760_000_000_000;

fn clock(now: Instant) -> DatahubPollClock {
    DatahubPollClock { now, now_ms: NOW_MS, today: NaiveDate::from_ymd_opt(2026, 10, 14).unwrap() }
}

/// A key-less dial of the datahub at `addr`: what the dial-reset tests switch between.
fn datahub_dial(addr: &str) -> DatahubDial {
    DatahubDial { address: addr.to_string(), keys: None, epoch: 0 }
}

fn snap<T>(rows: Vec<T>, generation: u64, ok: u32, total: u32) -> DatahubPollOutcome<T> {
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
) -> DatahubPollOutcome<T> {
    DatahubPollOutcome::Snapshot {
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
            timestamp_ms: NOW_MS - i as i64,
        })
        .collect()
}

fn earning(date: &str) -> CalendarEquityEarningsRow {
    CalendarEquityEarningsRow {
        date: date.into(),
        symbol: "AAPL".into(),
        hour: "Pre-mkt".into(),
        eps_est: "1.00".into(),
        eps_act: String::new(),
        surprise: String::new(),
    }
}

fn dividend(date: &str) -> CalendarEquityDividendRow {
    CalendarEquityDividendRow {
        date: date.into(),
        symbol: "KO".into(),
        pay_date: String::new(),
        amount: "0.50 $".into(),
        yield_percent: String::new(),
        freq: String::new(),
    }
}

fn ipo(date: &str) -> CalendarEquityIpoRow {
    CalendarEquityIpoRow {
        date: date.into(),
        symbol: "NEW".into(),
        company: "New Co".into(),
        exchange: "NASDAQ".into(),
        price: String::new(),
        shares: String::new(),
        status: "Upcoming".into(),
    }
}

fn event(ts_ms: i64) -> CalendarEconomicEvent {
    CalendarEconomicEvent {
        timestamp_ms: ts_ms,
        currency: "USD".into(),
        title: "CPI".into(),
        impact: "High".into(),
        forecast: "0.3%".into(),
        previous: "0.2%".into(),
        actual: String::new(),
    }
}

fn news_ask(g: Option<u64>) -> DatahubPollAsk {
    DatahubPollAsk::News { if_generation: g }
}

fn equity_ask(g: Option<u64>) -> DatahubPollAsk {
    DatahubPollAsk::Equity { week_start: WEEK.into(), if_generation: g }
}

fn asked(
    answers: Vec<(DatahubPollAsk, Result<DatahubPollReply, DatahubLinkError>)>,
) -> DatahubPollPass {
    DatahubPollPass::Asked { addr: ADDRESS.into(), answers }
}

/// A failure of the LINK: the client was left unusable, so a later pass may get through.
fn link_lost(cause: &str) -> DatahubLinkError {
    DatahubLinkError { cause: cause.into(), transient: true }
}

/// An answer the datahub gave: a refusal, said at once.
fn refused(cause: &str) -> DatahubLinkError {
    DatahubLinkError { cause: cause.into(), transient: false }
}

fn news_pass(
    g: Option<u64>,
    outcome: DatahubPollOutcome<NewsItem>,
) -> (Vec<DatahubPollAsk>, DatahubPollPass) {
    let ask = news_ask(g);
    (vec![ask.clone()], asked(vec![(ask, Ok(DatahubPollReply::News(NewsReply { outcome })))]))
}

fn equity_pass(
    earnings: DatahubPollOutcome<CalendarEquityEarningsRow>,
    dividends: DatahubPollOutcome<CalendarEquityDividendRow>,
    ipos: DatahubPollOutcome<CalendarEquityIpoRow>,
) -> (Vec<DatahubPollAsk>, DatahubPollPass) {
    let ask = equity_ask(None);
    let reply = DatahubPollReply::Equity(CalendarEquityReply { earnings, dividends, ipos });
    (vec![ask.clone()], asked(vec![(ask, Ok(reply))]))
}

/// What the tool holds for [`WEEK`].
fn held(td: &DatahubPollSnapshot) -> &CalendarEquityWeek {
    &td.calendar_weeks[WEEK]
}

fn merge_news(
    st: &mut DatahubPollState,
    td: &mut DatahubPollSnapshot,
    now: Instant,
    pass: (Vec<DatahubPollAsk>, DatahubPollPass),
) {
    st.merge(td, &pass.0, pass.1, &clock(now));
}

// ---- the link --------------------------------------------------------------------------------

#[test]
fn a_fresh_interest_wakes_the_thread_and_a_live_one_does_not() {
    let (tx, rx) = channel::<()>();
    let link = DatahubPollLink::new(tx);
    let t0 = Instant::now();
    link.want_at(DatahubPollInterest::News, t0);
    assert!(rx.try_recv().is_ok(), "a new interest wakes the thread");
    link.want_at(DatahubPollInterest::News, t0 + Duration::from_secs(10));
    assert!(rx.try_recv().is_err(), "a renewed live interest does not");
    assert_eq!(link.live(t0 + Duration::from_secs(10)), [DatahubPollInterest::News]);
}

#[test]
fn every_shown_week_is_live_a_new_one_wakes_the_thread_and_a_lapsed_one_is_forgotten() {
    let (tx, rx) = channel::<()>();
    let link = DatahubPollLink::new(tx);
    let t0 = Instant::now();
    let monday = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
    let next = monday + chrono::Days::new(7);
    assert!(link.live_weeks(t0).is_empty(), "no week before a window draws");
    link.show_week_at(next, t0);
    assert!(rx.try_recv().is_ok(), "a new week wakes the thread");
    link.show_week_at(monday, t0);
    assert!(rx.try_recv().is_ok(), "a second window's week too");
    link.show_week_at(monday, t0 + Duration::from_secs(10));
    assert!(rx.try_recv().is_err(), "a week drawn again does not");
    assert_eq!(
        link.live_weeks(t0 + Duration::from_secs(10)),
        [monday, next],
        "both, earliest first"
    );
    assert_eq!(link.live_weeks(t0 + INTEREST_TTL), [monday], "the week no window draws lapses");
}

#[test]
fn an_interest_lapses_after_its_ttl_and_wakes_again_when_it_returns() {
    let (tx, rx) = channel::<()>();
    let link = DatahubPollLink::new(tx);
    let t0 = Instant::now();
    link.want_at(DatahubPollInterest::Calendar, t0);
    let _ = rx.try_recv();
    assert!(link.live(t0 + INTEREST_TTL).is_empty(), "a closed window is forgotten");
    link.want_at(DatahubPollInterest::Calendar, t0 + INTEREST_TTL + Duration::from_secs(1));
    assert!(rx.try_recv().is_ok(), "a window reopened after the lapse wakes the thread");
}

#[test]
fn a_refresh_is_remembered_once_and_wakes_the_thread() {
    let (tx, rx) = channel::<()>();
    let link = DatahubPollLink::new(tx);
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
    let st = DatahubPollState::default();
    let t0 = Instant::now();
    assert!(st.plan(&[], false, &[WEEK], t0).is_empty());
    assert_eq!(st.plan(&[DatahubPollInterest::News], false, &[WEEK], t0), [news_ask(None)]);
    assert_eq!(
        st.plan(&[DatahubPollInterest::Calendar], false, &[WEEK], t0),
        [DatahubPollAsk::Economic { if_generation: None }, equity_ask(None)],
        "the day-strip counts need the economic AND the equity feeds"
    );
}

#[test]
fn an_asked_feed_is_not_asked_again_until_its_minute_is_up_unless_forced() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(2), 7, 16, 16)));
    assert!(
        st.plan(&[DatahubPollInterest::News], false, &[WEEK], t0 + Duration::from_secs(30))
            .is_empty()
    );
    assert_eq!(
        st.plan(&[DatahubPollInterest::News], false, &[WEEK], t0 + POLL_EVERY),
        [news_ask(Some(7))],
        "due after a minute, and the held generation rides along"
    );
    assert_eq!(
        st.plan(&[DatahubPollInterest::News], true, &[WEEK], t0 + Duration::from_secs(1)),
        [news_ask(Some(7))],
        "the Refresh pill does not wait"
    );
}

#[test]
fn a_feed_still_warming_on_the_server_is_asked_again_in_seconds() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, DatahubPollOutcome::Warming));
    assert!(
        st.plan(&[DatahubPollInterest::News], false, &[WEEK], t0 + Duration::from_secs(1))
            .is_empty()
    );
    assert_eq!(
        st.plan(&[DatahubPollInterest::News], false, &[WEEK], t0 + WARMING_RETRY),
        [news_ask(None)]
    );
    assert!(td.news_status.contains("first poll"), "{}", td.news_status);
}

// ---- the merge -------------------------------------------------------------------------------

#[test]
fn a_snapshot_replaces_the_rows_and_says_live_with_its_age_and_sources() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    merge_news(&mut st, &mut td, Instant::now(), news_pass(None, snap(headlines(2), 7, 15, 16)));
    assert_eq!(td.news.len(), 2);
    assert_eq!(
        td.news_status,
        format!(
            "2 headlines · LIVE · 15 of 16 sources · fetched 1m ago · via the datahub at {ADDRESS}"
        )
    );
}

#[test]
fn a_snapshot_older_than_three_cycles_or_from_zero_sources_reads_stale() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let old = DatahubPollOutcome::Snapshot {
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
fn aged<T>(rows: Vec<T>, age_min: i64, cadence_secs: u32) -> DatahubPollOutcome<T> {
    DatahubPollOutcome::Snapshot {
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
        let (mut st, mut td) = (DatahubPollState::default(), DatahubPollSnapshot::default());
        merge_news(
            &mut st,
            &mut td,
            Instant::now(),
            news_pass(None, aged(headlines(1), age_min, cadence_secs)),
        );
        let stale = matches!(td.news_health, DatahubPollHealth::Stale);
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
        let (mut st, mut td) = (DatahubPollState::default(), DatahubPollSnapshot::default());
        let (plan, pass) = equity_pass(
            aged(vec![earning("2026-10-13")], age_min, cadence_secs),
            DatahubPollOutcome::Warming,
            DatahubPollOutcome::Warming,
        );
        st.merge(&mut td, &plan, pass, &clock(Instant::now()));
        let week = held(&td);
        assert_eq!(week.dividends_health, DatahubPollHealth::Warming);
        let stale = week.earnings_health == DatahubPollHealth::Stale;
        assert_eq!(stale, week.earnings_status.contains("STALE"), "colour and sentence agree");
        stale
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
    let (mut st, mut td) = (DatahubPollState::default(), DatahubPollSnapshot::default());
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, aged(headlines(1), 120, 3_600)));
    assert!(td.news_status.contains("LIVE"), "{}", td.news_status);
    merge_news(
        &mut st,
        &mut td,
        t0,
        news_pass(Some(1), DatahubPollOutcome::Unchanged { generation: 1 }),
    );
    assert!(td.news_status.contains("LIVE"), "{}", td.news_status);
    assert!(matches!(td.news_health, DatahubPollHealth::Live));
}

#[test]
fn a_refusal_is_the_servers_sentence_and_keeps_the_last_good_rows() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(3), 7, 16, 16)));
    merge_news(&mut st, &mut td, t0, news_pass(Some(7), DatahubPollOutcome::NotArmed));
    assert_eq!(td.news.len(), 3, "a refusal never blanks the window");
    assert_eq!(td.news_status, DatahubPollOutcome::<NewsItem>::NotArmed.describe("news"));
    assert_eq!(
        st.plan(&[DatahubPollInterest::News], true, &[WEEK], t0),
        [news_ask(None)],
        "a non-snapshot answer forgets the generation, so the next ask carries the rows"
    );
}

#[test]
fn unchanged_restores_the_live_line_after_a_failure_and_keeps_the_rows() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(2), 7, 16, 16)));
    st.merge(
        &mut td,
        &[news_ask(Some(7))],
        DatahubPollPass::ConnectFailed { addr: ADDRESS.into(), error: link_lost("refused") },
        &clock(t0),
    );
    assert_eq!(td.news_status, RECONNECTING_LINE);
    assert_eq!(td.news_health, DatahubPollHealth::Reconnecting);
    assert_eq!(td.news.len(), 2);

    merge_news(
        &mut st,
        &mut td,
        t0,
        news_pass(Some(7), DatahubPollOutcome::Unchanged { generation: 7 }),
    );
    assert!(
        td.news_status.contains("LIVE") && td.news_status.contains("2 headlines"),
        "{}",
        td.news_status
    );
    assert_eq!(td.news.len(), 2, "no rows were sent, none were lost");
}

/// **A refusal the datahub ANSWERED is shown at once**, in red, naming the feed; the rows stay.
#[test]
fn a_request_the_datahub_refused_names_the_feed_at_once_and_keeps_the_rows() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(2), 7, 16, 16)));
    let ask = news_ask(Some(7));
    st.merge(
        &mut td,
        std::slice::from_ref(&ask),
        asked(vec![(ask.clone(), Err(refused("unknown verb NewsFeed")))]),
        &clock(t0),
    );
    assert_eq!(td.news_status, failed_line(ADDRESS, "news", "unknown verb NewsFeed"));
    assert_eq!(td.news_health, DatahubPollHealth::Unavailable);
    assert_eq!(td.news.len(), 2);

    // A dial the datahub refused (a denied mac) is an answer too.
    let dial =
        DatahubPollPass::ConnectFailed { addr: ADDRESS.into(), error: refused("auth denied") };
    st.merge(&mut td, std::slice::from_ref(&ask), dial, &clock(t0));
    assert!(td.news_status.contains("auth denied"), "{}", td.news_status);
    assert_eq!(td.news_health, DatahubPollHealth::Unavailable);
}

/// **A failed LINK is transient**: a streak of one or two keeps the headlines under the grey
/// reconnecting line, the third names the cause in red, and a pass that reaches the datahub clears
/// it, so the next failure starts a new quiet streak.
#[test]
fn a_link_failure_streak_is_grey_for_two_passes_red_on_the_third_and_a_success_clears_it() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    merge_news(&mut st, &mut td, t0, news_pass(None, snap(headlines(2), 7, 16, 16)));
    let aborted = "An established connection was aborted by the software in your host machine. \
                   (os error 10053)";
    let ask = news_ask(Some(7));
    let fail = |st: &mut DatahubPollState, td: &mut DatahubPollSnapshot| {
        let pass = asked(vec![(ask.clone(), Err(link_lost(aborted)))]);
        st.merge(td, std::slice::from_ref(&ask), pass, &clock(t0));
    };
    for pass in 1..=2 {
        fail(&mut st, &mut td);
        assert_eq!(td.news_status, RECONNECTING_LINE, "pass {pass}");
        assert_eq!(td.news_health, DatahubPollHealth::Reconnecting, "pass {pass}: grey, never red");
        assert_eq!(td.news.len(), 2, "pass {pass}: the list stays");
    }
    fail(&mut st, &mut td);
    assert_eq!(td.news_health, DatahubPollHealth::Unavailable, "the third is red");
    assert!(
        td.news_status.contains(aborted) && td.news_status.contains(ADDRESS),
        "{}",
        td.news_status
    );
    assert_eq!(td.news.len(), 2);

    merge_news(
        &mut st,
        &mut td,
        t0,
        news_pass(Some(7), DatahubPollOutcome::Unchanged { generation: 7 }),
    );
    assert_eq!(td.news_health, DatahubPollHealth::Live, "a success clears it: {}", td.news_status);
    fail(&mut st, &mut td);
    assert_eq!(td.news_health, DatahubPollHealth::Reconnecting, "a new streak starts grey");
}

/// A link that fails mid-pass leaves the rest of the plan unanswered: each of those feeds says so
/// and waits out the cadence, so one outage is one failed pass and not a burst of re-dials.
#[test]
fn a_link_lost_mid_pass_marks_and_schedules_every_unanswered_feed() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    let plan = [DatahubPollAsk::Economic { if_generation: None }, equity_ask(None)];
    let economic = plan[0].clone();
    st.merge(&mut td, &plan, asked(vec![(economic, Err(link_lost("reset")))]), &clock(t0));
    let week = &td.calendar_weeks[WEEK];
    for status in
        [&td.calendar_status, &week.earnings_status, &week.dividends_status, &week.ipo_status]
    {
        assert_eq!(status, RECONNECTING_LINE);
    }
    let health = [td.calendar_health, week.earnings_health, week.dividends_health, week.ipo_health];
    assert_eq!(health, [DatahubPollHealth::Reconnecting; 4]);
    let interests = [DatahubPollInterest::Calendar];
    assert!(st.plan(&interests, false, &[WEEK], t0 + FEEDS_TICK).is_empty(), "nothing re-dials");
    assert_eq!(
        st.plan(&interests, false, &[WEEK], t0 + POLL_EVERY).len(),
        2,
        "both on the cadence"
    );
}

#[test]
fn no_backend_and_an_old_server_say_so_for_every_feed_that_was_to_be_asked() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    let plan = [DatahubPollAsk::Economic { if_generation: None }, equity_ask(None)];
    st.merge(&mut td, &plan, DatahubPollPass::NoBackend, &clock(t0));
    let week = held(&td);
    for status in
        [&td.calendar_status, &week.earnings_status, &week.dividends_status, &week.ipo_status]
    {
        assert_eq!(status, NO_BACKEND_NOTE);
    }
    // Not configured is not a fault: grey, like the market-data session's "no datahub configured".
    let health = [td.calendar_health, week.earnings_health, week.dividends_health, week.ipo_health];
    assert_eq!(health, [DatahubPollHealth::Unasked; 4]);
    st.merge(
        &mut td,
        &plan,
        DatahubPollPass::ServerUnsupported {
            addr: ADDRESS.into(),
            features: vec!["options_chain".into()],
        },
        &clock(t0),
    );
    assert!(
        td.calendar_status.contains("tool_feeds")
            && td.calendar_status.contains("nothing was sent")
    );
    assert_eq!(td.calendar_health, DatahubPollHealth::Unavailable, "an old server is red");
    assert!(td.news_status.is_empty(), "a feed nobody asked about is left alone");
}

#[test]
fn a_keyed_feed_without_its_key_names_it_while_the_others_land() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let (plan, pass) = equity_pass(
        DatahubPollOutcome::NotConfigured { key_name: "FINNHUB_API_KEY".into() },
        snap(vec![dividend("2026-10-13")], 4, 1, 1),
        snap(vec![ipo("2026-10-14"), ipo("")], 4, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(Instant::now()));
    let week = held(&td);
    assert!(week.earnings_status.contains("vike-cli secrets set FINNHUB_API_KEY"));
    assert!(week.earnings_status.contains("DATAHUB"), "{}", week.earnings_status);
    assert!(week.dividends_status.contains("LIVE"));
    assert!(week.ipo_status.contains("LIVE"));
    assert_eq!(week.earnings_health, DatahubPollHealth::Unavailable, "red, whatever its words");
    assert_eq!(week.dividends_health, DatahubPollHealth::Live);
    assert_eq!(week.ipo_health, DatahubPollHealth::Live);
    assert!(week.earnings.is_empty());
    assert_eq!(week.counts.get("2026-10-13"), Some(&(0, 1, 0)));
    assert_eq!(week.counts.get("2026-10-14"), Some(&(0, 0, 1)));
    assert_eq!(week.counts.len(), 2, "an IPO with no date belongs to no day");
}

#[test]
fn the_day_counts_follow_the_rows_of_each_snapshot() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-13"), earning("2026-10-13"), earning("2026-10-15")], 2, 1, 1),
        snap(vec![dividend("2026-10-13")], 2, 1, 1),
        snap(Vec::<CalendarEquityIpoRow>::new(), 2, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(t0));
    assert_eq!(held(&td).counts.get("2026-10-13"), Some(&(2, 1, 0)));
    assert_eq!(held(&td).counts.get("2026-10-15"), Some(&(1, 0, 0)));

    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-16")], 3, 1, 1),
        DatahubPollOutcome::Unchanged { generation: 2 },
        DatahubPollOutcome::Unchanged { generation: 2 },
    );
    st.merge(&mut td, &plan, pass, &clock(t0));
    assert_eq!(
        held(&td).counts.get("2026-10-13"),
        Some(&(0, 1, 0)),
        "earnings replaced, dividends kept"
    );
    assert_eq!(held(&td).counts.get("2026-10-16"), Some(&(1, 0, 0)));
}

#[test]
fn the_economic_calendar_is_shown_in_the_viewers_timezone() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let ask = DatahubPollAsk::Economic { if_generation: None };
    let reply = CalendarEconomicReply { outcome: snap(vec![event(NOW_MS)], 1, 1, 1) };
    st.merge(
        &mut td,
        std::slice::from_ref(&ask),
        asked(vec![(ask.clone(), Ok(DatahubPollReply::Economic(reply)))]),
        &clock(Instant::now()),
    );
    assert_eq!(td.calendar.len(), 1);
    // The RAW row is adopted: the Calendar window derives its country, flag and importance where
    // it draws (`vike-calendar`'s `CalendarEconomicEventView::from_wire`), in its own zone.
    assert_eq!(td.calendar[0], event(NOW_MS));
    assert!(td.calendar_status.contains("1 events · LIVE"), "{}", td.calendar_status);
    assert_eq!(td.calendar_health, DatahubPollHealth::Live);
}

#[test]
fn the_monday_of_a_day_is_its_weeks_first_day() {
    let monday = NaiveDate::from_ymd_opt(2026, 6, 29).unwrap();
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
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-13")], 3, 1, 1),
        snap(vec![dividend("2026-10-13")], 3, 1, 1),
        snap(vec![ipo("2026-10-13")], 3, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(t0));
    assert_eq!(
        st.plan(&[DatahubPollInterest::Calendar], true, &[WEEK], t0)[1],
        equity_ask(Some(3)),
        "all three at generation 3"
    );

    // one feed goes Warming (a restarted server): the next ask is a full one
    let (plan, pass) = equity_pass(
        DatahubPollOutcome::Warming,
        DatahubPollOutcome::Unchanged { generation: 3 },
        DatahubPollOutcome::Unchanged { generation: 3 },
    );
    st.merge(&mut td, &plan, pass, &clock(t0));
    assert_eq!(st.plan(&[DatahubPollInterest::Calendar], true, &[WEEK], t0)[1], equity_ask(None));
    assert_eq!(held(&td).earnings.len(), 1, "and the earnings held are still shown");
}

#[test]
fn each_shown_week_is_asked_and_held_on_its_own_and_an_unshown_week_is_forgotten() {
    const NEXT: &str = "2026-10-19";
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let t0 = Instant::now();
    let both = [WEEK, NEXT];
    let ask = |week: &str, g: Option<u64>| DatahubPollAsk::Equity {
        week_start: week.into(),
        if_generation: g,
    };
    assert_eq!(
        st.plan(&[DatahubPollInterest::Calendar], false, &both, t0)[1..],
        [ask(WEEK, None), ask(NEXT, None)],
        "two windows on two weeks: one equity ask each"
    );
    let reply = |earnings: &str, g: u64| -> Result<DatahubPollReply, DatahubLinkError> {
        Ok(DatahubPollReply::Equity(CalendarEquityReply {
            earnings: snap(vec![earning(earnings)], g, 1, 1),
            dividends: snap(Vec::<CalendarEquityDividendRow>::new(), g, 1, 1),
            ipos: snap(Vec::<CalendarEquityIpoRow>::new(), g, 1, 1),
        }))
    };
    let plan = [ask(WEEK, None), ask(NEXT, None)];
    let answers = asked(vec![
        (ask(WEEK, None), reply("2026-10-13", 3)),
        (ask(NEXT, None), reply("2026-10-20", 8)),
    ]);
    st.merge(&mut td, &plan, answers, &clock(t0));
    assert_eq!(td.calendar_weeks[WEEK].counts.get("2026-10-13"), Some(&(1, 0, 0)));
    assert_eq!(td.calendar_weeks[NEXT].counts.get("2026-10-20"), Some(&(1, 0, 0)));
    assert!(
        !td.calendar_weeks[WEEK].counts.contains_key("2026-10-20"),
        "neither takes the other's rows"
    );
    assert_eq!(
        st.plan(&[DatahubPollInterest::Calendar], true, &both, t0)[1..],
        [ask(WEEK, Some(3)), ask(NEXT, Some(8))],
        "each week carries its own generation"
    );

    st.keep_weeks(&mut td, &[NEXT]);
    assert!(!td.calendar_weeks.contains_key(WEEK), "a week no window shows is forgotten");
    assert_eq!(td.calendar_weeks[NEXT].earnings.len(), 1, "the shown one is kept");
    assert_eq!(
        st.plan(&[DatahubPollInterest::Calendar], true, &both, t0)[1..],
        [ask(WEEK, None), ask(NEXT, Some(8))],
        "a week shown again is asked from scratch"
    );
}

#[test]
fn a_week_the_server_refuses_is_its_sentence() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    let refused = |r: DatahubPollRefusal| {
        (r.clone(), DatahubPollOutcome::<CalendarEquityEarningsRow>::Refused(r))
    };
    let (r, earnings) = refused(DatahubPollRefusal::WeekOutOfRange {
        allowed_from: "2026-10-05".into(),
        allowed_to: "2026-10-19".into(),
    });
    let (plan, pass) = equity_pass(
        earnings,
        DatahubPollOutcome::Refused(r.clone()),
        DatahubPollOutcome::Refused(r.clone()),
    );
    st.merge(&mut td, &plan, pass, &clock(Instant::now()));
    let week = held(&td);
    for status in [&week.earnings_status, &week.dividends_status, &week.ipo_status] {
        assert_eq!(status, &r.describe());
    }
}

// ---- a switched datahub ------------------------------------------------------------------------

#[test]
fn a_switched_datahub_clears_everything_the_old_one_answered() {
    let mut st = DatahubPollState::default();
    let mut td = DatahubPollSnapshot::default();
    assert!(
        !st.observe_dial(&mut td, Some(&datahub_dial("<host>:1"))),
        "the first dial clears nothing"
    );
    merge_news(&mut st, &mut td, Instant::now(), news_pass(None, snap(headlines(2), 7, 16, 16)));
    let (plan, pass) = equity_pass(
        snap(vec![earning("2026-10-13")], 3, 1, 1),
        snap(vec![dividend("2026-10-13")], 3, 1, 1),
        snap(vec![ipo("2026-10-13")], 3, 1, 1),
    );
    st.merge(&mut td, &plan, pass, &clock(Instant::now()));

    assert!(
        !st.observe_dial(&mut td, Some(&datahub_dial("<host>:1"))),
        "the same dial keeps its data"
    );
    assert_eq!(td.news.len(), 2);
    td.calendar_health = DatahubPollHealth::Live;
    assert!(st.observe_dial(&mut td, Some(&datahub_dial("<host>:1"))), "another address clears");
    assert!(td.news.is_empty() && td.news_status.is_empty());
    assert_eq!(td.news_health, DatahubPollHealth::Unasked);
    assert_eq!(td.calendar_health, DatahubPollHealth::Unasked);
    assert!(td.calendar_weeks.is_empty(), "every week's rows, counts and statuses");
    assert_eq!(
        st.plan(&[DatahubPollInterest::News], false, &[WEEK], Instant::now()),
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
