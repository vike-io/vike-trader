//! `CsvBarsFormatWords`: a CSV file's written form as the operator WORDS it, the one owned words type.
//! The `csv-bars` import carries it on the datahub's wire (`vike_datahub_client::archive::ImportTarget`)
//! and `crate::csv_bars::CsvBarsFormat::parse` builds the decoder's format from it, so a file is read
//! the same through every door. Nothing here reads the machine's locale.

use serde::{Deserialize, Serialize};

/// The options as an operator writes them, each `None` for its default, so a file in the plain form
/// needs none. The words:
///
/// - `delimiter`: `,` (the default), `;`, or `tab`;
/// - `decimal`: `.` (the default) or `,`;
/// - `thousands`: absent (the default: no separator), `space`, `.` or `,`. `space` is any of a space,
///   a no-break space and a narrow no-break space, the three a spreadsheet writes;
/// - `time`: `epoch` (the default: an integer epoch in s, ms or µs, the unit read off the values) or a
///   pattern of `YYYY`, `MM`, `DD`, `HH`, `mm`, `ss` and `SSS` (milliseconds) between literal
///   characters, each number exactly that many digits: `DD.MM.YYYY HH:mm`, `YYYY-MM-DDTHH:mm:ss`;
/// - `tz`: the zone a text time is written in: `UTC` (the default), a fixed offset `+03:00` / `-05:30`,
///   or an IANA name such as `Europe/Berlin`, whose clock changes it follows.
///
/// On the wire a word left at its default is left off the frame, and the whole value is left off
/// when every word is ([`Self::is_plain`]). ⚠ A key that is none of the five is REFUSED, never
/// ignored: a misspelt word (`timezone` for `tz`) would otherwise read as its default, and a file
/// written in Moscow time would be stored as UTC.
///
/// It IS a backtest profile's `[data] file_format` table, field for field (`vike-backtest` reads the
/// TOML straight into it), and the profile's published schema reads this file as TEXT
/// (`crate::PROFILE_TABLE_SOURCES`): each field's first doc paragraph is the sentence that schema
/// shows for its key, and its parser takes only `/// doc`, one-line `#[attr]`s and `pub name: Type,`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CsvBarsFormatWords {
    /// Between fields: `","` (the default), `";"` or `"tab"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delimiter: Option<String>,
    /// The decimal mark: `"."` (the default) or `","`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimal: Option<String>,
    /// Between thousands: absent (none, the default), `"space"`, `"."` or `","`; never the decimal mark
    /// or the delimiter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thousands: Option<String>,
    /// `"epoch"` (the default: an integer epoch in s, ms or µs) or a text time's pattern of `YYYY`,
    /// `MM`, `DD`, `HH`, `mm`, `ss` and `SSS`, each that many digits: `"DD.MM.YYYY HH:mm"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// The zone of a text time: `"UTC"` (the default), a fixed offset `"+03:00"`, or an IANA name
    /// such as `"Europe/Berlin"`. A wall-clock time the zone passed twice or never is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

impl CsvBarsFormatWords {
    /// Whether every word is the default, so a frame need not carry it.
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}
