//! The helpers the crate's unit tests share (`#[cfg(test)]` only): one headless frame that clears its
//! texture uploads and asserts its geometry ([`run_frame`]), the coverage, key and tree fixtures, the
//! painted warning icon, and the design-system harness (`installed`, `shapes_of`, `fills`, `texts`).

use vike_data::SeriesCoverage;
use vike_model::StoreKindId;
use vike_ui_theme::appearance::{self, ThemeAppearance};

use crate::inventory::{DataManagerInventorySeriesKey, DataManagerInventoryVenue};

/// One headless frame whose texture uploads are deliberately discarded, and whose geometry is
/// asserted sane.
///
/// egui 0.36 made `TexturesDelta` PANIC on drop while it still holds unapplied deltas —
/// "Deltas need to be handled. If you want to drop this intentionally call `clear` before
/// dropping." Every harness below renders into no backend at all: they run a frame to read a
/// widget rect, an action, or the emitted shapes. Discarding the uploads IS the intent, so it
/// is stated once here rather than at eight call sites.
///
/// Every frame also goes through `vike_ui_theme::frame_sanity::assert_frame_sane` — the ONE
/// shared geometry invariant, behind that crate's `test-support` feature. It is stated here
/// (once) for the same reason the clear is: every test that renders a frame gets it, including
/// the ones added after this comment.
///
/// ⚠ The assertion runs AFTER the clear, and the order is load-bearing. `clear` touches
/// `textures_delta` and never `shapes`, so it cannot hide anything the assertion reads — while
/// asserting first leaves the deltas unapplied, so dropping the frame during the assertion's
/// unwind panics a SECOND time in the destructor and the process ABORTS with `SIGABRT`,
/// printing a backtrace instead of the coordinate that was wrong.
pub(crate) fn run_frame(
    context: &egui::Context,
    raw: egui::RawInput,
    f: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut out = context.run_ui(raw, f);
    out.textures_delta.clear();
    vike_ui_theme::frame_sanity::assert_frame_sane(&out);
    out
}

pub(crate) fn coverage(rows: u64, bytes: u64, first: i64, last: i64) -> SeriesCoverage {
    SeriesCoverage { first_timestamp: first, last_timestamp: last, rows, bytes, parts: 1, dates: 1 }
}

pub(crate) fn key(venue: &str, symbol: &str) -> DataManagerInventorySeriesKey {
    DataManagerInventorySeriesKey {
        venue: venue.into(),
        symbol: symbol.into(),
        kind: StoreKindId::Bar,
        interval: Some("1m".into()),
        grouped: false,
        source: None,
    }
}

/// A tree built via `build_inventory_tree` (real rollups, not the zero-filled `venue_fixture` totals
/// above) so `global_span`/`is_stale` behave exactly as they do in production. Whole-tree span
/// is `[0, 1000]` (from okx's `BTC-USDT` at 1000): binance/FRESH ends at 990 (1% behind, not
/// stale), binance/STALE ends at 100 (90% behind, stale), okx/BTC-USDT ends at 1000 (not
/// stale).
pub(crate) fn tree_fixture() -> Vec<DataManagerInventoryVenue> {
    use vike_data::SeriesId;
    let sid = |venue: &str, symbol: &str| {
        SeriesId::per_symbol(StoreKindId::Bar, venue, symbol, Some("1m".into()))
    };
    let inventory = vec![
        (sid("binance", "FRESH"), coverage(10, 1, 0, 990)),
        (sid("binance", "STALE"), coverage(10, 1, 0, 100)),
        (sid("okx", "BTC-USDT"), coverage(5, 1, 0, 1000)),
    ];
    crate::inventory::build_inventory_tree(inventory)
}

/// The warning icon as painted text — what the partial column's header and markers draw.
pub(crate) fn warning() -> String {
    vike_ui_theme::icons::WARNING.accessible_label("")
}

/// A context with `a` installed and one empty pass behind it, so the fonts are live.
pub(crate) fn installed(a: &ThemeAppearance) -> egui::Context {
    let context = egui::Context::default();
    appearance::install(&context, a);
    let _ = run_frame(&context, egui::RawInput::default(), |_| {});
    context
}

fn wide() -> egui::RawInput {
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
    egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() }
}

/// Every shape one pass of `add` paints.
pub(crate) fn shapes_of(
    context: &egui::Context,
    add: impl FnMut(&mut egui::Ui),
) -> Vec<egui::Shape> {
    vike_ui_theme::painted::flatten(run_frame(context, wide(), add).shapes)
}

pub(crate) fn fills(shapes: &[egui::Shape]) -> Vec<egui::Color32> {
    shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) => Some(r.fill),
            _ => None,
        })
        .collect()
}

pub(crate) fn texts(shapes: &[egui::Shape]) -> Vec<String> {
    shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}
