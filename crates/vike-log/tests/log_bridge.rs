//! Proves the `log` -> `tracing` bridge: a `log::info!` record must reach a tracing subscriber,
//! which is how third-party crates (wgpu/eframe/ureq/tungstenite) get captured in production.
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct SharedBuffer(Arc<Mutex<Vec<u8>>>);
impl Write for SharedBuffer {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl SharedBuffer {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[test]
fn log_facade_record_bridges_into_tracing() {
    let _ = tracing_log::LogTracer::init(); // global, idempotent — installs log -> tracing
    let buffer = SharedBuffer::default();
    let make = {
        let buffer = buffer.clone();
        move || buffer.clone()
    };
    let sub = tracing_subscriber::fmt().with_writer(make).finish();
    vike_log::capture::scoped(sub, || {
        log::info!("bridged_marker_xyz");
    });
    assert!(
        buffer.contents().contains("bridged_marker_xyz"),
        "log::info! did not reach the tracing subscriber"
    );
}

/// A bridged record keeps its OWN target in the recent tail: `fmt` hands the writer metadata whose
/// target is `log` for every bridged record, so without the tail's format every egui and wgpu line
/// would be "other".
#[test]
fn a_bridged_record_is_kept_under_its_own_crates_source() {
    use tracing_subscriber::layer::SubscriberExt;
    let _ = tracing_log::LogTracer::init();
    let recent = vike_log::recent::RecentLines::default();
    let sub = tracing_subscriber::registry().with(recent.layer());
    vike_log::capture::scoped(sub, || {
        log::info!(target: "egui_wgpu", "There are 4 available wgpu adapters");
    });
    let lines = recent.lines();
    let line =
        lines.iter().find(|l| l.text.contains("wgpu adapters")).expect("the record was kept");
    assert_eq!(&*line.source, "egui", "{:?}", line.text);
    assert!(line.text.contains("egui_wgpu: There are 4"), "{:?}", line.text);
}
