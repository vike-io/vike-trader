//! The Studio's BACKEND: how the shell reaches what is not a widget — the backtesthub (which also
//! forwards a Build to the strategy builder) and the store's catalog. Every entry point is either a pure function or a
//! `spawn_*` that runs the call on a worker thread and hands back the `Receiver` that
//! `StudioState::poll` folds in. Nothing here draws: `catalog` takes an `egui::Context` only to
//! request a repaint after its answer is sent.
//!
//! * `backtesthub` — the run backend (`StudioRunBackend::{Source, Named}`), the `spawn_backtesthub_*`
//!   dispatches, the engine <-> wire conversions, the device-key refusal, and the named-run roster;
//!   its `build` child holds a Build's pre-dial refusals, since a Build is a backtesthub dispatch too
//!   (no person holds the builder's key);
//! * `catalog` — the off-thread catalog walk behind the toolbar's Refresh button (the walk, its one
//!   message, and the spawn);
//! * `csv_upload` — the Data pane's upload of a user's CSV to the datahub, on the device key
//!   (decision 0272): the file read, the send and its progress, off the paint thread.
//!
//! **What belongs here:** a call out of the process (or off the paint thread), the types that
//! describe it, and the pure conversions around it. **What does not:** anything a pane draws, and
//! the decision of WHEN to call — `StudioState`'s dispatchers and `poll` (`crate::studio`) own both.
//!
//! **Direction.** `backend` sits below `crate::panes`: a pane may name it, it may not name a pane.
//! Measured when this layout was cut, neither direction exists in code. No module here names a
//! pane in production code (`catalog.rs` mentions `StudioSlicePicker` and `StudioDataBrowserPane` in its docs
//! and builds them in its own tests), and no pane calls into `backend` either — the shell calls
//! both and carries each worker's answer to the pane that renders it. The docs of `slice_picker` and
//! `data_browser` do point at `catalog`.
//!
//! The modules are `pub(crate)`, not private, for the reason `crate::panes` gives:
//! `lib.rs` re-exports their public surface and the shell imports from them.

pub(crate) mod backtesthub;
pub(crate) mod catalog;
pub(crate) mod csv_upload;
