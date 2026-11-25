//! The app's STATUS BAR: the strip along the bottom of the main window — the headline feed or
//! backend line and its dot, followed IN THE SAME ITEM by what this desktop may do on that backend
//! (`OBSERVING`, `CONTROL: …`, `ADMIN: …`), and the clock, the time zone and the chart and venue
//! counts on the right.
//!
//! It moved down out of `vike-desktop` in step 7 of the design system
//! (`docs/superpowers/specs/2026-09-28-gui-design-system-design.md` §9), for the reason
//! `crate::ui::caption` did: here the CI roster runs its tests. The shell keeps the frame's reads of
//! `App` the strip is drawn from, and clears the latched control error when [`status_bar()`] reports
//! that the operator dismissed it.
//!
//! It paints from the INSTALLED appearance (`vike_ui_theme::appearance::current`), never from
//! `vike_ui_theme::palette`'s compile-time Graphite: the theme's background under the strip, its
//! border as the hairline above it and as the separators, its secondary grey for the status line
//! and its caption grey for the clock, the zone and the counts. Its words are the Strong role —
//! persistent chrome text is the menu's role — and its dot the Caption role. The dots and the
//! control segment keep `crate::ui::status_bar::dot`'s status colours, which are the same in every theme
//! (spec §3.2).
//!
//! Its GEOMETRY is fixed chrome: its height (`status_bar::STATUS_BAR_HEIGHT`) and the gaps between its
//! items do not follow density.

use egui::{Align, Layout, RichText};
use vike_model::feed_status::DataClientStatus;
use vike_ui_theme::appearance;
use vike_ui_theme::components::ThemeStatus;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::theme::ThemePalette;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::status_bar;

use crate::backend::tradehub_control::BackendAccess;
use dot::{control_dot_color, control_text, feed_dot_color};

// The bottom status strip's two DOT COLOURS, as pure functions.
pub(crate) mod dot;

/// The access half of the status strip's ONE backend item: the words to paint after the connection
/// line, plus the facts painted AROUND them. `None` at the call site means there is no backend
/// connection at all — the default local path, where the strip renders byte-identically to a build
/// that never had one.
///
/// ⚠ ONE item, not two: the connection line names the daemon and its address, these words say what
/// this desktop may do there. Two segments each naming the daemon (`… — OBSERVING …` beside
/// `… · CONTROL live`) said it twice and contradicted themselves.
///
/// ⚠ It is a struct rather than three loose parameters for a reason that is NOT the argument count.
/// The loose form admitted a state that means nothing — a `connected` answer, or a latched error,
/// with no segment to paint either on (`(None, true, true)` type-checked and was silently ignored)
/// — and `Option<BackendAccessSegment>` cannot express it. `clippy::too_many_arguments` falling quiet is
/// a consequence; an `#[allow]` was the alternative and would have kept the nonsense state.
///
/// ⚠ `connected` is the handle's OWN `TradehubControlHandle::is_connected` bool, and it travels HERE,
/// beside the line it was built from, precisely so nobody re-derives it by searching that line for
/// `"disconnected"` — the round trip through prose this segment's dot was just fixed for. The two
/// fields are constructed together, from one read, in [`backend_access`].
#[derive(Clone)]
pub struct BackendAccessSegment {
    /// What this desktop may do: [`BackendAccess::Observe`] paints plain, a writing role in the armed amber.
    pub access: BackendAccess,
    /// `crate::backend::tradehub_control::access_line`'s output — the role's words and the truncated
    /// `last_error` tail, never the daemon (the connection line before it names it).
    pub line: String,
    pub connected: bool,
    /// Whether a LATCHED refusal is showing — a node's `Response::Error`, or a refusal the client
    /// latched itself before the wire (`TradehubControlHandle::latch_client_refusal`) — which is what
    /// makes the segment click-to-dismiss.
    pub(crate) has_error: bool,
}

/// The access half of the strip for the backend this window is connected to (`None` without one), built
/// from what the shell holds: the backend's record and its control channel. The role is what the node said
/// on that channel (`crate::backend::tradehub_control::control_access`, CONTROL from an older node); without a
/// channel the connection is read-only. The window is re-titled in the same words when they change
/// (`crate::backend::identity::window_title`): the title set before the loop names no role.
///
/// ONE read of the handle's `is_connected`, used by both the words and the dot beside them, and the
/// LATCHED `last_error` (a strip must not lose a refusal between repaints; only a click clears it).
pub fn backend_access(
    context: &egui::Context,
    backend: Option<&crate::backend::registry::BackendRecord>,
    control: Option<&vike_tradehub_client::TradehubControlHandle>,
) -> Option<BackendAccessSegment> {
    use crate::backend::tradehub_control::{access_label, access_line, control_access};
    let backend = backend?;
    let (access, who) =
        control.map_or((BackendAccess::Observe, None), |c| control_access(c.admitted()));
    let label = access_label(access, who.as_deref());
    let title = crate::backend::identity::window_title(Some(backend), Some(&label));
    if context.input(|i| i.viewport().title.as_deref() != Some(title.as_str())) {
        context.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }
    let connected = control.is_some_and(|c| c.is_connected());
    let last_error = control.and_then(|c| c.last_error());
    Some(BackendAccessSegment {
        access,
        line: access_line(access, who.as_deref(), connected, last_error.as_deref()),
        connected,
        has_error: last_error.is_some(),
    })
}

/// What the strip shows this frame.
pub struct StatusBar<'a> {
    /// The headline feed or backend status: its words are shown, its state is the dot
    /// (`feed_dot_color`).
    pub status: &'a DataClientStatus,
    pub display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
    /// Now, for the clock — a parameter, so a test draws a known time.
    pub now_ms: i64,
    pub n_charts: usize,
    /// How many venues the observed node runs an account on (`BackendNodeWords::venues`): what it trades on,
    /// never the desktop's own market-data feeds, which read 0 with the datahub out of reach.
    pub n_venues: usize,
    /// How many running accounts have lost their way to the venue (`BackendNodeWords::lost`): said only when
    /// there are some, because the Account window names which.
    pub n_lost: usize,
    /// `None` on every path with no backend connection.
    pub access: Option<BackendAccessSegment>,
}

/// The strip as the main window's bottom panel. `true` when the operator clicked the control
/// segment to dismiss its latched error.
pub fn status_bar(ui: &mut egui::Ui, bar: &StatusBar<'_>) -> bool {
    let mut dismissed = false;
    egui::Panel::bottom("statusbar")
        .resizable(false)
        .default_size(status_bar::STATUS_BAR_HEIGHT)
        .min_size(status_bar::STATUS_BAR_HEIGHT)
        .max_size(status_bar::STATUS_BAR_HEIGHT)
        .show_separator_line(false)
        .frame(
            egui::Frame::NONE
                .fill(ThemePalette::of(appearance::current(ui.ctx()).theme).background),
        )
        .show(ui, |ui| dismissed = strip(ui, bar));
    dismissed
}

/// The bottom status bar: a live connection dot + status text on the left, workspace summary
/// (open charts / venues / timezone / clock) on the right — the egui analog of the PySide status
/// bar. Purely presentational.
/// Returns `true` when the operator CLICKED the remote-control segment to dismiss its latched
/// refusal — the node's or the client's own (only possible while [`BackendAccessSegment::has_error`]);
/// the caller then calls `TradehubControlHandle::clear_last_error`. Nothing else clears it — a
/// refusal stays on the strip until it has been seen, which is what a status-bar "last error" is
/// for.
fn strip(ui: &mut egui::Ui, bar: &StatusBar<'_>) -> bool {
    let look = appearance::current(ui.ctx());
    let t = ThemePalette::of(look.theme);
    // Persistent chrome text is the menu's role (owner decision 1 of the step-7 plan).
    let px = look.text_size.px(ThemeTextRole::Strong);
    // 1px top divider so the strip reads as a distinct bar against the desktop.
    let r = ui.max_rect();
    ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(stroke::HAIRLINE, t.border));

    // Connection state → dot color: the STATE the line's producer set, never its words
    // (`crates/vike-app-core/src/ui/status_bar/dot.rs`'s module doc).
    let dot_col = feed_dot_color(bar.status);

    let mut dismissed = false;
    ui.horizontal_centered(|ui| {
        ui.add_space(space::LG);
        let dot_px = look.text_size.px(ThemeTextRole::Caption);
        ui.label(RichText::new("●").color(dot_col).size(dot_px));
        ui.add_space(space::XS);
        ui.label(RichText::new(&bar.status.text).color(t.text_secondary).size(px));

        // What this desktop may do on the backend, IN THE SAME ITEM as the connection line (`None`, the
        // default local path, renders nothing, so the strip is byte-identical off this path). Read-only
        // is the plain `OBSERVING`. A writing role is painted in a loud amber to flag that this
        // observer can drive REAL orders on the remote daemon, with a red when its link is down — from
        // `access.connected`, the handle's OWN `is_connected` bool. It used to be recovered by searching
        // the rendered line for `"disconnected"`, i.e. from a string built out of that same bool; a
        // connected channel whose latched `last_error` tail carried the word painted red.
        if let Some(access) = &bar.access {
            ui.label(RichText::new("·").color(t.text_secondary).size(px));
            if access.access == BackendAccess::Observe {
                ui.label(RichText::new(&access.line).color(t.text_secondary).size(px));
            } else {
                // The words keep `control_dot_color`'s status colours, fixed in every theme.
                let text = RichText::new(&access.line).color(control_dot_color(access.connected));
                let text = control_text(ui.style(), text.size(px).strong(), access.connected);
                if access.has_error {
                    // The words carry a LATCHED refusal (the node's, or one the client refused
                    // before the wire), so make them click-to-dismiss: it persists across repaints
                    // by design, and without this it would sit on the bar for the rest of the
                    // session. A dismiss clears only the banner — it says nothing about any command
                    // (each command's own outcome rides its `CommandTicket`).
                    let response =
                        ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text(
                            "click to dismiss this error (the command outcomes are unaffected)",
                        );
                    dismissed = response.clicked();
                } else {
                    ui.label(text);
                }
            }
        }

        // Right cluster: clock · timezone · venues · charts (laid out right-to-left).
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(space::LG);
            // Clock in the active display timezone (blank on tz-conversion failure).
            let clock = vike_ui_theme::time_zone::to_naive(bar.now_ms, bar.display_time_zone)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_default();
            ui.label(RichText::new(clock).color(t.text_tertiary).size(px).monospace());
            ui.add_space(space::XL);
            ui.label(RichText::new(bar.display_time_zone.label()).color(t.text_tertiary).size(px));
            ui.add_space(space::XL);
            ui.label(RichText::new("│").color(t.border).size(px));
            ui.add_space(space::XL);
            let plural = if bar.n_charts == 1 { "" } else { "s" };
            let charts = format!("{} chart{plural}", bar.n_charts);
            ui.label(RichText::new(charts).color(t.text_tertiary).size(px));
            ui.add_space(space::XL);
            let plural = if bar.n_venues == 1 { "" } else { "s" };
            let venues = format!("{} venue{plural}", bar.n_venues);
            ui.label(RichText::new(venues).color(t.text_tertiary).size(px));
            if bar.n_lost > 0 {
                let plural = if bar.n_lost == 1 { "" } else { "s" };
                let lost = format!("{} account{plural} lost", bar.n_lost);
                ui.add_space(space::XL);
                ui.label(RichText::new(lost).color(ThemeStatus::Error.color()).size(px));
            }
        });
    });
    dismissed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::caption::test_shapes::{pass, raw};
    use egui::Shape;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::sync::{Arc, Mutex};
    use vike_ui_theme::appearance::ThemeAppearance;
    use vike_ui_theme::painted::{context_with, lines, texts};
    use vike_ui_theme::theme::{ThemePalette, ThemePaletteId};
    use vike_ui_theme::time_zone::ThemeDisplayTimeZone;
    use vike_ui_theme::type_scale::{ThemeTextRole, ThemeTextSize};

    /// A status for a test strip; leaked, so a `StatusBar<'static>` can lend it.
    fn said(text: &str) -> &'static DataClientStatus {
        use vike_model::feed_status::DataClientConnectionState::Connected;
        Box::leak(Box::new(DataClientStatus::new(Connected, text)))
    }

    fn bar(access: Option<BackendAccessSegment>) -> StatusBar<'static> {
        StatusBar {
            status: said("live"),
            display_time_zone: ThemeDisplayTimeZone::Utc,
            now_ms: 0,
            n_charts: 2,
            n_venues: 3,
            n_lost: 0,
            access,
        }
    }

    /// The strip is the theme's background under a hairline in the theme's border. Both were
    /// Graphite's on every theme.
    #[test]
    fn the_strip_is_each_themes_background_under_its_border() {
        for id in ThemePaletteId::ALL {
            let context =
                context_with(&ThemeAppearance { theme: id, ..ThemeAppearance::default() });
            let shapes = pass(&context, raw(), |ui| {
                status_bar(ui, &bar(None));
            });
            let t = ThemePalette::of(id);
            let bar_h = status_bar::STATUS_BAR_HEIGHT;
            let strip = shapes
                .iter()
                .any(|s| matches!(s, Shape::Rect(r) if r.fill == t.background && r.rect.height() == bar_h));
            assert!(strip, "{id:?}: the strip's fill");
            assert!(lines(&shapes).contains(&t.border), "{id:?}: the hairline");
        }
    }

    /// The status line is the theme's secondary grey; the clock, the zone and the counts its caption
    /// grey; the separators its border. They were neutral greys 180 and 150 on every theme.
    #[test]
    fn its_words_are_the_themes_secondary_and_caption_greys() {
        for id in ThemePaletteId::ALL {
            let context =
                context_with(&ThemeAppearance { theme: id, ..ThemeAppearance::default() });
            let shapes = pass(&context, raw(), |ui| {
                status_bar(ui, &bar(None));
            });
            let t = ThemePalette::of(id);
            let words = texts(&shapes);
            let colour = |w: &str| words.iter().find(|(s, _, _)| s == w).map(|(_, _, c)| *c);
            assert_eq!(colour("live"), Some(t.text_secondary), "{id:?}");
            for w in ["00:00:00", "UTC", "2 charts", "3 venues"] {
                assert_eq!(colour(w), Some(t.text_tertiary), "{id:?}: {w}");
            }
            assert_eq!(colour("│"), Some(t.border), "{id:?}: the separator");
        }
    }

    /// Its words are the Strong role and its dot the Caption role, at both text sizes (owner
    /// decision 1: persistent chrome text is the menu's role).
    #[test]
    fn its_words_follow_the_text_size() {
        for size in ThemeTextSize::ALL {
            let context =
                context_with(&ThemeAppearance { text_size: size, ..ThemeAppearance::default() });
            let shapes = pass(&context, raw(), |ui| {
                status_bar(ui, &bar(None));
            });
            for (text, px, _) in texts(&shapes) {
                let role =
                    if text == "●" { ThemeTextRole::Caption } else { ThemeTextRole::Strong };
                assert_eq!(px, size.px(role), "{size:?}: {text:?}");
            }
        }
    }

    /// The count reads as a sentence: one venue is `1 venue`.
    #[test]
    fn one_venue_is_singular() {
        let context = context_with(&ThemeAppearance::default());
        let one = StatusBar { n_venues: 1, ..bar(None) };
        let shapes = pass(&context, raw(), |ui| {
            status_bar(ui, &one);
        });
        let words = texts(&shapes);
        assert!(words.iter().any(|(s, _, _)| s == "1 venue"), "{words:?}");
    }

    /// A lost account is said in an error's colour, and nothing is said while none is.
    #[test]
    fn lost_accounts_are_said_only_when_there_are_some() {
        let context = context_with(&ThemeAppearance::default());
        let drawn = |n_lost| {
            let lost = StatusBar { n_lost, ..bar(None) };
            texts(&pass(&context, raw(), |ui| {
                status_bar(ui, &lost);
            }))
        };
        assert!(!drawn(0).iter().any(|(s, _, _)| s.contains("lost")));
        let words = drawn(2);
        let line = words.iter().find(|(s, _, _)| s == "2 accounts lost");
        assert_eq!(line.map(|(_, _, c)| *c), Some(ThemeStatus::Error.color()), "{words:?}");
    }

    /// A latched control error is dismissed by a click on it — behaviour the move keeps, and which
    /// no test ran while the strip lived in the CI-excluded shell.
    #[test]
    fn a_latched_error_is_dismissed_by_a_click() {
        const LINE: &str = "control: refused by the node";
        let dismissed = Arc::new(Mutex::new(false));
        let sink = Arc::clone(&dismissed);
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 200.0)).build_ui(move |ui| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let access = BackendAccessSegment {
                access: BackendAccess::Control,
                line: LINE.to_string(),
                connected: false,
                has_error: true,
            };
            if status_bar(ui, &bar(Some(access))) {
                *sink.lock().unwrap() = true;
            }
        });
        h.run();
        // The segment's own node. A selectable egui `Label` also carries `TextRun` children with the
        // same value (egui's text-selection accessibility), so a query by value alone finds two.
        h.get_by(|n| {
            n.role() == egui::accesskit::Role::Label && n.value().as_deref() == Some(LINE)
        })
        .click();
        h.run();
        assert!(*dismissed.lock().unwrap());
    }

    /// The owner's report: the node was named twice and `OBSERVING` stood beside `CONTROL live`. Now
    /// ONE item names it once, and the word after it says the access (`OBSERVING` only read-only).
    fn one_item(access: BackendAccessSegment) -> Vec<(String, f32, egui::Color32)> {
        let context = context_with(&ThemeAppearance::default());
        let item = StatusBar {
            status: said("tradehub [LIVE + demo + paper] — 203.0.113.7:7879 (connected)"),
            ..bar(Some(access))
        };
        texts(&pass(&context, raw(), |ui| {
            status_bar(ui, &item);
        }))
    }

    #[test]
    fn an_observe_only_link_is_one_item_that_says_observing() {
        let line =
            crate::backend::tradehub_control::access_line(BackendAccess::Observe, None, true, None);
        let words = one_item(BackendAccessSegment {
            access: BackendAccess::Observe,
            line,
            connected: true,
            has_error: false,
        });
        let t = ThemePalette::of(ThemeAppearance::default().theme);
        let said = |w: &str| words.iter().filter(|(s, _, _)| s.contains(w)).count();
        assert_eq!(said("tradehub ["), 1, "the node is named once: {words:?}");
        assert_eq!(
            said("│"),
            1,
            "no separator splits the item, only the right cluster's: {words:?}"
        );
        let observing = words.iter().find(|(s, _, _)| s == "OBSERVING");
        assert_eq!(observing.map(|(_, _, c)| *c), Some(t.text_secondary), "plain: {words:?}");
        assert_eq!(said("CONTROL") + said("ADMIN"), 0, "{words:?}");
    }

    #[test]
    fn a_live_control_link_is_one_item_with_the_warning_and_no_observing() {
        for (access, who, words_of) in [
            (BackendAccess::Control, None, "CONTROL: can place REAL orders"),
            (BackendAccess::Admin, Some("owner/vike"), "ADMIN (owner/vike): can place REAL orders"),
        ] {
            let line = crate::backend::tradehub_control::access_line(access, who, true, None);
            let words =
                one_item(BackendAccessSegment { access, line, connected: true, has_error: false });
            let said = |w: &str| words.iter().filter(|(s, _, _)| s.contains(w)).count();
            assert_eq!(said("tradehub ["), 1, "{access:?}: the node is named once: {words:?}");
            assert_eq!(said("│"), 1, "{access:?}: one item: {words:?}");
            assert_eq!(said("OBSERVING"), 0, "{access:?}: read-only words beside a writing role");
            let warning = words.iter().find(|(s, _, _)| s.contains(words_of));
            assert_eq!(
                warning.map(|(_, _, c)| *c),
                Some(control_dot_color(true)),
                "{access:?}: the warning colour: {words:?}"
            );
        }
    }
}
