//! `history_render` — **the sentences a [`HistoryChannel`] row is read aloud in, its JSON, and the
//! reference page written from it.**
//!
//! Three consumers read a row and must not word it differently: `vike-cli data source show` (a
//! terminal), the same verb's `--json`, and the generated reference page
//! (`docs/reference/history-channels.md`). So the phrasing lives HERE, once, beside the table, and
//! each consumer composes its own layout from these methods rather than re-spelling a cell.
//!
//! # A rolling window resolves to a date at RENDER time
//!
//! [`HistoryDepth::Lookback`] stores days and no date, because "the last 183 days" moves every
//! day and a stored date would be wrong by tomorrow. The CLI passes today and gets "back to
//! 2026-03-31"; the reference page passes nothing, because a committed page cannot carry a
//! date that changes daily, and says "counted back from the day you ask" instead. Every function
//! here is pure — the clock is the caller's.
//!
//! # ⚠ The three blanks are worded differently on purpose
//!
//! A cell that is [`HistoryDepth::Unstated`] (or its siblings) reads "not stated by the sources
//! read" when a source was read and is silent, and "not known (unmeasured)" when nobody looked.
//! Neither is ever "unlimited", "no limit" or anything a reader could take for a promise;
//! `no_rendering_ever_says_unlimited` scans every row's every rendering for it.

use serde_json::{Value, json};
use vike_model::{MS_PER_DAY, epoch_ms_to_utc_date};

use crate::history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, HistoryKind, HistoryLane, Pace, PerRequest, history_channels_for,
};

/// The UTC date `days` days before the day `today_ms` falls on.
fn date_before(today_ms: i64, days: u32) -> String {
    epoch_ms_to_utc_date(today_ms - i64::from(days) * MS_PER_DAY)
}

/// A rolling window in words, with its date when the caller knows today.
fn window_text(days: u32, today_ms: Option<i64>) -> String {
    match today_ms {
        Some(today) => format!("the last {days} days (back to {})", date_before(today, days)),
        None => format!("the last {days} days, counted back from the day you ask"),
    }
}

impl ChannelClass {
    /// The lowercase word every rendering and the JSON use.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            ChannelClass::Request => "request",
            ChannelClass::Bulk => "bulk",
            ChannelClass::Vendor => "vendor",
        }
    }
}

impl HistoryKind {
    /// The token the JSON carries.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            HistoryKind::Bars => "bars",
            HistoryKind::Quotes => "quotes",
            HistoryKind::Trades => "trades",
            HistoryKind::Book => "book",
            HistoryKind::Funding => "funding",
        }
    }

    /// The words a reader is shown. A quote tick and a trade print are both "ticks" to a user, so
    /// each says so.
    #[must_use]
    pub fn phrase(self) -> &'static str {
        match self {
            HistoryKind::Bars => "bars",
            HistoryKind::Quotes => "quote ticks (bid/ask)",
            HistoryKind::Trades => "trade ticks",
            HistoryKind::Book => "order-book updates",
            HistoryKind::Funding => "funding rates",
        }
    }
}

impl HistoryLane {
    /// The lane's name, spelt exactly as `crates/vike-datahub/src/backfill.rs`'s `BackfillLane`
    /// spells the variant — the string `crates/vike-ops/tests/history_channels_gate.rs` matches
    /// against that file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            HistoryLane::Klines => "Klines",
            HistoryLane::TickBars => "TickBars",
            HistoryLane::Funding => "Funding",
            HistoryLane::CredentialedKlines => "CredentialedKlines",
        }
    }

    /// What the lane produces, in words.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            HistoryLane::Klines => "the venue's own bars",
            HistoryLane::TickBars => "ticks stored as quotes, bars resampled from them",
            HistoryLane::Funding => "the funding-rate series",
            HistoryLane::CredentialedKlines => {
                "the venue's own bars, through a credential the operator stored"
            }
        }
    }
}

impl ChannelState {
    /// The state in one sentence.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            ChannelState::Built(lane) => {
                format!("built: datahub lane {} ({})", lane.name(), lane.describe())
            }
            ChannelState::Designed(reason) => format!("designed, not built: {reason}"),
        }
    }
}

impl EvidenceSource {
    /// One source in one line, with its date.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            EvidenceSource::Documented { url, checked } => {
                format!("vendor documentation, read {checked}: {url}")
            }
            EvidenceSource::Measured { on, by } => format!("measured {on}: {by}"),
            EvidenceSource::Reported { by, url, checked } => {
                format!("third-party report, read {checked}: {by} — {url}")
            }
        }
    }
}

impl HistoryDepth {
    /// The depth in words. `today_ms` is `Some` to resolve a rolling window to a date and `None`
    /// for the committed page; `unknown` is what an [`HistoryDepth::Unstated`] says, which the row
    /// chooses because only the row knows whether a source was read.
    #[must_use]
    pub fn text(&self, today_ms: Option<i64>, unknown: &str) -> String {
        match self {
            HistoryDepth::Since { date, scope } => format!("since {date} ({scope})"),
            HistoryDepth::Lookback { days } => window_text(*days, today_ms),
            HistoryDepth::LookbackByStep { steps, otherwise } => {
                let mut parts: Vec<String> = steps
                    .iter()
                    .map(|s| format!("{}: only {}", s.bars, window_text(s.days, today_ms)))
                    .collect();
                parts.push(format!("everything else: {}", otherwise.text(today_ms, unknown)));
                parts.join("; ")
            }
            HistoryDepth::PerInstrument { probe } => format!(
                "back to each instrument's own first data, which differs per instrument; ask \
                 with {probe}"
            ),
            HistoryDepth::Unstated => unknown.to_string(),
        }
    }

    /// The depth as a JSON object with a `kind` discriminator, its fields, and `text`.
    fn as_json(&self, today_ms: i64, unknown: &str) -> Value {
        let text = self.text(Some(today_ms), unknown);
        match self {
            HistoryDepth::Since { date, scope } => {
                json!({ "kind": "since", "date": date, "scope": scope, "text": text })
            }
            HistoryDepth::Lookback { days } => json!({
                "kind": "lookback",
                "days": days,
                "since": date_before(today_ms, *days),
                "text": text,
            }),
            HistoryDepth::LookbackByStep { steps, otherwise } => {
                let steps: Vec<Value> = steps
                    .iter()
                    .map(|s| {
                        json!({
                            "bars": s.bars,
                            "days": s.days,
                            "since": date_before(today_ms, s.days),
                        })
                    })
                    .collect();
                json!({
                    "kind": "lookback_by_step",
                    "steps": steps,
                    "otherwise": otherwise.as_json(today_ms, unknown),
                    "text": text,
                })
            }
            HistoryDepth::PerInstrument { probe } => {
                json!({ "kind": "per_instrument", "probe": probe, "text": text })
            }
            HistoryDepth::Unstated => json!({ "kind": "unstated", "text": text }),
        }
    }
}

impl PerRequest {
    /// The per-request size in words; `unknown` as for [`HistoryDepth::text`].
    #[must_use]
    pub fn text(&self, unknown: &str) -> String {
        match self {
            PerRequest::Rows(n) => format!("at most {n} rows per request"),
            PerRequest::Span { days } => format!("one request spans at most {days} days"),
            PerRequest::File(what) => format!("one file per request: {what}"),
            PerRequest::SeeVendor(text) => (*text).to_string(),
            PerRequest::Unstated => unknown.to_string(),
        }
    }

    fn as_json(&self, unknown: &str) -> Value {
        let text = self.text(unknown);
        match self {
            PerRequest::Rows(n) => json!({ "kind": "rows", "rows": n, "text": text }),
            PerRequest::Span { days } => json!({ "kind": "span", "days": days, "text": text }),
            PerRequest::File(what) => json!({ "kind": "file", "file": what, "text": text }),
            PerRequest::SeeVendor(_) => json!({ "kind": "see_vendor", "text": text }),
            PerRequest::Unstated => json!({ "kind": "unstated", "text": text }),
        }
    }
}

impl Pace {
    /// The pace in words; `unknown` as for [`HistoryDepth::text`].
    #[must_use]
    pub fn text(&self, unknown: &str) -> String {
        match self {
            Pace::Stated(text) => (*text).to_string(),
            Pace::Unstated => unknown.to_string(),
        }
    }

    fn as_json(&self, unknown: &str) -> Value {
        let kind = match self {
            Pace::Stated(_) => "stated",
            Pace::Unstated => "unstated",
        };
        json!({ "kind": kind, "text": self.text(unknown) })
    }
}

impl Access {
    /// What it takes, in words; `unknown` as for [`HistoryDepth::text`].
    #[must_use]
    pub fn text(&self, unknown: &str) -> String {
        match self {
            Access::Keyless => "keyless: no credential needed".to_string(),
            Access::Credential(what) => format!("needs {what}"),
            Access::Session(what) => format!("needs {what}"),
            Access::Paid { unit, note } => format!("paid: {unit}. {note}"),
            Access::Unstated => unknown.to_string(),
        }
    }

    fn as_json(&self, unknown: &str) -> Value {
        let text = self.text(unknown);
        match self {
            Access::Keyless => json!({ "kind": "keyless", "text": text }),
            Access::Credential(_) => json!({ "kind": "credential", "text": text }),
            Access::Session(_) => json!({ "kind": "session", "text": text }),
            Access::Paid { unit, note } => {
                json!({ "kind": "paid", "unit": unit, "note": note, "text": text })
            }
            Access::Unstated => json!({ "kind": "unstated", "text": text }),
        }
    }
}

impl HistoryEvidence {
    /// One line per source; a row with none says so.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        match self {
            HistoryEvidence::Unmeasured => vec![
                "none: no source was read and nothing was measured for this row's limits"
                    .to_string(),
            ],
            HistoryEvidence::Sourced(sources) => sources.iter().map(EvidenceSource::text).collect(),
        }
    }

    fn as_json(&self) -> Value {
        match self {
            HistoryEvidence::Unmeasured => json!({ "kind": "unmeasured", "sources": [] }),
            HistoryEvidence::Sourced(sources) => {
                let sources: Vec<Value> = sources
                    .iter()
                    .map(|source| match source {
                        EvidenceSource::Documented { url, checked } => {
                            json!({ "kind": "documented", "url": url, "checked": checked })
                        }
                        EvidenceSource::Measured { on, by } => {
                            json!({ "kind": "measured", "on": on, "by": by })
                        }
                        EvidenceSource::Reported { by, url, checked } => {
                            json!({ "kind": "reported", "by": by, "url": url, "checked": checked })
                        }
                    })
                    .collect();
                json!({ "kind": "sourced", "sources": sources })
            }
        }
    }
}

impl HistoryChannel {
    /// What a blank cell says on THIS row: "not known" when nobody looked, "not stated" when a
    /// source was read and is silent. See this module's doc.
    fn unknown_word(&self) -> &'static str {
        match self.evidence {
            HistoryEvidence::Unmeasured => "not known (unmeasured)",
            HistoryEvidence::Sourced(_) => "not stated by the sources read",
        }
    }

    /// `serves`, `none_found` or `not_classified` — the three things a row can be, as the JSON
    /// carries them.
    #[must_use]
    pub fn presence_word(&self) -> &'static str {
        if self.is_absent() {
            "none_found"
        } else if self.is_unclassified() {
            "not_classified"
        } else {
            "serves"
        }
    }

    /// What the channel serves, in words.
    #[must_use]
    pub fn kinds_text(&self) -> String {
        if self.is_unclassified() {
            return self.unknown_word().to_string();
        }
        if self.is_absent() {
            return "nothing: no such channel was found".to_string();
        }
        self.kinds.iter().map(|k| k.phrase()).collect::<Vec<_>>().join(", ")
    }

    /// How far back, in words. `today_ms` as for [`HistoryDepth::text`].
    #[must_use]
    pub fn depth_text(&self, today_ms: Option<i64>) -> String {
        self.depth.text(today_ms, self.unknown_word())
    }

    /// How much one request carries, in words.
    #[must_use]
    pub fn per_request_text(&self) -> String {
        self.per_request.text(self.unknown_word())
    }

    /// How fast it may be pulled, in words.
    #[must_use]
    pub fn pace_text(&self) -> String {
        self.pace.text(self.unknown_word())
    }

    /// What it takes, in words.
    #[must_use]
    pub fn access_text(&self) -> String {
        self.access.text(self.unknown_word())
    }

    /// What vike does with it today, in one sentence.
    #[must_use]
    pub fn state_text(&self) -> String {
        self.state.text()
    }

    /// One line per evidence source, each dated.
    #[must_use]
    pub fn evidence_lines(&self) -> Vec<String> {
        self.evidence.lines()
    }

    /// The whole row as one JSON object, with every rolling window resolved against `today_ms`.
    /// Each cell is an object carrying a `kind` discriminator, its own fields and a `text` — the
    /// sentence the terminal prints — so a consumer can branch on the kind and never has to parse
    /// the sentence. An unstated depth is `{"kind": "unstated"}`, never a number and never
    /// anything that reads as unlimited.
    #[must_use]
    pub fn as_json(&self, today_ms: i64) -> Value {
        let unknown = self.unknown_word();
        let kinds: Vec<&str> = self.kinds.iter().map(|k| k.word()).collect();
        let state = match self.state {
            ChannelState::Built(lane) => json!({
                "kind": "built",
                "lane": lane.name(),
                "text": self.state.text(),
            }),
            ChannelState::Designed(reason) => json!({
                "kind": "designed",
                "reason": reason,
                "text": self.state.text(),
            }),
        };
        json!({
            "class": self.class.word(),
            "name": self.name,
            "presence": self.presence_word(),
            "kinds": kinds,
            "kinds_text": self.kinds_text(),
            "depth": self.depth.as_json(today_ms, unknown),
            "per_request": self.per_request.as_json(unknown),
            "pace": self.pace.as_json(unknown),
            "access": self.access.as_json(unknown),
            "state": state,
            "evidence": self.evidence.as_json(),
            "note": self.note,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// The reference page.
// ---------------------------------------------------------------------------------------------

/// Everything above the first venue. Written in prose because a page that is one line per cell
/// answers "what fields exist" and not "what can I get" — and it carries the honesty rules of
/// [`crate::history_channels_for`]'s table in the words a reader needs them in.
const REFERENCE_HEAD: &str = "\
# History channels — how far back each venue goes

*Generated from the table in `crates/vike-catalog/src/history.rs` by the renderer beside it. Do not \
edit this page: change a row there, regenerate the page with `cargo run -q -p vike-catalog \
--example history_reference`, and read the diff. The `the_page_equals_its_render` test in \
`crates/vike-ops/tests/history_channels_gate.rs` fails while the two differ.*

You can ask what a venue offers before you spend an hour asking it for data. `vike-cli data source \
show VENUE` shows the rows below for that venue, with a rolling window resolved to a date as of the \
day you ask. It reaches nothing: every line comes from a table compiled into the binary, each \
dated by the day a maintainer read or measured it, and none of it is checked against the vendor \
when you run the command.

## How to read a channel

A **channel** is one way to get a venue's history. A **request** channel is the venue's own query \
API, recent or deep depending on what you ask for. A **bulk** channel is a store the venue itself \
offers, files in a bucket rather than a query. A **vendor** channel is a third party that \
republishes the history. So \"archive or recent\" is not a switch: it is which channels a venue \
has, and what each one costs and reaches.

**How far back** has five forms, and the last two need care. A **date** says history starts then, \
for the instruments named beside it. A **window** says only the last N days are served, so the \
oldest date you can ask for moves every day. A window can also depend on the bar size, with the \
rest stated separately. **Per instrument** says history starts at that instrument's own first \
data, and names the probe that answers. **Not stated** means the sources read say nothing about \
it, which is not the same as unlimited, and this page never renders it that way. **Not known** \
is weaker still: nobody has read a source or measured anything for that cell.

**What vike does today** is *built* when a datahub lane fetches the channel, and the lane is \
named, or *designed* when nothing does, and the reason says what it waits on. **Evidence** says \
where a row's limits were read: the vendor's documentation, a third party's report or a \
measurement, each with its date. A row with none says so, and its limits are blank rather than \
guessed.

";

/// Everything below the last venue.
const REFERENCE_TAIL: &str = "\
## What this page does not cover

Which bar intervals a venue serves is the interval table's question, and which instruments exist \
is the catalog's. A per-instrument first-data date needs a probe against the venue, which this \
page never makes. Whether your credentials work, or your network reaches a vendor, is never \
answered here.
";

/// Distinct kind phrases across `rows`, in first-seen order, as a list a sentence can use.
fn kinds_sentence(rows: &[&HistoryChannel]) -> String {
    let mut seen: Vec<&'static str> = Vec::new();
    for row in rows {
        for kind in row.kinds {
            let phrase = kind.phrase();
            if !seen.contains(&phrase) {
                seen.push(phrase);
            }
        }
    }
    match seen.as_slice() {
        [] => String::new(),
        [only] => (*only).to_string(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// What one venue's channels come to, in a paragraph.
fn venue_intro(venue: &str, rows: &[HistoryChannel]) -> String {
    let built: Vec<&HistoryChannel> =
        rows.iter().filter(|r| matches!(r.state, ChannelState::Built(_))).collect();
    let mut text = if built.is_empty() {
        format!("vike does not fetch anything from {venue} through the datahub today.")
    } else {
        format!("vike fetches {} from {venue} today, through the datahub.", kinds_sentence(&built))
    };
    let names: Vec<String> =
        rows.iter().map(|r| format!("\"{}\" ({})", r.name, r.class.word())).collect();
    text.push_str(&format!(" The channels this table declares for {venue}: {}.", names.join("; ")));
    // A venue whose every serving channel is blank says so in words, because a page of "not known"
    // cells is easy to skim past as if it were an answer.
    let serving: Vec<&HistoryChannel> = rows.iter().filter(|r| !r.kinds.is_empty()).collect();
    if !serving.is_empty()
        && serving.iter().all(|r| matches!(r.evidence, HistoryEvidence::Unmeasured))
    {
        text.push_str(" How far back any of them goes has not been read or measured yet.");
    }
    text
}

/// One channel's subsection.
fn channel_section(row: &HistoryChannel) -> String {
    let mut out = format!("### {}\n\n*{} channel*\n\n", row.name, row.class.word());
    if row.is_unclassified() {
        // The reason is a clause (`no datahub lane serves …`), so it follows a colon; a blank row is
        // always `Designed`, which `an_unclassified_row_is_a_blank_that_says_why` holds.
        let reason = match row.state {
            ChannelState::Designed(reason) => reason,
            ChannelState::Built(_) => "built",
        };
        out.push_str(&format!("**Not classified.** vike does not fetch it: {reason}.\n\n"));
        return out;
    }
    if row.is_absent() {
        out.push_str("**None found.**");
        if !row.note.is_empty() {
            out.push_str(&format!(" {}", row.note));
        }
        out.push_str("\n\n");
        push_evidence(&mut out, row);
        return out;
    }
    out.push_str(&format!("- **Serves:** {}\n", row.kinds_text()));
    out.push_str(&format!("- **How far back:** {}\n", row.depth_text(None)));
    out.push_str(&format!("- **Per request:** {}\n", row.per_request_text()));
    out.push_str(&format!("- **Pace:** {}\n", row.pace_text()));
    out.push_str(&format!("- **Access:** {}\n", row.access_text()));
    out.push_str(&format!("- **What vike does today:** {}\n", row.state_text()));
    out.push('\n');
    if !row.note.is_empty() {
        out.push_str(&format!("{}\n\n", row.note));
    }
    push_evidence(&mut out, row);
    out
}

/// The evidence lines of one row, as a small list under an italic lead-in.
fn push_evidence(out: &mut String, row: &HistoryChannel) {
    out.push_str("*Evidence:*\n\n");
    for line in row.evidence_lines() {
        out.push_str(&format!("- {line}\n"));
    }
    out.push('\n');
}

/// **The reference page, rendered from the table.** One section per roster venue in roster order,
/// each a paragraph and then one subsection per channel. Deterministic and clock-free: a committed
/// page cannot carry a date that changes daily, so a rolling window is worded "counted back from
/// the day you ask" here and resolved to a date only by `vike-cli data source show`.
///
/// `crates/vike-ops/tests/history_channels_gate.rs` holds the committed page equal to this.
#[must_use]
pub fn history_reference() -> String {
    let mut out = String::from(REFERENCE_HEAD);
    let links: Vec<String> =
        vike_model::VENUES.iter().map(|venue| format!("[{venue}](#{venue})")).collect();
    out.push_str(&format!("**Venues:** {}\n\n", links.join(", ")));
    for &venue in vike_model::VENUES {
        let rows = history_channels_for(venue);
        out.push_str(&format!("## {venue}\n\n{}\n\n", venue_intro(venue, rows)));
        for row in rows {
            out.push_str(&channel_section(row));
        }
    }
    out.push_str(REFERENCE_TAIL);
    out
}

#[path = "history_render_tests.rs"]
#[cfg(test)]
mod history_render_tests;
