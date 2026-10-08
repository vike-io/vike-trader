use super::*;

/// The day every date-dependent assertion is written against: a fixed instant, never the clock, so
/// these tests read the same tomorrow.
fn today() -> i64 {
    vike_model::time::days_from_civil(2026, 9, 30) * MS_PER_DAY
}

fn row(venue: &str, name_part: &str) -> HistoryChannel {
    *history_channels_for(venue)
        .iter()
        .find(|r| r.name.contains(name_part))
        .unwrap_or_else(|| panic!("no {venue} row whose name contains {name_part:?}"))
}

/// **A rolling window resolves to a DATE when the caller knows today, and says how it counts when
/// it does not.** The committed page cannot carry a date that changes daily, so it must take the
/// second form; the terminal takes the first.
#[test]
fn a_window_resolves_to_a_date_when_today_is_known_and_says_so_when_it_is_not() {
    let window = HistoryDepth::Lookback { days: 183 };
    assert_eq!(
        window.text(Some(today()), "unknown"),
        "the last 183 days (back to 2026-03-31)",
        "2026-09-30 minus 183 days"
    );
    assert_eq!(
        window.text(None, "unknown"),
        "the last 183 days, counted back from the day you ask"
    );
    // The same instant a day later moves the date by a day — the reason a window stores days.
    assert_eq!(
        window.text(Some(today() + MS_PER_DAY), "unknown"),
        "the last 183 days (back to 2026-04-01)"
    );
}

/// Every shape of depth in words, on the rows that carry it. IBKR's is the one that composes.
#[test]
fn depth_is_worded_for_every_shape() {
    let ibkr = row("ibkr", "reqHistoricalData");
    assert_eq!(
        ibkr.depth_text(Some(today())),
        "bars of 30 seconds or less: only the last 183 days (back to 2026-03-31); everything \
         else: back to each instrument's own first data, which differs per instrument; ask with \
         reqHeadTimestamp, per instrument and data type"
    );
    let oanda = row("oanda", "candles");
    assert!(oanda.depth_text(None).starts_with("since 2005-01-03 (5-second candles"), "{oanda:?}");
    assert!(row("dukascopy", "JForex").depth_text(None).contains("getTimeOfFirstCandle"));
}

/// **A blank is worded by whether anybody looked.** "Not known" says no source was read;
/// "not stated" says one was and it is silent. The two are different claims and a reader must not
/// have to guess which they are holding.
#[test]
fn a_blank_is_worded_by_whether_anyone_looked() {
    let unread = row("binance", "klines");
    assert_eq!(unread.depth_text(None), "not known (unmeasured)");
    assert_eq!(unread.per_request_text(), "not known (unmeasured)");
    assert_eq!(unread.pace_text(), "not known (unmeasured)");

    let silent = row("ibkr", "reqHistoricalTicks");
    assert_eq!(silent.depth_text(None), "not stated by the sources read");
    assert_eq!(silent.pace_text(), "not stated by the sources read");
}

/// **THE RULE THE WHOLE TABLE EXISTS FOR.** No rendering of any row — a cell, the JSON, a note, the
/// page — ever says a depth is unlimited, unbounded or absent. The vendor's silence is not a
/// promise. (The page's own head states the rule in a sentence that names the word, so the body
/// is scanned rather than the whole page.)
#[test]
fn no_rendering_ever_says_unlimited() {
    fn clean(what: &str, text: &str) {
        let lower = text.to_lowercase();
        for word in ["unlimited", "no limit", "unbounded", "infinite", "no restriction"] {
            assert!(!lower.contains(word), "{what} says {word:?}: {text}");
        }
    }
    for &venue in vike_model::VENUES {
        for r in history_channels_for(venue) {
            let what = format!("{venue}/{}", r.name);
            clean(&what, &r.kinds_text());
            clean(&what, &r.depth_text(None));
            clean(&what, &r.depth_text(Some(today())));
            clean(&what, &r.per_request_text());
            clean(&what, &r.pace_text());
            clean(&what, &r.access_text());
            clean(&what, &r.state_text());
            clean(&what, &r.evidence_lines().join("\n"));
            clean(&what, r.note);
            clean(&what, &r.as_json(today()).to_string());
        }
    }
    let page = history_reference();
    let body = page.strip_prefix(REFERENCE_HEAD).expect("the page opens with its head");
    clean("the reference page body", body);
    // The control: the head DOES name the word, so the scan above is not passing because the
    // word was never in the vocabulary.
    assert!(REFERENCE_HEAD.contains("not the same as unlimited"));
}

/// Every row becomes one JSON object whose cells each carry a `kind` a consumer can branch on and
/// a `text` a terminal prints — and a blank depth is `unstated`, never a number.
#[test]
fn every_row_is_a_json_object_with_discriminated_cells() {
    for &venue in vike_model::VENUES {
        for r in history_channels_for(venue) {
            let doc = r.as_json(today());
            let what = format!("{venue}/{}", r.name);
            assert_eq!(doc["class"], r.class.word(), "{what}");
            assert_eq!(doc["name"], r.name, "{what}");
            assert!(doc["kinds"].is_array(), "{what}: {doc}");
            for cell in ["depth", "per_request", "pace", "access", "state", "evidence"] {
                assert!(doc[cell]["kind"].is_string(), "{what}: `{cell}` has no kind: {doc}");
            }
            for cell in ["depth", "per_request", "pace", "access", "state"] {
                let text = doc[cell]["text"].as_str().unwrap_or("");
                assert!(!text.is_empty(), "{what}: `{cell}` has no text: {doc}");
            }
            if r.depth == HistoryDepth::Unstated {
                assert_eq!(doc["depth"]["kind"], "unstated", "{what}");
                assert!(doc["depth"].get("days").is_none() && doc["depth"].get("date").is_none());
            }
            let presence = doc["presence"].as_str().expect("a presence");
            assert!(["serves", "none_found", "not_classified"].contains(&presence), "{what}");
        }
    }
}

/// A window in JSON carries the date it resolved to, so a consumer never redoes the arithmetic.
#[test]
fn json_resolves_a_window_to_its_date() {
    let doc = row("ibkr", "reqHistoricalData").as_json(today());
    assert_eq!(doc["depth"]["kind"], "lookback_by_step");
    assert_eq!(doc["depth"]["steps"][0]["days"], 183);
    assert_eq!(doc["depth"]["steps"][0]["since"], "2026-03-31");
    assert_eq!(doc["depth"]["otherwise"]["kind"], "per_instrument");
    assert_eq!(doc["state"]["kind"], "designed");
    assert_eq!(doc["evidence"]["kind"], "sourced");
    assert_eq!(doc["evidence"]["sources"].as_array().map(Vec::len), Some(4));
}

/// The three things a row can be are three different JSON `presence` values.
#[test]
fn presence_tells_a_finding_from_a_blank_from_a_channel() {
    assert_eq!(row("oanda", "candles").presence_word(), "serves");
    assert_eq!(row("oanda", "bulk archive").presence_word(), "none_found");
    assert_eq!(row("ig", "not classified").presence_word(), "not_classified");
    // ...and the words a reader is shown differ too.
    assert!(row("oanda", "bulk archive").kinds_text().starts_with("nothing"));
    assert_eq!(row("ig", "not classified").kinds_text(), "not known (unmeasured)");
}

/// A built row names its lane; a designed one says it is not built and why.
#[test]
fn state_is_worded_for_both_shapes() {
    let built = row("dukascopy", "HTTP datafeed").state_text();
    assert!(built.starts_with("built: datahub lane TickBars"), "{built}");
    let credentialed = row("oanda", "candles").state_text();
    assert!(credentialed.starts_with("built: datahub lane CredentialedKlines"), "{credentialed}");
    let designed = row("ibkr", "reqHistoricalData").state_text();
    assert!(designed.starts_with("designed, not built: "), "{designed}");
}

/// The lane names ARE the variant identifiers — the property
/// `crates/vike-datahub/tests/history_channels_gate.rs` relies on when it looks each one up as a
/// variant of the datahub's own enum.
#[test]
fn a_lane_is_named_exactly_as_its_variant() {
    for lane in [
        HistoryLane::Klines,
        HistoryLane::TickBars,
        HistoryLane::Funding,
        HistoryLane::CredentialedKlines,
    ] {
        assert_eq!(lane.name(), format!("{lane:?}"));
        assert!(lane.describe().len() > 10);
    }
}

/// A quote tick and a trade print are both "ticks" to a reader, and the words say so.
#[test]
fn kinds_are_worded_for_a_reader() {
    assert!(HistoryKind::Quotes.phrase().contains("ticks"));
    assert!(HistoryKind::Quotes.phrase().contains("bid/ask"));
    assert!(HistoryKind::Trades.phrase().contains("ticks"));
    assert_eq!(
        row("ibkr", "reqHistoricalTicks").kinds_text(),
        "quote ticks (bid/ask), trade ticks"
    );
}

/// **The page is deterministic, complete and in roster order** — the three properties a drift gate
/// over it depends on.
#[test]
fn the_page_is_deterministic_and_covers_every_roster_venue_in_order() {
    let page = history_reference();
    assert_eq!(page, history_reference(), "the render must be a pure function of the table");
    assert!(page.ends_with('\n'), "a text file ends with a newline");
    assert!(!page.contains('\r'), "the page is LF, whatever box renders it");
    let mut at = 0;
    for &venue in vike_model::VENUES {
        let heading = format!("\n## {venue}\n");
        let found = page[at..]
            .find(&heading)
            .unwrap_or_else(|| panic!("{venue} has no section, or it is out of roster order"));
        at += found + heading.len();
    }
    assert!(page.contains("## What this page does not cover"));
}

/// A committed page cannot carry a date that moves: no window is resolved on it.
#[test]
fn the_page_resolves_no_rolling_window_to_a_date() {
    let page = history_reference();
    assert!(!page.contains("(back to "), "a resolved window would make the page wrong tomorrow");
    assert!(page.contains("counted back from the day you ask"), "the IBKR window is still stated");
}

/// The page opens with the rules a reader needs before any row: the three classes, the five forms
/// of depth, the difference between "not stated" and "not known", and how to ask for one venue.
#[test]
fn the_page_opens_with_the_honesty_rules() {
    for needle in [
        "A **request** channel",
        "A **bulk** channel",
        "A **vendor** channel",
        "**Not stated** means the sources read say nothing",
        "which is not the same as unlimited",
        "**Not known** is weaker still",
        "vike-cli data source show VENUE",
    ] {
        assert!(REFERENCE_HEAD.contains(needle), "the head lost {needle:?}");
    }
}

/// A venue's opening paragraph says what vike fetches from it TODAY — derived from its built rows,
/// never typed — and lists its channels.
#[test]
fn a_venue_paragraph_says_what_is_fetched_today() {
    let binance = venue_intro("binance", history_channels_for("binance"));
    assert!(
        binance.starts_with(
            "vike fetches bars and funding rates from binance today, through the datahub."
        ),
        "{binance}"
    );
    let dukascopy = venue_intro("dukascopy", history_channels_for("dukascopy"));
    assert!(
        dukascopy.starts_with("vike fetches quote ticks (bid/ask) from dukascopy today"),
        "{dukascopy}"
    );
    let oanda = venue_intro("oanda", history_channels_for("oanda"));
    assert!(
        oanda.starts_with("vike fetches bars from oanda today, through the datahub."),
        "{oanda}"
    );
    assert!(
        oanda.contains("\"v20 REST candles\" (request)")
            && oanda.contains("\"bulk archive\" (bulk)"),
        "{oanda}"
    );
    // A venue with no built lane says the opposite, and says it about the datahub.
    let ibkr = venue_intro("ibkr", history_channels_for("ibkr"));
    assert!(
        ibkr.starts_with("vike does not fetch anything from ibkr through the datahub today."),
        "{ibkr}"
    );
}

/// A venue whose every serving channel is blank SAYS so — a page of "not known" cells is easy to
/// skim past as if it were an answer — and a venue with anything read does not say it, because for
/// that venue the sentence would be false.
#[test]
fn a_venue_with_nothing_read_says_how_far_back_is_not_known() {
    let sentence = "How far back any of them goes has not been read or measured yet.";
    for venue in ["binance", "bybit", "okx", "aster", "deribit", "hyperliquid"] {
        let intro = venue_intro(venue, history_channels_for(venue));
        assert!(intro.contains(sentence), "{venue}: {intro}");
    }
    for venue in ["oanda", "ibkr", "dukascopy"] {
        let intro = venue_intro(venue, history_channels_for(venue));
        assert!(!intro.contains(sentence), "{venue} has rows that were read: {intro}");
    }
}

/// The page opens its venue list with one anchor per roster venue, in roster order, so a reader of
/// a page this long can jump to the venue they came for.
#[test]
fn the_page_lists_every_venue_as_a_link_to_its_section() {
    let page = history_reference();
    let line = page
        .lines()
        .find(|l| l.starts_with("**Venues:** "))
        .unwrap_or_else(|| panic!("the page has no venue list"));
    let mut at = 0;
    for &venue in vike_model::VENUES {
        let link = format!("[{venue}](#{venue})");
        let found = line[at..]
            .find(&link)
            .unwrap_or_else(|| panic!("{venue} is missing from the venue list, or out of order"));
        at += found + link.len();
    }
}

/// The three subsection shapes on the page — a channel with its facts, a finding, and a blank — each
/// its own test, so a break in one shape cannot hide a break in another.
#[test]
fn a_channel_section_lists_its_facts_and_its_evidence() {
    let serves = channel_section(&row("oanda", "candles"));
    for label in ["- **Serves:**", "- **How far back:**", "- **Per request:**", "- **Pace:**"] {
        assert!(serves.contains(label), "{label} missing: {serves}");
    }
    assert!(serves.contains("- **Access:**") && serves.contains("- **What vike does today:**"));
    assert!(serves.contains("*Evidence:*"));
}

/// A finding is worded as one and describes no channel.
#[test]
fn a_finding_section_says_none_found_and_lists_no_fact() {
    let finding = channel_section(&row("ibkr", "bulk archive"));
    assert!(finding.contains("**None found.**") && !finding.contains("- **Serves:**"), "{finding}");
}

/// A blank says it is not classified, gives its reason as one sentence and cites nothing.
#[test]
fn a_blank_section_says_not_classified_and_cites_no_evidence() {
    let blank = channel_section(&row("fxcm", "not classified"));
    assert!(blank.contains("**Not classified.**") && !blank.contains("*Evidence:*"), "{blank}");
    // The reason follows a colon and the sentence ends on a full stop, so it reads as one sentence.
    assert!(
        blank.contains("vike does not fetch it: the bridge has no data path at all"),
        "{blank}"
    );
    assert!(blank.trim_end().ends_with('.'), "{blank}");
}
