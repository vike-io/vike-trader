//! **The Calendar window**: one window with four pages, the ForexFactory economic week and the
//! week's Earnings, Dividends and IPOs ([`calendar_window`]), over a 7-day day strip in the app's
//! display time zone; the window's view state ([`CalendarState`]); the economic row the views draw
//! (`economic`'s `CalendarEconomicEventView`, [`CalendarEconomicImportance`]), the currency-to-country map behind
//! the flag column (`economic`'s `currency_country`) and the bundled flag PNGs ([`FLAG_PNGS`]).
//!
//! **Nothing here fetches.** The rows are the datahub's, held RAW by the ONE datahub poll
//! (`vike_datahub_client::tool_feeds::poll::DatahubPollSnapshot`): the window records the week it
//! shows and `DatahubPollInterest::Calendar` on the poll's link every frame it draws, so the poll
//! thread keeps asking `EconomicCalendar` and `EquityCalendar` for that week, and derives every day,
//! clock time and country where it draws, from the raw instant, in the zone it is handed (one
//! `CalendarWeek` for the range, the cards and the list). `vike-app-core` (one layer up) hands it
//! the snapshot, the decoded flags, the zone and the [`CalendarState`] it holds as
//! `WindowView::calendar` (`calendar_tool_content_at`); the desktop decodes [`FLAG_PNGS`] once with its
//! egui context (`App::load_flag_images`).

mod economic;
mod equity;
mod state;
mod week;
mod window;

pub use economic::CalendarEconomicImportance;
pub use economic::flag::FLAG_PNGS;
pub use state::CalendarState;
pub use window::calendar_window;
