//! The Studio's PANES: one module per surface `StudioState` (`crate::studio`) puts on screen.
//!
//! * `editor` — the Rhai code editor (`editor/syntax.rs` is its highlighter);
//! * `picker` — the data-slice picker;
//! * `data_browser`, `indicators`, `templates_gallery` — the Data, Indicators and Templates tool
//!   panes;
//! * `saved` — saved strategies, and the comparison of several of them over the picked slice;
//! * `chat` — the AI copilot (`chat/connect.rs` generates its "Connect to Claude" command, behind
//!   the non-default `mcp` feature);
//! * `research` — the user's studies and the runs they left behind, plus the surface a study's
//!   result renders on;
//! * `results` — the tabbed backtest-result surface.
//!
//! **What belongs here:** a surface's own state, how it draws itself, and the pure helpers its
//! tests reach (row builders, summaries, diffs). **What does not:** deciding which pane is showing,
//! starting a worker, or folding a worker's answer back in — that is `StudioState`'s, in
//! `crate::studio`. A pane takes one thing from the shell in code today, the shared title strip
//! `crate::studio::pane_header`.
//!
//! A pane may name `crate::backend`, which sits below it (that module's doc carries the direction
//! rule), but none does in code today: the shell calls `backend` and hands each pane the answer, so
//! the only mentions are the docs of `picker` and `data_browser` pointing at `backend::catalog`.
//!
//! A pane's child module (`chat/connect.rs`, `editor/syntax.rs`) is private to that pane: nothing
//! else names it. The nine panes themselves are `pub(crate)` rather than private because `lib.rs`
//! re-exports the public surface from them (`pub use panes::saved::{...}` and its siblings) and the
//! shell imports from them, and neither can name a module that stays private to `panes`.

pub(crate) mod chat;
pub(crate) mod data_browser;
pub(crate) mod editor;
pub(crate) mod indicators;
pub(crate) mod picker;
pub(crate) mod research;
pub(crate) mod results;
pub(crate) mod saved;
pub(crate) mod templates_gallery;
