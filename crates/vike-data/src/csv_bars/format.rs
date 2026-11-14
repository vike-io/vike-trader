//! `CsvBarsFormat`: how a file's cells are written, the EXPLICIT options a regional export needs (Excel with
//! Russian or European settings: `;` between fields, `,` as the decimal mark, a space or `.` between
//! thousands, text dates such as `09.11.2026 10:00`). The owner's addition of 2026-11-09 to decision 0268.
//!
//! Nothing here reads the machine's locale: every option is a word its caller passes
//! ([`CsvBarsFormatWords`], the one grammar the `csv-bars` import and a profile's `data.file_format`
//! share), and with none passed a file in a regional form is refused by the decoder, naming the option
//! to set. A combination that cannot be read one way only is refused when the format is built.

use chrono::TimeZone;

use super::CsvBarsFormatWords;

/// A file's written form, built only by [`CsvBarsFormat::parse`] (or [`Default`], the plain form: `,`
/// between fields, `.` as the decimal mark, no thousands separator, integer epoch times), so a format
/// that cannot be read one way does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvBarsFormat {
    pub(super) delimiter: char,
    pub(super) decimal: char,
    pub(super) thousands: Option<char>,
    pub(super) time: Option<CsvBarsTimePattern>,
    zone: Zone,
}

impl Default for CsvBarsFormat {
    fn default() -> Self {
        CsvBarsFormat {
            delimiter: ',',
            decimal: '.',
            thousands: None,
            time: None,
            zone: Zone::Offset(0),
        }
    }
}

/// Where a text time's clock is: a fixed offset east of UTC in seconds, or a named zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Offset(i64),
    Named(chrono_tz::Tz),
}

impl CsvBarsFormat {
    /// Build the format the words name.
    ///
    /// # Errors
    /// The sentence naming the word that is wrong, or the two options that collide: a decimal mark or
    /// a thousands separator equal to the delimiter, or a thousands separator equal to the decimal mark,
    /// would make one cell readable two ways; a zone without a text time names the zone of nothing (an
    /// epoch is UTC).
    pub fn parse(words: &CsvBarsFormatWords) -> Result<Self, String> {
        let delimiter = match words.delimiter.as_deref() {
            None | Some(",") => ',',
            Some(";") => ';',
            Some("tab" | "\t") => '\t',
            Some(other) => {
                return Err(format!(
                    "delimiter {other:?} is not one of `,` (the default), `;` or `tab`"
                ));
            }
        };
        let decimal = match words.decimal.as_deref() {
            None | Some(".") => '.',
            Some(",") => ',',
            Some(other) => {
                return Err(format!("decimal {other:?} is not `.` (the default) or `,`"));
            }
        };
        let thousands = match words.thousands.as_deref() {
            None => None,
            Some("space") => Some(' '),
            Some(".") => Some('.'),
            Some(",") => Some(','),
            Some(other) => {
                return Err(format!(
                    "thousands {other:?} is not `space`, `.` or `,` (absent: no separator)"
                ));
            }
        };
        if decimal == delimiter {
            return Err(format!(
                "decimal `{decimal}` is also the delimiter, so a number would split into two cells"
            ));
        }
        if let Some(t) = thousands {
            if t == decimal {
                return Err(format!(
                    "thousands `{t}` is also the decimal mark, so `1{t}234` would read two ways"
                ));
            }
            if t == delimiter {
                return Err(format!(
                    "thousands `{t}` is also the delimiter, so a number would split into cells"
                ));
            }
        }
        let time = match words.time.as_deref() {
            None | Some("epoch") => None,
            Some(pattern) => Some(CsvBarsTimePattern::parse(pattern)?),
        };
        let zone = match words.tz.as_deref() {
            None => Zone::Offset(0),
            Some(_) if time.is_none() => {
                return Err("tz names the zone of a TEXT time, and these times are integer \
                            epochs, which are UTC: set the time option, or drop tz"
                    .to_string());
            }
            Some(tz) => Zone::parse(tz)?,
        };
        Ok(CsvBarsFormat { delimiter, decimal, thousands, time, zone })
    }

    /// `cell` as a number in this format, or `None`: a thousands separator only between whole groups of
    /// three digits, the decimal mark this format's and no other.
    pub(super) fn number(&self, cell: &str) -> Option<f64> {
        let other_mark = if self.decimal == '.' { ',' } else { '.' };
        let mut s = match self.thousands {
            Some(sep) => self.ungroup(cell, sep)?,
            None => cell.to_string(),
        };
        if s.contains(other_mark) {
            return None;
        }
        if self.decimal == ',' {
            s = s.replace(',', ".");
        }
        s.parse::<f64>().ok().filter(|x| x.is_finite())
    }

    /// `cell` with its thousands separators taken out, `None` when one sits anywhere but between
    /// whole groups of three digits of the integer part.
    fn ungroup(&self, cell: &str, sep: char) -> Option<String> {
        let is_sep = |c: char| match sep {
            ' ' => matches!(c, ' ' | '\u{a0}' | '\u{202f}'),
            sep => c == sep,
        };
        let (sign, rest) = cell.strip_prefix('-').map_or(("", cell), |r| ("-", r));
        let (int, frac) = rest.find(self.decimal).map_or((rest, ""), |i| rest.split_at(i));
        if frac.contains(is_sep) {
            return None;
        }
        let groups: Vec<&str> = int.split(is_sep).collect();
        if groups.len() > 1 {
            let digits = |g: &&str| g.bytes().all(|b| b.is_ascii_digit());
            let grouped = (1..=3).contains(&groups[0].len())
                && groups[1..].iter().all(|g| g.len() == 3)
                && groups.iter().all(digits);
            if !grouped {
                return None;
            }
        }
        Some(format!("{sign}{}{frac}", groups.concat()))
    }

    /// A text time in this format's pattern and zone, as epoch ms, `Err` saying why it is not one; `None`
    /// for an epoch format, whose times the decoder reads itself.
    pub(super) fn text_time(&self, cell: &str) -> Option<Result<i64, String>> {
        let pattern = self.time.as_ref()?;
        Some(pattern.read(cell).and_then(|local| self.zone.to_utc_ms(local)))
    }
}

impl Zone {
    fn parse(tz: &str) -> Result<Zone, String> {
        if tz == "UTC" {
            return Ok(Zone::Offset(0));
        }
        let sign = match tz.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => 0,
        };
        if sign != 0 {
            let b = tz.as_bytes();
            let two = |i: usize| -> Option<i64> {
                let (h, l) = (*b.get(i)?, *b.get(i + 1)?);
                (h.is_ascii_digit() && l.is_ascii_digit())
                    .then(|| i64::from((h - b'0') * 10 + l - b'0'))
            };
            return match (tz.len(), two(1), b.get(3).copied(), two(4)) {
                (6, Some(h), Some(b':'), Some(m)) if h <= 14 && m <= 59 => {
                    Ok(Zone::Offset(sign * (h * 3_600 + m * 60)))
                }
                _ => {
                    Err(format!("tz {tz:?} is not a fixed offset `+HH:MM` or `-HH:MM` up to 14:00"))
                }
            };
        }
        tz.parse::<chrono_tz::Tz>().map(Zone::Named).map_err(|_| {
            format!(
                "tz {tz:?} is not `UTC`, a fixed offset `+HH:MM` / `-HH:MM`, or an IANA zone name \
                 such as `Europe/Berlin`"
            )
        })
    }

    /// The UTC instant of a local wall-clock reading, refused when the zone's clock passed it twice or
    /// never (a clock change), which no reading of the file can settle.
    fn to_utc_ms(self, wall: Wall) -> Result<i64, String> {
        let Wall { y, mo, d, h, mi, s, ms } = wall;
        match self {
            Zone::Offset(secs) => {
                let days = vike_utility::calendar::days_from_civil(y, mo, d);
                let wall =
                    ((days * 24 + i64::from(h)) * 60 + i64::from(mi)) * 60 + i64::from(s) - secs;
                Ok(wall * 1_000 + i64::from(ms))
            }
            Zone::Named(tz) => {
                let naive = i32::try_from(y)
                    .ok()
                    .and_then(|y| chrono::NaiveDate::from_ymd_opt(y, mo, d))
                    .and_then(|date| date.and_hms_milli_opt(h, mi, s, ms))
                    .ok_or_else(|| "is no date and time".to_string())?;
                match tz.from_local_datetime(&naive) {
                    chrono::LocalResult::Single(t) => Ok(t.timestamp_millis()),
                    chrono::LocalResult::Ambiguous(..) => Err(format!(
                        "happened twice in {tz} (the clock went back), and the file does not say \
                         which"
                    )),
                    chrono::LocalResult::None => {
                        Err(format!("never happened in {tz} (the clock went forward)"))
                    }
                }
            }
        }
    }
}

/// A wall-clock reading out of a text time, every field already in range.
#[derive(Debug, Clone, Copy)]
struct Wall {
    y: i64,
    mo: u32,
    d: u32,
    h: u32,
    mi: u32,
    s: u32,
    ms: u32,
}

/// One piece of a time pattern: a fixed-width number, or a character that must be there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    Milli,
    Lit(char),
}

/// The tokens, longest first, and their width in digits.
const TOKENS: [(&str, Piece, usize); 7] = [
    ("YYYY", Piece::Year, 4),
    ("SSS", Piece::Milli, 3),
    ("MM", Piece::Month, 2),
    ("DD", Piece::Day, 2),
    ("HH", Piece::Hour, 2),
    ("mm", Piece::Minute, 2),
    ("ss", Piece::Second, 2),
];

/// A parsed `time` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CsvBarsTimePattern {
    written: String,
    pieces: Vec<Piece>,
}

impl CsvBarsTimePattern {
    fn parse(written: &str) -> Result<Self, String> {
        let mut pieces = Vec::new();
        let mut rest = written;
        while let Some(c) = rest.chars().next() {
            if let Some((token, piece, _)) = TOKENS.iter().find(|(t, ..)| rest.starts_with(*t)) {
                if pieces.contains(piece) {
                    return Err(format!("time pattern {written:?} names {token} twice"));
                }
                pieces.push(*piece);
                rest = &rest[token.len()..];
            } else if c.is_ascii_alphabetic() && c != 'T' {
                return Err(format!(
                    "time pattern {written:?} holds {c:?}, which is no token: the tokens are \
                     YYYY, MM, DD, HH, mm, ss and SSS, and `T` is the one letter taken literally"
                ));
            } else {
                pieces.push(Piece::Lit(c));
                rest = &rest[c.len_utf8()..];
            }
        }
        for (token, piece) in [("YYYY", Piece::Year), ("MM", Piece::Month), ("DD", Piece::Day)] {
            if !pieces.contains(&piece) {
                return Err(format!(
                    "time pattern {written:?} has no {token}: a bar's time is a date"
                ));
            }
        }
        Ok(CsvBarsTimePattern { written: written.to_string(), pieces })
    }

    /// The wall-clock reading `cell` spells in this pattern, every field in range.
    fn read(&self, cell: &str) -> Result<Wall, String> {
        let miss = || format!("does not match the time pattern {:?}", self.written);
        let mut at = Wall { y: 0, mo: 0, d: 0, h: 0, mi: 0, s: 0, ms: 0 };
        let mut rest = cell;
        for piece in &self.pieces {
            if let Piece::Lit(c) = piece {
                rest = rest.strip_prefix(*c).ok_or_else(miss)?;
                continue;
            }
            let width = TOKENS.iter().find(|(_, p, _)| p == piece).map_or(0, |t| t.2);
            let digits = rest.get(..width).filter(|d| d.bytes().all(|b| b.is_ascii_digit()));
            let n: u32 = digits.ok_or_else(miss)?.parse().map_err(|_| miss())?;
            rest = &rest[width..];
            match piece {
                Piece::Year => at.y = i64::from(n),
                Piece::Month => at.mo = n,
                Piece::Day => at.d = n,
                Piece::Hour => at.h = n,
                Piece::Minute => at.mi = n,
                Piece::Second => at.s = n,
                Piece::Milli => at.ms = n,
                Piece::Lit(_) => {}
            }
        }
        if !rest.is_empty() {
            return Err(miss());
        }
        let real_day = (1..=12).contains(&at.mo)
            && at.d >= 1
            && vike_utility::calendar::civil_from_days(vike_utility::calendar::days_from_civil(
                at.y, at.mo, at.d,
            )) == (at.y, at.mo, at.d);
        if !real_day || at.h > 23 || at.mi > 59 || at.s > 59 {
            return Err(format!("is no real date and time in the pattern {:?}", self.written));
        }
        Ok(at)
    }
}

#[path = "format_tests.rs"]
#[cfg(test)]
mod format_tests;
