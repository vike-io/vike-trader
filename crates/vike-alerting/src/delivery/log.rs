//! [`LogSink`]: the always-registered sink, ONE type for every daemon that mounts an engine. Every
//! fired alert becomes one structured `tracing` record on the vike-log file/stderr layer, NEVER
//! stdout (which stays each daemon's protocol channel). What differs between daemons is a parameter:
//! the level, the message prefix and whether the record carries the alert's `title`.
//!
//! ⚠ **The record is written at the DAEMON's callsite**, through the
//! [`log_sink_record!`](crate::log_sink_record) its
//! [`LogSink::new`] is handed: a `tracing` target is a constant of the callsite, so a record written
//! here would carry this module's path instead of the daemon's, and change the daemon's log line.

use tracing::Level;

use super::{AlertSink, FiredAlert};

/// One alert's record, as a [`LogSink`] hands it to its daemon's callsite
/// ([`log_sink_record!`](crate::log_sink_record)).
#[derive(Debug, Clone, Copy)]
pub struct LogSinkRecord<'a> {
    /// The level the record is written at.
    pub level: Level,
    /// The message opens with this and `": "`.
    pub prefix: &'static str,
    /// The alert's title, a field of the record; `None` writes no `title` field.
    pub title: Option<&'a str>,
    /// The fired alert: its `rule_id`, `ts_ms` and `body`.
    pub alert: &'a FiredAlert,
}

/// One structured record per fired alert: `"{prefix}: {body}"` at `level`, with the fields `rule`,
/// `title` (only when `with_title`) and `ts_ms`, in that order, written by the daemon's `record`.
#[derive(Clone, Copy)]
pub struct LogSink {
    level: Level,
    prefix: &'static str,
    with_title: bool,
    record: fn(&LogSinkRecord<'_>),
}

impl std::fmt::Debug for LogSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogSink")
            .field("level", &self.level)
            .field("prefix", &self.prefix)
            .field("with_title", &self.with_title)
            .finish_non_exhaustive()
    }
}

impl LogSink {
    /// A sink writing at `level`, its message opening with `prefix` and `": "`, the alert's title a
    /// field when `with_title`. `record` is the daemon's callsite:
    /// `|r| vike_alerting::log_sink_record!(r)`.
    #[must_use]
    pub const fn new(
        level: Level,
        prefix: &'static str,
        with_title: bool,
        record: fn(&LogSinkRecord<'_>),
    ) -> Self {
        LogSink { level, prefix, with_title, record }
    }
}

impl AlertSink for LogSink {
    fn deliver(&self, alert: &FiredAlert) {
        let title = self.with_title.then_some(alert.title.as_str());
        (self.record)(&LogSinkRecord { level: self.level, prefix: self.prefix, title, alert });
    }
}

/// **Write one [`LogSinkRecord`] at the callsite that expands this**, so the record's `target` is
/// the expanding module's path. A `None` title records no field at all, so a sink without a title
/// writes exactly `rule` and `ts_ms`. The level of a callsite is a constant too: one per level.
#[macro_export]
macro_rules! log_sink_record {
    // The internal arm first: a literal `@` never starts the public arm's `expr` parse.
    (@levels $record:expr, $($level:ident)+) => {{
        let r: &$crate::LogSinkRecord<'_> = $record;
        let title = r.title.map(::tracing::field::display);
        $(
            if r.level == ::tracing::Level::$level {
                ::tracing::event!(
                    ::tracing::Level::$level,
                    rule = %r.alert.rule_id,
                    title,
                    ts_ms = r.alert.ts_ms,
                    "{}: {}",
                    r.prefix,
                    r.alert.body
                );
            }
        )+
    }};
    ($record:expr) => {
        $crate::log_sink_record!(@levels $record, ERROR WARN INFO DEBUG TRACE)
    };
}
