//! Real data for the tool windows, fetched on background threads. **Every one of them asks the
//! backend's DATAHUB; none opens a socket to a provider or a venue** (decision 0128 for the Options
//! tool, and the tool-feeds record for the rest):
//!   News     → the datahub's `NewsFeed` snapshot (it polls the RSS feeds)
//!   Calendar → the datahub's `EconomicCalendar` (ForexFactory) and `EquityCalendar` (Finnhub
//!              earnings, FMP dividends, Nasdaq IPOs) snapshots; [`ToolFeedsLink`] is what the News
//!              and Calendar windows record their interest on, and `tool_feeds`' module doc carries
//!              the cadence, the held connection and the no-fallback rule.
//!   Options  → the `OptionsChain` verb for the chains and the deribit `VenueCatalog` listing for
//!              the [`vike_options::OptionIndex`] the Greeks tool looks names up in.
//!              [`OptionsLink`] is what the windows record their picks on; `options`' module doc
//!              carries the cadence, the held connection and the no-fallback rule.
//!              (The Deribit `options_provider` that stood here left with decision 0128: the
//!              provider and both feeds are the datahub's options lane now.)
//! (Data Manager reads the app's own live-feed state, not here.)
//! Also home to [`ToolView`] — the per-window VIEW state for these tools.
//!
//! This module reads NEITHER the environment nor the credential store: the provider keys live on
//! the datahub's box, and the datahub dial arrives as a closure resolved by the app root and
//! handed to [`spawn_tool_fetchers`].

mod calendar;
mod fetchers;
mod news;
pub(crate) mod options;
mod options_wire;
mod tool_feeds;
mod tool_feeds_wire;
mod view;

pub use calendar::{CalEvent, Importance, currency_country};
pub use fetchers::spawn_tool_fetchers;
pub use news::{NEWS_CATEGORIES, NEWS_MARKETS, classify_news};
pub use options::{OptionsLink, OptionsPoller, options_selection};
pub use tool_feeds::{FeedInterest, FeedsClock, ToolFeedsLink, ToolFeedsPoller};
pub use view::{FeedHealth, OptOrderTicket, ToolData, ToolView};
