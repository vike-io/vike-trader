//! The Polymarket scalp-cockpit widgets — reusable egui panels for the rolling up/down markets a
//! scalper trades. RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! Chart-engine-free by construction: pure `egui` painting with no eframe/wgpu, no
//! egui_plot / ChartState / indicators, and no dependency on the execution core. That is what
//! keeps the crate in CI (same shape as vike-chart / vike-panels). The widgets are data-in /
//! actions-out: a stateless-render `draw` over caller-owned view state + borrowed per-frame
//! inputs, returning neutral actions the app maps to its own commands — the same seam the DOM
//! ladder uses.
//!
//! The Phase-1 widget set:
//! - [`chain`] — the window-chain rail: Polymarket's 5-minute up/down markets are a deterministic
//!   chain of `window_ts` buckets, so a scalper sees the current plus the next few windows with
//!   live MM:SS countdowns in one persistent strip.
//! - [`ladder`] — the probability DOM ladder: a 0–1 (¢) click-trade ladder over one market's
//!   resting depth (Up green / Down red), rest-a-limit on click, inline ✕ to cancel.
//! - [`header`] — the Price-to-Beat header: reference vs live spot + signed Δ, Up/Down odds and a
//!   big escalating MM:SS countdown, in one glance.
//! - [`ticket`] — the one-click ticket: quick-size chips, an arm/confirm safety toggle and two-up
//!   BUY UP / BUY DOWN buttons with a fee-inclusive payout preview.
//!
//! Every widget shares the data-in / actions-out seam and a pure, unit-tested core beneath the
//! egui paint. All four modules expose a `draw`; the re-exports below rename each to a
//! widget-specific `draw_*` so a caller can `use vike_cockpit::*` without a collision.

pub mod chain;
pub mod header;
pub mod ladder;
pub mod ticket;

pub use chain::{
    chain_windows, draw as draw_chain_rail, fmt_countdown, window_close_ms, window_open_ms,
    ChainRailAction, ChainRailInputs, ChainRailState, Window, WindowCard,
};
pub use header::{draw as draw_ptb_header, ptb_delta, PtbInputs};
pub use ladder::{
    draw as draw_ladder, ProbLadderAction, ProbLadderInputs, ProbLadderState, ProbLevel,
    ProbMarker, ProbOrder,
};
pub use ticket::{draw as draw_ticket, payout_preview, TicketAction, TicketInputs, TicketState};
