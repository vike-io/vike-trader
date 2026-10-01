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
//!   resting depth (Up / Down in the market colours), rest-a-limit on click, inline ✕ to cancel.
//! - [`header`] — the Price-to-Beat header: reference vs live spot + signed Δ, Up/Down odds and a
//!   big escalating MM:SS countdown, in one glance.
//! - [`ticket`] — the one-click ticket: quick-size chips, the Armed switch and two-up
//!   BUY UP / BUY DOWN buttons with a fee-inclusive payout preview.
//!
//! Every colour and size the widgets draw comes from `vike_ui_theme`: the installed appearance's
//! tokens (`components::Tokens`), its status colours and the component kit. The cockpit follows the
//! theme, the market colours, density and text size like every other screen, and paints no colour
//! of its own.
//!
//! Every widget shares the data-in / actions-out seam and a pure, unit-tested core beneath the
//! egui paint. All four modules expose a `draw`; the re-exports below rename each to a
//! widget-specific `draw_*` so a caller can `use vike_cockpit::*` without a collision.

pub mod chain;
pub mod header;
pub mod ladder;
pub mod ticket;

/// A Display or Hero number: JetBrains Mono SemiBold at `role`'s size on the installed scale. That
/// is the weight the design session's type-scale page gave both roles (the migration plan's
/// decision 3). `vike_ui_theme::font::mono_semibold` is the same family for a `RichText`; a painter
/// needs the `FontId`.
pub(crate) fn number_font(
    t: &vike_ui_theme::components::Tokens,
    role: vike_ui_theme::type_scale::TextRole,
) -> egui::FontId {
    egui::FontId::new(
        t.text.px(role),
        egui::FontFamily::Name(vike_ui_theme::fonts::MONO_SEMIBOLD.into()),
    )
}

pub use chain::{
    ChainRailAction, ChainRailInputs, ChainRailState, Window, WindowCard, chain_windows,
    draw as draw_chain_rail, fmt_countdown, window_close_ms, window_open_ms,
};
pub use header::{PtbInputs, draw as draw_ptb_header, ptb_delta};
pub use ladder::{
    ProbLadderAction, ProbLadderInputs, ProbLadderState, ProbLevel, ProbMarker, ProbOrder,
    draw as draw_ladder,
};
pub use ticket::{TicketAction, TicketInputs, TicketState, draw as draw_ticket, payout_preview};
