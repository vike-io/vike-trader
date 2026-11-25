//! The recent-lines tail: the last [`RECENT_LINES_KEPT`] lines this process logged, kept in memory
//! for a window that shows a process its own log (the desktop's Help → Console).
//!
//! [`RecentLines::layer`] is the one way lines get in: a kept line is the console's own `fmt` rendering
//! of the event (time, level, target, fields), not a second formatter, plus its [`RecentLine::source`],
//! named once as the line arrives so a window filtering by it never re-derives it per frame. A binary
//! hands one to [`crate::LogConfig::recent_lines`]; `init` installs that layer under the console's filter
//! (`RUST_LOG` > the console level, with the credential and noise pins), and records the rolling
//! file's directory beside the lines ([`RecentLines::log_dir`]).
//!
//! ⚠ The lock is held only to push a line or to copy them, never while a caller draws: an event logged
//! from inside a draw (egui logs through the `log` bridge) would re-enter it and deadlock.
//!
//! ⚠ `fmt` hands a writer only `event.metadata()`, whose target is `log` for EVERY record of the `log`
//! bridge (egui, eframe, wgpu). `TargetFirst` writes the bridged record's own target ahead of the
//! rendering and the writer splits it off, so those lines are not all "other".

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tracing::{Event, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::{self, FmtContext, FormatEvent, FormatFields, format::Writer};
use tracing_subscriber::registry::LookupSpan;

/// How many lines the tail keeps; the oldest goes when one more arrives.
pub const RECENT_LINES_KEPT: usize = 5_000;

/// Ends the target [`TargetFirst`] writes ahead of a rendering (the ASCII unit separator).
const TARGET_END: char = '\u{1f}';

/// One logged event: its level, its source, and the console's rendering of it without colour codes or
/// the trailing newline (a multi-line message keeps its inner newlines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentLine {
    pub level: tracing::Level,
    /// The part of the program that logged it, from its target's crate: a workspace crate by its own
    /// name less `vike_`, `_` as `-` (`vike_app_core::ui` is `app-core`); egui's crates are `egui`,
    /// wgpu's and naga `wgpu`, anything else `other`.
    pub source: Arc<str>,
    pub text: Arc<str>,
}

/// [`RecentLine::source`] of an event logged under `target`.
fn source_of(target: &str) -> Arc<str> {
    let krate = target.split_once("::").map_or(target, |(krate, _)| krate);
    match krate.strip_prefix("vike_") {
        Some(name) if !name.is_empty() => name.replace('_', "-").into(),
        _ if krate == "egui" || krate == "eframe" || krate.starts_with("egui_") => "egui".into(),
        _ if krate.starts_with("wgpu") || krate == "naga" => "wgpu".into(),
        _ => "other".into(),
    }
}

#[derive(Default)]
struct Tail {
    lines: VecDeque<RecentLine>,
    log_dir: Option<PathBuf>,
}

/// A shared handle on the tail: every clone sees the same lines.
#[derive(Clone, Default)]
pub struct RecentLines(Arc<Mutex<Tail>>);

impl std::fmt::Debug for RecentLines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecentLines").finish_non_exhaustive()
    }
}

impl RecentLines {
    /// The kept lines, oldest first. A copy (each text is shared, not duplicated), so the caller
    /// draws with no lock held.
    pub fn lines(&self) -> Vec<RecentLine> {
        self.tail().lines.iter().cloned().collect()
    }

    /// The directory of this run's rolling log file, once `init` opened it; `None` when the run
    /// writes no file (file logging off, or its directory could not be opened).
    pub fn log_dir(&self) -> Option<PathBuf> {
        self.tail().log_dir.clone()
    }

    /// The layer that keeps lines here: `fmt`'s default rendering, no colour codes, each line with
    /// its source. `init` installs it under the console's filter; a test installs it directly.
    pub fn layer<S>(&self) -> impl Layer<S> + use<S>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fmt::layer()
            .event_format(TargetFirst)
            .with_writer(TailWriter(self.clone()))
            .with_ansi(false)
    }

    pub(crate) fn set_log_dir(&self, dir: &Path) {
        self.tail().log_dir = Some(dir.to_path_buf());
    }

    fn push(&self, line: RecentLine) {
        let mut tail = self.tail();
        if tail.lines.len() >= RECENT_LINES_KEPT {
            tail.lines.pop_front();
        }
        tail.lines.push_back(line);
    }

    fn tail(&self) -> MutexGuard<'_, Tail> {
        // A panic while the lock was held leaves only whole lines behind: keep showing them.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The tail's event format: the event's own target and [`TARGET_END`], then `fmt`'s default rendering
/// (the console's). A `log` record's own target is its normalized metadata's.
struct TargetFirst;

impl<S, N> FormatEvent<S, N> for TargetFirst
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let normalized = event.normalized_metadata();
        let target = normalized.as_ref().unwrap_or(event.metadata()).target();
        write!(writer, "{target}{TARGET_END}")?;
        fmt::format().format_event(ctx, writer, event)
    }
}

/// The `MakeWriter` behind [`RecentLines::layer`], private so no line gets in without its target.
struct TailWriter(RecentLines);

/// One event's writer: `fmt` writes the rendered event into it, and dropping it keeps the line.
struct LineWriter {
    tail: RecentLines,
    level: tracing::Level,
    bytes: Vec<u8>,
}

impl std::io::Write for LineWriter {
    fn write(&mut self, rendered: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(rendered);
        Ok(rendered.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LineWriter {
    fn drop(&mut self) {
        let rendered = String::from_utf8_lossy(&self.bytes);
        let (target, text) = rendered.split_once(TARGET_END).unwrap_or(("", &*rendered));
        let text = text.trim_end();
        if !text.is_empty() {
            let source = source_of(target);
            self.tail.push(RecentLine { level: self.level, source, text: text.into() });
        }
    }
}

impl<'a> fmt::MakeWriter<'a> for TailWriter {
    type Writer = LineWriter;

    fn make_writer(&'a self) -> LineWriter {
        LineWriter { tail: self.0.clone(), level: tracing::Level::INFO, bytes: Vec::new() }
    }

    /// `fmt` asks through this one with the event's metadata, which is where the level comes from.
    fn make_writer_for(&'a self, metadata: &tracing::Metadata<'_>) -> LineWriter {
        LineWriter { tail: self.0.clone(), level: *metadata.level(), bytes: Vec::new() }
    }
}

#[cfg(test)]
#[path = "recent_tests.rs"]
mod recent_tests;
