//! Pure UTC civil-calendar ⇄ epoch conversions, integer-exact, chrono-free, valid across the full
//! `i64` range: the `date=` partition key, the RFC-3339 instants, the label parsers and
//! calendar-month arithmetic.
//!
//! All of it is built on the calendar's two primitives, `civil_from_days`/`days_from_civil`
//! (Howard Hinnant's proleptic-Gregorian algorithms), which live one floor down in
//! `crates/vike-util/src/calendar.rs` — the single home for the day ⇄ date conversion the venue
//! bridges, the backtest schedule/harness and the collectors each used to carry a hand copy of.
//! Every caller names them `vike_util::calendar::X`; this module re-exports neither.
//!
//! The directory also holds the other time-shaped modules: the [`clock::Clock`] seam (Test/Live) and
//! the [`schedule::TimeRule`] wall-clock firing rule.

use vike_util::calendar::{civil_from_days, days_from_civil};

pub mod clock;
pub mod schedule;

/// Epoch-ms (UTC) → `"YYYY-MM-DD"`. Floors to the UTC day; pre-1970 (negative) ms floor toward
/// -inf so the last millisecond of a day and the first of the next never share a bucket
/// (e.g. `-1` → `1969-12-31`, `0` → `1970-01-01`). This is the `date=` partition key the
/// DataFusion hist store splits on.
pub fn epoch_ms_to_utc_date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Epoch-ms (UTC) → `"YYYY-MM-DDTHH:MM:SSZ"` — the second-resolution RFC 3339 instant, for a stored
/// TEXT timestamp a human reads.
///
/// The sibling of [`epoch_ms_to_utc_date`] one resolution down, and it exists because a DATE cannot
/// answer the question `account.last_verified_at` is for: *did this credential authenticate today,
/// or three weeks ago* needs a time as well as a day once two mounts happen in one day.
///
/// Sub-second precision is deliberately dropped. The column is written once per authenticated
/// SESSION ESTABLISHMENT (the credential-schema spec §4.5 fixes that so it can never become a
/// hot-path write), so milliseconds would be noise in a value whose whole job is to be read by an
/// operator. Pre-1970 instants floor toward -inf on the DAY exactly as [`epoch_ms_to_utc_date`]
/// does, so the two never disagree about which day an instant belongs to.
pub fn epoch_ms_to_utc_timestamp(ms: i64) -> String {
    let day = ms.div_euclid(86_400_000);
    let (y, mo, d) = civil_from_days(day);
    let rem = ms.rem_euclid(86_400_000) / 1000;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Epoch-ms (UTC) → `"YYYY-MM-DDTHH:MM:SS.mmmZ"` — the MILLISECOND-resolution RFC 3339 instant, the
/// sibling of [`epoch_ms_to_utc_timestamp`] that keeps the milliseconds.
///
/// ⚠ **On the wire, byte for byte.** It is the OKX V5 REST `OK-ACCESS-TIMESTAMP` (signed into the
/// prehash, so a single changed byte is a rejected request) and the incident-bundle manifest's human
/// time fields. Both carried their own copy until they were routed here, and the golden values in
/// `time_tests.rs` are the strings those copies produced. Pre-1970 instants floor toward -inf on the
/// day, the second and the millisecond, exactly as [`epoch_ms_to_utc_date`] does (`-1` →
/// `1969-12-31T23:59:59.999Z`).
pub fn epoch_ms_to_utc_timestamp_ms(ms: i64) -> String {
    let (y, mo, d) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    let ms_of_day = ms.rem_euclid(MS_PER_DAY);
    let secs = ms_of_day / 1000;
    let (h, mi, s, milli) = (secs / 3600, (secs % 3600) / 60, secs % 60, ms_of_day % 1000);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

/// `"YYYY-MM-DDTHH:MM:SS[.mmm]Z"` → epoch-ms: the exact inverse of [`epoch_ms_to_utc_timestamp_ms`]
/// (and of [`utc_rfc3339`]'s second-resolution spelling), for an instant an operator types — a
/// `--expires` on a good-till-date order. UTC only, the `Z` is required, and a date the calendar
/// does not have (`2026-02-30`) or a field out of range is `None`, never normalised into a nearby
/// instant.
pub fn parse_utc_timestamp_ms(s: &str) -> Option<i64> {
    let body = s.trim().strip_suffix('Z')?;
    let (date, time) = body.split_once('T')?;
    let mut d = date.splitn(3, '-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: u32 = d.next()?.parse().ok()?;
    let dd: u32 = d.next()?.parse().ok()?;
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (time, None),
    };
    let mut t = hms.splitn(3, ':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;
    let milli: i64 = match frac {
        None => 0,
        Some(f) if (1..=3).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit()) => {
            f.parse::<i64>().ok()? * 10_i64.pow(3 - f.len() as u32)
        }
        Some(_) => return None,
    };
    if !(1..=12).contains(&mo)
        || !(1..=31).contains(&dd)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0..60).contains(&sec)
    {
        return None;
    }
    let days = days_from_civil(y, mo, dd);
    if civil_from_days(days) != (y, mo, dd) {
        return None; // a day the month does not have
    }
    days.checked_mul(MS_PER_DAY)?.checked_add(((h * 60 + mi) * 60 + sec) * 1000 + milli)
}

/// Milliseconds in one UTC calendar day — the [`TimeInForce::Day`](crate::TimeInForce::Day) session
/// boundary, and the day length every `div_euclid` day floor in this module divides by. A plain UTC
/// day is the "session" concept here: neither the backtest nor the live core models a per-venue
/// exchange-session calendar.
pub const MS_PER_DAY: i64 = 86_400_000;

/// UTC calendar-day index of an epoch-ms timestamp (floor division, so pre-1970 ts stay monotone).
pub fn utc_day(ts_ms: i64) -> i64 {
    ts_ms.div_euclid(MS_PER_DAY)
}

/// Unix seconds as RFC-3339 UTC to the second — the ONE spelling every run-manifest timestamp
/// takes (`vike_runs`), so two producers cannot disagree about the format of a common field.
///
/// ⚠ **Chrono-free, and that is load-bearing rather than tidiness.** The run store is the crate
/// `vike-runs`, a leaf at layer 15 that names only the vocabulary and the helper floor, so that
/// `vike-cli` can read a run without linking the engine (`crates/vike-cli/Cargo.toml`'s `vike-model`
/// rationale argues the same trade for the client-order-id generator). `chrono` may not reach it
/// through this formatter, so the calendar math is [`civil_from_days`] — Howard Hinnant's proleptic-Gregorian algorithm, integer-exact across the
/// whole `i64` range, whose one home is `vike_util::calendar`.
///
/// A second no calendar date can hold falls back to the raw number rather than panicking: a manifest
/// is metadata about a run that already succeeded, and must never be the thing that kills it.
pub fn utc_rfc3339(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    // The one unrepresentable case: a year outside four digits has no RFC-3339 spelling, so the raw
    // number is the honest answer. `i64::MIN`/`i64::MAX` seconds are both far outside it.
    if !(0..=9999).contains(&y) {
        return unix_secs.to_string();
    }
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Epoch-ms (UTC) floored to UTC-midnight of its own calendar day — same flooring semantics as
/// [`epoch_ms_to_utc_date`] (pre-1970 floors toward -inf), returning the numeric midnight instant
/// instead of a `"YYYY-MM-DD"` label. The home for any `1d`-interval consolidator that must store
/// its bar `ts` UTC-midnight-floored (e.g. the EOD collector normalizing a source's market-open
/// timestamp) rather than re-deriving the divide-by-day-length by hand at the call site.
pub fn floor_ms_to_utc_day(ms: i64) -> i64 {
    ms.div_euclid(86_400_000) * 86_400_000
}

/// Epoch-ms (UTC) → ISO weekday index, **Monday = 0** through Sunday = 6. Built on the same
/// `div_euclid` day floor as [`floor_ms_to_utc_day`], so pre-1970 timestamps land on the correct
/// weekday rather than skewing by a day.
///
/// The `+ 3` anchor is 1970-01-01 being a **Thursday** (index 3) — pinned by a test that derives it
/// from [`days_from_civil`] rather than trusting the constant. The single home for the weekday math
/// `vike_venues::session` needs to map a timestamp onto the trading week.
pub fn utc_weekday(ms: i64) -> u32 {
    (ms.div_euclid(86_400_000) + 3).rem_euclid(7) as u32
}

/// Epoch-ns (UTC) → `"YYYY-MM-DD"`. Same flooring semantics as [`epoch_ms_to_utc_date`] at
/// nanosecond granularity (used by the Polymarket raw-frame tap, whose `local_ns` receive
/// stamps drive the daily gzip rollover).
pub fn epoch_ns_to_utc_date(ns: i64) -> String {
    let (y, m, d) = civil_from_days(ns.div_euclid(86_400_000_000_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parse a `YYYY-MM-DDTHH` hour label into `(year, month, day, hour)`, validating that month,
/// day, and hour fall in range (`1..=12`, `1..=31`, `0..24`). `None` on anything malformed —
/// the strict validation prevents an out-of-range field from being handed to
/// [`days_from_civil`] as garbage.
pub fn parse_hour_label(s: &str) -> Option<(i64, u32, u32, u32)> {
    let (date, hh) = s.split_once('T')?;
    let mut parts = date.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    let h: u32 = hh.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(0..24).contains(&h) {
        return None;
    }
    Some((y, m, d, h))
}

/// A window span as a `[walkforward]` profile spells it — the duration grammar of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §11.
///
/// ⚠ A SEPARATE grammar from [`interval_ms`](vike_marketdata::interval::interval_ms),
/// deliberately. That one answers "how long is a BAR" and has roughly fourteen production callers — the hist store's interval parse, the
/// backfill supervisor's cadence check, `vike_bridge_core`'s pacer, the oanda and deribit
/// market feeds — so widening it with `w`/`mo`/`y` would silently widen what a store
/// interval and a collector cadence accept. It is also a single-trailing-char split, which
/// makes the two-char `mo` and the four-char `bars` structurally unreachable there.
///
/// Three differences beyond the suffix set: a zero count is REFUSED here (`interval_ms("0m")`
/// answers `Some(0)`, and `BacktestProfile::validate` has had to compensate for that on
/// `engine.timeframes` ever since), the error is a `Result` carrying the valid set rather
/// than a bare `None`, and there is no `s` — a walk-forward window measured in seconds is
/// not a thing this grammar admits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    /// An EXACT number of milliseconds: `m` minutes, `h` hours, `d` days (always 24h — this
    /// module is UTC-only, so there is no DST wrinkle), `w` weeks (always 7d).
    Ms(i64),
    /// CALENDAR months — `mo`, and `y` as twelve of them. Not a fixed multiple: one month
    /// from 31 January is 28 days and from 31 March it is 30, so this variant carries the
    /// COUNT and the milliseconds are knowable only against an anchor
    /// ([`add_calendar_months`]).
    Months(i64),
    /// A BAR count — `bars`. Resolution-relative by construction, so it needs neither an
    /// anchor nor an interval to become a number of rows.
    Bars(usize),
}

/// Parse one `[walkforward]` duration. Trimmed and case-insensitive; a bare number, a zero
/// count, an unknown suffix and an overflowing count are each an `Err` naming the valid set.
pub fn parse_span(s: &str) -> Result<Span, String> {
    const WANT: &str = "want 90m | 4h | 30d | 2w | 3mo | 1y | 5000bars";
    let raw = s.trim();
    if raw.is_empty() {
        return Err(format!("empty duration ({WANT})"));
    }
    // Checked on the RAW string, before the lowercase below can hide it: `m` already means
    // minutes in this tree (`interval = "1m"`), and case as the only distinguisher is how
    // `1M`/`1m` bugs happen. Spelled as its own refusal so the operator is told what to
    // write rather than that `M` is unknown.
    if raw.ends_with('M') {
        return Err(format!("{raw:?}: months are `mo`, never `M` — `m` is minutes here ({WANT})"));
    }
    let lower = raw.to_ascii_lowercase();
    let digits = lower.len() - lower.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (num, unit) = lower.split_at(digits);
    if num.is_empty() {
        return Err(format!("{raw:?} has no count ({WANT})"));
    }
    if unit.is_empty() {
        // ⚠ The two example spellings are written WITHOUT a space (`90bars`, `90d`) on
        // purpose: they are paste-able answers to the mistake, not prose about it. A
        // "90 bars" here would name no string this grammar accepts.
        return Err(format!(
            "{raw:?} has no unit — a bare number is refused so it cannot silently mean \
             {num}bars or {num}d ({WANT})"
        ));
    }
    let n: i64 = num.parse().map_err(|_| format!("{raw:?}: count does not fit ({WANT})"))?;
    if n == 0 {
        return Err(format!(
            "{raw:?} is zero-length — a window of no time makes every boundary the same \
             instant ({WANT})"
        ));
    }
    let ms = |mult: i64| {
        n.checked_mul(mult)
            .map(Span::Ms)
            .ok_or_else(|| format!("{raw:?}: count does not fit ({WANT})"))
    };
    match unit {
        "m" => ms(60_000),
        "h" => ms(3_600_000),
        "d" => ms(86_400_000),
        "w" => ms(7 * 86_400_000),
        "mo" => Ok(Span::Months(n)),
        "y" => n
            .checked_mul(12)
            .map(Span::Months)
            .ok_or_else(|| format!("{raw:?}: count does not fit ({WANT})")),
        "bars" => Ok(Span::Bars(n as usize)),
        other => Err(format!("unknown duration suffix {other:?} in {raw:?} ({WANT})")),
    }
}

/// Days in the civil month `(y, m)`, derived rather than tabulated — the difference between
/// the first of this month and the first of the next. [`days_from_civil`]'s Mar-based month
/// index makes `m = 13` land exactly on `(y + 1, 1, 1)`, so `m = 12` needs no branch and the
/// leap year needs no rule.
fn days_in_month(y: i64, m: u32) -> u32 {
    (days_from_civil(y, m + 1, 1) - days_from_civil(y, m, 1)) as u32
}

/// `ms` advanced by `months` CALENDAR months, clamping to the target month's last day and
/// carrying the time of day through untouched. Chrono-free, exact across the full `i64`
/// range, and the reason [`Span::Months`] stores a count rather than a duration.
///
/// 31 January + 1 month is 28 February (29 in a leap year); 31 March + 1 month is 30 April.
/// A negative `months` walks backwards under the same rule.
pub fn add_calendar_months(ms: i64, months: i64) -> i64 {
    let day = ms.div_euclid(86_400_000);
    let time_of_day = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(day);
    let total = y * 12 + (m as i64 - 1) + months;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let nd = d.min(days_in_month(ny, nm));
    days_from_civil(ny, nm, nd) * 86_400_000 + time_of_day
}

/// Every calendar day from `start` to `end_inclusive` (both `(year, month, day)`), ascending.
/// Built directly over [`days_from_civil`]/[`civil_from_days`] so it can never diverge from the
/// store's `date=` partition-key math — the reason this module exists. Empty when
/// `start > end_inclusive`.
///
/// The single home for the day-range iterator the collectors re-derived (vike-backfill's tardis
/// adapter hand-rolled its own `days_in_month` leap-year loop). Callers that key on
/// `(i32, u32, u32)` day tuples cast the year at the boundary (`y as i64` / `y as i32`).
pub fn days_in_range(
    start: (i64, u32, u32),
    end_inclusive: (i64, u32, u32),
) -> Vec<(i64, u32, u32)> {
    let s = days_from_civil(start.0, start.1, start.2);
    let e = days_from_civil(end_inclusive.0, end_inclusive.1, end_inclusive.2);
    (s..=e).map(civil_from_days).collect()
}

/// Every UTC hour label `"YYYY-MM-DDTHH"` from `from` to `to` **inclusive**, ascending. Both bounds
/// are `YYYY-MM-DDTHH` labels ([`parse_hour_label`]); day/month/year rollovers fold naturally
/// through the hours-since-epoch arithmetic. Empty on an unparsable bound or when `from` is after
/// `to`. The hour-granularity sibling of [`days_in_range`] (the pmxt archive backfill's hour walk).
pub fn hours_in_range(from: &str, to: &str) -> Vec<String> {
    let (Some((fy, fm, fd, fh)), Some((ty, tm, td, th))) =
        (parse_hour_label(from), parse_hour_label(to))
    else {
        return Vec::new();
    };
    let start_hours = days_from_civil(fy, fm, fd) * 24 + fh as i64;
    let end_hours = days_from_civil(ty, tm, td) * 24 + th as i64;
    if start_hours > end_hours {
        return Vec::new();
    }
    (start_hours..=end_hours)
        .map(|total_h| {
            let (days, h) = (total_h.div_euclid(24), total_h.rem_euclid(24));
            let (y, m, d) = civil_from_days(days);
            format!("{y:04}-{m:02}-{d:02}T{h:02}")
        })
        .collect()
}

/// Split a `YYYY-MM-DD` UTC date into a validated `(year, month, day)` tuple, range-checking month
/// (`1..=12`) and day (`1..=31`) before any calendar math so an out-of-range field can't be handed
/// to [`days_from_civil`] as garbage. `Err(String)` (a CLI-printable reason) on the wrong field
/// count or a non-numeric/out-of-range field.
///
/// The tuple-returning sibling of [`parse_date_label`], for callers that iterate calendar days
/// (the tardis day-span backfill) rather than needing a single epoch-ms instant.
pub fn parse_ymd(s: &str) -> Result<(i64, u32, u32), String> {
    let s = s.trim();
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(format!("bad date {s:?} (want epoch-ms or YYYY-MM-DD)"));
    }
    let y: i64 = parts[0].parse().map_err(|_| format!("bad year in {s:?}"))?;
    let m: u32 = parts[1].parse().map_err(|_| format!("bad month in {s:?}"))?;
    let d: u32 = parts[2].parse().map_err(|_| format!("bad day in {s:?}"))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(format!("out-of-range month/day in {s:?}"));
    }
    Ok((y, m, d))
}

/// Parse a CLI `--start`/`--end`/`--date` label into epoch-**milliseconds** (UTC). A bare integer
/// is taken verbatim as epoch-ms (any sign — pre-1970 allowed); otherwise the string must be a
/// `YYYY-MM-DD` UTC date ([`parse_ymd`]), which maps to that day's midnight-UTC epoch-ms via
/// [`days_from_civil`]. `Err(String)` carries a human-readable reason for the CLI to print.
///
/// The single home for the epoch-ms-or-date arg parse the backfill collectors each hand-rolled —
/// hyperliquid candles + funding carried byte-identical copies, and ibkr a `time`-crate variant
/// (dedup finding A11); consolidating here also drops vike-backfill's direct `time` dependency.
pub fn parse_date_label(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if let Ok(ms) = s.parse::<i64>() {
        return Ok(ms);
    }
    let (y, m, d) = parse_ymd(s)?;
    Ok(days_from_civil(y, m, d) * 86_400_000)
}

#[path = "time_tests.rs"]
#[cfg(test)]
mod time_tests;
