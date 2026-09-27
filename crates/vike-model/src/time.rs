//! Pure UTC civil-calendar ⇄ epoch conversions — Howard Hinnant's proleptic-Gregorian
//! algorithms (<http://howardhinnant.github.io/date_algorithms.html>): integer-exact,
//! chrono-free, valid across the full `i64` range.
//!
//! This is the single home for calendar math that was previously re-derived per crate — the
//! venue bridges (`ig`, `polymarket`), the backtest schedule/harness, and the collectors each
//! carried a hand copy of `days_from_civil`/`civil_from_days`. Consolidating here removes the
//! divergence foot-gun (a fix in one copy silently skewing a partition key or resolution
//! timestamp in another) and gives every layer a lower-than-everyone home to reach for.

/// Days since 1970-01-01 → `(year, month, day)` (Howard Hinnant's `civil_from_days`,
/// proleptic Gregorian). Valid for the full `i64` day range.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468; // shift epoch to 0000-03-01
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day-of-era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year-of-era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year (Mar-based) [0, 365]
    let mp = (5 * doy + 2) / 153; // month-of-year (Mar=0) [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d)
}

/// `(year, month, day)` → days since 1970-01-01 (Howard Hinnant's `days_from_civil`,
/// proleptic Gregorian). The exact inverse of [`civil_from_days`].
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // month index, Mar-based [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

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
/// [`crate::session`] needs to map a timestamp onto the trading week.
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

/// Parse a vike interval string (`"30s"`/`"1m"`/`"5m"`/`"4h"`/`"1d"`) into epoch-**milliseconds**:
/// an integer count followed by exactly one unit char — `s`/`m`/`h`/`d`. Returns `None` for
/// anything malformed (fewer than 2 chars, empty or non-digit count, unknown unit).
///
/// The single home for the interval vocabulary that was re-parsed — divergently — in three places
/// (`vike-backtest`'s `parse_timeframe` rejected seconds, `vike-data`'s took `s`/`m`/`h`/`d`,
/// `vike-ibkr`'s took a fixed whitelist), so a `"30s"` series that loaded fine from the hist store
/// could not be named as a backtest timeframe. Every previously-accepted string maps to the exact
/// same value here, so delegating is a safe monotonic widening.
pub fn interval_ms(interval: &str) -> Option<i64> {
    if interval.len() < 2 {
        return None;
    }
    let (num, unit) = interval.split_at(interval.len() - 1);
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(n * mult)
}

/// **Can the still-forming-bar guard MEASURE this step?** [`interval_ms`] with the value thrown
/// away, given a name — because this question is asked by four separate REFUSALS in this workspace
/// and the answer is the whole of
/// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s bug B.
///
/// The argument, once, here, rather than re-derived at each caller:
///
/// * A kline collector drops the venue's still-forming last candle by comparing `bar.ts + step`
///   against "now", and the step comes from [`interval_ms`], which reads a count plus ONE of
///   `s`/`m`/`h`/`d`. `1w`, `1M` and `1mo` have no width there.
/// * The guard therefore DECLINES on those spellings rather than acting — deliberately, since a
///   defensive filter must never panic or drop data on a string it does not understand
///   (`crates/vike-backfill/src/klines.rs`'s `unparseable_interval_leaves_bars_untouched` pins
///   exactly that).
/// * So the DECISION belongs to whoever is about to write, and what it costs if nobody makes it is
///   permanent: the venue's open candle is stored as a closed bar AND the window spends its commit
///   key, so `vike_data::DataFusionHist`'s `commit_rows` answers a corrective re-fetch with `Ok(0)`
///   — a silent success over the wrong row, with no verb in this workspace that retires a single
///   key.
///
/// ⚠ **Widening [`interval_ms`] is NOT the fix and is argued against in-tree**: that parser is also
/// a supervisor cadence validator, a resample step and a page-span estimate, and a calendar month
/// is not a multiple of anything. 0059 records both reasons. This predicate exists so the four
/// callers name one question instead of four spellings of `interval_ms(..).is_none()`, and so a
/// gate can SEE which writers consult it — `crates/vike-ops/tests/kline_ingest_gate.rs` derives the
/// bar producers from `crates/vike-data/src/store_kind.rs` and demands every one of them either
/// name this function or carry a written row saying why it need not.
///
/// ⚠ It answers about the STEP VOCABULARY, never about a venue: `4h` measures here and is refused
/// by `crates/vike-backfill/src/bin/hyperliquid_backfill.rs`'s `INTERVALS` all the same, and
/// `interval=funding` (the reserved label `crates/vike-backfill/src/funding_rate.rs` writes bars
/// under) does not measure and is not a candle at all.
#[must_use]
pub fn measures_bar_step(interval: &str) -> bool {
    interval_ms(interval).is_some()
}

/// A window span as a `[walkforward]` profile spells it — the duration grammar of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §11.
///
/// ⚠ A SEPARATE grammar from [`interval_ms`], deliberately. That one answers "how long is a
/// BAR" and has roughly fourteen production callers — the hist store's interval parse, the
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant formatter, and the property that makes it safe to sit beside
    /// [`epoch_ms_to_utc_date`]: the two never disagree about which DAY an instant belongs to,
    /// including across the pre-1970 sign boundary where a naive `/` would skew by a day.
    #[test]
    fn the_utc_timestamp_agrees_with_the_utc_date_on_every_boundary() {
        assert_eq!(epoch_ms_to_utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(epoch_ms_to_utc_timestamp(-1), "1969-12-31T23:59:59Z");
        assert_eq!(epoch_ms_to_utc_timestamp(86_399_999), "1970-01-01T23:59:59Z");
        assert_eq!(epoch_ms_to_utc_timestamp(1_787_356_800_000), "2026-08-22T00:00:00Z");
        assert_eq!(
            epoch_ms_to_utc_timestamp(1_787_356_800_000 + 45_296_000),
            "2026-08-22T12:34:56Z"
        );
        // Sub-second precision is dropped rather than rounded: a truncation cannot move an instant
        // across a day boundary, which a round-half-up would at 23:59:59.500.
        assert_eq!(epoch_ms_to_utc_timestamp(86_399_500), "1970-01-01T23:59:59Z");
        for ms in [-86_400_001i64, -1, 0, 1, 86_399_999, 1_787_356_800_123] {
            let stamp = epoch_ms_to_utc_timestamp(ms);
            assert_eq!(&stamp[..10], epoch_ms_to_utc_date(ms), "day halves disagree at {ms}");
        }
    }

    #[test]
    fn interval_ms_units_and_rejects() {
        assert_eq!(interval_ms("30s"), Some(30_000));
        assert_eq!(interval_ms("1s"), Some(1_000));
        assert_eq!(interval_ms("1m"), Some(60_000));
        assert_eq!(interval_ms("5m"), Some(300_000));
        assert_eq!(interval_ms("15m"), Some(900_000));
        assert_eq!(interval_ms("1h"), Some(3_600_000));
        assert_eq!(interval_ms("4h"), Some(14_400_000));
        assert_eq!(interval_ms("1d"), Some(86_400_000));
        // malformed → None
        assert_eq!(interval_ms("m"), None); // len < 2
        assert_eq!(interval_ms("1x"), None); // unknown unit
        assert_eq!(interval_ms("-1m"), None); // non-digit count
        assert_eq!(interval_ms("1.5m"), None); // non-digit count
        assert_eq!(interval_ms("xm"), None); // empty/non-digit count
    }

    #[test]
    fn epoch_and_day_boundaries() {
        assert_eq!(epoch_ms_to_utc_date(0), "1970-01-01");
        assert_eq!(epoch_ms_to_utc_date(86_400_000 - 1), "1970-01-01"); // last ms of day 0
        assert_eq!(epoch_ms_to_utc_date(86_400_000), "1970-01-02"); // first ms of day 1
    }

    #[test]
    fn pre_epoch_floors_toward_negative_infinity() {
        assert_eq!(epoch_ms_to_utc_date(-1), "1969-12-31");
        assert_eq!(epoch_ms_to_utc_date(-86_400_000), "1969-12-31");
        assert_eq!(epoch_ms_to_utc_date(-86_400_000 - 1), "1969-12-30");
    }

    #[test]
    fn floor_ms_to_utc_day_matches_the_date_string_floor() {
        // floor_ms_to_utc_day(ms) must be the epoch-ms whose epoch_ms_to_utc_date is the same day,
        // and re-flooring the floored value must be a no-op (idempotent).
        for ms in [0, 1, 86_400_000 - 1, 86_400_000, -1, -86_400_000, 1_700_000_500_123_i64] {
            let floored = floor_ms_to_utc_day(ms);
            assert_eq!(epoch_ms_to_utc_date(floored), epoch_ms_to_utc_date(ms));
            assert_eq!(floor_ms_to_utc_day(floored), floored, "idempotent");
            assert!(floored <= ms, "floor never moves forward in time");
        }
        assert_eq!(floor_ms_to_utc_day(0), 0);
        assert_eq!(floor_ms_to_utc_day(86_400_000 - 1), 0);
        assert_eq!(floor_ms_to_utc_day(86_400_000), 86_400_000);
        assert_eq!(floor_ms_to_utc_day(-1), -86_400_000, "pre-epoch floors toward -inf");
    }

    /// The `utc_weekday` anchor, derived rather than asserted: 1970-01-01 is a Thursday, and every
    /// weekday index is checked against a date whose day-of-week is a known calendar fact.
    #[test]
    fn utc_weekday_monday_is_zero() {
        assert_eq!(utc_weekday(0), 3, "1970-01-01 was a Thursday");
        // 2024-01-01 was a Monday; walk a full week from it.
        let monday = days_from_civil(2024, 1, 1) * 86_400_000;
        for (i, ms) in (0..7).map(|d| (d, monday + d * 86_400_000)) {
            assert_eq!(utc_weekday(ms), i as u32, "day +{i} from Monday 2024-01-01");
        }
        // stable across the whole day, and wraps back to Monday after seven
        assert_eq!(utc_weekday(monday + 86_400_000 - 1), 0);
        assert_eq!(utc_weekday(monday + 7 * 86_400_000), 0);
        // pre-1970 floors toward -inf: the day before Thursday 1970-01-01 is a Wednesday.
        assert_eq!(utc_weekday(-1), 2);
    }

    #[test]
    fn leap_day_2000() {
        assert_eq!(epoch_ms_to_utc_date(951_782_400_000), "2000-02-29");
        assert_eq!(epoch_ms_to_utc_date(951_868_800_000), "2000-03-01");
    }

    #[test]
    fn known_recent_dates() {
        assert_eq!(epoch_ms_to_utc_date(1_609_459_200_000), "2021-01-01");
        assert_eq!(epoch_ms_to_utc_date(1_609_459_200_000 - 1), "2020-12-31");
    }

    #[test]
    fn civil_roundtrips_days() {
        // days_from_civil ∘ civil_from_days == identity across a wide range.
        for z in [-146_097_i64, -719_468, -1, 0, 1, 18_628, 100_000, 999_999] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z, "roundtrip failed at z={z}");
        }
    }

    #[test]
    fn ns_date_matches_ms_date() {
        let ms = 1_609_459_200_000_i64;
        assert_eq!(epoch_ns_to_utc_date(ms * 1_000_000), epoch_ms_to_utc_date(ms));
    }

    #[test]
    fn parse_hour_label_validates_ranges() {
        assert_eq!(parse_hour_label("2026-07-12T05"), Some((2026, 7, 12, 5)));
        assert_eq!(parse_hour_label("2026-13-45T05"), None); // out-of-range month/day
        assert_eq!(parse_hour_label("2026-07-12T24"), None); // out-of-range hour
        assert_eq!(parse_hour_label("garbage"), None);
    }

    #[test]
    fn days_in_range_spans_leap_day() {
        // Reproduces the old vike-backfill tardis `days_in_range` output across a leap-day boundary.
        assert_eq!(
            days_in_range((2024, 2, 27), (2024, 3, 1)),
            vec![(2024, 2, 27), (2024, 2, 28), (2024, 2, 29), (2024, 3, 1)]
        );
        assert_eq!(days_in_range((2023, 12, 31), (2024, 1, 1)), vec![(2023, 12, 31), (2024, 1, 1)]);
        assert_eq!(days_in_range((2024, 5, 5), (2024, 5, 5)), vec![(2024, 5, 5)]);
        // start after end → empty (the primitives-based range; the old tardis copy returned
        // `vec![start]` here — a benign, documented behavior change).
        assert!(days_in_range((2024, 5, 6), (2024, 5, 5)).is_empty());
    }

    #[test]
    fn parse_date_label_epoch_ms_passthrough() {
        assert_eq!(parse_date_label("1704067200000"), Ok(1_704_067_200_000));
        assert_eq!(parse_date_label("0"), Ok(0));
        assert_eq!(parse_date_label("-1"), Ok(-1)); // pre-1970 epoch-ms allowed
        assert_eq!(parse_date_label("  42  "), Ok(42)); // trims surrounding whitespace
    }

    #[test]
    fn parse_date_label_ymd_to_midnight_utc_ms() {
        assert_eq!(parse_date_label("1970-01-01"), Ok(0));
        assert_eq!(parse_date_label("2021-01-01"), Ok(1_609_459_200_000));
        assert_eq!(parse_date_label("2024-01-01"), Ok(1_704_067_200_000));
        // leap day maps to its own midnight-UTC instant.
        assert_eq!(parse_date_label("2000-02-29"), Ok(951_782_400_000));
    }

    #[test]
    fn parse_date_label_rejects_garbage() {
        assert!(parse_date_label("garbage").is_err());
        assert!(parse_date_label("2024-01").is_err()); // wrong field count
        assert!(parse_date_label("2024-13-01").is_err()); // out-of-range month
        assert!(parse_date_label("2024-01-32").is_err()); // out-of-range day
        assert!(parse_date_label("2024-xx-01").is_err()); // non-numeric field
    }

    #[test]
    fn parse_ymd_validated_tuple() {
        assert_eq!(parse_ymd("2024-02-29"), Ok((2024, 2, 29)));
        assert_eq!(parse_ymd(" 2023-12-31 "), Ok((2023, 12, 31)));
        assert!(parse_ymd("1704067200000").is_err()); // an epoch-ms is not a YYYY-MM-DD
        assert!(parse_ymd("2024-00-10").is_err()); // month 0 out of range
    }

    #[test]
    fn hours_in_range_labels_and_rollovers() {
        assert_eq!(hours_in_range("2026-04-13T22", "2026-04-13T22"), vec!["2026-04-13T22"]);
        assert_eq!(
            hours_in_range("2026-04-13T22", "2026-04-14T01"),
            vec!["2026-04-13T22", "2026-04-13T23", "2026-04-14T00", "2026-04-14T01"]
        );
        assert_eq!(
            hours_in_range("2026-01-31T23", "2026-02-01T01"),
            vec!["2026-01-31T23", "2026-02-01T00", "2026-02-01T01"]
        );
        assert!(hours_in_range("2026-04-14T01", "2026-04-13T22").is_empty());
        assert!(hours_in_range("garbage", "2026-04-13T22").is_empty());
        assert!(hours_in_range("2026-04-13T22", "not-a-date").is_empty());
        assert!(hours_in_range("2026-04-13", "2026-04-13T22").is_empty()); // missing T-hour
    }

    /// Every suffix the `[walkforward]` duration grammar accepts, and the refusal that is the
    /// point of it existing separately from `interval_ms`: a bare number must not silently
    /// become bars or days.
    #[test]
    fn parse_span_accepts_every_suffix_and_refuses_a_bare_number() {
        assert_eq!(parse_span("90m"), Ok(Span::Ms(90 * 60_000)));
        assert_eq!(parse_span("4h"), Ok(Span::Ms(4 * 3_600_000)));
        assert_eq!(parse_span("30d"), Ok(Span::Ms(30 * 86_400_000)));
        assert_eq!(parse_span("2w"), Ok(Span::Ms(2 * 7 * 86_400_000)));
        assert_eq!(parse_span("3mo"), Ok(Span::Months(3)));
        assert_eq!(parse_span("1y"), Ok(Span::Months(12)));
        assert_eq!(parse_span("5000bars"), Ok(Span::Bars(5000)));
        assert_eq!(parse_span(" 12MO "), Ok(Span::Months(12))); // trimmed, case-insensitive
        let e = parse_span("90").unwrap_err();
        assert!(e.contains("no unit") && e.contains("90bars"), "{e}");
        assert!(parse_span("1s").is_err()); // `s` is deliberately NOT in this grammar
        assert!(parse_span("1x").is_err());
        assert!(parse_span("").is_err());
    }

    /// The two traps this grammar exists to not inherit. `interval_ms("0m")` answers `Some(0)`
    /// — a zero-length window makes every boundary the same instant — and `1M` is the
    /// minutes/months case collision the spec bans by name.
    #[test]
    fn parse_span_refuses_zero_and_refuses_uppercase_m_by_name() {
        assert_eq!(interval_ms("0m"), Some(0)); // the hole this grammar must not copy
        let z = parse_span("0m").unwrap_err();
        assert!(z.contains("zero"), "{z}");
        assert!(parse_span("0bars").is_err());
        let m = parse_span("1M").unwrap_err();
        assert!(m.contains("mo") && m.contains("minute"), "{m}");
    }

    /// Calendar months clamp to the shorter month and are exact across a leap year — the
    /// property that makes `mo` a different answer from `30d`.
    #[test]
    fn add_calendar_months_clamps_a_short_month_and_keeps_the_time_of_day() {
        let at = |y, m, d, tod: i64| days_from_civil(y, m, d) * 86_400_000 + tod;
        assert_eq!(add_calendar_months(at(2026, 1, 31, 0), 1), at(2026, 2, 28, 0));
        assert_eq!(add_calendar_months(at(2024, 1, 31, 0), 1), at(2024, 2, 29, 0)); // leap
        assert_eq!(add_calendar_months(at(2024, 3, 31, 0), 1), at(2024, 4, 30, 0));
        assert_eq!(add_calendar_months(at(2024, 2, 29, 0), 12), at(2025, 2, 28, 0));
        assert_eq!(add_calendar_months(at(2026, 6, 15, 0), 6), at(2026, 12, 15, 0));
        assert_eq!(add_calendar_months(at(2026, 12, 15, 0), 1), at(2027, 1, 15, 0));
        assert_eq!(add_calendar_months(at(2026, 1, 31, 3_600_000), 1), at(2026, 2, 28, 3_600_000));
    }

    /// [`measures_bar_step`] is [`interval_ms`] with the value dropped — asserted as an
    /// EQUIVALENCE rather than by re-listing the vocabulary, so the predicate can never drift from
    /// the parser it names, and then pinned on the three spellings
    /// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s bug B is
    /// about.
    #[test]
    fn the_bar_step_predicate_is_the_parser_with_the_value_dropped() {
        for iv in [
            "1m", "5m", "30s", "1h", "4h", "1d", "7d", "1w", "1M", "1mo", "funding", "", "m", "0m",
            "-1m", "1x", "1.5h",
        ] {
            assert_eq!(
                measures_bar_step(iv),
                interval_ms(iv).is_some(),
                "the predicate and the parser disagree about {iv:?}"
            );
        }
    }

    /// The three spellings bug B is about, named one at a time — a fold over a list would pass over
    /// an empty one, and these three are the claim.
    ///
    /// `1w`, `1M` and `1mo` are REAL intervals that three dispatched venues serve, which is why the
    /// answer "the guard declines" was right for a garbage string and wrong for these. `7d` is here
    /// as the contrast: a week the vocabulary CAN measure, and the step an operator who wants
    /// weekly bars from a measurable window should ask for.
    #[test]
    fn a_week_and_a_month_have_no_bar_width_but_seven_days_does() {
        assert!(!measures_bar_step("1w"), "a week is outside the s/m/h/d vocabulary");
        assert!(!measures_bar_step("1M"), "a calendar month is not a multiple of anything");
        assert!(!measures_bar_step("1mo"), "the two-character month spelling is outside it too");
        assert!(measures_bar_step("7d"), "seven days is a plain multiple and measures fine");
        assert!(
            !measures_bar_step("funding"),
            "the reserved funding-rate label is not a step and must never be read as one"
        );
    }
}
