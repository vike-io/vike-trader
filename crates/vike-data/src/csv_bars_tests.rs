//! `decode_bars` in the plain format: a good file in each unit and mapping, and every rule's refusal
//! with its line. The regional options and their refusals are `csv_bars/format_tests.rs`'s.

use super::*;

const T0_S: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z

/// [`decode_bars`] in the plain format, [`CsvBarsFormat::default`].
fn decode(text: &str, columns: &CsvBarsColumns) -> Result<CsvBars, CsvBarsError> {
    decode_bars(text, columns, &CsvBarsFormat::default())
}

/// A header and rows `(time, o, h, l, c, v)` as CSV text.
fn csv(header: &str, rows: &[(i64, f64, f64, f64, f64, f64)]) -> String {
    let mut s = format!("{header}\n");
    for (t, o, h, l, c, v) in rows {
        s.push_str(&format!("{t},{o},{h},{l},{c},{v}\n"));
    }
    s
}

/// Three 1m bars at `t0 * scale`, in that unit.
fn minute_rows(scale: i64) -> Vec<(i64, f64, f64, f64, f64, f64)> {
    (0..3).map(|i| ((T0_S + 60 * i) * scale, 10.0, 11.0, 9.0, 10.5, 2.0)).collect()
}

#[track_caller]
fn refused(text: &str, columns: &CsvBarsColumns, line: usize, rule: CsvBarsRule) -> CsvBarsError {
    let r = decode(text, columns).expect_err("this file must be refused");
    assert_eq!((r.line, r.rule), (line, rule), "{r}");
    assert!(r.to_string().starts_with(&format!("line {line}: {}: ", rule.as_str())), "{r}");
    r
}

#[test]
fn a_good_file_decodes_in_each_unit() {
    for (scale, unit) in [
        (1, CsvBarsTimeUnit::Seconds),
        (1_000, CsvBarsTimeUnit::Millis),
        (1_000_000, CsvBarsTimeUnit::Micros),
    ] {
        let got = decode(
            &csv("time,open,high,low,close,volume", &minute_rows(scale)),
            &CsvBarsColumns::Header,
        )
        .unwrap();
        assert_eq!(got.unit, unit);
        assert_eq!(got.step_ms, Some(60_000));
        let ts: Vec<i64> = got.bars.iter().map(|b| b.timestamp).collect();
        assert_eq!(ts, [T0_S * 1_000, (T0_S + 60) * 1_000, (T0_S + 120) * 1_000]);
        assert_eq!(got.bars[0], Bar::ohlcv(T0_S * 1_000, 10.0, 11.0, 9.0, 10.5, 2.0));
    }
}

/// Header names in any case and among the aliases, an extra column, a byte-order mark, quoted cells
/// and blank lines: all read, and the line numbers still count the file's own lines.
#[test]
fn the_header_is_found_by_name_and_the_rest_is_ignored() {
    let text = format!(
        "\u{feff}Timestamp,\"O\",H,L,C,Vol,trades\n\n{},\"10\",11,9,10.5,2,7\n{},10,11,9,10.5,2,7\n\n",
        T0_S * 1_000,
        (T0_S + 60) * 1_000
    );
    let got = decode(&text, &CsvBarsColumns::Header).unwrap();
    assert_eq!(got.bars.len(), 2);
    assert_eq!(got.bars[0].open, 10.0);
    // ...and the same file refused on line 4 when that line breaks a rule: blank lines are counted.
    let bad = text.replace(
        &format!("{},10,11", (T0_S + 60) * 1_000),
        &format!("{},10,8", (T0_S + 60) * 1_000),
    );
    refused(&bad, &CsvBarsColumns::Header, 4, CsvBarsRule::Ohlc);
}

#[test]
fn an_explicit_mapping_names_each_column() {
    let text = csv("t,first,top,bottom,last,qty", &minute_rows(1_000));
    let mapping =
        CsvBarsColumns::parse("volume=qty, time=t,open=first,high=top,low=bottom,close=last")
            .unwrap();
    assert_eq!(decode(&text, &mapping).unwrap().bars.len(), 3);
    // The header does not know these names, so the default refuses the same file.
    refused(&text, &CsvBarsColumns::Header, 1, CsvBarsRule::Header);
    // A mapping is exact: the case matters.
    let upper =
        CsvBarsColumns::parse("volume=QTY,time=t,open=first,high=top,low=bottom,close=last")
            .unwrap();
    refused(&text, &upper, 1, CsvBarsRule::Header);
}

#[test]
fn a_mapping_names_time_and_close_and_the_bar_fields_together() {
    assert_eq!(CsvBarsColumns::parse("header"), Ok(CsvBarsColumns::Header));
    assert_eq!(
        CsvBarsColumns::parse("time=date, close=value"),
        Ok(CsvBarsColumns::Named(CsvBarsColumnNames {
            time: "date".into(),
            open: None,
            high: None,
            low: None,
            close: "value".into(),
            volume: None,
        }))
    );
    for (spec, needle) in [
        ("time=t,open=o,close=c", "names open but not high, low"),
        ("open=o,high=h,low=l,close=c", "must name time and close"),
        ("time=t,volume=v", "must name time and close"),
        ("time=t,time=u,open=o,high=h,low=l,close=c,volume=v", "time is mapped twice"),
        ("when=t,open=o,high=h,low=l,close=c,volume=v", "is not a field"),
        ("time,open=o,high=h,low=l,close=c,volume=v", "is not `field=header-name`"),
        ("time=,open=o,high=h,low=l,close=c,volume=v", "mapped to no header name"),
    ] {
        let why = CsvBarsColumns::parse(spec).unwrap_err();
        assert!(why.contains(needle), "{spec:?}: {why}");
    }
}

/// Decision 0272: a file without a volume column stores 0, and a file of one value per row stores a bar
/// whose open, high and low are its close — by the header's names and by an explicit mapping alike.
#[test]
fn a_file_without_volume_or_of_one_value_per_row_reads() {
    let t = |m: i64| (T0_S + 60 * m) * 1_000;
    let no_volume =
        format!("time,open,high,low,close\n{},10,11,9,10.5\n{},10,11,9,10.5\n", t(0), t(1));
    let got = decode(&no_volume, &CsvBarsColumns::Header).unwrap();
    assert_eq!(got.bars[0], Bar::ohlcv(t(0), 10.0, 11.0, 9.0, 10.5, 0.0));
    let one_value = format!("ts,close\n{},4.25\n{},4.5\n", t(0), t(1));
    let got = decode(&one_value, &CsvBarsColumns::Header).unwrap();
    assert_eq!(got.bars[1], Bar::ohlcv(t(1), 4.5, 4.5, 4.5, 4.5, 0.0));
    assert_eq!(got.step_ms, Some(60_000));
    // A sentiment index: its own names, mapped. Its volume column is read when the mapping names it.
    let index = format!("date,value,count\n{},71,3\n{},64,5\n", t(0), t(1));
    refused(&index, &CsvBarsColumns::Header, 1, CsvBarsRule::Header);
    let got = decode(&index, &CsvBarsColumns::parse("time=date,close=value").unwrap()).unwrap();
    assert_eq!(got.bars[0], Bar::ohlcv(t(0), 71.0, 71.0, 71.0, 71.0, 0.0));
    let got = decode(&index, &CsvBarsColumns::parse("time=date,close=value,volume=count").unwrap())
        .unwrap();
    assert_eq!(got.bars[1].volume, 5.0);
}

#[test]
fn the_header_cells_are_the_first_lines_as_the_decoder_splits_it() {
    let plain = CsvBarsFormat::default();
    assert_eq!(
        header_cells("\u{feff}\n timestamp_ms, \"O\" ,h\n1,2,3\n", &plain),
        ["timestamp_ms", "O", "h"]
    );
    assert!(header_cells("", &plain).is_empty());
    let semicolon = CsvBarsFormat::parse(&CsvBarsFormatWords {
        delimiter: Some(";".to_string()),
        ..CsvBarsFormatWords::default()
    })
    .unwrap();
    assert_eq!(header_cells("Datum;Schluss\n", &semicolon), ["Datum", "Schluss"]);
    // Bounded, and cut on a character boundary.
    let wide = vec!["c"; HEADER_CELLS_MAX + 9].join(",");
    assert_eq!(header_cells(&wide, &plain).len(), HEADER_CELLS_MAX);
    let long = "é".repeat(HEADER_CELL_MAX_BYTES);
    let cut = &header_cells(&long, &plain)[0];
    assert!(cut.len() <= HEADER_CELL_MAX_BYTES && cut.chars().all(|c| c == 'é'), "{cut}");
}

#[test]
fn a_file_of_one_bar_has_no_step() {
    let got = decode(
        &csv("time,open,high,low,close,volume", &minute_rows(1)[..1]),
        &CsvBarsColumns::Header,
    )
    .unwrap();
    assert_eq!((got.bars.len(), got.step_ms), (1, None));
}

/// A missing bar is a gap and allowed; the step is the commonest distance, the shorter on a tie.
#[test]
fn a_gap_is_allowed_and_the_step_is_the_commonest_distance() {
    let at = |m: i64| ((T0_S + 60 * m) * 1_000, 10.0, 11.0, 9.0, 10.5, 2.0);
    let header = "time,open,high,low,close,volume";
    let got = decode(&csv(header, &[at(0), at(1), at(2), at(5), at(6)]), &CsvBarsColumns::Header)
        .unwrap();
    assert_eq!(got.step_ms, Some(60_000));
    // One distance of 60 s and one of 120 s: a tie, so the shorter, and 120 s is its gap.
    let got = decode(&csv(header, &[at(0), at(1), at(3)]), &CsvBarsColumns::Header).unwrap();
    assert_eq!(got.step_ms, Some(60_000));
}

#[test]
fn a_second_interval_is_refused_on_its_line() {
    let at = |s: i64| ((T0_S + s) * 1_000, 10.0, 11.0, 9.0, 10.5, 2.0);
    let text = csv("time,open,high,low,close,volume", &[at(0), at(60), at(120), at(180), at(210)]);
    let r = refused(&text, &CsvBarsColumns::Header, 6, CsvBarsRule::Interval);
    assert!(r.detail.contains("30000 ms after line 5") && r.detail.contains("60000 ms"), "{r}");
    // The sparse door reads no step off the rows (a user's lane holds them to its interval's grid).
    let rows =
        decode_bars_sparse(&text, &CsvBarsColumns::Header, &CsvBarsFormat::default()).unwrap();
    assert_eq!(rows.len(), 5);
}

#[test]
fn an_empty_file_and_a_header_alone_are_refused() {
    refused("", &CsvBarsColumns::Header, 1, CsvBarsRule::Empty);
    refused("\n \n", &CsvBarsColumns::Header, 1, CsvBarsRule::Empty);
    refused("time,open,high,low,close,volume\n", &CsvBarsColumns::Header, 2, CsvBarsRule::Empty);
}

#[test]
fn a_header_that_does_not_name_each_field_once_is_refused() {
    let rows = minute_rows(1_000);
    let r = refused(
        &csv("time,open,high,close,volume", &[]),
        &CsvBarsColumns::Header,
        1,
        CsvBarsRule::Header,
    );
    assert!(r.detail.contains("names open, high but not low"), "{r}");
    let r = refused(
        &csv("time,open,high,low,volume", &[]),
        &CsvBarsColumns::Header,
        1,
        CsvBarsRule::Header,
    );
    assert!(r.detail.contains("no close column"), "{r}");
    // A headerless file reads its first row as the header, and says so.
    let headerless = csv("1700000000000,10,11,9,10.5,2", &rows);
    let r = refused(&headerless, &CsvBarsColumns::Header, 1, CsvBarsRule::Header);
    assert!(r.detail.contains("the first line must be the header"), "{r}");
    let r = refused(
        &csv("time,ts,open,high,low,close,volume", &[]),
        &CsvBarsColumns::Header,
        1,
        CsvBarsRule::Header,
    );
    assert!(r.detail.contains("2 columns could be time"), "{r}");
    let one_column = CsvBarsColumns::parse("time=t,open=p,high=p,low=l,close=c,volume=v").unwrap();
    let r = refused(&csv("t,p,l,c,v", &[]), &one_column, 1, CsvBarsRule::Header);
    assert!(r.detail.contains("open and high are both mapped"), "{r}");
}

#[test]
fn a_row_with_another_cell_count_is_refused() {
    let text = format!("time,open,high,low,close,volume\n{},10,11,9,10.5\n", T0_S);
    refused(&text, &CsvBarsColumns::Header, 2, CsvBarsRule::Cells);
}

/// UTC only: a time is an integer epoch, never a text time, an offset or a fraction.
#[test]
fn a_time_that_is_not_an_integer_epoch_is_refused() {
    for t in ["2023-11-14T22:13:20Z", "2023-11-14T22:13:20+02:00", "1700000000.5", ""] {
        let text = format!("time,open,high,low,close,volume\n{t},10,11,9,10.5,2\n");
        let r = refused(&text, &CsvBarsColumns::Header, 2, CsvBarsRule::Time);
        assert!(r.detail.contains("UTC"), "{t:?}: {r}");
    }
    let text =
        format!("time,open,high,low,close,volume\n{},10,11,9,10.5,2\n", T0_S * 1_000_000 + 1);
    let r = refused(&text, &CsvBarsColumns::Header, 2, CsvBarsRule::Time);
    assert!(r.detail.contains("not a whole millisecond"), "{r}");
}

#[test]
fn a_time_in_no_unit_or_in_a_second_unit_is_refused() {
    let header = "time,open,high,low,close,volume";
    for t in [0, -1, 5_000_000_000, 31_535_999] {
        refused(
            &csv(header, &[(t, 10.0, 11.0, 9.0, 10.5, 2.0)]),
            &CsvBarsColumns::Header,
            2,
            CsvBarsRule::Unit,
        );
    }
    let rows =
        [(T0_S, 10.0, 11.0, 9.0, 10.5, 2.0), ((T0_S + 60) * 1_000, 10.0, 11.0, 9.0, 10.5, 2.0)];
    let r = refused(&csv(header, &rows), &CsvBarsColumns::Header, 3, CsvBarsRule::Unit);
    assert!(r.detail.contains("in ms where line 2 is in s"), "{r}");
}

/// The three units' ranges are the calendar's 1971 and 2100, and they do not touch (the module's const
/// assertion): each boundary value names exactly the unit it should.
#[test]
fn the_unit_ranges_are_the_calendars_and_disjoint() {
    let day = 86_400_000;
    assert_eq!(EARLIEST_MS, vike_utility::calendar::days_from_civil(1971, 1, 1) * day);
    assert_eq!(END_MS, vike_utility::calendar::days_from_civil(2100, 1, 1) * day);
    assert_eq!(CsvBarsTimeUnit::of(EARLIEST_MS / 1_000), Some(CsvBarsTimeUnit::Seconds));
    assert_eq!(CsvBarsTimeUnit::of(END_MS - 1), Some(CsvBarsTimeUnit::Millis));
    assert_eq!(CsvBarsTimeUnit::of((END_MS - 1) * 1_000), Some(CsvBarsTimeUnit::Micros));
    assert_eq!(CsvBarsTimeUnit::of(END_MS / 1_000), None, "2100 is past the range in every unit");
}

#[test]
fn rows_out_of_order_and_a_repeated_time_are_refused() {
    let header = "time,open,high,low,close,volume";
    let bar = |s: i64| ((T0_S + s) * 1_000, 10.0, 11.0, 9.0, 10.5, 2.0);
    let r = refused(
        &csv(header, &[bar(0), bar(60), bar(30)]),
        &CsvBarsColumns::Header,
        4,
        CsvBarsRule::Order,
    );
    assert!(r.detail.contains("before line 3"), "{r}");
    let r = refused(
        &csv(header, &[bar(0), bar(60), bar(60)]),
        &CsvBarsColumns::Header,
        4,
        CsvBarsRule::Duplicate,
    );
    assert!(r.detail.contains("line 3's time again"), "{r}");
}

#[test]
fn a_value_that_is_not_a_finite_number_is_refused() {
    for (cells, field) in
        [("x,11,9,10.5,2", "open"), ("10,NaN,9,10.5,2", "high"), ("10,11,9,10.5,inf", "volume")]
    {
        let text = format!("time,open,high,low,close,volume\n{T0_S},{cells}\n");
        let r = refused(&text, &CsvBarsColumns::Header, 2, CsvBarsRule::Number);
        assert!(r.detail.starts_with(field), "{cells}: {r}");
    }
}

#[test]
fn a_bar_outside_its_own_range_is_refused() {
    for row in [
        (T0_S, 12.0, 11.0, 9.0, 10.5, 2.0),  // open above high
        (T0_S, 10.0, 11.0, 9.0, 8.0, 2.0),   // close below low
        (T0_S, 10.0, 11.0, 10.2, 10.5, 2.0), // low above open
        (T0_S, 10.0, 11.0, 9.0, 11.5, 2.0),  // close above high
    ] {
        refused(
            &csv("time,open,high,low,close,volume", &[row]),
            &CsvBarsColumns::Header,
            2,
            CsvBarsRule::Ohlc,
        );
    }
}

#[test]
fn a_negative_volume_is_refused() {
    let row = (T0_S, 10.0, 11.0, 9.0, 10.5, -1.0);
    refused(
        &csv("time,open,high,low,close,volume", &[row]),
        &CsvBarsColumns::Header,
        2,
        CsvBarsRule::Volume,
    );
}
