//! The history table's tests: the pinned matrix, roster completeness, and the shared helpers.

use super::*;

#[cfg(test)]
mod render;
#[cfg(test)]
mod rules;

/// Every `(venue, row)` of the table, in roster order: the walk most of these tests make.
fn all_rows() -> impl Iterator<Item = (&'static str, &'static HistoryChannel)> {
    vike_model::VENUES
        .iter()
        .flat_map(|&venue| history_channels_for(venue).iter().map(move |row| (venue, row)))
}

/// One row's structural signature: everything a maintainer could change silently EXCEPT the free
/// text (notes, reasons, the sentences inside a cell). Ten fields, `|`-separated, so the pin below
/// reads as a ladder and a change shows as exactly one differing line.
fn signature(venue: &str, row: &HistoryChannel) -> String {
    format!(
        "{venue} | {} | {} | {} | {} | {} | {} | {} | {} | {}",
        row.class.word(),
        row.name,
        kinds_token(row.kinds),
        depth_token(&row.depth),
        per_request_token(&row.per_request),
        match row.pace {
            Pace::Stated(_) => "stated",
            Pace::Unstated => "unstated",
        },
        match row.access {
            Access::Keyless => "keyless",
            Access::Credential(_) => "credential",
            Access::Session(_) => "session",
            Access::Paid { .. } => "paid",
            Access::Unstated => "unstated",
        },
        match row.state {
            ChannelState::Built(lane) => format!("built:{}", lane.name()),
            ChannelState::Designed(_) => "designed".to_string(),
        },
        evidence_token(&row.evidence),
    )
}

fn kinds_token(kinds: &[HistoryKind]) -> String {
    if kinds.is_empty() {
        return "none".to_string();
    }
    kinds.iter().map(|k| k.word()).collect::<Vec<_>>().join("+")
}

fn depth_token(depth: &HistoryDepth) -> String {
    match depth {
        HistoryDepth::Since { date, .. } => format!("since:{date}"),
        HistoryDepth::Lookback { days } => format!("lookback:{days}"),
        HistoryDepth::LookbackByStep { steps, otherwise } => {
            let days: Vec<String> = steps.iter().map(|s| s.days.to_string()).collect();
            format!("steps:{}>{}", days.join("/"), depth_token(otherwise))
        }
        HistoryDepth::PerInstrument { .. } => "per_instrument".to_string(),
        HistoryDepth::Unstated => "unstated".to_string(),
    }
}

fn per_request_token(per_request: &PerRequest) -> String {
    match per_request {
        PerRequest::Rows(n) => format!("rows:{n}"),
        PerRequest::Span { days } => format!("span:{days}"),
        PerRequest::File(_) => "file".to_string(),
        PerRequest::SeeVendor(_) => "see_vendor".to_string(),
        PerRequest::Unstated => "unstated".to_string(),
    }
}

/// `unmeasured`, or `d<documented>m<measured>r<reported>` — how many sources of each class.
fn evidence_token(evidence: &HistoryEvidence) -> String {
    match evidence {
        HistoryEvidence::Unmeasured => "unmeasured".to_string(),
        HistoryEvidence::Sourced(sources) => {
            let count =
                |want: fn(&EvidenceSource) -> bool| sources.iter().filter(|s| want(s)).count();
            let documented = count(|s| matches!(s, EvidenceSource::Documented { .. }));
            let measured = count(|s| matches!(s, EvidenceSource::Measured { .. }));
            let reported = count(|s| matches!(s, EvidenceSource::Reported { .. }));
            format!("d{documented}m{measured}r{reported}")
        }
    }
}

/// The whole table, pinned VERBATIM in structure — one line per `(venue, channel)` row, in roster
/// order. The playbook's STEP 1 requirement: a row that changes without this copy changing is a
/// silent behaviour change, and the free text (notes, reasons, sentences) is deliberately not
/// pinned, because a reworded caveat is not a changed fact.
///
/// ⚠ `#[rustfmt::skip]`: the last line of this literal is a `just new-venue` marker, and the lines
/// are longer than `max_width` because a row's whole identity is one string — see
/// `crates/vike-catalog/src/addressing_tests.rs`'s `PINNED` for the rustfmt hazard it guards.
#[rustfmt::skip]
const PINNED: &[&str] = &[
    "binance | request | klines (spot and futures) | bars | unstated | unstated | unstated | keyless | built:Klines | unmeasured",
    "binance | request | funding-rate history | funding | unstated | unstated | unstated | keyless | built:Funding | unmeasured",
    "bybit | request | kline history | bars | unstated | unstated | unstated | keyless | built:Klines | unmeasured",
    "okx | request | history-candles | bars | unstated | unstated | unstated | keyless | built:Klines | unmeasured",
    "deribit | request | chart data (get_tradingview_chart_data) | bars | unstated | unstated | unstated | keyless | built:Klines | unmeasured",
    "oanda | request | v20 REST candles | bars | since:2005-01-03 | rows:5000 | stated | credential | built:CredentialedKlines | d3m1r0",
    "oanda | bulk | bulk archive | none | unstated | unstated | unstated | unstated | designed | d1m0r0",
    "ig | request | history, not classified | none | unstated | unstated | unstated | unstated | designed | unmeasured",
    "fxcm | request | history, not classified | none | unstated | unstated | unstated | unstated | designed | unmeasured",
    "dukascopy | request | HTTP datafeed, one .bi5 file per instrument-hour | quotes | per_instrument | file | stated | keyless | built:TickBars | d0m1r1",
    "dukascopy | bulk | S3 bulk archive (requester pays) | quotes | unstated | file | stated | paid | designed | d1m0r0",
    "dukascopy | request | HTTP candle files | bars | per_instrument | unstated | unstated | unstated | designed | d0m0r1",
    "dukascopy | request | JForex history service | bars+quotes | per_instrument | unstated | unstated | session | designed | d1m0r0",
    "polymarket | request | history, not classified | none | unstated | unstated | unstated | unstated | designed | unmeasured",
    "polymarket | vendor | data.vike.io archive | book+trades+quotes | unstated | file | unstated | credential | designed | unmeasured",
    "polymarket | vendor | pmxt archive (stopped publishing) | book+trades+quotes | unstated | file | unstated | unstated | designed | d0m1r0",
    "ibkr | request | reqHistoricalData bars | bars | steps:183>per_instrument | see_vendor | stated | session | designed | d4m0r0",
    "ibkr | request | reqHistoricalTicks ticks | quotes+trades | unstated | rows:1000 | unstated | session | designed | d1m0r0",
    "ibkr | bulk | bulk archive | none | unstated | unstated | unstated | unstated | designed | d1m0r0",
    "ctrader | request | history, not classified | none | unstated | unstated | unstated | unstated | designed | unmeasured",
    "alpaca | request | history, not classified | none | unstated | unstated | unstated | unstated | designed | unmeasured",
    "aster | request | klines | bars | unstated | unstated | unstated | keyless | built:Klines | unmeasured",
    "hyperliquid | request | candleSnapshot candles | bars | unstated | unstated | unstated | keyless | built:Klines | unmeasured",
    "hyperliquid | request | funding history | funding | unstated | unstated | unstated | keyless | built:Funding | unmeasured",
    // vike:new-venue:row // TODO(new-venue: {venue}): a scaffolded venue owns exactly this one pinned row, the blank one. Replace it
    // vike:new-venue:row // with one line per channel once the vendor's history has been read and the arm in `history_channels_for` says so.
    // vike:new-venue:row "{venue} | request | history, not classified | none | unstated | unstated | unstated | unstated | designed | unmeasured",
];

/// The whole matrix, row for row. A failure prints both sides, so the one changed line is the diff.
#[test]
fn history_matrix_is_pinned() {
    let mut actual = Vec::new();
    for (venue, row) in all_rows() {
        actual.push(signature(venue, row));
    }
    let pinned: Vec<String> = PINNED.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(actual, pinned, "a history row drifted from its pin — the diff is the changed row");
}

/// This module's own source, read at RUNTIME rather than `include_str!`ed — the idiom
/// `crates/vike-catalog/src/intervals_tests.rs` uses, and the reason this file stays out of
/// `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`'s ratchet.
fn own_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/history/mod.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// **Completeness over the roster, the playbook shape.** Every roster venue has a NAMED arm — a
/// SOURCE scan, because a value comparison cannot tell a deliberate blank row from an absent one —
/// at least one row, and a line in the pin; and the pin names no venue the roster lacks.
#[test]
fn every_roster_venue_has_a_named_arm_a_row_and_a_pin() {
    let src = own_source();
    for &venue in vike_model::VENUES {
        assert!(
            src.contains(&format!("\"{venue}\" =>")),
            "roster venue {venue} has no NAMED arm in `history_channels_for` — it would fall \
             through to the empty answer, and a named arm is what proves a venue was classified \
             rather than forgotten"
        );
        assert!(
            !history_channels_for(venue).is_empty(),
            "roster venue {venue} declares no history row at all"
        );
        assert!(
            PINNED.iter().any(|line| line.starts_with(&format!("{venue} | "))),
            "roster venue {venue} has no pinned history row"
        );
    }
    for line in PINNED {
        let venue = line.split(" | ").next().expect("a pinned line names its venue");
        assert!(
            vike_model::VENUES.contains(&venue),
            "the pin carries a row for {venue}, which is not on the roster"
        );
    }
}

/// The scan above must be able to FAIL — without this it answers "named" for a venue that is not
/// there and the completeness test is green over an empty table.
#[test]
fn the_named_arm_scan_can_actually_fail() {
    let src = own_source();
    assert!(src.contains("\"dukascopy\" =>"), "the scan cannot see a real arm");
    assert!(!src.contains("\"no-such-venue\" =>"), "the scan matches a venue that has no arm");
}

/// An unknown venue string declares NOTHING — an empty slice, which says "not declared" and is not
/// the same as "no channel exists". The fallback must not invent a row.
#[test]
fn an_unknown_venue_declares_nothing() {
    assert!(history_channels_for("no-such-venue").is_empty());
    assert!(history_channels_for("").is_empty());
}

/// Names identify a channel within its venue, so a repeat would make a pin line ambiguous and a
/// page section's heading a duplicate.
#[test]
fn a_channel_name_is_unique_within_its_venue_and_never_empty() {
    for &venue in vike_model::VENUES {
        let mut seen = std::collections::BTreeSet::new();
        for row in history_channels_for(venue) {
            assert!(!row.name.trim().is_empty(), "{venue}: a channel with no name");
            assert!(seen.insert(row.name), "{venue}: two channels are both called {:?}", row.name);
        }
    }
}

/// `YYYY-MM-DD`, spelt exactly — `vike_model::parse_date_label` also accepts a bare integer
/// as epoch-milliseconds, so a typo such as `"20260930"` would slip through it.
fn is_iso_date(s: &str) -> bool {
    crate::baseline::is_iso_date(s) && vike_model::parse_date_label(s).is_ok()
}

/// A dated claim may not be dated in the future: a maintainer read a page or ran a probe BEFORE
/// writing the row. (The clock read is a test's, never the table's.)
fn is_in_the_past(date: &str) -> bool {
    vike_model::parse_date_label(date).is_ok_and(|ms| ms <= vike_model::now_ms())
}
