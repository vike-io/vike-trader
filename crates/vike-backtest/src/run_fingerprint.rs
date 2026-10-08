//! What a run's INPUTS were, and the ADDRESS over them.
//!
//! # Why this is not in `vike_model::runs`
//!
//! `vike_model::runs` is UNGATED and dependency-light on purpose: its module doc pre-authorises a whole
//! MOVE down to `vike-model` if a producer ever arrives that cannot depend on this crate, and
//! forbids a `pub use` shim behind it. `vike-data` is an OPTIONAL dependency here, so naming
//! `SeriesId` there would either break the default build or make that move impossible. So the
//! manifest carries the fingerprint as a STRING it neither computes nor validates — the same shape
//! `vike_model::runs::RunManifest::git_sha` already has — and this module is the backtest producer's
//! answer to what its inputs are.
//!
//! # The RECORD and the ADDRESS are different, deliberately
//!
//! [`DataFingerprint`] is the RECORD: it carries the store path, the byte and part counts and the
//! ingest commit keys, because an investigation wants all of them. [`DataFingerprint::canonical`]
//! is the ADDRESS: it renders only the facts a compaction, a manifest rebuild or a move to another
//! box cannot change. Hashing a byte count would make every ordinary maintenance run orphan every
//! baseline, and a baseline that is orphaned by housekeeping is not a baseline — which is precisely
//! what `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`'s decision 8 needs one
//! for. The tests below pin both halves of that split.
//!
//! # Nothing here formats a float
//!
//! `vike_data::SeriesCoverage` is integers throughout, and that is load-bearing rather than lucky:
//! a float rendering is exactly where two platforms disagree, and
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` records that
//! this workspace has already paid for that once.

use serde::{Deserialize, Serialize};
use vike_data::{SeriesCoverage, SeriesId};

use crate::harness::{BacktestProfile, DataKind};

/// Which stored series a profile's slice resolves to.
///
/// ⚠ **MOVED here from `crates/vike-backtest/src/backtest_cli.rs`, where it was
/// `fingerprint_series_ids`.** It answers ONE question — which series will this run open — and a
/// second consumer arrived that could not reach it there: `crate::data_plan` runs the same
/// resolution for the `data.require_coverage` gate and the `data.explain` plan, and both must work
/// on the REMOTE route, where the store is a trait-only `Arc<dyn HistStore>` (a DEFAULT build)
/// while `backtest_cli` needs `datafusion-store` to compile at all. A copy would have been a second
/// answer to "what does this run read", which is the fact a run's whole ADDRESS rests on. The name
/// widened with the consumers: it is the run's PLANNED slice, and the fingerprint is one of three
/// things now derived from it.
///
/// # Bar mode
///
/// One `kind=bar` series per symbol, sub-partitioned by the bar step — a real path segment in this
/// store, not a column. ⚠ **Never a grouped one**: `crates/vike-data/src/store/store_kind.rs` sets
/// `grouped: false` for `bar` and says why at the row ("no `symbol_col` column exists here, so a
/// bar series can never be grouped"), and `DataFusionHist::load_bars` reads the per-symbol
/// directory alone.
///
/// # Tick mode
///
/// One series per LANE the entry's `crate::hist_replay::SeriesKind` admits — `wants_quotes` /
/// `wants_trades` / `wants_books`, the replay loader's OWN predicates, because `SeriesKind::Tick`
/// (the default, and what the `venue` x `symbols` expansion produces) means quotes AND trades AND
/// book events: a fingerprint naming only one would claim the run depended on less data than it did.
///
/// ⚠ **...PLUS every GROUPED series of the same `(kind, venue)`, and that half is not optional.**
/// All three tick kinds are `grouped: true`, and `DataFusionHist`'s `scan_quotes`/`scan_trades`/
/// `scan_book_updates` each read the per-symbol directory AND union every `group=` directory under
/// `kind=…/venue=…`, filtering by the row-level symbol column. So on a grouped store — which is
/// what a recorded Polymarket tape is — the per-symbol path does not exist and ALL the rows the run
/// replays live in the group. Naming only the per-symbol id there made the data half of the address
/// a CONSTANT: a backfill could add a month, the run could replay half again as many rows and
/// produce a materially different curve, and [`input_fingerprint`] would be unchanged — so a later
/// `diff`/`gate` would attribute a DATA change to an engine edit. Silent in both directions.
///
/// ⚠ **The accepted residual, stated rather than discovered.** A grouped series' coverage is the
/// coverage of the WHOLE GROUP, so rows belonging to a symbol this run never read still move the
/// address. That is a FALSE NEGATIVE — two runs with genuinely identical inputs can address
/// differently, and a comparison then reports "no comparable baseline", which is conservative and
/// visible. The alternative is the FALSE POSITIVE above, where two runs over different data address
/// the same and a comparison silently blames the wrong thing. Narrowing this needs per-symbol
/// statistics inside a grouped part, which the store does not expose.
///
/// ⚠ **The same residual reads DIFFERENTLY for a coverage gate, and that is worth knowing before
/// reusing this list there.** A grouped series' coverage spans the whole group, so a window the
/// group covers reads as covered for a symbol whose own rows may be absent from it — a FALSE
/// NEGATIVE for the gate, i.e. it can pass a run it should have stopped. That is the conservative
/// direction for an ADDRESS and the permissive one for a GATE, and no narrowing exists on either
/// side until the store exposes per-symbol statistics inside a grouped part.
///
/// `held` is the store's own inventory (`DataFusionHist::list_series`, or the
/// `vike_data::HistStore::inventory` ids on the trait route). It is a PARAMETER rather than a
/// second call so the walk that answers "what does this store hold" happens exactly once.
pub fn planned_series_ids(profile: &BacktestProfile, held: &[SeriesId]) -> Vec<SeriesId> {
    let mut out = Vec::new();
    for s in profile.data.resolved_series() {
        match profile.data.kind {
            DataKind::Bar => out.push(SeriesId::per_symbol(
                "bar",
                &s.venue,
                &s.symbol,
                Some(profile.data.interval.clone()),
            )),
            DataKind::Tick => {
                for (wanted, kind) in [
                    (s.kind.wants_quotes(), "quote"),
                    (s.kind.wants_trades(), "trade"),
                    (s.kind.wants_books(), "book"),
                ] {
                    if !wanted {
                        continue;
                    }
                    out.push(SeriesId::per_symbol(kind, &s.venue, &s.symbol, None));
                    out.extend(
                        held.iter()
                            .filter(|h| h.group.is_some() && h.kind == kind && h.venue == s.venue)
                            .cloned(),
                    );
                }
            }
        }
    }
    // Two symbols of one venue pull the SAME grouped directories, and the reader reads each group
    // once. Sorted so the collector's output order is a function of the inputs rather than of the
    // profile's symbol order — [`DataFingerprint::canonical`] sorts its lines too, so this is for
    // the RECORD's readability rather than for the address.
    out.sort();
    out.dedup();
    out
}

/// The version of the `detail.data` subtree this module writes into a run manifest.
pub const DATA_FINGERPRINT_SCHEMA: u32 = 1;

/// The most ingest commit keys one series contributes to a run record, as a leading PREFIX.
///
/// ⚠ **A bound is not a preference here, and the number it replaces was UNBOUNDED.** A per-symbol
/// series carries a handful of keys, so this never binds there. A GROUPED series' log is the whole
/// VENUE's flush log: `DataFusionHist::commit_rows` pushes one key per keyed append, the production
/// recorder flushes at `max_rows: 5_000` / `max_age: 30s`, and the log is pruned only by retention.
/// The age bound ALONE is ~2,880 keys/day/series, so a 30-day window is ~86,000 keys — and at
/// roughly 75 bytes once `serde_json::to_string_pretty` puts one per line that is about **6 MB per
/// grouped series**, multiplied by the kinds the profile wants and the groups the venue holds, in
/// EVERY run directory. Against [`vike_model::runs::MAX_EQUITY_SAMPLES`]'s own budget — "a thousand
/// stored runs cost about 600 MB, a bound an operator can reason about", on a box that also hosts
/// the live daemon — one run would have blown the whole allowance.
///
/// 256 keys is roughly **19 KB per series**, and the cost is PARAMETRIC because the bound is
/// per-series while the SERIES COUNT is not bounded here at all:
/// [`planned_series_ids`] names every held grouped
/// series of the same `(kind, venue)` for each wanted lane, so a tick profile wanting L lanes over
/// a venue holding G groups records `L · G` grouped series and costs about `L · G · 19 KB`. Three
/// lanes over three groups is nine series — read that against
/// [`vike_model::runs::MAX_EQUITY_SAMPLES`]'s ~600 KB-per-run budget and decide; this doc deliberately
/// writes no total, because the earlier spelling wrote one ("stays in the tens of KB") and it was
/// wrong by an order of magnitude for exactly that case.
///
/// What the bound IS unconditionally: far more than a per-symbol series ever holds, and a small
/// fraction of one day of a grouped venue's log — which is the case where the log stops being this
/// run's provenance anyway (see [`SeriesFingerprint::commits`]).
///
/// The shape is [`vike_model::runs::MAX_TRADES`]'s exactly, for the reason stated there: a bounded log
/// is a PREFIX plus a true count, never a sample. [`SeriesFingerprint::commits_len`] is the count.
pub const MAX_COMMIT_KEYS: usize = 256;

/// Bound one series' commit log to [`MAX_COMMIT_KEYS`]: the kept leading PREFIX and the TRUE count
/// it was taken from. (It was described as a CHRONOLOGICAL prefix; see [`SeriesFingerprint::commits`]
/// for why manifest v3 no longer guarantees that, and why nothing depends on it.)
///
/// ⚠ **A function rather than two lines at the call site, and the reason is testability.** The only
/// caller is `crates/vike-backtest/src/backtest_cli/run_record.rs`'s `collect_data_fingerprint`,
/// which needs a real store holding hundreds of commit keys before the bound binds at all — so a
/// test driving the collector over any store a test can cheaply build proves that the bound was NOT
/// EXERCISED, and passes whether the truncation is there or not. (Measured: removing the truncation
/// left the whole 819-test suite green.) Pure, the primitive is provable directly, exactly as
/// [`vike_model::runs::decimate`] is.
pub fn bound_commits(commits: Vec<String>) -> (Vec<String>, usize) {
    let source_len = commits.len();
    (commits.into_iter().take(MAX_COMMIT_KEYS).collect(), source_len)
}

/// One series the run READ, and what the store held for it at the moment the run started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesFingerprint {
    /// Which series — `(kind, venue, symbol, interval)`, or a `group=` leaf.
    pub id: SeriesId,
    /// What the store's manifest said it held. `None` when the store holds no such series at all
    /// — a REAL and reportable state (a tick profile naming a lane that was never recorded), and a
    /// different answer from an empty series, which is why it is an `Option` rather than a default.
    pub coverage: Option<SeriesCoverage>,
    /// The series' INGEST COMMIT KEYS — the store's only record of WHO WROTE it — as a PREFIX
    /// bounded by [`MAX_COMMIT_KEYS`]. [`Self::commits_len`] is how many the series actually
    /// carries, so a reader always knows whether it is holding all of a short log or part of a long
    /// one.
    ///
    /// ⚠ **This said CHRONOLOGICAL prefix, and at manifest v3 that is no longer guaranteed.** The
    /// store stopped keeping a separate append-ordered `commits` array and now answers from the
    /// part that holds each key's rows (`crates/vike-data/src/store/datafusion_hist/manifest.rs`'s
    /// `Manifest::commit_keys`), so the order is `files` order: still commit order for an
    /// append-only series, and NOT after a compaction, whose merge output joins at the end carrying
    /// the union of its inputs' keys. Nothing here breaks — this field is a RECORD, and
    /// [`input_fingerprint`] states outright that the ingest commit keys are NOT in the address —
    /// but a reader comparing two prefixes should not read the order as a timeline.
    ///
    /// Recorded because an investigation wants it; NOT addressed, because
    /// `crates/vike-data/src/store/datafusion_hist/manifest.rs`'s `rebuild_manifest` re-derives keys from
    /// part footers and can come back with fewer than the data was written with. That split is what
    /// makes bounding it safe: truncating a RECORD loses detail, where truncating an ADDRESS would
    /// change it.
    ///
    /// ⚠ **For a GROUPED series this is the whole VENUE's flush log, not this run's provenance.**
    /// One grouped directory holds every symbol of its group, so its keys name the collector that
    /// wrote the GROUP — the rows this run actually read are a subset nothing here can separate
    /// out. The prefix is still worth keeping (it names the collector, which is the question that
    /// gets asked), and it is precisely why the bound exists rather than the field.
    pub commits: Vec<String>,
    /// How many commit keys the series actually carries, before [`MAX_COMMIT_KEYS`] truncated
    /// [`Self::commits`]. Equal to `commits.len()` for every series under the bound.
    ///
    /// ⚠ **`#[serde(default)]` rather than a [`DATA_FINGERPRINT_SCHEMA`] bump, and the choice is
    /// stated because both were available.** This field joined the shape after v1 was authored, so
    /// requiring it would have made "schema 1" name two shapes — the exact back-compat doctrine
    /// `vike_model::runs::RunManifest` and `vike_analytics::report::BacktestReport` both follow:
    /// a field added LATER defaults, the originals stay required. Bumping to v2 instead would be
    /// defensible and is REFUSED for one reason: v1 has never shipped, so a bump would spend a
    /// version number describing a distinction no document on any disk can exhibit, while
    /// defaulting costs nothing and keeps ONE rule across all three persisted documents.
    #[serde(default)]
    pub commits_len: usize,
}

/// The data slice a run read, as the store held it — the RECORD half.
///
/// ⚠ **The GROUPED-SERIES residual, with its magnitude.** A grouped series' coverage is the
/// coverage of the WHOLE GROUP, so rows belonging to a symbol this run never read still move the
/// address ([`planned_series_ids`] argues why naming
/// the group anyway is the safe direction). On an ACTIVELY RECORDED group — the Polymarket case the
/// grouped layout exists for — that coverage moves continuously, so two runs over an INACTIVE
/// symbol (a resolved market, whose own rows can no longer change) address differently EVERY TIME
/// and baseline comparison is **permanently unusable there**, not occasionally unreliable. Read
/// that as the stated cost of the fix rather than as an edge case to plan around.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFingerprint {
    /// [`DATA_FINGERPRINT_SCHEMA`] at write time; `0` in a document written before this existed.
    #[serde(default)]
    pub schema: u32,
    /// WHERE the run read its data — RECORDED, never ADDRESSED (see the module doc).
    ///
    /// ⚠ **It is a path only when the run read LOCAL files.** Since
    /// `docs/decisions/0084-only-the-datahub-touches-the-store.md` routed the run path, a run
    /// without `--store` reads over the wire and this field names the DATAHUB'S ADDRESS instead —
    /// because the root such a run would otherwise resolve is the SERVER's, and a directory
    /// recorded client-side is a guess about another box's filesystem. Documents written before
    /// that always hold a path and nothing tells the two apart by shape, which is exactly why the
    /// never-ADDRESSED rule was already the contract: read it as provenance prose.
    pub store: String,
    /// The run's requested window, inclusive, in epoch ms. `None` is unbounded, which is a
    /// different answer from zero.
    pub from_ms: Option<i64>,
    /// See [`Self::from_ms`].
    pub to_ms: Option<i64>,
    /// One entry per series the profile resolves to, in whatever order the collector produced them
    /// — [`Self::canonical`] sorts, so this order is presentation only.
    pub series: Vec<SeriesFingerprint>,
}

impl DataFingerprint {
    /// The ADDRESS half: a deterministic, platform-independent rendering of the facts a compaction,
    /// a manifest rebuild or a move to another box cannot change.
    ///
    /// Sorted, so the order the loader happened to list series in cannot move the address. One
    /// fact per line, integers only, ASCII only — a rendering somebody can paste into an issue
    /// beside the digest and check by eye.
    pub fn canonical(&self) -> String {
        use std::fmt::Write;
        let mut lines: Vec<String> = Vec::with_capacity(self.series.len());
        for s in &self.series {
            let mut line = String::new();
            let _ = write!(
                line,
                "series {} {} {} {} {}",
                s.id.kind,
                s.id.venue,
                if s.id.symbol.is_empty() { "-" } else { &s.id.symbol },
                s.id.group.as_deref().unwrap_or("-"),
                s.id.interval.as_deref().unwrap_or("-"),
            );
            match &s.coverage {
                // ⚠ `bytes` and `parts` are deliberately ABSENT — see the module doc. A compaction
                // changes both and not one row.
                // ⚠ `commits`/`commits_len` are ABSENT here too, like `bytes` and `parts` — the
                // module doc's RECORD-versus-ADDRESS split. That is also what makes truncating the
                // log safe: the address cannot move because the bound changed.
                Some(c) => {
                    let _ = write!(
                        line,
                        " coverage {} {} {} {}",
                        c.first_ts, c.last_ts, c.rows, c.dates
                    );
                }
                None => line.push_str(" coverage missing"),
            }
            lines.push(line);
        }
        lines.sort();

        let mut out = format!("data-fingerprint v{DATA_FINGERPRINT_SCHEMA}\n");
        let _ = writeln!(
            out,
            "range {} {}",
            self.from_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string()),
            self.to_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string()),
        );
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// The domain tag every input address opens with. Bumping it invalidates every stored address
/// deliberately — which is what you want when the SET of facts being hashed changes, because two
/// addresses computed over different fact sets are not comparable and must not look it.
///
/// ⚠ `vike-run` in the string is the BACKTEST run, not the crate of that name (merged into
/// `vike-mount` by docs/decisions/0098). The tag is hashed into every stored address, so it does not
/// move with a crate.
pub const INPUT_FINGERPRINT_VERSION: &str = "vike-run-inputs v1";

/// The run's INPUT ADDRESS: lowercase hex SHA-256 over the resolved config TEXT and
/// [`DataFingerprint::canonical`].
///
/// # What is in it, and what is deliberately not
///
/// **In:** the profile as the operator wrote it, byte for byte, and the content facts of every
/// series the run reads. **Not in:** the build, the platform, the store path, the byte and part
/// counts, the ingest commit keys, and — above all — the RESULT.
///
/// The build is out because the question this exists to answer is "did my engine edit change the
/// result?", which means finding the run that had the SAME inputs and a DIFFERENT build. An address
/// including the build would never match across a commit boundary, every run would be its own
/// baseline, and nothing would ever be comparable. `vike_model::runs::RunManifest::git_sha` carries the
/// build separately, which is where a comparison reads it FROM.
///
/// The result is out because
/// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` records that
/// this workspace's equity fold still differs between MSVC and glibc at the last bit. An address
/// over `(config, data)` is reproducible on both boxes; one over the curve is not — and a
/// non-reproducible address is not an address.
///
/// ⚠ The two halves are SEPARATED on the wire by a token that cannot occur in either, so a config
/// ending in the text the canonical rendering opens with cannot produce the same bytes as a
/// different pair. A hash over a bare concatenation collides by construction rather than by luck.
///
/// The config is the TEXT rather than a serialized struct because `BacktestProfile` and its nine
/// nested config types derive `Deserialize` ONLY — see `vike_model::runs::CONFIG_FILE` for the argument
/// in full.
pub fn input_fingerprint(profile_toml: &str, data: &DataFingerprint) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(INPUT_FINGERPRINT_VERSION.as_bytes());
    h.update(b"\n--config--\n");
    h.update(profile_toml.as_bytes());
    h.update(b"\n--data--\n");
    h.update(data.canonical().as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[path = "run_fingerprint_tests.rs"]
#[cfg(test)]
mod run_fingerprint_tests;
