use super::*;
use tracing_subscriber::layer::SubscriberExt;

/// Every ERROR and WARN line adds one to its target's count, whatever its text; an INFO, DEBUG or TRACE line counts
/// nowhere, and a target that logged only those has no row.
#[test]
fn each_error_and_warn_line_counts_once_under_its_target_and_nothing_quieter_counts() {
    let before = now_ms();
    let counts = counted(|| {
        tracing::error!(target: "vike_tradehub::publish", venue = "bybit", "the first failure");
        tracing::error!(target: "vike_tradehub::publish", "another text, the same target");
        tracing::warn!(target: "vike_tradehub::publish", "a warning");
        tracing::warn!(target: "vike_core::reconcile", "a warning elsewhere");
        tracing::info!(target: "vike_core::reconcile", "not counted");
        tracing::debug!(target: "vike_quiet", "not counted");
        tracing::trace!(target: "vike_quiet", "not counted");
    });
    let after = now_ms();
    let mut by_target = counts.by_target();
    // The newest ERROR line's clock reading is the one field a test cannot spell: in the window, then normalized.
    let latest = by_target["vike_tradehub::publish"].latest_error_ms;
    assert!((before..=after).contains(&latest), "{before} <= {latest} <= {after}");
    by_target.get_mut("vike_tradehub::publish").expect("counted").latest_error_ms = 1;
    assert_eq!(
        by_target,
        BTreeMap::from([
            (
                "vike_core::reconcile".to_string(),
                LevelCounts { errors: 0, warnings: 1, latest_error_ms: 0 },
            ),
            (
                "vike_tradehub::publish".to_string(),
                LevelCounts { errors: 2, warnings: 1, latest_error_ms: 1 },
            ),
        ]),
        "a target with WARN lines only has no ERROR time"
    );
}

/// The layer's level hint is WARN, so the subscriber's most verbose level (the max over its layers, which every
/// callsite is first compared with) is no higher for it: an INFO, DEBUG or TRACE callsite another layer has off stays
/// off, and a quiet hot path stays as cheap as it was.
#[test]
fn the_layer_raises_no_level_above_warn() {
    let layer = LineCounts::default().layer::<tracing_subscriber::Registry>();
    assert_eq!(layer.max_level_hint(), Some(LevelFilter::WARN));
    let subscriber = tracing_subscriber::registry().with(LineCounts::default().layer());
    assert_eq!(tracing::Subscriber::max_level_hint(&subscriber), Some(LevelFilter::WARN));
}

/// No `init` ran in this test binary, so the process has no counts: not counted, never "nothing logged".
#[test]
fn a_process_with_no_init_has_no_counts() {
    assert!(process_line_counts().is_none());
}
