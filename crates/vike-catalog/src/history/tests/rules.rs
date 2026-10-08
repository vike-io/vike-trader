//! The honesty rules every row obeys: evidence, blanks, findings, lanes, dates.

use super::*;

/// **The evidence rules, as tests.** A Documented or Reported source carries an `https` URL and
/// the day it was read; a Measured source carries the day and what observed; and a sourced row
/// lists at least one source — otherwise it is not "sourced".
#[test]
fn every_source_carries_its_date_and_where_it_came_from() {
    for (venue, row) in all_rows() {
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

/// **Nothing claims a limit without a source.** A row whose evidence is `Unmeasured` carries
/// `Unstated` depth and pace and no per-request LIMIT (`Rows`, `Span`, `SeeVendor`) — the cells that
/// need a source — so the blank is the whole truth and no maintainer's guess is dressed as a fact.
///
/// A per-request `File` is the one cell allowed to stand without a source: it names the unit the
/// workspace's own collector asks an archive for (one UTC day of one stream), a layout read off that
/// code the way `access` and `state` are, and not a limit any vendor page could raise or lower.
#[test]
fn an_unmeasured_row_claims_no_limit() {
    for (venue, row) in all_rows() {
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

/// **A finding that no channel exists is a FINDING.** It carries a written source and a note
/// saying how far the claim reaches, and every cell that would describe a channel is blank — a
/// row cannot both say nothing was found and describe what was.
#[test]
fn an_absent_row_is_a_finding_with_a_source() {
    let mut seen = 0;
    for (venue, row) in all_rows() {
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
    // The control: the table really does carry findings, so the loop above is not vacuous.
    assert!(seen >= 2, "expected the oanda and ibkr archive findings, saw {seen}");
}

/// The other blank: a row nobody classified is `Designed`, says why in one line, carries no note
/// and no source — and it is the shape the scaffold renders.
#[test]
fn an_unclassified_row_is_a_blank_that_says_why() {
    let mut seen = 0;
    for (venue, row) in all_rows() {
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
    for (venue, row) in all_rows() {
        let ChannelState::Built(_) = row.state else { continue };
        assert!(!row.kinds.is_empty(), "{venue}/{}: a lane that serves nothing", row.name);
        assert_ne!(row.access, Access::Unstated, "{venue}/{}: built, access unknown", row.name);
        assert_eq!(row.class, ChannelClass::Request, "{venue}/{}", row.name);
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
    for (venue, row) in all_rows() {
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
    for (venue, row) in all_rows() {
        check(venue, row.name, &row.depth);
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
