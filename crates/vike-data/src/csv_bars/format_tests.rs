//! The regional options: each option read, each combination and word refused, and a regional file
//! refused without its options, naming the one to set. The Excel example first.

use super::*;
use crate::csv_bars::{CsvBarsColumns, CsvBarsError, CsvBarsRule, CsvBarsTimeUnit, decode_bars};

/// The format the words name, which must build.
fn fmt(words: &CsvBarsFormatWords) -> CsvBarsFormat {
    CsvBarsFormat::parse(words).expect("these options build a format")
}

/// One word, as the words type holds it.
fn w(word: &str) -> Option<String> {
    Some(word.to_string())
}

/// Excel with Russian settings: `;` between fields, `,` decimals, thousands grouped by a space (a
/// no-break space on the second row, as a spreadsheet writes it), and Moscow's wall clock as text.
const EXCEL_RU: &str = "time;open;high;low;close;volume\n\
                        09.11.2026 10:00;1 234,5;1 240,0;1 230,25;1 238,75;12,5\n\
                        09.11.2026 11:00;1\u{a0}238,75;1\u{a0}250;1\u{a0}238;1\u{a0}249,5;7\n";

/// The options that read [`EXCEL_RU`], in the zone `tz`.
fn excel_ru(tz: &str) -> CsvBarsFormatWords {
    CsvBarsFormatWords {
        delimiter: w(";"),
        decimal: w(","),
        thousands: w("space"),
        time: w("DD.MM.YYYY HH:mm"),
        tz: w(tz),
    }
}

/// 2026-11-09 at `hour`:00 UTC, in epoch ms.
fn nov_9_utc(hour: i64) -> i64 {
    vike_utility::calendar::days_from_civil(2026, 11, 9) * 86_400_000 + hour * 3_600_000
}

#[track_caller]
fn refused(
    text: &str,
    format: &CsvBarsFormat,
    line: usize,
    rule: CsvBarsRule,
    needle: &str,
) -> CsvBarsError {
    let r =
        decode_bars(text, &CsvBarsColumns::Header, format).expect_err("this file must be refused");
    assert_eq!((r.line, r.rule), (line, rule), "{r}");
    assert!(r.detail.contains(needle), "missing {needle:?} in: {r}");
    r
}

#[test]
fn the_excel_example_reads_with_its_options() {
    for tz in ["+03:00", "Europe/Moscow"] {
        let got = decode_bars(EXCEL_RU, &CsvBarsColumns::Header, &fmt(&excel_ru(tz))).unwrap();
        assert_eq!(got.unit, CsvBarsTimeUnit::Text);
        assert_eq!(got.step_ms, Some(3_600_000));
        let first = &got.bars[0];
        assert_eq!(first.timestamp, nov_9_utc(7), "10:00 in Moscow is 07:00 UTC ({tz})");
        assert_eq!(
            (first.open, first.high, first.low, first.close, first.volume),
            (1234.5, 1240.0, 1230.25, 1238.75, 12.5)
        );
        let second = &got.bars[1];
        assert_eq!((second.timestamp, second.open, second.high), (nov_9_utc(8), 1238.75, 1250.0));
    }
}

/// With no options, nothing about the Excel file is guessed: each option missing is the refusal that
/// names it, one at a time, and the machine's locale is consulted by nothing.
#[test]
fn the_excel_example_without_its_options_is_refused_naming_each() {
    let r = refused(
        EXCEL_RU,
        &CsvBarsFormat::default(),
        1,
        CsvBarsRule::Format,
        "set the delimiter option",
    );
    assert!(r.detail.contains("`;`"), "{r}");

    let delimiter = CsvBarsFormatWords { delimiter: w(";"), ..Default::default() };
    refused(EXCEL_RU, &fmt(&delimiter), 2, CsvBarsRule::Time, "set the time option to its pattern");

    let text_time = CsvBarsFormatWords { time: w("DD.MM.YYYY HH:mm"), ..delimiter };
    let r = refused(EXCEL_RU, &fmt(&text_time), 2, CsvBarsRule::Number, "decimal option to `,`");
    assert!(r.detail.contains("thousands option to `space`"), "{r}");

    let comma = CsvBarsFormatWords { decimal: w(","), ..text_time };
    let r = refused(EXCEL_RU, &fmt(&comma), 2, CsvBarsRule::Number, "thousands option to `space`");
    assert!(!r.detail.contains("decimal option"), "the decimal mark is already right: {r}");
}

#[test]
fn each_delimiter_reads_its_own_file_and_refuses_another() {
    let rows = |d: &str| {
        format!("time{d}open{d}high{d}low{d}close{d}volume\n1700000000{d}10{d}11{d}9{d}10.5{d}2\n")
    };
    for (word, d) in [(",", ","), (";", ";"), ("tab", "\t")] {
        let f = fmt(&CsvBarsFormatWords { delimiter: w(word), ..Default::default() });
        assert_eq!(
            decode_bars(&rows(d), &CsvBarsColumns::Header, &f).unwrap().bars.len(),
            1,
            "{word}"
        );
    }
    refused(
        &rows("\t"),
        &CsvBarsFormat::default(),
        1,
        CsvBarsRule::Format,
        "delimiter option to `tab`",
    );
    let semicolon = fmt(&CsvBarsFormatWords { delimiter: w(";"), ..Default::default() });
    refused(&rows(","), &semicolon, 1, CsvBarsRule::Format, "delimiter option to `,`");
}

/// Each decimal mark and thousands separator, read where it is the format's and refused where not.
#[test]
fn decimal_marks_and_thousands_separators_read_strictly() {
    let cases = [
        // (delimiter, decimal, thousands, the cell, its value)
        (";", ",", Some("."), "1.234.567,25", Some(1_234_567.25)),
        (";", ".", Some(","), "1,234.5", Some(1234.5)),
        (",", ".", Some("space"), "1 234.5", Some(1234.5)),
        (";", ",", Some("space"), "-1\u{202f}234,5", Some(-1234.5)),
        (";", ",", None, "12,5", Some(12.5)),
        (";", ",", Some("space"), "12 34,5", None), // a group of two is no thousands grouping
        (";", ",", Some("space"), "1 234,56 7", None), // nor a separator inside the fraction
        (";", ",", Some("."), "1.234.5", None),     // a `.` group holding the decimal part
        (";", ",", None, "12.5", None),             // the other mark, in a comma-decimal number
        (",", ".", None, "1 234.5", None),          // a space, with no separator set
    ];
    for (delimiter, decimal, thousands, cell, want) in cases {
        let f = fmt(&CsvBarsFormatWords {
            delimiter: w(delimiter),
            decimal: w(decimal),
            thousands: thousands.map(str::to_string),
            ..Default::default()
        });
        assert_eq!(f.number(cell), want, "{cell:?} with {decimal:?} / {thousands:?}");
    }
}

#[test]
fn each_time_pattern_reads_its_text_and_refuses_what_it_cannot_settle() {
    let day = vike_utility::calendar::days_from_civil(2026, 11, 9) * 86_400_000;
    for (pattern, cell, ms) in [
        ("DD.MM.YYYY HH:mm", "09.11.2026 10:00", day + 10 * 3_600_000),
        ("YYYY-MM-DD HH:mm:ss", "2026-11-09 10:00:30", day + 10 * 3_600_000 + 30_000),
        ("YYYY-MM-DDTHH:mm:ss.SSS", "2026-11-09T10:00:30.250", day + 10 * 3_600_000 + 30_250),
        ("DD.MM.YYYY", "09.11.2026", day),
    ] {
        let f = fmt(&CsvBarsFormatWords { time: w(pattern), ..Default::default() });
        assert_eq!(f.text_time(cell), Some(Ok(ms)), "{pattern}: {cell}");
        let f =
            fmt(&CsvBarsFormatWords { time: w(pattern), tz: w("-05:30"), ..Default::default() });
        assert_eq!(
            f.text_time(cell),
            Some(Ok(ms + 5 * 3_600_000 + 30 * 60_000)),
            "{pattern} -05:30"
        );
    }
    let f = fmt(&CsvBarsFormatWords { time: w("DD.MM.YYYY HH:mm"), ..Default::default() });
    for (cell, why) in [
        ("9.11.2026 10:00", "does not match"), // two digits each, never one
        ("09.11.2026 10:00:00", "does not match"),
        ("2026-11-09 10:00", "does not match"),
        ("31.02.2026 10:00", "no real date"),
        ("09.13.2026 10:00", "no real date"),
        ("09.11.2026 24:00", "no real date"),
    ] {
        let got = f.text_time(cell).expect("a text format").unwrap_err();
        assert!(got.contains(why), "{cell:?}: {got}");
    }
    assert_eq!(
        CsvBarsFormat::default().text_time("09.11.2026 10:00"),
        None,
        "epoch is the default"
    );
}

/// A wall-clock time a named zone's clock passed twice (2026-10-25 02:30 in Berlin) or never
/// (2026-03-29 02:30) is refused: no reading of the file settles which instant it was.
#[test]
fn a_clock_change_in_a_named_zone_is_refused_never_guessed() {
    let berlin = CsvBarsFormatWords {
        time: w("YYYY-MM-DD HH:mm"),
        tz: w("Europe/Berlin"),
        ..Default::default()
    };
    let f = fmt(&berlin);
    assert!(f.text_time("2026-10-25 02:30").unwrap().unwrap_err().contains("happened twice"));
    assert!(f.text_time("2026-03-29 02:30").unwrap().unwrap_err().contains("never happened"));
    // Summer is UTC+2 and winter UTC+1: the zone, unlike a fixed offset, follows the clock change.
    let summer = vike_utility::calendar::days_from_civil(2026, 7, 1) * 86_400_000 + 10 * 3_600_000;
    let winter = vike_utility::calendar::days_from_civil(2026, 12, 1) * 86_400_000 + 11 * 3_600_000;
    assert_eq!(f.text_time("2026-07-01 12:00"), Some(Ok(summer)));
    assert_eq!(f.text_time("2026-12-01 12:00"), Some(Ok(winter)));
}

#[test]
fn a_word_no_option_takes_and_options_that_collide_are_refused() {
    for (words, needle) in [
        (CsvBarsFormatWords { delimiter: w("|"), ..Default::default() }, "delimiter \"|\""),
        (CsvBarsFormatWords { decimal: w(";"), ..Default::default() }, "decimal \";\""),
        (CsvBarsFormatWords { thousands: w("_"), ..Default::default() }, "thousands \"_\""),
        (CsvBarsFormatWords { thousands: w(""), ..Default::default() }, "thousands \"\""),
        (CsvBarsFormatWords { decimal: w(","), ..Default::default() }, "is also the delimiter"),
        (
            CsvBarsFormatWords { thousands: w("."), ..Default::default() },
            "is also the decimal mark",
        ),
        (CsvBarsFormatWords { thousands: w(","), ..Default::default() }, "is also the delimiter"),
        (
            CsvBarsFormatWords {
                delimiter: w(";"),
                decimal: w(","),
                thousands: w(","),
                ..Default::default()
            },
            "is also the decimal mark",
        ),
        (CsvBarsFormatWords { tz: w("UTC"), ..Default::default() }, "zone of a TEXT time"),
        (CsvBarsFormatWords { time: w("yyyy-MM-DD"), ..Default::default() }, "which is no token"),
        (CsvBarsFormatWords { time: w("DD.MM HH:mm"), ..Default::default() }, "has no YYYY"),
        (CsvBarsFormatWords { time: w("YYYY-MM-DD DD"), ..Default::default() }, "names DD twice"),
    ] {
        let why = CsvBarsFormat::parse(&words).unwrap_err();
        assert!(why.contains(needle), "{words:?}: {why}");
    }
    let text =
        |tz: &str| CsvBarsFormatWords { time: w("YYYY-MM-DD"), tz: w(tz), ..Default::default() };
    for tz in ["UTC+3", "+3", "+03", "+25:00", "+03:60", "Mars/Olympus", "+03-00"] {
        let why = CsvBarsFormat::parse(&text(tz)).unwrap_err();
        assert!(why.contains("tz"), "{tz}: {why}");
    }
    for tz in ["UTC", "+00:00", "+14:00", "-12:00", "Asia/Kolkata"] {
        assert!(CsvBarsFormat::parse(&text(tz)).is_ok(), "{tz}");
    }
    assert_eq!(CsvBarsFormat::parse(&CsvBarsFormatWords::default()), Ok(CsvBarsFormat::default()));
}

/// The words in their serialized form: a word at its default is left out, every word round-trips,
/// and a key that is none of the five is refused, so a misspelt `timezone` cannot read as UTC.
#[test]
fn the_words_serialize_sparse_and_refuse_a_key_they_do_not_have() {
    let words = excel_ru("Europe/Moscow");
    let json = serde_json::to_string(&words).unwrap();
    assert_eq!(
        json,
        r#"{"delimiter":";","decimal":",","thousands":"space","time":"DD.MM.YYYY HH:mm","tz":"Europe/Moscow"}"#
    );
    assert_eq!(serde_json::from_str::<CsvBarsFormatWords>(&json).unwrap(), words);
    let sparse = CsvBarsFormatWords { delimiter: w(";"), ..Default::default() };
    assert_eq!(serde_json::to_string(&sparse).unwrap(), r#"{"delimiter":";"}"#);
    assert!(CsvBarsFormatWords::default().is_plain() && !sparse.is_plain());
    assert_eq!(serde_json::from_str::<CsvBarsFormatWords>("{}").unwrap(), Default::default());

    let misspelt = r#"{"time":"DD.MM.YYYY HH:mm","timezone":"Europe/Moscow"}"#;
    let why = serde_json::from_str::<CsvBarsFormatWords>(misspelt).unwrap_err().to_string();
    assert!(why.contains("unknown field `timezone`"), "{why}");
}
