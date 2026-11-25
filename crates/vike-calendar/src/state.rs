//! The Calendar window's per-window VIEW state.

/// The Calendar window's view state, one per window (`vike-app-core` holds it as
/// `WindowView::calendar`): the page, the selected day card, the High-only filter and the week shown.
/// VIEW state, not fetched data: the rows are the poll's.
///
/// ⚠ `Default` is written by hand: `selected_day` starts at `-1` (no card), and a derive would
/// select Monday.
#[derive(Clone)]
pub struct CalendarState {
    pub page: u8,         // 0=Economic 1=Earnings 2=Dividends 3=IPO
    pub selected_day: i8, // -1 = none
    pub high_only: bool,
    pub weeks_from_now: i32, // the week shown: 0 = this week, -1 = the one before, 1 = the next
}

impl Default for CalendarState {
    fn default() -> Self {
        Self { page: 0, selected_day: -1, high_only: false, weeks_from_now: 0 }
    }
}
