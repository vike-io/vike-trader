//! `chart_seed` — **when the backend's store holds nothing for a chart, ASK THE SERVER TO FETCH
//! IT**, and when it will not, say why.
//!
//! # Where this sits
//!
//! [`store_bars`](crate::store_bars) closed half of the empty-chart defect: a chart now READS the
//! backend's store for the interval it was given. Its own module doc named the other half —
//!
//! > It never asks the backend to FETCH from a venue. […] against a store with no `5m` rows it
//! > returns nothing and the chart's badge says so.
//!
//! — and on a box whose store holds no bar rows at all, that "returns nothing" is every key a chart
//! can ask for. This module is the other half, and it is a THIN caller by design: every bound on the
//! fetch is the server's, so what is left here is one plan rule, one blocking call, and one sentence
//! for the operator.
//!
//! # Why this can use the desktop's OWN key, where `backfill_wire` cannot
//!
//! [`backfill_wire::run_wire_backfill`](crate::backfill_wire::run_wire_backfill) carries a ⚠ saying
//! it dials UNAUTHENTICATED on purpose — `Request::Backfill` is `VerbScope::Control`, and
//! [`backend_registry::BackendRecord::datahub_observe_key`](crate::backend_registry::BackendRecord)
//! states why this binary may not hold a datahub control key (that scope also carries the verbs
//! which compile client-supplied Rhai). The consequence it declares is that against any datahub
//! bound off-box — which the bind guard requires to be keyed — that path cannot work at all.
//!
//! `Request::SeedSeries` is `VerbScope::Observe`
//! (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`), so this module dials with the
//! observe key the desktop already resolves for its store reads and its market-data plane. That is
//! the narrow case `run_wire_backfill`'s ⚠ names as needing "a SERVER-side or CLI-side path that
//! already holds control scope" — reached instead by making the narrow case not need control.
//!
//! # The ONCE rule is the CALLER's, and it is already written
//!
//! Nothing here holds a ledger. The plan's input is
//! [`store_bars::StoreReadOutcome::empty`](crate::store_bars::StoreReadOutcome::empty), which is
//! produced by a read the caller already gated on `App::store_asked` — a key is recorded as asked
//! BEFORE the read is spawned, so a series is read once per session and therefore seeded at most
//! once per session. ⚠ **That is only the near leg**: the SERVER keeps its own per-process ledger
//! (`crates/vike-datahub/src/seed.rs`'s `SeedLane`) and answers a repeat off its store without
//! calling a venue, so a client that loses its ledger still costs one fetch. Neither leg substitutes
//! for the other, the same pairing every capability on this wire uses.

use vike_datahub_client::{DatahubClient, FEATURE_SEED_SERIES, NodeKeys, Scope};

use crate::store_bars::StoreBarRequest;

/// How a chart-seed run ended. A plain value, so the outcome folding is testable without a GUI and
/// without a socket — the [`crate::backfill_wire::WireBackfillReport`] shape, deliberately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChartSeedReport {
    /// Nothing was planned — every empty series was already asked, or there were none.
    NothingToDo,
    /// The TCP dial (or the handshake riding it) failed; nothing was sent.
    ConnectFailed { addr: String, error: String },
    /// The server advertises no [`FEATURE_SEED_SERIES`]. Refused CLIENT-side, before a frame.
    ///
    /// ⚠ **On a modern server this almost always means the OPERATOR has not armed the lane**,
    /// not that the server is old — which is why the rendered line names the variable rather than a
    /// rebuild. It is the one capability on this wire whose absence is a configuration rather than a
    /// build.
    ServerUnsupported { addr: String, features: Vec<String> },
    /// The run happened. `seeded` counts series the server reported rows for, `already` those its
    /// own ledger answered, `empty` those it fetched and found nothing for (a symbol the venue does
    /// not list), `failed` those it refused or errored on.
    ///
    /// ⚠ `unarmed` is SEPARATE from `failed` and must stay so: a `SeedDone { armed: false }` is a
    /// SUCCESS, and folding it into a failure count would report a correctly-configured default as
    /// a fault.
    Ran {
        seeded: usize,
        already: usize,
        empty: usize,
        failed: usize,
        unarmed: bool,
        rows: u64,
        first_error: Option<String>,
    },
}

/// The plan: which of the EMPTY series to ask the server to seed.
///
/// Deliberately the smallest rule that can exist, because every real bound is the server's: drop
/// what this client can already predict the server would refuse — an interval outside
/// [`vike_datahub_client::seed::SEED_INTERVALS`] or a symbol outside its charset — and deduplicate.
///
/// ⚠ **The local validation is for the MESSAGE, never for the enforcement.** The server re-checks
/// both at its own door before it dispatches to a bridge, which is where the property actually
/// lives (`crates/vike-datahub-client/src/seed.rs`'s module doc argues it). Filtering here only
/// keeps a `1s` chart from spending a round trip to be told what this process already knew.
pub fn plan_chart_seeds(empty: &[StoreBarRequest]) -> Vec<StoreBarRequest> {
    let mut out: Vec<StoreBarRequest> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in empty {
        if vike_datahub_client::seed::validate_seed_interval(&r.interval).is_err() {
            continue;
        }
        if vike_datahub_client::seed::validate_seed_symbol(&r.symbol).is_err() {
            continue;
        }
        if seen.insert(r.key()) {
            out.push(r.clone());
        }
    }
    out
}

/// Ask the datahub at `addr` to seed each planned series, blocking — call it from the worker thread
/// the store read already runs on, never the UI thread.
///
/// The capability check happens ONCE, up front (the `run_wire_backfill` shape): an unadvertising
/// server returns [`ChartSeedReport::ServerUnsupported`] with zero request frames sent, rather than
/// collecting the same local refusal once per series.
///
/// `keys` is the desktop's OBSERVE pair — see this module's header for why it may hold one here and
/// may not for `Backfill`. `None` dials unauthenticated, which is correct against a key-less
/// loopback dev server and fails at the handshake against a keyed one, naming what is missing.
pub fn run_chart_seed(
    addr: &str,
    keys: Option<&NodeKeys>,
    plan: &[StoreBarRequest],
) -> ChartSeedReport {
    if plan.is_empty() {
        return ChartSeedReport::NothingToDo;
    }
    let dialled = match keys {
        Some(k) => DatahubClient::connect_authed(addr, k, Scope::Observe),
        None => DatahubClient::connect(addr),
    };
    let mut client = match dialled {
        Ok(c) => c,
        Err(e) => {
            return ChartSeedReport::ConnectFailed { addr: addr.to_string(), error: e.to_string() };
        }
    };
    if !client.features().iter().any(|f| f == FEATURE_SEED_SERIES) {
        return ChartSeedReport::ServerUnsupported {
            addr: addr.to_string(),
            features: client.features().to_vec(),
        };
    }
    let (mut seeded, mut already, mut empty, mut failed) = (0usize, 0usize, 0usize, 0usize);
    let mut unarmed = false;
    let mut rows = 0u64;
    let mut first_error: Option<String> = None;
    for r in plan {
        match client.seed_series(&r.venue, &r.symbol, &r.interval) {
            Ok(done) if !done.armed => unarmed = true,
            Ok(done) if done.repeated => already += 1,
            Ok(done) if done.first_ts.is_some() => {
                rows += done.rows_written;
                seeded += 1;
            }
            // Fetched, and the venue had nothing — an honest answer for a symbol it does not list.
            Ok(_) => empty += 1,
            Err(e) => {
                tracing::warn!(series = %r.key(), "chart seed via {addr} failed: {e}");
                if first_error.is_none() {
                    first_error = Some(e);
                }
                failed += 1;
            }
        }
    }
    ChartSeedReport::Ran { seeded, already, empty, failed, unarmed, rows, first_error }
}

/// One [`ChartSeedReport`] → the sentence an operator staring at an empty chart needs, or `None`
/// when there is nothing worth saying.
///
/// ⚠ **The `ServerUnsupported` and `unarmed` lines are the whole point of this function.** Without
/// them, a server that will not fetch and a venue that has no such symbol produce the identical
/// blank chart, and the operator's next move is to doubt the venue. Each line therefore names the
/// EXACT variable and the box it goes on.
pub fn render_chart_seed_status(report: &ChartSeedReport) -> Option<String> {
    match report {
        ChartSeedReport::NothingToDo => None,
        ChartSeedReport::ConnectFailed { addr, error } => Some(format!(
            "This chart has no history and the datahub at {addr} could not be reached ({error}) — \
             nothing was requested."
        )),
        ChartSeedReport::ServerUnsupported { addr, features } => Some(format!(
            "This chart has no history and the datahub at {addr} will not fetch any: it does not \
             advertise \"{FEATURE_SEED_SERIES}\" (it advertised [{}]) — nothing was sent. Set \
             VIKE_DATAHUB_CHART_SEED=1 on that server and restart it; if it still does not \
             advertise, its build is missing `--features backfill-serve`.",
            features.join(", ")
        )),
        ChartSeedReport::Ran { unarmed: true, .. } => Some(format!(
            "This chart has no history and the datahub answered without fetching: its chart-seed \
             lane is not armed. Set VIKE_DATAHUB_CHART_SEED=1 on that server and restart it. (It \
             is off by default because an armed lane spends that box's venue-API budget for \
             read-only clients — {FEATURE_SEED_SERIES} is the capability it then advertises.)"
        )),
        ChartSeedReport::Ran { seeded, already, empty, failed, rows, first_error, .. } => {
            if *seeded == 0 && *already == 0 && *empty == 0 && *failed == 0 {
                return None;
            }
            let mut line = format!(
                "Chart history requested from the datahub: {seeded} fetched ({rows} rows), \
                 {already} already held, {empty} not offered by the venue, {failed} failed"
            );
            if let Some(e) = first_error {
                line.push_str(&format!(" — first error: {e}"));
            }
            Some(line)
        }
    }
}

/// Everything the background thread needs to run the gap arm: where to dial, what to sign with, and
/// where to leave the one sentence an operator may need to read.
///
/// A VALUE rather than a closure, so the whole gap arm lives in this CI-gated crate and the shell
/// carries only construction and rendering — the `ci_excluded_gui_shell_ratchet` rule. It is
/// constructed only when the caller actually resolved a datahub address; `None` at the call site is
/// byte-identical to the behaviour before this module existed.
pub struct SeedDial {
    /// The resolved datahub address, already chosen by the caller's own address resolution.
    pub addr: String,
    /// The desktop's OBSERVE pair. See this module's header for why it may hold one here.
    pub keys: Option<NodeKeys>,
    /// Where [`Self::run`] leaves [`render_chart_seed_status`]'s line for the shell to paint. `None`
    /// means there is nothing worth saying, and the shell shows the ordinary empty-chart hint.
    pub note: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl SeedDial {
    /// Build one from the facts a COMPOSITION ROOT owns — the resolved address, the settings
    /// directory, the process environment sweep and the active backend record's key NAME — doing
    /// the observe-key resolution here rather than in the shell.
    ///
    /// ⚠ It is the exact twin of [`crate::backend_registry::remote_hist_store`] and lands for the
    /// same reason that function's body did: the hard half is the key resolution, the resolution
    /// belongs beside the other one that reads the same name, and
    /// `crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs` is why a GUI-only wrapper is the
    /// wrong home for it. The shell keeps only what it structurally must — the two `OnceLock`s
    /// holding the settings directory and the environment, which only a binary may read.
    ///
    /// Key-resolution notices are LOGGED here rather than returned: this is the third caller of the
    /// same resolution in one frame, and a decision-0051 legacy-store notice the operator has
    /// already seen twice is not worth a third surface.
    pub fn resolve(
        addr: String,
        settings_dir: Option<&str>,
        env: &std::collections::HashMap<String, String>,
        key_name: &str,
        note: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> Self {
        let (keys, notices) =
            crate::backend_registry::datahub_observe_keys(settings_dir, env, key_name);
        for n in notices {
            tracing::info!("{n}");
        }
        Self { addr, keys, note }
    }

    /// Plan, run, publish the note, and answer the series the SERVER reported rows for — the ones
    /// a re-read will actually find something in.
    ///
    /// ⚠ The note is written on EVERY outcome including the quiet ones, because a stale line from a
    /// previous batch is worse than no line: it would tell an operator to set a variable they have
    /// since set.
    pub fn run(&self, empty: &[StoreBarRequest]) -> Vec<StoreBarRequest> {
        let plan = plan_chart_seeds(empty);
        let report = run_chart_seed(&self.addr, self.keys.as_ref(), &plan);
        let line = render_chart_seed_status(&report);
        if let Ok(mut slot) = self.note.lock() {
            *slot = line;
        }
        match report {
            // Only a run that actually seeded something is worth re-reading for. `already` is
            // deliberately included: the server holds those rows, this session simply has not read
            // them (a fresh desktop against a long-lived daemon is exactly that case).
            ChartSeedReport::Ran { seeded, already, .. } if seeded + already > 0 => plan,
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(venue: &str, symbol: &str, interval: &str) -> StoreBarRequest {
        StoreBarRequest {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
        }
    }

    #[test]
    fn the_plan_drops_what_the_server_would_refuse_and_deduplicates() {
        let plan = plan_chart_seeds(&[
            req("binance", "BTCUSDT", "5m"),
            req("binance", "BTCUSDT", "5m"), // duplicate: one request
            req("binance", "BTCUSDT", "1s"), // binance serves it; the SET does not
            req("binance", "BTC USDT", "5m"), // charset
            req("okx", "BTC-USDT-SWAP", "1h"),
        ]);
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert_eq!(plan[0].symbol, "BTCUSDT");
        assert_eq!(plan[1].venue, "okx");
    }

    #[test]
    fn an_empty_plan_sends_nothing_and_says_nothing() {
        let r = run_chart_seed("127.0.0.1:1", None, &[]);
        assert_eq!(r, ChartSeedReport::NothingToDo);
        assert_eq!(render_chart_seed_status(&r), None);
    }

    #[test]
    fn an_old_or_unarmed_server_is_told_apart_from_an_unreachable_one_and_names_the_switch() {
        // The test the brief's fifth requirement names: the client does not send, and the operator
        // is told WHY rather than watching an empty chart.
        let line = render_chart_seed_status(&ChartSeedReport::ServerUnsupported {
            addr: "<host>:7878".to_string(),
            features: vec!["load_bars".to_string(), "backfill".to_string()],
        })
        .expect("a line");
        assert!(line.contains("<host>:7878"), "{line}");
        assert!(line.contains("VIKE_DATAHUB_CHART_SEED=1"), "the switch is named: {line}");
        assert!(line.contains("restart"), "and the action: {line}");
        assert!(line.contains("backfill-serve"), "and the build fallback: {line}");
        assert!(line.contains("load_bars, backfill"), "the advertised set is shown: {line}");
        assert!(line.contains("nothing was sent"), "{line}");

        let down = render_chart_seed_status(&ChartSeedReport::ConnectFailed {
            addr: "<host>:7878".to_string(),
            error: "connection refused".to_string(),
        })
        .expect("a line");
        assert!(down.contains("could not be reached"), "{down}");
        assert!(!down.contains("VIKE_DATAHUB_CHART_SEED"), "a dead server is not a config problem");
    }

    #[test]
    fn an_unarmed_answer_is_reported_as_configuration_not_as_failure() {
        // `armed: false` is a SUCCESS. The line must send the operator to the switch, and must not
        // be reachable through the failure count.
        let line = render_chart_seed_status(&ChartSeedReport::Ran {
            seeded: 0,
            already: 0,
            empty: 0,
            failed: 0,
            unarmed: true,
            rows: 0,
            first_error: None,
        })
        .expect("a line");
        assert!(line.contains("not armed"), "{line}");
        assert!(line.contains("VIKE_DATAHUB_CHART_SEED=1"), "{line}");
        assert!(!line.contains("failed"), "an unarmed lane is not a failure: {line}");
    }

    #[test]
    fn a_ran_line_counts_each_outcome_and_carries_the_first_error_verbatim() {
        let refusal = "seed: venue `polymarket` has no collector in this build. \
                       Supported: [binance, bybit, okx, aster, deribit, hyperliquid]";
        let line = render_chart_seed_status(&ChartSeedReport::Ran {
            seeded: 2,
            already: 1,
            empty: 1,
            failed: 1,
            unarmed: false,
            rows: 1_200,
            first_error: Some(refusal.to_string()),
        })
        .expect("a line");
        assert!(line.contains("2 fetched"), "{line}");
        assert!(line.contains("1200 rows"), "{line}");
        assert!(line.contains("1 already held"), "{line}");
        assert!(line.contains("1 not offered by the venue"), "{line}");
        assert!(line.contains(refusal), "the server's own refusal surfaces: {line}");
    }

    #[test]
    fn a_run_that_did_nothing_at_all_says_nothing() {
        assert_eq!(
            render_chart_seed_status(&ChartSeedReport::Ran {
                seeded: 0,
                already: 0,
                empty: 0,
                failed: 0,
                unarmed: false,
                rows: 0,
                first_error: None,
            }),
            None
        );
    }
}
