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
use vike_app_core::backend::connection::BackendAction;
use vike_app_core::backend::editor::{BackendEditorState, BackendRegistryUpdate};
use vike_app_core::backend::registry::BackendRegistry;
use vike_app_core::data::history_column::HistoryChannelColumnLoad;
use vike_app_core::ui::windows::WindowView;
use vike_app_core::ui::windows::data_manager::rail::{DataManagerRailCounts, rail};
use vike_app_core::ui::windows::{
    BackendPickerSources, BackendSettingsEditState, BackendSettingsState,
    BackendSettingsWriteRequest, DataManagerDestination, data_tool_content,
};
use vike_data::datasets::{SymbolSet, SymbolSetStore};
use vike_data::{InstrumentCoverageKey, InstrumentCoveragePartialDay, SeriesCoverage, SeriesId};
use vike_model::StoreKindId;
use vike_ui_theme::appearance::{self, ThemeAppearance};
use vike_ui_theme::harness::{self, tree_node_texts};
use vike_ui_theme::metrics::ThemeDensity;
use vike_ui_theme::theme::{ThemePalette, ThemePaletteId};
use vike_ui_theme::type_scale::ThemeTextSize;

use crate::tool_context::Owned;

/// The Gateway destination's grid for these screens: the whole roster, the first venue's
/// DEFAULT account holding a DEMO and a LIVE row and no credential — so the destination draws its
/// edit controls (the account rows a credential can be written to, decision 0156 item 6) with
/// nothing stored.
pub(crate) fn credentials_grids() -> vike_account_manager::AccountManagerGrids {
    let venue = vike_model::VENUES[0];
    let row = |id: i64, tier: vike_model::AccountTier| vike_secrets::AccountRow {
        id,
        venue: venue.to_string(),
        tier,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: vike_secrets::fixture_account_symbol(venue).map(str::to_string),
        facts: Default::default(),
    };
    let dir = vike_secrets::account_directory::AccountDirectory::from_rows(
        vike_secrets::AccountRows::Known(vec![
            row(1, vike_model::AccountTier::Demo),
            row(2, vike_model::AccountTier::Live),
        ]),
        Some(Default::default()),
    );
    vike_account_manager::AccountManagerGrids::from_directory(&dir, &HashMap::new())
}

const DAY: i64 = 86_400_000;

impl Owned {
    /// Everything `WindowToolContext` borrows, owned, so every pass can build one. The store is small, but it
    /// reaches every branch a destination draws a colour from:
    /// - two venues, one series with a gap and one stale series;
    /// - a partial day;
    /// - two live feeds;
    /// - a DataSet with a stored and an unstored member;
    /// - a feed status.
    pub(crate) fn seeded() -> Self {
        let sid =
            |v: &str, s: &str| SeriesId::per_symbol(StoreKindId::Bar, v, s, Some("1m".to_string()));
        let coverage = |last_day: i64| SeriesCoverage {
            first_timestamp: 0,
            last_timestamp: last_day * DAY,
            rows: 1_000,
            bytes: 64_000,
            parts: 1,
            dates: 1,
        };
        let tree = vike_data_manager::build_inventory_tree(vec![
            (sid("binance", "BTCUSDT"), coverage(100)),
            // 80 days behind a 100-day window: stale.
            (sid("binance", "ETHUSDT"), coverage(20)),
            (sid("okx", "BTC-USDT"), coverage(99)),
        ]);
        let mut gaps = vike_data_manager::DataManagerInventoryGapMap::new();
        gaps.insert(
            vike_data_manager::DataManagerInventorySeriesKey {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                kind: StoreKindId::Bar,
                interval: Some("1m".into()),
                grouped: false,
                source: None,
            },
            vec![(10 * DAY, 12 * DAY)],
        );
        let mut partials = vike_data_manager::DataManagerInventoryPartialDayMap::new();
        partials.insert(
            InstrumentCoverageKey { venue: "okx".into(), label: "BTC-USDT".into(), grouped: false },
            vec![InstrumentCoveragePartialDay { day: 50, missing_kinds: vec![StoreKindId::Quote] }],
        );
        Owned {
            symbol_set_store: SymbolSetStore {
                sets: vec![SymbolSet {
                    name: "Majors".into(),
                    symbols: vec!["BTCUSDT".into(), "SOLUSDT".into()],
                    provider: "binance".into(),
                    interval: "1m".into(),
                    benchmark: String::new(),
                    user: true,
                }],
                writable: true,
            },
            tree,
            gaps,
            partials,
            feeds: vec![
                ("binance:BTCUSDT@1m".into(), 1_440, DAY, 2 * DAY),
                ("binance:ETHUSDT@1m".into(), 60, DAY, DAY + 3_600_000),
            ],
            statuses: HashMap::from([(
                "binance".to_string(),
                Arc::new(Mutex::new(vike_model::feed_status::DataClientStatus::new(
                    vike_model::feed_status::DataClientConnectionState::Connected,
                    "connected",
                ))),
            )]),
            history: Some(planted_history()),
            ..Owned::new(vike_execution::CoreSnapshot::empty("", ""))
        }
    }
}

/// A SERVED history-channels answer for the By-venue table's HISTORY column: this build's own rows
/// standing in for a server's, every built lane mounted except BINANCE's kline lane — so one stored
/// venue's cell carries the warning flag and the other's does not.
fn planted_history() -> HistoryChannelColumnLoad {
    let mut load =
        HistoryChannelColumnLoad::served(HistoryChannelColumnLoad::compiled(100 * DAY).report);
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

/// A window on `destination`, with a series opened (so the inspector draws its details), a feed selected,
/// a log line, and the first-shown store load already asked for.
pub(crate) fn window_on(destination: DataManagerDestination) -> WindowView {
    WindowView {
        data_destination: destination,
        stored_last_selection: Some(vike_data_manager::DataManagerInventorySeriesKey {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            kind: StoreKindId::Bar,
            interval: Some("1m".into()),
            grouped: false,
            source: None,
        }),
        data_selection: Some("binance:BTCUSDT@1m".into()),
        data_log: vec!["12:00:00  Refreshed · 2 series".into()],
        stored_auto_requested: Some(vike_app_core::backend::registry::device_key_epoch()),
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

/// One pass. Texture uploads are discarded (a headless pass has no backend to apply them to, and
/// epaint 0.36 panics on dropping unapplied ones), the geometry is asserted sane, and the shapes
/// are flattened.
fn run(context: &egui::Context, frame: u32, add: impl FnMut(&mut egui::Ui)) -> Vec<Shape> {
    let mut full = context.run_ui(raw(frame), add);
    full.textures_delta.clear();
    vike_ui_theme::frame_sanity::assert_frame_sane(&full);
    vike_ui_theme::painted::flatten(std::mem::take(&mut full.shapes))
}

/// Every destination's shapes under `a`:
/// - one context, with the appearance installed and one empty pass behind it (the fonts land on
///   the next pass);
/// - then two passes per destination on a fresh window. The second pass is the one read, once the
///   first has settled the layout.
fn frames(a: &ThemeAppearance) -> Vec<(DataManagerDestination, Vec<Shape>)> {
    frames_on(a, None)
}

/// [`frames`] with `datahub_address` as the RESOLVED datahub address (`WindowToolContext::execution_history`'s), which is
/// what decides the mount the store bar, the Store screen and the providers name.
fn frames_on(
    a: &ThemeAppearance,
    datahub_address: Option<&'static str>,
) -> Vec<(DataManagerDestination, Vec<Shape>)> {
    let owned = Owned::seeded();
    let context = egui::Context::default();
    appearance::install(&context, a);
    run(&context, 0, |_| {});
    // The ten params `DataManagerDestination::Gateway`/`DataManagerDestination::Backend` need and no other destination
    // does — see `data_body`'s doc. An empty registry and no active backend renders the same
    // "no backends configured" / idle-settings state `data_manager_backend_strip.rs`'s own
    // harnesses use;
    // nothing here asserts on their content, only that every destination paints sane geometry.
    let backends = BackendRegistry::default();
    let grids = credentials_grids();
    let settings = BackendSettingsState::Idle;
    let mut out = Vec::new();
    for (i, destination) in DataManagerDestination::ALL.into_iter().enumerate() {
        let mut tv = window_on(destination);
        let picker = BackendPickerSources { backends: &backends, active: None, reported: None };
        let mut action: Option<BackendAction> = None;
        let mut editor = BackendEditorState::Closed;
        let mut registry: Option<BackendRegistryUpdate> = None;
        let mut settings_refresh = false;
        let mut settings_edit = BackendSettingsEditState::Idle;
        let mut settings_write: Option<BackendSettingsWriteRequest> = None;
        let mut shapes = Vec::new();
        for pass in 0..2 {
            let frame = (1 + 2 * i + pass) as u32;
            shapes = run(&context, frame, |ui| {
                let mut tool_context = owned.context();
                tool_context.execution_history.datahub_address = datahub_address;
                data_tool_content(
                    ui,
                    &tool_context,
                    &mut tv,
                    &grids,
                    &picker,
                    &mut action,
                    &mut editor,
                    &mut registry,
                    &settings,
                    &mut settings_refresh,
                    &mut settings_edit,
                    &mut settings_write,
                )
            });
        }
        out.push((destination, shapes));
    }
    out
}

/// ⚠ A series whose gap probe failed is not a whole one and not a gapped one: Has gaps and the foot of
/// the window say how many, and the count of series with gaps leaves it out.
#[test]
fn a_failed_gap_probe_is_said_and_is_not_counted_as_a_hole() {
    let mut owned = Owned::seeded();
    owned.gaps.insert(
        vike_data_manager::DataManagerInventorySeriesKey {
            venue: "okx".into(),
            symbol: "BTC-USDT".into(),
            kind: StoreKindId::Bar,
            interval: Some("1m".into()),
            grouped: false,
            source: None,
        },
        Vec::new(),
    );
    let counts = DataManagerRailCounts::from_context(&owned.context());
    assert_eq!((counts.gaps, counts.gaps_unknown), (1, 1));

    let context = egui::Context::default();
    appearance::install(&context, &ThemeAppearance::default());
    run(&context, 0, |_| {});
    let backends = BackendRegistry::default();
    let grids = credentials_grids();
    let settings = BackendSettingsState::Idle;
    let mut tv = window_on(DataManagerDestination::HasGaps);
    let picker = BackendPickerSources { backends: &backends, active: None, reported: None };
    let mut action: Option<BackendAction> = None;
    let mut editor = BackendEditorState::Closed;
    let mut registry: Option<BackendRegistryUpdate> = None;
    let mut settings_refresh = false;
    let mut settings_edit = BackendSettingsEditState::Idle;
    let mut settings_write: Option<BackendSettingsWriteRequest> = None;
    let mut shapes = Vec::new();
    for frame in 1..3 {
        shapes = run(&context, frame, |ui| {
            data_tool_content(
                ui,
                &owned.context(),
                &mut tv,
                &grids,
                &picker,
                &mut action,
                &mut editor,
                &mut registry,
                &settings,
                &mut settings_refresh,
                &mut settings_edit,
                &mut settings_write,
            )
        });
    }
    let said: Vec<String> = words(&shapes).into_iter().map(|(text, _)| text).collect();
    let said = said.join(" ");
    assert!(said.contains("the gap check of 1 more series failed"), "Has gaps: {said}");
    assert!(said.contains("1 not checked for gaps"), "the foot: {said}");
    assert!(said.contains("1 with gaps"), "the foot's count leaves it out: {said}");
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
    let g = ThemePalette::of(ThemePaletteId::Graphite);
    vec![
        ("background", g.background),
        ("surface", g.surface),
        ("card", g.card),
        ("hover", g.hover),
        ("border", g.border),
        ("text", g.text),
        ("text_ui", g.text_ui),
        ("text_secondary", g.text_secondary),
        ("text_tertiary", g.text_tertiary),
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
    for theme in [ThemePaletteId::Midnight, ThemePaletteId::Dusk, ThemePaletteId::Carbon] {
        let own = ThemePalette::of(theme);
        for (destination, shapes) in
            frames(&ThemeAppearance { theme, ..ThemeAppearance::default() })
        {
            let seen = colours(&shapes);
            if !seen.contains(&own.surface) {
                findings.push(format!(
                    "{theme:?} {destination:?}: nothing painted in this theme's surface"
                ));
            }
            for (name, c) in graphite_only() {
                if seen.contains(&c) {
                    findings.push(format!("{theme:?} {destination:?}: Graphite {name} {c:?}"));
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
    for theme in ThemePaletteId::ALL {
        let accent = ThemePalette::of(theme).accent;
        for (destination, shapes) in
            frames(&ThemeAppearance { theme, ..ThemeAppearance::default() })
        {
            for (text, c) in words(&shapes) {
                if c == accent && !text.trim().is_empty() && !is_icon(&text) {
                    hits.push(format!("{theme:?} {destination:?}: {text:?}"));
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
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            for (destination, shapes) in
                frames(&ThemeAppearance { density, text_size, ..ThemeAppearance::default() })
            {
                let named = words(&shapes).iter().filter(|(t, _)| t == destination.label()).count();
                assert!(
                    named >= 2,
                    "{density:?} {text_size:?} {destination:?}: {named} copies of its name"
                );
            }
        }
    }
}

/// ⚠ **No destination names a local store, with a datahub resolved or without one.** The desktop has
/// had no local store engine since the fat build went: every series it shows is read from the
/// resolved datahub, and with none resolved it reads nothing. A "local store" mount, a "read + write"
/// access word or a "read from disk" row is a store the operator would look for and not find.
#[test]
fn no_destination_names_a_local_store() {
    for address in [None, Some("127.0.0.1:17878")] {
        for (destination, shapes) in frames_on(&ThemeAppearance::default(), address) {
            let painted = words(&shapes);
            assert!(
                painted.iter().all(|(t, _)| {
                    let t = t.to_lowercase();
                    !t.contains("local store") && !t.contains("from disk")
                }),
                "{destination:?} (datahub {address:?}) names a store that does not exist: {painted:?}"
            );
        }
    }
}

/// **The sidebar says nothing to the developer.** A "NO CEILING" footer under the rail told the owner
/// that "a thirteenth tab would be a redesign — which is how this window got to seven": design
/// commentary painted on every destination. No painted word of any destination may carry it again.
#[test]
fn the_sidebar_paints_no_design_commentary() {
    for (destination, shapes) in frames(&ThemeAppearance::default()) {
        let painted = words(&shapes);
        assert!(
            painted.iter().all(|(t, _)| {
                let t = t.to_lowercase();
                !t.contains("ceiling") && !t.contains("thirteenth") && !t.contains("redesign")
            }),
            "{destination:?} paints design commentary: {painted:?}"
        );
    }
}

/// **The By-venue table carries a HISTORY column** (the history-channels design's §3.1 and §4):
/// its header, a stored venue whose lane is not mounted drawn with that flag in the WARNING
/// colour, and one whose lane is mounted drawn plain — at every density and text size. What this
/// cannot prove is that the column is legible or fits; that is a look on a real GPU.
#[test]
fn the_by_venue_table_carries_the_history_column() {
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let a = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            let warn = vike_ui_theme::components::ThemeStatus::Warning.color();
            let all = frames(&a);
            let (_, shapes) = all
                .iter()
                .find(|(d, _)| *d == DataManagerDestination::ByVenue)
                .expect("the By-venue frame");
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

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-ops/tests/architecture/parity_fold_gate.rs`'s `repo_root`, do not.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The Data Manager's files — the two crates' halves spec §9 point 7 names. `data_body` dispatches
/// to each of them.
const DATA_MANAGER_FILES: [&str; 23] = [
    "crates/vike-app-core/src/ui/windows/data_manager/mod.rs",
    "crates/vike-app-core/src/ui/windows/backend/provider_keys.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/activity_log.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/cached_feeds.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/gaps.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/instruments.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/inventory/mod.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/inventory/inspector.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/inventory/polymarket_proxy.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/overview.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/providers.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/rail.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/store.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/symbol_sets/mod.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/symbol_sets/members.rs",
    "crates/vike-app-core/src/ui/windows/data_manager/venue_rollup.rs",
    "crates/vike-data-manager/src/inventory/filter.rs",
    "crates/vike-data-manager/src/inventory/grid.rs",
    "crates/vike-data-manager/src/inventory/list.rs",
    "crates/vike-data-manager/src/inventory/mod.rs",
    "crates/vike-data-manager/src/inventory/partial_days.rs",
    "crates/vike-data-manager/src/inventory/table.rs",
    "crates/vike-data-manager/src/polymarket_proxy.rs",
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
    assert!(hits.is_empty(), "read a theme token or components::ThemeStatus instead: {hits:#?}");
}

/// **Every disable goes through the kit.** `ThemeButtonAction::disabled_because` is the kit's only way to
/// disable a button, and it takes the reason (spec §4.2: a control with nothing behind it is
/// disabled and says why). egui's `add_enabled` takes no reason, so a dead control with no reason
/// is one call away from it.
#[test]
fn the_data_manager_disables_controls_only_through_the_kit() {
    let hits: Vec<String> =
        ["add_enabled", "set_enabled(", ".disable()"].iter().flat_map(|n| lines_with(n)).collect();
    assert!(hits.is_empty(), "disable through ThemeButtonAction::disabled_because: {hits:#?}");
}

/// **The rail's mapping, read off the tree.** Every destination appears once. The selected one is a
/// Label carrying its count, and the other eleven are buttons. A gap-free store draws no has-gaps
/// count. The row SHAPE is the kit's and is tested there; which rows exist, and what they say, is
/// the Data Manager's.
#[test]
fn the_rail_offers_every_destination_and_names_the_selected_one_as_a_label() {
    let counts = DataManagerRailCounts {
        series: 430,
        feeds: 14,
        symbol_sets: 6,
        venues: 6,
        ..Default::default()
    };
    let mut h = Harness::builder().with_size(egui::vec2(240.0, 720.0)).build_ui(move |ui| {
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        let mut destination = DataManagerDestination::AllSeries;
        rail(ui, &mut destination, &counts);
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
    for d in
        DataManagerDestination::ALL.into_iter().filter(|d| *d != DataManagerDestination::AllSeries)
    {
        assert_eq!(
            buttons.iter().filter(|b| b.starts_with(d.label())).count(),
            1,
            "{d:?} offered once: {buttons:?}"
        );
    }
    assert!(buttons.iter().any(|b| b == "Has gaps"), "no count on a gap-free store: {buttons:?}");
    assert!(buttons.iter().any(|b| b == "Cached feeds 14"), "{buttons:?}");
}

// -----------------------------------------------------------------------------------------------
// C1 regression: the real credential EDITOR, reached through the real top-level dispatch.
// -----------------------------------------------------------------------------------------------
//
// The defect these pinned was a full-height grid above the editor (the deleted venue-arming grid)
// whose inner `ScrollArea` swallowed the Gateway destination's fixed-height body, leaving the
// editor laid out below the visible region. The grid is gone; the subject that remains is the
// editor ALONE inside that fixed-height body: drawn through the SAME `data_tool_content` dispatch
// `frames()` calls, at both capture sizes, it must be rendered AND reachable by a real click, not
// merely present in the accessibility tree (a node can carry a bounding box and still sit outside
// the clip rect that would let a pointer event reach it — see
// `data_manager_credentials_panel.rs`'s identical concern).

/// Drives the REAL `data_tool_content` dispatch on `DataManagerDestination::Gateway`, at `size` — the
/// composition `crates/vike-desktop/src/tool_window.rs`'s `WindowKind::Data` arm builds every frame.
fn credentials_destination_harness(size: egui::Vec2) -> Harness<'static, ()> {
    destination_harness(size, window_on(DataManagerDestination::Gateway))
}

/// The REAL `data_tool_content` dispatch over the seeded store, on `tv`'s destination, at `size`.
fn destination_harness(size: egui::Vec2, mut tv: WindowView) -> Harness<'static, ()> {
    let owned = Owned::seeded();
    let backends = BackendRegistry::default();
    let grids = credentials_grids();
    let settings = BackendSettingsState::Idle;
    let mut action: Option<BackendAction> = None;
    let mut editor = BackendEditorState::Closed;
    let mut registry: Option<BackendRegistryUpdate> = None;
    let mut settings_refresh = false;
    let mut settings_edit = BackendSettingsEditState::Idle;
    let mut settings_write: Option<BackendSettingsWriteRequest> = None;

    // ⚠ `picker` borrows `backends`, so it is built INSIDE the closure, every frame, from the
    // `backends`/etc. this `move` closure captured by VALUE — not built once outside and moved in
    // already-borrowed, which would try to outlive the stack frame that owns `backends`.
    let mut h = Harness::builder().with_size(size).build_ui(move |ui| {
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        let picker = BackendPickerSources { backends: &backends, active: None, reported: None };
        data_tool_content(
            ui,
            &owned.context(),
            &mut tv,
            &grids,
            &picker,
            &mut action,
            &mut editor,
            &mut registry,
            &settings,
            &mut settings_refresh,
            &mut settings_edit,
            &mut settings_write,
        );
    });
    h.run();
    h
}

/// **The credential editor's pencil is on screen and a click on it actually opens the form**, at
/// both the Gateway destination's native capture size (`data_shot_size`'s `1200x700`, what
/// `VIKE_SHOT_WIN=gateway` captures) and the old standalone Connections window's compact
/// `760x620` footprint (what `05-gateway-compact` captures) — the two sizes C1's brief names.
#[test]
fn the_credentials_edit_button_is_reachable_at_the_native_and_old_window_sizes() {
    for size in [egui::vec2(1200.0, 700.0), egui::vec2(760.0, 620.0)] {
        let mut h = credentials_destination_harness(size);

        let pencils = harness::nodes(&h, Role::Button, "edit credentials");
        assert!(
            !pencils.is_empty(),
            "{size:?}: no edit-credentials button rendered at all: {}",
            tree_node_texts(&h)
        );
        // The real window's Gateway body is a SCROLLABLE region at this size (the C1 fix), so
        // a real operator reaches the pencil by scrolling to it, same as `scroll_to_me`'s genuine
        // `ScrollIntoView` accesskit action does — never `click_accesskit`, which would click
        // straight through a clip rect and prove nothing about reachability.
        pencils[0].scroll_to_me();
        drop(pencils);
        h.run();
        h.run();
        let pencils = harness::nodes(&h, Role::Button, "edit credentials");
        pencils[0].click();
        drop(pencils);
        h.run();
        h.run();

        let fields: Vec<_> = h
            .root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::PasswordInput)
            .collect();
        assert!(
            !fields.is_empty(),
            "{size:?}: clicking the pencil opened no form — it is in the tree but the click \
             reached nothing, which is what a clipped `interact_rect` looks like from here: {}",
            tree_node_texts(&h)
        );
    }
}

/// **`Add account` is on screen and a click on it actually opens the create-account form**, at
/// both sizes.
#[test]
fn the_credentials_add_account_button_is_reachable_at_the_native_and_old_window_sizes() {
    for size in [egui::vec2(1200.0, 700.0), egui::vec2(760.0, 620.0)] {
        let mut h = credentials_destination_harness(size);

        let add = harness::nodes(&h, Role::Button, "Add account");
        assert_eq!(
            add.len(),
            1,
            "{size:?}: expected exactly one Add account button: {}",
            tree_node_texts(&h)
        );
        add[0].scroll_to_me();
        drop(add);
        h.run();
        h.run();
        let add = harness::nodes(&h, Role::Button, "Add account");
        add[0].click();
        drop(add);
        h.run();
        h.run();

        let create = harness::nodes(&h, Role::Button, "Create");
        assert_eq!(
            create.len(),
            1,
            "{size:?}: clicking Add account opened no create-account form — it is in the tree but \
             the click reached nothing: {}",
            tree_node_texts(&h)
        );
    }
}

/// The Activity log's footnote sits ABOVE the window's foot line, not on it.
///
/// Found on a real-GPU capture of the running desktop (2026-10-05): the destination's log panel is a
/// `ScrollArea` with `auto_shrink([false, false])`, so it takes every pixel the body has left, and
/// the footnote drawn after it landed below the body's bottom edge — on top of the foot line's
/// "N series indexed" text. Measured here as geometry (the CPU computes layout, so this needs no
/// GPU): the footnote's bottom edge must not reach the foot line's top edge.
///
/// Reddens on the footnote being drawn after a panel that already claimed the whole remaining
/// height (the log panel's height reserved without the footnote's own).
#[test]
fn the_activity_log_footnote_sits_above_the_foot_line() {
    let all = frames(&ThemeAppearance::default());
    let (_, shapes) = all
        .iter()
        .find(|(d, _)| *d == DataManagerDestination::ActivityLog)
        .expect("frames() renders every destination, the Activity log included");
    let bounds_of = |needle: &str| -> egui::Rect {
        let found: Vec<egui::Rect> = shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Text(t) if t.galley.job.text.contains(needle) => {
                    Some(t.visual_bounding_rect())
                }
                _ => None,
            })
            .collect();
        assert_eq!(found.len(), 1, "expected exactly one text run holding {needle:?}: {found:?}");
        found[0]
    };
    let footnote = bounds_of("in-memory session log");
    let foot_line = bounds_of("series indexed");
    assert!(
        footnote.bottom() <= foot_line.top(),
        "the footnote ({footnote:?}) reaches into the foot line ({foot_line:?}): the log panel \
         claimed the footnote's height as well as its own"
    );
}

/// **The Overview lists the datahub's open split warning** in the owner's words (the split in words next to the
/// symbol, the refresh command), beside the Instruments screen that holds the venue's Refresh. The list is the view's
/// (`crates/vike-app-core/src/data/split_warnings.rs`), seeded here as a read that landed.
#[test]
fn the_overview_lists_an_open_split_warning() {
    use vike_app_core::data::split_warnings::SplitWarningsAnswer;
    let warning = vike_datahub_client::catalog::SplitWarning {
        venue: "dukascopy".into(),
        symbol: "NVDA.CFD".into(),
        date: "2026-11-20".into(),
        new_shares: 10,
        old_shares: 1,
        price_before: 1400.0,
        price_after: 140.0,
    };
    let mut tv = window_on(DataManagerDestination::Overview);
    let egui_context = egui::Context::default();
    let now = std::time::Instant::now();
    let landed: SplitWarningsAnswer = Ok(Some(vec![warning]));
    tv.split_warnings.poll("", now, true, Some(move || landed), &egui_context);
    let none: Option<fn() -> SplitWarningsAnswer> = None;
    let deadline = now + std::time::Duration::from_secs(10);
    while tv.split_warnings.poll("", std::time::Instant::now(), false, none, &egui_context) {
        assert!(std::time::Instant::now() < deadline, "the seeded read never landed");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let h = destination_harness(egui::vec2(1200.0, 700.0), tv);
    let texts = tree_node_texts(&h);
    assert!(
        texts.contains(
            "NVDA.CFD: looks like a 10-for-1 split on 2026-11-20 (price 1400 → 140); run vike-cli data \
             catalog refresh --venue dukascopy"
        ),
        "{texts}"
    );
    assert!(texts.contains("dukascopy / NVDA.CFD"), "{texts}");
}
