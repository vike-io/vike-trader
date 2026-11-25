//! A research bin whose history read FAILS exits failure and prints no report. `cheap_np_depth` and
//! `cheap_np_askgate` read a token's book, trades and quotes per entry: a read that failed used to
//! count as a token with no book (`entries_without_book_series`) or a print with no match
//! (`anchor_fallback`), in a report with exit 0 that looks like a measurement.
//!
//! The shipped bins, dialling the real datahub server over a store whose chosen read fails.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_paths::state_path::SETTINGS_DIR_ENV;

const TOKEN: &str = "TKN";
/// The entry's own stamp: the one book row sits at it, so a range past it reads empty.
const ENTRY_TIMESTAMP: i64 = 2_000;

/// One book row at [`ENTRY_TIMESTAMP`] and no trades or quotes, with the read named `failing` refused.
struct OneFailingRead {
    failing: Option<&'static str>,
}

impl OneFailingRead {
    fn refuse(&self, verb: &str) -> Result<(), HistoryStoreError> {
        match self.failing {
            Some(failing) if failing == verb => {
                Err(HistoryStoreError::Io("the datahub went away".to_string()))
            }
            _ => Ok(()),
        }
    }
}

impl HistoryStore for OneFailingRead {
    fn scan_book_updates(
        &self,
        _: &str,
        _: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.refuse("scan_book_updates")?;
        // Honours the range: the client's pages end on an empty one.
        Ok(range
            .start
            .is_none_or(|start| start <= ENTRY_TIMESTAMP)
            .then(|| L2BookUpdate {
                timestamp: ENTRY_TIMESTAMP,
                local_timestamp: ENTRY_TIMESTAMP,
                sequence: 0,
                kind: L2BookUpdateKind::Snapshot,
                tick_size: 0.01,
                bids: vec![L2BookLevel::new(0.25, 10.0)],
                asks: vec![L2BookLevel::new(0.30, 10.0)],
                symbol: TOKEN.to_string(),
            })
            .into_iter()
            .collect())
    }
    fn scan_trades(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.refuse("scan_trades")?;
        Ok(Vec::new())
    }
    fn scan_quotes(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.refuse("scan_quotes")?;
        Ok(Vec::new())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_symbol_properties, scan_equity,
        scan_exec_fills, scan_exec_orders);
}

/// Write the `config.datahub_addr` row the bin dials, and nothing else a bin reads.
fn plant_hub(settings: &Path, hub: &str) {
    vike_secrets::plant_settings_rows(
        settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "datahub_addr".to_string(),
                value: serde_json::Value::String(hub.to_string()).to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("plant the datahub address");
}

/// Serve `store`, write `file` into a scratch directory whose settings dial that server, and run
/// `bin` there with `args`.
fn run_over(
    bin: &str,
    store: Arc<dyn HistoryStore + Send + Sync>,
    file: (&str, String),
    args: &[&str],
) -> Output {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let hub = listener.local_addr().expect("the bound address").to_string();
    std::thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });

    let scratch = tempfile::tempdir().expect("a scratch directory");
    let settings = scratch.path().join("settings");
    std::fs::create_dir_all(&settings).expect("the settings directory");
    plant_hub(&settings, &hub);
    std::fs::write(scratch.path().join(file.0), file.1).expect("write the input file");

    Command::new(bin)
        .current_dir(scratch.path())
        .env(SETTINGS_DIR_ENV, &settings)
        .args(args)
        .output()
        .expect("run the bin")
}

/// Run `bin` over one entry on a store where the read named `failing` fails.
fn run_bin(bin: &str, failing: Option<&'static str>) -> Output {
    let entries = format!(
        "sts,timestamp_ms,outcome_index,ask,edge,won,token_id\n1,{ENTRY_TIMESTAMP},0,0.3,0.06,1,{TOKEN}\n"
    );
    run_over(
        bin,
        Arc::new(OneFailingRead { failing }),
        ("entries.csv", entries),
        &["--entries", "entries.csv"],
    )
}

const BINS: [&str; 2] =
    [env!("CARGO_BIN_EXE_cheap_np_depth"), env!("CARGO_BIN_EXE_cheap_np_askgate")];

#[test]
fn a_failed_read_exits_failure_and_prints_no_report() {
    for bin in BINS {
        for verb in ["scan_book_updates", "scan_trades", "scan_quotes"] {
            let out = run_bin(bin, Some(verb));
            let (stdout, stderr) =
                (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            assert_eq!(out.status.code(), Some(1), "{bin} with {verb} failing: {stdout}\n{stderr}");
            assert!(stdout.is_empty(), "{bin} with {verb} failing printed a report: {stdout}");
            assert!(
                stderr.contains(verb) && stderr.contains(TOKEN) && stderr.contains("went away"),
                "{bin}: the line names the read, the token and the cause: {stderr}"
            );
        }
    }
}

/// The same store with nothing failing: the bins still measure, and an entry whose trades and quotes
/// are truly empty is still a report and not a failure.
#[test]
fn a_store_that_answers_still_reports() {
    for bin in BINS {
        let out = run_bin(bin, None);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{bin}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("history from the datahub at"), "{bin} reports: {stdout}");
    }
}
