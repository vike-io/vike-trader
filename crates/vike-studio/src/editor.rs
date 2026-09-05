//! The code-editor pane: `egui_code_editor::CodeEditor` bound to the strategy source string.
use crate::syntax::rhai_syntax;
use egui_code_editor::{CodeEditor, ColorTheme};

pub struct EditorPane {
    pub source: String,
}

impl Default for EditorPane {
    fn default() -> Self {
        Self { source: DEFAULT_SCRIPT.to_string() }
    }
}

const DEFAULT_SCRIPT: &str = r#"const FAST = 5;
const SLOW = 20;
const QTY  = 1.0;

// SMA crossover: long above, short below.
fn on_bar() {
    let f = sma(FAST);
    let s = sma(SLOW);
    if s.is_nan() { return; }            // warm-up
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 {
        market(if delta > 0.0 { 1 } else { -1 }, abs(delta));
    }
}
"#;

/// Compile `src` as a `RhaiStrategy` (the same compile path `run_slice` uses) and collapse the
/// result to `Ok(())`/`Err(message)` — the pure core of the editor's compile-status dot. Rhai has
/// no notion of warnings, so this is a strict green/red split.
pub fn compile_status(src: &str) -> Result<(), String> {
    vike_script::RhaiStrategy::<vike_backtest::SimBroker>::compile(src)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Parse the 1-based line number out of a Rhai error message, if one is recoverable.
///
/// `vike_script::ScriptError` is a stringified `rhai` error (see `vike-script/src/strategy.rs`'s
/// doc on `ScriptError`), so there is no structured `Position` to read here — only the rendered
/// text. Empirically (verified against `rhai` 1.25.1, both parse errors and top-level runtime
/// errors), that text always ends with a `(line N, position M)` suffix, e.g.
/// `"Expecting ')' to close the parameters list of function 'on_bar' (line 1, position 12)"`.
/// This parses that suffix; a message without it (or a malformed one) returns `None` rather than
/// panicking — never assume the suffix is present.
pub fn error_line(err: &str) -> Option<u32> {
    let digits_start = err.find("(line ")? + "(line ".len();
    let digits: String = err[digits_start..].chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Render a compile error for the inline banner below the editor: `✗ line N · <message>` when a
/// line number is recoverable (the redundant `(line N, position M)` suffix is dropped from the
/// message body since the prefix already carries it), else a plain `✗ <message>` — never panics.
pub fn format_compile_error(err: &str) -> String {
    match error_line(err) {
        Some(line) => {
            let msg = err.find("(line ").map(|idx| err[..idx].trim_end()).unwrap_or(err);
            format!("✗ line {line} · {msg}")
        }
        None => format!("✗ {err}"),
    }
}

/// The editor's font size — one authority for both the render call and `rows_for_height`.
const EDITOR_FONT_SIZE: f32 = 13.0;

/// How many editor rows fit in `avail_height` pixels at `font_size` — the pure core of the
/// fill-the-panel sizing in [`EditorPane::ui_sized`] (the old fixed `with_rows(20)` left a dead
/// gap under the editor in any panel taller than ~420px). egui_code_editor 0.3.7's real row
/// pitch is ≈ `font_size * 1.16` (measured from a VIKE_SHOT capture: 45 rows over ~680 logical
/// px at font 13 — the first-guess 1.5 factor left a ~22%-of-panel dead gap). A slight overshoot
/// is harmless (the widget's own scroll region just reaches the panel bottom); the result is
/// clamped to `8..=400` so a degenerate available height never yields a zero-row editor.
pub fn rows_for_height(avail_height: f32, font_size: f32) -> usize {
    let row_h = font_size * 1.16;
    ((avail_height / row_h) as usize).clamp(8, 400)
}

impl EditorPane {
    /// Render at the default fixed height (20 rows) — kept for callers without a height budget.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.ui_sized(ui, 20.0 * EDITOR_FONT_SIZE * 1.16);
    }

    /// Render the editor sized to fill `avail_height` pixels (the Studio's editor panel passes
    /// its remaining panel height so the code area reaches the bottom instead of stopping at a
    /// fixed 20 rows).
    pub fn ui_sized(&mut self, ui: &mut egui::Ui, avail_height: f32) {
        // NOTE: egui_code_editor 0.3.7's `show` signature is `(ui, text, syntax)` — text before
        // syntax. The crate's own top-of-lib.rs doc example (`.show(ui, &self.syntax,
        // &mut self.code)`) is stale for this version and does NOT match the compiled API.
        CodeEditor::default()
            .id_source("vike-studio-editor")
            .with_rows(rows_for_height(avail_height, EDITOR_FONT_SIZE))
            .with_fontsize(EDITOR_FONT_SIZE)
            .with_theme(ColorTheme::GRUVBOX)
            .with_numlines(true)
            .show(ui, &mut self.source, &rhai_syntax());
    }
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
        let e = EditorPane::default();
        assert!(e.source.contains("fn on_bar"));
        assert!(e.source.contains("sma(FAST)"));
    }
    #[test]
    fn rhai_syntax_is_defined() {
        let _ = crate::syntax::rhai_syntax(); // constructs without panic
    }

    #[test]
    fn compile_status_is_ok_for_the_default_sma_cross_script() {
        assert_eq!(compile_status(&EditorPane::default().source), Ok(()));
    }

    #[test]
    fn compile_status_is_a_nonempty_err_for_broken_source() {
        let err = compile_status("fn on_bar( {").expect_err("malformed script must not compile");
        assert!(!err.is_empty());
    }

    /// `error_line` parses a real Rhai parse-error message (captured empirically against `rhai`
    /// 1.25.1 by compiling `"fn on_bar( {"`).
    #[test]
    fn error_line_parses_a_known_rhai_error_string() {
        let msg =
            "Expecting ')' to close the parameters list of function 'on_bar' (line 1, position 12)";
        assert_eq!(error_line(msg), Some(1));
    }

    /// A message with no `(line N, position M)` suffix (or any garbage string) returns `None`
    /// rather than panicking.
    #[test]
    fn error_line_is_none_for_a_message_without_a_line_marker() {
        assert_eq!(error_line("some generic failure"), None);
        assert_eq!(error_line(""), None);
        assert_eq!(error_line("(line )"), None); // malformed suffix, no digits
    }

    /// Multi-line source: the parsed line number matches where the error actually is, not just
    /// line 1 — regression against an implementation that only looks at the first line.
    #[test]
    fn error_line_reports_the_actual_failing_line_in_a_multiline_script() {
        let err = compile_status("fn on_bar() {\n  let x = ;\n}")
            .expect_err("malformed script must not compile");
        assert_eq!(error_line(&err), Some(2));
    }

    #[test]
    fn format_compile_error_prefixes_the_line_number_and_drops_the_redundant_suffix() {
        let msg = "unknown function 'ema_cross' (line 8, position 3)";
        let out = format_compile_error(msg);
        assert_eq!(out, "✗ line 8 · unknown function 'ema_cross'");
    }

    #[test]
    fn format_compile_error_falls_back_to_the_plain_message_without_a_line_number() {
        let out = format_compile_error("some generic failure");
        assert_eq!(out, "✗ some generic failure");
    }

    /// End-to-end: the real compile-status error for the known-broken default test script threads
    /// cleanly through `format_compile_error` (no panic, and the line number lands on the actual
    /// syntax error).
    #[test]
    fn format_compile_error_on_a_real_compile_failure_never_panics() {
        let err = compile_status("fn on_bar( {").expect_err("malformed script must not compile");
        let out = format_compile_error(&err);
        assert!(out.starts_with("✗ line 1 ·"), "got: {out}");
    }
}
