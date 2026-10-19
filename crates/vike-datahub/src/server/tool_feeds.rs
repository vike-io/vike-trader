//! The tool-feed verb family — `NewsFeed`, `EconomicCalendar`, `EquityCalendar`: the feeds' current
//! state, answered from the mounted tool-feeds lane and never from the store. `handle_request` (the
//! parent module) routes each request here and passes the lane and nothing else — no `store` handle,
//! the `options_chain` shape.

use vike_datahub_client::proto::Response;
use vike_datahub_client::tool_feeds::{
    EconomicCalendarReply, EquityCalendarReply, NewsReply, ToolFeedOutcome, validate_equity_week,
    validate_equity_week_shape,
};

use crate::tool_feeds::ToolFeedsLane;

/// **Serve the news feed** — the `Request::NewsFeed` handler.
///
/// # ⚠ Every outcome is a SUCCESS, `options_chain_verb`'s shape
///
/// An unarmed build, a feed whose first poll has not landed and an unchanged generation are all
/// `Response::NewsFeed` values: the tool windows ask once a minute, and a stream of errors is the
/// wrong shape for a routine answer. The first call also STARTS the feed's poll thread
/// (`crate::tool_feeds::worker`'s module doc).
pub(super) fn news_feed_verb(if_generation: Option<u64>, lane: Option<&ToolFeedsLane>) -> Response {
    let outcome = match lane {
        Some(lane) => lane.news(if_generation),
        None => ToolFeedOutcome::NotArmed,
    };
    Response::NewsFeed(NewsReply { outcome })
}

/// **Serve the economic calendar** — the `Request::EconomicCalendar` handler; see [`news_feed_verb`].
pub(super) fn economic_calendar_verb(
    if_generation: Option<u64>,
    lane: Option<&ToolFeedsLane>,
) -> Response {
    let outcome = match lane {
        Some(lane) => lane.economic_calendar(if_generation),
        None => ToolFeedOutcome::NotArmed,
    };
    Response::EconomicCalendar(EconomicCalendarReply { outcome })
}

/// **Serve one week of the equity calendars** — the `Request::EquityCalendar` handler.
///
/// # The gate order, and why each step is where it is
///
/// 1. **SHAPE** (`validate_equity_week_shape`) — the one genuinely exceptional input, refused as
///    [`Response::Error`] before anything is consulted. The message never echoes the input.
/// 2. **ARMING** — before the week, so an unarmed server says so for EVERY week rather than leaking
///    which ones it would have served.
/// 3. **THE WEEK** (`validate_equity_week` against THIS server's date) — a Monday within
///    `EQUITY_WEEKS_AHEAD` weeks either side; otherwise all three feeds answer `Refused`, naming the
///    weeks served. ⚠ After the lane exists and before any poller is touched, so a refused week
///    starts nothing and spends no provider call.
/// 4. **THE FEEDS** — three outcomes for one week, each its own: earnings and dividends are keyed
///    (each `NotConfigured` on its own), the IPO feed is not.
pub(super) fn equity_calendar_verb(
    week_start: &str,
    if_generation: Option<u64>,
    lane: Option<&ToolFeedsLane>,
) -> Response {
    // Gate 1.
    if let Err(why) = validate_equity_week_shape(week_start) {
        return Response::Error(format!("equity calendar: {why}"));
    }
    // Gate 2 — a SUCCESS, not a refusal.
    let Some(lane) = lane else {
        return Response::EquityCalendar(EquityCalendarReply {
            earnings: ToolFeedOutcome::NotArmed,
            dividends: ToolFeedOutcome::NotArmed,
            ipos: ToolFeedOutcome::NotArmed,
        });
    };
    // Gate 3.
    if let Err(refusal) = validate_equity_week(week_start, &lane.today_iso()) {
        return Response::EquityCalendar(EquityCalendarReply {
            earnings: ToolFeedOutcome::Refused(refusal.clone()),
            dividends: ToolFeedOutcome::Refused(refusal.clone()),
            ipos: ToolFeedOutcome::Refused(refusal),
        });
    }
    // Gate 4.
    Response::EquityCalendar(lane.equity_calendar(week_start, if_generation))
}

#[cfg(test)]
mod tests {
    //! Every gate, over scripted seams — no provider, no feature: these run on the DEFAULT build.

    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use vike_datahub_client::tool_feeds::{ToolFeedRefusal, is_unchanged};

    use super::*;
    use crate::tool_feeds::news::NewsFeedSpec;
    use crate::tool_feeds::{
        Endpoints, FINNHUB_API_KEY, FMP_API_KEY, FeedHttp, HttpGet, KeyRead, KeyReader, Secret,
    };

    /// 2026-10-14 12:00:00 UTC, a Wednesday: the served Mondays are 2026-10-05, -12 and -19.
    const NOW_MS: i64 = 1_791_979_200_000;

    struct Provider {
        requests: Mutex<Vec<String>>,
    }

    impl FeedHttp for Provider {
        fn get(&self, request: &HttpGet) -> Result<String, String> {
            let url = request.url().to_string();
            self.requests.lock().unwrap().push(url.clone());
            if url.contains("news.test") {
                Ok("<rss><item><title>Headline</title><pubDate>Wed, 14 Oct 2026 10:00:00 +0000</pubDate></item></rss>".into())
            } else if url.contains("calendar.test") {
                Ok(r#"[{"title":"CPI","country":"USD","date":"2026-10-14T08:30:00-04:00","impact":"High"}]"#.into())
            } else if url.contains("finnhub.test") {
                Ok(r#"{"earningsCalendar":[{"date":"2026-10-13","symbol":"JPM","hour":"bmo"}]}"#
                    .into())
            } else if url.contains("fmp.test") {
                Ok(r#"[{"date":"2026-10-14","symbol":"KO"}]"#.into())
            } else if url.contains("nasdaq.test") {
                Ok(r#"{"data":{"priced":{"rows":[{"pricedDate":"10/14/2026","proposedTickerSymbol":"AAA"}]}}}"#.into())
            } else {
                Err("unscripted".into())
            }
        }
    }

    struct Keys {
        finnhub: bool,
        fmp: bool,
    }

    impl KeyReader for Keys {
        fn read(&self, name: &str) -> KeyRead {
            let held =
                (name == FINNHUB_API_KEY && self.finnhub) || (name == FMP_API_KEY && self.fmp);
            if held { KeyRead::Present(Secret::new("a-key")) } else { KeyRead::Absent }
        }
    }

    fn endpoints() -> Endpoints {
        Endpoints {
            news: vec![NewsFeedSpec {
                name: "N".into(),
                market: "stocks".into(),
                url: "http://news.test/rss".into(),
            }],
            calendar_url: "http://calendar.test/ff.json".into(),
            finnhub_base: "http://finnhub.test".into(),
            fmp_base: "http://fmp.test".into(),
            nasdaq_base: "http://nasdaq.test".into(),
        }
    }

    fn lane(finnhub: bool, fmp: bool) -> (ToolFeedsLane, Arc<Provider>) {
        let provider = Arc::new(Provider { requests: Mutex::new(Vec::new()) });
        let lane = ToolFeedsLane::new(
            Arc::clone(&provider) as Arc<dyn FeedHttp>,
            Arc::new(Keys { finnhub, fmp }),
            endpoints(),
            crate::tool_feeds::Cadences::default(),
            Arc::new(|| NOW_MS),
            true,
        );
        (lane, provider)
    }

    /// Ask until `done` accepts the answer (the poll thread lands it behind the first request).
    fn until<T>(mut ask: impl FnMut() -> T, done: impl Fn(&T) -> bool) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let answer = ask();
            if done(&answer) {
                return answer;
            }
            assert!(Instant::now() < deadline, "the poll never landed");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn equity(r: Response) -> EquityCalendarReply {
        match r {
            Response::EquityCalendar(reply) => reply,
            other => panic!("expected EquityCalendar, got {other:?}"),
        }
    }

    #[test]
    fn an_unarmed_server_answers_not_armed_for_every_feed() {
        let news = news_feed_verb(None, None);
        assert!(matches!(
            news,
            Response::NewsFeed(NewsReply { outcome: ToolFeedOutcome::NotArmed })
        ));
        let econ = economic_calendar_verb(Some(3), None);
        assert!(matches!(
            econ,
            Response::EconomicCalendar(EconomicCalendarReply {
                outcome: ToolFeedOutcome::NotArmed
            })
        ));
        let eq = equity(equity_calendar_verb("2026-10-12", None, None));
        assert_eq!(eq.earnings, ToolFeedOutcome::NotArmed);
        assert_eq!(eq.dividends, ToolFeedOutcome::NotArmed);
        assert_eq!(eq.ipos, ToolFeedOutcome::NotArmed);
    }

    #[test]
    fn a_malformed_week_is_the_one_error_even_when_unarmed() {
        let (l, _) = lane(true, true);
        for bad in ["", "2026-10-1x", "next week", "2026-02-30", "2026-10-12T00:00"] {
            for lane in [None, Some(&l)] {
                match equity_calendar_verb(bad, None, lane) {
                    Response::Error(msg) => {
                        assert!(msg.starts_with("equity calendar:"), "{msg}");
                        assert!(bad.len() < 4 || !msg.contains(bad), "the input was echoed: {msg}");
                    }
                    other => panic!("{bad}: expected Error, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn a_refused_week_names_the_served_range_and_starts_no_poller() {
        let (l, provider) = lane(true, true);
        for week in ["2026-10-13", "2026-09-28", "2026-10-26", "2027-10-11"] {
            let eq = equity(equity_calendar_verb(week, None, Some(&l)));
            let refusal = ToolFeedRefusal::WeekOutOfRange {
                allowed_from: "2026-10-05".into(),
                allowed_to: "2026-10-19".into(),
            };
            assert_eq!(eq.earnings, ToolFeedOutcome::Refused(refusal.clone()), "{week}");
            assert_eq!(eq.dividends, ToolFeedOutcome::Refused(refusal.clone()), "{week}");
            assert_eq!(eq.ipos, ToolFeedOutcome::Refused(refusal), "{week}");
        }
        assert!(!l.any_started(), "a refused week starts no poll thread");
        assert!(provider.requests.lock().unwrap().is_empty(), "…and spends no provider call");
        // the three served weeks are served
        for week in ["2026-10-05", "2026-10-12", "2026-10-19"] {
            let eq = equity(equity_calendar_verb(week, None, Some(&l)));
            assert!(!matches!(eq.ipos, ToolFeedOutcome::Refused(_)), "{week}");
        }
    }

    #[test]
    fn the_first_request_starts_the_feed_and_the_next_answers_rows() {
        let (l, provider) = lane(true, true);
        assert!(
            !l.any_started() && provider.requests.lock().unwrap().is_empty(),
            "lazy until asked"
        );
        let first = news_feed_verb(None, Some(&l));
        assert!(matches!(
            first,
            Response::NewsFeed(NewsReply { outcome: ToolFeedOutcome::Warming })
        ));
        let landed = until(
            || news_feed_verb(None, Some(&l)),
            |r| {
                matches!(
                    r,
                    Response::NewsFeed(NewsReply { outcome: ToolFeedOutcome::Snapshot { .. } })
                )
            },
        );
        let Response::NewsFeed(NewsReply {
            outcome: ToolFeedOutcome::Snapshot { rows, generation, sources_ok, sources_total, .. },
        }) = landed
        else {
            unreachable!()
        };
        assert_eq!(
            (rows.len(), rows[0].title.as_str(), sources_ok, sources_total),
            (1, "Headline", 1, 1)
        );
        // an equal generation is the cheap answer
        match news_feed_verb(Some(generation), Some(&l)) {
            Response::NewsFeed(NewsReply {
                outcome: ToolFeedOutcome::Unchanged { generation: g },
            }) => {
                assert_eq!(g, generation)
            }
            other => panic!("expected Unchanged, got {other:?}"),
        }
        // …and only the news feed was touched
        let requests = provider.requests.lock().unwrap().clone();
        assert!(requests.iter().all(|u| u.contains("news.test")), "{requests:?}");
        assert_eq!(requests.len(), 1, "one request burst is one poll: {requests:?}");
        let econ = until(
            || economic_calendar_verb(None, Some(&l)),
            |r| {
                matches!(
                    r,
                    Response::EconomicCalendar(EconomicCalendarReply {
                        outcome: ToolFeedOutcome::Snapshot { .. }
                    })
                )
            },
        );
        let Response::EconomicCalendar(EconomicCalendarReply { outcome }) = econ else {
            unreachable!()
        };
        assert_eq!(outcome.rows().unwrap()[0].currency, "USD");
    }

    #[test]
    fn a_week_serves_one_generation_across_its_three_feeds() {
        let (l, _) = lane(true, true);
        let reply = until(
            || equity(equity_calendar_verb("2026-10-12", None, Some(&l))),
            |r| {
                [r.earnings.generation(), r.dividends.generation(), r.ipos.generation()]
                    .iter()
                    .all(Option::is_some)
            },
        );
        assert_eq!(reply.earnings.rows().unwrap()[0].symbol, "JPM");
        assert_eq!(reply.dividends.rows().unwrap()[0].symbol, "KO");
        assert_eq!(reply.ipos.rows().unwrap()[0].symbol, "AAA");
        let g = reply.earnings.generation().unwrap();
        assert!(
            [reply.dividends.generation(), reply.ipos.generation()].iter().all(|x| *x == Some(g)),
            "one if_generation must be able to match all three: {reply:?}"
        );
        let again = equity(equity_calendar_verb("2026-10-12", Some(g), Some(&l)));
        for outcome in
            [again.earnings.generation(), again.dividends.generation(), again.ipos.generation()]
        {
            assert!(is_unchanged(Some(g), outcome.unwrap()));
        }
        assert!(matches!(again.earnings, ToolFeedOutcome::Unchanged { .. }));
        assert!(matches!(again.dividends, ToolFeedOutcome::Unchanged { .. }));
        assert!(matches!(again.ipos, ToolFeedOutcome::Unchanged { .. }));
        // another week is another entry with its own number
        let other = until(
            || equity(equity_calendar_verb("2026-10-19", Some(g), Some(&l))),
            |r| r.ipos.generation().is_some(),
        );
        assert!(matches!(other.ipos, ToolFeedOutcome::Snapshot { .. }), "not Unchanged: {other:?}");
    }

    #[test]
    fn each_keyed_feed_reports_its_own_missing_key_and_the_keyless_one_lands() {
        let (l, provider) = lane(false, true);
        let reply = until(
            || equity(equity_calendar_verb("2026-10-12", None, Some(&l))),
            |r| {
                matches!(r.earnings, ToolFeedOutcome::NotConfigured { .. })
                    && r.ipos.rows().is_some()
            },
        );
        assert_eq!(
            reply.earnings,
            ToolFeedOutcome::NotConfigured { key_name: FINNHUB_API_KEY.into() }
        );
        assert!(
            reply.dividends.rows().is_some() || matches!(reply.dividends, ToolFeedOutcome::Warming)
        );
        assert!(
            provider.requests.lock().unwrap().iter().all(|u| !u.contains("finnhub.test")),
            "no key, no Finnhub request"
        );
        let (l, _) = lane(true, false);
        let reply = until(
            || equity(equity_calendar_verb("2026-10-12", None, Some(&l))),
            |r| matches!(r.dividends, ToolFeedOutcome::NotConfigured { .. }),
        );
        assert_eq!(
            reply.dividends,
            ToolFeedOutcome::NotConfigured { key_name: FMP_API_KEY.into() }
        );
    }
}
