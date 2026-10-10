//! The Studio's BACKEND: how the shell reaches what is not a widget — the compute daemon, the
//! strategy builder, and the store's catalog. Every entry point is either a pure function or a
//! `spawn_*` that runs the call on a worker thread and hands back the `Receiver` that
//! `StudioState::poll` folds in. Nothing here draws: `catalog` takes an `egui::Context` only to
//! request a repaint after its answer is sent.
//!
//! * `remote` — the run backend (`Backend::{Remote, Named}`), the `spawn_*_remote` dispatches, the
//!   engine <-> wire conversions, the compute key and its one resolver, and the named-run roster;
//! * `plugin_build` — the BUILDER client. A third service with its own domain, protocol version and
//!   key, so a module beside `remote` rather than inside it; its own doc carries the argument;
//! * `catalog` — the off-thread catalog walk behind the toolbar's Refresh button (the walk, its one
//!   message, and the spawn).
//!
//! **What belongs here:** a call out of the process (or off the paint thread), the types that
//! describe it, and the pure conversions around it. **What does not:** anything a pane draws, and
//! the decision of WHEN to call — `StudioState`'s dispatchers and `poll` (`crate::studio`) own both.
//!
//! **Direction.** `backend` sits below `crate::panes`: a pane may name it, it may not name a pane.
//! Measured when this layout was cut, neither direction exists in code. No module here names a
//! pane in production code (`catalog.rs` mentions `SlicePicker` and `DataBrowserPane` in its docs
//! and builds them in its own tests), and no pane calls into `backend` either — the shell calls
//! both and carries each worker's answer to the pane that renders it. The docs of `picker` and
//! `data_browser` do point at `catalog`.
//!
//! The three modules are `pub(crate)`, not private, for the reason `crate::panes` gives:
//! `lib.rs` re-exports their public surface and the shell imports from them.

pub(crate) mod catalog;
// Flow step 2 of the runtime-plugin design: the BUILDER client. Its own doc carries the argument
// for why it is not inside `remote`.
pub(crate) mod plugin_build;
pub(crate) mod remote;
