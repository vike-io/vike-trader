use super::*;
use tracing_subscriber::layer::SubscriberExt;

/// The tail keeps the newest [`RECENT_LINES_KEPT`] events, oldest first, each with its level, its
/// source and the console's rendering of it; one more event pushes the oldest out.
#[test]
fn the_tail_keeps_the_newest_lines_with_their_levels() {
    let recent = RecentLines::default();
    let subscriber = tracing_subscriber::registry().with(recent.layer());
    crate::capture::scoped(subscriber, || {
        for n in 0..RECENT_LINES_KEPT {
            tracing::info!("line {n}");
        }
        tracing::warn!(venue = "bybit", "the last line");
    });
    let lines = recent.lines();
    assert_eq!(lines.len(), RECENT_LINES_KEPT, "the tail is bounded");
    assert!(lines[0].text.ends_with("line 1"), "line 0 went first: {:?}", lines[0].text);
    let last = lines.last().unwrap();
    assert_eq!(last.level, tracing::Level::WARN);
    assert!(last.text.contains("WARN"), "{:?}", last.text);
    assert!(last.text.contains("the last line venue=\"bybit\""), "{:?}", last.text);
    assert!(!last.text.ends_with('\n') && !last.text.contains('\u{1b}'), "{:?}", last.text);
    assert!(last.text.contains("vike_log::recent::recent_tests:"), "{:?}", last.text);
    assert!(!last.text.contains(TARGET_END), "the target prefix is split off: {:?}", last.text);
    assert_eq!(&*last.source, "log");
    assert_eq!(recent.log_dir(), None, "no file was opened");
}

/// A target's source is its crate's: a workspace crate's own name, egui's and wgpu's crates grouped,
/// anything else `other`.
#[test]
fn a_source_is_named_from_the_targets_crate() {
    for (target, source) in [
        ("vike_desktop", "desktop"),
        ("vike_app_core::backend::server_link", "app-core"),
        ("egui_wgpu", "egui"),
        ("eframe::native::run", "egui"),
        ("egui", "egui"),
        ("wgpu_hal::vulkan::instance", "wgpu"),
        ("naga::front", "wgpu"),
        ("hyper::client", "other"),
        ("vike_", "other"),
        ("", "other"),
    ] {
        assert_eq!(&*source_of(target), source, "{target:?}");
    }
}
