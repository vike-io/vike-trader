//! The WIRE arm of the Data-Manager / chart-gap backfill (split-plane REQ-9, the GUI half): run
//! planned [`BackfillJob`]s through the datahub backfill-on-demand verb
//! (`crates/vike-datahub-client/src/client.rs`'s `backfill`) instead of the GUI's own venue REST
//! calls — "History is fetched by the backend, once, into the store — clients request, never
//! fetch" (split-plane Principle 3). The rows land in the SERVER's store (write-through before
//! the `BackfillDone` frame is written), so the caller re-reads them from the remote store
//! afterwards; nothing streams back through this module.
//!
//! Shape: [`run_wire_backfill`] does the blocking TCP work (dial, capability check, one verb call
//! per planned range) and returns a [`WireBackfillReport`] — a plain value, so the outcome
//! folding is testable against a scripted loopback server (the
//! `crates/vike-datahub-client/tests/backfill_negotiation.rs` pattern) without a GUI.
//! [`render_wire_backfill_status`] turns the report into the one status line the Data Manager's
//! existing backfill slot renders. Every refusal shape gets a sentence naming the fix — an
//! off-roster venue surfaces the SERVER's own refusal text, an old server is told apart from an
//! unreachable one — never a silent no-op.

use vike_datahub_client::{DatahubClient, FEATURE_BACKFILL};

use crate::backfill_plan::BackfillJob;

/// How a wire backfill run ended. One value per bulk run, rendered by
/// [`render_wire_backfill_status`] into the Data Manager's status slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireBackfillReport {
    /// The TCP dial (or the version handshake riding it) failed — nothing was sent.
    ConnectFailed { addr: String, error: String },
    /// The server's `Welcome.features` does not advertise [`FEATURE_BACKFILL`] — it predates the
    /// verb, or was built without `backfill-serve`. Refused CLIENT-side, before any request frame
    /// (the verb shipped without a `PROTO_VERSION` bump, so this capability check is the whole of
    /// its compatibility story — see `DatahubClient::backfill`'s doc). `features` is what the
    /// server DID advertise, for the operator's diagnosis.
    ServerUnsupported { addr: String, features: Vec<String> },
    /// The run happened. `ok`/`failed` count JOBS (a job fails when ANY of its ranges did —
    /// the local arm's tally semantics); `skipped` passes through the planner's unsupported-kind
    /// count; `rows_written` sums the server's per-range answers. `first_error` carries the first
    /// failure's text VERBATIM — for an off-roster venue that is the server's own refusal naming
    /// its supported set — so a failed run always says why, not just how many.
    Ran { ok: usize, failed: usize, skipped: usize, rows_written: u64, first_error: Option<String> },
}

/// Run `jobs` against the datahub at `addr`, one verb call per planned range, blocking — call it
/// from the same worker-thread shape the local arm uses, never the UI thread. `skipped` is the
/// planner's unsupported-series count, threaded through to the final tally untouched.
///
/// The capability check happens ONCE, up front: an unadvertised server returns
/// [`WireBackfillReport::ServerUnsupported`] with zero request frames sent, rather than
/// collecting the same per-call refusal `jobs.len()` times from `DatahubClient::backfill`'s own
/// guard. After that, a per-range failure (server refusal, transport death mid-run) marks its job
/// failed and the run continues — the local arm's keep-going shape, so one bad venue never
/// strands the rest of the selection.
pub fn run_wire_backfill(addr: &str, jobs: &[BackfillJob], skipped: usize) -> WireBackfillReport {
    let mut client = match DatahubClient::connect(addr) {
        Ok(client) => client,
        Err(e) => {
            return WireBackfillReport::ConnectFailed {
                addr: addr.to_string(),
                error: e.to_string(),
            }
        }
    };
    if !client.features().iter().any(|f| f == FEATURE_BACKFILL) {
        return WireBackfillReport::ServerUnsupported {
            addr: addr.to_string(),
            features: client.features().to_vec(),
        };
    }
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut rows_written = 0u64;
    let mut first_error: Option<String> = None;
    for job in jobs {
        let mut job_ok = true;
        for &(start_ms, end_ms) in &job.ranges {
            match client.backfill(&job.venue, &job.symbol, &job.interval, start_ms, end_ms) {
                Ok(done) => rows_written += done.rows_written,
                Err(e) => {
                    tracing::warn!(
                        "wire backfill {}/{}/{} [{start_ms},{end_ms}] via {addr} failed: {e}",
                        job.venue,
                        job.symbol,
                        job.interval
                    );
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                    job_ok = false;
                }
            }
        }
        if job_ok {
            ok += 1;
        } else {
            failed += 1;
        }
    }
    WireBackfillReport::Ran { ok, failed, skipped, rows_written, first_error }
}

/// The in-flight status line the wire arm shows while its worker runs — the wire twin of the
/// local arm's "Backfilling N series (M skipped)…", naming the datahub so an operator watching a
/// slow run knows which plane is fetching.
pub fn wire_backfill_running_status(job_count: usize, skipped: usize, addr: &str) -> String {
    format!("Backfilling {job_count} series via datahub {addr} ({skipped} skipped)…")
}

/// One [`WireBackfillReport`] → the one status line the Data Manager's backfill slot renders.
/// Each refusal shape names its fix; the `Ran` line carries `first_error` verbatim so a
/// server-side refusal (e.g. an off-roster venue: "venue `X` has no collector … Supported: […]")
/// reaches the operator instead of dissolving into a bare failure count.
pub fn render_wire_backfill_status(report: &WireBackfillReport) -> String {
    match report {
        WireBackfillReport::ConnectFailed { addr, error } => {
            format!("Backfill failed: datahub {addr} unreachable ({error}) — nothing was sent")
        }
        WireBackfillReport::ServerUnsupported { addr, features } => format!(
            "Backfill unavailable: the datahub at {addr} predates backfill-on-demand (no \
             \"{FEATURE_BACKFILL}\" in its advertised features [{}]) — nothing was sent. Upgrade \
             it to a `backfill-serve` build, or unset config.datahub_addr to backfill the local \
             store.",
            features.join(", ")
        ),
        WireBackfillReport::Ran { ok, failed, skipped, rows_written, first_error } => {
            let mut line = format!(
                "Backfill done via datahub: {ok} backfilled ({rows_written} rows), {failed} \
                 failed, {skipped} skipped"
            );
            if let Some(e) = first_error {
                line.push_str(&format!(" — first error: {e}"));
            }
            line
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connect_failed_line_names_the_addr_and_says_nothing_was_sent() {
        let line = render_wire_backfill_status(&WireBackfillReport::ConnectFailed {
            addr: "<host>:7878".to_string(),
            error: "connection refused".to_string(),
        });
        assert!(line.contains("<host>:7878"), "{line}");
        assert!(line.contains("connection refused"), "{line}");
        assert!(line.contains("nothing was sent"), "{line}");
    }

    #[test]
    fn the_unsupported_server_line_says_it_predates_backfill_and_names_both_fixes() {
        let line = render_wire_backfill_status(&WireBackfillReport::ServerUnsupported {
            addr: "<host>:7878".to_string(),
            features: vec!["backtest".to_string(), "load_bars".to_string()],
        });
        assert!(line.contains("predates backfill-on-demand"), "{line}");
        assert!(line.contains("backtest, load_bars"), "the advertised set is shown: {line}");
        assert!(line.contains("backfill-serve"), "the server-side fix is named: {line}");
        assert!(line.contains("config.datahub_addr"), "the client-side fix is named: {line}");
    }

    #[test]
    fn the_ran_line_counts_like_the_local_arm_and_carries_the_first_error_verbatim() {
        let refusal = "backfill: venue `deribit` has no collector in this build. \
                       Supported: [binance, bybit, okx]";
        let line = render_wire_backfill_status(&WireBackfillReport::Ran {
            ok: 2,
            failed: 1,
            skipped: 3,
            rows_written: 940,
            first_error: Some(refusal.to_string()),
        });
        assert!(line.contains("2 backfilled"), "{line}");
        assert!(line.contains("940 rows"), "{line}");
        assert!(line.contains("1 failed"), "{line}");
        assert!(line.contains("3 skipped"), "{line}");
        assert!(line.contains(refusal), "the server's own refusal text surfaces: {line}");
    }

    #[test]
    fn a_clean_ran_line_has_no_error_suffix() {
        let line = render_wire_backfill_status(&WireBackfillReport::Ran {
            ok: 1,
            failed: 0,
            skipped: 0,
            rows_written: 12,
            first_error: None,
        });
        assert!(!line.contains("first error"), "{line}");
    }

    #[test]
    fn the_running_status_names_the_datahub() {
        let line = wire_backfill_running_status(4, 2, "127.0.0.1:7878");
        assert!(line.contains("4 series"), "{line}");
        assert!(line.contains("127.0.0.1:7878"), "{line}");
        assert!(line.contains("2 skipped"), "{line}");
    }
}
