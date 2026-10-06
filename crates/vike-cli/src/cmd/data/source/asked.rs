//! `show VENUE --addr A`: the one reach this group has, and how its answer is rendered.

use vike_datahub_client::history::{
    COMPILED_TABLE_CAPTION, ChannelReport, CredentialPresence, HeldKind, HistoryChannelsReport,
    compiled_report,
};
use vike_model::time::epoch_ms_to_utc_date;
use vike_node_proto::auth::{NodeKeys, Scope};

use crate::exit::CmdResult;

use super::render::{detail, reach_cell, show_notes};
use super::{CLASS_W, HISTORY_NOTE, NOT_VERIFIED, Row};

// ── `show VENUE --addr A`: the one reach this group has ──────────────────────────────────────────

/// What `show VENUE --addr A` got from the datahub at `addr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Asked {
    /// The address that was asked, as the operator typed it.
    pub(super) addr: String,
    /// The rows — the SERVER's, with its overlay, when `served`; this binary's own compiled table
    /// with the overlay marked not known, when the server is older than the read.
    pub(super) report: HistoryChannelsReport,
    /// Whether the server answered the history-channels read. `false` is a server OLDER than it:
    /// refused client-side with nothing sent, and answered from this binary's table — the design's
    /// §2.3 fallback, under [`COMPILED_TABLE_CAPTION`].
    pub(super) served: bool,
}

/// Ask the datahub at `addr` for the history channels, on the Observe scope the read requires.
///
/// An unreachable datahub is an error on the connect rung, through `crate::cmd::data`'s `connect`
/// like every other verb that dials one. A datahub that does not advertise the read is NOT an
/// error: its absence means the server is older than the verb, and the honest answer is this
/// binary's own table, labelled — the rows are vendor facts compiled into both ends, and only the
/// overlay is the server's, which this never pretends to have.
pub fn ask_the_datahub(addr: &str, keys: Option<&NodeKeys>, today_ms: i64) -> CmdResult<Asked> {
    let mut client = super::connect(addr, keys, Scope::Read)?;
    if !client.serves_history_channels() {
        return Ok(Asked {
            addr: addr.to_string(),
            report: compiled_report(today_ms),
            served: false,
        });
    }
    let report = client.history_channels()?;
    Ok(Asked { addr: addr.to_string(), report, served: true })
}

/// The heading over the asked channels — which table the rows are, and who said so.
fn asked_heading(asked: &Asked) -> String {
    if asked.served {
        format!(
            "history — as the datahub at {} declares it, with what that server says about its own \
             lanes (its clock: {}):",
            asked.addr,
            epoch_ms_to_utc_date(asked.report.as_of_ms)
        )
    } else {
        format!("history — {COMPILED_TABLE_CAPTION} (the datahub at {} was asked):", asked.addr)
    }
}

/// The footnote the asked channels carry in place of [`HISTORY_NOTE`], which says the rows are
/// compiled into THIS binary — true of the fallback, and not of a served answer.
fn asked_history_note(asked: &Asked) -> String {
    if asked.served {
        format!(
            "⚠ the channels above are the table compiled into the datahub at {}, not a probe: each \
             row's evidence carries the date a maintainer read a vendor page or measured, and \
             nothing was checked against a vendor. `mounted`, `credential` and `held` are that \
             server's own answers; the credential line is a word, never the key. `not known` means \
             nobody has read a source or measured anything, `not stated` means one was read and is \
             silent — and neither is a promise about how far back the data goes.",
            asked.addr
        )
    } else {
        HISTORY_NOTE.to_string()
    }
}

/// The line an `--addr` answer ends on, in place of [`NOT_VERIFIED`], which says no server was
/// asked — false once one was.
pub(super) fn asked_closer(asked: &Asked) -> String {
    format!(
        "⚠ the history block above came from the datahub at {} and nothing was read from a vendor. \
         No line here is a probe of what your box, your network or your key actually reaches.",
        asked.addr
    )
}

/// [`show_notes`] with its two server-denying footnotes replaced by their `--addr` twins.
fn asked_notes(row: &Row, asked: &Asked) -> Vec<String> {
    show_notes(row)
        .into_iter()
        .map(|n| {
            if n == HISTORY_NOTE {
                asked_history_note(asked)
            } else if n == NOT_VERIFIED {
                asked_closer(asked)
            } else {
                n
            }
        })
        .collect()
}

/// One served channel as terminal lines — [`channel_lines`]' layout over the wire's cells, plus the
/// server's overlay: `mounted:` on a built row, `credential:` wherever a lane reads one. Every word
/// is the cell's own `text`, so a depth form this binary has never heard of still prints.
fn report_channel_lines(ch: &ChannelReport) -> Vec<String> {
    let mut lines = vec![format!("  {:<w$} {}", ch.class, ch.name, w = CLASS_W)];
    match ch.presence.as_str() {
        "not_classified" => {
            lines.push(detail("state:", &format!("not classified — {}", ch.state.text)));
        }
        "none_found" => lines.push(detail("result:", "none found in what was read")),
        _ => {
            lines.push(detail("serves:", &ch.kinds_text));
            lines.push(detail("depth:", &ch.depth.text));
            lines.push(detail("per request:", &ch.per_request.text));
            lines.push(detail("pace:", &ch.pace.text));
            lines.push(detail("access:", &ch.access.text));
            lines.push(detail("state:", &ch.state.text));
        }
    }
    match ch.mounted {
        Some(true) => {
            lines.push(detail("mounted:", "yes — that datahub's collector table carries this lane"))
        }
        Some(false) => lines.push(detail(
            "mounted:",
            "no — that datahub mounts no lane for it, so a fetch through it is refused there",
        )),
        None => {}
    }
    if ch.credential != CredentialPresence::NotNeeded {
        lines.push(detail("credential:", ch.credential.phrase()));
    }
    for (i, source) in ch.evidence.iter().enumerate() {
        lines.push(detail(if i == 0 { "evidence:" } else { "" }, &source.text));
    }
    if !ch.note.is_empty() {
        lines.push(detail("note:", &ch.note));
    }
    lines
}

/// What the asked datahub's store holds for the venue, one line per kind — or why there is none.
fn held_lines(asked: &Asked, held: &[HeldKind]) -> Vec<String> {
    if !asked.served {
        return vec![format!(
            "held: not known — the datahub at {} is older than the history-channels read",
            asked.addr
        )];
    }
    if held.is_empty() {
        return vec![format!("held: nothing — the store at {} holds no series for it", asked.addr)];
    }
    let mut lines = vec![format!("held — what the store at {} holds for it:", asked.addr)];
    for h in held {
        lines.push(format!(
            "  {:<w$} {} series, {} rows, {} .. {}",
            h.kind,
            h.series,
            h.rows,
            epoch_ms_to_utc_date(h.first_ts),
            epoch_ms_to_utc_date(h.last_ts),
            w = CLASS_W
        ));
    }
    lines
}

/// `show VENUE --addr A`, as a table: the same four cells as [`show_lines`], then the venue's
/// channels as the asked datahub declares them with its overlay, what its store holds, and the
/// notes. Pure — the answer arrives as a value.
pub(super) fn asked_lines(row: &Row, asked: &Asked) -> Vec<String> {
    let mut lines = vec![
        format!("source:   {}", row.name),
        format!("state:    {}", row.state.as_str()),
        format!("reaches:  {}", reach_cell(row)),
        format!("cost:     {}", row.cost),
        String::new(),
        asked_heading(asked),
    ];
    match asked.report.venue(&row.name) {
        Some(venue) => {
            for ch in &venue.channels {
                lines.push(String::new());
                lines.extend(report_channel_lines(ch));
            }
            lines.push(String::new());
            lines.extend(held_lines(asked, &venue.held));
        }
        None => {
            lines.push(String::new());
            lines.push(format!(
                "the datahub at {} declares no history channels for `{}` — its build's roster does \
                 not carry the venue, which says nothing about what the venue offers.",
                asked.addr, row.name
            ));
        }
    }
    for note in asked_notes(row, asked) {
        lines.push(String::new());
        lines.push(note);
    }
    lines
}

/// `show VENUE --addr A --json`: [`show_json`]'s document with `channels` and `held` from the asked
/// datahub, and a `datahub` object saying who answered and whether the server or this binary's
/// own table supplied the rows. `verified_against_the_vendor` stays a hard `false`: a datahub is
/// not a vendor.
pub(super) fn asked_json(row: &Row, asked: &Asked) -> String {
    let venue = asked.report.venue(&row.name);
    let doc = serde_json::json!({
        "source": row.name,
        "state": row.state.as_str(),
        "reaches": row.reaches,
        "cost": row.cost,
        "venue_token": row.venue_token,
        "verified_against_the_vendor": false,
        "holds": Vec::<serde_json::Value>::new(),
        "channels_declared": venue.is_some(),
        "channels_as_of": epoch_ms_to_utc_date(asked.report.as_of_ms),
        "datahub": {
            "addr": asked.addr,
            "served": asked.served,
            "caption": (!asked.served).then_some(COMPILED_TABLE_CAPTION),
        },
        "channels": venue.map(|v| v.channels.clone()).unwrap_or_default(),
        "held": venue.filter(|_| asked.served).map(|v| v.held.clone()),
        "notes": asked_notes(row, asked),
    });
    serde_json::to_string_pretty(&doc).expect("a tree of strings, numbers and bools")
}
