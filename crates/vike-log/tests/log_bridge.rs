//! Proves the `log` -> `tracing` bridge: a `log::info!` record must reach a tracing subscriber,
//! which is how third-party crates (wgpu/eframe/ureq/tungstenite) get captured in production.
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);
impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl SharedBuf {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[test]
fn log_facade_record_bridges_into_tracing() {
    let _ = tracing_log::LogTracer::init(); // global, idempotent — installs log -> tracing
    let buf = SharedBuf::default();
    let make = {
        let buf = buf.clone();
        move || buf.clone()
    };
    let sub = tracing_subscriber::fmt().with_writer(make).finish();
    tracing::subscriber::with_default(sub, || {
        log::info!("bridged_marker_xyz");
    });
    assert!(
        buf.contents().contains("bridged_marker_xyz"),
        "log::info! did not reach the tracing subscriber"
    );
}
