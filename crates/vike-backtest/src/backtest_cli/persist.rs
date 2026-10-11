//! The run manifest's shared `detail` halves (data, realism) and a search's data WITNESS.

use crate::harness::{self, BacktestProfile};
use crate::run_fingerprint;

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
/// pid-form run id. `DataFusionHist::series_facts` removed the second parse, and a search was
/// already paying the first one for `crate::trial_ledger::SearchIdentity::store_data`, so both
/// producers fill this in now for the same one-parse-per-series price. The single-run call site is
/// still BELOW the sweep branch; nothing was hoisted.
///
/// What a resume compares is still `SearchIdentity` rather than this — it witnesses the ingest
/// COMMIT KEYS, which the ADDRESS deliberately excludes, so the two answer different questions and
/// neither replaces the other.
pub(super) fn run_detail_data(
    profile: &BacktestProfile,
    data: Option<&run_fingerprint::DataFingerprint>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": match profile.data.kind {
            harness::DataKind::Bar => "bar",
            harness::DataKind::Tick => "tick",
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

/// The `realism` half of a run manifest's `detail`: the COST MODEL, in the two keys a LISTING
/// needs.
///
/// ⚠ **The DIGEST, not the whole stamp, and that asymmetry is the point.** The full resolved key
/// set is in `report.json` (`vike_analytics::report::BacktestReport::realism`), which is where a
/// reader goes to compare two runs key by key. The manifest's job is the one a listing does without
/// opening anything else — "is this run comparable to the other forty", and above all "was this one
/// FREE" — so it carries the verdict and a one-line summary and nothing more. Repeating forty keys
/// in a second document per run would buy no reader anything and would make the two documents a
/// pair somebody has to keep in step.
///
/// Factored out for the same reason as [`run_detail_data`]: two producers answer this about the
/// same profile, and two copies of these keys would drift the first time either changed.
pub(super) fn run_detail_realism(profile: &BacktestProfile) -> serde_json::Value {
    let stamp = harness::report::realism_stamp(profile);
    serde_json::json!({
        // The REASON, or `null`. Not a bool: an operator reading a listing row needs to know WHICH
        // cost channel was off, and `true` sends them back to the profile to work it out.
        "frictionless": stamp.frictionless,
        "digest": stamp.digest(),
    })
}

/// What the store HELD for every series this search's profile resolves to — the witness
/// `crate::trial_ledger::SearchIdentity::store_data` carries, and the input `--resume`'s gate needs
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
/// parses per series where this cost one. That is no longer true: `DataFusionHist::series_facts`
/// answers coverage and commits from ONE parse, so the collector costs exactly what this walk cost,
/// and everything this function needs is already in the record the collector returns. The witness
/// is now a pure function of [`run_fingerprint::DataFingerprint`], which is why it takes one.
///
/// ⚠ **The rendered TEXT is byte-identical to what the walk produced**, and that is load-bearing
/// rather than tidy: `store_data` is compared as a STRING by
/// `crate::trial_ledger::SearchIdentity::differences`, so a search started by an older binary must
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
/// `crate::run_fingerprint::SeriesFingerprint::commits` is deliberately NOT part of the run
/// ADDRESS.
///
/// ⚠ **The accepted cost of that choice**, from the same place:
/// `crates/vike-data/src/store/datafusion_hist/manifest.rs`'s `rebuild_manifest` re-derives keys from part
/// footers and can come back with FEWER than the data was written with. So a manifest rebuild can
/// move this witness without the data moving, and a resume is then REFUSED that need not have been.
/// That is the safe direction — a needless re-run costs time, a wrong reuse costs the answer — and
/// it is why the refusal message names a rebuild-shaped cause rather than asserting a data change.
///
/// ⚠ The keys are bounded by `crate::run_fingerprint::bound_commits`, so a series with a long flush
/// log contributes a bounded prefix plus its true length: an actively-recorded group still moves the
/// witness on its very next write, and the witness itself cannot grow without limit.
pub(super) fn data_witness(data: &run_fingerprint::DataFingerprint) -> String {
    use std::fmt::Write;
    let mut lines: Vec<String> = Vec::with_capacity(data.series.len());
    for s in &data.series {
        let mut line = format!(
            "series {} {} {} {} {}",
            s.id.kind,
            s.id.venue,
            if s.id.symbol.is_empty() { "-" } else { &s.id.symbol },
            s.id.group.as_deref().unwrap_or("-"),
            s.id.interval.as_deref().unwrap_or("-"),
        );
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
        lines.push(line);
    }
    // SORTED, so the order the loader happened to list series in cannot move the witness — the same
    // rule `run_fingerprint::DataFingerprint::canonical` states for the address.
    lines.sort();
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}
