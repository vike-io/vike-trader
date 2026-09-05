//! `RawTap` — best-effort raw-frame capture writer actor (raw-first WS frame tap,
//! docs/superpowers/specs/2026-07-11-raw-frame-tap-design.md).
//!
//! Mirrors `vike_data::live_rec::RecorderSink`: the hot side (`RawTapHandle::frame`, called from
//! the pump thread via `TappedStream`) only ever `try_send`s a row onto a bounded channel — a full
//! or disconnected channel bumps a dropped counter, never blocks or panics. ALL file I/O happens on
//! ONE writer thread that owns the gzip files, so a slow disk never stalls the traded feed. Raw
//! capture is strictly insurance: losing a frame is acceptable; affecting the live feed is not.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::VENUE;

/// Config for [`RawTap::spawn`]. `dir` is the capture root; files land under
/// `dir/polymarket/<token>/date=YYYY-MM-DD/frames-<seq>.jsonl.gz`.
#[derive(Debug, Clone)]
pub struct RawCaptureConfig {
    pub dir: PathBuf,
    /// Bound on the hot-side channel; `try_send` fails (drop+count) past this.
    pub channel_cap: usize,
}

impl Default for RawCaptureConfig {
    fn default() -> Self {
        Self { dir: PathBuf::new(), channel_cap: 65_536 }
    }
}

/// The exact on-disk line for one captured frame: `<local_ns>\t<raw-json>\n`. The `\t` split is
/// safe because a JSON frame never contains a raw newline (serialized frames are single-line) and
/// the leading integer never contains a tab — Task 4's `GzFileStream` inverts this by splitting once
/// on the first `\t`.
pub(crate) fn raw_line(local_ns: i64, text: &str) -> String {
    format!("{local_ns}\t{text}\n")
}

/// One captured frame in flight to the writer thread.
struct Msg {
    token: String,
    local_ns: i64,
    text: String,
}

enum Cmd {
    Frame(Msg),
    Shutdown,
}

/// The cheap hot-side handle held by `TappedStream`. Only ever `try_send`s.
pub struct RawTapHandle {
    tx: SyncSender<Cmd>,
    dropped: Arc<AtomicU64>,
}

impl RawTapHandle {
    /// Enqueue one raw frame for `token_id`, stamped with local receive-ns. Non-blocking: a full or
    /// disconnected channel counts a dropped frame instead of blocking the pump thread or panicking.
    pub fn frame(&self, token_id: &str, local_ns: i64, text: &str) {
        let msg = Cmd::Frame(Msg { token: token_id.to_string(), local_ns, text: text.to_string() });
        if self.tx.try_send(msg).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Owner-side: diagnostics + deterministic teardown. `Drop` mirrors `shutdown()` so a dropped owner
/// never leaks the writer thread (the crate's "never leak a background thread" discipline).
pub struct RawTapOwner {
    tx: SyncSender<Cmd>,
    dropped: Arc<AtomicU64>,
    join: Option<JoinHandle<()>>,
}

impl RawTapOwner {
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
    /// Test accessor for the shared counter (lets a test poll it without consuming the owner).
    #[cfg(test)]
    pub(crate) fn dropped_atomic(&self) -> &Arc<AtomicU64> {
        &self.dropped
    }
    pub fn shutdown(mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for RawTapOwner {
    fn drop(&mut self) {
        if let Some(j) = self.join.take() {
            let _ = self.tx.send(Cmd::Shutdown);
            let _ = j.join();
        }
    }
}

pub struct RawTap;

impl RawTap {
    /// Spawn the writer thread; return the hot-side handle + the owner handle.
    pub fn spawn(cfg: RawCaptureConfig) -> std::io::Result<(Arc<RawTapHandle>, RawTapOwner)> {
        let (tx, rx) = mpsc::sync_channel(cfg.channel_cap);
        let dropped = Arc::new(AtomicU64::new(0));
        let join = thread::Builder::new()
            .name("vike-polymarket-rawtap".into())
            .spawn(move || writer_loop(rx, cfg.dir))?;
        let handle = Arc::new(RawTapHandle { tx: tx.clone(), dropped: dropped.clone() });
        let owner = RawTapOwner { tx, dropped, join: Some(join) };
        Ok((handle, owner))
    }
}

/// One open gzip file for a `(token, UTC-day)`.
struct OpenFile {
    date: String,
    enc: GzEncoder<std::io::BufWriter<std::fs::File>>,
}

/// The writer thread: owns one open gzip file per token, rolling on a UTC-day change. A file/IO
/// error is logged and the frame dropped — never propagated (best-effort insurance).
fn writer_loop(rx: Receiver<Cmd>, root: PathBuf) {
    let mut files: HashMap<String, OpenFile> = HashMap::new();
    let mut seq: u64 = 0;
    while let Ok(cmd) = rx.recv() {
        let msg = match cmd {
            Cmd::Frame(m) => m,
            Cmd::Shutdown => break,
        };
        let date = vike_model::time::epoch_ns_to_utc_date(msg.local_ns);
        // roll on day change (or first frame) for this token
        let need_new = files.get(&msg.token).map(|f| f.date != date).unwrap_or(true);
        if need_new {
            if let Some(old) = files.remove(&msg.token) {
                let _ = old.enc.finish(); // flush + close the previous day's file
            }
            match open_file(&root, &msg.token, &date, seq) {
                Ok(of) => {
                    files.insert(msg.token.clone(), of);
                    seq += 1;
                }
                Err(e) => {
                    tracing::warn!(token = %msg.token, %e, "RawTap: open file failed — dropping frame");
                    continue;
                }
            }
        }
        if let Some(f) = files.get_mut(&msg.token) {
            let line = raw_line(msg.local_ns, &msg.text);
            if let Err(e) = f.enc.write_all(line.as_bytes()) {
                tracing::warn!(token = %msg.token, %e, "RawTap: write failed — dropping frame");
            }
        }
    }
    // flush + close every open file on shutdown/disconnect
    for (_tok, f) in files.drain() {
        let _ = f.enc.finish();
    }
}

fn open_file(
    root: &std::path::Path,
    token: &str,
    date: &str,
    seq: u64,
) -> std::io::Result<OpenFile> {
    let dir = root.join(VENUE).join(token).join(format!("date={date}"));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("frames-{seq:08}.jsonl.gz"));
    let file = std::fs::File::create(path)?;
    let enc = GzEncoder::new(std::io::BufWriter::new(file), Compression::default());
    Ok(OpenFile { date: date.to_string(), enc })
}

use std::time::Duration;

use crate::market_feed::{MarketStream, StreamErr};

/// A [`MarketStream`] decorator that tees each successfully-read frame (verbatim text + local
/// receive-ns) to a [`RawTapHandle`] before returning it unchanged — the raw-first capture seam.
/// `tap == None` is a branchless passthrough (zero-overhead when capture is off). `send_text` and
/// `since_last_frame` pass straight through, so `run_session` is completely unaware of it.
pub struct TappedStream<S: MarketStream> {
    inner: S,
    tap: Option<Arc<RawTapHandle>>,
    token_id: String,
}

impl<S: MarketStream> TappedStream<S> {
    pub fn new(inner: S, tap: Option<Arc<RawTapHandle>>, token_id: String) -> Self {
        Self { inner, tap, token_id }
    }
}

impl<S: MarketStream> MarketStream for TappedStream<S> {
    fn read_frame(&mut self) -> Result<String, StreamErr> {
        let r = self.inner.read_frame();
        if let (Some(tap), Ok(text)) = (&self.tap, &r) {
            tap.frame(&self.token_id, now_ns(), text);
        }
        r
    }
    fn send_text(&mut self, s: &str) -> Result<(), StreamErr> {
        self.inner.send_text(s)
    }
    fn since_last_frame(&self) -> Duration {
        self.inner.since_last_frame()
    }
}

// Local receive time as epoch-nanoseconds (the tap's latency-reconstruction clock): the shared
// `vike_model::now_ns`.
use vike_model::now_ns;

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    const DAY_NS: i64 = 86_400_000_000_000;

    fn read_gz_lines(path: &std::path::Path) -> Vec<String> {
        let f = std::fs::File::open(path).unwrap();
        let mut s = String::new();
        GzDecoder::new(f).read_to_string(&mut s).unwrap();
        s.lines().map(|l| l.to_string()).collect()
    }

    fn count_gz(dir: &std::path::Path) -> usize {
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    n += count_gz(&p);
                } else if p.extension().is_some_and(|x| x == "gz") {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn frames_flush_and_gunzip_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RawCaptureConfig { dir: dir.path().to_path_buf(), channel_cap: 1024 };
        let (handle, owner) = RawTap::spawn(cfg).unwrap();
        handle.frame("TOK", 1_000, r#"{"event_type":"book","x":1}"#);
        handle.frame("TOK", 2_000, r#"{"event_type":"price_change","y":2}"#);
        owner.shutdown();

        // one file for TOK on 1970-01-01
        let series = dir.path().join("polymarket").join("TOK").join("date=1970-01-01");
        assert_eq!(count_gz(&series), 1, "one gz file for the token/day");
        let file = std::fs::read_dir(&series).unwrap().next().unwrap().unwrap().path();
        let lines = read_gz_lines(&file);
        assert_eq!(
            lines,
            vec![
                "1000\t{\"event_type\":\"book\",\"x\":1}".to_string(),
                "2000\t{\"event_type\":\"price_change\",\"y\":2}".to_string(),
            ]
        );
    }

    #[test]
    fn utc_day_rollover_splits_files() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RawCaptureConfig { dir: dir.path().to_path_buf(), channel_cap: 1024 };
        let (handle, owner) = RawTap::spawn(cfg).unwrap();
        handle.frame("TOK", 1_000, "{}"); // 1970-01-01 (local_ns tiny)
        handle.frame("TOK", DAY_NS + 1_000, "{}"); // 1970-01-02
        owner.shutdown();
        assert!(dir.path().join("polymarket/TOK/date=1970-01-01").exists());
        assert!(dir.path().join("polymarket/TOK/date=1970-01-02").exists());
        assert_eq!(count_gz(dir.path()), 2, "one gz file per UTC day");
    }

    #[test]
    fn overflow_drops_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RawCaptureConfig { dir: dir.path().to_path_buf(), channel_cap: 1 };
        let (handle, owner) = RawTap::spawn(cfg).unwrap();
        for i in 0..50_000i64 {
            handle.frame("TOK", i, "{}"); // flood a capacity-1 channel
        }
        // give the writer a beat, then assert some were dropped (never blocked/panicked)
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            owner.dropped_atomic().load(Ordering::Relaxed) > 0,
            "overflow must drop, not block"
        );
        owner.shutdown();
    }

    #[test]
    fn raw_line_format() {
        assert_eq!(raw_line(42, "{\"a\":1}"), "42\t{\"a\":1}\n");
    }

    use crate::market_feed::{MarketStream, StreamErr};
    // The shared scripted MarketStream double (testing-arch Phase 4c) — replaces the inline
    // `CannedStream` copy that used to live here (queued Ok(text) frames, then Err(Closed)).
    use vike_bridge_core::scripted::ScriptedStream;

    #[test]
    fn tapped_stream_tees_each_frame_and_passes_through() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RawCaptureConfig { dir: dir.path().to_path_buf(), channel_cap: 1024 };
        let (handle, owner) = RawTap::spawn(cfg).unwrap();
        let canned = ScriptedStream::from_texts(["{\"a\":1}", "{\"b\":2}"]);
        let mut tapped = TappedStream::new(canned, Some(handle), "TOK".to_string());
        assert_eq!(tapped.read_frame().unwrap(), "{\"a\":1}"); // text passes through verbatim
        assert_eq!(tapped.read_frame().unwrap(), "{\"b\":2}");
        assert!(matches!(tapped.read_frame(), Err(StreamErr::Closed(_)))); // Err not tapped
        owner.shutdown();

        // Read the date directory dynamically (now_ns() returns current time, so date varies)
        let poly_tok_dir = dir.path().join("polymarket").join("TOK");
        let date_dir = std::fs::read_dir(&poly_tok_dir).unwrap().next().unwrap().unwrap().path();
        let file = std::fs::read_dir(&date_dir).unwrap().next().unwrap().unwrap().path();
        let lines = read_gz_lines(&file);
        assert_eq!(lines.len(), 2, "exactly the two Ok frames tapped, the Err was not");
        assert!(lines[0].ends_with("\t{\"a\":1}"));
        assert!(lines[1].ends_with("\t{\"b\":2}"));
    }

    #[test]
    fn tapped_stream_none_is_passthrough() {
        let canned = ScriptedStream::from_texts(["{\"a\":1}"]);
        let mut tapped = TappedStream::new(canned, None, "TOK".to_string());
        assert_eq!(tapped.read_frame().unwrap(), "{\"a\":1}"); // no tap, still passes through
        assert!(tapped.send_text("PING").is_ok()); // send_text passes through
    }
}
