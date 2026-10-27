//! **The ANSWER SHAPE and the bounds for the three TOOL-FEED verbs that BOTH ends need** —
//! [`Request::NewsFeed`](crate::proto::Request::NewsFeed),
//! [`Request::EconomicCalendar`](crate::proto::Request::EconomicCalendar) and
//! [`Request::EquityCalendar`](crate::proto::Request::EquityCalendar): the row DTOs, the outcome enum
//! that keeps "this server cannot answer" from ever looking like "the feed is empty", and the pure
//! week rule. Declared ungated: a default `vike-datahub` build must DECODE the verbs in order to
//! answer them cleanly. [`crate::options`] is the model, copied shape for shape.
//!
//! # What the verbs are
//!
//! The desktop's News, Economic Calendar, Earnings, Dividends and IPO windows read the DATAHUB, not
//! the providers: the data daemon polls the news RSS feeds, the ForexFactory weekly JSON, Finnhub
//! (earnings), FMP (dividends) and Nasdaq (IPOs) on ITS cadence, and a client asks for the current
//! value. No request names a URL, a key or a cadence, and the provider keys live on the DATAHUB box
//! (the client never sees one), so these are Observe verbs for the options verb's reason
//! (`crate::proto::required_scope`'s arm carries the argument).
//!
//! # ⚠ The rows are RAW, and the display layer is the desktop's
//!
//! [`CalEvent`] carries the event instant (`ts_ms`, UTC) and the currency code, nothing formatted:
//! the day label, the clock time and the country name depend on the VIEWER's timezone and taste, so
//! they are derived from `ts_ms` on the desktop. The same goes for [`NewsItem`]'s topic bucket
//! (`classify_news` stays in `vike-app-core`). The three equity row types carry the strings the
//! desktop renders today (`"Pre-mkt"`, `"1.23"`, `"+4.5%"`): their formatting is not
//! timezone-dependent and the server owns it.
//!
//! # ⚠ The outcome is an ENUM because an empty list is a LIE for five states
//!
//! A server without the lane, a server whose first poll has not landed, a keyed feed whose key the
//! server does not hold, a week the server will not serve and a poll that simply found no rows would
//! all render as "no rows" through a bare `Vec`. They are different variants of
//! [`ToolFeedOutcome`], so a client cannot collapse them by accident; a [`ToolFeedOutcome::Snapshot`]
//! whose `rows` is EMPTY still means what it says.
//!
//! # Generations: cheap polling
//!
//! Each feed's cache carries a `generation` counter the server bumps when a poll CHANGES it. A
//! request naming `if_generation: Some(g)` where `g` equals the current generation is answered
//! [`ToolFeedOutcome::Unchanged`] (no rows on the wire) — [`is_unchanged`] is that rule, one
//! function for the server's handler and the tests.
//!
//! # The cadence rides every Snapshot
//!
//! The server's poll cadence is the operator's to set (`config.datahub_news_poll_secs` and its two
//! siblings), so how long a held snapshot may sit before a client calls it STALE cannot be a
//! constant the client guesses: a `Snapshot` carries `cadence_secs`, and the desktop flags a feed
//! past three of them (never below the limit it has always used). `0` is "the server did not say".

use serde::{Deserialize, Serialize};

/// The most news items the server holds and answers (newest first). **The server's bound, no
/// request moves it**: the desktop kept 500 before the feed moved server-side.
pub const MAX_NEWS_ITEMS: usize = 500;

/// The longest [`NewsItem::summary`], in characters: the server truncates what the feed's
/// description carries (HTML already stripped) to this. **The server's bound.**
pub const NEWS_SUMMARY_CHARS: usize = 400;

/// How many weeks beside the server's current week an [`Request::EquityCalendar`] may name, in
/// EACH direction: `1` serves last, this and next week. **The server's bound, no request moves
/// it.** Symmetric on purpose — see [`validate_equity_week`].
///
/// [`Request::EquityCalendar`]: crate::proto::Request::EquityCalendar
pub const EQUITY_WEEKS_AHEAD: i64 = 1;

/// One news headline, as the server's RSS poll parsed it. The topic bucket
/// (`classify_news(title, tags)`) is the DESKTOP's: it is derived from `title` and `tags`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsItem {
    /// The provider's display name (`"CoinDesk"`).
    pub source: String,
    /// The headline.
    pub title: String,
    /// The plain-text description, HTML stripped, at most [`NEWS_SUMMARY_CHARS`] characters.
    pub summary: String,
    /// The article link, which drives "Open original".
    pub url: String,
    /// The feed's market (`crypto`, `forex`, `stocks`, `global`), which drives the Market filter.
    pub market: String,
    /// The feed's `<category>` values.
    pub tags: Vec<String>,
    /// The publication instant, epoch milliseconds UTC.
    pub ts_ms: i64,
}

/// One economic-calendar event, RAW: no clock time, no day label, no country name (see the module
/// doc). An unparsable feed date is `ts_ms == 0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalEvent {
    /// The event instant, epoch milliseconds UTC.
    pub ts_ms: i64,
    /// The feed's currency code (`"USD"`, `"EUR"`) — the desktop derives the country and flag.
    pub currency: String,
    /// The event title.
    pub title: String,
    /// The feed's impact word (`"High"`, `"Medium"`, `"Low"`, `"Holiday"`).
    pub impact: String,
    /// The consensus forecast as the feed spells it; empty when there is none.
    pub forecast: String,
    /// The previous release as the feed spells it; empty when there is none.
    pub previous: String,
    /// The actual release as the feed spells it; empty until it lands.
    pub actual: String,
}

/// One earnings row (Finnhub), the strings the Earnings window renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EarningsRow {
    /// The report day, ISO `YYYY-MM-DD` (the group key).
    pub date: String,
    /// The ticker.
    pub symbol: String,
    /// `"Pre-mkt"`, `"After-hrs"`, `"Mid-day"` or empty.
    pub hour: String,
    /// The EPS estimate, two decimals; empty when the provider has none.
    pub eps_est: String,
    /// The EPS actual, two decimals; empty until reported.
    pub eps_act: String,
    /// The signed surprise percentage (`"+4.5%"`); empty when it cannot be computed.
    pub surprise: String,
}

/// One dividend row (FMP), the strings the Dividends window renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DividendRow {
    /// The ex-dividend day, ISO `YYYY-MM-DD` (the group key).
    pub date: String,
    /// The ticker.
    pub symbol: String,
    /// The payment day as the provider spells it; empty when unknown.
    pub pay_date: String,
    /// The amount (`"0.24 $"`); empty when the provider has none.
    pub amount: String,
    /// The yield (`"1.20%"`); empty when the provider has none.
    pub yield_pct: String,
    /// The frequency word as the provider spells it; empty when unknown.
    pub freq: String,
}

/// One IPO row (Nasdaq), the strings the IPO window renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpoRow {
    /// The priced or expected day, ISO `YYYY-MM-DD` (the group key); empty when unparsable.
    pub date: String,
    /// The proposed ticker.
    pub symbol: String,
    /// The company name.
    pub company: String,
    /// The proposed exchange.
    pub exchange: String,
    /// The proposed share price as Nasdaq spells it.
    pub price: String,
    /// The shares offered as Nasdaq spells it.
    pub shares: String,
    /// `"Priced"` or `"Upcoming"`.
    pub status: String,
}

/// Why the server answered no rows for a request — the REFUSALS, each a different fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolFeedRefusal {
    /// **The requested equity week is outside what this server serves**: `week_start` is not a
    /// Monday, or lies more than [`EQUITY_WEEKS_AHEAD`] weeks from the server's current week.
    /// `allowed_from` and `allowed_to` are the first and last Monday it serves, ISO dates (both
    /// empty only when the server could not read its own date).
    WeekOutOfRange { allowed_from: String, allowed_to: String },
}

impl ToolFeedRefusal {
    /// A one-line operator-facing sentence. The client renders this rather than assembling its own,
    /// so the daemon and the desktop cannot describe the same refusal differently.
    pub fn describe(&self) -> String {
        match self {
            Self::WeekOutOfRange { allowed_from, .. } if allowed_from.is_empty() => {
                "the requested week is outside what this datahub serves: a week is named by its \
                 Monday, as an ISO date, within a week of the datahub's current week."
                    .to_string()
            }
            Self::WeekOutOfRange { allowed_from, allowed_to } => format!(
                "the requested week is outside what this datahub serves: a week is named by its \
                 Monday, as an ISO date, between {allowed_from} and {allowed_to}."
            ),
        }
    }
}

/// What the server DID about one tool-feed request, for one feed of row type `T`.
///
/// The states are mutually exclusive by construction (the module doc's "five states").
/// ⚠ `PartialEq` but NOT `Eq`, by the options outcome's rule: a row type may carry floats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolFeedOutcome<T> {
    /// The feed's current rows, newest data the server holds. **Rows the server kept from an
    /// earlier poll when a later one failed are still served here** (last-good): `sources_ok` of
    /// `sources_total` says how many of the feed's sources answered on the LATEST poll, so a
    /// client can show "12 of 16 feeds" instead of silence.
    Snapshot {
        /// The rows. EMPTY still means what it says: the poll succeeded and found none.
        rows: Vec<T>,
        /// When the server last fetched anything for this feed, epoch milliseconds UTC.
        fetched_at_ms: i64,
        /// The cache's generation: equal to a later request's `if_generation` means nothing moved.
        generation: u64,
        /// How many of the feed's sources answered on the latest poll.
        sources_ok: u32,
        /// How many sources the feed has (16 RSS feeds, 1 calendar file, ...).
        sources_total: u32,
        /// **How often the server re-polls this feed, in seconds** (its `config.datahub_*_poll_secs`
        /// row, or the lane's default): the number a client derives "how old may these rows be
        /// before I call them stale" from (the desktop's is
        /// `crates/vike-app-core/src/tools/tool_feeds.rs`'s `stale_after`). `0` means the server
        /// did not say (a server that predates the field), and a client then keeps the limit it
        /// has always used. A pure addition to a reply: an older client ignores the key, and an
        /// older server leaves it out and this decodes as `0`, so it needs no feature string and no
        /// version bump (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`,
        /// verdict 3).
        #[serde(default)]
        cadence_secs: u32,
    },
    /// The request's `if_generation` equals the server's current generation: nothing changed, no
    /// rows were sent. A SUCCESS.
    Unchanged { generation: u64 },
    /// **The lane is armed but this feed has landed no poll yet** — its first poll is in flight
    /// (the server polls LAZILY, from the first request) or every poll so far failed. Ask again;
    /// it clears by itself.
    Warming,
    /// **The feed needs an API key the server does not hold**: `key_name` is the credential's name
    /// (never its value) to store on the DATAHUB's box.
    NotConfigured { key_name: String },
    /// **This server serves no tool feeds** — its build lacks the lane (`vike-datahub`'s
    /// `tool-feeds-serve` feature). A SUCCESS, not an error: nothing was polled for this request.
    NotArmed,
    /// Nothing was answered for the request. See [`ToolFeedRefusal`].
    Refused(ToolFeedRefusal),
}

impl<T> ToolFeedOutcome<T> {
    /// The rows, or `None` for every non-`Snapshot` outcome.
    ///
    /// ⚠ A convenience for a caller that has ALREADY branched on the outcome — never a substitute
    /// for doing so, which is the collapse this module's whole shape exists to prevent.
    pub fn rows(&self) -> Option<&[T]> {
        match self {
            Self::Snapshot { rows, .. } => Some(rows),
            _ => None,
        }
    }

    /// The cache generation this outcome speaks for: a `Snapshot`'s or an `Unchanged`'s, `None`
    /// for the states that hold no cache. It is the value a client sends back as `if_generation`.
    pub fn generation(&self) -> Option<u64> {
        match self {
            Self::Snapshot { generation, .. } | Self::Unchanged { generation } => Some(*generation),
            _ => None,
        }
    }

    /// A one-line operator-facing sentence for every outcome, including the successful ones.
    /// `feed` is the feed's human name (`"news"`, `"economic calendar"`, `"earnings"`), so one
    /// sentence table serves all five windows.
    pub fn describe(&self, feed: &str) -> String {
        match self {
            Self::Snapshot { rows, sources_ok, sources_total, .. } => {
                let partial = if sources_ok < sources_total {
                    " (the rest failed on the latest poll; the last good rows are kept)"
                } else {
                    ""
                };
                format!(
                    "{feed}: {} rows from {sources_ok} of {sources_total} sources{partial}.",
                    rows.len()
                )
            }
            Self::Unchanged { generation } => {
                format!("{feed}: unchanged since generation {generation}.")
            }
            Self::Warming => format!(
                "{feed}: this datahub has not finished its first poll yet. Ask again; this \
                 clears by itself once a poll lands."
            ),
            Self::NotConfigured { key_name } => format!(
                "{feed} needs the API key `{key_name}` and this datahub holds none. Store it on the \
                 DATAHUB's own box with `vike-cli secrets set {key_name}` (the desktop never holds \
                 it); the datahub reads it on its next poll."
            ),
            Self::NotArmed => format!(
                "this datahub serves no tool feeds: its build carries no tool-feeds lane (the \
                 `tool-feeds-serve` feature of vike-datahub). Rebuild it with that feature — the \
                 shipped image carries it. Nothing was polled for {feed}."
            ),
            Self::Refused(r) => r.describe(),
        }
    }
}

/// The [`Response::NewsFeed`](crate::proto::Response::NewsFeed) payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsReply {
    /// What happened.
    pub outcome: ToolFeedOutcome<NewsItem>,
}

/// The [`Response::EconomicCalendar`](crate::proto::Response::EconomicCalendar) payload: the
/// ForexFactory week the server holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EconomicCalendarReply {
    /// What happened.
    pub outcome: ToolFeedOutcome<CalEvent>,
}

/// The [`Response::EquityCalendar`](crate::proto::Response::EquityCalendar) payload: three feeds
/// for ONE week, each reporting its OWN outcome — earnings and dividends are keyed (each can be
/// `NotConfigured` on its own), the IPO feed is keyless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquityCalendarReply {
    /// Finnhub earnings.
    pub earnings: ToolFeedOutcome<EarningsRow>,
    /// FMP dividends.
    pub dividends: ToolFeedOutcome<DividendRow>,
    /// Nasdaq IPOs.
    pub ipos: ToolFeedOutcome<IpoRow>,
}

/// **The generation rule**, one function for the server's handler: a request naming
/// `if_generation: Some(g)` is answered [`ToolFeedOutcome::Unchanged`] exactly when `g` equals the
/// cache's `current` generation. `None` (no generation held) is never unchanged, and neither is a
/// generation that does not match — a client that restarted a server holds a stale number and must
/// get the rows.
pub fn is_unchanged(if_generation: Option<u64>, current: u64) -> bool {
    if_generation == Some(current)
}

/// The shape rule for `week_start` that the client applies BEFORE sending and the server's door
/// applies again: an ISO calendar date `YYYY-MM-DD` that EXISTS (ten bytes, digits with dashes at
/// offsets 4 and 7, month 1-12, a day the month has). Whether it is a Monday and near enough is
/// [`validate_equity_week`]'s question, which needs the server's date.
///
/// ⚠ **The message never ECHOES the input** (`crate::options::validate_options_request`'s rule).
pub fn validate_equity_week_shape(week_start: &str) -> Result<(), String> {
    if parse_iso_date(week_start).is_none() {
        return Err("an equity-calendar week_start is not an ISO date: it is `YYYY-MM-DD` of a \
                    real calendar day — ten bytes, digits with dashes at offsets 4 and 7 \
                    (`2026-10-12`)."
            .to_string());
    }
    Ok(())
}

/// **The week rule**, the pure function the server's handler calls with its own clock:
/// `week_start` must be a MONDAY (an ISO date) within [`EQUITY_WEEKS_AHEAD`] weeks of the Monday of
/// the week containing `today_iso`, in EITHER direction; otherwise
/// [`ToolFeedRefusal::WeekOutOfRange`] naming the first and last Monday served.
///
/// ⚠ **Symmetric, deliberately.** A desktop's local Monday and the server's differ for a few hours
/// each week (a viewer east of the server is in next week while the server is still in this one,
/// one west of it the reverse), so a window of "this week and later" would refuse a correct client
/// every Sunday night. The server decides which of the served weeks it actually polls.
///
/// A `week_start` that is not a calendar date at all (callers run
/// [`validate_equity_week_shape`] first and answer it as a malformed request) or a `today_iso` the
/// server could not read is refused with this variant too, the latter with EMPTY bounds.
pub fn validate_equity_week(week_start: &str, today_iso: &str) -> Result<(), ToolFeedRefusal> {
    let Some(today) = parse_iso_date(today_iso) else {
        return Err(ToolFeedRefusal::WeekOutOfRange {
            allowed_from: String::new(),
            allowed_to: String::new(),
        });
    };
    let this_monday = today - weekday_from_monday(today);
    let first = this_monday - 7 * EQUITY_WEEKS_AHEAD;
    let last = this_monday + 7 * EQUITY_WEEKS_AHEAD;
    let refusal = || ToolFeedRefusal::WeekOutOfRange {
        allowed_from: vike_model::time::epoch_ms_to_utc_date(first * vike_model::MS_PER_DAY),
        allowed_to: vike_model::time::epoch_ms_to_utc_date(last * vike_model::MS_PER_DAY),
    };
    let Some(week) = parse_iso_date(week_start) else {
        return Err(refusal());
    };
    if weekday_from_monday(week) != 0 || week < first || week > last {
        return Err(refusal());
    }
    Ok(())
}

/// `YYYY-MM-DD` to days since 1970-01-01, or `None` when it is not a real calendar day.
fn parse_iso_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let shaped = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        });
    if !shaped {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || d < 1 {
        return None;
    }
    // A real calendar day is one the civil round trip hands back unchanged: a 29 February in a
    // common year, or a 31 April, lands in the next month and is refused.
    let days = vike_util::calendar::days_from_civil(y, m, d);
    (vike_util::calendar::civil_from_days(days) == (y, m, d)).then_some(days)
}

/// 0 for Monday ... 6 for Sunday (1970-01-01, day 0, was a Thursday).
fn weekday_from_monday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(week: &str, today: &str) -> (String, String) {
        match validate_equity_week(week, today).expect_err("must be refused") {
            ToolFeedRefusal::WeekOutOfRange { allowed_from, allowed_to } => {
                (allowed_from, allowed_to)
            }
        }
    }

    #[test]
    fn the_calendar_arithmetic_agrees_with_known_dates() {
        // 2026-10-12 is a Monday; 1970-01-01 a Thursday; 2000-02-29 exists; 2100-02-29 does not.
        assert_eq!(weekday_from_monday(parse_iso_date("2026-10-12").unwrap()), 0);
        assert_eq!(weekday_from_monday(0), 3);
        assert!(parse_iso_date("2000-02-29").is_some());
        assert!(parse_iso_date("2028-02-29").is_some());
        assert!(parse_iso_date("2100-02-29").is_none());
        assert!(parse_iso_date("2027-02-29").is_none());
        // The refusals the old `is_leap`/`days_in_month` table gave, now from the civil round trip.
        for bad in ["2026-04-31", "2026-13-01", "2026-00-10", "2026-01-00", "2026-01-32"] {
            assert!(parse_iso_date(bad).is_none(), "{bad}");
        }
        assert!(parse_iso_date("2026-01-31").is_some());
        assert!(parse_iso_date("0000-02-29").is_some(), "year 0 is a Gregorian leap year");
        for d in ["2026-10-12", "1970-01-01", "2028-02-29", "2026-12-31", "1969-12-31"] {
            let days = parse_iso_date(d).unwrap();
            assert_eq!(vike_model::time::epoch_ms_to_utc_date(days * vike_model::MS_PER_DAY), d);
        }
    }

    #[test]
    fn a_monday_within_a_week_of_this_week_is_served() {
        // today = Monday 2026-10-12, then every other weekday of that week: the window is the same.
        for today in [
            "2026-10-12",
            "2026-10-13",
            "2026-10-15",
            "2026-10-18", // Sunday: still this week
        ] {
            for week in ["2026-10-05", "2026-10-12", "2026-10-19"] {
                validate_equity_week(week, today)
                    .unwrap_or_else(|e| panic!("{week}/{today}: {e:?}"));
            }
        }
    }

    #[test]
    fn a_non_monday_or_a_distant_week_is_refused_naming_the_served_range() {
        for week in [
            "2026-10-13", // Tuesday
            "2026-10-18", // Sunday
            "2026-09-28", // two weeks back
            "2026-10-26", // two weeks ahead
            "2027-10-11",
        ] {
            let (from, to) = refused(week, "2026-10-14");
            assert_eq!((from.as_str(), to.as_str()), ("2026-10-05", "2026-10-19"), "{week}");
        }
        // a date that does not exist is refused by the same function, never accepted
        refused("2026-02-30", "2026-10-14");
        refused("not-a-date", "2026-10-14");
    }

    #[test]
    fn the_window_crosses_a_year_and_a_month_boundary() {
        // Wednesday 2026-12-30: this Monday is 2026-12-28, next is 2027-01-04.
        validate_equity_week("2027-01-04", "2026-12-30").expect("next week, next year");
        let (from, to) = refused("2027-01-11", "2026-12-30");
        assert_eq!((from.as_str(), to.as_str()), ("2026-12-21", "2027-01-04"));
        // leap day: Tuesday 2028-02-29, this Monday 2028-02-28
        validate_equity_week("2028-03-06", "2028-02-29").expect("next Monday after a leap day");
    }

    #[test]
    fn an_unreadable_today_is_a_refusal_with_empty_bounds() {
        let (from, to) = refused("2026-10-12", "garbage");
        assert!(from.is_empty() && to.is_empty());
        let s = ToolFeedRefusal::WeekOutOfRange { allowed_from: from, allowed_to: to }.describe();
        assert!(s.contains("Monday") && !s.contains("between"), "{s}");
    }

    #[test]
    fn the_shape_rule_names_the_field_and_never_echoes_the_input() {
        validate_equity_week_shape("2026-10-12").expect("well formed");
        for bad in ["", "2026-9-012", "2026/10/12", "2026-13-01", "2026-02-30", "12-10-2026x"] {
            let err = validate_equity_week_shape(bad).expect_err("malformed");
            assert!(err.contains("week_start") && err.contains("YYYY-MM-DD"), "{err}");
            if bad.len() > 3 {
                assert!(!err.contains(bad), "the refusal echoed the input back: {err}");
            }
        }
    }

    #[test]
    fn only_an_equal_generation_is_unchanged() {
        assert!(is_unchanged(Some(7), 7));
        assert!(!is_unchanged(Some(6), 7));
        assert!(!is_unchanged(Some(8), 7), "a server that restarted holds a LOWER number");
        assert!(!is_unchanged(None, 0), "no generation held is never unchanged");
    }

    #[test]
    fn every_outcome_says_something_different() {
        let snap = |ok: u32| ToolFeedOutcome::<NewsItem>::Snapshot {
            rows: Vec::new(),
            fetched_at_ms: 1,
            generation: 4,
            sources_ok: ok,
            sources_total: 16,
            cadence_secs: 300,
        };
        let all = [
            snap(16),
            snap(12),
            ToolFeedOutcome::Unchanged { generation: 4 },
            ToolFeedOutcome::Warming,
            ToolFeedOutcome::NotConfigured { key_name: "FINNHUB_API_KEY".into() },
            ToolFeedOutcome::NotArmed,
            ToolFeedOutcome::Refused(ToolFeedRefusal::WeekOutOfRange {
                allowed_from: "2026-10-05".into(),
                allowed_to: "2026-10-19".into(),
            }),
        ];
        let sentences: Vec<String> = all.iter().map(|o| o.describe("news")).collect();
        for (i, a) in sentences.iter().enumerate() {
            for b in &sentences[i + 1..] {
                assert_ne!(a, b, "two outcomes read the same");
            }
        }
        assert!(sentences[0].contains("0 rows from 16 of 16"), "{}", sentences[0]);
        assert!(
            sentences[1].contains("12 of 16") && sentences[1].contains("last good"),
            "{}",
            sentences[1]
        );
        assert!(sentences[4].contains("vike-cli secrets set FINNHUB_API_KEY"), "{}", sentences[4]);
        assert!(sentences[4].contains("DATAHUB"), "{}", sentences[4]);
        assert!(sentences[5].contains("tool-feeds-serve"), "{}", sentences[5]);
        assert!(sentences[6].contains("2026-10-05") && sentences[6].contains("2026-10-19"));
        assert_eq!(all[0].generation(), Some(4));
        assert_eq!(all[2].generation(), Some(4));
        assert_eq!(all[3].generation(), None);
        assert!(all[0].rows().is_some() && all[2].rows().is_none());
    }

    /// The Snapshot as an OLD server wrote it, before it carried its cadence.
    const OLD_SNAPSHOT: &str = r#"{"Snapshot":{"rows":[],"fetched_at_ms":5,"generation":9,
        "sources_ok":3,"sources_total":4}}"#;

    /// **A server that predates the cadence decodes as `0` ("not said")**, not as an error: the
    /// field is a pure addition to a reply, so it moves neither the protocol version nor a feature
    /// string (decision 0112, verdict 3).
    #[test]
    fn a_snapshot_from_a_server_without_the_cadence_decodes_as_not_said() {
        let decoded: ToolFeedOutcome<NewsItem> = serde_json::from_str(OLD_SNAPSHOT).unwrap();
        assert!(
            matches!(decoded, ToolFeedOutcome::Snapshot { generation: 9, cadence_secs: 0, .. }),
            "{decoded:?}"
        );
    }

    /// **A CLIENT that predates the cadence still decodes a new server's Snapshot**: the key is
    /// unknown to it and ignored, because neither this enum nor the reply structs deny unknown
    /// fields. The old client's shape is spelled out here as the enum looked before the field.
    #[test]
    fn a_snapshot_with_the_cadence_decodes_in_an_older_clients_shape() {
        #[derive(Debug, Deserialize)]
        enum Before {
            Snapshot {
                rows: Vec<NewsItem>,
                fetched_at_ms: i64,
                generation: u64,
                sources_ok: u32,
                sources_total: u32,
            },
        }
        let new = ToolFeedOutcome::<NewsItem>::Snapshot {
            rows: Vec::new(),
            fetched_at_ms: 5,
            generation: 9,
            sources_ok: 3,
            sources_total: 4,
            cadence_secs: 1_800,
        };
        let wire = serde_json::to_string(&new).unwrap();
        assert!(wire.contains("\"cadence_secs\":1800"), "the new key is on the wire: {wire}");
        let Before::Snapshot { rows, fetched_at_ms, generation, sources_ok, sources_total } =
            serde_json::from_str::<Before>(&wire).unwrap_or_else(|e| panic!("{wire}: {e}"));
        assert!(rows.is_empty());
        assert_eq!((fetched_at_ms, generation, sources_ok, sources_total), (5, 9, 3, 4));
    }

    /// The cadence survives a round trip, so what the server says is what the client reads.
    #[test]
    fn the_cadence_round_trips() {
        let new = ToolFeedOutcome::<NewsItem>::Snapshot {
            rows: Vec::new(),
            fetched_at_ms: 5,
            generation: 9,
            sources_ok: 3,
            sources_total: 4,
            cadence_secs: 86_400,
        };
        let back: ToolFeedOutcome<NewsItem> =
            serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }
}
