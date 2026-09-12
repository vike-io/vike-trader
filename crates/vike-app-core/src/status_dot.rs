//! The bottom status strip's two DOTS, as pure functions: a `&str` feed status and a `bool` control
//! link in, an `egui::Color32` out.
//!
//! It lives here rather than inside `crates/vike-desktop/src/main.rs`'s `status_bar` for the reason
//! [`crate::tradehub_control::control_status_line`] gives one module over: `vike-app` is the one
//! workspace member outside the derived CI roster (`xtask::ci::tables`'s `EXCLUDE_FROM_CI`), so a
//! decision left in the shell is compiled by the `app-check` job and RUN by nothing. The shell keeps
//! the widget calls only it can make; what colour the dot should be is decided — and tested — here.
//!
//! # ⚠ The feed dot is [`vike_model::feed_status::parse_feed_status`], not a second classifier
//!
//! `status_bar` used to hand-roll its own, and it disagreed with the shared one — the same parser
//! the Connections tool (`vike_connections::view`) and the headless daemon's health gate
//! (`vike_ops::reconcile_config::health_from_feed_status`) both read — on three points:
//!
//!   1. **Precedence.** It tested `reconnect`/`connecting` BEFORE `fault`/`error`; the shared parser
//!      tests `disconnected`, then the error family, then the connected family, then `connect`. So
//!      every venue's commonest failure line — `"{key} ws error (reconnecting): {e}"`, written by
//!      binance/bybit/okx/deribit/hyperliquid/polymarket alike — painted the strip AMBER while the
//!      Connections tool painted the same feed's row RED. One feed, two colours, one app.
//!   2. **What counts as connected.** It accepted only `live`; the shared parser also accepts
//!      `connected`, `streaming` and `subscribed`. No producer emits the latter three today, so this
//!      one was a latent divergence rather than a live one — but the observe bridge's
//!      `"observe connect to {addr} failed ({e}); retrying…"` fell through every arm of the
//!      hand-rolled chain to the DEFAULT grey, where the shared parser reads `failed` as an error.
//!   3. **A bool recovered from prose.** The control dot ran `control.contains("disconnected")` over
//!      a string [`crate::tradehub_control::control_status_line`] had just BUILT from a `bool` one
//!      crate down. That round trip is now gone — [`control_dot_color`] takes the bool — and with it
//!      a reachable false red: a CONNECTED channel whose latched `last_error` tail happened to
//!      contain the word `disconnected` painted the segment as though the link were down.
//!
//! The colours were the strip's own and unchanged, so #1569 moved only the CLASSIFICATION; it left
//! the Connections tool's near-identical-but-different amber, red and grey alone on the ground that
//! unifying two palettes is a different change from unifying two classifiers.
//!
//! ⚠ **That second change has since landed, and it went the strip's way.** The four colours are now
//! `vike_ui_theme::palette::status`, whose doc carries the table of what differed and the rule that
//! chose the survivors; the strip repainted NOTHING (its set was adopted, because two of its four
//! were already canonical palette constants) and the Connections tool folded three private
//! near-copies into it. `AMBER` and `MUTED` below are local NAMES for two of those shared constants,
//! not values.

use vike_model::feed_status::{ConnectionState, parse_feed_status};

/// A link in transition, and also the loud colour the armed remote-control segment is painted in.
///
/// A LOCAL NAME for the shared constant, not a second value: the strip's `(230,180,40)` became
/// `vike_ui_theme::palette::status::CONNECTING` when the two status palettes were unified, and the
/// alias survives only because half this module's uses of it are the ARMED segment rather than a
/// `Connecting` state — see [`control_dot_color`].
const AMBER: egui::Color32 = vike_ui_theme::palette::status::CONNECTING;

/// The muted grey — `Disconnected` and `Unknown` share it, exactly as the Connections tool's Status
/// column does: both mean "nothing live to show", and only the label text tells them apart (this
/// strip has no label, so here they are genuinely one state). Also a local name for the shared
/// constant, `vike_ui_theme::palette::status::MUTED`.
const MUTED: egui::Color32 = vike_ui_theme::palette::status::MUTED;

/// Dot colour for a classified [`ConnectionState`]. Split from [`feed_dot_color`] so the mapping can
/// be asserted per-variant with an exhaustive `match` — a new state added to the enum is a compile
/// error here rather than a silent fall-through to grey.
pub fn dot_color_for(state: ConnectionState) -> egui::Color32 {
    match state {
        ConnectionState::Connected => vike_ui_theme::palette::ACCENT,
        ConnectionState::Connecting => AMBER,
        ConnectionState::Error => vike_ui_theme::palette::DOWN,
        ConnectionState::Disconnected | ConnectionState::Unknown => MUTED,
    }
}

/// The LEFT dot: the headline feed/backend status line (`App::status` — the binance feed's own line
/// on the local-core path, the observe bridge's link line under `--observe`, and `"CORE FAULT: …"`
/// ahead of either), classified by the ONE shared parser.
pub fn feed_dot_color(status: &str) -> egui::Color32 {
    dot_color_for(parse_feed_status(status))
}

/// The RIGHT dot: the remote `Scope::Control` segment, from the handle's own
/// `RemoteControlHandle::is_connected` bool rather than from the rendered line.
///
/// Amber while connected is not a "healthy" green by mistake — the segment is loud on purpose,
/// because an observer with an armed control channel can place REAL orders on the daemon. Red is
/// the link being down.
pub fn control_dot_color(connected: bool) -> egui::Color32 {
    if connected { AMBER } else { vike_ui_theme::palette::DOWN }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classifier the GUI shell carried until 2026-08-29, copied VERBATIM and FROZEN.
    ///
    /// It is here for one job: to let the tests below assert the exact set of strings whose dot
    /// CHANGES COLOUR, against the code that used to decide them rather than against a description
    /// of it. It is never called by anything but this module's tests, and it must never be
    /// "kept in sync" — the whole point is that it is a historical artefact. If someone reverts
    /// [`feed_dot_color`] to this logic, the three disagreement tests below go red together.
    fn hand_rolled_until_2026_08_29(status: &str) -> egui::Color32 {
        let low = status.to_ascii_lowercase();
        if low.contains("reconnect") || low.contains("connecting") {
            AMBER
        } else if low.contains("fault") || low.contains("error") {
            vike_ui_theme::palette::DOWN
        } else if low.contains("live") {
            vike_ui_theme::palette::ACCENT
        } else {
            MUTED
        }
    }

    #[test]
    fn every_state_maps_to_the_strips_own_colour() {
        assert_eq!(dot_color_for(ConnectionState::Connected), vike_ui_theme::palette::ACCENT);
        assert_eq!(dot_color_for(ConnectionState::Connecting), AMBER);
        assert_eq!(dot_color_for(ConnectionState::Error), vike_ui_theme::palette::DOWN);
        assert_eq!(dot_color_for(ConnectionState::Disconnected), MUTED);
        assert_eq!(dot_color_for(ConnectionState::Unknown), MUTED);
    }

    /// ⚠ DISAGREEMENT 1 — precedence. The commonest failure line every venue writes.
    ///
    /// Spelled from the producers: `crates/bridges/binance/src/family/market_feed.rs`,
    /// `crates/bridges/bybit/src/market_feed.rs`, `crates/bridges/okx/src/market_feed.rs`,
    /// `crates/bridges/deribit/src/market_feed.rs`,
    /// `crates/bridges/hyperliquid/src/market_feed.rs` and
    /// `crates/bridges/polymarket/src/market_feed.rs` all format exactly this shape through their
    /// own `set_status`.
    #[test]
    fn a_ws_error_that_is_also_reconnecting_now_reads_as_an_error() {
        let s = "btcusdt@kline_1m ws error (reconnecting): connection reset";
        assert_eq!(hand_rolled_until_2026_08_29(s), AMBER, "the old classifier read it as amber");
        assert_eq!(feed_dot_color(s), vike_ui_theme::palette::DOWN);
        assert_eq!(
            feed_dot_color(s),
            dot_color_for(parse_feed_status(s)),
            "the strip and the Connections tool must reach the same verdict from the same string"
        );
    }

    /// ⚠ DISAGREEMENT 2 — what counts as connected, in BOTH of its halves.
    ///
    /// The LATENT half: `connected`/`streaming`/`subscribed` are accepted by the shared parser and
    /// were not by the shell. No producer in `crates/bridges/*/src/market_feed.rs` writes any of the
    /// three today, so this half changes no pixel until one does — it is pinned so that the day one
    /// does, the strip and the Connections tool already agree.
    ///
    /// The LIVE half: the shell's chain had no arm for `failed` at all, so the observe bridge's
    /// dial-failure line (`crates/vike-app-core/src/observe_bridge.rs`, the `Err(e)` arm of the
    /// reconnect loop) fell through to the DEFAULT grey.
    #[test]
    fn the_connected_family_is_the_shared_one_and_failed_is_an_error() {
        for s in ["connected", "streaming ticks", "subscribed to btcusdt@kline_1m"] {
            assert_eq!(hand_rolled_until_2026_08_29(s), MUTED, "the old classifier greyed `{s}`");
            assert_eq!(feed_dot_color(s), vike_ui_theme::palette::ACCENT, "`{s}`");
        }
        let dial = "observe connect to 127.0.0.1:9301 failed (connection refused); retrying…";
        assert_eq!(hand_rolled_until_2026_08_29(dial), MUTED);
        assert_eq!(feed_dot_color(dial), vike_ui_theme::palette::DOWN);
    }

    /// The other side of the precedence change, and the one that reads as a LOSS unless it is
    /// written down: `disconnected` now wins over `reconnecting`, so the observe bridge's
    /// link-dropped line goes AMBER -> GREY. That is the shared parser's documented rule (its arm 1
    /// exists so `"disconnected"` never false-positives on the `"connected"` inside it), and this
    /// test exists so the regression is a decision on record rather than a surprise.
    #[test]
    fn a_dropped_link_that_is_reconnecting_now_reads_as_disconnected() {
        let s = "disconnected from 127.0.0.1:9301, reconnecting…";
        assert_eq!(hand_rolled_until_2026_08_29(s), AMBER);
        assert_eq!(feed_dot_color(s), MUTED);
    }

    /// The strings that must NOT move — the ones the strip spends almost all of its time showing.
    /// A unification that repainted the healthy case would be a far worse change than the one it
    /// fixed, so the no-op half is asserted, not assumed.
    #[test]
    fn the_everyday_lines_keep_the_colour_they_had() {
        for s in [
            "connecting to Binance…",           // `App::new`'s initial line
            "LIVE · Binance",                   // the family market-feed lane
            "LIVE · Hyperliquid trades BTC",    // …and a per-lane variant
            "btcusdt@kline_1m seed error: 418", // the seed/warmup failure lane
            "CORE FAULT: handler panicked",     // `core_sync`'s fault line, ahead of the feed's
        ] {
            assert_eq!(feed_dot_color(s), hand_rolled_until_2026_08_29(s), "`{s}` changed colour");
        }
    }

    /// ⚠ THE ONE EVERYDAY LINE THAT DID CHANGE COLOUR — it was in the list above, and it was pinned
    /// one test down as an OPEN gap.
    ///
    /// The pin read: `"OBSERVING <addr>"` matches no token in the shared parser, so a HEALTHY
    /// observe link reads grey; closing it means teaching
    /// `crates/vike-app-core/src/observe_bridge.rs`'s `observing_status` to say `connected`, or
    /// teaching the parser a new token. The FIRST was chosen — the parser is shared by three crates
    /// while the producer is the side that knows it is connected; the argument is on that function,
    /// along with the accident it also removes (a LIVE daemon's `[LIVE]` tag was classifying the
    /// line all by itself, so the dot tracked arming rather than connectivity).
    ///
    /// That the shared parser gained NOTHING is asserted here in the only way that cannot rot: the
    /// literal it used to be handed still reads `Unknown`.
    #[test]
    fn a_healthy_observe_link_now_reads_connected() {
        let line = crate::observe_bridge::observing_status("127.0.0.1:9301", None);
        assert_eq!(parse_feed_status(&line), ConnectionState::Connected, "`{line}`");
        assert_eq!(feed_dot_color(&line), vike_ui_theme::palette::ACCENT);
        assert_eq!(
            hand_rolled_until_2026_08_29(&line),
            MUTED,
            "the shell's frozen classifier greyed it too — this is a fix, not a regression"
        );
        assert_eq!(parse_feed_status("OBSERVING 127.0.0.1:9301"), ConnectionState::Unknown);
    }

    /// ⚠ DISAGREEMENT 3 — the bool round trip, and the false red it could produce.
    ///
    /// Built through the REAL [`crate::tradehub_control::control_status_line`] so the string is the
    /// one the strip actually renders, not a plausible-looking hand-written twin.
    #[test]
    fn the_control_dot_reads_the_bool_and_not_the_rendered_line() {
        let line = crate::tradehub_control::control_status_line(
            true,
            true,
            Some("peer disconnected while the order was in flight"),
            None,
        )
        .expect("a present control channel renders a line");
        assert!(
            line.contains("disconnected"),
            "the premise: a CONNECTED channel's line can carry that word in its error tail"
        );
        assert_eq!(
            control_dot_color(true),
            AMBER,
            "the link is up, so the segment stays the loud armed amber"
        );
        assert_eq!(control_dot_color(false), vike_ui_theme::palette::DOWN);
    }
}
