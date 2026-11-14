//! The run manifest's shared `detail` halves (data, realism) and a search's data WITNESS.

use crate::run_fingerprint;
use vike_backtest::data_plan::DataPlanSeriesRole;
use vike_backtest::harness::ProfileBacktest;
use vike_data::HistoryStore;
use vike_model::run_request::replay::ReplayKind;

/// The `data` half of a run manifest's `detail` — what slice was loaded and, when the producer
/// could ask, what the store held for it.
///
/// Factored out of [`persist_run`]'s literal rather than written twice: [`finish_search_run`]
/// answers the same question about the same profile, and two copies of these keys would drift the
/// first time either changed.
///
/// ⚠ **`store` is deliberately NOT folded in here**, even though both producers also record it. It
/// sits one level UP, beside `strategy` and `build`, in the document `persist_run` has been writing
/// since before this function existed — moving it inside `data` to make one tidier helper would
/// change the shape of a document already on people's disks for no reader's benefit. The search
/// producer spells the same one-line key at the same level, which is what keeps the two manifests
/// the same SHAPE rather than merely sharing a function.
///
/// `data` is `None` for a producer that could not inventory the store, and for nothing else.
///
/// ⚠ **A SEARCH used to be permanently that producer and is not any more.** The claim here was that
/// `collect_data_fingerprint` costs two manifest parses per series, so a search — which is about to
/// run hundreds of backtests — could not afford it and recorded `data.fingerprint: null` with a
/// pid-form run id. `HistoryStoreDataFusion::series_facts` removed the second parse, and a search was
/// already paying the first one for `vike_backtest::trial_ledger::SearchIdentity::store_data`, so both
/// producers fill this in now for the same one-parse-per-series price. The single-run call site is
/// still BELOW the sweep branch; nothing was hoisted.
///
/// What a resume compares is still `SearchIdentity` rather than this — it witnesses the ingest
/// COMMIT KEYS, which the ADDRESS deliberately excludes, so the two answer different questions and
/// neither replaces the other.
pub(super) fn run_detail_data(
    profile: &ProfileBacktest,
    data: Option<&run_fingerprint::DataFingerprint>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": match profile.data.kind {
            ReplayKind::Bar => "bar",
            ReplayKind::Tick => "tick",
        },
        "interval": profile.data.interval,
        "from": profile.data.from,
        "to": profile.data.to,
        // `resolved_series` so BOTH profile spellings — `venue` x `symbols` and the cross-venue
        // `[[data.series]]` array — record the same thing: what was loaded.
        "series": profile
            .data
            .resolved_series()
            .iter()
            .map(|s| format!("{}:{}", s.venue, s.symbol))
            .collect::<Vec<_>>(),
        // The fingerprint NESTS under `data` beside the slice it describes, so a reader that
        // already knows where to look for the window finds the bytes there too. `null` when this
        // producer had no store to ask.
        "fingerprint": data,
    })
}

/// **The KEPT instrument rows the run read** (`vike_runs::RunManifest::kept_rows`): the row of each series the
/// profile loads and of each strategy input the store was asked for (`data`'s inputs), each once in that order.
/// Empty for a run no door handed the lists (`ProfileBacktest::kept`), and for one whose venues keep none.
pub(super) fn kept_rows_read(
    profile: &ProfileBacktest,
    data: Option<&run_fingerprint::DataFingerprint>,
) -> Vec<vike_model::Instrument> {
    let series = profile.data.resolved_series().into_iter().map(|s| (s.venue, s.symbol));
    let inputs = data.into_iter().flat_map(|d| &d.inputs).map(|i| &i.series.id);
    let mut read: Vec<vike_model::Instrument> = Vec::new();
    for (venue, symbol) in series.chain(inputs.map(|id| (id.venue.clone(), id.symbol.clone()))) {
        if let Some(row) = profile.kept.row(&venue, &symbol)
            && !read.contains(row)
        {
            read.push(row.clone());
        }
    }
    read
}

/// The `realism` half of a run manifest's `detail`: the COST MODEL, in the keys a LISTING needs.
///
/// ⚠ **The words and the digest, not the whole stamp, and that asymmetry is the point.** The full
/// resolved key set is in `report.json` (`vike_analytics::report::BacktestReport::realism`), which
/// is where a reader goes to compare two runs key by key. The manifest's job is the one a listing
/// does without opening anything else — "is this run comparable to the other forty", and above all
/// "what was it charged" — so it carries the stamp's words and a one-line summary and nothing more.
/// Repeating forty keys in a second document per run would buy no reader anything and would make
/// the two documents a pair somebody has to keep in step.
///
/// Factored out for the same reason as [`run_detail_data`]: two producers write it (a run stamps
/// with `realism_stamp_of_run`, a search its base profile with `realism_stamp`), and two copies of
/// these keys would drift the first time either changed.
pub(super) fn run_detail_realism(stamp: &vike_analytics::RealismStamp) -> serde_json::Value {
    serde_json::json!({
        // The RELEASED key (released JSON only gains fields): the reason when every fill was free,
        // else `null`. A run that states no fee is charged its venue's table, so it is `null`.
        "frictionless": stamp.charged_nothing,
        "digest": stamp.digest(),
        // What the run was charged, in words (`vike_analytics::realism::RealismStampCosts`).
        "charged": stamp.charged,
    })
}

/// What the store HELD for every series this search's profile resolves to — the witness
/// `vike_backtest::trial_ledger::SearchIdentity::store_data` carries, and the input `--resume`'s gate needs
/// in order to be sound at all.
///
/// # Why this exists, in one sentence
///
/// The store PATH is not the store. Without this, a search killed at trial 60 of 200, a backfill or
/// a corrected re-fetch inside the profile's own `[data] from/to`, and a `--resume` would rank 60
/// cached scores computed on dataset A against 140 computed on dataset B — no error, no warning,
/// and no field in any persisted document from which a reader could detect it afterwards.
/// `SearchIdentity`'s own doc carries the argument in full.
///
/// # ⚠ It costs NOTHING, because it is a RENDERING rather than a read
///
/// **This used to walk the store itself** — one `list_series` plus one `series_commits` per series
/// — and the argument for a second walk was that [`collect_data_fingerprint`] cost TWO manifest
/// parses per series where this cost one. That is no longer true: `HistoryStoreDataFusion::series_facts`
/// answers coverage and commits from ONE parse, so the collector costs exactly what this walk cost,
/// and everything this function needs is already in the record the collector returns. The witness
/// is now a pure function of [`run_fingerprint::DataFingerprint`], which is why it takes one.
///
/// ⚠ **The rendered TEXT is byte-identical to what the walk produced**, and that is load-bearing
/// rather than tidy: `store_data` is compared as a STRING by
/// `vike_backtest::trial_ledger::SearchIdentity::differences`, so a search started by an older binary must
/// still be `--resume`-able by this one. `coverage: None` is exactly the `!held` case the walk
/// tested, and `commits`/`commits_len` are the same bounded prefix and true count it wrote.
///
/// The one behaviour that did NOT move with it is the DIAGNOSTIC: the collector's failure reaches
/// stderr as `run NOT addressed` on the single-run path, and the search's call site still prints
/// its own line naming the witness. A search has never printed the single-run wording and still
/// does not.
///
/// # What it CATCHES and what it MISSES — both stated
///
/// It is the INGEST COMMIT KEYS: the store's own record of who wrote each series, appended by every
/// write. A backfill that adds a day, a re-fetch that replaces a corrected day and a delete all
/// move it — including the re-fetch case, which a coverage-based witness (rows, dates, first/last
/// ts) would MISS whenever the corrected day has the same shape as the day it replaced. That is why
/// the commits are the witness here even though
/// `crate::run_fingerprint::DataFingerprintSeries::commits` is deliberately NOT part of the run
/// ADDRESS.
///
/// ⚠ **The accepted cost of that choice**, from the same place:
/// `crates/vike-store/src/datafusion_hist/manifest.rs`'s `rebuild_manifest` re-derives keys from part
/// footers and can come back with FEWER than the data was written with. So a manifest rebuild can
/// move this witness without the data moving, and a resume is then REFUSED that need not have been.
/// That is the safe direction — a needless re-run costs time, a wrong reuse costs the answer — and
/// it is why the refusal message names a rebuild-shaped cause rather than asserting a data change.
///
/// ⚠ The keys are bounded by `crate::run_fingerprint::bound_commits`, so a series with a long flush
/// log contributes a bounded prefix plus its true length: an actively-recorded group still moves the
/// witness on its very next write, and the witness itself cannot grow without limit.
///
/// An input's series (decision 0271) is witnessed the same way, one `input <series>` line keyed by
/// the series it reads (the lane included), so a re-import of a lane refuses a resume as a backfill of
/// a price lane does. WHICH series the strategy reads under which name is
/// `vike_backtest::trial_ledger::SearchIdentity::inputs`, with its own refusal sentence. No input: no
/// line, the bytes an older binary wrote.
///
/// A conversion pair (decision 0243, `engine.conversion`) is witnessed the same way, one
/// `conversion "<QUOTE>" <series>` line, so a re-import or a backfill of the pair refuses a resume
/// instead of ranking scores booked at two rates. Which pair the profile names is in its bytes
/// (`SearchIdentity::profile_fnv1a64`). No pair: no line.
pub(super) fn data_witness(data: &run_fingerprint::DataFingerprint) -> String {
    let mut lines: Vec<String> = data.series.iter().map(witness_line).collect();
    lines.extend(
        data.inputs.iter().map(|i| format!("input {:?}{}", i.read, witness_facts(&i.series))),
    );
    lines.extend(data.conversions.iter().map(|c| {
        format!("conversion {:?} {:?}{}", c.quote_currency, c.read, witness_facts(&c.series))
    }));
    joined_witness_lines(lines)
}

/// One series' witness line.
fn witness_line(s: &run_fingerprint::DataFingerprintSeries) -> String {
    let mut line = format!(
        "series {} {} {} {} {}",
        s.id.kind,
        s.id.venue,
        if s.id.symbol.is_empty() { "-" } else { &s.id.symbol },
        s.id.group.as_deref().unwrap_or("-"),
        s.id.interval.as_deref().unwrap_or("-"),
    );
    line.push_str(&witness_facts(s));
    line
}

/// What a witness line says the store held of a series: ` absent`, or ` commits N` and the keys.
fn witness_facts(s: &run_fingerprint::DataFingerprintSeries) -> String {
    use std::fmt::Write;
    let mut line = String::new();
    match &s.coverage {
        // The store holds no such series — a REAL state, and a DIFFERENT one from a series that
        // exists and is empty. It is also the state a later seed or backfill moves AWAY from,
        // which is exactly what this witness is for. `coverage: None` is the collector's own
        // spelling of it, decided against the store's INVENTORY rather than against an
        // all-zero manifest fold (see [`collect_data_fingerprint`]).
        None => line.push_str(" absent"),
        Some(_) => {
            // ⚠ `commits_len` rather than `commits.len()`: the TRUE count, before
            // `run_fingerprint::bound_commits` truncated the prefix. An actively-recorded group
            // moves the witness on its very next write even past the bound, and the witness
            // itself cannot grow without limit.
            let _ = write!(line, " commits {}", s.commits_len);
            for key in &s.commits {
                let _ = write!(line, " {key}");
            }
        }
    }
    line
}

/// SORTED, so the order the loader happened to list series in cannot move the witness — the same
/// rule `run_fingerprint::DataFingerprint::canonical` states for the address.
fn joined_witness_lines(mut lines: Vec<String>) -> String {
    lines.sort();
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// What `vike_backtest::trial_ledger::SearchIdentity::store_data` records: [`data_witness`], then
/// everything else this search's scores are computed from that is neither the profile's bytes nor
/// a price lane.
///
/// A funding history (`engine.attach_funding`), a detail tape (`data.detail_interval`) and an
/// instrument grid (`engine.snap_to_properties`) are store series
/// [`data_witness`]'s input leaves out on purpose, because it also feeds the run ADDRESS; a winners
/// file (`[engine.resolution].path`) is not in the store at all. Without these, a resume after a
/// funding backfill or a corrected winners file ranks old scores against new with no error.
///
/// ⚠ **Each is added ONLY when its key is set, and with none set this reads nothing and answers
/// [`data_witness`]'s bytes**, because `store_data` is compared as a string: a search started by an
/// older binary over such a profile must stay resumable. A search that DID set one, started by an
/// older binary, is refused once: the safe direction, as [`data_witness`]'s doc argues.
///
/// The run ADDRESS does not move: [`collect_data_fingerprint`] and
/// `run_fingerprint::input_fingerprint` are not touched by any of this.
///
/// ⚠ **An instrument grid witnesses only the rows up to the window's end** ([`seen_past_the_window`]).
/// Its recorder adds a row every UTC day, and a fill reads the row as of its own time, so a row past the
/// window changes no score and must not refuse a resume.
///
/// An `Err` is an input that could not be read, which the caller records as
/// `vike_backtest::trial_ledger::DATA_UNREADABLE` (a witness that never compares equal).
pub(super) fn search_witness(
    data: &run_fingerprint::DataFingerprint,
    profile: &ProfileBacktest,
    store: &dyn HistoryStore,
) -> Result<String, String> {
    let mut witness = data_witness(data);
    let extra = vike_backtest::data_plan::auxiliary_series(profile);
    if !extra.is_empty() {
        let held =
            store.list_series().map_err(|e| format!("the store could not list its series: {e}"))?;
        let window_end = profile.range().map_err(|e| e.to_string())?.end;
        let last_day = window_end.map(vike_model::time::epoch_ms_to_utc_date);
        let mut lines = Vec::with_capacity(extra.len());
        for (id, role) in extra {
            let mut series = run_fingerprint::DataFingerprintSeries {
                id,
                coverage: None,
                commits: Vec::new(),
                commits_len: 0,
            };
            if held.contains(&series.id) {
                let (coverage, mut commits) = store
                    .series_facts(&series.id)
                    .map_err(|e| format!("the store could not read {:?}: {e}", series.id))?;
                let written = commits.len();
                if let (DataPlanSeriesRole::Properties, Some(last_day)) = (role, &last_day) {
                    commits.retain(|key| !seen_past_the_window(key, last_day));
                }
                // Every row past the window is a tape this run reads no row of: not held yet.
                if written == 0 || !commits.is_empty() {
                    series.coverage = Some(coverage);
                    (series.commits, series.commits_len) = run_fingerprint::bound_commits(commits);
                }
            }
            lines.push(witness_line(&series));
        }
        witness.push_str(&joined_witness_lines(lines));
    }
    let winners = profile.engine.resolution.as_ref();
    if let Some(file) = winners.and_then(|r| r.winners_file(profile.base_dir.as_deref())) {
        let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        witness.push_str(&format!("winners {}\n", vike_utility::fnv::fnv1a64_hex(&bytes)));
    }
    Ok(witness)
}

/// Whether a grid row's commit key names a UTC day after `last_day`: `PropertiesRecorder::record`'s key ends
/// in the day its row was seen. A key of any other shape names no day and stays, which moves the witness:
/// the safe direction, as [`data_witness`]'s doc argues.
fn seen_past_the_window(key: &str, last_day: &str) -> bool {
    let day = key.rsplit(':').next().unwrap_or_default();
    matches!(day.as_bytes(), [_, _, _, _, b'-', _, _, b'-', _, _]) && day > last_day
}
