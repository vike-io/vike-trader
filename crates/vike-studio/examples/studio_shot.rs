//! Headless Studio → PNG capture harness — the vike-studio twin of vike-chart's
//! `examples/export_png.rs`, and a lighter alternative to the `VIKE_SHOT` flow (which needs a full
//! `vike-desktop` build: eframe + winit — and every bridge crate too, until the desktop's `fat`
//! build was deleted on 2026-09-09).
//!
//! Renders the REAL `StudioState::ui` off-screen — no window, no eframe — into `studio-<tab>.png`:
//!
//!   1. seed a temp `HistoryStoreDataFusion` with a bar series AND a tick series so the picker has both
//!      row families, build a `StudioState` over it, and drive `ui()` through
//!      `egui::Context::run_ui` for a few frames (egui needs a second frame for combo/panel layout
//!      to settle);
//!   2. `context.tessellate(...)` → `Vec<ClippedPrimitive>`;
//!   3. paint those with the SAME production renderer eframe uses — `egui_wgpu::Renderer` — into an
//!      off-screen `Rgba8UnormSrgb` texture on a headless wgpu device (WARP/DX12 on Windows);
//!   4. copy the texture back and save it as a PNG via `image`;
//!   5. gate the readback with the shared non-golden pixel-liveness floors
//!      (`vike_ui_theme::pixel_liveness`), so a run that saved a blank frame reddens the
//!      `png-export` lane instead of exiting 0.
//!
//! Everything here is dev-only (the `png-export` feature); the library stays GPU-free.
//!
//! ```sh
//! cargo run -p vike-studio --features png-export --example studio_shot            # every tab
//! cargo run -p vike-studio --features png-export --example studio_shot -- strategy
//! ```

use std::sync::Arc;

use vike_backtesthub_client::proto::WireStrategyFile;
use vike_backtesthub_client::wire_studio::WalkforwardWire;
use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::params::ParamValue;
use vike_store::HistoryStoreDataFusion;
use vike_studio::{
    StudioAutorun, StudioChatApiKeys, StudioResultsTab, StudioSavedStrategy, StudioState,
    StudioStrategySource, StudioTab,
};
use vike_studio_core::{DataSlice, StrategySpec};

use vike_ui_theme::offscreen::{ThemeFrameCheckTextureDelta, rasterize};
use vike_utility::cli::Program;

// width*4 must be a 256-multiple to skip row padding on readback; 1600 is.
const PPP: f32 = 1.5;
const W_PX: u32 = 1600;
const H_PX: u32 = 1000;

/// Liveness floors for the three Studio poses. The mechanism and the rebaseline-free argument
/// live in `vike_ui_theme::pixel_liveness`; the numbers are facts about THESE scenes, so they
/// live beside them, and every margin is chosen to hold on both rasterizers that run this harness
/// (lavapipe in CI, DX12/WARP or a real driver on the dev box).
///
/// - `min_distinct_colors: 64` — every pose carries the two-row toolbar, the expanded left
///   editor full of syntax-highlighted Rhai (the default template), the tool rail's icons and a
///   POSED tools pane (native strategy form / sweep grid / two saved entries), so a healthy
///   frame's anti-aliased text alone measures distinct colours in the hundreds-to-thousands on
///   either rasterizer; 64 keeps over an order of magnitude of margin while a collapsed frame
///   measures 1.
/// - `max_dominant_share: 0.98` — bounds the one-colour-ate-the-frame class without any claim
///   about layout: even in the worst legitimate case where every panel shares one background
///   fill, the editor's glyph coverage plus toolbar/rail/tools chrome keeps any single colour
///   around 90% of a healthy frame, against 100.00% for a dead one.
/// - `min_quadrant_distinct: None`, DELIBERATELY (the chart harness argues the opposite for its
///   scene): the central panel is a mostly-flat getting-started block by design — no run result
///   exists in a capture — panel fills may legitimately share one colour, and which side of a
///   quadrant boundary a panel seam lands on is a LAYOUT accident that a resize or a default
///   panel-width change would flip. A quadrant floor here would gate on that accident, i.e. a
///   false-fire generator rather than a liveness signal.
const LIVENESS: vike_ui_theme::pixel_liveness::ThemeFrameCheckLivenessSpec =
    vike_ui_theme::pixel_liveness::ThemeFrameCheckLivenessSpec {
        min_distinct_colors: 64,
        max_dominant_share: 0.98,
        min_quadrant_distinct: None,
    };

/// The command line, read by `clap` through `vike_utility::cli::Program` (decision 0155): the captures
/// to take, none meaning every one. A name that is no capture is skipped, and `main` refuses a line
/// that names none at all.
#[derive(clap::Parser, Debug, PartialEq, Eq)]
#[command(name = "studio_shot", about = "Headless Studio to PNG capture harness")]
struct Cli {
    /// A capture: strategy, sweep, saved, data, indicators, performance or validation (default:
    /// all).
    #[arg(value_name = "TAB")]
    tabs: Vec<String>,
}

/// One capture: its file name, the tool tab it poses (by the name `new_with_qa` arms its workspace
/// guard with), and — for the two RESULT poses — the results tab the central panel shows over real
/// hub runs (`hub_results`).
type Capture = (&'static str, &'static str, StudioTab, Option<StudioResultsTab>);

fn main() {
    let Cli { tabs: want } =
        Program::new("studio_shot", concat!("studio_shot ", env!("CARGO_PKG_VERSION"))).parse();
    let captures: Vec<Capture> = [
        ("strategy", "strategy", StudioTab::Strategy, None),
        ("sweep", "sweep", StudioTab::Sweep, None),
        ("saved", "saved", StudioTab::Saved, None),
        // Added for the Data Manager migration's render check (GUI design system step 7): the
        // Data pane was a real, wired `StudioTab` variant (`from_qa_str` already mapped "data" to
        // it) that this harness had simply never listed as a nameable capture, so `studio-data.png`
        // could not be produced by either build. Posing it costs nothing the other three tabs did
        // not already pay.
        ("data", "data", StudioTab::Data, None),
        // The Data pane's Import CSV form (decision 0272), open over a preview the datahub answered:
        // the file's header offered as the column choices and its first rows as read.
        ("data-import", "data", StudioTab::Data, None),
        // The catalog work's four panes (the Studio audit's PR 2): the Indicators list with what a
        // script cannot call; the Performance tab beside the Saved pane's compare table; the
        // Validation tab beside the Sweep pane's walk-forward controls. The two result poses run
        // real backtests through an in-process backtesthub first — a capture of an empty panel
        // would show none of the tables they exist to show.
        ("indicators", "indicators", StudioTab::Indicators, None),
        ("performance", "saved", StudioTab::Saved, Some(StudioResultsTab::Performance)),
        ("validation", "sweep", StudioTab::Sweep, Some(StudioResultsTab::Validation)),
    ]
    .into_iter()
    .filter(|(n, ..)| want.is_empty() || want.iter().any(|w| w == n))
    .collect();
    assert!(
        !captures.is_empty(),
        "no known capture in {want:?} \
         (strategy|sweep|saved|data|data-import|indicators|performance|validation)"
    );

    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(seeded_store(dir.path()));

    for (name, qa_tab, tab, results) in captures {
        // The pairing above is the compile-time half — it pins that each capture's pose still
        // refers to a live `StudioTab` variant. `render_frames` takes the NAME rather than the
        // variant because the name is what arms `new_with_qa`'s workspace guard; this asserts the
        // two halves still agree before a frame is painted.
        assert_eq!(
            StudioTab::from_qa_str(qa_tab),
            Some(tab),
            "capture name/variant pairing drifted"
        );
        let (context, tex_deltas, out) = render_frames(&store, dir.path(), name, qa_tab, results);
        let image = rasterize(&context, tex_deltas, out, [W_PX, H_PX], PPP, "studio_shot");
        let path = format!("studio-{name}.png");
        image.save(&path).expect("write PNG");
        // Liveness AFTER the save, so a red run leaves its PNG on disk as the diagnostic; the
        // lane fails on the panic's exit code either way. Same treatment as `export_png.rs`.
        let report = vike_ui_theme::pixel_liveness::pixel_report(
            image.as_raw(),
            image.width(),
            image.height(),
        );
        println!("pixel liveness [{name}]: {report}");
        vike_ui_theme::pixel_liveness::assert_pixels_live(&report, &LIVENESS);
        println!("wrote {path} ({W_PX}x{H_PX})");
    }
}

/// A store with BOTH families the picker now lists: 400 `binance/BTCUSDT 1m` bars (so the Rhai
/// default script and `buy_hold` both have something to trade) and a `polymarket/TKN` quote+trade
/// tick series (so the tick picker row is visible and pickable).
fn seeded_store(root: &std::path::Path) -> HistoryStoreDataFusion {
    let store = HistoryStoreDataFusion::open(root).expect("open hist store");
    let base: i64 = 1_767_225_600_000;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            let t = i as f64;
            let close = 60_000.0 + 2.0 * t + 400.0 * (t / 14.3).sin();
            let open = 60_000.0 + 2.0 * (t - 1.0) + 400.0 * ((t - 1.0) / 14.3).sin();
            Bar {
                timestamp: base + 60_000 * i,
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

    let quotes: Vec<TickQuote> = (0..300)
        .map(|i| TickQuote {
            timestamp: base + 1_000 * i,
            local_timestamp: base + 1_000 * i,
            bid: 0.30 + (i % 11) as f64 * 0.001,
            ask: 0.31 + (i % 11) as f64 * 0.001,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: "TKN".into(),
        })
        .collect();
    let trades: Vec<TickTrade> = (0..300)
        .map(|i| TickTrade {
            timestamp: base + 1_000 * i,
            local_timestamp: base + 1_000 * i,
            price: 0.305 + (i % 11) as f64 * 0.001,
            size: 5.0,
            is_buyer_maker: i % 2 == 0,
            symbol: "TKN".into(),
        })
        .collect();
    store.append_quotes("polymarket", "TKN", &quotes, None).expect("append quotes");
    store.append_trades("polymarket", "TKN", &trades, None).expect("append trades");
    store
}

/// Build a `StudioState` posed for the tab named `qa_tab` and run a few `ui()` frames, keeping the
/// last frame's `FullOutput` plus every frame's texture deltas.
///
/// ⚠ The pose is set THROUGH `crates/vike-studio/src/studio/mod.rs`'s `new_with_qa`, never by assigning
/// `right_tab`/`tools_collapsed` afterwards, and that is a correctness rule rather than a style
/// one. `new_with_qa` derives `qa_workspace_readonly` from the tab argument alone
/// (`qa_tab.is_some()`), and that flag is the ONLY thing standing between a capture run and the
/// operator's real Studio workspace: `maybe_persist_workspace` runs at the end of every `ui()`
/// frame and writes the current pose to `<project>/settings/state` once it has drifted from what
/// was loaded (and held still for the save's quiet period). Posing by field assignment leaves the
/// flag FALSE, so the frames below would persist the FORCED tab and expanded state over the user's
/// own file — and `scripts/qa_shots.sh`
/// cannot prevent it from outside, because it isolates `VIKE_SETTINGS_DIR` while
/// `vike_paths::state_path::project_state_dir` resolves by walking up from the working directory
/// instead. Pass the name and let the constructor arm the guard.
fn render_frames(
    store: &Arc<HistoryStoreDataFusion>,
    state_dir: &std::path::Path,
    name: &str,
    qa_tab: &str,
    results: Option<StudioResultsTab>,
) -> (egui::Context, Vec<ThemeFrameCheckTextureDelta>, egui::FullOutput) {
    let context = egui::Context::default();
    bind_named_font_families(&context);
    context.set_pixels_per_point(PPP);

    // state_dir = the store's root, the same pairing vike-desktop passes for a local store.
    // No provider keys: a capture harness must never open the credential store (`just qa-shots`
    // runs against a throwaway credential-free settings root for exactly that reason).
    // `StudioAutorun::Off` — the capture poses the shell, it never kicks off a run.
    let mut st = StudioState::new_with_qa(
        store.clone(),
        state_dir.to_path_buf(),
        StudioChatApiKeys::default(),
        Some(qa_tab),
        StudioAutorun::Off,
    );
    assert!(
        st.right_tab == StudioTab::from_qa_str(qa_tab).expect("main() only passes known tab names"),
        "`new_with_qa` did not adopt the posed tab — the readonly guard keys off the same \
         argument, so a silent miss here is a capture run writing over the user's workspace",
    );
    // Pose the new surfaces so the capture actually SHOWS them rather than an empty default.
    st.strategy_source = StudioStrategySource::File;
    // The served list, POSED (nothing is dialled): one file in each state the scan reports.
    st.strategy_files = Some(Ok(posed_strategy_files()));
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())];
    // The rows are the picked file's: put away the Rhai buffer's first (`StudioState::poll`).
    st.poll();
    st.grid = vec![vike_studio::StudioParamRow::scanned("size", "1, 2, 3")];
    st.saved.strategies = vec![
        StudioSavedStrategy::rhai("sma-cross", vike_studio::StudioEditorPane::default().source),
        StudioSavedStrategy::file("hold-2", "trend/buy_hold.rs", st.file_params.clone()),
    ];
    if let Some(tab) = results {
        hub_results(&mut st, store);
        st.tab = tab;
    }
    if qa_tab == "indicators" {
        // A search that lists an indicator a script cannot call, so the capture shows that row.
        st.indicators.search = "zig".to_string();
    }
    if name == "data-import" {
        pose_csv_import(&mut st);
    }

    let screen = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(W_PX as f32 / PPP, H_PX as f32 / PPP),
    );

    // The font atlas texture is emitted only in the FIRST frame's `textures_delta.set` and never
    // resent, so accumulate every frame's deltas IN ORDER and apply them all before rendering the
    // last frame (same reasoning as vike-chart's harness).
    let mut tex_deltas: Vec<ThemeFrameCheckTextureDelta> = Vec::new();
    let mut last = None;
    for f in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f as f64 / 60.0),
            ..Default::default()
        };
        let mut out = context.run_ui(raw, |ui| st.ui(ui));
        // egui 0.36 turned `textures_delta.set` from a `Vec<(TextureId, ImageDelta)>` into a map
        // whose value is a SmallVec — one texture id can carry several deltas in a frame. Flatten
        // back into the (id, delta) pairs `update_texture` takes. Mirrors `export_png.rs`.
        tex_deltas.extend(
            out.textures_delta
                .set
                .iter()
                .flat_map(|(id, deltas)| deltas.iter().map(move |d| (*id, d.clone()))),
        );
        // Harvested above into `tex_deltas`, so this frame's own list is handled and must be
        // cleared: egui 0.36 panics on a dropped `TexturesDelta` that still holds entries, and
        // every frame but the last IS dropped here. Same as `export_png.rs`.
        out.textures_delta.clear();
        last = Some(out);
    }
    (context.clone(), tex_deltas, last.expect("rendered at least one frame"))
}

/// A served strategy-file list with one file in each state: built, not built, a long build failure,
/// cannot run here, edited, a Rhai file, and a broken header.
fn posed_strategy_files() -> Vec<WireStrategyFile> {
    let file = |file: &str, build: &str, broken: &str, edited: bool| WireStrategyFile {
        file: file.to_string(),
        build: build.to_string(),
        broken: broken.to_string(),
        edited,
    };
    vec![
        file("trend/buy_hold.rs", "built 3f9a1c0e", "", false),
        file("trend/momentum.rs", "not built", "", true),
        file(
            "grid_dca/a_rather_long_strategy_file_name_that_must_truncate.rs",
            "build failed: error[E0425]: cannot find value `rungz` in this scope\n --> src/strategy.rs:41:9",
            "",
            false,
        ),
        file("market_making/spread_maker.rs", "cannot run here: no sandbox on this box", "", false),
        file("trend/sma_cross.rhai", "", "", false),
        file("other/notes.rhai", "", "no header: a strategy file opens with comment lines", false),
    ]
}

/// The Import CSV form open over a preview the datahub answered (the answer is POSED: nothing is
/// sent): a daily rate file with its own column names, mapped from the header the preview returned.
/// The dial is set so the buttons are live, and is never dialled — no button is pressed.
fn pose_csv_import(st: &mut StudioState) {
    use vike_datahub_client::archive::ImportPlanPreviewRow;
    use vike_datahub_client::upload::CsvUploadDone;
    st.datahub_upload = Some(vike_studio::StudioCsvUploadDial {
        address: "127.0.0.1:7878".to_string(),
        keys: None,
    });
    let day = 16_436 * vike_model::MS_PER_DAY; // 2015-01-01
    let row = |d: i64, close: &str| ImportPlanPreviewRow {
        timestamp: day + d * vike_model::MS_PER_DAY,
        open: close.to_string(),
        high: close.to_string(),
        low: close.to_string(),
        close: close.to_string(),
        volume: "0".to_string(),
    };
    let form = &mut st.data_browser.import;
    form.open = true;
    form.path = r"C:\Users\me\Downloads\USDT-USD.csv".to_string();
    form.label = "coinbase".to_string();
    form.symbol = "USDT-USD".to_string();
    form.interval = "1d".to_string();
    form.finish(
        true,
        Ok(CsvUploadDone {
            header: vec!["timestamp_ms".to_string(), "rate".to_string()],
            preview: vec![row(0, "1.0012"), row(1, "1.0008"), row(2, "0.9997")],
            bars: 3650,
            days: 3650,
            first_day: Some(day),
            last_day: Some(day + 3649 * vike_model::MS_PER_DAY),
            ..CsvUploadDone::default()
        }),
    );
    form.map_columns = true;
    form.picks = [1, 0, 0, 0, 2, 0];
}

/// Fill the result surfaces the way a session fills them — every number from the BACKTESTHUB, none
/// simulated here: a Run of the editor's SMA-cross script, a sweep and a walk-forward of the posed
/// native strategy, and "Compare all" over the saved list (one `RunSlice` per row, as
/// `StudioState`'s own Compare all sends them), all over an unauthenticated loopback hub serving the
/// seeded store — the hermetic server the crate's own tests dial.
fn hub_results(st: &mut StudioState, store: &Arc<HistoryStoreDataFusion>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let address = listener.local_addr().expect("local addr").to_string();
    let served: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    std::thread::spawn(move || {
        let _ = vike_backtesthub::backtesthub_server::serve(listener, served);
    });
    let slice = DataSlice::bars("binance", "BTCUSDT", "1m", TimestampRange::all());
    fn wait<T>(rx: std::sync::mpsc::Receiver<T>) -> T {
        rx.recv().expect("the hub answers")
    }

    let cross = StrategySpec::rhai(vike_studio::StudioEditorPane::default().source);
    st.last = Some(wait(vike_studio::spawn_backtesthub_run(
        address.clone(),
        None,
        cross,
        slice.clone(),
        vike_studio::StudioRunEngine::default(),
        false,
    )));
    let native = st.current_spec();
    let grid = vec![(
        "size".to_string(),
        vec![ParamValue::Int(1), ParamValue::Int(2), ParamValue::Int(3)],
    )];
    st.sweep_last = Some(wait(vike_studio::spawn_backtesthub_sweep(
        address.clone(),
        None,
        native.clone(),
        slice.clone(),
        grid,
        vike_studio::StudioRunEngine::default(),
        false,
    )));
    st.walkforward_last = Some(wait(vike_studio::spawn_backtesthub_walkforward(
        address.clone(),
        None,
        native,
        slice.clone(),
        WalkforwardWire::fixed(st.walkforward_splits),
        vike_studio::StudioRunEngine::default(),
        false,
    )));
    // Compare all ranks each row by its SUMMARY (`crate::backend::backtesthub`'s `run_backtesthub_slice_summary`).
    let compared: Vec<(String, Result<vike_backtesthub_client::summary::RunSummaryWire, String>)> =
        st.saved
            .strategies
            .iter()
            .map(|s| {
                let summary = vike_studio_core::to_wire_spec(&s.spec())
                    .map_err(|e| e.to_string())
                    .and_then(|spec| {
                        vike_backtesthub_client::BacktesthubClient::connect(address.as_str())
                            .map_err(|e| e.to_string())?
                            .run_slice_summary(spec, vike_studio_core::to_wire_slice(&slice), None)
                    });
                (s.name.clone(), summary)
            })
            .collect();
    let sources: Vec<(String, StudioStrategySource)> =
        st.saved.strategies.iter().map(|s| (s.name.clone(), s.source)).collect();
    st.saved.compare_rows = Some(vike_studio::saved_compare_rows(&compared, |name| {
        sources.iter().find(|(n, _)| n == name).map_or(StudioStrategySource::Rhai, |(_, s)| *s)
    }));
}

/// Installs the app's type (faces AND sizes), so the picture's text is the app's.
fn bind_named_font_families(context: &egui::Context) {
    vike_ui_theme::appearance::install_type(
        context,
        vike_ui_theme::type_scale::ThemeTextSize::Small,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &[&str]) -> Result<Cli, vike_utility::cli::CliExit> {
        Program::new("studio_shot", "studio_shot 0").try_parse_from(line.iter().copied())
    }

    /// POSITIVE argv pins (decision 0155): `justfile`'s and `scripts/ci_feature_suite.sh`'s bare
    /// `studio_shot` (every tab) and `scripts/qa_shots.sh`'s one tab name.
    #[test]
    fn the_documented_lines_are_accepted_with_their_meaning() {
        assert_eq!(parse(&[]).unwrap().tabs, Vec::<String>::new());
        assert_eq!(parse(&["strategy"]).unwrap().tabs, ["strategy"]);
        assert_eq!(parse(&["sweep", "saved", "data"]).unwrap().tabs, ["sweep", "saved", "data"]);
    }

    #[test]
    fn an_unknown_flag_is_a_usage_error() {
        let exit = parse(&["--nope"]).unwrap_err();
        assert_eq!((exit.code, exit.to_stdout), (2, false));
    }
}
