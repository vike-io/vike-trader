//! `ls` and `show` rendered: the table, a venue's channel block, and the `--json` documents.

use vike_catalog::{ChannelState, HistoryChannel};
use vike_model::time::epoch_ms_to_utc_date;

use super::roster::{
    HISTORY_HEADING, VIKE_BASE, VIKE_CLASSES, VIKE_KEYS, VIKE_LICENCE, built_row, ls_notes,
    note_lines, undeclared_note,
};
use super::{
    CLASS_W, DESIGNED_COST, DETAIL_INDENT, HISTORY_NOTE, LABEL_W, NOT_VERIFIED, Row, SOURCES,
    State, VENUE_TOKEN, VIKE, col,
};

/// The table plus its footer. Pure, so the shape is unit-tested without a process.
pub(super) fn ls_lines(rows: &[Row]) -> Vec<String> {
    let name_w = col("SOURCE", rows.iter().map(|r| r.name.len()));
    let state_w = col("STATE", rows.iter().map(|r| r.state.as_str().len()));
    let reach_w = col("REACHES", rows.iter().map(|r| reach_cell(r).len()));

    let mut lines = vec![format!(
        "{:<name_w$}  {:<state_w$}  {:<reach_w$}  {}",
        "SOURCE", "STATE", "REACHES", "COST"
    )];
    for r in rows {
        lines.push(format!(
            "{:<name_w$}  {:<state_w$}  {:<reach_w$}  {}",
            r.name,
            r.state.as_str(),
            reach_cell(r),
            r.cost
        ));
    }
    lines.push(String::new());
    // ⚠ RENDERED from [`ls_notes`], never re-spelled: the footer is that derivation and nothing
    // else, which is what makes the table and the document one set of footnotes rather than two
    // lists that happen to agree today. See [`ls_notes`] for what the re-spelling cost.
    lines.extend(ls_notes().into_iter().flat_map(note_lines));
    lines
}

/// The REACHES cell for a row that reaches nothing. A dash would read as "not applicable"; this
/// reads as the answer it actually is.
pub(super) fn reach_cell(row: &Row) -> &'static str {
    row.reaches.unwrap_or("nothing yet")
}

/// What a `designed` row's `show` must not leave the operator to find out by typing the value.
///
/// ⚠ A designed row used to print `state: designed` and its cost cell and stop — no statement that
/// `--source NAME` is refused today, and no "Built today: …" the way `super::parse_source`'s own
/// refusal carries one. So learning what IS usable cost a round trip through a refusal, which is
/// the round trip this group was built to remove.
///
/// The built half is RENDERED from [`SOURCES`], never typed: that refusal and this note are two
/// sentences nothing holds equal, so this one derives its list rather than restating theirs.
fn designed_note(name: &str) -> String {
    let built: Vec<String> = SOURCES.iter().copied().map(|s| built_row(s).name).collect();
    format!(
        "⚠ `--source {name}` is REFUSED today — by name, with what it is waiting on, which is the \
         COST cell above. Built today: {}. `{VENUE_TOKEN}` is the default and stands for any venue \
         token, so it needs no --source at all.",
        built.join(", ")
    )
}

/// The footnotes a `show` carries — the facts the four cells alone would misreport, in the order
/// both of its renderings emit them.
///
/// ⚠ **This is what `notes` MEANS in this group's every document, and pinning it down is a
/// correction.** `show --json` used to set `notes` to [`show_lines`] — the ENTIRE human rendering,
/// `source:   vike` and `state:    designed` and the blank lines included — while `ls --json` set
/// the same field to three footnotes. One field name, two categorically different documents, from
/// two verbs of one group: a consumer that rendered `notes` as a bullet list printed `state:
/// designed` twice under `show` and three footnotes under `ls`, and a consumer that grepped `notes`
/// for the unverified warning found it under one verb and not the other. Every structured cell the
/// document already carries as a FIELD is now carried once.
pub(super) fn show_notes(row: &Row) -> Vec<String> {
    let mut notes = Vec::new();
    // The row an operator reached by typing something this side does not recognise. Saying so is
    // the difference between "your venue is fine" and "nothing here judged your venue", and only
    // the second is true.
    if row.venue_token && row.name != VENUE_TOKEN {
        notes.push(format!(
            "`{name}` is not a source NAME this side knows, so it is taken as a VENUE token — the \
             same answer `--source {name}` gets. Whether that venue is one a datahub can reach is \
             a property of that process, and nothing here asked it.",
            name = row.name
        ));
    }
    if row.state == State::Designed {
        notes.push(designed_note(&row.name));
        notes.push(DESIGNED_COST.to_string());
    }
    if row.name == VIKE {
        notes.push(VIKE_LICENCE.to_string());
        notes.push(VIKE_KEYS.to_string());
        notes.push(VIKE_BASE.to_string());
    }
    if row.declared {
        notes.push(HISTORY_NOTE.to_string());
    } else if row.venue_token && row.name != VENUE_TOKEN {
        notes.push(undeclared_note(&row.name));
    }
    notes.push(NOT_VERIFIED.to_string());
    notes
}

/// One detail line of a channel: the label, padded, then the text. An empty label continues the
/// line above it, which is how a second evidence source hangs under the first.
pub(super) fn detail(label: &str, text: &str) -> String {
    format!("{DETAIL_INDENT}{label:<w$}{text}", w = LABEL_W)
}

/// One channel as terminal lines: its class and name, then one line per cell.
///
/// A channel that serves something prints every cell; a FINDING that no channel exists prints the
/// finding, its scope and its source and nothing that would describe a channel; and a row nobody
/// classified says so and stops. Which of the three it is comes from the row itself, so this
/// function cannot disagree with the JSON's `presence`.
pub(super) fn channel_lines(ch: &HistoryChannel, today_ms: i64) -> Vec<String> {
    let mut lines = vec![format!("  {:<w$} {}", ch.class.word(), ch.name, w = CLASS_W)];
    if ch.is_unclassified() {
        let reason = match ch.state {
            ChannelState::Designed(reason) => reason,
            ChannelState::Built(_) => "built",
        };
        lines.push(detail("state:", &format!("not classified — {reason}")));
        return lines;
    }
    if ch.is_absent() {
        lines.push(detail("result:", "none found in what was read"));
    } else {
        lines.push(detail("serves:", &ch.kinds_text()));
        lines.push(detail("depth:", &ch.depth_text(Some(today_ms))));
        lines.push(detail("per request:", &ch.per_request_text()));
        lines.push(detail("pace:", &ch.pace_text()));
        lines.push(detail("access:", &ch.access_text()));
        lines.push(detail("state:", &ch.state_text()));
    }
    for (i, source) in ch.evidence_lines().iter().enumerate() {
        lines.push(detail(if i == 0 { "evidence:" } else { "" }, source));
    }
    if !ch.note.is_empty() {
        lines.push(detail("note:", ch.note));
    }
    lines
}

/// A roster venue's whole history block: the heading, then each channel, blank-line separated.
fn history_lines(channels: &[HistoryChannel], today_ms: i64) -> Vec<String> {
    let mut lines = vec![HISTORY_HEADING.to_string()];
    for ch in channels {
        lines.push(String::new());
        lines.extend(channel_lines(ch, today_ms));
    }
    lines
}

/// `show NAME`. Pure, for [`ls_lines`]'s reason — and it takes the RESOLVED row rather than a
/// string, so there is no name left for this function to invent a default for. `today_ms` is the
/// caller's clock: a rolling window resolves against it, and a test passes a fixed instant.
pub(super) fn show_lines(row: &Row, today_ms: i64) -> Vec<String> {
    let mut lines = vec![
        format!("source:   {}", row.name),
        format!("state:    {}", row.state.as_str()),
        format!("reaches:  {}", reach_cell(row)),
        format!("cost:     {}", row.cost),
    ];
    if row.name == VIKE {
        lines.push(String::new());
        lines.push("holds TWO CLASSES, and they are different kinds of thing:".to_string());
        for (class, what) in VIKE_CLASSES {
            lines.push(format!("  {class}: {what}"));
        }
    }
    if row.declared {
        lines.push(String::new());
        lines.extend(history_lines(row.channels, today_ms));
    }
    for note in show_notes(row) {
        lines.push(String::new());
        lines.push(note);
    }
    lines
}

/// `ls --json`: one object per source, carrying the same four cells the table carries — and the
/// same footnotes, under the same field name the other verb uses for the same kind of thing.
///
/// `reaches` is `null` rather than a sentence on a designed row, because a machine reader asking
/// "can I use this" wants a value it can test rather than prose it has to match on.
pub(super) fn ls_json(rows: &[Row]) -> String {
    let sources: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "state": r.state.as_str(),
                "reaches": r.reaches,
                "cost": r.cost,
                "venue_token": r.venue_token,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "count": sources.len(),
        "sources": sources,
        "verified_against_the_vendor": false,
        "notes": ls_notes(),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, bools and nulls; serialization is total")
}

/// `show NAME --json`.
///
/// ⚠ `verified_against_the_vendor` is the field this whole phase turns on, and it is a hard `false`
/// rather than an omission: a consumer folding this document has no prose to read, so the ONE thing
/// it must not be able to assume is that the description was checked. It becomes a real answer the
/// day a vendor read exists; until then it says what happened, which is nothing.
///
/// `channels` carries a roster venue's history rows — each cell an object with a `kind` a consumer
/// branches on and a `text` the terminal prints — and is an EMPTY array for every other source.
/// `channels_declared` is what tells that emptiness apart: `false` says this build declares nothing
/// for the name, which is not the same as the venue having no channel. `channels_as_of` is the UTC
/// date every rolling window in `channels` was resolved against.
pub(super) fn show_json(row: &Row, today_ms: i64) -> String {
    let holds: Vec<serde_json::Value> = if row.name == VIKE {
        VIKE_CLASSES
            .iter()
            .map(|(class, what)| serde_json::json!({ "class": class, "what": what }))
            .collect()
    } else {
        Vec::new()
    };
    let channels: Vec<serde_json::Value> =
        row.channels.iter().map(|ch| ch.as_json(today_ms)).collect();
    let doc = serde_json::json!({
        "source": row.name,
        "state": row.state.as_str(),
        "reaches": row.reaches,
        "cost": row.cost,
        "venue_token": row.venue_token,
        "verified_against_the_vendor": false,
        "holds": holds,
        "channels_declared": row.declared,
        "channels_as_of": epoch_ms_to_utc_date(today_ms),
        "channels": channels,
        "notes": show_notes(row),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, bools and nulls; serialization is total")
}
