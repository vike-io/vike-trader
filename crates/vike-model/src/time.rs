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
/// a resample step and a page-span estimate (and was a cadence validator for the collector
/// supervisor docs/decisions/0094 deleted), and a calendar month is not a multiple of anything. 0059 records both reasons. This predicate exists so the four
/// callers name one question instead of four spellings of `interval_ms(..).is_none()`, and so a
/// gate can SEE which writers consult it — `crates/vike-ops/tests/kline_ingest_gate.rs` derives the
/// bar producers from `crates/vike-data/src/store_kind.rs` and demands every one of them either
/// name this function or carry a written row saying why it need not.
///
/// ⚠ It answers about the STEP VOCABULARY, never about a venue: a step that measures here can still
/// be one a venue does not serve (the deleted one-shot `hyperliquid_backfill` program's own
/// allow-list refused `4h` all the same), and `interval=funding` (the reserved label
/// `crates/vike-backfill/src/funding_rate.rs` writes bars under) does not measure and is not a
/// candle at all.
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

#[path = "time_tests.rs"]
#[cfg(test)]
mod time_tests;
