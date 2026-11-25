//! RETENTION of saved runs: which backtest runs the rule "keep the newest N per strategy file" lets go
//! ([`RunRetention::runs_to_prune`]), the prune that removes them ([`RunRetention::prune`]), and the one remover
//! of a run directory with its two entry points: [`remove_run`] for a FINISHED run (the prune's) and
//! [`remove_unfinished_run`] for one a writer of this process minted and could not finish.
//!
//! # Why
//!
//! The backtesthub saves every backtest it answers as a run (book W35), and nothing removed one, so a box that
//! backtests hundreds of strategy files again and again only grows its disk. The rule is OFF unless the operator
//! sets `config.saved_runs_per_strategy`: these are the user's results, and an automatic delete is never a
//! silent default. The backtesthub applies it after each save; `vike-cli backtest prune --dry-run` lists what it
//! would remove.
//!
//! # What is counted
//!
//! Only a run of kind [`BACKTEST_RUN_KIND`] whose manifest names its strategy file
//! ([`RunManifest::strategy_file`]). Per strategy file the newest [`RunRetention::keep_per_strategy`] are kept,
//! newest by [`RunManifest::finished_at`] (then by run id), so the run a save has just written is never the one
//! that goes. Every backtest the backtesthub saves names its file, whatever its `detail.protocol` (a single run, a
//! walk-forward, a scan, a combinatorial walk, a permutation test:
//! `crates/vike-backtesthub/src/backtesthub_server/saved_run.rs`'s `detail_of`), so they count
//! together. A run of ANY other kind (a study, a `forward` test, a kind invented next year), a backtest naming
//! no strategy file (one saved before its protocol recorded it) and a directory whose manifest does not read are
//! never counted and never pruned.
//!
//! # What is kept although it is over the count
//!
//! - a run a MARK points at, now or in the mark's history ([`Mark::history`]): a moved mark still names a run a
//!   person once chose;
//! - a run whose tag sidecar ([`META_FILE`]) holds a tag or a note, or cannot be read;
//! - a run ANOTHER RUN names: its id appears in the manifest, report or config of a run that is not counted (a
//!   study's recipe names the runs it reads, a forward test its backtest);
//! - a run a [`RunRetention::referencing_documents`] file names (the alert rules: a drift rule's
//!   `reference_run`).
//!
//! The naming checks are a substring match on the run id, so a false match only KEEPS a run.
//!
//! # A doubt is a refusal
//!
//! A marks store, a mark or a referencing document that EXISTS and cannot be read refuses the whole prune:
//! what it names cannot be known, so nothing is deleted. A run whose sidecar cannot be read is kept.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use super::read::{is_one_run_id, read_doc};
use super::scan::scan_runs_reading;
#[cfg(doc)]
use super::scan_runs;
use super::{
    BACKTEST_RUN_KIND, CONFIG_FILE, MANIFEST_FILE, Mark, REPORT_FILE, RunConfig, RunDir,
    RunManifest, RunReadError, RunScan, ScanDiagnostic, ScannedRun, finished_run_dir, read_meta,
};

#[cfg(doc)]
use super::META_FILE;

/// The key under [`RunManifest::detail`] where a backtest producer records the strategy FILE it ran
/// (`crates/vike-backtesthub/src/backtesthub_server/saved_run.rs`'s `detail_of`,
/// `crates/vike-backtesthub/src/backtest_cli.rs`'s `persist_run`).
const STRATEGY_FILE_KEY: &str = "strategy_file";

impl RunManifest {
    /// The strategy file this run is counted under by [`RunRetention`]: `detail.strategy_file` of a run of kind
    /// [`BACKTEST_RUN_KIND`], `None` for every other kind and for a backtest that names none.
    pub fn strategy_file(&self) -> Option<&str> {
        if self.kind != BACKTEST_RUN_KIND {
            return None;
        }
        self.detail.get(STRATEGY_FILE_KEY)?.as_str()
    }
}

/// The retention rule for saved runs, with the two places outside the runs root it must read: the marks store
/// and the documents that may name a run. Built by a BINARY (the backtesthub daemon, `vike-cli`), which resolved
/// the paths; this crate resolves none.
#[derive(Clone, Debug)]
pub struct RunRetention {
    /// How many backtest runs of one strategy file are kept, the newest. At least 1: [`Self::runs_to_prune`]
    /// refuses 0, which would delete the run a save has just written.
    pub keep_per_strategy: usize,
    /// The mark store, `<user_data>/marks` (a sibling of the runs root).
    pub marks_root: PathBuf,
    /// Text files outside the runs root that may name a run by id: the alert rules file. An absent file names
    /// nothing.
    pub referencing_documents: Vec<PathBuf>,
}

impl RunRetention {
    /// The runs of `scan` this rule removes, oldest first within each strategy file. Reads nothing when no
    /// strategy file is over the count.
    pub fn runs_to_prune<'a>(&self, scan: &'a RunScan) -> Result<Vec<&'a ScannedRun>, String> {
        if self.keep_per_strategy == 0 {
            return Err("keeping 0 runs per strategy file would delete the run just saved; the \
                        least is 1"
                .to_string());
        }
        if let Some(root) =
            scan.diagnostics.iter().find(|d| matches!(d, ScanDiagnostic::RootUnreadable { .. }))
        {
            return Err(root.to_string());
        }
        let mut by_file: BTreeMap<&str, Vec<&ScannedRun>> = BTreeMap::new();
        for run in &scan.runs {
            if let Some(file) = run.manifest.strategy_file() {
                by_file.entry(file).or_default().push(run);
            }
        }
        let mut over: Vec<&ScannedRun> = Vec::new();
        for runs in by_file.values_mut() {
            runs.sort_by(|a, b| {
                (&a.manifest.finished_at, &a.run_id).cmp(&(&b.manifest.finished_at, &b.run_id))
            });
            over.extend(&runs[..runs.len().saturating_sub(self.keep_per_strategy)]);
        }
        if over.is_empty() {
            return Ok(over);
        }

        let marked = marked_runs(&self.marks_root)?;
        // shortcut: a counted backtest's documents name a strategy file and a data slice, never a run, so only
        // the runs that are not counted are read for names; read the backtests too the day a profile names a run.
        let mut naming: Vec<String> = Vec::new();
        for run in scan.runs.iter().filter(|run| run.manifest.strategy_file().is_none()) {
            for file in [MANIFEST_FILE, REPORT_FILE, CONFIG_FILE] {
                naming.extend(text_if_present(&run.dir.join(file))?);
            }
        }
        for document in &self.referencing_documents {
            naming.extend(text_if_present(document)?);
        }
        Ok(over
            .into_iter()
            .filter(|run| {
                !marked.contains(&run.run_id)
                    && !annotated(&run.dir)
                    && !naming.iter().any(|text| text.contains(run.run_id.as_str()))
            })
            .collect())
    }

    /// Scan `runs_root` and remove every run [`Self::runs_to_prune`] names. `Ok` holds the removed run ids; the
    /// first failure stops the prune and names the runs already removed. The scan is [`scan_runs`]'s with each
    /// manifest read for what this rule counts by ([`read_counted_manifest`]): the same runs, counted the same.
    pub fn prune(&self, runs_root: &Path) -> Result<Vec<String>, String> {
        let scan = scan_runs_reading(runs_root, read_counted_manifest);
        let mut removed = Vec::new();
        for run in self.runs_to_prune(&scan)? {
            // A prune after a concurrent save may have removed it first.
            if !run.dir.exists() {
                continue;
            }
            if let Err(why) = remove_run(runs_root, &run.run_id) {
                return Err(format!("{why} (removed before it: {removed:?})"));
            }
            removed.push(run.run_id.clone());
        }
        Ok(removed)
    }
}

/// A manifest as [`RunRetention::prune`] reads it: [`RunManifest`]'s every common field, read and checked as that
/// type reads it, and `detail` kept as `detail.strategy_file` alone ([`CountedDetail`]). A bar run's event-log recipe
/// stamps every day of its bars under `detail` (26 kB for six months, 145 kB for three years), and the prune reads
/// every saved run's manifest after each save: each `detail` is checked, none is built. A manifest one read refuses,
/// the other refuses (the retention tests mutate every field), so the prune counts exactly the runs
/// [`scan_runs`] lists.
#[derive(Deserialize)]
struct CountedManifest {
    #[serde(default)]
    schema: u32,
    run_id: String,
    kind: String,
    produced_by: String,
    started_at: String,
    finished_at: String,
    git_sha: Option<String>,
    #[serde(default)]
    strategy_sdk: Option<u32>,
    #[serde(default)]
    fingerprint: Option<String>,
    config: RunConfig,
    #[serde(default)]
    detail: CountedDetail,
    #[serde(default)]
    kept_rows: Vec<vike_model::Instrument>,
}

/// A manifest's `detail` as the retention counts by it: the string under its top-level `strategy_file` key (an
/// object's LAST such key, which is the one `serde_json::Value` keeps: [`RunManifest::strategy_file`]'s reading).
/// Every other value is visited as `serde_json::Value` visits it (each number parsed, each string's escapes
/// decoded), so it refuses what that refuses, and then dropped.
#[derive(Default)]
struct CountedDetail(Option<String>);

impl<'de> Deserialize<'de> for CountedDetail {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Checked { top: true }.deserialize(deserializer).map(CountedDetail)
    }
}

/// One JSON value, visited and dropped; at the `top`, an object's `strategy_file` string is kept ([`CountedDetail`]).
struct Checked {
    top: bool,
}

impl<'de> DeserializeSeed<'de> for Checked {
    type Value = Option<String>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Checked {
    type Value = Option<String>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element_seed(Checked { top: false })?.is_some() {}
        Ok(None)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut file = None;
        while let Some(named) = map.next_key_seed(KeyIs(STRATEGY_FILE_KEY))? {
            if self.top && named {
                file = map.next_value::<serde_json::Value>()?.as_str().map(str::to_string);
            } else {
                map.next_value_seed(Checked { top: false })?;
            }
        }
        Ok(file)
    }
}

/// A map key, read as `serde_json::Value` reads one (a string, its escapes decoded): whether it is the given name.
struct KeyIs(&'static str);

impl<'de> DeserializeSeed<'de> for KeyIs {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<bool, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for KeyIs {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a string key")
    }

    fn visit_str<E: serde::de::Error>(self, key: &str) -> Result<bool, E> {
        Ok(key == self.0)
    }
}

/// [`RunRetention::prune`]'s read of the manifest in `dir`: a [`CountedManifest`] as the [`RunManifest`] it is
/// counted as, its `detail` `{"strategy_file": …}` when it names one and `null` when it does not. Its failures are
/// [`super::read_manifest`]'s, for the same files.
pub(crate) fn read_counted_manifest(dir: &Path) -> Result<RunManifest, RunReadError> {
    let counted: CountedManifest = read_doc(dir, MANIFEST_FILE)?;
    let detail = match counted.detail.0 {
        Some(file) => {
            let mut detail = serde_json::Map::new();
            detail.insert(STRATEGY_FILE_KEY.to_string(), serde_json::Value::String(file));
            serde_json::Value::Object(detail)
        }
        None => serde_json::Value::Null,
    };
    Ok(RunManifest {
        schema: counted.schema,
        run_id: counted.run_id,
        kind: counted.kind,
        produced_by: counted.produced_by,
        started_at: counted.started_at,
        finished_at: counted.finished_at,
        git_sha: counted.git_sha,
        strategy_sdk: counted.strategy_sdk,
        fingerprint: counted.fingerprint,
        config: counted.config,
        detail,
        kept_rows: counted.kept_rows,
    })
}

/// Remove the FINISHED run `run_id` under `runs_root`, its whole directory.
///
/// The id is held to one directory name of the run-id alphabet and its manifest must read
/// ([`finished_run_dir`]'s rule), so no value can name a path outside `runs_root`, and a directory still being
/// written is not touched. A run directory that is a LINK is refused: what it points at is not under
/// `runs_root`.
pub fn remove_run(runs_root: &Path, run_id: &str) -> Result<(), String> {
    let dir =
        finished_run_dir(runs_root, run_id).map_err(|e| format!("not removing {run_id:?}: {e}"))?;
    let kind = std::fs::symlink_metadata(&dir)
        .map_err(|e| format!("not removing {}: {e}", dir.display()))?
        .file_type();
    if !kind.is_dir() {
        return Err(format!(
            "not removing {}: it is a link, not a run directory under {}",
            dir.display(),
            runs_root.display()
        ));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))
}

/// **Remove the UNFINISHED run `run`, the directory a writer of THIS process minted ([`super::create_run_dir`]'s
/// [`RunDir`]) and could not finish**: the other end of a run's life from [`remove_run`]'s, for a writer that streams a
/// run's documents and stops part-way (the backtesthub's summarized search, whose half-written `report.json` is no
/// document and which no prune would ever count). The same guards: `run`'s id is ONE directory name of the run-id
/// alphabet and its path is that name under `runs_root`, so no value can name a path outside it, and a LINK is
/// refused. A run whose [`MANIFEST_FILE`] exists is FINISHED and refused here: removing it is [`remove_run`]'s, under
/// the retention's rules.
pub fn remove_unfinished_run(runs_root: &Path, run: &RunDir) -> Result<(), String> {
    let dir = &run.path;
    if !is_one_run_id(&run.run_id) || *dir != runs_root.join(&run.run_id) {
        return Err(format!(
            "not removing {}: it is not the run {:?} under {}",
            dir.display(),
            run.run_id,
            runs_root.display()
        ));
    }
    let kind = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("not removing {}: {e}", dir.display()))?
        .file_type();
    if !kind.is_dir() {
        return Err(format!(
            "not removing {}: it is a link, not a run directory under {}",
            dir.display(),
            runs_root.display()
        ));
    }
    if dir.join(MANIFEST_FILE).exists() {
        return Err(format!(
            "not removing {}: it holds a manifest, so it is a finished run (`remove_run` removes those)",
            dir.display()
        ));
    }
    std::fs::remove_dir_all(dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))
}

/// Every run id a mark under `marks_root` points at, now or in its history. No store is no mark.
fn marked_runs(marks_root: &Path) -> Result<BTreeSet<String>, String> {
    let refuse = |path: &Path, e: &dyn std::fmt::Display| {
        format!(
            "cannot read the mark store at {} ({e}) — no run is pruned while a mark cannot be read",
            path.display()
        )
    };
    let mut ids = BTreeSet::new();
    let mut dirs = vec![marks_root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound && dir == marks_root => continue,
            Err(e) => return Err(refuse(&dir, &e)),
        };
        for entry in entries {
            let path = entry.map_err(|e| refuse(&dir, &e))?.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                let text = std::fs::read_to_string(&path).map_err(|e| refuse(&path, &e))?;
                let mark: Mark = serde_json::from_str(&text).map_err(|e| refuse(&path, &e))?;
                ids.insert(mark.run_id);
                ids.extend(mark.history.into_iter().map(|moved| moved.run_id));
            }
        }
    }
    Ok(ids)
}

/// Whether a person attached a tag or a note to the run, or its sidecar cannot be read (then nobody can say).
fn annotated(dir: &Path) -> bool {
    match read_meta(dir) {
        Ok(meta) => !meta.tags.is_empty() || !meta.notes.is_empty(),
        Err(_) => true,
    }
}

/// The text of `path`, `None` when there is no such file; any other failure refuses the prune.
fn text_if_present(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!(
            "cannot read {} ({e}) — no run is pruned while what it names cannot be known",
            path.display()
        )),
    }
}
