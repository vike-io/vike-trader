//! The shared store double the Studio's two disclosure tests refuse a catalog scan with.
//!
//! MOVED here, not copied. It grew inside `crates/vike-studio/tests/picker_disclosure.rs`, which
//! drives `SlicePicker::ui` alone; `crates/vike-studio/tests/studio_shell_render.rs` then needed
//! the same refusal in order to drive the SHELL's central-panel placeholder rather than the
//! picker's toolbar, and this repo's standing rule is that a law spelled twice is the defect. An
//! integration test cannot `use` another integration test — each `tests/*.rs` is its own crate —
//! so `tests/common/mod.rs`, included by both with `mod common;`, is the one place it can live. (A
//! `tests/` SUBDIRECTORY module is not itself a test target, so this file builds no extra binary
//! and runs no extra test.) The precedent is `crates/vike-chart/tests/common/mod.rs`, which moved
//! that crate's `Case` harness out of `draw_characterization.rs` the day a second file needed it.
//!
//! ⚠ **The two tests split the SURFACE, not the fixture.** `picker_disclosure.rs` asserts what the
//! TOOLBAR shows (`SlicePicker::ui`'s combo text and its `⚠ scan failed` chip);
//! `studio_shell_render.rs` asserts what the CENTRAL PANEL shows, which is a second rendering of
//! the same fact and a second place it was once rendered as a lie —
//! `crates/vike-studio/src/studio.rs`'s `empty_state` says so in its own doc, and until that file
//! existed nothing reached it.
//!
//! ⚠ **This double overrides `list_series` and NOTHING else**, which is the whole failure it
//! models: the picker's walk is ONE call of that catalog verb — `vike_studio_core::series_lists`,
//! which folds `bar_series`/`tick_series`/`depth_only_series`'s three filters over a single
//! `list_series` answer (it used to be three calls that happened to agree) — so the picker treats
//! a refusal as one indivisible outcome rather than listing whichever part answered, by
//! construction rather than by three impls staying in step.
//!
//! ⚠ ...and what that leaves the SHELL test, which reads more of the store than the picker does.
//! `crates/vike-data/src/hist.rs`'s `inventory` is a DEFAULTED trait verb and is not overridden
//! here, and that default REFUSES — deliberately, its own doc says, so no caller is handed a
//! fabricated empty inventory beside a refused series list. So over this double the Studio's
//! Data pane reads as a FAILED store too, but with the DEFAULT's words ("this store cannot
//! enumerate its inventory"), not [`REASON`]: an assertion that [`REASON`] reached the screen can
//! only have been satisfied by the picker's half, which is what the shell test claims and all it
//! claims. It says nothing about the Data pane on purpose — an honest half, not an unstated gap.
//! (This paragraph used to say the default answered an empty `Ok`; it has not since the catalog
//! pair moved to refusing defaults, and `crates/vike-studio/src/catalog.rs`'s module doc names
//! the stores where the two verbs genuinely CAN disagree.)
//!
//! ⚠ **The posed-state fixture family lives here too, and it MOVED — never copied.**
//! [`empty_store`] / [`seeded_store`] / [`qa_name`] / [`state`] grew inside
//! `crates/vike-studio/tests/studio_shell_render.rs`, and
//! `crates/vike-studio/tests/tessellation_goldens.rs` then became their second consumer: the
//! goldens record the SAME posed shell the a11y suite drives, so both files need the same
//! workspace-scrubbed `StudioState` over the same store shapes, and a fixture spelled twice
//! drifts exactly the way a law spelled twice does. [`state`]'s field scrub is the READ half of
//! the workspace hazard the render suite's module doc walks through — it is what makes a frame
//! (and therefore a frame RECORD) machine-independent instead of a copy of whichever
//! `studio_workspace.json` the box last wrote — and its qa-name round-trip assert guards the
//! WRITE half: `crates/vike-studio/src/studio.rs`'s `new_with_qa` silently IGNORES a name it
//! does not recognise, and a session that fell through that way restores the developer's real
//! workspace AND leaves `qa_workspace_readonly` false, so the first rendered frame could persist
//! the pose over the developer's own file.

// Both including binaries use a SUBSET of this module, and each compiles it separately — so an
// item only one of them names is dead code in the other, and `-D warnings` would fail on a helper
// that IS used, just not by the binary being compiled. The standard `tests/common` idiom; it
// suppresses nothing about the crate under test.
#![allow(dead_code)]

use std::sync::Arc;

use vike_data::{
    DataError, DataFusionHist, ExecFillRow, ExecOrderRow, HistStore, SeriesId, TsRange,
};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};
use vike_studio::{ChatApiKeys, EditorPane, RightTab, StoreHandle, StudioState};

/// The planted reason, so an assertion can prove the STORE's own words reach the operator rather
/// than a generic message the picker made up.
pub const REASON: &str = "datahub <host>:7878: connection refused";

/// A `HistStore` whose CATALOG verb refuses — the shape of a `RemoteHistStore` talking to a server
/// that went away. Every other verb is off the picker's walk and says so if reached; the walk
/// touches `list_series` only, so a working stub here would be dead code posing as coverage.
pub struct RefusingCatalogStore;

fn off_walk(verb: &str) -> DataError {
    DataError::Query(format!("RefusingCatalogStore: {verb} is not part of the picker's walk"))
}

impl HistStore for RefusingCatalogStore {
    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        Err(DataError::Query(REASON.to_string()))
    }

    // ---- everything below is unreachable for the picker's walk ----
    fn load_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        Err(off_walk("load_bars"))
    }
    fn scan_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        Err(off_walk("scan_quotes"))
    }
    fn scan_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        Err(off_walk("scan_trades"))
    }
    fn append_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _bars: &[Bar],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("append_bars"))
    }
    fn append_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[QuoteTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("append_quotes"))
    }
    fn append_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[TradeTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("append_trades"))
    }
    fn append_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _updates: &[BookUpdate],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("append_book_updates"))
    }
    fn scan_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Err(off_walk("scan_book_updates"))
    }
    fn append_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[(i64, SymbolProperties)],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("append_symbol_properties"))
    }
    fn scan_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Err(off_walk("scan_symbol_properties"))
    }
    fn append_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[EquitySample],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("append_equity"))
    }
    fn scan_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        Err(off_walk("scan_equity"))
    }
    fn append_exec_fills(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecFillRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("append_exec_fills"))
    }
    fn scan_exec_fills(&self, _venue: &str, _symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Err(off_walk("scan_exec_fills"))
    }
    fn append_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecOrderRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("append_exec_orders"))
    }
    fn scan_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, DataError> {
        Err(off_walk("scan_exec_orders"))
    }
    fn resample_quotes_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("resample_quotes_to_bars"))
    }
    fn resample_trades_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(off_walk("resample_trades_to_bars"))
    }
}

// ============================ the posed-state fixture family ============================

/// A store that ANSWERED and holds nothing — the control every disclosure assertion over the
/// shell needs in order to mean anything, and the empty-store pose of the goldens suite. A real
/// `DataFusionHist` rather than a memory double: its `list_series` returns a real empty answer
/// somebody computed, where a double would reach the same value through the trait DEFAULT — the
/// weaker fixture, and one that cannot hold bars anyway.
///
/// The `TempDir` comes back because the store keeps reading that directory on every frame;
/// dropping it would delete the store out from under the harness.
pub fn empty_store() -> (tempfile::TempDir, StoreHandle) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store: StoreHandle = Arc::new(DataFusionHist::open(dir.path()).expect("open hist store"));
    (dir, store)
}

/// [`empty_store`] plus 400 one-minute `binance/BTCUSDT` bars on a deterministic wave.
///
/// A wave rather than the sawtooth `crates/vike-studio/src/studio.rs`'s own `seeded_store` uses,
/// and the same series `crates/vike-studio/examples/studio_shot.rs` captures with, because this
/// fixture has a second job: it must give the RESULTS pane a moving equity curve and real trades
/// to plot, not merely give `▶ Run` something to enable on.
pub fn seeded_store() -> (tempfile::TempDir, StoreHandle) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = DataFusionHist::open(dir.path()).expect("open hist store");
    let base: i64 = 1_767_225_600_000;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            let t = i as f64;
            let close = 60_000.0 + 2.0 * t + 400.0 * (t / 14.3).sin();
            let open = 60_000.0 + 2.0 * (t - 1.0) + 400.0 * ((t - 1.0) / 14.3).sin();
            Bar {
                ts: base + 60_000 * i,
                open,
                high: open.max(close) + 25.0,
                low: open.min(close) - 25.0,
                close,
                volume: 10.0 + (t / 5.0).cos().abs() * 90.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("append bars");
    (dir, Arc::new(store))
}

/// The raw tab name `vike-app`'s `main.rs` reads out of `VIKE_STUDIO_TAB` and hands to
/// `crates/vike-studio/src/studio.rs`'s `new_with_qa`, parsed back there by `from_qa_str`.
///
/// Exhaustive on purpose: an EIGHTH `RightTab` is a compile error here rather than a tab the
/// consuming suites silently never pose. (It said "a seventh" until Research became the seventh.)
pub fn qa_name(t: RightTab) -> &'static str {
    match t {
        RightTab::Sweep => "sweep",
        RightTab::Strategy => "strategy",
        RightTab::Data => "data",
        RightTab::Indicators => "indicators",
        RightTab::Saved => "saved",
        RightTab::Research => "research",
        RightTab::Chat => "chat",
    }
}

/// A `StudioState` posed on `tab` and scrubbed of everything a restored workspace could have
/// carried in. See this module's doc for the WRITE hazard the qa constructor closes and the READ
/// hazard these five assignments close — and for the maintenance obligation both consumers
/// inherit: a new field on `crates/vike-studio/src/workspace.rs`'s `StudioWorkspace` must join
/// this reset, or every frame rendered over it becomes machine-dependent again.
pub fn state(
    store: &StoreHandle,
    dir: &tempfile::TempDir,
    tab: RightTab,
    keys: ChatApiKeys,
) -> StudioState {
    let name = qa_name(tab);
    assert_eq!(
        RightTab::from_qa_str(name),
        Some(tab),
        "{name:?} must parse back to the tab it names: an unrecognised name is IGNORED, and the \
         session then restores the workspace AND leaves qa_workspace_readonly false"
    );
    let mut st =
        StudioState::new_with_qa(store.clone(), dir.path().to_path_buf(), keys, Some(name), false);
    assert_eq!(st.right_tab, tab, "the QA tab override must actually take");
    st.editor = EditorPane::default();
    st.saved_source = st.editor.source.clone();
    st.editor_collapsed = false;
    st.tools_collapsed = false;
    st.template_idx = 0;
    st
}
