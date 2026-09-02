//! Syntax highlighting for the Rhai editor. Rhai's surface (fn/let/const/if/else/while/return, //
//! comments, strings, numbers) is a subset of Rust's, so the built-in Rust syntax highlights it
//! well for the MVP. A bespoke Rhai `Syntax` (coloring the host verbs sma/buy/... specially) is a
//! documented follow-up.
pub fn rhai_syntax() -> egui_code_editor::Syntax {
    egui_code_editor::Syntax::rust()
}
