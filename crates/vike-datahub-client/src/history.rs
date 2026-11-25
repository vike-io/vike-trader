//! **The answer SHAPE for [`Request::HistoryChannels`](crate::proto::Request::HistoryChannels) —
//! through which doors each venue's history comes, as THIS datahub's build declares them, plus what
//! that server can say about its own lanes — and the default-lookback clamp a client computes from
//! it.** `docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` is the design; its §2
//! is this module's half of it. Declared ungated: a default `vike-datahub` build must DECODE the
//! verb in order to answer it.
//!
//! # What the reply is, and what it is not
//!
//! The ROWS are `vike_catalog::history_channels_for`'s, compiled into the server: every date in them
//! is the day a maintainer read a vendor page or measured, never something the server probed. The
//! OVERLAY is the server's own, and the only part a client cannot know by itself:
//!
//! * [`HistoryChannelWire::mounted`] — whether a `Built` row's lane is in this server's collector table;
//! * [`HistoryChannelWire::credential`] — whether the credential that lane reads is STORED on the
//!   gatewayhub's box, as a presence word and never a value ([`CredentialPresence`]);
//! * [`HistoryChannelVenue::held`] — what the server's store holds for the venue, per `kind=`.
//!
//! **Nothing here calls a venue**: how far back one INSTRUMENT goes is a later per-instrument probe.
//!
//! # ⚠ Every cell is an OPEN `kind` string plus its `text`, not a closed enum
//!
//! The shape `crates/vike-catalog/src/history/render.rs`'s `as_json` already gives
//! `vike-cli data source show --json`. A closed enum would let a newer server's new depth form make
//! the WHOLE reply undecodable for an older client; an open kind renders the server's `text` and
//! branches only on the kinds it knows. [`HistoryChannelCell::date`] and [`HistoryChannelCell::days`] are the machine fields
//! [`bars_lookback_floor_ms`] reads. The cells are DERIVED from `as_json` ([`channel_report`]), so
//! no kind word is spelled a second time here.
//!
//! # Why the builder is here and not in the server
//!
//! A client talking to a server OLDER than the verb renders its own compiled table in the same shape
//! ([`compiled_report`], the design's §2.3 fallback, overlay marked not known). One builder below
//! both ends, so the fallback can never word a row differently from a server.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use vike_catalog::{HistoryChannel, HistoryChannelState, history_channels_for};
use vike_data::{SeriesCoverage, SeriesId};
use vike_model::StoreKindId;
use vike_model::{CredentialPresence, MS_PER_DAY};

/// The whole reply: one entry per roster venue, in `vike_model::VENUES` order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChannelsReport {
    /// The SERVER's clock, epoch-ms, that every rolling window in the reply was resolved against —
    /// and the instant [`bars_lookback_floor_ms`] counts back from.
    pub as_of_ms: i64,
    /// One per roster venue, roster order.
    pub venues: Vec<HistoryChannelVenue>,
}

impl HistoryChannelsReport {
    /// The entry for `venue`, or `None` for a name that is not on the server's roster.
    #[must_use]
    pub fn venue(&self, venue: &str) -> Option<&HistoryChannelVenue> {
        self.venues.iter().find(|v| v.venue == venue)
    }
}

/// One venue's channels and what the server's store holds for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChannelVenue {
    /// The roster slug.
    pub venue: String,
    /// The venue's rows as the SERVER's build declares them, in the table's order.
    pub channels: Vec<HistoryChannelWire>,
    /// What the server's store holds for the venue, one entry per `kind=`, sorted by kind. Empty
    /// when the store holds nothing for it — or, on a client's own compiled table, because no
    /// server was asked ([`compiled_report`]).
    #[serde(default)]
    pub held: Vec<HistoryChannelHeldKind>,
}

/// One `(venue, channel)` row, cells rendered, plus the server's overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChannelWire {
    /// `request`, `bulk` or `vendor`.
    pub class: String,
    /// The channel's own name, unique within its venue.
    pub name: String,
    /// `serves`, `none_found` or `not_classified` — the three things a row can be.
    pub presence: String,
    /// What it serves, as the store's kind words (`bars`, `quotes`, …). Empty for a finding that no
    /// such channel exists and for an unclassified row; [`Self::presence`] says which.
    pub kinds: Vec<String>,
    /// What it serves, in the words a reader is shown.
    pub kinds_text: String,
    /// How far back it goes. A rolling window carries [`HistoryChannelCell::days`] and the date it resolves to on
    /// [`HistoryChannelsReport::as_of_ms`] in [`HistoryChannelCell::date`]; a fixed start carries its date.
    pub depth: HistoryChannelCell,
    /// How much one request carries.
    pub per_request: HistoryChannelCell,
    /// How fast it may be pulled.
    pub pace: HistoryChannelCell,
    /// What it takes to use it — in words, never a credential name or value.
    pub access: HistoryChannelCell,
    /// `built` or `designed`, with the sentence.
    pub state: HistoryChannelCell,
    /// The datahub lane a `built` row names (`Klines`, `TickBars`, `Funding`,
    /// `CredentialedKlines`); `None` on a designed row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// **Is that lane in this server's collector table?** `Some` only on a `built` row; `None` on a
    /// designed one, and on every row of a client's own compiled table, where no server was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounted: Option<bool>,
    /// Whether the credential the lane reads is stored on the gatewayhub's box — see
    /// [`CredentialPresence`].
    pub credential: CredentialPresence,
    /// Where the row's limits were read, one cell per source: `documented`, `measured` or
    /// `reported`, each with its date in [`HistoryChannelCell::date`] — or one `unmeasured` cell when nobody
    /// looked. The machine half [`bars_lookback_floor_ms`] branches on.
    pub evidence: Vec<HistoryChannelCell>,
    /// What the cells cannot say. Empty when there is nothing to add.
    pub note: String,
}

/// One rendered cell: an OPEN `kind` a client branches on and the `text` it prints — see this
/// module's doc for why the kind is a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChannelCell {
    /// The discriminator (`since`, `lookback`, `per_instrument`, `unstated`, `rows`, …). A kind a
    /// client does not know is still rendered by its `text`.
    pub kind: String,
    /// The sentence a terminal prints.
    pub text: String,
    /// A `YYYY-MM-DD` the cell carries: a fixed start, the date a rolling window resolves to, or
    /// the day an evidence source was read or measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// A day count the cell carries: a rolling window's length, or a per-request span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<u32>,
}

/// What the server's store holds for one venue under one `kind=`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChannelHeldKind {
    /// The store's kind (`bar`, `quote`, `trade`, … on the wire).
    pub kind: StoreKindId,
    /// How many series of that kind the store holds for the venue.
    pub series: u64,
    /// Their rows, summed.
    pub rows: u64,
    /// The earliest row across them, epoch-ms.
    pub first_timestamp: i64,
    /// The latest row across them, epoch-ms.
    pub last_timestamp: i64,
}

/// A cell out of one of `as_json`'s cell objects: its `kind`, its `text`, and whichever date and
/// day count it carries (`date` for a fixed start, `since` for a resolved window, `checked`/`on`
/// for an evidence source; `days` for a window or a span).
fn cell_of(v: &Value) -> HistoryChannelCell {
    let text_of = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
    HistoryChannelCell {
        kind: text_of("kind").unwrap_or_default(),
        text: text_of("text").unwrap_or_default(),
        date: ["date", "since", "checked", "on"].into_iter().find_map(text_of),
        days: v.get("days").and_then(Value::as_u64).and_then(|d| u32::try_from(d).ok()),
    }
}

/// **One catalog row as the wire carries it**, every rolling window resolved against `as_of_ms`,
/// with the server's `mounted` and `credential` answers. Built from the row's own `as_json` and its
/// own sentence methods, so the wire, the terminal and the `--json` document say one thing.
#[must_use]
pub fn channel_report(
    row: &HistoryChannel,
    as_of_ms: i64,
    mounted: Option<bool>,
    credential: CredentialPresence,
) -> HistoryChannelWire {
    let json = row.as_json(as_of_ms);
    let lines = row.evidence_lines();
    let evidence = match json["evidence"]["sources"].as_array() {
        Some(sources) if !sources.is_empty() => sources
            .iter()
            .zip(&lines)
            .map(|(source, line)| HistoryChannelCell { text: line.clone(), ..cell_of(source) })
            .collect(),
        // An unmeasured row has no source to carry: one cell saying so, in the row's own words.
        _ => vec![HistoryChannelCell {
            kind: json["evidence"]["kind"].as_str().unwrap_or_default().to_string(),
            text: lines.join("; "),
            date: None,
            days: None,
        }],
    };
    HistoryChannelWire {
        class: row.class.word().to_string(),
        name: row.name.to_string(),
        presence: row.presence_word().to_string(),
        kinds: row.kinds.iter().map(|k| k.word().to_string()).collect(),
        kinds_text: row.kinds_text(),
        depth: cell_of(&json["depth"]),
        per_request: cell_of(&json["per_request"]),
        pace: cell_of(&json["pace"]),
        access: cell_of(&json["access"]),
        state: cell_of(&json["state"]),
        lane: match row.state {
            HistoryChannelState::Built(lane) => Some(lane.name().to_string()),
            HistoryChannelState::Designed(_) => None,
        },
        mounted,
        credential,
        evidence,
        note: row.note.to_string(),
    }
}

/// **THIS CLIENT's own compiled table in the reply's shape**, for a datahub older than the verb —
/// the design's §2.3 fallback. Every overlay field says it was not asked: `mounted` is `None` and
/// `credential` is [`CredentialPresence::NotChecked`] on every row that needs one, and `held` is
/// empty. The rows are vendor facts compiled into both ends, so they are honest; the overlay is the
/// server's alone, and this never pretends to have it — which is also why
/// [`bars_lookback_floor_ms`] never clamps from it.
#[must_use]
pub fn compiled_report(as_of_ms: i64) -> HistoryChannelsReport {
    HistoryChannelsReport {
        as_of_ms,
        venues: vike_model::VENUES
            .iter()
            .map(|&venue| HistoryChannelVenue {
                venue: venue.to_string(),
                channels: history_channels_for(venue)
                    .iter()
                    .map(|row| {
                        let credential = match row.state {
                            HistoryChannelState::Built(_)
                                if matches!(
                                    row.access,
                                    vike_catalog::HistoryChannelAccess::Credential(_)
                                ) =>
                            {
                                CredentialPresence::NotChecked
                            }
                            _ => CredentialPresence::NotNeeded,
                        };
                        channel_report(row, as_of_ms, None, credential)
                    })
                    .collect(),
                held: Vec::new(),
            })
            .collect(),
    }
}

/// The caption a client shows over [`compiled_report`]'s rows — one spelling for the CLI and the GUI.
pub const COMPILED_TABLE_CAPTION: &str = "this client's table — the datahub is older than the history-channels verb, so whether a lane \
     is mounted and whether its credential is stored are not known";

/// **What the server's store holds for `venue`, per `kind=`** — summed from one `Inventory` read.
/// Pure, so the server's answer and a test's planted inventory go through one fold.
#[must_use]
pub fn held_for(
    venue: &str,
    inventory: &[(SeriesId, SeriesCoverage)],
) -> Vec<HistoryChannelHeldKind> {
    let mut held: Vec<HistoryChannelHeldKind> = Vec::new();
    for (id, cov) in inventory.iter().filter(|(id, _)| id.venue == venue) {
        match held.iter_mut().find(|h| h.kind == id.kind) {
            Some(h) => {
                h.series += 1;
                h.rows += cov.rows;
                h.first_timestamp = h.first_timestamp.min(cov.first_timestamp);
                h.last_timestamp = h.last_timestamp.max(cov.last_timestamp);
            }
            None => held.push(HistoryChannelHeldKind {
                kind: id.kind,
                series: 1,
                rows: cov.rows,
                first_timestamp: cov.first_timestamp,
                last_timestamp: cov.last_timestamp,
            }),
        }
    }
    held.sort_by_key(|h| h.kind);
    held
}

/// **The earliest start a DEFAULT lookback for `venue`'s bars should ask for, or `None` to leave
/// the default alone** — the design's §3.3 clamp, a pure function of a served reply.
///
/// A row yields a floor ONLY when every one of these holds:
///
/// 1. it is `built` and its lane is MOUNTED on the server that answered (`mounted == Some(true)` —
///    so a client's own [`compiled_report`], where nothing is mounted-known, never clamps), and it
///    serves `bars`;
/// 2. its evidence is DOCUMENTED or MEASURED — every source, and at least one. A row carrying a
///    `reported` source (a third party's claim) or none at all (`unmeasured`) never clamps: a window
///    nobody stated must not shorten what a user asks for;
/// 3. its depth is a rolling window in days (`lookback` with [`HistoryChannelCell::days`]). A fixed start never
///    needs a clamp against a window counted back from today, and its scope is text that cannot be
///    matched to an instrument; a per-step window's bar-size bound is a sentence today; per
///    instrument and unstated never do.
///
/// The floor is `as_of_ms` minus that many days. With more than one qualifying row the DEEPEST
/// window wins, so the clamp never shortens a request below what some mounted bars channel serves.
///
/// ⚠ **On today's table no row qualifies** (`on_todays_table_the_clamp_changes_nothing`): OANDA's
/// depth is a fixed start, IBKR's window is per step with no lane, dukascopy's built feed is per
/// instrument, every other built row is unmeasured. It fires once a row carries a rolling window.
#[must_use]
pub fn bars_lookback_floor_ms(venue: &HistoryChannelVenue, as_of_ms: i64) -> Option<i64> {
    venue
        .channels
        .iter()
        .filter(|ch| ch.state.kind == "built" && ch.mounted == Some(true))
        .filter(|ch| ch.kinds.iter().any(|k| k == "bars"))
        .filter(|ch| {
            !ch.evidence.is_empty()
                && ch.evidence.iter().all(|e| e.kind == "documented" || e.kind == "measured")
        })
        .filter(|ch| ch.depth.kind == "lookback")
        .filter_map(|ch| ch.depth.days)
        .map(|days| as_of_ms - i64::from(days) * MS_PER_DAY)
        .min()
}

/// **A default window's start, raised to `floor` when there is one** — the clamp applied. A start
/// already later than the floor is left where it is: the clamp only ever shortens a default that
/// reaches past what the venue serves, and never moves a start EARLIER.
#[must_use]
pub fn clamp_default_start(start_ms: i64, floor: Option<i64>) -> i64 {
    floor.map_or(start_ms, |f| start_ms.max(f))
}

#[path = "history_tests.rs"]
#[cfg(test)]
mod history_tests;
