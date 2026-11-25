//! **The News window**: the filterable headline list and its reader pane ([`news_tool_content`]),
//! the window's view state ([`NewsView`]), the topic classifier behind the Category filter and the
//! reader's chips (`category`'s `classify_news`, `NEWS_CATEGORIES`, `NEWS_MARKETS`), Follow chart's words
//! for the focused chart's asset ([`NewsFollowCache`]) and the bundled source icons
//! ([`NEWS_LOGO_PNGS`]).
//!
//! **Nothing here fetches.** The headlines are the datahub's
//! (`vike_datahub_client::tool_feeds::NewsItem`), asked for by the ONE datahub poll
//! (`vike_datahub_client::tool_feeds::poll`). The window records `DatahubPollInterest::News` on the
//! poll's link every frame it draws, so the poll thread keeps asking `NewsFeed`, and its Refresh
//! button wakes the poll now. Everything it reads arrives as [`NewsInputs`]: `vike-app-core` (one
//! layer up) builds them with `WindowToolContext::news_inputs` and holds the [`NewsView`] as
//! `WindowView::news`. The shell decodes [`NEWS_LOGO_PNGS`] once with its egui context
//! (`App::news_logos`) and opens [`NewsView::open_url`] after the frame.

mod category;
mod follow;
mod logo;
mod view;
mod window;

pub use follow::NewsFollowCache;
pub use logo::NEWS_LOGO_PNGS;
pub use view::NewsView;
pub use window::{NewsInputs, news_tool_content};
