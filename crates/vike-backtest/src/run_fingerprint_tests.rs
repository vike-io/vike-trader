use super::*;

fn cov(first: i64, last: i64, rows: u64) -> SeriesCoverage {
    SeriesCoverage { first_ts: first, last_ts: last, rows, bytes: 4_096, parts: 3, dates: 2 }
}

fn a_fingerprint() -> DataFingerprint {
    DataFingerprint {
        schema: DATA_FINGERPRINT_SCHEMA,
        store: "/srv/vike/store".to_string(),
        from_ms: Some(1_756_000_000_000),
        to_ms: Some(1_756_999_000_000),
        series: vec![
            SeriesFingerprint {
                id: SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string())),
                coverage: Some(cov(1_756_000_000_000, 1_756_999_000_000, 277)),
                commits: vec!["binance:BTCUSDT:1h:0-1".to_string()],
                commits_len: 1,
            },
            SeriesFingerprint {
                id: SeriesId::per_symbol("bar", "binance", "ETHUSDT", Some("1h".to_string())),
                coverage: Some(cov(1_756_000_000_000, 1_756_999_000_000, 277)),
                commits: Vec::new(),
                commits_len: 0,
            },
        ],
    }
}

/// The property the whole address rests on: the same inputs render the same text, every time,
/// on every box. Nothing here formats a float — `SeriesCoverage` is integers throughout — which
/// is deliberate, because a float rendering is exactly where two platforms disagree.
#[test]
fn the_same_inputs_render_the_same_canonical_text() {
    assert_eq!(a_fingerprint().canonical(), a_fingerprint().canonical());
}

/// The ORDER the loader happened to list series in is not a fact about the data, so it may not
/// move the address — otherwise swapping two lines in a `[[data.series]]` array would orphan
/// every baseline.
#[test]
fn the_order_the_series_arrived_in_does_not_move_the_address() {
    let mut reversed = a_fingerprint();
    reversed.series.reverse();

    assert_eq!(reversed.canonical(), a_fingerprint().canonical());
}

/// ⚠ **THE LAYOUT/CONTENT SPLIT, as a test.** `bytes`, `parts`, the commit log and the store
/// PATH all change under an ordinary compaction, a re-ingest or a move to another box, with not
/// one row different. Hashing any of them would make every maintenance run orphan every
/// baseline — which is the one thing decision 8's comparison cannot survive.
#[test]
fn layout_and_location_are_recorded_but_not_addressed() {
    let base = a_fingerprint().canonical();

    let mut compacted = a_fingerprint();
    compacted.series[0].coverage = Some(SeriesCoverage {
        bytes: 1_000_000,
        parts: 1,
        ..compacted.series[0].coverage.clone().unwrap()
    });
    assert_eq!(compacted.canonical(), base, "a compaction changed no row");

    let mut rebuilt = a_fingerprint();
    rebuilt.series[0].commits = Vec::new();
    assert_eq!(rebuilt.canonical(), base, "a manifest rebuild can lose keys the data still has");

    let mut moved = a_fingerprint();
    moved.store = "/mnt/other/store".to_string();
    assert_eq!(moved.canonical(), base, "the same tape under two paths is the same tape");
}

/// ...and the other half of that split: a row count, a boundary or a date count IS content, and
/// every one of them must move the address.
#[test]
fn every_content_fact_moves_the_address() {
    let base = a_fingerprint().canonical();

    for mutate in [
        (|c: &mut SeriesCoverage| c.rows += 1) as fn(&mut SeriesCoverage),
        |c: &mut SeriesCoverage| c.first_ts -= 1,
        |c: &mut SeriesCoverage| c.last_ts += 1,
        |c: &mut SeriesCoverage| c.dates += 1,
    ] {
        let mut changed = a_fingerprint();
        let mut c = changed.series[0].coverage.clone().unwrap();
        mutate(&mut c);
        changed.series[0].coverage = Some(c);
        assert_ne!(changed.canonical(), base, "a content change must move the address");
    }

    let mut narrowed = a_fingerprint();
    narrowed.to_ms = Some(1_756_500_000_000);
    assert_ne!(narrowed.canonical(), base, "the REQUESTED window is an input too");
}

/// A series the store does not hold is a real and reportable state — a backtest over a slice
/// with a missing lane is a run whose result means something different — and it is DISTINCT
/// from an empty one.
#[test]
fn a_missing_series_is_addressed_differently_from_an_empty_one() {
    let mut missing = a_fingerprint();
    missing.series[1].coverage = None;

    let mut empty = a_fingerprint();
    empty.series[1].coverage = Some(SeriesCoverage::default());

    assert_ne!(missing.canonical(), empty.canonical());
    assert!(missing.canonical().contains("missing"), "{}", missing.canonical());
}

const A_PROFILE: &str = "[data]\nvenue = \"binance\"\n";

/// An address is a function of its inputs and nothing else — the property every later verb
/// that compares two runs is built on.
#[test]
fn the_same_config_and_the_same_data_address_the_same() {
    assert_eq!(
        input_fingerprint(A_PROFILE, &a_fingerprint()),
        input_fingerprint(A_PROFILE, &a_fingerprint())
    );
}

/// An address is lowercase hex of a fixed width, because it becomes part of a DIRECTORY NAME
/// and is pasted into issues beside the canonical text it was taken over.
#[test]
fn an_address_is_sixty_four_lowercase_hex_characters() {
    let fp = input_fingerprint(A_PROFILE, &a_fingerprint());

    assert_eq!(fp.len(), 64, "{fp}");
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{fp}");
}

/// One character of the config moves it — the whole point, since an engine knob IS an input.
#[test]
fn one_changed_config_character_moves_the_address() {
    assert_ne!(
        input_fingerprint(A_PROFILE, &a_fingerprint()),
        input_fingerprint(&A_PROFILE.replace("binance", "bybit"), &a_fingerprint())
    );
}

/// ...and so does one more row in the store, because the DATA is an input too and "same
/// profile, more bars" is a different run.
#[test]
fn one_more_row_in_the_store_moves_the_address() {
    let mut grown = a_fingerprint();
    let mut c = grown.series[0].coverage.clone().unwrap();
    c.rows += 1;
    grown.series[0].coverage = Some(c);

    assert_ne!(
        input_fingerprint(A_PROFILE, &grown),
        input_fingerprint(A_PROFILE, &a_fingerprint())
    );
}

/// The two halves are SEPARATED on the wire, so a config ending in the text the canonical
/// rendering opens with cannot produce the same bytes as a different pair. A hash over a
/// concatenation with no separator is the classic way two distinct inputs collide by
/// construction rather than by luck.
#[test]
fn the_config_and_the_data_cannot_bleed_into_one_another() {
    let data = a_fingerprint();
    let smuggled = format!("{A_PROFILE}{}", data.canonical());

    assert_ne!(
        input_fingerprint(&smuggled, &DataFingerprint::default()),
        input_fingerprint(A_PROFILE, &data)
    );
}

/// ⚠ **A TRUNCATED commit log must not move the address.** `commits` is RECORD and the address
/// is content; `MAX_COMMIT_KEYS` exists because a grouped series' log is the whole venue's
/// flush log (~6 MB of JSON for a 30-day window, in every run directory). Bounding a RECORD is
/// only safe while the ADDRESS cannot see it, so that is asserted rather than assumed.
#[test]
fn truncating_the_commit_log_does_not_move_the_address() {
    let base = a_fingerprint().canonical();

    let mut truncated = a_fingerprint();
    truncated.series[0].commits = Vec::new();
    truncated.series[0].commits_len = 86_000;

    assert_eq!(truncated.canonical(), base, "a bounded RECORD may not change the ADDRESS");
    assert_eq!(
        input_fingerprint(A_PROFILE, &truncated),
        input_fingerprint(A_PROFILE, &a_fingerprint()),
        "...and therefore may not change the input address either"
    );
}

/// ⚠ **THE BOUND, proved on the primitive because the collector cannot exercise it.** A grouped
/// series' log is the whole venue's flush log — ~2,880 keys/day at the recorder's 30s age bound,
/// ~86,000 for a 30-day window, about 6 MB of JSON per series in EVERY run directory, and
/// exactly zero before the grouped series were named at all. The shape is
/// [`vike_model::runs::MAX_TRADES`]'s: a PREFIX plus the true count, never a sample.
#[test]
fn a_commit_log_over_the_bound_becomes_a_prefix_that_declares_its_true_length() {
    let long: Vec<String> =
        (0..MAX_COMMIT_KEYS + 500).map(|i| format!("polymarket:book:btc-5m:{i}")).collect();

    let (kept, source_len) = bound_commits(long);

    assert_eq!(kept.len(), MAX_COMMIT_KEYS, "the bound must bind");
    assert_eq!(source_len, MAX_COMMIT_KEYS + 500, "...and the true count must survive it");
    assert_eq!(kept[0], "polymarket:book:btc-5m:0", "a PREFIX keeps the FIRST keys, in order");
    assert_eq!(kept[1], "polymarket:book:btc-5m:1");
    assert_eq!(
        kept[MAX_COMMIT_KEYS - 1],
        format!("polymarket:book:btc-5m:{}", MAX_COMMIT_KEYS - 1)
    );
}

/// Under the bound nothing is touched — the per-symbol case, which is every series in every
/// store that has no grouped layout, and where the count equals the kept length.
#[test]
fn a_commit_log_under_the_bound_is_kept_whole() {
    let short = vec!["binance:BTCUSDT:1h:0-1".to_string(), "binance:BTCUSDT:1h:1-2".to_string()];

    let (kept, source_len) = bound_commits(short.clone());

    assert_eq!(kept, short);
    assert_eq!(source_len, 2);

    let (kept, source_len) = bound_commits(Vec::new());
    assert!(kept.is_empty(), "an empty log is a real state — a keyless append records nothing");
    assert_eq!(source_len, 0);
}
