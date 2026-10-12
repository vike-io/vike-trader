//! `venues`: THE CAPABILITY MATRIX — this build's declarations beside what the server answered.

use std::io;

use vike_datahub_client::{DatahubClient, FEATURE_BACKFILL, FEATURE_VENUE_CATALOG};
use vike_model::{caps_for, venues::venue_caps::LiveDataCaps};
use vike_node_proto::auth::{NodeKeys, Scope};

use crate::exit::CmdResult;

use super::json::venues_json;
use super::{Args, ServerView, Skew, VenueRow, col};

/// The lanes a declared [`LiveDataCaps`] serves, named.
///
/// ⚠ **The destructuring is the completeness gate and it is the COMPILER's**, not a test's: a
/// sixth lane added to `vike_model::venues::venue_caps::LiveDataCaps` makes this pattern fail to compile,
/// because a struct pattern without `..` must bind every field. That is the same property
/// `vike_model::VENUES`' own roster test buys by walking the tree — a new thing cannot be silently
/// absent — bought here for free.
pub(super) fn live_lanes(caps: &LiveDataCaps) -> Vec<&'static str> {
    let LiveDataCaps { bars, quotes, trades, book, depth } = *caps;
    [("bars", bars), ("quotes", quotes), ("trades", trades), ("book", book), ("depth", depth)]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name)
        .collect()
}

/// The whole matrix, DERIVED from the canonical roster. Never a venue list typed here: every one
/// of those in this repository has rotted, and `vike_model::VENUES` is the roster every per-venue
/// table already iterates.
pub(super) fn matrix() -> Vec<VenueRow> {
    vike_model::VENUES
        .iter()
        .map(|venue| {
            let caps = caps_for(venue);
            VenueRow {
                venue,
                live: live_lanes(&caps.live_data),
                backfill_bars: caps.backfill_bars,
                backfill_ticks: caps.backfill_ticks,
            }
        })
        .collect()
}

/// Which `io::ErrorKind`s mean the far side SPOKE.
///
/// `PermissionDenied` is the client's own classification of a served refusal — a denied mac, a
/// scope this key does not hold, or a server advertising `auth` against a box with no node keys in
/// its store. `InvalidData` is every way the answer was not one this client can proceed on: a
/// PROTO_VERSION mismatch (`DatahubClient`'s `check_proto_version`, wrapped at that kind), a
/// `Welcome` carrying no nonce on a keyed server, and a reply that is not a datahub's at all.
///
/// ⚠ The last one is why this is deliberately a claim about the CONNECTION rather than about the
/// peer's identity: a wrong service on the port lands in `InvalidData` too, and reporting *that*
/// as "reached and refused" is honest — something answered — where reporting it as an absent
/// datahub is not. Everything else (refused, timed out, unresolvable) is the socket, and is the
/// one case where nothing was said.
pub(super) fn answered_and_refused(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidData)
}

/// `venues`' own handshake — the ONE place in this group that does not go through
/// `crate::cmd::data`'s [`connect`].
///
/// ⚠ **It must not, and that is deliberate rather than an oversight.** [`connect`] folds every
/// `DatahubClient::connect*` failure into one `CliError::connect` sentence, which is right for the
/// three verbs that need a working connection: there, the only actionable fact is that they have
/// no answer. This verb's entire product is the distinction, so it reads the `io::ErrorKind` the
/// client already classifies (see [`answered_and_refused`]) and keeps the two apart.
///
/// The message carried is the client's own, WITHOUT [`connect`]'s `cannot connect to datahub at
/// {addr}` prefix: [`venues_lines`] names the address on that same line, so the prefix printed it
/// twice — and asserted "cannot connect" about connections that had connected.
fn ask_the_server(addr: &str, keys: Option<&NodeKeys>) -> ServerView {
    let opened = match keys {
        Some(k) => DatahubClient::connect_authed(addr, k, Scope::Read),
        None => DatahubClient::connect(addr),
    };
    match opened {
        Ok(client) => ServerView::Answered(client.features().to_vec()),
        Err(e) if answered_and_refused(e.kind()) => ServerView::Refused(e.to_string()),
        Err(e) => ServerView::Unreachable(e.to_string()),
    }
}

/// The server-wide capabilities the matrix's own columns depend on, and what each one's absence
/// costs — one row per feature, each naming the VERB it gates.
///
/// ⚠ These are per-SERVER rather than per-venue, which is why they are a block under the table and
/// not two more columns: a cell repeated identically on every row reads as a per-venue fact, and
/// it is not one. `md_venue=` is the exception that proves it — that advertisement IS per venue,
/// so it is the column.
pub(super) const SERVER_VERBS: &[(&str, &str)] = &[
    (
        FEATURE_BACKFILL,
        "`data hist fetch` — without it the BACKFILL column above names a capability this server \
         will not act on",
    ),
    (
        FEATURE_VENUE_CATALOG,
        "`data catalog ls` and `refresh` — without it this group cannot reach a venue's list at \
         all",
    ),
];

/// Compute the skew, or `None` unless an advertisement SURVIVED the handshake — in which case
/// there is no difference to state, only a missing half.
///
/// ⚠ **This said `None` when the server was never asked, and that covered [`ServerView::Refused`],
/// where something WAS asked and answered.** The distinction is [`ServerView::Answered`]'s alone: a
/// refused connection may well have carried a full `Welcome`, and this side discarded it
/// ([`ServerView::md_venues`] says so at the discard), so what is missing here is the far half of
/// the comparison rather than the question.
pub(super) fn skew(rows: &[VenueRow], view: &ServerView) -> Option<Skew> {
    let ServerView::Answered(_) = view else { return None };
    let served = view.md_venues();
    let declared_here_unserved_there = rows
        .iter()
        .filter(|r| !r.live.is_empty() && !served.iter().any(|s| s == r.venue))
        .map(|r| r.venue)
        .collect();
    let served_there_unknown_here =
        served.iter().filter(|s| !rows.iter().any(|r| r.venue == s.as_str())).cloned().collect();
    Some(Skew { declared_here_unserved_there, served_there_unknown_here })
}

/// `data catalog venues` — the local matrix, plus whatever the datahub was willing to say.
///
/// ⚠ **An unreachable datahub is REPORTED, never fatal.** §8.1: the local columns are the bulk of
/// the answer, and a box with no datahub is exactly the box whose operator needs to know what this
/// build can do. It is also the one verb in this group that opens a socket it does not need.
pub(super) fn execute_venues(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let rows = matrix();
    let view = ask_the_server(&args.addr, keys);
    if args.json {
        println!("{}", venues_json(args, &rows, &view));
    } else {
        for line in venues_lines(&rows, &view, &args.addr) {
            println!("{line}");
        }
    }
    Ok(())
}

/// A row's live-lane cell, or `-` for a venue with no live feed at all.
fn lanes_cell(row: &VenueRow) -> String {
    if row.live.is_empty() { "-".to_string() } else { row.live.join(",") }
}

/// A row's backfill cell — the KINDS, not a boolean, because `bars` and `ticks` are different
/// answers to "what can I get for this venue without a vendor".
fn backfill_cell(row: &VenueRow) -> String {
    let kinds: Vec<&str> = [("bars", row.backfill_bars), ("ticks", row.backfill_ticks)]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name)
        .collect();
    if kinds.is_empty() { "-".to_string() } else { kinds.join(",") }
}

/// The SERVER's cell for one venue: three states, and `?` is not `no`.
fn feed_cell(venue: &str, served: &[String], asked: bool) -> &'static str {
    if !asked {
        "?"
    } else if served.iter().any(|s| s == venue) {
        "served"
    } else {
        "not served"
    }
}

/// The matrix, rendered.
pub(super) fn venues_lines(rows: &[VenueRow], view: &ServerView, addr: &str) -> Vec<String> {
    let served = view.md_venues();
    let asked = matches!(view, ServerView::Answered(_));
    let venue_w = col("VENUE", rows.iter().map(|r| r.venue.len()));
    let lanes_w = col("LIVE LANES", rows.iter().map(|r| lanes_cell(r).len()));
    let bf_w = col("BACKFILL", rows.iter().map(|r| backfill_cell(r).len()));
    let mut lines = vec![
        // ⚠ The provenance is in the HEADER, because that is what stops the last column being read
        // as a correction of the first two. They are three independent statements.
        format!(
            "{:<venue_w$}  {:<lanes_w$}  {:<bf_w$}  LIVE FEED",
            "VENUE", "LIVE LANES", "BACKFILL"
        ),
        format!(
            "{:<venue_w$}  {:<lanes_w$}  {:<bf_w$}  (that datahub)",
            "", "(this build)", "(this build)"
        ),
    ];
    for r in rows {
        lines.push(format!(
            "{:<venue_w$}  {:<lanes_w$}  {:<bf_w$}  {}",
            r.venue,
            lanes_cell(r),
            backfill_cell(r),
            feed_cell(r.venue, &served, asked)
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "this build: {} venues on the roster · {} with a live feed · {} that backfill anything",
        rows.len(),
        rows.iter().filter(|r| !r.live.is_empty()).count(),
        rows.iter().filter(|r| r.backfill_bars || r.backfill_ticks).count()
    ));
    match view {
        ServerView::Unreachable(why) => {
            lines.push(format!("the datahub at {addr}: NOT REACHED — {why}"));
            lines.push(
                "so the LIVE FEED column is `?` on every row: unasked, which is not the same as \
                 unserved. Everything above it is this build's own declaration and is unaffected."
                    .to_string(),
            );
        }
        // ⚠ Its OWN arm, and the ⚠ is the reason the third state exists: every one of these
        // used to print as `NOT REACHED`, which told the operator the box had no datahub when it
        // had one that answered and said no.
        //
        // ⚠ **The second line used to read "that server was reached and never asked", and that is
        // a claim about the FAR side which the commonest case contradicts.** A keyed datahub met
        // with no node keys completes `handshake()` — `Welcome.features`, `md_venue=` entries and
        // all — and `DatahubClient::connect` only then raises `PermissionDenied`, so the
        // advertisement existed and THIS side discarded it with the connection
        // ([`ServerView::md_venues`] says so at the discard). Telling an operator the server was
        // never asked sends them to look at the server for a decision this binary made.
        ServerView::Refused(why) => {
            lines.push(format!("the datahub at {addr}: REACHED, and it REFUSED — {why}"));
            lines.push(
                "so the LIVE FEED column is `?` on every row: nothing here read an advertisement \
                 it could stand behind. ⚠ On the commonest served refusal — a keyed datahub with \
                 no node keys in this box's store — that server DID advertise its live venues in \
                 the `Welcome` before refusing the connection, and THIS side dropped them with it \
                 rather than report half a handshake. So `?` is this binary's choice, not the \
                 server's silence. ⚠ This is NOT an absent datahub either — a PROTOCOL VERSION \
                 SKEW between this binary and that server lands here, as does a key it would not \
                 take, and each is a different thing to go and fix. Everything above it is this \
                 build's own declaration and is unaffected."
                    .to_string(),
            );
        }
        ServerView::Answered(_) => {
            lines.push(format!(
                "the datahub at {addr}: serves live market data for {} of them",
                served.iter().filter(|s| rows.iter().any(|r| r.venue == s.as_str())).count()
            ));
            for (feature, why) in SERVER_VERBS {
                let state = match view.serves(feature) {
                    Some(true) => "serves",
                    Some(false) => "does NOT serve",
                    None => "was not asked for",
                };
                lines.push(format!("  {state} `{feature}` — {why}"));
            }
            // ⚠ This arm is the only one that renders [`SERVER_VERBS`], and deliberately: an
            // unreached server said nothing about them either, and printing "does NOT serve" for a
            // server nobody asked would be the merge this verb exists to avoid.
        }
    }
    if let Some(s) = skew(rows, view) {
        if !s.declared_here_unserved_there.is_empty() {
            // ⚠ **This sentence used to end `a `data realtime watch` on one of them is refused by
            // the server, not by this binary`, and it named no route instead.** The reason it was
            // struck has since EXPIRED and the sentence is still right to leave out, which is worth
            // a reader's time because the two are different arguments:
            //
            // * THEN, the route did not exist — `crate::cmd::data`'s `parse` refused the whole
            //   group on its own usage rung, in this binary, before any socket.
            // * NOW it does (`crate::cmd::data::realtime`'s `watch` ships), and the struck half is
            //   STILL false — for a reason that survives the route: the skew set includes a server
            //   advertising NO market-data plane at all, and against one of those
            //   `vike_datahub_client::DatahubClient::md_subscribe` refuses LOCALLY on the
            //   capability, "nothing was sent". So "refused by the server, not by this binary" is
            //   exactly wrong in the case this fixture plants, and which side says no is the ONE
            //   thing this verb exists to keep legible.
            //
            // What is true needs no route at all, and is what it says. The test below
            // (`the_build_column_and_the_server_column_are_never_merged_into_one_verdict`) holds it.
            lines.push(format!(
                "⚠ SKEW — this build declares a live feed for {}, and that datahub advertises \
                 none. The lanes on the left are this binary's own adapters; whether a live \
                 subscription is SERVED is that server's answer, and for these venues it is no.",
                s.declared_here_unserved_there.join(", ")
            ));
        }
        if !s.served_there_unknown_here.is_empty() {
            lines.push(format!(
                "⚠ SKEW — that datahub serves live market data for {}, which this build's venue \
                 roster does not name: this binary is OLDER than that server.",
                s.served_there_unknown_here.join(", ")
            ));
        }
    }
    lines
}
