//! The Polymarket scalp-cockpit widgets — reusable egui panels for the rolling up/down markets a
//! scalper trades. RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! Chart-engine-free by construction: pure `egui` painting with no eframe/wgpu and no
//! egui_plot / ChartState / indicators. That is what keeps the crate in CI (same shape as
//! vike-chart). The widgets name no execution type: [`CockpitCommand`], the intent the dispatcher
//! ([`crate::dispatch`]) turns into an order, is plain data. The widgets are data-in /
//! actions-out: a stateless-render `draw` over caller-owned view state + borrowed per-frame
//! inputs, returning neutral actions the app maps to its own commands — the same seam the DOM
//! ladder uses.
//!
//! The Phase-1 widget set:
//! - [`chain_rail`] — the window-chain rail: Polymarket's 5-minute up/down markets are a deterministic
//!   chain of `window_ts` buckets ([`up_down_window`], the chain's pure math), so a scalper sees the
//!   current plus the next few windows with live MM:SS countdowns in one persistent strip.
//! - [`probability_ladder`] — the probability DOM ladder: a 0–1 (¢) click-trade ladder over one
//!   market's resting depth (Up / Down in the market colours), rest-a-limit on click, inline ✕ to
//!   cancel; [`bucket_probability_levels`] buckets a book onto its rungs.
//! - [`price_to_beat`] — the Price-to-Beat header: reference vs live spot + signed Δ, Up/Down odds
//!   and a big escalating MM:SS countdown, in one glance.
//! - [`ticket`] — the one-click ticket: quick-size chips, the Armed switch and two-up
//!   BUY UP / BUY DOWN buttons with a fee-inclusive payout preview.
//!
//! Every colour and size the widgets draw comes from `vike_ui_theme`: the installed appearance's
//! tokens (`components::ThemeTokens`), its status colours and the component kit. The cockpit follows the
//! theme, the market colours, density and text size like every other screen, and paints no colour
//! of its own.
//!
//! Every widget shares the data-in / actions-out seam and a pure, unit-tested core beneath the
//! egui paint. Each module names its draw once, after its widget (`draw_chain_rail`,
//! `draw_price_to_beat`, `draw_probability_ladder`, `draw_cockpit_ticket`), so a caller can
//! `use vike_order_entry::cockpit::*` without a collision and without an alias.

pub mod chain_rail;
pub mod price_to_beat;
pub mod probability_ladder;
pub mod ticket;
pub mod up_down_window;

pub use chain_rail::{
    CockpitChainRailAction, CockpitChainRailInputs, CockpitChainRailState, draw_chain_rail,
};
pub use price_to_beat::{CockpitPriceToBeatInputs, draw_price_to_beat};
pub use probability_ladder::{
    CockpitProbabilityLadderAction, CockpitProbabilityLadderInputs, CockpitProbabilityLadderLevel,
    CockpitProbabilityLadderMarker, CockpitProbabilityLadderOrder, CockpitProbabilityLadderState,
    bucket_probability_levels, draw_probability_ladder, live_rungs, rung_of,
};
pub use ticket::{
    CockpitTicketAction, CockpitTicketInputs, CockpitTicketState, draw_cockpit_ticket,
};
pub use up_down_window::{CockpitUpDownWindowCard, chain_windows, window_close_ms, window_open_ms};

/// One resolved Polymarket cockpit order intent, translated from a cockpit action at the
/// window-loop drain (where the ticket stake + live book are in scope) and folded onto the command
/// lane after the loop — the cockpit twin of the Trade window's `(TradeWindowAddress, TradeWindowAction)` drain.
#[derive(Debug, Clone, PartialEq)]
pub enum CockpitCommand {
    /// Submit an order for `token` (venue `"polymarket"`): a market taker (`price == None`) or a
    /// resting limit (`price == Some`). `side` is +1 buy YES / −1 sell YES.
    Submit { token: String, side: i32, price: Option<f64>, quantity: f64 },
    /// Cancel a resting order by order-client-id (the ladder's inline ✕).
    Cancel(String),
}
