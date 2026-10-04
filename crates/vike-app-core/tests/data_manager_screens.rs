//! **The Data Manager, held to the design system — every destination, in every theme.**
//!
//! Step 7 of the design system (`docs/superpowers/specs/2026-09-28-gui-design-system-design.md`
//! §9) moved this window off `vike_ui_theme::palette` — Graphite's values, compiled in — and onto
//! the installed appearance. These gates keep it there.
//!
//! - Two read the RENDERED frame of every destination (`data_tool_content` over a seeded store),
//!   because a leftover constant shows only where it is drawn.
//! - Two read the SOURCE, because a path the seed does not reach (a confirm dialog, an error state)
//!   draws nothing here.
//! - One reads the rail off the accessibility tree.
//! - One is the safety net under all of them: every destination paints sane geometry at every
//!   density and text size.
//!
//! Under Graphite the colour check is vacuous — its tokens ARE the palette's values — so the three
//! other themes are the ones asked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::Color32;
use egui::accesskit::Role;
use egui::epaint::Shape;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use vike_app_core::data::catalog_refresh::CatalogRefresh;
use vike_app_core::data::history_column::HistoryLoad;
use vike_app_core::tools::{ToolData, ToolView};
use vike_app_core::ui::tool_views::data_rail::{RailCounts, rail};
use vike_app_core::ui::tool_views::{BookCtx, DataDest, StoredCtx, ToolCtx, data_tool_content};
use vike_data::datasets::{DataSet, Store};
use vike_data::{InstrumentKey, PartialDay, SeriesCoverage, SeriesId};
use vike_model::change_journal::Proc;
use vike_ui_theme::appearance::{self, Appearance};
use vike_ui_theme::metrics::Density;
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::TextSize;

const DAY: i64 = 86_400_000;

/// Everything `ToolCtx` borrows, owned, so every pass can build one. The store is small, but it
/// reaches every branch a destination draws a colour from:
/// - two venues, one series with a gap and one stale series;
/// - a partial day;
/// - two live feeds;
/// - a DataSet with a stored and an unstored member;
/// - a feed status.
struct Owned {
    td: ToolData,
    snap: vike_core::CoreSnapshot,
    textures: HashMap<String, egui::TextureHandle>,
    dsets: Store,
    tree: Vec<vike_data_manager::VenueNode>,
    gaps: vike_data_manager::GapMap,
    partials: vike_data_manager::PartialDayMap,
    feeds: Vec<(String, usize, i64, i64)>,
    statuses: HashMap<String, Arc<Mutex<String>>>,
    proc: Proc,
    catalog: CatalogRefresh,
    symbols: Arc<vike_catalog::Catalog>,
    poly_names: HashMap<String, String>,
    history: HistoryLoad,
}

impl Owned {
    fn seeded() -> Self {
        let sid = |v: &str, s: &str| SeriesId::per_symbol("bar", v, s, Some("1m".to_string()));
        let cov = |last_day: i64| SeriesCoverage {
            first_ts: 0,
            last_ts: last_day * DAY,
            rows: 1_000,
            bytes: 64_000,
            parts: 1,
            dates: 1,
        };
        let tree = vike_data_manager::build_tree(vec![
            (sid("binance", "BTCUSDT"), cov(100)),
            // 80 days behind a 100-day window: stale.
            (sid("binance", "ETHUSDT"), cov(20)),
            (sid("okx", "BTC-USDT"), cov(99)),
        ]);
        let mut gaps = vike_data_manager::GapMap::new();
        gaps.insert(
            vike_data_manager::SeriesKey {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                kind: "bar".into(),
                interval: Some("1m".into()),
            },
            vec![(10 * DAY, 12 * DAY)],
        );
        let mut partials = vike_data_manager::PartialDayMap::new();
        partials.insert(
            InstrumentKey { venue: "okx".into(), label: "BTC-USDT".into(), grouped: false },
            vec![PartialDay { day: 50, missing_kinds: vec!["quote".into()] }],
        );
        // The catalog does no I/O at construction, and nothing here ever sends.
        let (tx, _rx) = std::sync::mpsc::channel();
        Owned {
            td: ToolData::default(),
            snap: vike_core::CoreSnapshot::empty("", ""),
            textures: HashMap::new(),
            dsets: Store {
                sets: vec![DataSet {
                    name: "Majors".into(),
                    symbols: vec!["BTCUSDT".into(), "SOLUSDT".into()],
                    provider: "binance".into(),
                    interval: "1m".into(),
                    benchmark: String::new(),
                    user: true,
                }],
            },
            tree,
            gaps,
            partials,
            feeds: vec![
                ("BTCUSDT@1m".into(), 1_440, DAY, 2 * DAY),
                ("ETHUSDT@1m".into(), 60, DAY, DAY + 3_600_000),
            ],
            statuses: HashMap::from([(
                "binance".to_string(),
                Arc::new(Mutex::new("connected".to_string())),
            )]),
            proc: Proc::new("vike-test", 4711, "0.1.0"),
            catalog: CatalogRefresh::new(Vec::new(), None, None, None, tx),
            symbols: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
            poly_names: HashMap::new(),
            history: planted_history(),
        }
    }

    fn ctx(&self) -> ToolCtx<'_> {
        ToolCtx {
            td: &self.td,
            snap: &self.snap,
            flags: &self.textures,
            logos: &self.textures,
            journal_dir: None,
            feeds: &self.feeds,
            dsets: &self.dsets,
            display_tz: vike_chart::DisplayTz::default(),
            stored: StoredCtx {
                tree: &self.tree,
                gaps: &self.gaps,
                partials: &self.partials,
                loading: false,
                load_error: None,
                backfill_status: "",
                delete_unavailable: None,
                partials_note: None,
                history: Some(&self.history),
            },
            feed_statuses: &self.statuses,
            credentials: vike_connections::CredentialWrite {
                store: Path::new(""),
                journal: None,
                proc: &self.proc,
                now_ms: 0,
            },
            book: BookCtx { symbol: "", venue: "", book: None, stale: false },
            catalog: &self.catalog,
            symbols: &self.symbols,
            last_trade: None,
            directory: None,
            directory_unavailable: false,
            control_link: vike_app_core::ui::tool_views::ControlLink::Open,
            poly_names: &self.poly_names,
        }
    }
}

/// A SERVED history-channels answer for the By-venue table's HISTORY column: this build's own rows
/// standing in for a server's, every built lane mounted except BINANCE's kline lane — so one stored
/// venue's cell carries the warning flag and the other's does not.
fn planted_history() -> HistoryLoad {
    let mut load = HistoryLoad::served(HistoryLoad::compiled(100 * DAY).report);
    for venue in &mut load.report.venues {
        for ch in &mut venue.channels {
            if ch.state.kind == "built" {
                ch.mounted =
                    Some(!(venue.venue == "binance" && ch.lane.as_deref() == Some("Klines")));
            }
        }
    }
    load
}

/// A window on `dest`, with a series opened (so the inspector draws its details), a feed selected,
/// a log line, and the first-shown store load already asked for.
fn window_on(dest: DataDest) -> ToolView {
    ToolView {
        data_dest: dest,
        stored_last_sel: Some((
            "binance".into(),
            "BTCUSDT".into(),
            "bar".into(),
            Some("1m".into()),
        )),
        data_sel: Some("BTCUSDT@1m".into()),
        data_log: vec!["12:00:00  Refreshed · 2 series".into()],
        stored_auto_requested: true,
        ..Default::default()
    }
}

fn raw(frame: u32) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1500.0, 1000.0))),
        time: Some(f64::from(frame) / 60.0),
        ..Default::default()
    }
}

fn flatten(shape: Shape, out: &mut Vec<Shape>) {
    match shape {
        Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, out)),
        s => out.push(s),
    }
}

/// One pass. Texture uploads are discarded (a headless pass has no backend to apply them to, and
/// epaint 0.36 panics on dropping unapplied ones), the geometry is asserted sane, and the shapes
/// are flattened.
fn run(ctx: &egui::Context, frame: u32, add: impl FnMut(&mut egui::Ui)) -> Vec<Shape> {
    let mut full = ctx.run_ui(raw(frame), add);
    full.textures_delta.clear();
    vike_ui_theme::frame_sanity::assert_frame_sane(&full);
    let mut flat = Vec::new();
    for clipped in std::mem::take(&mut full.shapes) {
        flatten(clipped.shape, &mut flat);
    }
    flat
}

/// Every destination's shapes under `a`:
/// - one context, with the appearance installed and one empty pass behind it (the fonts land on
///   the next pass);
/// - then two passes per destination on a fresh window. The second pass is the one read, once the
///   first has settled the layout.
fn frames(a: &Appearance) -> Vec<(DataDest, Vec<Shape>)> {
    let owned = Owned::seeded();
    let ctx = egui::Context::default();
    appearance::install(&ctx, a);
    run(&ctx, 0, |_| {});
    let mut out = Vec::new();
    for (i, dest) in DataDest::ALL.into_iter().enumerate() {
        let mut tv = window_on(dest);
        let mut shapes = Vec::new();
        for pass in 0..2 {
            let frame = (1 + 2 * i + pass) as u32;
            shapes = run(&ctx, frame, |ui| data_tool_content(ui, &owned.ctx(), &mut tv, None));
        }
        out.push((dest, shapes));
    }
    out
}

/// `(text, colour)` for every run of text painted. Each section of a multi-section galley counts
/// on its own, with its placeholder colour resolved the way the painter resolves it.
fn words(shapes: &[Shape]) -> Vec<(String, Color32)> {
    let mut out = Vec::new();
    for s in shapes {
        if let Shape::Text(t) = s {
            let job = &t.galley.job;
            for sec in &job.sections {
                let own = if sec.format.color == Color32::PLACEHOLDER {
                    t.fallback_color
                } else {
                    sec.format.color
                };
                let c = t.override_text_color.unwrap_or(own);
                // epaint 0.36's `byte_range` is a range of `ByteIndex`, a `usize` newtype.
                let bytes = sec.byte_range.start.0..sec.byte_range.end.0;
                out.push((job.text[bytes].to_string(), c));
            }
        }
    }
    out
}

/// Every colour painted: text runs, rect fills and strokes, circle fills and strokes, and line
/// strokes.
fn colours(shapes: &[Shape]) -> Vec<Color32> {
    let mut out: Vec<Color32> = words(shapes).into_iter().map(|(_, c)| c).collect();
    for s in shapes {
        match s {
            Shape::Rect(r) => {
                out.push(r.fill);
                out.push(r.stroke.color);
            }
            Shape::Circle(c) => {
                out.push(c.fill);
                out.push(c.stroke.color);
            }
            Shape::LineSegment { stroke, .. } => out.push(stroke.color),
            _ => {}
        }
    }
    out
}

/// Is `text` an icon — nothing but Private Use Area codepoints? An icon is a SHAPE, so the accent
/// may colour it (the rail's selected row does).
fn is_icon(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && t.chars().all(|c| ('\u{E000}'..='\u{F8FF}').contains(&c))
}

/// Graphite's neutrals and accent — the values of `palette`'s app constants — plus `palette::WARN`,
/// which no theme carries. On a non-Graphite screen, any of these is a compiled-in colour the
/// migration missed.
fn graphite_only() -> Vec<(&'static str, Color32)> {
    let g = Theme::of(ThemeId::Graphite);
    vec![
        ("bg", g.bg),
        ("surface", g.surface),
        ("card", g.card),
        ("hover", g.hover),
        ("border", g.border),
        ("text", g.text),
        ("text_ui", g.text_ui),
        ("text2", g.text2),
        ("text3", g.text3),
        ("accent", g.accent),
        ("WARN", Color32::from_rgb(255, 174, 0)),
    ]
}

/// **Every destination follows the installed theme.** Two checks, per theme, per destination:
/// - no shape is painted in a Graphite colour;
/// - the window drew in THIS theme's surface, which is the anti-vacuity check: a frame that drew
///   nothing passes the first check for the wrong reason.
#[test]
fn every_destination_follows_the_installed_theme() {
    let mut findings = Vec::new();
    for theme in [ThemeId::Midnight, ThemeId::Dusk, ThemeId::Carbon] {
        let own = Theme::of(theme);
        for (dest, shapes) in frames(&Appearance { theme, ..Appearance::default() }) {
            let seen = colours(&shapes);
            if !seen.contains(&own.surface) {
                findings
                    .push(format!("{theme:?} {dest:?}: nothing painted in this theme's surface"));
            }
            for (name, c) in graphite_only() {
                if seen.contains(&c) {
                    findings.push(format!("{theme:?} {dest:?}: Graphite {name} {c:?}"));
                }
            }
        }
    }
    assert!(findings.is_empty(), "compiled-in Graphite colours: {findings:#?}");
}

/// **The accent is a shape, never a word's colour** (spec §2), on every theme and destination.
#[test]
fn no_destination_draws_a_word_in_the_accent() {
    let mut hits = Vec::new();
    for theme in ThemeId::ALL {
        let accent = Theme::of(theme).accent;
        for (dest, shapes) in frames(&Appearance { theme, ..Appearance::default() }) {
            for (text, c) in words(&shapes) {
                if c == accent && !text.trim().is_empty() && !is_icon(&text) {
                    hits.push(format!("{theme:?} {dest:?}: {text:?}"));
                }
            }
        }
    }
    assert!(hits.is_empty(), "a word drawn in the accent: {hits:#?}");
}

/// **Every destination paints sane geometry at every density and text size.** `run` asserts each
/// pass sane. The rail and the crumb both print the destination's name, so fewer than two copies of
/// it means the window did not draw.
#[test]
fn every_destination_renders_sanely_at_every_density_and_text_size() {
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            for (dest, shapes) in
                frames(&Appearance { density, text_size, ..Appearance::default() })
            {
                let named = words(&shapes).iter().filter(|(t, _)| t == dest.label()).count();
                assert!(
                    named >= 2,
                    "{density:?} {text_size:?} {dest:?}: {named} copies of its name"
                );
            }
        }
    }
}

/// **The By-venue table carries a HISTORY column** (the history-channels design's §3.1 and §4):
/// its header, a stored venue whose lane is not mounted drawn with that flag in the WARNING
/// colour, and one whose lane is mounted drawn plain — at every density and text size. What this
/// cannot prove is that the column is legible or fits; that is a look on a real GPU.
#[test]
fn the_by_venue_table_carries_the_history_column() {
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            let a = Appearance { density, text_size, ..Appearance::default() };
            let warn = vike_ui_theme::components::Status::Warning.color();
            let all = frames(&a);
            let (_, shapes) =
                all.iter().find(|(d, _)| *d == DataDest::ByVenue).expect("the By-venue frame");
            let painted = words(shapes);
            let at = format!("{density:?} {text_size:?}");
            assert!(painted.iter().any(|(t, _)| t == "HISTORY"), "{at}: no HISTORY header");
            let flagged = "not known (unmeasured) \u{00B7} not mounted";
            assert!(
                painted.iter().any(|(t, c)| t == flagged && *c == warn),
                "{at}: binance's unmounted lane must read `{flagged}` in the warning colour: \
                 {painted:?}"
            );
            assert!(
                painted.iter().any(|(t, c)| t == "not known (unmeasured)" && *c != warn),
                "{at}: okx's mounted lane reads plain: {painted:?}"
            );
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The Data Manager's files — the two crates' halves spec §9 point 7 names. `data_body` dispatches
/// to each of them. `venues.rs`, the Venues destination, is migrated by THIS PR too (Task 11,
/// owner ruling 2026-09-29) but does not join this list: its disabled switch buttons carry no
/// reason yet, which would fail the disable-reason gate every file here already passes.
const DATA_MANAGER_FILES: [&str; 6] = [
    "crates/vike-app-core/src/ui/tool_views/data.rs",
    "crates/vike-app-core/src/ui/tool_views/data_rail.rs",
    "crates/vike-app-core/src/ui/tool_views/data_screens.rs",
    "crates/vike-app-core/src/ui/tool_views/stored.rs",
    "crates/vike-app-core/src/ui/tool_views/instruments.rs",
    "crates/vike-data-manager/src/view.rs",
];

/// `file:line: text` for every non-comment line of those files that contains `needle`.
fn lines_with(needle: &str) -> Vec<String> {
    let mut out = Vec::new();
    for rel in DATA_MANAGER_FILES {
        let text = std::fs::read_to_string(workspace_root().join(rel))
            .unwrap_or_else(|e| panic!("{rel}: {e}"));
        for (i, line) in text.lines().enumerate() {
            let l = line.trim_start();
            if !l.starts_with("//") && l.contains(needle) {
                out.push(format!("{rel}:{}: {l}", i + 1));
            }
        }
    }
    out
}

/// **No `palette` read is left** — the source half of the theme check, covering the paths the seed
/// never draws (spec §9 point 7: those constants are Graphite's values).
#[test]
fn the_data_manager_reads_no_palette_constant() {
    let hits: Vec<String> =
        ["palette::", "ui_theme::palette"].iter().flat_map(|n| lines_with(n)).collect();
    assert!(hits.is_empty(), "read a theme token or components::Status instead: {hits:#?}");
}

/// **Every disable goes through the kit.** `ActionButton::disabled_because` is the kit's only way to
/// disable a button, and it takes the reason (spec §4.2: a control with nothing behind it is
/// disabled and says why). egui's `add_enabled` takes no reason, so a dead control with no reason
/// is one call away from it.
#[test]
fn the_data_manager_disables_controls_only_through_the_kit() {
    let hits: Vec<String> =
        ["add_enabled", "set_enabled(", ".disable()"].iter().flat_map(|n| lines_with(n)).collect();
    assert!(hits.is_empty(), "disable through ActionButton::disabled_because: {hits:#?}");
}

/// **The rail's mapping, read off the tree.** Every destination appears once. The selected one is a
/// Label carrying its count, and the other eleven are buttons. A gap-free store draws no has-gaps
/// count. The row SHAPE is the kit's and is tested there; which rows exist, and what they say, is
/// the Data Manager's.
#[test]
fn the_rail_offers_every_destination_and_names_the_selected_one_as_a_label() {
    let counts = RailCounts { series: 430, gaps: 0, feeds: 14, datasets: 6, venues: 6 };
    let mut h = Harness::builder().with_size(egui::vec2(240.0, 720.0)).build_ui(move |ui| {
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        let mut dest = DataDest::AllSeries;
        rail(ui, &mut dest, &counts);
    });
    h.run();
    let named = |role: Role| -> Vec<String> {
        h.root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == role)
            .map(|n| {
                let a = n.accesskit_node();
                a.label()
                    .map(|s| s.to_string())
                    .or_else(|| a.value().map(|s| s.to_string()))
                    .unwrap_or_default()
            })
            .collect()
    };
    let labels = named(Role::Label);
    assert_eq!(labels.iter().filter(|l| *l == "All series 430").count(), 1, "{labels:?}");
    let buttons = named(Role::Button);
    for d in DataDest::ALL.into_iter().filter(|d| *d != DataDest::AllSeries) {
        assert_eq!(
            buttons.iter().filter(|b| b.starts_with(d.label())).count(),
            1,
            "{d:?} offered once: {buttons:?}"
        );
    }
    assert!(buttons.iter().any(|b| b == "Has gaps"), "no count on a gap-free store: {buttons:?}");
    assert!(buttons.iter().any(|b| b == "Cached feeds 14"), "{buttons:?}");
}
