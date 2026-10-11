//! Real data for the tool windows, fetched on background threads:
//!   News     → Cointelegraph RSS (ureq, rustls)
//!   Calendar → ForexFactory weekly JSON (ureq, rustls)
//!   Options  → the backend's DATAHUB, never the venue: the `OptionsChain` verb for the chains and
//!              the deribit `VenueCatalog` listing for the [`vike_options::OptionIndex`] the Greeks
//!              tool looks names up in. [`OptionsLink`] is what the windows record their picks on;
//!              `options`' module doc carries the cadence, the held connection and the no-fallback
//!              rule.
//!              (The Deribit `options_provider` that stood here left with decision 0128: the
//!              provider and both feeds are the datahub's options lane now.)
//! (Data Manager reads the app's own live-feed state, not here.)
//! Also home to [`ToolView`] — the per-window VIEW state for these tools.
//!
//! This module reads NEITHER the environment nor the credential store: the third-party
//! data-provider keys arrive as [`ToolApiKeys`] and the datahub dial as a closure, both resolved
//! by the app root and handed to [`spawn_tool_fetchers`].

mod api_keys;
mod calendar;
mod fetchers;
mod news;
pub(crate) mod options;
mod options_wire;
mod view;

pub use api_keys::{FINNHUB_API_KEY, FMP_API_KEY, ToolApiKeys};
pub use calendar::{CalEvent, DividendRow, EarningsRow, Importance, IpoRow, currency_country};
pub use fetchers::spawn_tool_fetchers;
pub use news::{NEWS_CATEGORIES, NEWS_MARKETS, NewsItem, classify_news};
pub use options::{OptionsLink, OptionsPoller, options_selection};
pub use view::{OptOrderTicket, ToolData, ToolView};
