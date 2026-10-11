use super::*;
use egui::{Rect, pos2, vec2};
use std::assert_matches;
use vike_chart::chart::MoveTarget;

/// ⚠⚠ **THE MIGRATION QUESTION, ANSWERED OUT LOUD.** When the root's declaration moves the
/// family, an operator who was relying on the working-directory walk now reads a DIFFERENT
/// `workspace.json`, `layouts/`, `last_layout.txt` and `backends.json` — a saved layout and a
/// whole backend registry appearing to vanish. Nothing here copies, moves or deletes a file
/// the operator wrote, so the ONLY thing standing between them and "my backends are gone" is
/// this sentence. Reddens on a relocation that goes quiet.
///
/// Pure inputs — planted directories, no declaration, no environment — so this runs beside the
/// process-wide declaration sequence in
/// `crates/vike-app-core/tests/workspace_state_dir_declaration.rs` without racing it.
#[test]
fn a_relocation_that_strands_a_family_member_says_so_and_names_both_directories() {
    let root = std::env::temp_dir().join(format!("vike-reloc-{}", std::process::id()));
    let old = root.join("old");
    let new = root.join("new");
    std::fs::create_dir_all(&old).expect("plant the old state dir");

    // Nothing at the old location: a move that strands nothing needs no warning.
    assert_eq!(
        relocation_notice(Some(&old), Some(&new)),
        None,
        "an empty old directory loses the operator nothing"
    );

    // …now plant a registry there. THIS is the measured failure's own file.
    std::fs::write(old.join(crate::backend::backend_registry::BACKENDS_FILE), "{}")
        .expect("plant a backends.json");
    let notice =
        relocation_notice(Some(&old), Some(&new)).expect("a stranded family member is reported");
    assert!(notice.contains(&old.display().to_string()), "names the OLD directory: {notice}");
    assert!(notice.contains(&new.display().to_string()), "names the NEW directory: {notice}");
    assert!(
        notice.contains(crate::backend::backend_registry::BACKENDS_FILE),
        "…and names the member that was left behind: {notice}"
    );
    assert!(
        notice.contains("copy them across"),
        "…and says the copy is the operator's to make — no variable keeps the old answer: {notice}"
    );
    // ⚠ The file is still there afterwards. This workspace never relocates a file the operator
    // wrote — the notice is a report, not a migration.
    assert!(
        old.join(crate::backend::backend_registry::BACKENDS_FILE).exists(),
        "reporting a relocation must never move or delete anything"
    );

    // An answer that did not change says nothing, whatever sits in the directory.
    assert_eq!(relocation_notice(Some(&old), Some(&old)), None);
    assert_eq!(relocation_notice(None, None), None);
    let _ = std::fs::remove_dir_all(&root);
}

/// Every member of the family is in the list a relocation reports on — so a member added later
/// cannot be stranded silently.
#[test]
fn the_family_member_list_holds_every_file_this_module_resolves() {
    assert!(FAMILY_MEMBERS.contains(&WORKSPACE_FILE));
    assert!(FAMILY_MEMBERS.contains(&LAYOUTS_SUBDIR));
    assert!(FAMILY_MEMBERS.contains(&LAST_LAYOUT_FILE));
    assert!(
        FAMILY_MEMBERS.contains(&crate::backend::backend_registry::BACKENDS_FILE),
        "the registry reaches `base_dir` through this module, so it moves with the family"
    );
}

fn chart_win(sym: &str, style: ChartStyle) -> WinState {
    let r = Rect::from_min_size(pos2(40.0, 30.0), vec2(700.0, 440.0));
    let mut w = WinState::new(&format!("t-{sym}"), sym, "1m", WinKind::Chart, r);
    w.style = style;
    w.options.show_volume = false;
    w.follow.on = false;
    w.add_indicator("sma", &[]);
    w.add_indicator("macd", &[]);
    if let Some(a) = w.indicators.last_mut() {
        a.visible = false;
    }
    w
}

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

#[test]
fn disk_round_trip_and_corrupt_file() {
    // ⚠ The directory is UNIQUE per run, and that is a CI-reliability property rather than a
    // tidiness one. This test used a fixed `<temp>/vike-layout-test`; CI runs as one user and
    // agents as another ON THE SAME BOX, whichever creates the directory first OWNS it, and
    // every later `create_dir_all` under the other user then returns `PermissionDenied` —
    // forever, because nothing ever cleans `/tmp`. Reproduced on the CI box by pre-creating the
    // path as the other user. `crates/vike-ops/tests/hygiene/temp_path_gate.rs` is the gate.
    //
    // ⚠ The `TempDir` is BOUND, not discarded: `tempfile::tempdir().unwrap().path()` drops the
    // guard at the end of the statement and deletes the directory before the first write.
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();
    let p = dir.join("workspace.json");
    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    save_to(&wins, DisplayTz::Local, true, &[], &p).unwrap();
    let ws = load_from(&p).expect("file written above");
    assert_eq!(ws, capture(&wins, DisplayTz::Local, true));
    // corrupt file → None, never a panic (startup must survive it)
    std::fs::write(&p, "{not json").unwrap();
    assert!(load_from(&p).is_none());
    assert!(load_from(&dir.join("absent.json")).is_none());
    // No `remove_dir_all`: dropping `tmp` removes the tree, on the panic path too.
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
fn indicator_favs_round_trip_and_old_file_defaults_empty() {
    // Feature part (b): favourites written by `save_to` survive a disk round-trip,
    // and a file with NO `indicator_favs` key at all (pre-feature) loads empty.
    let dir = std::env::temp_dir().join(format!("vike-favs-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("workspace.json");
    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    let favs = vec!["rsi".to_string(), "macd".to_string()];

    save_to(&wins, DisplayTz::Local, false, &favs, &p).unwrap();
    let ws = load_from(&p).expect("file written above");
    assert_eq!(ws.indicator_favs, favs, "favourites round-trip verbatim, in order");

    // An OLD file with no `indicator_favs` key still parses, defaulting to empty.
    let raw = std::fs::read_to_string(&p).unwrap();
    assert!(raw.contains("indicator_favs"));
    let stripped: serde_json::Value = {
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v.as_object_mut().unwrap().remove("indicator_favs");
        v
    };
    let back: Workspace = serde_json::from_value(stripped).unwrap();
    assert!(back.indicator_favs.is_empty(), "pre-feature file ⇒ empty favourites");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn asset_class_round_trips_and_old_file_defaults_none() {
    // Feed-routing slice 1: `WinSnap::asset_class` is the persisted twin of
    // `WinState::asset_class`. A window with a derivative asset class must survive a
    // disk round-trip, AND an old file with no `asset_class` key at all (pre-feature)
    // must still load, defaulting to `None` (spot/legacy — byte-identical behavior).
    let dir = std::env::temp_dir().join(format!("vike-asset-class-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("workspace.json");

    let mut wins = vec![chart_win("BTC-USDT-SWAP", ChartStyle::Candles)];
    wins[0].venue = "okx".into();
    wins[0].asset_class = Some(AssetClass::CryptoPerp);
    save_to(&wins, DisplayTz::Local, false, &[], &p).unwrap();

    let ws = load_from(&p).expect("file written above");
    assert_eq!(ws.wins[0].asset_class, Some(AssetClass::CryptoPerp));
    let rebuilt = apply(&ws);
    assert_eq!(rebuilt[0].asset_class, Some(AssetClass::CryptoPerp));

    // An OLD file with no `asset_class` key at all still parses, defaulting to None.
    let raw = std::fs::read_to_string(&p).unwrap();
    assert!(raw.contains("asset_class"));
    let stripped: serde_json::Value = {
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["wins"][0].as_object_mut().unwrap().remove("asset_class");
        v
    };
    let back: Workspace = serde_json::from_value(stripped).unwrap();
    assert_eq!(back.wins[0].asset_class, None, "pre-feature file ⇒ None (spot/legacy)");

    // An ordinary spot window (asset_class: None) never writes the key at all
    // (`skip_serializing_if`), so an existing all-spot workspace file is untouched.
    let spot = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    let spot_json = serde_json::to_string(&capture(&spot, DisplayTz::Local, false)).unwrap();
    assert!(!spot_json.contains("asset_class"));

    let _ = std::fs::remove_dir_all(&dir);
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

/// (f) C1 Task 5: pane MEMBERSHIP round-trips across a uid reassignment, the
/// same way T9/T10's heights already did. Merge three oscillators into one
/// pane, capture→JSON→parse→apply (the rebuild reassigns fresh uids — proven
/// below, mirroring `v2_round_trip_is_a_fixed_point_across_a_uid_change`'s
/// throwaway-overlay trick), and assert the merge survives: all three
/// oscillators share ONE pane again, not three.
#[test]
fn c1_pane_membership_round_trips_across_uid_reassignment() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("pm", "BTCUSDT", "1m", WinKind::Chart, r);
    // A throwaway overlay first (uid 0, discarded before capture) means the
    // REBUILD — which only recreates persisted (surviving) indicators — hands
    // out uids that do NOT match this run's, proving the round-trip doesn't
    // depend on uid stability (same trick as the uid-change test above).
    w.add_indicator("sma", &[]); // overlay, uid 0
    w.add_indicator("rsi", &[]); // uid 1
    w.add_indicator("rsi", &[]); // uid 2
    w.add_indicator("rsi", &[]); // uid 3
    w.remove_indicator(0);
    let uids: Vec<u64> = w.indicators.iter().map(|a| a.uid).collect();
    assert_eq!(uids, vec![1, 2, 3], "precondition: three oscillators at uids 1..=3");

    // merge #2 and #3 into #1's pane, leaving one pane with all three members
    let p0 = w.study_pane[&uids[0]];
    w.move_study(uids[1], MoveTarget::Into(p0));
    w.move_study(uids[2], MoveTarget::Into(p0));
    assert_eq!(w.present_study_panes().len(), 1, "precondition: merged into one pane");

    let snap = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    let json = serde_json::to_string(&snap).unwrap();
    let parsed: Workspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    let w2 = &rebuilt[0];

    // the rebuild assigns DIFFERENT uids (no throwaway overlay this time)
    let new_uids: Vec<u64> = w2.indicators.iter().map(|a| a.uid).collect();
    assert_ne!(new_uids, uids, "rebuild must reassign oscillator uids");

    // membership survives: all three oscillators share ONE pane again
    assert_eq!(w2.present_study_panes().len(), 1);
    assert_eq!(w2.study_pane.len(), 3);
    let key = w2.present_study_panes()[0];
    assert!(w2.study_pane.values().all(|k| *k == key));
}

/// (g) C1 Task 5: an old snapshot predating pane membership (no
/// `pane_membership` key at all — the same "never bricks an old save" shape
/// as `v1_json_loads_with_flatten_and_custom_defaults`, but for a v2-shaped
/// file that already has `panes` heights from T9/T10) falls back to one pane
/// per oscillator, in add order — today's pre-Task-5 default layout — and a
/// re-capture reproduces the SAME ordinal-keyed heights the old file had,
/// proving old-file heights are preserved across the fallback.
#[test]
fn c1_old_snapshot_without_panes_field_loads_as_one_pane_per_oscillator() {
    const JSON: &str = r#"{
            "version": 2,
            "wins": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "style": 0,
                "pos": [0.0, 0.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "follow": false,
                "auto_y": true,
                "panes": [[{"Study":0}, 0.5], [{"Study":2}, 0.25]],
                "indicators": [
                    { "name": "rsi", "visible": true },
                    { "name": "rsi", "visible": true },
                    { "name": "rsi", "visible": true }
                ]
            }]
        }"#;
    let ws: Workspace =
        serde_json::from_str(JSON).expect("a v2 JSON with no pane_membership key must parse");
    assert!(
        ws.wins[0].pane_membership.is_empty(),
        "precondition: no pane_membership key in this fixture"
    );

    let rebuilt = apply(&ws);
    let w = &rebuilt[0];
    // fallback: each oscillator in its own pane, in order
    assert_eq!(w.present_study_panes().len(), 3);
    assert!(w.study_pane.values().all(|k| matches!(k, PaneKey::Study(_))));
    let distinct: std::collections::HashSet<PaneKey> = w.study_pane.values().copied().collect();
    assert_eq!(distinct.len(), 3, "one DISTINCT pane per oscillator, not merged");

    // old-file heights (already Study(osc index)-keyed under the default
    // one-pane-per-oscillator layout) still land on the right pane.
    let recaptured = capture(&rebuilt, DisplayTz::Local, false);
    let heights = &recaptured.wins[0].panes;
    assert!(heights.contains(&(PaneKey::Study(0), 0.5)));
    assert!(heights.contains(&(PaneKey::Study(2), 0.25)));
}

/// (h) C2b Task 8: compare symbols + own-pane placement + per-symbol scale
/// pin all round-trip together, the same way C1's pane membership survived a
/// uid reassignment (`c1_pane_membership_round_trips_across_uid_reassignment`).
/// A throwaway own-pane compare symbol is added then removed FIRST, consuming
/// `PaneKey::Series(1)`; the surviving own-pane symbol is then `Series(2)`, so
/// `apply`'s rebuild — which restarts `next_series_pane_id` fresh at 1 — is
/// proven to assign a DIFFERENT id, yet membership still lands correctly
/// because it is persisted as an ORDINAL into `compare`, not the volatile id.
#[test]
fn c2b_compare_series_pane_and_scale_round_trip_across_pane_id_reassignment() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("cs", "BTCUSDT", "1m", WinKind::Chart, r);

    // Throwaway own-pane symbol first — consumes PaneKey::Series(1), then is
    // removed entirely (cascades out of both `compare` and `series_pane`).
    w.add_compare("ADAUSDT");
    w.move_series_to_new_pane("ADAUSDT");
    w.remove_compare("ADAUSDT");

    // ETHUSDT: stays overlaid in the price pane (C2a default), explicitly
    // pinned Percent (also the default — proves a stored-but-default entry
    // round-trips too, not just a stored non-default one).
    w.add_compare("ETHUSDT");
    w.series_scale.insert("ETHUSDT".to_string(), ScaleAssign::Percent);
    // SOLUSDT: moved to its own pane, pinned Right.
    w.add_compare("SOLUSDT");
    w.move_series_to_new_pane("SOLUSDT");
    w.series_scale.insert("SOLUSDT".to_string(), ScaleAssign::Right);

    let original_pane = w.series_pane["SOLUSDT"];
    assert_eq!(original_pane, PaneKey::Series(2), "precondition: the throwaway pane consumed id 1");

    let snap = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    let win = &snap.wins[0];
    assert_eq!(win.compare, vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]);
    // SOLUSDT is compare-index 1 — the sole member of the one own-pane group.
    assert_eq!(win.series_panes, vec![vec![1]]);
    assert!(win.series_scale.contains(&("ETHUSDT".to_string(), ScaleAssign::Percent)));
    assert!(win.series_scale.contains(&("SOLUSDT".to_string(), ScaleAssign::Right)));

    let json = serde_json::to_string(&snap).unwrap();
    let parsed: Workspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    let w2 = &rebuilt[0];

    assert_eq!(w2.compare, vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]);
    assert!(!w2.series_pane.contains_key("ETHUSDT"), "ETHUSDT must stay overlaid");
    assert!(w2.series_pane.contains_key("SOLUSDT"), "SOLUSDT must be in its own pane");
    assert_eq!(w2.present_series_panes().len(), 1);
    let new_pane = w2.series_pane["SOLUSDT"];
    assert_ne!(new_pane, original_pane, "rebuild must reassign the series-pane id");
    assert_eq!(w2.series_scale_of("ETHUSDT"), ScaleAssign::Percent);
    assert_eq!(w2.series_scale_of("SOLUSDT"), ScaleAssign::Right);

    // fixed point: re-capture reproduces the same snapshot
    let second = capture(&rebuilt, DisplayTz::Local, false);
    assert_eq!(second.wins[0].compare, win.compare);
    assert_eq!(second.wins[0].series_panes, win.series_panes);
    assert_eq!(second.wins[0].series_scale, win.series_scale);
}

/// (i) C2b Task 8: an old (pre-C2b) v2 snapshot — no `compare`/`series_panes`/
/// `series_scale` keys at all — loads with zero compare overlays, the same
/// "never bricks an old save" shape as
/// `c1_old_snapshot_without_panes_field_loads_as_one_pane_per_oscillator`, and
/// re-capturing it reproduces the same empty shape (byte-identical to a chart
/// that predates the Compare feature entirely).
#[test]
fn c2b_old_snapshot_without_compare_fields_loads_with_no_overlays() {
    const JSON: &str = r#"{
            "version": 2,
            "wins": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "style": 0,
                "pos": [0.0, 0.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "follow": false,
                "auto_y": true,
                "indicators": []
            }]
        }"#;
    let ws: Workspace =
        serde_json::from_str(JSON).expect("a pre-C2b v2 JSON with no compare keys must parse");
    assert!(ws.wins[0].compare.is_empty(), "precondition: no compare key in this fixture");
    assert!(ws.wins[0].series_panes.is_empty());
    assert!(ws.wins[0].series_scale.is_empty());

    let rebuilt = apply(&ws);
    let w = &rebuilt[0];
    assert!(w.compare.is_empty(), "old file must load with zero compare overlays");
    assert!(w.series_pane.is_empty());
    assert!(w.present_series_panes().is_empty());
    assert_eq!(w.series_scale_of("ANYSYM"), ScaleAssign::Percent);

    // re-capture reproduces the same empty shape (byte-identical fixed point)
    let recaptured = capture(&rebuilt, DisplayTz::Local, false);
    assert!(recaptured.wins[0].compare.is_empty());
    assert!(recaptured.wins[0].series_panes.is_empty());
    assert!(recaptured.wins[0].series_scale.is_empty());
}

/// (j) C2 tidy FIX 2: a Series-pane HEIGHT survives capture→apply, the same way a
/// Study-pane height already does (`v2_round_trip_is_a_fixed_point_across_a_uid_change`)
/// and the way Series-pane MEMBERSHIP already did (test (h) above). Before this fix
/// `panes_to_snap`/`snap_to_panes` had a placeholder `PaneKey::Series(_) => None` arm (a
/// leftover from C2 Task 5, before `chart::draw` ever fed a live `Series` pane into
/// `w.panes`), so a dragged own-pane compare series always reloaded at the un-dragged
/// 0.16 default share. Uses the same throwaway-pane / pane-id-reassignment trick as test
/// (h) so this also proves the height round-trips by ORDINAL, not the volatile pane id.
#[test]
fn c2_series_pane_height_round_trips_across_pane_id_reassignment() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("sph", "BTCUSDT", "1m", WinKind::Chart, r);

    // Throwaway own-pane symbol first — consumes PaneKey::Series(1), then is removed
    // entirely (mirrors test (h)'s precondition setup).
    w.add_compare("ADAUSDT");
    w.move_series_to_new_pane("ADAUSDT");
    w.remove_compare("ADAUSDT");

    w.add_compare("SOLUSDT");
    w.move_series_to_new_pane("SOLUSDT");
    let pane = w.series_pane["SOLUSDT"];
    assert_eq!(pane, PaneKey::Series(2), "precondition: the throwaway pane consumed id 1");

    // Drag the pane's height away from its 0.16 default — the same `PaneFractions`
    // `layout`/`drag` calls `chart::draw`'s live separator-drag path makes, exercised
    // directly here like `v2_round_trip_is_a_fixed_point_across_a_uid_change` does for a
    // Study pane.
    let present = [PaneKey::Price, pane];
    w.panes.layout(&present, 500.0, 44.0);
    w.panes.drag(&present, 0, 40.0, 500.0, 44.0);

    let first = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    let stored = first.wins[0]
        .panes
        .iter()
        .find(|(p, _)| matches!(p, PaneKey::Series(_)))
        .copied()
        .expect("a Series pane height must be captured, not dropped");
    assert_eq!(stored.0, PaneKey::Series(0), "captured by ordinal, not the volatile pane id");
    assert!(
        (stored.1 - 0.16).abs() > 0.01,
        "height must be the DRAGGED value, not the 0.16 default, got {}",
        stored.1
    );

    let json = serde_json::to_string(&first).unwrap();
    let parsed: Workspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    let w2 = &rebuilt[0];
    let new_pane = w2.series_pane["SOLUSDT"];
    assert_ne!(new_pane, pane, "rebuild must reassign the series-pane id");

    // fixed point: re-capture reproduces the exact same (non-default) height
    let second = capture(&rebuilt, DisplayTz::Local, false);
    assert_eq!(
        second.wins[0].panes, first.wins[0].panes,
        "dragged Series-pane height must survive the round trip"
    );
}

/// (k) Chart single-max default: the authored UNIFIED sub-pane order (Volume
/// reordered BELOW a study pane) survives capture→apply, so the user's
/// arrangement of Volume/CVD relative to studies is persisted — not reset to
/// the old hardcoded Volume→CVD→studies sequence. Also proves `apply` does not
/// let a later `sync_sub_panes` re-prepend Volume (it stays where the user put
/// it, because it is already present in the restored order).
#[test]
fn sub_pane_order_round_trips_volume_below_a_study() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("spo", "BTCUSDT", "1m", WinKind::Chart, r);
    w.options.show_volume = true;
    w.add_indicator("rsi", &[]); // oscillator → its own study pane
    w.sync_sub_panes(); // default: [Volume, Study]
    let study = w.present_study_panes()[0];
    assert_eq!(w.pane_order, vec![PaneKey::Volume, study]);
    // Move the study UP above Volume → authored order becomes [Study, Volume].
    w.reorder_pane(study, true);
    assert_eq!(w.present_sub_panes(), vec![study, PaneKey::Volume]);

    let snap = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    // captured with the study by ordinal 0, Volume verbatim, order preserved.
    assert_eq!(snap.wins[0].sub_pane_order, vec![PaneKey::Study(0), PaneKey::Volume]);

    let json = serde_json::to_string(&snap).unwrap();
    let parsed: Workspace = serde_json::from_str(&json).unwrap();
    let mut rebuilt = apply(&parsed);
    let w2 = &mut rebuilt[0];
    // The study pane stays ABOVE Volume after reload.
    let restored = w2.present_sub_panes();
    assert_eq!(restored.len(), 2);
    assert_matches!(restored[0], PaneKey::Study(_), "study restored at the TOP slot");
    assert_eq!(restored[1], PaneKey::Volume, "Volume restored BELOW the study");
    // A subsequent per-frame sync must NOT re-prepend Volume to the front.
    w2.sync_sub_panes();
    let after_sync = w2.present_sub_panes();
    assert_matches!(after_sync[0], PaneKey::Study(_));
    assert_eq!(after_sync[1], PaneKey::Volume);

    // fixed point: re-capture reproduces the same unified order.
    let second = capture(&rebuilt, DisplayTz::Local, false);
    assert_eq!(second.wins[0].sub_pane_order, snap.wins[0].sub_pane_order);
}

/// (l) Chart single-max default forward-compat: an OLD save (no
/// `sub_pane_order` key) loads with the study-only order `pane_membership`
/// rebuilt, and `sync_sub_panes` then re-slots Volume→CVD at the FRONT — the
/// exact pre-reorder Volume→CVD→studies sequence, byte-identical to old saves.
#[test]
fn old_snapshot_without_sub_pane_order_defaults_volume_cvd_to_front() {
    const JSON: &str = r#"{
            "version": 2,
            "wins": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "style": 0,
                "pos": [0.0, 0.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "show_volume": true,
                "follow": false,
                "auto_y": true,
                "cvd_on": true,
                "indicators": [ { "name": "rsi", "visible": true } ]
            }]
        }"#;
    let ws: Workspace =
        serde_json::from_str(JSON).expect("a v2 JSON with no sub_pane_order key must parse");
    assert!(ws.wins[0].sub_pane_order.is_empty(), "precondition: no sub_pane_order key");

    let mut rebuilt = apply(&ws);
    let w = &mut rebuilt[0];
    // apply leaves the study-only order (no Volume/CVD spliced in yet).
    assert_eq!(w.pane_order.len(), 1);
    assert_matches!(w.pane_order[0], PaneKey::Study(_));
    // sync (as main.rs runs each frame) prepends Volume then CVD at the front.
    w.sync_sub_panes();
    let sub = w.present_sub_panes();
    assert_eq!(sub[0], PaneKey::Volume, "Volume defaults to the front");
    assert_eq!(sub[1], PaneKey::Cvd, "CVD defaults directly after Volume");
    assert_matches!(sub[2], PaneKey::Study(_), "study pane follows Volume/CVD");
}

// --- named / multiple saved layouts -----------------------------------

/// (m) filename sanitization: safe stems pass through; path separators,
/// dots, control/exotic chars become `_`; whitespace collapses + trims;
/// empty/whitespace/all-punctuation ⇒ `""` (the "no valid name" signal); the
/// result is bounded in length and never contains a path separator.
#[test]
fn sanitize_layout_name_is_path_injection_proof() {
    assert_eq!(sanitize_layout_name("My Scalping Layout"), "My Scalping Layout");
    assert_eq!(sanitize_layout_name("  trimmed  "), "trimmed");
    assert_eq!(sanitize_layout_name("a\t b   c"), "a b c"); // whitespace runs collapse
    assert_eq!(sanitize_layout_name("keep-dash_underscore"), "keep-dash_underscore");
    // path-traversal / separators / dots are neutralized, then edge-trimmed
    assert_eq!(sanitize_layout_name("../../etc/passwd"), "etc_passwd");
    assert_eq!(sanitize_layout_name("a/b\\c"), "a_b_c");
    assert_eq!(sanitize_layout_name(".hidden."), "hidden");
    // empty / whitespace / all-punctuation collapse to the "no valid name" signal
    assert_eq!(sanitize_layout_name(""), "");
    assert_eq!(sanitize_layout_name("   "), "");
    assert_eq!(sanitize_layout_name("...///"), "");
    // never yields a path separator, and stays bounded
    let s = sanitize_layout_name(&"x/".repeat(100));
    assert!(!s.contains('/') && !s.contains('\\'));
    assert!(s.chars().count() <= 64);
    // an invalid name has no layout_path; a valid one does
    assert!(layout_path("   ").is_none());
    assert!(layout_path("Valid Name").is_some());
}

/// (n) listing + save/load/delete round-trip through a temp `layouts/` dir,
/// independent of the process's real workspace dir (uses the `*_in`/`*_to`/
/// `load_from` seams the public helpers are built on, so no env juggling).
#[test]
fn named_layout_save_list_load_delete_round_trip() {
    let dir = std::env::temp_dir().join(format!("vike-layouts-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // empty dir ⇒ no layouts
    assert!(list_layouts_in(&dir).is_empty());

    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    let expected = capture(&wins, DisplayTz::Utc, true);

    // save two named layouts (via the same schema the public save_layout uses)
    for name in ["Alpha", "beta view"] {
        let p = dir.join(format!("{}.json", sanitize_layout_name(name)));
        save_to(&wins, DisplayTz::Utc, true, &[], &p).unwrap();
    }
    // listing is sorted case-insensitively and stem-named
    assert_eq!(list_layouts_in(&dir), vec!["Alpha".to_string(), "beta view".to_string()]);

    // load one back — full-fidelity round-trip through the shared load_from seam
    let loaded = load_from(&dir.join("Alpha.json")).expect("saved above");
    assert_eq!(loaded, expected);
    let rebuilt = apply(&loaded);
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(rebuilt[0].symbol, "BTCUSDT");

    // delete one → gone from the listing, the other survives
    std::fs::remove_file(dir.join("Alpha.json")).unwrap();
    assert_eq!(list_layouts_in(&dir), vec!["beta view".to_string()]);
    // deleting an absent file is not fatal (idempotent contract)
    assert_matches!(
        std::fs::remove_file(dir.join("Alpha.json")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    // a missing layout file loads as None (never a panic)
    assert!(load_from(&dir.join("no-such.json")).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `layouts/` DIRECTORY holds every named layout, and it is the ONLY directory any of
/// them is looked for in: listing it lists all of them, and a name resolves to exactly one
/// file inside it whether or not that file exists yet.
///
/// A layout file sitting in a SIBLING directory is invisible — the property that says the
/// listing is a plain read of one directory rather than a merge of several. (The behavioural
/// twin, over the directory a second rung would actually have used, is
/// [`nothing_in_the_state_family_is_read_from_beside_the_executable`].)
///
/// Env-free by construction — driven through the same `list_layouts_in`/`load_from` seams the
/// public helpers compose, over temp directories, so no ambient `$VIKE_WORKSPACE` can move the
/// answer.
#[test]
fn the_layout_directory_is_the_only_place_a_named_layout_is_looked_for() {
    let root =
        std::env::temp_dir().join(format!("vike-lay-one-{}-{}", std::process::id(), line!()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("settings/state").join(LAYOUTS_SUBDIR);
    let sibling = root.join("elsewhere").join(LAYOUTS_SUBDIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();

    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    for name in ["Fresh", "Shared"] {
        save_to(&wins, DisplayTz::Utc, true, &[], &dir.join(format!("{name}.json"))).unwrap();
    }
    // Same stem, different directory, and a different value so a mix-up would be visible.
    for name in ["Elsewhere", "Shared"] {
        save_to(&wins, DisplayTz::Utc, false, &[], &sibling.join(format!("{name}.json"))).unwrap();
    }

    // The listing is that ONE directory: "Elsewhere" is not in it, "Shared" appears once.
    assert_eq!(
        list_layouts_in(&dir),
        vec!["Fresh".to_string(), "Shared".to_string()],
        "a layout outside the layouts directory is not a layout"
    );
    // …and the file a name resolves to is the one inside it (the GPU-on copy, not GPU-off).
    assert!(load_from(&dir.join("Shared.json")).unwrap().gpu_render);
    // A name with no file resolves to a path inside the same directory — reported, not read.
    assert!(!dir.join("Nowhere.json").exists());
    assert!(load_from(&dir.join("Nowhere.json")).is_none());

    // Deleting the one copy takes the layout out of the listing for good.
    std::fs::remove_file(dir.join("Shared.json")).unwrap();
    assert_eq!(list_layouts_in(&dir), vec!["Fresh".to_string()]);

    let _ = std::fs::remove_dir_all(&root);
}

/// `workspace.json` has ONE path, and the reader and the writer agree on it — a typo on
/// either side would silently write a file the loader never opens.
///
/// Also pinned: the write CREATES its directory on the way (a first save on a fresh checkout
/// must not fail for want of `settings/state`), and an absent file resolves to that same path
/// rather than to a second candidate.
#[test]
fn the_workspace_file_has_one_path_and_the_reader_and_the_writer_agree() {
    assert_eq!(WORKSPACE_FILE, "workspace.json");

    // The composition, over a temp directory: no real `$HOME` is touched and no ambient variable
    // can move the answer.
    let root =
        std::env::temp_dir().join(format!("vike-ws-state-{}-{}", std::process::id(), line!()));
    let _ = std::fs::remove_dir_all(&root);
    let state = root.join("state");
    assert!(!state.exists(), "precondition: nothing has been written yet");
    let written =
        vike_model::paths::state_path::write_path(Some(state.as_path()), WORKSPACE_FILE).unwrap();
    assert_eq!(written, state.join(WORKSPACE_FILE));
    assert!(state.is_dir(), "the directory is created on the way");
    assert!(!written.exists(), "…and only the directory: the file is the caller's to write");
    let _ = std::fs::remove_dir_all(&root);

    // …and this module's own two entry points land on that same single join.
    let read = path().expect("these tests run from inside the checkout");
    let write = save_path().expect("…whose state directory is creatable");
    assert_eq!(read, write, "a load and a save must never name different files");
    if std::env::var("VIKE_WORKSPACE").is_err() {
        let dir = state_dir().expect("these tests run from inside the checkout");
        assert_eq!(read, dir.join(WORKSPACE_FILE));
    }
}

/// **The state family is read from ONE directory, and it is not the executable's.**
///
/// Structural half: `layouts/` and `last_layout.txt` share ONE base with `workspace.json`, so
/// the family can never half-move, and that base is the project's `settings/state` (or, under
/// `$VIKE_WORKSPACE`, the directory that override names — one variable moves all of it).
///
/// Behavioural half, and the reason this test writes files: it plants a complete set —
/// `workspace.json`, `layouts/<name>.json`, `last_layout.txt` — in the test binary's own
/// directory, and asserts every reader ignores all three. That is the assertion a second
/// read rung anywhere in this module would fail.
#[test]
fn nothing_in_the_state_family_is_read_from_beside_the_executable() {
    let base = base_dir().expect("these tests run from inside the checkout");
    assert_eq!(layouts_dir(), Some(base.join(LAYOUTS_SUBDIR)));
    assert_eq!(last_layout_path(), Some(base.join(LAST_LAYOUT_FILE)));
    assert_eq!(layout_path("Ghost Layout"), Some(base.join(LAYOUTS_SUBDIR).join(GHOST_FILE)));

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .expect("a test binary has a directory");
    assert_ne!(base, exe_dir, "the family's base is the state directory, not the binary's own");

    // Plant the whole family exactly where it must no longer be looked for…
    let ghost_dir = exe_dir.join(LAYOUTS_SUBDIR);
    let ghost_layout = ghost_dir.join(GHOST_FILE);
    let ghost_workspace = exe_dir.join(WORKSPACE_FILE);
    let ghost_pointer = exe_dir.join(LAST_LAYOUT_FILE);
    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    std::fs::create_dir_all(&ghost_dir).unwrap();
    save_to(&wins, DisplayTz::Utc, false, &[], &ghost_layout).unwrap();
    save_to(&wins, DisplayTz::Utc, false, &[], &ghost_workspace).unwrap();
    std::fs::write(&ghost_pointer, "Ghost Layout").unwrap();

    // …ask every reader, then clean up BEFORE asserting so a failure leaves nothing behind.
    let listed = list_layouts();
    let loaded = load_layout("Ghost Layout");
    let pointed = last_layout();
    let workspace = path();
    for f in [&ghost_layout, &ghost_workspace, &ghost_pointer] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir(&ghost_dir);

    assert!(!listed.iter().any(|n| n == "Ghost Layout"), "listed: {listed:?}");
    assert!(loaded.is_none(), "a layout beside the executable is not a layout");
    assert_ne!(pointed.as_deref(), Some("Ghost Layout"), "nor is a pointer at one");
    assert_ne!(workspace, Some(ghost_workspace), "nor is a workspace file");
}

/// The layout file the test above plants, named once so its two spellings cannot drift.
const GHOST_FILE: &str = "Ghost Layout.json";

// ------------------------------------------------- user indicators, saved by NAME

/// A stand-in for a compiled user prototype: `close * scale`, one declared knob.
#[derive(Clone)]
struct Scaled {
    scale: f64,
    last: f64,
}

impl vike_chart::indicators::Indicator for Scaled {
    fn on_bar(&mut self, bar: &vike_model::Bar) -> Vec<f64> {
        self.last = bar.close * self.scale;
        vec![self.last]
    }
    fn vectorize(&self, bars: &[vike_model::Bar]) -> Vec<Vec<f64>> {
        vec![bars.iter().map(|b| b.close * self.scale).collect()]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "scaled"
    }
}

/// Named once so the fixture's two spellings (installed name / snapshot name) cannot drift.
const USER_STUDY: &str = "ws_scaled_close";
/// A name nothing resolves — the workspace fixture's "the file is gone" case.
const VANISHED_STUDY: &str = "ws_study_that_no_longer_exists";

/// The name of THE user study these tests share, installed on first use.
///
/// `vike_chart::indicators::install_user_studies` is a once-per-process `OnceLock` and this
/// crate's lib tests run as threads in ONE process, so the install sits behind a `Once` — a
/// second bare call would be the `Err` that function exists to return.
fn installed_user_study() -> &'static str {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let meta = vike_chart::indicators::IndicatorMeta::user(
            USER_STUDY,
            "Scaled Close",
            vike_chart::indicators::RenderKind::Overlay,
            vec![vike_chart::indicators::ParamSpec {
                name: "scale",
                default: 2.0,
                min: 0.0,
                max: 10.0,
                step: 0.5,
            }],
            &|raw: &[f64]| {
                Box::new(Scaled { scale: raw.first().copied().unwrap_or(2.0), last: f64::NAN })
            },
        );
        vike_chart::indicators::install_user_studies(vec![meta]).expect("first install");
    });
    USER_STUDY
}

/// A USER indicator is added, persisted and restored exactly like a built-in — by NAME,
/// carrying its params and its per-line paint.
///
/// Non-vacuous: `add_indicator` resolved through `get` before this seam, so on a reverted tree
/// the second assert fails — the study cannot be added at all and there is nothing to capture.
/// The `close * 4` assertion on the rebuilt series proves the restore rebuilt the USER's
/// recurrence through the meta's factory rather than anything from the built-in catalog.
#[test]
fn a_user_indicator_round_trips_through_the_workspace_by_name() {
    let name = installed_user_study();
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("usr", "BTCUSDT", "1m", WinKind::Chart, r);
    w.add_indicator("rsi", &[]); // a built-in neighbour, to prove nothing shifts onto it
    w.add_indicator(name, &[]);
    assert_eq!(w.indicators.len(), 2, "the user study must actually be addable");
    assert_eq!(w.indicators[1].spec.name, name);
    w.indicators[1].set_params(vec![4.0], &[]);
    w.indicators[1].outputs[0].color = egui::Color32::from_rgb(9, 9, 9);

    let cap = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    assert_eq!(cap.wins[0].indicators[1].name, name, "persisted by NAME, like every study");
    assert_eq!(cap.wins[0].indicators[1].params, vec![4.0]);

    let json = serde_json::to_string(&cap).unwrap();
    let mut rebuilt = apply(&serde_json::from_str::<Workspace>(&json).unwrap());
    assert_eq!(rebuilt[0].indicators.len(), 2);
    {
        let restored = &rebuilt[0].indicators[1];
        assert_eq!(restored.spec.name, name);
        assert!(restored.spec.is_user());
        assert_eq!(restored.params, vec![4.0], "the user knob survived the round trip");
        assert_eq!(restored.outputs[0].color, egui::Color32::from_rgb(9, 9, 9));
    }

    // …and it computes: fold one real bar and read the user prototype's own formula back.
    let bars = [vike_chart::model::Bar { t: 0.0, ot: 0, o: 5.0, h: 5.0, l: 5.0, c: 5.0, v: 1.0 }];
    rebuilt[0].indicators[1].recompute_full(&bars);
    assert_eq!(
        rebuilt[0].indicators[1].outputs[0].series,
        vec![20.0],
        "close 5 * the restored scale 4"
    );
}

/// ⚠ THE DEGRADATION CASE. A workspace saved with a user indicator whose file has since been
/// deleted, renamed or broken must restore the way an unknown BUILT-IN already does: the study
/// is silently skipped, everything else comes back untouched, and nothing panics.
///
/// Non-vacuous in the way that matters — the claim is not the bare "an unknown name is a
/// no-op" (already true). The specific hazard is `apply`'s restore loop: it applies each
/// snapshot entry's params and per-line paint to `indicators.last_mut()`, so a skipped entry
/// that failed to `continue` would write the VANISHED study's `params: [4.0]` and its colour
/// onto the PRECEDING study — silent corruption of a study that did restore. The two asserts
/// on `neighbour` fail if that guard goes; the length assert alone would not notice.
#[test]
fn a_vanished_user_indicator_is_skipped_without_disturbing_its_neighbour() {
    let name = installed_user_study();
    assert!(vike_chart::indicators::get_any(VANISHED_STUDY).is_none(), "precondition");

    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WinState::new("gone", "BTCUSDT", "1m", WinKind::Chart, r);
    w.add_indicator("rsi", &[]);
    w.add_indicator(name, &[]);
    w.indicators[0].set_params(vec![9.0], &[]);
    w.indicators[1].set_params(vec![4.0], &[]);
    w.indicators[1].outputs[0].color = egui::Color32::from_rgb(1, 2, 3);

    let cap = capture(std::slice::from_ref(&w), DisplayTz::Local, false);
    let json = serde_json::to_string(&cap).unwrap();
    // The file is gone: the SAME snapshot, with the study's name no longer resolving.
    let orphaned = json.replace(name, VANISHED_STUDY);
    assert!(orphaned.contains(VANISHED_STUDY), "the fixture must really name the dead study");

    let rebuilt = apply(&serde_json::from_str::<Workspace>(&orphaned).unwrap());
    assert_eq!(rebuilt[0].indicators.len(), 1, "the vanished study is silently dropped");
    let neighbour = &rebuilt[0].indicators[0];
    assert_eq!(neighbour.spec.name, "rsi");
    assert_eq!(neighbour.params, vec![9.0], "the dead study's params must not land here");
    assert_ne!(
        neighbour.outputs[0].color,
        egui::Color32::from_rgb(1, 2, 3),
        "nor its per-line paint"
    );
}
