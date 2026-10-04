use super::*;

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
    for &venue in vike_model::VENUES {
        for row in history_channels_for(venue) {
            actual.push(signature(venue, row));
        }
    }
    let pinned: Vec<String> = PINNED.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(actual, pinned, "a history row drifted from its pin — the diff is the changed row");
}

/// This module's own source, read at RUNTIME rather than `include_str!`ed — the idiom
/// `crates/vike-catalog/src/intervals_tests.rs` uses, and the reason this file stays out of
/// `crates/vike-ops/tests/compile_time_path_gate.rs`'s ratchet.
fn own_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/history.rs");
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
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && vike_model::parse_date_label(s).is_ok()
}

/// A dated claim may not be dated in the future: a maintainer read a page or ran a probe BEFORE
/// writing the row. (The clock read is a test's, never the table's.)
fn is_in_the_past(date: &str) -> bool {
    vike_model::parse_date_label(date).is_ok_and(|ms| ms <= vike_model::now_ms())
}

/// **The evidence rules, as tests.** A Documented or Reported source carries an `https` URL and
/// the day it was read; a Measured source carries the day and what observed; and a sourced row
/// lists at least one source — otherwise it is not "sourced".
#[test]
fn every_source_carries_its_date_and_where_it_came_from() {
    for &venue in vike_model::VENUES {
        for row in history_channels_for(venue) {
            let HistoryEvidence::Sourced(sources) = row.evidence else { continue };
            assert!(!sources.is_empty(), "{venue}/{}: `Sourced` with no source", row.name);
            for source in sources {
                match source {
                    EvidenceSource::Documented { url, checked }
                    | EvidenceSource::Reported { url, checked, .. } => {
                        assert!(
                            url.starts_with("https://") && !url.contains(char::is_whitespace),
                            "{venue}/{}: a documented source needs an https URL: {url:?}",
                            row.name
                        );
                        assert!(
                            is_iso_date(checked) && is_in_the_past(checked),
                            "{venue}/{}: bad checked date {checked:?}",
                            row.name
                        );
                    }
                    EvidenceSource::Measured { on, by } => {
                        assert!(
                            is_iso_date(on) && is_in_the_past(on),
                            "{venue}/{}: bad measured date {on:?}",
                            row.name
                        );
                        assert!(by.len() > 20, "{venue}/{}: say what observed: {by:?}", row.name);
                    }
                }
                if let EvidenceSource::Reported { by, .. } = source {
                    assert!(by.len() > 10, "{venue}/{}: say who reported it: {by:?}", row.name);
                }
            }
        }
    }
}

/// **Nothing claims a limit without a source.** A row whose evidence is `Unmeasured` carries
/// `Unstated` depth and pace and no per-request LIMIT (`Rows`, `Span`, `SeeVendor`) — the cells that
/// need a source — so the blank is the whole truth and no maintainer's guess is dressed as a fact.
///
/// A per-request `File` is the one cell allowed to stand without a source: it names the unit the
/// workspace's own collector asks an archive for (one UTC day of one stream), a layout read off that
/// code the way `access` and `state` are, and not a limit any vendor page could raise or lower.
#[test]
fn an_unmeasured_row_claims_no_limit() {
    for &venue in vike_model::VENUES {
        for row in history_channels_for(venue) {
            if row.evidence != HistoryEvidence::Unmeasured {
                continue;
            }
            assert_eq!(
                row.depth,
                HistoryDepth::Unstated,
                "{venue}/{}: depth without a source",
                row.name
            );
            assert!(
                matches!(row.per_request, PerRequest::Unstated | PerRequest::File(_)),
                "{venue}/{}: a per-request limit without a source: {:?}",
                row.name,
                row.per_request
            );
            assert_eq!(row.pace, Pace::Unstated, "{venue}/{}: a pace without a source", row.name);
        }
    }
}

/// **A finding that no channel exists is a FINDING.** It carries a written source and a note
/// saying how far the claim reaches, and every cell that would describe a channel is blank — a
/// row cannot both say nothing was found and describe what was.
#[test]
fn an_absent_row_is_a_finding_with_a_source() {
    let mut seen = 0;
    for &venue in vike_model::VENUES {
        for row in history_channels_for(venue) {
            if !row.is_absent() {
                continue;
            }
            seen += 1;
            assert!(matches!(row.evidence, HistoryEvidence::Sourced(_)), "{venue}/{}", row.name);
            assert!(row.note.len() > 40, "{venue}/{}: scope the claim: {:?}", row.name, row.note);
            assert_eq!(row.depth, HistoryDepth::Unstated, "{venue}/{}", row.name);
            assert_eq!(row.per_request, PerRequest::Unstated, "{venue}/{}", row.name);
            assert_eq!(row.pace, Pace::Unstated, "{venue}/{}", row.name);
            assert_eq!(row.access, Access::Unstated, "{venue}/{}", row.name);
            assert!(matches!(row.state, ChannelState::Designed(_)), "{venue}/{}", row.name);
        }
    }
    // The control: the table really does carry findings, so the loop above is not vacuous.
    assert!(seen >= 2, "expected the oanda and ibkr archive findings, saw {seen}");
}

/// The other blank: a row nobody classified is `Designed`, says why in one line, carries no note
/// and no source — and it is the shape the scaffold renders.
#[test]
fn an_unclassified_row_is_a_blank_that_says_why() {
    let mut seen = 0;
    for &venue in vike_model::VENUES {
        for row in history_channels_for(venue) {
            if !row.is_unclassified() {
                continue;
            }
            seen += 1;
            let ChannelState::Designed(why) = row.state else {
                panic!("{venue}: an unclassified row cannot be built");
            };
            assert!(why.len() > 30, "{venue}: say why nothing is known: {why:?}");
            assert_eq!(row.access, Access::Unstated, "{venue}: access without a source");
            assert!(row.note.is_empty(), "{venue}: an unclassified row carries no note");
        }
    }
    assert!(seen >= 4, "expected several unclassified venues, saw {seen}");
    // The scaffolded row is one of them, by construction.
    assert!(UNCLASSIFIED.iter().all(HistoryChannel::is_unclassified));
    assert_eq!(UNCLASSIFIED.len(), 1);
}

/// A lane that exists serves something and needs something we can name — otherwise `Built` is a
/// claim about a lane with no product. And every lane is a REQUEST channel: the datahub's collector
/// table is the venue's own query API, so a bulk or vendor row is never `Built` through it.
#[test]
fn a_built_row_serves_something_names_its_access_and_is_a_request() {
    for &venue in vike_model::VENUES {
        for row in history_channels_for(venue) {
            let ChannelState::Built(_) = row.state else { continue };
            assert!(!row.kinds.is_empty(), "{venue}/{}: a lane that serves nothing", row.name);
            assert_ne!(row.access, Access::Unstated, "{venue}/{}: built, access unknown", row.name);
            assert_eq!(row.class, ChannelClass::Request, "{venue}/{}", row.name);
        }
    }
}

/// **A lane and its row agree about the credential, both ways.** Three datahub lanes read a public
/// endpoint anybody may call; the fourth reads a token the operator stored on the datahub's box. A
/// row over a keyless lane that says it needs a credential describes a fetch the lane does not make,
/// and a row over the credentialed lane that says it is keyless hides the one lane whose usual
/// failure is an absent key. The `match` is exhaustive, so a lane added to [`HistoryLane`] does not
/// compile until somebody has said which kind it is.
#[test]
fn a_lane_needs_a_credential_exactly_when_its_row_says_so() {
    let mut credentialed = 0;
    for &venue in vike_model::VENUES {
        for row in history_channels_for(venue) {
            let ChannelState::Built(lane) = row.state else { continue };
            let agrees = match lane {
                HistoryLane::CredentialedKlines => {
                    credentialed += 1;
                    matches!(row.access, Access::Credential(_))
                }
                HistoryLane::Klines | HistoryLane::TickBars | HistoryLane::Funding => {
                    row.access == Access::Keyless
                }
            };
            assert!(agrees, "{venue}/{}: lane {lane:?} against access {:?}", row.name, row.access);
        }
    }
    // The control: the credentialed lane is in the table, so the loop above is not vacuous.
    assert!(credentialed >= 1, "no row is built over the credentialed lane");
}

/// **Two tables, one fact.** `vike_model::caps_for`'s `backfill_bars`/`backfill_ticks` are the
/// declared per-venue capability the rest of the workspace already reads; a venue is `Built` here
/// exactly when that table says it can backfill bars, and has a tick lane exactly when it says
/// ticks. So the two cannot drift apart about what a venue's lane serves.
#[test]
fn built_rows_agree_with_the_venue_caps_table() {
    for &venue in vike_model::VENUES {
        let caps = vike_model::caps_for(venue);
        let rows = history_channels_for(venue);
        let bars = rows.iter().any(|r| {
            matches!(
                r.state,
                ChannelState::Built(
                    HistoryLane::Klines | HistoryLane::TickBars | HistoryLane::CredentialedKlines
                )
            )
        });
        let ticks =
            rows.iter().any(|r| matches!(r.state, ChannelState::Built(HistoryLane::TickBars)));
        assert_eq!(
            caps.backfill_bars, bars,
            "{venue}: `caps_for` and the history table disagree about bars"
        );
        assert_eq!(
            caps.backfill_ticks, ticks,
            "{venue}: `caps_for` and the history table disagree about ticks"
        );
    }
}

/// A floor date is a real date that has already happened, and a window is at least a day — a `0`
/// would render "the last 0 days", which reads as a refusal nobody wrote.
#[test]
fn a_date_is_real_and_a_window_is_positive() {
    fn check(venue: &str, name: &str, depth: &HistoryDepth) {
        match depth {
            HistoryDepth::Since { date, scope } => {
                assert!(is_iso_date(date) && is_in_the_past(date), "{venue}/{name}: {date:?}");
                assert!(scope.len() > 10, "{venue}/{name}: a floor needs its scope: {scope:?}");
            }
            HistoryDepth::Lookback { days } => assert!(*days > 0, "{venue}/{name}"),
            HistoryDepth::LookbackByStep { steps, otherwise } => {
                assert!(!steps.is_empty(), "{venue}/{name}: a by-step depth with no step");
                for s in *steps {
                    assert!(s.days > 0 && s.bars.len() > 5, "{venue}/{name}: {s:?}");
                }
                check(venue, name, otherwise);
            }
            HistoryDepth::PerInstrument { probe } => {
                assert!(probe.len() > 10, "{venue}/{name}: name the probe: {probe:?}");
            }
            HistoryDepth::Unstated => {}
        }
    }
    for &venue in vike_model::VENUES {
        for row in history_channels_for(venue) {
            check(venue, row.name, &row.depth);
        }
    }
}

/// **The three findings from the design's §1 that were the whole point, pinned by content.** OANDA
/// serves candles only and has no archive; IBKR's small-bar window is the one hard limit and it is
/// six months; Dukascopy is the one venue with all three classes. If any of these moves, a maintainer
/// re-read a vendor page and this is where that is recorded.
#[test]
fn the_three_venues_the_owner_asked_about_say_what_they_were_read_to_say() {
    let oanda = history_channels_for("oanda");
    assert_eq!(oanda[0].kinds, &[HistoryKind::Bars], "OANDA serves candles only, no ticks");
    assert!(oanda[1].is_absent() && oanda[1].class == ChannelClass::Bulk, "OANDA has no archive");

    let ibkr = history_channels_for("ibkr");
    match ibkr[0].depth {
        HistoryDepth::LookbackByStep { steps, otherwise } => {
            assert_eq!(steps.len(), 1);
            assert_eq!(steps[0].days, 183, "IBKR's small-bar window is six months");
            assert!(matches!(otherwise, HistoryDepth::PerInstrument { .. }));
        }
        other => panic!("ibkr bars must be a by-step window, got {other:?}"),
    }
    assert!(
        ibkr.iter().any(|r| r.is_absent() && r.class == ChannelClass::Bulk),
        "IBKR has no archive"
    );

    let classes: Vec<ChannelClass> =
        history_channels_for("dukascopy").iter().map(|r| r.class).collect();
    assert!(classes.contains(&ChannelClass::Request) && classes.contains(&ChannelClass::Bulk));
}
