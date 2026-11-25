//! The News window's per-window VIEW state: the selection, the filters, the reader, and the one OUT
//! slot the shell drains.

use std::collections::HashSet;

/// The News window's view state, one per window (`vike-app-core` holds it as `WindowView::news`). VIEW
/// state, not fetched data: the headlines are the poll's.
///
/// ⚠ `Default` is written by hand: `follow` and `reader_open` start `true`, and a derive would turn
/// both off.
#[derive(Clone)]
pub struct NewsView {
    pub selection: usize,
    pub query: String,
    pub markets: HashSet<String>,
    pub(crate) categories: HashSet<String>,
    pub providers: HashSet<String>,
    /// The "Follow chart" toggle: on, the list keeps the headlines that name the focused chart's
    /// asset ([`crate::NewsInputs::chart`]).
    pub follow: bool,
    /// The words that asset is named by, read through the catalog once per chart.
    pub follow_words: crate::NewsFollowCache,
    pub(crate) reader_open: bool,
    pub open_url: Option<String>, // OUT: app opens this URL after the frame
}

impl Default for NewsView {
    fn default() -> Self {
        Self {
            selection: 0,
            query: String::new(),
            markets: HashSet::new(),
            categories: HashSet::new(),
            providers: HashSet::new(),
            follow: true,
            follow_words: crate::NewsFollowCache::default(),
            reader_open: true,
            open_url: None,
        }
    }
}
