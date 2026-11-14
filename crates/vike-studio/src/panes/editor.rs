//! The code-editor pane: `egui_code_editor::CodeEditor` bound to the strategy source string.
use egui_code_editor::{CodeEditor, ColorTheme};
use vike_ui_theme::value::studio;

// Syntax highlighting for the Rhai editor: a child of this pane (`editor/syntax.rs`).
mod syntax;
use syntax::rhai_syntax;

pub struct EditorPane {
    pub source: String,
}

impl Default for EditorPane {
    fn default() -> Self {
        Self { source: DEFAULT_SCRIPT.to_string() }
    }
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

/// The strategy the SWEEP capture hook loads ([`crate::QaAutorun::Sweep`]) — [`DEFAULT_SCRIPT`]'s
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
/// `qa_workspace_readonly` derivation in `crates/vike-studio/src/studio.rs`'s `new_with_qa`, which
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
/// fill-the-panel sizing in [`EditorPane::ui_sized`] (the old fixed `with_rows(20)` left a dead
/// gap under the editor in any panel taller than ~420px). egui_code_editor 0.3.7's real row
/// pitch is ≈ `font_size * 1.16` (measured from a VIKE_SHOT capture: 45 rows over ~680 logical
/// px at font 13 — the first-guess 1.5 factor left a ~22%-of-panel dead gap). A slight overshoot
/// is harmless (the widget's own scroll region just reaches the panel bottom); the result is
/// clamped to `8..=400` so a degenerate available height never yields a zero-row editor.
pub(crate) fn rows_for_height(avail_height: f32, font_size: f32) -> usize {
    let row_h = font_size * 1.16;
    ((avail_height / row_h) as usize).clamp(8, 400)
}

impl EditorPane {
    /// Render at the default fixed height (20 rows) — kept for callers without a height budget.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.ui_sized(ui, 20.0 * studio::EDITOR_FONT_SIZE * 1.16);
    }

    /// Render the editor sized to fill `avail_height` pixels (the Studio's editor panel passes
    /// its remaining panel height so the code area reaches the bottom instead of stopping at a
    /// fixed 20 rows). Returns the code field's response, for the caller to name it.
    pub fn ui_sized(&mut self, ui: &mut egui::Ui, avail_height: f32) -> egui::Response {
        // NOTE: egui_code_editor 0.3.7's `show` signature is `(ui, text, syntax)` — text before
        // syntax. The crate's own top-of-lib.rs doc example (`.show(ui, &self.syntax,
        // &mut self.code)`) is stale for this version and does NOT match the compiled API.
        CodeEditor::default()
            .id_source("vike-studio-editor")
            .with_rows(rows_for_height(avail_height, studio::EDITOR_FONT_SIZE))
            .with_fontsize(studio::EDITOR_FONT_SIZE)
            .with_theme(ColorTheme::GRUVBOX)
            .with_numlines(true)
            .show(ui, &mut self.source, &rhai_syntax())
            .0
            .response
            .response
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

    /// Both templates are LONG-ONLY: a short on a spot pair is refused whole by the spot law, so a
    /// template that ever goes short closes nothing there. The run that proves it trades on a spot
    /// pair is `crates/vike-studio/src/studio_tests/worker_polling.rs`'s
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
    #[test]
    fn rhai_syntax_is_defined() {
        let _ = syntax::rhai_syntax(); // constructs without panic
    }

    #[test]
    fn compile_status_is_ok_for_the_default_sma_cross_script() {
        assert_eq!(compile_status(&EditorPane::default().source), Ok(()));
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
