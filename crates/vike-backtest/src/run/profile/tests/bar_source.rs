//! `[data] source` and `[data] file` (decision 0268): what each parses to, every refusal, and the
//! default that reads the venue's own candles as before.

use super::*;

/// `BAR_TOML` with `lines` added to its `[data]` table.
fn with_data(lines: &str) -> String {
    BAR_TOML.replacen("kind = \"bar\"\n", &format!("kind = \"bar\"\n{lines}\n"), 1)
}

/// `BAR_TOML` over its first symbol alone, with `lines` added to `[data]`: a file holds one series.
fn one_series(lines: &str) -> String {
    with_data(lines).replace("symbols = [\"BTCUSDT\", \"ETHUSDT\"]", "symbols = [\"BTCUSDT\"]")
}

fn load(toml: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(toml)
        .expect("this profile loads")
        .with_stand_in(vike_strategy_lab::stand_in)
}

/// Neither key: the venue's own candles, and no series source, exactly as before the keys existed.
#[test]
fn a_profile_naming_neither_key_reads_the_venues_candles() {
    let p = load(BAR_TOML);
    assert_eq!((p.data.source.as_deref(), p.data.file.as_deref()), (None, None));
    assert_eq!(p.data.bar_source().unwrap(), BarSource::Venue);
    assert_eq!(p.data.bar_series_source(), None);
}

#[test]
fn a_lane_and_a_file_parse_to_where_the_bars_come_from() {
    for lane in ["aggtrades", "tardis", "user-my_file"] {
        let p = load(&with_data(&format!("source = {lane:?}")));
        assert_eq!(p.data.bar_source().unwrap(), BarSource::Lane(lane));
        assert_eq!(p.data.bar_series_source(), Some(lane));
    }
    let p = load(&one_series("file = \"bars/btc.csv\""));
    assert_eq!(p.data.bar_source().unwrap(), BarSource::File("bars/btc.csv"));
    assert_eq!(p.data.bar_series_source(), Some(vike_data::store::store_kind::FILE_SOURCE));
    // A file run may still keep the universe at its default, which consults nothing.
    load(&one_series("file = \"x.csv\"\nuniverse = \"declared\""));
}

#[test]
fn both_keys_at_once_are_refused() {
    let toml = one_series("source = \"aggtrades\"\nfile = \"x.csv\"");
    refused_at_load(&toml, HarnessError::Validation, &["data.source and data.file", "set one"]);
    let p: ProfileBacktest = toml::from_str(&toml).unwrap();
    assert_eq!(p.validate_all()[0].key, "data.file", "the diagnostic names a key it can point at");
}

#[test]
fn a_word_that_is_no_lane_is_refused_with_the_stores_own_sentence() {
    // `user:my_file` is the colon spelling decision 0268 first wrote, and no lane holds a `:`.
    for lane in ["nobody", "user-My File", "user:my_file", "file", "", "Aggtrades"] {
        let toml = with_data(&format!("source = {lane:?}"));
        refused_at_load(
            &toml,
            HarnessError::Validation,
            &["data.source:", "not a lane the store declares"],
        );
    }
}

#[test]
fn a_lane_or_a_file_in_tick_mode_is_refused() {
    for line in ["source = \"aggtrades\"", "file = \"x.csv\""] {
        let toml =
            TICK_TOML.replacen("kind = \"tick\"\n", &format!("kind = \"tick\"\n{line}\n"), 1);
        refused_at_load(&toml, HarnessError::Validation, &["bar-mode only", "data.kind = \"bar\""]);
    }
}

#[test]
fn a_file_names_one_series_of_one_interval_and_plans_nothing_over_the_store() {
    refused_at_load(
        &with_data("file = \"x.csv\""),
        HarnessError::Validation,
        &["data.file holds ONE series", "names 2"],
    );
    refused_at_load(&one_series("file = \"\""), HarnessError::Validation, &["data.file is empty"]);
    refused_at_load(
        &one_series("file = \"x.csv\"\ndetail_interval = \"1m\""),
        HarnessError::Validation,
        &["data.detail_interval has no finer series"],
    );
    for line in ["require_coverage = true", "explain = true", "universe = \"covered\""] {
        refused_at_load(
            &one_series(&format!("file = \"x.csv\"\n{line}")),
            HarnessError::Validation,
            &["data.file is not in the store", "data.source"],
        );
    }
    // A lane IS in the store, so the same keys stand beside it.
    load(&with_data("source = \"aggtrades\"\nrequire_coverage = true\nexplain = true"));
}

/// `file_format` is the decoder's own words (`vike_data::csv_bars::CsvBarsFormatWords`), read only with
/// a file; absent is the plain form, a word the decoder refuses is refused at load, and so is a key it
/// lacks.
#[test]
fn a_file_format_parses_to_the_decoders_format_and_only_beside_a_file() {
    let excel = "file_format = { delimiter = \";\", decimal = \",\", thousands = \"space\", \
                 time = \"DD.MM.YYYY HH:mm\", tz = \"+03:00\" }";
    let p = load(&one_series(&format!("file = \"x.csv\"\n{excel}")));
    let words = vike_data::csv_bars::CsvBarsFormatWords {
        delimiter: Some(";".into()),
        decimal: Some(",".into()),
        thousands: Some("space".into()),
        time: Some("DD.MM.YYYY HH:mm".into()),
        tz: Some("+03:00".into()),
    };
    assert_eq!(
        p.data.csv_format().unwrap(),
        vike_data::csv_bars::CsvBarsFormat::parse(&words).unwrap()
    );
    let plain = load(&one_series("file = \"x.csv\""));
    assert_eq!(plain.data.csv_format().unwrap(), vike_data::csv_bars::CsvBarsFormat::default());

    let toml = with_data(excel);
    refused_at_load(
        &toml,
        HarnessError::Validation,
        &["data.file_format is set without data.file"],
    );
    let p: ProfileBacktest = toml::from_str(&toml).unwrap();
    assert_eq!(p.validate_all()[0].key, "data.file_format");
    for (format, needle) in [
        ("file_format = { decimal = \",\" }", "is also the delimiter"),
        ("file_format = { delimiter = \"|\" }", "delimiter \"|\""),
        ("file_format = { tz = \"+03:00\" }", "zone of a TEXT time"),
        ("file_format = { time = \"yyyy\" }", "which is no token"),
    ] {
        let toml = one_series(&format!("file = \"x.csv\"\n{format}"));
        refused_at_load(&toml, HarnessError::Validation, &["data.file_format:", needle]);
    }
    let toml = one_series("file = \"x.csv\"\nfile_format = { locale = \"ru\" }");
    refused_at_load(&toml, HarnessError::Parse, &["unknown field `locale`"]);
}
