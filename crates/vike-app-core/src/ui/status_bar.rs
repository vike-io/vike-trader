//! The app's STATUS BAR: the strip along the bottom of the main window — the headline feed or
//! backend line and its dot, the remote-control segment while a control channel is armed, and the
//! clock, the time zone and the chart and venue counts on the right.
//!
//! It moved down out of `vike-desktop` in step 7 of the design system
//! (`docs/superpowers/specs/2026-09-28-gui-design-system-design.md` §9), for the reason
//! `crate::ui::caption` did: here the CI roster runs its tests. The shell keeps the frame's reads of
//! `App` the strip is drawn from, and clears the latched control error when [`status_bar`] reports
//! that the operator dismissed it.
//!
//! It paints from the INSTALLED appearance (`vike_ui_theme::appearance::current`), never from
//! `vike_ui_theme::palette`'s compile-time Graphite: the theme's background under the strip, its
//! border as the hairline above it and as the separators, its secondary grey for the status line
//! and its caption grey for the clock, the zone and the counts. Its words are the Strong role —
//! persistent chrome text is the menu's role — and its dot the Caption role. The dots and the
//! control segment keep `crate::ui::status_dot`'s status colours, which are the same in every theme
//! (spec §3.2).
//!
//! Its GEOMETRY is fixed chrome: [`STATUS_BAR_H`] and the gaps between its items do not follow
//! density.

use egui::{Align, Layout, RichText};
use vike_ui_theme::appearance;
use vike_ui_theme::theme::Theme;
use vike_ui_theme::type_scale::TextRole;

use crate::ui::status_dot::{control_dot_color, control_text, feed_dot_color};

/// Height (pts) of the bottom status bar strip.
pub const STATUS_BAR_H: f32 = 22.0;

/// The status strip's remote-control segment: the line to paint, plus the two facts painted AROUND
/// it. `None` at the call site means there is no control channel at all — the default local path,
/// where the strip renders byte-identically to a build that never had one.
///
/// ⚠ It is a struct rather than three loose parameters for a reason that is NOT the argument count.
/// The loose form admitted a state that means nothing — a `connected` answer, or a latched error,
/// with no segment to paint either on (`(None, true, true)` type-checked and was silently ignored)
/// — and `Option<ControlSegment>` cannot express it. `clippy::too_many_arguments` falling quiet is
/// a consequence; an `#[allow]` was the alternative and would have kept the nonsense state.
///
/// ⚠ `connected` is the handle's OWN `RemoteControlHandle::is_connected` bool, and it travels HERE,
/// beside the line it was built from, precisely so nobody re-derives it by searching that line for
/// `"disconnected"` — the round trip through prose this segment's dot was just fixed for. The two
/// fields are constructed together, from one read, in `app_ui.rs`.
#[derive(Clone, Copy)]
pub struct ControlSegment<'a> {
    /// `crate::backend::tradehub_control::control_status_line`'s output — it already carries the
    /// daemon identity tag and the truncated `last_error` tail.
    pub line: &'a str,
    pub connected: bool,
    /// Whether a LATCHED refusal is showing — a node's `Response::Error`, or a refusal the client
    /// latched itself before the wire (`RemoteControlHandle::latch_client_refusal`) — which is what
    /// makes the segment click-to-dismiss.
    pub has_error: bool,
}

/// What the strip shows this frame.
pub struct StatusBar<'a> {
    /// The headline feed or backend line; its dot is `feed_dot_color`'s.
    pub status: &'a str,
    pub display_tz: vike_chart::DisplayTz,
    /// Now, for the clock — a parameter, so a test draws a known time.
    pub now_ms: i64,
    pub n_charts: usize,
    pub n_venues: usize,
    /// `None` on every path with no remote control channel.
    pub control: Option<ControlSegment<'a>>,
}

/// The strip as the main window's bottom panel. `true` when the operator clicked the control
/// segment to dismiss its latched error.
pub fn status_bar(ui: &mut egui::Ui, bar: &StatusBar<'_>) -> bool {
    let mut dismissed = false;
    egui::Panel::bottom("statusbar")
        .resizable(false)
        .default_size(STATUS_BAR_H)
        .min_size(STATUS_BAR_H)
        .max_size(STATUS_BAR_H)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(Theme::of(appearance::current(ui.ctx()).theme).bg))
        .show(ui, |ui| dismissed = strip(ui, bar));
    dismissed
}

/// The bottom status bar: a live connection dot + status text on the left, workspace summary
/// (open charts / venues / timezone / clock) on the right — the egui analog of the PySide status
/// bar. Purely presentational.
/// Returns `true` when the operator CLICKED the remote-control segment to dismiss its latched
/// refusal — the node's or the client's own (only possible while [`ControlSegment::has_error`]);
/// the caller then calls `RemoteControlHandle::clear_last_error`. Nothing else clears it — a
/// refusal stays on the strip until it has been seen, which is what a status-bar "last error" is
/// for.
fn strip(ui: &mut egui::Ui, bar: &StatusBar<'_>) -> bool {
    let look = appearance::current(ui.ctx());
    let t = Theme::of(look.theme);
    // Persistent chrome text is the menu's role (owner decision 1 of the step-7 plan).
    let px = look.text_size.px(TextRole::Strong);
    // 1px top divider so the strip reads as a distinct bar against the desktop.
    let r = ui.max_rect();
    ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, t.border));

    // Connection state → dot color, through the ONE shared classifier
    // (`vike_model::feed_status::parse_feed_status`, which the Connections tool and the headless
    // reconcile health gate also read). This block used to hand-roll a second one; what that cost —
    // and exactly which strings changed colour when it went — is
    // `crates/vike-app-core/src/ui/status_dot.rs`'s module doc.
    let dot_col = feed_dot_color(bar.status);

    let mut dismissed = false;
    ui.horizontal_centered(|ui| {
        ui.add_space(8.0);
        let dot_px = look.text_size.px(TextRole::Caption);
        ui.label(RichText::new("●").color(dot_col).size(dot_px));
        ui.add_space(2.0);
        ui.label(RichText::new(bar.status).color(t.text2).size(px));

        // Remote Scope::Write channel summary (observe mode only; `None` — the default local
        // path — renders nothing, so the strip is byte-identical off this path). Painted in a loud
        // amber to flag that this observer can drive REAL orders on the remote daemon, with a red
        // dot when the control link is down — from `control.connected`, the handle's OWN
        // `is_connected` bool. It used to be recovered by searching the rendered line for
        // `"disconnected"`, i.e. from a string `control_status_line` had just built out of that same
        // bool; a connected channel whose latched `last_error` tail carried the word painted red.
        if let Some(control) = bar.control {
            ui.add_space(12.0);
            ui.label(RichText::new("│").color(t.border).size(px));
            ui.add_space(12.0);
            // The segment's words keep `control_dot_color`'s status colours, fixed in every theme.
            let text = RichText::new(control.line).color(control_dot_color(control.connected));
            let text = control_text(ui.style(), text.size(px).strong(), control.connected);
            if control.has_error {
                // The segment carries a LATCHED refusal (the node's, or one the client refused
                // before the wire), so make it click-to-dismiss: it
                // persists across repaints by design, and without this it would sit on the bar for
                // the rest of the session. A dismiss clears only the banner — it says nothing about
                // any command (each command's own outcome rides its `CommandTicket`).
                let resp =
                    ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text(
                        "click to dismiss this error (the command outcomes are unaffected)",
                    );
                dismissed = resp.clicked();
            } else {
                ui.label(text);
            }
        }

        // Right cluster: clock · timezone · venues · charts (laid out right-to-left).
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(8.0);
            // Clock in the active display timezone (blank on tz-conversion failure).
            let clock = vike_chart::tz::to_naive(bar.now_ms, bar.display_tz)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_default();
            ui.label(RichText::new(clock).color(t.text3).size(px).monospace());
            ui.add_space(10.0);
            ui.label(RichText::new(bar.display_tz.label()).color(t.text3).size(px));
            ui.add_space(10.0);
            ui.label(RichText::new("│").color(t.border).size(px));
            ui.add_space(10.0);
            let plural = if bar.n_charts == 1 { "" } else { "s" };
            let charts = format!("{} chart{plural}", bar.n_charts);
            ui.label(RichText::new(charts).color(t.text3).size(px));
            ui.add_space(10.0);
            let venues = format!("{} venues", bar.n_venues);
            ui.label(RichText::new(venues).color(t.text3).size(px));
        });
    });
    dismissed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::caption::test_shapes::{ctx_with, lines, pass, raw, texts};
    use egui::Shape;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::sync::{Arc, Mutex};
    use vike_chart::DisplayTz;
    use vike_ui_theme::appearance::Appearance;
    use vike_ui_theme::theme::{Theme, ThemeId};
    use vike_ui_theme::type_scale::{TextRole, TextSize};

    fn bar(control: Option<ControlSegment<'_>>) -> StatusBar<'_> {
        StatusBar {
            status: "live",
            display_tz: DisplayTz::Utc,
            now_ms: 0,
            n_charts: 2,
            n_venues: 3,
            control,
        }
    }

    /// The strip is the theme's background under a hairline in the theme's border. Both were
    /// Graphite's on every theme.
    #[test]
    fn the_strip_is_each_themes_background_under_its_border() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let shapes = pass(&ctx, raw(), |ui| {
                status_bar(ui, &bar(None));
            });
            let t = Theme::of(id);
            let strip = shapes.iter().any(|s| {
                matches!(s, Shape::Rect(r) if r.fill == t.bg && r.rect.height() == STATUS_BAR_H)
            });
            assert!(strip, "{id:?}: the strip's fill");
            assert!(lines(&shapes).contains(&t.border), "{id:?}: the hairline");
        }
    }

    /// The status line is the theme's secondary grey; the clock, the zone and the counts its caption
    /// grey; the separators its border. They were neutral greys 180 and 150 on every theme.
    #[test]
    fn its_words_are_the_themes_secondary_and_caption_greys() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let shapes = pass(&ctx, raw(), |ui| {
                status_bar(ui, &bar(None));
            });
            let t = Theme::of(id);
            let words = texts(&shapes);
            let colour = |w: &str| words.iter().find(|(s, _, _)| s == w).map(|(_, _, c)| *c);
            assert_eq!(colour("live"), Some(t.text2), "{id:?}");
            for w in ["00:00:00", "UTC", "2 charts", "3 venues"] {
                assert_eq!(colour(w), Some(t.text3), "{id:?}: {w}");
            }
            assert_eq!(colour("│"), Some(t.border), "{id:?}: the separator");
        }
    }

    /// Its words are the Strong role and its dot the Caption role, at both text sizes (owner
    /// decision 1: persistent chrome text is the menu's role).
    #[test]
    fn its_words_follow_the_text_size() {
        for size in TextSize::ALL {
            let ctx = ctx_with(&Appearance { text_size: size, ..Appearance::default() });
            let shapes = pass(&ctx, raw(), |ui| {
                status_bar(ui, &bar(None));
            });
            for (text, px, _) in texts(&shapes) {
                let role = if text == "●" { TextRole::Caption } else { TextRole::Strong };
                assert_eq!(px, size.px(role), "{size:?}: {text:?}");
            }
        }
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
            let control = ControlSegment { line: LINE, connected: false, has_error: true };
            if status_bar(ui, &bar(Some(control))) {
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
}
