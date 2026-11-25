//! Pane membership, sub-pane order and compare-series panes across a capture/apply round trip.

use super::*;

/// (f) C1 Task 5: pane MEMBERSHIP round-trips across a uid reassignment, the
/// same way T9/T10's heights already did. Merge three oscillators into one
/// pane, capture→JSON→parse→apply (the rebuild reassigns fresh uids — proven
/// below, mirroring `v2_round_trip_is_a_fixed_point_across_a_uid_change`'s
/// throwaway-overlay trick), and assert the merge survives: all three
/// oscillators share ONE pane again, not three.
#[test]
fn c1_pane_membership_round_trips_across_uid_reassignment() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WindowState::new("pm", "BTCUSDT", "1m", WindowKind::Chart, r);
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
    w.move_study(uids[1], ChartPaneMoveTarget::Into(p0));
    w.move_study(uids[2], ChartPaneMoveTarget::Into(p0));
    assert_eq!(w.present_study_panes().len(), 1, "precondition: merged into one pane");

    let snapshot = capture(std::slice::from_ref(&w));
    let json = serde_json::to_string(&snapshot).unwrap();
    let parsed: WindowWorkspace = serde_json::from_str(&json).unwrap();
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
/// `pane_membership` key at all — the same absent-key shape as
/// `a_version_3_file_without_the_optional_keys_takes_their_defaults`, but for a
/// file that already has `panes` heights from T9/T10) falls back to one pane
/// per oscillator, in add order — today's pre-Task-5 default layout — and a
/// re-capture reproduces the SAME ordinal-keyed heights the old file had,
/// proving old-file heights are preserved across the fallback.
#[test]
fn c1_old_snapshot_without_panes_field_loads_as_one_pane_per_oscillator() {
    const JSON: &str = r#"{
            "version": 3,
            "windows": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "venue": "binance",
                "style": "Candles",
                "screen_position": [0.0, 0.0],
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
    let ws: WindowWorkspace =
        serde_json::from_str(JSON).expect("a v3 JSON with no pane_membership key must parse");
    assert!(
        ws.windows[0].pane_membership.is_empty(),
        "precondition: no pane_membership key in this fixture"
    );

    let rebuilt = apply(&ws);
    let w = &rebuilt[0];
    // fallback: each oscillator in its own pane, in order
    assert_eq!(w.present_study_panes().len(), 3);
    assert!(w.study_pane.values().all(|k| matches!(k, ChartPaneKey::Study(_))));
    let distinct: std::collections::HashSet<ChartPaneKey> =
        w.study_pane.values().copied().collect();
    assert_eq!(distinct.len(), 3, "one DISTINCT pane per oscillator, not merged");

    // old-file heights (already Study(osc index)-keyed under the default
    // one-pane-per-oscillator layout) still land on the right pane.
    let recaptured = capture(&rebuilt);
    let heights = &recaptured.windows[0].panes;
    assert!(heights.contains(&(ChartPaneKey::Study(0), 0.5)));
    assert!(heights.contains(&(ChartPaneKey::Study(2), 0.25)));
}

/// (h) C2b Task 8: compare symbols + own-pane placement + per-symbol scale
/// pin all round-trip together, the same way C1's pane membership survived a
/// uid reassignment (`c1_pane_membership_round_trips_across_uid_reassignment`).
/// A throwaway own-pane compare symbol is added then removed FIRST, consuming
/// `ChartPaneKey::Series(1)`; the surviving own-pane symbol is then `Series(2)`, so
/// `apply`'s rebuild — which restarts `next_series_pane_id` fresh at 1 — is
/// proven to assign a DIFFERENT id, yet membership still lands correctly
/// because it is persisted as an ORDINAL into `compare`, not the volatile id.
#[test]
fn c2b_compare_series_pane_and_scale_round_trip_across_pane_id_reassignment() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WindowState::new("cs", "BTCUSDT", "1m", WindowKind::Chart, r);
    w.venue = "binance".into();

    // Throwaway own-pane symbol first — consumes ChartPaneKey::Series(1), then is
    // removed entirely (cascades out of both `compare` and `series_pane`).
    w.add_compare("ADAUSDT");
    w.move_series_to_new_pane("ADAUSDT");
    w.remove_compare("ADAUSDT");

    // ETHUSDT: stays overlaid in the price pane (C2a default), explicitly
    // pinned Percent (also the default — proves a stored-but-default entry
    // round-trips too, not just a stored non-default one).
    w.add_compare("ETHUSDT");
    w.series_scale.insert("ETHUSDT".to_string(), ChartScaleAssignment::Percent);
    // SOLUSDT: moved to its own pane, pinned Right.
    w.add_compare("SOLUSDT");
    w.move_series_to_new_pane("SOLUSDT");
    w.series_scale.insert("SOLUSDT".to_string(), ChartScaleAssignment::Right);

    let original_pane = w.series_pane["SOLUSDT"];
    assert_eq!(
        original_pane,
        ChartPaneKey::Series(2),
        "precondition: the throwaway pane consumed id 1"
    );

    let snapshot = capture(std::slice::from_ref(&w));
    let win = &snapshot.windows[0];
    let saved: Vec<(&str, &str)> =
        win.compare.iter().map(|c| (c.venue.as_str(), c.symbol.as_str())).collect();
    assert_eq!(saved, vec![(w.venue.as_str(), "ETHUSDT"), (w.venue.as_str(), "SOLUSDT")]);
    // SOLUSDT is compare-index 1 — the sole member of the one own-pane group.
    assert_eq!(win.series_panes, vec![vec![1]]);
    assert!(win.series_scale.contains(&("ETHUSDT".to_string(), ChartScaleAssignment::Percent)));
    assert!(win.series_scale.contains(&("SOLUSDT".to_string(), ChartScaleAssignment::Right)));

    let json = serde_json::to_string(&snapshot).unwrap();
    let parsed: WindowWorkspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    let w2 = &rebuilt[0];

    let on_binance =
        |symbol: &str| ChartCompareSymbol { venue: "binance".into(), symbol: symbol.into() };
    assert_eq!(w2.compare, [on_binance("ETHUSDT"), on_binance("SOLUSDT")], "on the chart's venue");
    assert!(!w2.series_pane.contains_key("ETHUSDT"), "ETHUSDT must stay overlaid");
    assert!(w2.series_pane.contains_key("SOLUSDT"), "SOLUSDT must be in its own pane");
    assert_eq!(w2.present_series_panes().len(), 1);
    let new_pane = w2.series_pane["SOLUSDT"];
    assert_ne!(new_pane, original_pane, "rebuild must reassign the series-pane id");
    assert_eq!(w2.series_scale_of("ETHUSDT"), ChartScaleAssignment::Percent);
    assert_eq!(w2.series_scale_of("SOLUSDT"), ChartScaleAssignment::Right);

    // fixed point: re-capture reproduces the same snapshot
    let second = capture(&rebuilt);
    assert_eq!(second.windows[0].compare, win.compare);
    assert_eq!(second.windows[0].series_panes, win.series_panes);
    assert_eq!(second.windows[0].series_scale, win.series_scale);
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
            "version": 3,
            "windows": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "venue": "binance",
                "style": "Candles",
                "screen_position": [0.0, 0.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "follow": false,
                "auto_y": true,
                "indicators": []
            }]
        }"#;
    let ws: WindowWorkspace =
        serde_json::from_str(JSON).expect("a v3 JSON with no compare keys must parse");
    assert!(ws.windows[0].compare.is_empty(), "precondition: no compare key in this fixture");
    assert!(ws.windows[0].series_panes.is_empty());
    assert!(ws.windows[0].series_scale.is_empty());

    let rebuilt = apply(&ws);
    let w = &rebuilt[0];
    assert!(w.compare.is_empty(), "old file must load with zero compare overlays");
    assert!(w.series_pane.is_empty());
    assert!(w.present_series_panes().is_empty());
    assert_eq!(w.series_scale_of("ANYSYM"), ChartScaleAssignment::Percent);

    // re-capture reproduces the same empty shape (byte-identical fixed point)
    let recaptured = capture(&rebuilt);
    assert!(recaptured.windows[0].compare.is_empty());
    assert!(recaptured.windows[0].series_panes.is_empty());
    assert!(recaptured.windows[0].series_scale.is_empty());
}

/// (j) C2 tidy FIX 2: a Series-pane HEIGHT survives capture→apply, the same way a
/// Study-pane height already does (`v2_round_trip_is_a_fixed_point_across_a_uid_change`)
/// and the way Series-pane MEMBERSHIP already did (test (h) above). Before this fix
/// `panes_to_snapshot`/`snapshot_to_panes` had a placeholder `ChartPaneKey::Series(_) => None` arm (a
/// leftover from C2 Task 5, before `chart::draw` ever fed a live `Series` pane into
/// `w.panes`), so a dragged own-pane compare series always reloaded at the un-dragged
/// 0.16 default share. Uses the same throwaway-pane / pane-id-reassignment trick as test
/// (h) so this also proves the height round-trips by ORDINAL, not the volatile pane id.
#[test]
fn c2_series_pane_height_round_trips_across_pane_id_reassignment() {
    let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(800.0, 500.0));
    let mut w = WindowState::new("sph", "BTCUSDT", "1m", WindowKind::Chart, r);

    // Throwaway own-pane symbol first — consumes ChartPaneKey::Series(1), then is removed
    // entirely (mirrors test (h)'s precondition setup).
    w.add_compare("ADAUSDT");
    w.move_series_to_new_pane("ADAUSDT");
    w.remove_compare("ADAUSDT");

    w.add_compare("SOLUSDT");
    w.move_series_to_new_pane("SOLUSDT");
    let pane = w.series_pane["SOLUSDT"];
    assert_eq!(pane, ChartPaneKey::Series(2), "precondition: the throwaway pane consumed id 1");

    // Drag the pane's height away from its 0.16 default — the same `ChartPaneFractions`
    // `layout`/`drag` calls `chart::draw`'s live separator-drag path makes, exercised
    // directly here like `v2_round_trip_is_a_fixed_point_across_a_uid_change` does for a
    // Study pane.
    let present = [ChartPaneKey::Price, pane];
    w.panes.layout(&present, 500.0, 44.0);
    w.panes.drag(&present, 0, 40.0, 500.0, 44.0);

    let first = capture(std::slice::from_ref(&w));
    let stored = first.windows[0]
        .panes
        .iter()
        .find(|(p, _)| matches!(p, ChartPaneKey::Series(_)))
        .copied()
        .expect("a Series pane height must be captured, not dropped");
    assert_eq!(stored.0, ChartPaneKey::Series(0), "captured by ordinal, not the volatile pane id");
    assert!(
        (stored.1 - 0.16).abs() > 0.01,
        "height must be the DRAGGED value, not the 0.16 default, got {}",
        stored.1
    );

    let json = serde_json::to_string(&first).unwrap();
    let parsed: WindowWorkspace = serde_json::from_str(&json).unwrap();
    let rebuilt = apply(&parsed);
    let w2 = &rebuilt[0];
    let new_pane = w2.series_pane["SOLUSDT"];
    assert_ne!(new_pane, pane, "rebuild must reassign the series-pane id");

    // fixed point: re-capture reproduces the exact same (non-default) height
    let second = capture(&rebuilt);
    assert_eq!(
        second.windows[0].panes, first.windows[0].panes,
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
    let mut w = WindowState::new("spo", "BTCUSDT", "1m", WindowKind::Chart, r);
    w.options.show_volume = true;
    w.add_indicator("rsi", &[]); // oscillator → its own study pane
    w.sync_sub_panes(); // default: [Volume, Study]
    let study = w.present_study_panes()[0];
    assert_eq!(w.pane_order, vec![ChartPaneKey::Volume, study]);
    // Move the study UP above Volume → authored order becomes [Study, Volume].
    w.reorder_pane(study, true);
    assert_eq!(w.present_sub_panes(), vec![study, ChartPaneKey::Volume]);

    let snapshot = capture(std::slice::from_ref(&w));
    // captured with the study by ordinal 0, Volume verbatim, order preserved.
    assert_eq!(
        snapshot.windows[0].sub_pane_order,
        vec![ChartPaneKey::Study(0), ChartPaneKey::Volume]
    );

    let json = serde_json::to_string(&snapshot).unwrap();
    let parsed: WindowWorkspace = serde_json::from_str(&json).unwrap();
    let mut rebuilt = apply(&parsed);
    let w2 = &mut rebuilt[0];
    // The study pane stays ABOVE Volume after reload.
    let restored = w2.present_sub_panes();
    assert_eq!(restored.len(), 2);
    assert_matches!(restored[0], ChartPaneKey::Study(_), "study restored at the TOP slot");
    assert_eq!(restored[1], ChartPaneKey::Volume, "Volume restored BELOW the study");
    // A subsequent per-frame sync must NOT re-prepend Volume to the front.
    w2.sync_sub_panes();
    let after_sync = w2.present_sub_panes();
    assert_matches!(after_sync[0], ChartPaneKey::Study(_));
    assert_eq!(after_sync[1], ChartPaneKey::Volume);

    // fixed point: re-capture reproduces the same unified order.
    let second = capture(&rebuilt);
    assert_eq!(second.windows[0].sub_pane_order, snapshot.windows[0].sub_pane_order);
}

/// (l) Chart single-max default forward-compat: an OLD save (no
/// `sub_pane_order` key) loads with the study-only order `pane_membership`
/// rebuilt, and `sync_sub_panes` then re-slots Volume→CVD at the FRONT — the
/// exact pre-reorder Volume→CVD→studies sequence, byte-identical to old saves.
#[test]
fn old_snapshot_without_sub_pane_order_defaults_volume_cvd_to_front() {
    const JSON: &str = r#"{
            "version": 3,
            "windows": [{
                "kind": "chart",
                "symbol": "BTCUSDT",
                "interval": "1m",
                "venue": "binance",
                "style": "Candles",
                "screen_position": [0.0, 0.0],
                "size": [700.0, 440.0],
                "open": true,
                "minimized": false,
                "maximized": false,
                "show_volume": true,
                "follow": false,
                "auto_y": true,
                "cumulative_volume_delta_on": true,
                "indicators": [ { "name": "rsi", "visible": true } ]
            }]
        }"#;
    let ws: WindowWorkspace =
        serde_json::from_str(JSON).expect("a v3 JSON with no sub_pane_order key must parse");
    assert!(ws.windows[0].sub_pane_order.is_empty(), "precondition: no sub_pane_order key");

    let mut rebuilt = apply(&ws);
    let w = &mut rebuilt[0];
    // apply leaves the study-only order (no Volume/CVD spliced in yet).
    assert_eq!(w.pane_order.len(), 1);
    assert_matches!(w.pane_order[0], ChartPaneKey::Study(_));
    // sync (as main.rs runs each frame) prepends Volume then CVD at the front.
    w.sync_sub_panes();
    let sub = w.present_sub_panes();
    assert_eq!(sub[0], ChartPaneKey::Volume, "Volume defaults to the front");
    assert_eq!(sub[1], ChartPaneKey::CumulativeVolumeDelta, "CVD defaults directly after Volume");
    assert_matches!(sub[2], ChartPaneKey::Study(_), "study pane follows Volume/CVD");
}
