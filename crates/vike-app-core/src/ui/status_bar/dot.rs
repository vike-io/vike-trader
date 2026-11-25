//! The bottom status strip's two DOTS, as pure functions: a feed status and a `bool` control link
//! in, an `egui::Color32` out — and the control segment's text ([`control_text`]), which leads with
//! the warning icon while the link is up.
//!
//! It began as a module apart from the strip because the strip was `vike-desktop`'s, a crate outside
//! the derived CI roster (`xtask::ci::tables::roster`'s `EXCLUDE_FROM_CI`), where a decision is compiled by
//! the `app-check` job and executed by almost nothing. Since design system step 7 the strip is
//! `crate::ui::status_bar`, one module over; these stay pure functions because each is tested per
//! variant.
//!
//! # ⚠ The feed dot is the STATE its producer set, never a reading of the words
//!
//! [`feed_dot_color`] takes a [`DataClientStatus`] and colours its
//! [`state`](DataClientStatus::state). Until 2026-11-21 it classified the LINE by keyword (first
//! with a hand-rolled chain of this strip's own, then with a shared substring classifier that is
//! deleted), and every round of that went wrong on words: a `"ws error (reconnecting)"` read amber
//! in one classifier and red in the other, a daemon tagged `[LIVE]` read green while one tagged
//! `[paper]` read grey, and the desktop's no-device-key hint, which said "press Connect", painted a
//! hard error AMBER. The producer is the side that knows; it says so beside the words.
//!
//! The control dot reads a `bool` for the same reason: [`control_dot_color`] takes
//! `TradehubControlHandle::is_connected`, never the rendered line (a CONNECTED channel whose latched
//! `last_error` tail carried the word `disconnected` once painted red).
//!
//! ⚠ **The colours are the STATUS palette's own** (`vike_ui_theme::status::OK`, `WARNING`, `ERROR`
//! and `MUTED`), never the app accent: the design system binds the accent to the theme, and
//! "connected" must not change colour with it.
//!
//! ⚠ **Which colour each state wears is a row of `ui-theme.toml`, not a `match` here.** The feed
//! dot reads the `connection` map through [`vike_account_manager::AccountManagerFeedFact::row`] — the one binding of a
//! [`DataClientConnectionState`] to its row, shared with the Connections tool's `Status` cell, so the two
//! cannot disagree — and the control dot reads the `control_link` map. Both maps' rows are STATUS
//! roles, which are the same on every theme, and that is the whole reason these functions take no
//! theme: they resolve a row under the default appearance (`status_colour`).
//! `the_dots_read_their_rows_on_every_theme` holds that to every theme, and goes red the day a row
//! names a theme role (`text_tertiary`, say), which is the day these take a `ThemeTokens`.

use crate::backend::tradehub_control::BackendControlLink;
use vike_account_manager::AccountManagerFeedFact;
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::maps::{self, ThemeMapRow};

/// A row's colour with no theme in hand. Right for a STATUS role (`ok warning error info muted`), which
/// ignores the theme: both dots read only those.
fn status_colour(row: &ThemeMapRow) -> egui::Color32 {
    row.colour.resolve(&ThemeTokens::from_appearance(&ThemeAppearance::default()))
}

/// Dot colour for a [`DataClientConnectionState`]: its row of the `connection` map. Split from
/// [`feed_dot_color`] so the mapping can be asserted per-variant; a new state added to the enum is a
/// compile error in [`AccountManagerFeedFact::row`] rather than a silent fall-through to grey. `Disconnected` and
/// `Unknown` share the muted grey, exactly as the Connections tool's Status cell does: both mean
/// "nothing live to show", and only the label text tells them apart (this strip has no label, so here
/// they are genuinely one state).
pub(crate) fn dot_color_for(state: DataClientConnectionState) -> egui::Color32 {
    status_colour(AccountManagerFeedFact::State(state).row())
}

/// The LEFT dot: the headline feed/backend status (`App::status` — the observe bridge's link status,
/// or `"CORE FAULT: …"` ahead of it), coloured by the state its producer set.
pub(crate) fn feed_dot_color(status: &DataClientStatus) -> egui::Color32 {
    dot_color_for(status.state)
}

/// The remote control link's two states the dot can see: the `bool` of
/// `TradehubControlHandle::is_connected` is [`BackendControlLink::Open`] (the write channel is up: an
/// observer that can place REAL orders on the daemon) or [`BackendControlLink::Lost`] (it is down), as
/// a row of the `control_link` map. The public surface stays the `bool`.
impl BackendControlLink {
    /// This state's row of the `control_link` map: `colour` the dot's and the segment's colour,
    /// `icon` the glyph that leads its text. Exhaustive: a state without a row does not compile.
    /// `ReadOnly` (no control handle) never comes from a `bool`; it reads the down row.
    fn row(self) -> &'static ThemeMapRow {
        match self {
            BackendControlLink::Open => &maps::control_link::CONNECTED,
            BackendControlLink::Lost | BackendControlLink::ReadOnly => {
                &maps::control_link::DISCONNECTED
            }
        }
    }
}

/// The RIGHT dot: the remote `Scope::Write` segment, from the handle's own
/// `TradehubControlHandle::is_connected` bool rather than from the rendered line.
///
/// Amber while connected is not a "healthy" green by mistake — the segment is loud on purpose,
/// because an observer with an armed control channel can place REAL orders on the daemon. Red is
/// the link being down. (Both are the `control_link` map's rows.)
pub(crate) fn control_dot_color(connected: bool) -> egui::Color32 {
    status_colour(BackendControlLink::of(Some(connected)).row())
}

/// The remote `Scope::Write` segment's TEXT: led by the warning icon while the link is up, plain
/// while it is down. A live channel can place REAL orders, and the segment's amber alone is not a
/// warning. The line itself carries no glyph — an icon cannot live inside a string (it would draw
/// in a text family; `vike_ui_theme::icons`' module doc) — so the icon is added here: the row's
/// `icon`, which only the connected row has.
pub(crate) fn control_text(
    style: &egui::Style,
    words: egui::RichText,
    connected: bool,
) -> egui::WidgetText {
    match BackendControlLink::of(Some(connected)).row().icon() {
        Some(icon) => icon.before(style, words).into(),
        None => words.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_ui_theme::status::MUTED;

    #[test]
    fn every_state_maps_to_the_strips_own_colour() {
        assert_eq!(dot_color_for(DataClientConnectionState::Connected), vike_ui_theme::status::OK);
        assert_eq!(
            dot_color_for(DataClientConnectionState::Connecting),
            vike_ui_theme::status::WARNING
        );
        assert_eq!(dot_color_for(DataClientConnectionState::Error), vike_ui_theme::status::ERROR);
        assert_eq!(dot_color_for(DataClientConnectionState::Disconnected), MUTED);
        assert_eq!(dot_color_for(DataClientConnectionState::Unknown), MUTED);
    }

    /// The control dot reads the bool, and a CONNECTED channel whose latched error tail carries the
    /// word `disconnected` stays the loud armed amber.
    ///
    /// Built through the REAL [`crate::backend::tradehub_control::access_line`] so the string is the
    /// one the strip actually renders, not a plausible-looking hand-written twin.
    #[test]
    fn the_control_dot_reads_the_bool_and_not_the_rendered_line() {
        let line = crate::backend::tradehub_control::access_line(
            crate::backend::tradehub_control::BackendAccess::Control,
            None,
            true,
            Some("peer disconnected while the order was in flight"),
        );
        assert!(
            line.contains("disconnected"),
            "the premise: a CONNECTED channel's line can carry that word in its error tail"
        );
        assert_eq!(
            control_dot_color(true),
            vike_ui_theme::status::WARNING,
            "the link is up, so the segment stays the loud armed amber"
        );
        assert_eq!(control_dot_color(false), vike_ui_theme::status::ERROR);
    }

    /// A LIVE control channel can place REAL orders, and the segment's amber alone is not a
    /// warning: its text leads with the warning icon while the link is up — and only then.
    #[test]
    fn a_live_control_segment_leads_with_the_warning_icon() {
        let style = egui::Style::default();
        let words = || egui::RichText::new("CONTROL live — this observer can place REAL orders");
        match control_text(&style, words(), true) {
            egui::WidgetText::LayoutJob(job) => {
                assert_eq!(job.sections[0].format.font_id.family, vike_ui_theme::icons::family());
                assert!(job.text.starts_with(&vike_ui_theme::icons::WARNING.accessible_label("")));
                assert!(job.text.ends_with("REAL orders"), "{}", job.text);
            }
            other => panic!("a live segment must lead with the warning icon: {:?}", other.text()),
        }
        let down = control_text(&style, words(), false);
        assert!(
            !matches!(down, egui::WidgetText::LayoutJob(_)),
            "a disconnected segment carries no warning icon: {:?}",
            down.text()
        );
    }

    /// Which colour each dot wears, pinned to what the strip painted before the colours moved to
    /// `ui-theme.toml`: the `connection` map for a feed (connected the status green, connecting the status
    /// amber, an error the status red, a feed that is down or has not reported the status grey) and the
    /// `control_link` map for the control link (the loud amber while it is up, red while it is down) — on
    /// every theme. Every row is a status role, so the dots take no theme and this is the test that says
    /// so: it goes red the day a row names a role that moves with the theme, which is the day
    /// `status_colour` needs a `ThemeTokens`.
    #[test]
    fn the_dots_read_their_rows_on_every_theme() {
        use vike_ui_theme::status::{ERROR, OK, WARNING};
        for id in vike_ui_theme::theme::ThemePaletteId::ALL {
            let t = ThemeTokens::from_appearance(&ThemeAppearance {
                theme: id,
                ..ThemeAppearance::default()
            });
            for (state, colour) in [
                (DataClientConnectionState::Connected, OK),
                (DataClientConnectionState::Connecting, WARNING),
                (DataClientConnectionState::Disconnected, MUTED),
                (DataClientConnectionState::Error, ERROR),
                (DataClientConnectionState::Unknown, MUTED),
            ] {
                assert_eq!(dot_color_for(state), colour, "{id:?} {state:?}: the dot");
                assert_eq!(
                    AccountManagerFeedFact::State(state).row().colour.resolve(&t),
                    colour,
                    "{id:?} {state:?}: the row, resolved under this theme"
                );
            }
            for (connected, colour) in [(true, WARNING), (false, ERROR)] {
                assert_eq!(control_dot_color(connected), colour, "{id:?} {connected}: the dot");
                assert_eq!(
                    BackendControlLink::of(Some(connected)).row().colour.resolve(&t),
                    colour,
                    "{id:?} {connected}: the row, resolved under this theme"
                );
            }
        }
    }

    /// The `control_link` map is the control dot's own: each state reads the row of its own name, no two
    /// share one, no row is left without a state, and the `bool` converts to the state of its name. The
    /// warning icon is the connected row's alone.
    #[test]
    fn every_control_state_has_its_own_row_and_every_row_its_state() {
        let all =
            [(BackendControlLink::Open, "CONNECTED"), (BackendControlLink::Lost, "DISCONNECTED")];
        for (state, key) in all {
            assert_eq!(state.row().key, key, "{state:?} reads another row");
            assert_eq!(
                all.iter().filter(|(other, _)| other.row() == state.row()).count(),
                1,
                "{state:?}'s row is shared"
            );
        }
        for row in maps::control_link::ALL {
            assert!(all.iter().any(|(state, _)| state.row() == *row), "{} is no state", row.key);
        }
        assert_eq!(BackendControlLink::of(Some(true)), BackendControlLink::Open);
        assert_eq!(BackendControlLink::of(Some(false)), BackendControlLink::Lost);
        assert_eq!(BackendControlLink::Open.row().icon(), Some(vike_ui_theme::icons::WARNING));
        assert_eq!(BackendControlLink::Lost.row().icon(), None);
    }
}
