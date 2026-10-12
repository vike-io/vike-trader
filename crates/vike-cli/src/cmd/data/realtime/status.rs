//! `data realtime status` — what one datahub's handshake ADVERTISES about its live market data.

use std::io::{self, IsTerminal};

use vike_datahub_client::{FEATURE_MARKET_DATA, advertised_md_venues};
use vike_node_proto::auth::{NodeKeys, Scope};

use super::grammar::default_render;
use super::{Args, Render, col, connect};
use crate::exit::CmdResult;

// ─── `status` ────────────────────────────────────────────────────────────────────────────────────

/// The sentence EVERY `status` answer ends on, and the reason this verb is honest.
///
/// ⚠ It is unconditional rather than reserved for the empty case, which is the stronger claim: a
/// reader who met it only when the list was short would reasonably read its absence as "these ones
/// were checked". None of them were.
const NOT_A_PROBE: &str = "⚠ this is what the server ADVERTISES, not a liveness check. An entry \
                           means this datahub's build links a market-data client for that venue and \
                           would accept a subscription for it — nothing here opened a venue socket, \
                           timed a frame, or observed one. The verb that observes a frame is \
                           `data realtime watch`.";

/// `data realtime status` — the handshake, read back.
pub(super) fn execute_status(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // ⚠ Through the SHARED dial, unlike `catalog venues` — see the module doc: this verb has no
    // local half, so an unreachable datahub leaves nothing to render and is the connect rung.
    let client = connect(&args.addr, keys, Scope::Read)?;
    let features: Vec<String> = client.features().to_vec();
    let view = ServerFeeds::of(&features);
    let render =
        args.render.unwrap_or_else(|| default_render(args.verb, false, io::stdout().is_terminal()));
    let text = match render {
        // `watch`'s stream form cannot reach this verb — `parse` refuses it — and the arm is spelled
        // rather than left to a catch-all so a third rendering has to answer for itself here.
        Render::Json | Render::Jsonl => status_json(&args.addr, &view),
        Render::Table => status_lines(&args.addr, &view).join("\n"),
    };
    println!("{text}");
    Ok(())
}

/// What one handshake said about the market-data plane.
///
/// Two INDEPENDENT facts, kept apart because their absences mean different things: a server with no
/// plane mounted advertises neither the capability nor a venue, while a server with the plane and an
/// empty venue set advertises the first and not the second — and the second is a build that links no
/// venue feed, which is a different thing to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServerFeeds {
    /// Did the `Welcome` carry [`FEATURE_MARKET_DATA`]?
    pub(super) plane: bool,
    /// The venues it advertised, in ADVERTISEMENT ORDER — [`advertised_md_venues`]' own contract,
    /// preserved rather than sorted: the order is the server's statement about itself.
    pub(super) venues: Vec<String>,
}

impl ServerFeeds {
    /// Read one handshake. PURE over the feature list, so every rendering below is unit-tested
    /// against planted advertisements rather than against a server.
    pub(super) fn of(features: &[String]) -> Self {
        ServerFeeds {
            plane: features.iter().any(|f| f == FEATURE_MARKET_DATA),
            venues: advertised_md_venues(features),
        }
    }
}

/// The notes every answer carries, in the order both renderings emit them.
pub(super) fn status_notes(view: &ServerFeeds) -> Vec<String> {
    let mut notes = Vec::new();
    if !view.plane {
        notes.push(format!(
            "this server advertises no `{FEATURE_MARKET_DATA}` capability at all, so \
             `data realtime watch` is refused before anything is sent — with the server's own \
             sentence, which names what has to be set THERE. Whether the plane is mounted is a \
             runtime fact of that process, not of this binary"
        ));
    } else if view.venues.is_empty() {
        notes.push(
            "the market-data plane is mounted and advertises NO venue: the server would accept the \
             verb and refuse every spec, because its build links no venue feed. That is a build and \
             configuration question on the server, not a spelling one here"
                .to_string(),
        );
    } else {
        notes.push(
            "a venue that is NOT listed is refused per spec (`VenueNotServed`) before any venue is \
             called, so `data realtime watch` on one costs a round trip and nothing else"
                .to_string(),
        );
    }
    notes.push(NOT_A_PROBE.to_string());
    notes
}

/// The table.
pub(super) fn status_lines(addr: &str, view: &ServerFeeds) -> Vec<String> {
    let mut lines = vec![
        format!("datahub:     {addr}"),
        format!("market data: {}", if view.plane { "advertised" } else { "NOT advertised" }),
        String::new(),
    ];
    if view.venues.is_empty() {
        lines.push("no venue advertises a live feed on this datahub".to_string());
    } else {
        let venue_w = col("VENUE", view.venues.iter().map(String::len));
        lines.push(format!("{:<venue_w$}  LIVE FEED", "VENUE"));
        for v in &view.venues {
            lines.push(format!("{v:<venue_w$}  advertised"));
        }
    }
    for note in status_notes(view) {
        lines.push(String::new());
        lines.push(note);
    }
    lines
}

/// The document.
///
/// ⚠ `liveness_probed` is a hard `false` rather than an omission, the shape
/// `crate::cmd::data::source`'s `verified_against_the_vendor` already uses: a consumer folding this
/// has no prose to read, so the ONE thing it must not be able to assume is that anything was
/// measured.
pub(super) fn status_json(addr: &str, view: &ServerFeeds) -> String {
    let doc = serde_json::json!({
        "addr": addr,
        "market_data_advertised": view.plane,
        "venues": view.venues,
        "count": view.venues.len(),
        "liveness_probed": false,
        "notes": status_notes(view),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers and bools; serialization is total")
}
