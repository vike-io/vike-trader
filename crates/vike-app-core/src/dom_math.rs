//! DOM (depth-of-market) pure math — the venue-correct signed-position resolver
//! ([`signed_position_size`]) plus the classifier that decides what the DOM says when it has NO
//! book to draw ([`depth_absence`] and its two word tables). Extracted from `vike-app`'s `main.rs`
//! so these unit tests run in CI (the GUI crate is compile-checked only, never tested — wgpu build
//! weight). Depends only on `vike_model`.
//!
//! # ⚠ The synthetic book is GONE, and what it was is worth carrying
//!
//! This module used to export `default_price`/`tick_for`/`synth_book`, and
//! [`crate::tool_views::dom_tool_content`] called them whenever the real `(venue, symbol)` book was
//! absent or empty. `synth_book` produced 40 gapless, perfectly symmetric levels per side around a
//! seeded price, with uniform-random sizes from a counter-seeded LCG. Its seed was the DOM's
//! per-frame repaint counter, which `vike_panels::dom::draw` increments at its top — so the entire
//! ladder RE-ROLLED on every frame. It animated like a liquid market whose prices never moved, and
//! the only tells were a dim overlay and a `● STALE` badge. `default_price` fed the same illusion
//! from the other side: a table answering `62_800.0` for `BTCUSDT`, printed in the header in the
//! same amber and the same place a venue's real last price uses.
//!
//! The replacement is not a better placeholder. There is no placeholder: the widget draws nothing
//! ladder-shaped without a book, and this module's job is now to say WHY there is none. The
//! argument is [`crate::capture_seed`]'s, one plane over — *"A screenshot of a fabricated
//! `CoreSnapshot` would be a picture of a code path that does not exist, which for the panel that
//! shows a human their money is the worst thing to publish."* A fabricated BOOK is that same claim
//! about the panel a human CLICKS to send an order, and unlike a screenshot it is in front of them
//! while they do it.
//!
//! Nothing else called any of the three, and no capture path depended on them: `scripts/qa_shots.sh`
//! lists `VIKE_TOOL` among its deliberate omissions, and `.trader/shots/manifest.json`'s
//! `grid-dom.png` pose is BLOCKED with a `path_to_unblock` requiring the ladder be proven LIVE
//! before capture — which a synthetic ladder defeats rather than serves.

/// The venue-correct SIGNED position size for the DOM. One-way/paper positions carry direction in
/// the SIGN of `size` (their `position_side` is always `"BOTH"`, so a short is a NEGATIVE size — see
/// `Account::unrealized_pnl`, "sign rides in the signed size"); hedge-mode perps instead carry
/// direction in `position_side` (`"LONG"`/`"SHORT"`) with a magnitude size. So read the string only
/// as a hedge-mode override; otherwise trust the sign. (Reading the string alone made every short
/// render as a long with inverted P/L, and made Close/Reverse pick the wrong side.)
pub fn signed_position_size(size: f64, position_side: &str) -> f64 {
    if position_side.eq_ignore_ascii_case("short") || position_side.eq_ignore_ascii_case("sell") {
        -size.abs()
    } else if position_side.eq_ignore_ascii_case("long")
        || position_side.eq_ignore_ascii_case("buy")
    {
        size.abs()
    } else {
        size // one-way / "BOTH": already signed
    }
}

/// What the DOM knows about WHY it has no order book for this window's venue + symbol.
///
/// Derived from ONE fact the shell already holds: that venue's market-data status line, written by
/// `crates/vike-app-core/src/md_session.rs`'s `refresh_statuses` — the thread that dials,
/// subscribes and reads. This classifier adds no knowledge of its own; it picks the HEADLINE and
/// the NEXT STEP, while the status line itself goes on screen VERBATIM as the cause. That split is
/// deliberate: if a future status wording lands in a coarser bucket than it deserves, the operator
/// still has the exact words in front of them, so the worst outcome is a vague headline rather than
/// a wrong one.
///
/// The classification runs through `vike_model::feed_status::parse_feed_status` rather than a
/// second substring table written here — that function is the one authority the Connections tool
/// and `vike_tradehub::reconcile_config::health_from_feed_status` both read, and its own doc records
/// being got wrong twice by callers who reasoned about the strings instead of calling it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DepthAbsence {
    /// This venue has no market-data status producer at all — it is absent from the status map.
    /// A venue outside [`crate::split_plane::LOCAL_FEED_VENUES`], or a build that mounts no session.
    NoFeed,
    /// The link is down, idle, or has never resolved a datahub address.
    LinkDown,
    /// The link reported a fault.
    LinkFault,
    /// The link is dialling or subscribing.
    Dialling,
    /// The link is up and serving this venue — the book simply has not arrived for THIS symbol.
    NoSnapshot,
    /// The link is up and this venue is not being served: refused, or nothing subscribed. ⚠ The two
    /// are ONE variant on purpose. `refresh_statuses` renders both without a rung-3 keyword — the
    /// refusal arm deliberately so, since interpolating a server's own words once classified a
    /// refused venue as Connected — so `parse_feed_status` answers `Unknown` for each, and a
    /// classifier claiming to tell them apart would be guessing. The verbatim cause line DOES tell
    /// them apart, which is why it is the half that goes on screen.
    NotServed,
}

/// Classify the DOM's depth absence from that venue's market-data status line.
///
/// `None` ⇒ the venue is absent from the status map entirely (no producer), which is a different
/// statement from an empty string — a producer that has written nothing yet, which lands on
/// [`DepthAbsence::LinkDown`] because that is what `parse_feed_status` says an empty line means and
/// this function does not second-guess it.
pub fn depth_absence(status: Option<&str>) -> DepthAbsence {
    use vike_model::feed_status::ConnectionState as C;
    let Some(s) = status else {
        return DepthAbsence::NoFeed;
    };
    match vike_model::feed_status::parse_feed_status(s) {
        C::Connected => DepthAbsence::NoSnapshot,
        C::Connecting => DepthAbsence::Dialling,
        C::Disconnected => DepthAbsence::LinkDown,
        C::Error => DepthAbsence::LinkFault,
        C::Unknown => DepthAbsence::NotServed,
    }
}

/// The glance answer for one absence. Exhaustive with no `_` arm, so a new variant cannot silently
/// inherit another's words.
pub fn depth_absence_headline(a: DepthAbsence) -> &'static str {
    match a {
        DepthAbsence::NoFeed => "NO DEPTH FEED FOR THIS VENUE",
        DepthAbsence::LinkDown => "NO ORDER BOOK — MARKET-DATA LINK DOWN",
        DepthAbsence::LinkFault => "NO ORDER BOOK — MARKET-DATA LINK FAULT",
        DepthAbsence::Dialling => "NO ORDER BOOK YET — CONNECTING",
        DepthAbsence::NoSnapshot => "NO ORDER BOOK YET — AWAITING THE FIRST SNAPSHOT",
        DepthAbsence::NotServed => "NO ORDER BOOK — THIS VENUE IS NOT BEING SERVED",
    }
}

/// What an operator can actually do about one absence. Exhaustive for the same reason.
///
/// ⚠ Every one of these names the DATAHUB, never the tradehub. The observe connection the shell's
/// status bar reports carries no book at all — `vike_tradehub_client::wire::WireSnapshot` and
/// `vike_core::CoreSnapshot` each have no book, depth, quote or tape field — so "reconnect the
/// backend" would be advice that cannot work, offered at the exact moment somebody is trying to
/// find out why their ladder is blank.
pub fn depth_absence_next_step(a: DepthAbsence) -> &'static str {
    match a {
        DepthAbsence::NoFeed => {
            "This build mounts no market-data feed for it. Switch the DOM to a venue the datahub \
             serves (Connections lists them)."
        }
        DepthAbsence::LinkDown => {
            "Check the DATAHUB address in Connections. It is a different connection from the \
             tradehub the status bar reports."
        }
        DepthAbsence::LinkFault => {
            "Connections carries this venue's full message, and the log carries the server's own \
             words."
        }
        DepthAbsence::Dialling => "Nothing to do — the ladder fills when the first snapshot lands.",
        DepthAbsence::NoSnapshot => {
            "The link is serving this venue. If it persists, the datahub may have no depth stream \
             for this symbol."
        }
        DepthAbsence::NotServed => {
            "The datahub is reachable but streams no depth here — it may have refused the key, or \
             nothing has subscribed yet. Connections carries its exact words."
        }
    }
}

#[cfg(test)]
mod dom_position_tests {
    use super::signed_position_size;

    // A one-way / paper position always reports position_side "BOTH" with direction in the SIGN of
    // size; the DOM must NOT strip that sign (the bug: a short rendered as a long, P/L inverted, and
    // Close/Reverse picked the wrong side).
    #[test]
    fn one_way_both_keeps_the_signed_size() {
        assert_eq!(signed_position_size(-0.01, "BOTH"), -0.01); // short stays short
        assert_eq!(signed_position_size(0.01, "BOTH"), 0.01); // long stays long
        assert_eq!(signed_position_size(0.0, "BOTH"), 0.0); // flat
    }

    #[test]
    fn hedge_mode_string_overrides_the_magnitude() {
        // hedge perps carry a magnitude size + direction in the string
        assert_eq!(signed_position_size(0.01, "SHORT"), -0.01);
        assert_eq!(signed_position_size(0.01, "short"), -0.01);
        assert_eq!(signed_position_size(0.01, "sell"), -0.01);
        assert_eq!(signed_position_size(0.01, "LONG"), 0.01);
        assert_eq!(signed_position_size(0.01, "buy"), 0.01);
    }

    // The Close/Reverse exit side is derived from the signed size: long closes by SELL, short by BUY.
    #[test]
    fn exit_side_is_opposite_of_the_held_direction() {
        let exit = vike_model::closing_side;
        assert_eq!(exit(signed_position_size(-0.01, "BOTH")), 1); // short -> BUY to close
        assert_eq!(exit(signed_position_size(0.01, "BOTH")), -1); // long -> SELL to close
    }
}

#[cfg(test)]
mod depth_absence_tests {
    use super::*;

    /// A venue with NO status producer is not the same statement as a producer that has said
    /// nothing, and the two must not collapse: the first means "this build serves no feed here",
    /// the second "the link is down". Collapsing them would send an operator to check an address
    /// for a venue that was never going to stream.
    #[test]
    fn a_missing_producer_is_distinct_from_a_silent_one() {
        assert_eq!(depth_absence(None), DepthAbsence::NoFeed);
        assert_eq!(depth_absence(Some("")), DepthAbsence::LinkDown);
        assert_eq!(depth_absence(Some("   ")), DepthAbsence::LinkDown);
    }

    /// The arms `md_session`'s `refresh_statuses` can actually write, spelled as IT writes them —
    /// these are the strings this classifier meets in production, so they are the ones pinned. A
    /// paraphrase would gate nothing.
    #[test]
    fn the_real_status_lines_classify_as_the_session_intends() {
        // the serving arm — the ONLY one that may carry a rung-3 keyword
        assert_eq!(
            depth_absence(Some("datahub 127.0.0.1:7878 — 1/1 stream(s) live")),
            DepthAbsence::NoSnapshot,
            "a venue whose streams flow but whose BOOK has not arrived is awaiting a snapshot, \
             not disconnected"
        );
        // the wanted-but-not-serving arm carries `connecting`
        assert_eq!(
            depth_absence(Some("datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)")),
            DepthAbsence::Dialling
        );
        assert_eq!(
            depth_absence(Some(
                "datahub 127.0.0.1:7878 — connecting, 0/1 stream(s), the venue feed has sent no \
                 book"
            )),
            DepthAbsence::Dialling
        );
        // the refusal arm and the nothing-wanted arm are both keyword-free => Unknown => NotServed
        assert_eq!(
            depth_absence(Some(
                "datahub 127.0.0.1:7878 — refused BTCUSDT/Depth (that venue declares no such lane)"
            )),
            DepthAbsence::NotServed
        );
        assert_eq!(
            depth_absence(Some("datahub 127.0.0.1:7878 — no streams wanted on this venue")),
            DepthAbsence::NotServed
        );
        // the link half, once the reader has bracketed a dropped connection
        assert_eq!(
            depth_absence(Some("reconnecting (the link dropped) — connecting, 0/1 stream(s)")),
            DepthAbsence::Dialling
        );
        assert_eq!(depth_absence(Some("idle")), DepthAbsence::LinkDown);
    }

    /// A fault must never read as a reason to wait. `refresh_statuses`'s error arm carries the
    /// server's own message and spells `error:` deliberately — and that has to survive the trip
    /// through this classifier.
    #[test]
    fn a_faulted_link_says_fault_not_connecting() {
        assert_eq!(
            depth_absence(Some("datahub error: connection refused (reconnecting)")),
            DepthAbsence::LinkFault,
            "a mixed error+reconnect line is a FAULT — the precedence parse_feed_status pins"
        );
    }

    /// Every variant answers with its own words, no two share a line, and no next step sends the
    /// operator to the WRONG SOCKET. A `_` arm in either table, or a copy-pasted row, is what this
    /// catches.
    #[test]
    fn every_absence_has_its_own_words_and_names_the_right_socket() {
        let all = [
            DepthAbsence::NoFeed,
            DepthAbsence::LinkDown,
            DepthAbsence::LinkFault,
            DepthAbsence::Dialling,
            DepthAbsence::NoSnapshot,
            DepthAbsence::NotServed,
        ];
        let heads: Vec<&str> = all.iter().copied().map(depth_absence_headline).collect();
        let steps: Vec<&str> = all.iter().copied().map(depth_absence_next_step).collect();
        for (i, h) in heads.iter().enumerate() {
            assert!(!h.is_empty(), "{:?} has an empty headline", all[i]);
            assert!(!steps[i].is_empty(), "{:?} has an empty next step", all[i]);
            let lower = steps[i].to_lowercase();
            assert!(
                !lower.contains("tradehub observe") && !lower.contains("reconnect the backend"),
                "{:?} points the operator at the observe socket, which carries no book: {}",
                all[i],
                steps[i]
            );
        }
        let mut h = heads.clone();
        h.sort_unstable();
        h.dedup();
        assert_eq!(h.len(), heads.len(), "two absences share a headline: {heads:?}");
        let mut s = steps.clone();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), steps.len(), "two absences share a next step: {steps:?}");
    }
}
