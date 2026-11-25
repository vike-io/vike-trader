//! **The display time zone**: the zone every window draws its dates and clocks in (the chart's axis
//! labels, grid marks and clocks, the calendar's week, the execution history's times). One GLOBAL
//! setting (`preferences.display_timezone`; ChartStates are shared across windows, so marks caches
//! cannot be per-window). Default `Local` == pre-feature behavior.
//!
//! It lives in the theme crate, the display vocabulary every window reads, so a window that only
//! needs a clock does not depend on the chart (it moved here from `vike-chart`'s `tz.rs`).
use chrono::{NaiveDateTime, TimeZone};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ThemeDisplayTimeZone {
    #[default]
    Local,
    Utc,
    Named(chrono_tz::Tz),
}

impl ThemeDisplayTimeZone {
    /// UI + persistence order.
    pub const CURATED: [ThemeDisplayTimeZone; 13] = [
        ThemeDisplayTimeZone::Local,
        ThemeDisplayTimeZone::Utc,
        ThemeDisplayTimeZone::Named(chrono_tz::Europe::Dublin),
        ThemeDisplayTimeZone::Named(chrono_tz::Europe::London),
        ThemeDisplayTimeZone::Named(chrono_tz::Europe::Berlin),
        ThemeDisplayTimeZone::Named(chrono_tz::America::New_York),
        ThemeDisplayTimeZone::Named(chrono_tz::America::Chicago),
        ThemeDisplayTimeZone::Named(chrono_tz::America::Los_Angeles),
        ThemeDisplayTimeZone::Named(chrono_tz::Asia::Tokyo),
        ThemeDisplayTimeZone::Named(chrono_tz::Asia::Hong_Kong),
        ThemeDisplayTimeZone::Named(chrono_tz::Asia::Singapore),
        ThemeDisplayTimeZone::Named(chrono_tz::Asia::Kolkata),
        ThemeDisplayTimeZone::Named(chrono_tz::Australia::Sydney),
    ];
    pub fn label(self) -> &'static str {
        match self {
            ThemeDisplayTimeZone::Local => "Local",
            ThemeDisplayTimeZone::Utc => "UTC",
            ThemeDisplayTimeZone::Named(time_zone) => match time_zone.name().rsplit('/').next() {
                Some(city) => match city {
                    "New_York" => "New York",
                    "Los_Angeles" => "Los Angeles",
                    "Hong_Kong" => "Hong Kong",
                    other => other,
                },
                None => time_zone.name(),
            },
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            ThemeDisplayTimeZone::Local => "Local",
            ThemeDisplayTimeZone::Utc => "UTC",
            ThemeDisplayTimeZone::Named(time_zone) => time_zone.name(),
        }
    }
    pub fn parse(s: &str) -> ThemeDisplayTimeZone {
        match s {
            "Local" => ThemeDisplayTimeZone::Local,
            "UTC" => ThemeDisplayTimeZone::Utc,
            other => other
                .parse::<chrono_tz::Tz>()
                .map(ThemeDisplayTimeZone::Named)
                .unwrap_or(ThemeDisplayTimeZone::Local),
        }
    }
    /// The zone's identity in `ChartState`'s cache key, ONE key per zone: a key two zones share
    /// would leave the first zone's hour/day marks standing after a switch to the second. In-memory
    /// only, never stored, so a `chrono_tz` release reordering its zones moves nothing.
    pub fn time_zone_key(self) -> u32 {
        match self {
            ThemeDisplayTimeZone::Local => 0,
            ThemeDisplayTimeZone::Utc => 1,
            // `Tz` is a field-less enum: its discriminant is unique per IANA zone, no hash to collide.
            ThemeDisplayTimeZone::Named(time_zone) => 2 + time_zone as u32,
        }
    }
}

/// Wall-clock datetime of `timestamp_ms` in `time_zone`. The ONE conversion site — every
/// formatter/mark predicate goes through this so all display agrees.
pub fn to_naive(timestamp_ms: i64, time_zone: ThemeDisplayTimeZone) -> Option<NaiveDateTime> {
    let utc = chrono::DateTime::from_timestamp_millis(timestamp_ms)?;
    Some(match time_zone {
        ThemeDisplayTimeZone::Local => utc.with_timezone(&chrono::Local).naive_local(),
        ThemeDisplayTimeZone::Utc => utc.naive_utc(),
        ThemeDisplayTimeZone::Named(z) => z.from_utc_datetime(&utc.naive_utc()).naive_local(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn kolkata_shifts_wall_clock_minutes() {
        // 12:00:00 UTC == 17:30 in Asia/Kolkata (+5:30) — minute-of-hour moves.
        let timestamp = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let utc = to_naive(timestamp, ThemeDisplayTimeZone::Utc).unwrap();
        let kol =
            to_naive(timestamp, ThemeDisplayTimeZone::Named(chrono_tz::Asia::Kolkata)).unwrap();
        assert_eq!(utc.minute(), 0);
        assert_eq!(kol.minute(), 30);
        assert_eq!(kol.hour(), 17);
    }
    #[test]
    fn parse_roundtrips_every_curated_zone() {
        for time_zone in ThemeDisplayTimeZone::CURATED {
            assert_eq!(
                ThemeDisplayTimeZone::parse(time_zone.name()),
                time_zone,
                "roundtrip failed for {}",
                time_zone.name()
            );
        }
        assert_eq!(ThemeDisplayTimeZone::parse("Not/AZone"), ThemeDisplayTimeZone::Local);
    }
    #[test]
    fn time_zone_keys_are_unique_and_stable() {
        let mut keys: Vec<u32> =
            ThemeDisplayTimeZone::CURATED.iter().map(|t| t.time_zone_key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ThemeDisplayTimeZone::CURATED.len());
        assert_eq!(ThemeDisplayTimeZone::Local.time_zone_key(), 0);
        assert_eq!(ThemeDisplayTimeZone::Utc.time_zone_key(), 1);
    }
    #[test]
    fn every_zone_parse_accepts_has_its_own_key() {
        // `parse` takes any IANA name, not just the curated 13; a key two zones share leaves stale
        // hour/day marks on a switch between them (a non-curated name was once hashed to 16 bits).
        let zones = [ThemeDisplayTimeZone::Local, ThemeDisplayTimeZone::Utc].into_iter().chain(
            chrono_tz::TZ_VARIANTS.iter().map(|time_zone| ThemeDisplayTimeZone::Named(*time_zone)),
        );
        let mut owner = std::collections::HashMap::new();
        for time_zone in zones {
            if let Some(other) = owner.insert(time_zone.time_zone_key(), time_zone) {
                panic!(
                    "{} and {} share the key {}",
                    other.name(),
                    time_zone.name(),
                    time_zone.time_zone_key()
                );
            }
        }
    }
}
