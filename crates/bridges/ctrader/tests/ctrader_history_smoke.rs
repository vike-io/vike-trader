//! LIVE history measurement against the REAL cTrader demo endpoint: ONE session, about thirty trendbar
//! requests, nothing written anywhere. `#[ignore]` + self-skips without a demo account's creds.
//!
//!     cargo test -p vike-ctrader --test ctrader_history_smoke -- --ignored --nocapture
//!
//! **READ-ONLY — never places an order**: the session is `LiveHistoryWire`'s (the history source's own
//! socket) and sends nothing but `GET_TRENDBARS_REQ` after the handshake. It FOLLOWS the process's grant
//! and never spends a refresh token (`ConnConfig::sync_grant`; no refresh runs in a history session), so
//! an expired access token ends the run with a SKIP line instead of rotating the pair out from under the
//! gatewayhub that holds the account.
//!
//! ## What it is for
//!
//! `docs/ops/ctrader-history.md` lists what the history line does NOT know about our demo broker: how far
//! back each period goes, the largest window a request may span, how an answer is ordered and whether it
//! is cut with no flag, the reply latency, and where a daily or weekly bar begins. The fixtures in
//! `tests/offline/klines.rs` cannot answer any of them — they are written from the vendor's pages — and
//! this is the one run that asks the venue. It PRINTS a table and asserts only that the session worked
//! and at least one request was answered: a probe the broker answers oddly is a finding to read, not a
//! failure.
//!
//! ## The probes (30 requests)
//!
//! | group | requests | what to read |
//! |---|---|---|
//! | earliest bar | MN1 and W1 from timestamp 0 (2), D1 in four five-year windows (4), H1 in six one-month windows 1 to 10 years back (6), M1 in six one-day windows 1 to 60 months back (6) | the first stamp of the MN1 and W1 answers is the broker's depth for EURUSD at those periods; a window with `n=0` is past the depth |
//! | window ceiling | M1 windows of 2, 7, 14, 21 and 35 days (5), H1 windows of 4, 17, 35 and 52 weeks (4) | `n` against the span (a week of FX is about 7,200 M1 bars), `hasMore`, and whether the OLD end is missing with no flag |
//! | order and seam | M1 over 14 days (1) | `order` (ascending or descending) and `dup` |
//! | window edges | one known H1 bar of the 4-week answer, asked as `[S, S]` and as `[S+1, S+1]` (2) | whether the venue takes a bar stamped exactly ON a bound: `n=1` for `[S, S]` means `toTimestamp` is inclusive; `n=1` for `[S+1, S+1]` means `fromTimestamp` is inclusive (the source asks one millisecond below the first wanted stamp whichever it is, so a `0` there confirms the care was needed) |
//!
//! `Window` here is the wanted window and the wire asks `from - 1` (`vike_ctrader::klines::window_request`),
//! so `[S, S]` reaches the venue as `(S - 1, S)` and `[S+1, S+1]` as `(S, S + 1)`.
//!
//! The reply latency is the `took` of every line. The daily and weekly boundary is the hour of the D1 and
//! W1 stamps. The price side (bid, ask or mid) is NOT measured here: it needs the tick request, which the
//! bridge does not send.
//!
//! **`offgrid=<n>`** counts, for a period up to an hour, the stamps that are not a multiple of the period's
//! width in UTC. The history source FAILS a whole chunk that holds one (the venue's stamp convention is then
//! not the one it reads), with no escape hatch, so read this column before any long fetch: `0` on every
//! minute and hourly line is the pass, anything else is a finding to report. `-` is a period of four hours or
//! more, which has no grid rule.
//!
//! ⚠ FX is closed at weekends, so windows ending on a weekend answer short; the windows below end a week
//! or more back, but a run on a Monday morning reads closer to the weekend than one on a Wednesday.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::config::CtraderConfig;
use vike_ctrader::conn::LiveHistoryWire;
use vike_ctrader::event_mapper::trendbar_period_ms;
use vike_ctrader::klines::{HistoryConfigProvider, HistoryWire, Window};
use vike_ctrader::proto::{ProtoOaGetTrendbarsRes, ProtoOaTrendbar, ProtoOaTrendbarPeriod};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::now_ms;
use vike_model::time::utc_rfc3339;

const DAY_MS: i64 = 86_400_000;
const YEAR_MS: i64 = 365 * DAY_MS;

/// One line per answer: the stamps are the bars' OPEN, UTC.
fn describe(
    label: &str,
    period: ProtoOaTrendbarPeriod,
    res: &ProtoOaGetTrendbarsRes,
    took: Duration,
) -> String {
    let stamps: Vec<i64> = res
        .trendbar
        .iter()
        .filter_map(|t| t.utc_timestamp_in_minutes)
        .map(|minutes| i64::from(minutes) * 60)
        .collect();
    let order = if stamps.len() < 2 {
        "-"
    } else if stamps.windows(2).all(|w| w[0] < w[1]) {
        "ascending"
    } else if stamps.windows(2).all(|w| w[0] > w[1]) {
        "descending"
    } else {
        "MIXED"
    };
    let distinct: BTreeSet<i64> = stamps.iter().copied().collect();
    let at = |stamp: Option<&i64>| stamp.map_or("-".to_string(), |s| utc_rfc3339(*s));
    // The source FAILS a chunk holding a stamp off the grid of a period up to an hour, so the measurement
    // counts them here, where a human reads it first. Longer periods have no grid rule: `-`.
    let step_ms = trendbar_period_ms(period);
    let offgrid = if step_ms <= 3_600_000 {
        stamps.iter().filter(|s| (**s * 1000) % step_ms != 0).count().to_string()
    } else {
        "-".to_string()
    };
    format!(
        "{label:<26} {period:?} n={} hasMore={:?} order={order} dup={} offgrid={offgrid} first={} \
         last={} took={}ms",
        res.trendbar.len(),
        res.has_more,
        stamps.len() - distinct.len(),
        at(stamps.iter().min()),
        at(stamps.iter().max()),
        took.as_millis()
    )
}

/// The `offgrid=` count the measurement prints (offline, not ignored): for a period up to an hour, how
/// many stamps are not a multiple of the period's width. The source FAILS a chunk with any such stamp, so
/// the measurement has to be able to warn first.
#[test]
fn the_describe_line_counts_the_stamps_off_the_grid_of_a_period_up_to_an_hour() {
    let answer = |period: ProtoOaTrendbarPeriod, minutes: &[u32]| ProtoOaGetTrendbarsRes {
        period: period as i32,
        trendbar: minutes
            .iter()
            .map(|&m| ProtoOaTrendbar {
                low: Some(1),
                utc_timestamp_in_minutes: Some(m),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let took = Duration::from_millis(1);
    // 1001 and 1002 are off the five-minute grid, 1000 and 1005 are on it.
    let m5 = answer(ProtoOaTrendbarPeriod::M5, &[1000, 1001, 1002, 1005]);
    assert!(describe("x", ProtoOaTrendbarPeriod::M5, &m5, took).contains("offgrid=2"));
    let on_grid = answer(ProtoOaTrendbarPeriod::M5, &[1000, 1005]);
    assert!(describe("x", ProtoOaTrendbarPeriod::M5, &on_grid, took).contains("offgrid=0"));
    // From four hours up the broker's session decides the open: no rule, nothing counted.
    let h4 = answer(ProtoOaTrendbarPeriod::H4, &[1000, 1001]);
    assert!(describe("x", ProtoOaTrendbarPeriod::H4, &h4, took).contains("offgrid=-"));
}

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn the_first_measurement_of_what_the_demo_broker_serves() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) = CtraderConfig::from_credentials(AccountTier::Demo, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent");
        return;
    };
    println!(
        "ctrader demo account row {:?} (check it is the account you meant)",
        config.account_row
    );

    // The history session follows the grant and never refreshes it (module doc).
    let conn = config.to_conn_config();
    let provider: HistoryConfigProvider = Arc::new(move || Ok(conn.clone()));
    let mut wire = LiveHistoryWire::shared(provider);
    let mut session = match wire.open() {
        Ok(session) => session,
        Err(e) => {
            tracing::warn!(target: "vike_ctrader::smoke", "SKIP: connect/auth failed: {e:?}");
            return;
        }
    };
    let Some(symbol_id) = session.symbols().id_of("EURUSD") else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: the account lists no EURUSD");
        return;
    };

    let now = now_ms();
    let mut answered = 0usize;
    let mut asked = 0usize;
    let mut probe = |label: &str,
                     period: ProtoOaTrendbarPeriod,
                     from_ms: i64,
                     to_ms: i64|
     -> Option<ProtoOaGetTrendbarsRes> {
        asked += 1;
        wire.pace();
        let started = Instant::now();
        let answer = session.ask(&Window { symbol_id, period, from_ms: from_ms.max(0), to_ms });
        let took = started.elapsed();
        match answer {
            Ok(res) => {
                answered += 1;
                println!("{}", describe(label, period, &res, took));
                Some(res)
            }
            Err(e) => {
                println!("{label:<26} {period:?} ERROR {e:?} after {}ms", took.as_millis());
                None
            }
        }
    };

    // The earliest bar by period.
    probe("MN1 from 0", ProtoOaTrendbarPeriod::Mn1, 0, now);
    probe("W1 from 0", ProtoOaTrendbarPeriod::W1, 0, now);
    for back in 0..4 {
        let to = now - back * 5 * YEAR_MS;
        probe(
            &format!("D1 {}-{} years back", back * 5, back * 5 + 5),
            ProtoOaTrendbarPeriod::D1,
            to - 5 * YEAR_MS,
            to,
        );
    }
    for years in [1, 2, 3, 5, 8, 10] {
        let to = now - years * YEAR_MS;
        probe(&format!("H1 month {years}y back"), ProtoOaTrendbarPeriod::H1, to - 30 * DAY_MS, to);
    }
    for months in [1, 6, 12, 24, 36, 60] {
        let to = now - months * 30 * DAY_MS;
        probe(&format!("M1 day {months}mo back"), ProtoOaTrendbarPeriod::M1, to - DAY_MS, to);
    }

    // The window ceiling: windows of growing span ending a week back.
    let week_back = now - 7 * DAY_MS;
    for days in [2, 7, 14, 21, 35] {
        probe(
            &format!("M1 {days} days"),
            ProtoOaTrendbarPeriod::M1,
            week_back - days * DAY_MS,
            week_back,
        );
    }
    let mut four_weeks_of_stamps: Vec<i64> = Vec::new();
    for weeks in [4, 17, 35, 52] {
        let answer = probe(
            &format!("H1 {weeks} weeks"),
            ProtoOaTrendbarPeriod::H1,
            week_back - weeks * 7 * DAY_MS,
            week_back,
        );
        if weeks == 4 {
            four_weeks_of_stamps = answer
                .iter()
                .flat_map(|res| res.trendbar.iter())
                .filter_map(|t| t.utc_timestamp_in_minutes)
                .map(|minutes| i64::from(minutes) * 60_000)
                .collect();
            four_weeks_of_stamps.sort_unstable();
        }
    }

    // The window edges, on one bar the venue has just shown (the middle of the 4-week answer).
    if let Some(&known) = four_weeks_of_stamps.get(four_weeks_of_stamps.len() / 2) {
        println!("window edges: the known H1 bar is {}", utc_rfc3339(known / 1000));
        probe("H1 [S, S] (to edge)", ProtoOaTrendbarPeriod::H1, known, known);
        probe("H1 [S+1, S+1] (from edge)", ProtoOaTrendbarPeriod::H1, known + 1, known + 1);
    } else {
        println!("window edges: no bar to ask for, the 4-week H1 answer was empty or refused");
    }

    // Order and seam: a fortnight of minutes, no count.
    probe("M1 fortnight (order)", ProtoOaTrendbarPeriod::M1, now - 28 * DAY_MS, now - 14 * DAY_MS);

    println!("{answered} of {asked} requests answered");
    assert!(answered > 0, "not one trendbar request was answered: the session is not usable");
}
