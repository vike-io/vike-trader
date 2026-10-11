//! The pure halves: the builder against the catalog it is built from, the fold, and the clamp.
//! The wire halves are `crates/vike-datahub-client/tests/history_channels_negotiation.rs`'s and
//! `crates/vike-datahub/tests/history_channels.rs`'s.

use super::*;

/// 2026-10-02T00:00:00Z.
const AS_OF: i64 = 1_790_899_200_000;

fn documented(date: &str) -> Cell {
    Cell {
        kind: "documented".into(),
        text: "vendor page".into(),
        date: Some(date.into()),
        days: None,
    }
}

/// A served bars row with a `days`-day rolling window, mounted, on `evidence`.
fn window_row(days: u32, evidence: Vec<Cell>) -> ChannelReport {
    let base = compiled_report(AS_OF).venue("binance").expect("binance").channels[0].clone();
    ChannelReport {
        depth: Cell {
            kind: "lookback".into(),
            text: format!("the last {days} days"),
            date: None,
            days: Some(days),
        },
        state: Cell { kind: "built".into(), text: "built".into(), date: None, days: None },
        kinds: vec!["bars".into()],
        mounted: Some(true),
        evidence,
        ..base
    }
}

fn venue_of(channels: Vec<ChannelReport>) -> VenueHistory {
    VenueHistory { venue: "planted".into(), channels, held: Vec::new() }
}

/// **The builder says what the catalog says, cell for cell** — the same sentences the CLI's
/// table prints, through the catalog's own methods, for every row of every roster venue.
#[test]
fn every_row_is_the_catalogs_row_cell_for_cell() {
    let report = compiled_report(AS_OF);
    assert_eq!(report.venues.len(), vike_model::VENUES.len());
    for (venue, served) in vike_model::VENUES.iter().zip(&report.venues) {
        assert_eq!(served.venue, *venue, "roster order");
        let rows = history_channels_for(venue);
        assert_eq!(served.channels.len(), rows.len(), "{venue}");
        for (row, ch) in rows.iter().zip(&served.channels) {
            assert_eq!(ch.name, row.name);
            assert_eq!(ch.class, row.class.word());
            assert_eq!(ch.presence, row.presence_word());
            assert_eq!(ch.kinds_text, row.kinds_text());
            assert_eq!(ch.depth.text, row.depth_text(Some(AS_OF)), "{venue}/{}", row.name);
            assert_eq!(ch.per_request.text, row.per_request_text());
            assert_eq!(ch.pace.text, row.pace_text());
            assert_eq!(ch.access.text, row.access_text());
            assert_eq!(ch.state.text, row.state_text());
            assert_eq!(ch.note, row.note);
            let lines: Vec<&str> = ch.evidence.iter().map(|e| e.text.as_str()).collect();
            assert_eq!(lines.join("\n"), row.evidence_lines().join("\n"), "{venue}/{}", row.name);
            assert!(!ch.depth.kind.is_empty() && !ch.state.kind.is_empty(), "{ch:?}");
        }
    }
}

/// The machine fields the clamp reads are really there: OANDA's fixed start carries its date,
/// and every source of a sourced row carries the day it was read.
#[test]
fn a_fixed_start_and_a_source_carry_their_dates() {
    let report = compiled_report(AS_OF);
    let oanda = &report.venue("oanda").expect("oanda").channels[0];
    assert_eq!(oanda.depth.kind, "since");
    assert_eq!(oanda.depth.date.as_deref(), Some("2005-01-03"));
    assert!(oanda.evidence.iter().all(|e| e.date.is_some()), "{:?}", oanda.evidence);
    assert_eq!(oanda.lane.as_deref(), Some("CredentialedKlines"));
    assert_eq!(oanda.credential, CredentialPresence::NotChecked);
    let binance = &report.venue("binance").expect("binance").channels[0];
    assert_eq!(binance.credential, CredentialPresence::NotNeeded);
    assert_eq!(binance.evidence.len(), 1);
    assert_eq!(binance.evidence[0].kind, "unmeasured");
}

/// The design's planted case: a DOCUMENTED 7-day window clamps a 30-day default to 7 days.
#[test]
fn a_documented_seven_day_window_clamps_a_thirty_day_default() {
    let venue = venue_of(vec![window_row(7, vec![documented("2026-09-30")])]);
    let floor = bars_lookback_floor_ms(&venue, AS_OF).expect("a qualifying row clamps");
    assert_eq!(floor, AS_OF - 7 * MS_PER_DAY);
    let default_start = AS_OF - 30 * MS_PER_DAY;
    assert_eq!(clamp_default_start(default_start, Some(floor)), AS_OF - 7 * MS_PER_DAY);
    // A start already inside the window is never moved, and never moved EARLIER.
    let late = AS_OF - MS_PER_DAY;
    assert_eq!(clamp_default_start(late, Some(floor)), late);
    assert_eq!(clamp_default_start(default_start, None), default_start);
}

/// Every row the rule excludes leaves the default alone: reported, unmeasured, designed,
/// unmounted, unknown-mount, not bars, and a fixed start.
#[test]
fn only_a_mounted_documented_or_measured_bars_window_clamps() {
    let reported = Cell { kind: "reported".into(), ..documented("2026-09-30") };
    let unmeasured =
        Cell { kind: "unmeasured".into(), text: "none".into(), date: None, days: None };
    let measured = Cell { kind: "measured".into(), ..documented("2026-09-30") };
    let mut designed = window_row(7, vec![documented("2026-09-30")]);
    designed.state.kind = "designed".into();
    let mut unmounted = window_row(7, vec![documented("2026-09-30")]);
    unmounted.mounted = Some(false);
    let mut unknown_mount = window_row(7, vec![documented("2026-09-30")]);
    unknown_mount.mounted = None;
    let mut quotes = window_row(7, vec![documented("2026-09-30")]);
    quotes.kinds = vec!["quotes".into()];
    let mut since = window_row(7, vec![documented("2026-09-30")]);
    since.depth = Cell {
        kind: "since".into(),
        text: "since".into(),
        date: Some("2005-01-03".into()),
        days: None,
    };
    for (why, row) in [
        ("reported", window_row(7, vec![reported.clone()])),
        ("documented beside reported", window_row(7, vec![documented("2026-09-30"), reported])),
        ("unmeasured", window_row(7, vec![unmeasured])),
        ("no evidence at all", window_row(7, Vec::new())),
        ("designed", designed),
        ("unmounted", unmounted),
        ("mount not known", unknown_mount),
        ("not bars", quotes),
        ("a fixed start", since),
    ] {
        assert_eq!(bars_lookback_floor_ms(&venue_of(vec![row]), AS_OF), None, "{why} clamped");
    }
    // The control: MEASURED alone qualifies, so the loop above is not refusing everything.
    assert!(
        bars_lookback_floor_ms(&venue_of(vec![window_row(7, vec![measured])]), AS_OF).is_some()
    );
}

/// Two qualifying windows: the DEEPEST wins, so the clamp never cuts below what a mounted
/// channel serves.
#[test]
fn the_deepest_qualifying_window_wins() {
    let venue = venue_of(vec![
        window_row(7, vec![documented("2026-09-30")]),
        window_row(20, vec![documented("2026-09-30")]),
    ]);
    assert_eq!(bars_lookback_floor_ms(&venue, AS_OF), Some(AS_OF - 20 * MS_PER_DAY));
}

/// **The design's honest consequence, held:** on today's table the clamp changes nothing for
/// any roster venue, even with every built lane reported mounted.
#[test]
fn on_todays_table_the_clamp_changes_nothing() {
    let mut report = compiled_report(AS_OF);
    for venue in &mut report.venues {
        for ch in &mut venue.channels {
            if ch.state.kind == "built" {
                ch.mounted = Some(true);
            }
        }
        assert_eq!(bars_lookback_floor_ms(venue, AS_OF), None, "{} clamps", venue.venue);
    }
}

/// The client's own compiled table never clamps: nothing in it is known to be mounted.
#[test]
fn the_compiled_table_never_clamps() {
    let report = compiled_report(AS_OF);
    assert!(report.venues.iter().flat_map(|v| &v.channels).all(|c| c.mounted.is_none()));
    assert!(report.venues.iter().all(|v| bars_lookback_floor_ms(v, AS_OF).is_none()));
}

/// The store fold: per kind, summed, spanning, sorted — and another venue's series is not ours.
#[test]
fn held_sums_one_venues_series_per_kind() {
    let cov = |first, last, rows| SeriesCoverage {
        first_ts: first,
        last_ts: last,
        rows,
        bytes: 0,
        parts: 1,
        dates: 1,
    };
    let inventory = vec![
        (SeriesId::per_symbol("bar", "oanda", "EUR_USD", Some("5s".into())), cov(10, 20, 3)),
        (SeriesId::per_symbol("bar", "oanda", "USD_JPY", Some("1m".into())), cov(5, 15, 4)),
        (SeriesId::per_symbol("quote", "oanda", "EUR_USD", None), cov(7, 9, 2)),
        (SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".into())), cov(1, 99, 50)),
    ];
    let held = held_for("oanda", &inventory);
    assert_eq!(
        held,
        vec![
            HeldKind { kind: "bar".into(), series: 2, rows: 7, first_ts: 5, last_ts: 20 },
            HeldKind { kind: "quote".into(), series: 1, rows: 2, first_ts: 7, last_ts: 9 },
        ]
    );
    assert!(held_for("bybit", &inventory).is_empty());
}
