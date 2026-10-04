//! **The Data Manager's HISTORY column** — one cell per venue in the By-venue table, folded from
//! the datahub's history-channels read (`Request::HistoryChannels`,
//! `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md`), and the default-lookback
//! floors the bulk Backfill's plan takes from the same answer.
//! `docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §3 is the design.
//!
//! # What a cell says, and where its words come from
//!
//! Depth is a fact about a (venue, channel), so the cell is the depth of the venue's BUILT
//! channels — what a Backfill from this window can actually reach — and `no lane` for a venue whose
//! every row is designed (ibkr, ig, fxcm, ctrader, alpaca today). The cell is SHORT, composed from
//! each depth cell's machine fields (`since 2005-01-03`, `last 7 days`, `per instrument`); a kind
//! with no machine field prints the server's own `text` (`not known (unmeasured)`), and so does a
//! kind this build has never heard of. The hover carries every channel's full sentences, each
//! taken verbatim from the reply, and ends on [`HOVER_TAIL`]: the GUI spells no depth sentence of
//! its own.
//!
//! The server's overlay rides the same cell as flags — `not mounted`, `token absent`,
//! `store unreadable`, `token not checked` — drawn in the warning colour.
//!
//! # A datahub older than the read
//!
//! [`HistoryLoad::served`] is `false`: the cells come from THIS binary's own compiled table
//! (`vike_datahub_client::history::compiled_report`), carry no overlay flag, and the table shows
//! [`HistoryLoad::caption`] — the shape the Partial column's `PARTIALS_UNSERVED` note already uses.
//! Such a load NEVER clamps: [`lookback_floors`] answers nothing for it.
//!
//! # The clamp — wired, and inert on today's table
//!
//! [`lookback_floors`] feeds `crate::data::backfill_plan::plan_backfill_jobs`, which raises a
//! default window's start to a venue's floor. A floor exists only where
//! `vike_datahub_client::history::bars_lookback_floor_ms` finds a built, mounted, documented or
//! measured rolling window over bars — and no row has one today, so the clamp changes nothing
//! until a row that does is read (the design's §3.3 finding, the owner's Q2).

use std::collections::BTreeMap;

use vike_datahub_client::history::{
    COMPILED_TABLE_CAPTION, Cell, CredentialPresence, HistoryChannelsReport, VenueHistory,
    bars_lookback_floor_ms, compiled_report,
};

/// The cell of a venue no built channel serves.
pub const NO_LANE: &str = "no lane";

/// The last line of every cell's hover — the rows are a table, not a probe.
pub const HOVER_TAIL: &str =
    "As read by a maintainer on the dates shown; not checked against the vendor.";

/// What one background load got from the history-channels read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLoad {
    /// The rows: the SERVER's, with its overlay, when [`Self::served`]; this binary's own compiled
    /// table otherwise.
    pub report: HistoryChannelsReport,
    /// Whether the datahub answered the read. `false` is a datahub OLDER than it.
    pub served: bool,
}

impl HistoryLoad {
    /// A served answer.
    #[must_use]
    pub fn served(report: HistoryChannelsReport) -> Self {
        Self { report, served: true }
    }

    /// The fallback for a datahub older than the read: this binary's own table, resolved against
    /// `as_of_ms`, with the overlay marked not known.
    #[must_use]
    pub fn compiled(as_of_ms: i64) -> Self {
        Self { report: compiled_report(as_of_ms), served: false }
    }

    /// The caption the table shows over a fallback's cells, `None` over a served answer.
    #[must_use]
    pub fn caption(&self) -> Option<&'static str> {
        (!self.served).then_some(COMPILED_TABLE_CAPTION)
    }
}

/// One venue's HISTORY cell: its short depth text, the overlay flags beside it, and the hover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCell {
    /// The depth of the venue's built channels, or [`NO_LANE`].
    pub text: String,
    /// The server's overlay words — empty on a fallback, and empty when nothing is wrong.
    pub flags: Vec<&'static str>,
    /// Every channel, every sentence, ending on [`HOVER_TAIL`].
    pub hover: String,
}

impl HistoryCell {
    /// The cell as drawn: the text, then each flag after a middle dot.
    #[must_use]
    pub fn label(&self) -> String {
        std::iter::once(self.text.as_str())
            .chain(self.flags.iter().copied())
            .collect::<Vec<_>>()
            .join(" \u{00B7} ")
    }

    /// Whether the cell is drawn in the warning colour — whenever it carries a flag.
    #[must_use]
    pub fn warns(&self) -> bool {
        !self.flags.is_empty()
    }
}

/// One depth cell, short: a fixed start as its date, a rolling window as its length, a
/// per-instrument depth as those words — and anything else as the server's own text.
fn depth_label(cell: &Cell) -> String {
    match (cell.kind.as_str(), cell.date.as_deref(), cell.days) {
        ("since", Some(date), _) => format!("since {date}"),
        ("lookback", _, Some(days)) => format!("last {days} days"),
        ("lookback_by_step", _, _) => "per bar size".to_string(),
        ("per_instrument", _, _) => "per instrument".to_string(),
        _ => cell.text.clone(),
    }
}

/// The overlay word for a built channel's mount, or `None` when nothing is wrong.
fn mount_flag(mounted: Option<bool>) -> Option<&'static str> {
    (mounted == Some(false)).then_some("not mounted")
}

/// The overlay word for a built channel's credential, or `None` when nothing is wrong.
fn credential_flag(credential: CredentialPresence) -> Option<&'static str> {
    match credential {
        CredentialPresence::NotNeeded | CredentialPresence::Present => None,
        CredentialPresence::Absent => Some("token absent"),
        CredentialPresence::Unreadable => Some("store unreadable"),
        CredentialPresence::NotChecked => Some("token not checked"),
    }
}

/// **One venue's HISTORY cell** — see this module's doc. `served` is [`HistoryLoad::served`]: a
/// fallback cell carries no flag, because no server said anything about its own lanes.
#[must_use]
pub fn history_cell(venue: &VenueHistory, served: bool) -> HistoryCell {
    let built: Vec<_> = venue.channels.iter().filter(|c| c.state.kind == "built").collect();
    let mut labels: Vec<String> = Vec::new();
    for ch in &built {
        let label = depth_label(&ch.depth);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    let text = if labels.is_empty() { NO_LANE.to_string() } else { labels.join(", ") };

    let mut flags: Vec<&'static str> = Vec::new();
    if served {
        for ch in &built {
            for flag in
                [mount_flag(ch.mounted), credential_flag(ch.credential)].into_iter().flatten()
            {
                if !flags.contains(&flag) {
                    flags.push(flag);
                }
            }
        }
    }

    let mut hover: Vec<String> = Vec::new();
    if !served {
        hover.push(format!("{COMPILED_TABLE_CAPTION}."));
    }
    for ch in &venue.channels {
        hover.push(format!("{} channel — {}", ch.class, ch.name));
        hover.push(format!("  depth: {}", ch.depth.text));
        hover.push(format!("  access: {}", ch.access.text));
        hover.push(format!("  state: {}", ch.state.text));
        if served {
            match ch.mounted {
                Some(true) => hover.push("  mounted on this datahub".to_string()),
                Some(false) => hover.push("  NOT mounted on this datahub".to_string()),
                None => {}
            }
            if ch.credential != CredentialPresence::NotNeeded {
                hover.push(format!("  credential: {}", ch.credential.phrase()));
            }
        }
        for e in &ch.evidence {
            hover.push(format!("  evidence: {}", e.text));
        }
    }
    hover.push(HOVER_TAIL.to_string());
    HistoryCell { text, flags, hover: hover.join("\n") }
}

/// The cell a venue gets before any load has answered the history-channels read.
pub const NOT_LOADED: &str = "not loaded";

/// **What the By-venue table draws in one row's HISTORY cell**: `(label, warns, hover)`. With no
/// load yet it says [`NOT_LOADED`] rather than drawing nothing; a venue the answer does not carry
/// (a server whose roster predates the venue) says so; every other venue is [`history_cell`].
#[must_use]
pub fn column_cell(load: Option<&HistoryLoad>, venue: &str) -> (String, bool, String) {
    let Some(load) = load else {
        return (
            NOT_LOADED.to_string(),
            false,
            "The datahub's history-channels answer has not loaded yet — it rides the inventory \
             load, and Refresh asks again."
                .to_string(),
        );
    };
    match load.report.venue(venue) {
        Some(v) => {
            let cell = history_cell(v, load.served);
            (cell.label(), cell.warns(), cell.hover)
        }
        None => (
            "not declared".to_string(),
            false,
            "This datahub's table declares no history channels for this venue — a statement about \
             that table, not about the venue."
                .to_string(),
        ),
    }
}

/// **The default-lookback floor per venue, epoch-ms, counted back from `now_ms`** — what the bulk
/// Backfill's plan raises a default window's start to. EMPTY for no load and for a fallback: a
/// table no server confirmed is never a reason to shorten a request. A served answer yields a floor
/// for exactly the venues `bars_lookback_floor_ms` finds one for — none, on today's table.
#[must_use]
pub fn lookback_floors(load: Option<&HistoryLoad>, now_ms: i64) -> BTreeMap<String, i64> {
    let Some(load) = load.filter(|l| l.served) else { return BTreeMap::new() };
    load.report
        .venues
        .iter()
        .filter_map(|v| bars_lookback_floor_ms(v, now_ms).map(|floor| (v.venue.clone(), floor)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_datahub_client::history::ChannelReport;

    /// 2026-10-02T00:00:00Z.
    const AS_OF: i64 = 1_790_899_200_000;
    const DAY: i64 = 86_400_000;

    fn compiled_venue(venue: &str) -> VenueHistory {
        HistoryLoad::compiled(AS_OF).report.venue(venue).expect("a roster venue").clone()
    }

    /// The same rows as a SERVED answer, every built lane mounted and every credential stored.
    fn served_venue(venue: &str) -> VenueHistory {
        let mut v = compiled_venue(venue);
        for ch in &mut v.channels {
            if ch.state.kind == "built" {
                ch.mounted = Some(true);
                if ch.credential == CredentialPresence::NotChecked {
                    ch.credential = CredentialPresence::Present;
                }
            }
        }
        v
    }

    /// **The design's planted cells** (§4, PR 2's first row): OANDA's fixed start, a lane that is
    /// not mounted, a token that is absent, and a venue with no lane at all.
    #[test]
    fn each_cell_says_its_depth_and_the_servers_overlay() {
        let oanda = history_cell(&served_venue("oanda"), true);
        assert_eq!(oanda.label(), "since 2005-01-03");
        assert!(!oanda.warns());

        let mut absent = served_venue("oanda");
        absent.channels[0].credential = CredentialPresence::Absent;
        let cell = history_cell(&absent, true);
        assert_eq!(cell.label(), "since 2005-01-03 \u{00B7} token absent");
        assert!(cell.warns());

        let mut unmounted = served_venue("bybit");
        unmounted.channels[0].mounted = Some(false);
        let cell = history_cell(&unmounted, true);
        assert_eq!(cell.label(), "not known (unmeasured) \u{00B7} not mounted");

        let mut unreadable = served_venue("oanda");
        unreadable.channels[0].credential = CredentialPresence::Unreadable;
        assert_eq!(history_cell(&unreadable, true).flags, vec!["store unreadable"]);

        let ibkr = history_cell(&served_venue("ibkr"), true);
        assert_eq!(ibkr.label(), NO_LANE, "a venue with only designed rows has no lane");
        let dukascopy = history_cell(&served_venue("dukascopy"), true);
        assert_eq!(dukascopy.label(), "per instrument");
        // Two built rows with one depth say it once.
        assert_eq!(history_cell(&served_venue("binance"), true).text, "not known (unmeasured)");
    }

    /// The hover lists EVERY channel — built and designed — with its sentences, the server's overlay
    /// when there is one, and the line saying it is a table rather than a probe.
    #[test]
    fn the_hover_lists_every_channel_and_ends_on_the_tail() {
        let v = served_venue("oanda");
        let hover = history_cell(&v, true).hover;
        for ch in &v.channels {
            assert!(hover.contains(&ch.name), "{hover}");
            assert!(hover.contains(&ch.depth.text), "{hover}");
        }
        assert!(hover.contains("mounted on this datahub"), "{hover}");
        assert!(hover.contains(CredentialPresence::Present.phrase()), "{hover}");
        assert!(hover.contains("vendor documentation, read 2026-09-30"), "{hover}");
        assert!(hover.trim_end().ends_with(HOVER_TAIL), "{hover}");
    }

    /// A FALLBACK cell — a datahub older than the read — carries no flag even where the compiled
    /// row's word would be one, says in its hover whose table it is, and the load carries the
    /// caption the table shows.
    #[test]
    fn a_fallback_cell_carries_no_overlay_and_the_load_its_caption() {
        let load = HistoryLoad::compiled(AS_OF);
        let oanda = load.report.venue("oanda").expect("oanda");
        assert_eq!(oanda.channels[0].credential, CredentialPresence::NotChecked, "guard");
        let cell = history_cell(oanda, false);
        assert_eq!(cell.label(), "since 2005-01-03", "no flag on a table no server confirmed");
        assert!(cell.hover.starts_with(COMPILED_TABLE_CAPTION), "{}", cell.hover);
        assert!(!cell.hover.contains("mounted on this datahub"), "{}", cell.hover);
        assert_eq!(load.caption(), Some(COMPILED_TABLE_CAPTION));
        assert_eq!(HistoryLoad::served(load.report.clone()).caption(), None);
    }

    /// A depth form this build has never heard of prints the SERVER's text — the open-kind promise,
    /// carried to the cell.
    #[test]
    fn an_unknown_depth_kind_prints_the_servers_text() {
        let mut v = served_venue("bybit");
        v.channels[0].depth = Cell {
            kind: "candle_count_from_a_newer_server".into(),
            text: "the last 5000 candles".into(),
            date: None,
            days: None,
        };
        assert_eq!(history_cell(&v, true).text, "the last 5000 candles");
    }

    /// A served bars row with a documented 7-day window, mounted.
    fn seven_day_row(base: &ChannelReport) -> ChannelReport {
        ChannelReport {
            depth: Cell {
                kind: "lookback".into(),
                text: "the last 7 days".into(),
                date: None,
                days: Some(7),
            },
            state: Cell { kind: "built".into(), text: "built".into(), date: None, days: None },
            kinds: vec!["bars".into()],
            mounted: Some(true),
            evidence: vec![Cell {
                kind: "documented".into(),
                text: "vendor page".into(),
                date: Some("2026-09-30".into()),
                days: None,
            }],
            ..base.clone()
        }
    }

    /// **The clamp's two sides** (§4, PR 2's second and third rows): a SERVED answer with a
    /// documented 7-day window yields that venue's floor — and the SAME rows in a fallback yield
    /// none, because a table no server confirmed never shortens a request.
    #[test]
    fn a_served_window_yields_a_floor_and_a_fallback_never_does() {
        let mut served = HistoryLoad::served(HistoryLoad::compiled(AS_OF).report);
        let bybit = served.report.venues.iter_mut().find(|v| v.venue == "bybit").expect("bybit");
        bybit.channels[0] = seven_day_row(&bybit.channels[0]);
        let floors = lookback_floors(Some(&served), AS_OF);
        assert_eq!(floors, BTreeMap::from([("bybit".to_string(), AS_OF - 7 * DAY)]));

        let unserved = HistoryLoad { served: false, ..served.clone() };
        assert!(lookback_floors(Some(&unserved), AS_OF).is_empty(), "a fallback clamped");
        assert!(lookback_floors(None, AS_OF).is_empty(), "no load, no floor");

        // A REPORTED source beside the window: no floor (the evidence rule).
        let mut reported = served.clone();
        let bybit = reported.report.venues.iter_mut().find(|v| v.venue == "bybit").expect("bybit");
        bybit.channels[0].evidence[0].kind = "reported".into();
        assert!(lookback_floors(Some(&reported), AS_OF).is_empty(), "a reported row clamped");
    }

    /// The design's finding, held at the GUI's seam too: on today's table, served and every lane
    /// mounted, no venue gets a floor.
    #[test]
    fn todays_table_yields_no_floor() {
        let mut load = HistoryLoad::served(HistoryLoad::compiled(AS_OF).report);
        for v in &mut load.report.venues {
            for ch in &mut v.channels {
                if ch.state.kind == "built" {
                    ch.mounted = Some(true);
                }
            }
        }
        assert!(lookback_floors(Some(&load), AS_OF).is_empty());
    }

    /// The column never draws nothing: no load yet is [`NOT_LOADED`], a venue the answer does not
    /// carry says so, and a carried one is its [`history_cell`].
    #[test]
    fn the_column_says_not_loaded_and_not_declared_rather_than_nothing() {
        let (label, warns, hover) = column_cell(None, "oanda");
        assert_eq!((label.as_str(), warns), (NOT_LOADED, false));
        assert!(hover.contains("Refresh"), "{hover}");

        let load = HistoryLoad::served(HistoryLoad::compiled(AS_OF).report);
        let (label, _, _) = column_cell(Some(&load), "a-venue-from-a-newer-build");
        assert_eq!(label, "not declared");
        let (label, _, hover) = column_cell(Some(&load), "oanda");
        assert_eq!(label, history_cell(load.report.venue("oanda").expect("oanda"), true).label());
        assert!(hover.ends_with(HOVER_TAIL), "{hover}");
    }
}
