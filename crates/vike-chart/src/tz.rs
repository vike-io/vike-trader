//! Display-timezone selection for axis labels, grid marks, and clocks.
//! One GLOBAL setting (ChartStates are shared across windows, so marks caches
//! cannot be per-window). Default `Local` == pre-feature behavior.
use chrono::{NaiveDateTime, TimeZone};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DisplayTz {
    #[default]
    Local,
    Utc,
    Named(chrono_tz::Tz),
}

impl DisplayTz {
    /// UI + persistence order. Index is load-bearing for `tz_key` — append only.
    pub const CURATED: [DisplayTz; 13] = [
        DisplayTz::Local,
        DisplayTz::Utc,
        DisplayTz::Named(chrono_tz::Europe::Dublin),
        DisplayTz::Named(chrono_tz::Europe::London),
        DisplayTz::Named(chrono_tz::Europe::Berlin),
        DisplayTz::Named(chrono_tz::America::New_York),
        DisplayTz::Named(chrono_tz::America::Chicago),
        DisplayTz::Named(chrono_tz::America::Los_Angeles),
        DisplayTz::Named(chrono_tz::Asia::Tokyo),
        DisplayTz::Named(chrono_tz::Asia::Hong_Kong),
        DisplayTz::Named(chrono_tz::Asia::Singapore),
        DisplayTz::Named(chrono_tz::Asia::Kolkata),
        DisplayTz::Named(chrono_tz::Australia::Sydney),
    ];
    pub fn label(self) -> &'static str {
        match self {
            DisplayTz::Local => "Local",
            DisplayTz::Utc => "UTC",
            DisplayTz::Named(tz) => match tz.name().rsplit('/').next() {
                Some(city) => match city {
                    "New_York" => "New York",
                    "Los_Angeles" => "Los Angeles",
                    "Hong_Kong" => "Hong Kong",
                    other => other,
                },
                None => tz.name(),
            },
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            DisplayTz::Local => "Local",
            DisplayTz::Utc => "UTC",
            DisplayTz::Named(tz) => tz.name(),
        }
    }
    pub fn parse(s: &str) -> DisplayTz {
        match s {
            "Local" => DisplayTz::Local,
            "UTC" => DisplayTz::Utc,
            other => {
                other.parse::<chrono_tz::Tz>().map(DisplayTz::Named).unwrap_or(DisplayTz::Local)
            }
        }
    }
    pub fn tz_key(self) -> u32 {
        match self {
            DisplayTz::Local => 0,
            DisplayTz::Utc => 1,
            DisplayTz::Named(tz) => {
                match Self::CURATED.iter().position(|c| *c == DisplayTz::Named(tz)) {
                    Some(i) => i as u32, // curated named zones start at index 2 → keys 2..
                    None => 2 + u32::from(crc16(tz.name())), // non-curated (persistence only): stable per name
                }
            }
        }
    }
}

/// Tiny stable hash for non-curated zone names (cache identity only — collisions merely
/// share a cache slot across two exotic zones, never corrupt data).
fn crc16(s: &str) -> u16 {
    s.bytes().fold(0u16, |acc, b| {
        let mut x = acc ^ u16::from(b);
        for _ in 0..8 {
            x = if x & 1 != 0 { (x >> 1) ^ 0xA001 } else { x >> 1 };
        }
        x
    })
}

/// Wall-clock datetime of `ts_ms` in `tz`. The ONE conversion site — every
/// formatter/mark predicate goes through this so all display agrees.
pub fn to_naive(ts_ms: i64, tz: DisplayTz) -> Option<NaiveDateTime> {
    let utc = chrono::DateTime::from_timestamp_millis(ts_ms)?;
    Some(match tz {
        DisplayTz::Local => utc.with_timezone(&chrono::Local).naive_local(),
        DisplayTz::Utc => utc.naive_utc(),
        DisplayTz::Named(z) => z.from_utc_datetime(&utc.naive_utc()).naive_local(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn kolkata_shifts_wall_clock_minutes() {
        // 12:00:00 UTC == 17:30 in Asia/Kolkata (+5:30) — minute-of-hour moves.
        let ts = chrono::DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let utc = to_naive(ts, DisplayTz::Utc).unwrap();
        let kol = to_naive(ts, DisplayTz::Named(chrono_tz::Asia::Kolkata)).unwrap();
        assert_eq!(utc.minute(), 0);
        assert_eq!(kol.minute(), 30);
        assert_eq!(kol.hour(), 17);
    }
    #[test]
    fn parse_roundtrips_every_curated_zone() {
        for tz in DisplayTz::CURATED {
            assert_eq!(DisplayTz::parse(tz.name()), tz, "roundtrip failed for {}", tz.name());
        }
        assert_eq!(DisplayTz::parse("Not/AZone"), DisplayTz::Local);
    }
    #[test]
    fn tz_keys_are_unique_and_stable() {
        let mut keys: Vec<u32> = DisplayTz::CURATED.iter().map(|t| t.tz_key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), DisplayTz::CURATED.len());
        assert_eq!(DisplayTz::Local.tz_key(), 0);
        assert_eq!(DisplayTz::Utc.tz_key(), 1);
    }
}
