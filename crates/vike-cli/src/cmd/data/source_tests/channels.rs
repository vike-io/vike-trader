//! The history channels: the block `show VENUE` prints, and every rule its rendering keeps.
use super::*;

// ── The history channels: `docs/superpowers/specs/2026-09-30-history-channels-design.md`, Part A ──

/// **THE SURFACE THE OWNER ASKED FOR.** `show <venue>` prints that venue's channels — every one, by
/// name, with its class in the first column — and the document carries the same rows.
#[test]
fn a_roster_venue_prints_its_channels_in_the_table_and_the_document() {
    let text = show_text("dukascopy");
    assert!(text.contains(HISTORY_HEADING), "{text}");
    for ch in history_channels_for("dukascopy") {
        assert!(text.contains(ch.name), "`show dukascopy` must name `{}`: {text}", ch.name);
    }
    assert!(text.contains("  request HTTP datafeed"), "the class column, padded: {text}");
    assert!(text.contains("  bulk    S3 bulk archive"), "…and a bulk channel beside it: {text}");

    let doc = show_doc("dukascopy");
    assert_eq!(doc["channels_declared"], true);
    assert_eq!(doc["channels_as_of"], "2026-09-30");
    assert_eq!(
        doc["channels"].as_array().map(Vec::len),
        Some(history_channels_for("dukascopy").len()),
        "the document carries the same rows the table prints: {doc}"
    );
}

/// EVERY roster venue has a block — a named row is what proves it was classified — and none of them
/// is told it is undeclared. Iterates the roster, so a venue added to it is judged here with no edit.
#[test]
fn every_roster_venue_shows_at_least_one_channel_and_declares_it() {
    for &venue in VENUES {
        let doc = show_doc(venue);
        assert_eq!(doc["channels_declared"], true, "{venue}");
        let channels =
            doc["channels"].as_array().unwrap_or_else(|| panic!("{venue}: no channels array"));
        assert_eq!(channels.len(), history_channels_for(venue).len(), "{venue}");
        assert!(!channels.is_empty(), "{venue} declares nothing: {doc}");
        let text = show_text(venue);
        assert!(text.contains(HISTORY_HEADING), "{venue}: {text}");
        assert!(
            !text.contains("declares no history channels"),
            "{venue}: a roster venue was told it is undeclared: {text}"
        );
    }
}

/// **THE CHANNELS ARE NOT A PROBE, AND EVERY BLOCK SAYS SO.** Each roster venue's block carries
/// [`HISTORY_NOTE`] — in the table and as a footnote of the document — and its answer STILL ends on
/// [`NOT_VERIFIED`], the limit every answer in this group ends on. The table is compiled-in data
/// dated by the day a maintainer read a page; without the note a date beside a vendor URL reads
/// like a check this command just made.
#[test]
fn every_channel_block_carries_the_note_that_denies_a_probe_and_still_ends_on_the_limit() {
    for &venue in VENUES {
        let text = show_text(venue);
        assert!(text.contains(HISTORY_NOTE), "{venue}: no note denying a probe: {text}");
        assert!(text.trim_end().ends_with(NOT_VERIFIED), "{venue}: must END on the limit: {text}");
        let doc = show_doc(venue);
        let notes = doc["notes"].as_array().expect("`show --json` carries notes");
        assert!(notes.iter().any(|n| n == HISTORY_NOTE), "{venue}: the document lost it: {doc}");
    }
    // The control: a source that is not a venue carries no channels, so it carries no such note.
    assert!(!show_text("demo").contains(HISTORY_NOTE));
}

/// **AN EMPTY ANSWER MUST NOT READ AS "NONE EXIST".** A name that is not on the roster is still a
/// venue token, exactly as `--source` takes it, and gets no channels — and the note says that is a
/// statement about this table. The control: a roster venue does not get the note.
#[test]
fn an_unknown_venue_declares_no_channels_and_says_that_is_not_none_exist() {
    let text = show_text("no-such-venue");
    assert!(text.contains("declares no history channels for `no-such-venue`"), "{text}");
    assert!(text.contains("not about the venue"), "…and what that means: {text}");
    assert!(!text.contains(HISTORY_HEADING), "no block for a name the table never saw: {text}");
    let doc = show_doc("no-such-venue");
    assert_eq!(doc["channels_declared"], false);
    assert_eq!(doc["channels"].as_array().map(Vec::len), Some(0));
    assert!(!show_text("binance").contains("declares no history channels"));
}

/// The sources that are not venues carry no channels at all, and say `false` rather than nothing.
#[test]
fn a_non_venue_source_carries_no_channels() {
    for name in ["demo", "starter", "vike", "tardis", VENUE_TOKEN] {
        let doc = show_doc(name);
        assert_eq!(doc["channels_declared"], false, "{name}");
        assert_eq!(doc["channels"].as_array().map(Vec::len), Some(0), "{name}");
        assert!(!show_text(name).contains(HISTORY_HEADING), "{name}");
        assert!(!show_text(name).contains("declares no history channels"), "{name}");
    }
}

/// **A rolling window is resolved against the clock the CALLER passes**, in the table and in the
/// document — so the terminal says a DATE and a test can pin it. One day later the date moves,
/// which is the whole reason a window is stored as days.
#[test]
fn a_rolling_window_is_resolved_against_the_clock_the_caller_passes() {
    let text = show_text("ibkr");
    assert!(text.contains("back to 2026-03-31"), "the six-month window has a date: {text}");
    let doc = show_doc("ibkr");
    assert_eq!(doc["channels"][0]["depth"]["steps"][0]["since"], "2026-03-31", "{doc}");
    let later = show_lines(&row_of("ibkr"), today() + vike_model::MS_PER_DAY).join("\n");
    assert!(later.contains("back to 2026-04-01"), "{later}");
}

/// The block's layout: the class column, the detail indent that hangs the cells under the name, and
/// the cells in one fixed order.
#[test]
fn a_channels_cells_hang_under_its_name_in_a_fixed_order() {
    assert_eq!(DETAIL_INDENT.len(), 2 + CLASS_W + 1, "the indent is the width of the class column");
    for &venue in VENUES {
        for ch in history_channels_for(venue) {
            assert!(
                ch.class.word().len() <= CLASS_W,
                "{venue}: `{}` overflows the column",
                ch.name
            );
        }
    }
    let lines = channel_lines(&history_channels_for("oanda")[0], today());
    assert_eq!(lines[0], "  request v20 REST candles");
    let labels = ["serves:", "depth:", "per request:", "pace:", "access:", "state:", "evidence:"];
    for (i, label) in labels.iter().enumerate() {
        assert!(
            lines[i + 1].starts_with(&format!("{DETAIL_INDENT}{label}")),
            "`{label}` out of place: {lines:?}"
        );
    }
}

/// A row with several sources prints the first beside the `evidence:` label and hangs each further
/// one under it, on a line whose label is blank — so the sources read as one list.
#[test]
fn a_further_evidence_source_hangs_under_the_first() {
    let oanda = history_channels_for("oanda")[0];
    let lines = channel_lines(&oanda, today());
    let sources = oanda.evidence_lines().len();
    assert!(sources > 1, "the control needs a row with several sources");
    let first = lines
        .iter()
        .position(|l| l.trim_start().starts_with("evidence:"))
        .unwrap_or_else(|| panic!("no evidence line: {lines:?}"));
    let continuation = format!("{DETAIL_INDENT}{:<w$}", "", w = LABEL_W);
    for line in &lines[first + 1..first + sources] {
        assert!(
            line.starts_with(&continuation) && !line.trim().is_empty(),
            "a further source hangs under the first: {line:?}"
        );
    }
}

/// The three shapes a row can be print three different ways: a channel prints its cells (the test
/// above), a finding prints the finding and nothing that would describe a channel (this one), and a
/// blank says it is blank (the next).
#[test]
fn a_finding_prints_the_finding_and_no_cell_that_would_describe_a_channel() {
    let finding = channel_lines(&history_channels_for("oanda")[1], today());
    assert!(finding[0].contains("bulk archive"), "{finding:?}");
    assert!(finding.iter().any(|l| l.contains("none found in what was read")), "{finding:?}");
    assert!(
        !finding.iter().any(|l| l.contains("serves:") || l.contains("depth:")),
        "a finding describes no channel: {finding:?}"
    );
}

/// A row nobody classified prints that it is blank and why, and nothing else — two lines, the name
/// and the reason — so a scaffolded venue can never read as a channel with unknown limits.
#[test]
fn a_blank_prints_that_it_is_blank() {
    let blank = channel_lines(&history_channels_for("fxcm")[0], today());
    assert_eq!(blank.len(), 2, "{blank:?}");
    assert!(blank[1].contains("not classified —"), "{blank:?}");
}

/// Evidence prints with its date and its source — the property that keeps a row from being an
/// assertion nobody can check — and a blank cell says which kind of blank it is.
#[test]
fn evidence_prints_with_its_date_and_a_blank_says_which_kind() {
    let oanda = show_text("oanda");
    assert!(oanda.contains("vendor documentation, read 2026-09-30"), "{oanda}");
    assert!(oanda.contains("measured 2026-09-30"), "{oanda}");
    let binance = show_text("binance");
    assert!(
        binance.contains("not known (unmeasured)"),
        "nobody read a source for binance: {binance}"
    );
    let ibkr = show_text("ibkr");
    assert!(ibkr.contains("not stated by the sources read"), "IBKR's ticks: {ibkr}");
}

/// **THE RULE THE TABLE EXISTS FOR, at the terminal.** No venue's `show` ever says a depth is
/// unlimited — the vendor's silence is not a promise. (The footnote names neither word.)
#[test]
fn no_channel_line_ever_says_unlimited() {
    for &venue in VENUES {
        let text = show_text(venue).to_lowercase();
        for word in ["unlimited", "no limit", "unbounded", "infinite"] {
            assert!(!text.contains(word), "`show {venue}` says {word:?}: {text}");
        }
    }
}

/// **THE FIRST OF THE TWO WORDINGS THAT WERE FALSE: the venue-class COST cell.** It said "no
/// credentials — public market data", which is false for a venue whose history needs a token or a
/// gateway session. The row is a CLASS, so it now says most venues are public, that a credentialed
/// one is MARKED, and where to look.
#[test]
fn the_cost_cell_no_longer_claims_every_venue_needs_no_credential() {
    let cost = built_row(Source::Venue).cost;
    assert!(!cost.to_lowercase().contains("no credentials"), "{cost}");
    assert!(cost.contains("marked"), "a credentialed venue is marked: {cost}");
    assert!(cost.contains("data source show <venue>"), "…and the cell says where to look: {cost}");
}
