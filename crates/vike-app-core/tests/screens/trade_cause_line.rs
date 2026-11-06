//! **The Trade window says WHY its ladder is empty when the datahub does not answer.**
//!
//! A desktop whose tunnel does not reach its datahub mounts no feed and writes no status line for any
//! venue (`MdSession::sync_mount` mounts what the server ADVERTISES, and nothing has answered), so the
//! window's cause line had nothing to say: the headline named an absent feed over an EMPTY cause.
//! "Connection refused" lived only in `MdSession::link_status` and the log. `MdSession::unreachable_why`
//! now words that fact, `ToolCtx::datahub_unreachable` carries it and the window states it as the cause.
//!
//! The test dials the REAL market-data session at a port nothing listens on, mounts it the way the
//! shell does once per frame (`MdSession::sync_mount`), builds the window as the app does
//! (`crates/vike-desktop/src/tool_window.rs`'s `tool_content`: the title bar's slot, then
//! `trade_tool_content` in the body margin) and reads the painted text at 640, 1200 and 2560 points
//! and 1x and 2x, because every earlier test ran on a canvas where nothing is ever cut. The technique
//! is `crates/vike-app-core/tests/screens/account_window_screens.rs`'s: a `Shape::Text`'s galley says whether
//! it was elided, what it says and where it sits.
//!
//! ⚠ It names two of the roster's venues, on purpose: a file that names all (or all but one) is
//! taken by `crates/vike-ops/tests/venues/new_venue_gate.rs` for a per-venue table.

use vike_exec::{CoreSnapshot, VenueBlock};
use vike_model::AccountTier;

use crate::tool_ctx::Owned;
use crate::trade_frames::{Datahub, Linked, Txt, frames};

/// What the window says over an empty ladder for a venue the datahub does not feed.
const NO_FEED_HEADLINE: &str = "NO DEPTH FEED FOR THIS VENUE";
/// The words of the cause, before the address. A literal, not the constant: a changed word must
/// change this file too.
const CANNOT_REACH: &str = "Cannot reach the datahub at";

/// The window under test: binance's default account.
const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// The node: binance's default account, trading BTCUSDT.
fn node() -> CoreSnapshot {
    let mut snap = CoreSnapshot::empty(VENUE, SYMBOL);
    snap.portfolio.venues = vec![VenueBlock {
        venue: VENUE.to_string(),
        route_key: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        tier: Some(AccountTier::Demo),
        equity: 5_000.0,
        free_bp: 4_980.0,
        admits_symbols: true,
        ..Default::default()
    }];
    snap
}

/// The Trade window over `linked`'s mount, as the shell hands it: the session's status lines, and
/// its own answer to whether the datahub can be reached, taken once per frame. The window's book:
/// none, as for a venue with no feed.
fn window_over(linked: &Linked) -> Owned {
    Owned {
        statuses: linked.mount.feed_statuses.clone(),
        window: (VENUE, SYMBOL),
        stale: true,
        unreachable: linked.mount.session.unreachable_why(),
        ..Owned::new(node())
    }
}

/// ⚠ **A datahub that refuses the connection is NAMED as the cause**, in words, whole, at every width
/// and density: the window still says its headline (no feed for this venue), and beneath it the one
/// sentence that the datahub cannot be reached, with the address it tried. The venue has no status
/// line, which the test asserts of the mount first: it is the mount's output that is the window's input.
#[test]
fn a_datahub_that_refuses_is_named_as_the_cause_of_the_empty_ladder() {
    let linked = Linked::to(Datahub::Refusing);
    let addr = linked.addr.clone().expect("the refusing datahub has an address");
    assert!(linked.mount.feed_statuses.is_empty(), "nothing answered, so nothing mounted");
    let sentence = format!("{CANNOT_REACH} {addr}. Retrying.");
    let owned = window_over(&linked);
    let mut failures = Vec::new();
    if owned.unreachable.as_deref() != Some(sentence.as_str()) {
        failures.push(format!("the session says {:?}, not {sentence:?}", owned.unreachable));
    }
    for f in frames(&owned) {
        let (at, texts) = (f.at(), &f.texts);
        let painted: Vec<&Txt> = texts.iter().filter(|t| t.text == sentence).collect();
        match painted.as_slice() {
            [one] if f.is_whole(one) => {}
            [one] => {
                failures.push(format!("{at}: the cause is cut: {:?} {:?}", one.rect, one.clip))
            }
            other => failures.push(format!(
                "{at}: {} lines say {sentence:?}, one wanted. Lines that name the datahub: {:?}",
                other.len(),
                texts
                    .iter()
                    .map(|t| t.text.as_str())
                    .filter(|t| t.contains("datahub"))
                    .collect::<Vec<_>>()
            )),
        }
        if !texts.iter().any(|t| t.text == NO_FEED_HEADLINE) {
            failures.push(format!("{at}: the window lost its headline {NO_FEED_HEADLINE:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "the refused datahub must be the stated cause:\n{}",
        failures.join("\n")
    );
}

/// **The positive controls**: a datahub that ANSWERS (serving other venues) and a desktop with no
/// datahub address are not "cannot reach" and the window never says so, while it is still the same
/// window (the headline is there). Without this the test above would also pass for a window that
/// always drew the sentence.
#[test]
fn a_datahub_that_answers_or_is_not_configured_is_never_called_unreachable() {
    let mut failures = Vec::new();
    // "answers": a datahub that answers the handshake and serves another venue only.
    for (case, datahub) in [("answers", Datahub::Serves(&["okx"])), ("no address", Datahub::Absent)]
    {
        let linked = Linked::to(datahub);
        let owned = window_over(&linked);
        if owned.unreachable.is_some() {
            failures.push(format!("{case}: the session says {:?}", owned.unreachable));
        }
        for f in frames(&owned) {
            let (at, texts) = (f.at(), &f.texts);
            if texts.iter().any(|t| t.text.contains(CANNOT_REACH)) {
                failures.push(format!("{case}, {at}: the window says it cannot reach the datahub"));
            }
            if !texts.iter().any(|t| t.text == NO_FEED_HEADLINE) {
                failures
                    .push(format!("{case}, {at}: not the Trade window: no {NO_FEED_HEADLINE:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "an answering datahub is not unreachable:\n{}",
        failures.join("\n")
    );
}
