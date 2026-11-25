//! The order-entry egui widgets and the one dispatcher behind them. Chart-engine-free by
//! construction: no egui_plot / ChartState / indicators.
//!
//! - [`trade_window`] — the Trade window: the price ladder ([`trade_window::ladder`], whose
//!   bookless words are [`trade_window::ladder::absence`]'s), the tick chart beside it
//!   ([`trade_window::tick_chart`]), the order ticket ([`trade_window::ticket`]), the instrument
//!   bar ([`trade_window::instrument_bar`]) and the status line ([`trade_window::status_line`]) in
//!   one window, and the address its orders go to ([`trade_window::address`]). It replaced the
//!   DOM (depth-of-market) ladder that lived here before it. It is pure `egui` painting over
//!   `vike_marketdata::{L2Book, L2BookLevel}` and `vike_venues::VenueCapabilities` — data in,
//!   neutral actions out, the same seam the chart uses. It paints from the design system —
//!   `vike_ui_theme::components::ThemeTokens` and the component kit — so it follows the installed
//!   theme, market colours, text size and density.
//! - [`cockpit`] — the Polymarket scalp-cockpit widgets (decision 0157): the window-chain rail
//!   ([`cockpit::chain_rail`]) over the up/down window chain ([`cockpit::up_down_window`]), the
//!   Price-to-Beat header ([`cockpit::price_to_beat`]), the probability ladder
//!   ([`cockpit::probability_ladder`]) and the one-click ticket ([`cockpit::ticket`]).
//! - [`option_chain`] — the Options window's chain grid (from vike-chart), which prices nothing
//!   itself: its column model is `vike-options'`; and the order ticket a chain click opens
//!   ([`option_chain::ticket`]).
//! - [`dispatch`] — where their intents become orders: the ONE place a drained UI intent becomes a
//!   `vike_execution::Command` and meets the local preview cap. The widgets themselves name no
//!   execution type; the dispatcher reads the execution READ model (`CoreSnapshot`, `VenueBlock`)
//!   and emits `Command`s, so the crate takes `vike-execution` with `default-features = false` (no
//!   async runtime), plus `vike-catalog`, `vike-tradehub-client` and `vike-venues` (`LANE_TWINS`
//!   among its tables) for the rules it applies. Sending is the caller's.

#![warn(unreachable_pub)]

pub mod cockpit;
pub mod dispatch;
pub mod option_chain;
pub mod trade_window;
