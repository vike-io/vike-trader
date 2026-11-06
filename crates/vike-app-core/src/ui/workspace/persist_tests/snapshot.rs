//! Snapshot round trips: capture/apply, the v1/v2 serde formats, retired and absent keys.

use super::*;

/// The `user_sized` LATCH survives a save/load, and a file written before the key existed
/// loads as UNLATCHED.
///
/// ⚠ The latched fixture is built by HAND (`w.user_sized = true`), because `capture` only
/// takes `WinKind::Chart` windows and a chart is a `fills` kind `show_window` never lets
/// latch — so no fixture reachable through the UI could set it today. What is gated here is
/// the PLUMBING: field → capture → JSON → apply, which is what was missing, and which is what
/// would otherwise silently drop the value the day a tool window is captured.
///
/// MUTATIONS that must redden this: delete `user_sized: w.user_sized` from `capture` (or
/// `w.user_sized = s.user_sized` from `apply`) → the first half fails; change the field's
/// `#[serde(default)]` to a `default = ` that yields `true` → the second half fails.
#[test]
fn the_user_sized_latch_round_trips_and_an_old_file_loads_unlatched() {
    let mut wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    wins[0].user_sized = true;
    let ws = capture(&wins, DisplayTz::Local, false);
    assert!(ws.wins[0].user_sized, "the latch must reach the snapshot");

    // ...through real JSON, not just the in-memory structs: the key has to be WRITTEN.
    let json = serde_json::to_string(&ws).expect("a workspace serializes");
    assert!(json.contains("\"user_sized\":true"), "the key must be written: {json}");
    let reloaded: Workspace = serde_json::from_str(&json).expect("...and parses back");
    assert!(apply(&reloaded)[0].user_sized, "a latched window must reload LATCHED");

    // A PRE-FEATURE file — the same JSON with the key removed — must load as unlatched rather
    // than failing to parse or coming back latched.
    let older = json.replace("\"user_sized\":true,", "");
    assert!(!older.contains("user_sized"), "the probe must actually remove the key: {older}");
    let older: Workspace =
        serde_json::from_str(&older).expect("a pre-feature file must still parse");
    assert!(!older.wins[0].user_sized, "an absent key loads as UNLATCHED");
    assert!(!apply(&older)[0].user_sized, "...and restores as an egui-sized window");
}

#[test]
fn capture_apply_round_trip() {
    let mut wins = vec![
        chart_win("BTCUSDT", ChartStyle::Candles),
        chart_win("ETHUSDT", ChartStyle::HeikinAshi),
    ];
    wins[0].sync_group = Some(2); // task B8: one grouped, one ungrouped — both must round-trip
    // SP2 orderflow (Task 7): one window with orderflow on + a pinned tick size, one
    // left at the all-default off state — both shapes must round-trip.
    wins[0].cvd_on = true;
    wins[0].profile_on = true;
    wins[0].of_tick_size = Some(2.5);
    // GPU Phase 2 Task 3: `gpu_render: true` proves this isn't just always reading back false.
    let ws = capture(&wins, DisplayTz::Utc, true);
    assert_eq!(ws.version, VERSION);
    assert_eq!(ws.display_tz, "UTC");
    assert!(ws.gpu_render);
    assert_eq!(ws.wins.len(), 2);
    assert_eq!(ws.wins[0].symbol, "BTCUSDT");
    assert_eq!(ws.wins[0].pos, (40.0, 30.0));
    assert_eq!(ws.wins[0].size, (700.0, 440.0));
    assert!(!ws.wins[0].options.show_volume);
    assert!(!ws.wins[0].follow);
    assert_eq!(ws.wins[0].sync_group, Some(2));
    assert_eq!(ws.wins[1].sync_group, None);
    assert!(ws.wins[0].cvd_on);
    assert!(ws.wins[0].profile_on);
    assert_eq!(ws.wins[0].of_tick_size, Some(2.5));
    assert!(!ws.wins[1].cvd_on);
    assert!(!ws.wins[1].profile_on);
    assert_eq!(ws.wins[1].of_tick_size, None);
    let inds: Vec<(&str, bool)> =
        ws.wins[0].indicators.iter().map(|i| (i.name.as_str(), i.visible)).collect();
    assert_eq!(inds, vec![("sma", true), ("macd", false)]);

    // JSON round-trip is lossless
    let json = serde_json::to_string(&ws).unwrap();
    let back: Workspace = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ws);

    // apply → capture is a fixed point
    let rebuilt = apply(&back);
    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[0].symbol, "BTCUSDT");
    assert_eq!(rebuilt[1].style, ChartStyle::HeikinAshi);
    assert_eq!(rebuilt[0].indicators.len(), 2);
    assert!(!rebuilt[0].indicators[1].visible);
    assert!(!rebuilt[0].options.show_volume);
    assert!(!rebuilt[0].follow.on);
    assert_eq!(rebuilt[0].sync_group, Some(2));
    assert_eq!(rebuilt[1].sync_group, None);
    assert!(rebuilt[0].cvd_on);
    assert!(rebuilt[0].profile_on);
    assert_eq!(rebuilt[0].of_tick_size, Some(2.5));
    assert!(!rebuilt[1].cvd_on);
    assert!(!rebuilt[1].profile_on);
    assert_eq!(rebuilt[1].of_tick_size, None);
    assert_eq!(capture(&rebuilt, DisplayTz::Utc, ws.gpu_render), ws);
}

/// Decision 0117: a retired key just stops being read. A `workspace.json` saved while the chart's
/// backfill-hours knob existed carries `"of_backfill_hours"`; it must still LOAD with every other
/// global and window intact, loading must not touch the file, and the key must not come back out
/// when the workspace is serialized again — the operator's file loses it only when HE next saves.
#[test]
fn an_old_workspace_with_the_retired_backfill_hours_key_still_loads() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let p = tmp.path().join("workspace.json");
    let old = format!(
        r#"{{"version":{VERSION},"display_tz":"UTC","of_backfill_hours":5.0,"gpu_render":true,
            "indicator_favs":["rsi"],"wins":[{{"kind":"chart","symbol":"ETHUSDT","interval":"5m",
            "style":0,"pos":[40.0,30.0],"size":[700.0,440.0],"open":true,"minimized":false,
            "maximized":false,"show_volume":false,"follow":false,"auto_y":true,"indicators":[]}}]}}"#
    );
    std::fs::write(&p, &old).unwrap();

    let ws = load_from(&p).expect("an old file carrying the retired key must still load");
    assert_eq!(ws.display_tz, "UTC");
    assert!(ws.gpu_render);
    assert_eq!(ws.indicator_favs, vec!["rsi".to_string()]);
    let wins = apply(&ws);
    assert_eq!(wins.len(), 1);
    assert_eq!((wins[0].symbol.as_str(), wins[0].interval.as_str()), ("ETHUSDT", "5m"));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), old, "loading never rewrites the file");

    let resaved = serde_json::to_string(&ws).unwrap();
    assert!(!resaved.contains("of_backfill_hours"), "the retired key is written back: {resaved}");
}

#[test]
fn tool_windows_are_skipped() {
    let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 200.0));
    let wins = vec![
        chart_win("BTCUSDT", ChartStyle::Candles),
        WinState::new("trade", "", "", WinKind::Trade, r),
    ];
    assert_eq!(capture(&wins, DisplayTz::Local, false).wins.len(), 1);
}

#[test]
fn unknown_style_and_indicator_degrade_gracefully() {
    let ws = Workspace {
        version: VERSION,
        display_tz: "Local".into(),
        gpu_render: false,
        indicator_favs: Vec::new(),
        wins: vec![WinSnap {
            kind: "chart".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            venue: "binance".into(),
            asset_class: None,
            style: 999, // out of range → Candles
            pos: (0.0, 0.0),
            size: (100.0, 100.0),
            open: true,
            minimized: false,
            maximized: false,
            user_sized: false,
            follow: true,
            auto_y: true,
            scale: ScaleMode::default(),
            invert: false,
            options: ChartOptions::default(),
            panes: Vec::new(),
            pane_membership: Vec::new(),
            sub_pane_order: Vec::new(),
            sync_group: None,
            cvd_on: false,
            profile_on: false,
            of_tick_size: None,
            compare: Vec::new(),
            series_panes: Vec::new(),
            series_scale: Vec::new(),
            indicators: vec![IndSnap {
                name: "no-such-indicator".into(),
                visible: true,
                params: Vec::new(),
                lines: Vec::new(),
                source: None,
            }],
        }],
    };
    let rebuilt = apply(&ws);
    assert_eq!(rebuilt[0].style, ChartStyle::Candles);
    assert!(rebuilt[0].indicators.is_empty()); // unknown name silently dropped
}

/// (a) v1 forward-compat: a checked-in v1 JSON (version 1, top-level
/// NON-default `show_volume:false`, an indicator with NO `params`/`lines`)
/// must still load under the v2 structs — `show_volume` flows into the
/// flattened `options.show_volume`, and every ABSENT `ChartOptions` field
/// takes its CUSTOM `Default` value (not a type-zero). This is the
/// flatten + struct-level `#[serde(default)]` gate.
#[test]
fn v1_json_loads_with_flatten_and_custom_defaults() {
    const V1: &str = r#"{
            "version": 1,
            "wins": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "style": 0,
                "pos": [40.0, 30.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "show_volume": false,
                "follow": false,
                "auto_y": true,
                "indicators": [{ "name": "sma", "visible": true }]
            }]
        }"#;
    let ws: Workspace =
        serde_json::from_str(V1).expect("a v1 JSON must parse under the v2 structs");
    // task A6: a pre-A6 file has no `display_tz` key — must default to "Local", never brick.
    assert_eq!(ws.display_tz, "Local");
    // GPU Phase 2 Task 3: a pre-Task-3 file has no `gpu_render` key — must default to false.
    assert!(!ws.gpu_render);
    let win = &ws.wins[0];
    // the v1 top-level show_volume flowed into the flattened options
    assert!(!win.options.show_volume, "v1 show_volume:false must survive the flatten");
    // absent color/flag/precision fields defaulted to the CUSTOM values, not zeros
    let d = ChartOptions::default();
    assert_eq!(win.options.up, None, "an absent colour loads unset — it follows the appearance");
    assert_eq!(win.options.down, None);
    assert_eq!(win.options.cross, None);
    assert_eq!(win.options.precision, d.precision);
    assert!(win.options.show_grid, "absent show_grid must default on");
    // absent scale/panes default; the indicator loads with default (empty) params
    assert_eq!(win.scale, ScaleMode::Linear);
    assert!(win.panes.is_empty());
    assert!(win.indicators[0].params.is_empty());
    assert!(win.indicators[0].lines.is_empty());
    // task B8: absent `sync_group` key defaults to None (ungrouped) — a v1 file
    // predates sync groups entirely, so this exercises the same forward-compat path.
    assert_eq!(win.sync_group, None);
    // SP2 orderflow (Task 7): absent `cvd_on`/`profile_on`/`of_tick_size` keys default to
    // false/false/None — a v1 file predates orderflow entirely, same forward-compat path.
    assert!(!win.cvd_on);
    assert!(!win.profile_on);
    assert_eq!(win.of_tick_size, None);
    // Cross-venue: a v1 file predates the `venue` key entirely, so it defaults to Binance —
    // every existing workspace restores byte-identically to the single-venue behavior.
    assert_eq!(win.venue, "binance");

    // and apply carries the custom defaults onto a real WinState
    let rebuilt = apply(&ws);
    assert!(!rebuilt[0].options.show_volume);
    assert_eq!(rebuilt[0].options.up, None);
    assert_eq!(rebuilt[0].scale, ScaleMode::Linear);
    assert_eq!(rebuilt[0].sync_group, None);
    assert!(!rebuilt[0].cvd_on);
    assert!(!rebuilt[0].profile_on);
    assert_eq!(rebuilt[0].of_tick_size, None);
    assert_eq!(rebuilt[0].venue, "binance"); // default venue on a real WinState too
}

/// (b) v2 fixed point ACROSS A UID CHANGE: a window with a non-default scale,
/// custom `ChartOptions`, a dragged `PaneFractions` (incl. a Study pane) and an
/// oscillator carrying non-default params + a per-line paint edit survives
/// capture→serialize→parse→apply→capture unchanged. The `apply` step rebuilds
/// indicators through `add_indicator` (assigning NEW uids), so an equal
/// re-capture proves the Study-pane index↔uid translation survives a uid change
/// — persisting uids verbatim would re-capture a DIFFERENT Study key and fail.
#[test]
fn v2_round_trip_is_a_fixed_point_across_a_uid_change() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("fp", "BTCUSDT", "1m", WinKind::Chart, r);
    // non-default scale + custom options (custom up color, precision, volume off)
    w.scale = ScaleMode::Log;
    w.options.up = Some([1, 2, 3]);
    w.options.precision = Some(4);
    w.options.show_volume = false;
    // Seed the oscillator at a NON-zero uid so a fresh rebuild (uids from 0)
    // genuinely changes it: add a throwaway overlay (uid 0), the oscillator
    // (uid 1), then drop the overlay → indicators == [rsi @ uid 1].
    w.add_indicator("sma", &[]); // overlay, uid 0
    w.add_indicator("rsi", &[]); // oscillator, uid 1
    let osc_uid = w.indicators.last().unwrap().uid;
    assert_eq!(osc_uid, 1, "precondition: oscillator seeded at uid 1");
    w.remove_indicator(0);
    assert_eq!(w.indicators.len(), 1, "only the oscillator remains");
    // non-default params + a per-line paint edit on the oscillator
    w.indicators[0].set_params(vec![9.0], &[]);
    w.indicators[0].outputs[0].color = egui::Color32::from_rgb(7, 8, 9);
    w.indicators[0].outputs[0].width = 3.25;
    // a dragged pane layout referencing the oscillator by its (uid 1) PaneKey
    let present = [PaneKey::Price, PaneKey::Study(osc_uid)];
    w.panes.layout(&present, 500.0, 44.0);
    w.panes.drag(&present, 0, 40.0, 500.0, 44.0);

    let first = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    // the captured pane's Study is keyed by INDEX 0, NOT the uid (1)
    assert!(
        first.wins[0].panes.iter().any(|(p, _)| *p == PaneKey::Study(0)),
        "osc pane must be stored by index 0, got {:?}",
        first.wins[0].panes
    );
    assert!(
        !first.wins[0].panes.iter().any(|(p, _)| *p == PaneKey::Study(osc_uid)),
        "osc pane must NOT be stored by the volatile uid {osc_uid}"
    );

    let json = serde_json::to_string(&first).unwrap();
    let parsed: Workspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    // the rebuilt oscillator got a DIFFERENT uid than the original (0 vs 1)
    let new_uid = rebuilt[0].indicators[0].uid;
    assert_ne!(new_uid, osc_uid, "rebuild must reassign the oscillator uid");

    let second = capture(&rebuilt, DisplayTz::Local, false);
    assert_eq!(second.wins[0].scale, first.wins[0].scale);
    assert_eq!(second.wins[0].options, first.wins[0].options);
    assert_eq!(second.wins[0].panes, first.wins[0].panes);
    assert_eq!(second.wins[0].indicators, first.wins[0].indicators);
    assert_eq!(second, first, "full v2 round-trip must be a fixed point");
}

/// Foreign-source study (TradingView "symbol" input): a study's `(venue, symbol)`
/// source round-trips capture→JSON→apply, and a study WITHOUT a source captures no
/// `source` key at all (skip_serializing_if) and rebuilds as `None` — the
/// byte-identical same-symbol default.
#[test]
fn foreign_source_symbol_round_trips_and_defaults_to_none() {
    use vike_chart::indicators::SourceSymbol;
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("src", "BTCUSDT", "1m", WinKind::Chart, r);
    w.add_indicator("sma", &[]); // overlay, computes off a FOREIGN symbol
    w.add_indicator("rsi", &[]); // oscillator, stays on the primary (no source)
    w.indicators[0].source_symbol =
        Some(SourceSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() });

    let cap = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    // the sourced study carries the pair; the primary-sourced study carries None
    assert_eq!(cap.wins[0].indicators[0].source, Some(("bybit".into(), "ETHUSDT".into())));
    assert_eq!(cap.wins[0].indicators[1].source, None);

    let json = serde_json::to_string(&cap).unwrap();
    // a None source emits NO `source` key (skip_serializing_if) — save stays clean
    // and byte-identical to a pre-feature file for ordinary studies.
    assert_eq!(json.matches("\"source\"").count(), 1, "only the sourced study writes a key");

    let rebuilt = apply(&serde_json::from_str::<Workspace>(&json).unwrap());
    assert_eq!(
        rebuilt[0].indicators[0].source_symbol,
        Some(SourceSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() }),
        "foreign source survives the rebuild",
    );
    assert_eq!(rebuilt[0].indicators[1].source_symbol, None, "primary study stays sourceless");

    // an OLD file (no `source` key on any indicator) loads every study as primary.
    let legacy = json.replace(",\"source\":[\"bybit\",\"ETHUSDT\"]", "");
    let old = apply(&serde_json::from_str::<Workspace>(&legacy).unwrap());
    assert!(old[0].indicators.iter().all(|a| a.source_symbol.is_none()));
}

/// TradingView "Invert scale": the persisted `invert` flag round-trips, and an
/// OLD file with no `invert` key defaults to `false` (byte-identical to
/// pre-invert), same `#[serde(default)]` discipline as `scale`.
#[test]
fn invert_flag_round_trips_and_defaults_to_false_when_absent() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("inv", "BTCUSDT", "1m", WinKind::Chart, r);
    assert!(!w.invert, "fresh window is not inverted");
    w.invert = true;
    w.scale = ScaleMode::Log; // invert is orthogonal to the mode
    let json =
        serde_json::to_string(&capture(std::slice::from_ref(&w), DisplayTz::Local, false)).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["wins"][0]
            .as_object()
            .unwrap()
            .contains_key("invert"),
        "invert must be a top-level key: {json}"
    );
    let parsed: Workspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    assert!(rebuilt[0].invert, "invert flag must survive the round-trip");
    assert_eq!(rebuilt[0].scale, ScaleMode::Log, "mode survives alongside invert");

    // an OLD file that predates the invert key loads false (serde default).
    let stripped = json.replace(",\"invert\":true", "");
    assert!(!stripped.contains("\"invert\""), "precondition: invert key removed");
    let old: Workspace = serde_json::from_str(&stripped).unwrap();
    assert!(!apply(&old)[0].invert, "absent invert key defaults to false");
}

/// (c) no duplicate/nested keys: the flattened `options.show_volume` must be
/// emitted EXACTLY once (not also as a standalone `WinSnap` key) and never as
/// a nested `options` object.
#[test]
fn v2_serialization_has_no_duplicate_or_nested_option_keys() {
    let mut wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    wins[0].options.up = Some([1, 2, 3]);
    let json = serde_json::to_string(&capture(&wins, DisplayTz::Local, false)).unwrap();
    // raw-string guard: one window → `show_volume` appears exactly once
    assert_eq!(
        json.matches("\"show_volume\"").count(),
        1,
        "show_volume must be emitted once (flattened), not duplicated: {json}"
    );
    let val: serde_json::Value = serde_json::from_str(&json).unwrap();
    let win = val["wins"][0].as_object().unwrap();
    assert!(win.contains_key("show_volume"), "flattened show_volume at top level");
    assert!(!win.contains_key("options"), "options must be flattened, not nested");
    // a couple of other flattened/added keys confirm the shape
    assert!(win.contains_key("up"), "flattened color key at top level");
    assert!(!win.contains_key("down"), "an unset colour is not written");
    assert!(win.contains_key("scale"));
    assert!(win.contains_key("panes"));
    assert!(win.contains_key("sync_group"), "task B8: sync_group is a top-level key");
}

/// A workspace saved before chart colours followed the appearance: every colour still at the old
/// compiled default loads UNSET, and the one the user picked stays theirs — through the real
/// `#[serde(flatten)]` path, not `ChartOptions` alone.
#[test]
fn a_workspace_saved_before_chart_colours_followed_the_appearance_keeps_only_the_picked_ones() {
    const SAVED: &str = r#"{
        "version": 2,
        "wins": [{
            "kind": "chart", "symbol": "BTCUSDT", "interval": "1m", "style": 0,
            "pos": [0.0, 0.0], "size": [700.0, 440.0], "open": true, "minimized": false,
            "maximized": false, "follow": true, "auto_y": true, "show_volume": true,
            "up": [91, 190, 145], "down": [217, 84, 88], "border_up": [91, 190, 145],
            "border_down": [217, 84, 88], "wick_up": [91, 190, 145], "wick_down": [217, 84, 88],
            "up_s": [64, 186, 80], "down_s": [248, 82, 73], "line": [64, 186, 80],
            "grid": [1, 2, 3], "cross": [154, 164, 177], "bg": [13, 17, 23], "bg_top": [22, 27, 36],
            "indicators": []
        }]
    }"#;
    let ws: Workspace = serde_json::from_str(SAVED).expect("a pre-PR-7 workspace parses");
    let o = &ws.wins[0].options;
    assert_eq!(o.grid, Some([1, 2, 3]), "a colour the user picked is kept");
    let unset = [
        o.up,
        o.down,
        o.border_up,
        o.border_down,
        o.wick_up,
        o.wick_down,
        o.up_s,
        o.down_s,
        o.line,
        o.cross,
        o.bg,
        o.bg_top,
    ];
    assert!(unset.iter().all(Option::is_none), "every old default loads unset: {o:?}");
}

/// An edited chart colour survives capture → JSON → apply; an unset one is never written.
#[test]
fn an_edited_chart_colour_survives_the_workspace_round_trip() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("rt", "BTCUSDT", "1m", WinKind::Chart, r);
    w.options.up = Some([1, 2, 3]);
    let json =
        serde_json::to_string(&capture(std::slice::from_ref(&w), DisplayTz::Local, false)).unwrap();
    assert!(json.contains("\"up\":[1,2,3]"), "{json}");
    assert!(!json.contains("\"down\""), "an unset colour is not written: {json}");
    let back = apply(&serde_json::from_str::<Workspace>(&json).unwrap());
    assert_eq!((back[0].options.up, back[0].options.down), (Some([1, 2, 3]), None));
}

/// (d) task A6: `display_tz` persists by NAME and round-trips through `DisplayTz::parse`
/// for a curated NAMED (non Local/UTC) zone — exercises the IANA-string arm, not just the
/// two hardcoded `"Local"`/`"UTC"` arms.
#[test]
fn display_tz_round_trips_through_name_and_parse() {
    let tz = DisplayTz::CURATED
        .into_iter()
        .find(|tz| matches!(tz, DisplayTz::Named(_)))
        .expect("CURATED has at least one Named zone");
    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    let ws = capture(&wins, tz, false);
    assert_eq!(ws.display_tz, tz.name());
    let json = serde_json::to_string(&ws).unwrap();
    let back: Workspace = serde_json::from_str(&json).unwrap();
    assert_eq!(DisplayTz::parse(&back.display_tz), tz);
}

/// (e) a pre-A6 workspace file (no `display_tz` key at all, not just v1's other-absent-
/// fields shape) loads with the default `"Local"` — never bricks an old save.
#[test]
fn missing_display_tz_key_defaults_to_local() {
    let json = format!(r#"{{"version":{VERSION},"wins":[]}}"#);
    let ws: Workspace =
        serde_json::from_str(&json).expect("a workspace with no display_tz key must parse");
    assert_eq!(ws.display_tz, "Local");
    assert_eq!(DisplayTz::parse(&ws.display_tz), DisplayTz::Local);
    // GPU Phase 2 Task 3: same "never bricks an old save" guard for `gpu_render`.
    assert!(!ws.gpu_render);
}
