//! Snapshot round trips: capture/apply, the version-3 words, older versions refused, retired and
//! absent keys.

use super::*;

/// The `user_sized` LATCH survives a save/load, and a file written before the key existed
/// loads as UNLATCHED.
///
/// ⚠ The latched fixture is built by HAND (`w.user_sized = true`), because `capture` only
/// takes `WindowKind::Chart` windows and a chart is a `fills` kind `show_window` never lets
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
    let ws = capture(&wins);
    assert!(ws.windows[0].user_sized, "the latch must reach the snapshot");

    // ...through real JSON, not just the in-memory structs: the key has to be WRITTEN.
    let json = serde_json::to_string(&ws).expect("a workspace serializes");
    assert!(json.contains("\"user_sized\":true"), "the key must be written: {json}");
    let reloaded: WindowWorkspace = serde_json::from_str(&json).expect("...and parses back");
    assert!(apply(&reloaded)[0].user_sized, "a latched window must reload LATCHED");

    // A PRE-FEATURE file — the same JSON with the key removed — must load as unlatched rather
    // than failing to parse or coming back latched.
    let older = json.replace("\"user_sized\":true,", "");
    assert!(!older.contains("user_sized"), "the probe must actually remove the key: {older}");
    let older: WindowWorkspace =
        serde_json::from_str(&older).expect("a pre-feature file must still parse");
    assert!(!older.windows[0].user_sized, "an absent key loads as UNLATCHED");
    assert!(!apply(&older)[0].user_sized, "...and restores as a size nobody chose");
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
    wins[0].cumulative_volume_delta_on = true;
    wins[0].profile_on = true;
    wins[0].orderflow_tick_size = Some(2.5);
    let ws = capture(&wins);
    assert_eq!(ws.version, VERSION);
    assert_eq!(ws.windows.len(), 2);
    assert_eq!(ws.windows[0].symbol, "BTCUSDT");
    assert_eq!(ws.windows[0].screen_position, (40.0, 30.0));
    assert_eq!(ws.windows[0].size, (700.0, 440.0));
    assert!(!ws.windows[0].options.show_volume);
    assert!(!ws.windows[0].follow);
    assert_eq!(ws.windows[0].sync_group, Some(2));
    assert_eq!(ws.windows[1].sync_group, None);
    assert!(ws.windows[0].cumulative_volume_delta_on);
    assert!(ws.windows[0].profile_on);
    assert_eq!(ws.windows[0].orderflow_tick_size, Some(2.5));
    assert!(!ws.windows[1].cumulative_volume_delta_on);
    assert!(!ws.windows[1].profile_on);
    assert_eq!(ws.windows[1].orderflow_tick_size, None);
    let inds: Vec<(&str, bool)> =
        ws.windows[0].indicators.iter().map(|i| (i.name.as_str(), i.visible)).collect();
    assert_eq!(inds, vec![("sma", true), ("macd", false)]);

    // JSON round-trip is lossless
    let json = serde_json::to_string(&ws).unwrap();
    let back: WindowWorkspace = serde_json::from_str(&json).unwrap();
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
    assert!(rebuilt[0].cumulative_volume_delta_on);
    assert!(rebuilt[0].profile_on);
    assert_eq!(rebuilt[0].orderflow_tick_size, Some(2.5));
    assert!(!rebuilt[1].cumulative_volume_delta_on);
    assert!(!rebuilt[1].profile_on);
    assert_eq!(rebuilt[1].orderflow_tick_size, None);
    assert_eq!(capture(&rebuilt), ws);
}

/// Decision 0117: inside one version a retired key just stops being read. A version-3
/// `workspace.json` still carrying `"of_backfill_hours"` (the chart's deleted backfill-hours knob),
/// `"gpu_render"` or `"display_time_zone"` (now `preferences.*` rows) must still LOAD with every other
/// global and window intact, loading must not touch the file, and no retired key may come back out
/// when the workspace is serialized again — the operator's file loses them only when HE next saves.
#[test]
fn an_old_workspace_with_the_retired_backfill_hours_key_still_loads() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let p = tmp.path().join("workspace.json");
    let old = format!(
        r#"{{"version":{VERSION},"display_tz":"UTC","of_backfill_hours":5.0,"gpu_render":true,
            "indicator_favourites":["rsi"],"windows":[{{"kind":"chart","symbol":"ETHUSDT",
            "venue":"binance","interval":"5m","style":"Candles","screen_position":[40.0,30.0],"size":[700.0,440.0],
            "open":true,"minimized":false,"maximized":false,"show_volume":false,"follow":false,
            "auto_y":true,"indicators":[]}}]}}"#
    );
    std::fs::write(&p, &old).unwrap();

    let ws = load_from(&p)
        .expect("a version-3 file carrying the retired keys must still load")
        .expect("the file is there");
    assert_eq!(ws.indicator_favourites, vec!["rsi".to_string()]);
    let wins = apply(&ws);
    assert_eq!(wins.len(), 1);
    assert_eq!((wins[0].symbol.as_str(), wins[0].interval.as_str()), ("ETHUSDT", "5m"));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), old, "loading never rewrites the file");

    let resaved = serde_json::to_string(&ws).unwrap();
    for retired in ["of_backfill_hours", "gpu_render", "display_tz"] {
        assert!(!resaved.contains(retired), "{retired} is written back: {resaved}");
    }
}

#[test]
fn tool_windows_are_skipped() {
    let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 200.0));
    let wins = vec![
        chart_win("BTCUSDT", ChartStyle::Candles),
        WindowState::new("trade", "", "", WindowKind::Trade, r),
    ];
    assert_eq!(capture(&wins).windows.len(), 1);
}

#[test]
fn an_unknown_indicator_is_dropped() {
    let ws = WindowWorkspace {
        version: VERSION,
        indicator_favourites: Vec::new(),
        windows: vec![WindowSnapshot {
            kind: "chart".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            venue: "binance".into(),
            asset_class: None,
            style: ChartStyle::Candles,
            screen_position: (0.0, 0.0),
            size: (100.0, 100.0),
            open: true,
            minimized: false,
            maximized: false,
            user_sized: false,
            follow: true,
            auto_y: true,
            scale: ChartScaleMode::default(),
            invert: false,
            options: ChartSettings::default(),
            panes: Vec::new(),
            pane_membership: Vec::new(),
            sub_pane_order: Vec::new(),
            sync_group: None,
            cumulative_volume_delta_on: false,
            profile_on: false,
            orderflow_tick_size: None,
            compare: Vec::new(),
            series_panes: Vec::new(),
            series_scale: Vec::new(),
            indicators: vec![WindowSnapshotStudy {
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

/// A style is saved as its WORD, and a word this build does not know refuses the whole file — it
/// used to be an index that silently became Candles.
#[test]
fn an_unknown_style_word_refuses_the_file() {
    let p = std::path::Path::new("state/workspace.json");
    let json = |style: &str| {
        format!(
            r#"{{"version":{VERSION},"windows":[{{"kind":"chart","symbol":"BTCUSDT","interval":"1m","venue":"binance",
            "style":"{style}","screen_position":[0.0,0.0],"size":[700.0,440.0],"open":true,
            "minimized":false,"maximized":false,"follow":true,"auto_y":true,"indicators":[]}}]}}"#
        )
    };
    let ws = parse(p, &json("HeikinAshi")).expect("a known style word reads");
    assert_eq!(ws.windows[0].style, ChartStyle::HeikinAshi);
    let hint = parse(p, &json("Squiggle")).expect_err("an unknown style word refuses the file");
    assert!(hint.contains("state/workspace.json") && hint.contains("Squiggle"), "{hint}");
}

/// THE OWNER'S RULING OF 2026-11-17: a workspace or layout an older build wrote (version 1 or 2,
/// abbreviated words) is REFUSED before anything in it is read, with one line naming the file and
/// what to do — never read with empty fields.
#[test]
fn an_older_version_is_refused_before_anything_is_read() {
    let p = std::path::Path::new("state/layouts/Scalping.json");
    for version in [1, 2] {
        let old = format!(
            r#"{{"version":{version},"indicator_favs":["rsi"],"wins":[{{"kind":"chart",
            "symbol":"BTCUSDT","interval":"1m","venue":"binance","style":0,"pos":[40.0,30.0],"size":[700.0,440.0],
            "open":true,"minimized":false,"maximized":false,"follow":false,"auto_y":true,
            "bg":[1,2,3],"cvd_on":true,"indicators":[]}}]}}"#
        );
        let hint = parse(p, &old).expect_err("an older version must be refused");
        assert!(hint.starts_with("state/layouts/Scalping.json is a version-"), "{hint}");
        assert!(hint.contains(&format!("version-{version} layout from an older Vike Trader")));
        assert!(hint.contains("this build reads version 3"), "{hint}");
        assert!(hint.contains("move it away") && hint.contains("Nothing is saved over it"));
    }
    let newer = parse(p, r#"{"version":4,"windows":[]}"#).expect_err("a newer one too");
    assert!(newer.contains("version-4 layout and is not read"), "{newer}");
    let unstamped = parse(p, r#"{"windows":[]}"#).expect_err("no version, no read");
    assert!(unstamped.contains("could not be read") && unstamped.contains("version"));
}

/// A layout restores CHART windows only (`capture` writes `"kind": "chart"` and nothing else), so a
/// version-3 file holding another kind — the retired Tearsheet window — is REFUSED with a hint
/// naming the file and the kind, never rebuilt as a chart.
#[test]
fn a_layout_holding_a_window_kind_other_than_chart_is_refused() {
    let p = std::path::Path::new("state/layouts/Desk.json");
    let with_kind = |kind: &str| {
        format!(
            r#"{{"version":{VERSION},"windows":[{{"kind":"{kind}","symbol":"BTCUSDT","interval":"1m","venue":"binance",
            "style":"Candles","screen_position":[0.0,0.0],"size":[700.0,440.0],"open":true,
            "minimized":false,"maximized":false,"follow":true,"auto_y":true,"indicators":[]}}]}}"#
        )
    };
    assert_eq!(parse(p, &with_kind("chart")).expect("a chart reads").windows.len(), 1);
    let hint = parse(p, &with_kind("tearsheet")).expect_err("a tearsheet window is refused");
    assert_eq!(
        hint,
        "state/layouts/Desk.json holds a `tearsheet` window, and a layout restores chart windows \
         only: move it away and save the layout again. Nothing is saved over it until then."
    );
}

/// Version 3 reads no pre-v3 word: a pane saved under the pre-C1 tag `Osc` refuses the file (the
/// alias is gone), and a colour equal to a pre-PR-7 compiled default stays the user's pick through
/// the real `#[serde(flatten)]` path (the rule that unset it read only pre-v3 files).
#[test]
fn version_3_reads_no_pre_version_3_word() {
    let p = std::path::Path::new("state/workspace.json");
    let window = |extra: &str| {
        format!(
            r#"{{"version":{VERSION},"windows":[{{"kind":"chart","symbol":"BTCUSDT","interval":"1m","venue":"binance",
            "style":"Candles","screen_position":[0.0,0.0],"size":[700.0,440.0],"open":true,
            "minimized":false,"maximized":false,"follow":true,"auto_y":true,{extra}
            "indicators":[]}}]}}"#
        )
    };
    let hint = parse(p, &window(r#""panes":[[{"Osc":0},0.2]],"#)).expect_err("Osc is not read");
    assert!(hint.contains("unknown variant `Osc`") && hint.contains("state/workspace.json"));
    let ws = parse(p, &window(r#""up":[91,190,145],"background":[13,17,23],"#)).expect("reads");
    let o = &ws.windows[0].options;
    assert_eq!((o.up, o.background), (Some([91, 190, 145]), Some([13, 17, 23])), "still picked");
}

/// A saved compare overlay stores its OWN venue (W3-D's hand-off: `workspace::default_venue` is gone),
/// so an overlay of a non-Binance chart can never restore as Binance: a bybit chart's overlay is
/// written as `{"venue":"bybit","symbol":…}`, and a file whose window names no venue, or whose
/// overlay is a bare symbol (the shape before version 3), is refused rather than read on a default.
#[test]
fn a_saved_compare_overlay_stores_its_venue_and_nothing_defaults_to_binance() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WindowState::new("cmp", "BTCUSDT", "1m", WindowKind::Chart, r);
    w.venue = "bybit".into();
    w.add_compare("ETHUSDT");
    // An overlay added before the chart moved venue keeps its own: it saves and restores as okx.
    w.compare.push(ChartCompareSymbol { venue: "okx".into(), symbol: "SOLUSDT".into() });
    let json = serde_json::to_string(&capture(std::slice::from_ref(&w))).unwrap();
    let saved =
        r#""compare":[{"venue":"bybit","symbol":"ETHUSDT"},{"venue":"okx","symbol":"SOLUSDT"}]"#;
    assert!(json.contains(saved), "{json}");
    let back = apply(&serde_json::from_str::<WindowWorkspace>(&json).unwrap());
    assert_eq!((back[0].venue.as_str(), &back[0].compare), ("bybit", &w.compare));

    let p = std::path::Path::new("state/workspace.json");
    let window = |venue_and_compare: &str| {
        format!(
            r#"{{"version":{VERSION},"windows":[{{"kind":"chart","symbol":"BTCUSDT","interval":"1m",
            {venue_and_compare}"style":"Candles","screen_position":[0.0,0.0],"size":[700.0,440.0],
            "open":true,"minimized":false,"maximized":false,"follow":true,"auto_y":true,
            "indicators":[]}}]}}"#
        )
    };
    let read =
        parse(p, &window(r#""venue":"okx","compare":[{"venue":"okx","symbol":"ETH-USDT"}],"#));
    assert_eq!(read.expect("reads").windows[0].compare[0].venue, "okx");
    let no_venue = parse(p, &window(r#""compare":[],"#)).expect_err("a window names its venue");
    assert!(no_venue.contains("missing field `venue`"), "{no_venue}");
    let bare = parse(p, &window(r#""venue":"okx","compare":["ETH-USDT"],"#))
        .expect_err("an overlay names its venue");
    assert!(bare.contains("could not be read"), "{bare}");
}

/// Inside version 3 an ABSENT key takes its default: `show_volume` is a flattened top-level key,
/// every absent `ChartSettings` field takes its CUSTOM `Default` value (not a type-zero), and an
/// indicator with NO `params`/`lines` loads with the registry's. This is the flatten +
/// struct-level `#[serde(default)]` gate.
#[test]
fn a_version_3_file_without_the_optional_keys_takes_their_defaults() {
    let minimal = format!(
        r#"{{
            "version": {VERSION},
            "windows": [{{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "venue": "binance",
                "style": "Candles",
                "screen_position": [40.0, 30.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "show_volume": false,
                "follow": false,
                "auto_y": true,
                "indicators": [{{ "name": "sma", "visible": true }}]
            }}]
        }}"#
    );
    let ws = parse(std::path::Path::new("workspace.json"), &minimal)
        .expect("a version-3 file with only the required keys parses");
    let win = &ws.windows[0];
    // the top-level show_volume flowed into the flattened options
    assert!(!win.options.show_volume, "show_volume:false must survive the flatten");
    // absent color/flag/precision fields defaulted to the CUSTOM values, not zeros
    let d = ChartSettings::default();
    assert_eq!(win.options.up, None, "an absent colour loads unset — it follows the appearance");
    assert_eq!(win.options.down, None);
    assert_eq!(win.options.crosshair, None);
    assert_eq!(win.options.precision, d.precision);
    assert!(win.options.show_grid, "absent show_grid must default on");
    // absent scale/panes default; the indicator loads with default (empty) params
    assert_eq!(win.scale, ChartScaleMode::Linear);
    assert!(win.panes.is_empty());
    assert!(win.indicators[0].params.is_empty());
    assert!(win.indicators[0].lines.is_empty());
    // task B8: an absent `sync_group` key defaults to None (ungrouped).
    assert_eq!(win.sync_group, None);
    // SP2 orderflow (Task 7): absent `cumulative_volume_delta_on`/`profile_on`/
    // `orderflow_tick_size` keys default to false/false/None.
    assert!(!win.cumulative_volume_delta_on);
    assert!(!win.profile_on);
    assert_eq!(win.orderflow_tick_size, None);
    // The venue is the file's own, never a default.
    assert_eq!(win.venue, "binance");

    // and apply carries the custom defaults onto a real WindowState
    let rebuilt = apply(&ws);
    assert!(!rebuilt[0].options.show_volume);
    assert_eq!(rebuilt[0].options.up, None);
    assert_eq!(rebuilt[0].scale, ChartScaleMode::Linear);
    assert_eq!(rebuilt[0].sync_group, None);
    assert!(!rebuilt[0].cumulative_volume_delta_on);
    assert!(!rebuilt[0].profile_on);
    assert_eq!(rebuilt[0].orderflow_tick_size, None);
    assert_eq!(rebuilt[0].venue, "binance"); // the saved venue on a real WindowState too
}

/// (b) fixed point ACROSS A UID CHANGE: a window with a non-default scale,
/// custom `ChartSettings`, a dragged `ChartPaneFractions` (incl. a Study pane) and an
/// oscillator carrying non-default params + a per-line paint edit survives
/// capture→serialize→parse→apply→capture unchanged. The `apply` step rebuilds
/// indicators through `add_indicator` (assigning NEW uids), so an equal
/// re-capture proves the Study-pane index↔uid translation survives a uid change
/// — persisting uids verbatim would re-capture a DIFFERENT Study key and fail.
#[test]
fn round_trip_is_a_fixed_point_across_a_uid_change() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WindowState::new("fp", "BTCUSDT", "1m", WindowKind::Chart, r);
    // non-default scale + custom options (custom up color, precision, volume off)
    w.scale = ChartScaleMode::Logarithmic;
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
    // a dragged pane layout referencing the oscillator by its (uid 1) ChartPaneKey
    let present = [ChartPaneKey::Price, ChartPaneKey::Study(osc_uid)];
    w.panes.layout(&present, 500.0, 44.0);
    w.panes.drag(&present, 0, 40.0, 500.0, 44.0);

    let first = capture(std::slice::from_ref(&w));
    // the captured pane's Study is keyed by INDEX 0, NOT the uid (1)
    assert!(
        first.windows[0].panes.iter().any(|(p, _)| *p == ChartPaneKey::Study(0)),
        "osc pane must be stored by index 0, got {:?}",
        first.windows[0].panes
    );
    assert!(
        !first.windows[0].panes.iter().any(|(p, _)| *p == ChartPaneKey::Study(osc_uid)),
        "osc pane must NOT be stored by the volatile uid {osc_uid}"
    );

    let json = serde_json::to_string(&first).unwrap();
    let parsed: WindowWorkspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    // the rebuilt oscillator got a DIFFERENT uid than the original (0 vs 1)
    let new_uid = rebuilt[0].indicators[0].uid;
    assert_ne!(new_uid, osc_uid, "rebuild must reassign the oscillator uid");

    let second = capture(&rebuilt);
    assert_eq!(second.windows[0].scale, first.windows[0].scale);
    assert_eq!(second.windows[0].options, first.windows[0].options);
    assert_eq!(second.windows[0].panes, first.windows[0].panes);
    assert_eq!(second.windows[0].indicators, first.windows[0].indicators);
    assert_eq!(second, first, "full round-trip must be a fixed point");
}

/// Foreign-source study (TradingView "symbol" input): a study's `(venue, symbol)`
/// source round-trips capture→JSON→apply, and a study WITHOUT a source captures no
/// `source` key at all (skip_serializing_if) and rebuilds as `None` — the
/// byte-identical same-symbol default.
#[test]
fn foreign_source_symbol_round_trips_and_defaults_to_none() {
    use vike_chart::chart::study::ChartStudySourceSymbol;
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WindowState::new("src", "BTCUSDT", "1m", WindowKind::Chart, r);
    w.add_indicator("sma", &[]); // overlay, computes off a FOREIGN symbol
    w.add_indicator("rsi", &[]); // oscillator, stays on the primary (no source)
    w.indicators[0].source_symbol =
        Some(ChartStudySourceSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() });

    let cap = capture(std::slice::from_ref(&w));
    // the sourced study carries the pair; the primary-sourced study carries None
    assert_eq!(cap.windows[0].indicators[0].source, Some(("bybit".into(), "ETHUSDT".into())));
    assert_eq!(cap.windows[0].indicators[1].source, None);

    let json = serde_json::to_string(&cap).unwrap();
    // a None source emits NO `source` key (skip_serializing_if) — save stays clean
    // and byte-identical to a pre-feature file for ordinary studies.
    assert_eq!(json.matches("\"source\"").count(), 1, "only the sourced study writes a key");

    let rebuilt = apply(&serde_json::from_str::<WindowWorkspace>(&json).unwrap());
    assert_eq!(
        rebuilt[0].indicators[0].source_symbol,
        Some(ChartStudySourceSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() }),
        "foreign source survives the rebuild",
    );
    assert_eq!(rebuilt[0].indicators[1].source_symbol, None, "primary study stays sourceless");

    // an OLD file (no `source` key on any indicator) loads every study as primary.
    let legacy = json.replace(",\"source\":[\"bybit\",\"ETHUSDT\"]", "");
    let old = apply(&serde_json::from_str::<WindowWorkspace>(&legacy).unwrap());
    assert!(old[0].indicators.iter().all(|a| a.source_symbol.is_none()));
}

/// TradingView "Invert scale": the persisted `invert` flag round-trips, and an
/// OLD file with no `invert` key defaults to `false` (byte-identical to
/// pre-invert), same `#[serde(default)]` discipline as `scale`.
#[test]
fn invert_flag_round_trips_and_defaults_to_false_when_absent() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WindowState::new("inv", "BTCUSDT", "1m", WindowKind::Chart, r);
    assert!(!w.invert, "fresh window is not inverted");
    w.invert = true;
    w.scale = ChartScaleMode::Logarithmic; // invert is orthogonal to the mode
    let json = serde_json::to_string(&capture(std::slice::from_ref(&w))).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["windows"][0]
            .as_object()
            .unwrap()
            .contains_key("invert"),
        "invert must be a top-level key: {json}"
    );
    let parsed: WindowWorkspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    assert!(rebuilt[0].invert, "invert flag must survive the round-trip");
    assert_eq!(rebuilt[0].scale, ChartScaleMode::Logarithmic, "mode survives alongside invert");

    // an OLD file that predates the invert key loads false (serde default).
    let stripped = json.replace(",\"invert\":true", "");
    assert!(!stripped.contains("\"invert\""), "precondition: invert key removed");
    let old: WindowWorkspace = serde_json::from_str(&stripped).unwrap();
    assert!(!apply(&old)[0].invert, "absent invert key defaults to false");
}

/// (c) no duplicate/nested keys: the flattened `options.show_volume` must be
/// emitted EXACTLY once (not also as a standalone `WindowSnapshot` key) and never as
/// a nested `options` object.
#[test]
fn serialization_has_no_duplicate_or_nested_option_keys() {
    let mut wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    wins[0].options.up = Some([1, 2, 3]);
    let json = serde_json::to_string(&capture(&wins)).unwrap();
    // raw-string guard: one window → `show_volume` appears exactly once
    assert_eq!(
        json.matches("\"show_volume\"").count(),
        1,
        "show_volume must be emitted once (flattened), not duplicated: {json}"
    );
    let val: serde_json::Value = serde_json::from_str(&json).unwrap();
    let win = val["windows"][0].as_object().unwrap();
    assert!(win.contains_key("show_volume"), "flattened show_volume at top level");
    assert!(!win.contains_key("options"), "options must be flattened, not nested");
    // a couple of other flattened/added keys confirm the shape
    assert!(win.contains_key("up"), "flattened color key at top level");
    assert!(!win.contains_key("down"), "an unset colour is not written");
    assert!(win.contains_key("scale"));
    assert!(win.contains_key("panes"));
    assert!(win.contains_key("sync_group"), "task B8: sync_group is a top-level key");
}

/// A workspace saved before chart colours followed the appearance (version 2, every colour written)
/// is not read at all now: it is refused with the older-version hint, so none of its colours, the
/// picked one included, loads in any form.
#[test]
fn a_workspace_saved_before_chart_colours_followed_the_appearance_is_refused() {
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
    let hint = parse(std::path::Path::new("workspace.json"), SAVED).expect_err("refused");
    assert!(hint.contains("version-2 layout from an older Vike Trader"), "{hint}");
}

/// An edited chart colour survives capture → JSON → apply; an unset one is never written.
#[test]
fn an_edited_chart_colour_survives_the_workspace_round_trip() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WindowState::new("rt", "BTCUSDT", "1m", WindowKind::Chart, r);
    w.options.up = Some([1, 2, 3]);
    let json = serde_json::to_string(&capture(std::slice::from_ref(&w))).unwrap();
    assert!(json.contains("\"up\":[1,2,3]"), "{json}");
    assert!(!json.contains("\"down\""), "an unset colour is not written: {json}");
    let back = apply(&serde_json::from_str::<WindowWorkspace>(&json).unwrap());
    assert_eq!((back[0].options.up, back[0].options.down), (Some([1, 2, 3]), None));
}
