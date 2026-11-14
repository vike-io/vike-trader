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
