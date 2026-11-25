//! The code-editor pane: `egui_code_editor::CodeEditor` bound to the strategy source string.
//!
//! A line can be jumped to ([`StudioEditorPane::jump_to_line`], a click in the build-error list):
//! the next frame scrolls it into view and puts the cursor at its start. `egui_code_editor` 0.5.0
//! has no gutter-marker API (its line numbers are a non-interactive text field), so an error is
//! shown in the list under the editor, not beside its line.
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange};
use egui_code_editor::{CodeEditor, ColorTheme};
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::studio;

// Syntax highlighting for the Rhai editor: a child of this pane (`editor/syntax.rs`).
mod syntax;
use syntax::rhai_syntax;

pub struct StudioEditorPane {
    pub source: String,
    /// The line (1-based) a click asked for: the next frame scrolls to it and puts the cursor there.
    pub(crate) jump_to: Option<usize>,
    /// The last frame's laid-out text and where it was drawn: what a jump scrolls by, BEFORE the
    /// editor's scroll area closes (a target set after it is read by no one).
    laid_out: Option<(Arc<egui::Galley>, egui::Pos2)>,
    /// How wide the text was last frame, from the scroll content's left edge: the width the
    /// content is held at, so the editor's own inner scroll never has anything to scroll.
    content_width: f32,
    /// The cursor last frame: a move scrolls it into view.
    last_cursor: Option<CCursorRange>,
}

impl Default for StudioEditorPane {
    fn default() -> Self {
        Self {
            source: DEFAULT_SCRIPT.to_string(),
            jump_to: None,
            laid_out: None,
            content_width: 0.0,
            last_cursor: None,
        }
    }
}

/// The char index (what egui's cursor counts) where 1-based `line` of `source` starts; past the
/// last line, the end.
pub(crate) fn line_start_char(source: &str, line: usize) -> usize {
    source.split_inclusive('\n').take(line.saturating_sub(1)).map(|l| l.chars().count()).sum()
}

/// The template a fresh editor holds. LONG-ONLY, so it trades on every spot pair: a Studio run
/// states no margin mode, and the spot law refuses a short on a pair whose name says spot.
const DEFAULT_SCRIPT: &str = r#"const FAST = 5;
const SLOW = 20;
const QTY  = 1.0;

// SMA crossover, long-only: buy above, sell the long
// below. Never short, so it trades on a spot pair
// (which cannot sell what it does not hold) too.
fn on_bar(strategy) {
    let f = sma(FAST);
    let s = sma(SLOW);
    if s.is_nan() { return; }            // warm-up
    if f > s {
        strategy.entry("Long", OrderSide::Long).size(QTY);
    } else if strategy.position() > 0.0 {
        strategy.close("Long");
    }
}
"#;

/// The strategy the SWEEP capture hook loads ([`crate::StudioAutorun::Sweep`]) — [`DEFAULT_SCRIPT`]'s
/// exact logic with its two lookbacks exposed as `param()`s instead of `const`s.
///
/// ⚠ **It exists because the shipped default is not sweepable, and that is a real property of the
/// default rather than an oversight of this hook.** A sweep varies the values
/// `vike_rhai::discover_params` reports, that function reports what `param()` declared, and
/// `DEFAULT_SCRIPT` declares its lookbacks with `const`. So ▶ "Seed grid from params" produces an
/// EMPTY grid over it, `StudioState::start_sweep` returns early on an empty grid, and a capture
/// pointed at the Sweep tab renders the empty form — the exact frame
/// `.trader/shots/manifest.json`'s `grid-studio-sweep.png` exists to avoid.
///
/// It is the same strategy, and that is the point: the capture shows a REAL sweep of the app's own
/// example over real bars, not a strategy invented for a screenshot. `param()` with the same
/// defaults means the mid rung of every swept ladder reproduces `DEFAULT_SCRIPT`'s own behaviour.
///
/// ⚠ Loading it MUTATES `editor.source`, which a workspace persists — see the
/// `qa_workspace_readonly` derivation in `crates/vike-studio/src/studio/mod.rs`'s `new_with_qa`, which
/// arms the read-only guard for this autorun for that reason.
pub(crate) const SWEEP_CAPTURE_SCRIPT: &str = r#"let fast = param("fast", 5.0);
let slow = param("slow", 20.0);
const QTY = 1.0;

// SMA crossover, long-only: buy above, sell the long
// below. The default template, with its two lookbacks
// declared as sweepable params.
fn on_bar(strategy) {
    let f = sma(fast);
    let s = sma(slow);
    if s.is_nan() { return; }            // warm-up
    if f > s {
        strategy.entry("Long", OrderSide::Long).size(QTY);
    } else if strategy.position() > 0.0 {
        strategy.close("Long");
    }
}
"#;

/// Compile `src` and run its top level once — exactly the failures `RhaiStrategy::compile` (the
/// compile a Run's `"rhai"` arm does on the server) can return — and collapse the result to
/// `Ok(())`/`Err(error)`: the pure core of the editor's compile-status dot. Rhai has no notion of
/// warnings, so this is a strict green/red split. The error carries its line TYPED
/// (`vike_rhai::ScriptError::line`), so nothing here parses rhai's rendered text.
///
/// Through the broker-free `vike_rhai::discover_params` (the same engine, compile and one top-level
/// run, with no `Strategy<B>` mounted) rather than `RhaiStrategy::<B>::compile`: that type needs a
/// broker, and the only one this tree had to name was the simulator's, which the desktop does not link
/// (docs/decisions/0078's amendment of 2026-11-10).
pub(crate) fn compile_status(src: &str) -> Result<(), vike_rhai::ScriptError> {
    vike_rhai::discover_params(src).map(|_| ())
}

/// Render a compile error for the inline banner below the editor: `line N · <message>` when the
/// error knows its line (the message without its `(line N, position M)` suffix, since the prefix
/// already carries it), else rhai's whole message. The banner leads it with `icons::FAILED`; the
/// text carries no glyph, because an icon cannot live inside a string (`vike_ui_theme::icons`'
/// module doc).
pub(crate) fn format_compile_error(err: &vike_rhai::ScriptError) -> String {
    match err.line() {
        Some(line) => format!("line {line} · {}", err.summary()),
        None => err.to_string(),
    }
}

/// How many editor rows fit in `avail_height` pixels at `font_size` — the pure core of the
/// fill-the-panel sizing in [`StudioEditorPane::ui_sized`] (the old fixed `with_rows(20)` left a dead
/// gap under the editor in any panel taller than ~420px). egui_code_editor 0.3.7's real row
/// pitch is ≈ `font_size * 1.16` (measured from a VIKE_SHOT capture: 45 rows over ~680 logical
/// px at font 13 — the first-guess 1.5 factor left a ~22%-of-panel dead gap). A slight overshoot
/// is harmless (the widget's own scroll region just reaches the panel bottom); the result is
/// clamped to `8..=400` so a degenerate available height never yields a zero-row editor.
pub(crate) fn rows_for_height(avail_height: f32, font_size: f32) -> usize {
    let row_h = font_size * 1.16;
    ((avail_height / row_h) as usize).clamp(8, 400)
}

impl StudioEditorPane {
    /// Ask the next frame to scroll to 1-based `line` and put the cursor at its start.
    pub(crate) fn jump_to_line(&mut self, line: usize) {
        self.jump_to = Some(line);
    }

    /// Render the editor sized to fill `avail_height` pixels (the Studio's editor panel passes
    /// its remaining panel height so the code area reaches the bottom instead of stopping at a
    /// fixed 20 rows). Returns the code field's response, for the caller to name it.
    ///
    /// ⚠ **One scroll area for both axes, ours, with bars that show.** `egui_code_editor` puts the
    /// text in a horizontal scroll area of its own INSIDE its vertical one, so a long line's bar sat
    /// at the bottom of the whole text (below the panel on any file taller than it) and egui's
    /// floating style drew it only under a hovering pointer: long lines read as cut off at the
    /// panel's edge. Here the editor's own vertical scroll is off, the content is widened to the
    /// text's last measured width so its inner area never scrolls, and this area scrolls both axes
    /// with solid bars at the panel's bottom and right. The line numbers are painted by
    /// [`Self::code_ui`], not the editor, at the area's visible left edge, so they stay put while the
    /// text scrolls sideways under them. The cursor is followed here too, since the inner area
    /// swallows the text edit's own follow.
    pub(crate) fn ui_sized(&mut self, ui: &mut egui::Ui, avail_height: f32) -> egui::Response {
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        egui::ScrollArea::both()
            .id_salt("vike-studio-editor-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| self.code_ui(ui, avail_height))
            .inner
    }

    /// The code field inside [`Self::ui_sized`]'s scroll area: no wrap, as wide as the text was last
    /// frame, the asked line jumped to, the cursor kept in view.
    fn code_ui(&mut self, ui: &mut egui::Ui, avail_height: f32) -> egui::Response {
        ui.set_min_width(self.content_width);
        let jump = self.jump_to.take().map(|line| line_start_char(&self.source, line));
        if let (Some(at), Some((galley, origin))) = (jump, &self.laid_out) {
            let rect = cursor_rect(galley, *origin, CCursor::new(at));
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        }
        // The gutter the line numbers are painted in: room for the last line's digits, then an inner
        // margin, so text scrolled sideways under it never touches a number (the owner's pass of
        // 2026-11-21).
        let digits = self.source.lines().count().max(1).to_string().len().max(2) as f32;
        let numbers_width = digits * studio::EDITOR_FONT_SIZE * 0.6 + space::MD;
        let gutter_width = numbers_width + space::MD;
        // NOTE: egui_code_editor 0.3.7's `show` signature is `(ui, text, syntax)` — text before
        // syntax. The crate's own top-of-lib.rs doc example (`.show(ui, &self.syntax,
        // &mut self.code)`) is stale for this version and does NOT match the compiled API.
        let mut output = ui
            .horizontal_top(|ui| {
                ui.add_space(gutter_width);
                CodeEditor::default()
                    .id_source("vike-studio-editor")
                    // One row short of the panel: the estimate overshoots a little, and over the
                    // solid bars an overshoot is a vertical bar on a text that fits.
                    .with_rows(rows_for_height(avail_height, studio::EDITOR_FONT_SIZE) - 1)
                    .with_fontsize(studio::EDITOR_FONT_SIZE)
                    .with_theme(&ColorTheme::GRUVBOX.into())
                    .with_numlines(false)
                    .vscroll(false)
                    .show(ui, &mut self.source, &rhai_syntax())
                    .0
            })
            .inner;
        // The line numbers, at the scroll area's visible left edge (its clip), over the code: they
        // stay put while the text scrolls sideways under them.
        let clip = ui.clip_rect();
        let gutter =
            egui::Rect::from_x_y_ranges(clip.left()..=clip.left() + gutter_width, clip.y_range());
        let theme = ColorTheme::GRUVBOX;
        ui.painter().rect_filled(gutter, 0.0, theme.bg());
        let font = egui::FontId::monospace(studio::EDITOR_FONT_SIZE);
        let number = theme.type_color(egui_code_editor::TokenType::Comment(true));
        for (i, row) in output.galley.rows.iter().enumerate() {
            let y = output.galley_pos.y + row.rect().min.y;
            if y <= clip.bottom() && y + row.rect().height() >= clip.top() {
                let at = egui::pos2(gutter.left() + numbers_width - space::SM, y);
                let words = (i + 1).to_string();
                ui.painter().text(at, egui::Align2::RIGHT_TOP, words, font.clone(), number);
            }
        }
        let response = output.response.response.clone();
        if let Some(at) = jump {
            output.state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
            output.state.store(ui.ctx(), response.id);
            response.request_focus();
        }
        let cursor = output.cursor_range;
        if response.has_focus()
            && cursor != self.last_cursor
            && let Some(at) = cursor
        {
            ui.scroll_to_rect(cursor_rect(&output.galley, output.galley_pos, at.primary), None);
        }
        self.last_cursor = cursor;
        // The galley's right edge from the content's left, plus room for the cursor past the end.
        self.content_width =
            output.galley_pos.x - ui.min_rect().left() + output.galley.size().x + space::LG;
        self.laid_out = Some((output.galley, output.galley_pos));
        response
    }
}

/// Where egui's own text edit draws `cursor` of `galley` drawn at `origin`: the galley's rect, moved
/// to its origin.
fn cursor_rect(galley: &egui::Galley, origin: egui::Pos2, cursor: CCursor) -> egui::Rect {
    galley.pos_from_cursor(cursor).translate(origin.to_vec2() - egui::vec2(galley.rect.left(), 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_for_height_scales_with_available_height_and_clamps() {
        // 780px at 13px font ≈ 51 rows (measured pitch ≈ 15.08px) — the fill-the-panel case.
        assert_eq!(rows_for_height(780.0, 13.0), 51);
        // Degenerate heights clamp to the floor instead of yielding a zero-row editor.
        assert_eq!(rows_for_height(0.0, 13.0), 8);
        assert_eq!(rows_for_height(-50.0, 13.0), 8);
        // Absurd heights clamp to the ceiling.
        assert_eq!(rows_for_height(1.0e6, 13.0), 400);
    }

    #[test]
    fn default_source_is_a_runnable_sma_cross() {
        let e = StudioEditorPane::default();
        assert!(e.source.contains("fn on_bar"));
        assert!(e.source.contains("sma(FAST)"));
    }

    /// Both templates are LONG-ONLY: a short on a spot pair is refused whole by the spot law, so a
    /// template that ever goes short closes nothing there. The run that proves it trades on a spot
    /// pair is `crates/vike-studio/src/studio/tests/worker_polling.rs`'s
    /// `the_default_template_trades_on_a_spot_pair`.
    #[test]
    fn both_templates_never_go_short() {
        for src in [DEFAULT_SCRIPT, SWEEP_CAPTURE_SCRIPT] {
            assert!(!src.contains("OrderSide::Short"), "a template goes short:\n{src}");
            assert!(
                src.contains("strategy.close(\"Long\")"),
                "a template never sells its long:\n{src}"
            );
        }
    }
    /// A line's start counts CHARS, as egui's cursor does: a multi-byte letter above moves it by one.
    #[test]
    fn a_line_starts_at_its_char_index_and_past_the_end_is_the_end() {
        let source = "é\nab\nc";
        assert_eq!(line_start_char(source, 1), 0);
        assert_eq!(line_start_char(source, 2), 2);
        assert_eq!(line_start_char(source, 3), 5);
        assert_eq!(line_start_char(source, 9), 6);
        assert_eq!(line_start_char(source, 0), 0);
    }

    #[test]
    fn rhai_syntax_is_defined() {
        let _ = syntax::rhai_syntax(); // constructs without panic
    }

    #[test]
    fn compile_status_is_ok_for_the_default_sma_cross_script() {
        assert_eq!(compile_status(&StudioEditorPane::default().source), Ok(()));
    }

    #[test]
    fn compile_status_is_a_nonempty_err_for_broken_source() {
        let err = compile_status("fn on_bar( {").expect_err("malformed script must not compile");
        assert!(!err.to_string().is_empty());
    }

    /// Multi-line source: the line the banner names is where the error actually is, not just
    /// line 1 — regression against an implementation that only looks at the first line.
    #[test]
    fn the_banner_names_the_actual_failing_line_in_a_multiline_script() {
        let err = compile_status("fn on_bar() {\n  let x = ;\n}")
            .expect_err("malformed script must not compile");
        assert_eq!(err.line(), Some(2));
        assert!(
            format_compile_error(&err).starts_with("line 2 · "),
            "{}",
            format_compile_error(&err)
        );
    }

    /// End-to-end: the real compile error renders `line N · <words>` with the redundant
    /// `(line N, position M)` suffix dropped from the words.
    #[test]
    fn format_compile_error_prefixes_the_line_and_drops_the_redundant_suffix() {
        let err = compile_status("fn on_bar( {").expect_err("malformed script must not compile");
        let out = format_compile_error(&err);
        assert!(out.starts_with("line 1 · "), "got: {out}");
        assert!(!out.contains("(line"), "got: {out}");
    }
}
